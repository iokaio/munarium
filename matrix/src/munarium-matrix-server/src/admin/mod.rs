// SPDX-License-Identifier: Apache-2.0
//! `/admin` — Matrix's operator console.
//!
//! Served by the Matrix binary itself: no separate image, no Node at runtime,
//! no CDN, no new listener. Server-rendered HTML with inline SVG and **zero
//! JavaScript**, which is what lets the CSP be `default-src 'self'` with no
//! script source at all — every page works with scripting off because there is
//! nothing to switch off.
//!
//! **Mounted on the control role only.** The registry lives there, and so does
//! everything this console reads or writes. A `query`, `sync` or `reconcile`
//! container answers 404 on `/admin/*` the same way it answers 404 on the
//! registry — the surface is *absent*, not guarded, so there is no check to
//! misconfigure. `MUNARIUM_MATRIX_ADMIN=disabled` removes it from a control
//! container too, for a deployment that wants the API and nothing else.
//!
//! **The console is a client of the public API.** Every write it performs is a
//! `/v1` call `mxctl` could make, run through the same handler with the same
//! credential; there is no privileged in-process path and no second policy to
//! keep in step. Journal rows carry `via: admin-ui`.
//!
//! **Configuration is not file editing.** The repository stays the source of
//! truth. This console authors *drafts*, validates them with the same
//! validators `mxctl validate` uses, diffs them against what is applied, and
//! then either **exports** a bundle to commit — the default — or **applies in
//! place**, after which the deployment is flagged *drifted from git* until the
//! bundle lands. The server tree removed its own `/admin/authoring` pages in
//! August because a form that ends in a download served no purpose beside the
//! CLI; this configure loop earns its place by doing three things a CLI
//! cannot: seed a draft from a live introspect, diff it against the applied
//! version in one view, and show the drift flag.
//!
//! **Secrets and evidence never render here.** `credentialRef` names only; a
//! probe result is ok / denied / unreachable and never a connection string;
//! evidence rows are not shown at all — the evidence resolver is the server's
//! and is access-checked per session, so this console shows manifests, counts
//! and hashes and links out for the rest.
//!
//! Auth is mgmt-only by bearer or by the cookie `POST /admin/login` sets, CSRF
//! is a synchronizer token bound to the credential and to a per-process
//! secret, and every write additionally checks Origin/Host. See
//! `docs/security/admin-ui.md`.

mod chrome;
mod configure;
mod observe;
mod operate;
#[cfg(test)]
mod security_tests;

use crate::state::{AppState, Caller};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use std::sync::Arc;

const COOKIE: &str = "__munarium_matrix_admin";

/// Sent by a trusted view-only proxy — the demo's GET-only `/admin`
/// passthrough sends it to the server today. Deliberately the SAME header
/// name the server uses, so one proxy fronting both consoles sends one
/// header. It only ever REMOVES buttons, so a lenient parse is the safe side.
pub const VIEW_ONLY_HEADER: &str = "x-munarium-admin-view-only";

/// An authenticated admin request.
pub(crate) struct AdminCtx {
    pub caller: Caller,
    pub view_only: bool,
    /// The CSRF synchronizer token bound to this request's credential.
    pub csrf: String,
}

impl AdminCtx {
    /// Whether an action form should render as a button or as a note.
    pub fn can_act(&self) -> bool {
        !self.view_only
    }
}

// ---------------------------------------------------------------------------
// credentials
// ---------------------------------------------------------------------------

fn cookie_token(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    raw.split(';').find_map(|part| {
        let (k, v) = part.trim().split_once('=')?;
        (k == COOKIE && !v.is_empty()).then(|| v.to_string())
    })
}

fn credential(headers: &HeaderMap) -> Option<String> {
    crate::rest::bearer(headers)
        .map(String::from)
        .or_else(|| cookie_token(headers))
}

fn view_only_requested(headers: &HeaderMap) -> bool {
    headers
        .get(VIEW_ONLY_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | ""
            )
        })
        .unwrap_or(false)
}

