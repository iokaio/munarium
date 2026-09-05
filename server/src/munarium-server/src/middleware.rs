// SPDX-License-Identifier: Apache-2.0
//! The platform's cross-cutting layers: the uid contract + interaction capture.
//!
//! REST: one axum middleware buffers request/response bodies (capped for
//! capture; pass-through unchanged), enforces X-Munarium-Uid on /v1, stamps a
//! request id, opens the tracing span, and records the interaction.
//! Streaming responses (`text/event-stream`, 2026-08-23) are the one
//! exception to response buffering: the body passes through frame by frame
//! and the interaction row is recorded at END of stream by a body wrapper
//! (the same shape the gRPC plane uses for trailers), with the handler's
//! [`StreamOutcome`] slot supplying the attribution meta and the real
//! terminal status the stream carried inside its last event.
//!
//! gRPC: a tower layer over the tonic routes enforces munarium-uid metadata on
//! /mmp.v1.* calls and records the interaction envelope (method, uid,
//! tenant, latency). Proto bodies are not captured — the REST plane is the
//! full-body audit surface; gRPC rows carry the envelope only.

use crate::error::{ApiError, CustomError};
use crate::interactions::{self, InteractionMeta, InteractionRecord};
use crate::state::{AppState, Principal};
use axum::body::Body;
use axum::response::IntoResponse;
use std::sync::Arc;
use std::time::Instant;
use tracing::Instrument;

