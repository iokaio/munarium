// SPDX-License-Identifier: Apache-2.0
//! Durable artifact damage: bounded findings, fleet blocks, the release fence
//! and verified repair clearance (ClearView R10, ART1, ART2).
//!
//! The rules are specified in `server/docs/design/artifact-damage.md`. This
//! module is where they are enforced, inside single PostgreSQL transactions:
//!
//! - **Authenticated verifiers (ADR 0028 amendment).** An authoritative
//!   observation counts toward a block or a clearance only when it is signed
//!   by an enrolled, unrevoked verifier key and its `verifier_id` is that
//!   key's fingerprint ([`munarium_core::artifact_verifier`]). Anything else
//!   is recorded as [`Decision::Unauthenticated`] and counts for nothing. A
//!   candidate or first repair match whose key has since been revoked is
//!   replaced, never confirmed. Both keys of a pair are read under a share
//!   lock, so a revocation that commits before the decision is honoured and
//!   one that commits after it is ordered after it.
//! - **Independence (ART1).** A fleet block commits only when a second
//!   authoritative mismatch on the *same pinned object* (artifact, object key,
//!   expected digest, storage generation) at the pin's *current generation*
//!   comes from a *different verifier identity* with a *different fetch id*,
//!   inside the confirmation window. The same verifier running twice, the same
//!   fetch replayed under another name, a timeout, or a different pin is
//!   recorded and never counted.
//! - **Clearance (ART2).** Only a preauthorized repair attempt, scoped to the
//!   pin's active incident, can clear it, and only after two matching
//!   observations of the incident's object by different verifiers, each
//!   fetched after the attempt was authorized. A mismatch during verification
//!   invalidates the attempt. Clearance, its evidence and the generation
//!   change are one transaction.
//! - **Single-replica assurance (amendment, 20 November 2026).** A ledger
//!   built with [`ArtifactDamageLedger::with_single_replica`] also accepts the
//!   *same* verifier identity as confirmation, but only from a different run
//!   and a different fetch whose fetch time is at least the configured
//!   interval after the first, for a block and for a clearance alike. Such a
//!   block or clearance is recorded as [`Assurance::SingleReplica`]; every
//!   other one as [`Assurance::Independent`]. Without that call the rules
//!   above are unchanged.
//! - **Release fence.** A reader records the pin generation at admission and
//!   releases its result only if, under a share lock, the pin is still at that
//!   generation and unblocked. A block that commits first invalidates every
//!   read admitted earlier, even if a clearance follows.
//! - **Bounds (R10).** The observation history holds at most
//!   [`DEFAULT_HISTORY_CAP`] rows per tenant. When full it refuses new entries
//!   rather than evicting old ones, and nothing that blocks or clears a pin
//!   depends on it: candidates, incidents and attempts carry their own
//!   evidence.
//!
//! Tenant-scoped by construction, like [`crate::artifacts::ArtifactCatalog`].
//! The one environment-scoped read, [`verifying_attempts`], is a free
//! function so it cannot be mistaken for a tenant handle's method.

use chrono::{DateTime, Utc};
use sqlx::{Postgres, Row, Transaction};

/// Re-exported so callers without their own SQL or clock dependency can name
/// the handle and the fetch time.
pub use sqlx::PgPool;
pub type Timestamp = DateTime<Utc>;

use munarium_core::artifact_verifier::{
    fingerprint_of, parse_public_key, verify_observation, ObservationClaims,
};
use munarium_core::{KernelError, Result};

use crate::storage_err;

/// The object key of an artifact's manifest. Its expected digest is the
/// artifact id itself.
pub const MANIFEST_OBJECT: &str = "manifest.json";
/// Observation history entries kept per tenant before new ones are refused.
pub const DEFAULT_HISTORY_CAP: i64 = 1024;
/// How far apart two mismatches' fetch times may be and still confirm.
pub const DEFAULT_CONFIRM_WINDOW_SECS: i64 = 24 * 60 * 60;

/// The default spacing between the two observations a single-replica
/// deployment needs. See [`ArtifactDamageLedger::with_single_replica`].
pub const DEFAULT_SINGLE_REPLICA_INTERVAL_SECS: i64 = 5 * 60;
/// The shortest single-replica spacing accepted. Migration 0082 enforces the
/// same floor on every `single_replica` incident row.
pub const MIN_SINGLE_REPLICA_INTERVAL_SECS: i64 = 60;
/// The longest single-replica spacing accepted: well inside the 24-hour
/// confirmation window, so a spaced confirmation can always still count.
pub const MAX_SINGLE_REPLICA_INTERVAL_SECS: i64 = 60 * 60;

const MAX_ID: usize = 128;
const MAX_KEY: usize = 512;
const MAX_DETAIL: usize = 256;

/// Who observed: an authenticated verifier identity and one run of it.
///
/// Independence is judged on `verifier_id` alone; `run_id` distinguishes runs
/// of the same verifier in the history and never makes them independent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifierIdentity {
    pub verifier_id: String,
    pub run_id: String,
}

/// The pinned object an observation is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectPin {
    pub index_version_id: String,
    pub artifact_id: String,
    /// `manifest.json` or a component path from the trusted manifest.
    pub object_key: String,
    /// The digest the trusted pin expects for that object.
    pub expected_sha256: String,
    /// The store's object generation (an ETag or version id) when it reports
    /// one. `None` compares equal only to `None`.
    pub storage_generation: Option<String>,
}

/// What an authoritative fetch showed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Match,
    Mismatch,
    /// The object could not be read in time. Never damage, never a match.
    Unavailable,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Match => "match",
            Self::Mismatch => "mismatch",
            Self::Unavailable => "unavailable",
        }
    }
}

/// A verifier's signature over one observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationSignature {
    /// The pin's integrity generation the verifier observed at. It must be
    /// the pin's generation when the ledger records the observation.
    pub pin_generation: i64,
    /// Unpadded base64url Ed25519 signature over
    /// [`AuthoritativeObservation::claims`].
    pub signature: String,
}

/// One verifier's direct fetch from the authoritative store.
#[derive(Debug, Clone)]
pub struct AuthoritativeObservation {
    pub pin: ObjectPin,
    pub verifier: VerifierIdentity,
    /// Unique per fetch. A fetch id already recorded never counts again.
    pub fetch_id: String,
    /// When the fetch started, from [`ArtifactDamageLedger::db_now`].
    pub fetched_at: DateTime<Utc>,
    pub outcome: Outcome,
    pub observed_sha256: Option<String>,
    /// Set when the observation verifies a repair attempt.
    pub repair_attempt_id: Option<String>,
    /// Bounded diagnostics. Never corpus or query text.
    pub detail: Option<String>,
    /// The verifier's signature. Without a valid one from an enrolled,
    /// unrevoked key the observation never counts.
    pub signature: Option<ObservationSignature>,
}

impl AuthoritativeObservation {
    /// What the verifier signs, for `tenant_id` at `pin_generation`.
    pub fn claims<'a>(&'a self, tenant_id: &'a str, pin_generation: i64) -> ObservationClaims<'a> {
        ObservationClaims {
            tenant_id,
            index_version_id: &self.pin.index_version_id,
            artifact_id: &self.pin.artifact_id,
            object_key: &self.pin.object_key,
            expected_sha256: &self.pin.expected_sha256,
            storage_generation: self.pin.storage_generation.as_deref().unwrap_or(""),
            pin_generation,
            outcome: self.outcome.as_str(),
            observed_sha256: self.observed_sha256.as_deref(),
            verifier_id: &self.verifier.verifier_id,
            run_id: &self.verifier.run_id,
            fetch_id: &self.fetch_id,
            fetched_at_micros: self.fetched_at.timestamp_micros(),
            repair_attempt_id: self.repair_attempt_id.as_deref(),
        }
    }
}

/// Which independence rule produced a block or a clearance. Recorded on the
/// incident (`block_assurance`, `clear_assurance`) so audits can tell them
/// apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assurance {
    /// Two different verifier identities: the default rule.
    Independent,
    /// One verifier identity in a deployment declared single-replica: two
    /// distinct runs and direct fetches, spaced by the configured interval.
    SingleReplica,
}

impl Assurance {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Independent => "independent",
            Self::SingleReplica => "single_replica",
        }
    }
}

