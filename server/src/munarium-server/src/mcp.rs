// SPDX-License-Identifier: Apache-2.0
//! The opt-in MCP endpoint (ClearView A1–A9, `docs/design/mcp-analysis.md`;
//! operator guide `docs/ops/mcp.md`).
//!
//! **Off by default.** With `MUNARIUM_MCP` unset or `disabled` no `/mcp` route
//! is registered (the path answers the router's ordinary 404), no listener,
//! task or outbound call exists, and no MCP table is read or written. The
//! switch is read once at startup.
//!
//! When enabled, `POST /mcp` on the existing REST listener speaks JSON-RPC
//! over Streamable HTTP with unary `application/json` responses only (no SSE,
//! no MCP session id), in two protocol revisions (A3):
//!
//! - `2026-07-28`: stateless requests carrying their version and client
//!   metadata under `params._meta`, validated against the
//!   `MCP-Protocol-Version`, `Mcp-Method` and `Mcp-Name` headers;
//!   `server/discover`, `tools/list`, `tools/call`.
//! - `2025-11-25`: the `initialize` handshake, `notifications/initialized`,
//!   `ping`, then `tools/list` and `tools/call` under that revision's header.
//!
//! Every request is admitted on the REST plane's R7 ordinary pool AND the MCP
//! concurrency bound before its body is read, then authenticated: a
//! tenant-bound capability JWT carrying `mcp` (A1). Static role credentials
//! are refused even where they could call the native operation. The tool
//! table, its authority, the durable audit admission (A6) and the recovery
//! contract (§5.3) live in `mcp_tools`; the stores in `mcp_store`; the
//! generation allowances (A2/A8/A9) in `mcp_generation`.

use crate::error::{ApiError, CustomError};
use crate::state::{AppState, Principal};
use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// The modern revision (stateless, `server/discover`).
pub const MODERN: &str = "2026-07-28";
/// The legacy revision (`initialize` handshake).
pub const LEGACY: &str = "2025-11-25";
/// Every revision this profile serves, newest first.
pub const SUPPORTED: &[&str] = &[MODERN, LEGACY];

/// `params._meta` keys of the modern revision.
const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CLIENT_CAPS: &str = "io.modelcontextprotocol/clientCapabilities";
const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

/// JSON-RPC and MCP error codes.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
pub(crate) const INVALID_PARAMS: i64 = -32602;
const HEADER_MISMATCH: i64 = -32020;
const UNSUPPORTED_VERSION: i64 = -32022;

// ---------------------------------------------------------------------------
// settings
// ---------------------------------------------------------------------------

/// Whether new keyed work may start (`MUNARIUM_MCP_EXECUTION`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Execution {
    /// Every enabled form is listed and callable.
    Enabled,
    /// The five keyed tools list only their recovery form; the six bounded
    /// reads stay available. Receipts remain readable (§5.3).
    RecoveryOnly,
}

/// The validated MCP settings. Read once at startup, like the console login.
#[derive(Debug, Clone)]
pub struct McpSettings {
    /// `MUNARIUM_MCP`: `disabled` (default) or `enabled`.
    pub enabled: bool,
    /// `MUNARIUM_MCP_ALLOWED_ORIGINS`: exact `Origin` values accepted when a
    /// request carries one. Empty: any request WITH an `Origin` is refused,
    /// and server-side agents (which send none) are unaffected.
    pub allowed_origins: Vec<String>,
    /// `MUNARIUM_MCP_MAX_CONCURRENCY`: MCP requests in flight per replica,
    /// inside the REST plane's ordinary pool. 0 (default) resolves to half
    /// the ordinary pool, at least 1, at startup.
    pub max_concurrency: usize,
    /// `MUNARIUM_MCP_PAID_TOOLS`: `disabled` (default) omits the execution
    /// forms of the generating tools (`ask_collections`, `ask_session`).
    pub paid_tools: bool,
    /// `MUNARIUM_MCP_EXECUTION`: `enabled` (default) or `recovery-only`.
    pub execution: Execution,
    /// `MUNARIUM_MCP_RESULT_MAX_BYTES`: the whole serialized response.
    pub result_max_bytes: usize,
    /// `MUNARIUM_MCP_INPUT_MAX_BYTES`: the request body.
    pub input_max_bytes: usize,
    /// `MUNARIUM_MCP_AUDIT_DEADLINE_MS`: how long an admission waits for the
    /// durable store's commit acknowledgment before refusing (A6).
    pub audit_deadline_ms: u64,
    /// `MUNARIUM_MCP_CALL_DEADLINE_SECS`: the bound on one admitted tool's
    /// execution; a keyed outcome still missing after it is `unknown`.
    pub call_deadline_secs: u64,
    /// `MUNARIUM_MCP_KEY_MAX_VALIDITY_SECS`: the longest an operation key may
    /// live, measured on the shared store clock at claim time.
    pub key_max_validity_secs: u64,
    /// `MUNARIUM_MCP_GENERATION_POLICY`: the trusted collection/index
    /// generation policy (A2/A8), a JSON file. Absent: no scope may generate.
    pub generation: crate::mcp_generation::GenerationPolicy,
}

