// SPDX-License-Identifier: Apache-2.0
//! gRPC transport: tonic over the direct :50051 plane (or :443 via the
//! gateway). Errors decode the `google.rpc.ErrorInfo` structured detail;
//! commands carry auto-generated idempotency-key metadata and are re-sent
//! with the SAME key only when the failure provably shed the request before
//! execution (`is_command_retry_safe`) — an UNAVAILABLE or deadline expiry
//! on an established HTTP/2 stream cannot be distinguished from a call the
//! server is still running, and the server records an idempotency key only
//! AFTER a command completes. Plaintext is used exactly when the endpoint
//! scheme is `http://`.
//!
//! Transport notes (documented parity gaps, not bugs):
//! - `build_index` has no gRPC RPC — returns `Unsupported`.
//! - `health_ai` has no gRPC RPC — returns `Unsupported` (GET /healthai).
//! - The sealed evidence plane is REST-only in v1 and returns
//!   `Unsupported` here.
//! - The REST-only platform surface returns `Unsupported` here: `turn_stream`
//!   (SSE), the four bulk-upload routes, `get_source`, `findings`,
//!   chronology-rules, `providers.list`, the per-call token budgets
//!   (`providers.max_tokens` / `replace_max_tokens`), every reports method (the
//!   AdminService.Usage RPC is declared but UNIMPLEMENTED — not wired), the
//!   whole authoring plane, and `server_version`.
//! - proto3 scalars cannot carry "explicitly zero": `as_of_seq`/`limit`/
//!   `top_k`/`fact_limit` of `Some(0)`, a counter `budget` of `Some(0)`,
//!   a `ttl_secs` of `Some(0)`, and a `confidence`/`temperature` of
//!   `Some(0.0)` are rejected as `InvalidInput` here instead of silently
//!   meaning "absent" (REST carries them faithfully).
use crate::error::{from_status, MunariumError, Result};
use crate::planes::*;
use crate::{new_idem_key, MunariumClientOptions};
use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::StreamExt;
use munarium_api_types as dto;
use munarium_proto::mmp::v1 as pb;
use sha2::Digest as _;
use std::time::Duration;
use tonic::metadata::AsciiMetadataValue;
use tonic::transport::Channel;

pub struct GrpcTransport {
    channel: Channel,
    /// Pre-parsed `Bearer <token>` — validated once, cheap clone per call.
    auth: Option<AsciiMetadataValue>,
    /// Pre-parsed `munarium-uid` value (the uid contract).
    uid: Option<AsciiMetadataValue>,
    request_timeout: Duration,
    retries: u32,
}

/// A `Some(0)` that proto3 cannot distinguish from "absent".
fn reject_zero(name: &str, v: Option<u64>) -> Result<()> {
    if v == Some(0) {
        return Err(MunariumError::InvalidInput {
            detail: format!(
                "{name} = 0 cannot be represented on the gRPC wire (proto3 uses 0 for \
                 'absent'); omit it, or use the REST transport"
            ),
        });
    }
    Ok(())
}

impl GrpcTransport {
    pub async fn connect(options: MunariumClientOptions) -> Result<Self> {
        let url = options.endpoint.clone();
        let mut endpoint = Channel::from_shared(url.clone())
            .map_err(|e| MunariumError::InvalidInput {
                detail: format!("bad gRPC endpoint '{url}': {e}"),
            })?
            .connect_timeout(options.connect_timeout);
        if url.starts_with("https://") {
            endpoint = endpoint
                .tls_config(tonic::transport::ClientTlsConfig::new().with_native_roots())
                .map_err(|e| MunariumError::Transport {
                    detail: format!("tls config: {e}"),
                    may_have_reached_server: false,
                })?;
        }
        let channel = endpoint
            .connect()
            .await
            .map_err(|e| MunariumError::Transport {
                detail: format!("connect {url}: {e}"),
                may_have_reached_server: false,
            })?;
        let auth = match &options.token {
            Some(token) => Some(format!("Bearer {token}").parse().map_err(|_| {
                MunariumError::InvalidInput {
                    detail: "token contains non-metadata-safe characters".into(),
                }
            })?),
            None => None,
        };
        let uid = match &options.uid {
            Some(uid) => Some(uid.parse().map_err(|_| MunariumError::InvalidInput {
                detail: "uid contains non-metadata-safe characters".into(),
            })?),
            None => None,
        };
        Ok(Self {
            channel,
            auth,
            uid,
            request_timeout: options.request_timeout,
            retries: options.read_retries,
        })
    }

    /// Attach auth + uid metadata — used by every unary call and, directly,
    /// by the streaming PutSource so the uid contract covers it too.
    fn apply_auth<M>(&self, req: &mut tonic::Request<M>) {
        if let Some(auth) = &self.auth {
            req.metadata_mut().insert("authorization", auth.clone());
        }
        if let Some(uid) = &self.uid {
            req.metadata_mut().insert("munarium-uid", uid.clone());
        }
    }

    fn request<M>(&self, msg: M, idem: Option<&str>) -> tonic::Request<M> {
        let mut req = tonic::Request::new(msg);
        req.set_timeout(self.request_timeout);
        self.apply_auth(&mut req);
        if let Some(key) = idem {
            // Infallible by construction: every key reaching here came
            // through resolve_idem, which rejects non-metadata-safe keys
            // with a typed InvalidInput instead of silently dropping them.
            if let Ok(v) = key.parse() {
                req.metadata_mut().insert("idempotency-key", v);
            }
        }
        req
    }

    fn commands(&self) -> pb::command_service_client::CommandServiceClient<Channel> {
        pb::command_service_client::CommandServiceClient::new(self.channel.clone())
    }
    fn queries(&self) -> pb::query_service_client::QueryServiceClient<Channel> {
        pb::query_service_client::QueryServiceClient::new(self.channel.clone())
    }
    fn ingest_svc(&self) -> pb::ingest_service_client::IngestServiceClient<Channel> {
        pb::ingest_service_client::IngestServiceClient::new(self.channel.clone())
    }
    fn retrieval_svc(&self) -> pb::retrieval_service_client::RetrievalServiceClient<Channel> {
        pb::retrieval_service_client::RetrievalServiceClient::new(self.channel.clone())
    }
    fn runbook_svc(&self) -> pb::runbook_service_client::RunbookServiceClient<Channel> {
        pb::runbook_service_client::RunbookServiceClient::new(self.channel.clone())
    }
    fn provider_svc(&self) -> pb::provider_service_client::ProviderServiceClient<Channel> {
        pb::provider_service_client::ProviderServiceClient::new(self.channel.clone())
    }
    fn session_svc(&self) -> pb::session_service_client::SessionServiceClient<Channel> {
        pb::session_service_client::SessionServiceClient::new(self.channel.clone())
    }
    fn admin_svc(&self) -> pb::admin_service_client::AdminServiceClient<Channel> {
        pb::admin_service_client::AdminServiceClient::new(self.channel.clone())
    }

    /// The ONE retry loop, parameterized by the class predicate — one
    /// mechanism, two policies, so backoff/decoding fixes cannot diverge.
    async fn rpc_with<T, F, Fut>(
        &self,
        retryable: fn(&MunariumError) -> bool,
        mut call: F,
    ) -> Result<T>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = std::result::Result<T, tonic::Status>>,
    {
        let mut attempt = 0;
        loop {
            attempt += 1;
            match call().await {
                Ok(v) => return Ok(v),
                Err(status) => {
                    let err = from_status(status);
                    if retryable(&err) && attempt <= self.retries {
                        crate::retry::jitter_sleep(attempt).await;
                        continue;
                    }
                    return Err(err);
                }
            }
        }
    }

    /// Read path: retry transient failures (UNAVAILABLE / deadline / shed)
    /// with backoff — reads are safe to repeat unconditionally.
    async fn rpc_retry<T, F, Fut>(&self, call: F) -> Result<T>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = std::result::Result<T, tonic::Status>>,
    {
        self.rpc_with(MunariumError::is_transient, call).await
    }

    /// Command path: closures re-send the SAME idempotency key, and ONLY for
    /// failures that provably shed the request before execution
    /// (`is_command_retry_safe` — the typed `overloaded`). The server
    /// records an idempotency key AFTER a command completes, so a retry that
    /// overtakes an in-flight attempt would execute it twice; on gRPC no
    /// transport failure is provably undelivered (a failed lazy reconnect
    /// and a broken established stream both surface as UNAVAILABLE), so —
    /// matching the Python and .NET clients — transport failures surface to
    /// the caller here. (C7 fix — this path previously retried ANY
    /// transient with the same key, the premise C5 refuted.)
    async fn rpc_command<T, F, Fut>(&self, call: F) -> Result<T>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = std::result::Result<T, tonic::Status>>,
    {
        self.rpc_with(MunariumError::is_command_retry_safe, call)
            .await
    }
}