/// Request-id extension available to handlers (also echoed as
/// `x-munarium-request-id` on every response). The field is read by future
/// handler consumers; the extension itself is the contract.
#[derive(Debug, Clone)]
pub struct RequestId(#[allow(dead_code)] pub String);

/// The uid the request acts as (header-asserted, mismatch-checked against
/// JWT `sub` here so handlers can trust it).
#[derive(Debug, Clone)]
pub struct Uid(pub String);

/// The attribution contract for STREAMING handlers (`text/event-stream`).
/// A unary handler attaches an [`InteractionMeta`] to its finished response;
/// a stream has no finished response when its head goes out, so the handler
/// instead inserts a shared [`StreamOutcomeSlot`] into the response
/// extensions and fills it when the outcome is known (before it ends the
/// stream). The capture middleware reads the slot at end-of-stream — the
/// session/runbook attribution lands on the row exactly as on the unary
/// plane, and `status` lets the row record the REAL outcome (the HTTP
/// status of a stream is always 200; the failure rides its terminal
/// `error` event). A handler that streams without inserting a slot is still
/// recorded — generic meta, HTTP status — never silently dropped.
#[derive(Debug, Clone, Default)]
pub struct StreamOutcome {
    pub meta: InteractionMeta,
    /// The outcome the stream's terminal event carried: 200 for `done`,
    /// the problem status for `error`. None until the handler fills it.
    pub status: Option<u16>,
}

pub type StreamOutcomeSlot = Arc<std::sync::Mutex<StreamOutcome>>;

pub fn new_stream_outcome_slot() -> StreamOutcomeSlot {
    Arc::new(std::sync::Mutex::new(StreamOutcome::default()))
}

/// The acting uid, or the `anonymous` sentinel — the ONE place the fallback
/// lives. The capture middleware always inserts the `Uid` extension on every
/// /v1 request, so `None` only occurs on a route added outside the middleware.
pub fn uid_or_anonymous(uid: Option<&axum::Extension<Uid>>) -> String {
    uid.map(|axum::Extension(u)| u.0.clone())
        .unwrap_or_else(|| "anonymous".to_string())
}

pub fn new_request_id() -> String {
    format!("req-{}", uuid::Uuid::now_v7().simple())
}

fn header_uid(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get("x-munarium-uid")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// RED metrics for one served REST request. `route` is the matched-path
/// TEMPLATE, never the raw path (metrics.rs cardinality rules).
fn record_http_metrics(state: &AppState, route: &str, method: &str, status: u16, seconds: f64) {
    state.metrics.inc(
        "munarium_http_requests_total",
        crate::metrics::labels(&[
            ("plane", "rest"),
            ("route", route),
            ("method", method),
            ("status_class", crate::metrics::status_class(status)),
        ]),
    );
    state.metrics.observe(
        "munarium_http_request_duration_seconds",
        crate::metrics::labels(&[("plane", "rest"), ("route", route)]),
        seconds,
    );
}

/// The REST capture middleware. Applied to the whole router; only /v1 paths
/// are subject to the uid contract and recorded (meta routes — health,
/// version, docs, openapi, healthai — stay exempt from CAPTURE, but every
/// route including meta is counted in the RED metrics).
pub async fn capture(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let path = req.uri().path().to_string();
    let http_method = req.method().to_string();
    // The route TEMPLATE (`/v1/versions/{version_id}/facts`) — bounded
    // cardinality for the metrics labels; unmatched paths (404s) fall back
    // to the literal "(unmatched)" so junk paths cannot mint series.
    let route = req
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| "(unmatched)".to_string());
    let started = Instant::now();
    if !path.starts_with("/v1/") {
        let response = next.run(req).await;
        record_http_metrics(
            &state,
            &route,
            &http_method,
            response.status().as_u16(),
            started.elapsed().as_secs_f64(),
        );
        return response;
    }

    // Load shed BEFORE any auth or buffering work: at the concurrency
    // ceiling, refuse immediately with 503 `overloaded` + Retry-After
    // rather than queueing into a latency collapse. The permit is held for
    // the rest of this function (request lifetime).
    let _permit = match state.rest_permits.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            state.metrics.inc("munarium_load_shed_total", String::new());
            let mut r = ApiError::Custom(CustomError::overloaded()).into_response();
            r.headers_mut().insert(
                axum::http::header::RETRY_AFTER,
                axum::http::HeaderValue::from_static("1"),
            );
            record_http_metrics(
                &state,
                &route,
                &http_method,
                r.status().as_u16(),
                started.elapsed().as_secs_f64(),
            );
            return r;
        }
    };

    // Attribution sniff (authorization itself stays in the handlers) + the
    // one middleware-owned authz rule: a JWT's sub must match the asserted uid.
    let bearer = crate::rest::bearer(req.headers()).map(String::from);
    let authenticated = state.authenticate_principal(bearer.as_deref());
    let principal = authenticated.as_ref().ok().cloned();
    let uid = match header_uid(req.headers()) {
        Some(u) => u,
        // No header: with a capability JWT present, its `sub` IS the caller's
        // uid — use it so the require_uid=false rollout bridge works on the
        // JWT path (a missing header there is not ambiguous). Otherwise fall
        // back per policy.
        None => match &principal {
            Some(Principal::Access(a)) => a.uid.clone(),
            // A bearer that FAILED authentication (expired, invalid) with no
            // uid header: the caller's real problem is the credential, and
            // answering `uid-required` (400) would send them to add a header
            // and only then learn the token is dead. Every /v1 handler
            // rejects this case anyway; this just says so first.
            _ if bearer.is_some() && authenticated.is_err() => {
                let e = authenticated.expect_err("checked is_err");
                let r = ApiError::from(e).into_response();
                record_http_metrics(
                    &state,
                    &route,
                    &http_method,
                    r.status().as_u16(),
                    started.elapsed().as_secs_f64(),
                );
                return r;
            }
            _ if state.config.require_uid => {
                let r = ApiError::Custom(CustomError::uid_required()).into_response();
                record_http_metrics(
                    &state,
                    &route,
                    &http_method,
                    r.status().as_u16(),
                    started.elapsed().as_secs_f64(),
                );
                return r;
            }
            _ => "anonymous".to_string(),
        },
    };
    if let Some(Principal::Access(a)) = &principal {
        if a.uid != uid {
            let r = ApiError::Custom(CustomError::uid_mismatch(&uid, &a.uid)).into_response();
            record_http_metrics(
                &state,
                &route,
                &http_method,
                r.status().as_u16(),
                started.elapsed().as_secs_f64(),
            );
            return r;
        }
    }
    let tenant_id = principal
        .as_ref()
        .map(|p| p.tenant_id().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let token_jti = principal
        .as_ref()
        .and_then(|p| p.token_jti())
        .map(String::from);

    // Buffer the request body (the handler re-reads the same Bytes; no copy).
    //
    // The ceiling is the ROUTE's, not the largest route's. Only the six
    // document-carrying routes declare the 256 MiB `DefaultBodyLimit`; every
    // other /v1 route is bounded by axum's 2 MiB default at its extractor —
    // but that limit is applied AFTER this buffering, and authentication
    // lives in the handlers, so buffering everything at 256 MiB here let an
    // unauthenticated client hold 256 MiB per in-flight request, up to the
    // concurrency ceiling, with no credential. An unmatched path (a 404)
    // gets the small ceiling too.
    let body_cap = crate::rest::body_limit_for_route(&route);
    let (mut parts, body) = req.into_parts();
    let req_bytes = match axum::body::to_bytes(body, body_cap).await {
        Ok(b) => b,
        Err(_) => {
            let r = ApiError::Custom(CustomError {
                slug: "invalid-input",
                status: axum::http::StatusCode::PAYLOAD_TOO_LARGE,
                code: tonic::Code::InvalidArgument,
                title: "request body too large",
                detail: format!("request body exceeds the {body_cap} byte limit"),
            })
            .into_response();
            record_http_metrics(
                &state,
                &route,
                &http_method,
                r.status().as_u16(),
                started.elapsed().as_secs_f64(),
            );
            return r;
        }
    };

    let request_id = new_request_id();
    parts.extensions.insert(RequestId(request_id.clone()));
    parts.extensions.insert(Uid(uid.clone()));
    let req = axum::extract::Request::from_parts(parts, Body::from(req_bytes.clone()));

    // latency_ms measures from middleware entry (auth sniff + body buffering
    // included) — the caller-observed latency, and the same clock the RED
    // histogram uses.
    let span = tracing::info_span!("http", %uid, %request_id, method = %http_method, path = %path);
    let response = next.run(req).instrument(span).await;
    let cap = state.config.interaction_body_max;

    // Streaming responses (SSE) are never buffered: a `to_bytes` here would
    // hold every event until the handler finished and hand the client the
    // whole sequence in one burst — which is exactly what happened between
    // the streaming turn route landing and this branch (dev-guide §13
    // entry 16, 2026-08-23). The head goes out now with its request id;
    // the body is wrapped so the interaction row and the RED metrics are
    // recorded at END of stream with the handler's StreamOutcome (real
    // status + attribution) — the gRPC plane's trailer-capture shape.
    if is_event_stream(&response) {
        let slot = response.extensions().get::<StreamOutcomeSlot>().cloned();
        let (mut res_parts, res_body) = response.into_parts();
        res_parts.headers.insert(
            "x-munarium-request-id",
            axum::http::HeaderValue::from_str(&request_id)
                .unwrap_or(axum::http::HeaderValue::from_static("req-invalid")),
        );
        let record = InteractionRecord {
            tenant_id,
            uid,
            session_id: None,
            request_id,
            plane: "rest",
            method: format!("{http_method} {path}"),
            runbook_ref: None,
            collection_ids: None,
            token_jti,
            request: interactions::body_json(&req_bytes, cap),
            response: None, // filled at end of stream: streamed marker + byte count
            status: Some(res_parts.status.as_u16() as i32),
            latency_ms: 0, // measured at stream end by the wrapper
        };
        let wrapped = Body::new(SseCapture {
            inner: res_body,
            state: state.clone(),
            record: Some(record),
            route,
            http_method,
            started,
            slot,
            bytes_len: 0,
            _permit,
        });
        return axum::response::Response::from_parts(res_parts, wrapped);
    }

    let latency_ms = started.elapsed().as_millis().min(i32::MAX as u128) as i32;

    let meta = response
        .extensions()
        .get::<InteractionMeta>()
        .cloned()
        .unwrap_or_default();
    let (mut res_parts, res_body) = response.into_parts();
    let res_bytes = axum::body::to_bytes(res_body, usize::MAX)
        .await
        .unwrap_or_default();
    res_parts.headers.insert(
        "x-munarium-request-id",
        axum::http::HeaderValue::from_str(&request_id)
            .unwrap_or(axum::http::HeaderValue::from_static("req-invalid")),
    );

    let response_json = if meta.redact_response {
        Some(interactions::redacted())
    } else {
        interactions::body_json(&res_bytes, cap)
    };
    record_http_metrics(
        &state,
        &route,
        &http_method,
        res_parts.status.as_u16(),
        started.elapsed().as_secs_f64(),
    );
    interactions::record(
        &state,
        InteractionRecord {
            tenant_id,
            uid,
            session_id: meta.session_id,
            request_id,
            plane: "rest",
            method: format!("{http_method} {path}"),
            runbook_ref: meta.runbook_ref,
            collection_ids: meta.collection_ids,
            token_jti,
            request: interactions::body_json(&req_bytes, cap),
            response: response_json,
            status: Some(res_parts.status.as_u16() as i32),
            latency_ms,
        },
    );

    axum::response::Response::from_parts(res_parts, Body::from(res_bytes))
}