/// mgmt-or-redirect. Bearer wins; the cookie is the browser path.
///
/// A failure redirects rather than 401s, because the overwhelmingly common
/// cause is a browser with no cookie yet and a login form is the useful
/// answer. A script reads the `Location` header and learns the same thing.
fn admin_auth(state: &AppState, headers: &HeaderMap) -> Result<AdminCtx, Response> {
    let token = credential(headers);
    match state.authenticate(token.as_deref()) {
        Ok(caller) if caller.role == "mgmt" || caller.disabled_mode => Ok(AdminCtx {
            csrf: csrf_token(state, headers),
            view_only: view_only_requested(headers),
            caller,
        }),
        // `secure()` on the redirect too. A header set only on the pages that
        // render is a header missing exactly where a redirect could be framed
        // — found by the router-level test, which is why it is a router-level
        // test.
        _ => Err(secure(Redirect::to("/admin/login").into_response())),
    }
}

/// The **rw** credential an action form presents, per submission, never
/// stored.
///
/// The role invariant the security posture states: a leaked mgmt token cannot
/// change what the system does. Reads and administration are mgmt; applying an
/// asset, running a sync, promoting a mapping are rw — the same split `/v1`
/// draws, enforced here by asking for the rw token in the form rather than by
/// quietly widening the admin's own. A valid rw token for ANOTHER tenant is
/// refused, never silently applied across the boundary.
fn rw_credential(state: &AppState, admin: &AdminCtx, token: &str) -> Result<Caller, String> {
    let token = token.trim();
    let caller = state
        .authenticate((!token.is_empty()).then_some(token))
        .map_err(|e| format!("rw credential: {}", e.detail))?;
    caller.require_rw().map_err(|e| e.detail)?;
    if caller.tenant != admin.caller.tenant {
        return Err("that rw credential belongs to a different tenant".into());
    }
    Ok(caller)
}

// ---------------------------------------------------------------------------
// CSRF
// ---------------------------------------------------------------------------

/// A stateless synchronizer token: HMAC-shaped over the per-process boot
/// secret and the presented credential.
///
/// Bound to the credential so a token minted for one operator does not
/// authorize a form submitted with another's; bound to the boot secret so it
/// dies with the process, which is why a stale form after a restart is
/// refused instead of replayed. There is no session table because there are no
/// sessions: the credential IS the session.
fn csrf_token(state: &AppState, headers: &HeaderMap) -> String {
    use sha2::Digest as _;
    let cred = credential(headers).unwrap_or_default();
    let inner = sha2::Sha256::digest(format!("{}:{cred}", state.config.boot_secret).as_bytes());
    hex::encode(sha2::Sha256::digest(
        [state.config.boot_secret.as_bytes(), &inner[..]].concat(),
    ))
}

