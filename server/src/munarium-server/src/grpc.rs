// SPDX-License-Identifier: Apache-2.0
//! The direct gRPC plane (:50051, and :443 via the gateway). Tonic impls
//! convert proto <-> DTO at the boundary and call the SAME service layer as
//! REST — conformance diffs the two planes.

use crate::error::to_status;
use crate::service;
use crate::state::{request_hash, AppState};
use munarium_api_conv::{convert, Convert};
use munarium_api_types as dto;
use munarium_core::storage::StorageBackend;
use munarium_core::types::{ClaimStatus, PromiseStatus};
use munarium_core::KernelError;
use munarium_proto::mmp::v1 as pb;
use prost::Message;
use std::sync::Arc;
use tonic::{Request, Response, Status};

pub struct Ctx {
    pub tenant_id: String,
    pub role: String,
    pub store: Arc<dyn StorageBackend>,
}

impl Ctx {
    pub fn require_rw_pub(&self) -> Result<(), Status> {
        self.require_rw()
    }

    fn require_rw(&self) -> Result<(), Status> {
        if self.role == "rw" {
            Ok(())
        } else {
            Err(to_status(&KernelError::Forbidden(format!(
                "role '{}' cannot execute commands (rw required)",
                self.role
            ))))
        }
    }
}

pub async fn authenticate<T>(state: &AppState, req: &Request<T>) -> Result<Ctx, Status> {
    auth(state, req).await
}

/// For streaming RPCs: authenticate from an extracted bearer token so the
/// non-Sync request body is never held across an await.
pub async fn authenticate_token(state: &AppState, bearer: Option<&str>) -> Result<Ctx, Status> {
    let ctx = state.authenticate(bearer).map_err(|e| to_status(&e))?;
    let store = state
        .store_for(&ctx.tenant_id)
        .await
        .map_err(|e| to_status(&e))?;
    Ok(Ctx {
        tenant_id: ctx.tenant_id,
        role: ctx.role,
        store,
    })
}

async fn auth<T>(state: &AppState, req: &Request<T>) -> Result<Ctx, Status> {
    let bearer = req
        .metadata()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let ctx = state.authenticate(bearer).map_err(|e| to_status(&e))?;
    let store = state
        .store_for(&ctx.tenant_id)
        .await
        .map_err(|e| to_status(&e))?;
    Ok(Ctx {
        tenant_id: ctx.tenant_id,
        role: ctx.role,
        store,
    })
}

fn idem_key<T>(req: &Request<T>) -> Result<String, Status> {
    req.metadata()
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(String::from)
        .ok_or_else(|| {
            to_status(&KernelError::InvalidInput(
                "idempotency-key metadata is required on command RPCs".into(),
            ))
        })
}

/// gRPC idempotency: replay stored proto bytes (base64 of encoded response).
async fn with_idempotency<M, F, Fut>(
    state: &AppState,
    tenant: String,
    key: String,
    req_hash: String,
    exec: F,
) -> Result<Response<M>, Status>
where
    M: Message + Default,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<M, Status>>,
{
    // Plane-namespaced hash — see rest::with_idempotency: one key reused
    // across planes is a mismatch, never a cross-format decode.
    let req_hash = format!("grpc:{req_hash}");
    match state.idem_check(&tenant, &key, &req_hash).await {
        Ok(Some(stored)) => {
            let bytes =
                hex::decode(&stored).map_err(|_| Status::internal("corrupt idempotency record"))?;
            let msg = M::decode(&bytes[..])
                .map_err(|_| Status::internal("corrupt idempotency record"))?;
            return Ok(Response::new(msg));
        }
        Ok(None) => {}
        Err(e) => return Err(to_status(&e)),
    }
    let msg = exec().await?;
    state
        .idem_store(&tenant, &key, &req_hash, &hex::encode(msg.encode_to_vec()))
        .await;
    Ok(Response::new(msg))
}

// -- conversions --------------------------------------------------------------
// The pb <-> DTO mapping lives ONCE in munarium_api_types::wire (shared with
// every Rust gRPC client); this plane only re-exports the helpers it uses.

