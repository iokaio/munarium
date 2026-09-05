// SPDX-License-Identifier: Apache-2.0
//! The REST plane (:8080). Thin handlers: auth -> idempotency (commands) ->
//! shared service layer -> DTO. Errors leave as problem+json.

use crate::error::ApiError;
use crate::service;
use crate::state::{request_hash, AppState, TenantCtx};
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use munarium_api_conv::{convert, Convert};
use munarium_api_types as dto;
use munarium_core::storage::StorageBackend;
use munarium_core::types::ClaimStatus;
use munarium_core::KernelError;
use serde::Deserialize;
use std::sync::Arc;

type ApiResult<T> = Result<T, ApiError>;

/// Upload ceiling for `PUT /v1/sources` (the handler buffers the body).
pub const MAX_SOURCE_BYTES: usize = 256 * 1024 * 1024;

/// axum's own default body limit (2 MiB), restated so the capture
/// middleware's buffering ceiling and the extractors' agree by construction.
pub const DEFAULT_BODY_LIMIT: usize = 2 * 1024 * 1024;

/// The route TEMPLATES that carry whole documents and therefore declare the
/// 256 MiB `DefaultBodyLimit` below. Kept as one list so the middleware's
/// pre-authentication buffering ceiling (`body_limit_for_route`) cannot drift
/// from the routes' declared limits: a route added to one and not the other
/// is either 413'd at 2 MiB or buffered at 256 MiB before its handler has
/// looked at a credential.
pub const LARGE_BODY_ROUTES: &[&str] = &[
    "/v1/sources",
    "/v1/ingest",
    "/v1/ingest/batch",
    "/v1/ingest/bulk",
    "/v1/ingest/bulk/{bulk_id}/chunk",
    "/v1/evidence/{evidence_id}/bytes",
];

/// The body ceiling the capture middleware buffers a request under, by
/// matched route template. Unmatched paths get the small ceiling.
pub fn body_limit_for_route(route: &str) -> usize {
    if LARGE_BODY_ROUTES.contains(&route) {
        MAX_SOURCE_BYTES
    } else {
        DEFAULT_BODY_LIMIT
    }
}

/// Json extractor whose rejection is problem+json `invalid-input` (the
/// errors.md contract) instead of axum's plain-text default.
pub(crate) struct ProblemJson<T>(pub T);

impl<S, T> axum::extract::FromRequest<S> for ProblemJson<T>
where
    Json<T>: axum::extract::FromRequest<S, Rejection = axum::extract::rejection::JsonRejection>,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(v)) => Ok(Self(v)),
            Err(rej) => Err(ApiError::from(KernelError::InvalidInput(rej.body_text()))),
        }
    }
}

pub(crate) fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

/// Token-lifecycle failures get their own problem slugs (errors.md): the
/// auth layer speaks KernelError, so the sentinel details promote here.
pub(crate) fn promote_auth_error(e: KernelError) -> ApiError {
    if let KernelError::Unauthenticated(detail) = &e {
        if detail == crate::state::TOKEN_EXPIRED_DETAIL {
            return ApiError::Custom(crate::error::CustomError::token_expired());
        }
        if detail == crate::state::TOKEN_REVOKED_DETAIL {
            return ApiError::Custom(crate::error::CustomError::token_revoked());
        }
    }
    ApiError::Mesh(e)
}

pub(crate) async fn auth(
    state: &AppState,
    headers: &HeaderMap,
) -> ApiResult<(TenantCtx, Arc<dyn StorageBackend>)> {
    let ctx = state
        .authenticate(bearer(headers))
        .map_err(promote_auth_error)?;
    let store = state.store_for(&ctx.tenant_id).await?;
    Ok((ctx, store))
}

/// Auth without materializing a tenant store — for handlers that never touch
/// the ledger (or fetch a store themselves only on some paths).
pub(crate) fn auth_ctx(state: &AppState, headers: &HeaderMap) -> ApiResult<TenantCtx> {
    state
        .authenticate(bearer(headers))
        .map_err(promote_auth_error)
}

/// Resolve the caller to a data-plane AccessCtx carrying `uid` — the ONE
/// shared entry point for the query/ingest planes (sessions, turns, ingest)
/// and the collection-filtered `/v1/search`. Enforces the scope, runs the
/// optional revocation check, and promotes token-lifecycle errors. Static
/// rw/ro tokens map to unrestricted/query-only; mgmt is refused (management
/// stays off the data path). See docs/security-posture.md.
pub(crate) async fn data_plane_access(
    state: &AppState,
    headers: &HeaderMap,
    uid: &str,
    required_scope: &str,
) -> ApiResult<munarium_access::AccessCtx> {
    let principal = state
        .authenticate_principal(bearer(headers))
        .map_err(promote_auth_error)?;
    let access = principal.access_ctx(uid)?;
    if !access.has_scope(required_scope) {
        return Err(ApiError::Custom(crate::error::CustomError::scope_missing(
            required_scope,
        )));
    }
    if !access.jti.is_empty() {
        state
            .check_revocation(&access.tenant_id, &access.jti)
            .await
            .map_err(promote_auth_error)?;
    }
    Ok(access)
}

/// content_hash values are content addresses (hex sha-256). Rejecting
/// non-hex input keeps junk out of ledger subjects and makes byte-prefix
/// slicing safe (no multi-byte char boundaries).
pub(crate) fn validate_content_hash(hash: &str) -> Result<(), KernelError> {
    if hash.is_empty() {
        return Err(KernelError::InvalidInput("content_hash is required".into()));
    }
    if !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(KernelError::InvalidInput(
            "content_hash must be a hex digest".into(),
        ));
    }
    if hash.len() != 64 {
        return Err(KernelError::InvalidInput(
            "content_hash must be a 64-character sha-256 hex digest".into(),
        ));
    }
    Ok(())
}

fn idem_key(headers: &HeaderMap) -> ApiResult<String> {
    headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(String::from)
        .ok_or(ApiError::Mesh(KernelError::InvalidInput(
            "Idempotency-Key header is required on command requests".into(),
        )))
}

/// Command wrapper: replay a stored response or execute-and-record.
async fn with_idempotency<F, Fut>(
    state: &AppState,
    ctx: &TenantCtx,
    headers: &HeaderMap,
    body_hash: String,
    exec: F,
) -> ApiResult<axum::response::Response>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ApiResult<serde_json::Value>>,
{
    ctx.require_rw()?;
    let tenant = &ctx.tenant_id;
    let key = idem_key(headers)?;
    // Plane-namespaced hash: REST stores JSON, gRPC stores hex-encoded proto
    // in the SAME table. Reusing one Idempotency-Key across planes must
    // surface as idempotency-mismatch, never decode the other plane's bytes.
    let body_hash = format!("rest:{body_hash}");
    if let Some(stored) = state.idem_check(tenant, &key, &body_hash).await? {
        // Fail closed: a stored record that no longer parses is a server
        // fault, not a null replay.
        let v: serde_json::Value = serde_json::from_str(&stored).map_err(|_| {
            ApiError::Mesh(KernelError::Storage("corrupt idempotency record".into()))
        })?;
        return Ok(Json(v).into_response());
    }
    let value = exec().await?;
    state
        .idem_store(tenant, &key, &body_hash, &value.to_string())
        .await;
    Ok(Json(value).into_response())
}

fn json_value<T: serde::Serialize>(v: &T) -> serde_json::Value {
    serde_json::to_value(v).expect("DTO serialization is infallible")
}

