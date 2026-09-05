// SPDX-License-Identifier: Apache-2.0
//! gRPC services added at the milestone: Ingest (client-streaming PutSource,
//! RecordIngest), Retrieval (HybridSearch, GetIndexVersion, BuildIndex via
//! REST-only for now), and the shape half of RunbookService (the executor
//! RPCs answer UNIMPLEMENTED until the milestone — never fake).

use crate::error::to_status;
use crate::state::AppState;
use munarium_api_conv::Convert;
use munarium_proto::mmp::v1 as pb;
use std::sync::Arc;
use tonic::{Request, Response, Status, Streaming};

async fn auth_ctx<T>(state: &AppState, req: &Request<T>) -> Result<crate::grpc::Ctx, Status> {
    crate::grpc::authenticate(state, req).await
}

// -- IngestService --------------------------------------------------------

pub struct IngestSvc {
    pub state: Arc<AppState>,
}

#[tonic::async_trait]
impl pb::ingest_service_server::IngestService for IngestSvc {
    async fn put_source(
        &self,
        req: Request<Streaming<pb::PutSourceRequest>>,
    ) -> Result<Response<pb::PutSourceResponse>, Status> {
        let bearer = req
            .metadata()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(String::from);
        let ctx = crate::grpc::authenticate_token(&self.state, bearer.as_deref()).await?;
        ctx.require_rw_pub()?;
        let retrieval = self
            .state
            .retrieval_for(&ctx.tenant_id)
            .map_err(|e| to_status(&e))?;

        let mut stream = req.into_inner();
        let mut header: Option<pb::SourceHeader> = None;
        let mut bytes: Vec<u8> = Vec::new();
        while let Some(msg) = stream.message().await? {
            match msg.msg {
                Some(pb::put_source_request::Msg::Header(h)) => header = Some(h),
                Some(pb::put_source_request::Msg::Chunk(c)) => {
                    // tonic's `max_decoding_message_size` bounds one MESSAGE;
                    // a client stream can send any number of them, so the
                    // total is bounded here to the same ceiling the REST twin
                    // (`PUT /v1/sources`) enforces, or an rw token could grow
                    // this buffer without limit.
                    if bytes.len().saturating_add(c.len()) > crate::rest::MAX_SOURCE_BYTES {
                        return Err(Status::resource_exhausted(format!(
                            "source exceeds the {} byte limit",
                            crate::rest::MAX_SOURCE_BYTES
                        )));
                    }
                    bytes.extend_from_slice(&c)
                }
                None => {}
            }
        }
        let header =
            header.ok_or_else(|| Status::invalid_argument("first message must be the header"))?;
        // filename is the source's IDENTITY and its object-store path, so it
        // is required: a source with no path can never match a runbook's
        // filenamePrefix binding.
        if header.filename.trim().is_empty() {
            return Err(Status::invalid_argument(
                "SourceHeader.filename is required: it is the source's identity and storage path",
            ));
        }
        let (source_id, content_hash, already_existed) = retrieval
            .put_source(
                &header.declared_sha256,
                if header.media_type.is_empty() {
                    "application/octet-stream"
                } else {
                    &header.media_type
                },
                header.filename.trim(),
                if header.shape_ref.is_empty() {
                    None
                } else {
                    Some(&header.shape_ref)
                },
                &bytes,
            )
            .await
            .map_err(|e| to_status(&e))?;
        Ok(Response::new(pb::PutSourceResponse {
            source_id,
            content_hash,
            bytes_len: bytes.len() as u64,
            already_existed,
        }))
    }

    async fn record_ingest(
        &self,
        req: Request<pb::RecordIngestRequest>,
    ) -> Result<Response<pb::RecordIngestResponse>, Status> {
        let ctx = auth_ctx(&self.state, &req).await?;
        ctx.require_rw_pub()?;
        let inner = req.into_inner();
        crate::rest::validate_content_hash(&inner.content_hash).map_err(|e| to_status(&e))?;
        let mut claim = munarium_core::storage::NewClaim::fact(
            &format!(
                "source-{}",
                &inner.content_hash[..12.min(inner.content_hash.len())]
            ),
            "ingested",
            if inner.shape_ref.is_empty() {
                "unbound"
            } else {
                &inner.shape_ref
            },
        );
        claim.evidence = Some(serde_json::json!({ "content_hash": inner.content_hash }));
        let stored = ctx
            .store
            .append_claim(&inner.version_id, claim, None)
            .await
            .map_err(|e| to_status(&e))?;
        Ok(Response::new(pb::RecordIngestResponse {
            event_id: stored.id,
            seq: stored.seq,
        }))
    }

