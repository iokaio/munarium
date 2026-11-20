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
//! - a cookie set by POST /admin/login (browsers). Which cookie depends on
//!   `MUNARIUM_CONSOLE_SESSIONS` (ClearView U10, crate::console_sessions):
//!   - `legacy` (the v1.4 default): `__munarium_admin` holds the static mgmt
//!     token itself — acceptable for the demo posture and documented in
//!     docs/security-posture.md; HttpOnly + SameSite=Strict. No Secure
//!     attribute because the demo serves plain http on loopback; deployed
//!     environments front this with TLS at the gateway/ingress.
//!   - `enabled`: the cookie carries only an opaque, revocable session
//!     handle; the v1.4 cookie is ignored. POST /admin/logout signs out.
//!
//! Actions (the control-plane half) keep the role invariant the security
//! posture states — a leaked mgmt token cannot write the ledger:
//! - management-plane actions (issue / revoke capability tokens) run on the
//!   admin credential itself, exactly like their /v1 twins (mgmt role);
//! - the one rw action — approving a runbook gate, which appends ledger
//!   events when the run names a lineage — takes the rw credential IN THE
//!   FORM, per submission, never stored: the same token `mmctl` or curl
//!   would present. Under MUNARIUM_AUTH_MODE=disabled the pseudo-principal
//!   is already rw and the field may stay empty. Like token revocation it is
//!   reviewed first and confirmed by typing the target (ClearView U4), and
//!   the confirmation takes a heavy REST admission slot (R7, admission.rs).
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

mod console_sessions;
#[cfg(test)]
mod html_checks;
mod inventory;
mod jobs;
mod mcp;
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

const COOKIE: &str = crate::console_sessions::LEGACY_COOKIE;
use crate::console_sessions::Use as SessionUse;

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
    /// The secret the CSRF token and review plans derive from: the per-boot
    /// secret for a bearer or v1.4 cookie request, the session's own secret
    /// (shared by every replica) for a console session.
    pub binding: String,
    /// The console session this request authenticated with, if any.
    pub session: Option<String>,
    /// The replica that rendered the page (U2 header context): replicas can
    /// disagree about process-local state such as gauges.
    pub instance: String,
    /// The store mode this replica runs (U2 header context).
    pub store: &'static str,
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|h| h.to_str().ok())
        .flat_map(|raw| raw.split(';'))
        .find_map(|part| {
            let (k, v) = part.trim().split_once('=')?;
            (k == name && !v.is_empty()).then(|| v.to_string())
        })
}

