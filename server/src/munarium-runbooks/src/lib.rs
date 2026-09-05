// SPDX-License-Identifier: Apache-2.0
//! Runbook parsing + step-machine types (extended in the milestone). A runbook is a
//! versioned, declarative pipeline; the executor (server-side) walks the
//! steps, persisting every transition, pausing at approval gates, and
//! resuming after a kill from the persisted state.
//!
//! Two spec generations parse here:
//! - **v1** (`spec.shape`): the single-shape reindex pipeline, unchanged.
//! - **v2** (`spec.collections`): the retrieval application — one runbook
//!   spanning multiple compartmentalized collections, with declarative
//!   source bindings, retrieval knobs, per-task-level model defaults, and an
//!   optional RAG completion step for session turns.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub mod validate;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunbookDoc {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub kind: String,
    pub metadata: RunbookMeta,
    pub spec: RunbookSpec,
}

pub mod research;

pub use research::{
    validate_research, AnswerRoleSpec, CollectionEvidenceSpec, DataViewKind, DataViewParam,
    DataViewSpec, LayerRequirementSpec, ResearchError, ResearchLayerSpec, ResearchProfileSpec,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunbookMeta {
    pub name: String,
    pub version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunbookSpec {
    /// v1: the shape_ref this pipeline operates on, e.g. "cuad-contracts@3".
    /// None on v2 specs (which declare `collections` instead).
    #[serde(default)]
    pub shape: Option<String>,
    /// v2: the collections this retrieval application spans.
    #[serde(default)]
    pub collections: Vec<CollectionSpec>,
    /// v2: retrieval knobs (topK / rrfK / candidateN).
    #[serde(default)]
    pub retrieval: Option<RetrievalSpec>,
    /// The Munarium Matrix query contracts this runbook may read.
    /// A layer names one as `matrix:<name>`; the model never writes SQL.
    #[serde(default, rename = "dataViews", skip_serializing_if = "Vec::is_empty")]
    pub data_views: Vec<DataViewSpec>,
    /// v2: per-task-level model defaults + the API-override policy.
    #[serde(default)]
    pub models: Option<ModelsSpec>,
    /// v2: optional RAG completion for session turns.
    #[serde(default)]
    pub completion: Option<CompletionSpec>,
    /// v2: where this runbook's documents live in object storage.
    #[serde(default)]
    pub sources: Option<SourcesSpec>,
    /// v2: how the run plan is flattened across collections. Default
    /// `stepMajor` preserves the original execution order for every runbook
    /// that does not ask for anything else.
    #[serde(default)]
    pub execution: Option<ExecutionSpec>,
    /// Raw single-key step maps (`- resolveSources: {}`); converted by
    /// `parse_runbook` (serde_yaml 0.9 dropped single-key-map enum syntax).
    #[serde(default, rename = "steps")]
    steps_raw: Vec<serde_yaml::Value>,
    #[serde(skip)]
    pub steps: Vec<StepSpec>,
}

/// Where a runbook's documents live in object storage.
///
/// Blobs are addressed by their logical path, which is the same string a
/// collection's `filenamePrefix` matches — so declaring the prefix here says
/// "everything this runbook reads lives under `northgate/`" and makes the
/// binding checkable rather than a convention someone has to remember.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcesSpec {
    /// Blob container; defaults to the server's `MUNARIUM_AZURE_BLOB_CONTAINER`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    /// Path prefix every collection binding must sit under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
}

/// One compartmentalized collection a v2 runbook retrieves from.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionSpec {
    /// Tenant-unique collection name (stable handle; the executor resolves
    /// or creates the collection under this name).
    pub name: String,
    /// Shape governing this collection's sources.
    pub shape: String,
    /// Access level a capability token must dominate to search here.
    #[serde(default)]
    pub access_level: i32,
    /// Need-to-know tags a token must carry (all of them).
    #[serde(default)]
    pub compartments: Vec<String>,
    /// Declarative source binding: which uploaded sources feed this
    /// collection. Re-evaluated by every resolveSources step.
    #[serde(default)]
    pub sources: Option<SourceBinding>,
    /// Labels stamped
    /// onto evidence sealed from this collection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<CollectionEvidenceSpec>,
}

/// Declarative matchers binding uploaded sources to a collection. All
/// present matchers OR together with the explicit hash list; within the
/// prefix/media matchers both must hold when both are declared.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceBinding {
    #[serde(default)]
    pub filename_prefix: Option<String>,
    #[serde(default)]
    pub media_types: Vec<String>,
    #[serde(default)]
    pub content_hashes: Vec<String>,
}

impl SourceBinding {
    pub fn is_empty(&self) -> bool {
        self.filename_prefix.is_none()
            && self.media_types.is_empty()
            && self.content_hashes.is_empty()
    }
}

/// How a v2 run plan is flattened across collections.
///
/// This is a real operational lever, not a style choice. Only `cutover` can
/// pause a run (`StepSpec::requires_approval`), so under `stepMajor` the
/// FIRST request executes resolveSources + buildIndex + verify for every
/// collection before it reaches any gate — which is fine for a data room and
/// impossible for a 530 MB archive, because that whole build has to fit
/// inside one HTTP request (and a client disconnect wedges the step).
/// `collectionMajor` walks one collection through all its steps before
/// starting the next, so each request builds exactly one collection and the
/// approval gates chunk the work.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExecutionOrder {
    /// All of step 1 across collections, then all of step 2, ... (default;
    /// the original runbook-v2 behavior).
    #[default]
    StepMajor,
    /// Collection 1 through every step, then collection 2, ...
    CollectionMajor,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionSpec {
    #[serde(default)]
    pub order: ExecutionOrder,
}

/// Add retrieval vocabulary when a query contains at least one configured
/// trigger term. The terms are application policy: the retrieval engine only
/// performs case-insensitive, whole-token matching and appends additions.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryExpansionSpec {
    #[serde(default)]
    pub when_any: Vec<String>,
    #[serde(default)]
    pub add_terms: Vec<String>,
}

