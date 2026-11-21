// SPDX-License-Identifier: Apache-2.0
//! ClearView MCP qualification tests (`docs/design/mcp-analysis.md` §8).
//!
//! The memory-store tests run everywhere: default-off routing (MCP-RULE-6),
//! settings validation, entry authority (A1-T1), protocol fixtures for both
//! revisions (A3-T1), the compiled table (§6) and the memory-only A6 refusal.
//! The PostgreSQL tests need `MUNARIUM_TEST_DATABASE_URL` and skip loudly
//! without it: the eleven-tool suite with native parity (A5-T1/A5-T4), session
//! authority, durable admission under injected faults (A6-T1/A6-T2),
//! generation opt-in with a recording provider (A2-T1), allowances under
//! retries and concurrency (A8/A9), interrupted-call recovery across two
//! `AppState`s over one database (A5-T5) and the default-off rollback
//! (A5-T3). Every fixture is fictional.
use super::*;
use crate::config::{AuthMode, Config, DocIntelConfig, SourceStoreConfig, StoreKind};
use axum::body::to_bytes;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use tower::ServiceExt;

const SECRET: &[u8] = b"mcp-test-secret-0123456789abcdef";
const RW: &str = "mcp-test-rw";

fn config(database_url: Option<String>, tenant: &str, instance: &str) -> Config {
    Config {
        http_addr: "127.0.0.1:0".into(),
        grpc_addr: None,
        ops_addr: "127.0.0.1:0".into(),
        store: if database_url.is_some() {
            StoreKind::Postgres
        } else {
            StoreKind::Memory
        },
        source_store: SourceStoreConfig::Mem,
        database_url,
        platform: None,
        auth: AuthMode::Static(vec![(RW.into(), tenant.into(), "rw".into())]),
        shutdown_grace_secs: 1,
        token_secret: Some(SECRET.to_vec()),
        token_ttl_secs: 3600,
        require_uid: false,
        interaction_body_max: 32768,
        token_revocation_check: true,
        matrix_base_url: None,
        matrix_admin_url: None,
        max_concurrency: 16,
        db_max_conns: 6,
        idempotency_ttl_secs: 86400,
        replica_count: 1,
        registry_ttl_secs: 15,
        session_idle_ttl_secs: 0,
        evidence_purge_interval_secs: 0,
        managed_provider_diagnostics: false,
        max_tokens: munarium_api_types::MaxTokensBudgets::default(),
        instance_id: instance.into(),
        doc_intel: DocIntelConfig::None,
    }
}

fn enabled() -> McpSettings {
    McpSettings {
        enabled: true,
        ..Default::default()
    }
}

fn token(tenant: &str, uid: &str, level: i32, scopes: &[&str], rb: Option<&[&str]>) -> String {
    munarium_access::issue(
        SECRET,
        uid,
        tenant,
        level,
        Vec::new(),
        scopes.iter().map(|s| s.to_string()).collect(),
        rb.map(|r| r.iter().map(|s| s.to_string()).collect()),
        3600,
        format!("tok-{}", uuid::Uuid::new_v4().simple()),
    )
    .unwrap()
    .0
}

fn expired(tenant: &str, uid: &str) -> String {
    let now = chrono::Utc::now().timestamp();
    munarium_access::mint(
        SECRET,
        &munarium_access::AccessClaims {
            sub: uid.into(),
            ten: tenant.into(),
            lvl: 0,
            cmp: vec![],
            scopes: vec!["mcp".into(), "query".into()],
            rb: None,
            jti: "tok-expired".into(),
            iat: now - 7200,
            exp: now - 3600,
        },
    )
    .unwrap()
}

/// A modern (2026-07-28) request body.
fn modern(method: &str, params: Value) -> Value {
    let mut params = if params.is_object() {
        params
    } else {
        json!({})
    };
    params["_meta"] = json!({
        "io.modelcontextprotocol/protocolVersion": MODERN,
        "io.modelcontextprotocol/clientCapabilities": {},
        "io.modelcontextprotocol/clientInfo": {"name": "mcp-test-client", "version": "0"},
    });
    json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params})
}

struct Reply {
    status: u16,
    headers: axum::http::HeaderMap,
    body: Value,
}

