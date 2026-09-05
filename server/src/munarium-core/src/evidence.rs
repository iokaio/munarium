// SPDX-License-Identifier: Apache-2.0
//! Sealed evidence — the server-owned structured-evidence plane.
//!
//! An **evidence artifact** is the exact typed result an answer was computed
//! from, registered here so that later — after the source has moved on — the
//! question "what was this number actually computed from?" has an answer that
//! is access-checked, replayable for a stated period, and impossible to
//! confuse with a different result.
//!
//! The shape of a manifest is not invented here. It is
//! `contract/matrix/evidence-manifest.schema.json`, the vendored cross-tree
//! contract, and this module is its Rust mirror. When the two disagree
//! the schema wins — `tests/matrix_contract.rs` in `munarium-api-types` keeps
//! the examples honest, and [`CONTRACT_VERSION`] below is read from the
//! vendored file at compile time so the server cannot claim to speak a
//! contract version it was not built against.
//!
//! # Three ideas worth holding
//!
//! **Two hashes, never conflated.** `logical_result_hash` answers *is this the
//! same answer?*; `artifact_hash` answers *are these the same bytes?* A CSV
//! and a Parquet serialization of one result share the first and differ in the
//! second. Every commit verifies both, because verifying only the bytes would
//! let a re-serialization masquerade as a new result, and verifying only the
//! logical hash would let corrupted bytes through.
//!
//! **Evidence is regulated data.** Every artifact carries an authorization
//! equivalence class, and a session resolving it must *dominate* that class —
//! at least the access level, and every compartment. This is deliberately the
//! same primitive collections use ([`AuthorizationClass::dominated_by`]), so
//! there is one domination rule in the server and not two that drift.
//!
//! **A purged artifact keeps its row.** Retention deletes the *bytes*; the
//! metadata row survives with `purged_at` set, so a citation to expired
//! evidence resolves as `evidence-expired` — an honest statement about a
//! retention policy — rather than `not-found`, which would read as though the
//! citation had been fabricated.

use serde::{Deserialize, Serialize};

use crate::{KernelError, Result};

/// The vendored Matrix contract version this build speaks, read from
/// `server/contract/matrix/VERSION` at compile time.
///
/// Embedding it makes the vendored copy load-bearing rather than decorative:
/// delete the directory and the server stops compiling, which is a far better
/// failure than silently accepting manifests written against a contract
/// nobody checked. (`server/.dockerignore` must therefore keep `contract/` —
/// the note there says so, and this is the reason.)
pub const CONTRACT_VERSION: &str = include_str!("../../../contract/matrix/VERSION");

/// Reserved logical-path prefix for sealed artifact bytes.
///
/// Artifacts share the object store with documents, so the prefix is reserved
/// on the *document* write paths: a document at `evidence/...` could otherwise
/// collide with an artifact's blob, and — worse — a reader might infer
/// authorization from the path. Authorization comes from the database row,
/// never from where the bytes happen to live.
pub const EVIDENCE_PATH_PREFIX: &str = "evidence/";

/// Inline seal ceiling: at or below this, manifest and bytes arrive in one
/// request and commit atomically. Above it, the grant flow applies.
///
/// 1 MiB is the mode-B contract `maxBytes` ceiling, so the common case — a
/// query result backing one answer — is always a single round-trip.
pub const INLINE_SEAL_MAX_BYTES: usize = 1024 * 1024;

/// Hard ceiling on a single artifact, matching the body limit `PUT /v1/sources`
/// already declares.
pub const MAX_ARTIFACT_BYTES: usize = 256 * 1024 * 1024;

/// Default rows returned by a range read when the caller does not ask.
pub const DEFAULT_ROW_LIMIT: usize = 100;
/// Ceiling on a single range read. A caller wanting more pages.
pub const MAX_ROW_LIMIT: usize = 1000;

/// How long an upload grant stays usable. Short on purpose: a grant is a
/// single-use capability to write bytes under an id the server has already
/// committed to, so its blast radius is a function of its lifetime.
pub const GRANT_TTL_SECS: i64 = 900;

// ---------------------------------------------------------------------------
// Manifest — the mirror of evidence-manifest.schema.json
// ---------------------------------------------------------------------------

/// What the sealed bytes are. **Closed** — a new member is a major contract
/// bump, per the contract's compatibility rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// A query result table (mode B).
    Table,
    /// An exact count with its coverage (mode A).
    Count,
    /// A batch of typed observations (mode C).
    Observations,
}

