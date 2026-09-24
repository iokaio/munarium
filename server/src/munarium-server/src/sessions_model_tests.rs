// SPDX-License-Identifier: Apache-2.0
//! Exercise the complete turn against PostgreSQL and a loopback provider.
use super::*;
use crate::config::{AuthMode, Config, DocIntelConfig, SourceStoreConfig, StoreKind};
use serde_json::{json, Value};
use std::sync::Mutex;

#[tokio::test]
async fn turn_model_routing_calls_selected_provider_and_rejects_before_spending() {
    let Ok(database_url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
        eprintln!("skipped: requires MUNARIUM_TEST_DATABASE_URL (server/gates.ps1)");
        return;
    };
    let state = model_test_state(database_url).await;
    let tenant = format!("routing-{}", uuid::Uuid::new_v4().simple());
    let calls = Arc::new(Mutex::new(Vec::<Value>::new()));
    let capture = calls.clone();
    let app = axum::Router::new().route(
        "/api/chat",
        axum::routing::post(move |Json(body): Json<Value>| {
            let capture = capture.clone();
            async move {
                capture.lock().unwrap().push(body.clone());
                Json(json!({
                    "model": body["model"], "done": true, "done_reason": "stop",
                    "message": {"role": "assistant", "content": "[\"journey\"]"},
                    "prompt_eval_count": 9, "eval_count": 3
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let provider_task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    for name in ["baseline", "selected"] {
        state.providers.apply(&state, &tenant, &format!(
            "apiVersion: munarium.ioka.io/v1\nkind: ProviderConfig\nmetadata: {{ name: {name} }}\nspec:\n  provider: ollama\n  endpoint: {endpoint}\n  models:\n    complete: [{name}-fast, {name}-capable, explicit]\n    fast: {name}-fast\n    capable: {name}-capable\n"
        )).await.unwrap();
    }
    let yaml = r#"
apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: { name: routing, version: 1 }
spec:
  collections: [{ name: articles, shape: article@1 }]
  retrieval:
    modelQueryExpansion: { maxTerms: 4, maxTokens: 64, required: false }
  models:
    allowOverrides: [selected]
    tasks:
      query_expansion: { provider: baseline, tier: fast }
      completion: { provider: baseline, tier: capable }
  completion: { promptTemplate: "{query}\n{context}", maxTokens: 64 }
  steps: [{ buildIndex: {} }]
"#;
    // The test needs a registered collection, not an index or a paid ingestion.
    state
        .retrieval_for(&tenant)
        .unwrap()
        .ensure_collection("articles", "article@1", 0, &[], None)
        .await
        .unwrap();
    sqlx::query("INSERT INTO runbooks (tenant_id, runbook_ref, yaml) VALUES ($1,'routing@1',$2)")
        .bind(&tenant)
        .bind(yaml)
        .execute(state.pg_pool().unwrap())
        .await
        .unwrap();
    let access = AccessCtx::unrestricted("tester", &tenant);
    let session = op_create_session(&state, &access, "routing").await.unwrap();
    for (selection, expected) in [
        (Value::Null, vec!["baseline-fast", "baseline-capable"]),
        (json!({}), vec!["baseline-fast", "baseline-capable"]),
        (
            json!({"provider":"selected", "tier":"fast"}),
            vec!["selected-fast", "selected-fast"],
        ),
        (
            json!({"provider":"selected", "tier":"capable"}),
            vec!["selected-capable", "selected-capable"],
        ),
        (
            json!({"provider":"selected", "model":"explicit"}),
            vec!["explicit", "explicit"],
        ),
    ] {
        calls.lock().unwrap().clear();
        let request = serde_json::from_value(json!({
            "query":"Where did they visit?", "complete":true, "model_override":selection
        }))
        .unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (response, _) = op_turn(&state, &access, &session.session_id, request, Some(tx))
            .await
            .unwrap();
        let models: Vec<String> = calls
            .lock()
            .unwrap()
            .iter()
            .map(|body| body["model"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(models, expected);
        assert_eq!(response.completion.unwrap().model, expected[1]);
        let mut expansion_seen = false;
        while let Ok(event) = rx.try_recv() {
            if let dto::TurnProgressEvent::Expansion { model, .. } = event {
                assert_eq!(model, expected[0]);
                expansion_seen = true;
            }
        }
        assert!(expansion_seen);
    }
    for body in [
        json!({"complete":true, "model_override":{"provider":"forbidden"}}),
        json!({"complete":true, "model_override":{"tier":"fast"}}),
        json!({"complete":false, "model_override":{"provider":"selected"}}),
    ] {
        calls.lock().unwrap().clear();
        let mut body = body;
        body["query"] = json!("Where did they visit?");
        let result = op_turn(
            &state,
            &access,
            &session.session_id,
            serde_json::from_value(body).unwrap(),
            None,
        )
        .await;
        assert!(result.is_err());
        assert!(
            calls.lock().unwrap().is_empty(),
            "rejected turn spent on a provider"
        );
    }
    // Retrieval-only requests still use the expansion default and generate no answer.
    for selection in [Value::Null, json!({})] {
        calls.lock().unwrap().clear();
        let request = serde_json::from_value(json!({
            "query":"Where did they visit?", "complete":false, "model_override":selection
        }))
        .unwrap();
        let (response, _) = op_turn(&state, &access, &session.session_id, request, None)
            .await
            .unwrap();
        assert!(response.completion.is_none());
        let seen = calls.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0]["model"], "baseline-fast");
    }
    provider_task.abort();
}

async fn model_test_state(database_url: String) -> Arc<AppState> {
    AppState::new(Config {
        http_addr: "127.0.0.1:0".into(),
        grpc_addr: None,
        ops_addr: "127.0.0.1:0".into(),
        store: StoreKind::Postgres,
        database_url: Some(database_url),
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
        db_max_conns: 4,
        idempotency_ttl_secs: 86400,
        replica_count: 1,
        registry_ttl_secs: 15,
        session_idle_ttl_secs: 0,
        evidence_purge_interval_secs: 0,
        max_tokens: dto::MaxTokensBudgets::default(),
        instance_id: "routing-test".into(),
        source_store: SourceStoreConfig::Pg,
        doc_intel: DocIntelConfig::None,
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn turn_retry_attempts_and_ceiling_are_bounded() {
    let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
        eprintln!("PostgreSQL session retry tests NOT RUN: test database unset");
        return;
    };
    let state = model_test_state(url).await;
    for (truncated, ceiling) in [(false, 64u32), (true, 64), (true, u32::MAX)] {
        let tenant = format!("retry-{}", uuid::Uuid::new_v4().simple());
        let calls = Arc::new(Mutex::new(Vec::<Value>::new()));
        let capture = calls.clone();
        let app = axum::Router::new().route("/api/chat", axum::routing::post(move |Json(body): Json<Value>| {
            let capture = capture.clone();
            async move {
                let index = { let mut calls = capture.lock().unwrap(); let n = calls.len(); calls.push(body); n };
                let stop = if truncated && index == 0 { "length" } else { "stop" };
                let text = if index <= usize::from(truncated) { "A fabricated \"quotation not in evidence\"." } else { "No quotation." };
                Json(json!({"done":true,"done_reason":stop,"message":{"role":"assistant","content":text},
                    "prompt_eval_count":2,"eval_count":1}))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        struct Abort(tokio::task::JoinHandle<()>);
        impl Drop for Abort {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let _task = Abort(task);
        state.providers.apply(&state, &tenant, &format!(
            "apiVersion: munarium.ioka.io/v1\nkind: ProviderConfig\nmetadata: {{ name: fixture }}\nspec:\n  provider: ollama\n  endpoint: {endpoint}\n  models: {{ capable: fixture }}\n"
        )).await.unwrap();
        let yaml = format!(
            r#"
apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: {{ name: retry, version: 1 }}
spec:
  collections: [{{ name: articles, shape: article@1 }}]
  models:
    tasks:
      completion: {{ provider: fixture, tier: capable }}
  completion:
    promptTemplate: "{{query}}\n{{context}}"
    maxTokens: {ceiling}
    verification: {{ quotes: true, maxRetries: 1 }}
  steps: [{{ buildIndex: {{}} }}]
"#
        );
        state
            .retrieval_for(&tenant)
            .unwrap()
            .ensure_collection("articles", "article@1", 0, &[], None)
            .await
            .unwrap();
        sqlx::query("INSERT INTO runbooks (tenant_id, runbook_ref, yaml) VALUES ($1,'retry@1',$2)")
            .bind(&tenant)
            .bind(&yaml)
            .execute(state.pg_pool().unwrap())
            .await
            .unwrap();
        let access = AccessCtx::unrestricted("tester", &tenant);
        let session = op_create_session(&state, &access, "retry").await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let request =
            serde_json::from_value(json!({"query":"What is known?", "complete":true})).unwrap();
        let result = op_turn(&state, &access, &session.session_id, request, Some(tx)).await;
        let captured = calls.lock().unwrap();
        if ceiling == u32::MAX {
            assert!(
                matches!(result, Err(ApiError::Mesh(KernelError::InvalidInput(_)))),
                "{result:?}"
            );
            assert_eq!(
                captured.len(),
                1,
                "overflow must fail before retry dispatch"
            );
        } else {
            let (response, _) = result.unwrap();
            assert_eq!(
                response.completion.unwrap().input_tokens,
                if truncated { 6 } else { 4 }
            );
            let expected = if truncated { vec![0, 1, 2] } else { vec![0, 1] };
            let mut attempts = Vec::new();
            while let Ok(event) = rx.try_recv() {
                if let dto::TurnProgressEvent::Completion { attempt, .. } = event {
                    attempts.push(attempt);
                }
            }
            assert_eq!(attempts, expected);
            assert_eq!(captured.len(), expected.len());
            for (i, body) in captured.iter().enumerate() {
                assert_eq!(
                    body["options"]["num_predict"],
                    if truncated && i > 0 { 256 } else { 64 }
                );
            }
        }
    }
}
