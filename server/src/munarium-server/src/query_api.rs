// SPDX-License-Identifier: Apache-2.0
//! Collection-scoped answers: Server owns governance, retrieval and composition.
use crate::{
    answers_api::{AnswerContent, AnswerRequest, AnswerResponse, AnswerSource, SourceReference},
    error::ApiError,
    governance_api::{self, CollectionGovernance, Publication, QueryPolicy},
    state::AppState,
};
use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    Json,
};
use chrono::{NaiveDate, Utc};
use munarium_core::{
    retrieval::{CollectionInfo, CollectionSearchResult, SearchParams},
    KernelError,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};
use utoipa::ToSchema;

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CollectionQuery {
    pub question: String,
    pub collections: Vec<String>,
    /// Governing business date; defaults to today. It is not a caller version pin.
    pub effective_on: Option<String>,
}
#[derive(Debug, Serialize, ToSchema)]
pub struct CollectionQueryResponse {
    #[serde(flatten)]
    pub answer: AnswerResponse,
    pub governance_revisions: BTreeMap<String, i64>,
    pub vocabulary_revisions: BTreeMap<String, i64>,
    pub effective_on: String,
}

#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
pub struct PublicationDate {
    pub effective_on: Option<String>,
}

/// Authorize a retained original without downloading it or invoking a model.
/// The ingesting application owns the mapping from this identity to its bytes.
#[utoipa::path(get, path="/v1.2/collections/{id}/publications/{publication_id}",
    params(("id"=String,Path),("publication_id"=String,Path),PublicationDate),
    responses((status=200,body=Publication),(status=403,description="historical clearance required"),(status=404,description="publication unavailable")), tag="answers")]
