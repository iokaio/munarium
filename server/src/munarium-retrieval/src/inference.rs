// SPDX-License-Identifier: Apache-2.0
//! Inference stages at the retrieval coordinator (ClearView IR-0, IR-2,
//! IR-3, I1).
//!
//! The v1.4 path is unchanged and remains the default: a [`Retrieval`] with
//! no governor, or a collection the governor reports as unenrolled, searches
//! exactly as before. For an enrolled collection the governor supplies a
//! [`CollectionPlan`] and the coordinator applies it:
//!
//! 1. **Embedding.** When the index being read is a semantic index, the query
//!    is embedded through the plan's endpoint in that index's space and the
//!    vector leg runs over the companion vectors. A hash index (the prior
//!    approved index after a rollback, or a pinned historical one) is read
//!    with its own hash embedder: an index is always read in its own space.
//! 2. **Reranking** (single-collection reads). The head of the fused list is
//!    reordered by the endpoint's relevance order. Permutation only, all or
//!    nothing, and the stage receipt records what ran.
//! 3. **Policy (I1).** A stage that cannot run is a typed refusal under
//!    `require`, the default. Only an explicit `prefer` permits the stage's
//!    disclosed fallback: lexical-only search for the embedder, the original
//!    fused order for the reranker. Authorization and evidence checks run
//!    unchanged on every path; a fallback never widens what a caller sees.
//! 4. **Shadow.** Explicitly authorized shadow comparisons run after the
//!    served result is fixed, in their own task, and can only write their own
//!    comparison record.
//!
//! Admission, budgets, attribution and transport belong to the endpoint
//! implementation the governor hands out; this module never sees a
//! credential, an address or a tenant budget.

use std::sync::Arc;
use std::time::{Duration, Instant};

use munarium_core::inference::{
    blend_normalized, normalize, permute_prefix, refusal, EmbedCall, FailureKind,
    InferenceEndpoint, InferenceFailure, RerankCall, Stage, StagePolicy,
};
use munarium_core::retrieval::{PreparedSearchQuery, SearchHit, SearchResult};
use munarium_core::Result;
use munarium_retrieval_pg::semantic::{SemanticQuery, SemanticSpace};

/// Whether a read is one collection's own result or one member of a fan-out
/// whose results are merged afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadKind {
    /// The hits are the response (`POST /v1/search`, v1.2 search, answers).
    Single,
    /// The hits are merged with other collections' by raw leg scores, which a
    /// per-collection reorder cannot survive. The reranking stage is reported
    /// as not applicable for this read kind and is never charged.
    FanOut,
}

/// An endpoint, or the reason the stage cannot reach one.
pub type StageEndpoint = std::result::Result<Arc<dyn InferenceEndpoint>, InferenceFailure>;

/// A configured embedding stage.
#[derive(Clone)]
pub struct EmbeddingStage {
    pub policy: StagePolicy,
    pub endpoint: StageEndpoint,
    pub model: String,
    /// The semantic space the binding defines. A semantic index is read only
    /// when its recorded identity equals this one.
    pub space: SemanticSpace,
    pub query_instruction: String,
    pub passage_instruction: String,
}

/// A configured reranking stage.
#[derive(Clone)]
pub struct RerankStage {
    pub policy: StagePolicy,
    pub endpoint: StageEndpoint,
    pub model: String,
    /// The candidate cap: at most this many head candidates are scored.
    pub max_candidates: usize,
    /// A candidate passage longer than this is a refusal, never truncated.
    pub max_document_bytes: usize,
    /// The binding's rerank instruction (hub ADR 0029: Server applies it,
    /// the endpoint applies none). `None` sends the query unchanged.
    pub instruction: Option<String>,
}

impl RerankStage {
    /// The stage identity a receipt records: see [`rerank_identity`].
    pub fn identity(&self) -> String {
        rerank_identity(&self.model, self.instruction.as_deref())
    }
}

/// The `query` a reranking stage sends. With an instruction it is the
/// Qwen3-Reranker card's query part, `<Instruct>: {instruction}\n<Query>:
/// {query}`; the endpoint's fixed template supplies the system prompt, the
/// role markers and `\n<Document>: `. Without one the query is unchanged,
/// exactly as before the setting existed.
pub fn rerank_query(instruction: Option<&str>, query: &str) -> String {
    match instruction {
        Some(instruction) => format!("<Instruct>: {instruction}\n<Query>: {query}"),
        None => query.to_string(),
    }
}