    /// REST twin: the batch ingest plane. Data-plane auth
    /// (ingest scope + munarium-uid metadata), per-item outcomes, same op as
    /// POST /v1/ingest/batch. Bytes arrive native on this plane and are
    /// re-encoded for the shared op (whose contract is base64).
    async fn ingest_files(
        &self,
        req: Request<pb::IngestFilesRequest>,
    ) -> Result<Response<pb::IngestFilesResponse>, Status> {
        use base64::Engine as _;
        let access = crate::grpc_platform::data_plane_access(
            &self.state,
            req.metadata(),
            munarium_access::SCOPE_INGEST,
        )
        .await?;
        let inner = req.into_inner();
        let files: Vec<munarium_api_types::IngestFileRequest> = inner
            .files
            .into_iter()
            .map(|f| munarium_api_types::IngestFileRequest {
                filename: f.filename,
                media_type: f.media_type,
                content_base64: base64::engine::general_purpose::STANDARD.encode(&f.content),
                sha256: crate::grpc::none_if_empty(&f.sha256),
                collections: (!f.collections.is_empty()).then_some(f.collections),
            })
            .collect();
        let results = crate::ingest_api::op_ingest_batch(&self.state, &access, &files)
            .await
            .map_err(|e| e.into_status())?;
        Ok(Response::new(pb::IngestFilesResponse {
            results: results
                .into_iter()
                .map(|r| pb::IngestResult {
                    filename: r.filename,
                    source_id: r.source_id.unwrap_or_default(),
                    sha256: r.sha256.unwrap_or_default(),
                    existed: r.existed,
                    bound_to: r.bound_to,
                    error: r.error.unwrap_or_default(),
                })
                .collect(),
        }))
    }
}

// -- RetrievalService --------------------------------------------------------

pub struct RetrievalSvc {
    pub state: Arc<AppState>,
}

#[tonic::async_trait]
impl pb::retrieval_service_server::RetrievalService for RetrievalSvc {
    async fn hybrid_search(
        &self,
        req: Request<pb::HybridSearchRequest>,
    ) -> Result<Response<pb::HybridSearchResponse>, Status> {
        let ctx = auth_ctx(&self.state, &req).await?;
        let inner = req.into_inner();
        if !inner.filter_json.is_empty() {
            // The collection filter ({"collections":[...]}) is REST-first,
            // like index builds and /healthai — it is an access-controlled
            // data-plane read that rides the capability-token planes, which
            // the gRPC services do not expose yet (docs/api/grpc.md transport
            // gap). Until the gRPC data-plane twins land, gRPC rejects every
            // filter rather than serve one without the access check.
            return Err(to_status(&munarium_core::KernelError::InvalidInput(
                "search filter is REST-only today; use POST /v1/search (see docs/api/grpc.md)"
                    .into(),
            )));
        }
        let retrieval = self
            .state
            .retrieval_for(&ctx.tenant_id)
            .map_err(|e| to_status(&e))?;
        let result = retrieval
            .hybrid_search(munarium_core::retrieval::HybridQuery {
                query: inner.query,
                shape_ref: inner.shape_ref,
                top_k: if inner.top_k == 0 {
                    10
                } else {
                    inner.top_k as usize
                },
                filter: None,
                index_version: if inner.index_version.is_empty() {
                    None
                } else {
                    Some(inner.index_version)
                },
            })
            .await
            .map_err(|e| to_status(&e))?;
        let out: munarium_api_types::SearchResponse = result.convert();
        Ok(Response::new(pb::HybridSearchResponse {
            hits: out
                .hits
                .into_iter()
                .map(|h| pb::SearchHit {
                    chunk_id: h.chunk_id,
                    source_id: h.source_id,
                    source_path: h.source_path,
                    source_content_hash: h.source_content_hash,
                    text: h.text,
                    score: h.score,
                    lexical_rank: h.lexical_rank.unwrap_or(0) as f64,
                    vector_rank: h.vector_rank.unwrap_or(0) as f64,
                    metadata_json: h.metadata.map(|m| m.to_string()).unwrap_or_default(),
                })
                .collect(),
            envelope: Some(out.envelope.into()),
        }))
    }

