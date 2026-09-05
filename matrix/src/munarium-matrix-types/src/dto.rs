// SPDX-License-Identifier: Apache-2.0
//! REST DTOs for Matrix's own API (`/v1/...` on port 8180).
//!
//! Response shapes only — the request bodies for `execute` are the contract's
//! [`crate::contract::QueryIntent`], because the intent IS the cross-tree
//! contract and re-declaring it here would create a second normative copy.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// RFC 9457 problem+json, with the `matrix:` slug registry. Mirrors the
/// server's error shape so an operator reads one format across both services.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Problem {
    /// `https://munarium.ioka.io/problems/matrix/<slug>`
    #[serde(rename = "type")]
    pub problem_type: String,
    pub title: String,
    pub status: u16,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// The typed refusal, when the problem came from one. Clients key on
    /// `refusal.class` and never on the English title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<serde_json::Value>,
}

impl Problem {
    pub fn new(slug: &str, status: u16, title: &str, detail: impl Into<String>) -> Self {
        Self {
            problem_type: format!("https://munarium.ioka.io/problems/matrix/{slug}"),
            title: title.to_string(),
            status,
            detail: detail.into(),
            instance: None,
            refusal: None,
        }
    }

    /// The slug back out of the type URI — what a test asserts on.
    pub fn slug(&self) -> &str {
        self.problem_type.rsplit('/').next().unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ApplyResponse {
    /// `name@version`
    pub asset_ref: String,
    pub kind: String,
    /// True when this apply changed nothing (byte-identical re-apply).
    pub unchanged: bool,
    #[serde(default)]
    pub findings: Vec<crate::validate::Finding>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ValidateResponse {
    pub valid: bool,
    #[serde(default)]
    pub findings: Vec<crate::validate::Finding>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AssetSummary {
    pub asset_ref: String,
    pub name: String,
    pub version: u32,
    pub kind: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AssetListResponse {
    pub assets: Vec<AssetSummary>,
}

/// What `introspect` proved about the role, plus what it found in the schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct IntrospectResponse {
    pub source: String,
    pub posture: RolePostureReport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_fingerprint: Option<String>,
    #[serde(default)]
    pub tables: Vec<TableInfo>,
    /// A skeleton the operator edits, never applied automatically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_contract_yaml: Option<String>,
}

/// Each check is reported individually. A single boolean would hide *which*
/// requirement failed, and "your role is wrong" is not an actionable message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RolePostureReport {
    pub ok: bool,
    pub principal: String,
    #[serde(default)]
    pub checks: Vec<PostureCheck>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PostureCheck {
    pub name: String,
    pub required: bool,
    pub observed: bool,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TableInfo {
    pub name: String,
    #[serde(default)]
    pub columns: Vec<ColumnInfo>,
    #[serde(default)]
    pub row_security_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ColumnInfo {
    pub name: String,
    /// The source's own type name, verbatim — before any mapping.
    pub source_type: String,
    /// The canon@1 type it maps to, or `None` when it maps to nothing (which
    /// is why the column cannot be used).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logical_type: Option<String>,
    pub nullable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ProbeResponse {
    pub source: String,
    pub reachable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    /// Circuit-breaker state at probe time: closed | open | half_open.
    pub breaker: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// `POST /v1/datasources/{name}/planner/ask`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PlannerAskRequest {
    /// The question, in words.
    pub question: String,
    /// `assist` (default) or `evaluation`. Spelled rather than a boolean: the
    /// two modes differ in what they REFUSE, and a flag would leave a reader
    /// guessing which way it points.
    #[serde(default)]
    pub mode: Option<String>,
}

/// The answer. Deliberately NOT an `EvidenceBlock`: nothing here is sealed,
/// because nothing here was executed. An admitted proposal is run through a
/// contract, and it is that execution that produces citable evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PlannerAskResponse {
    pub source: String,
    pub mode: String,
    /// The pin: space, conversation, message, attachment, statement, query
    /// hash — and `pinned`, which is false everywhere today.
    pub pin: serde_json::Value,
    /// Whether the PLAN is reproducible. It is not; the field exists so a
    /// reader is told rather than left to assume.
    pub plan_pinned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prose: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_sql: Option<String>,
    /// Present only when the allowlist admitted the proposal. Run it through a
    /// contract; this route never executes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admitted_sql: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<serde_json::Value>,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct HealthDataResponse {
    pub healthy: bool,
    #[serde(default)]
    pub sources: Vec<ProbeResponse>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct VerifyResponse {
    pub contract: String,
    pub passed: usize,
    pub failed: usize,
    /// Metric views only: the fingerprint of the definition the questions
    /// ran under. Recorded by the server; what a later execute is held to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub questions: Vec<VerifiedQuestionResult>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct VerifiedQuestionResult {
    pub question: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logical_result_hash: Option<String>,
    #[serde(default)]
    pub failures: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SyncRunResponse {
    pub run_id: String,
    pub source: String,
    pub entity: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub records_read: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub records_rendered: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub records_excluded: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documents_uploaded: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documents_skipped: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count_evidence_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watermark: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<serde_json::Value>,
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MappingRunResponse {
    pub run_id: String,
    pub mapping: String,
    pub state: String,
    #[serde(default)]
    pub observations: u64,
    #[serde(default)]
    pub discrepancies: u64,
    #[serde(default)]
    pub ambiguous: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_evidence_id: Option<String>,
    #[serde(default)]
    pub findings_filed: u64,
}

/// One journal row. Parameters and results are **redacted by default**; an
/// explicit reveal is itself journaled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct JournalEntry {
    pub id: String,
    pub kind: String,
    pub tenant: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// For an `execute` row: the source's
    /// own statement window and the canonicalize+seal call. `duration_ms`
    /// minus both is Matrix's own share — bind, compile, budget, transport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seal_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_id: Option<String>,
    pub created_at: String,
    /// True when parameters/results exist but are withheld.
    #[serde(default)]
    pub redacted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct JournalListResponse {
    pub entries: Vec<JournalEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_before: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct VersionResponse {
    pub version: String,
    pub contract_version: String,
    pub role: String,
    /// The server version this build is pinned against, and whether the live
    /// server matches it.
    pub target_server_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_compatibility: Option<String>,
    /// Seconds since this process started. The first thing worth knowing when
    /// a deployment behaves as though it restarted.
    #[serde(default)]
    pub uptime_seconds: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct FreshnessReportRow {
    pub source: String,
    pub entity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watermark: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lag_seconds: Option<i64>,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct UsageReportRow {
    pub key: String,
    pub executions: u64,
    pub refusals: u64,
    pub rows: u64,
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p95_duration_ms: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_problem_carries_its_slug_in_the_type_uri() {
        let p = Problem::new(
            "source-unavailable",
            503,
            "source unavailable",
            "crm is down",
        );
        assert_eq!(p.slug(), "source-unavailable");
        assert!(p
            .problem_type
            .starts_with("https://munarium.ioka.io/problems/matrix/"));
    }
}

/// A queued job. Returned by the enqueue routes so a caller has something to
/// watch — a 202 with no id would leave an operator guessing.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct JobAccepted {
    pub accepted: usize,
    pub jobs: Vec<String>,
    pub detail: String,
}

/// `POST /v1/mappings/{name}/promote`. The decision id is the
/// operator's record — a ticket, a change number — and it is required, because
/// a promotion nobody can trace to a decision is a promotion nobody made.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PromoteRequest {
    pub decision_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `POST /v1/mappings/{name}/demote` and `.../rollback`.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct DecisionRequest {
    pub decision_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PromotionGates {
    pub identity_precision: f64,
    pub value_conformance: f64,
    pub min_identity_precision: f64,
    pub min_value_conformance: f64,
    pub observations: i64,
    pub run_id: Option<String>,
}

/// One completed run's gate values, with the verdict the CURRENT thresholds
/// would give it.
///
/// `would_pass` is computed against the thresholds in force right now, not the
/// ones in force when the run happened — which is the point. Lowering
/// `MUNARIUM_MATRIX_PROMOTION_MIN_IDENTITY_PRECISION` and re-reading this
/// series shows exactly which past runs the change would have admitted, before
/// anything is promoted under the new number.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct GateHistoryEntry {
    pub run_id: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    pub observations: i64,
    pub ambiguous: i64,
    pub nonconforming: i64,
    pub identity_precision: f64,
    pub value_conformance: f64,
    /// Signed distance from the threshold. NEGATIVE means the run failed the
    /// gate; a small positive number is a near-miss worth knowing about, and is
    /// invisible in a pass/fail column.
    pub identity_margin: f64,
    pub value_margin: f64,
    pub would_pass: bool,
}

/// `GET /v1/mappings/{name}/gate-history` — the promotion gates over time.
///
/// Exists because the owner confirmed the 0.95 identity-precision threshold
/// **with monitoring**: a number nobody can watch is a number nobody can
/// revise on evidence.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct GateHistory {
    pub mapping: String,
    pub min_identity_precision: f64,
    pub min_value_conformance: f64,
    /// Newest first.
    pub runs: Vec<GateHistoryEntry>,
    /// How many of `runs` would pass the current thresholds. A ratio far from
    /// 0 or 1 is the signal that a threshold is doing real work; 1.0 over a
    /// long series means it is not binding, and 0.0 means it is a wall.
    pub passing: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PromotionStatus {
    pub mapping: String,
    pub mode: String,
    pub promoted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promoted_version: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promoted_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gates: Option<PromotionGates>,
    pub authority_scopes: usize,
    /// The most recent reconcile pass, whatever its state — so an operator who
    /// just queued a pass can see whether it refused without reading the
    /// journal separately.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_run: Option<MappingRun>,
}

/// One reconcile pass as the store recorded it.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct MappingRun {
    pub run_id: String,
    /// `running` | `ok` | `refused`.
    pub state: String,
    pub observations: i64,
    pub discrepancies: i64,
    pub ambiguous: i64,
    pub findings_filed: i64,
    pub proposals: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RollbackResponse {
    pub mapping: String,
    pub decision_id: String,
    pub superseded: u64,
    pub skipped_no_prior: u64,
    pub already_rolled_back: u64,
    pub disputed: u64,
}