impl EvidenceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Table => "table",
            Self::Count => "count",
            Self::Observations => "observations",
        }
    }
}

/// Logical column types. **Closed** — the canon@1 scalar encodings are defined
/// exactly over this set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnType {
    Bool,
    Int64,
    Decimal,
    Float64,
    String,
    Bytes,
    Date,
    TimestampTz,
    TimestampNaive,
    Interval,
    Uuid,
    Json,
    Array,
}

/// How rows are identified — the canon@1 ordering rule for this result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RowIdRule {
    /// Row id derives from the declared key tuple; the result hashes as a
    /// multiset, so row order is irrelevant.
    Keys,
    /// Row id is the position; legal only under a total `order_by`.
    Position,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceColumn {
    /// Stable within the contract; survives a rename of the source column.
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub ty: ColumnType,
    pub nullable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<i32>,
    /// e.g. `USD`, `seconds`. Carried into the answer so a number is never
    /// unitless.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additivity: Option<String>,
    #[serde(default)]
    pub key: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub element_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceSchema {
    pub columns: Vec<EvidenceColumn>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceIdentity {
    pub row_id_rule: RowIdRule,
    #[serde(default)]
    pub order_by: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<i64>,
}

/// G4. A truncated block cannot support a completeness claim, and the server
/// enforces that; this is where it learns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Completeness {
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_max_rows: Option<i64>,
    /// Mode A: source rows this run covered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows_covered: Option<i64>,
    /// Mode A: rows excluded by policy or drift — reported, never silently
    /// dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows_excluded: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusion_reason: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Redaction {
    /// Columns the policy denied. They were never selected, so they are absent
    /// from the bytes; naming them here is how an operator sees WHY a column
    /// is missing.
    #[serde(default)]
    pub denied_columns: Vec<String>,
    #[serde(default)]
    pub masked: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceRef {
    pub source_id: String,
    pub source_version: i64,
    /// OPEN enum: postgres | databricks | landing | ...
    pub adapter: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_version: Option<String>,
    /// What actually executed it, as the engine reports itself. Part of the
    /// provenance, not of the logical identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver: Option<String>,
}

/// Every versioned artifact that shaped this result. A `None` means "not
/// applicable to this kind", never "unknown".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Versions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_contract: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_mapping: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compiler: Option<String>,
}

/// Hashes of the compiled plan and the bound parameters. The statement TEXT is
/// never sealed and never leaves Matrix.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanHashes {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_plan_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_parameters_hash: Option<String>,
}

/// G3. One marker per source that contributed. A cross-source result is a
/// vector precisely so it is never described as one atomic snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotMarker {
    pub source_id: String,
    /// Engine-native: a pg snapshot, a Delta version, a manifest id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marker: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    /// OPEN enum. G2 is only promised as `source_time_travel`.
    pub replay_level: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_expires_at: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Freshness {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watermark: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lag_seconds: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Execution {
    pub started_at: String,
    pub ended_at: String,
    /// G6. The identity the SOURCE saw — an RLS-subject role, a Unity Catalog
    /// principal. Not the end user's uid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_principal: Option<String>,
    /// Engine-side correlation id (Databricks statement_id, pg pid).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statement_id: Option<String>,
}

/// The equivalence class an artifact belongs to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthorizationClass {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub access_level: i32,
    #[serde(default)]
    pub compartments: Vec<String>,
}