/// Ask the runbook's query-expansion model for generic lexical variants at
/// turn time. The engine supplies the safety-constrained prompt; the runbook
/// controls whether the paid step runs and bounds its output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelQueryExpansionSpec {
    #[serde(default = "default_model_expansion_max_terms")]
    pub max_terms: usize,
    /// Optional since 2026-09-02: absent means the server's configured
    /// `query_expansion` budget (`GET /v1/max-tokens`; built-in 256), not a
    /// grammar constant. Validated to 32..=512 when declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// When false (default), provider/parse failure falls back to the original
    /// query. When true, expansion failure fails the turn.
    #[serde(default)]
    pub required: bool,
}

fn default_model_expansion_max_terms() -> usize {
    12
}

/// Generic two-stage collection selection for wide, sharded runbooks. A
/// bounded original-query probe chooses the strongest collections before the
/// full candidate pool and optional model expansion are evaluated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CollectionSelectionSpec {
    pub max_collections: usize,
    #[serde(default = "default_probe_candidate_n")]
    pub probe_candidate_n: i64,
    #[serde(default = "default_candidate_pool_per_collection")]
    pub candidate_pool_per_collection: usize,
    /// How much a collection's phrase evidence (the share of its probe pool
    /// containing one of the query's own adjacent content-word pairs
    /// verbatim) multiplies its density evidence:
    /// `density × (1 + phraseBoost × fraction)`. 0 = density only.
    /// Default 3: a pool 85% carrying the phrase counts 3.55×, one carrying
    /// it in 6% of hits 1.18× — so strong phrase evidence decides and weak
    /// phrase evidence yields to density (measured 2026-08-25).
    #[serde(default = "default_phrase_boost")]
    pub phrase_boost: f64,
}

fn default_phrase_boost() -> f64 {
    3.0
}

fn default_probe_candidate_n() -> i64 {
    50
}

fn default_candidate_pool_per_collection() -> usize {
    100
}

/// Route a query to an application-owned subset of the runbook's
/// collections when every configured trigger term is present. This is a
/// candidate-selection instruction, not corpus knowledge in the engine.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CollectionRouteSpec {
    #[serde(default)]
    pub when_all: Vec<String>,
    #[serde(default)]
    pub collections: Vec<String>,
}

/// Demote text carrying an application-defined marker without excluding it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContentDemotionSpec {
    pub contains: String,
    #[serde(default = "default_lexical_multiplier")]
    pub lexical_multiplier: f64,
    #[serde(default = "default_vector_distance_penalty")]
    pub vector_distance_penalty: f64,
    /// Collections this rule does NOT apply to (2026-08-25). A corpus-
    /// structure declaration, not a query rule: in a catalog collection the
    /// "metadata-only" record IS the content (a map collection's records
    /// describe the maps), so demoting it there excludes the collection's
    /// only answers. Names must be declared in `spec.collections`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub except_collections: Vec<String>,
    /// How the marker is matched (2026-08-25). `substring` (default): a
    /// case-insensitive substring of the chunk text — exact, but every
    /// candidate row's full text is detoasted and lowered to test it.
    /// `phrase`: the marker's words in sequence in the chunk's tsvector
    /// (`phraseto_tsquery`), stemmed and punctuation-insensitive, tested
    /// against the already-parsed vector the rank needs anyway — measured
    /// as the single most expensive per-row term of the lexical leg under
    /// load. Prefer `phrase` for a marker of three or more words; a
    /// one-word marker becomes a bare lexeme match, looser than intended.
    #[serde(default, rename = "match")]
    pub match_mode: DemotionMatch,
}

/// See [`ContentDemotionSpec::match_mode`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DemotionMatch {
    #[default]
    Substring,
    Phrase,
}

impl DemotionMatch {
    /// The rule-list wire value the retrieval engine reads.
    pub fn as_str(self) -> &'static str {
        match self {
            DemotionMatch::Substring => "substring",
            DemotionMatch::Phrase => "phrase",
        }
    }
}

fn default_lexical_multiplier() -> f64 {
    1.0
}

