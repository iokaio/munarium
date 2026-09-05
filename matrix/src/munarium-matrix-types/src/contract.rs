// SPDX-License-Identifier: Apache-2.0
//! Rust types for the vendored cross-tree contract
//! (`matrix/contract/*.schema.json`).
//!
//! These are the wire. Two rules make them safe to evolve:
//!
//! - **Producers are strict, consumers are tolerant.** These structs do NOT
//!   use `deny_unknown_fields` (unlike the asset grammar), so a server built
//!   against contract 0.2 can send a field this build has never heard of and
//!   this build ignores it. The conformance suite validates everything Matrix
//!   *emits* against the JSON Schemas, which is where strictness belongs.
//! - **Numbers that must be exact are strings.** `decimal` and `int64` values
//!   travel as text, because a JSON number is a double in most parsers.

use serde::{Deserialize, Serialize};

pub const CONTRACT_VERSION: &str = munarium_matrix_core::CONTRACT_VERSION;

fn contract_version() -> String {
    CONTRACT_VERSION.to_string()
}

// ---------------------------------------------------------------------------
// QueryIntent (server -> Matrix)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentKind {
    StructuredQuery,
    Semantic,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryIntent {
    #[serde(default = "contract_version")]
    pub contract_version: String,
    pub kind: IntentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic: Option<SemanticIntent>,
    #[serde(default)]
    pub parameters: std::collections::BTreeMap<String, TypedValueDto>,
    pub authorization: AuthorizationSnapshot,
    pub limits: IntentLimits,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness: Option<FreshnessObligation>,
    #[serde(default)]
    pub seal: SealPolicy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticIntent {
    pub provider: String,
    pub measures: Vec<String>,
    #[serde(default)]
    pub dimensions: Vec<String>,
    #[serde(default)]
    pub filters: Vec<SemanticFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grain: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticFilter {
    pub dimension: String,
    pub op: String,
    pub value: TypedValueDto,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthorizationSnapshot {
    pub tenant: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    pub access_level: i32,
    #[serde(default)]
    pub compartments: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runbook_ref: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct IntentLimits {
    pub max_rows: u64,
    pub max_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cells: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FreshnessObligation {
    pub max_staleness_seconds: u64,
    #[serde(default = "default_on_violation")]
    pub on_violation: FreshnessAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessAction {
    Refuse,
    Disclose,
}

fn default_on_violation() -> FreshnessAction {
    FreshnessAction::Refuse
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SealPolicy {
    #[serde(default = "default_seal_required")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_days: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

impl Default for SealPolicy {
    fn default() -> Self {
        Self {
            required: true,
            retention_days: None,
            idempotency_key: None,
        }
    }
}

fn default_seal_required() -> bool {
    true
}

/// A value with its logical type stated. The wire form of
/// [`munarium_matrix_core::value::Value`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypedValueDto {
    #[serde(rename = "type")]
    pub ty: munarium_matrix_core::value::ColumnType,
    pub value: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub element_type: Option<munarium_matrix_core::value::ColumnType>,
}

// ---------------------------------------------------------------------------
// EvidenceManifest (Matrix -> server)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Table,
    Count,
    Observations,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceManifest {
    #[serde(default = "contract_version")]
    pub contract_version: String,
    #[serde(default = "default_canon")]
    pub canon: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_id: Option<String>,
    pub tenant: String,
    pub kind: ArtifactKind,
    pub logical_result_hash: String,
    pub artifact_hash: String,
    pub bytes_len: u64,
    pub media_type: String,
    pub source: ManifestSource,
    #[serde(default)]
    pub versions: ManifestVersions,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<ManifestPlan>,
    pub schema: ManifestSchema,
    pub identity: ManifestIdentity,
    pub completeness: ManifestCompleteness,
    #[serde(default)]
    pub redaction: ManifestRedaction,
    pub snapshot_vector: Vec<SnapshotMarker>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness: Option<ManifestFreshness>,
    pub execution: ManifestExecution,
    pub authorization_class: munarium_matrix_core::result::AuthorizationClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<ManifestRetention>,
}

fn default_canon() -> String {
    munarium_matrix_core::CANON_VERSION.to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestSource {
    pub source_id: String,
    pub source_version: u32,
    pub adapter: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ManifestVersions {
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestPlan {
    pub canonical_plan_hash: String,
    pub bound_parameters_hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestSchema {
    pub columns: Vec<munarium_matrix_core::result::Column>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestIdentity {
    pub row_id_rule: munarium_matrix_core::result::RowIdRule,
    #[serde(default)]
    pub order_by: Vec<String>,
    #[serde(default)]
    pub rows: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestCompleteness {
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_max_rows: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows_covered: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows_excluded: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusion_reason: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ManifestRedaction {
    #[serde(default)]
    pub denied_columns: Vec<String>,
    #[serde(default)]
    pub masked: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotMarker {
    pub source_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marker: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
    pub replay_level: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestFreshness {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watermark: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lag_seconds: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestExecution {
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub ended_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_principal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statement_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestRetention {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub legal_hold: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purged_at: Option<chrono::DateTime<chrono::Utc>>,
}

// ---------------------------------------------------------------------------
// EvidenceBlock (Matrix -> server)
// ---------------------------------------------------------------------------

/// The closed set. `#[serde(tag = "kind")]` makes the wire form exactly the
/// schema's discriminated union, and an unknown kind fails to deserialize —
/// which is correct for a CLOSED enum.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvidenceBlock {
    CompleteTable {
        #[serde(default = "contract_version")]
        contract_version: String,
        evidence_id: String,
        manifest: Box<EvidenceManifest>,
        rows: Vec<BlockRow>,
        truncated: bool,
        #[serde(default)]
        derivations: Vec<munarium_matrix_core::derivation::ComputedDerivation>,
    },
    Count {
        #[serde(default = "contract_version")]
        contract_version: String,
        evidence_id: String,
        manifest: Box<EvidenceManifest>,
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        of: Option<String>,
        #[serde(default = "default_exact")]
        exact: bool,
    },
    DocumentHits {
        #[serde(default = "contract_version")]
        contract_version: String,
        hits: Vec<serde_json::Value>,
    },
    FactSlice {
        #[serde(default = "contract_version")]
        contract_version: String,
        version_id: String,
        as_of_seq: u64,
        facts: Vec<serde_json::Value>,
    },
    Refusal {
        #[serde(default = "contract_version")]
        contract_version: String,
        refusal: munarium_matrix_core::Refusal,
    },
}

fn default_exact() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockRow {
    pub row_id: String,
    /// Positional to `manifest.schema.columns`; `null` is SQL NULL.
    pub cells: Vec<Option<String>>,
}

impl EvidenceBlock {
    pub fn refusal(r: munarium_matrix_core::Refusal) -> Self {
        EvidenceBlock::Refusal {
            contract_version: contract_version(),
            refusal: r,
        }
    }

    /// True for the two kinds Matrix can produce that carry sealed evidence.
    pub fn evidence_id(&self) -> Option<&str> {
        match self {
            EvidenceBlock::CompleteTable { evidence_id, .. }
            | EvidenceBlock::Count { evidence_id, .. } => Some(evidence_id),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// ObservationBatch (Matrix -> server)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservationBatch {
    #[serde(default = "contract_version")]
    pub contract_version: String,
    pub mapping: String,
    pub batch_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sealed_evidence_id: Option<String>,
    pub observations: Vec<Observation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    pub entity_candidates: Vec<EntityCandidate>,
    pub property: String,
    pub value: TypedValueDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_time: Option<ValidTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transaction_time: Option<chrono::DateTime<chrono::Utc>>,
    pub change_kind: ChangeKind,
    pub origin: ConnectorOrigin,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityCandidate {
    pub subject: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_path: Option<String>,
    pub confidence: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolver: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ValidTime {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Insert,
    Update,
    Delete,
    /// Its own kind on purpose: a backdated change never becomes a correction
    /// without review.
    Backdated,
    Snapshot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectorOrigin {
    #[serde(default = "default_origin_kind")]
    pub kind: String,
    pub source_id: String,
    pub mapping_version: String,
    pub row_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_position: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_id: Option<String>,
}

fn default_origin_kind() -> String {
    "connector".to_string()
}
