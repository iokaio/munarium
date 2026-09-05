// SPDX-License-Identifier: Apache-2.0
//! REST transport: reqwest + rustls, problem+json error decoding, automatic
//! idempotency keys on commands, bounded retries. Retry classes:
//! - idempotent reads (GETs + search): transient errors (connect, 5xx
//!   gateway/overload) retried with backoff;
//! - core commands: re-sent with the SAME idempotency key ONLY when the
//!   request provably never reached the server (a connect-phase failure) or
//!   the server shed it before executing — the server records an idempotency
//!   key AFTER a command completes, so a possibly-delivered command is never
//!   re-sent (it could execute twice);
//! - non-idempotent un-keyed writes (turns, provider calls, ingest, …): sent
//!   exactly once.
use crate::error::{MunariumError, Result};
use crate::planes::*;
use crate::{new_idem_key, MunariumClientOptions};
use async_trait::async_trait;
use futures_util::StreamExt;
use munarium_api_types as dto;
use serde::de::DeserializeOwned;
use std::io::{self, Write};
use std::time::Duration;

const JSON_BODY_CHUNK_BYTES: usize = 64 * 1024;

/// A blocking serde_json writer backed by a bounded async channel. serde's
/// serializer borrows large string slices (notably base64 file content), and
/// this writer copies them out in small chunks with backpressure instead of
/// first materializing a second request-sized Vec.
struct JsonChunkWriter {
    sender: tokio::sync::mpsc::Sender<std::result::Result<bytes::Bytes, io::Error>>,
}

impl Write for JsonChunkWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        for chunk in buf.chunks(JSON_BODY_CHUNK_BYTES) {
            self.sender
                .blocking_send(Ok(bytes::Bytes::copy_from_slice(chunk)))
                .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "request body closed"))?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn streaming_json_chunks<B>(
    body: B,
) -> impl futures_core::Stream<Item = std::result::Result<bytes::Bytes, io::Error>>
where
    B: serde::Serialize + Send + 'static,
{
    let (sender, receiver) = tokio::sync::mpsc::channel(8);
    tokio::task::spawn_blocking(move || {
        let mut writer = JsonChunkWriter {
            sender: sender.clone(),
        };
        if let Err(error) = serde_json::to_writer(&mut writer, &body) {
            let _ = sender.blocking_send(Err(io::Error::other(format!(
                "unserializable request body: {error}"
            ))));
        }
    });
    futures_util::stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|item| (item, receiver))
    })
}

fn streaming_json_body<B>(body: B) -> reqwest::Body
where
    B: serde::Serialize + Send + 'static,
{
    reqwest::Body::wrap_stream(streaming_json_chunks(body))
}

pub struct RestTransport {
    http: reqwest::Client,
    /// Same client sans the per-request deadline. Used wherever a 30 s cap
    /// is a trap: streaming ingest, the file/bulk planes (256 MiB bodies),
    /// and unary turns (aborting client-side does NOT stop the server's
    /// paid completion — a timeout here is a double-spend invitation).
    http_stream: reqwest::Client,
    /// The SSE client: no overall deadline either, but a per-read idle
    /// watchdog — the server heartbeats comment keep-alives every 15 s, so
    /// 60 s of wire silence means a wedged peer, not a slow completion.
    http_sse: reqwest::Client,
    base: String,
    read_retries: u32,
}