pub(crate) use munarium_api_types::wire::none_if_empty;

fn parse_json_opt(s: &str) -> Option<serde_json::Value> {
    if s.is_empty() {
        None
    } else {
        serde_json::from_str(s).ok()
    }
}

// -- CommandService --------------------------------------------------------

pub struct CommandSvc {
    pub state: Arc<AppState>,
}

#[tonic::async_trait]
impl pb::command_service_server::CommandService for CommandSvc {
    async fn create_version(
        &self,
        req: Request<pb::CreateVersionRequest>,
    ) -> Result<Response<pb::CreateVersionResponse>, Status> {
        let ctx = auth(&self.state, &req).await?;
        ctx.require_rw()?;
        let key = idem_key(&req)?;
        let inner = req.into_inner();
        let hash = request_hash(&inner.encode_to_vec());
        with_idempotency(
            &self.state,
            ctx.tenant_id.clone(),
            key,
            hash,
            || async move {
                let id = ctx
                    .store
                    .create_version(
                        none_if_empty(&inner.parent_version_id).as_deref(),
                        parse_json_opt(&inner.metadata_json),
                    )
                    .await
                    .map_err(|e| to_status(&e))?;
                Ok(pb::CreateVersionResponse { version_id: id })
            },
        )
        .await
    }

    async fn propose_claim(
        &self,
        req: Request<pb::ProposeClaimRequest>,
    ) -> Result<Response<pb::ProposeClaimResponse>, Status> {
        let ctx = auth(&self.state, &req).await?;
        ctx.require_rw()?;
        let key = idem_key(&req)?;
        let inner = req.into_inner();
        let hash = request_hash(&inner.encode_to_vec());
        with_idempotency(
            &self.state,
            ctx.tenant_id.clone(),
            key,
            hash,
            || async move {
                let d: dto::ProposeClaimRequest = inner.clone().into();
                let expected = inner.expected_head;
                self.state
                    .ensure_shapes_loaded(&ctx.tenant_id)
                    .await
                    .map_err(|e| to_status(&e))?;
                let chronology = self
                    .state
                    .armed_chronology(ctx.store.as_ref(), &ctx.tenant_id, &inner.version_id)
                    .await
                    .map_err(|e| to_status(&e))?;
                let out = service::append_events(
                    ctx.store.as_ref(),
                    &self.state.shapes,
                    &ctx.tenant_id,
                    &inner.version_id,
                    std::slice::from_ref(&d),
                    None,
                    expected,
                    chronology.as_ref(),
                )
                .await
                .map_err(|e| to_status(&e))?;
                Ok(pb::ProposeClaimResponse {
                    claim: out.claims.into_iter().next().map(|c| convert(c).into()),
                    findings: out
                        .findings
                        .into_iter()
                        .map(|f| convert(f).into())
                        .collect(),
                    head_seq: out.head_seq,
                })
            },
        )
        .await
    }

    async fn append_events(
        &self,
        req: Request<pb::AppendEventsRequest>,
    ) -> Result<Response<pb::AppendEventsResponse>, Status> {
        let ctx = auth(&self.state, &req).await?;
        ctx.require_rw()?;
        let key = idem_key(&req)?;
        let inner = req.into_inner();
        let hash = request_hash(&inner.encode_to_vec());
        with_idempotency(
            &self.state,
            ctx.tenant_id.clone(),
            key,
            hash,
            || async move {
                let claims: Vec<dto::ProposeClaimRequest> =
                    inner.claims.iter().cloned().map(Into::into).collect();
                let expected = inner.expected_head;
                self.state
                    .ensure_shapes_loaded(&ctx.tenant_id)
                    .await
                    .map_err(|e| to_status(&e))?;
                let chronology = self
                    .state
                    .armed_chronology(ctx.store.as_ref(), &ctx.tenant_id, &inner.version_id)
                    .await
                    .map_err(|e| to_status(&e))?;
                let out = service::append_events(
                    ctx.store.as_ref(),
                    &self.state.shapes,
                    &ctx.tenant_id,
                    &inner.version_id,
                    &claims,
                    none_if_empty(&inner.candidate_text).as_deref(),
                    expected,
                    chronology.as_ref(),
                )
                .await
                .map_err(|e| to_status(&e))?;
                Ok(pb::AppendEventsResponse {
                    claims: out.claims.into_iter().map(|c| convert(c).into()).collect(),
                    findings: out
                        .findings
                        .into_iter()
                        .map(|f| convert(f).into())
                        .collect(),
                    head_seq: out.head_seq,
                })
            },
        )
        .await
    }