/// Idempotency body hash for a command request. DTO serialization is
/// structurally infallible, but a request-path panic is never acceptable —
/// the impossible case maps to a 500 instead of aborting the worker.
fn idem_body_hash<T: serde::Serialize>(req: &T) -> ApiResult<String> {
    let bytes = serde_json::to_vec(req)
        .map_err(|e| ApiError::Mesh(KernelError::Storage(format!("serialize request: {e}"))))?;
    Ok(request_hash(&bytes))
}

// ---------------------------------------------------------------------------
// command handlers
// ---------------------------------------------------------------------------

#[utoipa::path(post, path = "/v1/versions", request_body = dto::CreateVersionRequest,
    responses((status = 200, body = dto::CreateVersionResponse)), tag = "command")]
async fn create_version(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ProblemJson(req): ProblemJson<dto::CreateVersionRequest>,
) -> ApiResult<axum::response::Response> {
    let (ctx, store) = auth(&state, &headers).await?;
    let hash = idem_body_hash(&req)?;
    with_idempotency(&state, &ctx, &headers, hash, || async move {
        let id = store
            .create_version(req.parent_version_id.as_deref(), req.metadata.clone())
            .await?;
        Ok(json_value(&dto::CreateVersionResponse { version_id: id }))
    })
    .await
}

#[utoipa::path(post, path = "/v1/versions/{version_id}/claims",
    request_body = dto::ProposeClaimRequest,
    responses((status = 200, body = dto::ProposeClaimResponse),
              (status = 409, body = dto::Problem)), tag = "command")]
async fn propose_claim(
    State(state): State<Arc<AppState>>,
    Path(version_id): Path<String>,
    headers: HeaderMap,
    ProblemJson(req): ProblemJson<dto::ProposeClaimRequest>,
) -> ApiResult<axum::response::Response> {
    let (ctx, store) = auth(&state, &headers).await?;
    let hash = idem_body_hash(&req)?;
    state.ensure_shapes_loaded(&ctx.tenant_id).await?;
    let chronology = state
        .armed_chronology(store.as_ref(), &ctx.tenant_id, &version_id)
        .await?;
    let state2 = state.clone();
    let tenant = ctx.tenant_id.clone();
    with_idempotency(&state, &ctx, &headers, hash, || async move {
        let expected = req.expected_head;
        let out = service::append_events(
            store.as_ref(),
            &state2.shapes,
            &tenant,
            &version_id,
            std::slice::from_ref(&req),
            None,
            expected,
            chronology.as_ref(),
        )
        .await?;
        let claim = out.claims.into_iter().next().ok_or_else(|| {
            ApiError::Mesh(KernelError::Storage(
                "append returned no claim for a one-claim propose".into(),
            ))
        })?;
        Ok(json_value(&dto::ProposeClaimResponse {
            claim: claim.convert(),
            findings: out.findings.into_iter().map(convert).collect(),
            head_seq: out.head_seq,
        }))
    })
    .await
}

#[utoipa::path(post, path = "/v1/versions/{version_id}/events",
    request_body = dto::AppendEventsRequest,
    responses((status = 200, body = dto::AppendEventsResponse)), tag = "command")]
async fn append_events(
    State(state): State<Arc<AppState>>,
    Path(version_id): Path<String>,
    headers: HeaderMap,
    ProblemJson(req): ProblemJson<dto::AppendEventsRequest>,
) -> ApiResult<axum::response::Response> {
    let (ctx, store) = auth(&state, &headers).await?;
    let hash = idem_body_hash(&req)?;
    state.ensure_shapes_loaded(&ctx.tenant_id).await?;
    let chronology = state
        .armed_chronology(store.as_ref(), &ctx.tenant_id, &version_id)
        .await?;
    let state2 = state.clone();
    let tenant = ctx.tenant_id.clone();
    with_idempotency(&state, &ctx, &headers, hash, || async move {
        let out = service::append_events(
            store.as_ref(),
            &state2.shapes,
            &tenant,
            &version_id,
            &req.claims,
            req.candidate_text.as_deref(),
            req.expected_head,
            chronology.as_ref(),
        )
        .await?;
        Ok(json_value(&dto::AppendEventsResponse {
            claims: out.claims.into_iter().map(convert).collect(),
            findings: out.findings.into_iter().map(convert).collect(),
            head_seq: out.head_seq,
        }))
    })
    .await
}

#[utoipa::path(post, path = "/v1/versions/{version_id}/promises",
    request_body = dto::OpenPromiseRequest,
    responses((status = 200, body = dto::PromiseDto)), tag = "command")]
async fn open_promise(
    State(state): State<Arc<AppState>>,
    Path(version_id): Path<String>,
    headers: HeaderMap,
    ProblemJson(req): ProblemJson<dto::OpenPromiseRequest>,
) -> ApiResult<axum::response::Response> {
    let (ctx, store) = auth(&state, &headers).await?;
    let hash = idem_body_hash(&req)?;
    with_idempotency(&state, &ctx, &headers, hash, || async move {
        let p = store
            .register_promise(
                &version_id,
                &req.key,
                &req.kind,
                &req.description,
                req.origin_scope.as_deref(),
                req.due_scope.as_deref(),
            )
            .await?;
        Ok(json_value(&convert(p)))
    })
    .await
}

#[utoipa::path(post, path = "/v1/versions/{version_id}/promises/{key}/fulfill",
    responses((status = 200, body = dto::FulfillPromiseResponse)), tag = "command")]
async fn fulfill_promise(
    State(state): State<Arc<AppState>>,
    Path((version_id, key)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<axum::response::Response> {
    let (ctx, store) = auth(&state, &headers).await?;
    let hash = request_hash(format!("fulfill:{version_id}:{key}").as_bytes());
    with_idempotency(&state, &ctx, &headers, hash, || async move {
        let fulfilled = store.fulfill_promise(&version_id, &key).await?;
        Ok(json_value(&dto::FulfillPromiseResponse { fulfilled }))
    })
    .await
}

#[utoipa::path(post, path = "/v1/versions/{version_id}/anchors",
    request_body = dto::LockAnchorRequest,
    responses((status = 200, body = dto::AnchorDto)), tag = "command")]
async fn lock_anchor(
    State(state): State<Arc<AppState>>,
    Path(version_id): Path<String>,
    headers: HeaderMap,
    ProblemJson(req): ProblemJson<dto::LockAnchorRequest>,
) -> ApiResult<axum::response::Response> {
    let (ctx, store) = auth(&state, &headers).await?;
    let hash = idem_body_hash(&req)?;
    with_idempotency(&state, &ctx, &headers, hash, || async move {
        let a = store
            .lock_anchor(
                &version_id,
                &req.subject,
                &req.key,
                &req.value,
                req.scope_path.as_deref(),
                req.evidence.clone(),
            )
            .await?;
        Ok(json_value(&convert(a)))
    })
    .await
}

// ---------------------------------------------------------------------------
// query handlers
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct FactsQuery {
    scope_prefix: Option<String>,
    as_of_seq: Option<u64>,
    limit: Option<usize>,
    /// comma list: accepted,disputed
    statuses: Option<String>,
}

