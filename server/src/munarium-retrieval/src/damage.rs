// SPDX-License-Identifier: Apache-2.0
//! Durable artifact damage on the serving side (ClearView R10, ART1, ART2).
//!
//! The ledger in `munarium_store_pg::artifact_damage` decides; this module
//! supplies what a serving replica observes and enforces:
//!
//! - **Local isolation.** An integrity failure stops this replica using its
//!   own copy at once: hydration quarantines the key, and a copy that fails
//!   at open is evicted so the next read re-fetches and re-verifies.
//! - **Authoritative verification.** The replica then fetches the pinned
//!   artifact directly from the authoritative store, compares every object
//!   with the pin, and records what it saw under its own verifier identity,
//!   signed with its verifier key ([`DamageGuard::with_signing_key`]).
//!   Whether that confirms a fleet block is the ledger's decision; an
//!   unsigned observation, from a replica without a key, never counts.
//! - **Admission and release.** A read checks the pin at admission and must
//!   pass the release fence before its result leaves the serving plane.
//! - **Repair verification.** A replica verifies each preauthorized repair
//!   attempt once; two independent matches let the ledger clear the block.
//!
//! - **Single-replica assurance.** A deployment that declares exactly one
//!   serving replica ([`parse_replica_assurance`]) lets the ledger accept that
//!   replica's own second run and fetch, spaced by an interval, in place of a
//!   second replica. The replica then re-verifies its own due candidates and
//!   its own first repair match once the interval has passed.
//!
//! Everything here is opt-in through [`DamageMode`]. In `off`, the default,
//! no executor carries a [`DamageContext`] and the v1.4 path runs unchanged.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use munarium_core::artifact_verifier::VerifierSigningKey;
use munarium_core::{KernelError, Result};
use munarium_datastore::audit::{audit_artifact, ArtifactAudit, AuditOutcome};
use munarium_datastore::hydrate::L1Cache;
use munarium_datastore::store::ArtifactStore;
use munarium_datastore::verify::{Limits, ReaderCapabilities};
use munarium_datastore::ArtifactCacheKey;
use munarium_store_pg::artifact_damage::{
    check_single_replica_interval, ArtifactDamageLedger, AuthoritativeObservation, Authority,
    Decision, ObjectPin, ObservationSignature, Outcome, PgPool, PinAdmission, RecordResult,
    RepairAttempt, Timestamp, VerifierIdentity, DEFAULT_SINGLE_REPLICA_INTERVAL_SECS,
    MANIFEST_OBJECT,
};

use crate::executor::L0Cache;

/// How much of the damage protocol this process takes part in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DamageMode {
    /// The v1.4 behavior: per-process quarantine only, no durable state.
    Off,
    /// Record findings, isolate locally, honor existing blocks at admission
    /// and release; never commit a block or a clearance. Every replica runs
    /// this before any runs `enforce`, so all readers can honor a block
    /// before one can be committed.
    Observe,
    /// Observe, plus commit confirmed blocks and verified clearances.
    Enforce,
}

