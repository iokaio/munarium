// SPDX-License-Identifier: Apache-2.0
//! The platform gRPC twins (2026-08-18, dev-guide §13 entry 9): SessionService
//! (the data plane) and the served half of AdminService (access
//! tokens). Every RPC calls the SAME op_* function as its REST twin — the
//! guard chains here mirror the REST handlers exactly, translated to
//! metadata (`authorization` bearer + `munarium-uid`, the twin of X-Munarium-Uid).
//! The tenant-lifecycle AdminService RPCs answer UNIMPLEMENTED honestly —
//! tenancy is provisioned out of band in the demo posture (never fake).

use crate::error::to_status;
use crate::state::{AppState, Principal};
use munarium_proto::mmp::v1 as pb;
use std::sync::Arc;
use tonic::metadata::MetadataMap;
use tonic::{Request, Response, Status};

fn meta_bearer(md: &MetadataMap) -> Option<&str> {
    md.get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

/// The uid a data-plane call acts as — the same resolution the gRPC capture
/// middleware already enforced (munarium-uid metadata → JWT sub →
/// require_uid rejection → "anonymous"), recomputed here because tower
/// layers cannot pass values into tonic services.
fn resolve_uid(
    state: &AppState,
    md: &MetadataMap,
    principal: &Principal,
) -> Result<String, Status> {
    let from_md = md
        .get("munarium-uid")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    match from_md {
        Some(u) => Ok(u),
        None => match principal {
            Principal::Access(a) => Ok(a.uid.clone()),
            _ if state.config.require_uid => {
                Err(crate::error::CustomError::uid_required().to_status())
            }
            _ => Ok("anonymous".to_string()),
        },
    }
}

/// `munarium-uid` metadata or "anonymous" — attribution for management-plane
/// writes (issued_by / requested_by), the twin of middleware::uid_or_anonymous.
pub(crate) fn uid_or_anonymous_md(md: &MetadataMap) -> String {
    md.get("munarium-uid")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("anonymous")
        .to_string()
}

/// The gRPC twin of rest::data_plane_access: principal → uid → AccessCtx →
/// scope check → revocation check, with token-lifecycle errors promoted to
/// their typed slugs.
pub(crate) async fn data_plane_access(
    state: &AppState,
    md: &MetadataMap,
    scope: &str,
) -> Result<munarium_access::AccessCtx, Status> {
    let principal = state
        .authenticate_principal(meta_bearer(md))
        .map_err(|e| crate::rest::promote_auth_error(e).into_status())?;
    let uid = resolve_uid(state, md, &principal)?;
    let access = principal.access_ctx(&uid).map_err(|e| to_status(&e))?;
    if !access.has_scope(scope) {
        return Err(crate::error::CustomError::scope_missing(scope).to_status());
    }
    if !access.jti.is_empty() {
        state
            .check_revocation(&access.tenant_id, &access.jti)
            .await
            .map_err(|e| crate::rest::promote_auth_error(e).into_status())?;
    }
    Ok(access)
}

fn mgmt_principal(state: &AppState, md: &MetadataMap) -> Result<crate::state::TenantCtx, Status> {
    let ctx = state
        .authenticate(meta_bearer(md))
        .map_err(|e| crate::rest::promote_auth_error(e).into_status())?;
    ctx.require_mgmt().map_err(|e| to_status(&e))?;
    Ok(ctx)
}

// -- SessionService ----------------------------------------------------------

pub struct SessionSvc {
    pub state: Arc<AppState>,
}

fn session_to_pb(v: munarium_api_types::SessionResponse) -> pb::GetSessionResponse {
    pb::GetSessionResponse {
        session_id: v.session_id,
        uid: v.uid,
        runbook_ref: v.runbook_ref,
        access_level: v.access_level,
        compartments: v.compartments,
        state: v.state,
        created_at: v.created_at,
        turns: v
            .turns
            .into_iter()
            .map(|t| pb::SessionTurn {
                ordinal: t.ordinal,
                query: t.query,
                collections_searched: t.collections_searched,
                hits_json: t.hits.to_string(),
                envelope_json: t.envelope.to_string(),
                completion_json: t.completion.map(|c| c.to_string()).unwrap_or_default(),
                created_at: t.created_at,
            })
            .collect(),
    }
}

#[tonic::async_trait]
impl pb::session_service_server::SessionService for SessionSvc {
    async fn create_session(
        &self,
        req: Request<pb::CreateSessionRequest>,
    ) -> Result<Response<pb::CreateSessionResponse>, Status> {
        let access =
            data_plane_access(&self.state, req.metadata(), munarium_access::SCOPE_QUERY).await?;
        let inner = req.into_inner();
        let resp =
            crate::sessions_api::op_create_session(&self.state, &access, &inner.runbook_name)
                .await
                .map_err(|e| e.into_status())?;
        Ok(Response::new(pb::CreateSessionResponse {
            session_id: resp.session_id,
            runbook_ref: resp.runbook_ref,
            permitted_collections: resp.permitted_collections,
        }))
    }

    async fn turn(
        &self,
        req: Request<pb::TurnRequest>,
    ) -> Result<Response<pb::TurnResponse>, Status> {
        let access =
            data_plane_access(&self.state, req.metadata(), munarium_access::SCOPE_QUERY).await?;
        let inner = req.into_inner();
        let dto_req = munarium_api_types::TurnRequest {
            // gRPC carries the profile too; None on an older client,
            // which is exactly the legacy path.
            research_profile: crate::grpc::none_if_empty(&inner.research_profile),
            query: inner.query,
            top_k: (inner.top_k > 0).then_some(inner.top_k),
            complete: Some(inner.complete),
            model_override: inner
                .model_override
                .map(|o| munarium_api_types::ModelOverrideDto {
                    provider: crate::grpc::none_if_empty(&o.provider),
                    model: crate::grpc::none_if_empty(&o.model),
                    tier: crate::grpc::none_if_empty(&o.tier),
                }),
        };
        let (resp, meta) =
            crate::sessions_api::op_turn(&self.state, &access, &inner.session_id, dto_req, None)
                .await
                .map_err(|e| e.into_status())?;
        let mut response = Response::new(pb::TurnResponse {
            // Absent on a legacy turn, so an older client sees the
            // response it has always seen.
            hierarchy: resp.hierarchy.map(|h| pb::EvidenceHierarchyDecision {
                profile: h.profile,
                intent_kind: h.intent_kind.unwrap_or_default(),
                intent_explicit: h.intent_explicit,
                layers: h
                    .layers
                    .into_iter()
                    .map(|l| pb::LayerOutcome {
                        layer: l.layer,
                        role: l.role,
                        requirement: l.requirement,
                        block: l.block,
                        evidence_id: l.evidence_id.unwrap_or_default(),
                        supports_completeness: l.supports_completeness,
                        refusal_code: l.refusal_code.unwrap_or_default(),
                        elapsed_ms: l.elapsed_ms,
                    })
                    .collect(),
                completeness_available: h.completeness_available,
                disclosed_conflicts: h.disclosed_conflicts,
                conflicts_policy: h.conflicts_policy,
            }),
            session_id: resp.session_id,
            ordinal: resp.ordinal,
            collections_searched: resp.collections_searched,
            skipped: resp.skipped,
            hits: resp
                .hits
                .into_iter()
                .map(|h| pb::TurnHit {
                    collection: h.collection,
                    chunk_id: h.chunk_id,
                    source_id: h.source_id,
                    source_path: h.source_path,
                    source_content_hash: h.source_content_hash,
                    text: h.text,
                    score: h.score,
                })
                .collect(),
            envelopes: resp
                .envelopes
                .into_iter()
                .map(|e| pb::CollectionEnvelope {
                    collection: e.collection,
                    envelope: Some(e.envelope.into()),
                })
                .collect(),
            completion: resp.completion.map(|c| pb::TurnCompletion {
                provider: c.provider,
                model: c.model,
                was_override: c.was_override,
                text: c.text,
                input_tokens: c.input_tokens,
                output_tokens: c.output_tokens,
                verification: c.verification.map(|v| pb::TurnVerification {
                    checks: v.checks,
                    retries: v.retries,
                    first_pass_violations: v.first_pass_violations,
                    violations: v.violations,
                }),
            }),
        });
        // The capture layer reads this back off the http response extensions
        // so the interaction row carries session/runbook attribution — the
        // same channel the REST turn handler uses.
        response.extensions_mut().insert(meta);
        Ok(response)
    }

    async fn get_session(
        &self,
        req: Request<pb::GetSessionRequest>,
    ) -> Result<Response<pb::GetSessionResponse>, Status> {
        let principal = self
            .state
            .authenticate_principal(meta_bearer(req.metadata()))
            .map_err(|e| crate::rest::promote_auth_error(e).into_status())?;
        let tenant = principal.tenant_id().to_string();
        let inner_id = req.get_ref().session_id.clone();
        let resp = crate::sessions_api::op_get_session(&self.state, &tenant, &inner_id)
            .await
            .map_err(|e| to_status(&e))?;
        // Capability tokens read only their own sessions, through the same
        // guard chain as a turn (scope + revocation) — mirror of REST.
        if let Principal::Access(_) = &principal {
            let access =
                data_plane_access(&self.state, req.metadata(), munarium_access::SCOPE_QUERY)
                    .await?;
            if access.uid != resp.uid {
                return Err(to_status(&munarium_core::KernelError::Forbidden(
                    "session belongs to a different uid".into(),
                )));
            }
        }
        Ok(Response::new(session_to_pb(resp)))
    }

    async fn close_session(
        &self,
        req: Request<pb::CloseSessionRequest>,
    ) -> Result<Response<pb::GetSessionResponse>, Status> {
        let principal = self
            .state
            .authenticate_principal(meta_bearer(req.metadata()))
            .map_err(|e| crate::rest::promote_auth_error(e).into_status())?;
        let tenant = principal.tenant_id().to_string();
        let inner_id = req.get_ref().session_id.clone();
        let current = crate::sessions_api::op_get_session(&self.state, &tenant, &inner_id)
            .await
            .map_err(|e| to_status(&e))?;
        match &principal {
            Principal::Static(ctx) if ctx.role == "ro" => {
                return Err(to_status(&munarium_core::KernelError::Forbidden(
                    "role 'ro' cannot close sessions (a close is a write)".into(),
                )));
            }
            Principal::Access(_) => {
                let access =
                    data_plane_access(&self.state, req.metadata(), munarium_access::SCOPE_QUERY)
                        .await?;
                if access.uid != current.uid {
                    return Err(to_status(&munarium_core::KernelError::Forbidden(
                        "session belongs to a different uid".into(),
                    )));
                }
            }
            _ => {}
        }
        let resp = crate::sessions_api::op_close_session(&self.state, &tenant, &inner_id)
            .await
            .map_err(|e| to_status(&e))?;
        Ok(Response::new(session_to_pb(resp)))
    }
}

// -- AdminService ------------------------------------------------------------

pub struct AdminSvc {
    pub state: Arc<AppState>,
}

#[tonic::async_trait]
impl pb::admin_service_server::AdminService for AdminSvc {
    async fn create_tenant(
        &self,
        _req: Request<pb::CreateTenantRequest>,
    ) -> Result<Response<pb::CreateTenantResponse>, Status> {
        Err(Status::unimplemented(
            "tenant lifecycle is provisioned out of band in the demo posture (see admin.proto)",
        ))
    }

    async fn list_tenants(
        &self,
        _req: Request<pb::ListTenantsRequest>,
    ) -> Result<Response<pb::ListTenantsResponse>, Status> {
        Err(Status::unimplemented(
            "tenant lifecycle is provisioned out of band in the demo posture (see admin.proto)",
        ))
    }

    async fn usage(
        &self,
        _req: Request<pb::UsageRequest>,
    ) -> Result<Response<pb::UsageResponse>, Status> {
        Err(Status::unimplemented(
            "per-tenant usage reporting is REST-first: GET /v1/reports/usage (docs/api/grpc.md)",
        ))
    }

    async fn issue_access_token(
        &self,
        req: Request<pb::IssueAccessTokenRequest>,
    ) -> Result<Response<pb::IssueAccessTokenResponse>, Status> {
        let ctx = mgmt_principal(&self.state, req.metadata())?;
        let issued_by = uid_or_anonymous_md(req.metadata());
        let inner = req.into_inner();
        let dto_req = munarium_api_types::IssueTokenRequest {
            uid: inner.uid,
            access_level: inner.access_level,
            compartments: inner.compartments,
            scopes: inner.scopes,
            runbook_refs: (!inner.runbook_refs.is_empty()).then_some(inner.runbook_refs),
            ttl_secs: (inner.ttl_secs > 0).then_some(inner.ttl_secs),
        };
        // The response carries the signed JWT. The gRPC capture layer stores
        // no bodies (request/response are None on this plane), so the
        // "token material is never stored" contract holds by construction.
        let resp =
            crate::tokens_api::op_issue_token(&self.state, &issued_by, &ctx.tenant_id, dto_req)
                .await
                .map_err(|e| to_status(&e))?;
        Ok(Response::new(pb::IssueAccessTokenResponse {
            token: resp.token,
            jti: resp.jti,
            expires_at: resp.expires_at,
        }))
    }

    async fn list_access_tokens(
        &self,
        req: Request<pb::ListAccessTokensRequest>,
    ) -> Result<Response<pb::ListAccessTokensResponse>, Status> {
        let ctx = mgmt_principal(&self.state, req.metadata())?;
        let inner = req.into_inner();
        let uid = crate::grpc::none_if_empty(&inner.uid);
        let tokens = crate::reports_api::op_list_tokens(
            &self.state,
            &ctx.tenant_id,
            uid.as_deref(),
            inner.active,
        )
        .await
        .map_err(|e| to_status(&e))?;
        Ok(Response::new(pb::ListAccessTokensResponse {
            tokens: tokens
                .into_iter()
                .map(|t| pb::AccessTokenInfo {
                    jti: t.jti,
                    uid: t.uid,
                    access_level: t.access_level,
                    compartments: t.compartments,
                    scopes: t.scopes,
                    runbook_refs: t.runbook_refs.unwrap_or_default(),
                    issued_by: t.issued_by,
                    issued_at: t.issued_at,
                    expires_at: t.expires_at,
                    revoked_at: t.revoked_at.unwrap_or_default(),
                })
                .collect(),
        }))
    }

    async fn revoke_access_token(
        &self,
        req: Request<pb::RevokeAccessTokenRequest>,
    ) -> Result<Response<pb::RevokeAccessTokenResponse>, Status> {
        let ctx = mgmt_principal(&self.state, req.metadata())?;
        let inner = req.into_inner();
        let resp = crate::reports_api::op_revoke_token(&self.state, &ctx.tenant_id, inner.jti)
            .await
            .map_err(|e| to_status(&e))?;
        Ok(Response::new(pb::RevokeAccessTokenResponse {
            jti: resp.jti,
            revoked: resp.revoked,
            revocation_check_enabled: resp.revocation_check_enabled,
        }))
    }
}
