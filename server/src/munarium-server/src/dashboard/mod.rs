// SPDX-License-Identifier: Apache-2.0
//! /admin — the built-in operator console: the monitoring dashboards
//! (2026-08-17) plus the control-plane inventory, viewers, and actions
//! (2026-08-27). Server-rendered HTML + inline SVG (charts.rs), zero
//! JavaScript, no external assets. Mounted on the REST plane OUTSIDE /v1, so
//! the capture middleware counts it in the RED metrics but never records
//! interactions for it (dashboard polling must not spam the audit trail),
//! and it stays outside the OpenAPI contract like /docs.
//!
//! Auth: every page requires the mgmt role. Two credential paths:
//! - `Authorization: Bearer <mgmt token>` (curl, scripts, the demo BFF), or
//! - the `__munarium_admin` cookie set by POST /admin/login (browsers). The
//!   cookie holds the static mgmt token itself — acceptable for the demo
//!   posture and documented in docs/security-posture.md; HttpOnly +
//!   SameSite=Strict. No Secure attribute because the demo serves plain
//!   http on loopback; deployed environments front this with TLS at the
//!   gateway/ingress.
//!
//! Actions (the control-plane half) keep the role invariant the security
//! posture states — a leaked mgmt token cannot write the ledger:
//! - management-plane actions (issue / revoke capability tokens) run on the
//!   admin credential itself, exactly like their /v1 twins (mgmt role);
//! - the one rw action — approving a runbook gate, which appends ledger
//!   events when the run names a lineage — takes the rw credential IN THE
//!   FORM, per submission, never stored: the same token `mmctl` or curl
//!   would present. Under MUNARIUM_AUTH_MODE=disabled the pseudo-principal
//!   is already rw and the field may stay empty.
//!
//! Every state-changing form carries the stateless CSRF synchronizer token
//! (`csrf_token`), and a trusted view-only proxy sends
//! `X-Munarium-Admin-View-Only: 1` to have every action form rendered as a
//! note — how the demo's GET-only passthrough shows these pages without
//! offering a button that would 405 behind it.
//!
//! Data: every number on these pages comes from the api modules' `op_*`
//! functions (SQL stays in those modules) or from live process state
//! (metrics gauges, the registries). Memory-store mode renders an honest
//! "needs the postgres store" note wherever a table is the source; the
//! registries (shapes, providers, chronology rules) render on both stores.

mod inventory;
mod monitoring;
mod runbooks;
mod storage;

use crate::charts;
use crate::state::{AppState, TenantCtx};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use munarium_core::KernelError;
use std::sync::Arc;

const COOKIE: &str = "__munarium_admin";

/// Sent by a trusted view-only proxy (the demo BFF's GET-only passthrough):
/// action forms render as notes, so a page that cannot POST through the
/// proxy never offers a button that would fail behind it.
pub const VIEW_ONLY_HEADER: &str = "x-munarium-admin-view-only";

/// The authenticated admin request.
pub(crate) struct AdminCtx {
    pub tenant: TenantCtx,
    pub view_only: bool,
    /// The CSRF synchronizer token bound to this request's credential.
    pub csrf: String,
}

fn cookie_token(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    raw.split(';').find_map(|part| {
        let (k, v) = part.trim().split_once('=')?;
        (k == COOKIE && !v.is_empty()).then(|| v.to_string())
    })
}

