// SPDX-License-Identifier: Apache-2.0
//! The console's security properties, asserted at the ROUTER.
//!
//! Not unit tests of the helpers: those live beside them in `mod.rs` and prove
//! the helpers are right. These drive the assembled router, because the
//! properties that matter are properties of what a browser can actually reach.
//! The distinction earned its keep immediately — a correct `origin_ok` behind a
//! route that never calls it is a check that does not exist.
//!
//! Every test here names the thing an attacker would try.

use super::*;
use crate::config::{AuthMode, Config, Role};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use munarium_matrix_store::MatrixStore;
use tower::ServiceExt;

fn config(role: Role, auth: AuthMode, admin_enabled: bool) -> Config {
    Config {
        role,
        http_addr: "127.0.0.1:0".into(),
        ops_addr: "127.0.0.1:0".into(),
        grpc_addr: None,
        database_url: Some("postgres://unused".into()),
        db_max_conns: 1,
        auth,
        server_url: None,
        server_token_ref: None,
        target_server_version: "0.5.0".into(),
        max_concurrency: 8,
        egress_default_deny: true,
        log_format_json: false,
        instance_id: "test".into(),
        file_root: None,
        promotion_min_identity_precision: 0.95,
        promotion_min_value_conformance: 0.99,
        admin_enabled,
        boot_secret: "test-boot-secret".into(),
    }
}

/// Static tokens covering all three roles, so "mgmt-only" can be tested
/// against tokens that are valid and simply not mgmt — the interesting case.
fn tokens() -> AuthMode {
    AuthMode::Static(
        [("mxrw", "rw"), ("mxro", "ro"), ("mxmgmt", "mgmt")]
            .into_iter()
            .map(|(token, role)| crate::config::StaticToken {
                token: token.into(),
                tenant: "t1".into(),
                role: role.into(),
            })
            .collect(),
    )
}

fn state(role: Role, admin_enabled: bool) -> Arc<AppState> {
    AppState::new(
        config(role, tokens(), admin_enabled),
        MatrixStore::disconnected_for_tests(),
    )
}

async fn send(state: Arc<AppState>, req: Request<Body>) -> (StatusCode, HeaderMap, String) {
    let resp = crate::rest::router(state).oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

fn get(path: &str, token: Option<&str>) -> Request<Body> {
    let mut b = Request::get(path).header("host", "matrix.test");
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    b.body(Body::empty()).unwrap()
}

// ---------------------------------------------------------------------------
// reachability
// ---------------------------------------------------------------------------

#[tokio::test]
async fn only_a_control_role_serves_the_console() {
    // The surface is ABSENT on the other roles, not guarded — a 404 from a
    // missing route, which is why there is no check here to misconfigure.
    for role in [Role::Query, Role::Sync, Role::Reconcile] {
        let (status, _, _) = send(state(role, true), get("/admin", Some("mxmgmt"))).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{role:?} must not serve /admin"
        );
    }
    for role in [Role::Control, Role::All] {
        let (status, _, _) = send(state(role, true), get("/admin", Some("mxmgmt"))).await;
        assert_ne!(status, StatusCode::NOT_FOUND, "{role:?} must serve /admin");
    }
}