#[utoipa::path(get, path = "/v1/versions/{version_id}/facts",
    params(("version_id" = String, Path, description = "memory version id"),
           ("scope_prefix" = Option<String>, Query, description = "exact or prefix.% scope match"),
           ("as_of_seq" = Option<u64>, Query, description = "point-in-time pin"),
           ("limit" = Option<usize>, Query, description = "keep the NEWEST n"),
           ("statuses" = Option<String>, Query, description = "comma list: accepted,disputed")),
    responses((status = 200, body = dto::FactsResponse)), tag = "query")]
async fn get_facts(
    State(state): State<Arc<AppState>>,
    Path(version_id): Path<String>,
    Query(q): Query<FactsQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::FactsResponse>> {
    let (_ctx, store) = auth(&state, &headers).await?;
    let statuses = q
        .statuses
        .map(|s| {
            s.split(',')
                .filter_map(|p| match p.trim() {
                    "accepted" => Some(ClaimStatus::Accepted),
                    "disputed" => Some(ClaimStatus::Disputed),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Json(
        service::slice_facts(
            store.as_ref(),
            &version_id,
            q.scope_prefix,
            q.as_of_seq,
            statuses,
            q.limit,
        )
        .await?,
    ))
}

#[utoipa::path(get, path = "/v1/versions/{version_id}/head",
    responses((status = 200, body = dto::HeadResponse)), tag = "query")]
async fn get_head(
    State(state): State<Arc<AppState>>,
    Path(version_id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::HeadResponse>> {
    let (_ctx, store) = auth(&state, &headers).await?;
    Ok(Json(dto::HeadResponse {
        head_seq: store.head(&version_id).await?,
    }))
}

#[utoipa::path(get, path = "/v1/claims/{claim_id}",
    responses((status = 200, body = dto::GetClaimResponse)), tag = "query")]
async fn get_claim(
    State(state): State<Arc<AppState>>,
    Path(claim_id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::GetClaimResponse>> {
    let (_ctx, store) = auth(&state, &headers).await?;
    Ok(Json(service::get_claim(store.as_ref(), &claim_id).await?))
}

#[utoipa::path(get, path = "/v1/versions/{version_id}/lineage",
    responses((status = 200, body = dto::LineageResponse)), tag = "query")]
async fn get_lineage(
    State(state): State<Arc<AppState>>,
    Path(version_id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::LineageResponse>> {
    let (_ctx, store) = auth(&state, &headers).await?;
    Ok(Json(dto::LineageResponse {
        version_ids: store.lineage(&version_id).await?,
    }))
}

#[derive(Debug, Default, Deserialize)]
struct PinQuery {
    as_of_seq: Option<u64>,
    status: Option<String>,
    /// Promises route only (2026-08-17, §13 entry 14): compute
    /// `gate.promise-unfulfilled` warn findings for open promises due at
    /// this scope.
    overdue_scope: Option<String>,
    /// Promises route only: treat this read as the final unit — EVERY open
    /// promise is overdue.
    #[serde(rename = "final")]
    final_unit: Option<bool>,
}

#[utoipa::path(get, path = "/v1/versions/{version_id}/anchors",
    responses((status = 200, body = dto::AnchorsResponse)), tag = "query")]
async fn get_anchors(
    State(state): State<Arc<AppState>>,
    Path(version_id): Path<String>,
    Query(q): Query<PinQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::AnchorsResponse>> {
    let (_ctx, store) = auth(&state, &headers).await?;
    let anchors = store.anchors(&version_id, q.as_of_seq).await?;
    Ok(Json(dto::AnchorsResponse {
        anchors: anchors.into_values().map(convert).collect(),
    }))
}

#[utoipa::path(get, path = "/v1/versions/{version_id}/promises",
    params(("as_of_seq" = Option<u64>, Query), ("status" = Option<String>, Query, description = "open | fulfilled | expired | violated"),
           ("overdue_scope" = Option<String>, Query, description = "compute gate.promise-unfulfilled warn findings for open promises due at this scope (2026-08-17)"),
           ("final" = Option<bool>, Query, description = "treat this read as the final unit: every open promise is overdue")),
    responses((status = 200, body = dto::PromisesResponse)), tag = "query")]
async fn get_promises(
    State(state): State<Arc<AppState>>,
    Path(version_id): Path<String>,
    Query(q): Query<PinQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::PromisesResponse>> {
    let (_ctx, store) = auth(&state, &headers).await?;
    let mut promises = store.promises(&version_id, q.as_of_seq).await?;
    // The overdue view (2026-08-17, §13 entry 14): the kernel's ported
    // `find_overdue` finally has a wire surface. Computed over the FULL
    // pinned promise slice, before any status filter narrows it.
    let overdue_findings = if q.overdue_scope.is_some() || q.final_unit == Some(true) {
        let findings = munarium_core::promises::find_overdue(
            &promises,
            q.overdue_scope.as_deref(),
            q.final_unit == Some(true),
        );
        Some(findings.iter().map(convert).collect())
    } else {
        None
    };
    if let Some(status) = &q.status {
        promises.retain(|p| {
            matches!(
                (status.as_str(), p.status),
                ("open", munarium_core::types::PromiseStatus::Open)
                    | ("fulfilled", munarium_core::types::PromiseStatus::Fulfilled)
                    | ("expired", munarium_core::types::PromiseStatus::Expired)
                    | ("violated", munarium_core::types::PromiseStatus::Violated)
            )
        });
    }
    Ok(Json(dto::PromisesResponse {
        promises: promises.into_iter().map(convert).collect(),
        overdue_findings,
    }))
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct FindingsParams {
    as_of_seq: Option<u64>,
    severity: Option<String>,
    rule_id: Option<String>,
    rule_prefix: Option<String>,
    limit: Option<usize>,
}

/// GET /v1/versions/{version_id}/findings — the persisted gate findings
/// (2026-08-17; until then findings rode only the write response — §13
/// entry 12). REST-first: the grpc.md parity ledger records the gap.
#[utoipa::path(get, path = "/v1/versions/{version_id}/findings",
    params(("as_of_seq" = Option<u64>, Query, description = "bound by the seq findings were recorded at"),
           ("severity" = Option<String>, Query, description = "info | warn | block"),
           ("rule_id" = Option<String>, Query, description = "exact rule id, e.g. gate.ledger-conflict"),
           ("rule_prefix" = Option<String>, Query, description = "rule id prefix: `gate.` for the kernel gates, `matrix.` for connector findings"),
           ("limit" = Option<usize>, Query, description = "default 1000")),
    responses((status = 200, body = dto::FindingsResponse)), tag = "query")]
pub(crate) async fn get_findings(
    State(state): State<Arc<AppState>>,
    Path(version_id): Path<String>,
    Query(q): Query<FindingsParams>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::FindingsResponse>> {
    let (_ctx, store) = auth(&state, &headers).await?;
    let severity = match q.severity.as_deref() {
        None => None,
        Some("info") => Some(munarium_core::types::Severity::Info),
        Some("warn") => Some(munarium_core::types::Severity::Warn),
        Some("block") => Some(munarium_core::types::Severity::Block),
        Some(other) => {
            return Err(ApiError::Mesh(KernelError::InvalidInput(format!(
                "severity must be info|warn|block, got '{other}'"
            ))))
        }
    };
    Ok(Json(
        service::get_findings(
            store.as_ref(),
            &version_id,
            &munarium_core::storage::FindingsQuery {
                as_of_seq: q.as_of_seq,
                severity,
                rule_id: q.rule_id,
                rule_prefix: q.rule_prefix,
                limit: q.limit,
            },
        )
        .await?,
    ))
}

/// POST /v1/versions/{version_id}/findings — file findings computed OUTSIDE
/// the gates. The first caller is Munarium Matrix's
/// reconciliation pipeline, which compares a system of record against the
/// ledger and files `matrix.discrepancy-candidate` with both evidence sides.
///
/// Three rules, each load-bearing:
///
/// - **Warn/info only.** A `block` is refused with 400. Blocking is a gate
///   decision made against a candidate at write time; this route is not a
///   gate and must not be able to impersonate one.
/// - **Stamped at the current head.** The finding reads as filed at the seq
///   the ledger had when it arrived, so `as_of_seq` bounds it like every
///   other store: a pin before that seq does not see it.
/// - **Idempotent by content, not by header.** Identity is
///   `(rule_id, detail.evidence_ref, detail.claim_id)`; a finding whose
///   detail carries neither ref falls back to `(rule_id, scope_path,
///   message)`. A re-run of the same reconciliation files nothing twice.
///   No `Idempotency-Key` is required — the content IS the key.
///
/// Authentication: a static **rw** token, or a capability token carrying the
/// `findings` scope (the scope Matrix's service token holds). `ro` and `mgmt`
/// are refused — governance findings are a data-plane write.
#[utoipa::path(post, path = "/v1/versions/{version_id}/findings",
    request_body = dto::RecordFindingsRequest,
    responses((status = 200, body = dto::RecordFindingsResponse),
              (status = 400, body = dto::Problem, description = "a block severity, or an empty rule_id"),
              (status = 403, body = dto::Problem, description = "no findings scope / not rw")),
    tag = "command")]
