// SPDX-License-Identifier: Apache-2.0
//! Opt-in pre-execution command claims. A lost outcome is unresolved, not a
//! lease expiry or permission to repeat an effect. This is not an atomic
//! transaction spanning an arbitrary command and its response receipt.
use crate::{error::ApiError, state::AppState};
use axum::{
    extract::{Query, State},
    http::HeaderMap,
    Json,
};
use munarium_api_types as dto;
use munarium_core::{KernelError, Result};
use sqlx::Row;
use std::sync::Arc;

#[cfg(test)]
#[path = "command_recovery_tests.rs"]
mod tests;

pub(crate) enum Admission {
    Legacy,
    Claimed,
    Replay(String),
}

pub(crate) async fn prune_completed(pool: &sqlx::PgPool, ttl: u64) -> Result<u64> {
    if ttl == 0 {
        return Ok(0);
    }
    Ok(sqlx::query("DELETE FROM command_receipts WHERE state='completed' AND completed_at < now() - make_interval(secs => $1)")
        .bind(ttl as f64).execute(pool).await.map_err(storage)?.rows_affected())
}

fn storage(e: sqlx::Error) -> KernelError {
    KernelError::Storage(e.to_string())
}

pub(crate) fn unresolved() -> KernelError {
    KernelError::InvalidInput(format!("{}the command is in progress or may have executed; inspect its outcome before authorizing a new key", crate::error::COMMAND_UNRESOLVED_PREFIX))
}

pub(crate) async fn begin(
    state: &AppState,
    tenant: &str,
    key: &str,
    operation: &str,
    hash: &str,
) -> Result<Admission> {
    let Some(pool) = state.pg_pool() else {
        return Ok(Admission::Legacy);
    };
    let enabled: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM command_recovery_policies WHERE tenant_id = $1)",
    )
    .bind(tenant)
    .fetch_one(pool)
    .await
    .map_err(storage)?;
    if !enabled {
        return Ok(Admission::Legacy);
    }
    if key.is_empty() || key.len() > 256 {
        return Err(KernelError::InvalidInput(
            "guarded command keys must be 1-256 bytes".into(),
        ));
    }
    let mut tx = pool.begin().await.map_err(storage)?;
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('command-recovery/' || $1 || '/' || $2, 0))",
    )
    .bind(tenant)
    .bind(key)
    .execute(&mut *tx)
    .await
    .map_err(storage)?;
    let row = sqlx::query("SELECT operation, request_hash, response_body FROM command_receipts WHERE tenant_id=$1 AND key=$2")
        .bind(tenant).bind(key).fetch_optional(&mut *tx).await.map_err(storage)?;
    if let Some(row) = row {
        if row.get::<String, _>("operation") != operation
            || row.get::<String, _>("request_hash") != hash
        {
            return Err(KernelError::IdempotencyMismatch);
        }
        return row
            .get::<Option<String>, _>("response_body")
            .map(Admission::Replay)
            .ok_or_else(unresolved);
    }
    // Completed legacy receipts remain readable. They are not promoted into
    // evidence that their execution had a durable pre-dispatch claim.
    let legacy: Option<(String, String)> = sqlx::query_as(
        "SELECT request_hash, response_body FROM idempotency_keys WHERE tenant_id=$1 AND key=$2",
    )
    .bind(tenant)
    .bind(key)
    .fetch_optional(&mut *tx)
    .await
    .map_err(storage)?;
    if let Some((prior, body)) = legacy {
        return if prior == hash {
            Ok(Admission::Replay(body))
        } else {
            Err(KernelError::IdempotencyMismatch)
        };
    }
    sqlx::query("INSERT INTO command_receipts (tenant_id,key,operation,request_hash,state) VALUES ($1,$2,$3,$4,'unresolved')")
        .bind(tenant).bind(key).bind(operation).bind(hash).execute(&mut *tx).await.map_err(storage)?;
    tx.commit().await.map_err(storage)?;
    #[cfg(test)]
    crate::crash_recovery::barrier("command_claimed");
    Ok(Admission::Claimed)
}

