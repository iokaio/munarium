// SPDX-License-Identifier: Apache-2.0
//! REST routes for the sealed evidence plane.
//!
//! Thin by design: every handler resolves the principal, then calls one
//! `evidence_api::op_*`. The semantics live in `evidence_api`, so a gRPC twin
//! would be a translation rather than a rewrite — the same service/DTO split
//! the rest of this tree keeps.
//!
//! The plane is **REST-only in v1**; the plan says so explicitly, and nothing
//! about sealing wants a streaming RPC.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::Deserialize;

use crate::evidence_api;
use crate::evidence_api::ApiResult;
use crate::middleware::uid_or_anonymous;
use crate::state::AppState;
use munarium_api_types as dto;

/// `POST /v1/evidence` — seal an artifact inline, or take an upload grant.
///
/// Inline when `bytes_base64` is present and at or under 1 MiB; otherwise the
/// response carries a single-use grant for `PUT .../bytes`.
///
/// Idempotent by the domain tuple `(tenant, logical_result_hash,
/// policy_version, authorization_class)` — a retrying Matrix replica re-seals
/// nothing, and needs no `Idempotency-Key` header for that: this route does
/// not consult the header-keyed idempotency store the other commands use,
/// because the domain key is the stronger guarantee (it holds across
/// replicas and across headers).
///
/// Auth: static **rw**, or a capability token carrying the `evidence` scope,
/// which must additionally **dominate the class the manifest declares**. A
/// principal cannot seal evidence it could not itself read.
#[utoipa::path(post, path = "/v1/evidence",
    request_body = dto::SealEvidenceRequest,
    responses((status = 200, body = dto::SealEvidenceResponse),
              (status = 403, body = dto::Problem, description = "no evidence scope, or sealing above the caller's class"),
              (status = 409, body = dto::Problem, description = "the bytes do not match the declared hash or length"),
              (status = 413, body = dto::Problem, description = "over the inline cap; use the grant flow")),
    tag = "evidence")]
pub async fn seal(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uid: Option<axum::Extension<crate::middleware::Uid>>,
    crate::rest::ProblemJson(req): crate::rest::ProblemJson<dto::SealEvidenceRequest>,
) -> ApiResult<Json<dto::SealEvidenceResponse>> {
    let uid = uid_or_anonymous(uid.as_ref());
    let access = evidence_api::evidence_access(&state, &headers, &uid).await?;
    Ok(Json(
        evidence_api::op_seal(&state, &access, &req.manifest, req.bytes_base64.as_deref()).await?,
    ))
}

#[derive(Debug, Deserialize)]
pub struct GrantParams {
    /// The grant id handed back by the sealing call.
    pub grant: String,
}

/// `PUT /v1/evidence/{id}/bytes?grant=<id>` — the grant flow's upload step.
///
/// The bytes are verified against the manifest BEFORE the grant is spent, so a
/// corrupt upload can be retried; burning a single-use grant on a client-side
/// error would turn a recoverable mistake into an unrecoverable one.
#[utoipa::path(put, path = "/v1/evidence/{evidence_id}/bytes",
    params(("grant" = String, Query, description = "the single-use grant id from the seal response")),
    request_body(content = Vec<u8>, content_type = "application/octet-stream",
        description = "the artifact bytes"),
    responses((status = 204, description = "stored"),
              (status = 403, body = dto::Problem, description = "grant unknown, expired, already used, or for another artifact"),
              (status = 409, body = dto::Problem, description = "the bytes do not match the declared hash or length")),
    tag = "evidence")]
pub async fn put_bytes(
    State(state): State<Arc<AppState>>,
    Path(evidence_id): Path<String>,
    Query(q): Query<GrantParams>,
    headers: HeaderMap,
    uid: Option<axum::Extension<crate::middleware::Uid>>,
    body: axum::body::Bytes,
) -> ApiResult<axum::http::StatusCode> {
    let uid = uid_or_anonymous(uid.as_ref());
    let access = evidence_api::evidence_access(&state, &headers, &uid).await?;
    evidence_api::op_put_bytes(&state, &access, &evidence_id, &q.grant, &body).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// `POST /v1/evidence/{id}/commit` — make a granted artifact citable.
///
/// Re-reads the stored bytes and verifies both declared facts again. Commit is
/// the moment an artifact becomes citable, so it is the moment "these bytes
/// are that hash" has to be true — not merely the moment an upload happened to
/// return 204.
#[utoipa::path(post, path = "/v1/evidence/{evidence_id}/commit",
    responses((status = 200, body = dto::CommitEvidenceResponse),
              (status = 409, body = dto::Problem, description = "bytes missing or hash mismatch")),
    tag = "evidence")]
pub async fn commit(
    State(state): State<Arc<AppState>>,
    Path(evidence_id): Path<String>,
    headers: HeaderMap,
    uid: Option<axum::Extension<crate::middleware::Uid>>,
) -> ApiResult<Json<dto::CommitEvidenceResponse>> {
    let uid = uid_or_anonymous(uid.as_ref());
    let access = evidence_api::evidence_access(&state, &headers, &uid).await?;
    Ok(Json(
        evidence_api::op_commit(&state, &access, &evidence_id).await?,
    ))
}

/// `GET /v1/evidence/{id}` — the manifest, access-checked and audited.
///
/// A session that does not dominate the artifact's authorization class is
/// refused with `evidence-forbidden`, whose detail says nothing about the
/// artifact — learning "this exists and is above you" is itself a disclosure.
/// A purged artifact answers `evidence-expired` rather than 404, because the
/// citation was real and the retention policy is the honest reason it no
/// longer resolves.
#[utoipa::path(get, path = "/v1/evidence/{evidence_id}",
    responses((status = 200, body = dto::EvidenceManifestResponse),
              (status = 403, body = dto::Problem, description = "the session does not dominate the artifact's class"),
              (status = 409, body = dto::Problem, description = "manifest registered but bytes never committed"),
              (status = 410, body = dto::Problem, description = "purged under its retention policy")),
    tag = "evidence")]