async fn send_raw(
    router: &axum::Router,
    method: &str,
    token: Option<&str>,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> Reply {
    let mut b = axum::http::Request::builder().method(method).uri("/mcp");
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    let resp = router
        .clone()
        .oneshot(b.body(axum::body::Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let bytes = to_bytes(resp.into_body(), 64 * 1024 * 1024).await.unwrap();
    Reply {
        status,
        headers,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    }
}

const JSON_HEADERS: &[(&str, &str)] = &[
    ("content-type", "application/json"),
    ("accept", "application/json, text/event-stream"),
];

/// A modern request with matching headers.
async fn rpc(router: &axum::Router, token: &str, method: &str, params: Value) -> Reply {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut headers: Vec<(&str, &str)> = JSON_HEADERS.to_vec();
    headers.push(("mcp-protocol-version", MODERN));
    headers.push(("mcp-method", method));
    if method == "tools/call" {
        headers.push(("mcp-name", &name));
    }
    send_raw(
        router,
        "POST",
        Some(token),
        &headers,
        serde_json::to_vec(&modern(method, params)).unwrap(),
    )
    .await
}

/// A legacy (2025-11-25) request after the handshake.
async fn legacy(router: &axum::Router, token: &str, method: &str, params: Value) -> Reply {
    let mut headers: Vec<(&str, &str)> = JSON_HEADERS.to_vec();
    headers.push(("mcp-protocol-version", LEGACY));
    send_raw(
        router,
        "POST",
        Some(token),
        &headers,
        serde_json::to_vec(
            &json!({"jsonrpc": "2.0", "id": "l-1", "method": method, "params": params}),
        )
        .unwrap(),
    )
    .await
}

/// `tools/call` -> the tool result object.
async fn tool(router: &axum::Router, token: &str, name: &str, args: Value) -> Value {
    let r = rpc(
        router,
        token,
        "tools/call",
        json!({"name": name, "arguments": args}),
    )
    .await;
    assert_eq!(r.status, 200, "{name}: {}", r.body);
    r.body["result"].clone()
}

fn tool_names(list: &Reply) -> Vec<String> {
    list.body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

fn problem_slug(result: &Value) -> String {
    assert_eq!(result["isError"], true, "expected a tool error: {result}");
    result["_meta"][crate::mcp_tools::PROBLEM_SLUG]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn key(id: &str, secs: i64) -> Value {
    json!({
        "id": id,
        "expires_at": (chrono::Utc::now() + chrono::Duration::seconds(secs))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    })
}

fn fresh_key() -> Value {
    key(&format!("k{}", uuid::Uuid::new_v4().simple()), 3600)
}

fn counter(state: &AppState, name: &str, needle: &str) -> u64 {
    state.metrics.counter_sum(name, needle)
}

// ===========================================================================
// memory store: routing, settings, entry, protocol, catalog
// ===========================================================================

async fn mem_state(settings: McpSettings) -> Arc<AppState> {
    AppState::new_with_mcp(config(None, "tenant-m", "mcp-mem"), settings)
        .await
        .unwrap()
}

/// MCP-RULE-6 / COMPAT-T4: installation alone never enables MCP. With the
/// switch unset there is no `/mcp` route on any method, and the native
/// surface answers exactly as before.
#[tokio::test]
async fn default_off_registers_no_route() {
    let state = mem_state(McpSettings::default()).await;
    let router = crate::rest::router(state.clone());
    let t = token("tenant-m", "u", 0, &["mcp", "query"], None);
    for method in ["POST", "GET", "DELETE"] {
        let r = send_raw(&router, method, Some(&t), JSON_HEADERS, b"{}".to_vec()).await;
        assert_eq!(r.status, 404, "{method} /mcp must not exist when disabled");
    }
    let health = router
        .clone()
        .oneshot(
            axum::http::Request::get("/healthz")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(health.status(), 200);
    assert_eq!(counter(&state, "munarium_mcp_requests_total", ""), 0);
}

#[test]
fn settings_default_off_and_validate() {
    let none = McpSettings::from_lookup(|_| None).unwrap();
    assert!(!none.enabled && !none.paid_tools);
    assert_eq!(none.execution, Execution::Enabled);
    assert_eq!(none.result_max_bytes, 262_144);
    assert_eq!(none.input_max_bytes, 1_048_576);
    assert!(none.generation.is_empty());
    let bad = |k: &'static str, v: &'static str| {
        McpSettings::from_lookup(move |key| (key == k).then(|| v.to_string())).is_err()
    };
    assert!(bad("MUNARIUM_MCP", "on"));
    assert!(bad("MUNARIUM_MCP_PAID_TOOLS", "yes"));
    assert!(bad("MUNARIUM_MCP_EXECUTION", "paused"));
    assert!(bad("MUNARIUM_MCP_RESULT_MAX_BYTES", "12"));
    assert!(bad("MUNARIUM_MCP_ALLOWED_ORIGINS", "https://a.example/"));
    assert!(bad("MUNARIUM_MCP_ALLOWED_ORIGINS", "a.example"));
    assert!(bad(
        "MUNARIUM_MCP_GENERATION_POLICY",
        "/nonexistent/policy.json"
    ));
    assert!(bad("MUNARIUM_MCP_KEY_MAX_VALIDITY_SECS", "60"));
    let on = McpSettings::from_lookup(|k| match k {
        "MUNARIUM_MCP" => Some("enabled".into()),
        "MUNARIUM_MCP_EXECUTION" => Some("recovery-only".into()),
        "MUNARIUM_MCP_ALLOWED_ORIGINS" => Some("https://a.example, http://b.example:8080".into()),
        _ => None,
    })
    .unwrap();
    assert!(on.enabled);
    assert_eq!(on.execution, Execution::RecoveryOnly);
    assert_eq!(on.allowed_origins.len(), 2);

    // An enabled endpoint refuses incompatible configurations at startup.
    let mut c = config(None, "t", "i");
    assert!(on.check_compatible(&c).is_ok());
    c.token_secret = None;
    assert!(on
        .check_compatible(&c)
        .unwrap_err()
        .contains("MUNARIUM_TOKEN_SECRET"));
    c.auth = AuthMode::Disabled;
    assert!(on.check_compatible(&c).is_err());
    assert!(McpSettings::default().check_compatible(&c).is_ok());
}

#[tokio::test]
async fn an_incompatible_enabled_configuration_fails_startup() {
    let mut c = config(None, "t", "i");
    c.auth = AuthMode::Disabled;
    assert!(AppState::new_with_mcp(c, enabled()).await.is_err());
}

/// §7.4 / A6-T3: an MCP request takes one ordinary REST slot (R7) and an
/// MCP slot before its body is read; with either exhausted it is shed with
/// 503 `overloaded` and `Retry-After`, while health keeps answering.
#[tokio::test]
async fn r7_mcp_admission_sheds_inside_the_rest_ceiling() {
    let mut settings = enabled();
    settings.max_concurrency = 1;
    let state = AppState::new_with_mcp_admission(
        config(None, "tenant-m", "mcp-r7"),
        crate::admission::AdmissionConfig::new(16, 2, 4).unwrap(),
        settings,
    )
    .await
    .unwrap();
    assert_eq!(state.mcp.slot_capacity, 1);
    let router = crate::rest::router(state.clone());
    let t = token("tenant-m", "u", 0, &["mcp", "query"], None);
    let held = state.mcp.slots.clone().try_acquire_owned().unwrap();
    let r = rpc(&router, &t, "server/discover", json!({})).await;
    assert_eq!(r.status, 503);
    assert_eq!(r.headers["retry-after"], "1");
    assert!(r.body["type"].as_str().unwrap().ends_with("/overloaded"));
    drop(held);
    let ordinary: Vec<_> = (0..14)
        .map(|_| {
            state
                .admission
                .rest
                .ordinary
                .clone()
                .try_acquire_owned()
                .unwrap()
        })
        .collect();
    let r = rpc(&router, &t, "server/discover", json!({})).await;
    assert_eq!(r.status, 503, "MCP never takes the control reserve");
    let health = router
        .clone()
        .oneshot(
            axum::http::Request::get("/healthz")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(health.status(), 200);
    drop(ordinary);
    assert_eq!(
        rpc(&router, &t, "server/discover", json!({})).await.status,
        200
    );
    assert!(counter(&state, "munarium_admission_shed_total", "plane=\"rest\"") >= 1);
}

/// A1-T1 (entry rows of the authority table) and the credential refusals.
#[tokio::test]
async fn a1_entry_scope_and_credential_refusals() {
    let state = mem_state(enabled()).await;
    let router = crate::rest::router(state.clone());
    let t = "tenant-m";

    // `query` only: entry refused with 403 scope-missing.
    let r = rpc(
        &router,
        &token(t, "u", 0, &["query"], None),
        "server/discover",
        json!({}),
    )
    .await;
    assert_eq!(r.status, 403);
    assert!(r.body["type"].as_str().unwrap().ends_with("/scope-missing"));

    // `mcp` only: discovery succeeds, the list is empty.
    let mcp_only = token(t, "u", 0, &["mcp"], None);
    let d = rpc(&router, &mcp_only, "server/discover", json!({})).await;
    assert_eq!(d.status, 200, "{}", d.body);
    assert_eq!(
        tool_names(&rpc(&router, &mcp_only, "tools/list", json!({})).await).len(),
        0
    );
    // ...and a direct call is refused before dispatch for lack of `query`.
    let res = tool(&router, &mcp_only, "list_collections", json!({})).await;
    assert_eq!(problem_slug(&res), "scope-missing");

    // `mcp` + `query`: the nine query tools, never the evidence readers.
    let both = token(t, "u", 0, &["mcp", "query"], None);
    let names = tool_names(&rpc(&router, &both, "tools/list", json!({})).await);
    assert_eq!(
        names,
        vec![
            "list_collections",
            "list_runbooks",
            "search_collection",
            "ask_collections",
            "open_session",
            "ask_session",
            "get_session",
            "close_session",
            "authorize_publication"
        ]
    );
    let res = tool(
        &router,
        &both,
        "get_evidence",
        json!({"evidence_id": "ev-x"}),
    )
    .await;
    assert_eq!(problem_slug(&res), "scope-missing", "{res}");

    // `mcp` + read-only evidence: exactly the two readers. A v1.4 `evidence`
    // token with `mcp` is NOT admitted to them (0025 open choice: only the
    // read-only grant opens the MCP readers).
    let readers = token(t, "u", 0, &["mcp", "evidence_read"], None);
    assert_eq!(
        tool_names(&rpc(&router, &readers, "tools/list", json!({})).await),
        vec!["get_evidence", "read_evidence_rows"]
    );
    let sealer = token(t, "u", 0, &["mcp", "evidence"], None);
    assert!(tool_names(&rpc(&router, &sealer, "tools/list", json!({})).await).is_empty());

    // No bearer / invalid / expired: 401 with a Bearer challenge.
    let r = send_raw(&router, "POST", None, JSON_HEADERS, b"{}".to_vec()).await;
    assert_eq!(r.status, 401);
    assert!(r.headers["www-authenticate"]
        .to_str()
        .unwrap()
        .starts_with("Bearer"));
    let r = rpc(&router, "not-a-token", "server/discover", json!({})).await;
    assert_eq!(r.status, 401);
    let r = rpc(&router, &expired(t, "u"), "server/discover", json!({})).await;
    assert_eq!(r.status, 401);
    assert!(r.body["type"].as_str().unwrap().ends_with("/token-expired"));
    assert!(r.headers.contains_key("www-authenticate"));

    // Static role credentials are refused even with every native authority.
    let r = rpc(&router, RW, "server/discover", json!({})).await;
    assert_eq!(r.status, 403);

    // An asserted uid must equal the verified subject.
    let mut headers: Vec<(&str, &str)> = JSON_HEADERS.to_vec();
    headers.extend([
        ("mcp-protocol-version", MODERN),
        ("mcp-method", "server/discover"),
        ("x-munarium-uid", "someone-else"),
    ]);
    let r = send_raw(
        &router,
        "POST",
        Some(&both),
        &headers,
        serde_json::to_vec(&modern("server/discover", json!({}))).unwrap(),
    )
    .await;
    assert_eq!(r.status, 403);
    assert!(r.body["type"].as_str().unwrap().ends_with("/uid-mismatch"));

    // The `mcp` scope grants nothing on a native route.
    let native = router
        .clone()
        .oneshot(
            axum::http::Request::post("/v1.2/search")
                .header("authorization", format!("Bearer {mcp_only}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({"query": "x", "collection": "c"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(native.status(), 403);
}

/// A3-T1: modern discovery, headers, errors and unsupported features; the
/// legacy handshake; transport refusals.
#[tokio::test]
async fn a3_protocol_fixtures_for_both_revisions() {
    let mut settings = enabled();
    settings.allowed_origins = vec!["https://agent.example".into()];
    let state = mem_state(settings).await;
    let router = crate::rest::router(state.clone());
    let t = token("tenant-m", "u", 0, &["mcp", "query"], None);

    let d = rpc(&router, &t, "server/discover", json!({})).await;
    assert_eq!(d.status, 200);
    assert_eq!(d.headers["cache-control"], "private, no-store");
    let res = &d.body["result"];
    assert_eq!(res["resultType"], "complete");
    assert_eq!(res["supportedVersions"], json!([MODERN, LEGACY]));
    assert_eq!(res["capabilities"], json!({"tools": {}}));
    assert_eq!(
        res["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "munarium-server"
    );
    assert_eq!(d.body["id"], 7);

    // A client may start directly with a tool request (no handshake).
    let l = rpc(&router, &t, "tools/list", json!({})).await;
    assert_eq!(l.body["result"]["resultType"], "complete");
    for tool in l.body["result"]["tools"].as_array().unwrap() {
        assert_eq!(tool["inputSchema"]["additionalProperties"], false, "{tool}");
        assert_eq!(tool["inputSchema"]["type"], "object");
    }

    let body = |m: &str, p: Value| serde_json::to_vec(&modern(m, p)).unwrap();
    let mut h: Vec<(&str, &str)> = JSON_HEADERS.to_vec();
    // Header version differs from the body: -32020.
    h.extend([
        ("mcp-protocol-version", LEGACY),
        ("mcp-method", "tools/list"),
    ]);
    let r = send_raw(&router, "POST", Some(&t), &h, body("tools/list", json!({}))).await;
    assert_eq!(
        (r.status, r.body["error"]["code"].as_i64()),
        (400, Some(-32020))
    );
    // Mcp-Method differs from the method: -32020.
    let mut h: Vec<(&str, &str)> = JSON_HEADERS.to_vec();
    h.extend([
        ("mcp-protocol-version", MODERN),
        ("mcp-method", "tools/call"),
    ]);
    let r = send_raw(&router, "POST", Some(&t), &h, body("tools/list", json!({}))).await;
    assert_eq!(r.body["error"]["code"], -32020);
    // Mcp-Name differs, or is missing: -32020. A base64-encoded name decodes.
    for name in [None, Some("list_runbooks")] {
        let mut h: Vec<(&str, &str)> = JSON_HEADERS.to_vec();
        h.extend([
            ("mcp-protocol-version", MODERN),
            ("mcp-method", "tools/call"),
        ]);
        if let Some(n) = name {
            h.push(("mcp-name", n));
        }
        let r = send_raw(
            &router,
            "POST",
            Some(&t),
            &h,
            body("tools/call", json!({"name": "list_collections"})),
        )
        .await;
        assert_eq!(r.body["error"]["code"], -32020, "{name:?}");
    }
    assert_eq!(
        decode_header_value("=?base64?bGlzdF9jb2xsZWN0aW9ucw==?=").as_deref(),
        Some("list_collections")
    );
    // Unsupported modern version: -32022 with the supported list.
    let mut v = modern("tools/list", json!({}));
    v["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] = json!("2027-01-01");
    let mut h: Vec<(&str, &str)> = JSON_HEADERS.to_vec();
    h.extend([
        ("mcp-protocol-version", "2027-01-01"),
        ("mcp-method", "tools/list"),
    ]);
    let r = send_raw(
        &router,
        "POST",
        Some(&t),
        &h,
        serde_json::to_vec(&v).unwrap(),
    )
    .await;
    assert_eq!(r.status, 400);
    assert_eq!(r.body["error"]["code"], -32022);
    assert_eq!(
        r.body["error"]["data"]["supported"],
        json!([MODERN, LEGACY])
    );
    // A modern header without modern metadata never falls back to legacy.
    let mut h: Vec<(&str, &str)> = JSON_HEADERS.to_vec();
    h.extend([
        ("mcp-protocol-version", MODERN),
        ("mcp-method", "tools/list"),
    ]);
    let r = send_raw(
        &router,
        "POST",
        Some(&t),
        &h,
        serde_json::to_vec(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})).unwrap(),
    )
    .await;
    assert_eq!(
        (r.status, r.body["error"]["code"].as_i64()),
        (400, Some(-32602))
    );
    // No version anywhere: unsupported.
    let r = send_raw(
        &router,
        "POST",
        Some(&t),
        JSON_HEADERS,
        serde_json::to_vec(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})).unwrap(),
    )
    .await;
    assert_eq!(r.body["error"]["code"], -32022);
    // Unknown method and an unsupported optional feature: 404 / -32601.
    for m in ["resources/list", "subscriptions/listen", "tasks/get"] {
        let r = rpc(&router, &t, m, json!({})).await;
        assert_eq!(
            (r.status, r.body["error"]["code"].as_i64()),
            (404, Some(-32601)),
            "{m}"
        );
    }
    // Unknown tool name inside a valid call: -32602.
    let r = rpc(&router, &t, "tools/call", json!({"name": "complete"})).await;
    assert_eq!(r.body["error"]["code"], -32602);
    // Notifications: 202, empty body.
    let mut n = modern("notifications/cancelled", json!({}));
    n.as_object_mut().unwrap().remove("id");
    let mut h: Vec<(&str, &str)> = JSON_HEADERS.to_vec();
    h.extend([
        ("mcp-protocol-version", MODERN),
        ("mcp-method", "notifications/cancelled"),
    ]);
    let r = send_raw(
        &router,
        "POST",
        Some(&t),
        &h,
        serde_json::to_vec(&n).unwrap(),
    )
    .await;
    assert_eq!((r.status, r.body.clone()), (202, Value::Null));
    // Batches, wrong media types, oversized input, GET/DELETE, origin.
    let r = send_raw(&router, "POST", Some(&t), JSON_HEADERS, b"[{}]".to_vec()).await;
    assert_eq!(
        (r.status, r.body["error"]["code"].as_i64()),
        (400, Some(-32600))
    );
    let r = send_raw(
        &router,
        "POST",
        Some(&t),
        &[
            ("content-type", "text/plain"),
            ("accept", "application/json"),
        ],
        b"{}".to_vec(),
    )
    .await;
    assert_eq!(r.status, 415);
    let r = send_raw(
        &router,
        "POST",
        Some(&t),
        &[
            ("content-type", "application/json"),
            ("accept", "text/html"),
        ],
        b"{}".to_vec(),
    )
    .await;
    assert_eq!(r.status, 406);
    let r = send_raw(
        &router,
        "POST",
        Some(&t),
        JSON_HEADERS,
        vec![b' '; DEFAULT_INPUT_MAX_BYTES + 1],
    )
    .await;
    assert_eq!(r.status, 413);
    for m in ["GET", "DELETE"] {
        let r = send_raw(&router, m, Some(&t), &[], vec![]).await;
        assert_eq!(r.status, 405, "{m}");
        assert_eq!(r.headers["allow"], "POST");
    }
    let mut h: Vec<(&str, &str)> = JSON_HEADERS.to_vec();
    h.extend([
        ("origin", "https://evil.example"),
        ("mcp-protocol-version", MODERN),
        ("mcp-method", "server/discover"),
    ]);
    let r = send_raw(
        &router,
        "POST",
        Some(&t),
        &h,
        body("server/discover", json!({})),
    )
    .await;
    assert_eq!(r.status, 403);
    h[2] = ("origin", "https://agent.example");
    let r = send_raw(
        &router,
        "POST",
        Some(&t),
        &h,
        body("server/discover", json!({})),
    )
    .await;
    assert_eq!(r.status, 200);
    // An unreadable Origin is present, not absent; and every Origin header
    // is checked, not only the first.
    let with_origins = |origins: Vec<axum::http::HeaderValue>| {
        let mut b = axum::http::Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("authorization", format!("Bearer {t}"))
            .header("mcp-protocol-version", MODERN)
            .header("mcp-method", "server/discover");
        for (k, v) in JSON_HEADERS {
            b = b.header(*k, *v);
        }
        for o in origins {
            b = b.header("origin", o);
        }
        b.body(axum::body::Body::from(body("server/discover", json!({}))))
            .unwrap()
    };
    let allowed = axum::http::HeaderValue::from_static("https://agent.example");
    for (origins, expected) in [
        (
            vec![axum::http::HeaderValue::from_bytes(b"https://\xe9vil.example").unwrap()],
            403,
        ),
        (
            vec![
                allowed.clone(),
                axum::http::HeaderValue::from_static("https://evil.example"),
            ],
            403,
        ),
        (
            vec![
                allowed.clone(),
                axum::http::HeaderValue::from_bytes(b"https://agent.example\xff").unwrap(),
            ],
            403,
        ),
        (vec![allowed.clone(), allowed.clone()], 200),
    ] {
        let label = format!("{origins:?}");
        let r = router.clone().oneshot(with_origins(origins)).await.unwrap();
        assert_eq!(r.status().as_u16(), expected, "{label}");
    }

    // Legacy: initialize, initialized, ping, list under the legacy header.
    let init = send_raw(
        &router,
        "POST",
        Some(&t),
        JSON_HEADERS,
        serde_json::to_vec(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": LEGACY, "capabilities": {},
                       "clientInfo": {"name": "legacy-test", "version": "0"}}}))
        .unwrap(),
    )
    .await;
    assert_eq!(init.status, 200, "{}", init.body);
    assert_eq!(init.body["result"]["protocolVersion"], LEGACY);
    assert_eq!(init.body["result"]["capabilities"], json!({"tools": {}}));
    assert!(init.body["result"].get("resultType").is_none());
    assert!(
        init.headers.get("mcp-session-id").is_none(),
        "no MCP session id"
    );
    let mut h: Vec<(&str, &str)> = JSON_HEADERS.to_vec();
    h.push(("mcp-protocol-version", LEGACY));
    let r = send_raw(
        &router,
        "POST",
        Some(&t),
        &h,
        serde_json::to_vec(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, 202);
    assert_eq!(
        legacy(&router, &t, "ping", json!({})).await.body["result"],
        json!({})
    );
    let l = legacy(&router, &t, "tools/list", json!({})).await;
    assert_eq!(tool_names(&l).len(), 9);
    assert!(l.body["result"].get("resultType").is_none());
    let bad_init = send_raw(
        &router,
        "POST",
        Some(&t),
        JSON_HEADERS,
        serde_json::to_vec(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-03-26", "capabilities": {}}}))
        .unwrap(),
    )
    .await;
    assert_eq!(
        bad_init.body["error"]["data"]["supported"],
        json!([MODERN, LEGACY])
    );
}

/// A6-T1 (memory-only): with no durable audit store every tool call is
/// refused, content-free, and nothing is dispatched. Discovery and listing
/// are protocol operations and still work.
#[tokio::test]
async fn a6_a_memory_only_deployment_refuses_every_tool() {
    let state = mem_state(enabled()).await;
    let router = crate::rest::router(state.clone());
    let t = token("tenant-m", "u", 0, &["mcp", "query", "evidence_read"], None);
    for (name, args) in [
        ("list_collections", json!({})),
        ("list_runbooks", json!({})),
        (
            "search_collection",
            json!({"operation_key": fresh_key(), "query": "q", "collection": "c"}),
        ),
        (
            "open_session",
            json!({"operation_key": fresh_key(), "runbook": "r"}),
        ),
        ("get_session", json!({"session_id": "ses-x"})),
        (
            "close_session",
            json!({"operation_key": fresh_key(), "session_id": "ses-x"}),
        ),
        (
            "authorize_publication",
            json!({"collection": "c", "publication_id": "p"}),
        ),
        ("get_evidence", json!({"evidence_id": "ev"})),
        ("read_evidence_rows", json!({"evidence_id": "ev"})),
    ] {
        let res = tool(&router, &t, name, args).await;
        assert_eq!(problem_slug(&res), "mcp-audit-unavailable", "{name}: {res}");
        assert_eq!(res["structuredContent"]["executed"], false);
    }
    assert_eq!(counter(&state, "munarium_mcp_dispatches_total", ""), 0);
    assert_eq!(
        counter(
            &state,
            "munarium_mcp_audit_refusals_total",
            "no-durable-store"
        ),
        9
    );
    // Generating tools: with paid tools off only the recovery form is
    // offered; a direct execution is refused before admission.
    let list = rpc(&router, &t, "tools/list", json!({})).await;
    let ask = list.body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "ask_session")
        .unwrap()
        .clone();
    assert_eq!(
        ask["inputSchema"]["properties"].as_object().unwrap().len(),
        1,
        "recovery form only: {ask}"
    );
    let res = tool(
        &router,
        &t,
        "ask_session",
        json!({"operation_key": fresh_key(), "session_id": "s", "query": "q"}),
    )
    .await;
    assert_eq!(problem_slug(&res), "mcp-tool-unavailable");
    // Invalid arguments are a tool error before admission.
    let res = tool(&router, &t, "search_collection", json!({"operation_key": fresh_key(), "query": "q", "collection": "c", "route": "/v1/providers"})).await;
    assert_eq!(problem_slug(&res), "invalid-input");
    let res = tool(
        &router,
        &t,
        "search_collection",
        json!({"query": "q", "collection": "c"}),
    )
    .await;
    assert_eq!(
        problem_slug(&res),
        "invalid-input",
        "a keyed tool needs its key"
    );
}

/// §6: the table is closed. Every routed form targets an allowlisted native
/// route that exists, no excluded plane is reachable by any tool, and the
/// listing is deterministic and tenant-data free.
#[test]
fn the_compiled_table_is_closed() {
    use crate::mcp_tools::{Handler, CATALOG, ROUTE_ALLOWLIST};
    assert_eq!(CATALOG.len(), 11);
    let contract: Vec<String> = crate::openapi::doc().paths.paths.keys().cloned().collect();
    let mut routed = 0;
    for t in CATALOG {
        if let Handler::Routed { method, template } = t.handler {
            routed += 1;
            assert!(
                ROUTE_ALLOWLIST.contains(&(method, template)),
                "{} -> {method} {template} is not allowlisted",
                t.name
            );
            assert!(
                contract.iter().any(|p| p == template),
                "{template} is not a native route"
            );
        }
        assert!(
            t.scope == munarium_access::SCOPE_QUERY
                || t.scope == munarium_access::SCOPE_EVIDENCE_READ,
            "{}",
            t.name
        );
    }
    assert_eq!(routed, ROUTE_ALLOWLIST.len());
    for (_, template) in ROUTE_ALLOWLIST {
        for excluded in [
            "/v1/indexes",
            "/v1/index-",
            "/v1/collections",
            "/v1/shapes",
            "/v1/runs",
            "/v1/authoring",
            "/v1/chronology-rules",
            "/v1/sources",
            "/v1/ingest",
            "/v1/versions",
            "/v1/claims",
            "/v1/providers",
            "/v1/max-tokens",
            "/v1/monetary",
            "/v1/access-tokens",
            "/v1/reports",
            "/admin",
            "/v1/command-recovery",
            "/v1/source-retention",
            "/v1/platform",
            "/v1/retrieval-rollout",
            "/v1.2/vocabulary",
        ] {
            assert!(
                !template.starts_with(excluded),
                "{template} is excluded (§6)"
            );
        }
        assert!(
            !template.ends_with("/governance") && !template.contains("/vocabulary"),
            "{template}"
        );
    }
    // Evidence writes are never reachable: only the two GET readers.
    assert!(ROUTE_ALLOWLIST
        .iter()
        .filter(|(_, t)| t.starts_with("/v1/evidence"))
        .all(|(m, _)| *m == "GET"));
}

// ===========================================================================
// PostgreSQL
// ===========================================================================

struct Abort(tokio::task::JoinHandle<()>);
impl Drop for Abort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A recording ollama-compatible provider: counts physical arrivals and
/// reports `input`/`output` usage per call.
struct Recorder {
    arrivals: Arc<AtomicUsize>,
    endpoint: String,
    _server: Abort,
}

async fn recorder(input: u64, output: u64) -> Recorder {
    let arrivals = Arc::new(AtomicUsize::new(0));
    let seen = arrivals.clone();
    let app = axum::Router::new().route(
        "/api/chat",
        axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
            let seen = seen.clone();
            async move {
                seen.fetch_add(1, Ordering::SeqCst);
                // A structured (schema-constrained) answer for /v1.2/query,
                // plain text for session turns and expansion.
                let content = if body.get("format").is_some() {
                    json!({"status": "supported", "answer": "The alpha archive is described.",
                        "citations": [{"id": "p1", "quote": "Fictional orchid archive evidence from alpha."}]})
                    .to_string()
                } else {
                    "The fictional orchid archive is described [alpha].".to_string()
                };
                axum::Json(json!({"done": true, "done_reason": "stop",
                    "message": {"role": "assistant", "content": content},
                    "prompt_eval_count": input, "eval_count": output}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = Abort(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap()
    }));
    Recorder {
        arrivals,
        endpoint,
        _server: server,
    }
}

async fn indexed(state: &AppState, tenant: &str, name: &str, level: i32, text: &[u8]) -> String {
    let retrieval = state.retrieval_for(tenant).unwrap();
    let collection = retrieval
        .ensure_collection(name, "article@1", level, &[], None)
        .await
        .unwrap();
    let (source, _, _) = retrieval
        .put_source(
            "",
            "text/plain",
            &format!("{name}.txt"),
            Some("article@1"),
            text,
        )
        .await
        .unwrap();
    retrieval
        .bind_source(&collection.id, &source, None)
        .await
        .unwrap();
    retrieval
        .build_collection_index(&collection.id, 400, 1, true)
        .await
        .unwrap();
    collection.id
}

struct Fixture {
    url: String,
    tenant: String,
    a: Arc<AppState>,
    ra: axum::Router,
    rec: Recorder,
    alpha: String,
    beta: String,
}

fn policy(
    tenant: &str,
    owner: Option<&str>,
    request: Option<u64>,
    daily: Option<u64>,
) -> crate::mcp_generation::GenerationPolicy {
    let mut entries = Vec::new();
    for c in ["alpha", "beta"] {
        let mut e = json!({"tenant": tenant, "collection": c});
        if let Some(o) = owner {
            e["budget_owner"] = json!(o);
        }
        if let Some(r) = request {
            e["request_tokens"] = json!(r);
        }
        if let Some(d) = daily {
            e["daily_tokens"] = json!(d);
        }
        entries.push(e);
    }
    crate::mcp_generation::GenerationPolicy::parse(
        &json!({"version": 1, "collections": entries}).to_string(),
    )
    .unwrap()
}

/// A second replica over the same database with its own settings.
async fn replica(
    f: &Fixture,
    settings: McpSettings,
    instance: &str,
) -> (Arc<AppState>, axum::Router) {
    let state = AppState::new_with_mcp(config(Some(f.url.clone()), &f.tenant, instance), settings)
        .await
        .unwrap();
    state.fleet.set_environment(&state.fleet.replica_id);
    crate::replica_fleet::beat(&state).await.unwrap();
    apply_provider(&state, &f.tenant, &f.rec.endpoint).await;
    let router = crate::rest::router(state.clone());
    (state, router)
}

async fn apply_provider(state: &AppState, tenant: &str, endpoint: &str) {
    apply_named(state, tenant, "fixture", endpoint).await;
}

async fn apply_named(state: &AppState, tenant: &str, name: &str, endpoint: &str) {
    {
        state
            .providers
            .apply(state, tenant, &format!(
                "apiVersion: munarium.ioka.io/v1\nkind: ProviderConfig\nmetadata: {{ name: {name} }}\nspec:\n  provider: ollama\n  endpoint: {endpoint}\n  models: {{ complete: [fixture], fast: fixture, capable: fixture }}\n"
            ))
            .await
            .unwrap();
    }
}

async fn fixture(url: String, settings: McpSettings) -> Fixture {
    let tenant = format!("mcp-{}", uuid::Uuid::new_v4().simple());
    let a = AppState::new_with_mcp(config(Some(url.clone()), &tenant, "mcp-a"), settings)
        .await
        .unwrap();
    let rec = recorder(30, 10).await;
    apply_provider(&a, &tenant, &rec.endpoint).await;
    let alpha = indexed(
        &a,
        &tenant,
        "alpha",
        0,
        b"Fictional orchid archive evidence from alpha.",
    )
    .await;
    let beta = indexed(
        &a,
        &tenant,
        "beta",
        0,
        b"Fictional orchid archive notes from beta.",
    )
    .await;
    indexed(
        &a,
        &tenant,
        "hidden",
        5,
        b"Fictional orchid archive ledger, restricted.",
    )
    .await;
    for (name, collections, expansion, provider) in [
        ("handbook", "alpha, beta", false, "fixture"),
        ("expander", "alpha", true, "fixture"),
        ("secret", "hidden", false, "fixture"),
    ] {
        let retrieval = if expansion {
            "  retrieval:\n    modelQueryExpansion: { maxTerms: 4, maxTokens: 64, required: false }\n"
        } else {
            ""
        };
        let expansion_task = if expansion {
            format!("      query_expansion: {{ provider: {provider}, tier: fast }}\n")
        } else {
            String::new()
        };
        let yaml = format!(
            "apiVersion: munarium.ioka.io/v1\nkind: Runbook\nmetadata: {{ name: {name}, version: 1 }}\nspec:\n  collections: [{}]\n{retrieval}  models:\n    tasks:\n{expansion_task}      completion: {{ provider: {provider}, tier: fast }}\n  completion: {{ promptTemplate: \"{{query}}\\n{{context}}\", maxTokens: 64 }}\n  steps: [{{ buildIndex: {{}} }}]\n",
            collections
                .split(", ")
                .map(|c| format!("{{ name: {c}, shape: article@1 }}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        sqlx::query("INSERT INTO runbooks (tenant_id, runbook_ref, yaml) VALUES ($1, $2, $3)")
            .bind(&tenant)
            .bind(format!("{name}@1"))
            .bind(yaml)
            .execute(a.pg_pool().unwrap())
            .await
            .unwrap();
    }
    let ra = crate::rest::router(a.clone());
    // `ask_collections` (POST /v1.2/query) without a governance policy uses
    // the tenant's vocabulary model routing: point it at the recorder, with
    // background vocabulary generation off.
    let get = ra
        .clone()
        .oneshot(
            axum::http::Request::get("/v1.2/vocabulary-settings")
                .header("authorization", format!("Bearer {RW}"))
                .header("x-munarium-uid", "operator")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mut settings: Value =
        serde_json::from_slice(&to_bytes(get.into_body(), 1 << 20).await.unwrap()).unwrap();
    settings["provider"] = json!("fixture");
    settings["auto_generate"] = json!(false);
    let put = ra
        .clone()
        .oneshot(
            axum::http::Request::put("/v1.2/vocabulary-settings")
                .header("authorization", format!("Bearer {RW}"))
                .header("x-munarium-uid", "operator")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(settings.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put.status(), 200);
    Fixture {
        url,
        tenant,
        a,
        ra,
        rec,
        alpha,
        beta,
    }
}

impl Fixture {
    fn tok(&self, uid: &str, level: i32, scopes: &[&str]) -> String {
        token(&self.tenant, uid, level, scopes, None)
    }

    fn reader(&self, uid: &str) -> String {
        self.tok(uid, 0, &["mcp", "query", "evidence_read"])
    }

    async fn native(
        &self,
        method: &str,
        path: &str,
        tok: &str,
        uid: &str,
        body: Option<Value>,
    ) -> (u16, Vec<u8>) {
        let mut b = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("authorization", format!("Bearer {tok}"))
            .header("x-munarium-uid", uid)
            .header("content-type", "application/json");
        if method == "GET" {
            b = b.header("accept", "application/json");
        }
        let resp = self
            .ra
            .clone()
            .oneshot(
                b.body(match body {
                    Some(v) => axum::body::Body::from(v.to_string()),
                    None => axum::body::Body::empty(),
                })
                .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status().as_u16();
        (
            status,
            to_bytes(resp.into_body(), 64 << 20).await.unwrap().to_vec(),
        )
    }

    async fn sessions_of(&self, uid: &str) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM sessions WHERE tenant_id = $1 AND uid = $2")
            .bind(&self.tenant)
            .bind(uid)
            .fetch_one(self.a.pg_pool().unwrap())
            .await
            .unwrap()
    }

    async fn invocations(&self) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM mcp_invocations WHERE tenant_id = $1")
            .bind(&self.tenant)
            .fetch_one(self.a.pg_pool().unwrap())
            .await
            .unwrap()
    }

    async fn attempts(&self) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM mcp_token_attempts WHERE tenant_id = $1 AND state <> 'released'",
        )
        .bind(&self.tenant)
        .fetch_one(self.a.pg_pool().unwrap())
        .await
        .unwrap()
    }
}

fn database() -> Option<String> {
    match std::env::var("MUNARIUM_TEST_DATABASE_URL") {
        Ok(u) => Some(u),
        Err(_) => {
            eprintln!("ClearView MCP PostgreSQL tests NOT RUN: MUNARIUM_TEST_DATABASE_URL unset");
            None
        }
    }
}

fn session_of(result: &Value) -> String {
    result["structuredContent"]["session_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no session id: {result}"))
        .to_string()
}

fn paid() -> McpSettings {
    McpSettings {
        enabled: true,
        paid_tools: true,
        ..Default::default()
    }
}

#[tokio::test]
async fn a2_live_policy_mismatch_refuses_generation_until_fleet_converges() {
    let Some(url) = database() else { return };
    let f = fixture(url, enabled()).await;
    let policy_a = McpSettings {
        generation: policy(&f.tenant, Some("library"), Some(100_000), Some(1_000_000)),
        ..paid()
    };
    let policy_b = McpSettings {
        generation: policy(&f.tenant, Some("library"), Some(90_000), Some(1_000_000)),
        ..paid()
    };
    let (a, ra) = replica(&f, policy_a.clone(), "policy-a").await;
    let (b, rb) = replica(&f, policy_b, "policy-b").await;
    b.fleet.set_environment(&a.fleet.environment());
    crate::replica_fleet::beat(&b).await.unwrap();
    let tok = f.reader("fleet-reader");
    let question = || {
        json!({
            "operation_key": fresh_key(), "question": "What is in the orchid archive?",
            "collections": ["alpha"]
        })
    };
    for router in [&ra, &rb] {
        let result = tool(router, &tok, "ask_collections", question()).await;
        assert_eq!(
            problem_slug(&result),
            "mcp-generation-not-authorized",
            "{result}"
        );
    }
    assert_eq!(f.rec.arrivals.load(Ordering::SeqCst), 0);
    assert_eq!(f.attempts().await, 0);
    let report = crate::scale_diagnostics::op_scale_report(&a, &f.tenant).await;
    assert_eq!(
        report.replica_membership.mcp_generation_policy_mismatch,
        Some(true)
    );
    assert!(crate::scale_diagnostics::health_section(&report)
        .contains("live replicas disagree on the MCP generation policy"));

    // Replace the differing replica; a heartbeat from a separate environment
    // above and the expired heartbeat in this environment do not count.
    let (replacement, _) = replica(&f, policy_a, "policy-b-restarted").await;
    replacement.fleet.set_environment(&a.fleet.environment());
    crate::replica_fleet::beat(&replacement).await.unwrap();
    sqlx::query(
        "UPDATE server_replica_heartbeats SET expires_at = now()
          WHERE environment_id = $1 AND replica_id = $2",
    )
    .bind(a.fleet.environment())
    .bind(&b.fleet.replica_id)
    .execute(a.pg_pool().unwrap())
    .await
    .unwrap();
    let result = tool(&ra, &tok, "ask_collections", question()).await;
    assert_eq!(result["isError"], false, "{result}");
    assert!(f.rec.arrivals.load(Ordering::SeqCst) > 0);
    let report = crate::scale_diagnostics::op_scale_report(&a, &f.tenant).await;
    assert_eq!(
        report.replica_membership.mcp_generation_policy_mismatch,
        Some(false)
    );
}

/// A5-T1 / A5-T4: all eleven tools through `/mcp`, each compared with its
/// actual native result where one exists; the filtered listings and their
/// page boundaries; the session projection's native field values.
#[tokio::test]
async fn a5_the_eleven_tools_with_native_parity() {
    let Some(url) = database() else { return };
    let mut settings = paid();
    let t = format!("mcp-{}", uuid::Uuid::new_v4().simple());
    let _ = t;
    settings.generation = crate::mcp_generation::GenerationPolicy::default();
    let mut f = fixture(url, settings).await;
    // The generation policy names this fixture's tenant.
    let mut s = paid();
    s.generation = policy(&f.tenant, Some("library"), Some(100_000), Some(1_000_000));
    let (a, ra) = replica(&f, s, "mcp-a2").await;
    f.a = a;
    f.ra = ra;
    let uid = "parity-user";
    let tok = f.reader(uid);
    let r = &f.ra;

    // list_collections: authorized before paging; `hidden` never appears.
    let page1 = tool(r, &tok, "list_collections", json!({"limit": 1})).await;
    assert_eq!(page1["_meta"][crate::mcp_tools::RESULT_KIND], "projection");
    let p = &page1["structuredContent"];
    assert_eq!(p["total"], 2, "{page1}");
    assert_eq!(p["collections"][0]["name"], "alpha");
    let cursor = p["next_cursor"].as_str().unwrap().to_string();
    let page2 = tool(
        r,
        &tok,
        "list_collections",
        json!({"limit": 1, "cursor": cursor}),
    )
    .await;
    assert_eq!(page2["structuredContent"]["collections"][0]["name"], "beta");
    assert!(page2["structuredContent"]["next_cursor"].is_null());
    let text: Value = serde_json::from_str(page2["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        text, page2["structuredContent"],
        "text and structured agree"
    );
    // A cursor bound to another caller's authority is refused, not reused.
    let other = f.tok(uid, 5, &["mcp", "query"]);
    let res = tool(r, &other, "list_collections", json!({"cursor": cursor})).await;
    assert_eq!(problem_slug(&res), "invalid-input");
    // A cleared caller sees `hidden` too, beyond the first page.
    let cleared = tool(r, &other, "list_collections", json!({"limit": 2})).await;
    assert_eq!(cleared["structuredContent"]["total"], 3);

    // list_runbooks: `secret` (only hidden collections) is not listed, and
    // the projection carries no YAML.
    let rb = tool(r, &tok, "list_runbooks", json!({})).await;
    let names: Vec<&str> = rb["structuredContent"]["runbooks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["expander", "handbook"]);
    assert!(!rb.to_string().contains("apiVersion"));

    // search_collection: native parity, byte-for-byte text.
    let args = json!({"query": "orchid archive", "collection": "alpha", "top_k": 3});
    let mut call = args.clone();
    call["operation_key"] = fresh_key();
    let mcp = tool(r, &tok, "search_collection", call).await;
    assert_eq!(mcp["_meta"][crate::mcp_tools::RESULT_KIND], "native");
    let (status, native) = f
        .native("POST", "/v1.2/search", &tok, uid, Some(args))
        .await;
    assert_eq!(status, 200);
    assert_eq!(
        mcp["structuredContent"],
        serde_json::from_slice::<Value>(&native).unwrap()
    );
    assert_eq!(
        serde_json::from_str::<Value>(mcp["content"][0]["text"].as_str().unwrap()).unwrap(),
        mcp["structuredContent"]
    );
    // A hidden collection is refused exactly as natively (no disclosure).
    let hidden = tool(
        r,
        &tok,
        "search_collection",
        json!({"operation_key": fresh_key(), "query": "orchid", "collection": "hidden"}),
    )
    .await;
    assert_eq!(problem_slug(&hidden), "not-found");
    let (status, native) = f
        .native(
            "POST",
            "/v1.2/search",
            &tok,
            uid,
            Some(json!({"query": "orchid", "collection": "hidden"})),
        )
        .await;
    assert_eq!(status, 404);
    assert_eq!(
        hidden["structuredContent"],
        serde_json::from_slice::<Value>(&native).unwrap()
    );

    // ask_collections: a budgeted synthetic provider; same shape as native.
    let before = f.rec.arrivals.load(Ordering::SeqCst);
    let ask = tool(
        r,
        &tok,
        "ask_collections",
        json!({"operation_key": fresh_key(), "question": "What is described?", "collections": ["alpha"]}),
    )
    .await;
    assert_eq!(ask["isError"], false, "{ask}");
    let (status, native) = f
        .native(
            "POST",
            "/v1.2/query",
            &tok,
            uid,
            Some(json!({"question": "What is described?", "collections": ["alpha"]})),
        )
        .await;
    assert_eq!(status, 200);
    let native: Value = serde_json::from_slice(&native).unwrap();
    let keys = |v: &Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
    assert_eq!(keys(&ask["structuredContent"]), keys(&native));
    let arrivals = f.rec.arrivals.load(Ordering::SeqCst) - before;
    assert!(arrivals >= 1);

    // open_session / ask_session / get_session / close_session.
    let opened = tool(
        r,
        &tok,
        "open_session",
        json!({"operation_key": fresh_key(), "runbook": "handbook"}),
    )
    .await;
    assert_eq!(opened["isError"], false, "{opened}");
    let session = session_of(&opened);
    let turn = tool(
        r,
        &tok,
        "ask_session",
        json!({"operation_key": fresh_key(), "session_id": session, "query": "orchid archive", "complete": true}),
    )
    .await;
    assert_eq!(turn["isError"], false, "{turn}");
    assert_eq!(turn["structuredContent"]["coverage"]["policy"], "require");
    let ordinal = turn["structuredContent"]["ordinal"].as_u64().unwrap();
    let projection = tool(
        r,
        &tok,
        "get_session",
        json!({"session_id": session, "ordinal": ordinal}),
    )
    .await;
    assert_eq!(
        projection["_meta"][crate::mcp_tools::RESULT_KIND],
        "projection"
    );
    let (status, native) = f
        .native("GET", &format!("/v1/sessions/{session}"), &tok, uid, None)
        .await;
    assert_eq!(status, 200);
    let native: Value = serde_json::from_slice(&native).unwrap();
    let native_turn = native["turns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["ordinal"].as_u64() == Some(ordinal))
        .unwrap();
    assert_eq!(&projection["structuredContent"]["turns"][0], native_turn);
    for k in [
        "uid",
        "runbook_ref",
        "access_level",
        "compartments",
        "state",
        "created_at",
    ] {
        assert_eq!(projection["structuredContent"][k], native[k], "{k}");
    }
    let page = tool(
        r,
        &tok,
        "get_session",
        json!({"session_id": session, "limit": 1}),
    )
    .await;
    assert_eq!(
        page["structuredContent"]["turns"].as_array().unwrap().len(),
        1
    );
    let closed = tool(
        r,
        &tok,
        "close_session",
        json!({"operation_key": fresh_key(), "session_id": session}),
    )
    .await;
    assert_eq!(closed["structuredContent"]["state"], "closed");
    let again = tool(
        r,
        &tok,
        "close_session",
        json!({"operation_key": fresh_key(), "session_id": session}),
    )
    .await;
    assert_eq!(
        again["structuredContent"]["state"], "closed",
        "idempotent close"
    );

    // authorize_publication: refusal parity for an unknown publication.
    let pubn = tool(
        r,
        &tok,
        "authorize_publication",
        json!({"collection": "alpha", "publication_id": "pub-none"}),
    )
    .await;
    let (status, native) = f
        .native(
            "GET",
            "/v1.2/collections/alpha/publications/pub-none",
            &tok,
            uid,
            None,
        )
        .await;
    assert_eq!(
        pubn["structuredContent"],
        serde_json::from_slice::<Value>(&native).unwrap()
    );
    assert_eq!(
        pubn["structuredContent"]["status"].as_u64(),
        Some(status as u64)
    );

    // get_evidence / read_evidence_rows: a synthetic sealed CSV.
    let evidence_id = seal(&f).await;
    let manifest = tool(r, &tok, "get_evidence", json!({"evidence_id": evidence_id})).await;
    let (status, native) = f
        .native(
            "GET",
            &format!("/v1/evidence/{evidence_id}"),
            &tok,
            uid,
            None,
        )
        .await;
    assert_eq!(status, 200, "the read-only grant reads natively too");
    assert_eq!(
        manifest["structuredContent"],
        serde_json::from_slice::<Value>(&native).unwrap()
    );
    let rows = tool(
        r,
        &tok,
        "read_evidence_rows",
        json!({"evidence_id": evidence_id, "from": 0, "limit": 1}),
    )
    .await;
    let (_, native) = f
        .native(
            "GET",
            &format!("/v1/evidence/{evidence_id}/rows?from=0&limit=1"),
            &tok,
            uid,
            None,
        )
        .await;
    assert_eq!(
        rows["structuredContent"],
        serde_json::from_slice::<Value>(&native).unwrap()
    );

    // Each admitted call recorded once, on the mcp plane, content-free.
    let rows: Vec<(String, Option<Value>)> = sqlx::query_as(
        "SELECT method, request FROM interactions WHERE tenant_id = $1 AND plane = 'mcp'",
    )
    .bind(&f.tenant)
    .fetch_all(f.a.pg_pool().unwrap())
    .await
    .unwrap_or_default();
    // The writer is asynchronous; when rows have landed they carry no body.
    for (method, request) in rows {
        assert!(method.starts_with("tools/call "));
        let text = request.map(|r| r.to_string()).unwrap_or_default();
        assert!(!text.contains("orchid"), "{text}");
    }
    let (status, report) = f
        .native(
            "GET",
            "/v1/reports/timeseries?window=1h&plane=mcp",
            RW,
            "operator",
            None,
        )
        .await;
    let _ = report;
    assert!(
        status == 200 || status == 403,
        "the mcp plane filter is accepted: {status}"
    );
}

const CSV: &str = "region,amount\nEMEA,2770001.00\nAMER,900000.50\n";

async fn seal(f: &Fixture) -> String {
    use base64::Engine as _;
    use sha2::Digest as _;
    let sha = |b: &[u8]| format!("sha256:{}", hex::encode(sha2::Sha256::digest(b)));
    let manifest = json!({
        "contract_version": munarium_core::evidence::CONTRACT_VERSION.trim(),
        "canon": "canon@1",
        "tenant": f.tenant,
        "kind": "table",
        "logical_result_hash": format!("sha256:{}", "2".repeat(64)),
        "artifact_hash": sha(CSV.as_bytes()),
        "bytes_len": CSV.len(),
        "media_type": "text/csv; charset=utf-8",
        "source": { "source_id": "fiction", "source_version": 1, "adapter": "postgres" },
        "versions": { "policy": "policy@1" },
        "schema": { "columns": [
            {"id": "c1", "name": "region", "type": "string", "nullable": false, "key": true},
            {"id": "c2", "name": "amount", "type": "decimal", "nullable": false, "scale": 2}
        ]},
        "identity": { "row_id_rule": "keys", "rows": 2 },
        "completeness": { "truncated": false },
        "snapshot_vector": [{ "source_id": "fiction", "replay_level": "sealed_result" }],
        "execution": { "started_at": "2026-11-20T10:00:00Z", "ended_at": "2026-11-20T10:00:01Z" },
        "authorization_class": { "access_level": 0, "compartments": [] }
    });
    let body = json!({
        "manifest": manifest,
        "bytes_base64": base64::engine::general_purpose::STANDARD.encode(CSV.as_bytes()),
    });
    let (status, out) = f
        .native("POST", "/v1/evidence", RW, "sealer", Some(body))
        .await;
    let out: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(status, 200, "{out}");
    out["evidence_id"].as_str().unwrap().to_string()
}

/// A4-T1: the read-only grant reads natively, cannot seal, and is refused
/// MCP entry without `mcp`; `mcp` + `query` alone cannot reach the readers.
#[tokio::test]
async fn a4_the_read_only_evidence_grant() {
    let Some(url) = database() else { return };
    let f = fixture(url, enabled()).await;
    let id = seal(&f).await;
    let read_only = f.tok("auditor", 0, &["evidence_read"]);
    let (status, _) = f
        .native(
            "GET",
            &format!("/v1/evidence/{id}"),
            &read_only,
            "auditor",
            None,
        )
        .await;
    assert_eq!(status, 200);
    let (status, _) = f
        .native(
            "GET",
            &format!("/v1/evidence/{id}/rows"),
            &read_only,
            "auditor",
            None,
        )
        .await;
    assert_eq!(status, 200);
    let r = rpc(&f.ra, &read_only, "tools/list", json!({})).await;
    assert_eq!(r.status, 403, "no mcp scope, no entry");
    let seal_attempt = json!({"manifest": {}, "bytes_base64": ""});
    let (status, _) = f
        .native(
            "POST",
            "/v1/evidence",
            &read_only,
            "auditor",
            Some(seal_attempt),
        )
        .await;
    assert_eq!(status, 403, "the read-only grant cannot seal");
    let (status, _) = f
        .native(
            "POST",
            &format!("/v1/evidence/{id}/commit"),
            &read_only,
            "auditor",
            None,
        )
        .await;
    assert_eq!(status, 403, "...or commit");
    // Under-cleared: refused on both planes; cross-tenant: not found.
    let both = f.tok("auditor", 0, &["mcp", "query"]);
    assert_eq!(
        problem_slug(&tool(&f.ra, &both, "get_evidence", json!({"evidence_id": id})).await),
        "scope-missing"
    );
    let elsewhere = token(
        "another-tenant",
        "auditor",
        9,
        &["mcp", "evidence_read"],
        None,
    );
    let res = tool(
        &f.ra,
        &elsewhere,
        "get_evidence",
        json!({"evidence_id": id}),
    )
    .await;
    assert_eq!(res["isError"], true, "{res}");
    // The accesses audit records the MCP reads.
    let readers = f.tok("auditor", 0, &["mcp", "evidence_read"]);
    let ok = tool(
        &f.ra,
        &readers,
        "read_evidence_rows",
        json!({"evidence_id": id}),
    )
    .await;
    assert_eq!(ok["isError"], false, "{ok}");
    // A v1.4 `evidence` token keeps its native read.
    let v14 = f.tok("matrix", 0, &["evidence"]);
    let (status, _) = f
        .native("GET", &format!("/v1/evidence/{id}"), &v14, "matrix", None)
        .await;
    assert_eq!(status, 200);
}

/// Session authority (§5.1) and A1-T1's resource rows: another user, a
/// narrower same-uid token and a runbook outside the allowlist are refused;
/// a renewed token with the same authority continues.
#[tokio::test]
async fn a1_session_authority_and_renewal() {
    let Some(url) = database() else { return };
    let f = fixture(url, enabled()).await;
    let r = &f.ra;
    let wide = f.tok("owner", 3, &["mcp", "query"]);
    let opened = tool(
        r,
        &wide,
        "open_session",
        json!({"operation_key": fresh_key(), "runbook": "handbook"}),
    )
    .await;
    let session = session_of(&opened);
    let read = |tok: String| {
        let session = session.clone();
        async move { tool(r, &tok, "get_session", json!({"session_id": session})).await }
    };
    assert_eq!(
        read(f.tok("owner", 3, &["mcp", "query"])).await["isError"],
        false,
        "renewed"
    );
    assert_eq!(
        problem_slug(&read(f.tok("owner", 0, &["mcp", "query"])).await),
        "forbidden",
        "narrower"
    );
    assert_eq!(
        problem_slug(&read(f.tok("intruder", 9, &["mcp", "query"])).await),
        "forbidden",
        "other uid"
    );
    let restricted = token(
        &f.tenant,
        "owner",
        3,
        &["mcp", "query"],
        Some(&["expander"]),
    );
    assert_eq!(
        problem_slug(&read(restricted.clone()).await),
        "forbidden",
        "runbook allowlist"
    );
    let res = tool(
        r,
        &restricted,
        "open_session",
        json!({"operation_key": fresh_key(), "runbook": "handbook"}),
    )
    .await;
    assert_eq!(
        res["isError"], true,
        "the native runbook check applies: {res}"
    );
    // Turns and closes get the same check, after admission and before any
    // native work: the refused close leaves the session open.
    let res = tool(
        r,
        &f.tok("intruder", 9, &["mcp", "query"]),
        "close_session",
        json!({"operation_key": fresh_key(), "session_id": session}),
    )
    .await;
    assert_eq!(problem_slug(&res), "forbidden");
    let state: String =
        sqlx::query_scalar("SELECT state FROM sessions WHERE tenant_id = $1 AND id = $2")
            .bind(&f.tenant)
            .bind(&session)
            .fetch_one(f.a.pg_pool().unwrap())
            .await
            .unwrap();
    assert_eq!(state, "open");
    // A revoked token stops at entry.
    let revoked = f.tok("owner", 3, &["mcp", "query"]);
    let claims = munarium_access::verify(SECRET, &revoked).unwrap();
    sqlx::query(
        "INSERT INTO access_tokens (tenant_id, jti, uid, access_level, compartments, scopes, issued_by, expires_at, revoked_at)
         VALUES ($1, $2, 'owner', 3, '{}', '{mcp,query}', 'broker', now() + interval '1 hour', now())",
    )
    .bind(&f.tenant)
    .bind(&claims.jti)
    .execute(f.a.pg_pool().unwrap())
    .await
    .unwrap();
    let r2 = rpc(r, &revoked, "tools/list", json!({})).await;
    assert_eq!(r2.status, 401);
    assert!(r2.body["type"]
        .as_str()
        .unwrap()
        .ends_with("/token-revoked"));
}

/// A6-T1 / A6-T2: a confirmed record precedes every handler; a failure
/// before commit, an expired deadline and a committed-but-unacknowledged
/// write each refuse with zero reads, mutations and provider attempts.
#[tokio::test]
async fn a6_durable_admission_under_faults() {
    let Some(url) = database() else { return };
    let mut s = paid();
    s.audit_deadline_ms = 1_500;
    let f = fixture(url, McpSettings { ..s.clone() }).await;
    let mut s2 = s.clone();
    s2.generation = policy(&f.tenant, Some("library"), Some(100_000), Some(1_000_000));
    let (a, r) = replica(&f, s2, "mcp-a6").await;
    let uid = "a6-user";
    let tok = f.reader(uid);
    let arrivals = f.rec.arrivals.load(Ordering::SeqCst);

    // Normal: the record exists before the handler (dispatched_at after
    // admitted_at), and the dispatch is counted once.
    let opened = tool(
        &r,
        &tok,
        "open_session",
        json!({"operation_key": fresh_key(), "runbook": "handbook"}),
    )
    .await;
    let session = session_of(&opened);
    let (admitted_before, outcome): (bool, Option<String>) = sqlx::query_as(
        "SELECT admitted_at <= dispatched_at, outcome FROM mcp_invocations
          WHERE tenant_id = $1 AND tool = 'open_session'",
    )
    .bind(&f.tenant)
    .fetch_one(a.pg_pool().unwrap())
    .await
    .unwrap();
    assert!(admitted_before);
    assert_eq!(outcome.as_deref(), Some("completed"));
    assert_eq!(
        counter(&a, "munarium_mcp_dispatches_total", "open_session"),
        1
    );

    for (fault, label) in [
        ("fail", "store-error"),
        ("delay", "deadline"),
        ("lost", "lost-ack"),
    ] {
        a.mcp
            .faults
            .fail_before_commit
            .store(fault == "fail", Ordering::SeqCst);
        a.mcp
            .faults
            .delay_ms
            .store(if fault == "delay" { 3_000 } else { 0 }, Ordering::SeqCst);
        a.mcp
            .faults
            .lose_ack
            .store(fault == "lost", Ordering::SeqCst);
        let sessions = f.sessions_of(uid).await;
        let dispatches = counter(&a, "munarium_mcp_dispatches_total", "");
        let k = fresh_key();
        for (name, args) in [
            ("list_collections", json!({})),
            ("list_runbooks", json!({})),
            (
                "open_session",
                json!({"operation_key": k.clone(), "runbook": "handbook"}),
            ),
            (
                "ask_session",
                json!({"operation_key": fresh_key(), "session_id": session, "query": "orchid", "complete": true}),
            ),
            (
                "close_session",
                json!({"operation_key": fresh_key(), "session_id": session}),
            ),
            ("get_session", json!({"session_id": session})),
            (
                "search_collection",
                json!({"operation_key": fresh_key(), "query": "orchid", "collection": "alpha"}),
            ),
        ] {
            let res = tool(&r, &tok, name, args).await;
            assert_eq!(
                problem_slug(&res),
                "mcp-audit-unavailable",
                "{fault} {name}"
            );
            assert_eq!(res["structuredContent"]["executed"], false);
            assert!(!res.to_string().contains("injected"), "content-free");
        }
        assert_eq!(
            f.sessions_of(uid).await,
            sessions,
            "{fault}: zero mutations"
        );
        assert_eq!(
            counter(&a, "munarium_mcp_dispatches_total", ""),
            dispatches,
            "{fault}: zero dispatches"
        );
        assert_eq!(
            f.rec.arrivals.load(Ordering::SeqCst),
            arrivals,
            "{fault}: zero provider attempts"
        );
        assert!(
            counter(&a, "munarium_mcp_audit_refusals_total", label) >= 7,
            "{fault}"
        );
        a.mcp
            .faults
            .fail_before_commit
            .store(false, Ordering::SeqCst);
        a.mcp.faults.delay_ms.store(0, Ordering::SeqCst);
        a.mcp.faults.lose_ack.store(false, Ordering::SeqCst);
        if fault == "lost" {
            // The committed claim is marked withheld; a recovery read on a
            // second replica says it never ran, and dispatches nothing.
            let (b, rb) = replica(&f, enabled(), "mcp-a6-b").await;
            let mut status = String::new();
            for _ in 0..50 {
                let rcpt = tool(
                    &rb,
                    &tok,
                    "open_session",
                    json!({"operation_key": k.clone()}),
                )
                .await;
                status = rcpt["structuredContent"]["status"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                if status == "not_executed" {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            assert_eq!(status, "not_executed");
            assert_eq!(counter(&b, "munarium_mcp_dispatches_total", ""), 0);
        }
    }
    // Discovery and listing are protocol operations, unaffected by faults.
    a.mcp
        .faults
        .fail_before_commit
        .store(true, Ordering::SeqCst);
    assert_eq!(rpc(&r, &tok, "tools/list", json!({})).await.status, 200);
    a.mcp
        .faults
        .fail_before_commit
        .store(false, Ordering::SeqCst);
}

/// A2-T1: with a recording provider, generation is refused with zero
/// physical calls unless paid tools are on AND the scope names an owner and
/// both allowances; a zero allowance admits nothing; expansion on a
/// `complete: false` turn is held to the same rule.
#[tokio::test]
async fn a2_generation_opt_in_counts_physical_arrivals() {
    let Some(url) = database() else { return };
    let f = fixture(url, enabled()).await;
    let uid = "a2-user";
    let tok = f.reader(uid);
    let opened = tool(
        &f.ra,
        &tok,
        "open_session",
        json!({"operation_key": fresh_key(), "runbook": "handbook"}),
    )
    .await;
    let session = session_of(&opened);
    let exp = tool(
        &f.ra,
        &tok,
        "open_session",
        json!({"operation_key": fresh_key(), "runbook": "expander"}),
    )
    .await;
    let exp_session = session_of(&exp);
    let cases: Vec<(&str, McpSettings)> = vec![
        ("paid tools disabled", enabled()),
        ("no policy", paid()),
        (
            "no owner",
            McpSettings {
                generation: policy(&f.tenant, None, Some(9_999), Some(99_999)),
                ..paid()
            },
        ),
        (
            "no request allowance",
            McpSettings {
                generation: policy(&f.tenant, Some("o"), None, Some(99_999)),
                ..paid()
            },
        ),
        (
            "no daily allowance",
            McpSettings {
                generation: policy(&f.tenant, Some("o"), Some(9_999), None),
                ..paid()
            },
        ),
        (
            "zero request",
            McpSettings {
                generation: policy(&f.tenant, Some("o"), Some(0), Some(99_999)),
                ..paid()
            },
        ),
        (
            "zero daily",
            McpSettings {
                generation: policy(&f.tenant, Some("o"), Some(9_999), Some(0)),
                ..paid()
            },
        ),
    ];
    for (case, settings) in cases {
        let (_, r) = replica(&f, settings, "mcp-a2").await;
        let before = f.rec.arrivals.load(Ordering::SeqCst);
        let res = tool(&r, &tok, "ask_session", json!({"operation_key": fresh_key(), "session_id": session, "query": "orchid", "complete": true})).await;
        assert_eq!(res["isError"], true, "{case}: {res}");
        let slug = problem_slug(&res);
        assert!(
            [
                "mcp-tool-unavailable",
                "mcp-generation-not-authorized",
                "mcp-allowance-exhausted"
            ]
            .contains(&slug.as_str()),
            "{case}: {slug}"
        );
        // Expansion on a complete:false turn: never reaches the provider.
        let _ = tool(&r, &tok, "ask_session", json!({"operation_key": fresh_key(), "session_id": exp_session, "query": "orchid", "complete": false})).await;
        let _ = tool(
            &r,
            &tok,
            "ask_collections",
            json!({"operation_key": fresh_key(), "question": "q?", "collections": ["alpha"]}),
        )
        .await;
        assert_eq!(
            f.rec.arrivals.load(Ordering::SeqCst),
            before,
            "{case}: zero physical calls"
        );
    }
    // Authorized generation for a model without pricing data.
    let (_, r) = replica(
        &f,
        McpSettings {
            generation: policy(&f.tenant, Some("o"), Some(50_000), Some(500_000)),
            ..paid()
        },
        "mcp-a2-ok",
    )
    .await;
    let before = f.rec.arrivals.load(Ordering::SeqCst);
    let res = tool(&r, &tok, "ask_session", json!({"operation_key": fresh_key(), "session_id": session, "query": "orchid", "complete": true})).await;
    assert_eq!(res["isError"], false, "{res}");
    let arrivals = f.rec.arrivals.load(Ordering::SeqCst) - before;
    assert_eq!(
        arrivals as i64,
        f.attempts().await,
        "every arrival was an admitted attempt"
    );
    // Native traffic is untouched by the MCP policy (no MCP rows added).
    let attempts = f.attempts().await;
    let (status, _) = f
        .native(
            "POST",
            &format!("/v1/sessions/{session}/turns"),
            &f.tok(uid, 0, &["query"]),
            uid,
            Some(json!({"query": "orchid", "complete": true})),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(f.attempts().await, attempts);
}

fn ctx(
    tenant: &str,
    invocation: &str,
    policy: crate::mcp_generation::SharedPolicy,
) -> crate::mcp_generation::CallContext {
    crate::mcp_generation::CallContext {
        tenant: tenant.into(),
        invocation_id: invocation.into(),
        paid_enabled: true,
        policy: Ok(policy),
        admitted: Default::default(),
        settlement_failed: Default::default(),
    }
}

fn shared(request: u64, daily: &[(&str, &str, u64)]) -> crate::mcp_generation::SharedPolicy {
    crate::mcp_generation::SharedPolicy {
        payer: "shared".into(),
        request_ceiling: request,
        daily: daily
            .iter()
            .map(|(k, id, v)| ((k.to_string(), id.to_string()), *v))
            .collect(),
    }
}

async fn day_used(pool: &sqlx::PgPool, tenant: &str, kind: &str, id: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COALESCE(sum(COALESCE(a.settled_units, a.bound_units)), 0)::bigint
           FROM mcp_token_attempt_scopes s JOIN mcp_token_attempts a USING (attempt_id)
          WHERE s.tenant_id = $1 AND s.scope_kind = $2 AND s.scope_id = $3",
    )
    .bind(tenant)
    .bind(kind)
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// A8-T1/A8-T2/A9-T1/A9-T2 on the accounting engine directly: the shared
/// request ceiling across stages and retries, every daily scope reserved
/// together, no partial reservation on refusal, two replicas racing for the
/// last capacity, settlement once, uncertain usage held, bound violations.
#[tokio::test]
async fn a8_a9_allowances_under_retries_and_concurrency() {
    use crate::mcp_generation::{admit_attempt, settle};
    let Some(url) = database() else { return };
    let f = fixture(url, enabled()).await;
    let pool = f.a.pg_pool().unwrap().clone();
    let t = &f.tenant;
    // The governing example: Legal (2,000/request) and Finance (8,000).
    let both = shared(
        8_000,
        &[
            ("collection", "legal", 50_000),
            ("collection", "finance", 50_000),
        ],
    );
    let c = ctx(t, "inv-shared", both.clone());
    let a1 = admit_attempt(Some(&pool), &c, "fixture", "m", 5_000)
        .await
        .unwrap();
    settle(Some(&pool), &a1, Some(5_000)).await.unwrap();
    let a2 = admit_attempt(Some(&pool), &c, "fixture", "m", 2_000)
        .await
        .unwrap();
    settle(Some(&pool), &a2, Some(2_000)).await.unwrap();
    let a3 = admit_attempt(Some(&pool), &c, "fixture", "m", 1_000)
        .await
        .unwrap(); // a retry
    settle(Some(&pool), &a3, Some(1_000)).await.unwrap();
    let refused = admit_attempt(Some(&pool), &c, "fixture", "m", 1)
        .await
        .unwrap_err();
    assert!(refused
        .to_string()
        .contains(crate::mcp_generation::ALLOWANCE_PREFIX));
    assert_eq!(day_used(&pool, t, "collection", "legal").await, 8_000);
    assert_eq!(day_used(&pool, t, "collection", "finance").await, 8_000);
    let payer: i64 = sqlx::query_scalar(
        "SELECT COALESCE(sum(COALESCE(settled_units, bound_units)), 0)::bigint FROM mcp_token_attempts WHERE tenant_id = $1 AND payer = 'shared'",
    )
    .bind(t)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(payer, 8_000, "the common payer is charged once per attempt");
    // A duplicate settlement receipt changes nothing.
    assert_eq!(
        settle(Some(&pool), &a3, Some(999)).await.unwrap(),
        (false, 0)
    );
    // Legal alone keeps its 2,000 ceiling.
    let legal = ctx(
        t,
        "inv-legal",
        shared(2_000, &[("collection", "legal", 50_000)]),
    );
    assert!(admit_attempt(Some(&pool), &legal, "fixture", "m", 2_001)
        .await
        .is_err());

    // A9-T2: one contributor's daily balance below the bound refuses the
    // whole attempt with no partial debit.
    let low = ctx(
        t,
        "inv-low",
        shared(
            10_000,
            &[
                ("collection", "roomy", 50_000),
                ("collection", "tight", 100),
            ],
        ),
    );
    assert!(admit_attempt(Some(&pool), &low, "fixture", "m", 500)
        .await
        .is_err());
    assert_eq!(
        day_used(&pool, t, "collection", "roomy").await,
        0,
        "no partial reservation"
    );
    // Unknown contributor / no common payer refuse before any reservation.
    let unknown = crate::mcp_generation::CallContext {
        policy: Err("unknown contributor".into()),
        ..ctx(t, "inv-unknown", shared(1, &[]))
    };
    assert!(admit_attempt(Some(&pool), &unknown, "fixture", "m", 1)
        .await
        .unwrap_err()
        .to_string()
        .contains(crate::mcp_generation::NOT_AUTHORIZED_PREFIX));

    // Two replicas race for the last daily capacity: exactly one wins.
    let pool_b = sqlx::PgPool::connect(&f.url).await.unwrap();
    let race = shared(10_000, &[("collection", "race", 1_000)]);
    let ca = ctx(t, "inv-race-a", race.clone());
    let cb = ctx(t, "inv-race-b", race);
    let (ra, rb) = tokio::join!(
        admit_attempt(Some(&pool), &ca, "fixture", "m", 600),
        admit_attempt(Some(&pool_b), &cb, "fixture", "m", 600),
    );
    assert_eq!(ra.is_ok() as u8 + rb.is_ok() as u8, 1, "{ra:?} {rb:?}");
    assert_eq!(day_used(&pool, t, "collection", "race").await, 600);

    // A8-T2: uncertain usage keeps the full reservation against its day.
    let unsure = ctx(
        t,
        "inv-unsure",
        shared(10_000, &[("collection", "unsure", 10_000)]),
    );
    let u = admit_attempt(Some(&pool), &unsure, "fixture", "m", 700)
        .await
        .unwrap();
    settle(Some(&pool), &u, None).await.unwrap();
    assert_eq!(day_used(&pool, t, "collection", "unsure").await, 700);
    let (state, day_ok): (String, bool) = sqlx::query_as(
        "SELECT state, day = (now() AT TIME ZONE 'UTC')::date FROM mcp_token_attempts WHERE attempt_id = $1",
    )
    .bind(&u.attempt_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((state.as_str(), day_ok), ("uncertain", true));
    // A late reliable receipt settles once; a repeat does nothing.
    assert_eq!(settle(Some(&pool), &u, Some(650)).await.unwrap(), (true, 0));
    assert_eq!(
        settle(Some(&pool), &u, Some(650)).await.unwrap(),
        (false, 0)
    );
    // Overage is charged and recorded atomically; it doesn't suspend the model.
    let over = ctx(
        t,
        "inv-over",
        shared(5_010, &[("collection", "over", 100_000)]),
    );
    let o = admit_attempt(Some(&pool), &over, "fixture", "over-model", 100)
        .await
        .unwrap();
    assert_eq!(
        settle(Some(&pool), &o, Some(5_000)).await.unwrap(),
        (true, 4_900)
    );
    let observed: i64 =
        sqlx::query_scalar("SELECT observed_units FROM mcp_bound_violations WHERE attempt_id = $1")
            .bind(&o.attempt_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(observed, 5_000);
    assert_eq!(day_used(&pool, t, "collection", "over").await, 5_000);
    let last = admit_attempt(Some(&pool), &over, "fixture", "over-model", 10)
        .await
        .unwrap();
    assert_eq!(
        settle(Some(&pool), &last, Some(30)).await.unwrap(),
        (true, 20)
    );
    let next = admit_attempt(Some(&pool), &over, "fixture", "over-model", 1)
        .await
        .unwrap_err();
    assert!(
        next.to_string()
            .contains(crate::mcp_generation::ALLOWANCE_PREFIX),
        "{next}"
    );
    // No durable store: accounting is unavailable and nothing is sent.
    let none = admit_attempt(None, &over, "fixture", "m", 1)
        .await
        .unwrap_err();
    assert!(none
        .to_string()
        .contains(crate::mcp_generation::ACCOUNTING_PREFIX));
}

/// A8-T2: an MCP model call whose provider answers with an error, or whose
/// transport fails, leaves no attempt `reserved`: each physical attempt is
/// settled `uncertain` at its full bound (it may have run), charged once.
#[tokio::test]
async fn a8_failed_model_calls_settle_uncertain_at_the_full_bound() {
    let Some(url) = database() else { return };
    let f = fixture(url, enabled()).await;
    let (state, _) = replica(
        &f,
        McpSettings {
            generation: policy(&f.tenant, Some("o"), Some(50_000), Some(500_000)),
            ..paid()
        },
        "mcp-accounting",
    )
    .await;
    let pool = f.a.pg_pool().unwrap().clone();
    let t = &f.tenant;
    let arrivals = Arc::new(AtomicUsize::new(0));
    let seen = arrivals.clone();
    let app = axum::Router::new().route(
        "/api/chat",
        axum::routing::post(move || {
            let seen = seen.clone();
            async move {
                seen.fetch_add(1, Ordering::SeqCst);
                axum::http::StatusCode::BAD_REQUEST
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let refusing = format!("http://{}", listener.local_addr().unwrap());
    let _server = Abort(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap()
    }));
    // A port with nothing listening: the transport fails.
    let closed = {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    apply_named(&state, t, "refusing", &refusing).await;
    apply_named(&state, t, "unreachable", &closed).await;
    let store = state.store_for(t).await.unwrap();
    for (config, invocation) in [("refusing", "inv-error"), ("unreachable", "inv-transport")] {
        let c = Arc::new(ctx(
            t,
            invocation,
            shared(100_000, &[("collection", invocation, 100_000)]),
        ));
        let result = crate::mcp_generation::scope(
            c,
            crate::providers_api::op_complete(
                &state,
                t,
                store.as_ref(),
                config,
                munarium_api_types::CompleteRequest {
                    model: None,
                    provider: None,
                    tier: Some("fast".into()),
                    system: None,
                    prompt: Some("orchid?".into()),
                    max_tokens: Some(16),
                    temperature: None,
                    version_id: None,
                },
            ),
        )
        .await;
        assert!(result.is_err(), "{config}: the provider call failed");
        let rows: Vec<(String, Option<i64>, i64)> = sqlx::query_as(
            "SELECT state, settled_units, bound_units FROM mcp_token_attempts
              WHERE tenant_id = $1 AND invocation_id = $2",
        )
        .bind(t)
        .bind(invocation)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 1, "{config}: one physical attempt");
        let (state, settled, bound) = &rows[0];
        assert_eq!(state, "uncertain", "{config}: never left reserved");
        assert_eq!(*settled, None, "{config}: the full bound stays charged");
        assert_eq!(
            day_used(&pool, t, "collection", invocation).await,
            *bound,
            "{config}: charged exactly once, at the bound"
        );
        let spend = crate::mcp_store::spend(&f.a, t, invocation).await.unwrap();
        assert_eq!(spend["attempts"], 1, "{config}");
        assert_eq!(spend["outstanding_reserved_tokens"], *bound, "{config}");
    }
    assert_eq!(
        arrivals.load(Ordering::SeqCst),
        1,
        "the error reached the stub once"
    );

    let overage = recorder(500, 50).await;
    apply_named(&state, t, "overage", &overage.endpoint).await;
    let invocation = "inv-metered-overage";
    let c = Arc::new(ctx(
        t,
        invocation,
        shared(50_000, &[("collection", invocation, 50_000)]),
    ));
    crate::mcp_generation::scope(
        c,
        crate::providers_api::op_complete(
            &state,
            t,
            store.as_ref(),
            "overage",
            munarium_api_types::CompleteRequest {
                tier: Some("fast".into()),
                prompt: Some("orchid?".into()),
                max_tokens: Some(16),
                ..Default::default()
            },
        ),
    )
    .await
    .unwrap();
    let (reserved, settled): (i64, i64) = sqlx::query_as(
        "SELECT bound_units, settled_units FROM mcp_token_attempts
          WHERE tenant_id = $1 AND invocation_id = $2",
    )
    .bind(t)
    .bind(invocation)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(settled, 550);
    assert!(settled > reserved);
    assert_eq!(
        counter(&state, "munarium_mcp_generation_excess_tokens_total", ""),
        (settled - reserved) as u64
    );
}

/// A8-T1 end to end: a request allowance that covers the expansion stage but
/// not the answer stage refuses the answer with zero further arrivals; a
/// daily allowance is exhausted across requests while request capacity
/// remains; provider arrivals always equal admitted attempts.
#[tokio::test]
async fn a8_request_and_daily_limits_through_the_endpoint() {
    let Some(url) = database() else { return };
    let f = fixture(url, enabled()).await;
    let uid = "a8-user";
    let tok = f.reader(uid);
    let opened = tool(
        &f.ra,
        &tok,
        "open_session",
        json!({"operation_key": fresh_key(), "runbook": "expander"}),
    )
    .await;
    let session = session_of(&opened);
    let turn = || json!({"operation_key": fresh_key(), "session_id": session, "query": "orchid", "complete": true});
    let arrivals = || f.rec.arrivals.load(Ordering::SeqCst);
    // The recorder reports 30 + 10 tokens per call, so each settled attempt
    // uses 40; a reservation is checked at its full bound before dispatch.
    const SETTLED: u64 = 40;
    let (_, r) = replica(
        &f,
        McpSettings {
            generation: policy(&f.tenant, Some("o"), Some(1_000_000), Some(10_000_000)),
            ..paid()
        },
        "mcp-a8-m",
    )
    .await;
    let res = tool(&r, &tok, "ask_session", turn()).await;
    assert_eq!(res["isError"], false, "{res}");
    let rows: Vec<(i64, Option<i64>)> = sqlx::query_as(
        "SELECT bound_units, settled_units FROM mcp_token_attempts WHERE tenant_id = $1 ORDER BY created_at",
    )
    .bind(&f.tenant)
    .fetch_all(f.a.pg_pool().unwrap())
    .await
    .unwrap();
    assert_eq!(rows.len(), 2, "expansion + answer: {rows:?}");
    assert!(
        rows.iter().all(|(_, s)| *s == Some(SETTLED as i64)),
        "settled once from reported usage: {rows:?}"
    );
    let (expansion, answer) = (rows[0].0 as u64, rows[1].0 as u64);
    assert!(
        expansion <= SETTLED + answer,
        "fixture precondition: {expansion} vs {answer}"
    );

    // Request allowance: room for the expansion only. The expansion runs,
    // the answer is refused before dispatch: one arrival, one attempt.
    let (_, r) = replica(
        &f,
        McpSettings {
            generation: policy(&f.tenant, Some("o"), Some(expansion + 1), Some(10_000_000)),
            ..paid()
        },
        "mcp-a8-r",
    )
    .await;
    let (a0, t0) = (arrivals(), f.attempts().await);
    let res = tool(&r, &tok, "ask_session", turn()).await;
    assert_eq!(problem_slug(&res), "mcp-allowance-exhausted", "{res}");
    assert_eq!((arrivals() - a0, f.attempts().await - t0), (1, 1));

    // Daily allowance with request capacity to spare: room for one more
    // request (its settled expansion, then its answer at full bound). The
    // next request's answer cannot fit: refused, with at most its optional
    // expansion admitted first, and every arrival an admitted attempt.
    let so_far = day_used(f.a.pg_pool().unwrap(), &f.tenant, "collection", &f.alpha).await as u64;
    let (_, r) = replica(
        &f,
        McpSettings {
            generation: policy(
                &f.tenant,
                Some("o"),
                Some(1_000_000),
                Some(so_far + SETTLED + answer),
            ),
            ..paid()
        },
        "mcp-a8-d",
    )
    .await;
    let first = tool(&r, &tok, "ask_session", turn()).await;
    assert_eq!(first["isError"], false, "{first}");
    let (a1, t1) = (arrivals(), f.attempts().await);
    let second = tool(&r, &tok, "ask_session", turn()).await;
    assert_eq!(problem_slug(&second), "mcp-allowance-exhausted", "{second}");
    assert!(
        arrivals() - a1 <= 1,
        "the answer never reached the provider"
    );
    assert_eq!((arrivals() - a1) as i64, f.attempts().await - t1);
    assert_eq!(
        arrivals() as i64,
        f.attempts().await,
        "arrivals == admitted attempts"
    );
    let _ = f.beta.clone();
}

/// A5-T5: recovery after a lost reply, across two `AppState`s over one
/// database; duplicates and races; conflicts; isolation; recovery-only mode;
/// expiry and pruning; renewal and narrowed authority.
#[tokio::test]
async fn a5_t5_recovery_across_replicas() {
    let Some(url) = database() else { return };
    let f = fixture(url, paid()).await;
    let (b, rb) = replica(&f, paid(), "mcp-b").await;
    let uid = "t5-user";
    let tok = f.reader(uid);

    // open_session on A; the reply is "lost"; recover on B with the key.
    let k = fresh_key();
    let opened = tool(
        &f.ra,
        &tok,
        "open_session",
        json!({"operation_key": k.clone(), "runbook": "handbook"}),
    )
    .await;
    let session = session_of(&opened);
    let renewed = f.reader(uid); // a new jti, same authority
    let rcpt = tool(
        &rb,
        &renewed,
        "open_session",
        json!({"operation_key": k.clone()}),
    )
    .await;
    assert_eq!(
        rcpt["_meta"][crate::mcp_tools::RESULT_KIND],
        "receipt",
        "{rcpt}"
    );
    let s = &rcpt["structuredContent"];
    assert_eq!(
        (s["kind"].as_str(), s["status"].as_str()),
        (Some("operation_receipt"), Some("completed"))
    );
    assert_eq!(s["outcome"]["session_id"], json!(session));
    assert!(
        !rcpt.to_string().contains("permitted_collections"),
        "no body in a receipt"
    );
    // A duplicate execution on B returns the receipt, not a second session.
    let sessions = f.sessions_of(uid).await;
    let dup = tool(
        &rb,
        &tok,
        "open_session",
        json!({"operation_key": k.clone(), "runbook": "handbook"}),
    )
    .await;
    assert_eq!(dup["structuredContent"]["status"], "completed");
    assert_eq!(f.sessions_of(uid).await, sessions);
    assert_eq!(
        counter(&b, "munarium_mcp_dispatches_total", ""),
        0,
        "zero further dispatches"
    );
    // Changed arguments: a typed conflict.
    let conflict = tool(
        &rb,
        &tok,
        "open_session",
        json!({"operation_key": k.clone(), "runbook": "expander"}),
    )
    .await;
    assert_eq!(problem_slug(&conflict), "mcp-operation-conflict");
    // Another uid, the same key: indistinguishable from absent.
    let stranger = tool(
        &rb,
        &f.reader("t5-other"),
        "open_session",
        json!({"operation_key": k.clone()}),
    )
    .await;
    assert_eq!(stranger["structuredContent"]["status"], "not_found");
    let absent = tool(
        &rb,
        &tok,
        "open_session",
        json!({"operation_key": fresh_key()}),
    )
    .await;
    assert_eq!(
        absent["structuredContent"],
        json!({"kind": "operation_receipt", "tool": "open_session", "operation_key": absent["structuredContent"]["operation_key"], "status": "not_found"})
    );
    // Narrowed authority: the runbook leaves the allowlist; same response.
    let narrowed = token(&f.tenant, uid, 0, &["mcp", "query"], Some(&["expander"]));
    let hidden = tool(
        &rb,
        &narrowed,
        "open_session",
        json!({"operation_key": k.clone()}),
    )
    .await;
    assert_eq!(hidden["structuredContent"]["status"], "not_found");
    // Each lookup was admitted separately and linked to the original.
    let linked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM mcp_invocations i JOIN mcp_operations o
            ON o.invocation_id = i.original_invocation_id
         WHERE i.tenant_id = $1 AND o.key_id = $2",
    )
    .bind(&f.tenant)
    .bind(k["id"].as_str().unwrap())
    .fetch_one(f.a.pg_pool().unwrap())
    .await
    .unwrap();
    assert!(linked >= 3, "{linked}");

    // Race identical arguments under one key: one session. Under distinct
    // keys: two sessions.
    let before = f.sessions_of(uid).await;
    let same = fresh_key();
    let (x, y) = tokio::join!(
        tool(
            &f.ra,
            &tok,
            "open_session",
            json!({"operation_key": same.clone(), "runbook": "handbook"})
        ),
        tool(
            &rb,
            &tok,
            "open_session",
            json!({"operation_key": same.clone(), "runbook": "handbook"})
        ),
    );
    assert_eq!(f.sessions_of(uid).await, before + 1, "{x} {y}");
    let (x, y) = tokio::join!(
        tool(
            &f.ra,
            &tok,
            "open_session",
            json!({"operation_key": fresh_key(), "runbook": "handbook"})
        ),
        tool(
            &rb,
            &tok,
            "open_session",
            json!({"operation_key": fresh_key(), "runbook": "handbook"})
        ),
    );
    assert_eq!(f.sessions_of(uid).await, before + 3, "{x} {y}");

    // ask_session: recover the turn's ordinal, then read it by ordinal.
    let ka = fresh_key();
    let turn = tool(&f.ra, &tok, "ask_session", json!({"operation_key": ka.clone(), "session_id": session, "query": "orchid", "complete": false})).await;
    assert_eq!(turn["isError"], false, "{turn}");
    let rc = tool(
        &rb,
        &tok,
        "ask_session",
        json!({"operation_key": ka.clone()}),
    )
    .await;
    let ordinal = rc["structuredContent"]["outcome"]["turn_ordinal"]
        .as_u64()
        .unwrap();
    assert_eq!(Some(ordinal), turn["structuredContent"]["ordinal"].as_u64());
    assert_eq!(
        rc["structuredContent"]["spend"]["attempts"], 0,
        "zero provider work: {rc}"
    );
    let read = tool(
        &rb,
        &tok,
        "get_session",
        json!({"session_id": session, "ordinal": ordinal}),
    )
    .await;
    assert_eq!(
        read["structuredContent"]["turns"][0]["ordinal"].as_u64(),
        Some(ordinal)
    );

    // search_collection: a local-only search records zero inference work.
    let ks = fresh_key();
    tool(
        &f.ra,
        &tok,
        "search_collection",
        json!({"operation_key": ks.clone(), "query": "orchid", "collection": "alpha"}),
    )
    .await;
    let rs = tool(
        &rb,
        &tok,
        "search_collection",
        json!({"operation_key": ks.clone()}),
    )
    .await;
    assert_eq!(rs["structuredContent"]["status"], "completed");
    assert_eq!(
        rs["structuredContent"]["inference"]["generation"]["attempts"],
        0
    );

    // close_session: idempotent, its recovery returns the state.
    let kc = fresh_key();
    tool(
        &f.ra,
        &tok,
        "close_session",
        json!({"operation_key": kc.clone(), "session_id": session}),
    )
    .await;
    let rcl = tool(
        &rb,
        &tok,
        "close_session",
        json!({"operation_key": kc.clone()}),
    )
    .await;
    assert_eq!(
        rcl["structuredContent"]["outcome"]["session_id"],
        json!(session)
    );

    // ask_collections with spend: recovery creates zero new reservations and
    // reports settled usage separately from outstanding reservations.
    let (_, rpaid) = replica(
        &f,
        McpSettings {
            generation: policy(&f.tenant, Some("o"), Some(100_000), Some(1_000_000)),
            ..paid()
        },
        "mcp-b-paid",
    )
    .await;
    let kq = fresh_key();
    let asked = tool(&rpaid, &tok, "ask_collections", json!({"operation_key": kq.clone(), "question": "What is described?", "collections": ["alpha"]})).await;
    assert_eq!(asked["isError"], false, "{asked}");
    let attempts = f.attempts().await;
    // Disable new work and exhaust the budget: recovery still answers.
    let mut recovery_only = paid();
    recovery_only.execution = Execution::RecoveryOnly;
    recovery_only.generation = policy(&f.tenant, Some("o"), Some(0), Some(0));
    let (_, rro) = replica(&f, recovery_only, "mcp-b-ro").await;
    let listed = rpc(&rro, &tok, "tools/list", json!({})).await;
    let ask_tool = listed.body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "ask_collections")
        .unwrap()
        .clone();
    assert_eq!(
        ask_tool["inputSchema"]["required"],
        json!(["operation_key"])
    );
    let rq = tool(
        &rro,
        &tok,
        "ask_collections",
        json!({"operation_key": kq.clone()}),
    )
    .await;
    assert_eq!(rq["structuredContent"]["status"], "completed", "{rq}");
    let spend = &rq["structuredContent"]["spend"];
    assert!(spend["attempts"].as_i64().unwrap() >= 1);
    assert_eq!(spend["outstanding_reserved_tokens"], 0);
    assert!(spend["settled_tokens"].as_i64().unwrap() > 0);
    assert_eq!(f.attempts().await, attempts, "recovery made no reservation");
    let blocked = tool(
        &rro,
        &tok,
        "ask_collections",
        json!({"operation_key": fresh_key(), "question": "q?", "collections": ["alpha"]}),
    )
    .await;
    assert_eq!(problem_slug(&blocked), "mcp-tool-unavailable");

    // Expiry: a key already expired, beyond the window, or too short for the
    // work deadline cannot admit work; nothing runs.
    let sessions = f.sessions_of(uid).await;
    for (k, why) in [
        (
            key(&format!("k{}", uuid::Uuid::new_v4().simple()), -10),
            "expired",
        ),
        (
            key(&format!("k{}", uuid::Uuid::new_v4().simple()), 30 * 86_400),
            "window",
        ),
        (
            key(&format!("k{}", uuid::Uuid::new_v4().simple()), 60),
            "deadline",
        ),
    ] {
        let res = tool(
            &rb,
            &tok,
            "open_session",
            json!({"operation_key": k, "runbook": "handbook"}),
        )
        .await;
        assert_eq!(
            problem_slug(&res),
            "mcp-operation-key-invalid",
            "{why}: {res}"
        );
    }
    assert_eq!(f.sessions_of(uid).await, sessions);
    // A claimed key past its expiry is pruned; a late execution with it is
    // refused and its lookup finds nothing: zero new execution.
    let kp = fresh_key();
    tool(
        &f.ra,
        &tok,
        "open_session",
        json!({"operation_key": kp.clone(), "runbook": "handbook"}),
    )
    .await;
    sqlx::query("UPDATE mcp_operations SET expires_at = now() - interval '2 days' WHERE tenant_id = $1 AND key_id = $2")
        .bind(&f.tenant)
        .bind(kp["id"].as_str().unwrap())
        .execute(f.a.pg_pool().unwrap())
        .await
        .unwrap();
    assert!(
        crate::mcp_store::prune_keys(f.a.pg_pool().unwrap(), 86_400)
            .await
            .unwrap()
            >= 1
    );
    let sessions = f.sessions_of(uid).await;
    let mut late = kp.clone();
    late["expires_at"] = json!((chrono::Utc::now() - chrono::Duration::days(2))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    let res = tool(
        &rb,
        &tok,
        "open_session",
        json!({"operation_key": late.clone(), "runbook": "handbook"}),
    )
    .await;
    assert_eq!(problem_slug(&res), "mcp-operation-key-invalid");
    let gone = tool(&rb, &tok, "open_session", json!({"operation_key": late})).await;
    assert_eq!(gone["structuredContent"]["status"], "not_found");
    assert_eq!(f.sessions_of(uid).await, sessions);
    // The same pruned key id with a NEW, valid expiry: the tombstone keeps
    // it spent, so it is refused and nothing runs a second time.
    let reopened = key(kp["id"].as_str().unwrap(), 3600);
    let res = tool(
        &rb,
        &tok,
        "open_session",
        json!({"operation_key": reopened, "runbook": "handbook"}),
    )
    .await;
    assert_eq!(problem_slug(&res), "mcp-operation-key-invalid", "{res}");
    assert_eq!(
        f.sessions_of(uid).await,
        sessions,
        "a pruned key never reopens"
    );
    let tombstones: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM mcp_operation_tombstones WHERE tenant_id = $1 AND key_id = $2",
    )
    .bind(&f.tenant)
    .bind(kp["id"].as_str().unwrap())
    .fetch_one(f.a.pg_pool().unwrap())
    .await
    .unwrap();
    assert_eq!(tombstones, 1);
    // A live key presented again with a different expiry is not the same
    // key: refused, never a receipt or a second claim.
    let kx = fresh_key();
    tool(
        &f.ra,
        &tok,
        "open_session",
        json!({"operation_key": kx.clone(), "runbook": "handbook"}),
    )
    .await;
    let sessions = f.sessions_of(uid).await;
    let shifted = key(kx["id"].as_str().unwrap(), 7200);
    let res = tool(
        &rb,
        &tok,
        "open_session",
        json!({"operation_key": shifted, "runbook": "handbook"}),
    )
    .await;
    assert_eq!(problem_slug(&res), "mcp-operation-key-invalid", "{res}");
    assert_eq!(f.sessions_of(uid).await, sessions);
    let _ = f.beta.clone();
}

/// A5-T3 / A6-T3: with MCP rows in place, a replica with the switch off
/// serves every native read and write, `/mcp` is gone, and the report
/// filters work for every plane; the inner dispatch was counted once.
#[tokio::test]
async fn a5_t3_default_off_rollback_over_mcp_rows() {
    let Some(url) = database() else { return };
    let f = fixture(url, enabled()).await;
    let uid = "rollback-user";
    let tok = f.reader(uid);
    let rest_before = counter(&f.a, "munarium_admission_total", "plane=\"rest\"");
    let opened = tool(
        &f.ra,
        &tok,
        "open_session",
        json!({"operation_key": fresh_key(), "runbook": "handbook"}),
    )
    .await;
    let session = session_of(&opened);
    // Counted once: one MCP admission, and one REST-plane R7 slot for the
    // MCP request; the inner dispatch is not admitted again.
    assert_eq!(
        counter(&f.a, "munarium_mcp_admissions_total", "open_session"),
        1
    );
    assert_eq!(
        counter(&f.a, "munarium_admission_total", "plane=\"rest\"") - rest_before,
        1,
        "one R7 slot for the MCP request"
    );
    assert!(f.invocations().await >= 1);

    let (_, off) = replica(&f, McpSettings::default(), "mcp-off").await;
    let r = send_raw(&off, "POST", Some(&tok), JSON_HEADERS, b"{}".to_vec()).await;
    assert_eq!(r.status, 404);
    let native_tok = f.tok(uid, 0, &["query"]);
    let get = off
        .clone()
        .oneshot(
            axum::http::Request::get(format!("/v1/sessions/{session}"))
                .header("authorization", format!("Bearer {native_tok}"))
                .header("x-munarium-uid", uid)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get.status(), 200);
    let turn = off
        .clone()
        .oneshot(
            axum::http::Request::post(format!("/v1/sessions/{session}/turns"))
                .header("authorization", format!("Bearer {native_tok}"))
                .header("x-munarium-uid", uid)
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({"query": "orchid"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(turn.status(), 200);
    for plane in ["rest", "grpc", "mcp"] {
        let rows = crate::reports_api::op_timeseries(&f.a, &f.tenant, "1h", Some(plane)).await;
        assert!(rows.is_ok(), "{plane}");
    }
    assert!(
        crate::reports_api::op_timeseries(&f.a, &f.tenant, "1h", Some("ws"))
            .await
            .is_err()
    );
}