pub(crate) async fn finish(
    state: &AppState,
    tenant: &str,
    key: &str,
    operation: &str,
    hash: &str,
    body: &str,
) -> Result<()> {
    #[cfg(test)]
    crate::crash_recovery::receipt_barrier(hash, body);
    let pool = crate::runbooks_api::pool(state)?;
    let mut tx = pool.begin().await.map_err(storage)?;
    let updated = sqlx::query("UPDATE command_receipts SET state='completed', response_body=$5, completed_at=now() WHERE tenant_id=$1 AND key=$2 AND operation=$3 AND request_hash=$4 AND state='unresolved'")
        .bind(tenant).bind(key).bind(operation).bind(hash).bind(body).execute(&mut *tx).await.map_err(storage)?;
    if updated.rows_affected() != 1 {
        return Err(unresolved());
    }
    sqlx::query("INSERT INTO idempotency_keys (tenant_id,key,request_hash,response_body,status_code) VALUES ($1,$2,$3,$4,200) ON CONFLICT (tenant_id,key) DO NOTHING")
        .bind(tenant).bind(key).bind(hash).bind(body).execute(&mut *tx).await.map_err(storage)?;
    tx.commit().await.map_err(storage)?;
    #[cfg(test)]
    crate::crash_recovery::barrier("receipt_persisted");
    Ok(())
}

#[utoipa::path(get, operation_id="command_recovery_policy", path="/v1/command-recovery", responses((status=200, body=dto::CommandRecoveryPolicy)), tag="command")]
pub async fn policy(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> std::result::Result<Json<dto::CommandRecoveryPolicy>, ApiError> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    let pool = crate::runbooks_api::pool(&state)?;
    let enabled: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM command_recovery_policies WHERE tenant_id=$1)",
    )
    .bind(&ctx.tenant_id)
    .fetch_one(pool)
    .await
    .map_err(storage)?;
    Ok(Json(dto::CommandRecoveryPolicy {
        mode: if enabled { "guarded-v1" } else { "legacy" }.into(),
    }))
}

#[utoipa::path(post, operation_id="enable_command_recovery", path="/v1/command-recovery", request_body=dto::CommandRecoveryPolicy, responses((status=200, body=dto::CommandRecoveryPolicy)), tag="command")]
pub async fn enable(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    crate::rest::ProblemJson(request): crate::rest::ProblemJson<dto::CommandRecoveryPolicy>,
) -> std::result::Result<Json<dto::CommandRecoveryPolicy>, ApiError> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    if request.mode != "guarded-v1" {
        return Err(KernelError::InvalidInput(
            "only guarded-v1 activation is supported; drain command writers before activation"
                .into(),
        )
        .into());
    }
    sqlx::query(
        "INSERT INTO command_recovery_policies (tenant_id) VALUES ($1) ON CONFLICT DO NOTHING",
    )
    .bind(&ctx.tenant_id)
    .execute(crate::runbooks_api::pool(&state)?)
    .await
    .map_err(storage)?;
    Ok(Json(request))
}

#[derive(serde::Deserialize)]
pub struct ReceiptQuery {
    key: String,
}

#[utoipa::path(get, operation_id="command_recovery_receipt", path="/v1/command-recovery/receipt", params(("key"=String, Query, description="Original command idempotency key")), responses((status=200, body=dto::CommandRecoveryReceipt)), tag="command")]
pub async fn receipt(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ReceiptQuery>,
) -> std::result::Result<Json<dto::CommandRecoveryReceipt>, ApiError> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    let row = sqlx::query("SELECT operation,state,created_at,completed_at FROM command_receipts WHERE tenant_id=$1 AND key=$2")
        .bind(&ctx.tenant_id).bind(&query.key).fetch_optional(crate::runbooks_api::pool(&state)?).await.map_err(storage)?
        .ok_or_else(|| KernelError::NotFound {kind:"command-receipt",id:query.key.clone()})?;
    Ok(Json(dto::CommandRecoveryReceipt {
        key: query.key,
        operation: row.get("operation"),
        state: row.get("state"),
        created_at: row
            .get::<chrono::DateTime<chrono::Utc>, _>("created_at")
            .to_rfc3339(),
        completed_at: row
            .get::<Option<chrono::DateTime<chrono::Utc>>, _>("completed_at")
            .map(|t| t.to_rfc3339()),
    }))
}