/// What this process is allowed to commit. `observe` mode records and honors
/// blocks but commits neither a block nor a clearance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Authority {
    pub may_block: bool,
    pub may_clear: bool,
}

/// What the ledger decided for one observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Recorded without a state change.
    Recorded,
    /// The first unconfirmed mismatch on this object at this generation.
    Candidate,
    /// The verifier already holding the candidate (or first match) observed
    /// again. Counted once.
    SameVerifier,
    /// This fetch id was already recorded.
    ReplayedFetch,
    /// Two independent mismatches: the fleet block committed.
    Blocked {
        incident_id: String,
        generation: i64,
    },
    /// An independent confirmation exists, but this process may not block.
    BlockWithheld,
    /// Single-replica mode: the same verifier observed again from a new run
    /// and fetch, but sooner than the configured interval after its first
    /// observation (or with a fetch time in the future). Counted once.
    IntervalNotElapsed,
    /// The first fresh match for a repair attempt.
    RepairMatch,
    /// Two independent fresh matches: the block cleared.
    Cleared {
        incident_id: String,
        generation: i64,
    },
    /// A clearance would commit, but this process may not clear.
    ClearWithheld,
    /// A fresh mismatch invalidated the repair attempt.
    RepairInvalidated,
    /// The observation cannot bind to the pin or attempt it names.
    Rejected(String),
    /// Not signed by an enrolled, unrevoked verifier key for this tenant,
    /// pin and generation. Recorded, never counted.
    Unauthenticated(String),
}

impl Decision {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Recorded => "recorded",
            Self::Candidate => "candidate",
            Self::SameVerifier => "same_verifier",
            Self::ReplayedFetch => "replayed_fetch",
            Self::Blocked { .. } => "blocked",
            Self::BlockWithheld => "block_withheld",
            Self::IntervalNotElapsed => "interval_not_elapsed",
            Self::RepairMatch => "repair_match",
            Self::Cleared { .. } => "cleared",
            Self::ClearWithheld => "clear_withheld",
            Self::RepairInvalidated => "repair_invalidated",
            Self::Rejected(_) => "rejected",
            Self::Unauthenticated(_) => "unauthenticated",
        }
    }
}

/// A decision plus whether the observation reached the bounded history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordResult {
    pub decision: Decision,
    /// True when the history was full and this observation was not appended.
    /// The decision above was still made and committed.
    pub history_full: bool,
}

/// What a reader captured at admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinAdmission {
    pub index_version_id: String,
    pub artifact_id: String,
    pub generation: i64,
    /// The active incident when the pin is blocked.
    pub blocked_by: Option<String>,
}

/// The release fence's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseDecision {
    Authorized,
    Refused(String),
}

/// A pin's current integrity state, for operators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinState {
    pub index_version_id: String,
    pub artifact_id: String,
    pub generation: i64,
    pub state: String,
    pub active_incident_id: Option<String>,
}

/// One committed block and, when cleared, its clearance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incident {
    pub incident_id: String,
    pub index_version_id: String,
    pub artifact_id: String,
    pub object_key: String,
    pub expected_sha256: String,
    /// The stored generation both confirming mismatches observed; `None`
    /// when the store reported none.
    pub storage_generation: Option<String>,
    pub state: String,
    pub blocked_generation: i64,
    pub confirming_verifiers: [String; 2],
    /// `independent` or `single_replica`.
    pub block_assurance: String,
    pub cleared_generation: Option<i64>,
    pub clearing_attempt_id: Option<String>,
    pub clearing_workflow_id: Option<String>,
    pub clearing_verifiers: Option<[String; 2]>,
    /// `independent` or `single_replica` once cleared.
    pub clear_assurance: Option<String>,
    /// The stored generation both clearing matches observed, once cleared:
    /// the clearing attempt's recorded match generation. `None` when the
    /// incident is not cleared or the store reported no generation.
    pub clear_storage_generation: Option<String>,
}

/// A preauthorized repair attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairAttempt {
    pub attempt_id: String,
    pub incident_id: String,
    pub index_version_id: String,
    pub artifact_id: String,
    /// The incident's object, which the verifications must match.
    pub object_key: String,
    pub expected_sha256: String,
    pub workflow_id: String,
    pub authorized_by: String,
    pub state: String,
    pub first_match_verifier_id: Option<String>,
    /// When the first match's fetch started. A single-replica verifier waits
    /// the configured interval after it before verifying again.
    pub first_match_fetched_at: Option<Timestamp>,
}

fn bounded(label: &str, value: &str, max: usize) -> Result<()> {
    if value.is_empty() || value.len() > max || value.chars().any(char::is_control) {
        return Err(KernelError::InvalidInput(format!(
            "{label} must be 1..={max} bytes without control characters"
        )));
    }
    Ok(())
}

fn clip(detail: Option<&str>) -> Option<String> {
    detail.map(|d| {
        let mut out: String = d.chars().filter(|c| !c.is_control()).collect();
        while out.len() > MAX_DETAIL {
            out.pop();
        }
        out
    })
}

/// A stored generation column holds `''` for "the store reported none".
fn non_empty(generation: String) -> Option<String> {
    (!generation.is_empty()).then_some(generation)
}

fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn validate(o: &AuthoritativeObservation) -> Result<()> {
    bounded("index_version_id", &o.pin.index_version_id, MAX_KEY)?;
    bounded("artifact_id", &o.pin.artifact_id, MAX_KEY)?;
    bounded("object_key", &o.pin.object_key, MAX_KEY)?;
    bounded("verifier_id", &o.verifier.verifier_id, MAX_ID)?;
    bounded("verifier run_id", &o.verifier.run_id, MAX_ID)?;
    bounded("fetch_id", &o.fetch_id, MAX_ID)?;
    if let Some(g) = &o.pin.storage_generation {
        bounded("storage_generation", g, MAX_ID)?;
    }
    if let Some(a) = &o.repair_attempt_id {
        bounded("repair_attempt_id", a, MAX_ID)?;
    }
    if !is_sha256(&o.pin.expected_sha256) {
        return Err(KernelError::InvalidInput(
            "expected_sha256 must be a sha256 hex digest".into(),
        ));
    }
    match (o.outcome, o.observed_sha256.as_deref()) {
        (Outcome::Mismatch, Some(d)) if is_sha256(d) && d != o.pin.expected_sha256 => Ok(()),
        (Outcome::Match, Some(d)) if d == o.pin.expected_sha256 => Ok(()),
        (Outcome::Unavailable, None) => Ok(()),
        _ => Err(KernelError::InvalidInput(
            "a mismatch needs an observed digest that differs from the pin, a match one that \
             equals it, and an unavailable fetch none"
                .into(),
        )),
    }
}

/// Tenant-scoped access to the damage ledger.
#[derive(Debug, Clone)]
pub struct ArtifactDamageLedger {
    pool: PgPool,
    tenant_id: String,
    history_cap: i64,
    confirm_window_secs: i64,
    /// `Some(interval)` only in a deployment declared single-replica.
    single_replica_interval_secs: Option<i64>,
}

struct PinRow {
    generation: i64,
    state: String,
    active_incident_id: Option<String>,
}

impl ArtifactDamageLedger {
    pub fn new(pool: PgPool, tenant_id: impl Into<String>) -> Self {
        Self {
            pool,
            tenant_id: tenant_id.into(),
            history_cap: DEFAULT_HISTORY_CAP,
            confirm_window_secs: DEFAULT_CONFIRM_WINDOW_SECS,
            single_replica_interval_secs: None,
        }
    }

    /// Accept single-replica evidence (the 20 November 2026 amendment).
    ///
    /// Besides two different verifiers, a block or a clearance may then be
    /// confirmed by the *same* verifier identity from a different run and a
    /// different direct fetch, fetched at least `interval_secs` after the
    /// first observation and not in the future. Such a decision is recorded
    /// as [`Assurance::SingleReplica`]. Refuses an interval outside
    /// [`MIN_SINGLE_REPLICA_INTERVAL_SECS`]..=[`MAX_SINGLE_REPLICA_INTERVAL_SECS`].
    ///
    /// Call this only for a deployment that has exactly one serving replica:
    /// the caller checks the declared replica count.
    pub fn with_single_replica(mut self, interval_secs: i64) -> Result<Self> {
        check_single_replica_interval(interval_secs)?;
        self.single_replica_interval_secs = Some(interval_secs);
        Ok(self)
    }