    async fn open_promise(
        &self,
        req: Request<pb::OpenPromiseRequest>,
    ) -> Result<Response<pb::OpenPromiseResponse>, Status> {
        let ctx = auth(&self.state, &req).await?;
        ctx.require_rw()?;
        let key = idem_key(&req)?;
        let inner = req.into_inner();
        let hash = request_hash(&inner.encode_to_vec());
        with_idempotency(
            &self.state,
            ctx.tenant_id.clone(),
            key,
            hash,
            || async move {
                let p = ctx
                    .store
                    .register_promise(
                        &inner.version_id,
                        &inner.key,
                        &inner.kind,
                        &inner.description,
                        none_if_empty(&inner.origin_scope).as_deref(),
                        none_if_empty(&inner.due_scope).as_deref(),
                    )
                    .await
                    .map_err(|e| to_status(&e))?;
                Ok(pb::OpenPromiseResponse {
                    promise: Some(convert(p).into()),
                })
            },
        )
        .await
    }

    async fn fulfill_promise(
        &self,
        req: Request<pb::FulfillPromiseRequest>,
    ) -> Result<Response<pb::FulfillPromiseResponse>, Status> {
        let ctx = auth(&self.state, &req).await?;
        ctx.require_rw()?;
        let key = idem_key(&req)?;
        let inner = req.into_inner();
        let hash = request_hash(&inner.encode_to_vec());
        with_idempotency(
            &self.state,
            ctx.tenant_id.clone(),
            key,
            hash,
            || async move {
                let fulfilled = ctx
                    .store
                    .fulfill_promise(&inner.version_id, &inner.key)
                    .await
                    .map_err(|e| to_status(&e))?;
                Ok(pb::FulfillPromiseResponse { fulfilled })
            },
        )
        .await
    }

    async fn record_counts(
        &self,
        req: Request<pb::RecordCountsRequest>,
    ) -> Result<Response<pb::RecordCountsResponse>, Status> {
        let ctx = auth(&self.state, &req).await?;
        ctx.require_rw()?;
        let key = idem_key(&req)?;
        let inner = req.into_inner();
        let hash = request_hash(&inner.encode_to_vec());
        with_idempotency(
            &self.state,
            ctx.tenant_id.clone(),
            key,
            hash,
            || async move {
                ctx.store
                    .record_counts(
                        &inner.version_id,
                        &inner.key,
                        &inner.scope_path,
                        inner.count,
                        if inner.budget > 0 {
                            Some(inner.budget)
                        } else {
                            None
                        },
                    )
                    .await
                    .map_err(|e| to_status(&e))?;
                Ok(pb::RecordCountsResponse {})
            },
        )
        .await
    }

    async fn upsert_digest(
        &self,
        req: Request<pb::UpsertDigestRequest>,
    ) -> Result<Response<pb::UpsertDigestResponse>, Status> {
        let ctx = auth(&self.state, &req).await?;
        ctx.require_rw()?;
        let key = idem_key(&req)?;
        let inner = req.into_inner();
        let hash = request_hash(&inner.encode_to_vec());
        with_idempotency(
            &self.state,
            ctx.tenant_id.clone(),
            key,
            hash,
            || async move {
                let d = inner
                    .digest
                    .ok_or_else(|| Status::invalid_argument("digest is required"))?;
                ctx.store
                    .upsert_digest(&dto::DigestDto::from(d).convert())
                    .await
                    .map_err(|e| to_status(&e))?;
                Ok(pb::UpsertDigestResponse {})
            },
        )
        .await
    }