// ---------------------------------------------------------------------------
// conversions — the ONE shared boundary mapping (munarium_api_types::wire)
// ---------------------------------------------------------------------------

use munarium_api_types::wire::{none_if_empty as opt_str, propose_request_pb};

fn opt(s: String) -> Option<String> {
    opt_str(&s)
}

fn json_opt(s: &str) -> Option<serde_json::Value> {
    if s.is_empty() {
        None
    } else {
        serde_json::from_str(s).ok()
    }
}

/// Reject an explicitly-zero confidence before it hits the wire (proto3
/// cannot distinguish it from absent), then delegate to the shared mapper.
fn propose_pb(version_id: &str, r: dto::ProposeClaimRequest) -> Result<pb::ProposeClaimRequest> {
    if r.confidence == Some(0.0) {
        return Err(MunariumError::InvalidInput {
            detail: "confidence = 0.0 cannot be represented on the gRPC wire (proto3 uses \
                     0.0 for 'absent'); omit it, or use the REST transport"
                .into(),
        });
    }
    Ok(propose_request_pb(version_id, r))
}

// ---------------------------------------------------------------------------
// commands
// ---------------------------------------------------------------------------

#[async_trait]
impl CommandsPlane for GrpcTransport {
    async fn create_version(
        &self,
        req: dto::CreateVersionRequest,
        idem: IdemKey,
    ) -> Result<dto::CreateVersionResponse> {
        let key = resolve_idem(idem)?;
        let msg = pb::CreateVersionRequest {
            parent_version_id: req.parent_version_id.unwrap_or_default(),
            metadata_json: req.metadata.map(|v| v.to_string()).unwrap_or_default(),
        };
        let resp = self
            .rpc_command(|| {
                let mut client = self.commands();
                let req = self.request(msg.clone(), Some(&key));
                async move { client.create_version(req).await }
            })
            .await?
            .into_inner();
        Ok(dto::CreateVersionResponse {
            version_id: resp.version_id,
        })
    }

    async fn propose_claim(
        &self,
        version_id: &str,
        req: dto::ProposeClaimRequest,
        idem: IdemKey,
    ) -> Result<dto::ProposeClaimResponse> {
        let key = resolve_idem(idem)?;
        let msg = propose_pb(version_id, req)?;
        let resp = self
            .rpc_command(|| {
                let mut client = self.commands();
                let req = self.request(msg.clone(), Some(&key));
                async move { client.propose_claim(req).await }
            })
            .await?
            .into_inner();
        let claim = resp.claim.ok_or_else(|| MunariumError::Unexpected {
            status: None,
            detail: "ProposeClaimResponse without claim".into(),
        })?;
        Ok(dto::ProposeClaimResponse {
            claim: claim.into(),
            findings: resp.findings.into_iter().map(Into::into).collect(),
            head_seq: resp.head_seq,
        })
    }

    async fn append_events(
        &self,
        version_id: &str,
        req: dto::AppendEventsRequest,
        idem: IdemKey,
    ) -> Result<dto::AppendEventsResponse> {
        let key = resolve_idem(idem)?;
        let msg = pb::AppendEventsRequest {
            version_id: version_id.to_string(),
            expected_head: req.expected_head,
            claims: req
                .claims
                .into_iter()
                .map(|c| propose_pb(version_id, c))
                .collect::<Result<Vec<_>>>()?,
            candidate_text: req.candidate_text.unwrap_or_default(),
        };
        let resp = self
            .rpc_command(|| {
                let mut client = self.commands();
                let req = self.request(msg.clone(), Some(&key));
                async move { client.append_events(req).await }
            })
            .await?
            .into_inner();
        Ok(dto::AppendEventsResponse {
            claims: resp.claims.into_iter().map(Into::into).collect(),
            findings: resp.findings.into_iter().map(Into::into).collect(),
            head_seq: resp.head_seq,
        })
    }

    async fn open_promise(
        &self,
        version_id: &str,
        req: dto::OpenPromiseRequest,
        idem: IdemKey,
    ) -> Result<dto::PromiseDto> {
        let key = resolve_idem(idem)?;
        let msg = pb::OpenPromiseRequest {
            version_id: version_id.to_string(),
            key: req.key,
            kind: req.kind,
            description: req.description,
            origin_scope: req.origin_scope.unwrap_or_default(),
            due_scope: req.due_scope.unwrap_or_default(),
        };
        let resp = self
            .rpc_command(|| {
                let mut client = self.commands();
                let req = self.request(msg.clone(), Some(&key));
                async move { client.open_promise(req).await }
            })
            .await?
            .into_inner();
        let p = resp.promise.ok_or_else(|| MunariumError::Unexpected {
            status: None,
            detail: "OpenPromiseResponse without promise".into(),
        })?;
        Ok(p.into())
    }

    async fn fulfill_promise(
        &self,
        version_id: &str,
        promise_key: &str,
        idem: IdemKey,
    ) -> Result<dto::FulfillPromiseResponse> {
        let key = resolve_idem(idem)?;
        let msg = pb::FulfillPromiseRequest {
            version_id: version_id.to_string(),
            key: promise_key.to_string(),
            result_ref: String::new(),
        };
        let resp = self
            .rpc_command(|| {
                let mut client = self.commands();
                let req = self.request(msg.clone(), Some(&key));
                async move { client.fulfill_promise(req).await }
            })
            .await?
            .into_inner();
        Ok(dto::FulfillPromiseResponse {
            fulfilled: resp.fulfilled,
        })
    }

    async fn lock_anchor(
        &self,
        version_id: &str,
        req: dto::LockAnchorRequest,
        idem: IdemKey,
    ) -> Result<dto::AnchorDto> {
        let key = resolve_idem(idem)?;
        let msg = pb::LockAnchorRequest {
            version_id: version_id.to_string(),
            subject: req.subject,
            key: req.key,
            value: req.value,
            scope_path: req.scope_path.unwrap_or_default(),
            evidence_json: req.evidence.map(|v| v.to_string()).unwrap_or_default(),
        };
        let resp = self
            .rpc_command(|| {
                let mut client = self.commands();
                let req = self.request(msg.clone(), Some(&key));
                async move { client.lock_anchor(req).await }
            })
            .await?
            .into_inner();
        let a = resp.anchor.ok_or_else(|| MunariumError::Unexpected {
            status: None,
            detail: "LockAnchorResponse without anchor".into(),
        })?;
        Ok(a.into())
    }

    async fn record_counts(
        &self,
        version_id: &str,
        req: dto::RecordCountsRequest,
        idem: IdemKey,
    ) -> Result<()> {
        reject_zero("budget", req.budget)?;
        let key = resolve_idem(idem)?;
        let msg = pb::RecordCountsRequest {
            version_id: version_id.to_string(),
            key: req.key,
            scope_path: req.scope_path,
            count: req.count,
            budget: req.budget.unwrap_or(0),
        };
        self.rpc_command(|| {
            let mut client = self.commands();
            let req = self.request(msg.clone(), Some(&key));
            async move { client.record_counts(req).await }
        })
        .await?;
        Ok(())
    }

