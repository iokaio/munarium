// SPDX-License-Identifier: Apache-2.0
//! REST/JSON DTOs. One struct per wire message. Field names here are the OpenAPI
//! truth: snake_case, dotted rule ids preserved as strings. Never derive
//! ToSchema on prost types — mirror here instead.
//!
//! This crate depends on nothing of the server's: the conversions to and from
//! `munarium-core` domain types live in `munarium-api-conv` (`ToDto` / `ToCore`),
//! moved there on 2026-09-02 so the DTOs can ship in the public contract bundle.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[cfg(feature = "proto")]
pub mod wire;

// ---------------------------------------------------------------------------
// problem+json
// ---------------------------------------------------------------------------

/// RFC 9457 problem details, extended with mesh members.
/// Full registry: docs/api/errors.md.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Problem {
    /// e.g. "https://munarium.ioka.io/problems/policy-rejection"
    #[serde(rename = "type")]
    pub problem_type: String,
    pub title: String,
    pub status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate_findings: Option<Vec<GateFindingDto>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_citation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual: Option<u64>,
    /// shape-violation extension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape_ref: Option<String>,
    /// not-found extensions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

// ---------------------------------------------------------------------------
// shared mirrors of core types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClaimTypeDto {
    Fact,
    Update,
    Correction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStatusDto {
    Accepted,
    Disputed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceDto {
    Witnessed,
    Backfilled,
    Repaired,
    Emergent,
    CoverageRepair,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SeverityDto {
    Info,
    Warn,
    Block,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GateFindingDto {
    /// Dotted rule id, e.g. "gate.ledger-conflict".
    pub rule_id: String,
    pub severity: SeverityDto,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

/// Connector origin on a claim. Mirrors `munarium_core::ClaimOrigin`
/// field for field; the mirror exists so the wire type can carry `ToSchema`
/// without the kernel depending on utoipa.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ClaimOriginDto {
    pub kind: String,
    pub source_id: String,
    pub mapping_version: String,
    pub row_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_position: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ClaimDto {
    pub id: String,
    pub version_id: String,
    pub seq: u64,
    pub claim_type: ClaimTypeDto,
    pub subject: String,
    pub key: String,
    pub value: String,
    /// Canonical "subject.key=value".
    pub normalized_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_path: Option<String>,
    pub status: ClaimStatusDto,
    pub provenance: ProvenanceDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supersedes_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shape_ref: Option<String>,
    /// Connector origin; absent on model-extracted claims.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<ClaimOriginDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AnchorDto {
    pub id: String,
    pub version_id: String,
    pub detail_key: String,
    pub locked_value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked_at_scope: Option<String>,
    pub status: String,
    pub seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PromiseDto {
    pub id: String,
    pub version_id: String,
    pub key: String,
    pub kind: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_scope: Option<String>,
    /// open | fulfilled | expired | violated (the AS-OF status under a pin).
    pub status: String,
    pub seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fulfilled_seq: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ComposedContextDto {
    pub sections: Vec<SectionDto>,
    pub text: String,
    pub estimated_tokens: u64,
    pub content_hash: String,
    /// 0 = head.
    pub as_of_seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SectionDto {
    pub title: String,
    pub body: String,
}

// ---------------------------------------------------------------------------
// requests / responses
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct CreateVersionRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_version_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateVersionResponse {
    pub version_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProposeClaimRequest {
    /// Optimistic head check; omit (or 0) to skip.
    #[serde(default)]
    pub expected_head: Option<u64>,
    pub claim_type: ClaimTypeDto,
    pub subject: String,
    pub key: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_path: Option<String>,
    #[serde(default)]
    pub provenance: Option<ProvenanceDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape_ref: Option<String>,
    /// Connector origin. Optional; a connector sets it, nothing
    /// else should.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<ClaimOriginDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProposeClaimResponse {
    /// Recorded ACCEPTED or DISPUTED — blocked, never dropped.
    pub claim: ClaimDto,
    pub findings: Vec<GateFindingDto>,
    pub head_seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AppendEventsRequest {
    #[serde(default)]
    pub expected_head: Option<u64>,
    /// Batched; gated as ONE candidate unit.
    pub claims: Vec<ProposeClaimRequest>,
    /// Optional full output text for the text gates.
    #[serde(default)]
    pub candidate_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AppendEventsResponse {
    pub claims: Vec<ClaimDto>,
    pub findings: Vec<GateFindingDto>,
    pub head_seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OpenPromiseRequest {
    pub key: String,
    pub kind: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_scope: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FulfillPromiseResponse {
    pub fulfilled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LockAnchorRequest {
    pub subject: String,
    pub key: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HeadResponse {
    pub head_seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GetClaimResponse {
    pub claim: ClaimDto,
    pub superseded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FactsResponse {
    pub facts: Vec<ClaimDto>,
    /// The pin the slice was resolved at (0 = head).
    pub as_of_seq: u64,
    pub head_seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LineageResponse {
    pub version_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AnchorsResponse {
    pub anchors: Vec<AnchorDto>,
}

/// Applied chronology rules asset (POST /v1/chronology-rules, 2026-08-17 —
/// the sixth gate's arming surface).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApplyChronologyRulesResponse {
    pub name: String,
    /// Rule targets declared across order/contains/forbid_overlap/
    /// deadlines/durations — a quick sanity echo, not a validation result.
    pub rule_count: usize,
}

/// One persisted gate finding (GET /v1/versions/{id}/findings, 2026-08-17):
/// the finding plus the head seq its write settled at, so pinned reads
/// bound this store like every other.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StoredFindingDto {
    pub seq: u64,
    pub finding: GateFindingDto,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FindingsResponse {
    pub findings: Vec<StoredFindingDto>,
}

/// `POST /v1/versions/{id}/findings`: file findings a service
/// computed OUTSIDE the gates — today, Matrix's `matrix.discrepancy-candidate`.
/// Warn/info only: a `block` here is refused, because blocking is a gate
/// decision and this route is not a gate.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RecordFindingsRequest {
    pub findings: Vec<GateFindingDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RecordFindingsResponse {
    /// Findings written by this call.
    pub recorded: usize,
    /// Findings already on record with the same identity — see the route doc
    /// for what identity means — and therefore not written again.
    pub skipped_duplicates: usize,
    /// The head seq every finding in this call was stamped at.
    pub seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PromisesResponse {
    pub promises: Vec<PromiseDto>,
    /// Present only when the request asked for the overdue view
    /// (`overdue_scope=` or `final=true`): `gate.promise-unfulfilled` warn
    /// findings for open promises past their due scope, computed by the
    /// kernel's `find_overdue` over the full pinned slice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overdue_findings: Option<Vec<GateFindingDto>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RecordCountsRequest {
    pub key: String,
    pub scope_path: String,
    pub count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CounterDto {
    pub key: String,
    pub total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CountersResponse {
    pub counters: Vec<CounterDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DigestDto {
    pub version_id: String,
    pub tier: u8,
    pub scope_path: String,
    pub content: String,
    pub content_hash: String,
    pub built_from_seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DigestsResponse {
    pub digests: Vec<DigestDto>,
}

/// Generic acknowledgement for commands with no payload (counters, digests).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OkResponse {
    pub ok: bool,
}

// ---------------------------------------------------------------------------
// shapes + ingest + retrieval
// ---------------------------------------------------------------------------
// Note: Option members in the platform response DTOs deliberately serialize as
// explicit nulls (no skip_serializing_if) — the wire shape predates these
// types and clients already see the nulls.

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApplyShapeResponse {
    /// name@version
    pub shape_ref: String,
    pub yaml_hash: String,
    /// Set when the request named a version_id: the publication's ledger event.
    pub event_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PutSourceResponse {
    /// Stable identity of the source — derived from its logical path, and the
    /// handle for binding and `GET /v1/sources/{source_id}`.
    pub source_id: String,
    /// hex sha-256 — integrity of the stored bytes.
    pub content_hash: String,
    pub bytes_len: u64,
    /// True only when this path already held these exact bytes. Re-uploading
    /// a path with NEW content is an update, and reports false.
    pub already_existed: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct RecordIngestRequest {
    /// hex sha-256 of a previously PUT source. Required, validated as hex.
    #[serde(default)]
    pub content_hash: String,
    #[serde(default)]
    pub shape_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RecordIngestResponse {
    pub event_id: String,
    pub seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IndexStatusResponse {
    pub index_version: String,
    pub shape_ref: String,
    /// Ledger seq the index reflects (the envelope watermark).
    pub event_watermark: u64,
    pub active: bool,
    #[schema(value_type = Object)]
    pub manifest: serde_json::Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct SearchRequest {
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub shape_ref: Option<String>,
    /// Default 10.
    #[serde(default)]
    pub top_k: Option<u32>,
    /// None = the active index for the shape.
    #[serde(default)]
    pub index_version: Option<String>,
    /// `{"collections": ["<name-or-id>"]}` routes the search to that
    /// collection's partitioned index (exactly one). Any other filter shape
    /// is rejected as invalid-input.
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub filter: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SearchHitDto {
    pub chunk_id: String,
    /// Stable identity of the source (its logical path's id).
    pub source_id: String,
    /// The logical path — which document answered.
    pub source_path: String,
    /// hex sha-256 of the bytes that path held at index time.
    pub source_content_hash: String,
    pub text: String,
    pub score: f64,
    pub lexical_rank: Option<u32>,
    pub vector_rank: Option<u32>,
    /// Raw lexical-leg relevance (`ts_rank`) — magnitude-comparable across
    /// collections; absent when the hit had no lexical match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lexical_score: Option<f64>,
    /// Raw vector-leg cosine distance (lower = closer); absent when the hit
    /// was outside the vector candidate window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_distance: Option<f64>,
    #[schema(value_type = Option<Object>)]
    pub metadata: Option<serde_json::Value>,
}

/// Every retrieval answer carries one — surfaced, never hidden.
///
/// Sources are named three ways deliberately: ids are stable identity, paths
/// say *which document* answered, and hashes prove *which bytes* it held.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProvenanceEnvelopeDto {
    pub chunk_ids: Vec<String>,
    /// Stable identity of every source the answer drew on.
    pub source_ids: Vec<String>,
    /// The logical paths of those sources.
    pub source_paths: Vec<String>,
    /// hex sha-256 of every source the answer drew on.
    pub source_content_hashes: Vec<String>,
    pub index_version: String,
    /// Ledger seq the index reflects.
    pub event_watermark: u64,
    /// Embedding provider/model/dims, when applicable.
    pub provider_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SearchResponse {
    pub hits: Vec<SearchHitDto>,
    pub envelope: ProvenanceEnvelopeDto,
}

// ---------------------------------------------------------------------------
// providers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApplyProviderConfigResponse {
    pub config_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProviderHealthResponse {
    pub healthy: bool,
    /// The provider family (anthropic | openai | openrouter).
    pub provider: String,
    pub endpoint_fingerprint: String,
    /// Key validity / reachability detail — never key material.
    pub detail: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct CompleteRequest {
    /// Explicit model id — any model the selected provider supports. None =
    /// tier default (when `tier` set) or the config's first complete model.
    #[serde(default)]
    pub model: Option<String>,
    /// Provider family override (anthropic | openai | openrouter). Only
    /// honored on the reserved `default` config name; combines with the
    /// default rule when unset.
    #[serde(default)]
    pub provider: Option<String>,
    /// Model tier: `fast` (lesser model) or `capable`. Ignored when `model`
    /// is set explicitly.
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub system: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    /// Default 512.
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f64>,
    /// When set, the invocation is recorded as a ledger event in this lineage.
    #[serde(default)]
    pub version_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CompleteResponse {
    pub text: String,
    pub stop_reason: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// The provider family that served the request.
    pub provider: String,
    /// The resolved model id that served the request.
    pub model: String,
    /// Set when the request named a version_id.
    pub invocation_event_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct EmbedRequest {
    /// None = the config's first embed model.
    #[serde(default)]
    pub model: Option<String>,
    /// Provider family override (anthropic | openai | openrouter); only
    /// honored on the reserved `default` config name.
    #[serde(default)]
    pub provider: Option<String>,
    /// Required, non-empty.
    #[serde(default)]
    pub inputs: Vec<String>,
    #[serde(default)]
    pub version_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EmbedResponse {
    pub vectors: Vec<Vec<f32>>,
    pub dimensions: u64,
    /// True when served from the request-hash embedding cache.
    pub cache_hit: bool,
    /// The provider family that served the request.
    pub provider: String,
    /// The resolved model id that served the request.
    pub model: String,
    pub invocation_event_id: Option<String>,
}

/// One /healthai probe: a small live completion against one provider/tier.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HealthAiCheck {
    /// Provider family (anthropic | openai | openrouter).
    pub provider: String,
    /// Tier probed (fast | capable).
    pub tier: String,
    /// The model id probed.
    pub model: String,
    /// True when the model answered the probe.
    pub ok: bool,
    /// True when the probe was skipped because no credential is configured.
    pub skipped: bool,
    pub latency_ms: Option<u64>,
    /// Outcome detail — never key material, never response bodies.
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HealthAiResponse {
    /// True when every configured provider's probes succeeded and at least
    /// one provider credential is configured.
    pub healthy: bool,
    pub checks: Vec<HealthAiCheck>,
}

// ---------------------------------------------------------------------------
// runbooks
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApplyRunbookResponse {
    pub runbook_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RunbookRunResponse {
    pub run_id: String,
    /// running | awaiting_approval | done | failed
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RunbookStepDto {
    pub ordinal: u32,
    pub name: String,
    /// pending | running | awaiting_approval | done | failed
    pub state: String,
    #[schema(value_type = Option<Object>)]
    pub detail: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RunStatusResponse {
    pub run_id: String,
    pub runbook_ref: String,
    pub state: String,
    pub version_id: Option<String>,
    pub steps: Vec<RunbookStepDto>,
}

// ---------------------------------------------------------------------------
// runbook v2 surface
// ---------------------------------------------------------------------------

/// One collection a runbook spans, with its access requirements — the unit
/// of compartmentalization a caller must clear to see results from it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RunbookCollectionDto {
    pub name: String,
    /// The materialized collection id; None until the runbook is applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection_id: Option<String>,
    pub shape_ref: String,
    pub access_level: i32,
    pub compartments: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_index: Option<String>,
    pub source_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RunbookSummaryDto {
    /// name@version
    pub runbook_ref: String,
    pub name: String,
    pub version: u32,
    /// active | remove_requested | removed
    pub status: String,
    /// The minimum access level that sees ANY of this runbook's collections.
    pub min_access_level: i32,
    pub collections: Vec<RunbookCollectionDto>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RunbooksResponse {
    pub runbooks: Vec<RunbookSummaryDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RunbookInfoResponse {
    pub runbook_ref: String,
    pub name: String,
    pub version: u32,
    pub status: String,
    pub collections: Vec<RunbookCollectionDto>,
    /// Sibling versions of the same name (refs), including this one.
    pub versions: Vec<String>,
    /// The models block (defaults per task level + override policy), echoed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub models: Option<serde_json::Value>,
    /// Retrieval knobs in effect.
    #[schema(value_type = Object)]
    pub retrieval: serde_json::Value,
    /// Whether session turns can run a RAG completion step.
    pub has_completion: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ValidationFindingDto {
    /// error | warn | info
    pub severity: String,
    /// Stable dotted code, e.g. "steps.cutover-before-build".
    pub code: String,
    pub message: String,
    pub path: String,
}

/// AI-assisted improvement suggestion (advisory only).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SuggestionDto {
    pub title: String,
    pub rationale: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ValidateRunbookResponse {
    /// False when any error-severity finding is present.
    pub valid: bool,
    pub findings: Vec<ValidationFindingDto>,
    /// Present when ?suggest=true and a provider is configured.
    #[serde(default)]
    pub suggestions: Vec<SuggestionDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggest_note: Option<String>,
}

/// API-level model override (session turns; validate endpoint uses query
/// params of the same shape). Honored only when the runbook's
/// models.allowOverrides policy permits the provider.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct ModelOverrideDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// fast | capable | frontier
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
}

// ---------------------------------------------------------------------------
// collections
// ---------------------------------------------------------------------------

/// Create-or-update a compartmentalized data collection. There is no delete
/// API anywhere — collections retire softly; physical deletion is the DBA
/// runbook (docs/ops/index-deletion-runbook.md).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateCollectionRequest {
    /// Tenant-unique name (stable handle used by runbooks).
    pub name: String,
    /// Shape governing this collection's sources (immutable after creation).
    pub shape_ref: String,
    /// Access level a token must dominate to search this collection.
    #[serde(default)]
    pub access_level: i32,
    /// Need-to-know tags; a token must carry all of them.
    #[serde(default)]
    pub compartments: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CollectionDto {
    pub id: String,
    pub name: String,
    pub shape_ref: String,
    pub access_level: i32,
    pub compartments: Vec<String>,
    /// active | retired
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_at: String,
    /// Sources currently bound to this collection.
    pub source_count: i64,
    /// The active index version id, if one has been cut over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_index: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CollectionsResponse {
    pub collections: Vec<CollectionDto>,
}

// ---------------------------------------------------------------------------
// reporting + governance
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UsageRow {
    /// The grouping key value (a uid, session id, runbook ref, or collection id).
    pub key: String,
    /// API interactions attributed to the key in the window.
    pub interactions: i64,
    /// Session turns attributed to the key in the window.
    pub turns: i64,
    /// Completion token spend (sums over turns that ran a completion).
    pub completion_input_tokens: i64,
    pub completion_output_tokens: i64,
    pub avg_latency_ms: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UsageResponse {
    /// uid | session | runbook | collection
    pub group_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    pub rows: Vec<UsageRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AuditEntryDto {
    pub id: String,
    pub uid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub plane: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runbook_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_jti: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub request: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub response: Option<serde_json::Value>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AuditResponse {
    pub entries: Vec<AuditEntryDto>,
    /// Keyset cursor for the next (older) page: pass it back as `before`.
    /// Present only when this page was full — absence means the trail is
    /// exhausted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_before: Option<String>,
}

/// Model-spend rollup: token totals per resolved provider/model, split by
/// whether an API override chose the model. (Dollar pricing lives upstream —
/// the server reports the token facts.)
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CostRow {
    pub provider: String,
    pub model: String,
    pub turns: i64,
    pub overridden_turns: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CostResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    pub rows: Vec<CostRow>,
}

/// One (provider config × tier) row of today's spending-cap ledger
/// (GET /v1/reports/budgets). Usage comes from the enforcer's own window
/// expression, so this report can never disagree with the ceiling; a
/// configured cap with no usage yet still gets a row. Token facts only,
/// like CostRow.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BudgetRow {
    /// Provider config name (`demo-anthropic`, `default-openai`, …).
    pub config: String,
    /// fast | capable | frontier
    pub tier: String,
    /// The UTC day the row counts, `YYYY-MM-DD`.
    pub day: String,
    /// Tokens reserved by in-flight calls (estimates, not yet settled).
    pub held_tokens: i64,
    /// Tokens settled (provider actuals where reported, else estimates).
    pub settled_tokens: i64,
    pub reservations: i64,
    /// The configured daily ceiling; absent = this scope is unlimited (rows
    /// exist for unlimited scopes only when they saw capped traffic earlier
    /// in the day, before a config change).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Tokens left under the ceiling; absent when unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BudgetReportResponse {
    pub rows: Vec<BudgetRow>,
}

/// One time bucket of the traffic timeseries (GET /v1/reports/timeseries).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TimeseriesBucket {
    /// Bucket start, RFC 3339 UTC.
    pub bucket: String,
    pub requests: i64,
    pub errors_4xx: i64,
    pub errors_5xx: i64,
    pub p50_latency_ms: Option<f64>,
    pub p95_latency_ms: Option<f64>,
}

/// Bucketed request/error/latency series over the interactions audit trail.
/// Aggregates across every instance writing to the shared database, so a
/// cluster reads as one series.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TimeseriesResponse {
    /// 1h | 24h | 7d | 30d
    pub window: String,
    /// Bucket width the window resolved to.
    pub bucket_seconds: i64,
    /// rest | grpc when the query filtered by plane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plane: Option<String>,
    pub buckets: Vec<TimeseriesBucket>,
}

/// Per-endpoint traffic row (GET /v1/reports/endpoints). `method` is the
/// recorded interaction method string ("GET /v1/versions/{id}/facts" style
/// raw paths — the audit trail stores what was called, not the template).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EndpointRow {
    pub method: String,
    pub requests: i64,
    /// Fraction of requests with status >= 400.
    pub error_rate: f64,
    pub avg_latency_ms: Option<f64>,
    pub p95_latency_ms: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EndpointsResponse {
    pub window: String,
    pub rows: Vec<EndpointRow>,
}

/// Runbook run counts by state, with mean wall time (run creation to the
/// last step update) for the window.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RunbookRunsRow {
    pub state: String,
    pub runs: i64,
    pub avg_wall_ms: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RunbookStepsRow {
    pub state: String,
    pub steps: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RunbookReportResponse {
    pub window: String,
    pub runs: Vec<RunbookRunsRow>,
    pub steps: Vec<RunbookStepsRow>,
}

/// One time bucket of session activity (GET /v1/reports/sessions).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SessionsBucket {
    pub bucket: String,
    pub sessions_opened: i64,
    pub turns: i64,
    /// Distinct uids that took a turn in the bucket.
    pub active_uids: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SessionsReportResponse {
    pub window: String,
    pub bucket_seconds: i64,
    pub buckets: Vec<SessionsBucket>,
}

/// One layer's aggregate behaviour over the report window.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EvidenceLayerStatsDto {
    pub profile: String,
    pub layer: String,
    pub turns: i64,
    /// Turns where this layer refused.
    pub refusals: i64,
    /// Turns where this layer could support a completeness claim.
    pub complete: i64,
    /// Refusal codes seen, most frequent first.
    pub refusal_codes: Vec<String>,
    pub p50_ms: i64,
    pub p95_ms: i64,
}

/// How the evidence hierarchy actually behaved (`GET /v1/reports/evidence`).
///
/// The operational question this answers is "which layer is quietly refusing?"
/// A layer that refuses on most turns is either misconfigured or pointed at
/// something that is down, and either way the answers being served are
/// thinner than the runbook claims — while every one of those turns still
/// returns 200.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EvidenceReportResponse {
    pub window: String,
    /// Turns that ran a research profile.
    pub hierarchy_turns: i64,
    /// Turns on the legacy document path.
    pub legacy_turns: i64,
    /// Hierarchy turns where at least one layer could support a completeness
    /// claim.
    pub completeness_available: i64,
    pub layers: Vec<EvidenceLayerStatsDto>,
}

/// Munarium Matrix's health as this server sees it
/// (`GET /v1/reports/matrix`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MatrixReportResponse {
    /// False when MUNARIUM_MATRIX_BASE_URL is unset — the plane is not wired,
    /// which is different from wired-and-failing and must not read the same.
    pub configured: bool,
    /// Per-instance circuit-breaker state. Deliberately NOT per tenant: the
    /// breaker is shared, so a per-tenant reading would report a fact that
    /// does not exist.
    pub circuit_open: bool,
    pub consecutive_failures: u64,
    /// Data views declared across the tenant's applied runbooks.
    pub data_views: Vec<MatrixDataViewDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MatrixDataViewDto {
    pub runbook_ref: String,
    pub name: String,
    pub contract: String,
    pub access_level: i32,
}

/// One issued capability token (audit view — never the token material).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TokenInfoDto {
    pub jti: String,
    pub uid: String,
    pub access_level: i32,
    pub compartments: Vec<String>,
    pub scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runbook_refs: Option<Vec<String>>,
    pub issued_by: String,
    pub issued_at: String,
    pub expires_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TokensResponse {
    pub tokens: Vec<TokenInfoDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RevokeTokenResponse {
    pub jti: String,
    pub revoked: bool,
    /// The deny-list is only consulted when MUNARIUM_TOKEN_REVOCATION_CHECK=true.
    pub revocation_check_enabled: bool,
}

// ---------------------------------------------------------------------------
// ingestion
// ---------------------------------------------------------------------------

/// One file for the ingest plane. Content is base64 (JSON-safe); the
/// declared sha256, when present, is verified before commit (same
/// content-addressing contract as PUT /v1/sources).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IngestFileRequest {
    pub filename: String,
    pub media_type: String,
    pub content_base64: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Explicit collection names to bind into. Absent = auto-bind via the
    /// declarative `sources:` matchers of every active runbook the token may
    /// reach. Either way the token's level/compartments must permit each
    /// target collection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collections: Option<Vec<String>>,
}

/// Where a document actually went. Metadata only — never the bytes.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SourceInfoDto {
    pub source_id: String,
    /// The logical path: identity, and the blob name under the tenant prefix.
    pub filename: String,
    pub media_type: String,
    /// hex sha-256 — integrity of the stored bytes.
    pub content_hash: String,
    pub bytes_len: u64,
    /// `az` | `pg` | `mem`.
    pub storage_backend: String,
    /// Backend-resolved URI. Never carries a SAS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_uri: Option<String>,
    /// NULL until first indexed, then ok | empty | failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extraction_status: Option<String>,
    /// text | docx | pdf-text | ocr — OCR'd text is not equivalent evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extraction_method: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IngestResultDto {
    pub filename: String,
    /// Stable identity of the stored source — the handle for binding and for
    /// `GET /v1/sources/{source_id}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// True only when this path already held these exact bytes — a genuine
    /// idempotent replay. Re-uploading a path with NEW content reports false,
    /// because a rebuild is now owed.
    pub existed: bool,
    /// Collections this file is now bound to (from this call).
    pub bound_to: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IngestBatchRequest {
    pub files: Vec<IngestFileRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IngestBatchResponse {
    pub results: Vec<IngestResultDto>,
}

// ---------------------------------------------------------------------------
// bulk upload sessions — chunked, resumable corpus loading
// ---------------------------------------------------------------------------

/// One manifest entry: what the client intends to upload. `sha256` is the
/// declared content hash (hex) verified against every received chunk file;
/// the diff against already-stored sources also compares it, so an identical
/// re-run needs no bytes at all.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BulkManifestEntry {
    pub filename: String,
    pub sha256: String,
    pub bytes_len: u64,
    pub media_type: String,
}

/// Open a bulk upload session with the full manifest.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BulkOpenRequest {
    pub files: Vec<BulkManifestEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BulkOpenResponse {
    pub bulk_id: String,
    pub total: u64,
    /// Manifest entries whose logical path already holds these exact bytes —
    /// nothing to upload for these.
    pub already_present: u64,
    /// Filenames still owed bytes (the client's upload work list).
    pub needed: Vec<String>,
}

/// One chunk of files for an open session. The envelope and limits match
/// `POST /v1/ingest/batch` (at most 500 files, base64 content).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BulkChunkRequest {
    pub files: Vec<IngestFileRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BulkChunkResponse {
    pub bulk_id: String,
    /// Per-file outcomes, same shape as batch ingest.
    pub results: Vec<IngestResultDto>,
    pub stored: u64,
    pub skipped_existing: u64,
    pub pending: u64,
    pub failed: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BulkFileErrorDto {
    pub filename: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BulkStatusResponse {
    pub bulk_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// open | completed | expired
    pub status: String,
    pub total: u64,
    pub stored: u64,
    pub skipped_existing: u64,
    pub pending: u64,
    pub failed: u64,
    /// Failed entries with their last error (capped at 100).
    pub failures: Vec<BulkFileErrorDto>,
    /// Filenames still owed bytes. Populated only when the request asks for
    /// it (`?include_needed=true`) — for a large manifest this is the resume
    /// work list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needed: Option<Vec<String>>,
    pub created_at: String,
    pub expires_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BulkCompleteResponse {
    pub bulk_id: String,
    /// completed | incomplete — incomplete leaves the session open.
    pub status: String,
    pub total: u64,
    pub stored: u64,
    pub skipped_existing: u64,
    /// Manifest entries with no stored bytes (capped at 100; see counts).
    pub missing: Vec<String>,
    pub missing_count: u64,
    /// Entries whose stored content hash no longer matches the manifest —
    /// the path was overwritten with different bytes after this session
    /// declared it (capped at 100).
    pub mismatched: Vec<String>,
    pub mismatched_count: u64,
}

// ---------------------------------------------------------------------------
// runbook removal — double-pass, soft only
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RemovalRequestResponse {
    pub runbook_ref: String,
    /// Present this id to /remove-confirm within the TTL.
    pub removal_id: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RemovalConfirmRequest {
    pub removal_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RemovalConfirmResponse {
    pub runbook_ref: String,
    /// Always "removed" on success. The yaml, run history, collections, and
    /// index data are all retained — removal is visibility-only.
    pub status: String,
}

// ---------------------------------------------------------------------------
// sessions + turns
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateSessionResponse {
    pub session_id: String,
    /// The pinned name@version this session will use for every turn.
    pub runbook_ref: String,
    /// Collections the caller's access level/compartments permit — the
    /// least-privilege echo so a client knows what it can see.
    pub permitted_collections: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct TurnRequest {
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    /// Run the runbook's completion step (when the spec declares one).
    #[serde(default)]
    pub complete: Option<bool>,
    /// Model override — honored only under the runbook's allowOverrides policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_override: Option<ModelOverrideDto>,
    /// Run this turn through a named research profile (an evidence
    /// hierarchy) instead of the single-layer document path.
    ///
    /// Absent, and with no `retrieval.defaultResearchProfile` on the runbook,
    /// the turn executes and serializes EXACTLY as it always has — same
    /// retrieval, same response shape, same SSE sequence. That is the
    /// governing invariant of S-3.x, and it is what makes this field safe to
    /// add to a wire contract every existing client already speaks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research_profile: Option<String>,
}

/// What one evidence layer produced.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LayerOutcomeDto {
    pub layer: String,
    /// `supporting` | `primary` | `controlling`.
    pub role: String,
    /// `required` | `optional` | `fallback`.
    pub requirement: String,
    /// `document_hits` | `complete_table` | `count` | `fact_slice` | `refusal`.
    pub block: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_id: Option<String>,
    /// Whether an answer may make a completeness claim on THIS layer.
    /// Document hits are always false: retrieval returns what it found, never
    /// a proof that nothing else exists.
    pub supports_completeness: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal_code: Option<String>,
    pub elapsed_ms: u64,
}

/// Why the model saw what it saw. Deliberately about the DECISION,
/// not the content: which profile, which layers ran, which refused, whether a
/// completeness claim was permissible at all. No evidence rows appear here.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EvidenceHierarchyDecisionDto {
    pub profile: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_kind: Option<String>,
    /// True when the caller supplied the intent rather than a model producing
    /// it — so a keyless test result never reads as a planner result.
    pub intent_explicit: bool,
    pub layers: Vec<LayerOutcomeDto>,
    pub completeness_available: bool,
    #[serde(default)]
    pub disclosed_conflicts: u32,
    pub conflicts_policy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TurnHitDto {
    /// Which collection this hit came from.
    pub collection: String,
    pub chunk_id: String,
    /// Stable identity of the source document.
    pub source_id: String,
    /// The logical path — which document answered this turn.
    pub source_path: String,
    pub source_content_hash: String,
    pub text: String,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CollectionEnvelopeDto {
    pub collection: String,
    pub envelope: ProvenanceEnvelopeDto,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TurnCompletionDto {
    pub provider: String,
    pub model: String,
    /// Whether an API model override decided the provider/model.
    pub was_override: bool,
    pub text: String,
    /// Token totals across ALL completions this turn paid for, including
    /// verification retries (2026-08-18).
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Present when the runbook declares `completion.verification`
    /// (2026-08-18): the deterministic check outcome. Non-empty final
    /// `violations` mean the answer stands UNVERIFIED after the retry
    /// budget — the caller decides what that means for display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<TurnVerificationDto>,
}

/// Deterministic turn-verification outcome (the measured grounding checks,
/// server-side — quotes resolve in served text, citations name served
/// content). Violations are prefixed `quote: ` / `citation: `.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TurnVerificationDto {
    /// Which checks ran (`quotes`, `citations`).
    pub checks: Vec<String>,
    /// Corrective completions actually spent (each is a paid call).
    pub retries: u32,
    /// Violations found on the FIRST answer.
    pub first_pass_violations: Vec<String>,
    /// Violations remaining on the FINAL answer (empty = verified).
    pub violations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TurnResponse {
    pub session_id: String,
    pub ordinal: u32,
    /// Collections actually searched (post access filtering; collections
    /// without an active index are skipped and listed in `skipped`).
    pub collections_searched: Vec<String>,
    #[serde(default)]
    pub skipped: Vec<String>,
    pub hits: Vec<TurnHitDto>,
    pub envelopes: Vec<CollectionEnvelopeDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<TurnCompletionDto>,
    /// Present only when a research profile ran. `skip_serializing_if`
    /// is load-bearing — a legacy turn's JSON must not grow a key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hierarchy: Option<EvidenceHierarchyDecisionDto>,
}

/// One progress event on the streaming turn plane
/// (`POST /v1/sessions/{id}/turns/stream`, SSE). Each event names its stage;
/// the stream ends with an SSE `done` event carrying the full [`TurnResponse`]
/// (or an `error` event carrying the problem+json body).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum TurnProgressEvent {
    /// One collection probed with the original query (only with
    /// `retrieval.collectionSelection`; emitted per permitted collection as
    /// each probe completes, so the stream carries bytes from the turn's
    /// first hundred milliseconds instead of after the whole probe — a
    /// many-shard probe on a loaded database took minutes and the response's
    /// first byte with it, past the ingress timeout, 2026-08-25).
    Probe {
        collection: String,
        hits: u32,
        /// True when the collection has no active index and was skipped.
        skipped: bool,
    },
    /// Evidence-driven collection selection ran (only when the runbook
    /// declares `retrieval.collectionSelection`): every permitted collection
    /// was probed with the original query and the strongest `selected` got
    /// the deep, expanded search. The `retrieval` events that follow cover
    /// only the selected collections; the others' probe pools still enter
    /// the merge (selection spends the deep search, it does not exclude),
    /// so `collections_searched` on the response lists them all.
    Selection {
        probed: u32,
        selected: u32,
        /// The selected collections, in the runbook's order.
        collections: Vec<String>,
    },
    /// The runbook's `retrieval.modelQueryExpansion` step returned (one paid
    /// fast-tier call). `terms` are the accepted lexical variants — possibly
    /// empty, in which case the original query searched alone.
    Expansion {
        provider: String,
        model: String,
        terms: Vec<String>,
        input_tokens: u64,
        output_tokens: u64,
    },
    /// One collection searched (emitted per searched collection, in order).
    Retrieval {
        collection: String,
        hits: u32,
        /// True when the collection has no active index and was skipped.
        skipped: bool,
    },
    /// RRF merge across the searched collections is done.
    Merge { hits: u32 },
    /// The completion model resolved (policy-checked, pre-call). `model` is
    /// None when only a tier was resolved — the concrete id then appears on
    /// the `completion` event.
    Model {
        provider: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tier: Option<String>,
        was_override: bool,
    },
    /// One paid completion returned (the first answer, and each corrective
    /// retry). `attempt` is 0 for the first answer, 1.. for retries.
    Completion {
        attempt: u32,
        provider: String,
        model: String,
        input_tokens: u64,
        output_tokens: u64,
    },
    /// Deterministic verification ran over the current answer. Non-zero
    /// `violations` may trigger a corrective retry (each retry is a paid
    /// call).
    Verify {
        attempt: u32,
        checks: Vec<String>,
        violations: u32,
        /// Which layer's evidence this check ran against, when a
        /// hierarchy is in play. Absent on a legacy turn, so its `verify`
        /// event serializes byte-identically.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        layer: Option<String>,
    },
    /// The hierarchy stages. Emitted ONLY when a research profile
    /// runs, appended after the legacy variants so an existing turn's event
    /// sequence is unchanged.
    ///
    /// The resolved plan, before any layer executes.
    Profile {
        profile: String,
        layers: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        intent_kind: Option<String>,
        intent_explicit: bool,
    },
    /// A layer began.
    LayerStart {
        layer: String,
        role: String,
        requirement: String,
    },
    /// One source within a layer answered.
    LayerSource {
        layer: String,
        source: String,
        /// The provider that served it: `documents` | `facts` | `matrix`.
        provider: String,
    },
    /// A layer finished, with what it produced.
    LayerComplete {
        layer: String,
        block: String,
        supports_completeness: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        refusal_code: Option<String>,
        elapsed_ms: u64,
    },
    /// Whether ANY layer can support a completeness claim, and how many
    /// cross-layer conflicts were preserved for disclosure.
    Coverage {
        completeness_available: bool,
        disclosed_conflicts: u32,
    },
    /// The hierarchy's blocks were composed into the model's context.
    Compose {
        layers_used: u32,
        context_chars: u32,
        /// Layers dropped because they did not fit the budget. A
        /// `preserveCompleteResult` layer is dropped WHOLE or kept whole —
        /// half a table is not a smaller true answer, it is a false one.
        #[serde(default)]
        layers_dropped: Vec<String>,
    },
}

/// One provider config's resolved tier models — free introspection, no
/// provider calls (`GET /v1/providers`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProviderModelsDto {
    /// Config name (`demo-anthropic`, …) or `default-<family>` for the
    /// synthesized env-backed default.
    pub name: String,
    /// Provider family (`anthropic` | `openai` | `openrouter`).
    pub provider: String,
    /// `applied` (tenant-applied config) or `default` (synthesized from the
    /// conventional env var).
    pub source: String,
    /// Whether the config's credential currently resolves. Never the key.
    pub credential_ok: bool,
    /// Concrete model the `fast` tier resolves to for this config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast: Option<String>,
    /// Concrete model the `capable` tier resolves to for this config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capable: Option<String>,
    /// Concrete model the `frontier` tier resolves to for this config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frontier: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProviderListResponse {
    pub providers: Vec<ProviderModelsDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SessionTurnDto {
    pub ordinal: u32,
    pub query: String,
    pub collections_searched: Vec<String>,
    #[schema(value_type = Object)]
    pub hits: serde_json::Value,
    #[schema(value_type = Object)]
    pub envelope: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub completion: Option<serde_json::Value>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SessionResponse {
    pub session_id: String,
    pub uid: String,
    pub runbook_ref: String,
    pub access_level: i32,
    pub compartments: Vec<String>,
    pub state: String,
    pub created_at: String,
    pub turns: Vec<SessionTurnDto>,
}

// ---------------------------------------------------------------------------
// access tokens
// ---------------------------------------------------------------------------

/// Management-plane request to mint a capability JWT for an end user.
/// Callable only with a `mgmt` static token — see docs/security-posture.md.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IssueTokenRequest {
    /// End-user id the API manager authenticated (becomes the JWT `sub`;
    /// must match X-Munarium-Uid on every call made with the token).
    pub uid: String,
    /// Hierarchical access level (0..N, higher sees more).
    pub access_level: i32,
    /// Optional need-to-know compartment tags.
    #[serde(default)]
    pub compartments: Vec<String>,
    /// Capabilities: "query" and/or "ingest".
    pub scopes: Vec<String>,
    /// Optional runbook NAME allowlist; absent = any runbook the level permits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runbook_refs: Option<Vec<String>>,
    /// Optional TTL override; clamped to the server's 24 h ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IssueTokenResponse {
    /// The signed JWT. Never persisted server-side; treat as a secret.
    pub token: String,
    /// Token id — the audit/revocation key.
    pub jti: String,
    /// RFC 3339 expiry.
    pub expires_at: String,
}

// ---------------------------------------------------------------------------
// meta
// ---------------------------------------------------------------------------

/// GET /version — the served name + workspace version (unauthenticated
/// meta). Clients compare `version` against their TARGET_SERVER_VERSION to
/// catch a stale deploy early (the C5-addendum lesson).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct VersionInfo {
    pub name: String,
    pub version: String,
    /// Plane -> port map (documentation of the demo posture, not config).
    #[schema(value_type = Object)]
    pub planes: serde_json::Value,
}

// ---------------------------------------------------------------------------
// guided authoring (drafts, patterns, bundles)
// ---------------------------------------------------------------------------

/// One application pattern, summarized for the catalog listing.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PatternSummaryDto {
    pub id: String,
    pub name: String,
    pub description: String,
    /// The committed exemplar runbook to start from.
    pub start_from: String,
    /// What this pattern is strongest at, and the failure mode to design against.
    pub guidance: String,
    pub has_completion: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PatternsResponse {
    pub patterns: Vec<PatternSummaryDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NamedYamlDto {
    pub name: String,
    pub yaml: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PatternDetailResponse {
    pub id: String,
    pub name: String,
    pub description: String,
    pub start_from: String,
    /// What this pattern is strongest at, and the failure mode to design against.
    pub guidance: String,
    pub has_completion: bool,
    /// Design notes the deterministic validator cannot police.
    pub decision_notes: Vec<String>,
    /// The exemplar runbook, verbatim.
    pub runbook_yaml: String,
    /// The exemplar's shape dependencies, verbatim.
    pub shapes: Vec<NamedYamlDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateDraftRequest {
    /// Runbook name: ^[a-z0-9][a-z0-9-]*$ ('@' is the ref separator).
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern_id: Option<String>,
    /// Copy the pattern's exemplar documents into the draft (renamed)
    /// instead of starting from interview materialization.
    #[serde(default)]
    pub seed_from_exemplar: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct InterviewQuestionDto {
    pub id: String,
    pub prompt: String,
    pub guidance: String,
    /// string | text | int | bool | enum | areas | fields | map
    pub kind: String,
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Object)]
    pub default: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    /// Documentation of the slot this answer lands in.
    pub maps_to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct InterviewSectionDto {
    pub id: String,
    pub title: String,
    /// The document section that teaches this decision in full.
    pub doc_ref: String,
    pub questions: Vec<InterviewQuestionDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DraftDocumentDto {
    /// Path within the set, e.g. "runbooks/<name>.yaml".
    pub path: String,
    /// Shape | Runbook
    pub kind: String,
    pub yaml: String,
    /// sha256 hex of the YAML bytes.
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DocumentFindingsDto {
    pub path: String,
    pub findings: Vec<ValidationFindingDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DraftValidationResponse {
    /// False when any error-severity finding exists anywhere in the set.
    pub valid: bool,
    /// Per-document findings (parse + the document's own validator).
    pub documents: Vec<DocumentFindingsDto>,
    /// Cross-document findings (set.* codes).
    pub set_findings: Vec<ValidationFindingDto>,
    /// What still needs answering ("red TODOs expected" on a fresh draft).
    #[serde(default)]
    pub todos: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DraftSummaryDto {
    pub draft_id: String,
    pub name: String,
    /// interview | drafted | validated | exported (progress display only —
    /// export and apply always re-validate inline).
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern_id: Option<String>,
    pub created_by: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DraftsResponse {
    pub drafts: Vec<DraftSummaryDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DraftResponse {
    pub draft_id: String,
    pub name: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern_id: Option<String>,
    /// Flat map keyed by interview question id.
    #[schema(value_type = Object)]
    pub answers: serde_json::Value,
    /// The interview for this draft (completion section is pattern-gated).
    pub interview: Vec<InterviewSectionDto>,
    pub documents: Vec<DraftDocumentDto>,
    /// Fresh validation of the current documents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation: Option<DraftValidationResponse>,
    #[serde(default)]
    pub todos: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assist_note: Option<String>,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UpdateAnswersRequest {
    /// Flat map keyed by interview question id; replaces the stored answers.
    #[schema(value_type = Object)]
    pub answers: serde_json::Value,
    /// Re-materialize documents from the answers (default true). Pass false
    /// to store answers without touching documents (e.g. a seeded or
    /// assist-edited draft).
    #[serde(default = "default_true")]
    pub materialize: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DraftDeleteResponse {
    pub draft_id: String,
    /// Always "deleted" (soft; the row is retained).
    pub status: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct AssistDraftRequest {
    /// Corpus description for the drafting pass; defaults to the
    /// identity.description answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Extra instructions ("split the finance area", "tighten the template").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// Provider config name; "default" engages the tenant fallback chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// fast | capable | frontier
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AssistDraftResponse {
    /// The documents after the assist pass (unchanged when the pass degraded).
    pub documents: Vec<DraftDocumentDto>,
    #[serde(default)]
    pub suggestions: Vec<SuggestionDto>,
    /// Set when the pass degraded (no provider, budget, parse failure) —
    /// assist NEVER fails the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assist_note: Option<String>,
    pub validation: DraftValidationResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BundleToolDto {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BundleValidationDto {
    pub valid: bool,
    pub errors: u64,
    pub warns: u64,
    pub infos: u64,
}

/// The export bundle: self-contained, hash-manifested, applied to any
/// instance via the existing /v1/shapes + /v1/runbooks routes in
/// apply_order. `manifest_hash` = sha256 over the byte-sorted
/// "path\0hash\n" lines.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ExportDraftResponse {
    /// Always "MunariumAuthoringBundle".
    pub kind: String,
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub tool: BundleToolDto,
    pub draft_id: String,
    pub name: String,
    pub created_at: String,
    /// path -> YAML, verbatim.
    #[schema(value_type = Object)]
    pub files: std::collections::BTreeMap<String, String>,
    /// path -> sha256 hex.
    #[schema(value_type = Object)]
    pub hashes: std::collections::BTreeMap<String, String>,
    /// Shapes before runbooks.
    pub apply_order: Vec<String>,
    pub manifest_hash: String,
    pub validation: BundleValidationDto,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AppliedDocDto {
    pub path: String,
    /// Shape | Runbook
    pub kind: String,
    /// shape_ref or runbook_ref (name@version).
    pub r#ref: String,
    /// sha256 of the applied YAML.
    pub yaml_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApplyDraftResponse {
    pub applied: Vec<AppliedDocDto>,
}

// ---------------------------------------------------------------------------
// sealed evidence
// ---------------------------------------------------------------------------

/// `POST /v1/evidence` — seal an artifact.
///
/// Two forms in one route, distinguished by whether `bytes_base64` is present:
///
/// - **inline** (bytes present, at or under the 1 MiB cap): manifest and bytes
///   arrive together and commit atomically in one round-trip. This is the
///   common case — a query result backing one answer — and keeping it to a
///   single call is why the mode-B seal path does not cost a turn two
///   round-trips.
/// - **grant** (bytes absent): the server registers the manifest, assigns an
///   id, and returns a short-lived single-use grant to `PUT` the bytes. For
///   sync-run counts and observation batches, which are large.
///
/// The manifest is the vendored contract's `EvidenceManifest`
/// (`contract/matrix/evidence-manifest.schema.json`), carried verbatim.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SealEvidenceRequest {
    /// The contract manifest. Deliberately untyped at the DTO layer: it is
    /// deserialized into `munarium_core::evidence::EvidenceManifest` and
    /// validated there, so the contract has exactly one Rust mirror rather
    /// than a DTO copy that could drift from it.
    pub manifest: serde_json::Value,
    /// Standard base64 of the artifact bytes. Absent selects the grant flow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_base64: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EvidenceGrantDto {
    pub grant_id: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SealEvidenceResponse {
    pub evidence_id: String,
    /// `pending` after a grant is issued, `committed` after inline sealing or
    /// a completed grant flow. Only `committed` resolves.
    pub state: String,
    /// False when this call replayed an existing seal. The caller can tell an
    /// idempotent retry from a new artifact without comparing ids.
    pub created: bool,
    /// Present only on the grant path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant: Option<EvidenceGrantDto>,
}

/// `POST /v1/evidence/{id}/commit` — finish the grant flow.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CommitEvidenceResponse {
    pub evidence_id: String,
    pub state: String,
    /// False when the artifact was already committed — a replayed commit is
    /// reported, never silently restamped.
    pub committed: bool,
}

/// `GET /v1/evidence/{id}` — the manifest, access-checked.
///
/// The response **is** the manifest, unwrapped. The contract says so in as
/// many words ("server -> caller on `GET /v1/evidence/{id}`"), and an earlier
/// draft of this route wrapped it in `{evidence_id, state, manifest}` — which
/// the Matrix client, written against the contract, could not deserialize.
///
/// The wrapper's two extra fields were redundant anyway: `evidence_id` is set
/// on the manifest at read time, and `state` could only ever be `committed`,
/// because a pending artifact answers 409 and a purged one 410. A 200 already
/// means committed.
pub type EvidenceManifestResponse = serde_json::Value;

/// `GET /v1/evidence/{id}/rows` — a bounded, audited window over the sealed
/// bytes.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EvidenceRowsResponse {
    pub evidence_id: String,
    /// Zero-based index of the first row returned.
    pub from: usize,
    /// Rows in this page.
    pub rows: Vec<serde_json::Value>,
    /// Total rows in the artifact, when the serialization allows counting them
    /// without decoding everything. `None` for a format this server does not
    /// decode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<usize>,
    /// True when more rows follow this page.
    pub has_more: bool,
}

/// `GET /v1/evidence/{id}/accesses` — who resolved an artifact, and how it
/// went. Operator-facing, mgmt-gated.
///
/// Records *that* a read happened, never what was read: an audit table holding
/// the regulated data it audits is a second copy of the problem.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EvidenceAccessDto {
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

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EvidenceAccessesResponse {
    pub evidence_id: String,
    /// Newest first.
    pub accesses: Vec<EvidenceAccessDto>,
}

/// `POST /v1/evidence/{id}/legal-hold` — place or lift a hold.
///
/// A hold blocks deletion and nothing else; reads stay governed by the
/// artifact's authorization class. An instruction to preserve evidence that
/// also hid it would be a strange instruction.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LegalHoldRequest {
    pub hold: bool,
}

/// `DELETE /v1/evidence/{id}` — purge the bytes now.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PurgeEvidenceResponse {
    pub evidence_id: String,
    /// False when the artifact was already purged — a replayed purge is
    /// reported, not silently re-run.
    pub purged: bool,
    /// Always `purged`. The metadata row survives so citations keep resolving
    /// as `evidence-expired` rather than `not-found`.
    pub state: String,
}

// ---------------------------------------------------------------------------
// Index artifacts
//
// The derived-index tier: immutable, content-verified search artifacts built
// from an index version PostgreSQL already holds. These routes report and
// operate on that tier; they never change which version is ACTIVE, and none of
// them accepts a bare artifact hash as authority — every operation names a
// logical version and is reached through an authorized tenant.
// ---------------------------------------------------------------------------

/// One catalogued artifact.
///
/// The storage URI is deliberately absent: it carries a container prefix and an
/// opaque tenant path element, and a storage location is not part of what a
/// caller needs to know about an artifact.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IndexArtifactDto {
    /// sha256 of the canonical manifest. Content, never authority.
    pub artifact_id: String,
    pub engine_id: String,
    /// sealed | verified | failed | retired
    pub state: String,
    pub format_version: i32,
    pub bytes_len: i64,
    pub file_count: i32,
    /// sha256 of the canonical physical build plan. Two artifacts sharing it
    /// were built to the same physical recipe.
    pub artifact_plan_sha256: String,
}

/// Which artifact occupies one binding slot.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IndexArtifactBindingDto {
    /// staged | shadow | serving
    pub slot: String,
    pub artifact_id: String,
    pub generation: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IndexArtifactStatusDto {
    pub index_version_id: String,
    pub artifacts: Vec<IndexArtifactDto>,
    pub bindings: Vec<IndexArtifactBindingDto>,
}

/// Enqueue a durable build job (§8.6): the request path answers immediately
/// and a builder executes.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IndexBuildJobRequest {
    /// `backfill` | `rebuild` | `direct`.
    pub kind: String,
    /// backfill and direct jobs name a collection…
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection_id: Option<String>,
    /// …a rebuild names a version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_version_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_chars: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watermark_seq: Option<u64>,
    /// Runbook/execution correlation, carried on the job row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
}

/// One build job's state and bounded outcome.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IndexBuildJobDto {
    pub job_id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_version_id: Option<String>,
    /// pending | running | succeeded | failed | cancelled | superseded
    pub state: String,
    pub attempts: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IndexBuildJobListResponse {
    pub jobs: Vec<IndexBuildJobDto>,
}

/// Promote the `staged` binding into `serving` — a compare-and-swap against
/// BOTH generations the caller read (serving 0 = the slot is empty).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IndexArtifactPromoteRequest {
    pub expected_staged_generation: i64,
    #[serde(default)]
    pub expected_serving_generation: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// One scope's rollout selector row: which engine serves it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RetrievalRolloutDto {
    /// `collection` | `shape`.
    pub scope_kind: String,
    pub scope_id: String,
    /// `postgres` | `datastore`.
    pub serving: String,
    /// Hydrate `staged` bindings in the background while PostgreSQL still
    /// serves — the canary's first step.
    pub prewarm_staged: bool,
    pub required_versions_policy: String,
    pub generation: i64,
}

/// Create or change one scope's selector row.
///
/// Selecting `datastore` is gated on serving-required completeness; selecting
/// `postgres` — the rollback — never is.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RetrievalRolloutSetRequest {
    pub scope_kind: String,
    pub scope_id: String,
    pub serving: String,
    #[serde(default)]
    pub prewarm_staged: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_versions_policy: Option<String>,
    /// Present = compare-and-swap against the generation you read. Absent =
    /// first row for this scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_generation: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// §7.3 logical activation of a collection's index version, as a
/// compare-and-swap. For a datastore-routed collection the server refuses to
/// activate a version without a verified `serving` binding — promote first.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CollectionIndexActivateRequest {
    pub index_version_id: String,
    /// The active version this request believes it is replacing. `None` =
    /// nothing active. A mismatch is the superseded-build outcome: nothing
    /// changes and `activated` comes back false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_active: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CollectionIndexActivateResponse {
    /// False = the CAS found a different active version; the pointer is
    /// untouched and the built version stays valid.
    pub activated: bool,
    /// The collection's active version AFTER this call, whichever way it went.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
}

/// Bind a verified artifact into the `staged` or `shadow` slot.
///
/// The `serving` slot is deliberately not expressible here: what answers user
/// traffic changes through the promotion operation with its expectation and
/// per-node checks, never through a bind.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IndexArtifactBindRequest {
    /// `staged` or `shadow`.
    pub slot: String,
    pub artifact_id: String,
    /// Present = replace by compare-and-swap on the slot's current generation.
    /// Absent = the slot must be empty (a first bind).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_generation: Option<i64>,
    /// Recorded in the binding event, beside the actor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// What re-verifying one artifact against its stored bytes found.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IndexArtifactVerifyDto {
    pub artifact_id: String,
    pub verified: bool,
    /// Present only on failure, and bounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IndexArtifactVerifyResponse {
    pub index_version_id: String,
    pub results: Vec<IndexArtifactVerifyDto>,
}

/// What a build did.
///
/// `converged`, `already_built` and `deferred` are all successes: the first two
/// mean the artifact exists, and the third means another node is building it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IndexArtifactBuildDto {
    pub index_version_id: String,
    /// published | converged | already_built | deferred
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    pub chunks: u64,
    /// Whether this build filled the version's empty `staged` slot. A mirror
    /// never writes `serving`.
    pub bound_staged: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IndexArtifactBackfillRequest {
    pub collection_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IndexArtifactBackfillVersionDto {
    pub index_version_id: String,
    /// active | within_horizon — why this version is serving-required.
    pub reason: String,
    /// published | converged | already_built | deferred | failed
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IndexArtifactBackfillResponse {
    pub collection_id: String,
    /// The `required_versions_policy` the scope's rollout row declares.
    pub policy: String,
    /// True only when EVERY required version has a verified artifact. A scope
    /// with no required versions is not complete.
    pub complete: bool,
    pub versions: Vec<IndexArtifactBackfillVersionDto>,
}

// ---------------------------------------------------------------------------
// Per-call output-token budgets (2026-09-02) — GET/POST /v1/max-tokens
// ---------------------------------------------------------------------------

/// The per-call output-token ceilings (`max_tokens`) the server hands a
/// model provider, one per kind of paid call, as ONE object. Every field is
/// REQUIRED on the wire: `POST /v1/max-tokens` replaces the whole set, never
/// part of it, so a body missing a field is `invalid-input`, not a partial
/// update.
///
/// Precedence at call time: a runbook's own declaration where the grammar
/// has one (`completion.maxTokens`, `modelQueryExpansion.maxTokens`) > the
/// tenant's replacement set through this API > the process's
/// `MUNARIUM_MAX_TOKENS_*` environment > the built-ins (`Default`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MaxTokensBudgets {
    /// A session turn's answer; a runbook's `completion.maxTokens` overrides
    /// it. The truncation-aware retry pays one re-ask at 4x. Built-in 2,048.
    pub turn_completion: u32,
    /// The `modelQueryExpansion` variant-generation call; a runbook's
    /// `modelQueryExpansion.maxTokens` overrides it. Built-in 256.
    pub query_expansion: u32,
    /// `POST /v1/providers/{name}/complete` when the request omits
    /// `max_tokens`. Built-in 1,024.
    pub complete_default: u32,
    /// Each `/healthai` probe completion. Built-in 512.
    pub healthai_probe: u32,
    /// The evidence hierarchy's one-word question classifier. Built-in 32.
    pub hierarchy_classifier: u32,
    /// The evidence hierarchy's semantic-intent task (names only). Built-in 480.
    pub hierarchy_intent: u32,
    /// The runbook validation AI advisory pass. Built-in 2,048.
    pub runbook_advisory: u32,
    /// The guided-authoring assist draft. Built-in 8,192.
    pub authoring_assist: u32,
}

impl Default for MaxTokensBudgets {
    /// The built-ins: every per-call budget as doubled on 2026-09-02.
    fn default() -> Self {
        Self {
            turn_completion: 2048,
            query_expansion: 256,
            complete_default: 1024,
            healthai_probe: 512,
            hierarchy_classifier: 32,
            hierarchy_intent: 480,
            runbook_advisory: 2048,
            authoring_assist: 8192,
        }
    }
}

/// `GET /v1/max-tokens`, and what `POST /v1/max-tokens` answers with: the
/// effective budgets (flattened, so a GET body round-trips into a POST) plus
/// where they come from.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MaxTokensResponse {
    #[serde(flatten)]
    pub budgets: MaxTokensBudgets,
    /// `tenant` after the tenant replaced the set through the API;
    /// `environment` while the process defaults (env vars over built-ins)
    /// apply.
    pub source: String,
    /// RFC 3339 instant of the tenant's last replacement; absent for
    /// `environment`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}
