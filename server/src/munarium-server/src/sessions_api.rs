// SPDX-License-Identifier: Apache-2.0
//! the session/turn data plane — multiturn, uid-attributed retrieval
//! over a runbook's permitted collections, with optional RAG completion.
//!
//! Access model: a session pins the runbook name@version and snapshots the
//! caller's access level + compartments at creation (a mid-session token or
//! runbook change never alters an ongoing conversation). Every turn filters
//! the runbook's collections through the snapshot (`permits`) and merges
//! per-collection hybrid results by RRF score; each collection keeps its own
//! ProvenanceEnvelope.

use crate::error::{ApiError, CustomError};
use crate::interactions::InteractionMeta;
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::Json;
use munarium_access::AccessCtx;
use munarium_api_conv::Convert;
use munarium_api_types as dto;
use munarium_core::retrieval::SearchParams;
use munarium_core::retrieval::SearchResult;
use munarium_core::{KernelError, Result};
use std::sync::Arc;

type ApiResult<T> = std::result::Result<T, ApiError>;

/// Default context budget for the completion prompt (chars of merged hits)
/// when the runbook declares no `completion.contextCharBudget`. About ten
/// 1,500-char chunks — a `topK: 20` runbook should size its own.
const CONTEXT_CHAR_BUDGET: usize = 16_000;

// The per-turn completion ceiling lives in `max_tokens_api` since 2026-09-02
// (`MaxTokensBudgets::turn_completion`: built-in 2,048,
// `MUNARIUM_MAX_TOKENS_TURN_COMPLETION`, or the tenant's `/v1/max-tokens`
// replacement); a runbook's `completion.maxTokens` still wins. A ceiling, not
// spend — the provider bills only generated tokens. Reasoning models draw
// hidden reasoning from this same budget; the truncation-aware retry in
// `op_turn` pays one 4x re-ask when the ceiling exhausted before the visible
// answer did.

fn search_params(
    spec: &munarium_runbooks::RetrievalSpec,
    top_k_override: Option<u32>,
) -> SearchParams {
    SearchParams {
        top_k: top_k_override.map(|k| k as usize).unwrap_or(spec.top_k),
        rrf_k: spec.rrf_k,
        candidate_n: spec.candidate_n,
        query_expansion_weight: spec.query_expansion_weight,
        query_expansions: spec
            .query_expansions
            .iter()
            .map(|rule| munarium_core::retrieval::QueryExpansionRule {
                when_any: rule.when_any.clone(),
                add_terms: rule.add_terms.clone(),
            })
            .collect(),
        content_demotions: spec
            .content_demotions
            .iter()
            .map(|rule| munarium_core::retrieval::ContentDemotionRule {
                contains: rule.contains.clone(),
                lexical_multiplier: rule.lexical_multiplier,
                vector_distance_penalty: rule.vector_distance_penalty,
                match_mode: rule.match_mode.as_str().to_string(),
            })
            .collect(),
        query_lexemes: Vec::new(),
        minimum_should_match: 1,
        stop_term_fraction: 0.0,
    }
}

/// The search params for ONE collection: the base params with every
/// `contentDemotions` rule that names this collection in `exceptCollections`
/// dropped. The engine's demotion rule is collection-blind by design; the
/// runbook's collection-scoped exemption is applied here, where collection
/// names are known. Applies to the selection probe and the deep search
/// alike, so an exempt catalog collection's records count as evidence too.
fn scoped_params(
    base: &SearchParams,
    spec: &munarium_runbooks::RetrievalSpec,
    collection_name: &str,
) -> SearchParams {
    if spec
        .content_demotions
        .iter()
        .all(|rule| rule.except_collections.is_empty())
    {
        return base.clone();
    }
    let mut params = base.clone();
    params.content_demotions = spec
        .content_demotions
        .iter()
        .filter(|rule| !rule.except_collections.iter().any(|c| c == collection_name))
        .map(|rule| munarium_core::retrieval::ContentDemotionRule {
            contains: rule.contains.clone(),
            lexical_multiplier: rule.lexical_multiplier,
            vector_distance_penalty: rule.vector_distance_penalty,
            match_mode: rule.match_mode.as_str().to_string(),
        })
        .collect();
    params
}

fn model_expansion_prompt(query: &str, max_terms: usize) -> String {
    format!(
        "Generate up to {max_terms} generic lexical variants that may occur in documents \
         relevant to the search question below. Return ONLY a JSON array of lowercase, \
         single-word strings. Supply synonyms, related action nouns/verbs, and older/common \
         wording. Do not answer the question. Do not add names, places, organizations, dates, \
         numbers, or facts that are not already in the question. Omit words already present.\n\n\
         Search question: {query}"
    )
}

/// Strict-first parser with the same fenced/prefixed-array rescue used by
/// runbook suggestions. Generated terms are deliberately constrained to
/// lowercase single lexical tokens: the model may widen wording, never add
/// answer-shaped names, dates, or phrases to candidate selection.
fn parse_model_expansion(text: &str, query: &str, max_terms: usize) -> Result<Vec<String>> {
    let parsed: Vec<String> = serde_json::from_str(text)
        .or_else(|_| {
            let start = text.find('[');
            let end = text.rfind(']');
            match (start, end) {
                (Some(start), Some(end)) if end > start => serde_json::from_str(&text[start..=end]),
                _ => serde_json::from_str("[]"),
            }
        })
        .map_err(|e| KernelError::Provider(format!("query expansion parse: {e}")))?;

    let mut seen: std::collections::HashSet<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect();
    let mut terms = Vec::new();
    for raw in parsed {
        let raw = raw.trim();
        let term = raw.to_lowercase();
        let valid = (2..=40).contains(&term.len())
            && raw == term
            && term.chars().any(|c| c.is_alphabetic())
            && term
                .chars()
                .all(|c| c.is_alphabetic() || c == '-' || c == '\'');
        if valid && seen.insert(term.clone()) {
            terms.push(term);
            if terms.len() == max_terms {
                break;
            }
        }
    }
    Ok(terms)
}

/// One paid model-expansion call's outcome: the accepted terms plus the
/// provider/model/token facts the turn reports on its `expansion` progress
/// event (a paid step must be visible to the caller, not only to the log).
struct ModelExpansion {
    terms: Vec<String>,
    provider: String,
    model: String,
    input_tokens: u64,
    output_tokens: u64,
}

async fn expand_query_with_model(
    state: &AppState,
    tenant: &str,
    doc: &munarium_runbooks::RunbookDoc,
    query: &str,
    spec: &munarium_runbooks::ModelQueryExpansionSpec,
) -> ApiResult<ModelExpansion> {
    let resolved = crate::models::resolve_model(doc, "query_expansion", None)?;
    let store = state.store_for(tenant).await?;
    // The runbook's own `maxTokens` when declared, else the tenant's
    // `query_expansion` budget (`/v1/max-tokens`).
    let budget = match spec.max_tokens {
        Some(declared) => declared,
        None => {
            state
                .max_tokens
                .effective(state, tenant)
                .await?
                .query_expansion
        }
    };
    let response = crate::providers_api::op_complete(
        state,
        tenant,
        store.as_ref(),
        &resolved.provider_name,
        dto::CompleteRequest {
            prompt: Some(model_expansion_prompt(query, spec.max_terms)),
            system: None,
            model: resolved.model,
            tier: resolved.tier,
            provider: None,
            max_tokens: Some(budget),
            temperature: Some(0.0),
            version_id: None,
        },
    )
    .await?;
    let terms = parse_model_expansion(&response.text, query, spec.max_terms)?;
    tracing::info!(
        provider = %response.provider,
        model = %response.model,
        terms = ?terms,
        "runbook model query expansion completed"
    );
    Ok(ModelExpansion {
        terms,
        provider: response.provider,
        model: response.model,
        input_tokens: response.input_tokens,
        output_tokens: response.output_tokens,
    })
}

/// Data-plane auth for the query scope — the shared `data_plane_access`
/// helper (JWT or a static token's mapped capabilities; scope + revocation
/// enforced; token-lifecycle errors promoted). Ingest uses the same helper
/// with SCOPE_INGEST, so the two planes never drift on token-lifecycle
/// enforcement.
async fn auth_query(state: &AppState, headers: &HeaderMap, uid: &str) -> ApiResult<AccessCtx> {
    crate::rest::data_plane_access(state, headers, uid, munarium_access::SCOPE_QUERY).await
}