fn default_vector_distance_penalty() -> f64 {
    0.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetrievalSpec {
    #[serde(default = "default_top_k")]
    pub top_k: usize,
    #[serde(default = "default_rrf_k")]
    pub rrf_k: f64,
    #[serde(default = "default_candidate_n")]
    pub candidate_n: i64,
    /// Collections searched concurrently per turn (probe and deep search
    /// alike), 1..=16, default 4 (2026-08-25). Each in-flight search holds
    /// one pooled connection; the server pool defaults to 10. Sequential
    /// probing of 58 shards under a loaded database took ~4.5 s per shard
    /// and the first progress event — the response's first byte — went out
    /// after all of them, past the ingress timeout.
    #[serde(default = "default_search_concurrency")]
    pub search_concurrency: usize,
    /// How many of the query's normalized lexemes a chunk must hold to enter
    /// the lexical candidate pool: 1 (default — any one word, the OR leg's
    /// full behavior) or 2 (at least two, evaluated as a GIN-indexable
    /// tsquery of ANDed pairs before any rank is computed). 2 drops the rows
    /// that match a single, usually common, word — the bulk of what an
    /// OR query over a large shard scans and ranks — and those rows sort
    /// last under density ranking anyway (2026-08-25).
    #[serde(default = "default_minimum_should_match")]
    pub minimum_should_match: usize,
    /// Corpus-adaptive stop terms (2026-08-25): a query lexeme found in more
    /// than this fraction of a collection's chunks is dropped from that
    /// collection's lexical CANDIDATE predicate (it still counts toward the
    /// rank). 0 (default) disables; 0.05..=0.9 otherwise. The frequencies
    /// come from the corpus itself (`ts_stat` at build time), so the engine
    /// learns that "washington" is a stop word in a Washington letterbook
    /// shard and not elsewhere — the candidate set collapses from most of
    /// the shard to the rows holding the question's rarer words. If every
    /// query lexeme is frequent the full set is kept: the predicate is never
    /// empty.
    #[serde(default)]
    pub stop_term_fraction: f64,
    /// The evidence hierarchies this runbook offers. A turn selects
    /// one by name; absent a selection it gets `defaultResearchProfile`, and
    /// absent that, the legacy single-layer document path unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub research_profiles: Vec<ResearchProfileSpec>,
    /// The profile a turn gets when it names none. None keeps every existing
    /// runbook on exactly the path it has always taken.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_research_profile: Option<String>,
    /// Conditional vocabulary supplied by this runbook. No domain terms are
    /// compiled into the retrieval engine.
    #[serde(default)]
    pub query_expansions: Vec<QueryExpansionSpec>,
    /// Relative contribution of the expanded query to lexical/vector
    /// ranking. Candidate selection always sees the expanded vocabulary;
    /// values below 1 keep the caller's original words as the stronger
    /// relevance signal. With no matching expansion this has no effect.
    #[serde(default = "default_query_expansion_weight")]
    pub query_expansion_weight: f64,
    /// Optional provider-backed generic lexical expansion. No configured
    /// entity, domain, or anticipated-query vocabulary is required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_query_expansion: Option<ModelQueryExpansionSpec>,
    /// Optional evidence-driven narrowing for runbooks with many collections.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection_selection: Option<CollectionSelectionSpec>,
    /// Optional weighted global fusion (2026-08-25). Omitted = the
    /// unweighted two-leg merge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fusion: Option<FusionSpec>,
    /// Conditional collection routing supplied by this runbook. All matching
    /// routes are unioned; if none matches, every permitted collection is
    /// searched as before.
    #[serde(default)]
    pub collection_routes: Vec<CollectionRouteSpec>,
    /// Content markers and penalties supplied by this runbook. Matching is
    /// case-insensitive and a match demotes rather than filters the record.
    #[serde(default)]
    pub content_demotions: Vec<ContentDemotionSpec>,
}

/// Weighted reciprocal-rank fusion for the cross-collection merge. Each leg
/// contributes `weight / (rrfK + global rank)`; the defaults (1, 1, 0)
/// reproduce the unweighted merge byte-for-byte. `collectionEvidenceWeight`
/// adds a third leg fed by `collectionSelection`'s ranking — every hit also
/// scores `weight / (rrfK + rank of its collection)` — so a collection the
/// probe showed to be ABOUT the query's subject lends its chunks a prior a
/// collection merely USING the words does not get. Without
/// `collectionSelection` that leg has nothing to read and contributes zero.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FusionSpec {
    #[serde(default = "default_leg_weight")]
    pub lexical_weight: f64,
    #[serde(default = "default_leg_weight")]
    pub vector_weight: f64,
    #[serde(default)]
    pub collection_evidence_weight: f64,
    /// Multiplier on the leg contributions of hits that came from the
    /// unselected collections' original-query probe pools (only with
    /// `collectionSelection`). Those pools are ranked as their own stratum —
    /// their raw scores are not comparable with the expanded deep search's
    /// — so 1.0 means a probe rank-1 counts like a deep rank-1 and the
    /// collection-evidence leg arbitrates; lower values favor the deep
    /// search.
    #[serde(default = "default_leg_weight")]
    pub unselected_pool_weight: f64,
}

fn default_leg_weight() -> f64 {
    1.0
}

impl Default for FusionSpec {
    fn default() -> Self {
        Self {
            lexical_weight: 1.0,
            vector_weight: 1.0,
            collection_evidence_weight: 0.0,
            unselected_pool_weight: 1.0,
        }
    }
}

fn default_top_k() -> usize {
    10
}
fn default_rrf_k() -> f64 {
    60.0
}
fn default_candidate_n() -> i64 {
    50
}
fn default_search_concurrency() -> usize {
    4
}
fn default_minimum_should_match() -> usize {
    1
}
fn default_query_expansion_weight() -> f64 {
    1.0
}

impl Default for RetrievalSpec {
    fn default() -> Self {
        Self {
            top_k: default_top_k(),
            rrf_k: default_rrf_k(),
            candidate_n: default_candidate_n(),
            search_concurrency: default_search_concurrency(),
            minimum_should_match: default_minimum_should_match(),
            stop_term_fraction: 0.0,
            query_expansions: Vec::new(),
            query_expansion_weight: default_query_expansion_weight(),
            model_query_expansion: None,
            collection_selection: None,
            fusion: None,
            collection_routes: Vec::new(),
            content_demotions: Vec::new(),
            research_profiles: Vec::new(),
            default_research_profile: None,
        }
    }
}

/// A provider/model choice. `provider` names a tenant ProviderConfig;
/// `model` pins an exact model id; `tier` (fast|capable) resolves through
/// the provider config's tier map. At least one field must be present.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ModelSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
}

impl ModelSpec {
    pub fn is_empty(&self) -> bool {
        self.provider.is_none() && self.model.is_none() && self.tier.is_none()
    }
}

/// The model-using task levels a runbook can pin defaults for. Extensible —
/// unknown keys are validation errors so typos fail closed.
pub const TASK_LEVELS: &[&str] = &[
    "completion",
    "validation",
    "embedding",
    "query_expansion",
    // Resolving what a question is ASKING, so the hierarchy can pick a
    // profile. Runs on the server; Matrix never calls a model provider.
    "intent",
];

/// Who may override the runbook's model choices via the API.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum OverridePolicy {
    /// Overrides rejected (the default when the block is absent).
    #[default]
    None,
    /// Any configured provider/model may be requested.
    All,
    /// Only these provider names may be requested.
    Allowlist(Vec<String>),
}