pub async fn get_manifest(
    State(state): State<Arc<AppState>>,
    Path(evidence_id): Path<String>,
    headers: HeaderMap,
    uid: Option<axum::Extension<crate::middleware::Uid>>,
) -> ApiResult<Json<dto::EvidenceManifestResponse>> {
    let uid = uid_or_anonymous(uid.as_ref());
    let access = evidence_api::evidence_access(&state, &headers, &uid).await?;
    Ok(Json(
        evidence_api::op_get_manifest(&state, &access, &evidence_id).await?,
    ))
}

#[derive(Debug, Deserialize)]
pub struct RowParams {
    #[serde(default)]
    pub from: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `GET /v1/evidence/{id}/rows?from=&limit=` — a bounded, audited window.
///
/// Range-capped at 1000 rows and audited per call. Served for the canonical
/// CSV form only: Parquet artifacts are sealed and replayed byte-for-byte but
/// not decoded here, because pulling a Parquet reader into the image to
/// paginate rows is a large dependency for a convenience, and G1 — replay — is
/// about the bytes, which are intact either way.
#[utoipa::path(get, path = "/v1/evidence/{evidence_id}/rows",
    params(("from" = Option<usize>, Query, description = "zero-based first row, default 0"),
           ("limit" = Option<usize>, Query, description = "default 100, capped at 1000")),
    responses((status = 200, body = dto::EvidenceRowsResponse),
              (status = 403, body = dto::Problem, description = "the session does not dominate the artifact's class"),
              (status = 410, body = dto::Problem, description = "purged under its retention policy")),
    tag = "evidence")]
pub async fn get_rows(
    State(state): State<Arc<AppState>>,
    Path(evidence_id): Path<String>,
    Query(q): Query<RowParams>,
    headers: HeaderMap,
    uid: Option<axum::Extension<crate::middleware::Uid>>,
) -> ApiResult<Json<dto::EvidenceRowsResponse>> {
    let uid = uid_or_anonymous(uid.as_ref());
    let access = evidence_api::evidence_access(&state, &headers, &uid).await?;
    Ok(Json(
        evidence_api::op_get_rows(
            &state,
            &access,
            &evidence_id,
            q.from.unwrap_or(0),
            q.limit.unwrap_or_else(evidence_api::default_row_limit),
        )
        .await?,
    ))
}

#[derive(Debug, Deserialize)]
pub struct AccessesParams {
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `GET /v1/evidence/{id}/accesses` — the resolution audit for one artifact.
///
/// **Mgmt-gated**, not evidence-scoped: this answers "who has been reading
/// this?", which is an operator's question about the deployment rather than a
/// participant's question about the data. A service that can seal evidence has
/// no business enumerating who read it.
///
/// Returns *that* reads happened — uid, kind, outcome, time — and never the
/// rows themselves.
#[utoipa::path(get, path = "/v1/evidence/{evidence_id}/accesses",
    params(("limit" = Option<usize>, Query, description = "default 100, capped at 1000")),
    responses((status = 200, body = dto::EvidenceAccessesResponse),
              (status = 403, body = dto::Problem, description = "mgmt role required")),
    tag = "evidence")]
pub async fn get_accesses(
    State(state): State<Arc<AppState>>,
    Path(evidence_id): Path<String>,
    Query(q): Query<AccessesParams>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::EvidenceAccessesResponse>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    let limit = q
        .limit
        .unwrap_or_else(evidence_api::default_row_limit)
        .clamp(1, munarium_core::evidence::MAX_ROW_LIMIT);
    let rows = evidence_api::op_accesses(&state, &ctx.tenant_id, &evidence_id, limit).await?;
    Ok(Json(dto::EvidenceAccessesResponse {
        evidence_id,
        accesses: rows
            .into_iter()
            .map(|a| dto::EvidenceAccessDto {
                uid: a.uid,
                kind: a.kind,
                row_from: a.row_from,
                row_limit: a.row_limit,
                outcome: a.outcome,
                at: a.at,
            })
            .collect(),
    }))
}

/// `DELETE /v1/evidence/{evidence_id}` — purge one artifact's bytes now.
///
/// **Mgmt-gated.** Retention is an operator's decision, not a participant's: a
/// service that can seal evidence must not be able to destroy it.
///
/// Refuses `evidence-on-hold` (409) when a legal hold is in force — which is
/// what makes a hold mean anything, and what keeps that slug a reachable
/// refusal rather than dead vocabulary. The metadata row survives with
/// `purged_at`, so every citation keeps resolving as `evidence-expired`.
#[utoipa::path(delete, path = "/v1/evidence/{evidence_id}",
    responses((status = 200, body = dto::PurgeEvidenceResponse),
              (status = 403, body = dto::Problem, description = "mgmt role required"),
              (status = 409, body = dto::Problem, description = "under legal hold")),
    tag = "evidence")]
pub async fn purge(
    State(state): State<Arc<AppState>>,
    Path(evidence_id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::PurgeEvidenceResponse>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    let purged = evidence_api::op_purge(&state, &ctx.tenant_id, &evidence_id).await?;
    Ok(Json(dto::PurgeEvidenceResponse {
        evidence_id,
        purged,
        state: "purged".into(),
    }))
}

/// `POST /v1/evidence/{evidence_id}/legal-hold` — place or lift a hold.
///
/// **Mgmt-gated**, for the same reason as purge. A hold survives an expiry: an
/// artifact past its retention date but under hold is skipped by the janitor
/// indefinitely, which is exactly what a hold is for.
#[utoipa::path(post, path = "/v1/evidence/{evidence_id}/legal-hold",
    request_body = dto::LegalHoldRequest,
    responses((status = 204, description = "hold placed or lifted"),
              (status = 403, body = dto::Problem, description = "mgmt role required"),
              (status = 404, body = dto::Problem, description = "no such artifact")),
    tag = "evidence")]