pub(crate) async fn record_findings(
    State(state): State<Arc<AppState>>,
    Path(version_id): Path<String>,
    headers: HeaderMap,
    ProblemJson(req): ProblemJson<dto::RecordFindingsRequest>,
) -> ApiResult<Json<dto::RecordFindingsResponse>> {
    use munarium_core::storage::FindingsQuery;
    use munarium_core::types::{GateFinding, Severity};

    let principal = state
        .authenticate_principal(bearer(&headers))
        .map_err(promote_auth_error)?;
    let tenant = match &principal {
        crate::state::Principal::Static(c) => {
            c.require_rw()?;
            c.tenant_id.clone()
        }
        crate::state::Principal::Access(a) => {
            if !a.has_scope(munarium_access::SCOPE_FINDINGS) {
                return Err(ApiError::Custom(crate::error::CustomError::scope_missing(
                    munarium_access::SCOPE_FINDINGS,
                )));
            }
            if !a.jti.is_empty() {
                state
                    .check_revocation(&a.tenant_id, &a.jti)
                    .await
                    .map_err(promote_auth_error)?;
            }
            a.tenant_id.clone()
        }
    };
    let store = state.store_for(&tenant).await?;

    if req.findings.is_empty() {
        return Err(ApiError::Mesh(KernelError::InvalidInput(
            "findings must not be empty".into(),
        )));
    }
    let mut incoming: Vec<GateFinding> = Vec::with_capacity(req.findings.len());
    for f in req.findings {
        if f.rule_id.trim().is_empty() {
            return Err(ApiError::Mesh(KernelError::InvalidInput(
                "rule_id is required on every finding".into(),
            )));
        }
        let severity: Severity = f.severity.convert();
        if severity == Severity::Block {
            return Err(ApiError::Mesh(KernelError::InvalidInput(format!(
                "finding '{}' has severity block; this route files warn/info only — \
                 blocking is a gate decision, and this route is not a gate",
                f.rule_id
            ))));
        }
        incoming.push(GateFinding {
            rule_id: f.rule_id,
            severity,
            message: f.message,
            scope_path: f.scope_path,
            detail: f.detail,
        });
    }

    // Content identity, computed once per finding and once per existing row.
    fn identity(f: &GateFinding) -> (String, String, String) {
        let pick = |k: &str| {
            f.detail
                .as_ref()
                .and_then(|d| d.get(k))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        match (pick("evidence_ref"), pick("claim_id")) {
            (None, None) => (
                f.rule_id.clone(),
                f.scope_path.clone().unwrap_or_default(),
                f.message.clone(),
            ),
            (e, c) => (
                f.rule_id.clone(),
                e.unwrap_or_default(),
                c.unwrap_or_default(),
            ),
        }
    }

    // Existing rows for the rule ids in play. The read is capped, and the cap
    // is stated: a lineage carrying more than this many findings under one
    // rule id is beyond what dedup-by-read can promise, and a duplicate would
    // then slip through rather than a finding being lost. Loud in the log.
    const DEDUP_READ_CAP: usize = 50_000;
    let mut seen = std::collections::HashSet::new();
    let mut rule_ids: Vec<&str> = incoming.iter().map(|f| f.rule_id.as_str()).collect();
    rule_ids.sort_unstable();
    rule_ids.dedup();
    for rule in rule_ids {
        let existing = store
            .findings(
                &version_id,
                &FindingsQuery {
                    rule_id: Some(rule.to_string()),
                    limit: Some(DEDUP_READ_CAP),
                    ..Default::default()
                },
            )
            .await?;
        if existing.len() >= DEDUP_READ_CAP {
            tracing::warn!(
                version_id,
                rule,
                "findings dedup read hit its cap; duplicates under this rule may not be detected"
            );
        }
        seen.extend(existing.iter().map(|r| identity(&r.finding)));
    }

    let mut to_write = Vec::new();
    let mut skipped = 0usize;
    for f in incoming {
        if seen.insert(identity(&f)) {
            to_write.push(f);
        } else {
            skipped += 1;
        }
    }

    let seq = store.head(&version_id).await?;
    if !to_write.is_empty() {
        store.record_findings(&version_id, seq, &to_write).await?;
    }
    Ok(Json(dto::RecordFindingsResponse {
        recorded: to_write.len(),
        skipped_duplicates: skipped,
        seq,
    }))
}

#[derive(Debug, Default, Deserialize)]
struct ContextQuery {
    scope: Option<String>,
    budget_tokens: Option<u64>,
    fact_limit: Option<usize>,
    as_of_seq: Option<u64>,
    /// Declared in the proto but not yet implemented — rejected explicitly.
    as_of_date: Option<String>,
}

#[utoipa::path(get, path = "/v1/versions/{version_id}/context",
    params(("version_id" = String, Path, description = "memory version id"),
           ("scope" = Option<String>, Query, description = "active scope path"),
           ("budget_tokens" = Option<u64>, Query, description = "token budget; digest tiers degrade before fact trimming"),
           ("fact_limit" = Option<usize>, Query, description = "max facts composed (default 60)"),
           ("as_of_seq" = Option<u64>, Query, description = "point-in-time pin")),
    responses((status = 200, body = dto::ComposedContextDto)), tag = "query")]
async fn get_context(
    State(state): State<Arc<AppState>>,
    Path(version_id): Path<String>,
    Query(q): Query<ContextQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::ComposedContextDto>> {
    let (_ctx, store) = auth(&state, &headers).await?;
    Ok(Json(
        service::compose_context(
            store.as_ref(),
            &version_id,
            q.scope.as_deref(),
            q.budget_tokens,
            q.fact_limit,
            q.as_of_seq,
            q.as_of_date.as_deref().filter(|s| !s.is_empty()),
        )
        .await?,
    ))
}

#[utoipa::path(post, path = "/v1/versions/{version_id}/counters",
    request_body = dto::RecordCountsRequest,
    responses((status = 200)), tag = "command")]