impl AuthorizationClass {
    /// The simple-security property, stated from the artifact's side: a reader
    /// at `level` holding `compartments` may resolve this artifact iff the
    /// level dominates AND every compartment on the artifact is held.
    ///
    /// Deliberately the same rule `AccessCtx::permits` applies to a
    /// collection. Evidence is not a weaker class of data than the documents
    /// it sits beside, and two domination rules in one server is one too many.
    ///
    /// `all_compartments` is the unrestricted principal (static rw / auth
    /// disabled) and clears the compartment gate, never the level gate — the
    /// level is still compared, so an explicitly low-level principal cannot
    /// read above itself just by being unrestricted in compartments.
    pub fn dominated_by(
        &self,
        level: i32,
        compartments: &[String],
        all_compartments: bool,
    ) -> bool {
        level >= self.access_level
            && (all_compartments
                || self
                    .compartments
                    .iter()
                    .all(|c| compartments.iter().any(|have| have == c)))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Retention {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub legal_hold: bool,
    /// Set by the server's purge job. A purged artifact KEEPS this row so a
    /// citation resolves `evidence-expired` rather than `not-found`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purged_at: Option<String>,
}

/// The manifest: everything needed to prove what an answer was computed from,
/// decide who may read it, and say honestly whether it can be replayed.
///
/// The bytes are separate — this names them by `artifact_hash` and never
/// embeds them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceManifest {
    pub contract_version: String,
    /// Always `canon@1` in this contract major.
    pub canon: String,
    /// Assigned by the SERVER at seal. Absent in the seal request, present in
    /// every read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_id: Option<String>,
    pub tenant: String,
    pub kind: EvidenceKind,
    pub logical_result_hash: String,
    pub artifact_hash: String,
    pub bytes_len: i64,
    pub media_type: String,
    pub source: SourceRef,
    pub versions: Versions,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<PlanHashes>,
    pub schema: EvidenceSchema,
    pub identity: EvidenceIdentity,
    pub completeness: Completeness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redaction: Option<Redaction>,
    pub snapshot_vector: Vec<SnapshotMarker>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness: Option<Freshness>,
    pub execution: Execution,
    pub authorization_class: AuthorizationClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<Retention>,
}

/// Accepted serializations. Both are canonical forms; the CSV form exists so a
/// small artifact is readable without a Parquet reader.
pub const MEDIA_TYPE_PARQUET: &str = "application/vnd.apache.parquet";
pub const MEDIA_TYPE_CSV: &str = "text/csv; charset=utf-8";

fn is_hash(s: &str) -> bool {
    s.len() == 71
        && s.starts_with("sha256:")
        && s[7..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl EvidenceManifest {
    /// The domain idempotency tuple: sealing the same logical result, under
    /// the same policy, for the same authorization class, is the same seal.
    ///
    /// Note what is *absent*: `artifact_hash`. Re-serializing one logical
    /// result must not mint a second artifact — that is the whole reason the
    /// two hashes are distinct. The authorization class IS present, because
    /// the same rows read under two different classes are two different
    /// artifacts as far as who-may-read-this is concerned.
    pub fn domain_key(&self) -> String {
        use sha2::Digest;
        let mut compartments = self.authorization_class.compartments.clone();
        compartments.sort();
        // The compartments are joined with the same unit separator as the
        // outer fields, not a comma: nothing validates a compartment tag
        // against commas, so `["a,b"]` and `["a", "b"]` joined by "," were
        // one key, and `find_by_domain_key` could have replayed an artifact
        // sealed under a different authorization class. (A single-compartment
        // class hashes the same as before; only multi-compartment keys move.)
        let material = format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
            self.tenant,
            self.logical_result_hash,
            self.versions.policy.as_deref().unwrap_or(""),
            self.authorization_class.access_level,
            compartments.join("\u{1f}"),
        );
        format!(
            "dk-{}",
            hex::encode(sha2::Sha256::digest(material.as_bytes()))
        )
    }

    /// Structural validation at the door. Producers are strict; this is the
    /// consumer being strict about the things it will later rely on, so a
    /// malformed manifest is refused at seal rather than discovered at
    /// resolution when the source is long gone.
    pub fn validate(&self) -> Result<()> {
        let bad = |m: String| Err(KernelError::InvalidInput(m));

        if self.canon != "canon@1" {
            return bad(format!(
                "canon must be 'canon@1', got '{}'; this server implements exactly one \
                 canonicalization version",
                self.canon
            ));
        }
        let want_major = CONTRACT_VERSION.trim().split('.').next().unwrap_or("");
        let got_major = self.contract_version.trim().split('.').next().unwrap_or("");
        if want_major != got_major {
            return bad(format!(
                "contract major version mismatch: manifest declares '{}', this server was built \
                 against '{}'. A major bump is a wire break, so the two trees must be deployed \
                 together",
                self.contract_version,
                CONTRACT_VERSION.trim()
            ));
        }
        if self.tenant.trim().is_empty() {
            return bad("tenant is required".into());
        }
        if !is_hash(&self.logical_result_hash) {
            return bad(format!(
                "logical_result_hash must be 'sha256:<64 lowercase hex>', got '{}'",
                self.logical_result_hash
            ));
        }
        if !is_hash(&self.artifact_hash) {
            return bad(format!(
                "artifact_hash must be 'sha256:<64 lowercase hex>', got '{}'",
                self.artifact_hash
            ));
        }
        if self.bytes_len < 0 {
            return bad("bytes_len must not be negative".into());
        }
        if self.bytes_len as usize > MAX_ARTIFACT_BYTES {
            return bad(format!(
                "bytes_len {} exceeds the {MAX_ARTIFACT_BYTES}-byte ceiling",
                self.bytes_len
            ));
        }
        if self.media_type != MEDIA_TYPE_PARQUET && self.media_type != MEDIA_TYPE_CSV {
            return bad(format!(
                "media_type must be '{MEDIA_TYPE_PARQUET}' or '{MEDIA_TYPE_CSV}', got '{}'",
                self.media_type
            ));
        }
        if self.schema.columns.is_empty() {
            return bad("schema.columns must not be empty".into());
        }
        let mut ids: Vec<&str> = self.schema.columns.iter().map(|c| c.id.as_str()).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        if ids.len() != before {
            return bad("schema.columns contains duplicate column ids".into());
        }
        if self.snapshot_vector.is_empty() {
            return bad(
                "snapshot_vector must name at least one source; an artifact with no snapshot \
                 marker cannot state its freshness (G3)"
                    .into(),
            );
        }
        if self.authorization_class.access_level < 0 {
            return bad("authorization_class.access_level must not be negative".into());
        }
        // Retention timestamps are compared as TEXT by the memory store and
        // cast to `timestamptz` by the Postgres store, so a value that is not
        // RFC 3339 was a 500 on one backend and an artifact with undefined
        // retention on the other. Refused at the door instead.
        if let Some(r) = &self.retention {
            for (field, value) in [("expires_at", &r.expires_at), ("purged_at", &r.purged_at)] {
                if let Some(v) = value {
                    if chrono::DateTime::parse_from_rfc3339(v).is_err() {
                        return bad(format!(
                            "retention.{field} must be an RFC 3339 timestamp, got '{v}'"
                        ));
                    }
                }
            }
        }

        // canon@1 rule 3, enforced where it can still be acted on. A result
        // that cannot name its rows cannot be sealed at all: under `position`
        // the row ids mean nothing without a total order, so a later citation
        // to row 7 would resolve to whatever row 7 happened to be.
        match self.identity.row_id_rule {
            RowIdRule::Position => {
                if self.identity.order_by.is_empty() {
                    return bad(
                        "identity.row_id_rule is 'position' but order_by is empty; positional row \
                         ids are meaningless without a total ordering (canon@1 rule 3)"
                            .into(),
                    );
                }
            }
            RowIdRule::Keys => {
                if !self.schema.columns.iter().any(|c| c.key) {
                    return bad(
                        "identity.row_id_rule is 'keys' but no column is marked key; the row id \
                         would have nothing to derive from (canon@1 rule 3)"
                            .into(),
                    );
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Stored record, grants, audit
// ---------------------------------------------------------------------------

/// Lifecycle of an artifact row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceState {
    /// A grant has been issued; the bytes have not been committed. Nothing
    /// resolves in this state — a pending artifact is not evidence yet.
    Pending,
    /// Bytes committed, both hashes verified. The only readable state.
    Committed,
    /// Retention purged the bytes; the row survives so citations resolve
    /// `evidence-expired` rather than `not-found`.
    Purged,
}

impl EvidenceState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Committed => "committed",
            Self::Purged => "purged",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "committed" => Some(Self::Committed),
            "purged" => Some(Self::Purged),
            _ => None,
        }
    }
}

/// A stored artifact: the manifest plus the server-owned lifecycle facts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceArtifact {
    pub evidence_id: String,
    pub tenant: String,
    pub state: EvidenceState,
    pub manifest: EvidenceManifest,
    /// Blob path under the reserved keyspace. Recorded so a purge knows what
    /// to delete without recomputing it.
    pub blob_path: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committed_at: Option<String>,
}

/// A single-use capability to upload bytes for an artifact the server has
/// already assigned an id to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceGrant {
    pub grant_id: String,
    pub evidence_id: String,
    pub tenant: String,
    pub expires_at: String,
    /// Set when the grant is spent. A grant is single-use: the second attempt
    /// is refused even inside the TTL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_at: Option<String>,
}

/// One resolution, recorded. Deliberately records *that* a read happened and
/// by whom — never the rows read. An audit table holding the regulated data it
/// audits is a second copy of the problem.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceAccess {
    pub evidence_id: String,
    pub tenant: String,
    pub uid: String,
    /// `manifest` | `rows`
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_from: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_limit: Option<i64>,
    /// `ok` | `denied` | `expired` | `on-hold`
    pub outcome: String,
    pub at: String,
}