pub const DEFAULT_RESULT_MAX_BYTES: usize = 262_144;
pub const DEFAULT_INPUT_MAX_BYTES: usize = 1_048_576;
pub const DEFAULT_AUDIT_DEADLINE_MS: u64 = 2_000;
pub const DEFAULT_CALL_DEADLINE_SECS: u64 = 300;
pub const DEFAULT_KEY_MAX_VALIDITY_SECS: u64 = 86_400;

impl Default for McpSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            allowed_origins: Vec::new(),
            max_concurrency: 0,
            paid_tools: false,
            execution: Execution::Enabled,
            result_max_bytes: DEFAULT_RESULT_MAX_BYTES,
            input_max_bytes: DEFAULT_INPUT_MAX_BYTES,
            audit_deadline_ms: DEFAULT_AUDIT_DEADLINE_MS,
            call_deadline_secs: DEFAULT_CALL_DEADLINE_SECS,
            key_max_validity_secs: DEFAULT_KEY_MAX_VALIDITY_SECS,
            generation: Default::default(),
        }
    }
}

impl McpSettings {
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Every value is validated whether or not the endpoint is enabled, so a
    /// typo is found before the switch is turned on, not after.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let get = |k: &str| {
            lookup(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let switch = |k: &str, default: bool| -> Result<bool, String> {
            match get(k).as_deref() {
                None => Ok(default),
                Some("enabled") => Ok(true),
                Some("disabled") => Ok(false),
                Some(other) => Err(format!("{k} must be enabled or disabled, got '{other}'")),
            }
        };
        let number = |k: &str, default: u64, min: u64, max: u64| -> Result<u64, String> {
            match get(k) {
                None => Ok(default),
                Some(v) => {
                    let n: u64 = v
                        .parse()
                        .map_err(|e| format!("{k}: '{v}' is not a whole number: {e}"))?;
                    if n < min || n > max {
                        return Err(format!("{k} must be between {min} and {max}, got {n}"));
                    }
                    Ok(n)
                }
            }
        };
        let execution = match get("MUNARIUM_MCP_EXECUTION").as_deref() {
            None | Some("enabled") => Execution::Enabled,
            Some("recovery-only") => Execution::RecoveryOnly,
            Some(other) => {
                return Err(format!(
                    "MUNARIUM_MCP_EXECUTION must be enabled or recovery-only, got '{other}'"
                ))
            }
        };
        let allowed_origins = get("MUNARIUM_MCP_ALLOWED_ORIGINS")
            .map(|v| {
                v.split(',')
                    .map(|o| o.trim().to_string())
                    .filter(|o| !o.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for o in &allowed_origins {
            if !(o.starts_with("https://") || o.starts_with("http://")) || o.ends_with('/') {
                return Err(format!(
                    "MUNARIUM_MCP_ALLOWED_ORIGINS entries are exact origins like \
                     https://agent.example.com (scheme and host, no path), got '{o}'"
                ));
            }
        }
        let generation = match get("MUNARIUM_MCP_GENERATION_POLICY") {
            None => crate::mcp_generation::GenerationPolicy::default(),
            Some(path) => {
                let text = std::fs::read_to_string(&path).map_err(|e| {
                    format!("MUNARIUM_MCP_GENERATION_POLICY: cannot read '{path}': {e}")
                })?;
                crate::mcp_generation::GenerationPolicy::parse(&text)
                    .map_err(|e| format!("MUNARIUM_MCP_GENERATION_POLICY: {e}"))?
            }
        };
        let s = Self {
            enabled: switch("MUNARIUM_MCP", false)?,
            allowed_origins,
            max_concurrency: number("MUNARIUM_MCP_MAX_CONCURRENCY", 0, 0, 100_000)? as usize,
            paid_tools: switch("MUNARIUM_MCP_PAID_TOOLS", false)?,
            execution,
            result_max_bytes: number(
                "MUNARIUM_MCP_RESULT_MAX_BYTES",
                DEFAULT_RESULT_MAX_BYTES as u64,
                4_096,
                16 * 1_048_576,
            )? as usize,
            input_max_bytes: number(
                "MUNARIUM_MCP_INPUT_MAX_BYTES",
                DEFAULT_INPUT_MAX_BYTES as u64,
                1_024,
                crate::rest::DEFAULT_BODY_LIMIT as u64,
            )? as usize,
            audit_deadline_ms: number(
                "MUNARIUM_MCP_AUDIT_DEADLINE_MS",
                DEFAULT_AUDIT_DEADLINE_MS,
                50,
                60_000,
            )?,
            call_deadline_secs: number(
                "MUNARIUM_MCP_CALL_DEADLINE_SECS",
                DEFAULT_CALL_DEADLINE_SECS,
                1,
                3_600,
            )?,
            key_max_validity_secs: number(
                "MUNARIUM_MCP_KEY_MAX_VALIDITY_SECS",
                DEFAULT_KEY_MAX_VALIDITY_SECS,
                60,
                7 * 86_400,
            )?,
            generation,
        };
        if s.key_max_validity_secs <= s.call_deadline_secs {
            return Err(format!(
                "MUNARIUM_MCP_KEY_MAX_VALIDITY_SECS ({}) must exceed \
                 MUNARIUM_MCP_CALL_DEADLINE_SECS ({}): a key must outlive the work it admits",
                s.key_max_validity_secs, s.call_deadline_secs
            ));
        }
        Ok(s)
    }

    /// The configuration an enabled endpoint cannot run with. Checked at
    /// startup with an actionable message; a disabled endpoint checks nothing
    /// here, so the default configuration can never fail on MCP grounds.
    pub fn check_compatible(&self, config: &crate::config::Config) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        if matches!(config.auth, crate::config::AuthMode::Disabled) {
            return Err(
                "MUNARIUM_MCP=enabled requires authentication: MCP entry needs a \
                        capability JWT carrying the mcp scope, and MUNARIUM_AUTH_MODE=disabled \
                        authenticates nobody"
                    .into(),
            );
        }
        if config.token_secret.is_none() {
            return Err(
                "MUNARIUM_MCP=enabled requires MUNARIUM_TOKEN_SECRET: MCP accepts only \
                        capability JWTs, which cannot be verified without it"
                    .into(),
            );
        }
        if config.platform.is_some() {
            return Err(
                "MUNARIUM_MCP=enabled is not supported with the platform-v1 authority \
                        profile; leave MUNARIUM_MCP unset on platform replicas"
                    .into(),
            );
        }
        Ok(())
    }
}

/// Per-process MCP state held in `AppState`.
pub struct McpRuntime {
    pub settings: McpSettings,
    /// The MCP concurrency bound, resolved at startup.
    pub slots: Arc<tokio::sync::Semaphore>,
    pub slot_capacity: usize,
    /// Test-only fault injection for the durable admission (A6-T1).
    #[cfg(test)]
    pub faults: crate::mcp_store::Faults,
}

impl McpRuntime {
    pub fn new(settings: McpSettings, ordinary_slots: usize) -> Self {
        let capacity = if settings.max_concurrency == 0 {
            (ordinary_slots / 2).max(1)
        } else {
            settings.max_concurrency
        };
        Self {
            settings,
            slots: Arc::new(tokio::sync::Semaphore::new(capacity)),
            slot_capacity: capacity,
            #[cfg(test)]
            faults: Default::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// routing
// ---------------------------------------------------------------------------

/// Marks an in-process dispatch from an admitted MCP tool call. The REST
/// capture middleware neither charges a second R7 slot nor records a second
/// interaction for it: the MCP layer already did both, once (§7.4, A6). Only
/// this module can construct it; it never comes off the wire.
#[derive(Debug, Clone)]
pub struct McpInner {
    #[allow(dead_code)]
    pub invocation_id: String,
}

/// The endpoint and its inner router. The inner router is the native REST
/// router WITHOUT `/mcp`, built once and cloned per dispatch.
pub struct McpService {
    pub state: Arc<AppState>,
    pub inner: axum::Router,
}

/// Mounts `/mcp` on the REST router. Called only when enabled.
pub fn mount(
    router: axum::Router<Arc<AppState>>,
    state: Arc<AppState>,
) -> axum::Router<Arc<AppState>> {
    let service = Arc::new(McpService {
        inner: crate::rest::router_without_mcp(state.clone()),
        state,
    });
    router.route(
        "/mcp",
        axum::routing::any(move |req: axum::extract::Request| {
            let service = service.clone();
            async move { handle(service, req).await }
        }),
    )
}

// ---------------------------------------------------------------------------
// responses
// ---------------------------------------------------------------------------

fn no_store(mut r: Response) -> Response {
    r.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    r
}

fn json_response(status: StatusCode, body: &Value) -> Response {
    let bytes = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    let mut r = (status, bytes).into_response();
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    no_store(r)
}

pub(crate) fn rpc_error(
    status: StatusCode,
    id: Option<&Value>,
    code: i64,
    message: &str,
    data: Option<Value>,
) -> Response {
    let mut error = json!({"code": code, "message": message});
    if let Some(d) = data {
        error["data"] = d;
    }
    let mut body = json!({"jsonrpc": "2.0", "error": error});
    if let Some(id) = id {
        body["id"] = id.clone();
    }
    json_response(status, &body)
}

fn problem(error: CustomError) -> Response {
    let unauthorized = error.status == StatusCode::UNAUTHORIZED;
    let mut r = ApiError::Custom(error).into_response();
    if unauthorized {
        // A bare Server advertises only the mechanism it supplies: a bearer
        // capability token from the deployment's broker (A1, A7).
        r.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer realm=\"munarium-mcp\""),
        );
    }
    no_store(r)
}

fn api_problem(e: ApiError) -> Response {
    let unauthorized = matches!(&e, ApiError::Custom(c) if c.status == StatusCode::UNAUTHORIZED)
        || matches!(
            &e,
            ApiError::Mesh(munarium_core::KernelError::Unauthenticated(_))
        );
    let mut r = e.into_response();
    if unauthorized {
        r.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer realm=\"munarium-mcp\""),
        );
    }
    no_store(r)
}

fn shed(state: &AppState, why: &'static str) -> Response {
    state.metrics.inc(
        "munarium_mcp_requests_total",
        crate::metrics::labels(&[("method", "(unparsed)"), ("outcome", why)]),
    );
    let mut r = problem(CustomError::overloaded());
    r.headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    r
}

fn count(state: &AppState, method: &str, outcome: &str) {
    // `method` is bounded: unknown methods are counted as "(other)".
    let method = match method {
        "server/discover"
        | "tools/list"
        | "tools/call"
        | "initialize"
        | "ping"
        | "notifications/initialized" => method,
        m if m.starts_with("notifications/") => "notifications/*",
        _ => "(other)",
    };
    state.metrics.inc(
        "munarium_mcp_requests_total",
        crate::metrics::labels(&[("method", method), ("outcome", outcome)]),
    );
}

// ---------------------------------------------------------------------------
// entry
// ---------------------------------------------------------------------------

/// The verified caller of one MCP request.
#[derive(Debug, Clone)]
pub struct Caller {
    pub access: munarium_access::AccessCtx,
    pub bearer: String,
}

/// A1 entry: a valid, unrevoked, tenant-bound capability JWT carrying `mcp`.
/// Static role credentials are refused. 401 carries a Bearer challenge.
pub(crate) async fn entry(state: &AppState, headers: &HeaderMap) -> Result<Caller, Response> {
    let Some(bearer) = crate::rest::bearer(headers) else {
        return Err(api_problem(ApiError::Mesh(
            munarium_core::KernelError::Unauthenticated("missing bearer token".into()),
        )));
    };
    let principal = state
        .authenticate_principal(Some(bearer))
        .map_err(|e| api_problem(crate::rest::promote_auth_error(e)))?;
    let access = match principal {
        Principal::Access(a) => a,
        Principal::Static(_) => {
            return Err(api_problem(ApiError::Mesh(
                munarium_core::KernelError::Forbidden(
                    "static role credentials cannot enter the MCP endpoint; use a capability \
                     token issued with the mcp scope"
                        .into(),
                ),
            )))
        }
    };
    if let Some(asserted) = headers
        .get("x-munarium-uid")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if asserted != access.uid {
            return Err(problem(CustomError::uid_mismatch(asserted, &access.uid)));
        }
    }
    if access.tenant_id.trim().is_empty() {
        return Err(api_problem(ApiError::Mesh(
            munarium_core::KernelError::Forbidden("the token is not bound to a tenant".into()),
        )));
    }
    state
        .check_revocation(&access.tenant_id, &access.jti)
        .await
        .map_err(|e| api_problem(crate::rest::promote_auth_error(e)))?;
    if !access.has_scope(munarium_access::SCOPE_MCP) {
        return Err(problem(CustomError::scope_missing(
            munarium_access::SCOPE_MCP,
        )));
    }
    Ok(Caller {
        access,
        bearer: bearer.to_string(),
    })
}