fn is_event_stream(response: &axum::response::Response) -> bool {
    response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.trim_start()
                .to_ascii_lowercase()
                .starts_with("text/event-stream")
        })
        .unwrap_or(false)
}

/// Pass-through body for streaming REST responses. Frames are forwarded the
/// moment the handler yields them (no buffering, no exact size hint, so
/// hyper uses chunked transfer — a `Content-Length` on a `text/event-stream`
/// response is the tell that buffering crept back). At end of stream —
/// normal completion, transport error, or a peer that vanished, all of
/// which reach `Drop` — the interaction row and the RED metrics are
/// recorded with the real elapsed time, the byte count of what was sent,
/// and, when the handler filled its [`StreamOutcome`] slot, the terminal
/// status and session/runbook attribution the stream actually carried.
struct SseCapture {
    inner: Body,
    state: Arc<AppState>,
    record: Option<InteractionRecord>,
    route: String,
    http_method: String,
    started: Instant,
    slot: Option<StreamOutcomeSlot>,
    bytes_len: usize,
    /// The load-shed permit, held until the STREAM ends rather than until
    /// the response head goes out. The streaming turn is the most expensive
    /// request the server serves — retrieval fan-out, expansion, completion,
    /// corrective retries — and releasing the permit with the head would
    /// leave exactly that work outside `MUNARIUM_MAX_CONCURRENCY`.
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl http_body::Body for SseCapture {
    type Data = bytes::Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        let poll = std::pin::Pin::new(&mut this.inner).poll_frame(cx);
        if let std::task::Poll::Ready(Some(Ok(frame))) = &poll {
            if let Some(data) = frame.data_ref() {
                this.bytes_len = this.bytes_len.saturating_add(data.len());
            }
        }
        poll
    }

