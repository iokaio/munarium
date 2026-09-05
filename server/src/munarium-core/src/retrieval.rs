// SPDX-License-Identifier: Apache-2.0
//! The RetrievalBackend trait. Stage one is in-Postgres hybrid
//! (munarium-retrieval-pg); a dedicated tier (OpenSearch) swaps in behind this
//! trait post-GA, gated by the retrieval-quality tolerance fixtures.

use crate::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridQuery {
    pub query: String,
    pub shape_ref: String,
    pub top_k: usize,
    pub filter: Option<serde_json::Value>,
    /// None = the active index for the shape.
    pub index_version: Option<String>,
}

// PartialEq is additive: EvidenceBlock::DocumentHits carries these,
// and a closed evidence enum that cannot be compared is awkward to test.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    pub chunk_id: String,
    /// Source identity — the logical path's stable id.
    pub source_id: String,
    /// The logical path itself: which document actually answered.
    pub source_path: String,
    /// Integrity of the bytes that path held at index time.
    pub source_content_hash: String,
    pub text: String,
    pub score: f64,
    pub lexical_rank: Option<u32>,
    pub vector_rank: Option<u32>,
    /// Raw lexical-leg relevance (`ts_rank`) — magnitude-comparable across
    /// collections sharing a shape, unlike the rank-derived `score`. Present
    /// when the hit appeared in the lexical leg.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lexical_score: Option<f64>,
    /// Raw vector-leg cosine distance (lower = closer) — magnitude-comparable
    /// across collections sharing one embedder. Present when the hit appeared
    /// in the vector leg.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_distance: Option<f64>,
    pub metadata: Option<serde_json::Value>,
}

/// Every retrieval answer carries one — reproducibility is the demo.
///
/// Sources are named three ways on purpose: `source_ids` are stable identity,
/// `source_paths` say *which document* answered (a bare hash never did), and
/// `source_content_hashes` prove *which bytes* it held when indexed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenanceEnvelope {
    pub chunk_ids: Vec<String>,
    pub source_ids: Vec<String>,
    pub source_paths: Vec<String>,
    pub source_content_hashes: Vec<String>,
    pub index_version: String,
    pub event_watermark: u64,
    pub provider_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub hits: Vec<SearchHit>,
    pub envelope: ProvenanceEnvelope,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexVersion {
    pub id: String,
    pub shape_ref: String,
    pub manifest: serde_json::Value,
    pub event_watermark: u64,
    pub active: bool,
}

#[async_trait]
pub trait RetrievalBackend: Send + Sync {
    async fn hybrid_search(&self, q: HybridQuery) -> Result<SearchResult>;
    async fn index_version(&self, shape_ref: &str) -> Result<IndexVersion>;
}

// ---------------------------------------------------------------------------
// Backend-neutral query policy and collection DTOs.
//
// These moved here from `munarium-retrieval-pg` when the retrieval coordinator
// was extracted. None of them touches SQL: they are the vocabulary a caller
// uses to ASK for retrieval and to describe what came back, and leaving them
// exposed from the PostgreSQL crate made that crate the de facto interface --
// which is what a second backend cannot live with.
// ---------------------------------------------------------------------------

/// Retrieval knobs a runbook's `retrieval:` block (or shape) supplies.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QueryExpansionRule {
    pub when_any: Vec<String>,
    pub add_terms: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ContentDemotionRule {
    pub contains: String,
    pub lexical_multiplier: f64,
    pub vector_distance_penalty: f64,
    /// `"substring"` (case-insensitive substring of the chunk text — the
    /// original contract) or `"phrase"` (the marker's words in sequence in
    /// the chunk's tsvector: `ts @@ phraseto_tsquery(marker)` — stemmed,
    /// punctuation-insensitive, and evaluated without touching the text
    /// column). Serialized as `match` for the SQL rule list.
    #[serde(rename = "match")]
    pub match_mode: String,
}

impl ContentDemotionRule {
    pub const SUBSTRING: &'static str = "substring";
    pub const PHRASE: &'static str = "phrase";
}