    /// The single-replica interval, when this ledger accepts such evidence.
    pub fn single_replica_interval_secs(&self) -> Option<i64> {
        self.single_replica_interval_secs
    }

    /// Whether a same-verifier observation confirms under single-replica
    /// assurance: `None` when it does, else the decision that refuses it.
    /// `first_*` is the earlier observation: the block candidate or the
    /// repair attempt's first match.
    async fn single_replica_refusal(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        o: &AuthoritativeObservation,
        first_run_id: &str,
        first_fetched_at: DateTime<Utc>,
    ) -> Result<Option<Decision>> {
        let Some(interval) = self.single_replica_interval_secs else {
            return Ok(Some(Decision::SameVerifier));
        };
        if first_run_id == o.verifier.run_id {
            // The same run cannot be its own confirmation.
            return Ok(Some(Decision::SameVerifier));
        }
        let now: DateTime<Utc> = sqlx::query_scalar("SELECT now()")
            .fetch_one(&mut **tx)
            .await
            .map_err(storage_err)?;
        let spaced = (o.fetched_at - first_fetched_at).num_seconds() >= interval;
        if o.fetched_at > now || !spaced {
            return Ok(Some(Decision::IntervalNotElapsed));
        }
        Ok(None)
    }

    /// Override the history bound (tests and operator sizing).
    pub fn with_history_cap(mut self, cap: i64) -> Self {
        self.history_cap = cap.max(0);
        self
    }

    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }

    /// The database clock, so fetch times and repair windows share one clock
    /// across replicas.
    pub async fn db_now(&self) -> Result<DateTime<Utc>> {
        sqlx::query_scalar("SELECT now()")
            .fetch_one(&self.pool)
            .await
            .map_err(storage_err)
    }

    // -- admission and release ----------------------------------------------

    /// Read a pin's generation and block at admission. No row is generation
    /// 0, unblocked: absence of a finding is unknown integrity, not a pass,
    /// and admission does not claim otherwise.
    pub async fn admit(&self, index_version_id: &str, artifact_id: &str) -> Result<PinAdmission> {
        let row = sqlx::query(
            "SELECT generation, active_incident_id FROM artifact_integrity_pins
              WHERE tenant_id = $1 AND index_version_id = $2 AND artifact_id = $3",
        )
        .bind(&self.tenant_id)
        .bind(index_version_id)
        .bind(artifact_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(PinAdmission {
            index_version_id: index_version_id.to_string(),
            artifact_id: artifact_id.to_string(),
            generation: row.as_ref().map(|r| r.get("generation")).unwrap_or(0),
            blocked_by: row.and_then(|r| r.get("active_incident_id")),
        })
    }

    /// The release fence: authorize a completed result only if every
    /// contributing pin is still at its admitted generation and unblocked.
    ///
    /// The pin rows are share-locked in a stable order, so a block in the
    /// middle of committing is waited for and then seen. A missing row is
    /// generation 0; it cannot be blocked, because a block needs a candidate
    /// row first.
    pub async fn release(&self, admitted: &[PinAdmission]) -> Result<ReleaseDecision> {
        if admitted.is_empty() {
            return Ok(ReleaseDecision::Authorized);
        }
        let mut pins: Vec<&PinAdmission> = admitted.iter().collect();
        pins.sort_by(|a, b| {
            (&a.index_version_id, &a.artifact_id).cmp(&(&b.index_version_id, &b.artifact_id))
        });
        let versions: Vec<String> = pins.iter().map(|p| p.index_version_id.clone()).collect();
        let artifacts: Vec<String> = pins.iter().map(|p| p.artifact_id.clone()).collect();
        let mut tx = self.pool.begin().await.map_err(storage_err)?;
        let rows = sqlx::query(
            "SELECT p.index_version_id, p.artifact_id, p.generation, p.active_incident_id
               FROM artifact_integrity_pins p
               JOIN unnest($2::text[], $3::text[]) AS w(v, a)
                 ON p.index_version_id = w.v AND p.artifact_id = w.a
              WHERE p.tenant_id = $1
              ORDER BY p.index_version_id, p.artifact_id
              FOR SHARE OF p",
        )
        .bind(&self.tenant_id)
        .bind(&versions)
        .bind(&artifacts)
        .fetch_all(&mut *tx)
        .await
        .map_err(storage_err)?;
        tx.commit().await.map_err(storage_err)?;
        for pin in pins {
            let current = rows.iter().find(|r| {
                r.get::<String, _>("index_version_id") == pin.index_version_id
                    && r.get::<String, _>("artifact_id") == pin.artifact_id
            });
            let (generation, blocked): (i64, Option<String>) = match current {
                Some(r) => (r.get("generation"), r.get("active_incident_id")),
                None => (0, None),
            };
            if let Some(incident) = blocked {
                return Ok(ReleaseDecision::Refused(format!(
                    "artifact {} is blocked by integrity incident {incident}",
                    pin.artifact_id
                )));
            }
            if generation != pin.generation {
                return Ok(ReleaseDecision::Refused(format!(
                    "artifact {} changed integrity generation from {} to {generation} during \
                     the read",
                    pin.artifact_id, pin.generation
                )));
            }
        }
        Ok(ReleaseDecision::Authorized)
    }

    // -- recording -----------------------------------------------------------

    async fn lock_tenant(&self, tx: &mut Transaction<'_, Postgres>) -> Result<()> {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 60))")
            .bind(format!("munarium-artifact-damage:{}", self.tenant_id))
            .execute(&mut **tx)
            .await
            .map_err(storage_err)?;
        Ok(())
    }

    /// Ensure the pin row exists and lock it. Errors when the catalog does not
    /// hold the artifact for this tenant.
    async fn lock_pin(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        index_version_id: &str,
        artifact_id: &str,
    ) -> Result<(PinRow, serde_json::Value)> {
        let manifest: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT artifact_manifest FROM index_artifacts
              WHERE tenant_id = $1 AND index_version_id = $2 AND artifact_id = $3
              FOR SHARE",
        )
        .bind(&self.tenant_id)
        .bind(index_version_id)
        .bind(artifact_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(storage_err)?;
        let Some(manifest) = manifest else {
            return Err(KernelError::NotFound {
                kind: "artifact",
                id: artifact_id.to_string(),
            });
        };
        sqlx::query(
            "INSERT INTO artifact_integrity_pins (tenant_id, index_version_id, artifact_id)
             VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        )
        .bind(&self.tenant_id)
        .bind(index_version_id)
        .bind(artifact_id)
        .execute(&mut **tx)
        .await
        .map_err(storage_err)?;
        let row = sqlx::query(
            "SELECT generation, state, active_incident_id FROM artifact_integrity_pins
              WHERE tenant_id = $1 AND index_version_id = $2 AND artifact_id = $3
              FOR UPDATE",
        )
        .bind(&self.tenant_id)
        .bind(index_version_id)
        .bind(artifact_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(storage_err)?;
        Ok((
            PinRow {
                generation: row.get("generation"),
                state: row.get("state"),
                active_incident_id: row.get("active_incident_id"),
            },
            manifest,
        ))
    }

    /// Append to the bounded history, or report it full. Never evicts.
    #[allow(clippy::too_many_arguments)]
    async fn append_history(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        kind: &str,
        index_version_id: &str,
        artifact_id: &str,
        object: Option<(&str, &str, &str)>,
        outcome: &str,
        observed_sha256: Option<&str>,
        verifier: &VerifierIdentity,
        fetch: Option<(&str, DateTime<Utc>)>,
        pin_generation: i64,
        incident_id: Option<&str>,
        repair_attempt_id: Option<&str>,
        decision: &str,
        detail: Option<String>,
    ) -> Result<bool> {
        let held: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM artifact_integrity_observations WHERE tenant_id = $1",
        )
        .bind(&self.tenant_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(storage_err)?;
        if held >= self.history_cap {
            return Ok(true);
        }
        sqlx::query(
            "INSERT INTO artifact_integrity_observations
                 (tenant_id, observation_id, index_version_id, artifact_id, kind, object_key,
                  expected_sha256, storage_generation, outcome, observed_sha256, verifier_id,
                  verifier_run_id, fetch_id, fetched_at, pin_generation, incident_id,
                  repair_attempt_id, decision, detail)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19)",
        )
        .bind(&self.tenant_id)
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(index_version_id)
        .bind(artifact_id)
        .bind(kind)
        .bind(object.map(|o| o.0))
        .bind(object.map(|o| o.1))
        .bind(object.map(|o| o.2).unwrap_or(""))
        .bind(outcome)
        .bind(observed_sha256)
        .bind(&verifier.verifier_id)
        .bind(&verifier.run_id)
        .bind(fetch.map(|f| f.0))
        .bind(fetch.map(|f| f.1))
        .bind(pin_generation)
        .bind(incident_id)
        .bind(repair_attempt_id)
        .bind(decision)
        .bind(detail)
        .execute(&mut **tx)
        .await
        .map_err(storage_err)?;
        Ok(false)
    }

    /// Record that a replica isolated its own local copy (ART1's first
    /// step). Local evidence only: it never counts toward a fleet block.
    pub async fn record_local_isolation(
        &self,
        index_version_id: &str,
        artifact_id: &str,
        verifier: &VerifierIdentity,
        detail: Option<&str>,
    ) -> Result<RecordResult> {
        bounded("verifier_id", &verifier.verifier_id, MAX_ID)?;
        bounded("verifier run_id", &verifier.run_id, MAX_ID)?;
        let mut tx = self.pool.begin().await.map_err(storage_err)?;
        self.lock_tenant(&mut tx).await?;
        let (pin, _) = self
            .lock_pin(&mut tx, index_version_id, artifact_id)
            .await?;
        let history_full = self
            .append_history(
                &mut tx,
                "local_isolation",
                index_version_id,
                artifact_id,
                None,
                "isolated",
                None,
                verifier,
                None,
                pin.generation,
                pin.active_incident_id.as_deref(),
                None,
                Decision::Recorded.as_str(),
                clip(detail),
            )
            .await?;
        tx.commit().await.map_err(storage_err)?;
        Ok(RecordResult {
            decision: Decision::Recorded,
            history_full,
        })
    }

    /// Record one authoritative observation and apply ART1 or ART2.
    pub async fn record_authoritative(
        &self,
        o: &AuthoritativeObservation,
        authority: Authority,
    ) -> Result<RecordResult> {
        validate(o)?;
        let mut tx = self.pool.begin().await.map_err(storage_err)?;
        self.lock_tenant(&mut tx).await?;
        let (pin, manifest) = self
            .lock_pin(&mut tx, &o.pin.index_version_id, &o.pin.artifact_id)
            .await?;

        let replayed: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM artifact_integrity_observations
                            WHERE tenant_id = $1 AND fetch_id = $2)",
        )
        .bind(&self.tenant_id)
        .bind(&o.fetch_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage_err)?;

        let decision = if replayed {
            Decision::ReplayedFetch
        } else if let Some(reason) = self.unauthenticated(&mut tx, o, pin.generation).await? {
            Decision::Unauthenticated(reason)
        } else if let Some(reason) = untrusted_pin(&o.pin, &manifest) {
            Decision::Rejected(reason)
        } else if let Some(attempt) = o.repair_attempt_id.as_deref() {
            self.decide_repair(&mut tx, o, &pin, attempt, authority)
                .await?
        } else {
            self.decide_confirmation(&mut tx, o, &pin, authority)
                .await?
        };

        let generation_now = match &decision {
            Decision::Blocked { generation, .. } | Decision::Cleared { generation, .. } => {
                *generation
            }
            _ => pin.generation,
        };
        let incident = match &decision {
            Decision::Blocked { incident_id, .. } | Decision::Cleared { incident_id, .. } => {
                Some(incident_id.clone())
            }
            _ => pin.active_incident_id.clone(),
        };
        let storage_generation = o.pin.storage_generation.clone().unwrap_or_default();
        // A replayed fetch is not appended: the unique fetch index would
        // refuse it, and the original row already records that fetch.
        let history_full = if decision == Decision::ReplayedFetch {
            false
        } else {
            let detail = match &decision {
                Decision::Rejected(reason) | Decision::Unauthenticated(reason) => {
                    Some(reason.as_str())
                }
                _ => o.detail.as_deref(),
            };
            self.append_history(
                &mut tx,
                "authoritative",
                &o.pin.index_version_id,
                &o.pin.artifact_id,
                Some((
                    o.pin.object_key.as_str(),
                    o.pin.expected_sha256.as_str(),
                    storage_generation.as_str(),
                )),
                o.outcome.as_str(),
                o.observed_sha256.as_deref(),
                &o.verifier,
                Some((o.fetch_id.as_str(), o.fetched_at)),
                generation_now,
                incident.as_deref(),
                o.repair_attempt_id.as_deref(),
                decision.as_str(),
                clip(detail),
            )
            .await?
        };
        tx.commit().await.map_err(storage_err)?;
        Ok(RecordResult {
            decision,
            history_full,
        })
    }

    /// Why an observation is not authenticated, if it is not: it must carry
    /// a signature, at the pin's current generation, from the enrolled and
    /// unrevoked key whose fingerprint is its verifier id. The key row is
    /// share-locked, so a concurrent revocation is either seen or ordered
    /// after this decision.
    async fn unauthenticated(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        o: &AuthoritativeObservation,
        pin_generation: i64,
    ) -> Result<Option<String>> {
        let Some(signature) = &o.signature else {
            return Ok(Some("unsigned observation".into()));
        };
        let key = sqlx::query(
            "SELECT public_key, revoked_at IS NULL AS active FROM artifact_verifier_keys
              WHERE fingerprint = $1 FOR SHARE",
        )
        .bind(&o.verifier.verifier_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(storage_err)?;
        let Some(key) = key else {
            return Ok(Some("verifier key is not enrolled".into()));
        };
        if !key.get::<bool, _>("active") {
            return Ok(Some("verifier key is revoked".into()));
        }
        if signature.pin_generation != pin_generation {
            return Ok(Some(format!(
                "signed at integrity generation {}, the pin is at {pin_generation}",
                signature.pin_generation
            )));
        }
        let public: String = key.get("public_key");
        Ok(verify_observation(
            &public,
            &o.claims(&self.tenant_id, signature.pin_generation),
            &signature.signature,
        )
        .err()
        .map(str::to_string))
    }

    /// Whether the enrolled key `fingerprint` is unrevoked, with its row
    /// share-locked until this transaction ends. Used for the stored first
    /// observation of a pair, exactly as [`Self::unauthenticated`] locks the
    /// current one: a revocation that commits first is seen (the lock waits
    /// for it and reads the committed row), and one that commits later is
    /// ordered after this decision. An unlocked read could see the key
    /// active, let the revocation commit, and then confirm on it anyway.
    async fn key_active(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        fingerprint: &str,
    ) -> Result<bool> {
        let active: Option<bool> = sqlx::query_scalar(
            "SELECT revoked_at IS NULL FROM artifact_verifier_keys
              WHERE fingerprint = $1 FOR SHARE",
        )
        .bind(fingerprint)
        .fetch_optional(&mut **tx)
        .await
        .map_err(storage_err)?;
        Ok(active.unwrap_or(false))
    }

    async fn decide_confirmation(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        o: &AuthoritativeObservation,
        pin: &PinRow,
        authority: Authority,
    ) -> Result<Decision> {
        if pin.state == "blocked" || o.outcome != Outcome::Mismatch {
            // A further mismatch on a blocked pin, a match, or an unavailable
            // fetch: evidence for the history, never a state change.
            return Ok(Decision::Recorded);
        }
        let storage_generation = o.pin.storage_generation.clone().unwrap_or_default();
        let observed = o.observed_sha256.clone().unwrap_or_default();
        let candidate = sqlx::query(
            "SELECT pin_generation, verifier_id, verifier_run_id, fetch_id, fetched_at,
                    observed_sha256,
                    abs(extract(epoch FROM (fetched_at - $7::timestamptz))) <= $8 AS fresh
               FROM artifact_integrity_candidates
              WHERE tenant_id = $1 AND index_version_id = $2 AND artifact_id = $3
                AND object_key = $4 AND expected_sha256 = $5 AND storage_generation = $6
              FOR UPDATE",
        )
        .bind(&self.tenant_id)
        .bind(&o.pin.index_version_id)
        .bind(&o.pin.artifact_id)
        .bind(&o.pin.object_key)
        .bind(&o.pin.expected_sha256)
        .bind(&storage_generation)
        .bind(o.fetched_at)
        .bind(self.confirm_window_secs as f64)
        .fetch_optional(&mut **tx)
        .await
        .map_err(storage_err)?;

        let candidate = match candidate {
            Some(c)
                if c.get::<i64, _>("pin_generation") == pin.generation
                    && c.get::<bool, _>("fresh") =>
            {
                Some(c)
            }
            _ => None,
        };
        let usable = match candidate {
            Some(c)
                if self
                    .key_active(tx, &c.get::<String, _>("verifier_id"))
                    .await? =>
            {
                Some(c)
            }
            _ => None,
        };
        let Some(first) = usable else {
            // No candidate, or one from an earlier generation, outside the
            // confirmation window or from a key revoked since: this
            // observation becomes the candidate.
            sqlx::query(
                "INSERT INTO artifact_integrity_candidates
                     (tenant_id, index_version_id, artifact_id, object_key, expected_sha256,
                      storage_generation, pin_generation, verifier_id, verifier_run_id,
                      fetch_id, fetched_at, observed_sha256)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)
                 ON CONFLICT (tenant_id, index_version_id, artifact_id, object_key,
                              expected_sha256, storage_generation)
                 DO UPDATE SET pin_generation = EXCLUDED.pin_generation,
                               verifier_id = EXCLUDED.verifier_id,
                               verifier_run_id = EXCLUDED.verifier_run_id,
                               fetch_id = EXCLUDED.fetch_id,
                               fetched_at = EXCLUDED.fetched_at,
                               observed_sha256 = EXCLUDED.observed_sha256,
                               recorded_at = now()",
            )
            .bind(&self.tenant_id)
            .bind(&o.pin.index_version_id)
            .bind(&o.pin.artifact_id)
            .bind(&o.pin.object_key)
            .bind(&o.pin.expected_sha256)
            .bind(&storage_generation)
            .bind(pin.generation)
            .bind(&o.verifier.verifier_id)
            .bind(&o.verifier.run_id)
            .bind(&o.fetch_id)
            .bind(o.fetched_at)
            .bind(&observed)
            .execute(&mut **tx)
            .await
            .map_err(storage_err)?;
            return Ok(Decision::Candidate);
        };

        // The independence rule. Both checks are needed: a different name on
        // the same fetch is a replay, and a fresh fetch by the same verifier
        // is a retry.
        if first.get::<String, _>("fetch_id") == o.fetch_id {
            return Ok(Decision::ReplayedFetch);
        }
        let assurance = if first.get::<String, _>("verifier_id") == o.verifier.verifier_id {
            if let Some(refused) = self
                .single_replica_refusal(
                    tx,
                    o,
                    &first.get::<String, _>("verifier_run_id"),
                    first.get::<DateTime<Utc>, _>("fetched_at"),
                )
                .await?
            {
                return Ok(refused);
            }
            Assurance::SingleReplica
        } else {
            Assurance::Independent
        };
        if !authority.may_block {
            return Ok(Decision::BlockWithheld);
        }

        let incident_id = format!("inc-{}", uuid::Uuid::new_v4().simple());
        let generation = pin.generation + 1;
        sqlx::query(
            "INSERT INTO artifact_integrity_incidents
                 (tenant_id, incident_id, index_version_id, artifact_id, object_key,
                  expected_sha256, storage_generation, state, blocked_generation,
                  first_verifier_id, first_run_id, first_fetch_id, first_fetched_at,
                  first_observed_sha256, second_verifier_id, second_run_id, second_fetch_id,
                  second_fetched_at, second_observed_sha256, block_assurance)
             VALUES ($1,$2,$3,$4,$5,$6,$7,'blocked',$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,
                     $19)",
        )
        .bind(&self.tenant_id)
        .bind(&incident_id)
        .bind(&o.pin.index_version_id)
        .bind(&o.pin.artifact_id)
        .bind(&o.pin.object_key)
        .bind(&o.pin.expected_sha256)
        .bind(&storage_generation)
        .bind(generation)
        .bind(first.get::<String, _>("verifier_id"))
        .bind(first.get::<String, _>("verifier_run_id"))
        .bind(first.get::<String, _>("fetch_id"))
        .bind(first.get::<DateTime<Utc>, _>("fetched_at"))
        .bind(first.get::<String, _>("observed_sha256"))
        .bind(&o.verifier.verifier_id)
        .bind(&o.verifier.run_id)
        .bind(&o.fetch_id)
        .bind(o.fetched_at)
        .bind(&observed)
        .bind(assurance.as_str())
        .execute(&mut **tx)
        .await
        .map_err(storage_err)?;
        self.move_pin(
            tx,
            &o.pin.index_version_id,
            &o.pin.artifact_id,
            generation,
            "blocked",
            Some(&incident_id),
        )
        .await?;
        Ok(Decision::Blocked {
            incident_id,
            generation,
        })
    }

    async fn decide_repair(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        o: &AuthoritativeObservation,
        pin: &PinRow,
        attempt_id: &str,
        authority: Authority,
    ) -> Result<Decision> {
        let attempt = sqlx::query(
            "SELECT a.incident_id, a.index_version_id, a.artifact_id, a.workflow_id, a.state,
                    a.first_match_verifier_id, a.first_match_run_id, a.first_match_fetch_id,
                    a.first_match_fetched_at, a.first_match_observed_sha256,
                    a.first_match_storage_generation,
                    $3::timestamptz >= a.authorized_at AS fresh,
                    i.object_key, i.expected_sha256
               FROM artifact_repair_attempts a
               JOIN artifact_integrity_incidents i
                 ON i.tenant_id = a.tenant_id AND i.incident_id = a.incident_id
              WHERE a.tenant_id = $1 AND a.attempt_id = $2
              FOR UPDATE OF a",
        )
        .bind(&self.tenant_id)
        .bind(attempt_id)
        .bind(o.fetched_at)
        .fetch_optional(&mut **tx)
        .await
        .map_err(storage_err)?;
        let Some(a) = attempt else {
            return Ok(Decision::Rejected(
                "no preauthorized repair attempt with that id".into(),
            ));
        };
        let state: String = a.get("state");
        if state != "verifying" {
            return Ok(Decision::Rejected(format!(
                "repair attempt is {state}, not verifying"
            )));
        }
        if a.get::<String, _>("index_version_id") != o.pin.index_version_id
            || a.get::<String, _>("artifact_id") != o.pin.artifact_id
        {
            return Ok(Decision::Rejected(
                "repair attempt is for a different pin".into(),
            ));
        }
        let incident_id: String = a.get("incident_id");
        if pin.active_incident_id.as_deref() != Some(incident_id.as_str()) {
            return Ok(Decision::Rejected(
                "repair attempt's incident is not the pin's active block".into(),
            ));
        }
        if !a.get::<bool, _>("fresh") {
            return Ok(Decision::Rejected(
                "fetched before the repair attempt was authorized".into(),
            ));
        }
        match o.outcome {
            Outcome::Unavailable => return Ok(Decision::Recorded),
            Outcome::Mismatch => {
                sqlx::query(
                    "UPDATE artifact_repair_attempts
                        SET state = 'invalidated', finished_at = now(),
                            finish_reason = 'a fresh mismatch during verification'
                      WHERE tenant_id = $1 AND attempt_id = $2",
                )
                .bind(&self.tenant_id)
                .bind(attempt_id)
                .execute(&mut **tx)
                .await
                .map_err(storage_err)?;
                return Ok(Decision::RepairInvalidated);
            }
            Outcome::Match => {}
        }
        if a.get::<String, _>("object_key") != o.pin.object_key
            || a.get::<String, _>("expected_sha256") != o.pin.expected_sha256
        {
            return Ok(Decision::Rejected(
                "a repair match must be on the incident's object and pin digest".into(),
            ));
        }
        let storage_generation = o.pin.storage_generation.clone().unwrap_or_default();
        let observed = o.observed_sha256.clone().unwrap_or_default();
        // A first match whose key was revoked since is replaced, never
        // confirmed. Its key row is share-locked like the current one.
        //
        // So is a first match of a different stored generation (the 20
        // November 2026 storage-generation decision): the two matches would
        // be of two different stored objects, and the earlier one says
        // nothing about the object now stored. The newer match becomes the
        // first, so two matches are only ever combined when both observed
        // the same generation. An absent generation (stored as '') is its
        // own value: it never combines with a known one.
        let same_generation = a
            .get::<Option<String>, _>("first_match_storage_generation")
            .unwrap_or_default()
            == storage_generation;
        let first_verifier = match a.get::<Option<String>, _>("first_match_verifier_id") {
            Some(v) if same_generation && self.key_active(tx, &v).await? => Some(v),
            _ => None,
        };
        let Some(first_verifier) = first_verifier else {
            sqlx::query(
                "UPDATE artifact_repair_attempts
                    SET first_match_verifier_id = $3, first_match_run_id = $4,
                        first_match_fetch_id = $5, first_match_fetched_at = $6,
                        first_match_observed_sha256 = $7, first_match_storage_generation = $8
                  WHERE tenant_id = $1 AND attempt_id = $2",
            )
            .bind(&self.tenant_id)
            .bind(attempt_id)
            .bind(&o.verifier.verifier_id)
            .bind(&o.verifier.run_id)
            .bind(&o.fetch_id)
            .bind(o.fetched_at)
            .bind(&observed)
            .bind(&storage_generation)
            .execute(&mut **tx)
            .await
            .map_err(storage_err)?;
            return Ok(Decision::RepairMatch);
        };
        if a.get::<Option<String>, _>("first_match_fetch_id")
            .as_deref()
            == Some(&o.fetch_id)
        {
            return Ok(Decision::ReplayedFetch);
        }
        let assurance = if first_verifier == o.verifier.verifier_id {
            let (Some(first_run), Some(first_fetched_at)) = (
                a.get::<Option<String>, _>("first_match_run_id"),
                a.get::<Option<DateTime<Utc>>, _>("first_match_fetched_at"),
            ) else {
                return Ok(Decision::SameVerifier);
            };
            if let Some(refused) = self
                .single_replica_refusal(tx, o, &first_run, first_fetched_at)
                .await?
            {
                return Ok(refused);
            }
            Assurance::SingleReplica
        } else {
            Assurance::Independent
        };
        if !authority.may_clear {
            return Ok(Decision::ClearWithheld);
        }

        let generation = pin.generation + 1;
        let workflow: String = a.get("workflow_id");
        sqlx::query(
            "UPDATE artifact_integrity_incidents
                SET state = 'cleared', cleared_generation = $3, cleared_at = now(),
                    clearing_attempt_id = $4, clearing_workflow_id = $5,
                    clear_first_verifier_id = $6, clear_first_run_id = $7,
                    clear_first_fetch_id = $8, clear_first_fetched_at = $9,
                    clear_first_observed_sha256 = $10,
                    clear_second_verifier_id = $11, clear_second_run_id = $12,
                    clear_second_fetch_id = $13, clear_second_fetched_at = $14,
                    clear_second_observed_sha256 = $15, clear_assurance = $16
              WHERE tenant_id = $1 AND incident_id = $2 AND state = 'blocked'",
        )
        .bind(&self.tenant_id)
        .bind(&incident_id)
        .bind(generation)
        .bind(attempt_id)
        .bind(&workflow)
        .bind(&first_verifier)
        .bind(a.get::<Option<String>, _>("first_match_run_id"))
        .bind(a.get::<Option<String>, _>("first_match_fetch_id"))
        .bind(a.get::<Option<DateTime<Utc>>, _>("first_match_fetched_at"))
        .bind(a.get::<Option<String>, _>("first_match_observed_sha256"))
        .bind(&o.verifier.verifier_id)
        .bind(&o.verifier.run_id)
        .bind(&o.fetch_id)
        .bind(o.fetched_at)
        .bind(&observed)
        .bind(assurance.as_str())
        .execute(&mut **tx)
        .await
        .map_err(storage_err)?;
        let finish_reason = match assurance {
            Assurance::Independent => "two independent fresh matches",
            Assurance::SingleReplica => "two spaced fresh matches from one replica",
        };
        sqlx::query(
            "UPDATE artifact_repair_attempts
                SET state = 'cleared', finished_at = now(), finish_reason = $3
              WHERE tenant_id = $1 AND attempt_id = $2",
        )
        .bind(&self.tenant_id)
        .bind(attempt_id)
        .bind(finish_reason)
        .execute(&mut **tx)
        .await
        .map_err(storage_err)?;
        self.move_pin(
            tx,
            &o.pin.index_version_id,
            &o.pin.artifact_id,
            generation,
            "cleared",
            None,
        )
        .await?;
        Ok(Decision::Cleared {
            incident_id,
            generation,
        })
    }

    /// Advance the pin's generation, set its state and drop every candidate
    /// recorded at an earlier generation.
    async fn move_pin(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        index_version_id: &str,
        artifact_id: &str,
        generation: i64,
        state: &str,
        incident: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE artifact_integrity_pins
                SET generation = $4, state = $5, active_incident_id = $6, updated_at = now()
              WHERE tenant_id = $1 AND index_version_id = $2 AND artifact_id = $3",
        )
        .bind(&self.tenant_id)
        .bind(index_version_id)
        .bind(artifact_id)
        .bind(generation)
        .bind(state)
        .bind(incident)
        .execute(&mut **tx)
        .await
        .map_err(storage_err)?;
        sqlx::query(
            "DELETE FROM artifact_integrity_candidates
              WHERE tenant_id = $1 AND index_version_id = $2 AND artifact_id = $3",
        )
        .bind(&self.tenant_id)
        .bind(index_version_id)
        .bind(artifact_id)
        .execute(&mut **tx)
        .await
        .map_err(storage_err)?;
        Ok(())
    }

    // -- repair authorization ------------------------------------------------

    /// Preauthorize a repair workflow for one active incident.
    ///
    /// Call after the separately authorized byte repair: the verification
    /// window opens now, and only fetches started from now on can count.
    /// Repeating the call with the same workflow returns the attempt already
    /// verifying; a different workflow supersedes it, voiding its matches.
    pub async fn authorize_repair(
        &self,
        incident_id: &str,
        workflow_id: &str,
        authorized_by: &str,
        reason: Option<&str>,
    ) -> Result<RepairAttempt> {
        bounded("incident_id", incident_id, MAX_ID)?;
        bounded("workflow_id", workflow_id, MAX_ID)?;
        bounded("authorized_by", authorized_by, MAX_ID)?;
        let mut tx = self.pool.begin().await.map_err(storage_err)?;
        self.lock_tenant(&mut tx).await?;
        let incident = sqlx::query(
            "SELECT index_version_id, artifact_id, state FROM artifact_integrity_incidents
              WHERE tenant_id = $1 AND incident_id = $2 FOR UPDATE",
        )
        .bind(&self.tenant_id)
        .bind(incident_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage_err)?
        .ok_or_else(|| KernelError::NotFound {
            kind: "integrity incident",
            id: incident_id.to_string(),
        })?;
        let state: String = incident.get("state");
        if state != "blocked" {
            return Err(KernelError::InvalidInput(format!(
                "incident {incident_id} is {state}; only an active block can be repaired"
            )));
        }
        let version: String = incident.get("index_version_id");
        let artifact: String = incident.get("artifact_id");
        let existing = sqlx::query(
            "SELECT attempt_id, workflow_id FROM artifact_repair_attempts
              WHERE tenant_id = $1 AND incident_id = $2 AND state = 'verifying'
              FOR UPDATE",
        )
        .bind(&self.tenant_id)
        .bind(incident_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage_err)?;
        if let Some(row) = existing {
            let attempt: String = row.get("attempt_id");
            if row.get::<String, _>("workflow_id") == workflow_id {
                tx.commit().await.map_err(storage_err)?;
                return self
                    .repair_attempt(&attempt)
                    .await?
                    .ok_or_else(|| KernelError::Storage("a verifying attempt vanished".into()));
            }
            sqlx::query(
                "UPDATE artifact_repair_attempts
                    SET state = 'superseded', finished_at = now(),
                        finish_reason = 'superseded by a newer authorization'
                  WHERE tenant_id = $1 AND attempt_id = $2",
            )
            .bind(&self.tenant_id)
            .bind(&attempt)
            .execute(&mut *tx)
            .await
            .map_err(storage_err)?;
        }
        let attempt_id = format!("rep-{}", uuid::Uuid::new_v4().simple());
        sqlx::query(
            "INSERT INTO artifact_repair_attempts
                 (tenant_id, attempt_id, incident_id, index_version_id, artifact_id,
                  workflow_id, authorized_by, reason)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
        )
        .bind(&self.tenant_id)
        .bind(&attempt_id)
        .bind(incident_id)
        .bind(&version)
        .bind(&artifact)
        .bind(workflow_id)
        .bind(authorized_by)
        .bind(clip(reason))
        .execute(&mut *tx)
        .await
        .map_err(storage_err)?;
        tx.commit().await.map_err(storage_err)?;
        self.repair_attempt(&attempt_id)
            .await?
            .ok_or_else(|| KernelError::Storage("a new repair attempt vanished".into()))
    }

    // -- reads ---------------------------------------------------------------

    pub async fn repair_attempt(&self, attempt_id: &str) -> Result<Option<RepairAttempt>> {
        let row = sqlx::query(
            "SELECT a.attempt_id, a.incident_id, a.index_version_id, a.artifact_id,
                    i.object_key, i.expected_sha256, a.workflow_id, a.authorized_by, a.state,
                    a.first_match_verifier_id, a.first_match_fetched_at
               FROM artifact_repair_attempts a
               JOIN artifact_integrity_incidents i
                 ON i.tenant_id = a.tenant_id AND i.incident_id = a.incident_id
              WHERE a.tenant_id = $1 AND a.attempt_id = $2",
        )
        .bind(&self.tenant_id)
        .bind(attempt_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(row.map(|r| to_attempt(&r)))
    }

    pub async fn pin_state(
        &self,
        index_version_id: &str,
        artifact_id: &str,
    ) -> Result<Option<PinState>> {
        let row = sqlx::query(
            "SELECT index_version_id, artifact_id, generation, state, active_incident_id
               FROM artifact_integrity_pins
              WHERE tenant_id = $1 AND index_version_id = $2 AND artifact_id = $3",
        )
        .bind(&self.tenant_id)
        .bind(index_version_id)
        .bind(artifact_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(row.map(|r| to_pin(&r)))
    }

    /// Every pin with a finding, blocked first.
    pub async fn pins(&self) -> Result<Vec<PinState>> {
        let rows = sqlx::query(
            "SELECT index_version_id, artifact_id, generation, state, active_incident_id
               FROM artifact_integrity_pins WHERE tenant_id = $1
              ORDER BY (state = 'blocked') DESC, updated_at DESC
              LIMIT 500",
        )
        .bind(&self.tenant_id)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(rows.iter().map(to_pin).collect())
    }

    /// Incidents, newest first.
    pub async fn incidents(&self) -> Result<Vec<Incident>> {
        let rows = sqlx::query(
            "SELECT i.incident_id, i.index_version_id, i.artifact_id, i.object_key,
                    i.expected_sha256, i.storage_generation, i.state, i.blocked_generation,
                    i.first_verifier_id, i.second_verifier_id, i.cleared_generation,
                    i.clearing_attempt_id, i.clearing_workflow_id, i.clear_first_verifier_id,
                    i.clear_second_verifier_id, i.block_assurance, i.clear_assurance,
                    a.first_match_storage_generation AS clear_storage_generation
               FROM artifact_integrity_incidents i
               LEFT JOIN artifact_repair_attempts a
                 ON a.tenant_id = i.tenant_id AND a.attempt_id = i.clearing_attempt_id
              WHERE i.tenant_id = $1
              ORDER BY i.blocked_at DESC, i.incident_id
              LIMIT 500",
        )
        .bind(&self.tenant_id)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(rows
            .iter()
            .map(|r| Incident {
                incident_id: r.get("incident_id"),
                index_version_id: r.get("index_version_id"),
                artifact_id: r.get("artifact_id"),
                object_key: r.get("object_key"),
                expected_sha256: r.get("expected_sha256"),
                storage_generation: non_empty(r.get("storage_generation")),
                state: r.get("state"),
                blocked_generation: r.get("blocked_generation"),
                confirming_verifiers: [r.get("first_verifier_id"), r.get("second_verifier_id")],
                block_assurance: r.get("block_assurance"),
                cleared_generation: r.get("cleared_generation"),
                clearing_attempt_id: r.get("clearing_attempt_id"),
                clearing_workflow_id: r.get("clearing_workflow_id"),
                clearing_verifiers: match (
                    r.get::<Option<String>, _>("clear_first_verifier_id"),
                    r.get::<Option<String>, _>("clear_second_verifier_id"),
                ) {
                    (Some(a), Some(b)) => Some([a, b]),
                    _ => None,
                },
                clear_assurance: r.get("clear_assurance"),
                clear_storage_generation: r
                    .get::<Option<String>, _>("clear_storage_generation")
                    .and_then(non_empty),
            })
            .collect())
    }

    /// How many observation history entries this tenant holds.
    pub async fn history_len(&self) -> Result<i64> {
        sqlx::query_scalar(
            "SELECT count(*) FROM artifact_integrity_observations WHERE tenant_id = $1",
        )
        .bind(&self.tenant_id)
        .fetch_one(&self.pool)
        .await
        .map_err(storage_err)
    }

    /// The most recent history entries: (verifier, outcome, decision).
    pub async fn history(&self, limit: i64) -> Result<Vec<(String, String, String)>> {
        let rows = sqlx::query(
            "SELECT verifier_id, outcome, decision FROM artifact_integrity_observations
              WHERE tenant_id = $1 ORDER BY recorded_at DESC, observation_id LIMIT $2",
        )
        .bind(&self.tenant_id)
        .bind(limit.clamp(1, 1000))
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(rows
            .iter()
            .map(|r| (r.get("verifier_id"), r.get("outcome"), r.get("decision")))
            .collect())
    }
}

/// Every repair attempt currently verifying, across tenants: `(tenant,
/// attempt)`. Environment-scoped like the serving warmer's selector read,
/// because a replica verifies for every tenant it serves.
pub async fn verifying_attempts(pool: &PgPool) -> Result<Vec<(String, RepairAttempt)>> {
    let rows = sqlx::query(
        "SELECT a.tenant_id, a.attempt_id, a.incident_id, a.index_version_id, a.artifact_id,
                i.object_key, i.expected_sha256, a.workflow_id, a.authorized_by, a.state,
                a.first_match_verifier_id, a.first_match_fetched_at
           FROM artifact_repair_attempts a
           JOIN artifact_integrity_incidents i
             ON i.tenant_id = a.tenant_id AND i.incident_id = a.incident_id
          WHERE a.state = 'verifying'
          ORDER BY a.authorized_at, a.attempt_id
          LIMIT 500",
    )
    .fetch_all(pool)
    .await
    .map_err(storage_err)?;
    Ok(rows
        .iter()
        .map(|r| (r.get("tenant_id"), to_attempt(r)))
        .collect())
}

/// An enrolled verifier key. Public material only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifierKeyRecord {
    /// The verifier identity: `ed25519:` and the SHA-256 of the public key.
    pub fingerprint: String,
    /// Unpadded base64url Ed25519 public key.
    pub public_key: String,
    pub label: String,
    pub enrolled_by: String,
    pub enrolled_at: Timestamp,
    pub revoked_at: Option<Timestamp>,
    pub revoked_by: Option<String>,
    pub revoke_reason: Option<String>,
}