async fn record_counts(
    State(state): State<Arc<AppState>>,
    Path(version_id): Path<String>,
    headers: HeaderMap,
    ProblemJson(req): ProblemJson<dto::RecordCountsRequest>,
) -> ApiResult<axum::response::Response> {
    let (ctx, store) = auth(&state, &headers).await?;
    let hash = idem_body_hash(&req)?;
    with_idempotency(&state, &ctx, &headers, hash, || async move {
        store
            .record_counts(
                &version_id,
                &req.key,
                &req.scope_path,
                req.count,
                req.budget,
            )
            .await?;
        Ok(serde_json::json!({ "ok": true }))
    })
    .await
}

#[utoipa::path(get, path = "/v1/versions/{version_id}/counters",
    responses((status = 200, body = dto::CountersResponse)), tag = "query")]
async fn get_counters(
    State(state): State<Arc<AppState>>,
    Path(version_id): Path<String>,
    Query(q): Query<PinQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::CountersResponse>> {
    let (_ctx, store) = auth(&state, &headers).await?;
    let counters = store.counter_totals(&version_id, q.as_of_seq).await?;
    Ok(Json(dto::CountersResponse {
        counters: counters.into_iter().map(convert).collect(),
    }))
}

#[utoipa::path(put, path = "/v1/versions/{version_id}/digests",
    request_body = dto::DigestDto,
    responses((status = 200)), tag = "command")]
async fn upsert_digest(
    State(state): State<Arc<AppState>>,
    Path(version_id): Path<String>,
    headers: HeaderMap,
    ProblemJson(req): ProblemJson<dto::DigestDto>,
) -> ApiResult<Json<serde_json::Value>> {
    let (ctx, store) = auth(&state, &headers).await?;
    ctx.require_rw()?;
    // The path names the version; the body must agree. The path id used to
    // be ignored, so `PUT /v1/versions/A/digests` with `version_id: B` in the
    // body wrote B's digest — same tenant, so no isolation break, but a
    // contract that says one thing and does another.
    if req.version_id != version_id {
        return Err(ApiError::Mesh(KernelError::InvalidInput(format!(
            "body version_id {:?} does not match the path's {version_id:?}",
            req.version_id
        ))));
    }
    store.upsert_digest(&req.convert()).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[utoipa::path(get, path = "/v1/versions/{version_id}/digests",
    responses((status = 200, body = dto::DigestsResponse)), tag = "query")]
async fn get_digests(
    State(state): State<Arc<AppState>>,
    Path(version_id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::DigestsResponse>> {
    let (_ctx, store) = auth(&state, &headers).await?;
    let digests = store.digests(&version_id).await?;
    Ok(Json(dto::DigestsResponse {
        digests: digests.into_iter().map(convert).collect(),
    }))
}

// ---------------------------------------------------------------------------
// shapes + ingest + retrieval
// ---------------------------------------------------------------------------

/// Apply a Shape (YAML body). Optional ?version_id= records the publication
/// as a ledger claim in that lineage.
#[utoipa::path(post, path = "/v1/shapes",
    params(("version_id" = Option<String>, Query, description = "record the publication as a ledger claim in this lineage")),
    request_body(content = String, content_type = "text/yaml",
        description = "apiVersion: munarium.ioka.io/v1, kind: Shape"),
    responses((status = 200, body = dto::ApplyShapeResponse)), tag = "shapes")]
async fn apply_shape(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ShapeApplyQuery>,
    headers: HeaderMap,
    yaml: String,
) -> ApiResult<Json<dto::ApplyShapeResponse>> {
    let ctx = auth_ctx(&state, &headers)?;
    ctx.require_rw()?;
    let resp = crate::runbooks_api::op_apply_shape(
        &state,
        &ctx.tenant_id,
        &yaml,
        q.version_id.as_deref(),
        None,
    )
    .await?;
    Ok(Json(resp))
}

#[derive(Debug, Default, Deserialize)]
struct ShapeApplyQuery {
    version_id: Option<String>,
}

/// Content-addressed source upload: raw bytes body; the server verifies
/// X-Content-Sha256 (when present) before commit.
#[utoipa::path(put, path = "/v1/sources",
    params(("X-Content-Sha256" = Option<String>, Header, description = "declared hex sha-256, verified before commit"),
           ("X-Filename" = Option<String>, Header, description = "original filename"),
           ("X-Shape-Ref" = Option<String>, Header, description = "shape binding for later chunking")),
    request_body(content = Vec<u8>, content_type = "application/octet-stream",
        description = "raw source bytes"),
    responses((status = 200, body = dto::PutSourceResponse)), tag = "ingest")]
async fn put_source(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> ApiResult<Json<dto::PutSourceResponse>> {
    let ctx = auth_ctx(&state, &headers)?;
    ctx.require_rw()?;
    let retrieval = state.retrieval_for(&ctx.tenant_id)?;
    let declared = headers
        .get("x-content-sha256")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let media_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream");
    // X-Filename is the source's IDENTITY and its object-store path, so it is
    // required. A source with no path could never match a runbook's
    // filenamePrefix binding — it would be unreachable by construction.
    let filename = headers
        .get("x-filename")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .ok_or_else(|| {
            KernelError::InvalidInput(
                "X-Filename header is required: it is the source's identity and storage path"
                    .into(),
            )
        })?;
    let shape_ref = headers.get("x-shape-ref").and_then(|v| v.to_str().ok());
    let (source_id, content_hash, already_existed) = retrieval
        .put_source(declared, media_type, filename, shape_ref, &body)
        .await?;
    Ok(Json(dto::PutSourceResponse {
        source_id,
        content_hash,
        bytes_len: body.len() as u64,
        already_existed,
    }))
}

/// Source metadata — where a document actually went. Never the bytes.
///
/// `/v1/sources` was PUT-only, so there was no way to ask "did this land, and
/// where?". That question is the whole observability story for object
/// storage, and the smoke test needs it to prove the blob path.
#[utoipa::path(get, path = "/v1/sources/{source_id}",
    params(("source_id" = String, Path, description = "source id ('src-…')")),
    responses((status = 200, body = dto::SourceInfoDto)), tag = "ingest")]
async fn get_source(
    State(state): State<Arc<AppState>>,
    Path(source_id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::SourceInfoDto>> {
    let ctx = auth_ctx(&state, &headers)?;
    let retrieval = state.retrieval_for(&ctx.tenant_id)?;
    let i = retrieval.source_info(&source_id).await?;
    Ok(Json(dto::SourceInfoDto {
        source_id: i.source_id,
        filename: i.filename,
        media_type: i.media_type,
        content_hash: i.content_hash,
        bytes_len: i.bytes_len,
        storage_backend: i.storage_backend,
        blob_uri: i.blob_uri,
        extraction_status: i.extraction_status,
        extraction_method: i.extraction_method,
        created_at: i.created_at,
    }))
}

/// Records the ingest.recorded event for a stored source.
#[utoipa::path(post, path = "/v1/versions/{version_id}/ingests",
    params(("version_id" = String, Path, description = "memory version id")),
    request_body = dto::RecordIngestRequest,
    responses((status = 200, body = dto::RecordIngestResponse)), tag = "ingest")]
