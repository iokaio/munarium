// SPDX-License-Identifier: Apache-2.0
//! Explicit vocabulary scope for applications with separately pinned file indexes.
use crate::{error::ApiError, state::AppState};
use axum::{extract::State, http::HeaderMap, Json};
use munarium_api_conv::Convert;
use munarium_core::{retrieval::SearchParams, KernelError};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::ToSchema;

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct VocabularySearchRequest {
    pub query: String,
    pub collection: String,
    pub index_version: Option<String>,
    /// Defaults to the searched collection. A different vocabulary scope must
    /// independently be authorized; it never widens the searched file scope.
    pub vocabulary_collection: Option<String>,
    pub top_k: Option<u32>,
}
#[derive(Serialize, ToSchema)]
pub struct VocabularySearchResponse {
    #[serde(flatten)]
    pub result: munarium_api_types::SearchResponse,
    pub vocabulary_revision: i64,
    pub expanded_query: String,
}
#[utoipa::path(post,path="/v1.2/search",request_body=VocabularySearchRequest,responses((status=200,body=VocabularySearchResponse)),tag="retrieval")]
pub async fn search(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uid: Option<axum::Extension<crate::middleware::Uid>>,
    crate::rest::ProblemJson(body): crate::rest::ProblemJson<VocabularySearchRequest>,
) -> Result<Json<VocabularySearchResponse>, ApiError> {
    let uid = crate::middleware::uid_or_anonymous(uid.as_ref());
    let access =
        crate::rest::data_plane_access(&state, &headers, &uid, munarium_access::SCOPE_QUERY)
            .await?;
    if body.query.trim().is_empty() || body.query.len() > 8000 {
        return Err(KernelError::InvalidInput(
            "query must be nonempty and at most 8000 bytes".into(),
        )
        .into());
    }
    let retrieval = state.retrieval_for(&access.tenant_id)?;
    let mut scopes = Vec::new();
    for name in [
        &body.collection,
        body.vocabulary_collection
            .as_ref()
            .unwrap_or(&body.collection),
    ] {
        let info = match retrieval.collection_by_id(name).await {
            Ok(c) => c,
            Err(KernelError::NotFound { .. }) => retrieval.collection_by_name(name).await?,
            Err(e) => return Err(e.into()),
        };
        if info.status != "active" || !access.permits(info.access_level, &info.compartments) {
            return Err(KernelError::NotFound {
                kind: "collection",
                id: name.clone(),
            }
            .into());
        }
        scopes.push(info);
    }
    let vocabulary = crate::vocabulary_api::load(&state, &access.tenant_id, &scopes[1].id).await?;
    let rules = if vocabulary.enabled {
        vocabulary
            .groups
            .into_iter()
            .map(|g| munarium_core::retrieval::QueryExpansionRule {
                when_any: g.clone(),
                add_terms: g,
            })
            .collect()
    } else {
        vec![]
    };
    let params = SearchParams {
        top_k: body.top_k.unwrap_or(10) as usize,
        query_expansions: rules,
        ..Default::default()
    };
    let expanded_query = munarium_retrieval::expand_query(&body.query, &params.query_expansions);
    let result = retrieval
        .search_collection(
            &scopes[0].id,
            &body.query,
            params,
            body.index_version.as_deref(),
        )
        .await?;
    Ok(Json(VocabularySearchResponse {
        result: result.convert(),
        vocabulary_revision: vocabulary.revision,
        expanded_query,
    }))
}