impl DamageMode {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim() {
            "" | "off" => Ok(Self::Off),
            "observe" => Ok(Self::Observe),
            "enforce" => Ok(Self::Enforce),
            other => Err(KernelError::InvalidInput(format!(
                "MUNARIUM_ARTIFACT_DAMAGE_MODE must be off|observe|enforce, got {other:?}"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Observe => "observe",
            Self::Enforce => "enforce",
        }
    }
}

/// Read `MUNARIUM_ARTIFACT_DAMAGE_REPLICAS` and
/// `MUNARIUM_ARTIFACT_DAMAGE_SINGLE_INTERVAL_SECS` (passed in raw) against the
/// declared `MUNARIUM_REPLICA_COUNT`.
///
/// Returns `None` for the default `multi` rule (two independent verifiers)
/// and `Some(interval)` for `single`. `single` is refused when the deployment
/// declares more than one replica, and an unknown value or an interval outside
/// the accepted range is refused rather than falling back to either rule. The
/// interval is ignored unless `single` is selected.
pub fn parse_replica_assurance(
    replicas: &str,
    interval_secs: &str,
    replica_count: u32,
) -> Result<Option<i64>> {
    match replicas.trim() {
        "" | "multi" => Ok(None),
        "single" => {
            if replica_count > 1 {
                return Err(KernelError::InvalidInput(format!(
                    "MUNARIUM_ARTIFACT_DAMAGE_REPLICAS=single requires a single-replica \
                     deployment, but MUNARIUM_REPLICA_COUNT is {replica_count}"
                )));
            }
            let interval = match interval_secs.trim() {
                "" => DEFAULT_SINGLE_REPLICA_INTERVAL_SECS,
                raw => raw.parse::<i64>().map_err(|e| {
                    KernelError::InvalidInput(format!(
                        "MUNARIUM_ARTIFACT_DAMAGE_SINGLE_INTERVAL_SECS: {e}"
                    ))
                })?,
            };
            check_single_replica_interval(interval).map_err(|e| {
                KernelError::InvalidInput(format!(
                    "MUNARIUM_ARTIFACT_DAMAGE_SINGLE_INTERVAL_SECS: {e}"
                ))
            })?;
            Ok(Some(interval))
        }
        other => Err(KernelError::InvalidInput(format!(
            "MUNARIUM_ARTIFACT_DAMAGE_REPLICAS must be multi|single, got {other:?}"
        ))),
    }
}

/// Mirror publication's view of fleet blocks (the 20 November 2026 decision
/// on artifact-damage open question 5).
///
/// A build whose result is a pin that carries an active fleet block must not
/// report success or bind it: a rebuild that converges on a blocked artifact
/// id would otherwise republish the same id. The gate is built only in
/// `observe` and `enforce` ([`PublicationGate::for_mode`] returns `None` for
/// `off`, which leaves publication exactly as before).
///
/// - `enforce` refuses the publication with a `datastore-unavailable` error
///   that names only the version, the artifact id and the incident id. An
///   unavailable pin check refuses too, as serving admission does.
/// - `observe` publishes as before and reports the blocked pin (a warning
///   and the build observer's `publication_blocked`). Like serving in
///   `observe`, it never changes an outcome on its own authority beyond
///   what it already honors: serving still refuses the blocked pin.
///
/// The refusal holds until an ART2 repair clears the block; a cleared pin
/// publishes normally.
#[derive(Debug, Clone)]
pub struct PublicationGate {
    mode: DamageMode,
    ledger: ArtifactDamageLedger,
}

/// A publication refused because its pin is blocked. Content-free: ids only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedPublication {
    pub index_version_id: String,
    pub artifact_id: String,
    pub incident_id: String,
}

impl std::fmt::Display for BlockedPublication {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "artifact {} of index version {} is blocked by integrity incident {}; publication \
             is refused until a verified repair clears it",
            self.artifact_id, self.index_version_id, self.incident_id
        )
    }
}

impl From<BlockedPublication> for KernelError {
    /// Classified as `datastore_unavailable` by a build job (C2).
    fn from(b: BlockedPublication) -> Self {
        KernelError::DatastoreUnavailable(b.to_string())
    }
}

/// What the gate decided for one pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublicationCheck {
    /// No active block: publish.
    Clear,
    /// Blocked, and this process only observes: publish, and report it.
    BlockedObserved(BlockedPublication),
    /// Blocked, and this process enforces: refuse.
    Refused(BlockedPublication),
    /// The pin could not be read. `enforce` refuses on it; `observe`
    /// publishes and reports it.
    Unavailable(String),
}

impl PublicationGate {
    /// The gate for `mode`, or `None` for `off`.
    pub fn for_mode(mode: DamageMode, pool: PgPool, tenant_id: &str) -> Option<Self> {
        match mode {
            DamageMode::Off => None,
            DamageMode::Observe | DamageMode::Enforce => Some(Self {
                mode,
                ledger: ArtifactDamageLedger::new(pool, tenant_id),
            }),
        }
    }

    pub fn mode(&self) -> DamageMode {
        self.mode
    }

    /// Read the pin's block. Never writes.
    pub async fn check(&self, index_version_id: &str, artifact_id: &str) -> PublicationCheck {
        match self.ledger.admit(index_version_id, artifact_id).await {
            Err(e) => PublicationCheck::Unavailable(e.to_string()),
            Ok(PinAdmission {
                blocked_by: None, ..
            }) => PublicationCheck::Clear,
            Ok(PinAdmission {
                blocked_by: Some(incident_id),
                ..
            }) => {
                let blocked = BlockedPublication {
                    index_version_id: index_version_id.to_string(),
                    artifact_id: artifact_id.to_string(),
                    incident_id,
                };
                if self.mode == DamageMode::Enforce {
                    PublicationCheck::Refused(blocked)
                } else {
                    PublicationCheck::BlockedObserved(blocked)
                }
            }
        }
    }
}

