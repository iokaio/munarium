// SPDX-License-Identifier: Apache-2.0
use crate::{
    error::ApiError,
    rest::{auth_ctx, ProblemJson},
    runbooks_api::pool,
    state::AppState,
};
use axum::{
    extract::{Query, State},
    http::HeaderMap,
    Json,
};
use munarium_api_types as dto;
use munarium_core::KernelError;
use sqlx::Row;
use std::sync::Arc;

#[cfg(test)]
#[path = "source_retention_tests.rs"]
mod tests;

fn record(row: sqlx::postgres::PgRow) -> dto::SourceRetentionRecord {
    dto::SourceRetentionRecord {
        source_id: row.get("source_id"),
        path: row.get("path"),
        denied: row.get("denied"),
        hold: row.get("hold"),
        cleanup_state: row.get("cleanup_state"),
        cleanup_blocked: row.get("cleanup_blocked"),
        updated_at: row
            .get::<chrono::DateTime<chrono::Utc>, _>("updated_at")
            .to_rfc3339(),
    }
}
fn storage(error: sqlx::Error) -> KernelError {
    KernelError::Storage(error.to_string())
}

#[utoipa::path(post, path="/v1/source-retention", request_body=dto::SourceRetentionRequest, responses((status=200,body=dto::SourceRetentionRecord)), tag="retention")]
pub async fn change_source_retention(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ProblemJson(request): ProblemJson<dto::SourceRetentionRequest>,
) -> Result<Json<dto::SourceRetentionRecord>, ApiError> {
    let ctx = auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    let pool = pool(&state)?;
    munarium_store_pg::source_retention::change(
        pool,
        &ctx.tenant_id,
        &request.path,
        &request.action,
    )
    .await?;
    let row = sqlx::query("SELECT * FROM source_retention WHERE tenant_id=$1 AND path=$2")
        .bind(&ctx.tenant_id)
        .bind(&request.path)
        .fetch_one(pool)
        .await
        .map_err(storage)?;
    Ok(Json(record(row)))
}

#[derive(serde::Deserialize)]
pub struct ListQuery {
    after: Option<String>,
}
#[utoipa::path(get, path="/v1/source-retention", params(("after"=Option<String>, Query, description="Last source_id from the previous page")), responses((status=200,body=dto::SourceRetentionList)), tag="retention")]
pub async fn list_source_retention(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Result<Json<dto::SourceRetentionList>, ApiError> {
    let ctx = auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    let rows = sqlx::query("SELECT * FROM source_retention WHERE tenant_id=$1 AND source_id>$2 ORDER BY source_id LIMIT 101")
        .bind(&ctx.tenant_id).bind(query.after.unwrap_or_default()).fetch_all(pool(&state)?).await.map_err(storage)?;
    let more = rows.len() > 100;
    let records: Vec<_> = rows.into_iter().take(100).map(record).collect();
    let next = if more {
        records.last().map(|r| r.source_id.clone())
    } else {
        None
    };
    Ok(Json(dto::SourceRetentionList { records, next }))
}
