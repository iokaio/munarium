// SPDX-License-Identifier: Apache-2.0
//! Collections API: create/list/get. Deliberately NO delete route —
//! collections retire softly (the removal lifecycle) and physical deletion of index
//! data is the DBA's manual runbook (docs/ops/index-deletion-runbook.md).

use crate::error::ApiError;
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use munarium_api_types as dto;
use munarium_core::{KernelError, Result};
use std::sync::Arc;

type ApiResult<T> = std::result::Result<T, ApiError>;

pub async fn collection_dto(
    retrieval: &munarium_retrieval::Retrieval,
    info: munarium_core::retrieval::CollectionInfo,
) -> Result<dto::CollectionDto> {
    let source_count = retrieval.collection_source_count(&info.id).await?;
    let active_index = retrieval.active_collection_index(&info.id).await?;
    Ok(dto::CollectionDto {
        id: info.id,
        name: info.name,
        shape_ref: info.shape_ref,
        access_level: info.access_level,
        compartments: info.compartments,
        status: info.status,
        description: info.description,
        created_at: info.created_at,
        source_count,
        active_index,
    })
}

pub async fn op_create_collection(
    state: &AppState,
    tenant_id: &str,
    req: dto::CreateCollectionRequest,
) -> Result<dto::CollectionDto> {
    // The shape must exist before a collection binds to it.
    state.ensure_shapes_loaded(tenant_id).await?;
    if state.shapes.get(tenant_id, &req.shape_ref).is_none() {
        return Err(KernelError::NotFound {
            kind: "shape",
            id: req.shape_ref.clone(),
        });
    }
    let retrieval = state.retrieval_for(tenant_id)?;
    let info = retrieval
        .ensure_collection(
            req.name.trim(),
            &req.shape_ref,
            req.access_level,
            &req.compartments,
            req.description.as_deref(),
        )
        .await?;
    collection_dto(&retrieval, info).await
}

pub async fn op_list_collections(
    state: &AppState,
    tenant_id: &str,
) -> Result<Vec<dto::CollectionDto>> {
    let retrieval = state.retrieval_for(tenant_id)?;
    let mut out = Vec::new();
    for info in retrieval.list_collections().await? {
        out.push(collection_dto(&retrieval, info).await?);
    }
    Ok(out)
}

pub async fn op_get_collection(
    state: &AppState,
    tenant_id: &str,
    id_or_name: &str,
) -> Result<dto::CollectionDto> {
    let retrieval = state.retrieval_for(tenant_id)?;
    let info = match retrieval.collection_by_id(id_or_name).await {
        Ok(info) => info,
        Err(KernelError::NotFound { .. }) => retrieval.collection_by_name(id_or_name).await?,
        Err(e) => return Err(e),
    };
    collection_dto(&retrieval, info).await
}

/// One built index version of a collection, for the /admin collection
/// viewer (2026-08-27). Manifests stay resolvable after `retireOld` reclaims
/// chunk storage, so a retired version still lists here — the operator sees
/// the whole build history, not just what is currently searchable.
pub struct IndexVersionRow {
    pub id: String,
    pub active: bool,
    pub watermark_seq: i64,
    pub built_at: String,
    pub manifest: serde_json::Value,
}

pub async fn op_collection_indexes(
    state: &AppState,
    tenant_id: &str,
    collection_id: &str,
) -> Result<Vec<IndexVersionRow>> {
    let rows: Vec<(String, bool, i64, String, serde_json::Value)> = sqlx::query_as(
        "SELECT id, active, watermark_seq, built_at::text, manifest FROM index_versions
          WHERE tenant_id = $1 AND collection_id = $2
          ORDER BY built_at DESC LIMIT 50",
    )
    .bind(tenant_id)
    .bind(collection_id)
    .fetch_all(crate::runbooks_api::pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    Ok(rows
        .into_iter()
        .map(
            |(id, active, watermark_seq, built_at, manifest)| IndexVersionRow {
                id,
                active,
                watermark_seq,
                built_at,
                manifest,
            },
        )
        .collect())
}

// ---------------------------------------------------------------------------
// REST handlers
// ---------------------------------------------------------------------------

/// POST /v1/collections
#[utoipa::path(
    post,
    path = "/v1/collections",
    tag = "collections",
    request_body = dto::CreateCollectionRequest,
    responses(
        (status = 200, description = "collection created or updated (idempotent by name)", body = dto::CollectionDto),
        (status = 400, description = "invalid input / shape mismatch"),
        (status = 404, description = "shape_ref not published")
    )
)]
pub async fn create_collection(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    crate::rest::ProblemJson(req): crate::rest::ProblemJson<dto::CreateCollectionRequest>,
) -> ApiResult<Json<dto::CollectionDto>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_rw()?;
    Ok(Json(
        op_create_collection(&state, &ctx.tenant_id, req).await?,
    ))
}

/// GET /v1/collections
#[utoipa::path(
    get,
    path = "/v1/collections",
    tag = "collections",
    responses((status = 200, description = "all collections with access requirements", body = dto::CollectionsResponse))
)]
pub async fn list_collections(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::CollectionsResponse>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    Ok(Json(dto::CollectionsResponse {
        collections: op_list_collections(&state, &ctx.tenant_id).await?,
    }))
}

/// GET /v1/collections/{id}
#[utoipa::path(
    get,
    path = "/v1/collections/{id}",
    tag = "collections",
    params(("id" = String, Path, description = "collection id (col-…) or tenant-unique name")),
    responses(
        (status = 200, description = "collection detail", body = dto::CollectionDto),
        (status = 404, description = "unknown collection")
    )
)]
pub async fn get_collection(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<dto::CollectionDto>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    Ok(Json(op_get_collection(&state, &ctx.tenant_id, &id).await?))
}