async fn record_ingest(
    State(state): State<Arc<AppState>>,
    Path(version_id): Path<String>,
    headers: HeaderMap,
    ProblemJson(req): ProblemJson<dto::RecordIngestRequest>,
) -> ApiResult<Json<dto::RecordIngestResponse>> {
    let (ctx, store) = auth(&state, &headers).await?;
    ctx.require_rw()?;
    let content_hash = req.content_hash;
    validate_content_hash(&content_hash)?;
    let shape_ref = req.shape_ref.unwrap_or_default();
    let mut claim = munarium_core::storage::NewClaim::fact(
        &format!("source-{}", &content_hash[..12.min(content_hash.len())]),
        "ingested",
        if shape_ref.is_empty() {
            "unbound"
        } else {
            &shape_ref
        },
    );
    claim.evidence = Some(serde_json::json!({ "content_hash": content_hash }));
    let stored = store.append_claim(&version_id, claim, None).await?;
    Ok(Json(dto::RecordIngestResponse {
        event_id: stored.id,
        seq: stored.seq,
    }))
}

/// Build (or re-activate) the index for a shape — side-by-side + atomic flip.
#[utoipa::path(post, path = "/v1/indexes/{shape_ref}/build",
    params(("shape_ref" = String, Path, description = "name@version"),
           ("version_id" = Option<String>, Query, description = "lineage whose head becomes the envelope watermark")),
    responses((status = 200, body = dto::IndexStatusResponse)), tag = "retrieval")]
async fn build_index(
    State(state): State<Arc<AppState>>,
    Path(shape_ref): Path<String>,
    Query(q): Query<BuildIndexQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::IndexStatusResponse>> {
    let (ctx, store) = auth(&state, &headers).await?;
    ctx.require_rw()?;
    state.ensure_shapes_loaded(&ctx.tenant_id).await?;
    let retrieval = state.retrieval_for(&ctx.tenant_id)?;
    let max_chars = state
        .shapes
        .get(&ctx.tenant_id, &shape_ref)
        .and_then(|s| s.doc.spec.chunking.as_ref().map(|c| c.max_chars))
        .unwrap_or(2000);
    // the envelope's event watermark: the named lineage's head at build time
    let watermark = match &q.version_id {
        Some(v) => store.head(v).await?,
        None => 0,
    };
    let iv = retrieval
        .build_index(&shape_ref, max_chars, watermark, true)
        .await?;
    Ok(Json(iv.convert()))
}

#[derive(Debug, Default, Deserialize)]
struct BuildIndexQuery {
    version_id: Option<String>,
}

#[utoipa::path(get, path = "/v1/indexes/{shape_ref}",
    params(("shape_ref" = String, Path, description = "name@version")),
    responses((status = 200, body = dto::IndexStatusResponse)), tag = "retrieval")]
async fn get_index(
    State(state): State<Arc<AppState>>,
    Path(shape_ref): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::IndexStatusResponse>> {
    let ctx = auth_ctx(&state, &headers)?;
    let retrieval = state.retrieval_for(&ctx.tenant_id)?;
    let iv = retrieval.index_version(&shape_ref).await?;
    Ok(Json(iv.convert()))
}

#[utoipa::path(post, path = "/v1/search",
    request_body = dto::SearchRequest,
    responses((status = 200, body = dto::SearchResponse)), tag = "retrieval")]
async fn hybrid_search(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uid: Option<axum::Extension<crate::middleware::Uid>>,
    ProblemJson(req): ProblemJson<dto::SearchRequest>,
) -> ApiResult<Json<dto::SearchResponse>> {
    // the one implemented filter is {"collections": ["<name-or-id>"]} —
    // route the search to that collection's partitioned index. One collection
    // per search so the single ProvenanceEnvelope stays truthful; merged
    // multi-collection retrieval rides the session plane. The filtered
    // path is a DATA-plane read: it enforces the collection's access level and
    // compartments (query scope), so a collection filter cannot read past a
    // caller's clearance. The unfiltered legacy shape-scoped path below stays
    // a control-plane read (static rw/ro).
    if let Some(filter) = &req.filter {
        let names = filter
            .as_object()
            .filter(|o| o.len() == 1)
            .and_then(|o| o.get("collections"))
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect::<Vec<_>>()
            })
            .ok_or_else(|| {
                KernelError::InvalidInput(
                    "the only supported filter is {\"collections\": [\"<name-or-id>\"]}".into(),
                )
            })?;
        let [name] = names.as_slice() else {
            return Err(ApiError::Mesh(KernelError::InvalidInput(
                "exactly one collection per search (multi-collection retrieval is the session plane)"
                    .into(),
            )));
        };
        let uid = uid
            .map(|axum::Extension(u)| u.0)
            .unwrap_or_else(|| "anonymous".to_string());
        let access =
            data_plane_access(&state, &headers, &uid, munarium_access::SCOPE_QUERY).await?;
        let retrieval = state.retrieval_for(&access.tenant_id)?;
        let info = match retrieval.collection_by_id(name).await {
            Ok(info) => info,
            Err(KernelError::NotFound { .. }) => retrieval.collection_by_name(name).await?,
            Err(e) => return Err(e.into()),
        };
        if info.status != "active" || !access.permits(info.access_level, &info.compartments) {
            // Same shape as a real miss: never reveal that a collection the
            // caller cannot clear exists.
            return Err(ApiError::Mesh(KernelError::NotFound {
                kind: "collection",
                id: name.clone(),
            }));
        }
        let params = munarium_core::retrieval::SearchParams {
            top_k: req.top_k.map(|k| k as usize).unwrap_or(10),
            ..Default::default()
        };
        let result = retrieval
            .search_collection(
                &info.id,
                req.query.as_deref().unwrap_or_default(),
                params,
                req.index_version.as_deref(),
            )
            .await?;
        return Ok(Json(result.convert()));
    }
    let ctx = auth_ctx(&state, &headers)?;
    let retrieval = state.retrieval_for(&ctx.tenant_id)?;
    let result = retrieval
        .hybrid_search(munarium_core::retrieval::HybridQuery {
            query: req.query.unwrap_or_default(),
            shape_ref: req.shape_ref.unwrap_or_default(),
            top_k: req.top_k.map(|k| k as usize).unwrap_or(10),
            filter: None,
            index_version: req.index_version,
        })
        .await?;
    Ok(Json(result.convert()))
}

// ---------------------------------------------------------------------------
// ops + docs
// ---------------------------------------------------------------------------

#[utoipa::path(get, path = "/healthz", security(()),
    responses((status = 200, body = dto::OkResponse)), tag = "meta")]
async fn healthz() -> Json<dto::OkResponse> {
    Json(dto::OkResponse { ok: true })
}

#[utoipa::path(get, path = "/readyz", security(()),
    responses((status = 200, body = dto::OkResponse),
              (status = 503, body = dto::OkResponse, description = "backing store unreachable")),
    tag = "meta")]
async fn readyz(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // One readiness probe for both planes: AppState::store_ready (the ops
    // plane's /readyz calls the same fn, so they cannot disagree). A
    // draining instance reports not-ready immediately.
    // Datastore admission (§9.2) joins the probe in datastore mode: a
    // replica with selected scopes whose serving-required set is not yet open
    // must not receive traffic. The check reads maintained state — the warmer
    // does the I/O, never this handler.
    let ready = !state.draining.load(std::sync::atomic::Ordering::Relaxed)
        && state.store_ready().await
        && state.datastore_readiness().admits();
    let status = if ready {
        axum::http::StatusCode::OK
    } else {
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(dto::OkResponse { ok: ready }))
}