    async fn lock_anchor(
        &self,
        req: Request<pb::LockAnchorRequest>,
    ) -> Result<Response<pb::LockAnchorResponse>, Status> {
        let ctx = auth(&self.state, &req).await?;
        ctx.require_rw()?;
        let key = idem_key(&req)?;
        let inner = req.into_inner();
        let hash = request_hash(&inner.encode_to_vec());
        with_idempotency(
            &self.state,
            ctx.tenant_id.clone(),
            key,
            hash,
            || async move {
                let a = ctx
                    .store
                    .lock_anchor(
                        &inner.version_id,
                        &inner.subject,
                        &inner.key,
                        &inner.value,
                        none_if_empty(&inner.scope_path).as_deref(),
                        parse_json_opt(&inner.evidence_json),
                    )
                    .await
                    .map_err(|e| to_status(&e))?;
                Ok(pb::LockAnchorResponse {
                    anchor: Some(convert(a).into()),
                })
            },
        )
        .await
    }
}

// -- QueryService --------------------------------------------------------

pub struct QuerySvc {
    pub state: Arc<AppState>,
}

#[tonic::async_trait]
impl pb::query_service_server::QueryService for QuerySvc {
    async fn get_head(
        &self,
        req: Request<pb::GetHeadRequest>,
    ) -> Result<Response<pb::GetHeadResponse>, Status> {
        let ctx = auth(&self.state, &req).await?;
        let inner = req.into_inner();
        let head = ctx
            .store
            .head(&inner.version_id)
            .await
            .map_err(|e| to_status(&e))?;
        Ok(Response::new(pb::GetHeadResponse { head_seq: head }))
    }

    async fn get_claim(
        &self,
        req: Request<pb::GetClaimRequest>,
    ) -> Result<Response<pb::GetClaimResponse>, Status> {
        let ctx = auth(&self.state, &req).await?;
        let inner = req.into_inner();
        let resp = service::get_claim(ctx.store.as_ref(), &inner.claim_id)
            .await
            .map_err(|e| to_status(&e))?;
        Ok(Response::new(pb::GetClaimResponse {
            claim: Some(resp.claim.into()),
            superseded: resp.superseded,
            superseded_by: resp.superseded_by.unwrap_or_default(),
        }))
    }

    async fn slice_facts(
        &self,
        req: Request<pb::SliceFactsRequest>,
    ) -> Result<Response<pb::SliceFactsResponse>, Status> {
        let ctx = auth(&self.state, &req).await?;
        let inner = req.into_inner();
        let statuses: Vec<ClaimStatus> = inner
            .statuses
            .iter()
            .filter_map(|s| pb::ClaimStatus::try_from(*s).ok())
            .filter(|s| *s != pb::ClaimStatus::Unspecified)
            .map(|s| dto::ClaimStatusDto::from(s).convert())
            .collect();
        let as_of = if inner.as_of_seq > 0 {
            Some(inner.as_of_seq)
        } else {
            None
        };
        let facts = ctx
            .store
            .slice_facts(
                &inner.version_id,
                &munarium_core::ledger::FactQuery {
                    scope_prefix: none_if_empty(&inner.scope_prefix),
                    as_of_seq: as_of,
                    statuses,
                    limit: if inner.limit > 0 {
                        Some(inner.limit as usize)
                    } else {
                        None
                    },
                },
            )
            .await
            .map_err(|e| to_status(&e))?;
        let head = ctx
            .store
            .head(&inner.version_id)
            .await
            .map_err(|e| to_status(&e))?;
        Ok(Response::new(pb::SliceFactsResponse {
            slice: Some(pb::FactSlice {
                facts: facts.into_iter().map(|c| convert(c).into()).collect(),
                as_of_seq: inner.as_of_seq,
                head_seq: head,
            }),
        }))
    }

    async fn get_lineage(
        &self,
        req: Request<pb::GetLineageRequest>,
    ) -> Result<Response<pb::GetLineageResponse>, Status> {
        let ctx = auth(&self.state, &req).await?;
        let inner = req.into_inner();
        let ids = ctx
            .store
            .lineage(&inner.version_id)
            .await
            .map_err(|e| to_status(&e))?;
        Ok(Response::new(pb::GetLineageResponse {
            lineage: Some(pb::Lineage { version_ids: ids }),
        }))
    }