    async fn get_index_version(
        &self,
        req: Request<pb::GetIndexVersionRequest>,
    ) -> Result<Response<pb::GetIndexVersionResponse>, Status> {
        let ctx = auth_ctx(&self.state, &req).await?;
        let retrieval = self
            .state
            .retrieval_for(&ctx.tenant_id)
            .map_err(|e| to_status(&e))?;
        let inner = req.into_inner();
        let iv = retrieval
            .index_version(&inner.shape_ref)
            .await
            .map_err(|e| to_status(&e))?;
        Ok(Response::new(pb::GetIndexVersionResponse {
            index_version: iv.id,
            event_watermark: iv.event_watermark,
            manifest_json: iv.manifest.to_string(),
            active: iv.active,
        }))
    }

    // REST twins: collections CRUD, same ops as /v1/collections.

    async fn create_collection(
        &self,
        req: Request<pb::CreateCollectionRequest>,
    ) -> Result<Response<pb::CollectionInfo>, Status> {
        let ctx = auth_ctx(&self.state, &req).await?;
        ctx.require_rw_pub()?;
        let inner = req.into_inner();
        let dto_req = munarium_api_types::CreateCollectionRequest {
            name: inner.name,
            shape_ref: inner.shape_ref,
            access_level: inner.access_level,
            compartments: inner.compartments,
            description: crate::grpc::none_if_empty(&inner.description),
        };
        let c = crate::collections_api::op_create_collection(&self.state, &ctx.tenant_id, dto_req)
            .await
            .map_err(|e| to_status(&e))?;
        Ok(Response::new(collection_info_pb(c)))
    }

    async fn list_collections(
        &self,
        req: Request<pb::ListCollectionsRequest>,
    ) -> Result<Response<pb::ListCollectionsResponse>, Status> {
        let ctx = auth_ctx(&self.state, &req).await?;
        let collections = crate::collections_api::op_list_collections(&self.state, &ctx.tenant_id)
            .await
            .map_err(|e| to_status(&e))?;
        Ok(Response::new(pb::ListCollectionsResponse {
            collections: collections.into_iter().map(collection_info_pb).collect(),
        }))
    }

    async fn get_collection(
        &self,
        req: Request<pb::GetCollectionRequest>,
    ) -> Result<Response<pb::CollectionInfo>, Status> {
        let ctx = auth_ctx(&self.state, &req).await?;
        let inner = req.into_inner();
        let c = crate::collections_api::op_get_collection(&self.state, &ctx.tenant_id, &inner.id)
            .await
            .map_err(|e| to_status(&e))?;
        Ok(Response::new(collection_info_pb(c)))
    }
}

fn collection_info_pb(c: munarium_api_types::CollectionDto) -> pb::CollectionInfo {
    pb::CollectionInfo {
        id: c.id,
        name: c.name,
        shape_ref: c.shape_ref,
        access_level: c.access_level,
        compartments: c.compartments,
        status: c.status,
        description: c.description.unwrap_or_default(),
        created_at: c.created_at,
        source_count: c.source_count,
        active_index: c.active_index.unwrap_or_default(),
    }
}

// -- RunbookService (shape half; executor arrives at the milestone) --------------------

pub struct RunbookSvc {
    pub state: Arc<AppState>,
}

#[tonic::async_trait]
impl pb::runbook_service_server::RunbookService for RunbookSvc {
    async fn apply_shape(
        &self,
        req: Request<pb::ApplyShapeRequest>,
    ) -> Result<Response<pb::ApplyShapeResponse>, Status> {
        let ctx = auth_ctx(&self.state, &req).await?;
        ctx.require_rw_pub()?;
        let inner = req.into_inner();
        let version_id = crate::grpc::none_if_empty(&inner.version_id);
        let resp = crate::runbooks_api::op_apply_shape(
            &self.state,
            &ctx.tenant_id,
            &inner.yaml,
            version_id.as_deref(),
            Some(ctx.store.clone()),
        )
        .await
        .map_err(|e| to_status(&e))?;
        Ok(Response::new(pb::ApplyShapeResponse {
            shape_ref: resp.shape_ref,
            event_id: resp.event_id.unwrap_or_default(),
        }))
    }