    fn is_end_stream(&self) -> bool {
        http_body::Body::is_end_stream(&self.inner)
    }

    fn size_hint(&self) -> http_body::SizeHint {
        // Deliberately the inner hint (unbounded for a stream): never claim
        // an exact length for a body we are forwarding live.
        http_body::Body::size_hint(&self.inner)
    }
}

impl Drop for SseCapture {
    fn drop(&mut self) {
        let Some(mut record) = self.record.take() else {
            return;
        };
        record.latency_ms = self.started.elapsed().as_millis().min(i32::MAX as u128) as i32;
        let mut outcome_status: Option<u16> = None;
        if let Some(slot) = &self.slot {
            if let Ok(outcome) = slot.lock() {
                record.session_id = outcome.meta.session_id.clone();
                record.runbook_ref = outcome.meta.runbook_ref.clone();
                record.collection_ids = outcome.meta.collection_ids.clone();
                outcome_status = outcome.status;
            }
        }
        // The terminal event's status is the final word (a `done` is 200,
        // an `error` carries its problem status); a stream that ended
        // before the handler filled the slot keeps the HTTP status it
        // opened with.
        if let Some(s) = outcome_status {
            record.status = Some(s as i32);
        }
        record.response = Some(serde_json::json!({
            "streamed": true,
            "content_type": "text/event-stream",
            "bytes_len": self.bytes_len,
        }));
        let status = record.status.unwrap_or(0).max(0) as u16;
        record_http_metrics(
            &self.state,
            &self.route,
            &self.http_method,
            status,
            self.started.elapsed().as_secs_f64(),
        );
        interactions::record(&self.state, record);
    }
}

// ---------------------------------------------------------------------------
// gRPC plane
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct GrpcCaptureLayer {
    pub state: Arc<AppState>,
}