/// The reranking stage's identity: the model name alone when no instruction
/// is configured (the Phase 6 identity, unchanged), otherwise the model name
/// with a digest of the instruction, so a receipt shows which instruction
/// shaped the order without carrying its text.
pub fn rerank_identity(model: &str, instruction: Option<&str>) -> String {
    match instruction {
        Some(instruction) => format!("{model}#instruct-{}", fingerprint(instruction)),
        None => model.to_string(),
    }
}

/// Explicitly authorized shadow work for one collection. The endpoints here
/// are metered as shadow work.
#[derive(Clone, Default)]
pub struct ShadowPlan {
    /// Rerank the served head in shadow (when reranking is not serving).
    pub rerank: Option<RerankStage>,
    /// Search this candidate semantic index in shadow (when it is not the
    /// index being served).
    pub semantic_index: Option<(String, EmbeddingStage)>,
}

/// What an enrolled read must do, from the collection's binding.
#[derive(Clone)]
pub struct CollectionPlan {
    /// The binding revision; recorded with every fallback and refusal.
    pub policy_revision: u64,
    /// One deadline for the whole read's stages: queueing, attempts and
    /// delivery.
    pub timeout: Duration,
    pub embedding: Option<EmbeddingStage>,
    pub rerank: Option<RerankStage>,
    pub shadow: ShadowPlan,
    pub recorder: Arc<dyn InferenceRecorder>,
}

/// Supplies plans. Implemented by Server from the binding table; a
/// collection with no binding is `None` and keeps the v1.4 path.
#[async_trait::async_trait]
pub trait InferenceGovernor: Send + Sync {
    async fn plan(&self, collection_id: &str, caller: &str) -> Result<Option<CollectionPlan>>;
}

/// A stage that did not apply on a governed read (I1).
#[derive(Debug, Clone)]
pub struct StageEvent {
    pub collection_id: String,
    pub stage: Stage,
    pub policy: StagePolicy,
    pub policy_revision: u64,
    /// `fallback` (prefer) or `refused` (require).
    pub outcome: &'static str,
    pub failure: InferenceFailure,
}

/// One shadow comparison. Chunk identities only; the query is a fingerprint.
#[derive(Debug, Clone)]
pub struct ShadowComparison {
    pub collection_id: String,
    /// `rerank` or `semantic_index`.
    pub kind: &'static str,
    pub query_fingerprint: String,
    pub served_index_id: String,
    pub candidate_index_id: Option<String>,
    /// `compared` or `failed`.
    pub outcome: &'static str,
    pub reason: Option<&'static str>,
    pub k: u32,
    pub overlap_at_k: Option<u32>,
    pub top1_agrees: Option<bool>,
    pub moved_positions: Option<u32>,
}

/// Durable records of governed reads. Failures to record are the
/// implementation's to log; they never change a served result.
#[async_trait::async_trait]
pub trait InferenceRecorder: Send + Sync {
    async fn stage_event(&self, event: StageEvent);
    async fn shadow(&self, comparison: ShadowComparison);
}

/// The receipt of one configured stage on one read (R1 disclosure).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageReceipt {
    pub stage: Stage,
    pub policy: StagePolicy,
    pub policy_revision: u64,
    /// True only when the stage's output shaped the served result.
    pub applied: bool,
    /// `ready` when it ran; otherwise the failure's diagnostic state, or
    /// `not_applicable`.
    pub state: &'static str,
    /// A content-free reason when the stage did not apply.
    pub reason: Option<&'static str>,
    /// The disclosed fallback that served instead (`lexical_only` or
    /// `fused_order`), only under `prefer`.
    pub fallback: Option<&'static str>,
    /// The stage identity: the semantic space, or the reranking model.
    pub identity: Option<String>,
    /// Candidates the reranker scored.
    pub scored: Option<u64>,
}

fn failed(kind: FailureKind, reason: &'static str) -> InferenceFailure {
    InferenceFailure::new(kind, reason)
}

fn check_deadline(deadline: Instant) -> std::result::Result<(), InferenceFailure> {
    if Instant::now() >= deadline {
        Err(failed(FailureKind::DeadlineExceeded, "deadline"))
    } else {
        Ok(())
    }
}