    async fn upsert_digest(&self, digest: dto::DigestDto) -> Result<()> {
        // gRPC UpsertDigest is a command RPC: idempotency-key metadata is
        // required (unlike the REST PUT, which is exempt by design).
        let key = new_idem_key();
        let msg = pb::UpsertDigestRequest {
            digest: Some(digest.into()),
        };
        self.rpc_command(|| {
            let mut client = self.commands();
            let req = self.request(msg.clone(), Some(&key));
            async move { client.upsert_digest(req).await }
        })
        .await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// query
// ---------------------------------------------------------------------------

#[async_trait]
impl QueryPlane for GrpcTransport {
    async fn head(&self, version_id: &str) -> Result<u64> {
        let resp = self
            .rpc_retry(|| {
                let mut client = self.queries();
                let req = self.request(
                    pb::GetHeadRequest {
                        version_id: version_id.to_string(),
                    },
                    None,
                );
                async move { client.get_head(req).await }
            })
            .await?;
        Ok(resp.into_inner().head_seq)
    }

    async fn get_claim(&self, claim_id: &str) -> Result<dto::GetClaimResponse> {
        let resp = self
            .rpc_retry(|| {
                let mut client = self.queries();
                let req = self.request(
                    pb::GetClaimRequest {
                        claim_id: claim_id.to_string(),
                    },
                    None,
                );
                async move { client.get_claim(req).await }
            })
            .await?
            .into_inner();
        let claim = resp.claim.ok_or_else(|| MunariumError::Unexpected {
            status: None,
            detail: "GetClaimResponse without claim".into(),
        })?;
        Ok(dto::GetClaimResponse {
            claim: claim.into(),
            superseded: resp.superseded,
            superseded_by: opt(resp.superseded_by),
        })
    }

    async fn facts(&self, version_id: &str, q: FactsQuery) -> Result<dto::FactsResponse> {
        reject_zero("as_of_seq", q.as_of_seq)?;
        reject_zero("limit", q.limit.map(|v| v as u64))?;
        let statuses: Vec<i32> = q
            .statuses
            .iter()
            .map(|s| pb::ClaimStatus::from(*s) as i32)
            .collect();
        let msg = pb::SliceFactsRequest {
            version_id: version_id.to_string(),
            scope_prefix: q.scope_prefix.unwrap_or_default(),
            as_of_seq: q.as_of_seq.unwrap_or(0),
            statuses,
            limit: to_u32("limit", q.limit)?,
        };
        let resp = self
            .rpc_retry(|| {
                let mut client = self.queries();
                let req = self.request(msg.clone(), None);
                async move { client.slice_facts(req).await }
            })
            .await?
            .into_inner();
        let slice = resp.slice.ok_or_else(|| MunariumError::Unexpected {
            status: None,
            detail: "SliceFactsResponse without slice".into(),
        })?;
        Ok(dto::FactsResponse {
            facts: slice.facts.into_iter().map(Into::into).collect(),
            as_of_seq: slice.as_of_seq,
            head_seq: slice.head_seq,
        })
    }

    async fn lineage(&self, version_id: &str) -> Result<dto::LineageResponse> {
        let resp = self
            .rpc_retry(|| {
                let mut client = self.queries();
                let req = self.request(
                    pb::GetLineageRequest {
                        version_id: version_id.to_string(),
                    },
                    None,
                );
                async move { client.get_lineage(req).await }
            })
            .await?
            .into_inner();
        Ok(dto::LineageResponse {
            version_ids: resp.lineage.map(|l| l.version_ids).unwrap_or_default(),
        })
    }

    async fn anchors(
        &self,
        version_id: &str,
        as_of_seq: Option<u64>,
    ) -> Result<dto::AnchorsResponse> {
        reject_zero("as_of_seq", as_of_seq)?;
        let msg = pb::ListAnchorsRequest {
            version_id: version_id.to_string(),
            as_of_seq: as_of_seq.unwrap_or(0),
        };
        let resp = self
            .rpc_retry(|| {
                let mut client = self.queries();
                let req = self.request(msg.clone(), None);
                async move { client.list_anchors(req).await }
            })
            .await?
            .into_inner();
        Ok(dto::AnchorsResponse {
            anchors: resp.anchors.into_iter().map(Into::into).collect(),
        })
    }

    async fn promises(
        &self,
        version_id: &str,
        as_of_seq: Option<u64>,
        status: Option<&str>,
    ) -> Result<dto::PromisesResponse> {
        crate::planes::check_promise_status(status)?;
        reject_zero("as_of_seq", as_of_seq)?;
        let msg = pb::ListPromisesRequest {
            version_id: version_id.to_string(),
            status: status.unwrap_or_default().to_string(),
            as_of_seq: as_of_seq.unwrap_or(0),
        };
        let resp = self
            .rpc_retry(|| {
                let mut client = self.queries();
                let req = self.request(msg.clone(), None);
                async move { client.list_promises(req).await }
            })
            .await?
            .into_inner();
        Ok(dto::PromisesResponse {
            promises: resp.promises.into_iter().map(Into::into).collect(),
            // The overdue view (?overdue_scope=, 2026-08-17) is REST-only;
            // the gRPC promises RPC carries no overdue params, so this
            // plane never populates it.
            overdue_findings: None,
        })
    }

    async fn counters(
        &self,
        version_id: &str,
        as_of_seq: Option<u64>,
    ) -> Result<dto::CountersResponse> {
        reject_zero("as_of_seq", as_of_seq)?;
        let msg = pb::CounterTotalsRequest {
            version_id: version_id.to_string(),
            as_of_seq: as_of_seq.unwrap_or(0),
        };
        let resp = self
            .rpc_retry(|| {
                let mut client = self.queries();
                let req = self.request(msg.clone(), None);
                async move { client.counter_totals(req).await }
            })
            .await?
            .into_inner();
        Ok(dto::CountersResponse {
            counters: resp.counters.into_iter().map(Into::into).collect(),
        })
    }

    async fn digests(&self, version_id: &str) -> Result<dto::DigestsResponse> {
        let resp = self
            .rpc_retry(|| {
                let mut client = self.queries();
                let req = self.request(
                    pb::ListDigestsRequest {
                        version_id: version_id.to_string(),
                    },
                    None,
                );
                async move { client.list_digests(req).await }
            })
            .await?
            .into_inner();
        Ok(dto::DigestsResponse {
            digests: resp.digests.into_iter().map(Into::into).collect(),
        })
    }

    async fn findings(
        &self,
        _version_id: &str,
        _q: FindingsQuery,
    ) -> Result<dto::FindingsResponse> {
        Err(MunariumError::Unsupported {
            detail: "findings have no gRPC RPC today — use the REST client \
                     (GET /v1/versions/{id}/findings)"
                .into(),
        })
    }

    async fn compose_context(
        &self,
        version_id: &str,
        q: ContextQuery,
    ) -> Result<dto::ComposedContextDto> {
        reject_zero("as_of_seq", q.as_of_seq)?;
        reject_zero("fact_limit", q.fact_limit.map(|v| v as u64))?;
        reject_zero("budget_tokens", q.budget_tokens)?;
        let msg = pb::ComposeContextRequest {
            version_id: version_id.to_string(),
            scope: q.scope.unwrap_or_default(),
            budget_tokens: q.budget_tokens.unwrap_or(0),
            fact_limit: to_u32("fact_limit", q.fact_limit)?,
            as_of_seq: q.as_of_seq.unwrap_or(0),
            as_of_date: String::new(),
        };
        let resp = self
            .rpc_retry(|| {
                let mut client = self.queries();
                let req = self.request(msg.clone(), None);
                async move { client.compose_context(req).await }
            })
            .await?
            .into_inner();
        let ctx = resp.context.ok_or_else(|| MunariumError::Unexpected {
            status: None,
            detail: "ComposeContextResponse without context".into(),
        })?;
        Ok(ctx.into())
    }
}

// ---------------------------------------------------------------------------
// ingest
// ---------------------------------------------------------------------------

#[async_trait]
impl IngestPlane for GrpcTransport {
    async fn put_source(
        &self,
        meta: SourceMeta,
        chunks: ChunkSource,
    ) -> Result<dto::PutSourceResponse> {
        // Uploads are idempotent by content address, so transient failures
        // retry — the ChunkSource factory rebuilds the stream per attempt.
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self.put_source_once(&meta, chunks()).await {
                Err(e) if e.is_transient() && attempt <= self.retries => {
                    crate::retry::jitter_sleep(attempt).await;
                }
                other => return other,
            }
        }
    }

    async fn record_ingest(
        &self,
        version_id: &str,
        req: dto::RecordIngestRequest,
    ) -> Result<dto::RecordIngestResponse> {
        let msg = pb::RecordIngestRequest {
            version_id: version_id.to_string(),
            content_hash: req.content_hash,
            shape_ref: req.shape_ref.unwrap_or_default(),
            metadata_json: String::new(),
        };
        let resp = self
            .ingest_svc()
            .record_ingest(self.request(msg, None))
            .await
            .map_err(from_status)?
            .into_inner();
        Ok(dto::RecordIngestResponse {
            event_id: resp.event_id,
            seq: resp.seq,
        })
    }

    async fn ingest(&self, file: dto::IngestFileRequest) -> Result<dto::IngestResultDto> {
        // Single-file parity with REST `POST /v1/ingest`, which returns a
        // typed 400 for an undecodable body: a local decode failure is an
        // ERROR here, not a per-item result (per-item outcomes are the
        // BATCH contract). A server-side per-item error surfaces as
        // Unexpected carrying the text — the gRPC wire has no slug for it
        // (documented parity gap).
        let mut results = self.ingest_files(vec![file]).await?;
        let result = results.pop().ok_or_else(|| MunariumError::Unexpected {
            status: None,
            detail: "IngestFilesResponse carried no result for the one file sent".into(),
        })?;
        match (&result.error, &result.source_id) {
            (Some(err), None) if err.starts_with("content_base64") || err.contains("gRPC wire") => {
                Err(MunariumError::InvalidInput {
                    detail: err.clone(),
                })
            }
            (Some(err), _) => Err(MunariumError::Unexpected {
                status: None,
                detail: format!("ingest failed: {err}"),
            }),
            _ => Ok(result),
        }
    }

    async fn ingest_batch(&self, req: dto::IngestBatchRequest) -> Result<dto::IngestBatchResponse> {
        check_bulk_chunk_size("batch", req.files.len())?;
        Ok(dto::IngestBatchResponse {
            results: self.ingest_files(req.files).await?,
        })
    }

    async fn bulk_open(&self, _req: dto::BulkOpenRequest) -> Result<dto::BulkOpenResponse> {
        Err(bulk_unsupported())
    }

    async fn bulk_chunk(
        &self,
        _bulk_id: &str,
        _files: Vec<dto::IngestFileRequest>,
    ) -> Result<dto::BulkChunkResponse> {
        Err(bulk_unsupported())
    }

    async fn bulk_status(
        &self,
        _bulk_id: &str,
        _include_needed: bool,
    ) -> Result<dto::BulkStatusResponse> {
        Err(bulk_unsupported())
    }

    async fn bulk_complete(&self, _bulk_id: &str) -> Result<dto::BulkCompleteResponse> {
        Err(bulk_unsupported())
    }

    async fn get_source(&self, _source_id: &str) -> Result<dto::SourceInfoDto> {
        Err(MunariumError::Unsupported {
            detail: "source metadata has no gRPC RPC today — use the REST client \
                     (GET /v1/sources/{source_id})"
                .into(),
        })
    }
}

fn bulk_unsupported() -> MunariumError {
    MunariumError::Unsupported {
        detail: "bulk upload sessions have no gRPC RPCs today — use the REST client \
                 (POST /v1/ingest/bulk …), or stream single sources via PutSource"
            .into(),
    }
}

/// Decode one file-plane entry to the wire shape. The REST plane carries
/// content as base64 INSIDE the JSON body; the gRPC message carries raw
/// bytes — so the client decodes here. A bad file yields an error STRING,
/// not a batch failure: the plane's contract is per-item outcomes.
fn ingest_file_pb(f: dto::IngestFileRequest) -> std::result::Result<pb::IngestFile, String> {
    use base64::Engine as _;
    if f.collections.as_ref().is_some_and(Vec::is_empty) {
        // REST `Some([])` means "bind to NO collection"; the proto3 empty
        // repeated field means absent = matcher auto-bind. The two cannot
        // agree on the wire, so an explicit [] is a sentinel case (like
        // Some(0)): reject rather than silently auto-bind.
        return Err(
            "collections = [] cannot be represented on the gRPC wire (proto3 \
                    empty = auto-bind); omit it, or use the REST transport"
                .into(),
        );
    }
    // The REST server TRIMS the base64 body before decoding, so a trailing
    // newline (the `base64` CLI's output) must succeed on both transports.
    let stripped: Vec<u8> = f
        .content_base64
        .bytes()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    let content = base64::engine::general_purpose::STANDARD
        .decode(&stripped)
        .map_err(|e| format!("content_base64 is not valid base64: {e}"))?;
    Ok(pb::IngestFile {
        filename: f.filename,
        media_type: f.media_type,
        content,
        sha256: f.sha256.unwrap_or_default(),
        collections: f.collections.unwrap_or_default(),
    })
}

impl GrpcTransport {
    /// The per-item contract holds ACROSS transports: a file whose base64
    /// cannot decode becomes its own error result (never sent), the valid
    /// remainder ships, and results splice back in input order — exactly
    /// the outcome the REST plane's server-side per-item handling produces.
    async fn ingest_files(
        &self,
        files: Vec<dto::IngestFileRequest>,
    ) -> Result<Vec<dto::IngestResultDto>> {
        // Boxed error slot: a local error result dwarfs the Ok arm and the
        // vector holds one slot per file (clippy::result_large_err).
        let mut slots: Vec<std::result::Result<pb::IngestFile, Box<dto::IngestResultDto>>> = files
            .into_iter()
            .map(|f| {
                let filename = f.filename.clone();
                ingest_file_pb(f).map_err(|error| {
                    Box::new(dto::IngestResultDto {
                        filename,
                        source_id: None,
                        sha256: None,
                        existed: false,
                        bound_to: Vec::new(),
                        error: Some(error),
                    })
                })
            })
            .collect();
        let to_send: Vec<pb::IngestFile> = slots
            .iter()
            .filter_map(|s| s.as_ref().ok().cloned())
            .collect();
        let mut server_results = if to_send.is_empty() {
            std::collections::VecDeque::new()
        } else {
            let msg = pb::IngestFilesRequest { files: to_send };
            // Content-addressed and per-item idempotent, but a batch can
            // partially apply — send once, like the REST file plane. And
            // DEADLINE-EXEMPT like the REST file/bulk sends: a 500-file body
            // runs to the 256 MiB ceiling.
            let mut req = tonic::Request::new(msg);
            self.apply_auth(&mut req);
            let resp = self
                .ingest_svc()
                .ingest_files(req)
                .await
                .map_err(from_status)?
                .into_inner();
            resp.results
                .into_iter()
                .map(|r| dto::IngestResultDto {
                    filename: r.filename,
                    source_id: opt(r.source_id),
                    sha256: opt(r.sha256),
                    existed: r.existed,
                    bound_to: r.bound_to,
                    error: opt(r.error),
                })
                .collect::<std::collections::VecDeque<_>>()
        };
        let sent_count = slots.iter().filter(|s| s.is_ok()).count();
        if server_results.len() != sent_count {
            // A surplus is as wrong as a shortfall: results splice back by
            // POSITION, so any count mismatch would mis-pair files silently.
            return Err(MunariumError::Unexpected {
                status: None,
                detail: format!(
                    "IngestFilesResponse carried {} results for {} files sent",
                    server_results.len(),
                    sent_count
                ),
            });
        }
        Ok(slots
            .drain(..)
            .map(|slot| match slot {
                Err(local_error) => *local_error,
                Ok(_) => server_results.pop_front().expect("count verified above"),
            })
            .collect())
    }
}

impl GrpcTransport {
    async fn put_source_once(
        &self,
        meta: &SourceMeta,
        mut chunks: BoxStream<'static, Vec<u8>>,
    ) -> Result<dto::PutSourceResponse> {
        let meta = meta.clone();
        let header = pb::PutSourceRequest {
            msg: Some(pb::put_source_request::Msg::Header(pb::SourceHeader {
                declared_sha256: meta.declared_sha256,
                media_type: meta.media_type.unwrap_or_default(),
                filename: meta.filename.unwrap_or_default(),
                shape_ref: meta.shape_ref.unwrap_or_default(),
            })),
        };
        // Feed a CONCRETE stream type (ReceiverStream) into tonic — a boxed
        // trait-object stream trips a rustc higher-ranked lifetime limitation
        // inside async-trait methods.
        let (tx, rx) = tokio::sync::mpsc::channel::<pb::PutSourceRequest>(16);
        tokio::spawn(async move {
            if tx.send(header).await.is_err() {
                return;
            }
            while let Some(c) = chunks.next().await {
                let msg = pb::PutSourceRequest {
                    msg: Some(pb::put_source_request::Msg::Chunk(c)),
                };
                if tx.send(msg).await.is_err() {
                    return;
                }
            }
        });
        // Streaming uploads run without the per-request deadline.
        let mut req = tonic::Request::new(tokio_stream::wrappers::ReceiverStream::new(rx));
        self.apply_auth(&mut req);
        let resp = self
            .ingest_svc()
            .put_source(req)
            .await
            .map_err(from_status)?
            .into_inner();
        Ok(dto::PutSourceResponse {
            source_id: resp.source_id,
            content_hash: resp.content_hash,
            bytes_len: resp.bytes_len,
            already_existed: resp.already_existed,
        })
    }
}

// ---------------------------------------------------------------------------
// retrieval
// ---------------------------------------------------------------------------

#[async_trait]
impl RetrievalPlane for GrpcTransport {
    async fn search(&self, req: dto::SearchRequest) -> Result<dto::SearchResponse> {
        reject_zero("top_k", req.top_k.map(|v| v as u64))?;
        let msg = pb::HybridSearchRequest {
            query: req.query.unwrap_or_default(),
            shape_ref: req.shape_ref.unwrap_or_default(),
            top_k: req.top_k.unwrap_or(0),
            filter_json: req.filter.map(|v| v.to_string()).unwrap_or_default(),
            index_version: req.index_version.unwrap_or_default(),
        };
        // Search is a read: same retry class as the query plane.
        let resp = self
            .rpc_retry(|| {
                let mut client = self.retrieval_svc();
                let req = self.request(msg.clone(), None);
                async move { client.hybrid_search(req).await }
            })
            .await?
            .into_inner();
        let envelope = resp.envelope.ok_or_else(|| MunariumError::Unexpected {
            status: None,
            detail: "HybridSearchResponse without ProvenanceEnvelope".into(),
        })?;
        Ok(dto::SearchResponse {
            hits: resp.hits.into_iter().map(Into::into).collect(),
            envelope: envelope.into(),
        })
    }