impl OverridePolicy {
    pub fn permits(&self, provider: &str) -> bool {
        match self {
            OverridePolicy::None => false,
            OverridePolicy::All => true,
            OverridePolicy::Allowlist(list) => list.iter().any(|p| p == provider),
        }
    }
}

impl Serialize for OverridePolicy {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            OverridePolicy::None => s.serialize_bool(false),
            OverridePolicy::All => s.serialize_bool(true),
            OverridePolicy::Allowlist(list) => list.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for OverridePolicy {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Flag(bool),
            List(Vec<String>),
        }
        Ok(match Raw::deserialize(d)? {
            Raw::Flag(true) => OverridePolicy::All,
            Raw::Flag(false) => OverridePolicy::None,
            Raw::List(list) => OverridePolicy::Allowlist(list),
        })
    }
}

/// Default model specification per task level (plan Part 3). Resolution
/// order: API request override (if the policy permits) → `tasks[<task>]` →
/// `default` → the tenant's provider fallback chain.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<ModelSpec>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tasks: BTreeMap<String, ModelSpec>,
    #[serde(default)]
    pub allow_overrides: OverridePolicy,
}

/// Optional RAG step for session turns: retrieval context is interpolated
/// into the template and sent to the resolved completion model.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionSpec {
    /// Shorthand for `models.tasks.completion.provider` (early-v2 compat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Shorthand for `models.tasks.completion.model`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Must reference `{context}` and `{query}`.
    pub prompt_template: String,
    /// Deterministic answer verification: opt-in port of the measured
    /// grounding checks into the turn loop. When declared,
    /// the turn's completion is checked against the SERVED hits and, on
    /// violations, granted one corrective completion with the violations
    /// attached (the `conformance_retry` shape).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<VerificationSpec>,
    /// Characters of served context the turn assembles for the model
    /// (2026-08-25). Hits past the budget are still retrieved and reported,
    /// but never reach the prompt — with the engine default (16,000) a
    /// `topK: 20` turn over 1,500-char chunks serves about ten of its twenty
    /// hits. Runbooks that widen `topK` should size this to match; the cost
    /// is input tokens per turn. Validated to 4,000..=400,000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_char_budget: Option<usize>,
    /// Completion-token ceiling per turn answer (2026-09-01). A ceiling, not
    /// spend — the provider bills only generated tokens — and the
    /// truncation-aware retry still pays one 4x re-ask on exhaustion, so the
    /// effective ceiling is 4x this value. Engine default 2,048 (1,024 until
    /// 2026-09-02, when every per-call budget was doubled). Exists
    /// because reasoning-always-on models draw hidden reasoning from this
    /// same budget: z-ai/glm-5.3 (the frontier tier) measured ~5k reasoning
    /// tokens on a hard revolution question and returned EMPTY text at the
    /// default even after the 4x retry. Validated to 256..=16,384.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

/// The turn-loop verification block. Both checks are deterministic string
/// work over data the turn already holds — no model judges anything.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationSpec {
    /// Double-quoted spans in the answer must resolve verbatim
    /// (whitespace-normalized) in the served hit text
    /// (`verify_quotes`).
    #[serde(default)]
    pub quotes: bool,
    /// Bracketed citations in the answer (`[collection/chunk]`, the exact
    /// labels the context block serves) must name content that was actually
    /// served this turn (`verify_citations`).
    #[serde(default)]
    pub citations: bool,
    /// Corrective completions per turn. Default 1 (the measured
    /// shape); clamped to 0..=2 at use — every retry is a paid call.
    #[serde(default = "default_verification_retries")]
    pub max_retries: u32,
}

fn default_verification_retries() -> u32 {
    1
}

impl RunbookSpec {
    /// v2 = declares collections; v1 = single shape (legacy executor path).
    pub fn is_v2(&self) -> bool {
        !self.collections.is_empty()
    }

    /// Declared plan-flattening order (default `stepMajor`).
    pub fn execution_order(&self) -> ExecutionOrder {
        self.execution.map(|e| e.order).unwrap_or_default()
    }

    /// The collections this runbook spans, with v1 specs normalized to one
    /// implicit level-0 collection named after the shape (display/info only
    /// — v1 EXECUTION stays on the legacy shape-scoped path).
    pub fn effective_collections(&self) -> Vec<CollectionSpec> {
        if self.is_v2() {
            return self.collections.clone();
        }
        match &self.shape {
            Some(shape_ref) => vec![CollectionSpec {
                name: shape_ref.split('@').next().unwrap_or(shape_ref).to_string(),
                shape: shape_ref.clone(),
                access_level: 0,
                compartments: Vec::new(),
                sources: None,
                evidence: None,
            }],
            None => Vec::new(),
        }
    }
}

/// The GA step vocabulary — the reindex pipeline from architecture.md §7.
/// v2 runs execute every step once per collection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StepSpec {
    /// v1: count the sources bound to the shape. v2: sync the collection's
    /// declarative source binding, then count.
    ResolveSources {},
    /// Side-by-side index build; NEVER activates (cutover does that).
    BuildIndex {},
    /// Deterministic verification of the built (inactive) index.
    Verify {},
    /// Check every declared data view against Matrix — the contract
    /// exists, is verified, and its declared columns match. Read-only, and
    /// deliberately a STEP rather than an apply-time check: it needs Matrix
    /// to be reachable, and apply must not depend on a second service being
    /// up.
    VerifyDataViews {},
    /// Atomic active-pointer flip; `approval: required` pauses the run.
    Cutover {
        #[serde(default)]
        approval: Option<String>,
    },
    /// Drop chunk data for versions beyond keep_versions (manifests stay).
    RetireOld {
        #[serde(default = "default_keep")]
        keep_versions: u32,
    },
}

fn default_keep() -> u32 {
    2
}