/// The runbook's collections the given (level, compartments) may search,
/// resolved to their materialized DB rows.
async fn permitted_collections(
    state: &AppState,
    tenant: &str,
    doc: &munarium_runbooks::RunbookDoc,
    level: i32,
    compartments: &[String],
) -> Result<Vec<munarium_core::retrieval::CollectionInfo>> {
    let retrieval = state.retrieval_for(tenant)?;
    // The probe carries the SESSION's snapshotted clearance exactly: it
    // clears only the compartments the session recorded (never all).
    let probe = AccessCtx {
        uid: String::new(),
        tenant_id: tenant.to_string(),
        level,
        compartments: compartments.to_vec(),
        all_compartments: false,
        scopes: Vec::new(),
        runbooks: None,
        jti: String::new(),
    };
    let mut out = Vec::new();
    for spec in doc.spec.effective_collections() {
        match retrieval.collection_by_name(&spec.name).await {
            Ok(info) => {
                if info.status == "active" && probe.permits(info.access_level, &info.compartments) {
                    out.push(info);
                }
            }
            // Unapplied collection: judge by the spec, but it cannot be
            // searched (no partition) — skip.
            Err(KernelError::NotFound { .. }) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

/// Apply runbook-owned query routing after access filtering. Matching is
/// deliberately small and deterministic: case-insensitive whole tokens, all
/// triggers required, matching routes unioned. A route never grants access;
/// inaccessible or inactive collections were already removed above.
fn route_collections(
    query: &str,
    routes: &[munarium_runbooks::CollectionRouteSpec],
    collections: Vec<munarium_core::retrieval::CollectionInfo>,
) -> Vec<munarium_core::retrieval::CollectionInfo> {
    let query_tokens: std::collections::HashSet<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect();
    let mut selected = std::collections::HashSet::new();
    let mut matched = false;
    for route in routes {
        if route
            .when_all
            .iter()
            .all(|term| query_tokens.contains(&term.trim().to_lowercase()))
        {
            matched = true;
            selected.extend(route.collections.iter().cloned());
        }
    }
    if !matched {
        return collections;
    }
    collections
        .into_iter()
        .filter(|collection| selected.contains(&collection.name))
        .collect()
}

/// Load a runbook for the session plane. A bare name resolves to the latest
/// NON-removed version; an exact name@version that was removed answers the
/// runbook-removed slug (410), and an unknown ref stays not-found.
async fn session_runbook(
    state: &AppState,
    tenant: &str,
    name_or_ref: &str,
) -> ApiResult<munarium_runbooks::RunbookDoc> {
    match crate::runbooks_api::load_runbook_with_status(state, tenant, name_or_ref, false).await {
        Ok((doc, _)) => Ok(doc),
        Err(KernelError::NotFound { .. }) => {
            // Nothing visible under that ref — distinguish removed from unknown.
            match crate::runbooks_api::load_runbook_with_status(state, tenant, name_or_ref, true)
                .await
            {
                Ok((doc, status)) if status == "removed" => Err(ApiError::Custom(
                    CustomError::runbook_removed(&doc.runbook_ref()),
                )),
                Ok((doc, _)) => Ok(doc),
                Err(e) => Err(e.into()),
            }
        }
        Err(e) => Err(e.into()),
    }
}

pub async fn op_create_session(
    state: &AppState,
    access: &AccessCtx,
    runbook_name: &str,
) -> ApiResult<dto::CreateSessionResponse> {
    let tenant = &access.tenant_id;
    let doc = session_runbook(state, tenant, runbook_name).await?;
    if !access.permits_runbook(&doc.metadata.name) {
        return Err(ApiError::Mesh(KernelError::Forbidden(format!(
            "the access token does not allow runbook '{}'",
            doc.metadata.name
        ))));
    }
    let permitted =
        permitted_collections(state, tenant, &doc, access.level, &access.compartments).await?;
    if permitted.is_empty() {
        return Err(ApiError::Mesh(KernelError::Forbidden(
            "no collections in this runbook are visible at your access level".into(),
        )));
    }
    let session_id = format!("ses-{}", uuid::Uuid::now_v7().simple());
    sqlx::query(
        "INSERT INTO sessions
            (tenant_id, id, uid, runbook_ref, token_jti, access_level, compartments)
         VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(tenant)
    .bind(&session_id)
    .bind(&access.uid)
    .bind(doc.runbook_ref())
    .bind((!access.jti.is_empty()).then_some(access.jti.clone()))
    .bind(access.level)
    .bind(&access.compartments)
    .execute(crate::runbooks_api::pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    Ok(dto::CreateSessionResponse {
        session_id,
        runbook_ref: doc.runbook_ref(),
        permitted_collections: permitted.into_iter().map(|c| c.name).collect(),
    })
}

struct SessionRow {
    uid: String,
    runbook_ref: String,
    access_level: i32,
    compartments: Vec<String>,
    state: String,
}

async fn load_session(state: &AppState, tenant: &str, session_id: &str) -> Result<SessionRow> {
    let row: Option<(String, String, i32, Vec<String>, String)> = sqlx::query_as(
        "SELECT uid, runbook_ref, access_level, compartments, state
           FROM sessions WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant)
    .bind(session_id)
    .fetch_optional(crate::runbooks_api::pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    let (uid, runbook_ref, access_level, compartments, session_state) =
        row.ok_or_else(|| KernelError::NotFound {
            kind: "session",
            id: session_id.to_string(),
        })?;
    Ok(SessionRow {
        uid,
        runbook_ref,
        access_level,
        compartments,
        state: session_state,
    })
}

/// Best-effort progress sink for the streaming turn plane: op_turn sends a
/// [`dto::TurnProgressEvent`] at each stage boundary when one is attached.
/// A dropped receiver never fails the turn (send errors are ignored).
pub type TurnProgressTx = tokio::sync::mpsc::UnboundedSender<dto::TurnProgressEvent>;

fn emit(progress: &Option<TurnProgressTx>, event: dto::TurnProgressEvent) {
    if let Some(tx) = progress {
        let _ = tx.send(event);
    }
}

/// Search every collection in `infos` with at most `concurrency` searches
/// in flight (each holds one pooled connection while it runs). `on_done`
/// fires as each search completes — in completion order, so a caller can
/// stream progress from the first result — and the returned results are in
/// `infos` order, so everything downstream (evidence ranking, the merge's
/// tie-breaks) stays deterministic regardless of which shard answered
/// first. A task that fails to join (a panic in a search) surfaces as a
/// storage error rather than a silently missing collection.
/// Apply one collection's scoped knobs to the shared prepared query.
///
/// The expansion and the query VECTOR are shared; cloning is an Arc refcount
/// bump, not a copy. Only what `scoped_params` actually varies per collection
/// is overridden -- today that is contentDemotions, via the runbook's
/// exceptCollections exemption. The knobs are re-read from the per-collection
/// params too, so a future field that starts varying is carried rather than
/// silently ignored.
///
/// One function, used by the fan-out AND by the shadow submit, so the
/// candidate provably searches the same prepared variant the reference did --
/// two copies of this adjustment would be two ways for them to diverge.
fn scoped_prepared(
    prepared: &munarium_core::retrieval::PreparedSearchQuery,
    params: &SearchParams,
) -> munarium_core::retrieval::PreparedSearchQuery {
    let mut prepared = prepared.clone();
    if let Some(plan) = prepared.lexical.as_mut() {
        plan.demotions = params.content_demotions.clone();
    }
    prepared.lexical_candidates = params.candidate_n;
    prepared.vector_candidates = params.candidate_n;
    prepared.top_k = params.top_k;
    prepared.rrf_k = params.rrf_k;
    prepared
}

async fn search_collections_bounded(
    retrieval: &munarium_retrieval::Retrieval,
    infos: &[munarium_core::retrieval::CollectionInfo],
    prepared: &munarium_core::retrieval::PreparedSearchQuery,
    params_for: &(dyn Fn(&munarium_core::retrieval::CollectionInfo) -> SearchParams + Sync),
    concurrency: usize,
    mut on_done: impl FnMut(&munarium_core::retrieval::CollectionInfo, &Result<SearchResult>) + Send,
) -> Result<Vec<(Result<SearchResult>, std::time::Duration)>> {
    let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency.max(1)));
    let mut tasks = tokio::task::JoinSet::new();
    for (index, info) in infos.iter().enumerate() {
        let retrieval = retrieval.clone();
        let collection_id = info.id.clone();
        let params = params_for(info);
        let prepared = scoped_prepared(prepared, &params);
        let semaphore = semaphore.clone();
        tasks.spawn(async move {
            let _permit = semaphore.acquire_owned().await;
            // Timed per collection so shadow comparisons can report the
            // reference's real latency rather than a fan-out average.
            let started = std::time::Instant::now();
            let result = retrieval
                .search_collection_prepared(&collection_id, &prepared, None)
                .await;
            (index, result, started.elapsed())
        });
    }
    let mut slots: Vec<Option<(Result<SearchResult>, std::time::Duration)>> =
        (0..infos.len()).map(|_| None).collect();
    while let Some(joined) = tasks.join_next().await {
        let (index, result, elapsed) = joined.map_err(|error| {
            KernelError::Storage(format!("collection search task failed: {error}"))
        })?;
        on_done(&infos[index], &result);
        slots[index] = Some((result, elapsed));
    }
    Ok(slots
        .into_iter()
        .map(|slot| slot.expect("every spawned search joined"))
        .collect())
}

/// The document layer, exactly as turns have always executed it.
///
/// Lifted out of `op_turn` unchanged so that the evidence hierarchy's
/// document provider and the legacy no-profile path run the *same code* rather
/// than two implementations that are supposed to agree. Route selection, model
/// query expansion, weighted fusion and the progress events are all still here,
/// in their original order.
#[derive(Default)]
pub(crate) struct DocumentRetrieval {
    pub merged: Vec<(String, munarium_core::retrieval::SearchHit)>,
    pub hits: Vec<dto::TurnHitDto>,
    pub envelopes: Vec<dto::CollectionEnvelopeDto>,
    pub searched: Vec<String>,
    pub skipped: Vec<String>,
}

pub(crate) async fn retrieve_documents(
    state: &AppState,
    tenant: &str,
    doc: &munarium_runbooks::RunbookDoc,
    permitted: Vec<munarium_core::retrieval::CollectionInfo>,
    req: &dto::TurnRequest,
    progress: &Option<TurnProgressTx>,
) -> ApiResult<DocumentRetrieval> {
    let retrieval_spec = doc.spec.retrieval.clone().unwrap_or_default();
    let permitted = route_collections(&req.query, &retrieval_spec.collection_routes, permitted);
    let final_top_k = req
        .top_k
        .map(|value| value as usize)
        .unwrap_or(retrieval_spec.top_k);

    // Optional generic two-stage candidate selection. Probe every permitted
    // collection with the ORIGINAL query and a bounded pool, rank the
    // collections by the evidence those pools carry (the query's own
    // phrases, then term density, then vector distance — see
    // `select_collection_indices`), and run the full expanded search only
    // over the strongest. No collection names or query vocabulary are
    // compiled into this path.
    let retrieval = state.retrieval_for(tenant)?;
    // Number-form normalization (2026-08-30, §13.5 entry 25; the tuning
    // study's class A). A question writing `4,436,097` cannot reach a corpus
    // writing `US4436097`: the parser makes one token of the doc form and
    // three of the query's, and every leg — lexical, the bag-of-words
    // vector, model expansion (which forbids numbers) — runs on those
    // tokens. Deterministically, with no vocabulary in the engine: the
    // query's identifier-shaped numbers contribute their joined digit forms,
    // plus the letter-prefixed forms the PERMITTED collections' own indexes
    // hold for those digits. The forms are appended to the query used for
    // the selection probe and the deep lexical/vector search — never to
    // routing, never to the question the completion prompt shows. A query
    // with no such number takes the existing path with no extra round trip.
    let number_digits = munarium_retrieval::number_query_digits(&req.query);
    let effective_query = if number_digits.is_empty() {
        req.query.clone()
    } else {
        let corpus_forms = retrieval
            .number_form_lexemes(&permitted, &number_digits)
            .await?;
        let mut extra: Vec<String> = number_digits;
        extra.extend(corpus_forms);
        let lower = req.query.to_lowercase();
        extra.retain(|t| !lower.contains(&t.to_lowercase()));
        extra.dedup();
        extra.truncate(8);
        if extra.is_empty() {
            req.query.clone()
        } else {
            format!("{} {}", req.query, extra.join(" "))
        }
    };
    let mut skipped = Vec::new();
    // Collection name → 1-based evidence rank from the selection probe; the
    // merge's optional collection-evidence leg reads it (empty = no leg).
    let mut collection_rank: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    // Probe pools of the collections NOT chosen for the deep search. They
    // were retrieved already (original query, serving policy applied) and
    // stay in the global merge: selection decides where the deep, expanded
    // search is spent, never what a turn may cite. Measured 2026-08-25 —
    // excluding them dropped the 1773 newspaper pages that report the tea's
    // destruction (the papers never say "tea party"; the narratives do).
    let mut unselected_pools: Vec<munarium_core::retrieval::CollectionSearchResult> = Vec::new();
    let selected = if let Some(selection) = &retrieval_spec.collection_selection {
        // The probe returns its WHOLE fused pool: the evidence function reads
        // every hit's raw leg scores and text. Truncating it to a few fused
        // hits would hand the ranking one or two lexical hits chosen by the
        // RRF rank-tie order (rank-1 of each leg scores the same), not the
        // pool's strongest evidence.
        let probe_top_k = u32::try_from(selection.probe_candidate_n.max(1)).unwrap_or(u32::MAX);
        let mut probe_params = search_params(&retrieval_spec, Some(probe_top_k));
        probe_params.candidate_n = selection.probe_candidate_n;
        probe_params.query_expansions.clear();
        probe_params.query_expansion_weight = 0.0;
        // One normalization round trip per query formulation, shared by every
        // probe; each collection derives its own candidate predicate from it.
        probe_params.query_lexemes = retrieval.query_lexemes(&effective_query).await?;
        probe_params.minimum_should_match = retrieval_spec.minimum_should_match;
        probe_params.stop_term_fraction = retrieval_spec.stop_term_fraction;
        // Bounded fan-out; a `probe` event streams per collection as it
        // completes so the response carries bytes from the first result.
        // Prepared once for the whole probe fan-out: N collections, one
        // embedding. The probe deliberately clears the expansion rules and
        // sets the weight to 0, so this is the ORIGINAL query's vector -- which
        // is the point of a probe, and why it cannot share the deep search's.
        let probe_prepared = retrieval.prepare_query(&effective_query, &probe_params);
        let probe_results = search_collections_bounded(
            &retrieval,
            &permitted,
            &probe_prepared,
            &|info| scoped_params(&probe_params, &retrieval_spec, &info.name),
            retrieval_spec.search_concurrency,
            |info, result| {
                let (hits, skipped) = match result {
                    Ok(result) => (result.hits.len() as u32, false),
                    Err(KernelError::NotFound { .. }) => (0, true),
                    Err(_) => (0, false),
                };
                emit(
                    progress,
                    dto::TurnProgressEvent::Probe {
                        collection: info.name.clone(),
                        hits,
                        skipped,
                    },
                );
            },
        )
        .await?;
        let mut probes = Vec::new();
        for (info, (result, _elapsed)) in permitted.iter().zip(probe_results) {
            match result {
                Ok(result) => probes.push(munarium_core::retrieval::CollectionSearchResult {
                    collection_id: info.id.clone(),
                    collection_name: info.name.clone(),
                    result,
                }),
                Err(KernelError::NotFound { .. }) => skipped.push(info.name.clone()),
                Err(error) => return Err(error.into()),
            }
        }
        // Rank EVERY probed collection (the merge's evidence leg reads all
        // ranks); the strongest `max_collections` get the deep search.
        let ranked = munarium_retrieval::select_collection_indices(
            &probes,
            probes.len(),
            &effective_query,
            selection.phrase_boost,
        );
        for (position, &index) in ranked.iter().enumerate() {
            collection_rank.insert(probes[index].collection_name.clone(), position + 1);
        }
        let selected_ids: std::collections::HashSet<String> = ranked
            .iter()
            .take(selection.max_collections)
            .map(|&index| probes[index].collection_id.clone())
            .collect();
        let narrowed: Vec<_> = permitted
            .iter()
            .filter(|info| selected_ids.contains(&info.id))
            .cloned()
            .collect();
        let probed = probes.len() as u32;
        unselected_pools = probes
            .into_iter()
            .filter(|probe| !selected_ids.contains(&probe.collection_id))
            .collect();
        // An all-empty probe is not evidence that no collection can answer;
        // preserve the prior all-permitted behavior as the safe fallback
        // (and drop the empty probe pools so nothing is merged twice).
        let chosen = if narrowed.is_empty() {
            unselected_pools.clear();
            permitted.clone()
        } else {
            narrowed
        };
        emit(
            progress,
            dto::TurnProgressEvent::Selection {
                probed,
                selected: chosen.len() as u32,
                collections: chosen.iter().map(|info| info.name.clone()).collect(),
            },
        );
        chosen
    } else {
        permitted
    };

    let mut params = search_params(&retrieval_spec, req.top_k);
    if let Some(selection) = &retrieval_spec.collection_selection {
        params.top_k = params.top_k.max(selection.candidate_pool_per_collection);
    }
    if let Some(expansion) = &retrieval_spec.model_query_expansion {
        match expand_query_with_model(state, tenant, doc, &req.query, expansion).await {
            Ok(result) => {
                emit(
                    progress,
                    dto::TurnProgressEvent::Expansion {
                        provider: result.provider,
                        model: result.model,
                        terms: result.terms.clone(),
                        input_tokens: result.input_tokens,
                        output_tokens: result.output_tokens,
                    },
                );
                if !result.terms.is_empty() {
                    // The generated variants apply to THIS query
                    // unconditionally; the rule's trigger is the query's own
                    // tokens so `expand_query`'s whole-token gate is a no-op.
                    let triggers: Vec<String> = req
                        .query
                        .split(|c: char| !c.is_alphanumeric())
                        .filter(|token| !token.is_empty())
                        .map(str::to_lowercase)
                        .collect();
                    params
                        .query_expansions
                        .push(munarium_core::retrieval::QueryExpansionRule {
                            when_any: triggers,
                            add_terms: result.terms,
                        });
                }
            }
            Err(error) if !expansion.required => {
                tracing::warn!(error = %error, "optional model query expansion unavailable; using original query");
            }
            Err(error) => return Err(error),
        }
    }

    // The deep search normalizes the EXPANDED query so the generated variants
    // count toward the two-term requirement; each selected collection then
    // drops its own stop terms and builds its candidate predicate.
    params.query_lexemes = retrieval
        .query_lexemes(&munarium_retrieval::expand_query(
            &effective_query,
            &params.query_expansions,
        ))
        .await?;
    params.minimum_should_match = retrieval_spec.minimum_should_match;
    params.stop_term_fraction = retrieval_spec.stop_term_fraction;

    // Full per-collection search; collections without an active index are
    // skipped and reported (never silently).
    // Prepared again, not reused: the deep search carries the model-generated
    // expansion rules the probe did not have, so its expanded query -- and
    // therefore its vector -- is a different query formulation.
    let deep_prepared = retrieval.prepare_query(&effective_query, &params);
    let deep_results = search_collections_bounded(
        &retrieval,
        &selected,
        &deep_prepared,
        &|info| scoped_params(&params, &retrieval_spec, &info.name),
        retrieval_spec.search_concurrency,
        |info, result| {
            let (hits, skipped) = match result {
                Ok(result) => (result.hits.len() as u32, false),
                Err(KernelError::NotFound { .. }) => (0, true),
                Err(_) => (0, false),
            };
            emit(
                progress,
                dto::TurnProgressEvent::Retrieval {
                    collection: info.name.clone(),
                    hits,
                    skipped,
                },
            );
        },
    )
    .await?;
    let mut results = Vec::new();
    for (info, (result, elapsed)) in selected.iter().zip(deep_results) {
        match result {
            Ok(result) => {
                // Shadow mode: consider comparing this collection's answer
                // against the datastore candidate. `submit` never waits, and
                // in every other mode `shadow_plane()` is None so this line
                // costs one branch. The candidate gets the SAME per-collection
                // prepared variant the reference searched with, rebuilt
                // through the one shared `scoped_prepared`.
                if let Some(plane) = state.shadow_plane() {
                    if let Some(pool) = state.pg_pool() {
                        let scoped = std::sync::Arc::new(scoped_prepared(
                            &deep_prepared,
                            &scoped_params(&params, &retrieval_spec, &info.name),
                        ));
                        plane.submit(
                            pool.clone(),
                            tenant,
                            &effective_query,
                            &scoped,
                            &result,
                            munarium_retrieval::shadow::PhaseLatency {
                                total_ms: elapsed.as_secs_f64() * 1000.0,
                                ..Default::default()
                            },
                        );
                    }
                }
                results.push(munarium_core::retrieval::CollectionSearchResult {
                    collection_id: info.id.clone(),
                    collection_name: info.name.clone(),
                    result,
                })
            }
            Err(KernelError::NotFound { .. }) => {
                if !skipped.contains(&info.name) {
                    skipped.push(info.name.clone());
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
    // The collections that did not get the deep search still contribute
    // their probe pools (original query, serving policy applied) — see the
    // note at `unselected_pools`. They are reported as searched, with their
    // envelopes, because they were.
    let probe_collections: std::collections::HashSet<String> = unselected_pools
        .iter()
        .map(|pool| pool.collection_name.clone())
        .collect();
    results.extend(unselected_pools);

    // Weighted global fusion: the runbook's `retrieval.fusion` leg weights
    // (defaults reproduce the unweighted merge) plus the collection-evidence
    // leg fed by the selection ranking above; the probe pools are ranked as
    // their own stratum (original-query scores are not comparable with the
    // expanded search's).
    let fusion = retrieval_spec.fusion.clone().unwrap_or_default();
    let merge_weights = munarium_core::retrieval::MergeWeights {
        lexical: fusion.lexical_weight,
        vector: fusion.vector_weight,
        collection_evidence: fusion.collection_evidence_weight,
        collection_rank,
        probe_collections,
        probe_weight: fusion.unselected_pool_weight,
    };
    let merged = munarium_retrieval::merge_hits_weighted(
        &results,
        final_top_k,
        params.rrf_k,
        &merge_weights,
    );
    emit(
        progress,
        dto::TurnProgressEvent::Merge {
            hits: merged.len() as u32,
        },
    );
    let hits: Vec<dto::TurnHitDto> = merged
        .iter()
        .map(|(collection, h)| dto::TurnHitDto {
            collection: collection.clone(),
            chunk_id: h.chunk_id.clone(),
            source_id: h.source_id.clone(),
            source_path: h.source_path.clone(),
            source_content_hash: h.source_content_hash.clone(),
            text: h.text.clone(),
            score: h.score,
        })
        .collect();
    let envelopes: Vec<dto::CollectionEnvelopeDto> = results
        .iter()
        .map(|r| dto::CollectionEnvelopeDto {
            collection: r.collection_name.clone(),
            envelope: r.result.envelope.clone().convert(),
        })
        .collect();
    let searched: Vec<String> = results.iter().map(|r| r.collection_name.clone()).collect();
    Ok(DocumentRetrieval {
        merged,
        hits,
        envelopes,
        searched,
        skipped,
    })
}

pub async fn op_turn(
    state: &AppState,
    access: &AccessCtx,
    session_id: &str,
    req: dto::TurnRequest,
    progress: Option<TurnProgressTx>,
) -> ApiResult<(dto::TurnResponse, InteractionMeta)> {
    let tenant = &access.tenant_id;
    let session = load_session(state, tenant, session_id).await?;
    if session.uid != access.uid {
        return Err(ApiError::Mesh(KernelError::Forbidden(
            "session belongs to a different uid".into(),
        )));
    }
    if session.state != "open" {
        // Typed refusal (2026-08-17): clients read the slug + state
        // extension, never the message text (errors.md discipline).
        return Err(ApiError::Custom(
            crate::error::CustomError::session_not_open(&session.state),
        ));
    }
    if req.query.trim().is_empty() {
        return Err(ApiError::Mesh(KernelError::InvalidInput(
            "query is required".into(),
        )));
    }
    let doc = session_runbook(state, tenant, &session.runbook_ref).await?;

    // Access filtering uses the SESSION's snapshot, not the live token.
    let permitted = permitted_collections(
        state,
        tenant,
        &doc,
        session.access_level,
        &session.compartments,
    )
    .await?;

    // The branch. A turn with no profile — no `research_profile` on
    // the request and no `defaultResearchProfile` on the runbook — takes the
    // identical call it always took, so its retrieval, its response bytes and
    // its SSE sequence are unchanged by construction rather than by testing.
    let profile =
        crate::evidence_hierarchy::resolve_profile(&doc, req.research_profile.as_deref())?;

    let (
        merged,
        hits,
        envelopes,
        searched,
        skipped,
        hierarchy_dto,
        hierarchy_context,
        hierarchy_blocks,
    ) = match profile {
        None => {
            let DocumentRetrieval {
                merged,
                hits,
                envelopes,
                searched,
                skipped,
            } = retrieve_documents(state, tenant, &doc, permitted, &req, &progress).await?;
            (merged, hits, envelopes, searched, skipped, None, None, None)
        }
        Some(profile) => {
            let intent =
                crate::evidence_hierarchy::resolve_intent(state, tenant, &doc, &req, &progress)
                    .await?;
            let plan = crate::evidence_hierarchy::build_plan(profile, intent);

            let store = state.store_for(tenant).await?;
            let fact_provider = crate::evidence_providers::FactProvider { store };
            // The session's OWN snapshot, not the live token: a mid-session
            // clearance change must not alter an ongoing conversation, and
            // Matrix picks its authorization class from exactly this.
            let matrix_provider = state.matrix_provider(
                crate::evidence_providers::SessionAuthorization {
                    tenant: tenant.to_string(),
                    uid: access.uid.clone(),
                    access_level: session.access_level,
                    compartments: session.compartments.clone(),
                    session_id: session_id.to_string(),
                    runbook_ref: session.runbook_ref.clone(),
                },
                &doc,
            );
            let mut providers: Vec<&dyn munarium_core::hierarchy::EvidenceProvider> = Vec::new();
            if let Some(m) = matrix_provider.as_ref() {
                providers.push(m);
            }
            providers.push(&fact_provider);

            let permitted_for_docs = permitted.clone();
            let outcome = crate::evidence_hierarchy::execute_plan(
                &plan,
                &providers,
                |layer| {
                    let permitted = permitted_for_docs.clone();
                    let doc = &doc;
                    let req = &req;
                    let progress = &progress;
                    async move {
                        // Narrow to the layer's pinned collections, then
                        // run the SAME retrieval the legacy path runs.
                        let scoped: Vec<_> = if layer.sources.is_empty() {
                            permitted
                        } else {
                            permitted
                                .into_iter()
                                .filter(|c| layer.sources.iter().any(|s| s == &c.name))
                                .collect()
                        };
                        retrieve_documents(state, tenant, doc, scoped, req, progress).await
                    }
                },
                |event| emit(&progress, event),
            )
            .await?;

            let budget = plan
                .context_char_budget
                .or_else(|| {
                    doc.spec
                        .completion
                        .as_ref()
                        .and_then(|c| c.context_char_budget)
                })
                .unwrap_or(CONTEXT_CHAR_BUDGET);
            let composed = crate::evidence_hierarchy::compose(&plan, &outcome.blocks, budget);
            emit(
                &progress,
                dto::TurnProgressEvent::Compose {
                    layers_used: composed.layers_used as u32,
                    context_chars: composed.context.len() as u32,
                    layers_dropped: composed.layers_dropped.clone(),
                },
            );

            let docs = outcome.documents.unwrap_or_else(|| DocumentRetrieval {
                merged: Vec::new(),
                hits: Vec::new(),
                envelopes: Vec::new(),
                searched: Vec::new(),
                skipped: Vec::new(),
            });
            (
                docs.merged,
                docs.hits,
                docs.envelopes,
                docs.searched,
                docs.skipped,
                Some(crate::evidence_hierarchy::decision_to_dto(
                    &outcome.decision,
                )),
                Some(composed.context),
                Some(outcome.blocks),
            )
        }
    };

    // Optional RAG completion through the shared model resolver.
    let mut completion_dto = None;
    let mut completion_audit = None;
    if req.complete.unwrap_or(false) {
        let template = doc
            .spec
            .completion
            .as_ref()
            .map(|c| c.prompt_template.clone())
            .ok_or_else(|| {
                KernelError::InvalidInput(
                    "this runbook declares no completion step (spec.completion)".into(),
                )
            })?;
        let override_req = req
            .model_override
            .as_ref()
            .map(|o| crate::models::ModelOverride {
                provider: o.provider.clone(),
                model: o.model.clone(),
                tier: o.tier.clone(),
            });
        let resolved = crate::models::resolve_model(&doc, "completion", override_req.as_ref())?;
        emit(
            &progress,
            dto::TurnProgressEvent::Model {
                provider: resolved.provider_name.clone(),
                model: resolved.model.clone(),
                tier: resolved.tier.clone(),
                was_override: resolved.was_override,
            },
        );
        // The runbook may size the served context (`completion.contextCharBudget`);
        // the engine default keeps the historical 16k. Hits past the budget
        // were retrieved and are reported, but never reach the model.
        let context_budget = doc
            .spec
            .completion
            .as_ref()
            .and_then(|c| c.context_char_budget)
            .unwrap_or(CONTEXT_CHAR_BUDGET);
        // A hierarchy turn's context was composed across every layer, in
        // trust order. A legacy turn builds it here exactly as it always has.
        let mut context = String::new();
        if let Some(prebuilt) = &hierarchy_context {
            context.push_str(prebuilt);
        } else {
            for (collection, h) in &merged {
                let entry = format!("[{}/{}] {}\n\n", collection, h.chunk_id, h.text);
                if context.len() + entry.len() > context_budget {
                    break;
                }
                context.push_str(&entry);
            }
        }
        let prompt = template
            .replace("{context}", &context)
            .replace("{query}", &req.query);
        let store = state.store_for(tenant).await?;
        let complete = |p: String, budget: u32| {
            let store = store.clone();
            let provider_name = resolved.provider_name.clone();
            let model = resolved.model.clone();
            let tier = resolved.tier.clone();
            async move {
                crate::providers_api::op_complete(
                    state,
                    tenant,
                    store.as_ref(),
                    &provider_name,
                    dto::CompleteRequest {
                        prompt: Some(p),
                        system: None,
                        model,
                        tier,
                        provider: None,
                        max_tokens: Some(budget),
                        temperature: None,
                        version_id: None,
                    },
                )
                .await
            }
        };
        // Runbook override first (`completion.maxTokens`, 2026-09-01 — added
        // when z-ai/glm-5.3's always-on reasoning exhausted the default AND
        // its 4x retry on a hard question, returning empty text), engine
        // default second. The retry below stays 4x whichever won.
        let base_budget = match doc.spec.completion.as_ref().and_then(|c| c.max_tokens) {
            Some(declared) => declared,
            // The tenant's replacement over the process defaults
            // (`/v1/max-tokens`, max_tokens_api.rs).
            None => {
                state
                    .max_tokens
                    .effective(state, tenant)
                    .await?
                    .turn_completion
            }
        };
        let mut budget = base_budget;
        let mut resp = complete(prompt.clone(), budget).await?;
        let mut total_in = resp.input_tokens;
        let mut total_out = resp.output_tokens;
        emit(
            &progress,
            dto::TurnProgressEvent::Completion {
                attempt: 0,
                provider: resp.provider.clone(),
                model: resp.model.clone(),
                input_tokens: resp.input_tokens,
                output_tokens: resp.output_tokens,
            },
        );

        // Truncation-aware retry (the stop-reason lesson of §17 lesson 1,
        // server-side). Reasoning models — gpt-5.4,
        // z-ai/glm-5.2 — spend hidden reasoning tokens from the completion
        // budget, so a turn can exhaust it before ANY visible text (empty
        // answer under the model badge) or mid-answer. The adapters pass the
        // provider's stop reason through verbatim: "max_tokens" (anthropic) /
        // "length" (openai dialect). Pay for exactly ONE retry at 4x budget;
        // max_tokens is a ceiling, not spend, so the retry costs only what
        // the model actually generates.
        let truncated = matches!(resp.stop_reason.as_str(), "max_tokens" | "length")
            || resp.text.trim().is_empty();
        if truncated {
            budget = base_budget * 4;
            let retry = complete(prompt.clone(), budget).await?;
            total_in += retry.input_tokens;
            total_out += retry.output_tokens;
            emit(
                &progress,
                dto::TurnProgressEvent::Completion {
                    attempt: 0,
                    provider: retry.provider.clone(),
                    model: retry.model.clone(),
                    input_tokens: retry.input_tokens,
                    output_tokens: retry.output_tokens,
                },
            );
            resp = retry;
        }

        // Deterministic verification + corrective retries (the measured
        // conformance_retry shape — dev-guide §13 entry 10). Pure
        // string checks over the SERVED hits; a violating answer gets up to
        // max_retries (clamped, default 1) corrective completions with the
        // violations and the original context attached, then stands as-is
        // with the outcome recorded.
        let mut verification_dto = None;
        let vspec = doc
            .spec
            .completion
            .as_ref()
            .and_then(|c| c.verification.clone());
        if let Some(vspec) = vspec.filter(|v| v.quotes || v.citations) {
            let served_texts: Vec<&str> = merged.iter().map(|(_, h)| h.text.as_str()).collect();
            let mut served_labels: Vec<String> = merged
                .iter()
                .map(|(collection, h)| format!("{}/{}", collection, h.chunk_id))
                .collect();
            served_labels.extend(merged.iter().map(|(_, h)| h.source_path.clone()));
            let label_refs: Vec<&str> = served_labels.iter().map(String::as_str).collect();
            // The sealed rows this turn served, so an
            // `[evidence/<id>#<row>]` citation and a typed assertion can both
            // be checked against what the model actually saw. Empty on a
            // legacy turn, where every check below is a no-op.
            let served_evidence = hierarchy_blocks
                .as_ref()
                .map(|b| crate::evidence_hierarchy::served_evidence(b))
                .unwrap_or_default();
            let run_checks = |text: &str| -> Vec<String> {
                let mut v = Vec::new();
                if vspec.quotes {
                    v.extend(
                        crate::verification::check_quotes(text, &served_texts)
                            .into_iter()
                            .map(|q| format!("quote: {q}")),
                    );
                }
                if vspec.citations {
                    v.extend(
                        crate::verification::check_citations(text, &label_refs)
                            .into_iter()
                            .map(|c| format!("citation: {c}")),
                    );
                    // It rides the SAME pass and the SAME retry budget. A
                    // second loop would double the paid calls a bad answer
                    // costs, for a class of error the one corrective re-ask
                    // already covers.
                    v.extend(
                        crate::verification::check_evidence_citations(text, &served_evidence)
                            .into_iter()
                            .map(|c| format!("citation: {c}")),
                    );
                }
                match crate::verification::extract_assertions(text) {
                    Ok(assertions) if !assertions.is_empty() => {
                        v.extend(
                            crate::verification::check_assertions(&assertions, &served_evidence)
                                .into_iter()
                                .map(|a| format!("assertion: {a}")),
                        );
                    }
                    Ok(_) => {}
                    // A block that does not parse is a violation in its own
                    // right, so the corrective retry fires on it.
                    Err(e) => v.push(format!("assertion: {e}")),
                }
                v
            };
            let checks_ran: Vec<String> = [
                vspec.quotes.then(|| "quotes".to_string()),
                vspec.citations.then(|| "citations".to_string()),
                // Named only when there was sealed evidence to check against,
                // so a legacy turn's `checks` list is unchanged.
                (!served_evidence.is_empty()).then(|| "assertions".to_string()),
            ]
            .into_iter()
            .flatten()
            .collect();
            let first_pass = run_checks(&resp.text);
            emit(
                &progress,
                dto::TurnProgressEvent::Verify {
                    // None on a legacy turn, and the field is
                    // skip_serializing_if, so this event's bytes are unchanged.
                    layer: None,
                    attempt: 0,
                    checks: checks_ran.clone(),
                    violations: first_pass.len() as u32,
                },
            );
            let mut violations = first_pass.clone();
            let mut retries = 0u32;
            while !violations.is_empty() && retries < vspec.max_retries.clamp(0, 2) {
                retries += 1;
                let quotes: Vec<String> = violations
                    .iter()
                    .filter_map(|v| v.strip_prefix("quote: ").map(String::from))
                    .collect();
                let cites: Vec<String> = violations
                    .iter()
                    .filter_map(|v| v.strip_prefix("citation: ").map(String::from))
                    .collect();
                // Corrective retries ride the current budget — raised when
                // the truncation retry fired, so a reasoning model gets the
                // same headroom for its repaired answer.
                let retry = complete(
                    crate::verification::corrective_prompt(&prompt, &resp.text, &quotes, &cites),
                    budget,
                )
                .await?;
                total_in += retry.input_tokens;
                total_out += retry.output_tokens;
                emit(
                    &progress,
                    dto::TurnProgressEvent::Completion {
                        attempt: retries,
                        provider: retry.provider.clone(),
                        model: retry.model.clone(),
                        input_tokens: retry.input_tokens,
                        output_tokens: retry.output_tokens,
                    },
                );
                resp.text = retry.text;
                resp.provider = retry.provider;
                resp.model = retry.model;
                violations = run_checks(&resp.text);
                emit(
                    &progress,
                    dto::TurnProgressEvent::Verify {
                        // None on a legacy turn, and the field is
                        // skip_serializing_if, so this event's bytes are unchanged.
                        layer: None,
                        attempt: retries,
                        checks: checks_ran.clone(),
                        violations: violations.len() as u32,
                    },
                );
            }
            verification_dto = Some(dto::TurnVerificationDto {
                checks: checks_ran,
                retries,
                first_pass_violations: first_pass,
                violations,
            });
        }

        completion_audit = Some(serde_json::json!({
            "resolved": resolved.audit_json(),
            "provider": resp.provider,
            "model": resp.model,
            "input_tokens": total_in,
            "output_tokens": total_out,
            "text": resp.text,
            "verification": verification_dto.clone(),
        }));
        completion_dto = Some(dto::TurnCompletionDto {
            provider: resp.provider,
            model: resp.model,
            was_override: resolved.was_override,
            text: resp.text,
            input_tokens: total_in,
            output_tokens: total_out,
            verification: verification_dto,
        });
    } else if req
        .model_override
        .as_ref()
        .map(|o| o.provider.is_some() || o.model.is_some() || o.tier.is_some())
        == Some(true)
    {
        // An override without a completion request would silently do nothing;
        // still enforce the policy so probing is visible, then reject.
        return Err(ApiError::Mesh(KernelError::InvalidInput(
            "model_override requires complete: true".into(),
        )));
    }

    // Persist the turn: the ordinal is allocated inside the INSERT itself.
    // Two concurrent turns can still compute the same MAX and collide on the
    // PK — that unique violation is retried here instead of surfacing as a
    // 500 storage-error.
    let mut ordinal: i32 = 0;
    for attempt in 0..3 {
        let inserted: std::result::Result<i32, sqlx::Error> = sqlx::query_scalar(
            "INSERT INTO session_turns
                (tenant_id, session_id, ordinal, uid, query, collections_searched,
                 hits, envelope, completion, hierarchy)
             VALUES ($1,$2,
                     (SELECT COALESCE(MAX(ordinal), 0) + 1 FROM session_turns
                       WHERE tenant_id = $1 AND session_id = $2),
                     $3,$4,$5,$6,$7,$8,$9)
             RETURNING ordinal",
        )
        .bind(tenant)
        .bind(session_id)
        .bind(&access.uid)
        .bind(&req.query)
        .bind(&searched)
        .bind(serde_json::to_value(&hits).unwrap_or_default())
        .bind(serde_json::to_value(&envelopes).unwrap_or_default())
        .bind(&completion_audit)
        // NULL when no profile ran — an empty object would claim a
        // hierarchy ran and decided nothing.
        .bind(
            hierarchy_dto
                .as_ref()
                .and_then(|h| serde_json::to_value(h).ok()),
        )
        .fetch_one(crate::runbooks_api::pool(state)?)
        .await;
        match inserted {
            Ok(o) => {
                ordinal = o;
                break;
            }
            Err(e) => {
                let unique_violation = e
                    .as_database_error()
                    .and_then(|d| d.code())
                    .map(|c| c == "23505")
                    .unwrap_or(false);
                if unique_violation && attempt < 2 {
                    continue;
                }
                return Err(ApiError::Mesh(KernelError::Storage(e.to_string())));
            }
        }
    }
    sqlx::query("UPDATE sessions SET last_turn_at = now() WHERE tenant_id = $1 AND id = $2")
        .bind(tenant)
        .bind(session_id)
        .execute(crate::runbooks_api::pool(state)?)
        .await
        .map_err(|e| KernelError::Storage(e.to_string()))?;

    // Names, not ids: session_turns.collections_searched uses names, so the
    // collection-grouped usage report keys stay consistent across both tables.
    let meta = InteractionMeta {
        session_id: Some(session_id.to_string()),
        runbook_ref: Some(session.runbook_ref.clone()),
        // Identical to the old `results.iter().map(collection_name)`: that IS
        // `searched`, which the extraction now returns directly.
        collection_ids: Some(searched.clone()),
        ..Default::default()
    };
    Ok((
        dto::TurnResponse {
            session_id: session_id.to_string(),
            ordinal: ordinal as u32,
            collections_searched: searched,
            skipped,
            hits,
            envelopes,
            completion: completion_dto,
            hierarchy: hierarchy_dto,
        },
        meta,
    ))
}

#[allow(clippy::type_complexity)]
pub async fn op_get_session(
    state: &AppState,
    tenant: &str,
    session_id: &str,
) -> Result<dto::SessionResponse> {
    let row: Option<(String, String, i32, Vec<String>, String, String)> = sqlx::query_as(
        "SELECT uid, runbook_ref, access_level, compartments, state, created_at::text
           FROM sessions WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant)
    .bind(session_id)
    .fetch_optional(crate::runbooks_api::pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    let (uid, runbook_ref, access_level, compartments, session_state, created_at) = row
        .ok_or_else(|| KernelError::NotFound {
            kind: "session",
            id: session_id.to_string(),
        })?;
    let turns: Vec<(
        i32,
        String,
        Vec<String>,
        serde_json::Value,
        serde_json::Value,
        Option<serde_json::Value>,
        String,
    )> = sqlx::query_as(
        "SELECT ordinal, query, collections_searched, hits, envelope, completion, created_at::text
           FROM session_turns WHERE tenant_id = $1 AND session_id = $2 ORDER BY ordinal",
    )
    .bind(tenant)
    .bind(session_id)
    .fetch_all(crate::runbooks_api::pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    Ok(dto::SessionResponse {
        session_id: session_id.to_string(),
        uid,
        runbook_ref,
        access_level,
        compartments,
        state: session_state,
        created_at,
        turns: turns
            .into_iter()
            .map(
                |(ordinal, query, searched, hits, envelope, completion, created_at)| {
                    dto::SessionTurnDto {
                        ordinal: ordinal as u32,
                        query,
                        collections_searched: searched,
                        hits,
                        envelope,
                        completion,
                        created_at,
                    }
                },
            )
            .collect(),
    })
}

/// One session row for the /admin sessions list (2026-08-27), most recent
/// activity first. `turns` is the persisted turn count; the detail page
/// reads the turns themselves through `op_get_session`.
pub struct SessionSummary {
    pub id: String,
    pub uid: String,
    pub runbook_ref: String,
    pub state: String,
    pub access_level: i32,
    pub compartments: Vec<String>,
    pub created_at: String,
    pub last_turn_at: Option<String>,
    pub turns: i64,
}

#[allow(clippy::type_complexity)]
pub async fn op_recent_sessions(
    state: &AppState,
    tenant: &str,
    state_filter: Option<&str>,
    limit: i64,
) -> Result<Vec<SessionSummary>> {
    let rows: Vec<(
        String,
        String,
        String,
        String,
        i32,
        Vec<String>,
        String,
        Option<String>,
        i64,
    )> = sqlx::query_as(
        "SELECT s.id, s.uid, s.runbook_ref, s.state, s.access_level, s.compartments,
                s.created_at::text, s.last_turn_at::text,
                (SELECT count(*) FROM session_turns t
                  WHERE t.tenant_id = s.tenant_id AND t.session_id = s.id)
           FROM sessions s
          WHERE s.tenant_id = $1 AND ($2::text IS NULL OR s.state = $2)
          ORDER BY COALESCE(s.last_turn_at, s.created_at) DESC LIMIT $3",
    )
    .bind(tenant)
    .bind(state_filter)
    .bind(limit)
    .fetch_all(crate::runbooks_api::pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    Ok(rows
        .into_iter()
        .map(
            |(
                id,
                uid,
                runbook_ref,
                session_state,
                access_level,
                compartments,
                created_at,
                last_turn_at,
                turns,
            )| SessionSummary {
                id,
                uid,
                runbook_ref,
                state: session_state,
                access_level,
                compartments,
                created_at,
                last_turn_at,
                turns,
            },
        )
        .collect())
}

// ---------------------------------------------------------------------------
// REST handlers
// ---------------------------------------------------------------------------

use crate::middleware::uid_or_anonymous as uid_of;

/// POST /v1/runbooks/{name}/sessions
#[utoipa::path(post, path = "/v1/runbooks/{name}/sessions",
    params(("name" = String, Path, description = "runbook name (latest) or name@version")),
    responses(
        (status = 200, description = "session created; runbook version pinned", body = dto::CreateSessionResponse),
        (status = 403, description = "scope missing / runbook not allowed / no permitted collections"),
        (status = 410, description = "runbook removed")
    ),
    tag = "sessions")]
pub async fn create_session(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    uid: Option<axum::Extension<crate::middleware::Uid>>,
) -> ApiResult<axum::response::Response> {
    let uid = uid_of(uid.as_ref());
    let access = auth_query(&state, &headers, &uid).await?;
    let resp = op_create_session(&state, &access, &name).await?;
    let meta = InteractionMeta {
        session_id: Some(resp.session_id.clone()),
        runbook_ref: Some(resp.runbook_ref.clone()),
        collection_ids: None,
        ..Default::default()
    };
    let mut response = Json(resp).into_response();
    response.extensions_mut().insert(meta);
    Ok(response)
}

/// POST /v1/sessions/{id}/turns
#[utoipa::path(post, path = "/v1/sessions/{id}/turns",
    params(("id" = String, Path, description = "session id (ses-…)")),
    request_body = dto::TurnRequest,
    responses(
        (status = 200, description = "access-filtered multi-collection retrieval (+ optional completion)", body = dto::TurnResponse),
        (status = 403, description = "uid/session mismatch, scope missing, or override-not-allowed"),
        (status = 410, description = "runbook removed")
    ),
    tag = "sessions")]
pub async fn turn(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    uid: Option<axum::Extension<crate::middleware::Uid>>,
    crate::rest::ProblemJson(req): crate::rest::ProblemJson<dto::TurnRequest>,
) -> ApiResult<axum::response::Response> {
    let uid = uid_of(uid.as_ref());
    let access = auth_query(&state, &headers, &uid).await?;
    let (resp, meta) = op_turn(&state, &access, &id, req, None).await?;
    let mut response = Json(resp).into_response();
    response.extensions_mut().insert(meta);
    Ok(response)
}

/// POST /v1/sessions/{id}/turns/stream — the same turn, streamed as SSE
/// phase-progress events (2026-08-23, built for the demo web app's live
/// "thinking" panel). Auth and refusals identical to the unary turn: failures
/// BEFORE the stream starts (auth, bad body) answer plain problem+json;
/// failures after are delivered as a terminal `error` event. Event names:
/// `progress` (a TurnProgressEvent per stage), then exactly one of
/// `done` (the full TurnResponse) or `error` (the problem+json body).
/// Honest by construction: events are emitted at the real op_turn stage
/// boundaries — nothing is synthesized. Delivery is live: the capture
/// middleware passes `text/event-stream` bodies through unbuffered (until
/// 2026-08-23 it buffered every /v1 response, so the whole event sequence
/// reached the client in one burst at turn end — dev-guide §13 entry 16).
/// Interaction capture happens at END of stream through the
/// [`crate::middleware::StreamOutcome`] slot this handler fills: the row
/// carries the same session/runbook/collection attribution as the unary
/// turn and the REAL outcome status (200 on `done`, the problem status on
/// `error`) rather than the 200 the stream opened with.
#[utoipa::path(post, path = "/v1/sessions/{id}/turns/stream",
    params(("id" = String, Path, description = "session id (ses-…)")),
    request_body = dto::TurnRequest,
    responses(
        (status = 200, description = "text/event-stream: `progress` events \
         (TurnProgressEvent), terminated by `done` (TurnResponse) or `error` \
         (problem+json)", body = dto::TurnProgressEvent),
        (status = 403, description = "uid/session mismatch, scope missing, or override-not-allowed")
    ),
    tag = "sessions")]
pub async fn turn_stream(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    uid: Option<axum::Extension<crate::middleware::Uid>>,
    crate::rest::ProblemJson(req): crate::rest::ProblemJson<dto::TurnRequest>,
) -> ApiResult<axum::response::Response> {
    use axum::response::sse::{Event, KeepAlive, Sse};
    use tokio_stream::StreamExt;
    let uid = uid_of(uid.as_ref());
    let access = auth_query(&state, &headers, &uid).await?;

    // End-of-stream attribution for the capture middleware (see the doc
    // comment): filled by the turn task before it sends the terminal event.
    let outcome = crate::middleware::new_stream_outcome_slot();
    let outcome_w = outcome.clone();
    let session_for_meta = id.clone();

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    let (progress_tx, mut progress_rx) =
        tokio::sync::mpsc::unbounded_channel::<dto::TurnProgressEvent>();

    // Forward progress events as they arrive (op_turn's sends are
    // non-blocking, so forwarding must not wait for the turn to finish).
    let fwd_tx = tx.clone();
    let forwarder = tokio::spawn(async move {
        while let Some(ev) = progress_rx.recv().await {
            let data = serde_json::to_string(&ev).unwrap_or_else(|_| "{}".into());
            if fwd_tx
                .send(Event::default().event("progress").data(data))
                .is_err()
            {
                break; // client went away; op_turn still completes and persists
            }
        }
    });

    let state2 = state.clone();
    tokio::spawn(async move {
        let result = op_turn(&state2, &access, &id, req, Some(progress_tx)).await;
        // progress_tx was moved into op_turn and is dropped by now, so the
        // forwarder drains and exits — awaiting it keeps the terminal event
        // strictly AFTER every progress event.
        let _ = forwarder.await;
        let event = match result {
            Ok((resp, meta)) => {
                if let Ok(mut o) = outcome_w.lock() {
                    o.meta = meta;
                    o.status = Some(200);
                }
                Event::default()
                    .event("done")
                    .data(serde_json::to_string(&resp).unwrap_or_else(|_| "{}".into()))
            }
            Err(e) => {
                let (status, problem) = match &e {
                    ApiError::Mesh(m) => crate::error::to_problem(m),
                    ApiError::Custom(c) => c.to_problem(),
                };
                if let Ok(mut o) = outcome_w.lock() {
                    // The session is known from the path even when the turn
                    // failed before op_turn could attribute it.
                    o.meta.session_id = Some(session_for_meta.clone());
                    o.status = Some(status.as_u16());
                }
                Event::default()
                    .event("error")
                    .data(serde_json::to_string(&problem).unwrap_or_else(|_| "{}".into()))
            }
        };
        // The slot is filled BEFORE the terminal event is sent, so by the
        // time the body wrapper sees end-of-stream the outcome is there.
        let _ = tx.send(event);
        // tx drops here, ending the SSE stream.
    });

    let stream = tokio_stream::wrappers::UnboundedReceiverStream::new(rx)
        .map(Ok::<_, std::convert::Infallible>);
    let mut response = Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response();
    response.extensions_mut().insert(outcome);
    Ok(response)
}

/// POST /v1/sessions/{id}/close — end a session's lifecycle (2026-08-17;
/// the `sessions.state` vocabulary finally has an API — §13 entry 11).
/// Idempotent: closing a session that is already closed or expired returns
/// its current state unchanged. The owner (capability JWT, query scope) or
/// a static rw/mgmt token may close; static `ro` may not (a close is a
/// write). Further turns against a non-open session answer 409
/// `session-not-open`.
#[utoipa::path(post, path = "/v1/sessions/{id}/close",
    params(("id" = String, Path)),
    responses((status = 200, body = dto::SessionResponse),
              (status = 403, description = "not your session / ro token"),
              (status = 404, description = "unknown session")),
    tag = "sessions")]
pub async fn close_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    uid: Option<axum::Extension<crate::middleware::Uid>>,
) -> ApiResult<Json<dto::SessionResponse>> {
    let principal = state
        .authenticate_principal(crate::rest::bearer(&headers))
        .map_err(crate::rest::promote_auth_error)?;
    let tenant = principal.tenant_id().to_string();
    let current = op_get_session(&state, &tenant, &id).await?;
    match &principal {
        crate::state::Principal::Static(ctx) if ctx.role == "ro" => {
            return Err(ApiError::Mesh(KernelError::Forbidden(
                "role 'ro' cannot close sessions (a close is a write)".into(),
            )));
        }
        crate::state::Principal::Access(_) => {
            // Same guard chain as a turn (scope + revocation), then own-uid.
            let uid = uid_of(uid.as_ref());
            let access = auth_query(&state, &headers, &uid).await?;
            if access.uid != current.uid {
                return Err(ApiError::Mesh(KernelError::Forbidden(
                    "session belongs to a different uid".into(),
                )));
            }
        }
        _ => {}
    }
    Ok(Json(op_close_session(&state, &tenant, &id).await?))
}

/// The close write itself, shared by both planes (the guard chains differ
/// per transport and stay in the handlers). Idempotent by construction:
/// only an 'open' session transitions.
pub async fn op_close_session(
    state: &AppState,
    tenant: &str,
    id: &str,
) -> Result<dto::SessionResponse> {
    sqlx::query(
        "UPDATE sessions SET state = 'closed'
          WHERE tenant_id = $1 AND id = $2 AND state = 'open'",
    )
    .bind(tenant)
    .bind(id)
    .execute(crate::runbooks_api::pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    op_get_session(state, tenant, id).await
}

/// GET /v1/sessions/{id}
#[utoipa::path(get, path = "/v1/sessions/{id}",
    params(("id" = String, Path)),
    responses((status = 200, body = dto::SessionResponse),
              (status = 403, description = "not your session"),
              (status = 404, description = "unknown session")),
    tag = "sessions")]
pub async fn get_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    uid: Option<axum::Extension<crate::middleware::Uid>>,
) -> ApiResult<Json<dto::SessionResponse>> {
    // Reading a transcript is a data-plane read: a capability token must
    // carry the query scope and pass the revocation check exactly like a
    // turn does (a revoked token must not keep reading history). Static
    // control/management tokens read any session in their tenant.
    let principal = state
        .authenticate_principal(crate::rest::bearer(&headers))
        .map_err(crate::rest::promote_auth_error)?;
    let tenant = principal.tenant_id().to_string();
    let resp = op_get_session(&state, &tenant, &id).await?;
    if let crate::state::Principal::Access(_) = &principal {
        // Go through the same guard as turns (scope + revocation), then the
        // own-uid check.
        let uid = uid_of(uid.as_ref());
        let access = auth_query(&state, &headers, &uid).await?;
        if access.uid != resp.uid {
            return Err(ApiError::Mesh(KernelError::Forbidden(
                "session belongs to a different uid".into(),
            )));
        }
    }
    Ok(Json(resp))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collection(name: &str) -> munarium_core::retrieval::CollectionInfo {
        munarium_core::retrieval::CollectionInfo {
            id: format!("col-{name}"),
            name: name.into(),
            shape_ref: "shape@1".into(),
            access_level: 0,
            compartments: Vec::new(),
            status: "active".into(),
            description: None,
            created_at: String::new(),
        }
    }

    #[test]
    fn scoped_params_drops_demotions_that_exempt_the_collection() {
        let spec = munarium_runbooks::RetrievalSpec {
            content_demotions: vec![
                munarium_runbooks::ContentDemotionSpec {
                    contains: "metadata record".into(),
                    lexical_multiplier: 0.001,
                    vector_distance_penalty: 2.0,
                    except_collections: vec!["catalog".into()],
                    match_mode: Default::default(),
                },
                munarium_runbooks::ContentDemotionSpec {
                    contains: "boilerplate".into(),
                    lexical_multiplier: 0.5,
                    vector_distance_penalty: 0.0,
                    except_collections: Vec::new(),
                    match_mode: Default::default(),
                },
            ],
            ..Default::default()
        };
        let base = search_params(&spec, None);
        assert_eq!(base.content_demotions.len(), 2);

        let letters = scoped_params(&base, &spec, "letters");
        assert_eq!(letters.content_demotions.len(), 2);

        let catalog = scoped_params(&base, &spec, "catalog");
        let markers: Vec<&str> = catalog
            .content_demotions
            .iter()
            .map(|rule| rule.contains.as_str())
            .collect();
        assert_eq!(markers, vec!["boilerplate"]);
        // Everything else on the params is untouched.
        assert_eq!(catalog.top_k, base.top_k);
        assert_eq!(catalog.candidate_n, base.candidate_n);
    }

    #[test]
    fn model_expansion_parser_accepts_only_bounded_lowercase_lexical_terms() {
        let text = r#"Here is the array: ["journey", "tour", "New York", "1791", "visit", "journey", "lodged"]"#;
        let terms = parse_model_expansion(text, "Which places did they visit?", 3).unwrap();
        assert_eq!(terms, vec!["journey", "tour", "lodged"]);
    }

    #[test]
    fn model_expansion_prompt_forbids_answer_shaped_additions() {
        let prompt = model_expansion_prompt("Who went where?", 9);
        assert!(prompt.contains("up to 9"));
        assert!(prompt.contains("Do not answer the question"));
        assert!(prompt.contains("Do not add names, places, organizations, dates"));
    }

    #[test]
    fn matching_collection_routes_narrow_candidates_and_nonmatches_do_not() {
        let all = vec![
            collection("gw-a"),
            collection("gw-b"),
            collection("newspapers"),
        ];
        let routes = vec![munarium_runbooks::CollectionRouteSpec {
            when_all: vec!["george".into(), "washington".into()],
            collections: vec!["gw-a".into(), "gw-b".into()],
        }];

        let routed = route_collections("What did George WASHINGTON visit?", &routes, all.clone());
        assert_eq!(
            routed.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["gw-a", "gw-b"]
        );
        let unchanged = route_collections("What happened in Boston?", &routes, all);
        assert_eq!(unchanged.len(), 3);
    }

    #[test]
    fn runbook_retrieval_policy_reaches_search_params() {
        let spec = munarium_runbooks::RetrievalSpec {
            top_k: 20,
            candidate_n: 400,
            query_expansion_weight: 0.2,
            query_expansions: vec![munarium_runbooks::QueryExpansionSpec {
                when_any: vec!["visit".into()],
                add_terms: vec!["journey".into()],
            }],
            content_demotions: vec![munarium_runbooks::ContentDemotionSpec {
                contains: "metadata-only".into(),
                lexical_multiplier: 0.05,
                vector_distance_penalty: 0.75,
                except_collections: Vec::new(),
                match_mode: Default::default(),
            }],
            ..Default::default()
        };

        let params = search_params(&spec, Some(7));
        assert_eq!(params.top_k, 7);
        assert_eq!(params.candidate_n, 400);
        assert_eq!(params.query_expansion_weight, 0.2);
        assert_eq!(params.query_expansions[0].add_terms, vec!["journey"]);
        assert_eq!(params.content_demotions[0].contains, "metadata-only");
        assert_eq!(params.content_demotions[0].lexical_multiplier, 0.05);
    }
}