#[tokio::test]
async fn admin_disabled_removes_every_route_including_the_login_form() {
    // A hardened deployment turns the console off; a login page that still
    // answered would advertise a console that is not there.
    for path in [
        "/admin",
        "/admin/login",
        "/admin/registry",
        "/admin/journal",
    ] {
        let (status, _, _) = send(state(Role::All, false), get(path, Some("mxmgmt"))).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path} with ADMIN=disabled");
    }
    // ...and the API is untouched by the switch.
    let (status, _, _) = send(state(Role::All, false), get("/version", None)).await;
    assert_eq!(status, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// authentication
// ---------------------------------------------------------------------------

#[tokio::test]
async fn anonymous_and_non_mgmt_tokens_are_redirected_to_login() {
    // rw and ro are VALID credentials. The console is mgmt-only, and the
    // interesting failure is a good token with the wrong role — not a bad one.
    for token in [None, Some("mxrw"), Some("mxro"), Some("not-a-token")] {
        let (status, headers, _) = send(state(Role::All, true), get("/admin", token)).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{token:?}");
        assert_eq!(
            headers.get("location").unwrap(),
            "/admin/login",
            "{token:?}"
        );
    }
    let (status, _, _) = send(state(Role::All, true), get("/admin", Some("mxmgmt"))).await;
    assert_ne!(status, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn every_read_page_is_mgmt_only() {
    // Enumerated rather than spot-checked: a page added without auth is
    // exactly the kind of hole nobody notices, so the list is the test.
    for path in [
        "/admin",
        "/admin/sources",
        "/admin/runs",
        "/admin/journal",
        "/admin/verification",
        "/admin/mappings",
        "/admin/registry",
        "/admin/author",
    ] {
        let (status, headers, _) = send(state(Role::All, true), get(path, Some("mxro"))).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{path} must refuse ro");
        assert_eq!(headers.get("location").unwrap(), "/admin/login", "{path}");
    }
}

// ---------------------------------------------------------------------------
// headers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_admin_response_carries_the_security_headers() {
    // Including the login page and the redirect — a header set only on the
    // pages that render is a header missing exactly where a redirect could be
    // framed.
    for (path, token) in [
        ("/admin/login", None),
        ("/admin", Some("mxmgmt")),
        ("/admin", None),
    ] {
        let (_, headers, _) = send(state(Role::All, true), get(path, token)).await;
        let csp = headers
            .get("content-security-policy")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert!(csp.contains("default-src 'self'"), "{path}: {csp}");
        // There is no JavaScript on any page, so the strictest policy is also
        // the accurate one: a future edit that adds a script is blocked by the
        // browser rather than shipped unnoticed.
        assert!(
            !csp.contains("script-src"),
            "{path} must declare no script source"
        );
        assert!(csp.contains("frame-ancestors 'none'"), "{path}");
        assert_eq!(headers.get("x-frame-options").unwrap(), "DENY", "{path}");
        assert_eq!(
            headers.get("x-content-type-options").unwrap(),
            "nosniff",
            "{path}"
        );
        // `same-origin`, never `no-referrer`: the latter makes a browser send
        // `Origin: null` on a form POST, which the Origin check then refuses —
        // the console's own login was unusable from a browser until the
        // browser tier found it (2026-08-30).
        assert_eq!(headers.get("referrer-policy").unwrap(), "same-origin");
        // These pages read live operational state; a cached copy in a shared
        // proxy would show one operator another tenant's rows.
        assert!(headers
            .get("cache-control")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("no-store"));
    }
}

#[tokio::test]
async fn the_login_cookie_is_httponly_samesite_strict_and_scoped_to_admin() {
    let req = Request::post("/admin/login")
        .header("host", "matrix.test")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from("token=mxmgmt"))
        .unwrap();
    let (status, headers, _) = send(state(Role::All, true), req).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let cookie = headers.get("set-cookie").unwrap().to_str().unwrap();
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(cookie.contains("SameSite=Strict"), "{cookie}");
    assert!(cookie.contains("Path=/admin"), "{cookie}");
    // No `Secure` on a plain-http request: a Secure cookie there is one the
    // browser silently drops, which reads as "login does not work".
    assert!(!cookie.contains("Secure"), "{cookie}");
}

#[tokio::test]
async fn behind_tls_termination_the_cookie_is_secure() {
    let req = Request::post("/admin/login")
        .header("host", "matrix.test")
        .header("x-forwarded-proto", "https")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from("token=mxmgmt"))
        .unwrap();
    let (_, headers, _) = send(state(Role::All, true), req).await;
    let cookie = headers.get("set-cookie").unwrap().to_str().unwrap();
    assert!(cookie.contains("Secure"), "{cookie}");
}

#[tokio::test]
async fn a_non_mgmt_login_sets_no_cookie() {
    for token in ["mxrw", "mxro", "wrong"] {
        let req = Request::post("/admin/login")
            .header("host", "matrix.test")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(format!("token={token}")))
            .unwrap();
        let (status, headers, body) = send(state(Role::All, true), req).await;
        assert_eq!(status, StatusCode::OK, "{token}");
        assert!(headers.get("set-cookie").is_none(), "{token}");
        assert!(body.contains("form"), "{token}");
    }
}

// ---------------------------------------------------------------------------
// CSRF and origin
// ---------------------------------------------------------------------------