    async fn index_status(&self, shape_ref: &str) -> Result<dto::IndexStatusResponse> {
        let resp = self
            .rpc_retry(|| {
                let mut client = self.retrieval_svc();
                let req = self.request(
                    pb::GetIndexVersionRequest {
                        shape_ref: shape_ref.to_string(),
                    },
                    None,
                );
                async move { client.get_index_version(req).await }
            })
            .await?
            .into_inner();
        Ok(dto::IndexStatusResponse {
            index_version: resp.index_version,
            shape_ref: shape_ref.to_string(),
            event_watermark: resp.event_watermark,
            active: resp.active,
            manifest: serde_json::from_str(&resp.manifest_json).unwrap_or(serde_json::Value::Null),
        })
    }

    async fn build_index(
        &self,
        _shape_ref: &str,
        _version_id: Option<&str>,
    ) -> Result<dto::IndexStatusResponse> {
        Err(MunariumError::Unsupported {
            detail: "index builds have no gRPC RPC today — use the REST client \
                     (POST /v1/indexes/{shape_ref}/build)"
                .into(),
        })
    }

    async fn create_collection(
        &self,
        req: dto::CreateCollectionRequest,
    ) -> Result<dto::CollectionDto> {
        let msg = pb::CreateCollectionRequest {
            name: req.name,
            shape_ref: req.shape_ref,
            access_level: req.access_level,
            compartments: req.compartments,
            description: req.description.unwrap_or_default(),
        };
        // Create-or-update — but not replay-keyed: send once.
        let resp = self
            .retrieval_svc()
            .create_collection(self.request(msg, None))
            .await
            .map_err(from_status)?
            .into_inner();
        Ok(collection_dto(resp))
    }