/// The query texts to embed, with the stage's instruction, and the blend
/// weight: the semantic counterpart of the hash embedder's original/expanded
/// blend. One input when the expansion is off, unchanged or full.
fn query_inputs(prepared: &PreparedSearchQuery, instruction: &str) -> (Vec<String>, f32) {
    let (original, expanded, weight) = match &prepared.lexical {
        Some(plan) => (
            plan.original.as_str(),
            plan.expanded.as_str(),
            plan.expansion_weight.clamp(0.0, 1.0) as f32,
        ),
        None => ("", "", 0.0),
    };
    let with = |t: &str| format!("{instruction}{t}");
    if weight <= 0.0 || original == expanded {
        (vec![with(original)], 0.0)
    } else if weight >= 1.0 {
        (vec![with(expanded)], 0.0)
    } else {
        (vec![with(original), with(expanded)], weight)
    }
}

/// Embed a query in a stage's space, normalized.
pub async fn embed_query(
    stage: &EmbeddingStage,
    prepared: &PreparedSearchQuery,
    deadline: Instant,
) -> std::result::Result<Vec<f32>, InferenceFailure> {
    check_deadline(deadline)?;
    let endpoint = stage.endpoint.as_ref().map_err(|f| *f)?;
    let (inputs, weight) = query_inputs(prepared, &stage.query_instruction);
    let reply = endpoint
        .embed(EmbedCall {
            model: &stage.model,
            inputs: &inputs,
            dimensions: stage.space.dimensions,
            deadline,
        })
        .await?;
    let mut vectors = reply.vectors.into_iter();
    let first = vectors
        .next()
        .ok_or(InferenceFailure::invalid("count_mismatch"))?;
    Ok(match vectors.next() {
        Some(second) => blend_normalized(&first, &second, weight),
        None => {
            let mut v = first;
            normalize(&mut v);
            v
        }
    })
}

/// The order a reranker gives the head of `hits`, or why it could not.
async fn rerank_order(
    stage: &RerankStage,
    query: &str,
    hits: &[SearchHit],
    deadline: Instant,
) -> std::result::Result<Vec<usize>, InferenceFailure> {
    check_deadline(deadline)?;
    let endpoint = stage.endpoint.as_ref().map_err(|f| *f)?;
    let n = hits.len().min(stage.max_candidates);
    let documents: Vec<String> = hits[..n].iter().map(|h| h.text.clone()).collect();
    if documents.iter().any(|d| d.len() > stage.max_document_bytes) {
        return Err(failed(FailureKind::InputTooLarge, "document_bytes"));
    }
    let query = rerank_query(stage.instruction.as_deref(), query);
    let reply = endpoint
        .rerank(RerankCall {
            model: &stage.model,
            query: &query,
            documents: &documents,
            deadline,
        })
        .await?;
    if reply.order.len() != n {
        return Err(InferenceFailure::invalid("count_mismatch"));
    }
    Ok(reply.order)
}

/// Reorder a result by a validated order and keep its envelope in step.
/// Every hit, score and citation identity is preserved.
fn apply_order(result: &mut SearchResult, order: &[usize]) -> bool {
    let hits = std::mem::take(&mut result.hits);
    match permute_prefix(hits.clone(), order) {
        Some(reordered) => {
            result.envelope.chunk_ids = reordered.iter().map(|h| h.chunk_id.clone()).collect();
            result.hits = reordered;
            true
        }
        None => {
            result.hits = hits;
            false
        }
    }
}

fn not_applicable(
    stage: Stage,
    policy: StagePolicy,
    revision: u64,
    reason: &'static str,
) -> StageReceipt {
    StageReceipt {
        stage,
        policy,
        policy_revision: revision,
        applied: false,
        state: "not_applicable",
        reason: Some(reason),
        fallback: None,
        identity: None,
        scored: None,
    }
}

fn fingerprint(query: &str) -> String {
    use sha2::Digest as _;
    hex::encode(&sha2::Sha256::digest(query.as_bytes())[..16])
}

impl crate::Retrieval {
    /// Attach the governor that supplies enrolled collections' plans.
    pub fn with_inference(mut self, governor: Arc<dyn InferenceGovernor>) -> Self {
        self.governor = Some(governor);
        self
    }

    /// Name the initiating caller every endpoint attempt is attributed to.
    pub fn with_caller(mut self, caller: &str) -> Self {
        self.caller = Some(Arc::from(caller));
        self
    }