#[derive(Debug, Clone)]
pub struct SearchParams {
    pub top_k: usize,
    pub rrf_k: f64,
    pub candidate_n: i64,
    pub query_expansions: Vec<QueryExpansionRule>,
    /// 0 = rank by the original query, 1 = rank by the fully expanded query.
    /// Candidate selection always uses the expanded query.
    pub query_expansion_weight: f64,
    pub content_demotions: Vec<ContentDemotionRule>,
    /// The EXPANDED query's normalized lexemes as `plainto_tsquery` prints
    /// them (quoted), computed once per query formulation.
    pub query_lexemes: Vec<String>,
    /// 1 = any query word makes a chunk a candidate; 2 = at least two (the
    /// OR of ANDed lexeme pairs, GIN-evaluated before any rank).
    pub minimum_should_match: usize,
    /// 0 = off; otherwise a query lexeme found in more than this fraction of
    /// the collection's chunks (per its build-time `index_lexeme_frequency`)
    /// is dropped from the candidate predicate — still ranked, no longer a
    /// candidate generator.
    pub stop_term_fraction: f64,
}

impl Default for SearchParams {
    fn default() -> Self {
        Self {
            top_k: 10,
            rrf_k: 60.0,
            candidate_n: 50,
            query_expansions: Vec::new(),
            query_expansion_weight: 1.0,
            content_demotions: Vec::new(),
            query_lexemes: Vec::new(),
            minimum_should_match: 1,
            stop_term_fraction: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CollectionInfo {
    pub id: String,
    pub name: String,
    pub shape_ref: String,
    pub access_level: i32,
    pub compartments: Vec<String>,
    pub status: String,
    pub description: Option<String>,
    pub created_at: String,
}

/// One collection's contribution to a multi-collection search.
#[derive(Debug, Clone)]
pub struct CollectionSearchResult {
    pub collection_id: String,
    pub collection_name: String,
    pub result: SearchResult,
}

/// Source metadata, backend-neutral.
#[derive(Debug, Clone, Serialize)]
pub struct SourceInfo {
    pub source_id: String,
    pub filename: String,
    pub media_type: String,
    pub content_hash: String,
    pub bytes_len: u64,
    pub storage_backend: String,
    pub blob_uri: Option<String>,
    pub extraction_status: Option<String>,
    pub extraction_method: Option<String>,
    pub created_at: String,
}

/// Weighted-RRF controls for the multi-collection merge. The defaults
/// reproduce the unweighted merge exactly (both legs at 1.0, no
/// collection-evidence leg).
///
/// NOTE for a second backend: this merge fuses from RAW leg scores, which is
/// sound only while ONE engine produces every score in the merge. Postgres
/// `ts_rank` and Tantivy BM25 are not numerically comparable, so a
/// multi-search that mixes engines must not use this path -- see
/// the datastore design: "cross-engine fusion is a merge hazard".
#[derive(Debug, Clone, PartialEq)]
pub struct MergeWeights {
    /// Multiplier on the lexical leg's `1/(k + global rank)` term.
    pub lexical: f64,
    /// Multiplier on the vector leg's term. A runbook on the built-in
    /// bag-of-words embedder may lower this: that embedder rewards the
    /// shortest chunk sharing any token, so its global rank-1s are often
    /// tables and fragments (measured 2026-08-25).
    pub vector: f64,
    /// Multiplier on the collection-evidence leg: every hit also receives
    /// `1/(k + rank of its collection)` where the rank is the position the
    /// caller's collection selection assigned (1 = strongest evidence).
    /// Hierarchical evidence — a collection shown to be ABOUT the query's
    /// subject lends its chunks a prior that a collection merely USING the
    /// query's words does not get. Collections absent from `collection_rank`
    /// receive no contribution from this leg.
    pub collection_evidence: f64,
    /// Collection name → 1-based evidence rank.
    pub collection_rank: std::collections::HashMap<String, usize>,
    /// Collections whose hits came from the ORIGINAL-query probe rather than
    /// the deep (expanded) search. Raw leg scores are only comparable within
    /// one query formulation — Postgres' OR `ts_rank` shrinks as the term
    /// count grows, so an original-query pool scores ~0.2 where the same
    /// chunk under a 19-term expansion scores ~0.03 — so these hits are
    /// ranked in their own lexical/vector lists (a second stratum) and RRF
    /// fuses ranks, never raw scores, across the two. Measured 2026-08-25:
    /// merging the strata raw let 46 unselected narrative pools push every
    /// letterbook hit out of the George Washington top 20.
    pub probe_collections: std::collections::HashSet<String>,
    /// Multiplier on the probe stratum's leg contributions (1.0 = a probe
    /// rank-1 counts like a deep rank-1; the collection-evidence leg then
    /// arbitrates between the strata).
    pub probe_weight: f64,
}

impl Default for MergeWeights {
    fn default() -> Self {
        Self {
            lexical: 1.0,
            vector: 1.0,
            collection_evidence: 0.0,
            collection_rank: std::collections::HashMap::new(),
            probe_collections: std::collections::HashSet::new(),
            probe_weight: 1.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Prepared queries.
//
// The coordinator builds these ONCE per request and hands the same value to
// every backend and every collection in a fan-out. Two reasons, and the second
// is the one that matters:
//
//   1. Work. `search_collection` derived the expanded query and the query
//      VECTOR internally, so an N-collection fan-out embedded the same query N
//      times.
//
//   2. Comparability. Shadow mode runs PostgreSQL and Datastore over one
//      request. If each derived its own plan and its own vector, a difference
//      in results could not be attributed to the engine -- which is the only
//      thing shadow mode exists to measure. Preparing once makes the inputs
//      identical by construction rather than by review.
// ---------------------------------------------------------------------------

/// A structured lexical query, so no backend re-derives expansion, phrases or
/// demotions from a string.
#[derive(Debug, Clone, PartialEq)]
pub struct LexicalQueryPlan {
    /// The caller's query, verbatim.
    pub original: String,
    /// The query after expansion rules. Equal to `original` when no rule fired.
    pub expanded: String,
    /// 0 = rank by the original, 1 = rank by the expanded, between = blend.
    /// Candidate SELECTION always uses the expanded query regardless.
    pub expansion_weight: f64,
    /// Normalized lexemes of the expanded query, as `plainto_tsquery` prints
    /// them. Empty means the backend falls back to its own OR query.
    pub lexemes: Vec<String>,
    pub demotions: Vec<ContentDemotionRule>,
    /// 1 = any query word makes a chunk a candidate; 2 = at least two.
    pub minimum_should_match: usize,
    /// 0 = off. Otherwise a lexeme in more than this fraction of a
    /// collection's chunks stops generating candidates there.
    ///
    /// Deliberately a POLICY value and not a resolved stop list: the list is
    /// per-collection and comes from that collection's own index statistics,
    /// so it cannot be computed once for a fan-out. The plan is shared; the
    /// per-shard candidate predicate is derived from the plan plus shard-local
    /// statistics.
    pub stop_term_fraction: f64,
    /// Bumped when the meaning of any field above changes, so a recorded plan
    /// can be read back correctly.
    pub policy_version: u32,
}

/// The current query-policy version. Bump when a field's meaning changes.
pub const QUERY_POLICY_VERSION: u32 = 1;

/// One request's prepared query: the lexical plan and the query vector, each
/// produced once.
#[derive(Debug, Clone)]
pub struct PreparedSearchQuery {
    pub lexical: Option<LexicalQueryPlan>,
    /// `None` is legitimate -- a lexical-only query, or a collection built
    /// without vectors. An adapter must NOT fabricate a vector leg for one.
    /// `Arc` because a fan-out shares it across collections without copying.
    pub embedding: Option<std::sync::Arc<[f32]>>,
    /// Per-leg candidate pool sizes.
    pub lexical_candidates: i64,
    pub vector_candidates: i64,
    pub top_k: usize,
    pub rrf_k: f64,
}

/// Produces a query vector, without the caller knowing where from.
///
/// Injected into the coordinator so that "prepare once" holds even if a
/// provider-backed embedder replaces the local one. Provider credentials and
/// configuration stay in Server; `munarium-datastore` never depends on them.
pub trait QueryEmbedder: Send + Sync {
    /// Embed one text.
    fn embed(&self, text: &str) -> Vec<f32>;

    /// Blend two embeddings by weight and renormalize.
    ///
    /// On the trait rather than free, because the blend is part of the
    /// embedder's contract: what "halfway between two queries" means depends
    /// on the space, and a caller must not assume it is linear interpolation.
    fn blend(&self, original: &str, expanded: &str, weight: f32) -> Vec<f32>;

    /// Stable identity for the provenance envelope, e.g. `local/local-hash@1/256`.
    fn fingerprint(&self) -> String;

    fn dimensions(&self) -> usize;
}