    async fn list_collections(&self) -> Result<dto::CollectionsResponse> {
        let resp = self
            .rpc_retry(|| {
                let mut client = self.retrieval_svc();
                let req = self.request(pb::ListCollectionsRequest {}, None);
                async move { client.list_collections(req).await }
            })
            .await?
            .into_inner();
        Ok(dto::CollectionsResponse {
            collections: resp.collections.into_iter().map(collection_dto).collect(),
        })
    }

    async fn get_collection(&self, id: &str) -> Result<dto::CollectionDto> {
        let msg = pb::GetCollectionRequest { id: id.to_string() };
        let resp = self
            .rpc_retry(|| {
                let mut client = self.retrieval_svc();
                let req = self.request(msg.clone(), None);
                async move { client.get_collection(req).await }
            })
            .await?
            .into_inner();
        Ok(collection_dto(resp))
    }
}

fn collection_dto(c: pb::CollectionInfo) -> dto::CollectionDto {
    dto::CollectionDto {
        id: c.id,
        name: c.name,
        shape_ref: c.shape_ref,
        access_level: c.access_level,
        compartments: c.compartments,
        status: c.status,
        description: opt(c.description),
        created_at: c.created_at,
        source_count: c.source_count,
        active_index: opt(c.active_index),
    }
}

// ---------------------------------------------------------------------------
// runbooks + shapes
// ---------------------------------------------------------------------------

#[async_trait]
impl RunbooksPlane for GrpcTransport {
    async fn apply_shape(
        &self,
        yaml: &str,
        version_id: Option<&str>,
    ) -> Result<dto::ApplyShapeResponse> {
        let msg = pb::ApplyShapeRequest {
            yaml: yaml.to_string(),
            version_id: version_id.unwrap_or_default().to_string(),
        };
        let resp = self
            .runbook_svc()
            .apply_shape(self.request(msg, None))
            .await
            .map_err(from_status)?
            .into_inner();
        Ok(dto::ApplyShapeResponse {
            shape_ref: resp.shape_ref,
            // The wire doesn't carry the hash, but it is defined as
            // sha256(yaml bytes) — computed locally for REST parity.
            yaml_hash: hex::encode(sha2::Sha256::digest(yaml.as_bytes())),
            event_id: opt(resp.event_id),
        })
    }

    async fn apply_runbook(&self, yaml: &str) -> Result<dto::ApplyRunbookResponse> {
        let msg = pb::ApplyRunbookRequest {
            yaml: yaml.to_string(),
        };
        let resp = self
            .runbook_svc()
            .apply_runbook(self.request(msg, None))
            .await
            .map_err(from_status)?
            .into_inner();
        Ok(dto::ApplyRunbookResponse {
            runbook_ref: resp.runbook_ref,
        })
    }

    async fn run_runbook(
        &self,
        name: &str,
        version_id: Option<&str>,
    ) -> Result<dto::RunbookRunResponse> {
        let params = version_id
            .map(|v| serde_json::json!({ "version_id": v }).to_string())
            .unwrap_or_default();
        let msg = pb::RunRunbookRequest {
            runbook_ref: name.to_string(),
            params_json: params,
        };
        let resp = self
            .runbook_svc()
            .run_runbook(self.request(msg, None))
            .await
            .map_err(from_status)?
            .into_inner();
        // state rides the response since the additive proto field; fall back
        // to GetRun against older servers that predate it.
        let state = if resp.state.is_empty() {
            self.get_run(&resp.run_id).await?.state
        } else {
            resp.state
        };
        Ok(dto::RunbookRunResponse {
            run_id: resp.run_id,
            state,
        })
    }

    async fn get_run(&self, run_id: &str) -> Result<dto::RunStatusResponse> {
        let resp = self
            .rpc_retry(|| {
                let mut client = self.runbook_svc();
                let req = self.request(
                    pb::GetRunRequest {
                        run_id: run_id.to_string(),
                    },
                    None,
                );
                async move { client.get_run(req).await }
            })
            .await?
            .into_inner();
        Ok(dto::RunStatusResponse {
            run_id: resp.run_id,
            runbook_ref: resp.runbook_ref,
            state: resp.state,
            version_id: opt(resp.version_id),
            steps: resp
                .steps
                .into_iter()
                .map(|s| dto::RunbookStepDto {
                    ordinal: s.ordinal,
                    name: s.name,
                    state: s.state,
                    detail: json_opt(&s.detail_json),
                })
                .collect(),
        })
    }

    async fn approve_step(&self, run_id: &str, ordinal: u32) -> Result<dto::RunbookRunResponse> {
        let msg = pb::ApproveStepRequest {
            run_id: run_id.to_string(),
            step_ordinal: ordinal,
            note: String::new(),
        };
        let resp = self
            .runbook_svc()
            .approve_step(self.request(msg, None))
            .await
            .map_err(from_status)?
            .into_inner();
        let state = if resp.state.is_empty() {
            self.get_run(run_id).await?.state
        } else {
            resp.state
        };
        Ok(dto::RunbookRunResponse {
            run_id: run_id.to_string(),
            state,
        })
    }

    async fn list(&self, include_removed: bool) -> Result<dto::RunbooksResponse> {
        let msg = pb::ListRunbooksRequest { include_removed };
        let resp = self
            .rpc_retry(|| {
                let mut client = self.runbook_svc();
                let req = self.request(msg, None);
                async move { client.list_runbooks(req).await }
            })
            .await?
            .into_inner();
        Ok(dto::RunbooksResponse {
            runbooks: resp.runbooks.into_iter().map(runbook_summary_dto).collect(),
        })
    }

    async fn get_info(&self, name: &str) -> Result<dto::RunbookInfoResponse> {
        let msg = pb::GetRunbookInfoRequest {
            name: name.to_string(),
        };
        let resp = self
            .rpc_retry(|| {
                let mut client = self.runbook_svc();
                let req = self.request(msg.clone(), None);
                async move { client.get_runbook_info(req).await }
            })
            .await?
            .into_inner();
        Ok(dto::RunbookInfoResponse {
            runbook_ref: resp.runbook_ref,
            name: resp.name,
            version: resp.version,
            status: resp.status,
            collections: resp
                .collections
                .into_iter()
                .map(runbook_collection_dto)
                .collect(),
            versions: resp.versions,
            models: json_opt(&resp.models_json),
            retrieval: serde_json::from_str(&resp.retrieval_json)
                .unwrap_or(serde_json::Value::Null),
            has_completion: resp.has_completion,
            created_at: resp.created_at,
        })
    }