impl<S> tower::Layer<S> for GrpcCaptureLayer {
    type Service = GrpcCapture<S>;

    fn layer(&self, inner: S) -> Self::Service {
        GrpcCapture {
            inner,
            state: self.state.clone(),
        }
    }
}

#[derive(Clone)]
pub struct GrpcCapture<S> {
    inner: S,
    state: Arc<AppState>,
}

/// Trailers-only gRPC rejection carrying the same ErrorInfo details as a
/// handler-emitted Status.
fn grpc_reject(custom: CustomError) -> axum::http::Response<tonic::body::BoxBody> {
    custom.to_status().into_http()
}

impl<S, ReqB> tower::Service<axum::http::Request<ReqB>> for GrpcCapture<S>
where
    S: tower::Service<
            axum::http::Request<ReqB>,
            Response = axum::http::Response<tonic::body::BoxBody>,
        > + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    ReqB: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future =
        std::pin::Pin<Box<dyn std::future::Future<Output = Result<S::Response, S::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: axum::http::Request<ReqB>) -> Self::Future {
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        let state = self.state.clone();
        let path = req.uri().path().to_string();

        // Only mmp services carry the uid contract; health/reflection pass.
        if !path.starts_with("/mmp.v1.") {
            return Box::pin(async move { inner.call(req).await });
        }

        let uid = req
            .headers()
            .get("munarium-uid")
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from);
        let bearer = crate::rest::bearer(req.headers()).map(String::from);

        Box::pin(async move {
            // Load shed at the same ceiling as REST (its own permit pool).
            // Health/reflection passed through above and never shed.
            let _permit = match state.grpc_permits.clone().try_acquire_owned() {
                Ok(p) => p,
                Err(_) => {
                    state.metrics.inc("munarium_load_shed_total", String::new());
                    return Ok(grpc_reject(CustomError::overloaded()));
                }
            };
            let principal = state.authenticate_principal(bearer.as_deref()).ok();
            let uid = match uid {
                Some(u) => u,
                // Mirror the REST arm: a JWT's sub is the uid when no header
                // is present, so the require_uid=false bridge works on gRPC too.
                None => match &principal {
                    Some(Principal::Access(a)) => a.uid.clone(),
                    _ if state.config.require_uid => {
                        return Ok(grpc_reject(CustomError::uid_required()));
                    }
                    _ => "anonymous".to_string(),
                },
            };
            if let Some(Principal::Access(a)) = &principal {
                if a.uid != uid {
                    return Ok(grpc_reject(CustomError::uid_mismatch(&uid, &a.uid)));
                }
            }
            let tenant_id = principal
                .as_ref()
                .map(|p| p.tenant_id().to_string())
                .unwrap_or_else(|| "unknown".to_string());
            let token_jti = principal
                .as_ref()
                .and_then(|p| p.token_jti())
                .map(String::from);

            let request_id = new_request_id();
            let started = Instant::now();
            let span = tracing::info_span!("grpc", %uid, %request_id, method = %path);
            let response = inner.call(req).instrument(span).await?;

            // Session/runbook attribution rides the response extensions —
            // the same channel the REST handlers use (the gRPC session
            // twins insert an InteractionMeta; everything else defaults).
            let meta = response
                .extensions()
                .get::<InteractionMeta>()
                .cloned()
                .unwrap_or_default();

            // Trailers-only errors surface grpc-status in the response
            // HEADERS; a successful unary call carries status 0 in the
            // TRAILERS, which only arrive at end-of-stream. Since
            // 2026-08-18 the body wrapper below awaits them, so the
            // interaction row records the REAL final status (through
            // v0.1.2 this layer recorded NULL for every completed stream —
            // the documented caveat, now retired).
            let header_status = response
                .headers()
                .get("grpc-status")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<i32>().ok());

            let record = InteractionRecord {
                tenant_id,
                uid,
                session_id: meta.session_id,
                request_id,
                plane: "grpc",
                method: path.clone(),
                runbook_ref: meta.runbook_ref,
                collection_ids: meta.collection_ids,
                token_jti,
                request: None,
                response: None,
                status: header_status,
                latency_ms: 0, // measured at stream end by the wrapper
            };
            Ok(response.map(|inner| {
                tonic::body::BoxBody::new(GrpcStatusCapture {
                    inner,
                    state,
                    record: Some(record),
                    route: path,
                    started,
                    trailer_status: None,
                })
            }))
        })
    }
}