fn to_key(r: &sqlx::postgres::PgRow) -> VerifierKeyRecord {
    VerifierKeyRecord {
        fingerprint: r.get("fingerprint"),
        public_key: r.get("public_key"),
        label: r.get("label"),
        enrolled_by: r.get("enrolled_by"),
        enrolled_at: r.get("enrolled_at"),
        revoked_at: r.get("revoked_at"),
        revoked_by: r.get("revoked_by"),
        revoke_reason: r.get("revoke_reason"),
    }
}

const KEY_COLUMNS: &str = "fingerprint, public_key, label, enrolled_by, enrolled_at, revoked_at, \
                           revoked_by, revoke_reason";

async fn verifier_key(pool: &PgPool, fingerprint: &str) -> Result<Option<VerifierKeyRecord>> {
    let row = sqlx::query(&format!(
        "SELECT {KEY_COLUMNS} FROM artifact_verifier_keys WHERE fingerprint = $1"
    ))
    .bind(fingerprint)
    .fetch_optional(pool)
    .await
    .map_err(storage_err)?;
    Ok(row.as_ref().map(to_key))
}

/// Enroll a verifier's public key (operator boundary: the deployment's
/// database credential, like `authorize-repair`). Enrolling the same key
/// again returns its record. A revoked key cannot be enrolled again: the
/// replica needs a new key.
pub async fn enroll_verifier(
    pool: &PgPool,
    public_key: &str,
    label: &str,
    enrolled_by: &str,
) -> Result<VerifierKeyRecord> {
    let key = parse_public_key(public_key)?;
    bounded("label", label, MAX_ID)?;
    bounded("enrolled_by", enrolled_by, MAX_ID)?;
    let fingerprint = fingerprint_of(&key);
    sqlx::query(
        "INSERT INTO artifact_verifier_keys (fingerprint, public_key, label, enrolled_by)
         VALUES ($1, $2, $3, $4) ON CONFLICT (fingerprint) DO NOTHING",
    )
    .bind(&fingerprint)
    .bind(public_key)
    .bind(label)
    .bind(enrolled_by)
    .execute(pool)
    .await
    .map_err(storage_err)?;
    let record = verifier_key(pool, &fingerprint)
        .await?
        .ok_or_else(|| KernelError::Storage("an enrolled verifier key vanished".into()))?;
    if record.revoked_at.is_some() {
        return Err(KernelError::InvalidInput(format!(
            "verifier key {fingerprint} was revoked and cannot be enrolled again; generate a \
             new key"
        )));
    }
    Ok(record)
}