/// Percent-encode a path segment (RFC 3986 unreserved characters pass).
/// Promise keys, shape refs, and runbook names are free-form — a raw '/'
/// or '?' must not change the route shape.
fn seg(s: &str) -> String {
    const KEEP: &[u8] = b"-._~";
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || KEEP.contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn version_query(version_id: Option<&str>) -> Vec<(&'static str, String)> {
    version_id
        .map(|v| vec![("version_id", v.to_string())])
        .unwrap_or_default()
}

/// Append an optional query param — the ONE idiom for optional params, so
/// encoding decisions cannot diverge per endpoint.
fn push_opt(params: &mut Vec<(&'static str, String)>, key: &'static str, v: Option<&str>) {
    if let Some(v) = v {
        params.push((key, v.to_string()));
    }
}

fn pin_query(as_of_seq: Option<u64>) -> Vec<(&'static str, String)> {
    as_of_seq
        .map(|s| vec![("as_of_seq", s.to_string())])
        .unwrap_or_default()
}

impl RestTransport {
    pub fn new(options: MunariumClientOptions) -> Result<Self> {
        // Auth + uid are default headers: parsed and validated once, free per
        // call, and cloned into both the unary and streaming clients so the
        // uid contract covers PUT /v1/sources too.
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(token) = &options.token {
            let value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| MunariumError::InvalidInput {
                    detail: "token contains non-header-safe characters".into(),
                })?;
            headers.insert(reqwest::header::AUTHORIZATION, value);
        }
        if let Some(uid) = &options.uid {
            let value = reqwest::header::HeaderValue::from_str(uid).map_err(|_| {
                MunariumError::InvalidInput {
                    detail: "uid contains non-header-safe characters".into(),
                }
            })?;
            headers.insert("x-munarium-uid", value);
        }
        let http = reqwest::Client::builder()
            .default_headers(headers.clone())
            .connect_timeout(options.connect_timeout)
            .timeout(options.request_timeout)
            .build()
            .map_err(|e| MunariumError::Transport {
                detail: e.to_string(),
                may_have_reached_server: false,
            })?;
        let http_stream = reqwest::Client::builder()
            .default_headers(headers.clone())
            .connect_timeout(options.connect_timeout)
            .build()
            .map_err(|e| MunariumError::Transport {
                detail: e.to_string(),
                may_have_reached_server: false,
            })?;
        let http_sse = reqwest::Client::builder()
            .default_headers(headers)
            .connect_timeout(options.connect_timeout)
            .read_timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(|e| MunariumError::Transport {
                detail: e.to_string(),
                may_have_reached_server: false,
            })?;
        Ok(Self {
            http,
            http_stream,
            http_sse,
            base: options.endpoint.trim_end_matches('/').to_string(),
            read_retries: options.read_retries,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Decode a response: success -> T, error -> typed problem+json error
    /// (Retry-After preserved on rate limits).
    async fn decode<T: DeserializeOwned>(resp: reqwest::Response) -> Result<T> {
        let status = resp.status();
        if status.is_success() {
            return resp
                .json::<T>()
                .await
                .map_err(|e| MunariumError::Unexpected {
                    status: Some(status.as_u16()),
                    detail: format!("undecodable success body: {e}"),
                });
        }
        Err(Self::decode_error(resp).await)
    }

    /// The ONE error-decoding path for non-success responses — problem+json
    /// through the slug registry with the Retry-After header preserved.
    /// Every consumer of an error response (unary decode, text reads, the
    /// SSE pre-stream refusal) goes through here so none can drift.
    async fn decode_error(resp: reqwest::Response) -> MunariumError {
        let code = resp.status().as_u16();
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_retry_after);
        match resp.json::<serde_json::Value>().await {
            Ok(body) => MunariumError::from_problem(code, retry_after, &body),
            Err(_) => MunariumError::Unexpected {
                status: Some(code),
                detail: format!("non-JSON error body (HTTP {code})"),
            },
        }
    }

    fn transport_err(e: reqwest::Error) -> MunariumError {
        if e.is_connect() || e.is_timeout() || e.is_request() {
            MunariumError::Transport {
                detail: e.to_string(),
                // A connect-phase failure provably never delivered the
                // request; a timeout on an established connection may have.
                may_have_reached_server: !e.is_connect(),
            }
        } else {
            MunariumError::Unexpected {
                status: None,
                detail: e.to_string(),
            }
        }
    }

    /// Idempotent-read path: retry transient outcomes with backoff.
    async fn read_request<T: DeserializeOwned, F>(&self, build: F) -> Result<T>
    where
        F: Fn() -> reqwest::RequestBuilder,
    {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let outcome = match build().send().await {
                Ok(resp) => Self::decode::<T>(resp).await,
                Err(e) => Err(Self::transport_err(e)),
            };
            match outcome {
                Err(e) if e.is_transient() && attempt <= self.read_retries => {
                    crate::retry::jitter_sleep(attempt).await;
                }
                other => return other,
            }
        }
    }

    async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T> {
        self.read_request(|| self.http.get(self.url(path)).query(query))
            .await
    }

    /// Read that happens to be a POST (search): same retry class as GETs.
    async fn post_json_read<B: serde::Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        // Serialize once; clone is a cheap buffer copy per retry attempt.
        let bytes = serde_json::to_vec(body).map_err(|e| MunariumError::InvalidInput {
            detail: format!("unserializable request body: {e}"),
        })?;
        self.read_request(|| {
            self.http
                .post(self.url(path))
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(bytes.clone())
        })
        .await
    }

    /// Core-command path: JSON body + idempotency key; re-sent with the
    /// SAME key only for provably-undelivered outcomes (connect-phase
    /// failure, pre-execution load shed) — see `is_command_retry_safe`.
    async fn command_json<B: serde::Serialize, T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: &B,
        idem: IdemKey,
    ) -> Result<T> {
        let key = idem.unwrap_or_else(new_idem_key);
        let bytes = serde_json::to_vec(body).map_err(|e| MunariumError::InvalidInput {
            detail: format!("unserializable request body: {e}"),
        })?;
        let mut attempt = 0;
        loop {
            attempt += 1;
            let req = self
                .http
                .request(method.clone(), self.url(path))
                .header("idempotency-key", &key)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(bytes.clone());
            let outcome = match req.send().await {
                Ok(resp) => Self::decode::<T>(resp).await,
                Err(e) => Err(Self::transport_err(e)),
            };
            match outcome {
                // NOT `is_transient`: a command whose request may already
                // have been delivered is surfaced to the caller instead of
                // being re-sent — see `is_command_retry_safe`.
                Err(e) if e.is_command_retry_safe() && attempt <= self.read_retries => {
                    crate::retry::jitter_sleep(attempt).await;
                }
                other => return other,
            }
        }
    }

    /// Non-idempotent un-keyed writes: sent exactly once.
    async fn write_json_once<B: serde::Serialize, T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
        body: &B,
    ) -> Result<T> {
        let req = self
            .http
            .request(method, self.url(path))
            .query(query)
            .json(body);
        match req.send().await {
            Ok(resp) => Self::decode(resp).await,
            Err(e) => Err(Self::transport_err(e)),
        }
    }

    async fn write_yaml_once<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        yaml: &str,
    ) -> Result<T> {
        let req = self
            .http
            .post(self.url(path))
            .query(query)
            .header(reqwest::header::CONTENT_TYPE, "text/yaml")
            .body(yaml.to_string());
        match req.send().await {
            Ok(resp) => Self::decode(resp).await,
            Err(e) => Err(Self::transport_err(e)),
        }
    }

    /// Send-once POST for the file/bulk planes: their bodies run to the
    /// server's 256 MiB ceiling, so the per-request deadline that suits
    /// small JSON writes would abort mid-upload on real corpora — these
    /// ride the deadline-exempt client (the same reasoning as `put_source`).
    async fn write_json_large_once<B, T>(&self, path: &str, body: B) -> Result<T>
    where
        B: serde::Serialize + Send + 'static,
        T: DeserializeOwned,
    {
        let req = self
            .http_stream
            .post(self.url(path))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(streaming_json_body(body));
        match req.send().await {
            Ok(resp) => Self::decode(resp).await,
            Err(e) => Err(Self::transport_err(e)),
        }
    }

    /// Bodyless send-once write. POST for every action route; DELETE only
    /// for the one delete in the surface (authoring draft cleanup).
    async fn write_empty_once<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T> {
        let req = self.http.request(method, self.url(path)).query(query);
        match req.send().await {
            Ok(resp) => Self::decode(resp).await,
            Err(e) => Err(Self::transport_err(e)),
        }
    }

    /// Idempotent-read path for a text (non-JSON) body — chronology rules
    /// come back as the applied YAML verbatim. Same retry class as
    /// `read_request` (which is JSON-typed and cannot host this decoder);
    /// both loops are thin shells over `decode_error`/`transport_err`.
    async fn get_text(&self, path: &str) -> Result<String> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let outcome = match self.http.get(self.url(path)).send().await {
                Ok(resp) if resp.status().is_success() => {
                    resp.text().await.map_err(Self::transport_err)
                }
                Ok(resp) => Err(Self::decode_error(resp).await),
                Err(e) => Err(Self::transport_err(e)),
            };
            match outcome {
                Err(e) if e.is_transient() && attempt <= self.read_retries => {
                    crate::retry::jitter_sleep(attempt).await;
                }
                other => return other,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// commands
// ---------------------------------------------------------------------------

#[async_trait]
impl CommandsPlane for RestTransport {
    async fn create_version(
        &self,
        req: dto::CreateVersionRequest,
        idem: IdemKey,
    ) -> Result<dto::CreateVersionResponse> {
        self.command_json(reqwest::Method::POST, "/v1/versions", &req, idem)
            .await
    }

    async fn propose_claim(
        &self,
        version_id: &str,
        req: dto::ProposeClaimRequest,
        idem: IdemKey,
    ) -> Result<dto::ProposeClaimResponse> {
        self.command_json(
            reqwest::Method::POST,
            &format!("/v1/versions/{}/claims", seg(version_id)),
            &req,
            idem,
        )
        .await
    }

    async fn append_events(
        &self,
        version_id: &str,
        req: dto::AppendEventsRequest,
        idem: IdemKey,
    ) -> Result<dto::AppendEventsResponse> {
        self.command_json(
            reqwest::Method::POST,
            &format!("/v1/versions/{}/events", seg(version_id)),
            &req,
            idem,
        )
        .await
    }

    async fn open_promise(
        &self,
        version_id: &str,
        req: dto::OpenPromiseRequest,
        idem: IdemKey,
    ) -> Result<dto::PromiseDto> {
        self.command_json(
            reqwest::Method::POST,
            &format!("/v1/versions/{}/promises", seg(version_id)),
            &req,
            idem,
        )
        .await
    }

    async fn fulfill_promise(
        &self,
        version_id: &str,
        key: &str,
        idem: IdemKey,
    ) -> Result<dto::FulfillPromiseResponse> {
        self.command_json(
            reqwest::Method::POST,
            &format!(
                "/v1/versions/{}/promises/{}/fulfill",
                seg(version_id),
                seg(key)
            ),
            &serde_json::json!({}),
            idem,
        )
        .await
    }

    async fn lock_anchor(
        &self,
        version_id: &str,
        req: dto::LockAnchorRequest,
        idem: IdemKey,
    ) -> Result<dto::AnchorDto> {
        self.command_json(
            reqwest::Method::POST,
            &format!("/v1/versions/{}/anchors", seg(version_id)),
            &req,
            idem,
        )
        .await
    }

    async fn record_counts(
        &self,
        version_id: &str,
        req: dto::RecordCountsRequest,
        idem: IdemKey,
    ) -> Result<()> {
        let _: dto::OkResponse = self
            .command_json(
                reqwest::Method::POST,
                &format!("/v1/versions/{}/counters", seg(version_id)),
                &req,
                idem,
            )
            .await?;
        Ok(())
    }

    async fn upsert_digest(&self, digest: dto::DigestDto) -> Result<()> {
        let path = format!("/v1/versions/{}/digests", seg(&digest.version_id));
        let req = self
            .http
            .put(self.url(&path))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&digest);
        let _: dto::OkResponse = match req.send().await {
            Ok(resp) => Self::decode(resp).await?,
            Err(e) => return Err(Self::transport_err(e)),
        };
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// query
// ---------------------------------------------------------------------------

#[async_trait]
impl QueryPlane for RestTransport {
    async fn head(&self, version_id: &str) -> Result<u64> {
        let resp: dto::HeadResponse = self
            .get_json(&format!("/v1/versions/{}/head", seg(version_id)), &[])
            .await?;
        Ok(resp.head_seq)
    }

    async fn get_claim(&self, claim_id: &str) -> Result<dto::GetClaimResponse> {
        self.get_json(&format!("/v1/claims/{}", seg(claim_id)), &[])
            .await
    }

    async fn facts(&self, version_id: &str, q: FactsQuery) -> Result<dto::FactsResponse> {
        let mut params: Vec<(&str, String)> = Vec::new();
        if let Some(p) = q.scope_prefix {
            params.push(("scope_prefix", p));
        }
        if let Some(s) = q.as_of_seq {
            params.push(("as_of_seq", s.to_string()));
        }
        if !q.statuses.is_empty() {
            let joined = q
                .statuses
                .iter()
                .map(|s| match s {
                    dto::ClaimStatusDto::Accepted => "accepted",
                    dto::ClaimStatusDto::Disputed => "disputed",
                })
                .collect::<Vec<_>>()
                .join(",");
            params.push(("statuses", joined));
        }
        if let Some(n) = q.limit {
            params.push(("limit", n.to_string()));
        }
        self.get_json(&format!("/v1/versions/{}/facts", seg(version_id)), &params)
            .await
    }

    async fn lineage(&self, version_id: &str) -> Result<dto::LineageResponse> {
        self.get_json(&format!("/v1/versions/{}/lineage", seg(version_id)), &[])
            .await
    }

    async fn anchors(
        &self,
        version_id: &str,
        as_of_seq: Option<u64>,
    ) -> Result<dto::AnchorsResponse> {
        self.get_json(
            &format!("/v1/versions/{}/anchors", seg(version_id)),
            &pin_query(as_of_seq),
        )
        .await
    }

    async fn promises(
        &self,
        version_id: &str,
        as_of_seq: Option<u64>,
        status: Option<&str>,
    ) -> Result<dto::PromisesResponse> {
        crate::planes::check_promise_status(status)?;
        let mut params = pin_query(as_of_seq);
        if let Some(s) = status {
            params.push(("status", s.to_string()));
        }
        self.get_json(
            &format!("/v1/versions/{}/promises", seg(version_id)),
            &params,
        )
        .await
    }

    async fn counters(
        &self,
        version_id: &str,
        as_of_seq: Option<u64>,
    ) -> Result<dto::CountersResponse> {
        self.get_json(
            &format!("/v1/versions/{}/counters", seg(version_id)),
            &pin_query(as_of_seq),
        )
        .await
    }

    async fn digests(&self, version_id: &str) -> Result<dto::DigestsResponse> {
        self.get_json(&format!("/v1/versions/{}/digests", seg(version_id)), &[])
            .await
    }

    async fn findings(&self, version_id: &str, q: FindingsQuery) -> Result<dto::FindingsResponse> {
        let mut params: Vec<(&'static str, String)> = Vec::new();
        if let Some(n) = q.as_of_seq {
            params.push(("as_of_seq", n.to_string()));
        }
        push_opt(&mut params, "severity", q.severity.as_deref());
        push_opt(&mut params, "rule_id", q.rule_id.as_deref());
        if let Some(n) = q.limit {
            params.push(("limit", n.to_string()));
        }
        self.get_json(
            &format!("/v1/versions/{}/findings", seg(version_id)),
            &params,
        )
        .await
    }

    async fn compose_context(
        &self,
        version_id: &str,
        q: ContextQuery,
    ) -> Result<dto::ComposedContextDto> {
        let mut params: Vec<(&str, String)> = Vec::new();
        if let Some(s) = q.scope {
            params.push(("scope", s));
        }
        if let Some(b) = q.budget_tokens {
            params.push(("budget_tokens", b.to_string()));
        }
        if let Some(f) = q.fact_limit {
            params.push(("fact_limit", f.to_string()));
        }
        if let Some(s) = q.as_of_seq {
            params.push(("as_of_seq", s.to_string()));
        }
        self.get_json(
            &format!("/v1/versions/{}/context", seg(version_id)),
            &params,
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// ingest
// ---------------------------------------------------------------------------

#[async_trait]
impl IngestPlane for RestTransport {
    async fn put_source(
        &self,
        meta: SourceMeta,
        chunks: ChunkSource,
    ) -> Result<dto::PutSourceResponse> {
        // Uploads are idempotent by content address, so transient failures
        // retry — the ChunkSource factory rebuilds the body per attempt.
        // Constant-memory: the payload is never buffered whole.
        let mut attempt = 0;
        loop {
            attempt += 1;
            let body =
                reqwest::Body::wrap_stream(chunks().map(Ok::<Vec<u8>, std::convert::Infallible>));
            let mut req = self.http_stream.put(self.url("/v1/sources")).header(
                reqwest::header::CONTENT_TYPE,
                meta.media_type
                    .as_deref()
                    .unwrap_or("application/octet-stream"),
            );
            if !meta.declared_sha256.is_empty() {
                req = req.header("x-content-sha256", &meta.declared_sha256);
            }
            if let Some(f) = &meta.filename {
                req = req.header("x-filename", f);
            }
            if let Some(s) = &meta.shape_ref {
                req = req.header("x-shape-ref", s);
            }
            let outcome = match req.body(body).send().await {
                Ok(resp) => Self::decode(resp).await,
                Err(e) => Err(Self::transport_err(e)),
            };
            match outcome {
                Err(e) if e.is_transient() && attempt <= self.read_retries => {
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
        self.write_json_once(
            reqwest::Method::POST,
            &format!("/v1/versions/{}/ingests", seg(version_id)),
            &[],
            &req,
        )
        .await
    }

    async fn ingest(&self, file: dto::IngestFileRequest) -> Result<dto::IngestResultDto> {
        self.write_json_large_once("/v1/ingest", file).await
    }

    async fn ingest_batch(&self, req: dto::IngestBatchRequest) -> Result<dto::IngestBatchResponse> {
        check_bulk_chunk_size("batch", req.files.len())?;
        self.write_json_large_once("/v1/ingest/batch", req).await
    }

    async fn bulk_open(&self, req: dto::BulkOpenRequest) -> Result<dto::BulkOpenResponse> {
        self.write_json_large_once("/v1/ingest/bulk", req).await
    }

    async fn bulk_chunk(
        &self,
        bulk_id: &str,
        files: Vec<dto::IngestFileRequest>,
    ) -> Result<dto::BulkChunkResponse> {
        check_bulk_chunk_size("bulk chunk", files.len())?;
        self.write_json_large_once(
            &format!("/v1/ingest/bulk/{}/chunk", seg(bulk_id)),
            dto::BulkChunkRequest { files },
        )
        .await
    }

    async fn bulk_status(
        &self,
        bulk_id: &str,
        include_needed: bool,
    ) -> Result<dto::BulkStatusResponse> {
        let params: Vec<(&str, String)> = if include_needed {
            vec![("include_needed", "true".into())]
        } else {
            Vec::new()
        };
        self.get_json(&format!("/v1/ingest/bulk/{}", seg(bulk_id)), &params)
            .await
    }

    async fn bulk_complete(&self, bulk_id: &str) -> Result<dto::BulkCompleteResponse> {
        self.write_empty_once(
            reqwest::Method::POST,
            &format!("/v1/ingest/bulk/{}/complete", seg(bulk_id)),
            &[],
        )
        .await
    }

    async fn get_source(&self, source_id: &str) -> Result<dto::SourceInfoDto> {
        self.get_json(&format!("/v1/sources/{}", seg(source_id)), &[])
            .await
    }
}

// ---------------------------------------------------------------------------
// retrieval
// ---------------------------------------------------------------------------

#[async_trait]
impl RetrievalPlane for RestTransport {
    async fn search(&self, req: dto::SearchRequest) -> Result<dto::SearchResponse> {
        // A read that happens to be a POST — same retry class as GETs.
        self.post_json_read("/v1/search", &req).await
    }

    async fn index_status(&self, shape_ref: &str) -> Result<dto::IndexStatusResponse> {
        self.get_json(&format!("/v1/indexes/{}", seg(shape_ref)), &[])
            .await
    }

    async fn build_index(
        &self,
        shape_ref: &str,
        version_id: Option<&str>,
    ) -> Result<dto::IndexStatusResponse> {
        self.write_empty_once(
            reqwest::Method::POST,
            &format!("/v1/indexes/{}/build", seg(shape_ref)),
            &version_query(version_id),
        )
        .await
    }

    async fn create_collection(
        &self,
        req: dto::CreateCollectionRequest,
    ) -> Result<dto::CollectionDto> {
        self.write_json_once(reqwest::Method::POST, "/v1/collections", &[], &req)
            .await
    }

    async fn list_collections(&self) -> Result<dto::CollectionsResponse> {
        self.get_json("/v1/collections", &[]).await
    }

    async fn get_collection(&self, id: &str) -> Result<dto::CollectionDto> {
        self.get_json(&format!("/v1/collections/{}", seg(id)), &[])
            .await
    }
}

// ---------------------------------------------------------------------------
// runbooks + shapes
// ---------------------------------------------------------------------------

#[async_trait]
impl RunbooksPlane for RestTransport {
    async fn apply_shape(
        &self,
        yaml: &str,
        version_id: Option<&str>,
    ) -> Result<dto::ApplyShapeResponse> {
        self.write_yaml_once("/v1/shapes", &version_query(version_id), yaml)
            .await
    }

    async fn apply_runbook(&self, yaml: &str) -> Result<dto::ApplyRunbookResponse> {
        self.write_yaml_once("/v1/runbooks", &[], yaml).await
    }

    async fn run_runbook(
        &self,
        name: &str,
        version_id: Option<&str>,
    ) -> Result<dto::RunbookRunResponse> {
        self.write_empty_once(
            reqwest::Method::POST,
            &format!("/v1/runbooks/{}/runs", seg(name)),
            &version_query(version_id),
        )
        .await
    }

    async fn get_run(&self, run_id: &str) -> Result<dto::RunStatusResponse> {
        self.get_json(&format!("/v1/runs/{}", seg(run_id)), &[])
            .await
    }

    async fn approve_step(&self, run_id: &str, ordinal: u32) -> Result<dto::RunbookRunResponse> {
        self.write_empty_once(
            reqwest::Method::POST,
            &format!("/v1/runs/{}/steps/{ordinal}/approve", seg(run_id)),
            &[],
        )
        .await
    }

    async fn list(&self, include_removed: bool) -> Result<dto::RunbooksResponse> {
        let params: Vec<(&str, String)> = if include_removed {
            vec![("include_removed", "true".into())]
        } else {
            Vec::new()
        };
        self.get_json("/v1/runbooks", &params).await
    }

    async fn get_info(&self, name: &str) -> Result<dto::RunbookInfoResponse> {
        self.get_json(&format!("/v1/runbooks/{}", seg(name)), &[])
            .await
    }

    async fn validate(
        &self,
        yaml: &str,
        opts: ValidateOptions,
    ) -> Result<dto::ValidateRunbookResponse> {
        let mut params: Vec<(&'static str, String)> = Vec::new();
        if opts.suggest {
            params.push(("suggest", "true".into()));
        }
        push_opt(&mut params, "provider", opts.provider.as_deref());
        push_opt(&mut params, "model", opts.model.as_deref());
        push_opt(&mut params, "tier", opts.tier.as_deref());
        self.write_yaml_once("/v1/runbooks/validate", &params, yaml)
            .await
    }

    async fn remove_request(&self, name: &str) -> Result<dto::RemovalRequestResponse> {
        self.write_empty_once(
            reqwest::Method::POST,
            &format!("/v1/runbooks/{}/remove-request", seg(name)),
            &[],
        )
        .await
    }

    async fn remove_confirm(
        &self,
        name: &str,
        removal_id: &str,
    ) -> Result<dto::RemovalConfirmResponse> {
        self.write_json_once(
            reqwest::Method::POST,
            &format!("/v1/runbooks/{}/remove-confirm", seg(name)),
            &[],
            &dto::RemovalConfirmRequest {
                removal_id: removal_id.to_string(),
            },
        )
        .await
    }

    async fn apply_chronology_rules(
        &self,
        yaml: &str,
    ) -> Result<dto::ApplyChronologyRulesResponse> {
        self.write_yaml_once("/v1/chronology-rules", &[], yaml)
            .await
    }

    async fn get_chronology_rules(&self, name: &str) -> Result<String> {
        self.get_text(&format!("/v1/chronology-rules/{}", seg(name)))
            .await
    }
}

// ---------------------------------------------------------------------------
// providers
// ---------------------------------------------------------------------------

#[async_trait]
impl ProvidersPlane for RestTransport {
    async fn apply_config(&self, yaml: &str) -> Result<dto::ApplyProviderConfigResponse> {
        self.write_yaml_once("/v1/providers", &[], yaml).await
    }

    async fn health(&self, name: &str) -> Result<dto::ProviderHealthResponse> {
        self.get_json(&format!("/v1/providers/{}/health", seg(name)), &[])
            .await
    }

    async fn health_ai(&self) -> Result<dto::HealthAiResponse> {
        self.get_json("/healthai", &[]).await
    }

    async fn complete(
        &self,
        name: &str,
        req: dto::CompleteRequest,
    ) -> Result<dto::CompleteResponse> {
        self.write_json_once(
            reqwest::Method::POST,
            &format!("/v1/providers/{}/complete", seg(name)),
            &[],
            &req,
        )
        .await
    }

    async fn embed(&self, name: &str, req: dto::EmbedRequest) -> Result<dto::EmbedResponse> {
        self.write_json_once(
            reqwest::Method::POST,
            &format!("/v1/providers/{}/embed", seg(name)),
            &[],
            &req,
        )
        .await
    }

    async fn list(&self) -> Result<dto::ProviderListResponse> {
        self.get_json("/v1/providers", &[]).await
    }

    async fn max_tokens(&self) -> Result<dto::MaxTokensResponse> {
        self.get_json("/v1/max-tokens", &[]).await
    }

    async fn replace_max_tokens(
        &self,
        budgets: &dto::MaxTokensBudgets,
    ) -> Result<dto::MaxTokensResponse> {
        // A whole-set replacement is a write like apply_config: sent once.
        // The DTO carries every field unconditionally, so a partial body —
        // which the server refuses — cannot be built from this method.
        self.write_json_once(reqwest::Method::POST, "/v1/max-tokens", &[], budgets)
            .await
    }
}

// ---------------------------------------------------------------------------
// sessions — including the streaming turn plane
// ---------------------------------------------------------------------------

#[async_trait]
impl SessionsPlane for RestTransport {
    async fn create(&self, runbook_name: &str) -> Result<dto::CreateSessionResponse> {
        self.write_empty_once(
            reqwest::Method::POST,
            &format!("/v1/runbooks/{}/sessions", seg(runbook_name)),
            &[],
        )
        .await
    }

    async fn turn(&self, session_id: &str, req: dto::TurnRequest) -> Result<dto::TurnResponse> {
        // A turn spends provider tokens — send-once, never auto-retried,
        // and DEADLINE-EXEMPT: a client-side abort does not stop the
        // server's paid completion (the transcript ordinal still advances),
        // so a 30 s cap on a capable-tier completion is a double-spend
        // invitation. The SSE variant is the way to watch a long turn.
        let path = format!("/v1/sessions/{}/turns", seg(session_id));
        let send = self
            .http_stream
            .post(self.url(&path))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&req)
            .send();
        match send.await {
            Ok(resp) => Self::decode(resp).await,
            Err(e) => Err(Self::transport_err(e)),
        }
    }

    async fn turn_stream(&self, session_id: &str, req: dto::TurnRequest) -> Result<TurnStream> {
        // No overall deadline (a capable-tier completion can exceed 30 s),
        // but a 60 s idle watchdog: the server heartbeats keep-alive
        // comments every 15 s, so a silent wire means a wedged peer and the
        // caller gets a typed transport error instead of hanging forever.
        let resp = self
            .http_sse
            .post(self.url(&format!("/v1/sessions/{}/turns/stream", seg(session_id))))
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&req)
            .send()
            .await
            .map_err(Self::transport_err)?;
        if !resp.status().is_success() {
            // Pre-stream failures (auth, refusals, shed) are plain
            // problem+json — decoded by the ONE error path, Retry-After
            // included.
            return Err(Self::decode_error(resp).await);
        }
        Ok(turn_event_stream(resp.bytes_stream().boxed()))
    }

    async fn get(&self, session_id: &str) -> Result<dto::SessionResponse> {
        self.get_json(&format!("/v1/sessions/{}", seg(session_id)), &[])
            .await
    }

    async fn close(&self, session_id: &str) -> Result<dto::SessionResponse> {
        self.write_empty_once(
            reqwest::Method::POST,
            &format!("/v1/sessions/{}/close", seg(session_id)),
            &[],
        )
        .await
    }
}

/// Classify one SSE event from the turn stream. Returns the item to yield
/// (None = skip) and whether it is terminal. Undecodable PROGRESS data is
/// skipped — a newer server may add stages this build cannot name, and
/// progress is informational — but an undecodable terminal event is an
/// error: the caller was owed a TurnResponse.
fn classify_turn_event(
    ev: crate::sse::SseEvent,
) -> Option<(Result<crate::planes::TurnStreamEvent>, bool)> {
    use crate::planes::TurnStreamEvent as E;
    match ev.event.as_str() {
        "progress" => serde_json::from_str::<dto::TurnProgressEvent>(&ev.data)
            .ok()
            .map(|p| (Ok(E::Progress(p)), false)),
        "done" => Some(match serde_json::from_str::<dto::TurnResponse>(&ev.data) {
            Ok(t) => (Ok(E::Done(Box::new(t))), true),
            Err(e) => (
                Err(MunariumError::Unexpected {
                    status: None,
                    detail: format!("undecodable SSE done event: {e}"),
                }),
                true,
            ),
        }),
        "error" => Some(match serde_json::from_str::<serde_json::Value>(&ev.data) {
            // The error event carries the same problem+json body the unary
            // route would have returned — decode through the one registry.
            Ok(body) => {
                let status = body["status"].as_u64().and_then(|s| u16::try_from(s).ok());
                (
                    Err(MunariumError::from_problem(
                        status.unwrap_or(500),
                        None,
                        &body,
                    )),
                    true,
                )
            }
            Err(e) => (
                Err(MunariumError::Unexpected {
                    status: None,
                    detail: format!("undecodable SSE error event: {e}"),
                }),
                true,
            ),
        }),
        _ => None, // unnamed/unknown events: ignored (forward-compat)
    }
}

/// Wrap the response byte stream into the typed turn-event stream. The
/// invariants live here: exactly one terminal item, everything after it
/// dropped, and a stream that ends WITHOUT a terminal event yields a typed
/// transport error — never a silent success.
type ByteStream =
    futures_core::stream::BoxStream<'static, std::result::Result<bytes::Bytes, reqwest::Error>>;

fn turn_event_stream(bytes: ByteStream) -> TurnStream {
    struct State {
        bytes: ByteStream,
        parser: crate::sse::SseParser,
        queue: std::collections::VecDeque<Result<crate::planes::TurnStreamEvent>>,
        terminal: bool,
    }
    let state = State {
        bytes,
        parser: crate::sse::SseParser::default(),
        queue: std::collections::VecDeque::new(),
        terminal: false,
    };
    Box::pin(futures_util::stream::unfold(state, |mut st| async move {
        loop {
            if let Some(item) = st.queue.pop_front() {
                return Some((item, st));
            }
            if st.terminal {
                return None;
            }
            match st.bytes.next().await {
                Some(Ok(chunk)) => match st.parser.push(&chunk) {
                    Ok(events) => {
                        for ev in events {
                            if st.terminal {
                                break; // nothing rides after the terminal event
                            }
                            if let Some((item, terminal)) = classify_turn_event(ev) {
                                st.queue.push_back(item);
                                st.terminal = terminal;
                            }
                        }
                    }
                    Err(_) => {
                        st.queue.push_back(Err(MunariumError::Unexpected {
                            status: None,
                            detail: format!(
                                "SSE peer exceeded the {} MiB event buffer without \
                                 completing an event",
                                crate::sse::MAX_EVENT_BYTES / (1024 * 1024)
                            ),
                        }));
                        st.terminal = true;
                    }
                },
                Some(Err(e)) => {
                    st.queue.push_back(Err(RestTransport::transport_err(e)));
                    st.terminal = true;
                }
                None => {
                    st.queue.push_back(Err(MunariumError::Transport {
                        detail: "SSE stream ended without a terminal done/error event".into(),
                        may_have_reached_server: true,
                    }));
                    st.terminal = true;
                }
            }
        }
    }))
}

// ---------------------------------------------------------------------------
// access tokens (mgmt)
// ---------------------------------------------------------------------------

#[async_trait]
impl TokensPlane for RestTransport {
    async fn mint(&self, req: dto::IssueTokenRequest) -> Result<dto::IssueTokenResponse> {
        self.write_json_once(reqwest::Method::POST, "/v1/access-tokens", &[], &req)
            .await
    }

    async fn list(&self, q: TokenListQuery) -> Result<dto::TokensResponse> {
        let mut params: Vec<(&'static str, String)> = Vec::new();
        push_opt(&mut params, "uid", q.uid.as_deref());
        if let Some(active) = q.active {
            params.push(("active", active.to_string()));
        }
        self.get_json("/v1/access-tokens", &params).await
    }

    async fn revoke(&self, jti: &str) -> Result<dto::RevokeTokenResponse> {
        self.write_empty_once(
            reqwest::Method::POST,
            &format!("/v1/access-tokens/{}/revoke", seg(jti)),
            &[],
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// reports (mgmt)
// ---------------------------------------------------------------------------

#[async_trait]
impl ReportsPlane for RestTransport {
    async fn usage(&self, q: UsageQuery) -> Result<dto::UsageResponse> {
        let mut params = Vec::new();
        push_opt(&mut params, "group_by", q.group_by.as_deref());
        push_opt(&mut params, "from", q.from.as_deref());
        push_opt(&mut params, "to", q.to.as_deref());
        self.get_json("/v1/reports/usage", &params).await
    }

    async fn audit(&self, q: AuditQuery) -> Result<dto::AuditResponse> {
        let mut params = Vec::new();
        push_opt(&mut params, "uid", q.uid.as_deref());
        push_opt(&mut params, "session_id", q.session_id.as_deref());
        push_opt(&mut params, "runbook", q.runbook.as_deref());
        push_opt(&mut params, "from", q.from.as_deref());
        push_opt(&mut params, "to", q.to.as_deref());
        if let Some(n) = q.limit {
            params.push(("limit", n.to_string()));
        }
        if q.bodies {
            params.push(("bodies", "true".into()));
        }
        push_opt(&mut params, "before", q.before.as_deref());
        self.get_json("/v1/reports/audit", &params).await
    }

    async fn cost(&self, from: Option<&str>, to: Option<&str>) -> Result<dto::CostResponse> {
        let mut params = Vec::new();
        push_opt(&mut params, "from", from);
        push_opt(&mut params, "to", to);
        self.get_json("/v1/reports/cost", &params).await
    }

    async fn timeseries(
        &self,
        window: Option<&str>,
        plane: Option<&str>,
    ) -> Result<dto::TimeseriesResponse> {
        let mut params = Vec::new();
        push_opt(&mut params, "window", window);
        push_opt(&mut params, "plane", plane);
        self.get_json("/v1/reports/timeseries", &params).await
    }

    async fn endpoints(
        &self,
        window: Option<&str>,
        limit: Option<i64>,
    ) -> Result<dto::EndpointsResponse> {
        let mut params = Vec::new();
        push_opt(&mut params, "window", window);
        if let Some(n) = limit {
            params.push(("limit", n.to_string()));
        }
        self.get_json("/v1/reports/endpoints", &params).await
    }

    async fn runbooks(&self, window: Option<&str>) -> Result<dto::RunbookReportResponse> {
        let mut params = Vec::new();
        push_opt(&mut params, "window", window);
        self.get_json("/v1/reports/runbooks", &params).await
    }

    async fn sessions(&self, window: Option<&str>) -> Result<dto::SessionsReportResponse> {
        let mut params = Vec::new();
        push_opt(&mut params, "window", window);
        self.get_json("/v1/reports/sessions", &params).await
    }

    async fn evidence(&self, window: Option<&str>) -> Result<dto::EvidenceReportResponse> {
        let mut params = Vec::new();
        push_opt(&mut params, "window", window);
        self.get_json("/v1/reports/evidence", &params).await
    }

    async fn matrix(&self) -> Result<dto::MatrixReportResponse> {
        // No window parameter: the route reports current breaker state and
        // the declared data views, neither of which is windowed.
        self.get_json("/v1/reports/matrix", &[]).await
    }
}

// ---------------------------------------------------------------------------
// guided authoring
// ---------------------------------------------------------------------------

#[async_trait]
impl AuthoringPlane for RestTransport {
    async fn list_patterns(&self) -> Result<dto::PatternsResponse> {
        self.get_json("/v1/authoring/patterns", &[]).await
    }

    async fn get_pattern(&self, id: &str) -> Result<dto::PatternDetailResponse> {
        self.get_json(&format!("/v1/authoring/patterns/{}", seg(id)), &[])
            .await
    }

    async fn create_draft(&self, req: dto::CreateDraftRequest) -> Result<dto::DraftResponse> {
        self.write_json_once(reqwest::Method::POST, "/v1/authoring/drafts", &[], &req)
            .await
    }

    async fn list_drafts(&self) -> Result<dto::DraftsResponse> {
        self.get_json("/v1/authoring/drafts", &[]).await
    }

    async fn get_draft(&self, draft_id: &str) -> Result<dto::DraftResponse> {
        self.get_json(&format!("/v1/authoring/drafts/{}", seg(draft_id)), &[])
            .await
    }

    async fn delete_draft(&self, draft_id: &str) -> Result<dto::DraftDeleteResponse> {
        self.write_empty_once(
            reqwest::Method::DELETE,
            &format!("/v1/authoring/drafts/{}", seg(draft_id)),
            &[],
        )
        .await
    }

    async fn put_answers(
        &self,
        draft_id: &str,
        req: dto::UpdateAnswersRequest,
    ) -> Result<dto::DraftResponse> {
        self.write_json_once(
            reqwest::Method::PUT,
            &format!("/v1/authoring/drafts/{}/answers", seg(draft_id)),
            &[],
            &req,
        )
        .await
    }

    async fn validate(&self, draft_id: &str) -> Result<dto::DraftValidationResponse> {
        self.write_empty_once(
            reqwest::Method::POST,
            &format!("/v1/authoring/drafts/{}/validate", seg(draft_id)),
            &[],
        )
        .await
    }

    async fn assist(
        &self,
        draft_id: &str,
        req: dto::AssistDraftRequest,
    ) -> Result<dto::AssistDraftResponse> {
        // A BYOK provider call rides behind this — send-once.
        self.write_json_once(
            reqwest::Method::POST,
            &format!("/v1/authoring/drafts/{}/assist", seg(draft_id)),
            &[],
            &req,
        )
        .await
    }

    async fn export(&self, draft_id: &str) -> Result<dto::ExportDraftResponse> {
        self.write_empty_once(
            reqwest::Method::POST,
            &format!("/v1/authoring/drafts/{}/export", seg(draft_id)),
            &[],
        )
        .await
    }

    async fn apply(&self, draft_id: &str) -> Result<dto::ApplyDraftResponse> {
        self.write_empty_once(
            reqwest::Method::POST,
            &format!("/v1/authoring/drafts/{}/apply", seg(draft_id)),
            &[],
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// meta
// ---------------------------------------------------------------------------

#[async_trait]
impl MetaPlane for RestTransport {
    async fn server_version(&self) -> Result<ServerVersionInfo> {
        self.get_json("/version", &[]).await
    }
}

/// `Retry-After` is either delta-seconds or an HTTP-date (RFC 9110 10.2.3).
/// Both forms yield a delay from now; a date in the past yields zero.
pub(crate) fn parse_retry_after(v: &str) -> Option<Duration> {
    let v = v.trim();
    if let Ok(secs) = v.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let secs = httpdate::parse_http_date(v)
        .ok()?
        .duration_since(std::time::SystemTime::now())
        .unwrap_or(Duration::ZERO);
    Some(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn large_json_encoding_is_equivalent_and_chunk_bounded() {
        let value = serde_json::json!({
            "files": [{
                "filename": "quotes-\"-and-unicode-ø.md",
                "content_base64": "a".repeat(200_000),
                "collections": ["support", "engineering"]
            }]
        });
        let stream = streaming_json_chunks(value.clone());
        futures_util::pin_mut!(stream);
        let mut encoded = Vec::new();
        let mut maximum_chunk = 0;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.expect("streamed serialization succeeds");
            maximum_chunk = maximum_chunk.max(chunk.len());
            encoded.extend_from_slice(&chunk);
        }
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&encoded).unwrap(),
            value
        );
        assert!(maximum_chunk <= JSON_BODY_CHUNK_BYTES);
        assert!(
            maximum_chunk < encoded.len(),
            "must not emit one giant buffer"
        );
    }
}

#[async_trait]
impl crate::planes::EvidencePlane for RestTransport {
    async fn evidence(&self, evidence_id: &str) -> Result<serde_json::Value> {
        self.get_json(&format!("/v1/evidence/{}", seg(evidence_id)), &[])
            .await
    }

    async fn evidence_rows(
        &self,
        evidence_id: &str,
        q: crate::planes::EvidenceRowsQuery,
    ) -> Result<dto::EvidenceRowsResponse> {
        let mut params: Vec<(&'static str, String)> = Vec::new();
        if let Some(n) = q.from {
            params.push(("from", n.to_string()));
        }
        if let Some(n) = q.limit {
            params.push(("limit", n.to_string()));
        }
        self.get_json(&format!("/v1/evidence/{}/rows", seg(evidence_id)), &params)
            .await
    }
}

/// The governing invariant of S-3.x, tested on the bytes this transport
/// actually puts on the wire: a caller that never names a research profile
/// must send and receive exactly what it sent and received before the field
/// existed. A `skip_serializing_if` dropped from either DTO is a silent wire
/// change no compiler catches, so it is pinned here rather than trusted.
#[cfg(test)]
mod hierarchy_wire_tests {
    use super::*;

    /// The body `turn` / `turn_stream` post is `.json(&req)` over this DTO,
    /// so serializing it here IS serializing the request.
    fn turn_body(req: &dto::TurnRequest) -> serde_json::Value {
        serde_json::to_value(req).expect("TurnRequest serializes")
    }

    #[test]
    fn a_turn_request_without_a_profile_gains_no_key() {
        let req = dto::TurnRequest {
            query: "vacation".into(),
            ..Default::default()
        };
        assert_eq!(
            turn_body(&req),
            serde_json::json!({ "query": "vacation", "complete": null }),
            "an unprofiled turn's body must be exactly what it was before \n             the research-profile field existed"
        );
    }

    #[test]
    fn a_named_profile_rides_the_documented_key() {
        let req = dto::TurnRequest {
            query: "holdings".into(),
            research_profile: Some("register-first".into()),
            ..Default::default()
        };
        assert_eq!(
            turn_body(&req)["research_profile"],
            serde_json::json!("register-first")
        );
    }

    #[test]
    fn a_response_without_a_hierarchy_round_trips_without_the_key() {
        let legacy = serde_json::json!({
            "session_id": "s-1",
            "ordinal": 1,
            "collections_searched": ["docs"],
            "skipped": [],
            "hits": [],
            "envelopes": []
        });
        let decoded: dto::TurnResponse =
            serde_json::from_value(legacy.clone()).expect("a legacy body still decodes");
        assert!(decoded.hierarchy.is_none());
        assert_eq!(
            serde_json::to_value(&decoded).expect("re-serializes"),
            legacy,
            "a legacy turn must not grow a `hierarchy` key on the way back out"
        );
    }

    #[test]
    fn a_hierarchy_decision_decodes_whole() {
        let body = serde_json::json!({
            "session_id": "s-1",
            "ordinal": 2,
            "collections_searched": ["docs"],
            "skipped": [],
            "hits": [],
            "envelopes": [],
            "hierarchy": {
                "profile": "register-first",
                "intent_kind": "enumerate",
                "intent_explicit": true,
                "layers": [{
                    "layer": "register",
                    "role": "controlling",
                    "requirement": "required",
                    "block": "complete_table",
                    "evidence_id": "ev-7",
                    "supports_completeness": true,
                    "elapsed_ms": 42
                }, {
                    "layer": "documents",
                    "role": "supporting",
                    "requirement": "optional",
                    "block": "refusal",
                    "supports_completeness": false,
                    "refusal_code": "evidence-on-hold",
                    "elapsed_ms": 3
                }],
                "completeness_available": true,
                "disclosed_conflicts": 1,
                "conflicts_policy": "disclose"
            }
        });
        let decoded: dto::TurnResponse = serde_json::from_value(body).expect("decodes");
        let h = decoded.hierarchy.expect("hierarchy present");
        assert_eq!(h.profile, "register-first");
        assert_eq!(h.layers.len(), 2);
        assert_eq!(h.layers[0].evidence_id.as_deref(), Some("ev-7"));
        // Absent, not empty: a refusal that named no reason and a layer that
        // produced no evidence must stay distinguishable from ones that did.
        assert_eq!(h.layers[0].refusal_code, None);
        assert_eq!(h.layers[1].evidence_id, None);
        assert_eq!(
            h.layers[1].refusal_code.as_deref(),
            Some("evidence-on-hold")
        );
        assert_eq!(h.disclosed_conflicts, 1);
    }

    /// Push one `progress` SSE event through the transport's own classifier.
    /// None = the classifier skipped it.
    fn progress(json: serde_json::Value) -> Option<dto::TurnProgressEvent> {
        let ev = crate::sse::SseEvent {
            event: "progress".into(),
            data: json.to_string(),
        };
        match classify_turn_event(ev) {
            Some((Ok(crate::planes::TurnStreamEvent::Progress(p)), false)) => Some(p),
            None => None,
            Some((item, terminal)) => {
                panic!("expected a non-terminal progress item, got terminal={terminal} {item:?}")
            }
        }
    }

    #[test]
    fn the_six_hierarchy_stages_classify_as_progress() {
        use dto::TurnProgressEvent as P;
        assert!(matches!(
            progress(serde_json::json!({
                "stage": "profile", "profile": "register-first",
                "layers": ["register", "documents"], "intent_explicit": false
            })),
            Some(P::Profile {
                intent_kind: None,
                ..
            })
        ));
        assert!(matches!(
            progress(serde_json::json!({
                "stage": "layer_start", "layer": "register",
                "role": "controlling", "requirement": "required"
            })),
            Some(P::LayerStart { .. })
        ));
        assert!(matches!(
            progress(serde_json::json!({
                "stage": "layer_source", "layer": "register",
                "source": "holdings@1", "provider": "matrix"
            })),
            Some(P::LayerSource { .. })
        ));
        assert!(matches!(
            progress(serde_json::json!({
                "stage": "layer_complete", "layer": "register",
                "block": "complete_table", "supports_completeness": true,
                "elapsed_ms": 42
            })),
            Some(P::LayerComplete {
                refusal_code: None,
                ..
            })
        ));
        assert!(matches!(
            progress(serde_json::json!({
                "stage": "coverage", "completeness_available": true,
                "disclosed_conflicts": 2
            })),
            Some(P::Coverage {
                disclosed_conflicts: 2,
                ..
            })
        ));
        assert!(matches!(
            progress(serde_json::json!({
                "stage": "compose", "layers_used": 2, "context_chars": 8000,
                "layers_dropped": ["documents"]
            })),
            Some(P::Compose { layers_used: 2, .. })
        ));
    }

    #[test]
    fn the_verify_stages_new_layer_field_is_optional_both_ways() {
        use dto::TurnProgressEvent as P;
        // The original shape carries no `layer`, and a server older than
        // this client emits exactly that.
        let legacy = serde_json::json!({
            "stage": "verify", "attempt": 0, "checks": ["quotes"], "violations": 0
        });
        assert!(matches!(
            progress(legacy.clone()),
            Some(P::Verify { layer: None, .. })
        ));
        let decoded: P = serde_json::from_value(legacy.clone()).expect("decodes");
        assert_eq!(
            serde_json::to_value(decoded).expect("re-serializes"),
            legacy,
            "a layerless verify event must not grow a `layer` key"
        );
        assert!(matches!(
            progress(serde_json::json!({
                "stage": "verify", "attempt": 1, "checks": ["citations"],
                "violations": 2, "layer": "register"
            })),
            Some(P::Verify { layer: Some(_), .. })
        ));
    }

    #[test]
    fn a_stage_this_build_cannot_name_is_skipped_not_fatal() {
        // Progress is informational; a newer server's stage must not end a
        // stream whose caller is owed a TurnResponse.
        assert!(progress(serde_json::json!({ "stage": "some_future_stage" })).is_none());
    }
}

/// The two `/v1/max-tokens` routes, driven through the REAL transport
/// against a one-exchange canned HTTP responder on loopback (no live
/// server): what goes on the wire (method, path, the eight required fields)
/// and what comes back (the flattened set, `source`, `updated_at`, a
/// problem+json refusal) are asserted on bytes, not on serde derives alone.
#[cfg(test)]
mod max_tokens_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const FIELDS: [&str; 8] = [
        "turn_completion",
        "query_expansion",
        "complete_default",
        "healthai_probe",
        "hierarchy_classifier",
        "hierarchy_intent",
        "runbook_advisory",
        "authoring_assist",
    ];

    /// What the responder saw: the request line, the raw header block, and
    /// the body bytes (exactly `Content-Length` of them).
    struct Captured {
        request_line: String,
        headers: String,
        body: Vec<u8>,
    }

    /// Serve exactly one HTTP/1.1 exchange: capture the request, answer
    /// with the canned status + JSON body, close. Returns the base URL to
    /// point a transport at and the handle that yields the capture.
    async fn canned_responder(
        status: u16,
        content_type: &'static str,
        body: String,
    ) -> (String, tokio::task::JoinHandle<Captured>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let base = format!("http://{}", listener.local_addr().expect("local addr"));
        let handle = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.expect("accept");
            let mut buf = Vec::new();
            let header_end = loop {
                let mut chunk = [0u8; 4096];
                let n = sock.read(&mut chunk).await.expect("read request");
                assert!(n > 0, "peer closed before the request head completed");
                buf.extend_from_slice(&chunk[..n]);
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos + 4;
                }
            };
            let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            while buf.len() < header_end + content_length {
                let mut chunk = [0u8; 4096];
                let n = sock.read(&mut chunk).await.expect("read body");
                assert!(n > 0, "peer closed mid-body");
                buf.extend_from_slice(&chunk[..n]);
            }
            let reason = match status {
                200 => "OK",
                400 => "Bad Request",
                403 => "Forbidden",
                _ => "Canned",
            };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\ncontent-type: {content_type}\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(response.as_bytes())
                .await
                .expect("write response");
            let _ = sock.shutdown().await;
            Captured {
                request_line: headers.lines().next().unwrap_or_default().to_string(),
                headers,
                body: buf[header_end..header_end + content_length].to_vec(),
            }
        });
        (base, handle)
    }

    fn transport(base: &str) -> RestTransport {
        RestTransport::new(
            MunariumClientOptions::new(base)
                .token("devtoken")
                .uid("user-1")
                .read_retries(0),
        )
        .expect("transport builds")
    }

    fn budgets() -> dto::MaxTokensBudgets {
        dto::MaxTokensBudgets {
            turn_completion: 8192,
            query_expansion: 64,
            complete_default: 4096,
            healthai_probe: 1,
            hierarchy_classifier: 48,
            hierarchy_intent: 600,
            runbook_advisory: 4096,
            authoring_assist: 65536,
        }
    }

    /// The GET shape: the eight budgets FLATTENED beside `source`.
    fn tenant_answer(b: dto::MaxTokensBudgets, updated_at: &str) -> serde_json::Value {
        let mut answer = serde_json::to_value(b).expect("budgets serialize");
        answer["source"] = "tenant".into();
        answer["updated_at"] = updated_at.into();
        answer
    }

    #[tokio::test]
    async fn get_decodes_the_flattened_set_its_source_and_updated_at() {
        let expected = budgets();
        let (base, responder) = canned_responder(
            200,
            "application/json",
            tenant_answer(expected, "2026-09-02T10:00:00Z").to_string(),
        )
        .await;
        let resp = transport(&base).max_tokens().await.expect("GET decodes");
        let seen = responder.await.expect("responder finished");
        assert_eq!(seen.request_line, "GET /v1/max-tokens HTTP/1.1");
        assert_eq!(resp.source, "tenant");
        assert_eq!(resp.updated_at.as_deref(), Some("2026-09-02T10:00:00Z"));
        assert_eq!(resp.budgets, expected, "all eight fields decode");
        // A GET body round-trips into a POST body: the decoded budgets
        // re-serialize to exactly the eight required keys, nothing else.
        let body = serde_json::to_value(resp.budgets).expect("re-serializes");
        let keys: Vec<&str> = body
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let mut expected_keys = FIELDS.to_vec();
        expected_keys.sort_unstable();
        let mut got = keys.clone();
        got.sort_unstable();
        assert_eq!(got, expected_keys);
    }

    #[test]
    fn an_environment_answer_has_no_updated_at() {
        // `updated_at` is ABSENT (not null) while the process defaults
        // apply; the client must read that as None, not as an error.
        let mut body = serde_json::to_value(dto::MaxTokensBudgets::default()).unwrap();
        body["source"] = "environment".into();
        let resp: dto::MaxTokensResponse = serde_json::from_value(body).expect("decodes");
        assert_eq!(resp.source, "environment");
        assert_eq!(resp.updated_at, None);
        assert_eq!(resp.budgets, dto::MaxTokensBudgets::default());
    }

    #[tokio::test]
    async fn replace_posts_all_eight_fields_and_decodes_the_answer() {
        let sent_budgets = budgets();
        let (base, responder) = canned_responder(
            200,
            "application/json",
            tenant_answer(sent_budgets, "2026-09-02T11:30:00Z").to_string(),
        )
        .await;
        let resp = transport(&base)
            .replace_max_tokens(&sent_budgets)
            .await
            .expect("POST decodes");
        let seen = responder.await.expect("responder finished");
        assert_eq!(seen.request_line, "POST /v1/max-tokens HTTP/1.1");
        let lower = seen.headers.to_ascii_lowercase();
        assert!(
            lower.contains("content-type: application/json"),
            "JSON body must be declared: {}",
            seen.headers
        );
        assert!(lower.contains("authorization: bearer devtoken"));
        assert!(lower.contains("x-munarium-uid: user-1"));
        let sent: serde_json::Value = serde_json::from_slice(&seen.body).expect("JSON body");
        let obj = sent.as_object().expect("an object");
        for field in FIELDS {
            assert!(
                obj.contains_key(field),
                "{field} missing from the POST body — the server refuses a partial set"
            );
        }
        assert_eq!(obj.len(), FIELDS.len(), "no extra keys ride the body");
        assert_eq!(sent["turn_completion"], 8192);
        assert_eq!(sent["authoring_assist"], 65536);
        assert_eq!(resp.budgets, sent_budgets);
        assert_eq!(resp.source, "tenant");
        assert_eq!(resp.updated_at.as_deref(), Some("2026-09-02T11:30:00Z"));
    }

    #[tokio::test]
    async fn an_out_of_range_field_is_the_typed_invalid_input() {
        let problem = serde_json::json!({
            "type": "https://munarium.ioka.io/problems/invalid-input",
            "title": "invalid input",
            "status": 400,
            "detail": "query_expansion must be within 32..=512 (got 4096)",
        });
        let (base, responder) =
            canned_responder(400, "application/problem+json", problem.to_string()).await;
        let bad = dto::MaxTokensBudgets {
            query_expansion: 4096,
            ..Default::default()
        };
        let err = transport(&base)
            .replace_max_tokens(&bad)
            .await
            .expect_err("a 400 is an error");
        responder.await.expect("responder finished");
        match err {
            MunariumError::InvalidInput { detail } => {
                assert!(detail.contains("query_expansion"), "{detail}");
            }
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_non_rw_replacement_is_the_typed_forbidden() {
        let problem = serde_json::json!({
            "type": "https://munarium.ioka.io/problems/forbidden",
            "title": "forbidden",
            "status": 403,
            "detail": "max-tokens replacement requires the rw role",
        });
        let (base, responder) =
            canned_responder(403, "application/problem+json", problem.to_string()).await;
        let err = transport(&base)
            .replace_max_tokens(&budgets())
            .await
            .expect_err("a 403 is an error");
        responder.await.expect("responder finished");
        assert!(
            matches!(err, MunariumError::Forbidden { .. }),
            "expected Forbidden, got {err:?}"
        );
    }
}