/// Load a replica's verifier signing key from `MUNARIUM_ARTIFACT_VERIFIER_KEY_FILE`.
/// The error names the path, never the file's contents.
pub fn load_verifier_key(path: &str) -> Result<VerifierSigningKey> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        KernelError::InvalidInput(format!(
            "MUNARIUM_ARTIFACT_VERIFIER_KEY_FILE {path:?} could not be read: {}",
            e.kind()
        ))
    })?;
    VerifierSigningKey::from_encoded(&text).map_err(|e| {
        KernelError::InvalidInput(format!("MUNARIUM_ARTIFACT_VERIFIER_KEY_FILE {path:?}: {e}"))
    })
}

/// Whether an unblocked pin at `current` releases a local isolation recorded
/// at `*at`, updating an unknown `*at` in place.
///
/// A known generation is released only by a later one: the pin was blocked
/// and then cleared after the isolation. An unknown generation (`None`) can
/// never be released by itself, because the clearance it would need may
/// have happened before the isolation. The first generation read afterwards
/// takes its place instead, so release waits for a clearance after that
/// read. That is never earlier than with the true generation, which was at
/// most the one read.
pub(crate) fn release_after_clearance(at: &mut Option<i64>, current: i64) -> bool {
    match *at {
        Some(isolated_at) => current > isolated_at,
        None => {
            *at = Some(current);
            false
        }
    }
}

/// Where an integrity failure was detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureSite {
    /// Bytes fetched for hydration did not match the pin. L1 quarantined the
    /// key.
    Hydrate,
    /// A hydrated local copy failed when opened: local damage.
    Open,
}

/// Process-wide damage state: one per replica, shared by every tenant's
/// executor like the L1 cache.
#[derive(Debug)]
pub struct DamageGuard {
    pub mode: DamageMode,
    /// This replica's verifier identity: its verifier key's fingerprint once
    /// [`DamageGuard::with_signing_key`] is applied. Without a key it is the
    /// configured instance name, and its observations never count.
    pub verifier_id: String,
    /// The verifier signing key. Never logged.
    signing_key: Option<VerifierSigningKey>,
    /// Locally isolated keys and the pin generation at isolation. A key is
    /// released only after the pin's generation advances unblocked, i.e. a
    /// block followed by a verified clearance. `None` is an unknown
    /// generation (the pin could not be read when the key was isolated):
    /// see [`release_after_clearance`].
    isolated: Mutex<HashMap<ArtifactCacheKey, Option<i64>>>,
    /// `(tenant, attempt)` pairs this replica has already verified.
    verified_attempts: Mutex<HashSet<(String, String)>>,
    /// `Some(interval)` when the deployment is declared single-replica.
    single_replica_interval_secs: Option<i64>,
    /// `(tenant, candidate fetch)` pairs this replica has already re-verified
    /// as a single replica.
    reconfirmed_candidates: Mutex<HashSet<(String, String)>>,
}

impl DamageGuard {
    pub fn new(mode: DamageMode, verifier_id: impl Into<String>) -> Self {
        Self {
            mode,
            verifier_id: verifier_id.into(),
            isolated: Mutex::new(HashMap::new()),
            verified_attempts: Mutex::new(HashSet::new()),
            signing_key: None,
            single_replica_interval_secs: None,
            reconfirmed_candidates: Mutex::new(HashSet::new()),
        }
    }

    /// Sign this replica's observations with `key`. Its verifier identity
    /// becomes the key's fingerprint, which is what the operator enrolls.
    pub fn with_signing_key(mut self, key: VerifierSigningKey) -> Self {
        self.verifier_id = key.fingerprint();
        self.signing_key = Some(key);
        self
    }

    /// Whether this replica signs its observations.
    pub fn signs(&self) -> bool {
        self.signing_key.is_some()
    }

    /// Declare this replica the deployment's only one (see
    /// [`parse_replica_assurance`]): its ledgers accept its own spaced second
    /// run as confirmation, recorded as single-replica assurance.
    pub fn with_single_replica(mut self, interval_secs: i64) -> Result<Self> {
        check_single_replica_interval(interval_secs)?;
        self.single_replica_interval_secs = Some(interval_secs);
        Ok(self)
    }