/// What a seal did. Distinguishing "created" from "replayed" is what lets a
/// caller tell an idempotent retry from a new artifact without comparing ids.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SealOutcome {
    pub evidence_id: String,
    pub created: bool,
    /// Present only on the grant path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant: Option<EvidenceGrant>,
}

/// Persistence for the evidence plane.
///
/// Separate from [`crate::storage::StorageBackend`] on purpose: that trait is
/// the ledger, and evidence is not ledger data. Mixing them would put an
/// artifact's retention clock in the same trait as `append_claim`, and the two
/// have nothing to say to each other.
#[async_trait::async_trait]
pub trait EvidenceStore: Send + Sync {
    /// Register a manifest, or return the existing artifact when the domain
    /// key already exists. `pending` when a grant is wanted, otherwise the
    /// caller commits immediately.
    async fn register(
        &self,
        artifact: &EvidenceArtifact,
        grant: Option<&EvidenceGrant>,
    ) -> Result<SealOutcome>;

    async fn get(&self, tenant: &str, evidence_id: &str) -> Result<Option<EvidenceArtifact>>;

    /// Look up by the domain idempotency tuple.
    async fn find_by_domain_key(
        &self,
        tenant: &str,
        domain_key: &str,
    ) -> Result<Option<EvidenceArtifact>>;