impl StepSpec {
    pub fn name(&self) -> &'static str {
        match self {
            StepSpec::ResolveSources {} => "resolveSources",
            StepSpec::BuildIndex {} => "buildIndex",
            StepSpec::Verify {} => "verify",
            StepSpec::VerifyDataViews {} => "verifyDataViews",
            StepSpec::Cutover { .. } => "cutover",
            StepSpec::RetireOld { .. } => "retireOld",
        }
    }

    pub fn requires_approval(&self) -> bool {
        matches!(self, StepSpec::Cutover { approval: Some(a) } if a == "required")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    Pending,
    Running,
    AwaitingApproval,
    Done,
    Failed,
}

impl StepState {
    pub fn as_str(&self) -> &'static str {
        match self {
            StepState::Pending => "pending",
            StepState::Running => "running",
            StepState::AwaitingApproval => "awaiting_approval",
            StepState::Done => "done",
            StepState::Failed => "failed",
        }
    }
}

pub fn parse_runbook(yaml: &str) -> Result<RunbookDoc, String> {
    let mut doc: RunbookDoc =
        serde_yaml::from_str(yaml).map_err(|e| format!("runbook yaml: {e}"))?;
    if doc.kind != "Runbook" {
        return Err(format!("kind must be Runbook, got '{}'", doc.kind));
    }
    // '@' separates name from version in a runbook_ref; a name containing it
    // would poison ref parsing (and the numeric version ordering).
    if doc.metadata.name.trim().is_empty() || doc.metadata.name.contains('@') {
        return Err(format!(
            "runbook name '{}' must be non-empty and must not contain '@'",
            doc.metadata.name
        ));
    }
    doc.spec.steps = doc
        .spec
        .steps_raw
        .iter()
        .map(step_from_value)
        .collect::<Result<Vec<_>, _>>()?;
    if doc.spec.steps.is_empty() {
        return Err("a runbook needs at least one step".into());
    }
    if doc.spec.shape.is_none() && doc.spec.collections.is_empty() {
        return Err("a runbook needs spec.shape (v1) or spec.collections (v2)".into());
    }
    if doc.spec.shape.is_some() && !doc.spec.collections.is_empty() {
        return Err("spec.shape and spec.collections are mutually exclusive".into());
    }
    // Research profiles fail closed at APPLY. Every check here would
    // otherwise fire mid-turn, in front of a user, with money already spent.
    {
        let collections: Vec<String> = doc
            .spec
            .collections
            .iter()
            .map(|c| c.name.clone())
            .collect();
        let retrieval = doc.spec.retrieval.as_ref();
        let profiles = retrieval
            .map(|r| r.research_profiles.as_slice())
            .unwrap_or(&[]);
        let default_profile = retrieval.and_then(|r| r.default_research_profile.as_deref());
        let budget = doc
            .spec
            .completion
            .as_ref()
            .and_then(|c| c.context_char_budget);
        // A semantic data view is asked through the `intent` model task; a
        // runbook that binds one and pins no such task would refuse every turn
        // at the layer with `intent-unresolved`, so it is refused here instead.
        let has_intent_task = doc
            .spec
            .models
            .as_ref()
            .map(|m| m.tasks.contains_key("intent"))
            .unwrap_or(false);
        for v in &doc.spec.data_views {
            if v.kind.is_semantic() && !has_intent_task {
                return Err(format!(
                    "spec.dataViews.{}: a semantic data view (kind {:?}) needs `models.tasks.intent` \
                     — the model task that turns the question into measures and dimensions",
                    v.name, v.kind
                ));
            }
        }
        if !profiles.is_empty() || !doc.spec.data_views.is_empty() || default_profile.is_some() {
            validate_research(
                profiles,
                &doc.spec.data_views,
                default_profile,
                &collections,
                budget,
            )
            .map_err(|e| e.to_string())?;
        }
    }

    // completion.provider/model are shorthand for models.tasks.completion —
    // normalize here so downstream resolution has ONE place to look.
    if let Some(completion) = &doc.spec.completion {
        if completion.provider.is_some() || completion.model.is_some() {
            let models = doc.spec.models.get_or_insert_with(ModelsSpec::default);
            let entry = models.tasks.entry("completion".to_string()).or_default();
            if entry.provider.is_none() {
                entry.provider = completion.provider.clone();
            }
            if entry.model.is_none() {
                entry.model = completion.model.clone();
            }
        }
    }
    Ok(doc)
}

fn step_from_value(v: &serde_yaml::Value) -> Result<StepSpec, String> {
    let map = v
        .as_mapping()
        .filter(|m| m.len() == 1)
        .ok_or_else(|| "each step must be a single-key map like `- buildIndex: {}`".to_string())?;
    let (key, body) = map.iter().next().expect("len checked");
    let key = key.as_str().unwrap_or_default();
    let get_field = |field: &str| -> Option<&serde_yaml::Value> {
        body.as_mapping()
            .and_then(|m| m.get(serde_yaml::Value::from(field)))
    };
    Ok(match key {
        "resolveSources" => StepSpec::ResolveSources {},
        "buildIndex" => StepSpec::BuildIndex {},
        "verify" => StepSpec::Verify {},
        "verifyDataViews" => StepSpec::VerifyDataViews {},
        "cutover" => {
            // `approval` is a closed vocabulary. Anything else used to parse
            // and mean "no approval" — `approval: Required`, `approval: true`,
            // `approval: require` all silently removed the human gate, and
            // the validator only noted the absence at Info. A typo in the one
            // field that pauses a cutover must not fail open.
            let approval = match get_field("approval") {
                None => None,
                Some(v) => match v.as_str() {
                    Some("required") => Some("required".to_string()),
                    Some("none") => None,
                    Some(other) => {
                        return Err(format!(
                            "cutover.approval must be 'required' or 'none', got '{other}'"
                        ))
                    }
                    None => {
                        return Err(
                            "cutover.approval must be the string 'required' or 'none'".into()
                        )
                    }
                },
            };
            StepSpec::Cutover { approval }
        }
        "retireOld" => {
            // `as u32` silently truncated: `keep_versions: 4294967296` became
            // 0, which is "reclaim everything".
            let keep_versions = match get_field("keep_versions") {
                None => default_keep(),
                Some(v) => match v.as_u64().map(u32::try_from) {
                    Some(Ok(n)) => n,
                    _ => {
                        return Err(format!(
                            "retireOld.keep_versions must be an integer between 0 and {}",
                            u32::MAX
                        ))
                    }
                },
            };
            StepSpec::RetireOld { keep_versions }
        }
        other => return Err(format!("unknown step '{other}'")),
    })
}