    /// The single-replica interval, when the deployment is declared so.
    pub fn single_replica_interval_secs(&self) -> Option<i64> {
        self.single_replica_interval_secs
    }

    /// The tenant's ledger under this replica's assurance rule.
    pub fn ledger(&self, pool: &PgPool, tenant: &str) -> ArtifactDamageLedger {
        let ledger = ArtifactDamageLedger::new(pool.clone(), tenant);
        match self.single_replica_interval_secs {
            // Validated when it was set; on the impossible failure keep the
            // stricter two-verifier rule.
            Some(interval) => ledger
                .clone()
                .with_single_replica(interval)
                .unwrap_or(ledger),
            None => ledger,
        }
    }

    pub fn active(&self) -> bool {
        self.mode != DamageMode::Off
    }

    pub fn authority(&self) -> Authority {
        let enforce = self.mode == DamageMode::Enforce;
        Authority {
            may_block: enforce,
            may_clear: enforce,
        }
    }

    fn isolated(&self) -> std::sync::MutexGuard<'_, HashMap<ArtifactCacheKey, Option<i64>>> {
        self.isolated.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn verified(&self) -> std::sync::MutexGuard<'_, HashSet<(String, String)>> {
        self.verified_attempts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn reconfirmed(&self) -> std::sync::MutexGuard<'_, HashSet<(String, String)>> {
        self.reconfirmed_candidates
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether this replica currently isolates `key`.
    pub fn is_isolated(&self, key: &ArtifactCacheKey) -> bool {
        self.isolated().contains_key(key)
    }

    fn identity(&self) -> VerifierIdentity {
        VerifierIdentity {
            verifier_id: self.verifier_id.clone(),
            run_id: format!("run-{}", uuid::Uuid::new_v4().simple()),
        }
    }
}

/// A tenant-scoped handle: the process guard plus the tenant's ledger.
#[derive(Debug, Clone)]
pub struct DamageContext {
    pub guard: Arc<DamageGuard>,
    pub ledger: ArtifactDamageLedger,
}

impl DamageContext {
    /// Admission: read the pin, refuse a blocked one, and release a local
    /// isolation that a verified clearance has since superseded.
    pub async fn admit(
        &self,
        index_version_id: &str,
        artifact_id: &str,
        key: &ArtifactCacheKey,
        cache: &L1Cache,
        l0: &L0Cache,
    ) -> Result<PinAdmission> {
        let admission = self.ledger.admit(index_version_id, artifact_id).await?;
        if admission.blocked_by.is_none() {
            let mut isolated = self.guard.isolated();
            if let Some(at) = isolated.get_mut(key) {
                if release_after_clearance(at, admission.generation) {
                    // The pin was blocked and then cleared after this replica
                    // isolated its copy. Discard the quarantined copy so the
                    // next hydration re-fetches and re-verifies it against
                    // the trusted pin; never trust the old local bytes.
                    isolated.remove(key);
                    cache.clear_quarantine(key);
                    let _ = cache.evict(key);
                    l0.remove(key);
                }
            }
        }
        Ok(admission)
    }

