// SPDX-License-Identifier: Apache-2.0
//! ClearView IR-1 to IR-4: the optional inference endpoint's
//! management surface. Probe a provider configuration, bind a collection to
//! it, enroll the collection and roll it back.
//!
//! Every value here is content-free: model names, fixed reason codes,
//! fingerprints and counts. No credential, endpoint address, query or passage
//! text is ever returned.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// One check of a conformance probe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct InferenceProbeCheckDto {
    /// `discovery` | `embedding` | `reranking`.
    pub check: String,
    pub passed: bool,
    /// A fixed reason code when the check failed.
    #[serde(default)]
    pub reason: Option<String>,
}

/// `POST /v1/providers/{name}/inference-probe` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct InferenceProbeResponse {
    pub probe_id: String,
    pub provider_config: String,
    /// True only when every check passed. A probe is valid until the
    /// configuration's text changes.
    pub passed: bool,
    /// 16 hex digits of the resolved base URL; never the address.
    pub endpoint_fingerprint: String,
    #[serde(default)]
    pub embed_model: Option<String>,
    #[serde(default)]
    pub embed_dimensions: Option<u64>,
    #[serde(default)]
    pub rerank_model: Option<String>,
    pub checks: Vec<InferenceProbeCheckDto>,
}

/// The embedding stage of a binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct InferenceEmbeddingSettings {
    /// A model listed in the configuration's `models.embed`.
    pub model: String,
    /// Prefixed to every query before embedding, exactly as given: the full
    /// literal prefix, labels and separators included. Part of the index
    /// identity.
    #[serde(default)]
    pub query_instruction: Option<String>,
    /// Prefixed to every passage before embedding. Part of the index identity.
    #[serde(default)]
    pub passage_instruction: Option<String>,
    /// `require` (the default when omitted) | `prefer`.
    #[serde(default)]
    pub policy: Option<String>,
}

/// The reranking stage of a binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct InferenceRerankSettings {
    /// A model listed in the configuration's `models.rerank`.
    pub model: String,
    /// Head candidates scored per read (default 20, at most 100).
    #[serde(default)]
    pub max_candidates: Option<u64>,
    /// A candidate passage over this many bytes refuses the stage (default
    /// 8,192). Server never truncates.
    #[serde(default)]
    pub max_document_bytes: Option<u64>,
    /// `require` (the default when omitted) | `prefer`.
    #[serde(default)]
    pub policy: Option<String>,
    /// The task instruction for an instruction-aware reranker (hub ADR 0029:
    /// Server applies it, the endpoint applies none). When set, Server sends
    /// the rerank `query` as `<Instruct>: {instruction}\n<Query>: {query}`,
    /// the Qwen3-Reranker card format; when omitted the query is sent
    /// unchanged. Part of the reranking stage identity. 1 to 1,024
    /// characters, no control characters. Absent from responses when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
}

/// Explicitly authorized shadow comparisons. Both require the
/// configuration's `budgets.shadowDailyTokens` allocation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct InferenceShadowSettings {
    /// Rerank served single-collection reads in shadow while reranking is
    /// not serving.
    #[serde(default)]
    pub rerank: bool,
    /// Search the built, not yet enrolled semantic index in shadow.
    #[serde(default)]
    pub semantic_index: bool,
}

/// `PUT /v1/collections/{id}/inference` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct InferenceBindingRequest {
    /// The binding revision this change replaces; 0 creates the binding.
    pub expected_revision: u64,
    /// An `openai-compatible` provider configuration of this tenant, with
    /// `budgets.dailyTotalTokens` set. Its budget pays for the collection.
    pub provider_config: String,
    #[serde(default)]
    pub embedding: Option<InferenceEmbeddingSettings>,
    #[serde(default)]
    pub rerank: Option<InferenceRerankSettings>,
    #[serde(default)]
    pub shadow: Option<InferenceShadowSettings>,
    /// One deadline for a read's stages (default 5,000; 100 to 60,000).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// The collection's placement requirement (ClearView SC2).
    /// Omitted keeps the binding's current requirement, or on
    /// a first binding records the configuration's declared placement; with
    /// no declaration either, no placement check applies (Phase 6).
    #[serde(default)]
    pub placement: Option<InferencePlacementRequirement>,
}

/// Where a collection's inference content may be processed (ClearView SC2).
/// Checked against the bound configuration's declared
/// `inference.placement` before any content is sent, on serving, shadow,
/// build and retry paths alike.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct InferencePlacementRequirement {
    /// `dedicated`: only an endpoint declared dedicated to this tenant.
    /// `shared`: a dedicated endpoint, or a shared one whose declaration
    /// explicitly lists this tenant.
    pub isolation: String,
    /// Processing rules the endpoint must declare with exactly these values
    /// (for example `location`, `retention`). An undeclared rule is
    /// incompatible.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub processing: std::collections::BTreeMap<String, String>,
}

/// `POST .../inference/enroll` and `.../inference/rollback` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct InferenceRevisionRequest {
    pub expected_revision: u64,
}

/// One configured stage's readiness, without calling the endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct InferenceStageStatusDto {
    /// `embedding` | `reranking`.
    pub stage: String,
    pub model: String,
    pub policy: String,
    /// `ready` | `disabled` | `unconfigured` | `probe_failed`.
    pub state: String,
    #[serde(default)]
    pub reason: Option<String>,
    /// The stage identity: the semantic space id (embedding), or the
    /// reranking model name, followed by `#instruct-` and a digest of the
    /// rerank instruction when one is configured.
    #[serde(default)]
    pub identity: Option<String>,
}

/// Endpoint usage attributed to one collection, by purpose, for the current
/// UTC day. `missing_usage` counts attempts charged at their estimate
/// because the endpoint reported no usage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct InferenceUsageDto {
    /// `serving` | `shadow` | `build` | `probe`.
    pub purpose: String,
    pub attempts: u64,
    pub accounted_units: u64,
    pub missing_usage: u64,
}

/// `GET /v1/collections/{id}/inference` and every binding change's response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CollectionInferenceDto {
    pub collection_id: String,
    /// `unbound` | `bound` | `enrolled` | `rolled_back`.
    pub state: String,
    /// 0 when unbound.
    pub revision: u64,
    #[serde(default)]
    pub provider_config: Option<String>,
    #[serde(default)]
    pub embedding: Option<InferenceEmbeddingSettings>,
    #[serde(default)]
    pub rerank: Option<InferenceRerankSettings>,
    #[serde(default)]
    pub shadow: Option<InferenceShadowSettings>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// The built semantic index, if any.
    #[serde(default)]
    pub semantic_index_id: Option<String>,
    /// Chunks and vectors of that index: complete when equal.
    #[serde(default)]
    pub semantic_chunks: Option<u64>,
    #[serde(default)]
    pub semantic_vectors: Option<u64>,
    /// The prior approved index rollback restores.
    #[serde(default)]
    pub prior_index_id: Option<String>,
    /// The collection's active index now.
    #[serde(default)]
    pub active_index_id: Option<String>,
    pub stages: Vec<InferenceStageStatusDto>,
    pub usage: Vec<InferenceUsageDto>,
    /// The binding's placement requirement (ClearView SC2). Absent when no
    /// placement applies, as in Phase 6.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<InferencePlacementRequirement>,
}