impl RunbookDoc {
    pub fn runbook_ref(&self) -> String {
        format!("{}@{}", self.metadata.name, self.metadata.version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RB: &str = r#"
apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: { name: tickets-reindex, version: 1 }
spec:
  shape: support-tickets@1
  steps:
    - resolveSources: {}
    - buildIndex: {}
    - verify: {}
    - cutover: { approval: required }
    - retireOld: { keep_versions: 2 }
"#;

    const RB_V2: &str = r#"
apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: { name: field-support, version: 2 }
spec:
  collections:
    - name: public-docs
      shape: support-tickets@1
      accessLevel: 0
      sources: { filenamePrefix: "public/", mediaTypes: [text/plain, text/markdown] }
    - name: internal-eng
      shape: support-tickets@1
      accessLevel: 2
      compartments: [eng]
      sources: { filenamePrefix: "eng/" }
  retrieval: { topK: 8, rrfK: 60, candidateN: 50 }
  models:
    default: { provider: anthropic-main, tier: capable }
    tasks:
      completion: { provider: anthropic-main, model: claude-fable-5 }
      embedding:  { provider: openai-main, model: text-embedding-3-small }
    allowOverrides: [anthropic-main, openai-main]
  completion:
    promptTemplate: "Answer using only the context.\n{context}\n\nQ: {query}"
  steps:
    - resolveSources: {}
    - buildIndex: {}
    - verify: {}
    - cutover: { approval: required }
    - retireOld: { keep_versions: 2 }
"#;

    #[test]
    fn execution_order_defaults_to_step_major_and_is_declarable() {
        // Every existing runbook — none of which declares `execution:` —
        // must keep the original order.
        assert_eq!(
            parse_runbook(RB_V2).expect("parses").spec.execution_order(),
            ExecutionOrder::StepMajor
        );
        let cm = RB_V2.replace(
            "  retrieval:",
            "  execution: { order: collectionMajor }\n  retrieval:",
        );
        assert_eq!(
            parse_runbook(&cm).expect("parses").spec.execution_order(),
            ExecutionOrder::CollectionMajor
        );
        // An explicit stepMajor is legal and means the default.
        let sm = RB_V2.replace(
            "  retrieval:",
            "  execution: { order: stepMajor }\n  retrieval:",
        );
        assert_eq!(
            parse_runbook(&sm).expect("parses").spec.execution_order(),
            ExecutionOrder::StepMajor
        );
        // A misspelled order is a parse error, not a silent fallback to the
        // default — the whole point of the field is that the operator gets
        // the ordering they asked for.
        let bad = RB_V2.replace(
            "  retrieval:",
            "  execution: { order: perCollection }\n  retrieval:",
        );
        assert!(parse_runbook(&bad).is_err());
    }

    #[test]
    fn parses_the_reindex_pipeline() {
        let doc = parse_runbook(RB).expect("parses");
        assert_eq!(doc.runbook_ref(), "tickets-reindex@1");
        assert_eq!(doc.spec.steps.len(), 5);
        assert_eq!(doc.spec.steps[3].name(), "cutover");
        assert!(doc.spec.steps[3].requires_approval());
        assert!(!doc.spec.steps[1].requires_approval());
        assert!(!doc.spec.is_v2());
        // v1 normalizes to one implicit level-0 collection for display
        let cols = doc.spec.effective_collections();
        assert_eq!(cols.len(), 1);
        assert_eq!(cols[0].name, "support-tickets");
        assert_eq!(cols[0].access_level, 0);
    }

    #[test]
    fn rejects_at_sign_in_name() {
        // '@' separates name from version in a runbook_ref; a name with '@'
        // would poison ref parsing and the numeric version ordering.
        let yaml = r#"
apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: { name: "support@eu", version: 1 }
spec:
  collections: [{ name: c1, shape: s@1 }]
  steps: [{ buildIndex: {} }]
"#;
        let err = parse_runbook(yaml).unwrap_err();
        assert!(err.contains("'@'"), "unexpected error: {err}");
    }

    #[test]
    fn parses_the_v2_retrieval_application() {
        let doc = parse_runbook(RB_V2).expect("parses");
        assert!(doc.spec.is_v2());
        let cols = &doc.spec.collections;
        assert_eq!(cols.len(), 2);
        assert_eq!(cols[1].access_level, 2);
        assert_eq!(cols[1].compartments, vec!["eng".to_string()]);
        assert_eq!(
            cols[0].sources.as_ref().unwrap().filename_prefix.as_deref(),
            Some("public/")
        );
        let retrieval = doc.spec.retrieval.as_ref().unwrap();
        assert_eq!(retrieval.top_k, 8);
        let models = doc.spec.models.as_ref().unwrap();
        assert_eq!(
            models.tasks["completion"].model.as_deref(),
            Some("claude-fable-5")
        );
        assert!(models.allow_overrides.permits("openai-main"));
        assert!(!models.allow_overrides.permits("groq"));
        assert!(doc.spec.completion.is_some());
    }

    #[test]
    fn parses_declarative_candidate_selection_and_reranking() {
        let yaml = RB_V2.replace(
            "  retrieval: { topK: 8, rrfK: 60, candidateN: 50 }",
            r#"  retrieval:
    topK: 8
    rrfK: 60
    candidateN: 50
    queryExpansions:
      - whenAny: [visit, visited]
        addTerms: [journey, tour]
    queryExpansionWeight: 0.2
    modelQueryExpansion:
      maxTerms: 10
      maxTokens: 96
      required: false
    collectionSelection:
      maxCollections: 4
      probeCandidateN: 25
      candidatePoolPerCollection: 40
    collectionRoutes:
      - whenAll: [internal, incident]
        collections: [internal-eng]
    contentDemotions:
      - contains: "metadata-only"
        lexicalMultiplier: 0.1
        vectorDistancePenalty: 0.5
        exceptCollections: [internal-eng]"#,
        );
        let doc = parse_runbook(&yaml).expect("parses");
        let retrieval = doc.spec.retrieval.as_ref().expect("retrieval");
        assert_eq!(retrieval.query_expansions.len(), 1);
        assert_eq!(
            retrieval.query_expansions[0].add_terms,
            vec!["journey", "tour"]
        );
        assert_eq!(retrieval.query_expansion_weight, 0.2);
        assert_eq!(
            retrieval
                .model_query_expansion
                .as_ref()
                .expect("model expansion")
                .max_terms,
            10
        );
        assert_eq!(
            retrieval
                .collection_selection
                .as_ref()
                .expect("collection selection")
                .candidate_pool_per_collection,
            40
        );
        assert_eq!(
            retrieval.content_demotions[0].except_collections,
            vec!["internal-eng".to_string()]
        );
        assert_eq!(retrieval.collection_routes.len(), 1);
        assert_eq!(
            retrieval.collection_routes[0].collections,
            vec!["internal-eng"]
        );
        assert_eq!(retrieval.content_demotions.len(), 1);
        assert_eq!(retrieval.content_demotions[0].lexical_multiplier, 0.1);
    }

    #[test]
    fn retrieval_policy_typos_fail_parsing() {
        let yaml = RB_V2.replace(
            "  retrieval: { topK: 8, rrfK: 60, candidateN: 50 }",
            "  retrieval: { topK: 8, queryExpansions: [{ whenAny: [x], addTerm: [y] }] }",
        );
        let err = parse_runbook(&yaml).unwrap_err();
        assert!(err.contains("unknown field"), "unexpected error: {err}");
    }

    #[test]
    fn completion_shorthand_normalizes_into_models() {
        let yaml = r#"
apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: { name: shorthand, version: 1 }
spec:
  collections:
    - { name: c1, shape: s@1 }
  completion:
    provider: anthropic-main
    promptTemplate: "{context} {query}"
  steps:
    - buildIndex: {}
"#;
        let doc = parse_runbook(yaml).expect("parses");
        assert_eq!(
            doc.spec.models.as_ref().unwrap().tasks["completion"]
                .provider
                .as_deref(),
            Some("anthropic-main")
        );
        // default override policy is closed
        assert!(!doc
            .spec
            .models
            .as_ref()
            .unwrap()
            .allow_overrides
            .permits("anthropic-main"));
    }

    #[test]
    fn v1_and_v2_are_mutually_exclusive() {
        let yaml = r#"
apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: { name: bad, version: 1 }
spec:
  shape: s@1
  collections: [{ name: c1, shape: s@1 }]
  steps: [{ buildIndex: {} }]
"#;
        assert!(parse_runbook(yaml).is_err());
        let neither = r#"
apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: { name: bad, version: 1 }
spec:
  steps: [{ buildIndex: {} }]
"#;
        assert!(parse_runbook(neither).is_err());
    }
}

#[cfg(test)]
mod research_grammar_tests {
    use super::*;

    /// A v2 runbook with no research grammar at all — the shape every runbook
    /// in the repo has today.
    const LEGACY: &str = r#"
apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: { name: northgate, version: 2 }
spec:
  collections:
    - { name: contracts, shape: contracts@1 }
    - { name: minutes, shape: minutes@1, accessLevel: 2 }
  retrieval: { topK: 20 }
  completion: { promptTemplate: "{context}\n{query}", contextCharBudget: 16000 }
  steps:
    - resolveSources: {}
    - buildIndex: {}
"#;

    const WITH_PROFILE: &str = r#"
apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: { name: northgate, version: 3 }
spec:
  collections:
    - name: contracts
      shape: contracts@1
      evidence: { labels: [dataroom, contract] }
    - { name: minutes, shape: minutes@1 }
  dataViews:
    - name: revenue
      contract: revenue_by_region@2
      description: quarterly revenue, by region
      accessLevel: 2
  retrieval:
    topK: 20
    defaultResearchProfile: diligence
    researchProfiles:
      - name: diligence
        layers:
          - name: register
            sources: [matrix:revenue]
            requirement: required
            role: controlling
            preserveCompleteResult: true
            maxBytes: 8000
            deadlineMs: 4000
          - name: documents
            sources: [contracts, minutes]
            role: primary
          - name: ledger
            sources: [facts:ver-2026-08-28]
            requirement: fallback
            role: supporting
  completion: { promptTemplate: "{context}\n{query}", contextCharBudget: 16000 }
  models:
    tasks:
      intent: { provider: anthropic, model: claude-haiku-4-5 }
  steps:
    - resolveSources: {}
    - buildIndex: {}
    - verifyDataViews: {}
"#;

    #[test]
    fn a_legacy_runbook_parses_with_the_research_grammar_absent() {
        // The governing invariant of S-3.x: a runbook that names no profile
        // must be untouched. Not "behaves similarly" — has no hierarchy at all,
        // so `op_turn` cannot take a new branch on its behalf.
        let doc = parse_runbook(LEGACY).expect("legacy runbook still parses");
        let r = doc.spec.retrieval.expect("retrieval");
        assert!(r.research_profiles.is_empty());
        assert!(r.default_research_profile.is_none());
        assert!(doc.spec.data_views.is_empty());
        assert!(doc.spec.collections.iter().all(|c| c.evidence.is_none()));
    }

    #[test]
    fn a_legacy_runbook_serializes_without_any_new_keys() {
        // skip_serializing_if on every added field: an existing runbook that
        // round-trips must not grow keys it never declared, or every stored
        // document silently changes the first time it is read and rewritten.
        let doc = parse_runbook(LEGACY).expect("parse");
        let out = serde_yaml::to_string(&doc).expect("serialize");
        for key in [
            "dataViews",
            "researchProfiles",
            "defaultResearchProfile",
            "evidence",
        ] {
            assert!(!out.contains(key), "legacy round-trip grew '{key}':\n{out}");
        }
    }

    #[test]
    fn the_full_research_grammar_parses() {
        let doc = parse_runbook(WITH_PROFILE).expect("profile runbook parses");
        assert_eq!(doc.spec.data_views.len(), 1);
        assert_eq!(doc.spec.data_views[0].contract, "revenue_by_region@2");
        assert_eq!(doc.spec.data_views[0].access_level, 2);

        let labels = &doc.spec.collections[0]
            .evidence
            .as_ref()
            .expect("evidence block")
            .labels;
        assert_eq!(
            labels,
            &vec!["dataroom".to_string(), "contract".to_string()]
        );

        let r = doc.spec.retrieval.as_ref().expect("retrieval");
        assert_eq!(r.default_research_profile.as_deref(), Some("diligence"));
        let p = &r.research_profiles[0];
        assert_eq!(p.layers.len(), 3);
        assert_eq!(p.layers[0].requirement, LayerRequirementSpec::Required);
        assert_eq!(p.layers[0].role, AnswerRoleSpec::Controlling);
        assert!(p.layers[0].preserve_complete_result);
        assert_eq!(p.layers[0].max_bytes, Some(8000));
        assert_eq!(p.layers[0].deadline_ms, Some(4000));
        // Order IS the hierarchy: the register outranks the documents because
        // it is first, not because of anything it declares.
        assert_eq!(p.layers[1].name, "documents");
        assert_eq!(p.layers[2].requirement, LayerRequirementSpec::Fallback);

        assert!(doc.spec.steps.iter().any(|s| s.name() == "verifyDataViews"));
    }

    #[test]
    fn a_data_view_binds_its_contracts_parameters_in_the_runbook() {
        // Found live (2026-08-29): the fixture contract declares `as_of` as
        // required, and with no way to bind it, only zero-parameter contracts
        // were reachable at all. Bound in the RUNBOOK, never at turn time —
        // letting a turn supply one would hand the caller a knob on a query
        // whose whole point is that it was declared in advance.
        let yaml = WITH_PROFILE.replace(
            "      accessLevel: 2",
            "      parameters:
        as_of: { type: date, value: \"2026-06-30\" }
      accessLevel: 2",
        );
        let doc = parse_runbook(&yaml).expect("parses");
        let p = &doc.spec.data_views[0].parameters;
        let as_of = p.get("as_of").expect("as_of is bound");
        assert_eq!(as_of.kind, "date");
        // TEXT, always. A decimal parameter round-tripped through a JSON
        // number would reach the source having lost the precision the
        // contract exists to keep.
        assert_eq!(as_of.value, "2026-06-30");
    }

    #[test]
    fn a_data_view_with_no_parameters_serializes_none() {
        let doc = parse_runbook(WITH_PROFILE).expect("parses");
        assert!(doc.spec.data_views[0].parameters.is_empty());
        let out = serde_yaml::to_string(&doc).expect("serialize");
        assert!(
            !out.contains("parameters"),
            "no empty key:
{out}"
        );
    }

    #[test]
    fn intent_is_a_task_level_and_resolves_from_the_runbook() {
        assert!(TASK_LEVELS.contains(&"intent"));
        let doc = parse_runbook(WITH_PROFILE).expect("parse");
        let models = doc.spec.models.expect("models");
        assert_eq!(
            models.tasks.get("intent").and_then(|m| m.model.clone()),
            Some("claude-haiku-4-5".to_string())
        );
    }

    #[test]
    fn a_profile_naming_an_undeclared_collection_is_refused_at_apply() {
        let bad = WITH_PROFILE.replace("sources: [contracts, minutes]", "sources: [invoices]");
        let err = parse_runbook(&bad).expect_err("must refuse");
        assert!(
            err.contains("invoices"),
            "the error names the source: {err}"
        );
        assert!(err.contains("documents"), "and the layer: {err}");
    }

    #[test]
    fn a_required_whole_table_over_budget_is_refused_at_apply() {
        // The runbook that would refuse EVERY turn. Caught once, at apply,
        // instead of once per paid turn forever.
        let bad = WITH_PROFILE.replace("maxBytes: 8000", "maxBytes: 64000");
        let err = parse_runbook(&bad).expect_err("must refuse");
        assert!(err.contains("64000") && err.contains("16000"), "{err}");
    }

    #[test]
    fn a_dangling_default_profile_is_refused_at_apply() {
        let bad = WITH_PROFILE.replace(
            "defaultResearchProfile: diligence",
            "defaultResearchProfile: nope",
        );
        let err = parse_runbook(&bad).expect_err("must refuse");
        assert!(err.contains("nope"), "{err}");
    }

    #[test]
    fn verify_data_views_is_a_real_step_name() {
        let doc = parse_runbook(WITH_PROFILE).expect("parse");
        let names: Vec<&str> = doc.spec.steps.iter().map(|s| s.name()).collect();
        assert!(names.contains(&"verifyDataViews"), "{names:?}");
        // Read-only: it must not be able to demand an approval pause the way
        // cutover does.
        assert!(!StepSpec::VerifyDataViews {}.requires_approval());
    }
}