#[utoipa::path(get, path = "/version", security(()),
    responses((status = 200, description = "server name, version, and plane map",
               body = dto::VersionInfo)), tag = "meta")]
async fn version_info() -> Json<dto::VersionInfo> {
    Json(dto::VersionInfo {
        name: "munarium-server".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        planes: serde_json::json!(
            { "rest": "8080", "grpc_direct": "50051", "grpc_gateway": "443 via gateway" }),
    })
}

async fn openapi_json() -> Json<serde_json::Value> {
    Json(serde_json::to_value(crate::openapi::doc()).expect("openapi serializes"))
}

/// A self-contained API landing page (upgraded 2026-08-17 from a Redoc
/// stub). No bundled renderer BY DESIGN — the strict-CSP/offline posture
/// bans CDN scripts and a vendored Redoc bundle is megabytes of JS this
/// binary does not want; the spec itself plus the committed guides are the
/// documentation surface.
async fn docs_page() -> Html<String> {
    Html(format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>munarium-server API</title>
<style>body{{font:15px/1.5 system-ui,sans-serif;max-width:640px;margin:40px auto;padding:0 16px;color:#111}}
h1{{font-size:20px}} li{{margin:4px 0}} code{{background:#f4f4f0;padding:1px 5px;border-radius:4px}}</style>
</head><body>
<h1>munarium-server {version}</h1>
<p>The machine-readable contract is <a href="/openapi.json">/openapi.json</a>
(generated from the handlers, CI drift-checked). The surface, by tag:</p>
<ul>
<li><strong>command / query</strong> — versions, claims, events, promises, anchors, counters, digests, facts, lineage, context (every read takes <code>as_of_seq</code>)</li>
<li><strong>shapes / ingest / retrieval</strong> — shape publication, content-addressed sources, index build/cutover, hybrid search</li>
<li><strong>providers</strong> — BYOK completion/embedding with invocation provenance; <a href="/healthai">/healthai</a> live-probes the default models (paid)</li>
<li><strong>runbooks / sessions</strong> — checkpointed step machines with approval gates; multiturn access-filtered sessions</li>
<li><strong>access-tokens / reports / collections</strong> — the management plane (mgmt role), including the dashboard report views</li>
<li><strong>/admin</strong> — the built-in operator console: monitoring dashboards plus the control-plane inventory (runbooks, shapes, collections, sessions, tokens, audit, findings) and its viewers (mgmt login)</li>
</ul>
<p>Human guides live in the repo: <code>server/docs/api/rest.md</code> (route map, auth, idempotency),
<code>server/docs/api/errors.md</code> (the problem-slug registry), <code>server/docs/api/grpc.md</code> (the gRPC twin planes).</p>
<p style="color:#666">No bundled Redoc by design: this page must render over a bare
port-forward with a strict CSP and zero external assets.</p>
</body></html>"#,
        version = env!("CARGO_PKG_VERSION"),
    ))
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/healthai", get(crate::providers_api::healthai))
        .route("/version", get(version_info))
        .route("/openapi.json", get(openapi_json))
        .route("/docs", get(docs_page))
        .route("/v1/versions", post(create_version))
        .route("/v1/versions/{version_id}/claims", post(propose_claim))
        .route("/v1/versions/{version_id}/events", post(append_events))
        .route(
            "/v1/versions/{version_id}/promises",
            post(open_promise).get(get_promises),
        )
        .route(
            "/v1/versions/{version_id}/promises/{key}/fulfill",
            post(fulfill_promise),
        )
        .route(
            "/v1/versions/{version_id}/anchors",
            post(lock_anchor).get(get_anchors),
        )
        .route(
            "/v1/versions/{version_id}/counters",
            post(record_counts).get(get_counters),
        )
        .route(
            "/v1/versions/{version_id}/digests",
            axum::routing::put(upsert_digest).get(get_digests),
        )
        .route("/v1/versions/{version_id}/head", get(get_head))
        .route(
            "/v1/versions/{version_id}/findings",
            get(get_findings).post(record_findings),
        )
        .route("/v1/versions/{version_id}/facts", get(get_facts))
        .route("/v1/versions/{version_id}/lineage", get(get_lineage))
        .route("/v1/versions/{version_id}/context", get(get_context))
        .route("/v1/claims/{claim_id}", get(get_claim))
        .route("/v1/shapes", post(apply_shape))
        .route(
            "/v1/chronology-rules",
            post(crate::chronology_api::apply_rules),
        )
        .route(
            "/v1/chronology-rules/{name}",
            get(crate::chronology_api::get_rules),
        )
        .route(
            "/v1/sources",
            axum::routing::put(put_source)
                // Sources are whole documents, not form posts: axum's 2 MiB
                // default would 413 any real corpus file. The handler buffers
                // into memory, so this cap is the memory guard.
                .layer(DefaultBodyLimit::max(MAX_SOURCE_BYTES)),
        )
        .route("/v1/versions/{version_id}/ingests", post(record_ingest))
        .route("/v1/indexes/{shape_ref}/build", post(build_index))
        .route("/v1/indexes/{shape_ref}", get(get_index))
        .route("/v1/search", post(hybrid_search))
        .route(
            "/v1/providers",
            post(crate::providers_api::apply_provider).get(crate::providers_api::list_providers),
        )
        .route(
            "/v1/providers/{name}/health",
            get(crate::providers_api::provider_health),
        )
        .route(
            "/v1/providers/{name}/complete",
            post(crate::providers_api::provider_complete),
        )
        .route(
            "/v1/providers/{name}/embed",
            post(crate::providers_api::provider_embed),
        )
        .route(
            "/v1/runbooks",
            post(crate::runbooks_api::apply_runbook).get(crate::runbooks_api::list_runbooks),
        )
        .route(
            "/v1/runbooks/validate",
            post(crate::runbooks_api::validate_runbook),
        )
        .route(
            "/v1/runbooks/{name}",
            get(crate::runbooks_api::get_runbook_info),
        )
        .route(
            "/v1/runbooks/{name}/runs",
            post(crate::runbooks_api::run_runbook),
        )
        // Guided authoring: catalog reads are open to any authenticated
        // role; draft commands are rw like the rest of the control plane.
        .route(
            "/v1/authoring/patterns",
            get(crate::authoring_api::list_patterns),
        )
        .route(
            "/v1/authoring/patterns/{id}",
            get(crate::authoring_api::get_pattern),
        )
        .route(
            "/v1/authoring/drafts",
            post(crate::authoring_api::create_draft).get(crate::authoring_api::list_drafts),
        )
        .route(
            "/v1/authoring/drafts/{draft_id}",
            get(crate::authoring_api::get_draft).delete(crate::authoring_api::delete_draft),
        )
        .route(
            "/v1/authoring/drafts/{draft_id}/answers",
            axum::routing::put(crate::authoring_api::update_answers),
        )
        .route(
            "/v1/authoring/drafts/{draft_id}/validate",
            post(crate::authoring_api::validate_draft),
        )
        .route(
            "/v1/authoring/drafts/{draft_id}/assist",
            post(crate::authoring_api::assist_draft),
        )
        .route(
            "/v1/authoring/drafts/{draft_id}/export",
            post(crate::authoring_api::export_draft),
        )
        .route(
            "/v1/authoring/drafts/{draft_id}/apply",
            post(crate::authoring_api::apply_draft),
        )
        .route(
            "/v1/runbooks/{name}/sessions",
            post(crate::sessions_api::create_session),
        )
        .route("/v1/sessions/{id}/turns", post(crate::sessions_api::turn))
        .route(
            "/v1/sessions/{id}/turns/stream",
            post(crate::sessions_api::turn_stream),
        )
        .route("/v1/sessions/{id}", get(crate::sessions_api::get_session))
        .route(
            "/v1/sessions/{id}/close",
            post(crate::sessions_api::close_session),
        )
        // Same 256 MiB ceiling as PUT /v1/sources. Without this layer axum's
        // 2 MiB default governs, and base64 is 4/3 — so a whole BATCH was
        // capped at ~1.5 MB of document bytes, which no real corpus fits in.
        .route(
            "/v1/ingest",
            post(crate::ingest_api::ingest_file).layer(DefaultBodyLimit::max(MAX_SOURCE_BYTES)),
        )
        .route(
            "/v1/ingest/batch",
            post(crate::ingest_api::ingest_batch).layer(DefaultBodyLimit::max(MAX_SOURCE_BYTES)),
        )
        // Bulk sessions: the manifest can be large (a 100k-entry manifest is
        // ~15 MB of JSON) and chunks carry the same base64 payloads as batch
        // ingest, so both writes get the same 256 MiB ceiling.
        .route(
            "/v1/ingest/bulk",
            post(crate::ingest_api::bulk_open).layer(DefaultBodyLimit::max(MAX_SOURCE_BYTES)),
        )
        .route(
            "/v1/ingest/bulk/{bulk_id}",
            get(crate::ingest_api::bulk_status),
        )
        .route(
            "/v1/ingest/bulk/{bulk_id}/chunk",
            post(crate::ingest_api::bulk_chunk).layer(DefaultBodyLimit::max(MAX_SOURCE_BYTES)),
        )
        .route(
            "/v1/ingest/bulk/{bulk_id}/complete",
            post(crate::ingest_api::bulk_complete),
        )
        .route("/v1/sources/{source_id}", get(get_source))
        // The sealed evidence plane. REST-only in v1.
        .route("/v1/evidence", post(crate::evidence_routes::seal))
        .route(
            "/v1/evidence/{evidence_id}",
            get(crate::evidence_routes::get_manifest).delete(crate::evidence_routes::purge),
        )
        .route(
            "/v1/evidence/{evidence_id}/legal-hold",
            post(crate::evidence_routes::legal_hold),
        )
        .route(
            "/v1/evidence/{evidence_id}/bytes",
            axum::routing::put(crate::evidence_routes::put_bytes)
                // Same ceiling as PUT /v1/sources: the handler buffers the
                // body, so this cap is the memory guard, not a policy.
                .layer(DefaultBodyLimit::max(MAX_SOURCE_BYTES)),
        )
        .route(
            "/v1/evidence/{evidence_id}/commit",
            post(crate::evidence_routes::commit),
        )
        .route(
            "/v1/evidence/{evidence_id}/rows",
            get(crate::evidence_routes::get_rows),
        )
        .route(
            "/v1/evidence/{evidence_id}/accesses",
            get(crate::evidence_routes::get_accesses),
        )
        .route(
            "/v1/runbooks/{name}/remove-request",
            post(crate::runbooks_api::remove_request),
        )
        .route(
            "/v1/runbooks/{name}/remove-confirm",
            post(crate::runbooks_api::remove_confirm),
        )
        .route("/v1/runs/{run_id}", get(crate::runbooks_api::get_run))
        .route(
            "/v1/runs/{run_id}/steps/{ordinal}/approve",
            post(crate::runbooks_api::approve_step),
        )
        .route(
            "/v1/access-tokens",
            post(crate::tokens_api::issue_access_token).get(crate::reports_api::list_tokens),
        )
        .route(
            "/v1/access-tokens/{jti}/revoke",
            post(crate::reports_api::revoke_token),
        )
        .route("/v1/reports/usage", get(crate::reports_api::usage))
        .route("/v1/reports/audit", get(crate::reports_api::audit))
        .route("/v1/reports/cost", get(crate::reports_api::cost))
        .route("/v1/reports/budgets", get(crate::reports_api::budgets))
        .route(
            "/v1/max-tokens",
            get(crate::max_tokens_api::get_max_tokens)
                .post(crate::max_tokens_api::replace_max_tokens),
        )
        .route(
            "/v1/reports/timeseries",
            get(crate::reports_api::timeseries),
        )
        .route("/v1/reports/endpoints", get(crate::reports_api::endpoints))
        // How the evidence hierarchy behaved, and whether the
        // structured-evidence plane is reachable.
        .route(
            "/v1/reports/evidence",
            get(crate::reports_api::evidence_report),
        )
        .route("/v1/reports/matrix", get(crate::reports_api::matrix_report))
        .route(
            "/v1/reports/runbooks",
            get(crate::reports_api::runbook_report),
        )
        .route(
            "/v1/reports/sessions",
            get(crate::reports_api::sessions_report),
        )
        .route(
            "/v1/collections",
            post(crate::collections_api::create_collection)
                .get(crate::collections_api::list_collections),
        )
        .route(
            "/v1/collections/{id}",
            get(crate::collections_api::get_collection),
        )
        // The derived-index tier. Reporting and operating
        // on artifacts; none of these changes which version is ACTIVE, and a
        // mirror never writes the `serving` binding.
        .route(
            "/v1/index-artifacts/backfill",
            post(crate::datastore_builds::backfill),
        )
        .route(
            "/v1/index-artifacts/{index_version_id}",
            get(crate::datastore_builds::artifact_status),
        )
        .route(
            "/v1/index-artifacts/{index_version_id}/verify",
            post(crate::datastore_builds::verify_artifacts),
        )
        .route(
            "/v1/index-artifacts/{index_version_id}/rebuild",
            post(crate::datastore_builds::rebuild_artifact),
        )
        .route(
            "/v1/index-artifacts/{index_version_id}/bind",
            post(crate::datastore_builds::bind_artifact),
        )
        .route(
            "/v1/index-artifacts/{index_version_id}/promote",
            post(crate::datastore_builds::promote_artifact),
        )
        .route(
            "/v1/collections/{collection_id}/activate-index",
            post(crate::datastore_builds::activate_collection_index),
        )
        // Durable build jobs (§8.6): enqueue here, execute in whichever
        // process runs the builder loop.
        .route(
            "/v1/index-build-jobs",
            post(crate::datastore_jobs::enqueue_job).get(crate::datastore_jobs::list_jobs),
        )
        .route(
            "/v1/index-build-jobs/{job_id}",
            get(crate::datastore_jobs::get_job),
        )
        .route(
            "/v1/index-build-jobs/{job_id}/cancel",
            post(crate::datastore_jobs::cancel_job),
        )
        // The rollout selector: which engine serves a scope, and the
        // operator's one-call rollback to PostgreSQL.
        .route(
            "/v1/retrieval-rollout",
            axum::routing::put(crate::datastore_serving::rollout_set),
        )
        .route(
            "/v1/retrieval-rollout/{scope_kind}/{scope_id}",
            get(crate::datastore_serving::rollout_get),
        )
        // /admin operator console — dashboards + control-plane viewers and
        // actions (mgmt-auth HTML; outside /v1 and outside the OpenAPI
        // contract, like /docs).
        .merge(crate::dashboard::routes())
        // uid contract + interaction capture over every /v1 route (meta
        // routes pass through untouched inside the middleware).
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::capture,
        ))
        .with_state(state)
}