    /// The first local integrity failure for `key`: isolate the copy,
    /// record the isolation and verify against the authoritative store.
    ///
    /// Runs once per key per isolation. Concurrent refusals for a key that is
    /// already isolated do not re-verify, so one replica produces one
    /// observation however many readers it was serving.
    #[allow(clippy::too_many_arguments)]
    pub async fn on_integrity_failure(
        &self,
        index_version_id: &str,
        artifact_id: &str,
        key: &ArtifactCacheKey,
        site: FailureSite,
        detail: &str,
        cache: &Arc<L1Cache>,
        l0: &L0Cache,
        store: Arc<dyn ArtifactStore>,
        reader: &ReaderCapabilities,
        limits: &Limits,
    ) -> Option<RecordResult> {
        // A pin that cannot be read leaves the generation unknown, never 0:
        // a 0 would let any later generation release the isolation without
        // a clearance that followed it.
        let generation = match self.ledger.admit(index_version_id, artifact_id).await {
            Ok(a) => Some(a.generation),
            Err(e) => {
                tracing::warn!(error = %e,
                    "reading the pin while isolating an artifact failed; the isolation waits for a clearance after the next readable generation");
                None
            }
        };
        {
            let mut isolated = self.guard.isolated();
            if isolated.contains_key(key) {
                return None;
            }
            isolated.insert(key.clone(), generation);
        }
        l0.remove(key);
        if site == FailureSite::Open {
            // The hydrated copy is damaged locally. Evict it now; the
            // authoritative fetch below says whether the source is too.
            let _ = cache.evict(key);
        }
        let identity = self.guard.identity();
        if let Err(e) = self
            .ledger
            .record_local_isolation(index_version_id, artifact_id, &identity, Some(detail))
            .await
        {
            tracing::warn!(error = %e, "recording a local artifact isolation failed");
        }
        let outcome = self
            .verify(store, index_version_id, artifact_id, reader, limits, None)
            .await;
        if site == FailureSite::Open {
            // Local damage is resolved by eviction; only a hydration failure
            // leaves a quarantine for a clearance to release.
            self.guard.isolated().remove(key);
        }
        match outcome {
            Ok(r) => {
                tracing::warn!(decision = ?r.decision, history_full = r.history_full,
                    "artifact integrity failure verified against the authoritative store");
                Some(r)
            }
            Err(e) => {
                tracing::warn!(error = %e, "authoritative artifact verification failed");
                None
            }
        }
    }

    /// One verifier run: fetch the pinned artifact directly from `store`,
    /// compare it with the pin, and record the result.
    pub async fn verify(
        &self,
        store: Arc<dyn ArtifactStore>,
        index_version_id: &str,
        artifact_id: &str,
        reader: &ReaderCapabilities,
        limits: &Limits,
        attempt: Option<&RepairAttempt>,
    ) -> Result<RecordResult> {
        // The generation the observation is signed at. If a block or a
        // clearance commits before it is recorded, the ledger refuses it as
        // signed for another generation: it counts for nothing.
        let generation = self
            .ledger
            .admit(index_version_id, artifact_id)
            .await?
            .generation;
        let fetched_at = self.ledger.db_now().await?;
        let id = artifact_id.to_string();
        let reader = reader.clone();
        let limits = *limits;
        let audit = tokio::task::spawn_blocking(move || {
            audit_artifact(store.as_ref(), &id, &reader, &limits)
        })
        .await
        .map_err(|e| KernelError::Storage(format!("verification task: {e}")))?;
        let mut observation = observation_from(
            audit.as_ref().map_err(|e| e.to_string()),
            index_version_id,
            artifact_id,
            attempt,
            self.guard.identity(),
            fetched_at,
        );
        if let Some(key) = &self.guard.signing_key {
            observation.signature = Some(ObservationSignature {
                pin_generation: generation,
                signature: key.sign(&observation.claims(self.ledger.tenant_id(), generation)),
            });
        }
        self.ledger
            .record_authoritative(&observation, self.guard.authority())
            .await
    }
}