fn cookie_token(headers: &HeaderMap) -> Option<String> {
    cookie_value(headers, COOKIE)
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
///
/// Under `MUNARIUM_CONSOLE_SESSIONS=enabled` the browser fallback is the
/// session cookie only: its handle must resolve to a live session that the
/// replica's current configuration still authorizes (crate::console_sessions).
/// The v1.4 cookie is then ignored. Under `legacy` the session cookie is
/// never read. A bearer header behaves the same in both modes.
async fn admin_auth(state: &AppState, headers: &HeaderMap) -> Result<AdminCtx, Response> {
    admin_auth_as(state, headers, SessionUse::Active).await
}

/// The query a self-reloading page's refresh URL carries ([`refresh_url`]).
const REFRESH_QUERY: &str = "refresh=1";

/// Where a self-reloading page's `<meta http-equiv="refresh">` points: the
/// page itself, marked so the reload is not counted as the operator's use
/// of a console session ([`admin_auth_for_page`]).
pub(crate) fn refresh_url(path: &str) -> String {
    format!("{path}?{REFRESH_QUERY}")
}

/// [`admin_auth`] for a page that may reload itself: a request for the
/// page's [`refresh_url`] checks the console session's liveness like any
/// other, but does not restart its idle clock, so a page left open cannot
/// keep an unattended session alive (ClearView U10 and U12). The marker can
/// only shorten a session, never extend or grant one.
pub(crate) async fn admin_auth_for_page(
    state: &AppState,
    headers: &HeaderMap,
    query: Option<&str>,
) -> Result<AdminCtx, Response> {
    let using = if query == Some(REFRESH_QUERY) {
        SessionUse::Passive
    } else {
        SessionUse::Active
    };
    admin_auth_as(state, headers, using).await
}

async fn admin_auth_as(
    state: &AppState,
    headers: &HeaderMap,
    using: SessionUse,
) -> Result<AdminCtx, Response> {
    let store = if state.pg_pool().is_some() {
        "postgres"
    } else {
        "memory"
    };
    let bearer = crate::rest::bearer(headers);
    let settings = &state.console.settings;
    if bearer.is_some() || !settings.sessions() {
        let token = bearer.map(String::from).or_else(|| cookie_token(headers));
        return match state.authenticate(token.as_deref()) {
            Ok(ctx) if ctx.role == "mgmt" || ctx.disabled_mode => Ok(AdminCtx {
                csrf: csrf_token(state, headers),
                binding: state.boot_secret.clone(),
                session: None,
                view_only: view_only_requested(headers),
                tenant: ctx,
                instance: state.config.instance_id.clone(),
                store,
            }),
            _ => Err(Redirect::to("/admin/login").into_response()),
        };
    }
    let handle = cookie_value(headers, settings.cookie_name());
    let live = match &handle {
        Some(h) => crate::console_sessions::resolve(state, h, using).await,
        None => Ok(None),
    };
    match live {
        Ok(Some(s)) => Ok(AdminCtx {
            csrf: derive_token(&s.binding_secret, &format!("session:{}", s.session_id)),
            binding: s.binding_secret,
            session: Some(s.session_id),
            view_only: view_only_requested(headers),
            tenant: s.tenant,
            instance: state.config.instance_id.clone(),
            store,
        }),
        Ok(None) => Err(to_login(state, handle.is_some())),
        Err(e) => {
            // Fail closed: a store that cannot answer authorizes nothing.
            tracing::warn!(error = %e, "console session lookup failed; refusing");
            Err(to_login(state, false))
        }
    }
}

/// The redirect to the login form; `clear` also removes a session cookie
/// that no longer names a live session.
fn to_login(state: &AppState, clear: bool) -> Response {
    let mut resp = Redirect::to("/admin/login").into_response();
    if clear {
        append_cookie(&mut resp, &state.console.settings.clear_cookie());
    }
    resp
}

fn append_cookie(resp: &mut Response, cookie: &str) {
    if let Ok(v) = axum::http::HeaderValue::from_str(cookie) {
        resp.headers_mut().append(axum::http::header::SET_COOKIE, v);
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

/// One console page: its URL, its nav name (the `active` key handlers pass
/// to `render`), the one-sentence purpose shown under its title, and the
/// documents that explain it (ClearView U11).
pub(crate) struct NavPage {
    pub href: &'static str,
    pub name: &'static str,
    pub purpose: &'static str,
    /// Paths under `server/docs/`. The console links to the published copy
    /// instead of serving or duplicating the text; `docs_coverage` fails if
    /// any of these files is missing.
    pub docs: &'static [&'static str],
}

/// Where the console's documentation links point. The server serves no
/// guides itself (`/docs` is the OpenAPI page), so links go to the public
/// repository; the same relative path names the file in a source checkout.
pub(crate) const DOCS_BASE: &str = "https://github.com/iokaio/munarium/blob/main/server/docs/";

/// The error registry, under `DOCS_BASE`.
pub(crate) const ERRORS_DOC: &str = "api/errors.md";

/// A problem slug, linked to its row in the error registry (ClearView U11).
/// Every slug the server emits has a `#problem-<slug>` anchor there, which
/// `docs_coverage` checks.
pub(crate) fn problem_link(slug: &str) -> String {
    registry_link("problem", slug)
}

/// The problem slug the API returns for `e`, linked to the registry.
pub(crate) fn problem_of(e: &KernelError) -> String {
    problem_link(crate::error::problem_slug(e))
}

/// A build-job failure code, linked to its registry row
/// (`#job-failure-<code>`) when it is one this server defines; a code a newer
/// builder wrote is shown unlinked rather than pointing at a missing anchor.
pub(crate) fn job_failure_link(code: &str) -> String {
    use munarium_store_pg::job_failure::JobFailureCode;
    if JobFailureCode::ALL.iter().any(|c| c.as_str() == code) {
        registry_link("job-failure", code)
    } else {
        format!("<code>{}</code>", charts::esc(code))
    }
}

fn registry_link(kind: &str, key: &str) -> String {
    format!(
        r#"<a href="{}" aria-label="{} in the error registry"><code>{}</code></a>"#,
        charts::esc(&format!("{DOCS_BASE}{ERRORS_DOC}#{kind}-{key}")),
        charts::esc(key),
        charts::esc(key)
    )
}

/// A navigation group: one top-level link (to its first page) and, when the
/// group has more than one page, a row of sub-tabs (ClearView U1). Grouped
/// by operator task; every v1.4 URL is still a destination.
pub(crate) struct NavGroup {
    pub label: &'static str,
    pub pages: &'static [NavPage],
}

const fn nav_page(
    href: &'static str,
    name: &'static str,
    purpose: &'static str,
    docs: &'static [&'static str],
) -> NavPage {
    NavPage {
        href,
        name,
        purpose,
        docs,
    }
}

/// One task on the overview's empty-deployment landing (ClearView U11,
/// after AIRK's task-first landing page).
pub(crate) struct FirstTask {
    pub title: &'static str,
    /// A path under `server/docs/`, optionally with a heading anchor;
    /// `docs_coverage` checks that both exist.
    pub doc: &'static str,
    pub does: &'static str,
}

/// The three first tasks an empty deployment offers instead of tiles
/// reading zero.
pub(crate) const FIRST_TASKS: &[FirstTask] = &[
    FirstTask {
        title: "Create an index",
        doc: "guides/getting-started.md#4-define-a-shape-and-a-runbook",
        does: "define a shape and a runbook, ingest documents, then build, inspect and approve the index.",
    },
    FirstTask {
        title: "Connect an agent",
        doc: "ops/mcp.md",
        does: "issue a capability token and connect an agent through the opt-in MCP endpoint.",
    },
    FirstTask {
        title: "Find missing results",
        doc: "guides/getting-started.md#common-first-run-problems",
        does: "when a runbook validates but retrieves nothing, check upload prefixes, media types, bound collections, run-step results and the approved cutover.",
    },
];

pub(crate) const NAV: &[NavGroup] = &[
    NavGroup {
        label: "overview",
        pages: &[nav_page(
            "/admin",
            "overview",
            "Whether anything needs attention, and the size of what is deployed.", &["observability.md", "ops/troubleshooting.md"])],
    },
    NavGroup {
        label: "traffic",
        pages: &[
            nav_page(
                "/admin/traffic",
                "traffic",
                "Request rate, errors and latency across the API over a time window.", &["observability.md"]),
            nav_page(
                "/admin/endpoints",
                "endpoints",
                "Request volume, errors and latency for each route.", &["observability.md", "api/rest.md"]),
            nav_page(
                "/admin/usage",
                "usage",
                "API usage over time, grouped by the dimension you choose.", &["observability.md"]),
        ],
    },
    NavGroup {
        label: "retrieval",
        pages: &[
            nav_page(
                "/admin/collections",
                "collections",
                "Collections, their access controls and index versions, and recent bulk uploads.", &["guides/getting-started.md", "guides/loading-corpora.md"]),
            nav_page(
                "/admin/storage",
                "storage",
                "Where collection data lives across the storage tiers and nodes.", &["guides/datastore.md"]),
            nav_page(
                "/admin/build-jobs",
                "build jobs",
                "Queued index build jobs: their state, recorded progress, transition history and per-version outcomes.", &["guides/datastore.md"]),
            nav_page(
                "/admin/sessions",
                "sessions",
                "Conversation sessions: activity over time and each session turn by turn.", &["guides/getting-started.md", "observability.md"]),
        ],
    },
    NavGroup {
        label: "runbooks",
        pages: &[nav_page(
            "/admin/runbooks",
            "runbooks",
            "Deployed runbooks, shapes and runs, including runs waiting for gate approval.", &["guides/getting-started.md", "ops/mmctl.md"])],
    },
    NavGroup {
        label: "access and audit",
        pages: &[
            nav_page(
                "/admin/tokens",
                "tokens",
                "Capability tokens issued to end users: review, issue and revoke them.", &["guides/managing-key-and-secrets.md", "security-posture.md"]),
            nav_page(
                "/admin/audit",
                "audit",
                "Every /v1 request, newest first, attributed to its uid.", &["observability.md", "security-posture.md"]),
            nav_page(
                "/admin/findings",
                "findings",
                "Gate findings recorded at write time and by Matrix reconciliation, newest first.", &["observability.md", "api/rest.md"]),
            nav_page(
                "/admin/mcp",
                "mcp",
                "This tenant's MCP activity by tool, its unknown outcomes, and the tokens that can enter MCP.", &["ops/mcp.md", "security-posture.md"]),
            nav_page(
                "/admin/console-sessions",
                "console sessions",
                "Signed-in console sessions for this tenant: when each started and was last used, and revoking them.", &["design/console-sessions.md", "security-posture.md"]),
        ],
    },
    NavGroup {
        label: "platform",
        pages: &[
            nav_page(
                "/admin/providers",
                "providers",
                "Configured model providers, whether their credentials resolve, and token spend.", &["guides/managing-key-and-secrets.md", "guides/ollama.md"]),
            nav_page(
                "/admin/health",
                "health",
                "This instance's process state and effective non-secret configuration.", &["ops/troubleshooting.md", "observability.md"]),
            nav_page(
                "/admin/matrix",
                "matrix",
                "Whether the structured-evidence plane is reachable, and which evidence layer refuses.", &["guides/evidence-hierarchy.md"]),
        ],
    },
];

/// The page registered under nav name `active`, if any.
pub(crate) fn nav_lookup(active: &str) -> Option<(&'static NavGroup, &'static NavPage)> {
    NAV.iter()
        .find_map(|g| g.pages.iter().find(|p| p.name == active).map(|p| (g, p)))
}

/// Six top-level links, then the active group's sub-tabs. The current page
/// carries `aria-current="page"` (and the `active` class for styling); its
/// group's top-level link carries `active` too. Detail pages (a session, a
/// shape) mark the list page they belong to.
fn nav(active: &str, admin: &AdminCtx) -> String {
    let view_only = admin.view_only;
    let current = nav_lookup(active);
    let current_group = current.map(|(g, _)| g.label);
    let groups: String = NAV
        .iter()
        .map(|g| {
            let first = &g.pages[0];
            let in_group = current_group == Some(g.label);
            let attrs = match (in_group, g.pages.len() == 1) {
                (true, true) => r#" class="active" aria-current="page""#,
                (true, false) => r#" class="active""#,
                _ => "",
            };
            format!(r#"<a href="{}"{attrs}>{}</a>"#, first.href, g.label)
        })
        .collect();
    let badge = if view_only {
        r#"<span class="badge">view-only</span>"#
    } else {
        ""
    };
    let subnav = match current {
        Some((g, _)) if g.pages.len() > 1 => {
            let tabs: String = g
                .pages
                .iter()
                .map(|p| {
                    let attrs = if p.name == active {
                        r#" class="active" aria-current="page""#
                    } else {
                        ""
                    };
                    format!(r#"<a href="{}"{attrs}>{}</a>"#, p.href, p.name)
                })
                .collect();
            format!(
                r#"<nav class="subnav" aria-label="{} pages">{tabs}</nav>"#,
                g.label
            )
        }
        _ => String::new(),
    };
    // Sign-out is offered to a console session only: a bearer request has
    // nothing to sign out, and the v1.4 pages stay as they were.
    let signout = match &admin.session {
        Some(_) if !view_only => format!(
            r#"<form class="action" method="post" action="/admin/logout">{}<button type="submit" class="secondary">sign out</button></form>"#,
            csrf_field(admin)
        ),
        _ => String::new(),
    };
    format!(
        r#"<nav aria-label="console"><span class="brand">munarium admin{badge}</span>{groups}{signout}</nav>{subnav}"#
    )
}

/// The header every console page shares (ClearView U2): the title, the
/// page's one-sentence purpose, optional actions (trusted HTML — callers
/// build them with `action`), and the context the numbers below were read
/// in: the credential's tenant, the replica that rendered the page, the
/// store mode, and the time of the read.
fn page_header(admin: &AdminCtx, active: &str, title: &str, actions: &str) -> String {
    let purpose = nav_lookup(active)
        .map(|(_, p)| {
            format!(
                r#"<p class="purpose">{}</p>{}"#,
                charts::esc(p.purpose),
                docs_line(p.docs)
            )
        })
        .unwrap_or_default();
    let actions = if actions.is_empty() {
        String::new()
    } else {
        format!(r#"<div class="actions">{actions}</div>"#)
    };
    format!(
        r#"<header class="pagehead"><div class="headrow"><h1>{}</h1>{actions}</div>{purpose}<p class="context">tenant <code>{}</code> · instance <code>{}</code> · store {} · read {}</p></header>"#,
        charts::esc(title),
        charts::esc(&admin.tenant.tenant_id),
        charts::esc(&admin.instance),
        admin.store,
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC"),
    )
}

/// The documentation line under a page's purpose (ClearView U11): each
/// linked document is named by its path under `server/docs/`.
fn docs_line(docs: &[&str]) -> String {
    if docs.is_empty() {
        return String::new();
    }
    let links: Vec<String> = docs
        .iter()
        .map(|d| link(&format!("{DOCS_BASE}{d}"), &format!("docs/{d}")))
        .collect();
    format!(
        r#"<p class="docs">Documentation: {}</p>"#,
        links.join(" · ")
    )
}

fn page(admin: &AdminCtx, active: &str, title: &str, actions: &str, body: &str) -> Html<String> {
    page_with_head(admin, active, title, actions, "", body)
}

/// [`page`] with extra trusted `<head>` content, such as the refresh a
/// running job's page carries (ClearView U12).
fn page_with_head(
    admin: &AdminCtx,
    active: &str,
    title: &str,
    actions: &str,
    head: &str,
    body: &str,
) -> Html<String> {
    Html(format!(
        r##"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">{head}<title>{} — munarium admin</title>{}</head><body><a class="skip" href="#main">skip to content</a>{}<main id="main">{}{body}</main></body></html>"##,
        charts::esc(title),
        charts::STYLE,
        nav(active, admin),
        page_header(admin, active, title, actions),
    ))
}

/// A full page for an authenticated admin request.
pub(crate) fn render(admin: &AdminCtx, active: &str, title: &str, body: &str) -> Response {
    page(admin, active, title, "", body).into_response()
}

/// A full page that reloads itself every `secs` seconds from `path`'s
/// [`refresh_url`], or a plain page when `None`. No script: a
/// `<meta http-equiv="refresh">`, emitted only while there is something to
/// watch (ClearView U12). The handler serves `path` through
/// [`admin_auth_for_page`], so the reloads do not keep a console session
/// from idling out.
pub(crate) fn render_refreshing(
    admin: &AdminCtx,
    active: &str,
    title: &str,
    refresh: Option<(u32, &str)>,
    body: &str,
) -> Response {
    let head = refresh
        .map(|(secs, path)| {
            format!(
                r#"<meta http-equiv="refresh" content="{secs};url={}">"#,
                charts::esc(&refresh_url(path))
            )
        })
        .unwrap_or_default();
    page_with_head(admin, active, title, "", &head, body).into_response()
}

/// A full page whose header carries actions (trusted HTML — build them with
/// `action` so a view-only proxy gets the note instead).
pub(crate) fn render_with_actions(
    admin: &AdminCtx,
    active: &str,
    title: &str,
    actions: &str,
    body: &str,
) -> Response {
    page(admin, active, title, actions, body).into_response()
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
        r#"<div class="notice">{} · problem {}</div>"#,
        charts::esc(&e.to_string()),
        problem_of(e)
    );
    (status, page(admin, active, title, "", &body)).into_response()
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
    format!(
        r#"<div class="notice">{prefix}{} · problem {}</div>"#,
        charts::esc(&msg),
        problem_of(e)
    )
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

/// One path segment, percent-encoded, for an API path the console prints.
pub(crate) fn path_segment(s: &str) -> String {
    inventory::urlencode(s)
}

/// One shell word: bare when it needs no quoting, otherwise single-quoted
/// (POSIX shells, PowerShell and cmd's mmctl all accept the bare form the
/// console's own identifiers use).
pub(crate) fn shell_word(s: &str) -> String {
    let bare = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "@._:/+=-".contains(c));
    if bare {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// The equivalent scripted route of a console detail page (ClearView U7):
/// the API request and, where one exists, the `mmctl` command, as
/// selectable text beside a "complete result" disclosure holding the whole
/// object the API returns. No script: the commands select with one click
/// (`user-select: all`), and the disclosure is a plain `<details>`.
///
/// `api` and `mmctl` are plain text (escaped here). `cli_note` explains a
/// missing CLI command. `value` is the complete result, when the page has
/// one that is not already shown in full.
pub(crate) fn complete_result(
    api: &str,
    mmctl: Option<&str>,
    cli_note: &str,
    value: Option<&serde_json::Value>,
) -> String {
    let cli = match mmctl {
        Some(cmd) => format!(r#"<code class="cmd">{}</code>"#, charts::esc(cmd)),
        None => format!(r#"<span class="muted">{}</span>"#, charts::esc(cli_note)),
    };
    let details = value
        .map(|v| {
            format!(
                r#"<details class="complete"><summary>complete result</summary>{}</details>"#,
                json_block(v)
            )
        })
        .unwrap_or_default();
    format!(
        r#"<section class="equiv" aria-label="equivalent API request and command"><h2>complete result</h2><dl><dt>API</dt><dd><code class="cmd">{}</code></dd><dt>mmctl</dt><dd>{cli}</dd></dl>{details}</section>"#,
        charts::esc(api)
    )
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

/// The tone of a status pill (ClearView U13). Each tone pairs a reserved
/// status color with text that stays legible on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    /// Nothing to report, or a plain fact.
    Neutral,
    Good,
    Warn,
    Bad,
}

/// A pill badge: the state's text label on its status color. The label is
/// always present, so the color is never the only cue.
pub(crate) fn pill(text: &str, tone: Tone) -> String {
    let class = match tone {
        Tone::Neutral => "badge",
        Tone::Good => "badge good",
        Tone::Warn => "badge warn",
        Tone::Bad => "badge bad",
    };
    format!(r#"<span class="{class}">{}</span>"#, charts::esc(text))
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
    derive_token(&state.boot_secret, &admin_credential(headers))
}

/// `hex(sha256(secret || sha256(secret || ":" || material)))`: the CSRF token
/// over the request's credential (or, for a console session, its id), and
/// the review plan over its fields.
fn derive_token(secret: &str, material: &str) -> String {
    use sha2::Digest as _;
    let inner = sha2::Sha256::digest(format!("{secret}:{material}").as_bytes());
    hex::encode(sha2::Sha256::digest(
        [secret.as_bytes(), &inner[..]].concat(),
    ))
}

fn csrf_ok(admin: &AdminCtx, provided: &str) -> bool {
    ct_eq(&admin.csrf, provided)
}

/// Constant-time comparison over fixed-length hex.
fn ct_eq(expected: &str, provided: &str) -> bool {
    if expected.len() != provided.len() {
        return false;
    }
    expected
        .bytes()
        .zip(provided.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

/// The review-then-confirm binding for a destructive console action
/// (ClearView U4). The review page embeds this value; the confirmation must
/// return it unchanged. It is bound to:
/// - the operator: `admin.csrf` is derived from the credential the request
///   authenticated with, so another credential computes a different plan;
/// - the tenant the credential belongs to;
/// - the intended action and its target;
/// - the target's current revision (`revision`: a digest of the row the
///   review showed), so a target that changed after review no longer
///   matches;
/// - the request's binding secret (`admin.binding`): the boot secret for a
///   bearer or v1.4 cookie request, so a restart expires every outstanding
///   review like the CSRF token; a console session's own secret otherwise,
///   so a review on one replica confirms on another and dies with the
///   session.
///
/// Stateless, like the CSRF token: nothing is stored between review and
/// confirmation. The nested hash is the same construction as `csrf_token`.
pub(crate) fn action_plan(admin: &AdminCtx, action: &str, target: &str, revision: &str) -> String {
    let fields = [
        "plan",
        admin.csrf.as_str(),
        admin.tenant.tenant_id.as_str(),
        action,
        target,
        revision,
    ];
    // Length-prefixed so no field can bleed into its neighbour.
    let mut material = String::new();
    for f in fields {
        material.push_str(&format!("{}:{f};", f.len()));
    }
    derive_token(&admin.binding, &material)
}

/// Whether a submitted plan is the one `action_plan` computes now.
pub(crate) fn plan_ok(
    admin: &AdminCtx,
    action: &str,
    target: &str,
    revision: &str,
    provided: &str,
) -> bool {
    ct_eq(&action_plan(admin, action, target, revision), provided)
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
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>munarium admin login</title>{}</head><body>
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

/// `POST /admin/login`. Under the default `legacy` login this is v1.4: the
/// cookie holds the token. Under `MUNARIUM_CONSOLE_SESSIONS=enabled` it
/// starts a revocable session, sets the cookie to its opaque handle and
/// removes any v1.4 cookie, so the static token no longer sits in the
/// browser. A session handle the browser already carried is signed out
/// first, so the session the new cookie replaces does not stay live,
/// unseen, until it expires. The token check is the same in both modes.
async fn login_submit(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Form(form): axum::extract::Form<LoginForm>,
) -> Response {
    let token = form.token.trim();
    match state.authenticate(Some(token)) {
        Ok(ctx)
            if (ctx.role == "mgmt" || ctx.disabled_mode) && state.console.settings.sessions() =>
        {
            if let Some(old) = cookie_value(&headers, state.console.settings.cookie_name()) {
                // Only a handle this browser holds can be named here, and
                // signing it out only ends a session: it grants nothing.
                if let Err(e) = crate::console_sessions::sign_out(&state, &old).await {
                    tracing::warn!(error = %e, "the replaced console session could not be signed out");
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        login_page(Some(
                            "the previous console session could not be signed out (session store unavailable), so no new one was started; try again",
                        )),
                    )
                        .into_response();
                }
            }
            match crate::console_sessions::create(&state, &ctx, token).await {
                Ok(handle) => {
                    let mut resp = Redirect::to("/admin").into_response();
                    append_cookie(&mut resp, &state.console.settings.set_cookie(&handle));
                    append_cookie(&mut resp, &crate::console_sessions::clear_legacy_cookie());
                    resp
                }
                Err(e) => {
                    tracing::warn!(error = %e, "console session could not be created");
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        login_page(Some(
                            "the console session could not be started (session store unavailable); try again",
                        )),
                    )
                        .into_response()
                }
            }
        }
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

/// `POST /admin/logout` — sign-out (ClearView U10). CSRF-protected like
/// every console form. A console session is revoked in the shared store, so
/// every replica refuses its handle from the next request, and its cookie is
/// cleared; a v1.4 cookie is just cleared. If the store cannot record the
/// sign-out, the response says so and the session stays as it was.
async fn logout(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Form(form): axum::extract::Form<CsrfOnlyForm>,
) -> Response {
    let admin = match admin_auth(&state, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    if !csrf_ok(&admin, &form._csrf) {
        return stale_form(&admin, "overview", "Sign out");
    }
    let mut resp = Redirect::to("/admin/login").into_response();
    if admin.session.is_some() {
        let settings = &state.console.settings;
        if let Some(handle) = cookie_value(&headers, settings.cookie_name()) {
            if let Err(e) = crate::console_sessions::sign_out(&state, &handle).await {
                let mut failed = notice(
                    &admin,
                    "overview",
                    "Sign out",
                    &format!("sign-out was not recorded, so the session is still active: {e}"),
                );
                *failed.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
                return failed;
            }
        }
        append_cookie(&mut resp, &settings.clear_cookie());
    } else if crate::rest::bearer(&headers).is_none() {
        append_cookie(&mut resp, &crate::console_sessions::clear_legacy_cookie());
    }
    resp
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
        // GET reviews; the v1.4 POST also reviews now (ClearView U4) and
        // only the typed confirmation below approves, under a heavy
        // admission slot (R7).
        .route(
            "/admin/runs/{run_id}/steps/{ordinal}/approve",
            get(runbooks::approve_review_page).post(runbooks::approve),
        )
        .route(
            runbooks::APPROVE_CONFIRM_ROUTE,
            post(runbooks::approve_confirm),
        )
        // control plane: inventory
        .route("/admin/providers", get(inventory::providers))
        .route("/admin/collections", get(inventory::collections))
        .route("/admin/storage", get(storage::storage))
        // ClearView U12: build jobs, read-only, from the R9 progress and
        // event records and the R6 per-version outcomes.
        .route("/admin/build-jobs", get(jobs::list))
        .route("/admin/build-jobs/{job_id}", get(jobs::detail))
        .route("/admin/collections/{id}", get(inventory::collection))
        .route("/admin/sessions", get(inventory::sessions))
        .route("/admin/sessions/{id}", get(inventory::session))
        .route("/admin/tokens", get(inventory::tokens))
        // GET is the issue form on its own page (U8); POST mints, as in v1.4.
        .route(
            "/admin/tokens/issue",
            get(inventory::token_issue_page).post(inventory::token_issue),
        )
        // GET reviews; the v1.4 POST also reviews now (ClearView U4) and
        // only the typed confirmation below acts.
        .route(
            "/admin/tokens/{jti}/revoke",
            get(inventory::token_revoke_review).post(inventory::token_revoke),
        )
        .route(
            "/admin/tokens/{jti}/revoke/confirm",
            post(inventory::token_revoke_confirm),
        )
        .route("/admin/audit", get(inventory::audit))
        .route("/admin/findings", get(inventory::findings))
        .route("/admin/matrix", get(inventory::matrix))
        .route("/admin/console-sessions", get(console_sessions::list))
        // ClearView §9.3: MCP activity, read-only; revocation is the token
        // review above.
        .route("/admin/mcp", get(mcp::page))
        .route(
            "/admin/console-sessions/{session_id}/revoke",
            get(console_sessions::revoke_review_page),
        )
        .route(
            "/admin/console-sessions/{session_id}/revoke/confirm",
            post(console_sessions::revoke_confirm),
        )
        .route("/admin/login", get(login_form).post(login_submit))
        .route("/admin/logout", post(logout))
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
            platform: None,
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
            managed_provider_diagnostics: false,
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
        // Grouped navigation (U1): every group is a top-level link on every
        // page, and each control-plane page is reachable and marked current
        // within its group.
        for group in NAV {
            assert!(
                body.contains(&format!(r#"href="{}""#, group.pages[0].href)),
                "nav lacks group {}: {body}",
                group.label
            );
        }
        for name in ["runbooks", "collections", "tokens", "audit", "findings"] {
            let (_, page) = nav_lookup(name).expect("registered");
            let body = body_string(get(&state, page.href, Some("m"), false).await).await;
            assert!(
                body.contains(&format!(r#"aria-current="page">{name}</a>"#)),
                "nav lacks {name}: {body}"
            );
        }
        let resp = get(&state, "/admin/authoring", Some("m"), false).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// The v1.4 console URLs. Bookmarks to any of them must keep working
    /// through the navigation reorganization (COMPAT-T5).
    const V14_PAGES: &[&str] = &[
        "/admin",
        "/admin/traffic",
        "/admin/endpoints",
        "/admin/usage",
        "/admin/providers",
        "/admin/runbooks",
        "/admin/collections",
        "/admin/storage",
        "/admin/sessions",
        "/admin/tokens",
        "/admin/audit",
        "/admin/findings",
        "/admin/matrix",
        "/admin/health",
    ];

    /// Navigation pages added after v1.4 (ClearView U10 and U12).
    const NEW_PAGES: &[&str] = &["/admin/console-sessions", "/admin/build-jobs", "/admin/mcp"];

    /// U2: every page has one `h1`, its purpose line, and the read context
    /// (tenant, instance, store mode, read time).
    #[tokio::test]
    async fn every_page_has_the_shared_header() {
        let state = state(static_auth()).await;
        for group in NAV {
            for page in group.pages {
                let body = body_string(get(&state, page.href, Some("m"), false).await).await;
                assert_eq!(body.matches("<h1>").count(), 1, "{}: {body}", page.href);
                assert!(
                    body.contains(&format!(
                        r#"<p class="purpose">{}</p>"#,
                        charts::esc(page.purpose)
                    )),
                    "{}: {body}",
                    page.href
                );
                // U11: the page links its documentation.
                for doc in page.docs {
                    assert!(
                        body.contains(&format!(r#"href="{DOCS_BASE}{doc}">docs/{doc}</a>"#)),
                        "{} lacks {doc}: {body}",
                        page.href
                    );
                }
                assert!(
                    body.contains(
                        r#"tenant <code>t</code> · instance <code>test-instance</code> · store memory · read "#
                    ),
                    "{}: {body}",
                    page.href
                );
            }
        }
        // Detail and error pages share the header too.
        let body = body_string(get(&state, "/admin/shapes/nope@9", Some("m"), false).await).await;
        assert_eq!(body.matches("<h1>").count(), 1, "{body}");
        assert!(body.contains(r#"class="context""#), "{body}");
    }

    #[tokio::test]
    async fn every_nav_link_is_a_served_route_and_v14_urls_still_serve() {
        let state = state(static_auth()).await;
        let mut hrefs = Vec::new();
        for group in NAV {
            assert!(!group.pages.is_empty(), "{}", group.label);
            for page in group.pages {
                hrefs.push(page.href);
                let resp = get(&state, page.href, Some("m"), false).await;
                assert_eq!(resp.status(), StatusCode::OK, "{}", page.href);
                let body = body_string(resp).await;
                // Exactly one current page, and it is this one.
                assert_eq!(
                    body.matches(r#"aria-current="page""#).count(),
                    1,
                    "{}: {body}",
                    page.href
                );
                assert!(
                    body.contains(&format!(
                        r#"href="{}" class="active" aria-current="page""#,
                        page.href
                    )),
                    "{}: {body}",
                    page.href
                );
            }
        }
        assert_eq!(NAV.len(), 6, "six top-level destinations");
        // Every v1.4 page is still in the navigation, and still served.
        for url in V14_PAGES {
            assert!(hrefs.contains(url), "{url} dropped from the nav");
            let resp = get(&state, url, Some("m"), false).await;
            assert_eq!(resp.status(), StatusCode::OK, "{url}");
        }
        // Every other nav page is new since v1.4 and listed here.
        for url in NEW_PAGES {
            assert!(hrefs.contains(url), "{url} missing from the nav");
            assert!(!V14_PAGES.contains(url));
        }
        assert_eq!(
            hrefs.len(),
            V14_PAGES.len() + NEW_PAGES.len(),
            "nav pages: {hrefs:?}"
        );
        let resp = get(&state, "/admin/login", None, false).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn view_only_header_replaces_every_action_form() {
        let state = state(static_auth()).await;
        // Without the header the tokens list offers the issue button, and the
        // issue page (its own view since U8) offers the issue form.
        let body = body_string(get(&state, "/admin/tokens", Some("m"), false).await).await;
        assert!(body.contains(r#"href="/admin/tokens/issue""#), "{body}");
        let body = body_string(get(&state, "/admin/tokens/issue", Some("m"), false).await).await;
        assert!(body.contains(r#"action="/admin/tokens/issue""#), "{body}");
        assert!(body.contains(r#"name="_csrf""#));
        // With it: the note, the badge, and no form or button on either.
        for path in ["/admin/tokens", "/admin/tokens/issue"] {
            let body = body_string(get(&state, path, Some("m"), true).await).await;
            assert!(body.contains("view-only passthrough"), "{body}");
            assert!(body.contains(r#"<span class="badge">view-only</span>"#));
            assert!(!body.contains(r#"name="_csrf""#), "{body}");
            assert!(!body.contains(r#"href="/admin/tokens/issue""#), "{body}");
            assert!(
                !body.contains("<form class=\"action\" method=\"post\""),
                "{body}"
            );
        }
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

    const APPROVE_CONFIRM: &str = "/admin/runs/run-x/steps/0/approve/confirm";

    /// The confirmation is where the rw credential is checked, per
    /// submission, before any store access (the mgmt/rw split).
    #[tokio::test]
    async fn approving_a_gate_takes_the_rw_credential_not_the_mgmt_cookie() {
        let state = state(static_auth()).await;
        let csrf = csrf_for(&state, "m");
        // No rw credential in the form: refused before any store access.
        let body = body_string(
            post_form(
                &state,
                APPROVE_CONFIRM,
                "m",
                &format!("_csrf={csrf}&confirm=run-x&rw_token="),
            )
            .await,
        )
        .await;
        assert!(body.contains("rw credential"), "{body}");
        // The mgmt token itself is not rw either.
        let body = body_string(
            post_form(
                &state,
                APPROVE_CONFIRM,
                "m",
                &format!("_csrf={csrf}&confirm=run-x&rw_token=m"),
            )
            .await,
        )
        .await;
        assert!(body.contains("rw required"), "{body}");
        // Another tenant's rw token is refused, never applied across.
        let body = body_string(
            post_form(
                &state,
                APPROVE_CONFIRM,
                "m",
                &format!("_csrf={csrf}&confirm=run-x&rw_token=w2"),
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
                APPROVE_CONFIRM,
                "m",
                &format!("_csrf={csrf}&confirm=run-x&rw_token=w"),
            )
            .await,
        )
        .await;
        assert!(body.contains("postgres"), "{body}");
        // And behind the view-only proxy the action is refused outright, on
        // the confirmation and on the v1.4 URL alike.
        for path in [APPROVE_CONFIRM, "/admin/runs/run-x/steps/0/approve"] {
            let body = body_string(
                post_form_view_only(
                    &state,
                    path,
                    "m",
                    &format!("_csrf={csrf}&confirm=run-x&rw_token=w"),
                )
                .await,
            )
            .await;
            assert!(body.contains("view-only"), "{path}: {body}");
        }
    }

    /// U4 for run-step approval, without a store: the review is a read and
    /// offers nothing it cannot back; the v1.4 POST is a review that still
    /// demands CSRF and ignores the rw token it carries; the confirmation
    /// refuses a wrong typed id, a missing CSRF token and a non-mgmt role
    /// before anything else.
    #[tokio::test]
    async fn approval_review_and_refusals_on_the_memory_store() {
        let state = state(static_auth()).await;
        let csrf = csrf_for(&state, "m");
        let review = "/admin/runs/run-x/steps/0/approve";
        let resp = get(&state, review, Some("m"), false).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(body.contains("postgres"), "{body}");
        assert!(plan_in(&body).is_none(), "{body}");
        // The v1.4 POST: CSRF still required, and with it the review — the
        // rw token in a v1.4 form approves nothing and is never echoed.
        let body =
            body_string(post_form(&state, review, "m", "_csrf=deadbeef&rw_token=w").await).await;
        assert!(body.contains("stale form"), "{body}");
        let body =
            body_string(post_form(&state, review, "m", &format!("_csrf={csrf}&rw_token=w")).await)
                .await;
        assert!(body.contains("postgres"), "{body}");
        assert!(!body.contains("rw required") && !body.contains("value=\"w\""));
        // The confirmation: missing CSRF, wrong typed id, wrong role.
        let body =
            body_string(post_form(&state, APPROVE_CONFIRM, "m", "confirm=run-x&rw_token=w").await)
                .await;
        assert!(body.contains("stale form"), "{body}");
        let resp = post_form(
            &state,
            APPROVE_CONFIRM,
            "m",
            &format!("_csrf={csrf}&confirm=run-y&rw_token=w"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK, "no run to re-review");
        let body = body_string(resp).await;
        assert!(body.contains("postgres"), "{body}");
        let resp = post_form(
            &state,
            APPROVE_CONFIRM,
            "w",
            &format!("_csrf={}&confirm=run-x&rw_token=w", csrf_for(&state, "w")),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        assert_eq!(resp.headers()["location"], "/admin/login");
    }

    /// R7: the console approval obtains a heavy admission slot on the REST
    /// plane, like `POST /v1/runs/{run_id}/steps/{ordinal}/approve`, and is
    /// shed with the overloaded notice (503, `Retry-After: 1`) when there is
    /// none — whether the heavy bound or the ordinary pool is full. The
    /// control reserve is never used. Every refusal that precedes admission
    /// (CSRF, typed id, rw credential) costs no slot.
    #[tokio::test]
    async fn console_approval_is_admitted_as_heavy_work() {
        let admission = crate::admission::AdmissionConfig::new(4, 1, 1).unwrap();
        let state = AppState::new_with_admission(test_config(static_auth()), admission)
            .await
            .expect("state");
        let csrf = csrf_for(&state, "m");
        let good = format!("_csrf={csrf}&confirm=run-x&rw_token=w");
        let shed_metric = r#"munarium_admission_shed_total{plane="rest",class="heavy"}"#;
        let admitted_metric =
            r#"munarium_admission_total{plane="rest",class="heavy",slot="ordinary"}"#;
        let metric = |series: &str| -> u64 {
            crate::metrics::render(&state)
                .lines()
                .find_map(|l| l.strip_prefix(series)?.strip_prefix(' ')?.parse().ok())
                .unwrap_or(0)
        };

        // The heavy bound is full: shed, nothing read, nothing approved.
        let heavy = state.admission.rest.heavy.as_ref().unwrap().clone();
        let held = heavy.clone().try_acquire_owned().unwrap();
        let resp = post_form(&state, APPROVE_CONFIRM, "m", &good).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(resp.headers()["retry-after"], "1");
        let body = body_string(resp).await;
        assert!(body.contains("overloaded"), "{body}");
        assert!(body.contains("Nothing was approved"), "{body}");
        assert!(!body.contains("postgres"), "shed before the store: {body}");
        assert_eq!(metric(shed_metric), 1);
        // A refusal before admission takes no slot and sheds nothing.
        let body = body_string(
            post_form(
                &state,
                APPROVE_CONFIRM,
                "m",
                &format!("_csrf={csrf}&confirm=run-x&rw_token=w2"),
            )
            .await,
        )
        .await;
        assert!(body.contains("different tenant"), "{body}");
        assert_eq!(metric(shed_metric), 1);
        drop(held);

        // The ordinary pool is full while the reserve is free: still shed —
        // heavy work never takes the control reserve.
        let n = state.admission.rest.ordinary.available_permits() as u32;
        let ordinary = state
            .admission
            .rest
            .ordinary
            .clone()
            .try_acquire_many_owned(n)
            .unwrap();
        let resp = post_form(&state, APPROVE_CONFIRM, "m", &good).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(state.admission.rest.reserve.available_permits(), 1);
        assert_eq!(heavy.available_permits(), 1, "the shed released its token");
        assert_eq!(metric(shed_metric), 2);
        drop(ordinary);

        // Room everywhere: admitted as heavy (the memory store then refuses
        // the run read), and every permit is returned afterwards.
        let body = body_string(post_form(&state, APPROVE_CONFIRM, "m", &good).await).await;
        assert!(body.contains("postgres"), "{body}");
        assert_eq!(metric(admitted_metric), 1);
        assert_eq!(heavy.available_permits(), 1);
        assert_eq!(state.admission.rest.ordinary.available_permits(), 3);
        // The review pages are reads and are never admitted or shed.
        let held = heavy.clone().try_acquire_owned().unwrap();
        let resp = get(
            &state,
            "/admin/runs/run-x/steps/0/approve",
            Some("m"),
            false,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        drop(held);
        assert_eq!(metric(shed_metric), 2);
    }

    /// Only the listed console route is admitted; anything else is refused
    /// rather than silently admitted, and every listed route is served.
    #[tokio::test]
    async fn console_heavy_routes_are_served_and_unlisted_routes_are_refused() {
        let state = state(static_auth()).await;
        assert!(crate::admission::admit_console(&state, "POST", "/admin/tokens/issue").is_none());
        assert!(
            crate::admission::admit_console(&state, "GET", runbooks::APPROVE_CONFIRM_ROUTE)
                .is_none()
        );
        assert!(
            crate::admission::admit_console(&state, "POST", runbooks::APPROVE_CONFIRM_ROUTE)
                .is_some()
        );
        for (method, template) in crate::admission::CONSOLE_HEAVY {
            let path = template
                .replace("{run_id}", "run-x")
                .replace("{ordinal}", "0");
            let resp = crate::rest::router(state.clone())
                .oneshot(
                    axum::http::Request::builder()
                        .method(*method)
                        .uri(&path)
                        .header("authorization", "Bearer m")
                        .header("content-type", "application/x-www-form-urlencoded")
                        .body(axum::body::Body::from("_csrf=x"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::OK,
                "{method} {path} is not served"
            );
            assert!(body_string(resp).await.contains("stale form"));
        }
    }

    // -----------------------------------------------------------------------
    // U4: token revocation is reviewed, then confirmed by typing the jti
    // -----------------------------------------------------------------------

    async fn post_form_view_only(
        state: &Arc<AppState>,
        path: &str,
        bearer: &str,
        form: &str,
    ) -> Response {
        crate::rest::router(state.clone())
            .oneshot(
                axum::http::Request::post(path)
                    .header("authorization", format!("Bearer {bearer}"))
                    .header(VIEW_ONLY_HEADER, "1")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(axum::body::Body::from(form.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    fn plan_in(body: &str) -> Option<String> {
        let start = body.find(r#"name="plan" value=""#)? + r#"name="plan" value=""#.len();
        let len = body[start..].find('"')?;
        Some(body[start..start + len].to_string())
    }

    /// Without a store the review cannot load its target, so it offers no
    /// confirmation; every refusal that precedes the store read still
    /// refuses. (The acting path is exercised against PostgreSQL below.)
    #[tokio::test]
    async fn revoke_review_and_refusals_on_the_memory_store() {
        let state = state(static_auth()).await;
        let csrf = csrf_for(&state, "m");
        // The review is a read and offers nothing it cannot back.
        let resp = get(&state, "/admin/tokens/tok-x/revoke", Some("m"), false).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(body.contains("postgres"), "{body}");
        assert!(plan_in(&body).is_none(), "{body}");
        // The v1.4 POST still demands the CSRF token.
        let body = body_string(
            post_form(&state, "/admin/tokens/tok-x/revoke", "m", "_csrf=deadbeef").await,
        )
        .await;
        assert!(body.contains("stale form"), "{body}");
        // The confirmation: missing CSRF, wrong role, view-only.
        let form = "plan=00&confirm=tok-x";
        let body =
            body_string(post_form(&state, "/admin/tokens/tok-x/revoke/confirm", "m", form).await)
                .await;
        assert!(body.contains("stale form"), "{body}");
        let resp = post_form(
            &state,
            "/admin/tokens/tok-x/revoke/confirm",
            "w",
            &format!("_csrf={}&{form}", csrf_for(&state, "w")),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        assert_eq!(resp.headers()["location"], "/admin/login");
        let body = body_string(
            post_form_view_only(
                &state,
                "/admin/tokens/tok-x/revoke/confirm",
                "m",
                &format!("_csrf={csrf}&{form}"),
            )
            .await,
        )
        .await;
        assert!(body.contains("view-only passthrough"), "{body}");
        let body = body_string(
            post_form_view_only(
                &state,
                "/admin/tokens/tok-x/revoke",
                "m",
                &format!("_csrf={csrf}"),
            )
            .await,
        )
        .await;
        assert!(body.contains("view-only passthrough"), "{body}");
    }

    async fn revoked_at(state: &AppState, tenant: &str, jti: &str) -> Option<String> {
        crate::reports_api::op_get_token(state, tenant, jti)
            .await
            .expect("token row")
            .revoked_at
    }

    async fn issue(state: &AppState, tenant: &str, uid: &str) -> String {
        crate::tokens_api::op_issue_token(
            state,
            "console-test",
            tenant,
            munarium_api_types::IssueTokenRequest {
                uid: uid.into(),
                access_level: 1,
                compartments: vec!["finance".into()],
                scopes: vec!["query".into()],
                runbook_refs: None,
                ttl_secs: Some(600),
            },
        )
        .await
        .expect("issue")
        .jti
    }

    /// The whole U4 contract against PostgreSQL: the preview never revokes;
    /// a wrong identifier, missing or foreign CSRF token, wrong role,
    /// view-only header, another operator's plan, a target changed after
    /// review, or an already-revoked target never acts; the correct
    /// confirmation revokes exactly once; a duplicate is harmless; and the
    /// `/v1` route is unchanged.
    #[tokio::test]
    async fn token_revocation_is_reviewed_then_confirmed_on_postgres() {
        let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
            eprintln!("console U4 PostgreSQL test NOT RUN: MUNARIUM_TEST_DATABASE_URL unset");
            return;
        };
        let tenant = format!("console-u4-{}", uuid::Uuid::new_v4().simple());
        let other = format!("{tenant}-other");
        let mut cfg = test_config(AuthMode::Static(vec![
            ("m".into(), tenant.clone(), "mgmt".into()),
            ("m2".into(), tenant.clone(), "mgmt".into()),
            ("w".into(), tenant.clone(), "rw".into()),
            ("mo".into(), other.clone(), "mgmt".into()),
        ]));
        cfg.store = StoreKind::Postgres;
        cfg.database_url = Some(url);
        let state = AppState::new(cfg).await.expect("state");
        let csrf = csrf_for(&state, "m");
        let jti = issue(&state, &tenant, "alice").await;
        let review = format!("/admin/tokens/{jti}/revoke");
        let confirm = format!("{review}/confirm");

        // The list links to the review; the review states the target and the
        // consequence and carries a plan. Refreshing it acts on nothing.
        let body = body_string(get(&state, "/admin/tokens", Some("m"), false).await).await;
        assert!(body.contains(&format!(r#"action="{review}""#)), "{body}");
        let resp = get(&state, &review, Some("m"), false).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(body.contains(&jti) && body.contains("alice"), "{body}");
        assert!(body.contains("NOT enforced at verify time"), "{body}");
        let plan = plan_in(&body).expect("confirmation offered");
        let again = body_string(get(&state, &review, Some("m"), false).await).await;
        assert_eq!(plan_in(&again).as_deref(), Some(plan.as_str()));
        // The v1.4 POST is now a preview too.
        let body =
            body_string(post_form(&state, &review, "m", &format!("_csrf={csrf}")).await).await;
        assert!(plan_in(&body).is_some(), "{body}");
        assert_eq!(revoked_at(&state, &tenant, &jti).await, None);

        let good = format!("_csrf={csrf}&plan={plan}&confirm={jti}");
        // Wrong identifier: the review again, with the refusal.
        let resp = post_form(
            &state,
            &confirm,
            "m",
            &format!("_csrf={csrf}&plan={plan}&confirm=tok-wrong"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body_string(resp).await.contains("does not match"));
        // Missing CSRF, and another credential's CSRF token.
        for form in [
            format!("plan={plan}&confirm={jti}"),
            format!("_csrf={}&plan={plan}&confirm={jti}", csrf_for(&state, "m2")),
        ] {
            let body = body_string(post_form(&state, &confirm, "m", &form).await).await;
            assert!(body.contains("stale form"), "{body}");
        }
        // Wrong role.
        let resp = post_form(&state, &confirm, "w", &good).await;
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        assert_eq!(resp.headers()["location"], "/admin/login");
        // View-only.
        let body = body_string(post_form_view_only(&state, &confirm, "m", &good).await).await;
        assert!(body.contains("view-only passthrough"), "{body}");
        // Another operator presenting this operator's plan, with its own
        // valid CSRF token: the plan is bound to the reviewer.
        let resp = post_form(
            &state,
            &confirm,
            "m2",
            &format!("_csrf={}&plan={plan}&confirm={jti}", csrf_for(&state, "m2")),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        // A forged plan.
        let resp = post_form(
            &state,
            &confirm,
            "m",
            &format!("_csrf={csrf}&plan={}&confirm={jti}", "0".repeat(64)),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        // Another tenant's operator cannot even see the target.
        let resp = get(&state, &review, Some("mo"), false).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let resp = post_form(
            &state,
            &confirm,
            "mo",
            &format!("_csrf={}&plan={plan}&confirm={jti}", csrf_for(&state, "mo")),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(revoked_at(&state, &tenant, &jti).await, None);

        // The correct confirmation revokes, once.
        let resp = post_form(&state, &confirm, "m", &good).await;
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        assert_eq!(resp.headers()["location"], "/admin/tokens");
        let first = revoked_at(&state, &tenant, &jti).await.expect("revoked");
        // A duplicate submission (a double click, a refresh of the POST)
        // changes nothing and says so.
        let resp = post_form(&state, &confirm, "m", &good).await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert!(body_string(resp).await.contains("already revoked"));
        assert_eq!(revoked_at(&state, &tenant, &jti).await, Some(first));
        // An empty plan offers no confirmation.
        let body = body_string(get(&state, &review, Some("m"), false).await).await;
        assert!(body.contains("nothing to confirm"), "{body}");
        assert!(plan_in(&body).is_none(), "{body}");

        // A target changed between preview and confirmation.
        let changed = issue(&state, &tenant, "bob").await;
        let body = body_string(
            get(
                &state,
                &format!("/admin/tokens/{changed}/revoke"),
                Some("m"),
                false,
            )
            .await,
        )
        .await;
        let plan = plan_in(&body).expect("plan");
        sqlx::query(
            "UPDATE access_tokens SET expires_at = expires_at + interval '1 hour'
              WHERE tenant_id = $1 AND jti = $2",
        )
        .bind(&tenant)
        .bind(&changed)
        .execute(state.pg_pool().expect("pool"))
        .await
        .expect("update");
        let resp = post_form(
            &state,
            &format!("/admin/tokens/{changed}/revoke/confirm"),
            "m",
            &format!("_csrf={csrf}&plan={plan}&confirm={changed}"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert!(body_string(resp).await.contains("no longer matches"));
        assert_eq!(revoked_at(&state, &tenant, &changed).await, None);

        // Revoked through /v1 after review: the confirmation does not act,
        // and the /v1 route keeps its v1.4 behaviour (200, idempotent).
        let body = body_string(
            get(
                &state,
                &format!("/admin/tokens/{changed}/revoke"),
                Some("m"),
                false,
            )
            .await,
        )
        .await;
        let plan = plan_in(&body).expect("plan");
        for _ in 0..2 {
            let resp = crate::rest::router(state.clone())
                .oneshot(
                    axum::http::Request::post(format!("/v1/access-tokens/{changed}/revoke"))
                        .header("authorization", "Bearer m")
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            assert!(body_string(resp).await.contains(r#""revoked":true"#));
        }
        let v1_time = revoked_at(&state, &tenant, &changed)
            .await
            .expect("revoked");
        let resp = post_form(
            &state,
            &format!("/admin/tokens/{changed}/revoke/confirm"),
            "m",
            &format!("_csrf={csrf}&plan={plan}&confirm={changed}"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert_eq!(revoked_at(&state, &tenant, &changed).await, Some(v1_time));
    }

    const APPROVAL_SHAPE: &str = "apiVersion: munarium.ioka.io/v1\nkind: Shape\nmetadata: {name: approval, version: 1}\nspec:\n  fact:\n    schema: {type: object}\n";
    const APPROVAL_BOOK: &str = "apiVersion: munarium.ioka.io/v1\nkind: Runbook\nmetadata: {name: approval, version: 1}\nspec:\n  shape: approval@1\n  steps:\n    - resolveSources: {}\n    - buildIndex: {}\n    - verify: {}\n    - cutover: {approval: required}\n    - retireOld: {keep_versions: 2}\n";

    /// A fresh run of the fictional approval runbook, stopped at step 3.
    async fn awaiting_run(state: &AppState, tenant: &str) -> String {
        let store = state.store_for(tenant).await.expect("store");
        let version = store.create_version(None, None).await.expect("version");
        let (run, status) =
            crate::runbooks_api::op_run_runbook(state, tenant, "approval@1", Some(&version))
                .await
                .expect("run");
        assert_eq!(status, "awaiting_approval");
        run
    }

    async fn step_state(state: &AppState, tenant: &str, run: &str, ordinal: usize) -> String {
        crate::runbooks_api::op_get_run(state, tenant, run)
            .await
            .expect("run")
            .steps[ordinal]
            .state
            .clone()
    }

    /// U4 and R7 for run-step approval against PostgreSQL: the review and the
    /// v1.4 POST never approve; a wrong typed id, a missing or foreign rw
    /// credential, another operator's or a forged plan, another tenant, a
    /// step changed after review, and a full heavy bound never approve; the
    /// correct confirmation approves exactly once; a duplicate, or one
    /// arriving after `/v1` approved the step, changes nothing.
    #[tokio::test]
    async fn run_step_approval_is_reviewed_admitted_then_confirmed_on_postgres() {
        let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
            eprintln!("console approval PostgreSQL test NOT RUN: MUNARIUM_TEST_DATABASE_URL unset");
            return;
        };
        let tenant = format!("console-approve-{}", uuid::Uuid::new_v4().simple());
        let other = format!("{tenant}-other");
        let mut cfg = test_config(AuthMode::Static(vec![
            ("m".into(), tenant.clone(), "mgmt".into()),
            ("m2".into(), tenant.clone(), "mgmt".into()),
            ("w".into(), tenant.clone(), "rw".into()),
            ("mo".into(), other.clone(), "mgmt".into()),
            ("wo".into(), other.clone(), "rw".into()),
        ]));
        cfg.store = StoreKind::Postgres;
        cfg.database_url = Some(url);
        let admission = crate::admission::AdmissionConfig::new(4, 0, 1).unwrap();
        let state = AppState::new_with_admission(cfg, admission)
            .await
            .expect("state");
        crate::runbooks_api::op_apply_shape(&state, &tenant, APPROVAL_SHAPE, None, None)
            .await
            .expect("shape");
        state
            .retrieval_for(&tenant)
            .expect("retrieval")
            .put_source(
                "",
                "text/plain",
                "approval/notice.txt",
                Some("approval@1"),
                b"A fictional approval notice.",
            )
            .await
            .expect("source");
        crate::runbooks_api::op_apply_runbook(&state, &tenant, APPROVAL_BOOK)
            .await
            .expect("runbook");
        let run = awaiting_run(&state, &tenant).await;
        let csrf = csrf_for(&state, "m");
        let review = format!("/admin/runs/{run}/steps/3/approve");
        let confirm = format!("{review}/confirm");

        // The run page links to the review; the review states the step, what
        // runs next and the consequence, and carries a plan. Refreshing it
        // approves nothing.
        let body =
            body_string(get(&state, &format!("/admin/runs/{run}"), Some("m"), false).await).await;
        assert!(body.contains(&format!(r#"action="{review}""#)), "{body}");
        let body = body_string(get(&state, &review, Some("m"), false).await).await;
        assert!(
            body.contains("cutover") && body.contains("retireOld"),
            "{body}"
        );
        assert!(body.contains("rw</strong> operation"), "{body}");
        let plan = plan_in(&body).expect("confirmation offered");
        let again = body_string(get(&state, &review, Some("m"), false).await).await;
        assert_eq!(plan_in(&again).as_deref(), Some(plan.as_str()));
        // The v1.4 POST, even carrying a valid rw token, is a review.
        let body =
            body_string(post_form(&state, &review, "m", &format!("_csrf={csrf}&rw_token=w")).await)
                .await;
        assert!(plan_in(&body).is_some(), "{body}");
        assert_eq!(
            step_state(&state, &tenant, &run, 3).await,
            "awaiting_approval"
        );

        let good = format!("_csrf={csrf}&plan={plan}&confirm={run}&rw_token=w");
        // Wrong typed id.
        let resp = post_form(
            &state,
            &confirm,
            "m",
            &format!("_csrf={csrf}&plan={plan}&confirm=run-wrong&rw_token=w"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body_string(resp).await.contains("does not match"));
        // No rw credential, the mgmt token as rw, another tenant's rw token.
        for (rw, why) in [
            ("", "rw credential"),
            ("m", "rw required"),
            ("wo", "different tenant"),
        ] {
            let body = body_string(
                post_form(
                    &state,
                    &confirm,
                    "m",
                    &format!("_csrf={csrf}&plan={plan}&confirm={run}&rw_token={rw}"),
                )
                .await,
            )
            .await;
            assert!(body.contains(why), "{rw}: {body}");
        }
        // Wrong role and view-only.
        let resp = post_form(&state, &confirm, "w", &good).await;
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        let body = body_string(post_form_view_only(&state, &confirm, "m", &good).await).await;
        assert!(body.contains("view-only passthrough"), "{body}");
        // Another operator presenting this operator's plan, and a forged one.
        let resp = post_form(
            &state,
            &confirm,
            "m2",
            &format!(
                "_csrf={}&plan={plan}&confirm={run}&rw_token=w",
                csrf_for(&state, "m2")
            ),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let resp = post_form(
            &state,
            &confirm,
            "m",
            &format!(
                "_csrf={csrf}&plan={}&confirm={run}&rw_token=w",
                "0".repeat(64)
            ),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        // Another tenant's operator cannot see the run or act on it.
        let resp = get(&state, &review, Some("mo"), false).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let resp = post_form(
            &state,
            &confirm,
            "mo",
            &format!(
                "_csrf={}&plan={plan}&confirm={run}&rw_token=wo",
                csrf_for(&state, "mo")
            ),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        // R7: no heavy slot, no approval.
        let heavy = state.admission.rest.heavy.as_ref().unwrap().clone();
        let held = heavy.clone().try_acquire_owned().unwrap();
        let resp = post_form(&state, &confirm, "m", &good).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(resp.headers()["retry-after"], "1");
        drop(held);
        assert_eq!(
            step_state(&state, &tenant, &run, 3).await,
            "awaiting_approval"
        );

        // The correct confirmation approves, once, and the run resumes.
        let resp = post_form(&state, &confirm, "m", &good).await;
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        assert_eq!(resp.headers()["location"], format!("/admin/runs/{run}"));
        assert_eq!(step_state(&state, &tenant, &run, 3).await, "done");
        let resp = post_form(&state, &confirm, "m", &good).await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert!(body_string(resp).await.contains("not awaiting approval"));
        let body = body_string(get(&state, &review, Some("m"), false).await).await;
        assert!(
            body.contains("nothing to confirm") && plan_in(&body).is_none(),
            "{body}"
        );

        // A step changed between review and confirmation.
        let changed = awaiting_run(&state, &tenant).await;
        let review = format!("/admin/runs/{changed}/steps/3/approve");
        let plan = plan_in(&body_string(get(&state, &review, Some("m"), false).await).await)
            .expect("plan");
        sqlx::query(
            "UPDATE runbook_steps SET detail = '{\"note\":\"changed\"}'::jsonb
              WHERE tenant_id = $1 AND run_id = $2 AND ordinal = 3",
        )
        .bind(&tenant)
        .bind(&changed)
        .execute(state.pg_pool().expect("pool"))
        .await
        .expect("update");
        let resp = post_form(
            &state,
            &format!("{review}/confirm"),
            "m",
            &format!("_csrf={csrf}&plan={plan}&confirm={changed}&rw_token=w"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert!(body_string(resp).await.contains("no longer matches"));
        assert_eq!(
            step_state(&state, &tenant, &changed, 3).await,
            "awaiting_approval"
        );

        // Approved through /v1 after review: the confirmation does not act.
        let plan = plan_in(&body_string(get(&state, &review, Some("m"), false).await).await)
            .expect("plan");
        let resp = crate::rest::router(state.clone())
            .oneshot(
                axum::http::Request::post(format!("/v1/runs/{changed}/steps/3/approve"))
                    .header("authorization", "Bearer w")
                    .header("x-munarium-uid", "console-test")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let resp = post_form(
            &state,
            &format!("{review}/confirm"),
            "m",
            &format!("_csrf={csrf}&plan={plan}&confirm={changed}&rw_token=w"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert_eq!(step_state(&state, &tenant, &changed, 3).await, "done");
    }

    // -----------------------------------------------------------------------
    // U8: bounded lists
    // -----------------------------------------------------------------------

    #[test]
    fn console_list_bounds_and_cursors() {
        use crate::reports_api::{console_page_limit, keyset_page, parse_keyset};
        assert_eq!(console_page_limit(None), 50);
        assert_eq!(console_page_limit(Some("")), 50);
        assert_eq!(console_page_limit(Some("x")), 50);
        assert_eq!(console_page_limit(Some("0")), 1);
        assert_eq!(console_page_limit(Some("-5")), 1);
        assert_eq!(console_page_limit(Some("7")), 7);
        assert_eq!(console_page_limit(Some("100000")), 200);
        // Cursors: absent is the first page; malformed is caller error.
        assert!(parse_keyset(None).unwrap().is_none());
        assert!(parse_keyset(Some(" ")).unwrap().is_none());
        for bad in ["2026-01-01", "nope~tok-1", "2026-01-01T00:00:00Z~", "~tok"] {
            assert!(
                matches!(parse_keyset(Some(bad)), Err(KernelError::InvalidInput(_))),
                "{bad}"
            );
        }
        let (ts, key) = parse_keyset(Some("2026-01-01T00:00:00.000123Z~tok-1"))
            .unwrap()
            .unwrap();
        assert_eq!(ts.timestamp_subsec_micros(), 123);
        assert_eq!(key, "tok-1");
        // A full page plus one: trimmed, cursor from the last row kept.
        let rows = (0..4)
            .map(|i| (i, format!("ts{i}"), format!("k{i}")))
            .collect::<Vec<_>>();
        let page = keyset_page(rows.clone(), 3, 9);
        assert_eq!(page.rows, vec![0, 1, 2]);
        assert_eq!(page.total, 9);
        assert_eq!(page.next_before.as_deref(), Some("ts2~k2"));
        // Exactly the limit: no further page, so no cursor.
        let page = keyset_page(rows[..3].to_vec(), 3, 3);
        assert_eq!(page.rows.len(), 3);
        assert!(page.next_before.is_none());
    }

    /// The "older" link of a rendered list page, unescaped, if any.
    fn older_href(body: &str) -> Option<String> {
        let end = body.find(r#"">older →</a>"#)?;
        let start = body[..end].rfind(r#"href=""#)? + r#"href=""#.len();
        Some(body[start..end].replace("&amp;", "&"))
    }

    /// Walk a list from its first page through every "older" link, checking
    /// each page's bound and count, and return each page's row keys in order.
    async fn walk_pages(
        state: &Arc<AppState>,
        first: &str,
        limit: usize,
        total: usize,
        row_key: &dyn Fn(&str) -> Vec<String>,
    ) -> Vec<String> {
        let mut seen = Vec::new();
        let mut next = Some(first.to_string());
        let mut pages = 0;
        while let Some(url) = next {
            pages += 1;
            assert!(pages <= total + 1, "pagination does not terminate");
            let resp = get(state, &url, Some("m"), false).await;
            assert_eq!(resp.status(), StatusCode::OK, "{url}");
            let body = body_string(resp).await;
            let keys = row_key(&body);
            assert!(keys.len() <= limit, "{url}: {} rows", keys.len());
            assert!(
                body.contains(&format!("showing {} of {total}", keys.len())),
                "{url}: {body}"
            );
            if pages > 1 {
                assert!(body.contains("← newest</a>"), "{url}: {body}");
            }
            let shown = keys.len();
            seen.extend(keys);
            next = older_href(&body);
            if next.is_none() {
                assert_eq!(seen.len(), total, "last page {url}");
            } else {
                assert_eq!(shown, limit, "a page before the last is full: {url}");
            }
        }
        seen
    }

    /// Every `<code>…</code>` cell whose text starts with `prefix`.
    fn codes(body: &str, prefix: &str) -> Vec<String> {
        body.split("<code>")
            .skip(1)
            .filter_map(|c| c.split_once("</code>").map(|(v, _)| v.to_string()))
            .filter(|v| v.starts_with(prefix))
            .collect()
    }

    #[tokio::test]
    async fn token_session_and_finding_lists_are_bounded_on_postgres() {
        let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
            eprintln!("console U8 PostgreSQL test NOT RUN: MUNARIUM_TEST_DATABASE_URL unset");
            return;
        };
        let tenant = format!("console-u8-{}", uuid::Uuid::new_v4().simple());
        let mut cfg = test_config(AuthMode::Static(vec![(
            "m".into(),
            tenant.clone(),
            "mgmt".into(),
        )]));
        cfg.store = StoreKind::Postgres;
        cfg.database_url = Some(url);
        let state = AppState::new(cfg).await.expect("state");
        let pool = state.pg_pool().expect("pool").clone();

        // Tokens: seven, three sharing one issued_at so the jti tiebreaker
        // is exercised at a page boundary.
        let mut jtis = Vec::new();
        for i in 0..7 {
            jtis.push(issue(&state, &tenant, &format!("user{i}")).await);
        }
        sqlx::query(
            "UPDATE access_tokens SET issued_at = '2026-01-01T00:00:00.5Z'
              WHERE tenant_id = $1 AND jti = ANY($2)",
        )
        .bind(&tenant)
        .bind(&jtis[1..4])
        .execute(&pool)
        .await
        .unwrap();
        let tokens = walk_pages(&state, "/admin/tokens?limit=2", 2, 7, &|b: &str| {
            codes(b, "tok-")
        })
        .await;
        let mut sorted = tokens.clone();
        sorted.sort();
        sorted.dedup();
        let mut expected = jtis.clone();
        expected.sort();
        assert_eq!(sorted, expected, "every token exactly once: {tokens:?}");
        // The uid filter carries through the count and the links.
        let body =
            body_string(get(&state, "/admin/tokens?uid=user3", Some("m"), false).await).await;
        assert!(body.contains("showing 1 of 1"), "{body}");
        // The bound holds without a limit parameter, and a huge one clamps.
        let body = body_string(get(&state, "/admin/tokens", Some("m"), false).await).await;
        assert!(body.contains("showing 7 of 7"), "{body}");
        assert!(older_href(&body).is_none());
        let body =
            body_string(get(&state, "/admin/tokens?limit=99999", Some("m"), false).await).await;
        assert!(body.contains("showing 7 of 7"), "{body}");
        // A malformed cursor is a caller error on the page, never a 500.
        let resp = get(&state, "/admin/tokens?before=garbage", Some("m"), false).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(body_string(resp).await.contains("before:"));

        // Sessions: five, two sharing their activity time.
        let mut ids = Vec::new();
        for i in 0..5 {
            let id = format!("ses-u8-{i}");
            let at = if i < 2 {
                "2026-02-01T00:00:00Z".to_string()
            } else {
                format!("2026-02-0{}T00:00:00Z", i + 1)
            };
            sqlx::query(
                "INSERT INTO sessions (tenant_id, id, uid, runbook_ref, access_level, created_at)
                 VALUES ($1, $2, 'u', 'rb@1', 0, $3::timestamptz)",
            )
            .bind(&tenant)
            .bind(&id)
            .bind(&at)
            .execute(&pool)
            .await
            .unwrap();
            ids.push(id);
        }
        let session_rows = |b: &str| -> Vec<String> {
            b.split(r#"<a href="/admin/sessions/"#)
                .skip(1)
                .filter_map(|c| c.split_once('"').map(|(v, _)| v.to_string()))
                .collect()
        };
        let sessions = walk_pages(&state, "/admin/sessions?limit=2", 2, 5, &session_rows).await;
        let mut sorted = sessions.clone();
        sorted.sort();
        assert_eq!(sorted, ids, "every session exactly once: {sessions:?}");
        assert_eq!(sessions[0], "ses-u8-4", "newest first");

        // Findings: six at one recorded_at, two of them identical in every
        // displayed column, so only the tie-count cursor can page them.
        for (i, msg) in ["a", "b", "c", "dup", "dup", "e"].iter().enumerate() {
            sqlx::query(
                "INSERT INTO gate_findings
                   (tenant_id, version_id, seq, rule_id, severity, message, recorded_at)
                 VALUES ($1, 'ver-u8', $2, 'gate.u8', 'warn', $3, '2026-03-01T00:00:00Z')",
            )
            .bind(&tenant)
            .bind(if *msg == "dup" { 9 } else { i as i64 })
            .bind(*msg)
            .execute(&pool)
            .await
            .unwrap();
        }
        let finding_rows = |b: &str| -> Vec<String> {
            b.split("<tr><td>")
                .skip(1)
                .map(|row| {
                    row.split("<td>")
                        .last()
                        .and_then(|c| c.split_once("</td>"))
                        .map(|(v, _)| v.to_string())
                        .unwrap_or_default()
                })
                .collect()
        };
        let mut findings = walk_pages(&state, "/admin/findings?limit=4", 4, 6, &finding_rows).await;
        findings.sort();
        assert_eq!(findings, ["a", "b", "c", "dup", "dup", "e"]);
        // Filters narrow the count, and a page of one walks all six.
        let body =
            body_string(get(&state, "/admin/findings?severity=block", Some("m"), false).await)
                .await;
        assert!(body.contains("showing 0 of 0"), "{body}");
        let mut findings = walk_pages(
            &state,
            "/admin/findings?rule=gate.&severity=warn&limit=1",
            1,
            6,
            &finding_rows,
        )
        .await;
        findings.sort();
        assert_eq!(findings, ["a", "b", "c", "dup", "dup", "e"]);
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

    // -----------------------------------------------------------------------
    // U3: the attention strip
    // -----------------------------------------------------------------------

    /// On the memory store the table-backed checks cannot be read: they are
    /// listed as unavailable, the strip is not an all-clear, and the
    /// process-state check (datastore admission) is still observed.
    #[tokio::test]
    async fn overview_attention_on_the_memory_store_is_not_an_all_clear() {
        let state = state(static_auth()).await;
        let body = body_string(get(&state, "/admin", Some("m"), false).await).await;
        let strip_at = body.find(r#"<section class="attention""#).expect("strip");
        assert!(
            strip_at < body.find("<h2>control plane</h2>").unwrap(),
            "the strip leads the page"
        );
        assert!(
            body.contains("could not be read, so this is not an all-clear"),
            "{body}"
        );
        for what in [
            "runs awaiting gate approval",
            "blocking gate findings in the last 24 hours",
            "index build jobs failed in the last 24 hours",
            "guarded commands with an unresolved outcome",
        ] {
            assert!(
                body.contains(&format!("unavailable</span> {what}")),
                "{what}: {body}"
            );
        }
        // Observed (process state) and zero on this replica: not listed.
        assert!(!body.contains("cannot serve yet"), "{body}");
        assert!(
            !body.contains(r#"<span class="badge good">clear</span>"#),
            "{body}"
        );
    }

    /// Against PostgreSQL: each condition is an observed count for this
    /// tenant, listed with its label and link; rows outside the condition
    /// (a running run, an old finding, a leased attempt, a completed command)
    /// do not count; another tenant sees none of them and reads clear.
    #[tokio::test]
    async fn overview_attention_counts_observed_state_on_postgres() {
        let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
            eprintln!("console U3 PostgreSQL test NOT RUN: MUNARIUM_TEST_DATABASE_URL unset");
            return;
        };
        let tenant = format!("console-u3-{}", uuid::Uuid::new_v4().simple());
        let quiet = format!("{tenant}-quiet");
        let mut cfg = test_config(AuthMode::Static(vec![
            ("m".into(), tenant.clone(), "mgmt".into()),
            ("mq".into(), quiet.clone(), "mgmt".into()),
        ]));
        cfg.store = StoreKind::Postgres;
        cfg.database_url = Some(url);
        let state = AppState::new(cfg).await.expect("state");
        let pool = state.pg_pool().expect("pool").clone();
        for sql in [
            "INSERT INTO runbook_runs (tenant_id, id, runbook_ref, state) VALUES
               ($1, 'run-u3-a', 'rb@1', 'awaiting_approval'),
               ($1, 'run-u3-b', 'rb@1', 'awaiting_approval'),
               ($1, 'run-u3-c', 'rb@1', 'running')",
            "INSERT INTO gate_findings
               (tenant_id, version_id, seq, rule_id, severity, message, recorded_at) VALUES
               ($1, 'ver-u3', 1, 'gate.u3', 'block', 'm', now()),
               ($1, 'ver-u3', 2, 'gate.u3', 'block', 'm', now() - interval '2 days')",
            "INSERT INTO index_build_jobs
               (tenant_id, job_id, kind, scope_kind, scope_id, state, requested_by, updated_at)
             VALUES
               ($1, 'job-u3-a', 'backfill', 'collection', 'col-a', 'failed', 't', now()),
               ($1, 'job-u3-b', 'backfill', 'collection', 'col-b', 'failed', 't',
                now() - interval '3 days'),
               ($1, 'job-u3-c', 'backfill', 'collection', 'col-c', 'pending', 't', now())",
            "INSERT INTO index_build_attempts
               (tenant_id, attempt_id, index_version_id, artifact_plan_sha256, mode, state,
                owner_node_id, lease_expires_at) VALUES
               ($1, 'att-u3-a', 'iv-a', 'p', 'mirror', 'running', 'n',
                now() - interval '1 minute'),
               ($1, 'att-u3-b', 'iv-b', 'p', 'mirror', 'running', 'n',
                now() + interval '1 hour')",
            "INSERT INTO command_receipts
               (tenant_id, key, operation, request_hash, state, response_body, completed_at)
             VALUES
               ($1, 'k1', 'op', 'h', 'unresolved', NULL, NULL),
               ($1, 'k2', 'op', 'h', 'completed', '{}', now())",
        ] {
            sqlx::query(sql)
                .bind(&tenant)
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("{sql}: {e}"));
        }

        let body = body_string(get(&state, "/admin", Some("m"), false).await).await;
        let command_doc = format!("{DOCS_BASE}ops/command-recovery.md");
        for (n, href, what) in [
            (2, "/admin/runbooks", "runs awaiting gate approval"),
            (
                1,
                "/admin/findings?severity=block",
                "blocking gate findings in the last 24 hours",
            ),
            (
                1,
                "/admin/build-jobs",
                "index build jobs failed in the last 24 hours",
            ),
            (
                1,
                "/admin/storage",
                "index build attempts still marked running after their lease expired",
            ),
            (
                1,
                command_doc.as_str(),
                "guarded commands with an unresolved outcome (in progress, or lost and to be inspected)",
            ),
        ] {
            assert!(
                body.contains(&format!(
                    r#"{n}</span> <a href="{}">{what}</a>"#,
                    charts::esc(href)
                )),
                "{what}: {body}"
            );
        }
        assert!(!body.contains("unavailable</span>"), "{body}");

        // Another tenant sees none of it, and reads clear.
        let body = body_string(get(&state, "/admin", Some("mq"), false).await).await;
        assert!(
            body.contains(r#"<span class="badge good">clear</span>"#),
            "{body}"
        );
        assert!(body.contains("7 of 7 checks read"), "{body}");
    }

    // -----------------------------------------------------------------------
    // U6: separate facts stay separate
    // -----------------------------------------------------------------------

    /// Against PostgreSQL: the collections list and a collection's page show
    /// status, active index version, serving engine, residency on this
    /// replica and build jobs as separate values read from the tables.
    #[tokio::test]
    async fn collection_facts_render_separately_on_postgres() {
        let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
            eprintln!("console U6 PostgreSQL test NOT RUN: MUNARIUM_TEST_DATABASE_URL unset");
            return;
        };
        let tenant = format!("console-u6-{}", uuid::Uuid::new_v4().simple());
        let mut cfg = test_config(AuthMode::Static(vec![(
            "m".into(),
            tenant.clone(),
            "mgmt".into(),
        )]));
        cfg.store = StoreKind::Postgres;
        cfg.database_url = Some(url);
        let state = AppState::new(cfg).await.expect("state");
        let pool = state.pg_pool().expect("pool").clone();
        let col = format!("col-u6-{}", uuid::Uuid::new_v4().simple());
        let bare = format!("{col}-bare");
        let version = format!("iv-u6-{}", uuid::Uuid::new_v4().simple());
        let seeds: [(&str, Vec<&str>); 4] = [
            (
                "INSERT INTO collections (tenant_id, id, name, shape_ref) VALUES
                   ($1, $2, 'served', 's@1'), ($1, $3, 'bare', 's@1')",
                vec![&tenant, &col, &bare],
            ),
            (
                "INSERT INTO index_versions
                   (tenant_id, id, shape_ref, manifest, active, collection_id)
                 VALUES ($1, $2, 's@1', '{}'::jsonb, true, $3)",
                vec![&tenant, &version, &col],
            ),
            (
                "INSERT INTO retrieval_rollout
                   (tenant_id, scope_kind, scope_id, serving, changed_by)
                 VALUES ($1, 'collection', $2, 'datastore', 'test')",
                vec![&tenant, &col],
            ),
            (
                "INSERT INTO index_build_jobs
                   (tenant_id, job_id, kind, scope_kind, scope_id, state, requested_by)
                 VALUES ($1, 'job-u6-a', 'backfill', 'collection', $2, 'pending', 't'),
                        ($1, 'job-u6-b', 'backfill', 'collection', $3, 'succeeded', 't')",
                vec![&tenant, &col, &bare],
            ),
        ];
        for (sql, params) in seeds {
            let mut q = sqlx::query(sql);
            for p in params {
                q = q.bind(p);
            }
            q.execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("{sql}: {e}"));
        }
        let body = body_string(get(&state, "/admin/collections", Some("m"), false).await).await;
        for header in [
            "<th>collection status</th>",
            "<th>active index version</th>",
            "<th>serving engine</th>",
            "<th>serving artifact on this replica</th>",
            "<th>build jobs</th>",
        ] {
            assert!(body.contains(header), "{header}: {body}");
        }
        // The served collection: four facts, four cells.
        assert!(
            body.contains(&format!(
                r#"<td><span class="badge good">active</span></td><td><span class="badge good">exists</span> <code>{}</code></td><td><span class="badge">datastore</span></td><td><span class="muted">no serving artifact bound</span></td><td>0 running · 1 queued</td>"#,
                charts::esc(&short(&version, 16))
            )),
            "{body}"
        );
        // The bare one: active, but no version, default engine, no builds
        // (a finished job is not one).
        assert!(
            body.contains(
                r#"<td><span class="badge good">active</span></td><td><span class="badge warn">none</span> searches find nothing until a cutover</td><td><span class="badge">postgres</span> <span class="muted">default: no rollout row</span></td><td><span class="muted">no active version</span></td><td>0 running · 0 queued</td>"#
            ),
            "{body}"
        );
        assert!(!body.contains("unavailable"), "{body}");

        let body = body_string(
            get(
                &state,
                &format!("/admin/collections/{col}"),
                Some("m"),
                false,
            )
            .await,
        )
        .await;
        assert!(body.contains("<h2>operational state</h2>"), "{body}");
        assert!(
            body.contains(r#"(instance <code>test-instance</code>)"#),
            "{body}"
        );
        assert!(body.contains("0 running · 1 queued"), "{body}");
        assert_classes_styled("/admin/collections/{id}", &body);
        assert_status_cues_have_text("/admin/collections/{id}", &body);
    }

    // -----------------------------------------------------------------------
    // U7: complete result and the equivalent route
    // -----------------------------------------------------------------------

    #[test]
    fn equivalent_commands_are_quoted_and_escaped() {
        assert_eq!(shell_word("run-1a2b"), "run-1a2b");
        assert_eq!(shell_word("contracts@3"), "contracts@3");
        assert_eq!(shell_word("a b"), "'a b'");
        assert_eq!(shell_word("it's"), r"'it'\''s'");
        assert_eq!(shell_word("$(rm -rf /)"), "'$(rm -rf /)'");
        assert_eq!(shell_word(""), "''");
        assert_eq!(path_segment("a/b c"), "a%2Fb%20c");

        let html = complete_result(
            "GET /v1/runs/<x>",
            Some("mmctl get run '<x>'"),
            "",
            Some(&serde_json::json!({"run_id": "<x>"})),
        );
        assert!(
            html.contains(r#"<code class="cmd">GET /v1/runs/&lt;x&gt;</code>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<code class="cmd">mmctl get run &#39;&lt;x&gt;&#39;</code>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<details class="complete"><summary>complete result</summary><pre>"#),
            "{html}"
        );
        assert!(
            html.contains("&quot;run_id&quot;: &quot;&lt;x&gt;&quot;"),
            "{html}"
        );
        assert!(!html.contains("<script"), "{html}");
        // No CLI command: the note says so instead of inventing one, and a
        // page whose full result is already shown gets no disclosure.
        let html = complete_result("GET /v1/x", None, "no mmctl command reads this", None);
        assert!(html.contains(r#"<span class="muted">no mmctl command reads this</span>"#));
        assert!(!html.contains("<details"), "{html}");
        assert_classes_styled("complete_result", &html);
    }

    /// Every route and command the console prints exists: the API paths
    /// are in the served OpenAPI contract and the mmctl commands are the
    /// CLI's own usage lines.
    #[test]
    fn printed_routes_and_commands_exist() {
        let paths = crate::openapi::doc().paths.paths;
        for p in [
            "/v1/runs/{run_id}",
            "/v1/runbooks/{name}",
            "/v1/collections/{id}",
            "/v1/sessions/{id}",
            "/v1/chronology-rules/{name}",
        ] {
            assert!(paths.contains_key(p), "{p} is not in the contract");
        }
        let cli = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../munarium-cli/src/main.rs"
        ));
        for usage in [
            "usage: mmctl get run <run-id>",
            "usage: mmctl runbook info <name[@version]>",
        ] {
            assert!(cli.contains(usage), "mmctl no longer has `{usage}`");
        }
    }

    /// Against PostgreSQL: the run, session and collection pages each end
    /// with one equivalent-route section and one complete-result
    /// disclosure holding the object the API returns.
    #[tokio::test]
    async fn detail_pages_offer_the_complete_result_on_postgres() {
        let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
            eprintln!("console U7 PostgreSQL test NOT RUN: MUNARIUM_TEST_DATABASE_URL unset");
            return;
        };
        let tenant = format!("console-u7-{}", uuid::Uuid::new_v4().simple());
        let mut cfg = test_config(AuthMode::Static(vec![(
            "m".into(),
            tenant.clone(),
            "mgmt".into(),
        )]));
        cfg.store = StoreKind::Postgres;
        cfg.database_url = Some(url);
        let state = AppState::new(cfg).await.expect("state");
        let pool = state.pg_pool().expect("pool").clone();
        for sql in [
            "INSERT INTO runbook_runs (tenant_id, id, runbook_ref, state)
             VALUES ($1, 'run-u7', 'rb@1', 'awaiting_approval')",
            "INSERT INTO runbook_steps (tenant_id, run_id, ordinal, name, state)
             VALUES ($1, 'run-u7', 0, 'cutover', 'awaiting_approval')",
            "INSERT INTO sessions (tenant_id, id, uid, runbook_ref, access_level)
             VALUES ($1, 'ses-u7', 'u', 'rb@1', 0)",
            "INSERT INTO collections (tenant_id, id, name, shape_ref)
             VALUES ($1, 'col-u7', 'u7', 's@1')",
        ] {
            sqlx::query(sql)
                .bind(&tenant)
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("{sql}: {e}"));
        }
        for (path, api, cli, key) in [
            (
                "/admin/runs/run-u7",
                "GET /v1/runs/run-u7",
                Some("mmctl get run run-u7"),
                "&quot;run_id&quot;: &quot;run-u7&quot;",
            ),
            (
                "/admin/sessions/ses-u7",
                "GET /v1/sessions/ses-u7",
                None,
                "&quot;session_id&quot;: &quot;ses-u7&quot;",
            ),
            (
                "/admin/collections/col-u7",
                "GET /v1/collections/col-u7",
                None,
                "&quot;id&quot;: &quot;col-u7&quot;",
            ),
        ] {
            let resp = get(&state, path, Some("m"), false).await;
            assert_eq!(resp.status(), StatusCode::OK, "{path}");
            let body = body_string(resp).await;
            assert_eq!(
                body.matches(r#"<section class="equiv""#).count(),
                1,
                "{path}"
            );
            assert_eq!(
                body.matches(r#"<details class="complete">"#).count(),
                1,
                "{path}"
            );
            assert!(
                body.contains(&format!(r#"<code class="cmd">{api}</code>"#)),
                "{path}: {body}"
            );
            match cli {
                Some(cmd) => assert!(
                    body.contains(&format!(r#"<code class="cmd">{cmd}</code>"#)),
                    "{path}: {body}"
                ),
                None => assert!(body.contains("no mmctl command reads"), "{path}"),
            }
            let disclosure = &body[body.find(r#"<details class="complete">"#).unwrap()..];
            assert!(disclosure.contains(key), "{path}: {disclosure}");
            assert!(!body.contains("<script"), "{path}");
            assert_classes_styled(path, &body);
        }
    }

    // -----------------------------------------------------------------------
    // U14: the pages as pages
    // -----------------------------------------------------------------------

    /// Every page that renders without a database, as an operator and
    /// behind a view-only proxy: one h1, a language, labelled controls,
    /// CSRF on every post form, no script or positive tabindex, and focus
    /// that moves skip link → navigation → content.
    #[tokio::test]
    async fn every_page_is_labelled_and_keyboard_operable() {
        let state = state(static_auth()).await;
        for path in memory_pages() {
            for view_only in [false, true] {
                let resp = get(&state, &path, Some("m"), view_only).await;
                let body = body_string(resp).await;
                html_checks::assert_operable(&path, &body);
                if view_only && path != "/admin/login" {
                    // A view-only proxy is offered no form that posts.
                    assert!(
                        !html_checks::tags(&body)
                            .iter()
                            .any(|t| t.name == "form" && t.attr("method") == Some("post")),
                        "{path}: post form behind view-only"
                    );
                }
            }
        }
    }

    /// The anchors the registry declares (`<a id="…">`).
    fn registry_ids() -> std::collections::BTreeSet<String> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs")
            .join(ERRORS_DOC);
        let text = std::fs::read_to_string(&path).expect("errors.md");
        text.match_indices("<a id=\"")
            .filter_map(|(i, _)| {
                let rest = &text[i + 7..];
                rest.find('"').map(|end| rest[..end].to_string())
            })
            .collect()
    }

    /// Every registry anchor a page links to.
    fn registry_links(body: &str) -> Vec<String> {
        let needle = format!("{DOCS_BASE}{ERRORS_DOC}#");
        body.match_indices(&needle)
            .filter_map(|(i, _)| {
                let rest = &body[i + needle.len()..];
                rest.find('"').map(|end| rest[..end].to_string())
            })
            .collect()
    }

    /// U11: every problem slug a page shows links to its row in the error
    /// registry, at an anchor the registry has. A whole-page failure (the
    /// missing shape) names `not-found`; a section that needs PostgreSQL
    /// names the slug its API read returns.
    #[tokio::test]
    async fn every_problem_slug_links_to_a_registry_anchor_that_exists() {
        let ids = registry_ids();
        assert!(ids.contains("problem-not-found"), "{ids:?}");
        let state = state(static_auth()).await;
        let mut linked = std::collections::BTreeSet::new();
        for path in memory_pages() {
            for view_only in [false, true] {
                let body = body_string(get(&state, &path, Some("m"), view_only).await).await;
                for anchor in registry_links(&body) {
                    assert!(
                        ids.contains(&anchor),
                        "{path}: #{anchor} is not in errors.md"
                    );
                    linked.insert(anchor);
                }
            }
        }
        let body = body_string(get(&state, "/admin/shapes/nope@9", Some("m"), false).await).await;
        assert!(
            body.contains(&format!(
                r#"· problem <a href="{DOCS_BASE}api/errors.md#problem-not-found" aria-label="not-found in the error registry"><code>not-found</code></a></div>"#
            )),
            "{body}"
        );
        // The memory store's section notes name the API's slug too.
        assert!(linked.contains("problem-invalid-input"), "{linked:?}");
        // A slug the registry lacks would fail the check above.
        assert!(!ids.contains("problem-no-such-slug"));
    }

    /// U11 (empty-deployment landing): on the memory store the
    /// control-plane counts cannot be read, so the overview shows the tiles
    /// with the failed read named, never the empty landing (U5).
    #[tokio::test]
    async fn a_failed_overview_read_never_shows_the_empty_landing() {
        let state = state(static_auth()).await;
        let body = body_string(get(&state, "/admin", Some("m"), false).await).await;
        assert!(!body.contains("Nothing is deployed"), "{body}");
        for t in FIRST_TASKS {
            assert!(!body.contains(t.title), "{body}");
        }
        assert!(body.contains(r#"<div class="tiles">"#), "{body}");
        assert!(body.contains("#problem-invalid-input"), "{body}");
    }

    /// Navigation by role: every console page, list or detail, is served to
    /// the mgmt role only. No credential, a non-mgmt role and an unknown
    /// token all land on the login page; nothing is rendered for them.
    #[tokio::test]
    async fn every_page_refuses_roles_other_than_mgmt() {
        let state = state(static_auth()).await;
        let mut paths: Vec<String> = memory_pages()
            .into_iter()
            .filter(|p| p != "/admin/login")
            .collect();
        paths.extend(
            [
                "/admin/runs/run-x",
                "/admin/sessions/ses-x",
                "/admin/collections/col-x",
                "/admin/runbooks/rb",
                "/admin/chronology-rules/r",
            ]
            .map(String::from),
        );
        for path in &paths {
            for who in [None, Some("w"), Some("w2"), Some("nope")] {
                let resp = get(&state, path, who, false).await;
                assert_eq!(resp.status(), StatusCode::SEE_OTHER, "{path} as {who:?}");
                assert_eq!(resp.headers()["location"], "/admin/login", "{path}");
            }
            let resp = get(&state, path, Some("m"), false).await;
            assert_ne!(resp.status(), StatusCode::SEE_OTHER, "{path} as mgmt");
        }
    }

    /// Against PostgreSQL, the pages that carry the console's actions: the
    /// token list, the revocation review with its typed confirmation, the
    /// issue page and a run awaiting approval. Each passes the same checks,
    /// and the confirmation form is fully labelled, CSRF-bound and leaves a
    /// way back that changes nothing.
    #[tokio::test]
    async fn action_pages_are_labelled_and_keyboard_operable_on_postgres() {
        let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
            eprintln!("console U14 PostgreSQL test NOT RUN: MUNARIUM_TEST_DATABASE_URL unset");
            return;
        };
        let tenant = format!("console-u14-{}", uuid::Uuid::new_v4().simple());
        let mut cfg = test_config(AuthMode::Static(vec![(
            "m".into(),
            tenant.clone(),
            "mgmt".into(),
        )]));
        cfg.store = StoreKind::Postgres;
        cfg.database_url = Some(url);
        let state = AppState::new(cfg).await.expect("state");
        let pool = state.pg_pool().expect("pool").clone();
        let jti = issue(&state, &tenant, "alice").await;
        for sql in [
            "INSERT INTO runbook_runs (tenant_id, id, runbook_ref, state)
             VALUES ($1, 'run-u14', 'rb@1', 'awaiting_approval')",
            "INSERT INTO runbook_steps (tenant_id, run_id, ordinal, name, state)
             VALUES ($1, 'run-u14', 0, 'cutover', 'awaiting_approval')",
            "INSERT INTO sessions (tenant_id, id, uid, runbook_ref, access_level)
             VALUES ($1, 'ses-u14', 'u', 'rb@1', 0)",
            "INSERT INTO collections (tenant_id, id, name, shape_ref)
             VALUES ($1, 'col-u14', 'u14', 's@1')",
        ] {
            sqlx::query(sql)
                .bind(&tenant)
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("{sql}: {e}"));
        }
        let review = format!("/admin/tokens/{jti}/revoke");
        for path in [
            "/admin",
            "/admin/tokens",
            "/admin/tokens/issue",
            review.as_str(),
            "/admin/runs/run-u14",
            "/admin/runs/run-u14/steps/0/approve",
            "/admin/sessions",
            "/admin/sessions/ses-u14",
            "/admin/collections",
            "/admin/collections/col-u14",
            "/admin/findings",
            "/admin/storage",
        ] {
            for view_only in [false, true] {
                let body = body_string(get(&state, path, Some("m"), view_only).await).await;
                html_checks::assert_operable(path, &body);
                assert_classes_styled(path, &body);
                assert_status_cues_have_text(path, &body);
            }
        }

        // The list's revoke control names its target for assistive tech.
        let body = body_string(get(&state, "/admin/tokens", Some("m"), false).await).await;
        assert!(
            body.contains(&format!(r#"aria-label="review revoking token {jti}""#)),
            "{body}"
        );
        // The confirmation: inside one post form, the CSRF token, the plan,
        // a labelled required field for the typed jti and a named danger
        // button; after it, a link back that changes nothing.
        let body = body_string(get(&state, &review, Some("m"), false).await).await;
        assert_confirmation_form(
            &body,
            &[],
            r#"<a href="/admin/tokens">← back to tokens (nothing changes)</a>"#,
        );
        // The run page links each awaiting step to its review (U4), and the
        // review's confirmation names its credential field and the typed id.
        let body = body_string(get(&state, "/admin/runs/run-u14", Some("m"), false).await).await;
        assert!(
            body.contains(r#"aria-label="review approving step 0 of run run-u14""#),
            "{body}"
        );
        let body = body_string(
            get(
                &state,
                "/admin/runs/run-u14/steps/0/approve",
                Some("m"),
                false,
            )
            .await,
        )
        .await;
        assert!(
            body.contains(r#"<label>rw token <input type="password" name="rw_token""#),
            "{body}"
        );
        assert!(body.contains(r#"name="confirm" autocomplete="off" spellcheck="false" required"#));
        assert!(plan_in(&body).is_some(), "{body}");
        assert!(
            body.contains(
                r#"<a href="/admin/runs/run-u14">← back to the run (nothing changes)</a>"#
            ),
            "{body}"
        );
        // The approval's confirmation is the same structure as revocation's,
        // with the per-submission rw credential inside the same form.
        assert_confirmation_form(
            &body,
            &[("rw_token", "password")],
            r#"<a href="/admin/runs/run-u14">← back to the run (nothing changes)</a>"#,
        );
        // Behind the view-only proxy the review is shown and no
        // confirmation is offered.
        let body = body_string(
            get(
                &state,
                "/admin/runs/run-u14/steps/0/approve",
                Some("m"),
                true,
            )
            .await,
        )
        .await;
        assert!(plan_in(&body).is_none(), "{body}");
        assert!(!body.contains(r#"name="rw_token""#), "{body}");
        assert!(body.contains("view-only passthrough"), "{body}");
    }

    /// U11 (empty-deployment landing), on PostgreSQL: a tenant with nothing
    /// deployed gets the three first tasks, each linked to its guide, and no
    /// tiles reading zero; a token alone does not end it, and the first
    /// collection (in any status) brings the tiles back.
    #[tokio::test]
    async fn an_empty_deployment_gets_the_first_tasks_on_postgres() {
        let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
            eprintln!(
                "console U11 landing PostgreSQL test NOT RUN: MUNARIUM_TEST_DATABASE_URL unset"
            );
            return;
        };
        let tenant = format!("console-empty-{}", uuid::Uuid::new_v4().simple());
        let mut cfg = test_config(AuthMode::Static(vec![(
            "m".into(),
            tenant.clone(),
            "mgmt".into(),
        )]));
        cfg.store = StoreKind::Postgres;
        cfg.database_url = Some(url);
        let state = AppState::new(cfg).await.expect("state");
        let pool = state.pg_pool().expect("pool").clone();
        issue(&state, &tenant, "alice").await;

        for view_only in [false, true] {
            let body = body_string(get(&state, "/admin", Some("m"), view_only).await).await;
            assert!(body.contains("Nothing is deployed"), "{body}");
            for t in FIRST_TASKS {
                assert!(
                    body.contains(&format!(
                        r#"<a href="{DOCS_BASE}{}">{}</a>"#,
                        t.doc, t.title
                    )),
                    "{}: {body}",
                    t.title
                );
            }
            assert!(body.contains("tokens active: 1"), "{body}");
            assert!(!body.contains("runs awaiting approval"), "{body}");
            assert!(!body.contains("block findings"), "{body}");
            html_checks::assert_operable("/admin", &body);
            assert_classes_styled("/admin", &body);
        }

        // A retired collection still makes the deployment non-empty: the
        // tiles return, reading what is there.
        sqlx::query(
            "INSERT INTO collections (tenant_id, id, name, shape_ref, status)
             VALUES ($1, 'col-empty', 'retired-one', 's@1', 'retired')",
        )
        .bind(&tenant)
        .execute(&pool)
        .await
        .expect("collection");
        let body = body_string(get(&state, "/admin", Some("m"), false).await).await;
        assert!(!body.contains("Nothing is deployed"), "{body}");
        assert!(body.contains("runs awaiting approval"), "{body}");
        for t in FIRST_TASKS {
            assert!(!body.contains(t.title), "{body}");
        }
    }

    /// One typed confirmation (U4), checked as a form (U14): inside one post
    /// form with class `action confirm`, the CSRF token and plan as hidden
    /// fields, the `extra` (name, type) fields, a labelled required text
    /// field for the typed identifier and a named danger button; after the
    /// form, `back`, a link that changes nothing.
    fn assert_confirmation_form(body: &str, extra: &[(&str, &str)], back: &str) {
        let tags = html_checks::tags(body);
        let form = tags
            .iter()
            .position(|t| {
                t.name == "form" && !t.closing && t.attr("class") == Some("action confirm")
            })
            .expect("confirmation form");
        let end = tags[form..]
            .iter()
            .position(|t| t.name == "form" && t.closing)
            .unwrap()
            + form;
        let inside = &tags[form..end];
        assert_eq!(tags[form].attr("method"), Some("post"));
        let input = |name: &str| {
            inside
                .iter()
                .find(|t| t.name == "input" && t.attr("name") == Some(name))
        };
        assert_eq!(input("_csrf").and_then(|t| t.attr("type")), Some("hidden"));
        assert_eq!(input("plan").and_then(|t| t.attr("type")), Some("hidden"));
        for (name, kind) in extra {
            assert_eq!(
                input(name).and_then(|t| t.attr("type")),
                Some(*kind),
                "{name} inside the confirmation form"
            );
        }
        let typed = input("confirm").expect("typed field");
        assert!(typed.has("required") && typed.attr("type") == Some("text"));
        assert!(inside.iter().any(|t| t.name == "label" && !t.closing));
        assert!(inside
            .iter()
            .any(|t| t.name == "button" && t.attr("class") == Some("danger")));
        let after = &body[tags[end].at..];
        assert!(after.contains(back), "{after}");
    }

    // -----------------------------------------------------------------------
    // U13: one visual vocabulary
    // -----------------------------------------------------------------------

    /// The pages a structural check renders without a database: every
    /// navigation page, the token issue page, a revocation review, a missing
    /// shape (the error panel) and the login form.
    fn memory_pages() -> Vec<String> {
        let mut pages: Vec<String> = NAV
            .iter()
            .flat_map(|g| g.pages.iter().map(|p| p.href.to_string()))
            .collect();
        pages.extend(
            [
                "/admin/tokens/issue",
                "/admin/tokens/tok-x/revoke",
                "/admin/runs/run-x/steps/0/approve",
                "/admin/shapes/nope@9",
                "/admin/login",
            ]
            .map(String::from),
        );
        pages
    }

    /// The text that follows `tag`'s matching `</span>`, up to the next tag.
    fn text_after_span(body: &str, tag: &html_checks::Tag) -> String {
        let close = body[tag.at..].find("</span>").expect("closed span") + tag.at + 7;
        let end = body[close..].find('<').map_or(body.len(), |e| close + e);
        body[close..end].trim().to_string()
    }

    /// The text inside a span, up to its first child tag or its end.
    fn text_inside_span(body: &str, tag: &html_checks::Tag) -> String {
        let open = body[tag.at..].find('>').expect("tag end") + tag.at + 1;
        let end = body[open..].find('<').map_or(body.len(), |e| open + e);
        body[open..end].trim().to_string()
    }

    /// Every status color on a page comes with a text label: a swatch is
    /// followed by its label, and a badge carries its text.
    pub(super) fn assert_status_cues_have_text(path: &str, body: &str) {
        for t in html_checks::tags(body) {
            let Some(class) = t.attr("class") else {
                continue;
            };
            let classes: Vec<&str> = class.split_whitespace().collect();
            if t.closing || t.name != "span" {
                continue;
            }
            if classes.contains(&"swatch") {
                assert!(
                    !text_after_span(body, &t).is_empty(),
                    "{path}: a swatch without a label at byte {}",
                    t.at
                );
            }
            if classes.contains(&"badge") {
                assert!(
                    !text_inside_span(body, &t).is_empty(),
                    "{path}: an empty badge at byte {}",
                    t.at
                );
            }
        }
    }

    pub(super) fn assert_classes_styled(path: &str, body: &str) {
        for class in html_checks::classes(body) {
            assert!(
                html_checks::styled(charts::STYLE, &class),
                "{path}: class {class:?} has no rule in charts::STYLE"
            );
        }
    }

    #[tokio::test]
    async fn every_class_is_styled_and_every_status_color_has_text() {
        let state = state(static_auth()).await;
        for path in memory_pages() {
            let body = body_string(get(&state, &path, Some("m"), false).await).await;
            assert_classes_styled(&path, &body);
            assert_status_cues_have_text(&path, &body);
        }
        // The cue helpers themselves, over every reserved state.
        for (html, label) in [
            (state_badge("done"), "done"),
            (state_badge("failed"), "failed"),
            (state_badge("awaiting_approval"), "awaiting_approval"),
            (state_badge("running"), "running"),
            (severity_badge("block"), "block"),
            (severity_badge("warn"), "warn"),
            (severity_badge("info"), "info"),
            (pill("open", Tone::Neutral), "open"),
            (pill("verified", Tone::Good), "verified"),
            (pill("stale", Tone::Warn), "stale"),
            (pill("failed", Tone::Bad), "failed"),
        ] {
            assert!(html.contains(label), "{html}");
            assert_classes_styled(label, &html);
            assert_status_cues_have_text(label, &html);
        }
        // Hostile labels stay escaped inside a pill.
        assert_eq!(
            pill("<b>", Tone::Bad),
            r#"<span class="badge bad">&lt;b&gt;</span>"#
        );
    }

    /// The status tokens have text-safe variants in both color schemes, and
    /// identifiers share one monospace token.
    #[test]
    fn the_stylesheet_defines_text_tones_and_a_monospace_token() {
        let css = charts::STYLE;
        for token in ["--good-ink:", "--warning-ink:", "--critical-ink:"] {
            assert_eq!(css.matches(token).count(), 2, "{token} in light and dark");
        }
        assert!(css.contains("--mono:"));
        assert!(css.contains("code, .id { font-family: var(--mono)"));
        // No inline width styles remain on the issue form.
        let admin_form = super::inventory::issue_form_for_test();
        assert!(!admin_form.contains("style="), "{admin_form}");
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

    // -----------------------------------------------------------------------
    // U10 / LOGIN1: revocable console sessions
    // -----------------------------------------------------------------------

    use crate::console_sessions::{ConsoleSessionSettings, SESSION_COOKIE};

    async fn sessions_state(auth: AuthMode, settings: ConsoleSessionSettings) -> Arc<AppState> {
        AppState::new_with_console(test_config(auth), settings)
            .await
            .expect("state")
    }

    async fn send(state: &Arc<AppState>, req: axum::http::Request<axum::body::Body>) -> Response {
        crate::rest::router(state.clone())
            .oneshot(req)
            .await
            .unwrap()
    }

    async fn login(state: &Arc<AppState>, token: &str) -> Response {
        send(
            state,
            axum::http::Request::post("/admin/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(axum::body::Body::from(format!("token={token}")))
                .unwrap(),
        )
        .await
    }

    fn set_cookies(resp: &Response) -> Vec<String> {
        resp.headers()
            .get_all("set-cookie")
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .collect()
    }

    /// The value a response sets for cookie `name`, if any.
    fn cookie_set(resp: &Response, name: &str) -> Option<String> {
        set_cookies(resp).iter().find_map(|c| {
            let v = c.strip_prefix(&format!("{name}="))?;
            Some(v.split(';').next().unwrap_or_default().to_string())
        })
    }

    /// Signs in through `state` and returns the session handle.
    async fn sign_in(state: &Arc<AppState>, token: &str) -> String {
        let resp = login(state, token).await;
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        assert_eq!(resp.headers()["location"], "/admin");
        let handle = cookie_set(&resp, SESSION_COOKIE).expect("session cookie");
        assert!(crate::console_sessions::well_formed(&handle), "{handle}");
        handle
    }

    async fn get_with_cookie(state: &Arc<AppState>, path: &str, cookie: &str) -> Response {
        send(
            state,
            axum::http::Request::get(path)
                .header("cookie", cookie)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
    }

    async fn post_with_cookie(
        state: &Arc<AppState>,
        path: &str,
        cookie: &str,
        form: &str,
    ) -> Response {
        send(
            state,
            axum::http::Request::post(path)
                .header("cookie", cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(axum::body::Body::from(form.to_string()))
                .unwrap(),
        )
        .await
    }

    fn session_cookie(handle: &str) -> String {
        format!("{SESSION_COOKIE}={handle}")
    }

    /// Whether `handle` opens the console on `state`.
    async fn admitted(state: &Arc<AppState>, handle: &str) -> bool {
        let resp = get_with_cookie(state, "/admin", &session_cookie(handle)).await;
        match resp.status() {
            StatusCode::OK => true,
            StatusCode::SEE_OTHER => {
                assert_eq!(resp.headers()["location"], "/admin/login");
                false
            }
            other => panic!("unexpected {other}"),
        }
    }

    fn csrf_in(body: &str) -> Option<String> {
        let start = body.find(r#"name="_csrf" value=""#)? + r#"name="_csrf" value=""#.len();
        let len = body[start..].find('"')?;
        Some(body[start..start + len].to_string())
    }

    /// A page rendered for `handle`, with its CSRF token.
    async fn page_csrf(state: &Arc<AppState>, path: &str, handle: &str) -> (String, String) {
        let body = body_string(get_with_cookie(state, path, &session_cookie(handle)).await).await;
        let csrf = csrf_in(&body).unwrap_or_else(|| panic!("no csrf on {path}: {body}"));
        (body, csrf)
    }

    /// The default is the v1.4 login, unchanged: the cookie holds the token,
    /// a session cookie is never read, no sign-out is offered, and the
    /// sessions page says the replica uses the v1.4 login.
    #[tokio::test]
    async fn the_v1_4_login_stays_the_default() {
        let state = state(static_auth()).await;
        assert_eq!(state.console.settings, ConsoleSessionSettings::default());
        let resp = login(&state, "m").await;
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            set_cookies(&resp),
            vec!["__munarium_admin=m; HttpOnly; SameSite=Strict; Path=/admin".to_string()]
        );
        let resp = get_with_cookie(&state, "/admin", "__munarium_admin=m").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(!body.contains("/admin/logout"), "{body}");
        // Non-mgmt and unknown tokens are refused as in v1.4.
        let body = body_string(login(&state, "w").await).await;
        assert!(body.contains("not a management (mgmt) token"), "{body}");
        let body = body_string(login(&state, "nope").await).await;
        assert!(body.contains("invalid token"), "{body}");
        // A session handle means nothing to a legacy replica.
        let fake = format!("mcs1_{}", "a".repeat(64));
        assert!(!admitted(&state, &fake).await);
        let body =
            body_string(get(&state, "/admin/console-sessions", Some("m"), false).await).await;
        assert!(body.contains("MUNARIUM_CONSOLE_SESSIONS=legacy"), "{body}");
        assert!(body.contains(r#"<div class="empty">none</div>"#), "{body}");
        // The v1.4 cookie can still be cleared through the new sign-out
        // route, which keeps its CSRF check.
        let csrf = csrf_for(&state, "m");
        let resp = post_with_cookie(
            &state,
            "/admin/logout",
            "__munarium_admin=m",
            &format!("_csrf={}", "0".repeat(64)),
        )
        .await;
        assert!(body_string(resp).await.contains("stale form"));
        let mut headers = HeaderMap::new();
        headers.insert("cookie", "__munarium_admin=m".parse().unwrap());
        let cookie_csrf = csrf_token(&state, &headers);
        assert_eq!(cookie_csrf, csrf, "the v1.4 token binds to the credential");
        let resp = post_with_cookie(
            &state,
            "/admin/logout",
            "__munarium_admin=m",
            &format!("_csrf={csrf}"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        assert!(set_cookies(&resp)[0].starts_with("__munarium_admin=; Max-Age=0;"));
    }

    /// Enabled: the cookie carries only an opaque handle and the v1.4
    /// cookie is removed and ignored; bearer callers are unaffected; the
    /// session's CSRF token protects actions; sign-out ends the session.
    #[tokio::test]
    async fn session_login_issues_an_opaque_handle_and_signs_out() {
        let state = sessions_state(static_auth(), ConsoleSessionSettings::enabled()).await;
        let resp = login(&state, "m").await;
        let cookies = set_cookies(&resp);
        assert_eq!(cookies.len(), 2, "{cookies:?}");
        assert!(
            cookies[0].starts_with("__munarium_console=mcs1_"),
            "{cookies:?}"
        );
        assert!(cookies[0].ends_with("; HttpOnly; SameSite=Strict; Path=/admin"));
        assert!(
            cookies[1].starts_with("__munarium_admin=; Max-Age=0;"),
            "{cookies:?}"
        );
        let handle = cookie_set(&resp, SESSION_COOKIE).unwrap();
        assert_ne!(handle, "m");
        // The roles the v1.4 login refuses are still refused.
        assert!(cookie_set(&login(&state, "w").await, SESSION_COOKIE).is_none());
        assert!(cookie_set(&login(&state, "nope").await, SESSION_COOKIE).is_none());

        assert!(admitted(&state, &handle).await);
        // The v1.4 cookie is ignored once sessions are on.
        let resp = get_with_cookie(&state, "/admin", "__munarium_admin=m").await;
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        // Bearer callers (scripts, the view-only proxy) are unchanged.
        let resp = get(&state, "/admin/health", Some("m"), false).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(body.contains("console login"), "{body}");
        assert!(
            body.contains("sessions (opaque handle; idle 1800 s"),
            "{body}"
        );
        assert!(!body.contains("/admin/logout"), "bearer: {body}");

        // The session's pages offer sign-out and pass the U14 checks.
        let (body, csrf) = page_csrf(&state, "/admin/tokens/issue", &handle).await;
        assert!(body.contains(r#"action="/admin/logout""#), "{body}");
        for path in ["/admin", "/admin/tokens/issue", "/admin/console-sessions"] {
            let body =
                body_string(get_with_cookie(&state, path, &session_cookie(&handle)).await).await;
            html_checks::assert_operable(path, &body);
            assert_classes_styled(path, &body);
            assert_status_cues_have_text(path, &body);
        }
        assert_ne!(
            csrf,
            csrf_for(&state, "m"),
            "bound to the session, not the token"
        );
        // A CSRF-protected action with the session's token, and without it.
        let form = "uid=alice&access_level=1&scope_query=1";
        let resp = post_with_cookie(
            &state,
            "/admin/tokens/issue",
            &session_cookie(&handle),
            &format!("_csrf={}&{form}", csrf_for(&state, "m")),
        )
        .await;
        assert!(body_string(resp).await.contains("stale form"));
        let resp = post_with_cookie(
            &state,
            "/admin/tokens/issue",
            &session_cookie(&handle),
            &format!("_csrf={csrf}&{form}"),
        )
        .await;
        assert!(body_string(resp).await.contains("token minted"));

        // Sign-out needs the CSRF token, then ends the session and clears
        // the cookie.
        let resp =
            post_with_cookie(&state, "/admin/logout", &session_cookie(&handle), "_csrf=x").await;
        assert!(body_string(resp).await.contains("stale form"));
        assert!(admitted(&state, &handle).await);
        let resp = post_with_cookie(
            &state,
            "/admin/logout",
            &session_cookie(&handle),
            &format!("_csrf={csrf}"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        assert_eq!(resp.headers()["location"], "/admin/login");
        assert!(set_cookies(&resp)[0].starts_with("__munarium_console=; Max-Age=0;"));
        let resp = get_with_cookie(&state, "/admin", &session_cookie(&handle)).await;
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        assert!(
            set_cookies(&resp)[0].starts_with("__munarium_console=; Max-Age=0;"),
            "a dead handle's cookie is cleared"
        );
        // A form rendered for the ended session is dead too.
        let resp = post_with_cookie(
            &state,
            "/admin/tokens/issue",
            &session_cookie(&handle),
            &format!("_csrf={csrf}&{form}"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    }

    /// With `MUNARIUM_CONSOLE_COOKIE_SECURE=true` the cookie satisfies the
    /// `__Host-` prefix, and only that cookie name is read.
    #[tokio::test]
    async fn the_secure_cookie_uses_the_host_prefix() {
        let settings = ConsoleSessionSettings {
            secure_cookie: true,
            ..ConsoleSessionSettings::enabled()
        };
        let state = sessions_state(static_auth(), settings).await;
        let resp = login(&state, "m").await;
        let handle = cookie_set(&resp, crate::console_sessions::SECURE_SESSION_COOKIE).unwrap();
        assert!(set_cookies(&resp)[0].ends_with("; HttpOnly; SameSite=Strict; Secure; Path=/"));
        let resp = get_with_cookie(
            &state,
            "/admin",
            &format!("__Host-munarium_console={handle}"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(
            !admitted(&state, &handle).await,
            "the plain name is not read"
        );
    }

    /// Forged, tampered and malformed handles open nothing, and neither does
    /// a static token presented as a handle.
    #[tokio::test]
    async fn forged_and_tampered_handles_are_refused() {
        let state = sessions_state(static_auth(), ConsoleSessionSettings::enabled()).await;
        let handle = sign_in(&state, "m").await;
        assert!(admitted(&state, &handle).await);
        let last = handle.chars().last().unwrap();
        let flipped = format!(
            "{}{}",
            &handle[..handle.len() - 1],
            if last == '0' { '1' } else { '0' }
        );
        for bad in [
            flipped,
            format!("mcs1_{}", "0".repeat(64)),
            handle[..handle.len() - 1].to_string(),
            handle.to_uppercase(),
            "m".to_string(),
            crate::console_sessions::handle_digest(&handle),
        ] {
            assert!(!admitted(&state, &bad).await, "{bad}");
        }
        assert!(
            admitted(&state, &handle).await,
            "the real handle still works"
        );
    }

    /// Idle and absolute expiry on the memory store (an injected clock).
    #[tokio::test]
    async fn idle_and_absolute_expiry_on_the_memory_store() {
        let settings = ConsoleSessionSettings {
            idle_secs: 600,
            max_secs: 3600,
            ..ConsoleSessionSettings::enabled()
        };
        let state = sessions_state(static_auth(), settings).await;
        // Idle: each request restarts the idle clock; a gap ends it.
        let idle = sign_in(&state, "m").await;
        state.console.advance_clock(599);
        assert!(admitted(&state, &idle).await);
        state.console.advance_clock(599);
        assert!(admitted(&state, &idle).await);
        state.console.advance_clock(600);
        assert!(!admitted(&state, &idle).await);
        // Absolute: busy all the way, still ends at the lifetime.
        let busy = sign_in(&state, "m").await;
        for _ in 0..7 {
            state.console.advance_clock(500);
            assert!(admitted(&state, &busy).await);
        }
        state.console.advance_clock(100);
        assert!(!admitted(&state, &busy).await, "3600 s after sign-in");
        // The list shows both as expired, not live.
        let body =
            body_string(get(&state, "/admin/console-sessions", Some("m"), false).await).await;
        assert_eq!(body.matches(">expired</span>").count(), 2, "{body}");
    }

    /// The MCP page is tenant-scoped, so it shows no process-wide counter:
    /// those sum every tenant's MCP traffic on this replica. They are on the
    /// replica-scoped health page, labelled as covering all tenants.
    #[tokio::test]
    async fn replica_wide_mcp_counters_are_on_the_health_page_only() {
        let state = state(static_auth()).await;
        // Another tenant's traffic, as the process counters record it.
        state.metrics.inc_by(
            "munarium_mcp_audit_refusals_total",
            crate::metrics::labels(&[("reason", "store-error")]),
            7_301,
        );
        state.metrics.inc_by(
            "munarium_mcp_requests_total",
            crate::metrics::labels(&[("method", "tools/call"), ("outcome", "shed")]),
            7_302,
        );
        state
            .metrics
            .inc_by("munarium_interactions_dropped_total", String::new(), 7_303);
        let mcp = body_string(get(&state, "/admin/mcp", Some("m"), false).await).await;
        for leaked in [
            "7301",
            "7302",
            "7303",
            "audit admission refusals",
            "load shed at MCP entry",
        ] {
            assert!(!mcp.contains(leaked), "{leaked}: {mcp}");
        }
        assert!(mcp.contains(r#"href="/admin/health""#), "{mcp}");
        let health = body_string(get(&state, "/admin/health", Some("m"), false).await).await;
        assert!(health.contains("this replica, all tenants"), "{health}");
        for shown in ["7301", "7302", "7303"] {
            assert!(health.contains(shown), "{shown}: {health}");
        }
        for path in ["/admin/mcp", "/admin/health"] {
            let body = body_string(get(&state, path, Some("m"), false).await).await;
            html_checks::assert_operable(path, &body);
        }
    }

    /// A self-reloading page's refresh (the build-job page's meta refresh)
    /// checks the session but does not restart its idle clock, so a page
    /// left open cannot keep an unattended session alive. The operator's own
    /// request for the same page still does.
    #[tokio::test]
    async fn a_page_refresh_does_not_keep_an_idle_session_alive() {
        let settings = ConsoleSessionSettings {
            idle_secs: 600,
            max_secs: 3600,
            ..ConsoleSessionSettings::enabled()
        };
        let state = sessions_state(static_auth(), settings).await;
        let page = "/admin/build-jobs/bjob-refresh";
        let refresh = refresh_url(page);
        assert_eq!(refresh, "/admin/build-jobs/bjob-refresh?refresh=1");

        let idle = sign_in(&state, "m").await;
        for _ in 0..2 {
            state.console.advance_clock(250);
            // Still live, so the refresh is served (the memory store has no
            // build jobs, so the page is the honest unavailable panel).
            let resp = get_with_cookie(&state, &refresh, &session_cookie(&idle)).await;
            assert_eq!(resp.status(), StatusCode::OK, "within the idle limit");
        }
        state.console.advance_clock(250);
        // 750 s since the last operator request, with only refreshes in
        // between: the session has idled out, and a refresh is refused too.
        assert!(!admitted(&state, &idle).await, "refreshes kept it alive");
        let resp = get_with_cookie(&state, &refresh, &session_cookie(&idle)).await;
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);

        // The operator opening the same page is activity.
        let active = sign_in(&state, "m").await;
        state.console.advance_clock(500);
        let resp = get_with_cookie(&state, page, &session_cookie(&active)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        state.console.advance_clock(500);
        assert!(admitted(&state, &active).await);
    }

    /// Signing in again from a browser that holds a session signs the
    /// replaced session out, so it does not stay live and unseen until it
    /// expires. A cookie that names no live session does not stop a login.
    #[tokio::test]
    async fn signing_in_again_signs_out_the_replaced_session() {
        let state = sessions_state(static_auth(), ConsoleSessionSettings::enabled()).await;
        let first = sign_in(&state, "m").await;
        assert!(admitted(&state, &first).await);
        let relogin = |cookie: String| {
            let state = state.clone();
            async move {
                send(
                    &state,
                    axum::http::Request::post("/admin/login")
                        .header("cookie", cookie)
                        .header("content-type", "application/x-www-form-urlencoded")
                        .body(axum::body::Body::from("token=m"))
                        .unwrap(),
                )
                .await
            }
        };
        let resp = relogin(session_cookie(&first)).await;
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        let second = cookie_set(&resp, SESSION_COOKIE).expect("a new session");
        assert_ne!(second, first);
        assert!(!admitted(&state, &first).await, "the replaced session");
        assert!(admitted(&state, &second).await);
        let body =
            body_string(get(&state, "/admin/console-sessions", Some("m"), false).await).await;
        assert_eq!(body.matches(">signed out</span>").count(), 1, "{body}");
        assert_eq!(body.matches(">live</span>").count(), 1, "{body}");

        // A malformed or already ended handle does not stop the login.
        for stale in ["not-a-handle".to_string(), first.clone()] {
            let resp = relogin(session_cookie(&stale)).await;
            assert_eq!(resp.status(), StatusCode::SEE_OTHER);
            assert!(cookie_set(&resp, SESSION_COOKIE).is_some());
        }
        // A failed login signs nothing out.
        let resp = send(
            &state,
            axum::http::Request::post("/admin/login")
                .header("cookie", session_cookie(&second))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(axum::body::Body::from("token=nope"))
                .unwrap(),
        )
        .await;
        assert!(body_string(resp).await.contains("invalid token"));
        assert!(admitted(&state, &second).await);
    }

    /// Console revocation behind review and typed confirmation (U4), on the
    /// memory store: another session is ended, refusals act on nothing, and
    /// a duplicate changes nothing. Another tenant cannot see the session.
    #[tokio::test]
    async fn session_revocation_is_reviewed_then_confirmed() {
        let auth = AuthMode::Static(vec![
            ("m".into(), "t".into(), "mgmt".into()),
            ("m2".into(), "t".into(), "mgmt".into()),
            ("mo".into(), "t2".into(), "mgmt".into()),
        ]);
        let state = sessions_state(auth, ConsoleSessionSettings::enabled()).await;
        let mine = sign_in(&state, "m").await;
        state.console.advance_clock(1);
        let theirs = sign_in(&state, "m2").await;
        let records = crate::console_sessions::list(&state, "t").await.unwrap();
        assert_eq!(records.len(), 2);
        let target = records
            .iter()
            .find(|r| r.created_at == records.iter().map(|r| r.created_at).max().unwrap())
            .unwrap()
            .session_id
            .clone();
        let (body, csrf) = page_csrf(&state, "/admin/console-sessions", &mine).await;
        assert!(body.contains(">this session</span>"), "{body}");
        assert!(
            body.contains(&format!(
                r#"aria-label="review revoking console session {target}""#
            )),
            "{body}"
        );
        let review = format!("/admin/console-sessions/{target}/revoke");
        let confirm = format!("{review}/confirm");
        let body =
            body_string(get_with_cookie(&state, &review, &session_cookie(&mine)).await).await;
        html_checks::assert_operable(&review, &body);
        assert_classes_styled(&review, &body);
        let plan = plan_in(&body).expect("plan");
        // The review is a read.
        assert!(admitted(&state, &theirs).await);
        let cookie = session_cookie(&mine);
        // Wrong typed id, missing CSRF, forged plan, view-only.
        let resp = post_with_cookie(
            &state,
            &confirm,
            &cookie,
            &format!("_csrf={csrf}&plan={plan}&confirm=cs-x"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let resp = post_with_cookie(
            &state,
            &confirm,
            &cookie,
            &format!("plan={plan}&confirm={target}"),
        )
        .await;
        assert!(body_string(resp).await.contains("stale form"));
        let resp = post_with_cookie(
            &state,
            &confirm,
            &cookie,
            &format!("_csrf={csrf}&plan={}&confirm={target}", "0".repeat(64)),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let resp = send(
            &state,
            axum::http::Request::post(&confirm)
                .header("cookie", &cookie)
                .header(VIEW_ONLY_HEADER, "1")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(axum::body::Body::from(format!(
                    "_csrf={csrf}&plan={plan}&confirm={target}"
                )))
                .unwrap(),
        )
        .await;
        assert!(body_string(resp).await.contains("view-only passthrough"));
        // The plan is bound to the reviewing operator: the bearer's own
        // CSRF token and the session's plan do not match.
        let resp = post_form(
            &state,
            &confirm,
            "m",
            &format!(
                "_csrf={}&plan={plan}&confirm={target}",
                csrf_for(&state, "m")
            ),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        // Another tenant sees nothing.
        let resp = get(&state, &review, Some("mo"), false).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body =
            body_string(get(&state, "/admin/console-sessions", Some("mo"), false).await).await;
        assert!(!body.contains(&target), "{body}");
        assert!(admitted(&state, &theirs).await);

        let good = format!("_csrf={csrf}&plan={plan}&confirm={target}");
        let resp = post_with_cookie(&state, &confirm, &cookie, &good).await;
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        assert_eq!(resp.headers()["location"], "/admin/console-sessions");
        assert!(!admitted(&state, &theirs).await);
        assert!(admitted(&state, &mine).await);
        let resp = post_with_cookie(&state, &confirm, &cookie, &good).await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert!(body_string(resp).await.contains("already ended"));
        let body = body_string(get_with_cookie(&state, &review, &cookie).await).await;
        assert!(
            body.contains("nothing to confirm") && plan_in(&body).is_none(),
            "{body}"
        );
        let body =
            body_string(get_with_cookie(&state, "/admin/console-sessions", &cookie).await).await;
        assert!(body.contains(">revoked</span>"), "{body}");
    }

    // -- PostgreSQL: two replicas over one database -------------------------

    async fn pg_replica(
        url: &str,
        tokens: Vec<(String, String, String)>,
        settings: ConsoleSessionSettings,
    ) -> Arc<AppState> {
        let mut cfg = test_config(AuthMode::Static(tokens));
        cfg.store = StoreKind::Postgres;
        cfg.database_url = Some(url.to_string());
        AppState::new_with_console(cfg, settings)
            .await
            .expect("state")
    }

    async fn session_id_of(state: &AppState, tenant: &str, before: &[String]) -> String {
        crate::console_sessions::list(state, tenant)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.session_id)
            .find(|id| !before.contains(id))
            .expect("new session")
    }

    async fn session_ids(state: &AppState, tenant: &str) -> Vec<String> {
        crate::console_sessions::list(state, tenant)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.session_id)
            .collect()
    }

    /// LOGIN1-T2 in one process: two replicas (two `AppState`s, two pools)
    /// over one database. A session signed in on one replica works on the
    /// other, including a CSRF-protected action and a U4 review confirmed
    /// across replicas; sign-out and revocation on either are honoured by
    /// both; idle and absolute expiry hold on both, with the stricter
    /// replica setting winning; forged and tampered handles fail; another
    /// tenant sees nothing; and a rotated credential ends its sessions.
    #[tokio::test]
    async fn console_sessions_are_shared_and_revocable_across_replicas_on_postgres() {
        let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
            eprintln!("console sessions PostgreSQL test NOT RUN: MUNARIUM_TEST_DATABASE_URL unset");
            return;
        };
        let tenant = format!("console-u10-{}", uuid::Uuid::new_v4().simple());
        let other = format!("{tenant}-other");
        let tok = |t: &str, ten: &str, r: &str| (t.to_string(), ten.to_string(), r.to_string());
        let m = format!("m-{tenant}");
        let m2 = format!("m2-{tenant}");
        let mo = format!("mo-{tenant}");
        let tokens = vec![
            tok(&m, &tenant, "mgmt"),
            tok(&m2, &tenant, "mgmt"),
            tok(&mo, &other, "mgmt"),
        ];
        let enabled = ConsoleSessionSettings::enabled();
        let a = pg_replica(&url, tokens.clone(), enabled).await;
        let b = pg_replica(&url, tokens.clone(), enabled).await;
        assert_ne!(a.boot_secret, b.boot_secret, "two independent replicas");

        // Signed in on A, used on B.
        let h = sign_in(&a, &m).await;
        assert!(admitted(&b, &h).await);
        let (_, csrf) = page_csrf(&a, "/admin/tokens/issue", &h).await;
        let resp = post_with_cookie(
            &b,
            "/admin/tokens/issue",
            &session_cookie(&h),
            &format!("_csrf={csrf}&uid=alice&access_level=1&scope_query=1"),
        )
        .await;
        let body = body_string(resp).await;
        assert!(
            body.contains("token minted"),
            "CSRF across replicas: {body}"
        );
        // A U4 review rendered on A confirms on B.
        let jti = issue(&a, &tenant, "bob").await;
        let review = format!("/admin/tokens/{jti}/revoke");
        let body = body_string(get_with_cookie(&a, &review, &session_cookie(&h)).await).await;
        let plan = plan_in(&body).expect("plan");
        let resp = post_with_cookie(
            &b,
            &format!("{review}/confirm"),
            &session_cookie(&h),
            &format!("_csrf={csrf}&plan={plan}&confirm={jti}"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::SEE_OTHER, "plan across replicas");
        assert!(revoked_at(&a, &tenant, &jti).await.is_some());

        // Sign-out on A is refused on B.
        let resp = post_with_cookie(
            &a,
            "/admin/logout",
            &session_cookie(&h),
            &format!("_csrf={csrf}"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        assert!(!admitted(&b, &h).await);
        assert!(!admitted(&a, &h).await);
        let reason: Option<String> = sqlx::query_scalar(
            "SELECT revoked_reason FROM console_sessions WHERE tenant_id = $1 AND revoked_at IS NOT NULL",
        )
        .bind(&tenant)
        .fetch_one(a.pg_pool().unwrap())
        .await
        .unwrap();
        assert_eq!(reason.as_deref(), Some("sign-out"));

        // Revocation on B is refused on A; the revoking session is unharmed.
        let before = session_ids(&a, &tenant).await;
        let victim = sign_in(&a, &m2).await;
        let victim_id = session_id_of(&a, &tenant, &before).await;
        let operator = sign_in(&b, &m).await;
        assert!(admitted(&a, &victim).await && admitted(&b, &operator).await);
        let review = format!("/admin/console-sessions/{victim_id}/revoke");
        let (body, csrf) = page_csrf(&b, &review, &operator).await;
        let plan = plan_in(&body).expect("plan");
        // Another tenant cannot see or revoke it.
        assert_eq!(
            get(&a, &review, Some(&mo), false).await.status(),
            StatusCode::NOT_FOUND
        );
        let resp = post_with_cookie(
            &b,
            &format!("{review}/confirm"),
            &session_cookie(&operator),
            &format!("_csrf={csrf}&plan={plan}&confirm={victim_id}"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        assert!(!admitted(&a, &victim).await);
        assert!(!admitted(&b, &victim).await);
        assert!(admitted(&a, &operator).await);

        // Forged and tampered handles.
        let mut tampered = operator.clone();
        let flip = if tampered.ends_with('0') { "1" } else { "0" };
        tampered.replace_range(tampered.len() - 1.., flip);
        for bad in [tampered, format!("mcs1_{}", "f".repeat(64)), m.clone()] {
            assert!(!admitted(&a, &bad).await, "{bad}");
            assert!(!admitted(&b, &bad).await, "{bad}");
        }

        let pool = a.pg_pool().unwrap().clone();
        let age = |column: &'static str, secs: i64, handle: String| {
            let pool = pool.clone();
            async move {
                sqlx::query(&format!(
                    "UPDATE console_sessions SET {column} = {column} - make_interval(secs => $1::double precision)
                      WHERE handle_digest = $2"
                ))
                .bind(secs as f64)
                .bind(crate::console_sessions::handle_digest(&handle))
                .execute(&pool)
                .await
                .unwrap();
            }
        };
        // Idle expiry, on both replicas.
        let idle = sign_in(&a, &m).await;
        age("last_seen_at", 1_790, idle.clone()).await;
        assert!(admitted(&b, &idle).await, "within the idle bound");
        age("last_seen_at", 1_801, idle.clone()).await;
        assert!(!admitted(&b, &idle).await);
        assert!(!admitted(&a, &idle).await);
        // Recorded absolute expiry, on both replicas, even when busy.
        let busy = sign_in(&b, &m).await;
        age("expires_at", 28_801, busy.clone()).await;
        assert!(!admitted(&a, &busy).await);
        assert!(!admitted(&b, &busy).await);
        // The stricter replica setting wins: a replica with a 600 s idle
        // bound and 900 s lifetime refuses what the default replicas accept.
        let strict = pg_replica(
            &url,
            tokens.clone(),
            ConsoleSessionSettings {
                idle_secs: 600,
                max_secs: 900,
                ..enabled
            },
        )
        .await;
        let s1 = sign_in(&a, &m).await;
        age("last_seen_at", 700, s1.clone()).await;
        assert!(!admitted(&strict, &s1).await, "stricter idle");
        assert!(admitted(&a, &s1).await);
        let s2 = sign_in(&a, &m).await;
        age("created_at", 1_000, s2.clone()).await;
        assert!(!admitted(&strict, &s2).await, "stricter lifetime");
        assert!(admitted(&a, &s2).await);

        // Bound to the credential: a replica whose configuration no longer
        // has the token refuses its sessions; one that moved it to another
        // tenant or role does too.
        let rotated = pg_replica(&url, vec![tok(&m2, &tenant, "mgmt")], enabled).await;
        assert!(!admitted(&rotated, &s1).await);
        let moved = pg_replica(&url, vec![tok(&m, &other, "mgmt")], enabled).await;
        assert!(!admitted(&moved, &s1).await);
        let demoted = pg_replica(&url, vec![tok(&m, &tenant, "rw")], enabled).await;
        assert!(!admitted(&demoted, &s1).await);
        assert!(admitted(&a, &s1).await);

        // The handle is stored only as its digest.
        let stored: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM console_sessions WHERE handle_digest = $1 OR handle_digest = $2",
        )
        .bind(&s1)
        .bind(crate::console_sessions::handle_digest(&s1))
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(stored, 1);
        let leaked: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM console_sessions
              WHERE tenant_id = $1 AND (credential_digest = $2 OR binding_secret LIKE '%' || $2 || '%')",
        )
        .bind(&tenant)
        .bind(&m)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(leaked, 0, "the token is never stored");
    }

    /// LOGIN1 with the feature off and after it was on: the v1.4 login works
    /// beside the table; a legacy replica ignores live handles (rollback),
    /// can still list and revoke them, and the documented fleet-wide
    /// rollback revocation keeps a re-enabled fleet from honouring them.
    /// Rows a week past expiry are pruned at the next sign-in.
    #[tokio::test]
    async fn the_v1_4_login_and_rollback_work_beside_sessions_on_postgres() {
        let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
            eprintln!("console rollback PostgreSQL test NOT RUN: MUNARIUM_TEST_DATABASE_URL unset");
            return;
        };
        let tenant = format!("console-login1-{}", uuid::Uuid::new_v4().simple());
        let m = format!("m-{tenant}");
        let tokens = vec![(m.clone(), tenant.clone(), "mgmt".to_string())];
        let enabled = pg_replica(&url, tokens.clone(), ConsoleSessionSettings::enabled()).await;
        let legacy = pg_replica(&url, tokens.clone(), ConsoleSessionSettings::default()).await;

        // Off: the v1.4 login, cookie and all.
        let resp = login(&legacy, &m).await;
        assert_eq!(
            set_cookies(&resp),
            vec![format!(
                "__munarium_admin={m}; HttpOnly; SameSite=Strict; Path=/admin"
            )]
        );
        let v14 = format!("__munarium_admin={m}");
        assert_eq!(
            get_with_cookie(&legacy, "/admin", &v14).await.status(),
            StatusCode::OK
        );
        assert_eq!(
            session_ids(&legacy, &tenant).await.len(),
            0,
            "no row for a v1.4 login"
        );
        // The enabled replica does not honour the v1.4 cookie.
        assert_eq!(
            get_with_cookie(&enabled, "/admin", &v14).await.status(),
            StatusCode::SEE_OTHER
        );

        // On, then switched off: the legacy replica ignores live handles.
        let first = sign_in(&enabled, &m).await;
        let second = sign_in(&enabled, &m).await;
        assert!(admitted(&enabled, &first).await);
        assert!(!admitted(&legacy, &first).await);
        assert_eq!(
            get_with_cookie(&legacy, "/admin", &v14).await.status(),
            StatusCode::OK
        );
        // The legacy replica lists them and can revoke one (per tenant).
        let ids = session_ids(&legacy, &tenant).await;
        assert_eq!(ids.len(), 2);
        let body =
            body_string(get(&legacy, "/admin/console-sessions", Some(&m), false).await).await;
        assert!(ids.iter().all(|id| body.contains(id.as_str())), "{body}");
        assert_eq!(body.matches(">live</span>").count(), 2, "{body}");
        let first_id = crate::console_sessions::list(&legacy, &tenant)
            .await
            .unwrap()
            .into_iter()
            .last()
            .unwrap()
            .session_id;
        let review = format!("/admin/console-sessions/{first_id}/revoke");
        let plan = plan_in(&body_string(get(&legacy, &review, Some(&m), false).await).await)
            .expect("plan");
        let resp = post_form(
            &legacy,
            &format!("{review}/confirm"),
            &m,
            &format!(
                "_csrf={}&plan={plan}&confirm={first_id}",
                csrf_for(&legacy, &m)
            ),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        // Switched on again: the revoked handle stays dead; an unrevoked one
        // would be honoured again, which is why rollback ends with the
        // fleet-wide revocation (docs/design/console-sessions.md#rollback).
        let again = pg_replica(&url, tokens.clone(), ConsoleSessionSettings::enabled()).await;
        assert!(!admitted(&again, &first).await);
        assert!(admitted(&again, &second).await);
        sqlx::query(
            "UPDATE console_sessions SET revoked_at = now(), revoked_reason = 'revoked'
              WHERE revoked_at IS NULL AND tenant_id = $1",
        )
        .bind(&tenant)
        .execute(again.pg_pool().unwrap())
        .await
        .unwrap();
        assert!(!admitted(&again, &second).await);

        // Pruning: a row more than seven days past its expiry is deleted at
        // the next sign-in; a recent one is kept.
        sqlx::query(
            "UPDATE console_sessions SET expires_at = now() - interval '8 days',
                    created_at = now() - interval '9 days'
              WHERE tenant_id = $1 AND session_id = $2",
        )
        .bind(&tenant)
        .bind(&first_id)
        .execute(again.pg_pool().unwrap())
        .await
        .unwrap();
        let _ = sign_in(&again, &m).await;
        let ids = session_ids(&again, &tenant).await;
        assert!(!ids.contains(&first_id), "pruned");
        assert_eq!(ids.len(), 2);
    }

    /// A legacy (rolled-back) replica labels each session by the lifetimes
    /// recorded at sign-in, not by the defaults: a session signed in under
    /// longer configured values, which a re-enabled fleet would honour, is
    /// listed as live so the operator is prompted to revoke it.
    #[tokio::test]
    async fn a_legacy_replica_lists_sessions_by_their_recorded_lifetimes_on_postgres() {
        let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
            eprintln!(
                "console legacy liveness PostgreSQL test NOT RUN: MUNARIUM_TEST_DATABASE_URL unset"
            );
            return;
        };
        let tenant = format!("console-legacy-live-{}", uuid::Uuid::new_v4().simple());
        let m = format!("m-{tenant}");
        let tokens = vec![(m.clone(), tenant.clone(), "mgmt".to_string())];
        let long = ConsoleSessionSettings {
            idle_secs: 7_200,
            max_secs: 86_400,
            ..ConsoleSessionSettings::enabled()
        };
        let enabled = pg_replica(&url, tokens.clone(), long).await;
        let legacy = pg_replica(&url, tokens.clone(), ConsoleSessionSettings::default()).await;
        let handle = sign_in(&enabled, &m).await;
        let age = |interval: &'static str| {
            let pool = enabled.pg_pool().unwrap().clone();
            let tenant = tenant.clone();
            async move {
                sqlx::query(&format!(
                    "UPDATE console_sessions SET last_seen_at = now() - interval '{interval}',
                            created_at = now() - interval '{interval}'
                      WHERE tenant_id = $1"
                ))
                .bind(&tenant)
                .execute(&pool)
                .await
                .unwrap();
            }
        };
        // An hour without a request: past the default idle (1800 s), within
        // the recorded one (7200 s) and the recorded absolute expiry.
        age("1 hour").await;
        let rows = crate::console_sessions::list(&legacy, &tenant)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].live, "{rows:?}");
        let body =
            body_string(get(&legacy, "/admin/console-sessions", Some(&m), false).await).await;
        assert_eq!(body.matches(">live</span>").count(), 1, "{body}");
        assert!(body.contains("recorded idle and absolute expiry"), "{body}");
        // ...and an enabled replica with those settings does honour it.
        assert!(admitted(&enabled, &handle).await);
        // Past the recorded idle it is expired everywhere.
        age("3 hours").await;
        let rows = crate::console_sessions::list(&legacy, &tenant)
            .await
            .unwrap();
        assert!(!rows[0].live, "{rows:?}");
        assert!(!admitted(&enabled, &handle).await);
    }

    // -----------------------------------------------------------------------
    // U12: build-job progress from recorded events
    // -----------------------------------------------------------------------

    /// Against PostgreSQL: the job list and two job pages read the recorded
    /// rows. A running direct build shows its labelled progress with the
    /// total unknown and refreshes; a v1.4-claimed job whose oldest event was
    /// pruned says "older history pruned" and "history unavailable" and does
    /// not refresh. Another tenant's job is not found, and a malformed id
    /// reads nothing. Every page passes the U13/U14 checks. The seeded rows
    /// are never claimable: terminal, or running under an infinite lease.
    #[tokio::test]
    async fn build_job_pages_show_recorded_progress_and_history_on_postgres() {
        let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
            eprintln!("console U12 PostgreSQL test NOT RUN: MUNARIUM_TEST_DATABASE_URL unset");
            return;
        };
        let tenant = format!("console-u12-{}", uuid::Uuid::new_v4().simple());
        let other = format!("{tenant}-other");
        let mut cfg = test_config(AuthMode::Static(vec![
            ("m".into(), tenant.clone(), "mgmt".into()),
            ("mo".into(), other.clone(), "mgmt".into()),
        ]));
        cfg.store = StoreKind::Postgres;
        cfg.database_url = Some(url);
        let state = AppState::new(cfg).await.expect("state");
        let pool = state.pg_pool().expect("pool").clone();
        for sql in [
            // A running direct build under a fenced claim, 2 sources done,
            // total not yet known.
            "INSERT INTO index_build_jobs
               (tenant_id, job_id, kind, scope_kind, scope_id, state, claimed_by,
                claimed_at, attempts, requested_by)
             VALUES ($1, 'bjob-u12-run', 'direct', 'collection', 'col-u12', 'running',
                     'builder-a', 'infinity', 1, 'api:rw')",
            "INSERT INTO index_build_job_runs (tenant_id, job_id, job_generation, node_id)
             VALUES ($1, 'bjob-u12-run', 1, 'builder-a')",
            "UPDATE index_build_jobs
                SET progress_phase = 'extracting', progress_units_done = 2,
                    progress_advanced_at = now()
              WHERE tenant_id = $1 AND job_id = 'bjob-u12-run'",
            // A job a v1.4 builder claimed and finished: no run record.
            "INSERT INTO index_build_jobs
               (tenant_id, job_id, kind, scope_kind, scope_id, state, requested_by)
             VALUES ($1, 'bjob-u12-v14', 'backfill', 'collection', 'col-u12', 'pending',
                     'api:rw')",
            "UPDATE index_build_jobs
                SET state = 'running', claimed_by = 'builder-v14', claimed_at = 'infinity',
                    attempts = 1
              WHERE tenant_id = $1 AND job_id = 'bjob-u12-v14'",
            "UPDATE index_build_jobs SET state = 'succeeded'
              WHERE tenant_id = $1 AND job_id = 'bjob-u12-v14'",
            // Retention pruned its oldest event.
            "DELETE FROM index_build_job_events
              WHERE tenant_id = $1 AND job_id = 'bjob-u12-v14' AND job_event_seq = 1",
        ] {
            sqlx::query(sql)
                .bind(&tenant)
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("{sql}: {e}"));
        }

        let list = body_string(get(&state, "/admin/build-jobs", Some("m"), false).await).await;
        assert!(
            list.contains(r#"href="/admin/build-jobs/bjob-u12-run""#),
            "{list}"
        );
        assert!(
            list.contains(r#"href="/admin/build-jobs/bjob-u12-v14""#),
            "{list}"
        );
        assert!(list.contains("mmctl datastore jobs list"), "{list}");

        let run =
            body_string(get(&state, "/admin/build-jobs/bjob-u12-run", Some("m"), false).await)
                .await;
        assert!(
            run.contains(
                r#"<meta http-equiv="refresh" content="5;url=/admin/build-jobs/bjob-u12-run?refresh=1">"#
            ),
            "{run}"
        );
        // The reload target serves the same page.
        let reloaded = body_string(
            get(
                &state,
                "/admin/build-jobs/bjob-u12-run?refresh=1",
                Some("m"),
                false,
            )
            .await,
        )
        .await;
        assert!(reloaded.contains("2 sources done"), "{reloaded}");
        assert!(run.contains("2 sources done"), "{run}");
        assert!(run.contains("total unknown"), "{run}");
        assert!(run.contains(">extracting</span>"), "{run}");
        assert!(!run.contains("history unavailable"), "{run}");
        assert!(!run.contains("older history pruned"), "{run}");
        assert!(
            run.contains("none: this job touched no index version"),
            "{run}"
        );
        assert!(
            run.contains("mmctl datastore jobs get bjob-u12-run"),
            "{run}"
        );

        let v14 =
            body_string(get(&state, "/admin/build-jobs/bjob-u12-v14", Some("m"), false).await)
                .await;
        assert!(!v14.contains("http-equiv"), "{v14}");
        assert!(v14.contains("older history pruned"), "{v14}");
        assert!(v14.contains("Events before #2 are no longer kept"), "{v14}");
        assert!(v14.contains("history unavailable"), "{v14}");
        assert!(!v14.contains("touched no index version"), "{v14}");
        assert!(v14.contains("no count recorded"), "{v14}");

        for path in [
            "/admin/build-jobs",
            "/admin/build-jobs/bjob-u12-run",
            "/admin/build-jobs/bjob-u12-v14",
        ] {
            for view_only in [false, true] {
                let body = body_string(get(&state, path, Some("m"), view_only).await).await;
                html_checks::assert_operable(path, &body);
                assert_classes_styled(path, &body);
                assert_status_cues_have_text(path, &body);
            }
        }

        // Tenant isolation: the other tenant neither lists nor reads it.
        let list = body_string(get(&state, "/admin/build-jobs", Some("mo"), false).await).await;
        assert!(!list.contains("bjob-u12"), "{list}");
        let resp = get(&state, "/admin/build-jobs/bjob-u12-run", Some("mo"), false).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = body_string(resp).await;
        assert!(!body.contains("builder-a"), "{body}");

        // A malformed id is refused before any read.
        let resp = get(&state, "/admin/build-jobs/a%20b", Some("m"), false).await;
        let body = body_string(resp).await;
        assert!(body.contains(r#"class="notice""#), "{body}");
        assert!(!body.contains("<h2>progress</h2>"), "{body}");

        sqlx::query(
            "UPDATE index_build_jobs SET state = 'cancelled'
              WHERE tenant_id = $1 AND state = 'running'",
        )
        .bind(&tenant)
        .execute(&pool)
        .await
        .unwrap();
    }
}