    async fn apply_runbook(
        &self,
        req: Request<pb::ApplyRunbookRequest>,
    ) -> Result<Response<pb::ApplyRunbookResponse>, Status> {
        let ctx = auth_ctx(&self.state, &req).await?;
        ctx.require_rw_pub()?;
        let inner = req.into_inner();
        let runbook_ref =
            crate::runbooks_api::op_apply_runbook(&self.state, &ctx.tenant_id, &inner.yaml)
                .await
                .map_err(|e| to_status(&e))?;
        Ok(Response::new(pb::ApplyRunbookResponse {
            runbook_ref,
            event_id: String::new(),
        }))
    }

    async fn run_runbook(
        &self,
        req: Request<pb::RunRunbookRequest>,
    ) -> Result<Response<pb::RunRunbookResponse>, Status> {
        let ctx = auth_ctx(&self.state, &req).await?;
        ctx.require_rw_pub()?;
        let inner = req.into_inner();
        let version_id = serde_json::from_str::<serde_json::Value>(&inner.params_json)
            .ok()
            .and_then(|v| v["version_id"].as_str().map(String::from));
        let (run_id, state) = crate::runbooks_api::op_run_runbook(
            &self.state,
            &ctx.tenant_id,
            &inner.runbook_ref,
            version_id.as_deref(),
        )
        .await
        .map_err(|e| to_status(&e))?;
        Ok(Response::new(pb::RunRunbookResponse { run_id, state }))
    }

    async fn get_run(
        &self,
        req: Request<pb::GetRunRequest>,
    ) -> Result<Response<pb::GetRunResponse>, Status> {
        let ctx = auth_ctx(&self.state, &req).await?;
        let inner = req.into_inner();
        let v = crate::runbooks_api::op_get_run(&self.state, &ctx.tenant_id, &inner.run_id)
            .await
            .map_err(|e| to_status(&e))?;
        Ok(Response::new(pb::GetRunResponse {
            run_id: v.run_id,
            runbook_ref: v.runbook_ref,
            state: v.state,
            version_id: v.version_id.unwrap_or_default(),
            steps: v
                .steps
                .into_iter()
                .map(|s| pb::RunbookStepState {
                    ordinal: s.ordinal,
                    name: s.name,
                    state: s.state,
                    // Absent detail is the EMPTY string (the wire's
                    // "absent" sentinel), not the literal "null" — otherwise
                    // gRPC decodes Some(Null) where REST gives None.
                    detail_json: s.detail.map(|d| d.to_string()).unwrap_or_default(),
                    updated_at: None,
                })
                .collect(),
        }))
    }

    async fn approve_step(
        &self,
        req: Request<pb::ApproveStepRequest>,
    ) -> Result<Response<pb::ApproveStepResponse>, Status> {
        let ctx = auth_ctx(&self.state, &req).await?;
        ctx.require_rw_pub()?;
        let inner = req.into_inner();
        let state = crate::runbooks_api::op_approve_step(
            &self.state,
            &ctx.tenant_id,
            &inner.run_id,
            inner.step_ordinal as usize,
        )
        .await
        .map_err(|e| to_status(&e))?;
        Ok(Response::new(pb::ApproveStepResponse {
            event_id: String::new(),
            state,
        }))
    }

    // REST twins: runbook management, same ops as /v1/runbooks.

    async fn list_runbooks(
        &self,
        req: Request<pb::ListRunbooksRequest>,
    ) -> Result<Response<pb::ListRunbooksResponse>, Status> {
        let ctx = auth_ctx(&self.state, &req).await?;
        let inner = req.into_inner();
        let runbooks = crate::runbooks_api::op_list_runbooks(
            &self.state,
            &ctx.tenant_id,
            inner.include_removed,
        )
        .await
        .map_err(|e| to_status(&e))?;
        Ok(Response::new(pb::ListRunbooksResponse {
            runbooks: runbooks
                .into_iter()
                .map(|r| pb::RunbookSummary {
                    runbook_ref: r.runbook_ref,
                    name: r.name,
                    version: r.version,
                    status: r.status,
                    min_access_level: r.min_access_level,
                    collections: r
                        .collections
                        .into_iter()
                        .map(runbook_collection_pb)
                        .collect(),
                    created_at: r.created_at,
                })
                .collect(),
        }))
    }