    async fn validate(
        &self,
        yaml: &str,
        opts: ValidateOptions,
    ) -> Result<dto::ValidateRunbookResponse> {
        let msg = pb::ValidateRunbookRequest {
            yaml: yaml.to_string(),
            suggest: opts.suggest,
            provider: opts.provider.unwrap_or_default(),
            model: opts.model.unwrap_or_default(),
            tier: opts.tier.unwrap_or_default(),
        };
        // With suggest=true this spends provider tokens — send once.
        let resp = self
            .runbook_svc()
            .validate_runbook(self.request(msg, None))
            .await
            .map_err(from_status)?
            .into_inner();
        Ok(dto::ValidateRunbookResponse {
            valid: resp.valid,
            findings: resp
                .findings
                .into_iter()
                .map(|f| dto::ValidationFindingDto {
                    severity: f.severity,
                    code: f.code,
                    message: f.message,
                    path: f.path,
                })
                .collect(),
            suggestions: resp
                .suggestions
                .into_iter()
                .map(|sg| dto::SuggestionDto {
                    title: sg.title,
                    rationale: sg.rationale,
                    patch_hint: opt(sg.patch_hint),
                })
                .collect(),
            suggest_note: opt(resp.suggest_note),
        })
    }

    async fn remove_request(&self, name: &str) -> Result<dto::RemovalRequestResponse> {
        let msg = pb::RequestRemovalRequest {
            runbook_ref: name.to_string(),
        };
        let resp = self
            .runbook_svc()
            .request_removal(self.request(msg, None))
            .await
            .map_err(from_status)?
            .into_inner();
        Ok(dto::RemovalRequestResponse {
            runbook_ref: resp.runbook_ref,
            removal_id: resp.removal_id,
            expires_at: resp.expires_at,
        })
    }

    async fn remove_confirm(
        &self,
        name: &str,
        removal_id: &str,
    ) -> Result<dto::RemovalConfirmResponse> {
        let msg = pb::ConfirmRemovalRequest {
            runbook_ref: name.to_string(),
            removal_id: removal_id.to_string(),
        };
        let resp = self
            .runbook_svc()
            .confirm_removal(self.request(msg, None))
            .await
            .map_err(from_status)?
            .into_inner();
        Ok(dto::RemovalConfirmResponse {
            runbook_ref: resp.runbook_ref,
            status: resp.status,
        })
    }

    async fn apply_chronology_rules(
        &self,
        _yaml: &str,
    ) -> Result<dto::ApplyChronologyRulesResponse> {
        Err(MunariumError::Unsupported {
            detail: "chronology rules have no gRPC RPC today — use the REST client \
                     (POST /v1/chronology-rules)"
                .into(),
        })
    }

    async fn get_chronology_rules(&self, _name: &str) -> Result<String> {
        Err(MunariumError::Unsupported {
            detail: "chronology rules have no gRPC RPC today — use the REST client \
                     (GET /v1/chronology-rules/{name})"
                .into(),
        })
    }
}

fn runbook_collection_dto(c: pb::RunbookCollectionInfo) -> dto::RunbookCollectionDto {
    dto::RunbookCollectionDto {
        name: c.name,
        collection_id: opt(c.collection_id),
        shape_ref: c.shape_ref,
        access_level: c.access_level,
        compartments: c.compartments,
        active_index: opt(c.active_index),
        source_count: c.source_count,
    }
}

fn runbook_summary_dto(r: pb::RunbookSummary) -> dto::RunbookSummaryDto {
    dto::RunbookSummaryDto {
        runbook_ref: r.runbook_ref,
        name: r.name,
        version: r.version,
        status: r.status,
        min_access_level: r.min_access_level,
        collections: r
            .collections
            .into_iter()
            .map(runbook_collection_dto)
            .collect(),
        created_at: r.created_at,
    }
}

// ---------------------------------------------------------------------------
// providers
// ---------------------------------------------------------------------------

#[async_trait]
impl ProvidersPlane for GrpcTransport {
    async fn apply_config(&self, yaml: &str) -> Result<dto::ApplyProviderConfigResponse> {
        let msg = pb::ApplyProviderConfigRequest {
            yaml: yaml.to_string(),
        };
        let resp = self
            .provider_svc()
            .apply_provider_config(self.request(msg, None))
            .await
            .map_err(from_status)?
            .into_inner();
        Ok(dto::ApplyProviderConfigResponse {
            config_name: resp.config_name,
        })
    }

    async fn health(&self, name: &str) -> Result<dto::ProviderHealthResponse> {
        let resp = self
            .rpc_retry(|| {
                let mut client = self.provider_svc();
                let req = self.request(
                    pb::ProviderHealthRequest {
                        config_name: name.to_string(),
                    },
                    None,
                );
                async move { client.provider_health(req).await }
            })
            .await?
            .into_inner();
        Ok(dto::ProviderHealthResponse {
            healthy: resp.healthy,
            provider: resp.provider,
            endpoint_fingerprint: resp.endpoint_fingerprint,
            detail: resp.detail,
        })
    }

    async fn complete(
        &self,
        name: &str,
        req: dto::CompleteRequest,
    ) -> Result<dto::CompleteResponse> {
        if req.temperature == Some(0.0) {
            return Err(MunariumError::InvalidInput {
                detail: "temperature = 0.0 cannot be represented on the gRPC wire \
                         (proto3 uses 0.0 for 'absent'); omit it, or use the REST transport"
                    .into(),
            });
        }
        reject_zero("max_tokens", req.max_tokens.map(u64::from))?;
        let msg = pb::CompleteRequest {
            config_name: name.to_string(),
            model: req.model.unwrap_or_default(),
            provider: req.provider.unwrap_or_default(),
            tier: req.tier.unwrap_or_default(),
            system: req.system.unwrap_or_default(),
            prompt: req.prompt.unwrap_or_default(),
            max_tokens: req.max_tokens.unwrap_or(0),
            temperature: req.temperature.unwrap_or(0.0),
            tools_json: String::new(),
            version_id: req.version_id.unwrap_or_default(),
        };
        let resp = self
            .provider_svc()
            .complete(self.request(msg, None))
            .await
            .map_err(from_status)?
            .into_inner();
        Ok(dto::CompleteResponse {
            text: resp.text,
            stop_reason: resp.stop_reason,
            input_tokens: resp.input_tokens,
            output_tokens: resp.output_tokens,
            provider: resp.provider,
            model: resp.model,
            invocation_event_id: opt(resp.invocation_event_id),
        })
    }

    async fn embed(&self, name: &str, req: dto::EmbedRequest) -> Result<dto::EmbedResponse> {
        let msg = pb::EmbedRequest {
            config_name: name.to_string(),
            model: req.model.unwrap_or_default(),
            provider: req.provider.unwrap_or_default(),
            inputs: req.inputs,
            version_id: req.version_id.unwrap_or_default(),
        };
        let resp = self
            .provider_svc()
            .embed(self.request(msg, None))
            .await
            .map_err(from_status)?
            .into_inner();
        Ok(dto::EmbedResponse {
            vectors: resp.vectors.into_iter().map(|v| v.values).collect(),
            dimensions: resp.dimensions as u64,
            cache_hit: resp.cache_hit,
            provider: resp.provider,
            model: resp.model,
            invocation_event_id: opt(resp.invocation_event_id),
        })
    }

    async fn health_ai(&self) -> Result<dto::HealthAiResponse> {
        Err(MunariumError::Unsupported {
            detail: "healthai has no gRPC RPC today — use the REST client (GET /healthai)".into(),
        })
    }

    async fn list(&self) -> Result<dto::ProviderListResponse> {
        Err(MunariumError::Unsupported {
            detail: "provider disclosure has no gRPC RPC today — use the REST client \
                     (GET /v1/providers)"
                .into(),
        })
    }

    async fn max_tokens(&self) -> Result<dto::MaxTokensResponse> {
        Err(max_tokens_unsupported())
    }

    async fn replace_max_tokens(
        &self,
        _budgets: &dto::MaxTokensBudgets,
    ) -> Result<dto::MaxTokensResponse> {
        Err(max_tokens_unsupported())
    }
}

fn max_tokens_unsupported() -> MunariumError {
    MunariumError::Unsupported {
        detail: "per-call token budgets have no gRPC RPC today — use the REST client \
                 (GET/POST /v1/max-tokens)"
            .into(),
    }
}

// ---------------------------------------------------------------------------
// sessions
// ---------------------------------------------------------------------------

#[async_trait]
impl SessionsPlane for GrpcTransport {
    async fn create(&self, runbook_name: &str) -> Result<dto::CreateSessionResponse> {
        let msg = pb::CreateSessionRequest {
            runbook_name: runbook_name.to_string(),
        };
        // Opens server-side state — send once.
        let resp = self
            .session_svc()
            .create_session(self.request(msg, None))
            .await
            .map_err(from_status)?
            .into_inner();
        Ok(dto::CreateSessionResponse {
            session_id: resp.session_id,
            runbook_ref: resp.runbook_ref,
            permitted_collections: resp.permitted_collections,
        })
    }