/// Revoke an enrolled verifier key, permanently. From the moment it commits,
/// the key's observations count for nothing, and an unconfirmed candidate
/// or first repair match it made can no longer be confirmed. Committed
/// blocks and clearances keep their recorded evidence. Revoking an already
/// revoked key returns its record unchanged.
pub async fn revoke_verifier(
    pool: &PgPool,
    fingerprint: &str,
    revoked_by: &str,
    reason: Option<&str>,
) -> Result<VerifierKeyRecord> {
    bounded("fingerprint", fingerprint, MAX_ID)?;
    bounded("revoked_by", revoked_by, MAX_ID)?;
    sqlx::query(
        "UPDATE artifact_verifier_keys
            SET revoked_at = now(), revoked_by = $2, revoke_reason = $3
          WHERE fingerprint = $1 AND revoked_at IS NULL",
    )
    .bind(fingerprint)
    .bind(revoked_by)
    .bind(clip(reason))
    .execute(pool)
    .await
    .map_err(storage_err)?;
    verifier_key(pool, fingerprint)
        .await?
        .ok_or_else(|| KernelError::NotFound {
            kind: "verifier key",
            id: fingerprint.to_string(),
        })
}

/// Every enrolled verifier key, active first.
pub async fn verifier_keys(pool: &PgPool) -> Result<Vec<VerifierKeyRecord>> {
    let rows = sqlx::query(&format!(
        "SELECT {KEY_COLUMNS} FROM artifact_verifier_keys
          ORDER BY (revoked_at IS NULL) DESC, enrolled_at DESC, fingerprint LIMIT 500"
    ))
    .fetch_all(pool)
    .await
    .map_err(storage_err)?;
    Ok(rows.iter().map(to_key).collect())
}