/// Turn an audit into the one observation it supports.
///
/// Without a repair attempt: the first mismatch in manifest order, else the
/// first unreadable object, else a whole-artifact match on the manifest. With
/// one: any mismatch (which invalidates the attempt), else any unreadable
/// object, else a match on the incident's object, recorded only when every
/// pinned object matched.
fn observation_from(
    audit: std::result::Result<&ArtifactAudit, String>,
    index_version_id: &str,
    artifact_id: &str,
    attempt: Option<&RepairAttempt>,
    verifier: VerifierIdentity,
    fetched_at: Timestamp,
) -> AuthoritativeObservation {
    // The storage generation is the one the audit read the object's bytes
    // at, so a block or a clearance binds to that stored object. `None`
    // where the store reports no generation, or nothing was read.
    let pin = |key: &str, expected: &str, generation: Option<&String>| ObjectPin {
        index_version_id: index_version_id.to_string(),
        artifact_id: artifact_id.to_string(),
        object_key: key.to_string(),
        expected_sha256: expected.to_string(),
        storage_generation: generation.cloned(),
    };
    let (target_key, target_expected) = match attempt {
        Some(a) => (a.object_key.clone(), a.expected_sha256.clone()),
        None => (MANIFEST_OBJECT.to_string(), artifact_id.to_string()),
    };
    let (object, outcome, observed, detail) = match audit {
        Err(e) => (
            pin(&target_key, &target_expected, None),
            Outcome::Unavailable,
            None,
            format!("pin could not be interpreted: {e}"),
        ),
        Ok(audit) => {
            if let Some(bad) = audit.first_mismatch() {
                (
                    pin(
                        &bad.object_key,
                        &bad.expected_sha256,
                        bad.storage_generation.as_ref(),
                    ),
                    Outcome::Mismatch,
                    bad.observed_sha256.clone(),
                    "authoritative digest differs from the pin".to_string(),
                )
            } else if let Some(gone) = audit.first_unavailable() {
                (
                    pin(&gone.object_key, &gone.expected_sha256, None),
                    Outcome::Unavailable,
                    None,
                    "pinned object could not be read".to_string(),
                )
            } else {
                match audit
                    .object(&target_key)
                    .filter(|o| o.outcome == AuditOutcome::Match)
                {
                    Some(o) => (
                        pin(
                            &o.object_key,
                            &o.expected_sha256,
                            o.storage_generation.as_ref(),
                        ),
                        Outcome::Match,
                        o.observed_sha256.clone(),
                        "every pinned object matched".to_string(),
                    ),
                    None => (
                        pin(&target_key, &target_expected, None),
                        Outcome::Unavailable,
                        None,
                        "the incident's object is not in the pinned manifest".to_string(),
                    ),
                }
            }
        }
    };
    AuthoritativeObservation {
        pin: object,
        verifier,
        fetch_id: format!("fetch-{}", uuid::Uuid::new_v4().simple()),
        fetched_at,
        outcome,
        observed_sha256: observed,
        repair_attempt_id: attempt.map(|a| a.attempt_id.clone()),
        detail: Some(detail),
        signature: None,
    }
}

/// Verify every preauthorized repair attempt this replica has not verified
/// yet. Returns how many verifications it recorded.
///
/// A verifier counts once per attempt, so a replica verifies an attempt once
/// and then waits for an independent replica; repeating would only add
/// `SameVerifier` entries to the bounded history.
pub async fn verify_repairs(
    guard: &Arc<DamageGuard>,
    pool: &PgPool,
    stores: &dyn crate::mirror::ArtifactStoreFactory,
    reader: &ReaderCapabilities,
    limits: &Limits,
) -> Result<usize> {
    if !guard.active() {
        return Ok(0);
    }
    let single = guard.single_replica_interval_secs();
    let mut done = 0usize;
    for (tenant, attempt) in munarium_store_pg::artifact_damage::verifying_attempts(pool).await? {
        let marker = (tenant.clone(), attempt.attempt_id.clone());
        if guard.verified().contains(&marker) {
            continue;
        }
        if attempt.first_match_verifier_id.as_deref() == Some(guard.verifier_id.as_str()) {
            // This replica holds the first match. With two independent
            // verifiers it waits for another one; as a declared single
            // replica it verifies once more after the interval.
            let due = match (single, attempt.first_match_fetched_at) {
                (Some(secs), Some(first)) => {
                    let now = ArtifactDamageLedger::new(pool.clone(), &tenant)
                        .db_now()
                        .await?;
                    (now - first).num_seconds() >= secs
                }
                _ => false,
            };
            if !due {
                continue;
            }
        }
        // One attempt's failure must not stop the others: each is verified
        // or retried on its own, and an unverified attempt leaves its block
        // in force.
        let recorded =
            match verify_one(guard, pool, stores, reader, limits, &tenant, &attempt).await {
                Ok(Some(r)) => r,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(error = %e, "repair verification could not run");
                    continue;
                }
            };
        tracing::info!(decision = ?recorded.decision, "repair verification recorded");
        // A single replica's first match is followed by its own spaced
        // second verification, so it does not finish the attempt here.
        let again = matches!(
            recorded.decision,
            Decision::Recorded | Decision::Rejected(_) | Decision::IntervalNotElapsed
        ) || (single.is_some() && recorded.decision == Decision::RepairMatch);
        if !again {
            guard.verified().insert(marker);
        }
        done += 1;
    }
    Ok(done)
}