    async fn get_runbook_info(
        &self,
        req: Request<pb::GetRunbookInfoRequest>,
    ) -> Result<Response<pb::GetRunbookInfoResponse>, Status> {
        let ctx = auth_ctx(&self.state, &req).await?;
        let inner = req.into_inner();
        let v = crate::runbooks_api::op_runbook_info(&self.state, &ctx.tenant_id, &inner.name)
            .await
            .map_err(|e| to_status(&e))?;
        Ok(Response::new(pb::GetRunbookInfoResponse {
            runbook_ref: v.runbook_ref,
            name: v.name,
            version: v.version,
            status: v.status,
            collections: v
                .collections
                .into_iter()
                .map(runbook_collection_pb)
                .collect(),
            versions: v.versions,
            models_json: v.models.map(|m| m.to_string()).unwrap_or_default(),
            retrieval_json: v.retrieval.to_string(),
            has_completion: v.has_completion,
            created_at: v.created_at,
        }))
    }

    async fn validate_runbook(
        &self,
        req: Request<pb::ValidateRunbookRequest>,
    ) -> Result<Response<pb::ValidateRunbookResponse>, Status> {
        let ctx = auth_ctx(&self.state, &req).await?;
        ctx.require_rw_pub()?;
        let inner = req.into_inner();
        let override_req = crate::models::ModelOverride {
            provider: crate::grpc::none_if_empty(&inner.provider),
            model: crate::grpc::none_if_empty(&inner.model),
            tier: crate::grpc::none_if_empty(&inner.tier),
        };
        let override_ref = (!override_req.is_empty()).then_some(&override_req);
        let v = crate::runbooks_api::op_validate_runbook(
            &self.state,
            &ctx.tenant_id,
            &inner.yaml,
            inner.suggest,
            override_ref,
        )
        .await
        .map_err(|e| e.into_status())?;
        Ok(Response::new(pb::ValidateRunbookResponse {
            valid: v.valid,
            findings: v
                .findings
                .into_iter()
                .map(|f| pb::ValidationFinding {
                    severity: f.severity,
                    code: f.code,
                    message: f.message,
                    path: f.path,
                })
                .collect(),
            suggestions: v
                .suggestions
                .into_iter()
                .map(|sg| pb::RunbookSuggestion {
                    title: sg.title,
                    rationale: sg.rationale,
                    patch_hint: sg.patch_hint.unwrap_or_default(),
                })
                .collect(),
            suggest_note: v.suggest_note.unwrap_or_default(),
        }))
    }

    async fn request_removal(
        &self,
        req: Request<pb::RequestRemovalRequest>,
    ) -> Result<Response<pb::RequestRemovalResponse>, Status> {
        let ctx = auth_ctx(&self.state, &req).await?;
        ctx.require_rw_pub()?;
        let requested_by = crate::grpc_platform::uid_or_anonymous_md(req.metadata());
        let inner = req.into_inner();
        let v = crate::runbooks_api::op_request_removal(
            &self.state,
            &ctx.tenant_id,
            &inner.runbook_ref,
            &requested_by,
        )
        .await
        .map_err(|e| e.into_status())?;
        Ok(Response::new(pb::RequestRemovalResponse {
            runbook_ref: v.runbook_ref,
            removal_id: v.removal_id,
            expires_at: v.expires_at,
        }))
    }

    async fn confirm_removal(
        &self,
        req: Request<pb::ConfirmRemovalRequest>,
    ) -> Result<Response<pb::ConfirmRemovalResponse>, Status> {
        let ctx = auth_ctx(&self.state, &req).await?;
        ctx.require_rw_pub()?;
        let inner = req.into_inner();
        let v = crate::runbooks_api::op_confirm_removal(
            &self.state,
            &ctx.tenant_id,
            &inner.runbook_ref,
            &inner.removal_id,
        )
        .await
        .map_err(|e| e.into_status())?;
        Ok(Response::new(pb::ConfirmRemovalResponse {
            runbook_ref: v.runbook_ref,
            status: v.status,
        }))
    }
}

fn runbook_collection_pb(c: munarium_api_types::RunbookCollectionDto) -> pb::RunbookCollectionInfo {
    pb::RunbookCollectionInfo {
        name: c.name,
        collection_id: c.collection_id.unwrap_or_default(),
        shape_ref: c.shape_ref,
        access_level: c.access_level,
        compartments: c.compartments,
        active_index: c.active_index.unwrap_or_default(),
        source_count: c.source_count,
    }
}