    /// Mark an artifact committed. Returns false when it was already
    /// committed, so a replayed commit is visible rather than silent.
    async fn commit(&self, tenant: &str, evidence_id: &str, at: &str) -> Result<bool>;

    /// Spend a grant. Returns the grant iff it exists, matches the artifact,
    /// is unexpired and unused — the single-use check lives here so it is one
    /// atomic step in the Postgres store rather than a read-then-write race.
    async fn consume_grant(
        &self,
        tenant: &str,
        evidence_id: &str,
        grant_id: &str,
        now: &str,
    ) -> Result<Option<EvidenceGrant>>;

    async fn record_access(&self, access: &EvidenceAccess) -> Result<()>;

    /// Recent accesses, newest first. Operator-facing.
    async fn accesses(
        &self,
        tenant: &str,
        evidence_id: &str,
        limit: usize,
    ) -> Result<Vec<EvidenceAccess>>;

    // -- retention  ---------------------------------------

    /// Committed artifacts whose retention has expired and which are not on
    /// legal hold, across every tenant, oldest expiry first.
    ///
    /// Across tenants on purpose: the janitor is a deployment-wide obligation,
    /// not a per-tenant one, and a retention policy that only ran for tenants
    /// somebody remembered to sweep would not be a retention policy.
    async fn purge_due(&self, now: &str, limit: usize) -> Result<Vec<EvidenceArtifact>>;

    /// Mark an artifact purged. Returns false when it was already purged, so
    /// two instances sweeping at once cannot both claim the same row.
    ///
    /// This is called AFTER the bytes are deleted. The ordering is chosen for
    /// its failure mode: delete-then-mark can leave a row that still says
    /// `committed` while its bytes are gone — ugly for one sweep interval, but
    /// **self-healing**, because the next sweep still sees the row as due and
    /// completes it. Mark-then-delete would instead leave an artifact that
    /// reports itself purged while its regulated bytes are still on disk, and
    /// no later sweep would ever revisit it. A retention system may be briefly
    /// untidy; it may not quietly fail to delete.
    async fn mark_purged(&self, tenant: &str, evidence_id: &str, at: &str) -> Result<bool>;