/// As a declared single replica, re-verify each block candidate whose first
/// fetch is at least the interval old, once per candidate. The new run and
/// fetch supply the spaced second observation the ledger needs; whether it
/// blocks is still the ledger's decision. Returns how many re-verifications
/// it recorded. Does nothing unless the deployment is declared single-replica.
pub async fn confirm_single_replica_candidates(
    guard: &Arc<DamageGuard>,
    pool: &PgPool,
    stores: &dyn crate::mirror::ArtifactStoreFactory,
    reader: &ReaderCapabilities,
    limits: &Limits,
) -> Result<usize> {
    let Some(interval) = guard.single_replica_interval_secs() else {
        return Ok(0);
    };
    if !guard.active() {
        return Ok(0);
    }
    let mut done = 0usize;
    for due in
        munarium_store_pg::artifact_damage::single_replica_candidates_due(pool, interval).await?
    {
        let marker = (due.tenant_id.clone(), due.fetch_id.clone());
        if guard.reconfirmed().contains(&marker) {
            continue;
        }
        let catalog =
            munarium_store_pg::artifacts::ArtifactCatalog::new(pool.clone(), &due.tenant_id);
        let row = match catalog
            .artifact(&due.index_version_id, &due.artifact_id)
            .await
        {
            Ok(Some(row)) => row,
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!(error = %e, "single-replica confirmation could not run");
                continue;
            }
        };
        let store = match stores.store_for_prefix(&row.artifact_uri) {
            Ok(store) => store,
            Err(e) => {
                tracing::warn!(error = %e, "single-replica confirmation could not run");
                continue;
            }
        };
        let ctx = DamageContext {
            guard: Arc::clone(guard),
            ledger: guard.ledger(pool, &due.tenant_id),
        };
        match ctx
            .verify(
                store,
                &due.index_version_id,
                &due.artifact_id,
                reader,
                limits,
                None,
            )
            .await
        {
            Ok(r) => {
                tracing::warn!(decision = ?r.decision, history_full = r.history_full,
                    "single-replica confirmation verified against the authoritative store");
                if r.decision != Decision::IntervalNotElapsed {
                    guard.reconfirmed().insert(marker);
                }
                done += 1;
            }
            Err(e) => tracing::warn!(error = %e, "single-replica confirmation failed"),
        }
    }
    Ok(done)
}

async fn verify_one(
    guard: &Arc<DamageGuard>,
    pool: &PgPool,
    stores: &dyn crate::mirror::ArtifactStoreFactory,
    reader: &ReaderCapabilities,
    limits: &Limits,
    tenant: &str,
    attempt: &RepairAttempt,
) -> Result<Option<RecordResult>> {
    let catalog = munarium_store_pg::artifacts::ArtifactCatalog::new(pool.clone(), tenant);
    let Some(row) = catalog
        .artifact(&attempt.index_version_id, &attempt.artifact_id)
        .await?
    else {
        return Ok(None);
    };
    let store = stores.store_for_prefix(&row.artifact_uri)?;
    let ctx = DamageContext {
        guard: Arc::clone(guard),
        ledger: guard.ledger(pool, tenant),
    };
    ctx.verify(
        store,
        &attempt.index_version_id,
        &attempt.artifact_id,
        reader,
        limits,
        Some(attempt),
    )
    .await
    .map(Some)
}

#[cfg(test)]
mod replica_assurance_tests {
    use super::*;

    #[test]
    fn the_default_is_the_two_verifier_rule() {
        assert_eq!(parse_replica_assurance("", "", 1).unwrap(), None);
        assert_eq!(parse_replica_assurance("multi", "", 3).unwrap(), None);
        // The interval means nothing without `single`.
        assert_eq!(parse_replica_assurance("multi", "10", 1).unwrap(), None);
        let guard = DamageGuard::new(DamageMode::Enforce, "replica-a");
        assert_eq!(guard.single_replica_interval_secs(), None);
    }

    #[test]
    fn single_takes_a_conservative_default_interval() {
        assert_eq!(
            parse_replica_assurance("single", "", 1).unwrap(),
            Some(DEFAULT_SINGLE_REPLICA_INTERVAL_SECS)
        );
        assert_eq!(DEFAULT_SINGLE_REPLICA_INTERVAL_SECS, 300);
        assert_eq!(
            parse_replica_assurance(" single ", "900", 1).unwrap(),
            Some(900)
        );
    }

    #[test]
    fn single_is_refused_with_more_than_one_declared_replica() {
        for count in [2, 3, 10] {
            let err = parse_replica_assurance("single", "", count).unwrap_err();
            assert!(err.to_string().contains("MUNARIUM_REPLICA_COUNT"), "{err}");
        }
    }

