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
//!   with the pin, and records what it saw under its own verifier identity.
//!   Whether that confirms a fleet block is the ledger's decision.
//! - **Admission and release.** A read checks the pin at admission and must
//!   pass the release fence before its result leaves the serving plane.
//! - **Repair verification.** A replica verifies each preauthorized repair
//!   attempt once; two independent matches let the ledger clear the block.
//!
//! Everything here is opt-in through [`DamageMode`]. In `off`, the default,
//! no executor carries a [`DamageContext`] and the v1.4 path runs unchanged.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use munarium_core::{KernelError, Result};
use munarium_datastore::audit::{audit_artifact, ArtifactAudit, AuditOutcome};
use munarium_datastore::hydrate::L1Cache;
use munarium_datastore::store::ArtifactStore;
use munarium_datastore::verify::{Limits, ReaderCapabilities};
use munarium_datastore::ArtifactCacheKey;
use munarium_store_pg::artifact_damage::{
    ArtifactDamageLedger, AuthoritativeObservation, Authority, Decision, ObjectPin, Outcome,
    PgPool, PinAdmission, RecordResult, RepairAttempt, Timestamp, VerifierIdentity,
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
    /// This replica's verifier identity. Must be unique per replica; see the
    /// specification's independence criteria.
    pub verifier_id: String,
    /// Locally isolated keys and the pin generation at isolation. A key is
    /// released only after the pin's generation advances unblocked, i.e. a
    /// block followed by a verified clearance.
    isolated: Mutex<HashMap<ArtifactCacheKey, i64>>,
    /// `(tenant, attempt)` pairs this replica has already verified.
    verified_attempts: Mutex<HashSet<(String, String)>>,
}

impl DamageGuard {
    pub fn new(mode: DamageMode, verifier_id: impl Into<String>) -> Self {
        Self {
            mode,
            verifier_id: verifier_id.into(),
            isolated: Mutex::new(HashMap::new()),
            verified_attempts: Mutex::new(HashSet::new()),
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

    fn isolated(&self) -> std::sync::MutexGuard<'_, HashMap<ArtifactCacheKey, i64>> {
        self.isolated.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn verified(&self) -> std::sync::MutexGuard<'_, HashSet<(String, String)>> {
        self.verified_attempts
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
            if let Some(at) = isolated.get(key).copied() {
                if admission.generation > at {
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
        let generation = self
            .ledger
            .admit(index_version_id, artifact_id)
            .await
            .map(|a| a.generation)
            .unwrap_or(0);
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
        let fetched_at = self.ledger.db_now().await?;
        let id = artifact_id.to_string();
        let reader = reader.clone();
        let limits = *limits;
        let audit = tokio::task::spawn_blocking(move || {
            audit_artifact(store.as_ref(), &id, &reader, &limits)
        })
        .await
        .map_err(|e| KernelError::Storage(format!("verification task: {e}")))?;
        let observation = observation_from(
            audit.as_ref().map_err(|e| e.to_string()),
            index_version_id,
            artifact_id,
            attempt,
            self.guard.identity(),
            fetched_at,
        );
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
    let pin = |key: &str, expected: &str| ObjectPin {
        index_version_id: index_version_id.to_string(),
        artifact_id: artifact_id.to_string(),
        object_key: key.to_string(),
        expected_sha256: expected.to_string(),
        storage_generation: None,
    };
    let (target_key, target_expected) = match attempt {
        Some(a) => (a.object_key.clone(), a.expected_sha256.clone()),
        None => (MANIFEST_OBJECT.to_string(), artifact_id.to_string()),
    };
    let (object, outcome, observed, detail) = match audit {
        Err(e) => (
            pin(&target_key, &target_expected),
            Outcome::Unavailable,
            None,
            format!("pin could not be interpreted: {e}"),
        ),
        Ok(audit) => {
            if let Some(bad) = audit.first_mismatch() {
                (
                    pin(&bad.object_key, &bad.expected_sha256),
                    Outcome::Mismatch,
                    bad.observed_sha256.clone(),
                    "authoritative digest differs from the pin".to_string(),
                )
            } else if let Some(gone) = audit.first_unavailable() {
                (
                    pin(&gone.object_key, &gone.expected_sha256),
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
                        pin(&o.object_key, &o.expected_sha256),
                        Outcome::Match,
                        o.observed_sha256.clone(),
                        "every pinned object matched".to_string(),
                    ),
                    None => (
                        pin(&target_key, &target_expected),
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
    let mut done = 0usize;
    for (tenant, attempt) in munarium_store_pg::artifact_damage::verifying_attempts(pool).await? {
        let marker = (tenant.clone(), attempt.attempt_id.clone());
        if guard.verified().contains(&marker)
            || attempt.first_match_verifier_id.as_deref() == Some(guard.verifier_id.as_str())
        {
            continue;
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
        if !matches!(
            recorded.decision,
            Decision::Recorded | Decision::Rejected(_)
        ) {
            guard.verified().insert(marker);
        }
        done += 1;
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
        ledger: ArtifactDamageLedger::new(pool.clone(), tenant),
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