    /// Place or lift a legal hold. Returns false when the artifact is unknown.
    ///
    /// A hold blocks *deletion*, never *reading*: it is an instruction to
    /// preserve evidence, and an instruction to preserve something that also
    /// hid it would be a strange one. Reads stay governed by the authorization
    /// class exactly as before.
    async fn set_legal_hold(&self, tenant: &str, evidence_id: &str, hold: bool) -> Result<bool>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> EvidenceManifest {
        EvidenceManifest {
            contract_version: CONTRACT_VERSION.trim().to_string(),
            canon: "canon@1".into(),
            evidence_id: None,
            tenant: "demo".into(),
            kind: EvidenceKind::Table,
            logical_result_hash: format!("sha256:{}", "a".repeat(64)),
            artifact_hash: format!("sha256:{}", "b".repeat(64)),
            bytes_len: 12,
            media_type: MEDIA_TYPE_CSV.into(),
            source: SourceRef {
                source_id: "crm".into(),
                source_version: 1,
                adapter: "postgres".into(),
                adapter_version: None,
                engine: None,
                driver: None,
            },
            versions: Versions::default(),
            plan: None,
            schema: EvidenceSchema {
                columns: vec![EvidenceColumn {
                    id: "c1".into(),
                    name: "region".into(),
                    ty: ColumnType::String,
                    nullable: false,
                    scale: None,
                    unit: None,
                    additivity: None,
                    key: true,
                    element_type: None,
                }],
            },
            identity: EvidenceIdentity {
                row_id_rule: RowIdRule::Keys,
                order_by: vec![],
                rows: Some(1),
            },
            completeness: Completeness {
                truncated: false,
                declared_max_rows: None,
                rows_covered: None,
                rows_excluded: None,
                exclusion_reason: None,
            },
            redaction: None,
            snapshot_vector: vec![SnapshotMarker {
                source_id: "crm".into(),
                marker: None,
                isolation: None,
                started_at: None,
                ended_at: None,
                replay_level: "sealed_result".into(),
                replay_expires_at: None,
            }],
            freshness: None,
            execution: Execution {
                started_at: "2026-08-28T00:00:00Z".into(),
                ended_at: "2026-08-28T00:00:01Z".into(),
                effective_principal: None,
                statement_id: None,
            },
            authorization_class: AuthorizationClass {
                name: None,
                access_level: 2,
                compartments: vec!["fin".into()],
            },
            retention: None,
        }
    }

    #[test]
    fn a_valid_manifest_passes() {
        manifest().validate().expect("valid");
    }

    #[test]
    fn the_contract_version_is_the_vendored_one() {
        // Guards the include_str! path: if the vendored copy moved, this fails
        // loudly instead of the server silently speaking a version nobody set.
        let v = CONTRACT_VERSION.trim();
        assert_eq!(v.split('.').count(), 3, "vendored VERSION is {v:?}");
    }

    #[test]
    fn a_major_contract_mismatch_is_refused() {
        let mut m = manifest();
        m.contract_version = "99.0.0".into();
        let err = m.validate().expect_err("must refuse");
        assert!(format!("{err}").contains("major version mismatch"), "{err}");
    }

    #[test]
    fn a_minor_contract_difference_is_accepted() {
        // Producers are strict, consumers tolerant: a newer MINOR is additive
        // by the compatibility rule, so refusing it would break the rule the
        // contract states.
        let mut m = manifest();
        let major = CONTRACT_VERSION.trim().split('.').next().unwrap();
        m.contract_version = format!("{major}.99.0");
        m.validate().expect("minor drift is accepted");
    }

    #[test]
    fn hashes_must_be_prefixed_lowercase_sha256() {
        let mut m = manifest();
        m.artifact_hash = "b".repeat(64);
        assert!(m.validate().is_err(), "unprefixed hash must be refused");

        let mut m = manifest();
        m.logical_result_hash = format!("sha256:{}", "A".repeat(64));
        assert!(m.validate().is_err(), "uppercase hex must be refused");
    }

    #[test]
    fn positional_rows_need_a_total_ordering() {
        let mut m = manifest();
        m.identity.row_id_rule = RowIdRule::Position;
        m.identity.order_by = vec![];
        let err = m.validate().expect_err("must refuse");
        assert!(format!("{err}").contains("total ordering"), "{err}");

        m.identity.order_by = vec!["c1".into()];
        m.validate().expect("ordered positional identity is fine");
    }

    #[test]
    fn keyed_rows_need_a_key_column() {
        let mut m = manifest();
        m.schema.columns[0].key = false;
        let err = m.validate().expect_err("must refuse");
        assert!(
            format!("{err}").contains("no column is marked key"),
            "{err}"
        );
    }

    #[test]
    fn the_domain_key_ignores_the_byte_serialization() {
        // The point of two hashes: a CSV and a Parquet encoding of ONE result
        // must not mint two artifacts.
        let a = manifest();
        let mut b = manifest();
        b.artifact_hash = format!("sha256:{}", "c".repeat(64));
        b.media_type = MEDIA_TYPE_PARQUET.into();
        assert_eq!(a.domain_key(), b.domain_key());
    }

