// SPDX-License-Identifier: Apache-2.0
//! Operator diagnostics: real transport authority and zero-submission controls.
use super::*;
use axum::{
    body::{to_bytes, Body},
    http::Request as HttpRequest,
};
use munarium_proto::mmp::v1::{
    provider_service_server::ProviderService, server_api_service_server::ServerApiService,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use tower::ServiceExt;

struct Abort(tokio::task::JoinHandle<()>);
impl Drop for Abort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn rest(router: &axum::Router, path: &str, token: &str) -> (u16, Value) {
    let response = router
        .clone()
        .oneshot(
            HttpRequest::builder()
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let body = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

fn native(token: &str, config: bool) -> Request<pb::ServerApiRequest> {
    let mut request = Request::new(pb::ServerApiRequest {
        path_parameters: if config {
            HashMap::from([("name".into(), "fixture".into())])
        } else {
            HashMap::new()
        },
        ..Default::default()
    });
    request
        .metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    request
}

#[tokio::test]
async fn managed_probes_and_aliases_enforce_authority_and_shared_caps() {
    let state = usage_tests::test_state_with_diagnostics(
        None,
        crate::config::AuthMode::Static(vec![
            ("admin".into(), "tenant".into(), "mgmt".into()),
            ("reader".into(), "tenant".into(), "ro".into()),
            ("writer".into(), "tenant".into(), "rw".into()),
            ("other".into(), "other".into(), "mgmt".into()),
        ]),
        true,
    )
    .await;
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let health_calls = Arc::new(AtomicUsize::new(0));
    let health_observed = health_calls.clone();
    let app = axum::Router::new()
        .route("/api/chat", axum::routing::post(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            async { Json(json!({"message":{"content":"OK"},"done":true,"done_reason":"stop","prompt_eval_count":2,"eval_count":1})) }
        }))
        .route("/api/tags", axum::routing::get(move || {
            health_observed.fetch_add(1, Ordering::SeqCst);
            async { Json(json!({"models":[{"name":"fixture"}]})) }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let _server = Abort(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap()
    }));
    let yaml = |cap: &str| {
        format!("apiVersion: munarium.ioka.io/v1\nkind: ProviderConfig\nmetadata: {{name: fixture}}\nspec:\n  provider: ollama\n  endpoint: {endpoint}\n  credentialAlias: local-fixture\n  models: {{fast: fixture}}\n{cap}")
    };
    state
        .providers
        .apply(
            &state,
            "tenant",
            &yaml("  budgets: {dailyTotalTokens: 10000}"),
        )
        .await
        .unwrap();
    let router = crate::rest::router(state.clone());
    let grpc = crate::grpc_api::ServerApiSvc::new(state.clone());
    let typed = ProviderSvc {
        state: state.clone(),
    };
    for token in ["reader", "writer"] {
        for path in [
            "/healthai",
            "/v1/providers/fixture/health",
            "/v1/providers/fixture/diagnostics",
        ] {
            assert_eq!(rest(&router, path, token).await.0, 403);
        }
        assert_eq!(
            grpc.healthai(native(token, false))
                .await
                .unwrap_err()
                .code(),
            tonic::Code::PermissionDenied
        );
        assert_eq!(
            grpc.provider_diagnostics(native(token, true))
                .await
                .unwrap_err()
                .code(),
            tonic::Code::PermissionDenied
        );
        let mut request = Request::new(pb::ProviderHealthRequest {
            config_name: "fixture".into(),
        });
        request
            .metadata_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
        assert_eq!(
            typed.provider_health(request).await.unwrap_err().code(),
            tonic::Code::PermissionDenied
        );
        let (status, free) = rest(&router, "/v1/providers", token).await;
        assert_eq!(status, 200);
        assert!(!free.to_string().contains("local-fixture"));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(health_calls.load(Ordering::SeqCst), 0);
    let (status, diagnostic) = rest(&router, "/v1/providers/fixture/diagnostics", "admin").await;
    assert_eq!(status, 200);
    assert_eq!(diagnostic["credential_alias"], "local-fixture");
    assert_eq!(diagnostic["credential_source"], "none");
    let rpc = grpc
        .provider_diagnostics(native("admin", true))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        serde_json::from_slice::<Value>(&rpc.body).unwrap(),
        diagnostic
    );
    assert_eq!(
        rest(&router, "/v1/providers/fixture/diagnostics", "other")
            .await
            .0,
        404
    );
    assert_eq!(
        grpc.provider_diagnostics(native("other", true))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::NotFound
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0, "disclosure is free");
    let (_, health) = rest(&router, "/healthai", "admin").await;
    assert_eq!(health["healthy"], true);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        state.budgets().ledger("tenant").await.unwrap()[0].settled_units,
        3
    );
    state
        .providers
        .apply(&state, "tenant", &yaml("  budgets: {dailyTotalTokens: 0}"))
        .await
        .unwrap();
    let denied = grpc
        .healthai(native("admin", false))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        serde_json::from_slice::<Value>(&denied.body).unwrap()["healthy"],
        false
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // Omission does not select an uncapped env-backed fallback in managed mode.
    state
        .providers
        .apply(&state, "tenant", &yaml(""))
        .await
        .unwrap();
    let (_, skipped) = rest(&router, "/healthai", "admin").await;
    assert_eq!(skipped["healthy"], false);
    assert_eq!(skipped["checks"][0]["skipped"], true);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let store = state.store_for("tenant").await.unwrap();
    assert!(complete_guarded(
        &state,
        "tenant",
        store.as_ref(),
        "fixture",
        serde_json::from_value(json!({"tier":"fast","prompt":"test","max_tokens":1})).unwrap(),
        None,
        true
    )
    .await
    .is_err());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "dispatch checks the current config's cap"
    );
}

#[tokio::test]
async fn legacy_health_access_is_preserved_without_disclosing_missing_references() {
    let state = usage_tests::test_state_with_auth(
        None,
        crate::config::AuthMode::Static(vec![("reader".into(), "tenant".into(), "ro".into())]),
    )
    .await;
    state.providers.apply(&state, "tenant", "apiVersion: munarium.ioka.io/v1\nkind: ProviderConfig\nmetadata: {name: fixture}\nspec:\n  provider: ollama\n  endpoint: http://127.0.0.1:1\n  credentialRef: {env: FICTIONAL_DIAGNOSTIC_MISSING_REFERENCE}\n").await.unwrap();
    let router = crate::rest::router(state);
    let (status, body) = rest(&router, "/v1/providers/fixture/health", "reader").await;
    assert_eq!(
        status, 502,
        "legacy reader is authorized; credential refusal precedes HTTP"
    );
    assert!(!body
        .to_string()
        .contains("FICTIONAL_DIAGNOSTIC_MISSING_REFERENCE"));
    assert_eq!(
        rest(&router, "/v1/providers/fixture/diagnostics", "reader")
            .await
            .0,
        403
    );
}

#[derive(Clone, Default)]
struct Logs(Arc<std::sync::Mutex<Vec<u8>>>);
impl std::io::Write for Logs {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn provider_diagnostics_redact_references_and_upstream_secrets_across_transports() {
    let state = usage_tests::test_state_with_diagnostics(
        None,
        crate::config::AuthMode::Static(vec![
            ("admin".into(), "tenant".into(), "mgmt".into()),
            ("writer".into(), "tenant".into(), "rw".into()),
        ]),
        true,
    )
    .await;
    let logs = Logs::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    let _logging = tracing::subscriber::set_default(subscriber);
    let secret = "fictional-secret-never-disclose";
    let reference = format!("MUNARIUM_DIAGNOSTIC_{}", uuid::Uuid::new_v4().simple());
    std::env::set_var(&reference, secret);
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let app = axum::Router::new().route(
        "/chat/completions",
        axum::routing::post(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            async move { (axum::http::StatusCode::BAD_REQUEST, secret) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let _server = Abort(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap()
    }));
    let yaml = |credential: &str| {
        format!("apiVersion: munarium.ioka.io/v1\nkind: ProviderConfig\nmetadata: {{name: fixture}}\nspec:\n  provider: openai\n  endpoint: {endpoint}\n  credentialAlias: public-label\n  credentialRef: {{{credential}}}\n  models: {{fast: fixture}}\n  budgets: {{dailyTotalTokens: 10000}}\n")
    };
    state
        .providers
        .apply(&state, "tenant", &yaml(&format!("env: {reference}")))
        .await
        .unwrap();
    let router = crate::rest::router(state.clone());
    let grpc = crate::grpc_api::ServerApiSvc::new(state.clone());
    let (_, disclosure) = rest(&router, "/v1/providers/fixture/diagnostics", "admin").await;
    assert_eq!(disclosure["credential_source"], "env");
    assert_eq!(disclosure["credential_ok"], true);
    let request_body = json!({"model":"fixture","prompt":"test","max_tokens":1});
    let response = router
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri("/v1/providers/fixture/complete")
                .header("authorization", "Bearer writer")
                .header("content-type", "application/json")
                .body(Body::from(request_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 502);
    let body = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    let mut rpc = native("writer", true);
    rpc.get_mut().body = serde_json::to_vec(&request_body).unwrap();
    let error = grpc.provider_complete(rpc).await.unwrap_err();
    assert_eq!(error.code(), tonic::Code::Unavailable);
    for output in [
        String::from_utf8(body.to_vec()).unwrap(),
        error.to_string(),
        disclosure.to_string(),
    ] {
        assert!(!output.contains(secret));
        assert!(!output.contains(&reference));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    std::env::remove_var(&reference);
    let (_, missing) = rest(&router, "/healthai", "admin").await;
    assert!(!missing.to_string().contains(&reference));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    // A missing mounted-file reference is never substituted for an alias.
    let file = "fictional-missing-credential-sentinel";
    state
        .providers
        .apply(&state, "tenant", &yaml(&format!("file: {file}")))
        .await
        .unwrap();
    let (_, diagnostic) = rest(&router, "/v1/providers/fixture/diagnostics", "admin").await;
    assert_eq!(diagnostic["credential_source"], "file");
    assert_eq!(diagnostic["credential_ok"], false);
    assert!(!diagnostic.to_string().contains(file));
    let log_text = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    for sentinel in [secret, reference.as_str(), file] {
        assert!(!log_text.contains(sentinel));
    }
}
