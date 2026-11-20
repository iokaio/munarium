// SPDX-License-Identifier: Apache-2.0
//! B3-T1 and B3-T2: admission by work class with a control reserve
//! (ClearView R7, `admission.rs`).
//!
//! The saturation pattern follows `ops.rs`: a test holds the ordinary pool's
//! permits directly, so the next request meets a full plane without needing
//! slow handlers.

use crate::admission::{
    self, AdmissionConfig, ControlAuth, PlaneAdmission, WorkClass, GRPC_CONTROL, GRPC_HEAVY,
    REST_CONTROL, REST_HEAVY,
};
use crate::config::{AuthMode, Config, DocIntelConfig, SourceStoreConfig, StoreKind};
use crate::state::AppState;
use axum::body::Body;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tower::{Service, ServiceExt};

fn test_config(max_concurrency: usize, auth: AuthMode) -> Config {
    Config {
        http_addr: "127.0.0.1:0".into(),
        grpc_addr: None,
        ops_addr: "127.0.0.1:0".into(),
        store: StoreKind::Memory,
        database_url: None,
        platform: None,
        auth,
        shutdown_grace_secs: 1,
        token_secret: None,
        token_ttl_secs: 3600,
        require_uid: false,
        interaction_body_max: 32768,
        token_revocation_check: false,
        matrix_base_url: None,
        matrix_admin_url: None,
        max_concurrency,
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

/// mgmt "m", rw "w", ro "r", all in tenant "t".
fn static_auth() -> AuthMode {
    AuthMode::Static(vec![
        ("m".into(), "t".into(), "mgmt".into()),
        ("w".into(), "t".into(), "rw".into()),
        ("r".into(), "t".into(), "ro".into()),
    ])
}

async fn state_with(ceiling: usize, reserve: usize, heavy: usize, auth: AuthMode) -> Arc<AppState> {
    let admission = AdmissionConfig::new(ceiling, reserve, heavy).expect("feasible admission");
    AppState::new_with_admission(test_config(ceiling, auth), admission)
        .await
        .expect("state")
}

/// Every ordinary permit of a plane, held until dropped.
fn hold_all(sem: &Arc<tokio::sync::Semaphore>) -> tokio::sync::OwnedSemaphorePermit {
    let n = sem.available_permits() as u32;
    sem.clone().try_acquire_many_owned(n).expect("hold permits")
}

struct Reply {
    status: u16,
    retry_after: Option<String>,
    body: String,
}

impl Reply {
    fn shed(&self) -> bool {
        self.status == 503 && self.body.contains("overloaded")
    }
}

/// Replace every `{param}` with a concrete segment.
fn concrete(template: &str) -> String {
    template
        .split('/')
        .map(|seg| {
            if seg.starts_with('{') && seg.ends_with('}') {
                "x"
            } else {
                seg
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

async fn call(state: &Arc<AppState>, method: &str, template: &str, token: Option<&str>) -> Reply {
    let mut req = axum::http::Request::builder()
        .method(method)
        .uri(concrete(template))
        .header("x-munarium-uid", "tester")
        .header("content-type", "application/json");
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    let resp = crate::rest::router(state.clone())
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let retry_after = resp
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    Reply {
        status,
        retry_after,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

fn metric(state: &AppState, series: &str) -> u64 {
    let rendered = crate::metrics::render(state);
    rendered
        .lines()
        .find_map(|l| l.strip_prefix(series)?.strip_prefix(' ')?.parse().ok())
        .unwrap_or(0)
}

const ROLLOUT: (&str, &str) = ("PUT", "/v1/retrieval-rollout");
const REVOKE: (&str, &str) = ("POST", "/v1/access-tokens/{jti}/revoke");
const CANCEL: (&str, &str) = ("POST", "/v1/index-build-jobs/{job_id}/cancel");
const JOB_STATUS: (&str, &str) = ("GET", "/v1/index-build-jobs/{job_id}");
const ORDINARY: (&str, &str) = ("GET", "/v1/versions/{version_id}/head");
const HEAVY: (&str, &str) = ("POST", "/v1/indexes/{shape_ref}/build");
const REST_RESERVE: &str =
    r#"munarium_admission_total{plane="rest",class="control",slot="reserve"}"#;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[test]
fn defaults_are_v1_4_and_an_infeasible_reserve_is_rejected() {
    let none = |_: &str| None;
    // No settings: the v1.4 shape, including the one-slot ceiling.
    assert_eq!(
        AdmissionConfig::from_lookup(512, none).unwrap(),
        AdmissionConfig::legacy(512)
    );
    assert_eq!(
        AdmissionConfig::from_lookup(1, none).unwrap(),
        AdmissionConfig::legacy(1)
    );
    let set = |reserve: &'static str, heavy: &'static str| {
        move |k: &str| match k {
            "MUNARIUM_CONTROL_RESERVE" => Some(reserve.to_string()),
            "MUNARIUM_HEAVY_CONCURRENCY" => Some(heavy.to_string()),
            _ => None,
        }
    };
    assert_eq!(
        AdmissionConfig::from_lookup(1, set("0", "")).unwrap(),
        AdmissionConfig::legacy(1),
        "explicit zeros and empty values are off"
    );
    // One reserved slot on a one-slot plane leaves no ordinary slot.
    let err = AdmissionConfig::from_lookup(1, set("1", "0")).unwrap_err();
    assert!(err.contains("MUNARIUM_CONTROL_RESERVE"), "{err}");
    assert!(err.contains("at least one ordinary slot"), "{err}");
    assert!(AdmissionConfig::from_lookup(4, set("4", "0")).is_err());
    assert!(AdmissionConfig::from_lookup(4, set("9", "0")).is_err());
    let ok = AdmissionConfig::from_lookup(4, set("3", "1")).unwrap();
    assert_eq!(
        (ok.ordinary(), ok.control_reserve, ok.heavy_limit),
        (1, 3, 1)
    );
    // The heavy bound lives inside the ordinary capacity.
    let err = AdmissionConfig::from_lookup(4, set("1", "4")).unwrap_err();
    assert!(err.contains("MUNARIUM_HEAVY_CONCURRENCY"), "{err}");
    assert!(AdmissionConfig::from_lookup(4, set("1", "3")).is_ok());
    // Unparseable values name their variable.
    let err = AdmissionConfig::from_lookup(4, set("two", "0")).unwrap_err();
    assert!(err.starts_with("MUNARIUM_CONTROL_RESERVE"), "{err}");
    let err = AdmissionConfig::from_lookup(4, set("0", "-1")).unwrap_err();
    assert!(err.starts_with("MUNARIUM_HEAVY_CONCURRENCY"), "{err}");
}

#[tokio::test]
async fn the_default_state_is_one_full_pool_per_plane() {
    let state = AppState::new(test_config(7, static_auth())).await.unwrap();
    assert_eq!(state.admission.rest.ordinary.available_permits(), 7);
    assert_eq!(state.admission.grpc.ordinary.available_permits(), 7);
    for plane in [&state.admission.rest, &state.admission.grpc] {
        assert_eq!(plane.reserve.available_permits(), 0);
        assert!(plane.heavy.is_none());
    }
    let rendered = crate::metrics::render(&state);
    assert!(
        rendered.contains(r#"munarium_admission_slots{plane="rest",pool="ordinary"} 7"#),
        "{rendered}"
    );
    assert!(!rendered.contains(r#"pool="reserve""#), "{rendered}");
    assert!(!rendered.contains(r#"pool="heavy""#), "{rendered}");
    // A mismatched ceiling cannot be smuggled in.
    assert!(AppState::new_with_admission(
        test_config(7, static_auth()),
        AdmissionConfig::legacy(8)
    )
    .await
    .is_err());
}

/// The v1.4 single-slot plane: a control operation is shed like any other
/// request, before authentication, with Retry-After.
#[tokio::test]
async fn legacy_single_slot_plane_sheds_every_class_as_v1_4_did() {
    let state = AppState::new(test_config(1, static_auth())).await.unwrap();
    let _held = hold_all(&state.admission.rest.ordinary);
    for (method, template) in [ORDINARY, ROLLOUT, REVOKE, HEAVY] {
        for token in [Some("m"), Some("w"), None] {
            let r = call(&state, method, template, token).await;
            assert!(r.shed(), "{method} {template}: {} {}", r.status, r.body);
            assert_eq!(r.retry_after.as_deref(), Some("1"));
        }
    }
    assert_eq!(metric(&state, "munarium_load_shed_total"), 12);
    assert_eq!(metric(&state, REST_RESERVE), 0);
}

// ---------------------------------------------------------------------------
// B3-T1: REST
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rest_reserve_admits_only_authorized_control_after_ordinary_is_full() {
    let state = state_with(3, 1, 0, static_auth()).await;
    assert_eq!(state.admission.rest.ordinary.available_permits(), 2);
    let held = hold_all(&state.admission.rest.ordinary);

    // The next ordinary request is shed, as in v1.4.
    let r = call(&state, ORDINARY.0, ORDINARY.1, Some("w")).await;
    assert!(r.shed(), "{} {}", r.status, r.body);
    assert_eq!(r.retry_after.as_deref(), Some("1"));

    // Callers without the route's own authority cannot consume the reserve:
    // they are shed exactly as they would have been without one.
    for (op, token) in [
        (ROLLOUT, None),
        (ROLLOUT, Some("r")),
        (ROLLOUT, Some("m")),
        (ROLLOUT, Some("not-a-token")),
        (CANCEL, Some("r")),
        (CANCEL, Some("m")),
        (REVOKE, Some("w")),
        (REVOKE, Some("r")),
        (JOB_STATUS, None),
        (JOB_STATUS, Some("not-a-token")),
    ] {
        let r = call(&state, op.0, op.1, token).await;
        assert!(r.shed(), "{op:?} as {token:?}: {} {}", r.status, r.body);
        assert_eq!(r.retry_after.as_deref(), Some("1"));
    }
    assert_eq!(metric(&state, REST_RESERVE), 0);

    // Authorized control operations get a reserved slot and reach their
    // handler (whatever it answers on the memory store, it is not a shed).
    for (op, token) in [
        (ROLLOUT, "w"),
        (CANCEL, "w"),
        (REVOKE, "m"),
        (JOB_STATUS, "r"),
        (JOB_STATUS, "m"),
    ] {
        let r = call(&state, op.0, op.1, Some(token)).await;
        assert!(!r.shed(), "{op:?} as {token}: {} {}", r.status, r.body);
    }
    assert_eq!(metric(&state, REST_RESERVE), 5);

    // With the reserve itself in use, a further control request is shed.
    let reserve = hold_all(&state.admission.rest.reserve);
    let r = call(&state, ROLLOUT.0, ROLLOUT.1, Some("w")).await;
    assert!(r.shed(), "{} {}", r.status, r.body);
    drop(reserve);

    // With an ordinary slot free, control takes it and leaves the reserve.
    drop(held);
    let r = call(&state, ROLLOUT.0, ROLLOUT.1, Some("w")).await;
    assert!(!r.shed());
    assert_eq!(metric(&state, REST_RESERVE), 5);
    assert_eq!(
        metric(
            &state,
            r#"munarium_admission_total{plane="rest",class="control",slot="ordinary"}"#
        ),
        1
    );
    assert_eq!(
        metric(
            &state,
            r#"munarium_admission_shed_total{plane="rest",class="control"}"#
        ),
        11
    );
    assert_eq!(
        metric(
            &state,
            r#"munarium_admission_shed_total{plane="rest",class="ordinary"}"#
        ),
        1
    );
}

/// AUTH_MODE=disabled passes every role gate in the handlers, so it passes
/// the reserve's gate too: the reserve mirrors the handler, not a stricter rule.
#[tokio::test]
async fn disabled_auth_mode_mirrors_the_handlers_role_gates() {
    let state = state_with(2, 1, 0, AuthMode::Disabled).await;
    let _held = hold_all(&state.admission.rest.ordinary);
    for op in [ROLLOUT, REVOKE] {
        let r = call(&state, op.0, op.1, None).await;
        assert!(!r.shed(), "{op:?}: {} {}", r.status, r.body);
    }
    let r = call(&state, ORDINARY.0, ORDINARY.1, None).await;
    assert!(r.shed());
}

#[tokio::test]
async fn heavy_work_is_bounded_inside_the_ordinary_slots_and_never_reserved() {
    let state = state_with(3, 1, 1, static_auth()).await;

    // The heavy bound is full; ordinary slots are free. Heavy is shed, while
    // ordinary work is still admitted.
    let heavy = hold_all(state.admission.rest.heavy.as_ref().unwrap());
    let r = call(&state, HEAVY.0, HEAVY.1, Some("w")).await;
    assert!(r.shed(), "{} {}", r.status, r.body);
    assert_eq!(r.retry_after.as_deref(), Some("1"));
    let r = call(&state, ORDINARY.0, ORDINARY.1, Some("w")).await;
    assert!(!r.shed());
    drop(heavy);

    // Heavy token free, ordinary slots full, reserve free: heavy is still
    // shed — it never uses the reserve — and the heavy token is returned.
    let held = hold_all(&state.admission.rest.ordinary);
    for token in ["w", "m"] {
        let r = call(&state, HEAVY.0, HEAVY.1, Some(token)).await;
        assert!(r.shed(), "{} {}", r.status, r.body);
    }
    // POST /v1/runs/.../approve is heavy, not control.
    let r = call(
        &state,
        "POST",
        "/v1/runs/{run_id}/steps/{ordinal}/approve",
        Some("w"),
    )
    .await;
    assert!(r.shed());
    assert_eq!(metric(&state, REST_RESERVE), 0);
    assert_eq!(
        state
            .admission
            .rest
            .heavy
            .as_ref()
            .unwrap()
            .available_permits(),
        1,
        "a shed heavy request must release its heavy token"
    );
    assert_eq!(state.admission.rest.reserve.available_permits(), 1);
    drop(held);

    // Room everywhere: heavy is admitted (its handler answers; not a shed).
    let r = call(&state, HEAVY.0, HEAVY.1, Some("w")).await;
    assert!(!r.shed(), "{} {}", r.status, r.body);
    assert_eq!(
        metric(
            &state,
            r#"munarium_admission_shed_total{plane="rest",class="heavy"}"#
        ),
        4
    );
}

// ---------------------------------------------------------------------------
// B3-T1: combined count
// ---------------------------------------------------------------------------

#[test]
fn filling_every_class_stops_exactly_at_the_ceiling() {
    let config = AdmissionConfig::new(5, 2, 2).unwrap();
    let plane = PlaneAdmission::new(
        "rest",
        Arc::new(tokio::sync::Semaphore::new(config.ordinary())),
        config,
    );
    let mut held = Vec::new();
    let classes = [
        WorkClass::Heavy,
        WorkClass::Control(ControlAuth::Rw),
        WorkClass::Ordinary,
        WorkClass::Heavy,
        WorkClass::Control(ControlAuth::Mgmt),
        WorkClass::Control(ControlAuth::Authenticated),
        WorkClass::Ordinary,
    ];
    for _ in 0..4 {
        for class in classes {
            if let Some((p, _)) = plane.try_admit(class, |_| true) {
                held.push(p);
            }
        }
    }
    assert_eq!(held.len(), 5, "every class together stops at the ceiling");
    let in_use: usize = plane
        .usage()
        .iter()
        .filter(|(p, _, _)| *p != "heavy")
        .map(|u| u.2)
        .sum();
    assert_eq!(in_use, 5);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_admission_never_exceeds_the_plane_ceiling() {
    const CEILING: usize = 4;
    let config = AdmissionConfig::new(CEILING, 1, 2).unwrap();
    let plane = Arc::new(PlaneAdmission::new(
        "grpc",
        Arc::new(tokio::sync::Semaphore::new(config.ordinary())),
        config,
    ));
    let in_flight = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let heavy_in_flight = Arc::new(AtomicUsize::new(0));
    let heavy_peak = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for t in 0..32usize {
        let (plane, in_flight, peak) = (plane.clone(), in_flight.clone(), peak.clone());
        let (heavy_in_flight, heavy_peak) = (heavy_in_flight.clone(), heavy_peak.clone());
        tasks.push(tokio::spawn(async move {
            for i in 0..300usize {
                let class = match (t + i) % 4 {
                    0 => WorkClass::Ordinary,
                    1 => WorkClass::Heavy,
                    2 => WorkClass::Control(ControlAuth::Rw),
                    _ => WorkClass::Control(ControlAuth::Authenticated),
                };
                if let Some((permit, _)) = plane.try_admit(class, |_| true) {
                    let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    if class == WorkClass::Heavy {
                        let h = heavy_in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                        heavy_peak.fetch_max(h, Ordering::SeqCst);
                    }
                    tokio::task::yield_now().await;
                    if class == WorkClass::Heavy {
                        heavy_in_flight.fetch_sub(1, Ordering::SeqCst);
                    }
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                    drop(permit);
                }
                tokio::task::yield_now().await;
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    let peak = peak.load(Ordering::SeqCst);
    assert!(
        peak <= CEILING,
        "combined in-flight {peak} exceeded {CEILING}"
    );
    assert!(peak >= 2, "the stress run never overlapped (peak {peak})");
    assert!(heavy_peak.load(Ordering::SeqCst) <= 2);
    assert_eq!(plane.ordinary.available_permits(), CEILING - 1);
    assert_eq!(plane.reserve.available_permits(), 1);
}

// ---------------------------------------------------------------------------
// B3-T1: typed gRPC and the native bridge
// ---------------------------------------------------------------------------

/// Calls `path` through the gRPC capture layer around a stub service that
/// marks its response, so a reject and an admitted call are told apart.
async fn grpc_call(state: &Arc<AppState>, path: &str, token: Option<&str>) -> Option<String> {
    let stub = tower::service_fn(|_req: axum::http::Request<Body>| async {
        let mut resp = axum::http::Response::new(tonic::body::empty_body());
        resp.headers_mut()
            .insert("x-stub", axum::http::HeaderValue::from_static("reached"));
        Ok::<_, std::convert::Infallible>(resp)
    });
    let mut svc = tower::Layer::layer(
        &crate::middleware::GrpcCaptureLayer {
            state: state.clone(),
        },
        stub,
    );
    let mut req = axum::http::Request::post(path).header("munarium-uid", "tester");
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    let resp = svc
        .ready()
        .await
        .unwrap()
        .call(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    if resp.headers().contains_key("x-stub") {
        return None;
    }
    resp.headers()
        .get("grpc-status")
        .and_then(|v| v.to_str().ok())
        .map(String::from)
}

const RESOURCE_EXHAUSTED: &str = "8";

#[tokio::test]
async fn grpc_reserve_admits_only_an_authorized_control_method() {
    let state = state_with(2, 1, 1, static_auth()).await;
    let held = hold_all(&state.admission.grpc.ordinary);
    let revoke = "/mmp.v1.AdminService/RevokeAccessToken";

    let ordinary = grpc_call(&state, "/mmp.v1.QueryService/GetHead", Some("m")).await;
    assert_eq!(ordinary.as_deref(), Some(RESOURCE_EXHAUSTED));
    for token in [None, Some("w"), Some("r"), Some("bogus")] {
        let r = grpc_call(&state, revoke, token).await;
        assert_eq!(r.as_deref(), Some(RESOURCE_EXHAUSTED), "as {token:?}");
    }
    // Heavy never uses the reserve.
    let r = grpc_call(&state, "/mmp.v1.SessionService/Turn", Some("m")).await;
    assert_eq!(r.as_deref(), Some(RESOURCE_EXHAUSTED));
    assert_eq!(
        grpc_call(&state, revoke, Some("m")).await,
        None,
        "mgmt revoke must reach its handler through the reserve"
    );
    assert_eq!(
        metric(
            &state,
            r#"munarium_admission_total{plane="grpc",class="control",slot="reserve"}"#
        ),
        1
    );
    // The REST plane's pools are separate and untouched.
    assert_eq!(state.admission.rest.ordinary.available_permits(), 1);

    // Heavy bound on gRPC: full heavy token sheds heavy, not ordinary.
    drop(held);
    let heavy = hold_all(state.admission.grpc.heavy.as_ref().unwrap());
    let r = grpc_call(&state, "/mmp.v1.IngestService/PutSource", Some("w")).await;
    assert_eq!(r.as_deref(), Some(RESOURCE_EXHAUSTED));
    assert_eq!(
        grpc_call(&state, "/mmp.v1.QueryService/GetHead", Some("w")).await,
        None
    );
    drop(heavy);
}

#[tokio::test]
async fn legacy_single_slot_grpc_plane_sheds_control_too() {
    let state = AppState::new(test_config(1, static_auth())).await.unwrap();
    let _held = hold_all(&state.admission.grpc.ordinary);
    let r = grpc_call(&state, "/mmp.v1.AdminService/RevokeAccessToken", Some("m")).await;
    assert_eq!(r.as_deref(), Some(RESOURCE_EXHAUSTED));
    assert_eq!(metric(&state, "munarium_load_shed_total"), 1);
}

/// The native bridge replays through `rest::router`: it is not charged a
/// gRPC permit (single charge), and its calls take their REST route's class.
#[tokio::test]
async fn bridged_calls_are_charged_once_and_classified_by_rest_route() {
    use crate::pb::server_api_service_server::ServerApiService;
    let state = state_with(2, 1, 0, static_auth()).await;

    // No gRPC permit is taken for the bridge: it passes a full gRPC plane.
    let grpc_held = hold_all(&state.admission.grpc.ordinary);
    assert_eq!(
        grpc_call(&state, "/mmp.v1.ServerApiService/GetHead", Some("w")).await,
        None
    );
    drop(grpc_held);

    let svc = crate::grpc_api::ServerApiSvc::new(state.clone());
    let request = |token: &str, params: &[(&str, &str)]| {
        let mut input = crate::pb::ServerApiRequest::default();
        for (k, v) in params {
            input.path_parameters.insert(k.to_string(), v.to_string());
        }
        let mut r = tonic::Request::new(input);
        r.metadata_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
        r.metadata_mut()
            .insert("munarium-uid", "tester".parse().unwrap());
        r
    };
    let reason = |e: &tonic::Status| {
        use tonic_types::StatusExt;
        e.get_error_details()
            .error_info()
            .map(|i| i.reason.clone())
            .unwrap_or_default()
    };

    let _rest_held = hold_all(&state.admission.rest.ordinary);
    let ordinary = svc
        .get_head(request("w", &[("version_id", "v-x")]))
        .await
        .expect_err("ordinary bridged call is shed");
    assert_eq!(reason(&ordinary), "overloaded");
    let denied = svc
        .revoke_token(request("w", &[("jti", "j-x")]))
        .await
        .expect_err("rw cannot revoke, so it cannot take the reserve");
    assert_eq!(reason(&denied), "overloaded");
    match svc.revoke_token(request("m", &[("jti", "j-x")])).await {
        Ok(_) => {}
        Err(e) => assert_ne!(reason(&e), "overloaded", "{e:?}"),
    }
    assert_eq!(metric(&state, REST_RESERVE), 1);
    assert_eq!(
        state.admission.grpc.ordinary.available_permits(),
        1,
        "the bridge charges the REST plane only"
    );
}

// ---------------------------------------------------------------------------
// B3-T2: the classification against the registered routes
// ---------------------------------------------------------------------------

fn openapi_operations() -> Vec<(String, String)> {
    let doc = crate::openapi::doc();
    let mut out = Vec::new();
    for (path, item) in &doc.paths.paths {
        for (method, op) in [
            ("GET", &item.get),
            ("PUT", &item.put),
            ("POST", &item.post),
            ("DELETE", &item.delete),
            ("PATCH", &item.patch),
        ] {
            if op.is_some() {
                out.push((method.to_string(), path.clone()));
            }
        }
    }
    out
}

/// Every allowlisted control template is a registered route with that
/// method (the request matches it and takes the reserve), and every OTHER
/// admitted operation — under an auth mode that passes every role gate —
/// is shed rather than reserved.
#[tokio::test]
async fn only_allowlisted_routes_can_take_a_reserved_slot() {
    let state = state_with(2, 1, 0, AuthMode::Disabled).await;
    let _held = hold_all(&state.admission.rest.ordinary);
    let ops = openapi_operations();
    assert!(ops.len() > 100, "OpenAPI lists {} operations", ops.len());

    let mut reserved = 0;
    for (method, template) in &ops {
        if !template.starts_with("/v1/") && !template.starts_with("/v1.2/") {
            continue;
        }
        let listed = REST_CONTROL
            .iter()
            .any(|(m, t, _)| m == method && t == template);
        let r = call(&state, method, template, None).await;
        if listed {
            reserved += 1;
            assert!(!r.shed(), "{method} {template} must take the reserve");
        } else {
            assert!(
                r.shed(),
                "unlisted {method} {template} obtained a slot: {} {}",
                r.status,
                r.body
            );
        }
    }
    assert_eq!(
        reserved,
        REST_CONTROL.len(),
        "every control entry is in OpenAPI"
    );
    assert_eq!(metric(&state, REST_RESERVE), REST_CONTROL.len() as u64);
    // A path outside every route cannot either.
    let r = call(&state, "PUT", "/v1/retrieval-rollout/extra", None).await;
    assert!(r.shed());
}

/// Every heavy template is a registered route with that method: with the
/// heavy bound full, each is shed as HEAVY while ordinary slots are free.
#[tokio::test]
async fn every_heavy_template_is_a_registered_route() {
    let state = state_with(4, 1, 1, AuthMode::Disabled).await;
    let ops = openapi_operations();
    let _heavy = hold_all(state.admission.rest.heavy.as_ref().unwrap());
    for (i, (method, template)) in REST_HEAVY.iter().enumerate() {
        assert!(
            ops.iter().any(|(m, t)| m == method && t == template),
            "{method} {template} is not an OpenAPI operation"
        );
        let r = call(&state, method, template, None).await;
        assert!(r.shed(), "{method} {template} was not classified heavy");
        assert_eq!(
            metric(
                &state,
                r#"munarium_admission_shed_total{plane="rest",class="heavy"}"#
            ),
            i as u64 + 1
        );
    }
}

/// The unclassified default: whatever the authorizer would say, an ordinary
/// class is never offered the reserve (the authorizer is not even asked).
#[test]
fn an_unclassified_route_never_reaches_the_reserve() {
    assert_eq!(
        admission::rest_class("GET", "/v1/new-route"),
        WorkClass::Ordinary
    );
    assert_eq!(
        admission::rest_class("POST", "(unmatched)"),
        WorkClass::Ordinary
    );
    // Method is part of the key.
    assert_eq!(
        admission::rest_class("GET", "/v1/retrieval-rollout"),
        WorkClass::Ordinary
    );
    assert_eq!(
        admission::grpc_class("/mmp.v1.AdminService/IssueAccessToken"),
        WorkClass::Ordinary
    );
    let config = AdmissionConfig::new(2, 1, 0).unwrap();
    let plane = PlaneAdmission::new(
        "rest",
        Arc::new(tokio::sync::Semaphore::new(config.ordinary())),
        config,
    );
    let _held = plane.ordinary.clone().try_acquire_owned().unwrap();
    for class in [
        admission::rest_class("POST", "/v1/versions"),
        admission::grpc_class("/mmp.v1.CommandService/CreateVersion"),
        WorkClass::Heavy,
    ] {
        assert!(plane
            .try_admit(class, |_| panic!("authorizer consulted for {class:?}"))
            .is_none());
    }
    assert_eq!(plane.reserve.available_permits(), 1);
}

/// Minimal descriptor messages: only the fields this check reads.
#[derive(Clone, PartialEq, prost::Message)]
struct FileSet {
    #[prost(message, repeated, tag = "1")]
    file: Vec<File>,
}
#[derive(Clone, PartialEq, prost::Message)]
struct File {
    #[prost(string, optional, tag = "2")]
    package: Option<String>,
    #[prost(message, repeated, tag = "6")]
    service: Vec<ServiceDesc>,
}
#[derive(Clone, PartialEq, prost::Message)]
struct ServiceDesc {
    #[prost(string, optional, tag = "1")]
    name: Option<String>,
    #[prost(message, repeated, tag = "2")]
    method: Vec<MethodDesc>,
}
#[derive(Clone, PartialEq, prost::Message)]
struct MethodDesc {
    #[prost(string, optional, tag = "1")]
    name: Option<String>,
}

/// Every classified gRPC method path names a real method in the served
/// descriptor set, and none is a ServerApiService method (the bridge is
/// classified by its REST route, never here).
#[test]
fn every_classified_grpc_method_exists() {
    use prost::Message;
    let set = FileSet::decode(munarium_proto::FILE_DESCRIPTOR_SET).unwrap();
    let mut paths = std::collections::BTreeSet::new();
    for f in &set.file {
        for s in &f.service {
            for m in &s.method {
                paths.insert(format!(
                    "/{}.{}/{}",
                    f.package.as_deref().unwrap_or_default(),
                    s.name.as_deref().unwrap_or_default(),
                    m.name.as_deref().unwrap_or_default()
                ));
            }
        }
    }
    assert!(paths.contains("/mmp.v1.QueryService/GetHead"), "{paths:?}");
    for path in GRPC_CONTROL
        .iter()
        .map(|(p, _)| *p)
        .chain(GRPC_HEAVY.iter().copied())
    {
        assert!(paths.contains(path), "{path} is not a served gRPC method");
        assert!(!path.starts_with("/mmp.v1.ServerApiService/"));
    }
}
