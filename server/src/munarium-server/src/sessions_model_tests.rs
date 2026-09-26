// SPDX-License-Identifier: Apache-2.0
//! Exercise the complete turn against PostgreSQL and a loopback provider.
use super::*;
use crate::config::{AuthMode, Config, DocIntelConfig, SourceStoreConfig, StoreKind};
use serde_json::{json, Value};
use std::sync::Mutex;

#[tokio::test]
async fn anthropic_turn_retry_preserves_usage_and_rejects_non_exhaustion() {
    let state =
        crate::providers_api::usage_tests::test_state_with_auth(None, AuthMode::Disabled).await;
    std::env::set_var("MUNARIUM_TURN_POLICY_FIXTURE_KEY", "fixture-not-a-real-key");
    for (scenario, reason, text, cap, expected_calls, success) in [
        ("thinking", "max_tokens", "", 10000, 2, true),
        ("partial", "max_tokens", "partial answer", 10000, 2, true),
        ("empty", "end_turn", "", 10000, 1, false),
        ("refusal", "refusal", "", 10000, 1, false),
        ("refusal-text", "refusal", "Cannot answer", 10000, 1, false),
        ("tool", "tool_use", "", 10000, 1, false),
        ("malformed", "", "", 10000, 1, false),
        ("exhausted", "max_tokens", "", 10000, 2, false),
        ("denied", "max_tokens", "", 250, 1, false),
        ("ordinary", "end_turn", "Answer", 10000, 1, true),
    ] {
        let tenant = format!("turn-policy-{scenario}-{}", uuid::Uuid::new_v4().simple());
        let calls = Arc::new(Mutex::new(Vec::<Value>::new()));
        let sink = calls.clone();
        let app = axum::Router::new().route("/v1/messages", axum::routing::post(move |Json(body): Json<Value>| {
            let index = { let mut rows = sink.lock().unwrap(); let n = rows.len(); rows.push(body); n };
            async move {
                let final_answer = index > 0 && scenario != "exhausted";
                Json(json!({
                    "content":[{"type":"thinking","thinking":""},{"type":"text","text":if final_answer { "Answer" } else { text }}],
                    "stop_reason":if final_answer { "end_turn" } else { reason },
                    "usage":{"input_tokens":2,"output_tokens":if index == 0 {64} else {5}}
                }))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        state.providers.apply(&state, &tenant, &format!("apiVersion: munarium.ioka.io/v1\nkind: ProviderConfig\nmetadata: {{name: fixture}}\nspec:\n  provider: anthropic\n  endpoint: {endpoint}\n  credentialRef: {{env: MUNARIUM_TURN_POLICY_FIXTURE_KEY}}\n  models: {{capable: claude-sonnet-5}}\n  anthropic:\n    models:\n      claude-sonnet-5: {{effort: low, thinking: adaptive}}\n  budgets:\n    dailyTokens: {{capable: {cap}}}\n")).await.unwrap();
        let store = state.store_for(&tenant).await.unwrap();
        let version = store.create_version(None, None).await.unwrap();
        let invoke = |prompt: String, budget| {
            let version = version.clone();
            let state = &state;
            let tenant = &tenant;
            let store = &store;
            async move {
                crate::providers_api::op_complete(state, tenant, store.as_ref(), "fixture",
                    serde_json::from_value(json!({"prompt":prompt,"max_tokens":budget,"tier":"capable","temperature":0.0,"version_id":version})).unwrap()
                ).await
            }
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let result = complete_turn_answer(invoke, "fixture", 64, &Some(tx)).await;
        assert_eq!(result.is_ok(), success, "{scenario}");
        if scenario == "denied" {
            assert!(matches!(&result, Err(KernelError::RateLimited(_))));
        }
        if let Ok(completed) = result {
            assert_eq!(completed.response.text, "Answer");
            assert_eq!(completed.input_tokens, 2 * expected_calls as u64);
            assert_eq!(
                completed.output_tokens,
                if expected_calls == 2 { 69 } else { 64 }
            );
            assert_eq!(completed.response.stop_reason, "end_turn");
            let claim = store
                .get_claim(completed.response.invocation_event_id.as_deref().unwrap())
                .await
                .unwrap()
                .unwrap();
            assert!(claim.evidence.unwrap()["request_hash"].as_str().is_some());
        }
        {
            let rows = calls.lock().unwrap();
            assert_eq!(rows.len(), expected_calls, "{scenario}");
            for (index, body) in rows.iter().enumerate() {
                assert_eq!(body["max_tokens"], if index == 0 { 64 } else { 256 });
                assert!(body.get("temperature").is_none());
                assert_eq!(body["output_config"]["effort"], "low");
            }
        }
        let ledger = state.budgets().ledger(&tenant).await.unwrap();
        assert_eq!(ledger[0].held_units, 0, "{scenario}");
        assert_eq!(
            ledger[0].settled_units,
            if expected_calls == 2 { 73 } else { 66 },
            "{scenario}"
        );
        let mut attempts = Vec::new();
        while let Ok(dto::TurnProgressEvent::Completion { attempt, .. }) = rx.try_recv() {
            attempts.push(attempt);
        }
        assert_eq!(
            attempts,
            if expected_calls == 2 {
                vec![0, 1]
            } else {
                vec![0]
            }
        );
        task.abort();
    }
}

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
        managed_provider_diagnostics: false,
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

/// P07: sparse collection eligibility uses current collection policy even when
/// content is explicitly pinned. No record-level ACL capability is assumed.
#[tokio::test]
async fn sparse_collection_eligibility_rechecks_policy_and_preserves_old_pins() {
    let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
        eprintln!("P07 PostgreSQL authorization characterization NOT RUN: database unset");
        return;
    };
    let state = model_test_state(url.clone()).await;
    let tenant = format!("p07-{}", uuid::Uuid::new_v4().simple());
    let retrieval = state.retrieval_for(&tenant).unwrap();
    let mut names = Vec::new();
    for i in 0..24 {
        let name = format!("scope-{i:02}");
        let (level, compartments) = if i == 23 {
            (0, vec![])
        } else if i % 2 == 0 {
            (3, vec![])
        } else {
            (0, vec!["engineering".to_string()])
        };
        retrieval
            .ensure_collection(&name, "article@1", level, &compartments, None)
            .await
            .unwrap();
        names.push(name);
    }
    let yaml = format!("apiVersion: munarium.ioka.io/v1\nkind: Runbook\nmetadata: {{ name: sparse, version: 1 }}\nspec:\n  collections: [{}]\n  steps: [{{ buildIndex: {{}} }}]\n",
        names.iter().map(|name| format!("{{ name: {name}, shape: article@1 }}")).collect::<Vec<_>>().join(", "));
    let doc = munarium_runbooks::parse_runbook(&yaml).unwrap();
    let permitted = permitted_collections(&state, &tenant, &doc, 0, &[])
        .await
        .unwrap();
    assert_eq!(
        permitted
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        vec!["scope-23"]
    );
    // Independent expected sets for level-only and compartment-only clearance.
    let elevated = permitted_collections(&state, &tenant, &doc, 3, &[])
        .await
        .unwrap();
    assert_eq!(
        elevated.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        vec![
            "scope-00", "scope-02", "scope-04", "scope-06", "scope-08", "scope-10", "scope-12",
            "scope-14", "scope-16", "scope-18", "scope-20", "scope-22", "scope-23"
        ]
    );
    let compartment = permitted_collections(&state, &tenant, &doc, 0, &["engineering".into()])
        .await
        .unwrap();
    assert_eq!(
        compartment
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        vec![
            "scope-01", "scope-03", "scope-05", "scope-07", "scope-09", "scope-11", "scope-13",
            "scope-15", "scope-17", "scope-19", "scope-21", "scope-23"
        ]
    );
    let other_tenant = format!("other-{}", uuid::Uuid::new_v4().simple());
    assert!(
        permitted_collections(&state, &other_tenant, &doc, 3, &["engineering".into()])
            .await
            .unwrap()
            .is_empty()
    );
    let collection = &permitted[0];
    let (source, _, _) = retrieval
        .put_source(
            "",
            "text/plain",
            "old.txt",
            None,
            b"vacation original handbook",
        )
        .await
        .unwrap();
    retrieval
        .bind_source(&collection.id, &source, None)
        .await
        .unwrap();
    let old = retrieval
        .build_collection_index(&collection.id, 2000, 1, true)
        .await
        .unwrap();
    let (new_source, _, _) = retrieval
        .put_source(
            "",
            "text/plain",
            "new.txt",
            None,
            b"vacation replacement policy",
        )
        .await
        .unwrap();
    retrieval
        .bind_source(&collection.id, &new_source, None)
        .await
        .unwrap();
    let new = retrieval
        .build_collection_index(&collection.id, 2000, 2, true)
        .await
        .unwrap();
    assert_ne!(old.id, new.id);
    let pinned = retrieval
        .search_collection(
            &collection.id,
            "vacation",
            Default::default(),
            Some(&old.id),
        )
        .await
        .unwrap();
    assert_eq!(pinned.envelope.index_version, old.id);
    assert!(!pinned.hits.is_empty());
    assert!(pinned.hits.iter().all(|hit| hit.source_id == source));
    let current = retrieval
        .search_collection(&collection.id, "vacation", Default::default(), None)
        .await
        .unwrap();
    assert_eq!(current.envelope.index_version, new.id);
    assert!(current.hits.iter().any(|hit| hit.source_id == new_source));
    // A second connection changes current policy. A content pin cannot grant
    // the collection back to the next serving-boundary authorization check.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let id = collection.id.clone();
    let changed_tenant = tenant.clone();
    tokio::spawn(async move {
        sqlx::query("UPDATE collections SET access_level=4 WHERE tenant_id=$1 AND id=$2")
            .bind(changed_tenant)
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    })
    .await
    .unwrap();
    assert!(permitted_collections(&state, &tenant, &doc, 0, &[])
        .await
        .unwrap()
        .is_empty());
    sqlx::query(
        "UPDATE collections SET access_level=0, status='removed' WHERE tenant_id=$1 AND id=$2",
    )
    .bind(&tenant)
    .bind(&collection.id)
    .execute(state.pg_pool().unwrap())
    .await
    .unwrap();
    assert!(permitted_collections(&state, &tenant, &doc, 0, &[])
        .await
        .unwrap()
        .is_empty());
}