    /// The governed collection search: the v1.4 dispatch for an unenrolled
    /// collection, the plan otherwise.
    pub(crate) async fn search_governed(
        &self,
        collection_id: &str,
        prepared: &PreparedSearchQuery,
        index_version: Option<&str>,
        kind: ReadKind,
    ) -> Result<(SearchResult, crate::SearchExecution)> {
        let caller = self.caller.as_deref().unwrap_or("unattributed");
        let plan = match &self.governor {
            Some(governor) => governor.plan(collection_id, caller).await?,
            None => None,
        };
        let Some(plan) = plan else {
            return self
                .search_collection_dispatch(collection_id, prepared, index_version)
                .await;
        };
        self.pg
            .assert_scope_readable("collection", collection_id)
            .await?;
        let deadline = Instant::now() + plan.timeout;
        let revision = plan.policy_revision;
        let (index_id, _) = self
            .pg
            .resolve_index_version(collection_id, index_version)
            .await?;
        let semantic = self.pg.semantic_identity(&index_id).await?;
        let mut stages = Vec::new();

        let (mut result, mut execution) = match semantic {
            None => {
                if let Some(stage) = &plan.embedding {
                    // A hash index is read in its own space: after a rollback
                    // the prior approved index serves with no endpoint at all.
                    stages.push(not_applicable(
                        Stage::Embedding,
                        stage.policy,
                        revision,
                        "hash_index",
                    ));
                }
                self.search_collection_dispatch(collection_id, prepared, Some(&index_id))
                    .await?
            }
            Some((identity, dims)) => {
                let policy = plan
                    .embedding
                    .as_ref()
                    .map(|s| s.policy)
                    .unwrap_or_default();
                let embedded = match &plan.embedding {
                    _ if self.mode == crate::RetrievalMode::Datastore => {
                        Err(failed(FailureKind::Unconfigured, "engine_unsupported"))
                    }
                    None => Err(failed(FailureKind::Unconfigured, "stage_not_bound")),
                    Some(EmbeddingStage {
                        endpoint: Err(f), ..
                    }) => Err(*f),
                    Some(stage)
                        if stage.space.identity != identity || stage.space.dimensions != dims =>
                    {
                        Err(failed(FailureKind::Unconfigured, "identity_mismatch"))
                    }
                    Some(stage) => embed_query(stage, prepared, deadline).await,
                };
                match embedded {
                    Ok(vector) => {
                        let query = SemanticQuery {
                            vector,
                            fingerprint: format!("endpoint/{identity}/{dims}"),
                        };
                        let result = self
                            .pg
                            .search_collection_semantic(collection_id, prepared, &index_id, &query)
                            .await?;
                        stages.push(StageReceipt {
                            stage: Stage::Embedding,
                            policy,
                            policy_revision: revision,
                            applied: true,
                            state: "ready",
                            reason: None,
                            fallback: None,
                            identity: Some(identity),
                            scored: None,
                        });
                        (result, crate::SearchExecution::postgres(prepared))
                    }
                    Err(failure) => {
                        self.stage_not_applied(
                            &plan,
                            collection_id,
                            Stage::Embedding,
                            policy,
                            failure,
                        )
                        .await?;
                        // `prefer`: the disclosed lexical-only search of the
                        // same index. The vector leg is absent, not faked.
                        let mut lexical = prepared.clone();
                        lexical.embedding = None;
                        let result = self
                            .pg
                            .search_collection_prepared(collection_id, &lexical, Some(&index_id))
                            .await?;
                        stages.push(StageReceipt {
                            stage: Stage::Embedding,
                            policy,
                            policy_revision: revision,
                            applied: false,
                            state: failure.kind.state(),
                            reason: Some(failure.reason),
                            fallback: Some("lexical_only"),
                            identity: Some(identity),
                            scored: None,
                        });
                        (result, crate::SearchExecution::postgres(&lexical))
                    }
                }
            }
        };

        let mut reranked = false;
        if let Some(stage) = &plan.rerank {
            let query = prepared
                .lexical
                .as_ref()
                .map(|p| p.original.as_str())
                .unwrap_or_default();
            if kind == ReadKind::FanOut {
                stages.push(not_applicable(
                    Stage::Reranking,
                    stage.policy,
                    revision,
                    "merged_read",
                ));
            } else if query.trim().is_empty() {
                // An endpoint refuses an empty query; nothing is ranked.
                stages.push(not_applicable(
                    Stage::Reranking,
                    stage.policy,
                    revision,
                    "empty_query",
                ));
            } else if result.hits.len().min(stage.max_candidates) < 2 {
                stages.push(not_applicable(
                    Stage::Reranking,
                    stage.policy,
                    revision,
                    "too_few_candidates",
                ));
            } else {
                let outcome = match rerank_order(stage, query, &result.hits, deadline).await {
                    Ok(order) if apply_order(&mut result, &order) => Ok(order.len()),
                    Ok(_) => Err(InferenceFailure::invalid("not_a_permutation")),
                    Err(f) => Err(f),
                };
                match outcome {
                    Ok(scored) => {
                        reranked = true;
                        stages.push(StageReceipt {
                            stage: Stage::Reranking,
                            policy: stage.policy,
                            policy_revision: revision,
                            applied: true,
                            state: "ready",
                            reason: None,
                            fallback: None,
                            identity: Some(stage.identity()),
                            scored: Some(scored as u64),
                        });
                    }
                    Err(failure) => {
                        self.stage_not_applied(
                            &plan,
                            collection_id,
                            Stage::Reranking,
                            stage.policy,
                            failure,
                        )
                        .await?;
                        stages.push(StageReceipt {
                            stage: Stage::Reranking,
                            policy: stage.policy,
                            policy_revision: revision,
                            applied: false,
                            state: failure.kind.state(),
                            reason: Some(failure.reason),
                            fallback: Some("fused_order"),
                            identity: Some(stage.identity()),
                            scored: None,
                        });
                    }
                }
            }
        }

        self.spawn_shadow(&plan, collection_id, prepared, &result, kind, reranked);
        execution.stages = stages;
        Ok((result, execution))
    }