    async fn turn(&self, session_id: &str, req: dto::TurnRequest) -> Result<dto::TurnResponse> {
        reject_zero("top_k", req.top_k.map(u64::from))?;
        let msg = pb::TurnRequest {
            session_id: session_id.to_string(),
            query: req.query,
            top_k: req.top_k.unwrap_or(0),
            complete: req.complete.unwrap_or(false),
            model_override: req.model_override.map(|o| pb::SessionModelOverride {
                provider: o.provider.unwrap_or_default(),
                model: o.model.unwrap_or_default(),
                tier: o.tier.unwrap_or_default(),
            }),
            // Empty means "no research profile" on both sides of this wire
            // (the server maps "" back to None), so a caller that never
            // sets one sends the same bytes it always has. Unlike the
            // numeric fields there is nothing to `reject_zero`: an empty
            // profile NAME is not a legal profile, so proto3's zero value
            // and `None` genuinely mean the same thing here.
            research_profile: req.research_profile.unwrap_or_default(),
        };
        // A turn spends provider tokens — send once, never auto-retried,
        // and DEADLINE-EXEMPT like the REST twin: aborting client-side does
        // not stop the server's paid completion.
        let mut req = tonic::Request::new(msg);
        self.apply_auth(&mut req);
        let resp = self
            .session_svc()
            .turn(req)
            .await
            .map_err(from_status)?
            .into_inner();
        turn_response_dto(resp)
    }

    async fn turn_stream(&self, _session_id: &str, _req: dto::TurnRequest) -> Result<TurnStream> {
        Err(MunariumError::Unsupported {
            detail: "streaming turns have no gRPC RPC today — use the REST client \
                     (POST /v1/sessions/{id}/turns/stream), or the unary turn here"
                .into(),
        })
    }

    async fn get(&self, session_id: &str) -> Result<dto::SessionResponse> {
        let msg = pb::GetSessionRequest {
            session_id: session_id.to_string(),
        };
        let resp = self
            .rpc_retry(|| {
                let mut client = self.session_svc();
                let req = self.request(msg.clone(), None);
                async move { client.get_session(req).await }
            })
            .await?
            .into_inner();
        Ok(session_dto(resp))
    }