    #[test]
    fn the_domain_key_separates_authorization_classes() {
        // The same rows read under a different clearance are a different
        // artifact — otherwise an idempotent re-seal could hand a low-clearance
        // caller an id minted for a high-clearance read.
        let a = manifest();
        let mut b = manifest();
        b.authorization_class.access_level = 9;
        assert_ne!(a.domain_key(), b.domain_key());

        let mut c = manifest();
        c.authorization_class.compartments = vec!["hr".into()];
        assert_ne!(a.domain_key(), c.domain_key());
    }

    #[test]
    fn the_domain_key_is_order_insensitive_in_compartments() {
        let mut a = manifest();
        a.authorization_class.compartments = vec!["fin".into(), "hr".into()];
        let mut b = manifest();
        b.authorization_class.compartments = vec!["hr".into(), "fin".into()];
        assert_eq!(a.domain_key(), b.domain_key());
    }

    #[test]
    fn domination_needs_the_level_and_every_compartment() {
        let class = AuthorizationClass {
            name: None,
            access_level: 5,
            compartments: vec!["fin".into(), "hr".into()],
        };
        let both = vec!["fin".to_string(), "hr".to_string()];
        assert!(class.dominated_by(5, &both, false));
        assert!(class.dominated_by(9, &both, false));
        // Level too low.
        assert!(!class.dominated_by(4, &both, false));
        // Missing a compartment.
        assert!(!class.dominated_by(9, &["fin".to_string()], false));
        // Unrestricted clears compartments but NOT the level.
        assert!(class.dominated_by(5, &[], true));
        assert!(!class.dominated_by(4, &[], true));
    }

    #[test]
    fn a_manifest_round_trips_through_json() {
        let m = manifest();
        let text = serde_json::to_string(&m).expect("serialize");
        let back: EvidenceManifest = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(m, back);
    }

    #[test]
    fn an_empty_snapshot_vector_is_refused() {
        let mut m = manifest();
        m.snapshot_vector.clear();
        assert!(m.validate().is_err());
    }

    #[test]
    fn duplicate_column_ids_are_refused() {
        let mut m = manifest();
        let dup = m.schema.columns[0].clone();
        m.schema.columns.push(dup);
        let err = m.validate().expect_err("must refuse");
        assert!(format!("{err}").contains("duplicate column ids"), "{err}");
    }
}

#[cfg(test)]
mod message_hygiene {
    //! A guard against a defect that bit this package four times.
    //!
    //! A Rust string literal split across lines with `\` strips the newline
    //! *and* the leading indentation — but only if the backslash survives.
    //! Every tool between an author and the file (a heredoc, a regex, a
    //! templating pass) can eat it, and when it does the literal keeps
    //! compiling and quietly carries a run of twenty spaces into the middle of
    //! a sentence. These strings are RFC 9457 problem `detail` values: they
    //! reach operators and API clients verbatim.
    //!
    //! It is not caught by fmt, clippy, or any test that only checks
    //! `contains("some phrase")` — the phrase is usually on one side of the
    //! gap. So it is checked here, over the module's own source.

    /// Runs of whitespace inside a rendered message, in this file and the
    /// server-side messages that quote it.
    #[test]
    fn no_message_carries_a_run_of_spaces() {
        for (name, src) in [
            ("evidence.rs", include_str!("evidence.rs")),
            ("sources.rs", include_str!("sources.rs")),
        ] {
            for (i, line) in src.lines().enumerate() {
                let trimmed = line.trim_start();
                // Comments and doc comments are prose; indentation in them is
                // deliberate. Only string content matters here.
                if trimmed.starts_with("//") || !line.contains('"') {
                    continue;
                }
                // A run of 8+ spaces with non-space on BOTH sides never occurs
                // in a deliberately written message.
                let bytes = line.as_bytes();
                let mut run = 0usize;
                for (j, b) in bytes.iter().enumerate() {
                    if *b == b' ' {
                        run += 1;
                        continue;
                    }
                    if run >= 8 && j > run {
                        let before = bytes[j - run - 1];
                        if before != b' ' && before != b',' && before != b'{' {
                            panic!(
                                "{name}:{} carries a {run}-space run inside a string — a \
                                 line-continuation backslash was eaten:\n{line}",
                                i + 1
                            );
                        }
                    }
                    run = 0;
                }
            }
        }
    }
}