/// Validate a single-replica interval.
pub fn check_single_replica_interval(interval_secs: i64) -> Result<()> {
    if !(MIN_SINGLE_REPLICA_INTERVAL_SECS..=MAX_SINGLE_REPLICA_INTERVAL_SECS)
        .contains(&interval_secs)
    {
        return Err(KernelError::InvalidInput(format!(
            "the single-replica interval must be {MIN_SINGLE_REPLICA_INTERVAL_SECS}..=\
             {MAX_SINGLE_REPLICA_INTERVAL_SECS} seconds, got {interval_secs}"
        )));
    }
    Ok(())
}

/// A block candidate a single-replica verifier should confirm by fetching
/// again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueCandidate {
    pub tenant_id: String,
    pub index_version_id: String,
    pub artifact_id: String,
    /// The candidate's own fetch, so a verifier re-verifies it once.
    pub fetch_id: String,
}

/// Unconfirmed candidates, across tenants, whose fetch is at least
/// `interval_secs` old and still inside the confirmation window, on pins that
/// are not blocked and still at the candidate's generation.
/// Environment-scoped like [`verifying_attempts`]: a single-replica
/// deployment's only replica re-verifies them to supply the spaced second
/// observation.
pub async fn single_replica_candidates_due(
    pool: &PgPool,
    interval_secs: i64,
) -> Result<Vec<DueCandidate>> {
    check_single_replica_interval(interval_secs)?;
    let rows = sqlx::query(
        "SELECT c.tenant_id, c.index_version_id, c.artifact_id, c.fetch_id
           FROM artifact_integrity_candidates c
           JOIN artifact_integrity_pins p
             ON p.tenant_id = c.tenant_id AND p.index_version_id = c.index_version_id
            AND p.artifact_id = c.artifact_id
          WHERE p.state <> 'blocked' AND p.generation = c.pin_generation
            AND c.fetched_at <= now() - make_interval(secs => $1)
            AND c.fetched_at >= now() - make_interval(secs => $2)
          ORDER BY c.fetched_at, c.tenant_id, c.fetch_id
          LIMIT 500",
    )
    .bind(interval_secs as f64)
    .bind(DEFAULT_CONFIRM_WINDOW_SECS as f64)
    .fetch_all(pool)
    .await
    .map_err(storage_err)?;
    Ok(rows
        .iter()
        .map(|r| DueCandidate {
            tenant_id: r.get("tenant_id"),
            index_version_id: r.get("index_version_id"),
            artifact_id: r.get("artifact_id"),
            fetch_id: r.get("fetch_id"),
        })
        .collect())
}