fn post_form(path: &str, token: &str, origin: Option<&str>, body: &str) -> Request<Body> {
    let mut b = Request::post(path)
        .header("host", "matrix.test")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/x-www-form-urlencoded");
    if let Some(o) = origin {
        b = b.header("origin", o);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

/// The CSRF token this credential would be issued. Derived the same way the
/// page derives it, so the test proves the ROUTE checks it rather than proving
/// the helper agrees with itself.
fn csrf_for(state: &AppState, token: &str) -> String {
    let mut h = HeaderMap::new();
    h.insert(
        axum::http::header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
    );
    csrf_token(state, &h)
}

#[tokio::test]
async fn a_write_without_a_csrf_token_is_refused() {
    let st = state(Role::All, true);
    let (status, _, body) = send(
        st,
        post_form("/admin/sources/crm/probe", "mxmgmt", None, "rw_token=mxrw"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("stale form"), "{body}");
}

#[tokio::test]
async fn a_write_with_another_credentials_csrf_token_is_refused() {
    // The token is bound to the credential presented. Lifting one out of an
    // operator's page and replaying it under a different credential must not
    // work.
    let st = state(Role::All, true);
    let wrong = csrf_for(&st, "mxrw");
    let (_, _, body) = send(
        st,
        post_form(
            "/admin/sources/crm/probe",
            "mxmgmt",
            None,
            &format!("csrf={wrong}&rw_token=mxrw"),
        ),
    )
    .await;
    assert!(body.contains("stale form"), "{body}");
}

#[tokio::test]
async fn a_csrf_token_from_a_previous_process_is_refused() {
    // Bound to the boot secret, so it dies with the process: a form left open
    // across a restart is refused rather than replayed.
    let old = AppState::new(
        {
            let mut c = config(Role::All, tokens(), true);
            c.boot_secret = "a-previous-process".into();
            c
        },
        MatrixStore::disconnected_for_tests(),
    );
    let stale = csrf_for(&old, "mxmgmt");
    let (_, _, body) = send(
        state(Role::All, true),
        post_form(
            "/admin/sources/crm/probe",
            "mxmgmt",
            None,
            &format!("csrf={stale}&rw_token=mxrw"),
        ),
    )
    .await;
    assert!(body.contains("stale form"), "{body}");
}

#[tokio::test]
async fn a_cross_origin_post_is_refused_even_with_a_valid_csrf_token() {
    // Belt and braces: the synchronizer token is the primary defence, and this
    // catches the case where one has leaked somewhere it should not be.
    let st = state(Role::All, true);
    let good = csrf_for(&st, "mxmgmt");
    let (_, _, body) = send(
        st,
        post_form(
            "/admin/sources/crm/probe",
            "mxmgmt",
            Some("https://evil.example"),
            &format!("csrf={good}&rw_token=mxrw"),
        ),
    )
    .await;
    assert!(body.contains("origin does not match host"), "{body}");
}

#[tokio::test]
async fn a_valid_csrf_and_origin_reaches_the_rw_credential_check() {
    // Past both locks the request proceeds to the credential — and an EMPTY
    // rw token is refused there, which is the role invariant: a leaked mgmt
    // token cannot act.
    let st = state(Role::All, true);
    let good = csrf_for(&st, "mxmgmt");
    let (_, _, body) = send(
        st,
        post_form(
            "/admin/sources/crm/probe",
            "mxmgmt",
            Some("https://matrix.test"),
            &format!("csrf={good}&rw_token="),
        ),
    )
    .await;
    assert!(body.contains("rw credential"), "{body}");
    assert!(!body.contains("stale form"), "{body}");
}

#[tokio::test]
async fn an_mgmt_token_offered_as_the_rw_credential_is_refused() {
    // The whole point of asking for a second credential. mgmt reads and
    // administers; it does not apply assets or run work.
    let st = state(Role::All, true);
    let good = csrf_for(&st, "mxmgmt");
    let (_, _, body) = send(
        st,
        post_form(
            "/admin/sources/crm/probe",
            "mxmgmt",
            None,
            &format!("csrf={good}&rw_token=mxmgmt"),
        ),
    )
    .await;
    assert!(body.contains("cannot execute commands"), "{body}");
}

// ---------------------------------------------------------------------------
// view-only proxy
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_view_only_proxy_gets_notes_instead_of_buttons_and_writes_are_refused() {
    let st = state(Role::All, true);
    let req = Request::get("/admin/sources")
        .header("host", "matrix.test")
        .header("authorization", "Bearer mxmgmt")
        .header(VIEW_ONLY_HEADER, "1")
        .body(Body::empty())
        .unwrap();
    let (_, _, body) = send(st.clone(), req).await;
    assert!(body.contains("view-only"), "the badge is on the page");

    let good = csrf_for(&st, "mxmgmt");
    let mut req = post_form(
        "/admin/sources/crm/probe",
        "mxmgmt",
        None,
        &format!("csrf={good}&rw_token=mxrw"),
    );
    req.headers_mut()
        .insert(VIEW_ONLY_HEADER, HeaderValue::from_static("1"));
    let (_, _, body) = send(st, req).await;
    // The header removes buttons AND refuses the write. Rendering-only would
    // leave a POST that a determined client could still make behind a proxy
    // that cannot pass it.
    assert!(body.contains("view-only"), "{body}");
}

// ---------------------------------------------------------------------------
// what must never be on a page
// ---------------------------------------------------------------------------

#[test]
fn no_page_renders_a_credential_or_resolves_evidence() {
    // A grep over this module's own source, which is a blunt instrument and
    // the right one: the failure it guards against is someone adding a field
    // to a table without thinking about what is in it.
    let sources = [
        include_str!("observe.rs"),
        include_str!("configure.rs"),
        include_str!("operate.rs"),
    ];
    for src in sources {
        for banned in [
            "resolve_secret",
            "credential_ref.clone()",
            "evidence_rows",
            "resolve_evidence",
        ] {
            assert!(
                !src.contains(banned),
                "an admin page must not reach for `{banned}` — secrets and evidence \
                 rows are not console material"
            );
        }
    }
}

#[test]
fn the_login_form_never_echoes_the_token_back() {
    // A password field re-rendered with its value is a credential in the
    // page source and in the browser's back-forward cache.
    let src = include_str!("mod.rs");
    let form_start = src.find("form class=\"login\"").expect("the login form");
    let form = &src[form_start..form_start + 400];
    assert!(form.contains(r#"type="password""#));
    assert!(!form.contains("value="), "the token field carries no value");
}