    #[test]
    fn unknown_values_and_out_of_range_intervals_are_refused() {
        for (replicas, interval) in [
            ("Single", ""),
            ("one", ""),
            ("single", "59"),
            ("single", "0"),
            ("single", "-300"),
            ("single", "3601"),
            ("single", "5m"),
        ] {
            assert!(
                parse_replica_assurance(replicas, interval, 1).is_err(),
                "{replicas:?} {interval:?}"
            );
        }
        assert!(DamageGuard::new(DamageMode::Enforce, "r")
            .with_single_replica(30)
            .is_err());
    }
}

#[cfg(test)]
mod isolation_release_tests {
    use super::release_after_clearance;

    #[test]
    fn a_known_generation_is_released_only_by_a_later_one() {
        let mut at = Some(2);
        assert!(!release_after_clearance(&mut at, 2));
        assert!(!release_after_clearance(&mut at, 1));
        assert!(release_after_clearance(&mut at, 4));
        assert_eq!(at, Some(2));
    }

    /// An isolation whose generation could not be read is never released by
    /// the first generation seen afterwards, however high: a clearance
    /// before the isolation must not release it. It is released only by a
    /// clearance after that first read.
    #[test]
    fn an_unknown_generation_waits_for_a_clearance_after_the_next_read() {
        let mut at = None;
        assert!(!release_after_clearance(&mut at, 6));
        assert_eq!(at, Some(6));
        assert!(!release_after_clearance(&mut at, 6));
        assert!(!release_after_clearance(&mut at, 0));
        assert!(release_after_clearance(&mut at, 8));
    }
}

#[cfg(test)]
mod storage_generation_tests {
    use super::*;
    use munarium_datastore::audit::ObjectAudit;

    fn object(key: &str, outcome: AuditOutcome, generation: Option<&str>) -> ObjectAudit {
        let expected = if key == MANIFEST_OBJECT {
            "a".repeat(64)
        } else {
            "c".repeat(64)
        };
        ObjectAudit {
            object_key: key.into(),
            expected_sha256: expected.clone(),
            expected_bytes: None,
            observed_sha256: match outcome {
                AuditOutcome::Match => Some(expected),
                AuditOutcome::Mismatch => Some("b".repeat(64)),
                AuditOutcome::Unavailable => None,
            },
            observed_bytes: None,
            storage_generation: generation.map(str::to_string),
            outcome,
        }
    }

    fn observe(objects: Vec<ObjectAudit>) -> ObjectPin {
        let audit = ArtifactAudit {
            artifact_id: "a".repeat(64),
            objects,
        };
        observation_from(
            Ok(&audit),
            "idx2-v",
            &"a".repeat(64),
            None,
            VerifierIdentity {
                verifier_id: "v".into(),
                run_id: "r".into(),
            },
            Timestamp::default(),
        )
        .pin
    }

    /// The observation names the generation the audit read its object at:
    /// the mismatching object's, or the matched target's. An unread object
    /// carries none.
    #[test]
    fn an_observation_carries_the_generation_of_the_object_it_names() {
        let mismatch = observe(vec![
            object(MANIFEST_OBJECT, AuditOutcome::Match, Some("etag:m")),
            object("records/chunks.bin", AuditOutcome::Mismatch, Some("etag:c")),
        ]);
        assert_eq!(mismatch.object_key, "records/chunks.bin");
        assert_eq!(mismatch.storage_generation.as_deref(), Some("etag:c"));

        let clean = observe(vec![
            object(MANIFEST_OBJECT, AuditOutcome::Match, Some("etag:m")),
            object("records/chunks.bin", AuditOutcome::Match, Some("etag:c")),
        ]);
        assert_eq!(clean.object_key, MANIFEST_OBJECT);
        assert_eq!(clean.storage_generation.as_deref(), Some("etag:m"));

        let unread = observe(vec![
            object(MANIFEST_OBJECT, AuditOutcome::Match, Some("etag:m")),
            object("records/chunks.bin", AuditOutcome::Unavailable, None),
        ]);
        assert_eq!(unread.storage_generation, None);

        // A store without generations: absent, as before.
        let absent = observe(vec![object(
            "records/chunks.bin",
            AuditOutcome::Mismatch,
            None,
        )]);
        assert_eq!(absent.storage_generation, None);
    }
}