    async fn close(&self, session_id: &str) -> Result<dto::SessionResponse> {
        let msg = pb::CloseSessionRequest {
            session_id: session_id.to_string(),
        };
        // Idempotent by construction server-side, but still a write — sent
        // once, matching the REST transport.
        let resp = self
            .session_svc()
            .close_session(self.request(msg, None))
            .await
            .map_err(from_status)?
            .into_inner();
        Ok(session_dto(resp))
    }
}

fn turn_response_dto(resp: pb::TurnResponse) -> Result<dto::TurnResponse> {
    let envelopes = resp
        .envelopes
        .into_iter()
        .map(|e| {
            let envelope = e.envelope.ok_or_else(|| MunariumError::Unexpected {
                status: None,
                detail: format!(
                    "CollectionEnvelope for '{}' without ProvenanceEnvelope",
                    e.collection
                ),
            })?;
            Ok(dto::CollectionEnvelopeDto {
                collection: e.collection,
                envelope: envelope.into(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(dto::TurnResponse {
        session_id: resp.session_id,
        ordinal: resp.ordinal,
        collections_searched: resp.collections_searched,
        skipped: resp.skipped,
        hits: resp
            .hits
            .into_iter()
            .map(|h| dto::TurnHitDto {
                collection: h.collection,
                chunk_id: h.chunk_id,
                source_id: h.source_id,
                source_path: h.source_path,
                source_content_hash: h.source_content_hash,
                text: h.text,
                score: h.score,
            })
            .collect(),
        envelopes,
        completion: resp.completion.map(|c| dto::TurnCompletionDto {
            provider: c.provider,
            model: c.model,
            was_override: c.was_override,
            text: c.text,
            input_tokens: c.input_tokens,
            output_tokens: c.output_tokens,
            verification: c.verification.map(|v| dto::TurnVerificationDto {
                checks: v.checks,
                retries: v.retries,
                first_pass_violations: v.first_pass_violations,
                violations: v.violations,
            }),
        }),
        // Absent unless a research profile ran, so a legacy turn decodes to
        // exactly the TurnResponse it always did. The inner empty strings
        // go back to None through `opt`: proto3 cannot carry an absent
        // string, and an empty refusal_code beside `block: "refusal"` would
        // read as a refusal that named no reason.
        hierarchy: resp.hierarchy.map(|h| dto::EvidenceHierarchyDecisionDto {
            profile: h.profile,
            intent_kind: opt(h.intent_kind),
            intent_explicit: h.intent_explicit,
            layers: h
                .layers
                .into_iter()
                .map(|l| dto::LayerOutcomeDto {
                    layer: l.layer,
                    role: l.role,
                    requirement: l.requirement,
                    block: l.block,
                    evidence_id: opt(l.evidence_id),
                    supports_completeness: l.supports_completeness,
                    refusal_code: opt(l.refusal_code),
                    elapsed_ms: l.elapsed_ms,
                })
                .collect(),
            completeness_available: h.completeness_available,
            disclosed_conflicts: h.disclosed_conflicts,
            conflicts_policy: h.conflicts_policy,
        }),
    })
}

fn session_dto(resp: pb::GetSessionResponse) -> dto::SessionResponse {
    dto::SessionResponse {
        session_id: resp.session_id,
        uid: resp.uid,
        runbook_ref: resp.runbook_ref,
        access_level: resp.access_level,
        compartments: resp.compartments,
        state: resp.state,
        created_at: resp.created_at,
        turns: resp
            .turns
            .into_iter()
            .map(|t| dto::SessionTurnDto {
                ordinal: t.ordinal,
                query: t.query,
                collections_searched: t.collections_searched,
                // Stored transcript rows ride as JSON strings on the wire —
                // parse-or-Null keeps a mangled row visible instead of
                // failing the whole session read.
                hits: serde_json::from_str(&t.hits_json).unwrap_or(serde_json::Value::Null),
                envelope: serde_json::from_str(&t.envelope_json).unwrap_or(serde_json::Value::Null),
                completion: json_opt(&t.completion_json),
                created_at: t.created_at,
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// access tokens (mgmt) — AdminService's served trio
// ---------------------------------------------------------------------------

#[async_trait]
impl TokensPlane for GrpcTransport {
    async fn mint(&self, req: dto::IssueTokenRequest) -> Result<dto::IssueTokenResponse> {
        reject_zero("ttl_secs", req.ttl_secs)?;
        if req.runbook_refs.as_ref().is_some_and(Vec::is_empty) {
            // REST `Some([])` = no runbook allowed; proto3 empty = any runbook.
            return Err(MunariumError::InvalidInput {
                detail: "runbook_refs = [] cannot be represented on the gRPC wire (proto3 \
                         empty = any runbook); omit it, or use the REST transport"
                    .into(),
            });
        }
        let msg = pb::IssueAccessTokenRequest {
            uid: req.uid,
            access_level: req.access_level,
            compartments: req.compartments,
            scopes: req.scopes,
            runbook_refs: req.runbook_refs.unwrap_or_default(),
            ttl_secs: req.ttl_secs.unwrap_or(0),
        };
        // Minting twice issues two live tokens — send once.
        let resp = self
            .admin_svc()
            .issue_access_token(self.request(msg, None))
            .await
            .map_err(from_status)?
            .into_inner();
        Ok(dto::IssueTokenResponse {
            token: resp.token,
            jti: resp.jti,
            expires_at: resp.expires_at,
        })
    }

    async fn list(&self, q: TokenListQuery) -> Result<dto::TokensResponse> {
        let msg = pb::ListAccessTokensRequest {
            uid: q.uid.unwrap_or_default(),
            // proto3 bool: false = "all" — identical to the REST default,
            // so Some(false) and None land on the same wire value by design.
            active: q.active.unwrap_or(false),
        };
        let resp = self
            .rpc_retry(|| {
                let mut client = self.admin_svc();
                let req = self.request(msg.clone(), None);
                async move { client.list_access_tokens(req).await }
            })
            .await?
            .into_inner();
        Ok(dto::TokensResponse {
            tokens: resp
                .tokens
                .into_iter()
                .map(|t| dto::TokenInfoDto {
                    jti: t.jti,
                    uid: t.uid,
                    access_level: t.access_level,
                    compartments: t.compartments,
                    scopes: t.scopes,
                    runbook_refs: if t.runbook_refs.is_empty() {
                        None
                    } else {
                        Some(t.runbook_refs)
                    },
                    issued_by: t.issued_by,
                    issued_at: t.issued_at,
                    expires_at: t.expires_at,
                    revoked_at: opt(t.revoked_at),
                })
                .collect(),
        })
    }

    async fn revoke(&self, jti: &str) -> Result<dto::RevokeTokenResponse> {
        let msg = pb::RevokeAccessTokenRequest {
            jti: jti.to_string(),
        };
        let resp = self
            .admin_svc()
            .revoke_access_token(self.request(msg, None))
            .await
            .map_err(from_status)?
            .into_inner();
        Ok(dto::RevokeTokenResponse {
            jti: resp.jti,
            revoked: resp.revoked,
            revocation_check_enabled: resp.revocation_check_enabled,
        })
    }
}

// ---------------------------------------------------------------------------
// reports / authoring / meta — REST-only surfaces, honestly typed
// ---------------------------------------------------------------------------

fn reports_unsupported() -> MunariumError {
    MunariumError::Unsupported {
        detail: "reports have no gRPC RPCs today (AdminService.Usage is declared but \
                 UNIMPLEMENTED) — use the REST client (GET /v1/reports/…)"
            .into(),
    }
}

#[async_trait]
impl ReportsPlane for GrpcTransport {
    async fn usage(&self, _q: UsageQuery) -> Result<dto::UsageResponse> {
        Err(reports_unsupported())
    }
    async fn audit(&self, _q: AuditQuery) -> Result<dto::AuditResponse> {
        Err(reports_unsupported())
    }
    async fn cost(&self, _from: Option<&str>, _to: Option<&str>) -> Result<dto::CostResponse> {
        Err(reports_unsupported())
    }
    async fn timeseries(
        &self,
        _window: Option<&str>,
        _plane: Option<&str>,
    ) -> Result<dto::TimeseriesResponse> {
        Err(reports_unsupported())
    }
    async fn endpoints(
        &self,
        _window: Option<&str>,
        _limit: Option<i64>,
    ) -> Result<dto::EndpointsResponse> {
        Err(reports_unsupported())
    }
    async fn runbooks(&self, _window: Option<&str>) -> Result<dto::RunbookReportResponse> {
        Err(reports_unsupported())
    }
    async fn sessions(&self, _window: Option<&str>) -> Result<dto::SessionsReportResponse> {
        Err(reports_unsupported())
    }
    async fn evidence(&self, _window: Option<&str>) -> Result<dto::EvidenceReportResponse> {
        Err(reports_unsupported())
    }
    async fn matrix(&self) -> Result<dto::MatrixReportResponse> {
        Err(reports_unsupported())
    }
}

fn authoring_unsupported() -> MunariumError {
    MunariumError::Unsupported {
        detail: "guided authoring has no gRPC RPCs — use the REST client \
                 (/v1/authoring/…)"
            .into(),
    }
}

#[async_trait]
impl AuthoringPlane for GrpcTransport {
    async fn list_patterns(&self) -> Result<dto::PatternsResponse> {
        Err(authoring_unsupported())
    }
    async fn get_pattern(&self, _id: &str) -> Result<dto::PatternDetailResponse> {
        Err(authoring_unsupported())
    }
    async fn create_draft(&self, _req: dto::CreateDraftRequest) -> Result<dto::DraftResponse> {
        Err(authoring_unsupported())
    }
    async fn list_drafts(&self) -> Result<dto::DraftsResponse> {
        Err(authoring_unsupported())
    }
    async fn get_draft(&self, _draft_id: &str) -> Result<dto::DraftResponse> {
        Err(authoring_unsupported())
    }
    async fn delete_draft(&self, _draft_id: &str) -> Result<dto::DraftDeleteResponse> {
        Err(authoring_unsupported())
    }
    async fn put_answers(
        &self,
        _draft_id: &str,
        _req: dto::UpdateAnswersRequest,
    ) -> Result<dto::DraftResponse> {
        Err(authoring_unsupported())
    }
    async fn validate(&self, _draft_id: &str) -> Result<dto::DraftValidationResponse> {
        Err(authoring_unsupported())
    }
    async fn assist(
        &self,
        _draft_id: &str,
        _req: dto::AssistDraftRequest,
    ) -> Result<dto::AssistDraftResponse> {
        Err(authoring_unsupported())
    }
    async fn export(&self, _draft_id: &str) -> Result<dto::ExportDraftResponse> {
        Err(authoring_unsupported())
    }
    async fn apply(&self, _draft_id: &str) -> Result<dto::ApplyDraftResponse> {
        Err(authoring_unsupported())
    }
}

#[async_trait]
impl MetaPlane for GrpcTransport {
    async fn server_version(&self) -> Result<ServerVersionInfo> {
        Err(MunariumError::Unsupported {
            detail: "GET /version is a REST meta route — use the REST client, or gRPC \
                     server reflection"
                .into(),
        })
    }
}

/// Narrow a caller's `usize` bound to the wire's u32 without letting a
/// truncation forge `0` — the proto3 "absent" sentinel that would silently
/// turn a bounded request into an unbounded one.
fn to_u32(field: &str, v: Option<usize>) -> Result<u32> {
    match v {
        None => Ok(0),
        Some(n) => u32::try_from(n).map_err(|_| MunariumError::InvalidInput {
            detail: format!("{field} must fit in u32 (got {n})"),
        }),
    }
}

/// Resolve a command's idempotency key, generating one when absent and
/// rejecting a caller-supplied key that is not valid gRPC metadata. Failing
/// here names the offending key; dropping it silently would make the server
/// reject the command for a "missing" header the caller demonstrably sent.
fn resolve_idem(idem: IdemKey) -> Result<String> {
    match idem {
        None => Ok(new_idem_key()),
        Some(key) => {
            if key
                .parse::<tonic::metadata::MetadataValue<tonic::metadata::Ascii>>()
                .is_ok()
            {
                Ok(key)
            } else {
                Err(MunariumError::InvalidInput {
                    detail: format!(
                        "idempotency key is not valid gRPC metadata (printable ASCII): {key:?}"
                    ),
                })
            }
        }
    }
}

#[async_trait]
impl crate::planes::EvidencePlane for GrpcTransport {
    async fn evidence(&self, _evidence_id: &str) -> Result<serde_json::Value> {
        Err(MunariumError::Unsupported {
            detail: "the sealed evidence plane is REST-only in v1 — use the REST client                      (GET /v1/evidence/{id})"
                .into(),
        })
    }

    async fn evidence_rows(
        &self,
        _evidence_id: &str,
        _q: crate::planes::EvidenceRowsQuery,
    ) -> Result<dto::EvidenceRowsResponse> {
        Err(MunariumError::Unsupported {
            detail: "the sealed evidence plane is REST-only in v1 — use the REST client                      (GET /v1/evidence/{id}/rows)"
                .into(),
        })
    }
}

#[cfg(test)]
mod ingest_tests {
    use super::*;

    #[test]
    fn base64_tolerates_whitespace_like_the_rest_server() {
        let f = dto::IngestFileRequest {
            filename: "a.md".into(),
            media_type: "text/plain".into(),
            content_base64: "aGVs\nbG8=\n".into(),
            sha256: None,
            collections: None,
        };
        assert_eq!(ingest_file_pb(f).unwrap().content, b"hello");
        let bad = dto::IngestFileRequest {
            filename: "a.md".into(),
            media_type: "text/plain".into(),
            content_base64: "not*base64".into(),
            sha256: None,
            collections: None,
        };
        assert!(ingest_file_pb(bad).is_err());
        let empty_list = dto::IngestFileRequest {
            filename: "a.md".into(),
            media_type: "text/plain".into(),
            content_base64: "aGVsbG8=".into(),
            sha256: None,
            collections: Some(vec![]),
        };
        assert!(
            ingest_file_pb(empty_list).is_err(),
            "explicit [] diverges from REST on the wire — must be rejected"
        );
    }
}

#[cfg(test)]
mod max_tokens_tests {
    use super::*;

    /// A transport over a channel that never connects: the two REST-only
    /// budget methods must refuse BEFORE touching the wire, so an endpoint
    /// nothing listens on is exactly the right fixture.
    fn offline_transport() -> GrpcTransport {
        GrpcTransport {
            channel: Channel::from_static("http://127.0.0.1:1").connect_lazy(),
            auth: None,
            uid: None,
            request_timeout: Duration::from_secs(1),
            retries: 0,
        }
    }

    #[tokio::test]
    async fn the_budget_routes_are_honestly_unsupported_not_silent() {
        let t = offline_transport();
        let read = t.max_tokens().await;
        assert!(
            matches!(read, Err(MunariumError::Unsupported { .. })),
            "GET /v1/max-tokens has no gRPC twin: {read:?}"
        );
        let write = t
            .replace_max_tokens(&dto::MaxTokensBudgets::default())
            .await;
        assert!(
            matches!(write, Err(MunariumError::Unsupported { .. })),
            "POST /v1/max-tokens has no gRPC twin: {write:?}"
        );
    }
}