fn to_attempt(r: &sqlx::postgres::PgRow) -> RepairAttempt {
    RepairAttempt {
        attempt_id: r.get("attempt_id"),
        incident_id: r.get("incident_id"),
        index_version_id: r.get("index_version_id"),
        artifact_id: r.get("artifact_id"),
        object_key: r.get("object_key"),
        expected_sha256: r.get("expected_sha256"),
        workflow_id: r.get("workflow_id"),
        authorized_by: r.get("authorized_by"),
        state: r.get("state"),
        first_match_verifier_id: r.get("first_match_verifier_id"),
        first_match_fetched_at: r.get("first_match_fetched_at"),
    }
}

fn to_pin(r: &sqlx::postgres::PgRow) -> PinState {
    PinState {
        index_version_id: r.get("index_version_id"),
        artifact_id: r.get("artifact_id"),
        generation: r.get("generation"),
        state: r.get("state"),
        active_incident_id: r.get("active_incident_id"),
    }
}

/// Why an observation's expected digest is not the trusted pin's, if it is
/// not. The manifest's digest is the artifact id; a component's is the one
/// the catalog's manifest projection records, which the verifier re-derives
/// independently from manifest bytes that hash to the artifact id.
fn untrusted_pin(pin: &ObjectPin, manifest: &serde_json::Value) -> Option<String> {
    if pin.object_key == MANIFEST_OBJECT {
        return (pin.expected_sha256 != pin.artifact_id)
            .then(|| "the manifest's expected digest must be the artifact id".to_string());
    }
    let declared = manifest
        .get("components")
        .and_then(|c| c.as_array())
        .and_then(|components| {
            components
                .iter()
                .find(|c| c.get("path").and_then(|p| p.as_str()) == Some(pin.object_key.as_str()))
        })
        .and_then(|c| c.get("sha256"))
        .and_then(|s| s.as_str());
    match declared {
        Some(d) if d == pin.expected_sha256 => None,
        Some(_) => Some("expected digest is not the pinned component digest".into()),
        None => Some("object is not a component of the pinned artifact".into()),
    }
}
