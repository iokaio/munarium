// SPDX-License-Identifier: Apache-2.0
//! The management plane: capability-token issuance.
//!
//! POST /v1/access-tokens is callable only with a `mgmt` static token — the
//! API-management layer exchanges its long-lived credential for short-lived,
//! least-privilege JWTs it forwards to data-plane callers. Every issuance is
//! audited in access_tokens (jti + claims; never the token material).

use crate::error::ApiError;
use crate::state::AppState;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use munarium_api_types as dto;
use munarium_core::KernelError;
use std::sync::Arc;

type ApiResult<T> = std::result::Result<T, ApiError>;

pub async fn op_issue_token(
    state: &AppState,
    issued_by: &str,
    tenant_id: &str,
    req: dto::IssueTokenRequest,
) -> Result<dto::IssueTokenResponse, KernelError> {
    let secret = state.config.token_secret.as_ref().ok_or_else(|| {
        KernelError::InvalidInput(
            "token issuance requires MUNARIUM_TOKEN_SECRET (or _FILE) to be configured".into(),
        )
    })?;
    let jti = format!("tok-{}", uuid::Uuid::now_v7().simple());
    let ttl = req.ttl_secs.unwrap_or(state.config.token_ttl_secs);
    let (token, claims) = munarium_access::issue(
        secret,
        req.uid.trim(),
        tenant_id,
        req.access_level,
        req.compartments.clone(),
        req.scopes.clone(),
        req.runbook_refs.clone(),
        ttl,
        jti.clone(),
    )
    .map_err(|e| KernelError::InvalidInput(e.to_string()))?;

    let expires_at = chrono::DateTime::<chrono::Utc>::from_timestamp(claims.exp, 0)
        .ok_or_else(|| KernelError::InvalidInput("token expiry out of range".into()))?;

    // Issuance audit (pg mode). The memory store keeps dev friction-free.
    if let Some(pool) = state.pg_pool() {
        sqlx::query(
            "INSERT INTO access_tokens
               (tenant_id, jti, uid, access_level, compartments, scopes,
                runbook_refs, issued_by, expires_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
        )
        .bind(tenant_id)
        .bind(&jti)
        .bind(&claims.sub)
        .bind(claims.lvl)
        .bind(&claims.cmp)
        .bind(&claims.scopes)
        .bind(&claims.rb)
        .bind(issued_by)
        .bind(expires_at)
        .execute(pool)
        .await
        .map_err(|e| KernelError::Storage(e.to_string()))?;
    }

    Ok(dto::IssueTokenResponse {
        token,
        jti,
        expires_at: expires_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    })
}

/// POST /v1/access-tokens
#[utoipa::path(
    post,
    path = "/v1/access-tokens",
    tag = "access-tokens",
    request_body = dto::IssueTokenRequest,
    responses(
        (status = 200, description = "capability token minted", body = dto::IssueTokenResponse),
        (status = 400, description = "invalid request or token secret not configured"),
        (status = 403, description = "caller is not the management plane (mgmt role required)")
    )
)]
pub async fn issue_access_token(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uid: Option<axum::Extension<crate::middleware::Uid>>,
    crate::rest::ProblemJson(req): crate::rest::ProblemJson<dto::IssueTokenRequest>,
) -> ApiResult<axum::response::Response> {
    use axum::response::IntoResponse;
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    let issued_by = crate::middleware::uid_or_anonymous(uid.as_ref());
    let resp = op_issue_token(&state, &issued_by, &ctx.tenant_id, req).await?;
    // The response carries the signed JWT — flag it so the capture middleware
    // stores a redaction marker, never the token material (the issuance audit
    // in access_tokens keeps jti + claims only).
    let mut response = Json(resp).into_response();
    response
        .extensions_mut()
        .insert(crate::interactions::InteractionMeta {
            redact_response: true,
            ..Default::default()
        });
    Ok(response)
}