fn accepts_json(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::ACCEPT)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|m| {
            m.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .any(|m| m == "application/json" || m == "application/*" || m == "*/*")
}

fn is_json_content(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("application/json")
        })
        .unwrap_or(false)
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// The Streamable HTTP decoding rule: a value of the form
/// `=?base64?{Base64}?=` carries non-ASCII text; anything else is literal.
pub(crate) fn decode_header_value(raw: &str) -> Option<String> {
    use base64::Engine as _;
    match raw
        .strip_prefix("=?base64?")
        .and_then(|r| r.strip_suffix("?="))
    {
        Some(encoded) => base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()
            .and_then(|b| String::from_utf8(b).ok()),
        None => Some(raw.to_string()),
    }
}

fn server_info() -> Value {
    json!({"name": "munarium-server", "version": env!("CARGO_PKG_VERSION")})
}

/// Which revision a request is served under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revision {
    Modern,
    Legacy,
}

impl Revision {
    pub fn as_str(self) -> &'static str {
        match self {
            Revision::Modern => MODERN,
            Revision::Legacy => LEGACY,
        }
    }
}

/// Wraps a method result for the revision: the modern revision adds
/// `resultType` and the server identity under `_meta`.
pub(crate) fn finish_result(revision: Revision, mut result: Value) -> Value {
    if revision == Revision::Modern {
        if let Some(obj) = result.as_object_mut() {
            obj.insert("resultType".into(), json!("complete"));
            let meta = obj
                .entry("_meta")
                .or_insert_with(|| Value::Object(Map::new()));
            if let Some(m) = meta.as_object_mut() {
                m.insert(META_SERVER_INFO.into(), server_info());
            }
        }
    }
    result
}