fn csrf_ok(admin: &AdminCtx, provided: &str) -> bool {
    let expected = &admin.csrf;
    if expected.len() != provided.len() {
        return false;
    }
    // Constant-time over the fixed-length hex: a byte-at-a-time comparison
    // leaks the token's prefix through response timing.
    expected
        .bytes()
        .zip(provided.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

/// The Origin/Host check every write goes through, on top of CSRF.
///
/// Belt and braces on purpose. The synchronizer token is the primary defence;
/// this catches the case where a token has leaked into a page somewhere it
/// should not be. `Origin` is absent on some same-origin form posts, so an
/// absent header falls back to `Referer`, and an absent *both* is allowed —
/// refusing it would break `curl` against a local deployment, and the CSRF
/// token is still required. What is refused is an Origin that is PRESENT and
/// does not match the Host the request came in on.
fn origin_ok(headers: &HeaderMap) -> bool {
    let Some(host) = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
    else {
        return true;
    };
    let stated = headers
        .get(axum::http::header::ORIGIN)
        .or_else(|| headers.get(axum::http::header::REFERER))
        .and_then(|v| v.to_str().ok());
    let Some(stated) = stated else { return true };
    // Compare authorities, not whole URLs: a Referer carries a path.
    let authority = stated
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(stated);
    let authority = authority.split(['/', '?', '#']).next().unwrap_or(authority);
    authority.eq_ignore_ascii_case(host)
}

/// Everything a state-changing form must clear before it runs.
fn writable(admin: &AdminCtx, headers: &HeaderMap, csrf: &str) -> Result<(), String> {
    if admin.view_only {
        return Err("this console is being served view-only by a proxy".into());
    }
    if !origin_ok(headers) {
        return Err("origin does not match host".into());
    }
    if !csrf_ok(admin, csrf) {
        return Err("stale form (did the process restart?) — go back and retry".into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// page chrome
// ---------------------------------------------------------------------------

const NAV: &[(&str, &str)] = &[
    ("/admin", "overview"),
    ("/admin/sources", "sources"),
    ("/admin/runs", "runs"),
    ("/admin/journal", "journal"),
    ("/admin/verification", "verification"),
    ("/admin/mappings", "mappings"),
    ("/admin/registry", "registry"),
    ("/admin/author", "author"),
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
    format!(r#"<nav><span class="brand">munarium matrix{badge}</span>{links}</nav>"#)
}

/// The security headers every admin response carries.
///
/// `default-src 'self'` with **no** `script-src`: there is no JavaScript on
/// any page, so the strictest policy is also the accurate one, and a future
/// edit that adds a script will be blocked by the browser rather than shipped
/// unnoticed. `frame-ancestors 'none'` plus the legacy `X-Frame-Options` —
/// the modern directive is authoritative and the header covers a proxy that
/// strips CSP.
fn secure(mut resp: Response) -> Response {
    let h = resp.headers_mut();
    h.insert(
        "content-security-policy",
        HeaderValue::from_static(
            "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; \
             form-action 'self'; frame-ancestors 'none'; base-uri 'none'; object-src 'none'",
        ),
    );
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    // `same-origin`, NOT `no-referrer` (2026-08-30). Under `no-referrer` the
    // Fetch spec sets the `Origin` header of a non-CORS request — a form
    // POST — to `null`, so every browser submission arrived as `Origin:
    // null`, failed the Origin/Host check, and the login form could not log
    // anyone in. The conformance tier never saw it: reqwest sends no Origin
    // at all, which the check treats as "not a browser, fine". The first run
    // of the browser tier (`ui-smoke`) found it in its second assertion.
    // `same-origin` still sends nothing to a third party.
    h.insert("referrer-policy", HeaderValue::from_static("same-origin"));
    // These pages read live operational state; a cached copy in a shared
    // proxy would show one operator another's tenant.
    h.insert(
        "cache-control",
        HeaderValue::from_static("no-store, max-age=0"),
    );
    resp
}

fn page(active: &str, view_only: bool, title: &str, body: &str) -> Html<String> {
    Html(format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>{} — munarium matrix</title>{}</head><body>{}<main><h1>{}</h1>{body}</main></body></html>"#,
        chrome::esc(title),
        chrome::STYLE,
        nav(active, view_only),
        chrome::esc(title),
    ))
}

pub(crate) fn render(admin: &AdminCtx, active: &str, title: &str, body: &str) -> Response {
    secure(page(active, admin.view_only, title, body).into_response())
}

pub(crate) fn notice(admin: &AdminCtx, active: &str, title: &str, msg: &str) -> Response {
    render(
        admin,
        active,
        title,
        &format!(r#"<div class="notice">{}</div>"#, chrome::esc(msg)),
    )
}

/// A page whose only source failed. A missing object is 404 so scripts and
/// proxies see the truth; anything else stays 200 with the message on it,
/// because a 500 on a console page hides the very diagnosis it is showing.
pub(crate) fn error_page(admin: &AdminCtx, active: &str, title: &str, msg: &str) -> Response {
    let missing = msg.contains("not found") || msg.contains("unknown");
    let status = if missing {
        StatusCode::NOT_FOUND
    } else {
        StatusCode::OK
    };
    let body = format!(r#"<div class="notice">{}</div>"#, chrome::esc(msg));
    secure((status, page(active, admin.view_only, title, &body)).into_response())
}

/// A hidden CSRF field. Every state-changing form carries one.
pub(crate) fn csrf_field(admin: &AdminCtx) -> String {
    format!(
        r#"<input type="hidden" name="csrf" value="{}">"#,
        chrome::esc(&admin.csrf)
    )
}

/// An action, rendered as a button — or as a note when a view-only proxy is
/// fronting the console, so a page that cannot POST never offers a button
/// that would fail behind it.
pub(crate) fn action(
    admin: &AdminCtx,
    method_path: &str,
    label: &str,
    extra_fields: &str,
    danger: bool,
) -> String {
    if !admin.can_act() {
        return format!(r#"<span class="note">{}</span>"#, chrome::esc(label));
    }
    let class = if danger { r#" class="danger""# } else { "" };
    format!(
        r#"<form class="act" method="post" action="{}">{}{extra_fields}<button type="submit"{class}>{}</button></form>"#,
        chrome::esc(method_path),
        csrf_field(admin),
        chrome::esc(label)
    )
}

// ---------------------------------------------------------------------------
// login
// ---------------------------------------------------------------------------

fn login_page(error: Option<&str>) -> Response {
    let err = error
        .map(|e| format!(r#"<div class="notice">{}</div>"#, chrome::esc(e)))
        .unwrap_or_default();
    secure(
        Html(format!(
            r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>munarium matrix admin</title>{}</head><body>
<form class="login" method="post" action="/admin/login">
<h1>munarium matrix</h1>{err}
<label for="token">management token</label>
<input id="token" name="token" type="password" autocomplete="current-password" autofocus>
<button type="submit">sign in</button>
</form></body></html>"#,
            chrome::STYLE
        ))
        .into_response(),
    )
}

async fn login_form() -> Response {
    login_page(None)
}

#[derive(Debug, serde::Deserialize)]
pub struct LoginForm {
    #[serde(default)]
    token: String,
}

async fn login_submit(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Form(form): axum::extract::Form<LoginForm>,
) -> Response {
    // A login is itself a state change (it mints a cookie), so it takes the
    // same Origin check. It cannot take a CSRF token — there is no session to
    // bind one to yet, which is the one honest exception.
    if !origin_ok(&headers) {
        return login_page(Some("origin does not match host"));
    }
    match state.authenticate(Some(form.token.trim())) {
        Ok(c) if c.role == "mgmt" || c.disabled_mode => {
            // `Secure` is set when the request arrived over TLS. Behind ACA
            // ingress that is `X-Forwarded-Proto`, and behind nothing it is
            // absent — a `Secure` cookie on a plain-http loopback deployment
            // is a cookie the browser silently drops, which reads as "login
            // does not work".
            let https = headers
                .get("x-forwarded-proto")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.eq_ignore_ascii_case("https"))
                .unwrap_or(false);
            let secure_attr = if https { "; Secure" } else { "" };
            let cookie = format!(
                "{COOKIE}={}; HttpOnly; SameSite=Strict; Path=/admin{secure_attr}",
                form.token.trim()
            );
            // A token carrying a byte a header value cannot hold produces no
            // cookie. Redirecting anyway would send the operator to /admin,
            // which redirects straight back here — an apparent loop with no
            // explanation. Say what happened instead.
            let Ok(v) = HeaderValue::from_str(&cookie) else {
                return login_page(Some(
                    "that token contains a character a cookie cannot carry; \
                     use the Authorization header instead",
                ));
            };
            let mut resp = secure(Redirect::to("/admin").into_response());
            resp.headers_mut().insert(axum::http::header::SET_COOKIE, v);
            resp
        }
        Ok(_) => login_page(Some("that token is not a management (mgmt) token")),
        Err(_) => login_page(Some("invalid token")),
    }
}

async fn logout() -> Response {
    let mut resp = secure(Redirect::to("/admin/login").into_response());
    if let Ok(v) = HeaderValue::from_str(&format!(
        "{COOKIE}=; HttpOnly; SameSite=Strict; Path=/admin; Max-Age=0"
    )) {
        resp.headers_mut().insert(axum::http::header::SET_COOKIE, v);
    }
    resp
}

// ---------------------------------------------------------------------------
// router
// ---------------------------------------------------------------------------

/// Merged into the REST router by `rest::router` **only** when the role serves
/// the control plane and `MUNARIUM_MATRIX_ADMIN` is enabled. Absent otherwise
/// — a 404 from a missing route, not from a guard.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/admin", get(observe::overview))
        .route("/admin/sources", get(observe::sources))
        .route("/admin/sources/{name}", get(observe::source))
        .route("/admin/runs", get(observe::runs))
        .route("/admin/journal", get(observe::journal))
        .route("/admin/verification", get(observe::verification))
        .route("/admin/mappings", get(observe::mappings))
        .route("/admin/mappings/{name}", get(observe::mapping))
        // configure
        .route("/admin/registry", get(configure::registry))
        .route("/admin/registry/{kind}/{name}", get(configure::asset))
        .route(
            "/admin/author",
            get(configure::author).post(configure::draft),
        )
        .route("/admin/author/export", post(configure::export))
        .route("/admin/author/apply", post(configure::apply))
        // operate
        .route("/admin/sources/{name}/probe", post(operate::probe))
        .route(
            "/admin/sources/{name}/introspect",
            post(operate::introspect),
        )
        .route("/admin/sources/{name}/sync", post(operate::sync))
        .route("/admin/contracts/{name}/verify", post(operate::verify))
        .route("/admin/mappings/{name}/run", post(operate::run_mapping))
        .route("/admin/mappings/{name}/promote", post(operate::promote))
        .route("/admin/mappings/{name}/demote", post(operate::demote))
        .route("/admin/login", get(login_form).post(login_submit))
        .route("/admin/logout", post(logout))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn a_present_origin_that_disagrees_with_host_is_refused() {
        assert!(!origin_ok(&headers(&[
            ("host", "matrix.example"),
            ("origin", "https://evil.example"),
        ])));
        assert!(origin_ok(&headers(&[
            ("host", "matrix.example"),
            ("origin", "https://matrix.example"),
        ])));
    }

    #[test]
    fn an_absent_origin_is_allowed_because_curl_sends_none() {
        // The CSRF token is still required; this check is the second lock,
        // and refusing here would break a local `curl` for no security gain.
        assert!(origin_ok(&headers(&[("host", "matrix.example")])));
    }

    #[test]
    fn a_referer_is_compared_by_authority_not_by_whole_url() {
        assert!(origin_ok(&headers(&[
            ("host", "matrix.example"),
            ("referer", "https://matrix.example/admin/sources?x=1"),
        ])));
    }

    #[test]
    fn view_only_is_on_for_anything_but_an_explicit_off() {
        assert!(view_only_requested(&headers(&[(VIEW_ONLY_HEADER, "1")])));
        assert!(view_only_requested(&headers(&[(VIEW_ONLY_HEADER, "true")])));
        assert!(!view_only_requested(&headers(&[(VIEW_ONLY_HEADER, "0")])));
        assert!(!view_only_requested(&headers(&[(VIEW_ONLY_HEADER, "no")])));
        assert!(!view_only_requested(&HeaderMap::new()));
    }

    #[test]
    fn the_cookie_is_read_out_of_a_multi_cookie_header() {
        let h = headers(&[(
            "cookie",
            "other=1; __munarium_matrix_admin=mxmgmt; another=2",
        )]);
        assert_eq!(cookie_token(&h).as_deref(), Some("mxmgmt"));
    }

    #[test]
    fn a_bearer_beats_the_cookie() {
        let h = headers(&[
            ("authorization", "Bearer from-header"),
            ("cookie", "__munarium_matrix_admin=from-cookie"),
        ]);
        assert_eq!(credential(&h).as_deref(), Some("from-header"));
    }

    #[test]
    fn every_nav_entry_is_a_route_this_module_serves() {
        // A nav link to a 404 is the kind of rot nobody notices until an
        // operator clicks it during an incident.
        // The path string anywhere in this module, rather than immediately
        // after `.route(` — `cargo fmt` breaks a long route call across lines
        // and the tighter pattern went from "catches dead nav links" to
        // "catches whatever rustfmt did last". Weaker, and still fails the
        // moment a nav entry names a route nobody wrote.
        let src = include_str!("mod.rs");
        for (href, _) in NAV {
            assert!(
                src.matches(&format!("\"{href}\"")).count() >= 2,
                "nav links {href} but no route serves it"
            );
        }
    }
}