    async fn list_anchors(
        &self,
        req: Request<pb::ListAnchorsRequest>,
    ) -> Result<Response<pb::ListAnchorsResponse>, Status> {
        let ctx = auth(&self.state, &req).await?;
        let inner = req.into_inner();
        let as_of = if inner.as_of_seq > 0 {
            Some(inner.as_of_seq)
        } else {
            None
        };
        let anchors = ctx
            .store
            .anchors(&inner.version_id, as_of)
            .await
            .map_err(|e| to_status(&e))?;
        Ok(Response::new(pb::ListAnchorsResponse {
            anchors: anchors.into_values().map(|a| convert(a).into()).collect(),
        }))
    }

    async fn list_promises(
        &self,
        req: Request<pb::ListPromisesRequest>,
    ) -> Result<Response<pb::ListPromisesResponse>, Status> {
        let ctx = auth(&self.state, &req).await?;
        let inner = req.into_inner();
        let as_of = if inner.as_of_seq > 0 {
            Some(inner.as_of_seq)
        } else {
            None
        };
        let mut promises = ctx
            .store
            .promises(&inner.version_id, as_of)
            .await
            .map_err(|e| to_status(&e))?;
        if !inner.status.is_empty() {
            promises.retain(|p| {
                matches!(
                    (inner.status.as_str(), p.status),
                    ("open", PromiseStatus::Open)
                        | ("fulfilled", PromiseStatus::Fulfilled)
                        | ("expired", PromiseStatus::Expired)
                        | ("violated", PromiseStatus::Violated)
                )
            });
        }
        Ok(Response::new(pb::ListPromisesResponse {
            promises: promises.into_iter().map(|p| convert(p).into()).collect(),
        }))
    }

    async fn counter_totals(
        &self,
        req: Request<pb::CounterTotalsRequest>,
    ) -> Result<Response<pb::CounterTotalsResponse>, Status> {
        let ctx = auth(&self.state, &req).await?;
        let inner = req.into_inner();
        let as_of = if inner.as_of_seq > 0 {
            Some(inner.as_of_seq)
        } else {
            None
        };
        let counters = ctx
            .store
            .counter_totals(&inner.version_id, as_of)
            .await
            .map_err(|e| to_status(&e))?;
        Ok(Response::new(pb::CounterTotalsResponse {
            counters: counters.into_iter().map(|c| convert(c).into()).collect(),
        }))
    }

    async fn list_digests(
        &self,
        req: Request<pb::ListDigestsRequest>,
    ) -> Result<Response<pb::ListDigestsResponse>, Status> {
        let ctx = auth(&self.state, &req).await?;
        let inner = req.into_inner();
        let digests = ctx
            .store
            .digests(&inner.version_id)
            .await
            .map_err(|e| to_status(&e))?;
        Ok(Response::new(pb::ListDigestsResponse {
            digests: digests.into_iter().map(|d| convert(d).into()).collect(),
        }))
    }

    async fn compose_context(
        &self,
        req: Request<pb::ComposeContextRequest>,
    ) -> Result<Response<pb::ComposeContextResponse>, Status> {
        let ctx = auth(&self.state, &req).await?;
        let inner = req.into_inner();
        let as_of = if inner.as_of_seq > 0 {
            Some(inner.as_of_seq)
        } else {
            None
        };
        let d = service::compose_context(
            ctx.store.as_ref(),
            &inner.version_id,
            none_if_empty(&inner.scope).as_deref(),
            if inner.budget_tokens > 0 {
                Some(inner.budget_tokens)
            } else {
                None
            },
            if inner.fact_limit > 0 {
                Some(inner.fact_limit as usize)
            } else {
                None
            },
            as_of,
            none_if_empty(&inner.as_of_date).as_deref(),
        )
        .await
        .map_err(|e| to_status(&e))?;
        Ok(Response::new(pb::ComposeContextResponse {
            context: Some(d.into()),
        }))
    }
}