    /// Record a stage that did not apply, and refuse the read under
    /// `require`. Returns `Ok` only when the policy permits the fallback.
    async fn stage_not_applied(
        &self,
        plan: &CollectionPlan,
        collection_id: &str,
        stage: Stage,
        policy: StagePolicy,
        failure: InferenceFailure,
    ) -> Result<()> {
        let outcome = match policy {
            StagePolicy::Require => "refused",
            StagePolicy::Prefer => "fallback",
        };
        plan.recorder
            .stage_event(StageEvent {
                collection_id: collection_id.to_string(),
                stage,
                policy,
                policy_revision: plan.policy_revision,
                outcome,
                failure,
            })
            .await;
        match policy {
            StagePolicy::Require => Err(refusal(stage, failure, plan.policy_revision)),
            StagePolicy::Prefer => Ok(()),
        }
    }

    /// Start the plan's shadow comparisons over the fixed served result.
    fn spawn_shadow(
        &self,
        plan: &CollectionPlan,
        collection_id: &str,
        prepared: &PreparedSearchQuery,
        served: &SearchResult,
        kind: ReadKind,
        reranked: bool,
    ) {
        let rerank = plan.shadow.rerank.clone().filter(|_| {
            !reranked
                && kind == ReadKind::Single
                && served.hits.len() >= 2
                && prepared
                    .lexical
                    .as_ref()
                    .is_some_and(|p| !p.original.trim().is_empty())
        });
        let semantic = plan
            .shadow
            .semantic_index
            .clone()
            .filter(|(index, _)| *index != served.envelope.index_version);
        if rerank.is_none() && semantic.is_none() {
            return;
        }
        let pg = self.pg.clone();
        let recorder = plan.recorder.clone();
        let timeout = plan.timeout;
        let collection_id = collection_id.to_string();
        let prepared = prepared.clone();
        let served = served.clone();
        tokio::spawn(async move {
            let query = prepared
                .lexical
                .as_ref()
                .map(|p| p.original.clone())
                .unwrap_or_default();
            let query_fingerprint = fingerprint(&query);
            let base = |kind, candidate: Option<String>, k: usize| ShadowComparison {
                collection_id: collection_id.clone(),
                kind,
                query_fingerprint: query_fingerprint.clone(),
                served_index_id: served.envelope.index_version.clone(),
                candidate_index_id: candidate,
                outcome: "failed",
                reason: None,
                k: k as u32,
                overlap_at_k: None,
                top1_agrees: None,
                moved_positions: None,
            };
            if let Some(stage) = rerank {
                let deadline = Instant::now() + timeout;
                let k = served.hits.len().min(stage.max_candidates);
                let mut record = base("rerank", None, k);
                match rerank_order(&stage, &query, &served.hits, deadline).await {
                    Ok(order) => {
                        record.outcome = "compared";
                        record.top1_agrees = Some(order.first() == Some(&0));
                        record.moved_positions =
                            Some(order.iter().enumerate().filter(|(i, o)| i != *o).count() as u32);
                    }
                    Err(f) => record.reason = Some(f.reason),
                }
                recorder.shadow(record).await;
            }
            if let Some((index_id, stage)) = semantic {
                let deadline = Instant::now() + timeout;
                let k = served.hits.len();
                let mut record = base("semantic_index", Some(index_id.clone()), k);
                let candidate = match embed_query(&stage, &prepared, deadline).await {
                    Ok(vector) => pg
                        .search_collection_semantic(
                            &collection_id,
                            &prepared,
                            &index_id,
                            &SemanticQuery {
                                vector,
                                fingerprint: stage.space.fingerprint(),
                            },
                        )
                        .await
                        .map_err(|_| "search_failed"),
                    Err(f) => Err(f.reason),
                };
                match candidate {
                    Ok(candidate) => {
                        let served_ids: Vec<&str> =
                            served.hits.iter().map(|h| h.chunk_id.as_str()).collect();
                        let candidate_ids: Vec<&str> = candidate
                            .hits
                            .iter()
                            .take(k)
                            .map(|h| h.chunk_id.as_str())
                            .collect();
                        record.outcome = "compared";
                        record.overlap_at_k = Some(
                            candidate_ids
                                .iter()
                                .filter(|c| served_ids.contains(c))
                                .count() as u32,
                        );
                        record.top1_agrees = Some(served_ids.first() == candidate_ids.first());
                    }
                    Err(reason) => record.reason = Some(reason),
                }
                recorder.shadow(record).await;
            }
        });
    }