pub async fn legal_hold(
    State(state): State<Arc<AppState>>,
    Path(evidence_id): Path<String>,
    headers: HeaderMap,
    crate::rest::ProblemJson(req): crate::rest::ProblemJson<dto::LegalHoldRequest>,
) -> ApiResult<axum::http::StatusCode> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    evidence_api::op_set_legal_hold(&state, &ctx.tenant_id, &evidence_id, req.hold).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    //! The sealed evidence plane, end to end over the router.
    //!
    //! These are the required tests for the evidence plane,
    //! written against the router rather than the service functions, because
    //! every one is really a question about the *API's* behavior: what a
    //! caller may seal, what a caller may read, and what a caller learns when
    //! refused.
    //!
    //! The memory store backs them, so they run in every contributor's `cargo
    //! test` with no database.
    //!
    //! Package 1 left one deliberate omission here: nothing asserted that a
    //! purged artifact resolves `evidence-expired`, because nothing could
    //! reach the `Purged` state without a back door into the state machine.
    //! **Package 2 opened that door properly** — the janitor and the purge
    //! route — so the Retention section below is exactly the test that note
    //! deferred.

    use std::sync::Arc;

    use base64::Engine;
    use tower::ServiceExt;

    use crate::config::{AuthMode, Config, DocIntelConfig, SourceStoreConfig, StoreKind};
    use crate::state::AppState;

    const SECRET: &[u8] = b"evidence-plane-test-secret-32-bytes!!";

    fn test_config() -> Config {
        Config {
            http_addr: "127.0.0.1:0".into(),
            grpc_addr: None,
            ops_addr: "127.0.0.1:0".into(),
            store: StoreKind::Memory,
            database_url: None,
            // Static tokens, NOT Disabled: these tests are ABOUT the
            // authorization behavior, and `Disabled` maps every caller to an
            // unrestricted principal that dominates everything.
            auth: AuthMode::Static(vec![
                ("rw-token".into(), "tenant-default".into(), "rw".into()),
                ("mgmt-token".into(), "tenant-default".into(), "mgmt".into()),
            ]),
            shutdown_grace_secs: 1,
            token_secret: Some(SECRET.to_vec()),
            token_ttl_secs: 3600,
            require_uid: false,
            interaction_body_max: 32768,
            token_revocation_check: false,
            matrix_base_url: None,
            matrix_admin_url: None,
            max_concurrency: 4,
            db_max_conns: 2,
            idempotency_ttl_secs: 86_400,
            replica_count: 1,
            registry_ttl_secs: 15,
            session_idle_ttl_secs: 0,
            evidence_purge_interval_secs: 0,
            max_tokens: munarium_api_types::MaxTokensBudgets::default(),
            instance_id: "test-instance".into(),
            source_store: SourceStoreConfig::Mem,
            doc_intel: DocIntelConfig::None,
        }
    }

    async fn state() -> Arc<AppState> {
        AppState::new(test_config()).await.expect("state")
    }

    /// A capability token at `level`, holding `compartments` and `scopes`.
    fn token(level: i32, compartments: &[&str], scopes: &[&str]) -> String {
        munarium_access::issue(
            SECRET,
            "matrix",
            "tenant-default",
            level,
            compartments.iter().map(|s| s.to_string()).collect(),
            scopes.iter().map(|s| s.to_string()).collect(),
            None,
            3600,
            String::new(),
        )
        .expect("issue")
        .0
    }

    fn evidence_token(level: i32, compartments: &[&str]) -> String {
        token(level, compartments, &["evidence"])
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    }

    /// The canonical CSV form the manifest below describes.
    const CSV: &str = "region,amount\nEMEA,2770001.00\nAMER,900000.50\n";

    fn sha(bytes: &[u8]) -> String {
        use sha2::Digest;
        format!("sha256:{}", hex::encode(sha2::Sha256::digest(bytes)))
    }

    /// A valid manifest for `CSV`, at the given authorization class.
    fn manifest(level: i32, compartments: &[&str]) -> serde_json::Value {
        let bytes = CSV.as_bytes();
        serde_json::json!({
            "contract_version": munarium_core::evidence::CONTRACT_VERSION.trim(),
            "canon": "canon@1",
            "tenant": "tenant-default",
            "kind": "table",
            "logical_result_hash": format!("sha256:{}", "1".repeat(64)),
            "artifact_hash": sha(bytes),
            "bytes_len": bytes.len(),
            "media_type": "text/csv; charset=utf-8",
            "source": { "source_id": "crm", "source_version": 1, "adapter": "postgres" },
            "versions": { "policy": "policy@3" },
            "schema": { "columns": [
                {"id": "c1", "name": "region", "type": "string", "nullable": false, "key": true},
                {"id": "c2", "name": "amount", "type": "decimal", "nullable": false, "scale": 2}
            ]},
            "identity": { "row_id_rule": "keys", "rows": 2 },
            "completeness": { "truncated": false },
            "snapshot_vector": [{ "source_id": "crm", "replay_level": "sealed_result" }],
            "execution": {
                "started_at": "2026-08-28T10:00:00Z",
                "ended_at": "2026-08-28T10:00:01Z"
            },
            "authorization_class": {
                "access_level": level,
                "compartments": compartments.iter().map(|c| c.to_string()).collect::<Vec<_>>()
            }
        })
    }

    fn seal_body(manifest: serde_json::Value, bytes: Option<&[u8]>) -> axum::body::Body {
        let mut req = serde_json::json!({ "manifest": manifest });
        if let Some(b) = bytes {
            req["bytes_base64"] =
                serde_json::Value::String(base64::engine::general_purpose::STANDARD.encode(b));
        }
        axum::body::Body::from(serde_json::to_vec(&req).unwrap())
    }

    fn post_seal(token: &str, body: axum::body::Body) -> axum::http::Request<axum::body::Body> {
        axum::http::Request::post("/v1/evidence")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .header("x-munarium-uid", "matrix")
            .body(body)
            .unwrap()
    }

    /// NOTE the uid: it must equal the token's `sub`, or the uid-mismatch
    /// check refuses before authorization is ever consulted. Every token these
    /// tests mint is issued to `matrix`, so every request asserts that uid.
    fn get(path: &str, token: &str) -> axum::http::Request<axum::body::Body> {
        axum::http::Request::get(path)
            .header("authorization", format!("Bearer {token}"))
            .header("x-munarium-uid", "matrix")
            .body(axum::body::Body::empty())
            .unwrap()
    }

    // -----------------------------------------------------------------------
    // Seal
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn an_inline_seal_commits_in_one_round_trip() {
        let rest = crate::rest::router(state().await);
        let resp = rest
            .oneshot(post_seal(
                &evidence_token(5, &["fin"]),
                seal_body(manifest(2, &["fin"]), Some(CSV.as_bytes())),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["state"], "committed");
        assert_eq!(body["created"], true);
        assert!(body["evidence_id"].as_str().unwrap().starts_with("ev-"));
        // No grant on the inline path — the whole point is one call.
        assert!(body["grant"].is_null());
    }

    #[tokio::test]
    async fn a_token_without_the_evidence_scope_is_refused() {
        let rest = crate::rest::router(state().await);
        let resp = rest
            .oneshot(post_seal(
                &token(5, &["fin"], &["query"]),
                seal_body(manifest(2, &["fin"]), Some(CSV.as_bytes())),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::FORBIDDEN);
        let body = body_json(resp).await;
        assert!(
            body["type"].as_str().unwrap().contains("scope-missing"),
            "expected scope-missing, got {body}"
        );
    }

    #[tokio::test]
    async fn a_caller_cannot_seal_above_its_own_clearance() {
        // The forgery this prevents: a low-clearance service minting
        // high-clearance evidence that every later reader would trust.
        let resp = crate::rest::router(state().await)
            .oneshot(post_seal(
                &evidence_token(2, &["fin"]),
                seal_body(manifest(9, &["fin"]), Some(CSV.as_bytes())),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::FORBIDDEN);

        // Missing a compartment is the same refusal.
        let resp = crate::rest::router(state().await)
            .oneshot(post_seal(
                &evidence_token(9, &["fin"]),
                seal_body(manifest(2, &["fin", "hr"]), Some(CSV.as_bytes())),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn sealing_into_another_tenant_is_refused() {
        let mut m = manifest(2, &["fin"]);
        m["tenant"] = serde_json::Value::String("someone-else".into());
        let resp = crate::rest::router(state().await)
            .oneshot(post_seal(
                &evidence_token(5, &["fin"]),
                seal_body(m, Some(CSV.as_bytes())),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
        let body = body_json(resp).await;
        assert!(
            body["detail"].as_str().unwrap().contains("someone-else"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn bytes_that_do_not_match_the_declared_hash_are_refused() {
        let resp = crate::rest::router(state().await)
            .oneshot(post_seal(
                &evidence_token(5, &["fin"]),
                // Right manifest, wrong bytes.
                seal_body(manifest(2, &["fin"]), Some(b"region,amount\nEMEA,1\n")),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::CONFLICT);
        let body = body_json(resp).await;
        assert!(
            body["type"]
                .as_str()
                .unwrap()
                .contains("evidence-hash-mismatch"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn a_replayed_seal_returns_the_same_id_and_creates_nothing() {
        // The DOMAIN idempotency layer: same logical result, same policy, same
        // class — same seal, with no Idempotency-Key involved at all.
        let st = state().await;
        let tok = evidence_token(5, &["fin"]);

        let first = crate::rest::router(st.clone())
            .oneshot(post_seal(
                &tok,
                seal_body(manifest(2, &["fin"]), Some(CSV.as_bytes())),
            ))
            .await
            .unwrap();
        let first = body_json(first).await;
        assert_eq!(first["created"], true);

        let second = crate::rest::router(st)
            .oneshot(post_seal(
                &tok,
                seal_body(manifest(2, &["fin"]), Some(CSV.as_bytes())),
            ))
            .await
            .unwrap();
        let second = body_json(second).await;
        assert_eq!(second["created"], false, "a replay must not create");
        assert_eq!(
            first["evidence_id"], second["evidence_id"],
            "a replay must resolve to the SAME artifact"
        );
    }

    #[tokio::test]
    async fn the_same_result_under_a_different_class_is_a_different_artifact() {
        // Not idempotent across classes, on purpose: otherwise an idempotent
        // re-seal could hand a low-clearance caller an id minted for a
        // high-clearance read.
        let st = state().await;
        let a = body_json(
            crate::rest::router(st.clone())
                .oneshot(post_seal(
                    &evidence_token(9, &["fin", "hr"]),
                    seal_body(manifest(2, &["fin"]), Some(CSV.as_bytes())),
                ))
                .await
                .unwrap(),
        )
        .await;
        let b = body_json(
            crate::rest::router(st)
                .oneshot(post_seal(
                    &evidence_token(9, &["fin", "hr"]),
                    seal_body(manifest(7, &["fin", "hr"]), Some(CSV.as_bytes())),
                ))
                .await
                .unwrap(),
        )
        .await;
        assert_ne!(a["evidence_id"], b["evidence_id"]);
    }

    #[tokio::test]
    async fn an_oversized_inline_seal_names_the_grant_flow() {
        let big = vec![b'x'; munarium_core::evidence::INLINE_SEAL_MAX_BYTES + 1];
        let mut m = manifest(2, &["fin"]);
        m["artifact_hash"] = serde_json::Value::String(sha(&big));
        m["bytes_len"] = serde_json::Value::from(big.len());
        let resp = crate::rest::router(state().await)
            .oneshot(post_seal(
                &evidence_token(5, &["fin"]),
                seal_body(m, Some(&big)),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::PAYLOAD_TOO_LARGE);
        let body = body_json(resp).await;
        assert!(
            body["detail"].as_str().unwrap().contains("grant flow"),
            "the refusal must say what to do instead: {body}"
        );
    }

    #[tokio::test]
    async fn a_result_that_cannot_name_its_rows_is_refused_at_seal() {
        // canon@1 rule 3, enforced where it can still be acted on.
        let mut m = manifest(2, &["fin"]);
        m["identity"] = serde_json::json!({ "row_id_rule": "position" }); // no order_by
        let resp = crate::rest::router(state().await)
            .oneshot(post_seal(
                &evidence_token(5, &["fin"]),
                seal_body(m, Some(CSV.as_bytes())),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
        let body = body_json(resp).await;
        assert!(
            body["detail"].as_str().unwrap().contains("total ordering"),
            "{body}"
        );
    }

    // -----------------------------------------------------------------------
    // The grant flow
    // -----------------------------------------------------------------------

    async fn seal_with_grant(st: &Arc<AppState>, tok: &str) -> (String, String) {
        let body = body_json(
            crate::rest::router(st.clone())
                .oneshot(post_seal(tok, seal_body(manifest(2, &["fin"]), None)))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(body["state"], "pending");
        (
            body["evidence_id"].as_str().unwrap().to_string(),
            body["grant"]["grant_id"].as_str().unwrap().to_string(),
        )
    }

    fn put_bytes(
        id: &str,
        grant: &str,
        tok: &str,
        bytes: &[u8],
    ) -> axum::http::Request<axum::body::Body> {
        axum::http::Request::put(format!("/v1/evidence/{id}/bytes?grant={grant}"))
            .header("authorization", format!("Bearer {tok}"))
            .header("x-munarium-uid", "matrix")
            .body(axum::body::Body::from(bytes.to_vec()))
            .unwrap()
    }

    fn post_commit(id: &str, tok: &str) -> axum::http::Request<axum::body::Body> {
        axum::http::Request::post(format!("/v1/evidence/{id}/commit"))
            .header("authorization", format!("Bearer {tok}"))
            .header("x-munarium-uid", "matrix")
            .body(axum::body::Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn the_grant_flow_seals_bytes_then_commits() {
        let st = state().await;
        let tok = evidence_token(5, &["fin"]);
        let (id, grant) = seal_with_grant(&st, &tok).await;

        // Nothing resolves while pending — a pending artifact is not evidence.
        let resp = crate::rest::router(st.clone())
            .oneshot(get(&format!("/v1/evidence/{id}"), &tok))
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::CONFLICT);

        let resp = crate::rest::router(st.clone())
            .oneshot(put_bytes(&id, &grant, &tok, CSV.as_bytes()))
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::NO_CONTENT);

        let resp = crate::rest::router(st.clone())
            .oneshot(post_commit(&id, &tok))
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        assert_eq!(body_json(resp).await["committed"], true);

        // And now it resolves.
        let resp = crate::rest::router(st)
            .oneshot(get(&format!("/v1/evidence/{id}"), &tok))
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
    }

    #[tokio::test]
    async fn a_grant_is_single_use() {
        let st = state().await;
        let tok = evidence_token(5, &["fin"]);
        let (id, grant) = seal_with_grant(&st, &tok).await;

        let first = crate::rest::router(st.clone())
            .oneshot(put_bytes(&id, &grant, &tok, CSV.as_bytes()))
            .await
            .unwrap();
        assert_eq!(first.status(), axum::http::StatusCode::NO_CONTENT);

        let second = crate::rest::router(st)
            .oneshot(put_bytes(&id, &grant, &tok, CSV.as_bytes()))
            .await
            .unwrap();
        assert_eq!(
            second.status(),
            axum::http::StatusCode::FORBIDDEN,
            "a spent grant must not work twice"
        );
    }

    #[tokio::test]
    async fn corrupt_bytes_do_not_burn_the_grant() {
        // Verification runs BEFORE the grant is spent, so a client-side error
        // stays recoverable instead of becoming permanent.
        let st = state().await;
        let tok = evidence_token(5, &["fin"]);
        let (id, grant) = seal_with_grant(&st, &tok).await;

        let bad = crate::rest::router(st.clone())
            .oneshot(put_bytes(&id, &grant, &tok, b"not the bytes"))
            .await
            .unwrap();
        assert_eq!(bad.status(), axum::http::StatusCode::CONFLICT);

        let good = crate::rest::router(st)
            .oneshot(put_bytes(&id, &grant, &tok, CSV.as_bytes()))
            .await
            .unwrap();
        assert_eq!(
            good.status(),
            axum::http::StatusCode::NO_CONTENT,
            "the grant must still be usable after a rejected upload"
        );
    }

    #[tokio::test]
    async fn a_grant_for_another_artifact_is_refused() {
        let st = state().await;
        let tok = evidence_token(5, &["fin"]);
        let (id_a, _grant_a) = seal_with_grant(&st, &tok).await;

        // A second artifact, distinguished by its logical hash.
        let mut m = manifest(2, &["fin"]);
        m["logical_result_hash"] = serde_json::Value::String(format!("sha256:{}", "2".repeat(64)));
        let b = body_json(
            crate::rest::router(st.clone())
                .oneshot(post_seal(&tok, seal_body(m, None)))
                .await
                .unwrap(),
        )
        .await;
        let grant_b = b["grant"]["grant_id"].as_str().unwrap();

        let resp = crate::rest::router(st)
            .oneshot(put_bytes(&id_a, grant_b, &tok, CSV.as_bytes()))
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn committing_without_bytes_is_refused() {
        let st = state().await;
        let tok = evidence_token(5, &["fin"]);
        let (id, _grant) = seal_with_grant(&st, &tok).await;
        let resp = crate::rest::router(st)
            .oneshot(post_commit(&id, &tok))
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn a_replayed_commit_reports_it_rather_than_restamping() {
        let st = state().await;
        let tok = evidence_token(5, &["fin"]);
        let (id, grant) = seal_with_grant(&st, &tok).await;
        crate::rest::router(st.clone())
            .oneshot(put_bytes(&id, &grant, &tok, CSV.as_bytes()))
            .await
            .unwrap();

        let first = body_json(
            crate::rest::router(st.clone())
                .oneshot(post_commit(&id, &tok))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(first["committed"], true);

        let second = body_json(
            crate::rest::router(st)
                .oneshot(post_commit(&id, &tok))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(
            second["committed"], false,
            "a replayed commit must be visible, not silent — otherwise the retention \
             clock is restartable by anyone who can replay it"
        );
    }

    // -----------------------------------------------------------------------
    // Resolution
    // -----------------------------------------------------------------------

    async fn seal_inline(st: &Arc<AppState>, level: i32, compartments: &[&str]) -> String {
        let tok = evidence_token(9, &["fin", "hr"]);
        let body = body_json(
            crate::rest::router(st.clone())
                .oneshot(post_seal(
                    &tok,
                    seal_body(manifest(level, compartments), Some(CSV.as_bytes())),
                ))
                .await
                .unwrap(),
        )
        .await;
        body["evidence_id"]
            .as_str()
            .unwrap_or_else(|| panic!("seal failed: {body}"))
            .to_string()
    }

    #[tokio::test]
    async fn the_manifest_reads_back_with_its_assigned_id() {
        let st = state().await;
        let id = seal_inline(&st, 2, &["fin"]).await;
        let resp = crate::rest::router(st)
            .oneshot(get(
                &format!("/v1/evidence/{id}"),
                &evidence_token(5, &["fin"]),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["evidence_id"], serde_json::json!(id));
        assert_eq!(body["canon"], "canon@1");
        // The declared hashes survive verbatim — they ARE the identity.
        assert_eq!(body["artifact_hash"], sha(CSV.as_bytes()));
    }

    #[tokio::test]
    async fn an_under_cleared_session_cannot_resolve() {
        let st = state().await;
        let id = seal_inline(&st, 7, &["fin"]).await;

        // Level too low.
        let resp = crate::rest::router(st.clone())
            .oneshot(get(
                &format!("/v1/evidence/{id}"),
                &evidence_token(3, &["fin"]),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::FORBIDDEN);
        let body = body_json(resp).await;
        // The refusal must not describe the artifact: learning "this exists
        // and is above you" is itself a disclosure.
        let detail = body["detail"].as_str().unwrap();
        assert!(!detail.contains('7'), "the class must not leak: {detail}");
        assert!(
            !detail.contains("crm"),
            "the source must not leak: {detail}"
        );

        // Missing a compartment.
        let id = seal_inline(&st, 2, &["fin", "hr"]).await;
        let resp = crate::rest::router(st)
            .oneshot(get(
                &format!("/v1/evidence/{id}"),
                &evidence_token(9, &["fin"]),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn an_unknown_id_is_not_found() {
        let resp = crate::rest::router(state().await)
            .oneshot(get("/v1/evidence/ev-nope", &evidence_token(9, &["fin"])))
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn rows_are_served_paginated_and_capped() {
        let st = state().await;
        let id = seal_inline(&st, 2, &["fin"]).await;
        let tok = evidence_token(5, &["fin"]);

        let body = body_json(
            crate::rest::router(st.clone())
                .oneshot(get(&format!("/v1/evidence/{id}/rows"), &tok))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(body["total"], 2);
        assert_eq!(body["has_more"], false);
        let rows = body["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        // Column NAMES come from the manifest schema, not the CSV header.
        assert_eq!(rows[0]["region"], "EMEA");
        // The decimal survives as a string at its declared scale — the entire
        // reason the contract forbids JSON numbers for decimals.
        assert_eq!(rows[0]["amount"], "2770001.00");
        assert_eq!(rows[1]["amount"], "900000.50");

        // Paging.
        let body = body_json(
            crate::rest::router(st)
                .oneshot(get(&format!("/v1/evidence/{id}/rows?from=0&limit=1"), &tok))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(body["rows"].as_array().unwrap().len(), 1);
        assert_eq!(body["has_more"], true);
    }

    #[tokio::test]
    async fn an_under_cleared_session_cannot_read_rows_either() {
        let st = state().await;
        let id = seal_inline(&st, 7, &["fin"]).await;
        let resp = crate::rest::router(st)
            .oneshot(get(
                &format!("/v1/evidence/{id}/rows"),
                &evidence_token(3, &["fin"]),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn evidence_replays_after_the_source_would_have_changed() {
        // G1 in one test: seal, then read the exact rows back, repeatedly.
        // Nothing about the source is consulted on the read path — that is
        // what "replay" means here.
        let st = state().await;
        let id = seal_inline(&st, 2, &["fin"]).await;
        let tok = evidence_token(5, &["fin"]);
        for _ in 0..3 {
            let body = body_json(
                crate::rest::router(st.clone())
                    .oneshot(get(&format!("/v1/evidence/{id}/rows"), &tok))
                    .await
                    .unwrap(),
            )
            .await;
            assert_eq!(body["rows"][0]["amount"], "2770001.00");
        }
    }

    // -----------------------------------------------------------------------
    // The resolution audit
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn resolutions_are_audited_including_the_denials() {
        let st = state().await;
        let id = seal_inline(&st, 7, &["fin"]).await;

        // One allowed read and one refused one.
        crate::rest::router(st.clone())
            .oneshot(get(
                &format!("/v1/evidence/{id}"),
                &evidence_token(9, &["fin"]),
            ))
            .await
            .unwrap();
        crate::rest::router(st.clone())
            .oneshot(get(
                &format!("/v1/evidence/{id}/rows"),
                &evidence_token(3, &["fin"]),
            ))
            .await
            .unwrap();

        let body = body_json(
            crate::rest::router(st)
                .oneshot(
                    axum::http::Request::get(format!("/v1/evidence/{id}/accesses"))
                        .header("authorization", "Bearer mgmt-token")
                        .header("x-munarium-uid", "operator")
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        let rows = body["accesses"].as_array().unwrap();
        assert_eq!(
            rows.len(),
            2,
            "both the read AND the denial are audited: {body}"
        );
        // Newest first: the denial happened second.
        assert_eq!(rows[0]["outcome"], "denied");
        assert_eq!(rows[0]["kind"], "rows");
        assert_eq!(rows[1]["outcome"], "ok");
        assert_eq!(rows[1]["kind"], "manifest");
        // And nothing about the DATA is in the audit trail.
        let text = serde_json::to_string(&body).unwrap();
        assert!(
            !text.contains("2770001"),
            "the audit must not hold rows: {text}"
        );
        assert!(
            !text.contains("EMEA"),
            "the audit must not hold rows: {text}"
        );
    }

    #[tokio::test]
    async fn the_audit_is_mgmt_only() {
        // A service that can seal evidence has no business enumerating who
        // read it.
        let st = state().await;
        let id = seal_inline(&st, 2, &["fin"]).await;
        let resp = crate::rest::router(st)
            .oneshot(get(
                &format!("/v1/evidence/{id}/accesses"),
                &evidence_token(9, &["fin"]),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::FORBIDDEN);
    }

    // -----------------------------------------------------------------------
    // Retention
    // -----------------------------------------------------------------------
    //
    // The package-1 note here said the `evidence-expired` path had code and no
    // test, because nothing could reach the `Purged` state without a back door
    // into the state machine. The janitor and the purge route are that door,
    // opened properly, so the tests below are the ones that note deferred.

    /// A manifest with an explicit retention block.
    fn manifest_with_retention(
        level: i32,
        compartments: &[&str],
        expires_at: Option<&str>,
        legal_hold: bool,
    ) -> serde_json::Value {
        let mut m = manifest(level, compartments);
        m["retention"] = serde_json::json!({
            "expires_at": expires_at,
            "legal_hold": legal_hold,
        });
        m
    }

    fn mgmt(path: &str, method: &str) -> axum::http::request::Builder {
        axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("authorization", "Bearer mgmt-token")
            .header("x-munarium-uid", "operator")
    }

    async fn seal_with_retention(
        st: &Arc<AppState>,
        expires_at: Option<&str>,
        legal_hold: bool,
    ) -> String {
        let body = body_json(
            crate::rest::router(st.clone())
                .oneshot(post_seal(
                    &evidence_token(9, &["fin", "hr"]),
                    seal_body(
                        manifest_with_retention(2, &["fin"], expires_at, legal_hold),
                        Some(CSV.as_bytes()),
                    ),
                ))
                .await
                .unwrap(),
        )
        .await;
        body["evidence_id"]
            .as_str()
            .unwrap_or_else(|| panic!("seal failed: {body}"))
            .to_string()
    }

    #[tokio::test]
    async fn a_purged_artifact_resolves_expired_not_missing() {
        // The whole reason the metadata row survives a purge. A citation to
        // expired evidence must read as an honest statement about a retention
        // policy, never as though the citation had been fabricated.
        let st = state().await;
        let id = seal_with_retention(&st, Some("2020-01-01T00:00:00Z"), false).await;
        let tok = evidence_token(5, &["fin"]);

        // Before: it resolves.
        let resp = crate::rest::router(st.clone())
            .oneshot(get(&format!("/v1/evidence/{id}"), &tok))
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        let purged = crate::evidence_api::purge_once(&st, 100)
            .await
            .expect("sweep");
        assert_eq!(purged, 1, "the expired artifact must be swept");

        // After: 410 with the expired slug, NOT 404.
        let resp = crate::rest::router(st.clone())
            .oneshot(get(&format!("/v1/evidence/{id}"), &tok))
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::GONE);
        let body = body_json(resp).await;
        assert!(
            body["type"].as_str().unwrap().contains("evidence-expired"),
            "{body}"
        );

        // And the rows are gone too, by the same slug.
        let resp = crate::rest::router(st)
            .oneshot(get(&format!("/v1/evidence/{id}/rows"), &tok))
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::GONE);
    }

    #[tokio::test]
    async fn an_unexpired_artifact_is_not_swept() {
        let st = state().await;
        let id = seal_with_retention(&st, Some("2099-01-01T00:00:00Z"), false).await;
        assert_eq!(
            crate::evidence_api::purge_once(&st, 100)
                .await
                .expect("sweep"),
            0
        );
        let resp = crate::rest::router(st)
            .oneshot(get(
                &format!("/v1/evidence/{id}"),
                &evidence_token(5, &["fin"]),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
    }

    #[tokio::test]
    async fn an_artifact_with_no_retention_block_is_never_swept() {
        // An artifact nobody gave a lifetime to is KEPT, not guessed at. A
        // janitor that invented a default retention would delete regulated
        // data on a schedule nobody chose.
        let st = state().await;
        let id = seal_inline(&st, 2, &["fin"]).await; // no retention block at all
        assert_eq!(
            crate::evidence_api::purge_once(&st, 100)
                .await
                .expect("sweep"),
            0
        );
        let resp = crate::rest::router(st)
            .oneshot(get(
                &format!("/v1/evidence/{id}"),
                &evidence_token(5, &["fin"]),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
    }

    #[tokio::test]
    async fn a_legal_hold_survives_expiry() {
        // The point of a hold: expired AND held means kept, indefinitely.
        let st = state().await;
        let id = seal_with_retention(&st, Some("2020-01-01T00:00:00Z"), true).await;
        assert_eq!(
            crate::evidence_api::purge_once(&st, 100)
                .await
                .expect("sweep"),
            0,
            "an artifact on legal hold must survive its own expiry"
        );
        let resp = crate::rest::router(st)
            .oneshot(get(
                &format!("/v1/evidence/{id}"),
                &evidence_token(5, &["fin"]),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
    }

    #[tokio::test]
    async fn a_hold_blocks_deletion_but_never_reading() {
        let st = state().await;
        let id = seal_with_retention(&st, Some("2020-01-01T00:00:00Z"), true).await;

        // Delete is refused, with the slug that makes a hold mean something.
        let resp = crate::rest::router(st.clone())
            .oneshot(
                mgmt(&format!("/v1/evidence/{id}"), "DELETE")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::CONFLICT);
        let body = body_json(resp).await;
        assert!(
            body["type"].as_str().unwrap().contains("evidence-on-hold"),
            "{body}"
        );

        // Reading is untouched — a hold preserves evidence, it does not hide it.
        let resp = crate::rest::router(st)
            .oneshot(get(
                &format!("/v1/evidence/{id}"),
                &evidence_token(5, &["fin"]),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
    }

    #[tokio::test]
    async fn lifting_a_hold_lets_the_janitor_take_it() {
        let st = state().await;
        let id = seal_with_retention(&st, Some("2020-01-01T00:00:00Z"), true).await;
        assert_eq!(
            crate::evidence_api::purge_once(&st, 100)
                .await
                .expect("sweep"),
            0
        );

        let resp = crate::rest::router(st.clone())
            .oneshot(
                mgmt(&format!("/v1/evidence/{id}/legal-hold"), "POST")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"hold":false}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::NO_CONTENT);

        assert_eq!(
            crate::evidence_api::purge_once(&st, 100)
                .await
                .expect("sweep"),
            1,
            "with the hold lifted the expired artifact is due"
        );
    }

    #[tokio::test]
    async fn a_hold_can_be_placed_after_the_fact() {
        let st = state().await;
        let id = seal_with_retention(&st, Some("2020-01-01T00:00:00Z"), false).await;
        let resp = crate::rest::router(st.clone())
            .oneshot(
                mgmt(&format!("/v1/evidence/{id}/legal-hold"), "POST")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"hold":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::NO_CONTENT);
        assert_eq!(
            crate::evidence_api::purge_once(&st, 100)
                .await
                .expect("sweep"),
            0,
            "a hold placed after sealing must still stop the janitor"
        );
        // And the read reflects it — the column is overlaid onto the manifest.
        let body = body_json(
            crate::rest::router(st)
                .oneshot(get(
                    &format!("/v1/evidence/{id}"),
                    &evidence_token(5, &["fin"]),
                ))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(body["retention"]["legal_hold"], true, "{body}");
    }

    #[tokio::test]
    async fn purge_and_hold_are_mgmt_only() {
        // A service that can SEAL evidence must not be able to destroy it, nor
        // to lift a hold somebody placed on it.
        let st = state().await;
        let id = seal_with_retention(&st, Some("2020-01-01T00:00:00Z"), false).await;
        let tok = evidence_token(9, &["fin", "hr"]);

        let resp = crate::rest::router(st.clone())
            .oneshot(
                axum::http::Request::delete(format!("/v1/evidence/{id}"))
                    .header("authorization", format!("Bearer {tok}"))
                    .header("x-munarium-uid", "matrix")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::FORBIDDEN);

        let resp = crate::rest::router(st)
            .oneshot(
                axum::http::Request::post(format!("/v1/evidence/{id}/legal-hold"))
                    .header("authorization", format!("Bearer {tok}"))
                    .header("x-munarium-uid", "matrix")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"hold":false}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_replayed_purge_reports_it_rather_than_re_running() {
        let st = state().await;
        let id = seal_with_retention(&st, Some("2020-01-01T00:00:00Z"), false).await;

        let first = body_json(
            crate::rest::router(st.clone())
                .oneshot(
                    mgmt(&format!("/v1/evidence/{id}"), "DELETE")
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(first["purged"], true);

        let second = body_json(
            crate::rest::router(st)
                .oneshot(
                    mgmt(&format!("/v1/evidence/{id}"), "DELETE")
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(second["purged"], false, "a replayed purge must be visible");
    }

    #[tokio::test]
    async fn a_second_sweep_purges_nothing_twice() {
        // N-replica safety, in the small: the mark is conditional, so a second
        // sweep over the same artifact claims nothing.
        let st = state().await;
        let _ = seal_with_retention(&st, Some("2020-01-01T00:00:00Z"), false).await;
        assert_eq!(
            crate::evidence_api::purge_once(&st, 100)
                .await
                .expect("sweep"),
            1
        );
        assert_eq!(
            crate::evidence_api::purge_once(&st, 100)
                .await
                .expect("sweep"),
            0
        );
    }

    // -----------------------------------------------------------------------
    // The reserved keyspace
    // -----------------------------------------------------------------------
    //
    // There is no HTTP test here, and the reason is worth recording rather
    // than papering over. `PUT /v1/sources` requires the Postgres store, so
    // over a memory-backed router it refuses with "retrieval requires the
    // postgres store" — the same 400 the reserved-prefix rule would produce,
    // for an entirely different reason. A test asserting 400 would have
    // passed whether or not the guard existed, which is worse than no test.
    //
    // The rule is a pure predicate, so it is tested where it can actually be
    // exercised: `munarium_core::sources::refuse_reserved_document_path`, in
    // that module's own tests (prefix vs substring, nested paths, and the
    // paths that merely mention "evidence" and must still be accepted). The
    // one-line call site in `munarium-retrieval-pg::put_source` is covered by
    // the Postgres tier.
}