/// Awaits the response stream to its end so the interaction row and the RED
/// metrics carry the REAL final gRPC status: trailers-only errors already
/// sat in the headers, but a success's `grpc-status: 0` rides the trailers.
/// Completion fires from `Drop`, which covers every exit — normal
/// end-of-stream, transport error, and a peer that vanishes mid-stream
/// (recorded with whatever status was seen by then).
struct GrpcStatusCapture {
    inner: tonic::body::BoxBody,
    state: Arc<AppState>,
    record: Option<InteractionRecord>,
    route: String,
    started: Instant,
    trailer_status: Option<i32>,
}

impl http_body::Body for GrpcStatusCapture {
    type Data = bytes::Bytes;
    type Error = tonic::Status;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        let poll = std::pin::Pin::new(&mut this.inner).poll_frame(cx);
        if let std::task::Poll::Ready(Some(Ok(frame))) = &poll {
            if let Some(trailers) = frame.trailers_ref() {
                this.trailer_status = trailers
                    .get("grpc-status")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<i32>().ok())
                    // An absent grpc-status trailer on a completed stream
                    // means OK per the wire contract.
                    .or(Some(0));
            }
        }
        poll
    }

    // Both MUST delegate: a trailers-only error response (grpc-status in
    // the HEADERS, empty body) relies on is_end_stream() to get the
    // END_STREAM flag onto the headers frame. The default (false) made h2
    // close the stream without trailers, breaking EVERY error response —
    // caught live by the 2026-08-18 grpcurl probe.
    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for GrpcStatusCapture {
    fn drop(&mut self) {
        let Some(mut record) = self.record.take() else {
            return;
        };
        record.latency_ms = self.started.elapsed().as_millis().min(i32::MAX as u128) as i32;
        // Trailer status wins (it is the final word); header status covers
        // trailers-only rejections; a stream dropped before its trailers
        // keeps NULL, which now genuinely means "never completed".
        record.status = self.trailer_status.or(record.status);
        let class = match record.status {
            Some(0) => "ok",
            Some(_) => "error",
            None => "incomplete",
        };
        // UNIMPLEMENTED (12) is tonic's answer for a method that does not
        // exist. The path was minted by the caller — any `/mmp.v1.*` string
        // reaches this layer, authenticated or not — so it is labelled the
        // way the REST arm labels a 404 rather than becoming a metrics series
        // and an interaction row per junk path. (The metrics maps are never
        // pruned; a scanner would grow them without bound.)
        let unmatched = record.status == Some(tonic::Code::Unimplemented as i32);
        let route = if unmatched {
            "(unmatched)"
        } else {
            self.route.as_str()
        };
        self.state.metrics.inc(
            "munarium_http_requests_total",
            crate::metrics::labels(&[
                ("plane", "grpc"),
                ("route", route),
                ("method", "rpc"),
                ("status_class", class),
            ]),
        );
        self.state.metrics.observe(
            "munarium_http_request_duration_seconds",
            crate::metrics::labels(&[("plane", "grpc"), ("route", route)]),
            record.latency_ms as f64 / 1e3,
        );
        if !unmatched {
            interactions::record(&self.state, record);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AuthMode, Config, DocIntelConfig, SourceStoreConfig, StoreKind};
    use axum::response::sse::{Event, Sse};
    use axum::routing::get;
    use axum::Router;
    use std::time::Duration;
    use tokio_stream::StreamExt;
    use tower::ServiceExt;

    fn test_config() -> Config {
        Config {
            http_addr: "127.0.0.1:0".into(),
            grpc_addr: None,
            ops_addr: "127.0.0.1:0".into(),
            store: StoreKind::Memory,
            database_url: None,
            auth: AuthMode::Disabled,
            shutdown_grace_secs: 1,
            token_secret: None,
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

    /// Regression for dev-guide §13 entry 16 (2026-08-23): a `text/event-stream`
    /// response must reach the client frame by frame. The handler sends one
    /// event, then WAITS for the test to acknowledge it before sending the
    /// second and ending the stream. A middleware that buffers the body can
    /// never deliver the first frame (the handler is waiting on us), so the
    /// regression fails by timeout rather than by timing luck.
    #[tokio::test]
    async fn event_stream_passes_through_unbuffered() {
        let state = AppState::new(test_config()).await.expect("state");
        let gate = Arc::new(tokio::sync::Notify::new());
        let gate_h = gate.clone();
        let handler = move || {
            let gate = gate_h.clone();
            async move {
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<
                    Result<Event, std::convert::Infallible>,
                >();
                tokio::spawn(async move {
                    let _ = tx.send(Ok(Event::default().event("progress").data("first")));
                    gate.notified().await;
                    let _ = tx.send(Ok(Event::default().event("done").data("last")));
                });
                Sse::new(tokio_stream::wrappers::UnboundedReceiverStream::new(rx))
            }
        };
        let app = Router::new()
            .route("/v1/stream-test", get(handler))
            .layer(axum::middleware::from_fn_with_state(state.clone(), capture));

        // The head must come back while the handler is still parked on the
        // gate: a buffering middleware awaits the whole body first and this
        // oneshot never returns, so the timeout is the failure path.
        let resp = tokio::time::timeout(
            Duration::from_secs(5),
            app.oneshot(
                axum::http::Request::get("/v1/stream-test")
                    .header("x-munarium-uid", "tester")
                    .body(Body::empty())
                    .unwrap(),
            ),
        )
        .await
        .expect(
            "response head must arrive before the stream ends — the middleware buffered the stream",
        )
        .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        assert!(
            resp.headers().get("x-munarium-request-id").is_some(),
            "the request id rides the stream head like every other response"
        );
        assert!(
            http_body::Body::size_hint(resp.body()).exact().is_none(),
            "a streamed body must not advertise an exact length — that is the \
             Content-Length tell of a buffered stream"
        );

        let mut data = resp.into_body().into_data_stream();
        let first = tokio::time::timeout(Duration::from_secs(5), data.next())
            .await
            .expect("first frame must arrive BEFORE the handler finishes — the middleware buffered the stream")
            .expect("stream ended before its first frame")
            .expect("body error");
        assert!(
            String::from_utf8_lossy(&first).contains("event: progress"),
            "first frame was {:?}",
            String::from_utf8_lossy(&first)
        );

        // Release the handler; the rest of the stream must follow and end.
        gate.notify_one();
        let mut rest = Vec::new();
        while let Some(chunk) = data.next().await {
            rest.extend_from_slice(&chunk.expect("body error"));
        }
        assert!(String::from_utf8_lossy(&rest).contains("event: done"));
    }

    /// The real route through the real router: the turn stream on a memory
    /// store opens (200, text/event-stream, no exact length), carries its
    /// refusal as the terminal `error` event, and — once the body is drained
    /// — the capture wrapper has recorded the outcome the handler's
    /// StreamOutcome slot reported (4xx), not the 200 the stream opened with.
    #[tokio::test]
    async fn turn_stream_route_streams_and_records_terminal_status() {
        let state = AppState::new(test_config()).await.expect("state");
        let app = crate::rest::router(state.clone());
        let resp = app
            .oneshot(
                axum::http::Request::post("/v1/sessions/ses-0000/turns/stream")
                    .header("x-munarium-uid", "tester")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"query":"x"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let ct = resp
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        assert!(ct.starts_with("text/event-stream"), "content-type was {ct}");
        assert!(http_body::Body::size_hint(resp.body()).exact().is_none());

        let mut data = resp.into_body().into_data_stream();
        let mut body = Vec::new();
        while let Some(chunk) = data.next().await {
            body.extend_from_slice(&chunk.expect("body error"));
        }
        drop(data); // end of stream → the wrapper records the interaction + metrics
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("event: error"), "body was {text}");
        assert!(
            text.contains("requires the postgres store"),
            "the refusal rides the terminal event: {text}"
        );

        let rendered = crate::metrics::render(&state);
        assert!(
            rendered.contains(
                r#"route="/v1/sessions/{id}/turns/stream",method="POST",status_class="4xx""#
            ),
            "the recorded outcome must be the terminal event's status, not the 200 \
             the stream opened with; metrics were:\n{rendered}"
        );
    }
}