    /// Build (or find) a collection's semantic index for `space`, never
    /// activated (IR-2). Its vectors are filled by
    /// [`Self::fill_semantic_index`].
    pub async fn build_semantic_index(
        &self,
        collection_id: &str,
        max_chars: usize,
        watermark_seq: u64,
        space: &SemanticSpace,
    ) -> Result<munarium_core::retrieval::IndexVersion> {
        self.pg
            .build_semantic_index(collection_id, max_chars, watermark_seq, space)
            .await
    }

    /// The semantic identity an index version records, `None` for a hash
    /// index.
    pub async fn semantic_identity(&self, index_id: &str) -> Result<Option<(String, usize)>> {
        self.pg.semantic_identity(index_id).await
    }

    /// A semantic index's chunk and vector counts.
    pub async fn semantic_coverage(
        &self,
        collection_id: &str,
        index_id: &str,
    ) -> Result<munarium_retrieval_pg::semantic::SemanticCoverage> {
        self.pg.semantic_coverage(collection_id, index_id).await
    }

    /// Fill a semantic index's companion vectors through an embedding stage
    /// (IR-2): batch by batch, resumable, each batch under the build-job
    /// fence and stop signal when queued. Returns the final coverage.
    #[allow(clippy::too_many_arguments)]
    pub async fn fill_semantic_index(
        &self,
        collection_id: &str,
        index_id: &str,
        stage: &EmbeddingStage,
        batch: usize,
        batch_timeout: Duration,
        fence: Option<&munarium_store_pg::jobs::JobFence>,
        signal: Option<&munarium_store_pg::jobs::JobSignal>,
    ) -> Result<munarium_retrieval_pg::semantic::SemanticCoverage> {
        let endpoint = stage.endpoint.as_ref().map_err(|f| build_failure(*f))?;
        loop {
            if let (Some(fence), Some(signal)) = (fence, signal) {
                if signal.stopped().is_some() {
                    return Err(fence.stale());
                }
            }
            let pending = self
                .pg
                .semantic_chunks_pending(collection_id, index_id, batch.max(1) as i64)
                .await?;
            if pending.is_empty() {
                break;
            }
            let inputs: Vec<String> = pending
                .iter()
                .map(|(_, text)| format!("{}{text}", stage.passage_instruction))
                .collect();
            let reply = endpoint
                .embed(EmbedCall {
                    model: &stage.model,
                    inputs: &inputs,
                    dimensions: stage.space.dimensions,
                    deadline: Instant::now() + batch_timeout,
                })
                .await
                .map_err(build_failure)?;
            let rows: Vec<(String, Vec<f32>)> = pending
                .into_iter()
                .zip(reply.vectors)
                .map(|((id, _), mut v)| {
                    normalize(&mut v);
                    (id, v)
                })
                .collect();
            self.pg
                .write_semantic_vectors(collection_id, index_id, &rows, fence)
                .await?;
        }
        self.pg.semantic_coverage(collection_id, index_id).await
    }
}