pub async fn authorize_publication(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uid: Option<axum::Extension<crate::middleware::Uid>>,
    Path((id, publication_id)): Path<(String, String)>,
    Query(query): Query<PublicationDate>,
) -> Result<Json<Publication>, ApiError> {
    let uid = crate::middleware::uid_or_anonymous(uid.as_ref());
    let access =
        crate::rest::data_plane_access(&state, &headers, &uid, munarium_access::SCOPE_QUERY)
            .await?;
    let hidden = || KernelError::NotFound {
        kind: "publication",
        id: publication_id.clone(),
    };
    let info = governance_api::collection(&state, &access.tenant_id, &id).await?;
    if info.status != "active" || !access.permits(info.access_level, &info.compartments) {
        return Err(hidden().into());
    }
    let policy = governance_api::load(&state, &access.tenant_id, &info.id)
        .await?
        .ok_or_else(hidden)?;
    let today = Utc::now().date_naive();
    let at = query
        .effective_on
        .as_deref()
        .map(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d"))
        .transpose()
        .map_err(|_| KernelError::InvalidInput("invalid effective date".into()))?
        .unwrap_or(today);
    if at > today {
        return Err(KernelError::InvalidInput("future date is unavailable".into()).into());
    }
    if at < today && access.level < policy.query.historical_access_level {
        return Err(KernelError::Forbidden(
            "historical collection access requires additional clearance".into(),
        )
        .into());
    }
    let publication = governance_api::governing(&policy, at)?
        .unwrap_or_default()
        .into_iter()
        .find(|p| p.id == publication_id)
        .cloned()
        .ok_or_else(hidden)?;
    let member =
        governance_api::collection(&state, &access.tenant_id, &publication.collection).await?;
    if member.status != "active"
        || member.access_level > info.access_level
        || member.access_level > access.level
    {
        return Err(hidden().into());
    }
    // Re-read both authorization and policy after resolving the original.
    let snapshot = Snapshot {
        vocabulary_revision: crate::vocabulary_api::load(&state, &access.tenant_id, &info.id)
            .await?
            .revision,
        info: info.clone(),
        policy: Some(policy),
    };
    let target = Target {
        info: member,
        parent: info.id,
        publication: Some(publication.clone()),
        lexical_domain: String::new(),
    };
    recheck(&state, &headers, &uid, &[snapshot], &[target]).await?;
    Ok(Json(publication))
}
struct Snapshot {
    info: CollectionInfo,
    policy: Option<CollectionGovernance>,
    vocabulary_revision: i64,
}
#[derive(Clone)]
struct Target {
    info: CollectionInfo,
    parent: String,
    publication: Option<Publication>,
    lexical_domain: String,
}
fn no_answer(status: &str) -> AnswerResponse {
    AnswerResponse {
        api_version: "1.2".into(),
        content: AnswerContent {
            status: status.into(),
            answer: if status == "review" {
                "The files require a governing-version review before an answer can be prepared."
            } else {
                "No answer was found in the available files."
            }
            .into(),
            citations: vec![],
        },
        references: vec![],
        provider: String::new(),
        model: String::new(),
        input_tokens: 0,
        output_tokens: 0,
    }
}
fn unavailable() -> ApiError {
    KernelError::Forbidden("collection scope or governance changed during query".into()).into()
}

async fn recheck(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    uid: &str,
    snapshots: &[Snapshot],
    targets: &[Target],
) -> Result<(), ApiError> {
    let access =
        crate::rest::data_plane_access(state, headers, uid, munarium_access::SCOPE_QUERY).await?;
    for snapshot in snapshots {
        let info = governance_api::collection(state, &access.tenant_id, &snapshot.info.id).await?;
        if info.status != "active"
            || !access.permits(info.access_level, &info.compartments)
            || governance_api::load(state, &access.tenant_id, &info.id)
                .await?
                .map_or(0, |p| p.revision)
                != snapshot.policy.as_ref().map_or(0, |p| p.revision)
            || crate::vocabulary_api::load(state, &access.tenant_id, &info.id)
                .await?
                .revision
                != snapshot.vocabulary_revision
        {
            return Err(unavailable());
        }
    }
    for target in targets {
        let info = governance_api::collection(state, &access.tenant_id, &target.info.id).await?;
        if info.status != "active"
            || info.access_level > access.level
            || info.access_level != target.info.access_level
            || info.compartments != target.info.compartments
        {
            return Err(unavailable());
        }
    }
    Ok(())
}

#[utoipa::path(post, path="/v1.2/query", request_body=CollectionQuery, responses((status=200,body=CollectionQueryResponse),(status=403,description="query scope required"),(status=404,description="collection unavailable")), tag="answers")]
pub async fn query_collections(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uid: Option<axum::Extension<crate::middleware::Uid>>,
    crate::rest::ProblemJson(req): crate::rest::ProblemJson<CollectionQuery>,
) -> Result<Json<CollectionQueryResponse>, ApiError> {
    let uid = crate::middleware::uid_or_anonymous(uid.as_ref());
    let access =
        crate::rest::data_plane_access(&state, &headers, &uid, munarium_access::SCOPE_QUERY)
            .await?;
    let today = Utc::now().date_naive();
    let at = req
        .effective_on
        .as_deref()
        .map(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d"))
        .transpose()
        .map_err(|_| KernelError::InvalidInput("invalid effective date".into()))?
        .unwrap_or(today);
    if req.question.trim().is_empty()
        || req.question.len() > 8000
        || req.collections.is_empty()
        || req.collections.len() > 32
        || at > today
    {
        return Err(KernelError::InvalidInput(
            "invalid question, collection scope or future date".into(),
        )
        .into());
    }
    let retrieval = Arc::new(state.retrieval_for(&access.tenant_id)?);
    let mut snapshots = Vec::new();
    let mut targets = Vec::new();
    let mut pools = Vec::new();
    let mut decisions_require_review = false;
    let mut query_policy: Option<QueryPolicy> = None;
    let mut seen = HashSet::new();
    for name in &req.collections {
        let info = governance_api::collection(&state, &access.tenant_id, name).await?;
        if info.status != "active" || !access.permits(info.access_level, &info.compartments) {
            return Err(KernelError::NotFound {
                kind: "collection",
                id: name.clone(),
            }
            .into());
        }
        if !seen.insert(info.id.clone()) {
            continue;
        }
        let policy = governance_api::load(&state, &access.tenant_id, &info.id).await?;
        let q = policy
            .as_ref()
            .map(|p| p.query.for_access_level(access.level))
            .unwrap_or_default();
        if !q.enabled {
            return Err(KernelError::NotFound {
                kind: "collection",
                id: name.clone(),
            }
            .into());
        }
        if at < today && access.level < q.historical_access_level {
            return Err(KernelError::Forbidden(
                "historical collection access requires additional clearance".into(),
            )
            .into());
        }
        if let Some(combined) = &mut query_policy {
            if combined.provider != q.provider || combined.tier != q.tier {
                return Err(KernelError::InvalidInput(
                    "selected collections use different answer providers or tiers".into(),
                )
                .into());
            }
            combined.allow_external_processing &= q.allow_external_processing;
            combined.max_passages = combined.max_passages.min(q.max_passages);
            combined.max_output_tokens = match (combined.max_output_tokens, q.max_output_tokens) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            combined.max_context_characters = combined
                .max_context_characters
                .min(q.max_context_characters);
            combined.retrieval_concurrency =
                combined.retrieval_concurrency.min(q.retrieval_concurrency);
        } else {
            query_policy = Some(q.clone());
        }
        let vocabulary = crate::vocabulary_api::load(&state, &access.tenant_id, &info.id).await?;
        let rules = if vocabulary.enabled {
            vocabulary
                .groups
                .iter()
                .map(|g| munarium_core::retrieval::QueryExpansionRule {
                    when_any: g.clone(),
                    add_terms: g.clone(),
                })
                .collect()
        } else {
            vec![]
        };
        let params = SearchParams {
            top_k: q.candidates_per_index,
            query_expansions: rules,
            ..Default::default()
        };
        let prepared = Arc::new(retrieval.prepare_query(&req.question, &params));
        let mut members = Vec::new();
        if let Some(policy) = &policy {
            match governance_api::governing(policy, at)? {
                None => decisions_require_review = true,
                Some(publications) => {
                    for publication in publications {
                        let member = governance_api::collection(
                            &state,
                            &access.tenant_id,
                            &publication.collection,
                        )
                        .await?;
                        if member.status != "active"
                            || member.access_level > info.access_level
                            || member.access_level > access.level
                        {
                            return Err(unavailable());
                        }
                        members.push(Target {
                            info: member,
                            parent: info.id.clone(),
                            publication: Some(publication.clone()),
                            lexical_domain: String::new(),
                        });
                    }
                }
            }
        } else {
            members.push(Target {
                info: info.clone(),
                parent: info.id.clone(),
                publication: None,
                lexical_domain: String::new(),
            });
        }
        targets.extend(members.clone());
        snapshots.push(Snapshot {
            info,
            policy,
            vocabulary_revision: vocabulary.revision,
        });
        pools.push((members, prepared, q.retrieval_concurrency));
    }
    let query_policy = query_policy.expect("nonempty authorized collections");
    let mut results = Vec::new();
    if !decisions_require_review {
        for (members, prepared, concurrency) in pools {
            for batch in members.chunks(concurrency.min(query_policy.retrieval_concurrency)) {
                recheck(&state, &headers, &uid, &snapshots, &[]).await?;
                let mut tasks = tokio::task::JoinSet::new();
                for mut target in batch.iter().cloned() {
                    let retrieval = retrieval.clone();
                    let prepared = prepared.clone();
                    tasks.spawn(async move {
                        let pin = target
                            .publication
                            .as_ref()
                            .map(|p| p.index_version.as_str());
                        let (mut result, domain) = retrieval
                            .search_collection_measured(&target.info.id, &prepared, pin)
                            .await?;
                        // BM25 depends on the corpus statistics of its index;
                        // separate indexes and different expanded queries are
                        // separate lexical comparability domains.
                        target.lexical_domain = if domain.starts_with("datastore/") {
                            format!(
                                "{}/{}/{}",
                                target.parent, domain, result.envelope.index_version
                            )
                        } else {
                            format!("{}/{domain}", target.parent)
                        };
                        if let Some(p) = &target.publication {
                            if result.envelope.index_version != p.index_version {
                                return Err(KernelError::Storage(
                                    "governed index pin mismatch".into(),
                                ));
                            }
                            result.hits.retain(|h| {
                                h.source_id == p.source_id
                                    && h.source_content_hash == p.source_content_hash
                            });
                        }
                        Ok::<_, KernelError>((target, result))
                    });
                }
                while let Some(result) = tasks.join_next().await {
                    results.push(result.map_err(|_| {
                        KernelError::Storage("collection query worker failed".into())
                    })??);
                }
            }
        }
    }
    // Stable order removes scheduling from ranking and citation identities.
    results.sort_by(|a, b| {
        (&a.0.parent, &a.0.info.id, &a.1.envelope.index_version).cmp(&(
            &b.0.parent,
            &b.0.info.id,
            &b.1.envelope.index_version,
        ))
    });
    let merged_inputs: Vec<_> = results
        .iter()
        .enumerate()
        .map(|(i, (t, r))| CollectionSearchResult {
            collection_id: t.info.id.clone(),
            collection_name: i.to_string(),
            result: r.clone(),
        })
        .collect();
    let domains = results
        .iter()
        .enumerate()
        .map(|(i, (t, _))| {
            (
                i.to_string(),
                (
                    t.lexical_domain.clone(),
                    format!(
                        "{}/{}",
                        t.parent,
                        munarium_retrieval::merge::LOCAL_VECTOR_DOMAIN
                    ),
                ),
            )
        })
        .collect();
    let hits = munarium_retrieval::merge::merge_hits_in_domains(
        &merged_inputs,
        results.iter().map(|(_, r)| r.hits.len()).sum(),
        60.0,
        &Default::default(),
        &domains,
    );
    let mut sources = Vec::new();
    let mut references = HashMap::new();
    let mut used = HashSet::new();
    let mut characters = 0;
    let mut selected_documents: HashMap<String, HashSet<String>> = HashMap::new();
    for (pool, hit) in hits {
        if sources.len() == query_policy.max_passages {
            break;
        }
        if hit.text.trim().is_empty()
            || !used.insert((
                hit.source_id.clone(),
                hit.source_content_hash.clone(),
                hit.text.clone(),
            ))
        {
            continue;
        }
        let cost = serde_json::to_string(&hit.text)
            .map_err(|_| KernelError::Storage("passage serialization failed".into()))?
            .len();
        if characters + cost > query_policy.max_context_characters {
            continue;
        }
        let (target, result) = pool
            .parse::<usize>()
            .ok()
            .and_then(|i| results.get(i))
            .ok_or_else(|| KernelError::Storage("retrieval provenance missing".into()))?;
        let collection = target.info.id.clone();
        characters += cost;
        let id = format!("p{}", sources.len() + 1);
        if let Some(p) = &target.publication {
            selected_documents
                .entry(target.parent.clone())
                .or_default()
                .insert(p.document_id.clone());
        }
        sources.push(AnswerSource {
            id: id.clone(),
            collection: collection.clone(),
            index_version: result.envelope.index_version.clone(),
            source_id: hit.source_id.clone(),
            source_path: hit.source_path.clone(),
            source_content_hash: hit.source_content_hash.clone(),
            text: hit.text,
        });
        references.insert(
            id.clone(),
            SourceReference {
                id,
                collection_id: collection,
                collection_name: target.info.name.clone(),
                index_version: result.envelope.index_version.clone(),
                source_id: hit.source_id,
                source_path: hit.source_path,
                source_content_hash: hit.source_content_hash,
                chunk_id: hit.chunk_id,
                metadata: hit.metadata,
            },
        );
    }
    for snapshot in &snapshots {
        if let (Some(policy), Some(documents)) =
            (&snapshot.policy, selected_documents.get(&snapshot.info.id))
        {
            decisions_require_review |=
                governance_api::needs_amendment_review(policy, at, documents)?;
        }
    }
    recheck(&state, &headers, &uid, &snapshots, &targets).await?;
    let answer = if sources.is_empty() {
        no_answer(if decisions_require_review {
            "review"
        } else {
            "insufficient"
        })
    } else {
        crate::answers_api::compose_verified(
            &state,
            &access.tenant_id,
            AnswerRequest {
                question: req.question,
                sources,
                expected_provider: None,
                expected_model: None,
            },
            &references,
            Some(&query_policy),
            decisions_require_review,
        )
        .await?
    };
    recheck(&state, &headers, &uid, &snapshots, &targets).await?;
    Ok(Json(CollectionQueryResponse {
        answer,
        governance_revisions: snapshots
            .iter()
            .map(|s| {
                (
                    s.info.id.clone(),
                    s.policy.as_ref().map_or(0, |p| p.revision),
                )
            })
            .collect(),
        vocabulary_revisions: snapshots
            .iter()
            .map(|s| (s.info.id.clone(), s.vocabulary_revision))
            .collect(),
        effective_on: at.to_string(),
    }))
}