/// `X-Munarium-Admin-View-Only` is on for any value except an explicit
/// off (`0`, `false`, `no`), so a proxy that sends `1` and one that sends
/// `true` both get the view-only rendering; the header only ever REMOVES
/// buttons, never grants anything, so a lenient parse is the safe side.
fn view_only_requested(headers: &HeaderMap) -> bool {
    headers
        .get(VIEW_ONLY_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|v| !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no"))
        .unwrap_or(false)
}

/// The credential this request authenticated with (bearer wins, cookie is
/// the browser path) — the CSRF token is bound to it.
fn admin_credential(headers: &HeaderMap) -> String {
    crate::rest::bearer(headers)
        .map(String::from)
        .or_else(|| cookie_token(headers))
        .unwrap_or_default()
}

/// mgmt-or-bust: bearer wins, cookie is the browser fallback. Failures
/// redirect to the login form (browsers) — curl callers with a bad token get
/// the same redirect and can read the Location header.
fn admin_auth(state: &AppState, headers: &HeaderMap) -> Result<AdminCtx, Response> {
    let token = crate::rest::bearer(headers)
        .map(String::from)
        .or_else(|| cookie_token(headers));
    match state.authenticate(token.as_deref()) {
        Ok(ctx) if ctx.role == "mgmt" || ctx.disabled_mode => Ok(AdminCtx {
            csrf: csrf_token(state, headers),
            view_only: view_only_requested(headers),
            tenant: ctx,
        }),
        _ => Err(Redirect::to("/admin/login").into_response()),
    }
}

/// The rw credential a control-plane action form presents. It must
/// authenticate, carry the rw role, and belong to the admin's own tenant —
/// a valid rw token for ANOTHER tenant is refused, never silently applied
/// across the boundary. Checked before any store access, so the refusal
/// costs nothing and reads the same on every store.
fn rw_credential(state: &AppState, admin: &AdminCtx, token: &str) -> Result<TenantCtx, String> {
    let token = token.trim();
    let ctx = state
        .authenticate((!token.is_empty()).then_some(token))
        .map_err(|e| format!("rw credential: {e}"))?;
    ctx.require_rw().map_err(|e| e.to_string())?;
    if ctx.tenant_id != admin.tenant.tenant_id {
        return Err("rw credential belongs to a different tenant".into());
    }
    Ok(ctx)
}

// ---------------------------------------------------------------------------
// page chrome
// ---------------------------------------------------------------------------

const NAV: &[(&str, &str)] = &[
    ("/admin", "overview"),
    ("/admin/traffic", "traffic"),
    ("/admin/endpoints", "endpoints"),
    ("/admin/usage", "usage"),
    ("/admin/providers", "providers"),
    ("/admin/runbooks", "runbooks"),
    ("/admin/collections", "collections"),
    ("/admin/storage", "storage"),
    ("/admin/sessions", "sessions"),
    ("/admin/tokens", "tokens"),
    ("/admin/audit", "audit"),
    ("/admin/findings", "findings"),
    ("/admin/matrix", "matrix"),
    ("/admin/health", "health"),
];

fn nav(active: &str, view_only: bool) -> String {
    let links: String = NAV
        .iter()
        .map(|(href, name)| {
            let class = if *name == active {
                " class=\"active\""
            } else {
                ""
            };
            format!(r#"<a href="{href}"{class}>{name}</a>"#)
        })
        .collect();
    let badge = if view_only {
        r#"<span class="badge">view-only</span>"#
    } else {
        ""
    };
    format!(r#"<nav><span class="brand">munarium admin{badge}</span>{links}</nav>"#)
}

fn page(active: &str, view_only: bool, title: &str, body: &str) -> Html<String> {
    Html(format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>{} — munarium admin</title>{}</head><body>{}<main><h1>{}</h1>{body}</main></body></html>"#,
        charts::esc(title),
        charts::STYLE,
        nav(active, view_only),
        charts::esc(title),
    ))
}

/// A full page for an authenticated admin request.
pub(crate) fn render(admin: &AdminCtx, active: &str, title: &str, body: &str) -> Response {
    page(active, admin.view_only, title, body).into_response()
}

/// The honest panel for a page whose ONLY source failed (most commonly: the
/// memory store, no postgres). A missing object answers 404 so scripts and
/// proxies see the truth; everything else stays 200 with the message.
pub(crate) fn error_panel(
    admin: &AdminCtx,
    active: &str,
    title: &str,
    e: &KernelError,
) -> Response {
    let status = match e {
        KernelError::NotFound { .. } => StatusCode::NOT_FOUND,
        _ => StatusCode::OK,
    };
    let body = format!(
        r#"<div class="notice">{}</div>"#,
        charts::esc(&e.to_string())
    );
    (status, page(active, admin.view_only, title, &body)).into_response()
}

/// The per-section note for a page that still has other sources to show.
/// The "needs the postgres store" framing applies only when that is the
/// reason — a rejected `?window=` is a caller error and says so.
pub(crate) fn store_note(e: &KernelError) -> String {
    let msg = e.to_string();
    let prefix = if msg.contains("postgres") {
        "needs the postgres store: "
    } else {
        ""
    };
    format!(r#"<div class="notice">{prefix}{}</div>"#, charts::esc(&msg))
}

pub(crate) fn notice(admin: &AdminCtx, active: &str, title: &str, msg: &str) -> Response {
    let body = format!(r#"<div class="notice">{}</div>"#, charts::esc(msg));
    render(admin, active, title, &body)
}

pub(crate) fn stale_form(admin: &AdminCtx, active: &str, title: &str) -> Response {
    notice(
        admin,
        active,
        title,
        "stale form (server restarted?) — go back and retry",
    )
}

pub(crate) fn window_of(q: &WindowParam) -> &str {
    q.window.as_deref().unwrap_or("24h")
}

pub(crate) fn window_picker(path: &str, current: &str) -> String {
    let links: String = ["1h", "24h", "7d", "30d"]
        .iter()
        .map(|w| {
            if *w == current {
                format!("<strong>{w}</strong>")
            } else {
                format!(r#"<a href="{path}?window={w}">{w}</a>"#)
            }
        })
        .collect::<Vec<_>>()
        .join(" · ");
    format!(r#"<div class="legend">window: {links}</div>"#)
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct WindowParam {
    #[serde(default)]
    pub window: Option<String>,
    #[serde(default)]
    pub group_by: Option<String>,
}

// ---------------------------------------------------------------------------
// small HTML helpers — every user-derived string passes through esc()
// ---------------------------------------------------------------------------

pub(crate) fn link(href: &str, text: &str) -> String {
    format!(
        r#"<a href="{}">{}</a>"#,
        charts::esc(href),
        charts::esc(text)
    )
}

pub(crate) fn opt(s: &Option<String>) -> String {
    s.as_deref()
        .map(charts::esc)
        .unwrap_or_else(|| "—".to_string())
}

pub(crate) fn short(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        format!("{}…", s.chars().take(n).collect::<String>())
    } else {
        s.to_string()
    }
}

pub(crate) fn pre(text: &str) -> String {
    format!("<pre>{}</pre>", charts::esc(text))
}

pub(crate) fn json_block(v: &serde_json::Value) -> String {
    pre(&serde_json::to_string_pretty(v).unwrap_or_default())
}

/// A two-column definition table. Values are TRUSTED HTML — callers escape
/// (or link) them.
pub(crate) fn kv(rows: &[(&str, String)]) -> String {
    let mut out = String::from(r#"<table class="kv">"#);
    for (k, v) in rows {
        out.push_str(&format!("<tr><td>{}</td><td>{v}</td></tr>", charts::esc(k)));
    }
    out.push_str("</table>");
    out
}

/// A data table whose cells are TRUSTED HTML (callers escape), rendered
/// open — unlike `charts::data_table`, which is a chart's collapsed twin.
pub(crate) fn html_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    if rows.is_empty() {
        return r#"<div class="empty">none</div>"#.into();
    }
    let mut out = String::from(r#"<table class="data"><tr>"#);
    for h in headers {
        out.push_str(&format!("<th>{}</th>", charts::esc(h)));
    }
    out.push_str("</tr>");
    for row in rows {
        out.push_str("<tr>");
        for cell in row {
            out.push_str(&format!("<td>{cell}</td>"));
        }
        out.push_str("</tr>");
    }
    out.push_str("</table>");
    out
}

/// A state with its reserved color, always beside the text label.
pub(crate) fn state_badge(state: &str) -> String {
    format!(
        r#"<span class="swatch" style="background:{}"></span> {}"#,
        charts::state_color(state),
        charts::esc(state)
    )
}

/// Gate-finding severity colors: block → critical, warn → warning, info →
/// series 1 (an informational, not a fault).
pub(crate) fn severity_badge(severity: &str) -> String {
    let color = match severity {
        "block" => "var(--critical)",
        "warn" => "var(--warning)",
        _ => "var(--s1)",
    };
    format!(
        r#"<span class="swatch" style="background:{color}"></span> {}"#,
        charts::esc(severity)
    )
}

/// The hidden CSRF field every action form starts with.
pub(crate) fn csrf_field(admin: &AdminCtx) -> String {
    format!(
        r#"<input type="hidden" name="_csrf" value="{}">"#,
        charts::esc(&admin.csrf)
    )
}

/// Either the action form, or — behind a view-only proxy — the note that
/// replaces it.
pub(crate) fn action(admin: &AdminCtx, form: String) -> String {
    if admin.view_only {
        r#"<span class="viewonly">view-only passthrough — actions are available on the server's own /admin (mgmt login)</span>"#.into()
    } else {
        form
    }
}

// ---------------------------------------------------------------------------
// CSRF
// ---------------------------------------------------------------------------

/// Stateless synchronizer token: sha256(secret || sha256(secret || cred)),
/// hex. The nested hash blocks length-extension shenanigans without an hmac
/// dependency; the per-boot secret means a restart invalidates in-flight
/// forms (they re-render — accepted caveat).
fn csrf_token(state: &AppState, headers: &HeaderMap) -> String {
    use sha2::Digest as _;
    let cred = admin_credential(headers);
    let inner = sha2::Sha256::digest(format!("{}:{cred}", state.boot_secret).as_bytes());
    hex::encode(sha2::Sha256::digest(
        [state.boot_secret.as_bytes(), &inner[..]].concat(),
    ))
}

fn csrf_ok(admin: &AdminCtx, provided: &str) -> bool {
    let expected = &admin.csrf;
    // Constant-time comparison over the fixed-length hex.
    if expected.len() != provided.len() {
        return false;
    }
    expected
        .bytes()
        .zip(provided.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

#[derive(Debug, serde::Deserialize)]
pub struct CsrfOnlyForm {
    #[serde(default)]
    _csrf: String,
}

// ---------------------------------------------------------------------------
// login
// ---------------------------------------------------------------------------

fn login_page(error: Option<&str>) -> Html<String> {
    let err = error
        .map(|e| format!(r#"<div class="notice">{}</div>"#, charts::esc(e)))
        .unwrap_or_default();
    Html(format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>munarium admin login</title>{}</head><body>
<form class="login" method="post" action="/admin/login">
<h1>munarium admin</h1>{err}
<label for="token">management token</label>
<input id="token" name="token" type="password" autocomplete="current-password" autofocus>
<button type="submit">sign in</button>
</form></body></html>"#,
        charts::STYLE
    ))
}

async fn login_form() -> Html<String> {
    login_page(None)
}

#[derive(Debug, serde::Deserialize)]
pub struct LoginForm {
    #[serde(default)]
    token: String,
}

async fn login_submit(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Form(form): axum::extract::Form<LoginForm>,
) -> Response {
    match state.authenticate(Some(form.token.trim())) {
        Ok(ctx) if ctx.role == "mgmt" || ctx.disabled_mode => {
            let cookie = format!(
                "{COOKIE}={}; HttpOnly; SameSite=Strict; Path=/admin",
                form.token.trim()
            );
            let mut resp = Redirect::to("/admin").into_response();
            resp.headers_mut().insert(
                axum::http::header::SET_COOKIE,
                axum::http::HeaderValue::from_str(&cookie)
                    .unwrap_or(axum::http::HeaderValue::from_static("")),
            );
            resp
        }
        Ok(_) => login_page(Some("that token is not a management (mgmt) token")).into_response(),
        Err(_) => login_page(Some("invalid token")).into_response(),
    }
}

/// Merged into the REST router BEFORE with_state — the shared capture
/// middleware then wraps these routes too (metrics only; /admin is not /v1,
/// so interactions are never recorded for dashboard polling).
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        // monitoring
        .route("/admin", get(monitoring::overview))
        .route("/admin/traffic", get(monitoring::traffic))
        .route("/admin/endpoints", get(monitoring::endpoints))
        .route("/admin/usage", get(monitoring::usage))
        .route("/admin/health", get(monitoring::health))
        // control plane: runbooks hub + viewers + the gate action
        .route("/admin/runbooks", get(runbooks::hub))
        .route("/admin/runbooks/{name}", get(runbooks::runbook))
        .route("/admin/shapes/{shape_ref}", get(runbooks::shape))
        .route("/admin/chronology-rules/{name}", get(runbooks::chronology))
        .route("/admin/runs/{run_id}", get(runbooks::run))
        .route(
            "/admin/runs/{run_id}/steps/{ordinal}/approve",
            post(runbooks::approve),
        )
        // control plane: inventory
        .route("/admin/providers", get(inventory::providers))
        .route("/admin/collections", get(inventory::collections))
        .route("/admin/storage", get(storage::storage))
        .route("/admin/collections/{id}", get(inventory::collection))
        .route("/admin/sessions", get(inventory::sessions))
        .route("/admin/sessions/{id}", get(inventory::session))
        .route("/admin/tokens", get(inventory::tokens))
        .route("/admin/tokens/issue", post(inventory::token_issue))
        .route("/admin/tokens/{jti}/revoke", post(inventory::token_revoke))
        .route("/admin/audit", get(inventory::audit))
        .route("/admin/findings", get(inventory::findings))
        .route("/admin/matrix", get(inventory::matrix))
        .route("/admin/login", get(login_form).post(login_submit))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AuthMode, Config, DocIntelConfig, SourceStoreConfig, StoreKind};
    use tower::ServiceExt;

    fn test_config(auth: AuthMode) -> Config {
        Config {
            http_addr: "127.0.0.1:0".into(),
            grpc_addr: None,
            ops_addr: "127.0.0.1:0".into(),
            store: StoreKind::Memory,
            database_url: None,
            auth,
            shutdown_grace_secs: 1,
            token_secret: Some("test-secret-for-the-admin-tests".into()),
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

    /// One mgmt token ("m"), one rw token ("w"), and an rw token of ANOTHER
    /// tenant ("w2") — the cross-tenant refusal needs it.
    fn static_auth() -> AuthMode {
        AuthMode::Static(vec![
            ("m".into(), "t".into(), "mgmt".into()),
            ("w".into(), "t".into(), "rw".into()),
            ("w2".into(), "t2".into(), "rw".into()),
        ])
    }

    async fn state(auth: AuthMode) -> Arc<AppState> {
        AppState::new(test_config(auth)).await.expect("state")
    }

    /// The reciprocal link to Matrix's own console. Three deployments,
    /// three answers — and the middle one is the trap: a deployment that
    /// configured only the service-to-service URL still gets a link, because
    /// on a single-host deployment that URL IS the browsable one, while a deployment
    /// that configured neither gets no `<a>` at all.
    #[tokio::test]
    async fn matrix_page_links_to_the_console_a_browser_can_reach() {
        // Neither configured: no link at all. (The memory store cannot serve
        // the report itself; the link is rendered on that path too, which is
        // exactly what lets this run without Postgres.)
        let state = state(static_auth()).await;
        let body = body_string(get(&state, "/admin/matrix", Some("m"), false).await).await;
        assert!(!body.contains("operator console</a>"), "{body}");

        // Only the service address: the console is assumed beside it.
        let mut cfg = test_config(static_auth());
        cfg.matrix_base_url = Some("https://matrix.internal:8180/".into());
        let state = AppState::new(cfg).await.expect("state");
        let body = body_string(get(&state, "/admin/matrix", Some("m"), false).await).await;
        assert!(
            body.contains(r#"href="https://matrix.internal:8180/admin""#),
            "{body}"
        );

        // An explicit console URL wins over the service address, verbatim —
        // it names the console, so nothing is appended to it.
        let mut cfg = test_config(static_auth());
        cfg.matrix_base_url = Some("http://matrix.internal:8180".into());
        cfg.matrix_admin_url = Some("https://matrix.example.com/admin/".into());
        let state = AppState::new(cfg).await.expect("state");
        let body = body_string(get(&state, "/admin/matrix", Some("m"), false).await).await;
        assert!(
            body.contains(r#"href="https://matrix.example.com/admin""#),
            "{body}"
        );
        assert!(!body.contains("matrix.internal"), "{body}");
    }

    async fn body_string(resp: Response) -> String {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    async fn get(
        state: &Arc<AppState>,
        path: &str,
        bearer: Option<&str>,
        view_only: bool,
    ) -> Response {
        let mut req = axum::http::Request::get(path);
        if let Some(b) = bearer {
            req = req.header("authorization", format!("Bearer {b}"));
        }
        if view_only {
            req = req.header(VIEW_ONLY_HEADER, "1");
        }
        crate::rest::router(state.clone())
            .oneshot(req.body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap()
    }

    async fn post_form(state: &Arc<AppState>, path: &str, bearer: &str, form: &str) -> Response {
        crate::rest::router(state.clone())
            .oneshot(
                axum::http::Request::post(path)
                    .header("authorization", format!("Bearer {bearer}"))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(axum::body::Body::from(form.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    fn csrf_for(state: &AppState, bearer: &str) -> String {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            axum::http::HeaderValue::from_str(&format!("Bearer {bearer}")).unwrap(),
        );
        csrf_token(state, &headers)
    }

    #[tokio::test]
    async fn authoring_pages_are_gone_and_the_nav_says_so() {
        let state = state(static_auth()).await;
        let resp = get(&state, "/admin/health", Some("m"), false).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(!body.contains("authoring"), "{body}");
        for name in ["runbooks", "collections", "tokens", "audit", "findings"] {
            assert!(
                body.contains(&format!(">{name}</a>")),
                "nav lacks {name}: {body}"
            );
        }
        let resp = get(&state, "/admin/authoring", Some("m"), false).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn view_only_header_replaces_every_action_form() {
        let state = state(static_auth()).await;
        // Without the header the tokens page offers the issue form.
        let body = body_string(get(&state, "/admin/tokens", Some("m"), false).await).await;
        assert!(body.contains(r#"action="/admin/tokens/issue""#), "{body}");
        assert!(body.contains(r#"name="_csrf""#));
        // With it: the note, the badge, and no form.
        let body = body_string(get(&state, "/admin/tokens", Some("m"), true).await).await;
        assert!(body.contains("view-only passthrough"), "{body}");
        assert!(body.contains(r#"<span class="badge">view-only</span>"#));
        assert!(!body.contains(r#"name="_csrf""#), "{body}");
    }

    #[tokio::test]
    async fn token_issue_form_needs_csrf_then_mints_once() {
        let state = state(static_auth()).await;
        // Wrong synchronizer token: refused as a stale form, nothing minted.
        let resp = post_form(
            &state,
            "/admin/tokens/issue",
            "m",
            "_csrf=deadbeef&uid=alice&access_level=1&scope_query=1",
        )
        .await;
        let body = body_string(resp).await;
        assert!(body.contains("stale form"), "{body}");
        // Right token: the JWT renders exactly once, with its jti.
        let csrf = csrf_for(&state, "m");
        let resp = post_form(
            &state,
            "/admin/tokens/issue",
            "m",
            &format!(
                "_csrf={csrf}&uid=alice&access_level=1&scope_query=1&compartments=finance%2Clegal"
            ),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(body.contains("token minted"), "{body}");
        assert!(body.contains("tok-"), "{body}");
        assert!(body.contains(r#"class="secret""#), "{body}");
    }

    #[tokio::test]
    async fn approving_a_gate_takes_the_rw_credential_not_the_mgmt_cookie() {
        let state = state(static_auth()).await;
        let csrf = csrf_for(&state, "m");
        // No rw credential in the form: refused before any store access.
        let body = body_string(
            post_form(
                &state,
                "/admin/runs/run-x/steps/0/approve",
                "m",
                &format!("_csrf={csrf}&rw_token="),
            )
            .await,
        )
        .await;
        assert!(body.contains("rw credential"), "{body}");
        // The mgmt token itself is not rw either.
        let body = body_string(
            post_form(
                &state,
                "/admin/runs/run-x/steps/0/approve",
                "m",
                &format!("_csrf={csrf}&rw_token=m"),
            )
            .await,
        )
        .await;
        assert!(body.contains("rw required"), "{body}");
        // Another tenant's rw token is refused, never applied across.
        let body = body_string(
            post_form(
                &state,
                "/admin/runs/run-x/steps/0/approve",
                "m",
                &format!("_csrf={csrf}&rw_token=w2"),
            )
            .await,
        )
        .await;
        assert!(body.contains("different tenant"), "{body}");
        // The right rw token passes the credential gate; on the memory store
        // the run lookup is then what refuses (proving the gate was passed).
        let body = body_string(
            post_form(
                &state,
                "/admin/runs/run-x/steps/0/approve",
                "m",
                &format!("_csrf={csrf}&rw_token=w"),
            )
            .await,
        )
        .await;
        assert!(body.contains("postgres"), "{body}");
        // And behind the view-only proxy the action is refused outright.
        let resp = crate::rest::router(state.clone())
            .oneshot(
                axum::http::Request::post("/admin/runs/run-x/steps/0/approve")
                    .header("authorization", "Bearer m")
                    .header(VIEW_ONLY_HEADER, "1")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(axum::body::Body::from(format!("_csrf={csrf}&rw_token=w")))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(body_string(resp).await.contains("view-only"));
    }

    const SHAPE: &str = "apiVersion: munarium.ioka.io/v1\nkind: Shape\n\
        metadata: { name: contract-clauses, version: 1 }\n\
        spec:\n  fact:\n    schema:\n      type: object\n      required: [subject]\n\
        \x20 chunking: { max_chars: 512 }\n";

    #[tokio::test]
    async fn shape_viewer_and_runbooks_hub_render_from_the_registry() {
        // Disabled auth: the pseudo-principal is rw AND mgmt, so one router
        // publishes the shape and reads the admin pages.
        let state = state(AuthMode::Disabled).await;
        let resp = crate::rest::router(state.clone())
            .oneshot(
                axum::http::Request::post("/v1/shapes")
                    .header("content-type", "text/yaml")
                    .body(axum::body::Body::from(SHAPE))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{}", body_string(resp).await);

        // The hub lists the shape from the registry and says honestly that
        // the runbook/run tables need postgres.
        let body = body_string(get(&state, "/admin/runbooks", None, false).await).await;
        assert!(body.contains("contract-clauses@1"), "{body}");
        assert!(body.contains("/admin/shapes/contract-clauses@1"), "{body}");
        assert!(body.contains("postgres"), "{body}");

        // The viewer: metadata, the fact schema, chunking, and the yaml —
        // flagged as re-serialized because the memory store keeps no bytes.
        let resp = get(&state, "/admin/shapes/contract-clauses@1", None, false).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(body.contains("contract-clauses"), "{body}");
        assert!(body.contains("&quot;required&quot;"), "{body}");
        assert!(body.contains("512"), "{body}");
        assert!(body.contains("re-serialized"), "{body}");

        // Unknown shape: a real 404, not a 200 with a sad face.
        let resp = get(&state, "/admin/shapes/nope@9", None, false).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn health_shows_configuration_without_secrets() {
        let state = state(static_auth()).await;
        let body = body_string(get(&state, "/admin/health", Some("m"), false).await).await;
        assert!(body.contains("static — 3 tokens"), "{body}");
        assert!(body.contains("test-instance"), "{body}");
        assert!(body.contains("token secret"), "{body}");
        assert!(!body.contains("test-secret-for-the-admin-tests"), "{body}");
        // Static tokens never render either.
        assert!(!body.contains(">w2<"), "{body}");
    }
}
