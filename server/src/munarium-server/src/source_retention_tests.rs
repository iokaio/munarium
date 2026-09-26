// SPDX-License-Identifier: Apache-2.0
use super::*;
use axum::{
    body::{to_bytes, Body},
    http::Request,
};
use munarium_proto::mmp::v1::{
    self as pb, server_api_service_server::ServerApiService, session_service_server::SessionService,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use tower::ServiceExt;

async fn call(
    router: &axum::Router,
    method: &str,
    path: &str,
    token: &str,
    body: Value,
) -> (u16, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    (
        status,
        serde_json::from_slice(&to_bytes(response.into_body(), 1_000_000).await.unwrap()).unwrap(),
    )
}
fn native(token: &str, body: Value) -> tonic::Request<pb::ServerApiRequest> {
    let mut request = tonic::Request::new(pb::ServerApiRequest {
        body: serde_json::to_vec(&body).unwrap(),
        ..Default::default()
    });
    request
        .metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    request
}

#[tokio::test]
async fn retention_authority_sessions_and_provider_effect_denial() {
    let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
        eprintln!("UNAVAILABLE: isolated PostgreSQL required");
        return;
    };
    let tenant = format!("retention-api-{}", uuid::Uuid::new_v4().simple());
    let state = crate::providers_api::usage_tests::test_state_with_auth(
        Some(url),
        crate::config::AuthMode::Static(vec![
            ("admin".into(), tenant.clone(), "mgmt".into()),
            ("reader".into(), tenant.clone(), "ro".into()),
            ("writer".into(), tenant.clone(), "rw".into()),
            ("other".into(), format!("other-{tenant}"), "mgmt".into()),
        ]),
    )
    .await;
    let router = crate::rest::router(state.clone());
    let native_api = crate::grpc_api::ServerApiSvc::new(state.clone());
    for role in ["reader", "writer"] {
        assert_eq!(
            call(
                &router,
                "POST",
                "/v1/source-retention",
                role,
                json!({"path":"fixture.txt","action":"deny"})
            )
            .await
            .0,
            403
        );
        assert_eq!(
            call(&router, "GET", "/v1/source-retention", role, Value::Null)
                .await
                .0,
            403
        );
        assert_eq!(
            native_api
                .change_source_retention(native(
                    role,
                    json!({"path":"fixture.txt","action":"deny"})
                ))
                .await
                .unwrap_err()
                .code(),
            tonic::Code::PermissionDenied
        );
    }
    assert_eq!(
        call(&router, "GET", "/v1/source-retention", "other", Value::Null)
            .await
            .1["records"],
        json!([])
    );
    for path in ["../outside", "evidence/fixture", &"x".repeat(1025)] {
        assert_eq!(
            call(
                &router,
                "POST",
                "/v1/source-retention",
                "admin",
                json!({"path":path,"action":"deny"})
            )
            .await
            .0,
            400
        );
    }
    assert_eq!(
        call(
            &router,
            "POST",
            "/v1/source-retention",
            "admin",
            json!({"path":"x".repeat(1024),"action":"hold"})
        )
        .await
        .0,
        200
    );
    assert_eq!(
        call(
            &router,
            "POST",
            "/v1/source-retention",
            "admin",
            json!({"path":"fictional.txt","action":"restore"})
        )
        .await
        .0,
        400
    );
    let retrieval = state.retrieval_for(&tenant).unwrap();
    let col = retrieval
        .ensure_collection("articles", "article@1", 0, &[], None)
        .await
        .unwrap();
    let (source, _, _) = retrieval
        .put_source(
            "",
            "text/plain",
            "fictional.txt",
            Some("article@1"),
            b"Fictional orchid evidence.",
        )
        .await
        .unwrap();
    retrieval.bind_source(&col.id, &source, None).await.unwrap();
    retrieval
        .build_collection_index(&col.id, 400, 1, true)
        .await
        .unwrap();
    // A non-PG original cannot be advertised as erased by the PG-only cleaner.
    assert_eq!(
        call(
            &router,
            "POST",
            "/v1/source-retention",
            "admin",
            json!({"path":"fictional.txt","action":"deny-and-erase-pg-original"})
        )
        .await
        .0,
        400
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let app=axum::Router::new().route("/api/chat",axum::routing::post(move || { observed.fetch_add(1,Ordering::SeqCst); async {Json(json!({"message":{"content":"fictional reply"},"done":true,"done_reason":"stop","prompt_eval_count":2,"eval_count":2}))} }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let provider = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    state.providers.apply(&state,&tenant,&format!("apiVersion: munarium.ioka.io/v1\nkind: ProviderConfig\nmetadata: {{name: fixture}}\nspec:\n  provider: ollama\n  endpoint: {endpoint}\n  models: {{fast: fixture, capable: fixture}}\n")).await.unwrap();
    let yaml="apiVersion: munarium.ioka.io/v1\nkind: Runbook\nmetadata: {name: retention, version: 1}\nspec:\n  collections: [{name: articles, shape: article@1}]\n  models:\n    tasks:\n      completion: {provider: fixture, tier: fast}\n  completion: {promptTemplate: '{query} {context}', maxTokens: 64}\n  steps: [{buildIndex: {}}]\n";
    sqlx::query("INSERT INTO runbooks(tenant_id,runbook_ref,yaml) VALUES($1,'retention@1',$2)")
        .bind(&tenant)
        .bind(yaml)
        .execute(state.pg_pool().unwrap())
        .await
        .unwrap();
    let access = munarium_access::AccessCtx::unrestricted("fixture-user", &tenant);
    let session = crate::sessions_api::op_create_session(&state, &access, "retention")
        .await
        .unwrap();
    let request =
        || serde_json::from_value(json!({"query":"orchid evidence","complete":true})).unwrap();
    let before =
        crate::sessions_api::op_turn(&state, &access, &session.session_id, request(), None)
            .await
            .unwrap()
            .0;
    assert!(!before.hits.is_empty());
    assert!(calls.load(Ordering::SeqCst) > 0);
    calls.store(0, Ordering::SeqCst);
    let changed = native_api
        .change_source_retention(native(
            "admin",
            json!({"path":"fictional.txt","action":"deny"}),
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        serde_json::from_slice::<Value>(&changed.body).unwrap()["denied"],
        true
    );
    assert!(
        crate::sessions_api::op_turn(&state, &access, &session.session_id, request(), None)
            .await
            .unwrap_err()
            .to_string()
            .contains("source-denied")
    );
    assert_eq!(
        call(
            &router,
            "GET",
            &format!("/v1/sessions/{}", session.session_id),
            "reader",
            Value::Null
        )
        .await
        .0,
        410
    );
    let typed = crate::grpc_platform::SessionSvc {
        state: state.clone(),
    };
    let mut req = tonic::Request::new(pb::GetSessionRequest {
        session_id: session.session_id.clone(),
    });
    req.metadata_mut()
        .insert("authorization", "Bearer reader".parse().unwrap());
    assert_eq!(
        typed.get_session(req).await.unwrap_err().code(),
        tonic::Code::FailedPrecondition
    );
    assert!(crate::vocabulary_api::load(&state, &tenant, &col.id)
        .await
        .is_err());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "denial precedes provider submission, including a retained session"
    );
    assert!(
        !crate::sessions_api::op_get_session(&state, &tenant, &session.session_id)
            .await
            .unwrap()
            .turns
            .is_empty(),
        "trusted audit history remains retained"
    );
    let listed = native_api
        .list_source_retention(native("admin", Value::Null))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        serde_json::from_slice::<Value>(&listed.body).unwrap()["records"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    provider.abort();
}