fn rpc_result(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

// ---------------------------------------------------------------------------
// the handler
// ---------------------------------------------------------------------------

pub async fn handle(service: Arc<McpService>, req: axum::extract::Request) -> Response {
    let state = service.state.clone();
    if req.method() != Method::POST {
        // GET (a standalone stream) and DELETE (session teardown) are not part
        // of this profile; every other method is not either.
        count(&state, "(http)", "method-not-allowed");
        let mut r = (StatusCode::METHOD_NOT_ALLOWED, "").into_response();
        r.headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("POST"));
        return no_store(r);
    }
    // R7 before the body is read: an ordinary REST slot (MCP fits inside the
    // REST ceiling and never takes the control reserve), then the MCP bound.
    let Some(_permit) = state.admission.rest.admit(
        &state.metrics,
        crate::admission::WorkClass::Ordinary,
        |_| false,
    ) else {
        return shed(&state, "shed");
    };
    let Ok(_slot) = state.mcp.slots.clone().try_acquire_owned() else {
        return shed(&state, "shed-mcp");
    };
    let (parts, body) = req.into_parts();
    let headers = parts.headers;

    if let Some(origin) = header_str(&headers, "origin") {
        if !state
            .mcp
            .settings
            .allowed_origins
            .iter()
            .any(|o| o == origin)
        {
            count(&state, "(http)", "origin-refused");
            return rpc_error(
                StatusCode::FORBIDDEN,
                None,
                INVALID_REQUEST,
                "origin not allowed",
                None,
            );
        }
    }
    let caller = match entry(&state, &headers).await {
        Ok(c) => c,
        Err(r) => {
            count(&state, "(http)", "entry-refused");
            return r;
        }
    };
    if !is_json_content(&headers) {
        count(&state, "(http)", "unsupported-media-type");
        return rpc_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            None,
            INVALID_REQUEST,
            "the request body must be application/json",
            None,
        );
    }
    if !accepts_json(&headers) {
        count(&state, "(http)", "not-acceptable");
        return rpc_error(
            StatusCode::NOT_ACCEPTABLE,
            None,
            INVALID_REQUEST,
            "the Accept header must allow application/json (and text/event-stream)",
            None,
        );
    }
    let limit = state.mcp.settings.input_max_bytes;
    let bytes = match axum::body::to_bytes(body, limit).await {
        Ok(b) => b,
        Err(_) => {
            count(&state, "(http)", "too-large");
            return rpc_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                None,
                INVALID_REQUEST,
                &format!("the request body exceeds the {limit}-byte input limit"),
                None,
            );
        }
    };
    let message: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => {
            count(&state, "(http)", "parse-error");
            return rpc_error(
                StatusCode::BAD_REQUEST,
                None,
                PARSE_ERROR,
                "the body is not a JSON document",
                None,
            );
        }
    };
    let Some(obj) = message.as_object() else {
        count(&state, "(http)", "invalid");
        let why = if message.is_array() {
            "batches are not supported; send one JSON-RPC message per request"
        } else {
            "a JSON-RPC message must be an object"
        };
        return rpc_error(StatusCode::BAD_REQUEST, None, INVALID_REQUEST, why, None);
    };
    let id = obj.get("id").cloned();
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        count(&state, "(http)", "invalid");
        return rpc_error(
            StatusCode::BAD_REQUEST,
            id.as_ref(),
            INVALID_REQUEST,
            "jsonrpc must be \"2.0\"",
            None,
        );
    }
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        count(&state, "(http)", "invalid");
        return rpc_error(
            StatusCode::BAD_REQUEST,
            id.as_ref(),
            INVALID_REQUEST,
            "clients send requests and notifications only; this message has no method",
            None,
        );
    };
    if let Some(i) = &id {
        if !(i.is_string() || i.is_i64() || i.is_u64()) {
            count(&state, method, "invalid");
            return rpc_error(
                StatusCode::BAD_REQUEST,
                None,
                INVALID_REQUEST,
                "the request id must be a string or an integer",
                None,
            );
        }
    }
    let params = match obj.get("params") {
        None => Value::Object(Map::new()),
        Some(p) if p.is_object() => p.clone(),
        Some(_) => {
            count(&state, method, "invalid");
            return rpc_error(
                StatusCode::BAD_REQUEST,
                id.as_ref(),
                INVALID_PARAMS,
                "params must be an object",
                None,
            );
        }
    };

    // Revision selection. A request whose `_meta` names a version is modern
    // and validated strictly; it can never fall through to the legacy
    // dispatch. Only `initialize`, or a request under the legacy header with
    // no modern metadata, is legacy.
    let meta_version = params
        .get("_meta")
        .and_then(|m| m.get(META_VERSION))
        .and_then(Value::as_str);
    let header_version = header_str(&headers, "mcp-protocol-version");
    let revision = if let Some(v) = meta_version {
        if header_version != Some(v) {
            count(&state, method, "header-mismatch");
            return rpc_error(
                StatusCode::BAD_REQUEST,
                id.as_ref(),
                HEADER_MISMATCH,
                "Header mismatch: MCP-Protocol-Version must equal the request's protocolVersion",
                None,
            );
        }
        if v != MODERN {
            count(&state, method, "unsupported-version");
            return rpc_error(
                StatusCode::BAD_REQUEST,
                id.as_ref(),
                UNSUPPORTED_VERSION,
                "Unsupported protocol version",
                Some(json!({"supported": SUPPORTED, "requested": v})),
            );
        }
        Revision::Modern
    } else if method == "initialize" {
        Revision::Legacy
    } else {
        match header_version {
            Some(LEGACY) => Revision::Legacy,
            Some(MODERN) => {
                count(&state, method, "invalid");
                return rpc_error(
                    StatusCode::BAD_REQUEST,
                    id.as_ref(),
                    INVALID_PARAMS,
                    "a 2026-07-28 request must carry params._meta with its protocolVersion",
                    None,
                );
            }
            other => {
                count(&state, method, "unsupported-version");
                return rpc_error(
                    StatusCode::BAD_REQUEST,
                    id.as_ref(),
                    UNSUPPORTED_VERSION,
                    "Unsupported protocol version",
                    Some(json!({"supported": SUPPORTED, "requested": other})),
                );
            }
        }
    };

    if revision == Revision::Modern {
        if header_str(&headers, "mcp-method") != Some(method) {
            count(&state, method, "header-mismatch");
            return rpc_error(
                StatusCode::BAD_REQUEST,
                id.as_ref(),
                HEADER_MISMATCH,
                "Header mismatch: Mcp-Method must equal the request method",
                None,
            );
        }
        if id.is_some()
            && params
                .get("_meta")
                .and_then(|m| m.get(META_CLIENT_CAPS))
                .is_none()
        {
            count(&state, method, "invalid");
            return rpc_error(
                StatusCode::BAD_REQUEST,
                id.as_ref(),
                INVALID_PARAMS,
                "params._meta must carry io.modelcontextprotocol/clientCapabilities",
                None,
            );
        }
        if method == "tools/call" {
            let named = params.get("name").and_then(Value::as_str);
            let header_name = header_str(&headers, "mcp-name").and_then(decode_header_value);
            if named.is_none() || header_name.as_deref() != named {
                count(&state, method, "header-mismatch");
                return rpc_error(
                    StatusCode::BAD_REQUEST,
                    id.as_ref(),
                    HEADER_MISMATCH,
                    "Header mismatch: Mcp-Name must equal params.name",
                    None,
                );
            }
        }
    }

    // Notifications: accepted with an empty 202 (nothing to answer).
    let Some(id) = id else {
        count(&state, method, "notification");
        return no_store((StatusCode::ACCEPTED, "").into_response());
    };

    let result = match (revision, method) {
        (Revision::Modern, "server/discover") => Ok(json!({
            "supportedVersions": SUPPORTED,
            "capabilities": {"tools": {}},
        })),
        (Revision::Legacy, "initialize") => {
            let requested = params.get("protocolVersion").and_then(Value::as_str);
            if requested != Some(LEGACY) {
                count(&state, method, "unsupported-version");
                return rpc_error(
                    StatusCode::BAD_REQUEST,
                    Some(&id),
                    INVALID_PARAMS,
                    "Unsupported protocol version",
                    Some(json!({"supported": SUPPORTED, "requested": requested})),
                );
            }
            Ok(json!({
                "protocolVersion": LEGACY,
                "capabilities": {"tools": {}},
                "serverInfo": server_info(),
            }))
        }
        (_, "ping") => Ok(json!({})),
        (_, "tools/list") => Ok(crate::mcp_tools::list(&state, &caller)),
        (_, "tools/call") => {
            let request_id = crate::middleware::new_request_id();
            crate::mcp_tools::call(&service, &caller, revision, &request_id, &params).await
        }
        _ => {
            count(&state, method, "method-not-found");
            let status = if revision == Revision::Modern {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::OK
            };
            return rpc_error(
                status,
                Some(&id),
                METHOD_NOT_FOUND,
                "Method not found",
                None,
            );
        }
    };
    match result {
        Ok(result) => {
            let body = rpc_result(&id, finish_result(revision, result));
            let bytes = serde_json::to_vec(&body).unwrap_or_default();
            if bytes.len() > state.mcp.settings.result_max_bytes {
                // Only `tools/call` can grow; the tool layer bounds its own
                // results and turns an oversized one into a typed tool error,
                // so reaching here means a protocol result outgrew the bound.
                count(&state, method, "too-large");
                return rpc_error(
                    StatusCode::OK,
                    Some(&id),
                    INVALID_REQUEST,
                    "the response exceeds MUNARIUM_MCP_RESULT_MAX_BYTES",
                    None,
                );
            }
            count(&state, method, "ok");
            let mut r = (StatusCode::OK, Body::from(bytes)).into_response();
            r.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            no_store(r)
        }
        Err(e) => {
            count(&state, method, "protocol-error");
            let status = if revision == Revision::Modern {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::OK
            };
            rpc_error(status, Some(&id), e.code, &e.message, None)
        }
    }
}

/// A protocol-level (JSON-RPC) error from a method body.
#[derive(Debug)]
pub struct RpcFailure {
    pub code: i64,
    pub message: String,
}

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;