/// A build's endpoint failure as a kernel error: budget exhaustion is the
/// daily-cap refusal it is, everything else a content-free provider error.
fn build_failure(f: InferenceFailure) -> munarium_core::KernelError {
    match f.kind {
        FailureKind::BudgetExhausted => munarium_core::KernelError::RateLimited(format!(
            "semantic index build stopped: embedding budget exhausted ({})",
            f.reason
        )),
        _ => munarium_core::KernelError::Provider(format!(
            "semantic index build stopped: embedding stage {} ({})",
            f.kind.state(),
            f.reason
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use munarium_core::retrieval::{LexicalQueryPlan, QUERY_POLICY_VERSION};

    fn prepared(original: &str, expanded: &str, weight: f64) -> PreparedSearchQuery {
        PreparedSearchQuery {
            lexical: Some(LexicalQueryPlan {
                original: original.into(),
                expanded: expanded.into(),
                expansion_weight: weight,
                lexemes: vec![],
                demotions: vec![],
                minimum_should_match: 1,
                stop_term_fraction: 0.0,
                policy_version: QUERY_POLICY_VERSION,
            }),
            embedding: None,
            lexical_candidates: 50,
            vector_candidates: 50,
            top_k: 10,
            rrf_k: 60.0,
        }
    }

    #[test]
    fn query_inputs_follow_the_expansion_weight() {
        let (one, _) = query_inputs(&prepared("a", "a", 0.5), "Q: ");
        assert_eq!(one, vec!["Q: a"]);
        let (orig, _) = query_inputs(&prepared("a", "a b", 0.0), "");
        assert_eq!(orig, vec!["a"]);
        let (exp, _) = query_inputs(&prepared("a", "a b", 1.0), "");
        assert_eq!(exp, vec!["a b"]);
        let (both, w) = query_inputs(&prepared("a", "a b", 0.25), "I:");
        assert_eq!(both, vec!["I:a", "I:a b"]);
        assert!((w - 0.25).abs() < 1e-6);
    }

    #[test]
    fn the_rerank_query_is_the_qwen3_card_format_or_unchanged() {
        let task = "Given a web search query, retrieve relevant passages that answer the query";
        assert_eq!(
            rerank_query(Some(task), "What is the capital of China?").as_bytes(),
            b"<Instruct>: Given a web search query, retrieve relevant passages that answer the query\n<Query>: What is the capital of China?"
        );
        // Unset: the query is sent byte for byte as before.
        assert_eq!(rerank_query(None, "orchid  nebula\n"), "orchid  nebula\n");
    }

    #[test]
    fn the_rerank_identity_records_the_instruction() {
        // Unset keeps the Phase 6 identity: the model name.
        assert_eq!(rerank_identity("m", None), "m");
        let a = rerank_identity("m", Some("find the passage"));
        let b = rerank_identity("m", Some("find the passages"));
        assert!(a.starts_with("m#instruct-"), "{a}");
        assert_eq!(a.len(), "m#instruct-".len() + 32);
        assert_ne!(a, b, "a changed instruction is a changed identity");
        assert_ne!(a, rerank_identity("n", Some("find the passage")));
        assert_eq!(a, rerank_identity("m", Some("find the passage")));
        // The identity carries a digest, never the instruction text.
        assert!(!a.contains("find"));
    }

    #[test]
    fn a_build_budget_failure_is_the_daily_cap_class() {
        let e = build_failure(failed(FailureKind::BudgetExhausted, "daily_cap"));
        assert!(matches!(e, munarium_core::KernelError::RateLimited(_)));
        let e = build_failure(failed(FailureKind::Unavailable, "transport"));
        assert!(matches!(e, munarium_core::KernelError::Provider(_)));
    }
}
