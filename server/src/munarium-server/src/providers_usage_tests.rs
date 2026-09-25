// SPDX-License-Identifier: Apache-2.0
//! Scripted provider -> shared gateway -> real budget ledger regression tests.
use super::*;
use crate::config::{AuthMode, Config, DocIntelConfig, SourceStoreConfig, StoreKind};
use axum::http::StatusCode;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};

async fn test_state(database_url: Option<String>) -> Arc<AppState> {
    test_state_with_auth(database_url, AuthMode::Disabled).await
}

pub(crate) async fn test_state_with_auth(
    database_url: Option<String>,
    auth: AuthMode,
) -> Arc<AppState> {
    AppState::new(Config {
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
        auth,
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
        instance_id: "usage-test".into(),
        doc_intel: DocIntelConfig::None,
    })
    .await
    .unwrap()
}

struct Abort(tokio::task::JoinHandle<()>);
impl Drop for Abort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn settlement_case(
    state: &Arc<AppState>,
    family: &str,
    structured: bool,
    usage: Option<Value>,
    accounted: u64,
    input: u64,
    output: u64,
    model: &str,
) {
    let mut response = json!({"choices":[{"message":{"content":"fixture"},"finish_reason":"stop"}],
        "content":[{"type":"text","text":"fixture"}],"stop_reason":"end_turn"});
    if let Some(usage) = usage {
        response["usage"] = usage;
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let handler = move |Json(body): Json<Value>| {
        observed.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            body.get("response_format").is_some() || body.get("output_config").is_some(),
            structured
        );
        let response = response.clone();
        async move { Json(response) }
    };
    let app = axum::Router::new()
        .route("/chat/completions", axum::routing::post(handler.clone()))
        .route("/v1/messages", axum::routing::post(handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let _task = Abort(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap()
    }));
    let tenant = format!("usage-{}", uuid::Uuid::new_v4().simple());
    state.providers.apply(state, &tenant, &format!(
        "apiVersion: munarium.ioka.io/v1\nkind: ProviderConfig\nmetadata: {{ name: fixture }}\nspec:\n  provider: {family}\n  endpoint: {endpoint}\n  credentialRef: {{ env: MUNARIUM_USAGE_FIXTURE_KEY }}\n  models: {{ fast: {model} }}\n  budgets:\n    dailyTokens: {{ fast: 10 }}\n"
    )).await.unwrap();
    let store = state.store_for(&tenant).await.unwrap();
    let version = store.create_version(None, None).await.unwrap();
    let request = || {
        serde_json::from_value::<dto::CompleteRequest>(json!({
            "prompt":"test", "max_tokens":9, "tier":"fast", "version_id":version
        }))
        .unwrap()
    };
    let schema = structured.then(|| json!({"type":"object"}));
    let answer = complete_with_schema(
        state,
        &tenant,
        store.as_ref(),
        "fixture",
        request(),
        schema.clone(),
    )
    .await
    .unwrap();
    assert_eq!(answer.text, "fixture");
    assert_eq!((answer.input_tokens, answer.output_tokens), (input, output));
    let claim = store
        .get_claim(answer.invocation_event_id.as_deref().unwrap())
        .await
        .unwrap()
        .unwrap();
    let evidence = claim.evidence.unwrap();
    assert_eq!(evidence["input_tokens"], input);
    assert_eq!(evidence["output_tokens"], output);
    assert!(!evidence["request_hash"].as_str().unwrap().is_empty());
    for (direction, count) in [("input", input), ("output", output)] {
        let metric = format!("munarium_provider_tokens_total{{provider=\"{family}\",model=\"{model}\",direction=\"{direction}\"}} {count}\n");
        assert!(crate::metrics::render(state).contains(&metric), "{metric}");
    }
    let ledger = state.budgets().ledger(&tenant).await.unwrap();
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger[0].settled_units, accounted, "{family} {model}");
    assert_eq!(ledger[0].held_units, 0);
    let again =
        complete_with_schema(state, &tenant, store.as_ref(), "fixture", request(), schema).await;
    if accounted == 0 {
        assert!(again.is_ok(), "observed zero must free capacity");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    } else {
        assert!(matches!(again, Err(KernelError::RateLimited(_))));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "denial must precede dispatch"
        );
    }
    // A new PostgreSQL pool must observe the same settled amount.
    if let Some(url) = &state.config.database_url {
        let pool = sqlx::PgPool::connect(url).await.unwrap();
        let reopened = munarium_store_pg::PgBudgetStore::new(pool.clone());
        use munarium_core::budget::BudgetStore;
        assert_eq!(
            reopened.ledger(&tenant).await.unwrap()[0].settled_units,
            accounted
        );
        let id: String = sqlx::query_scalar("SELECT id FROM token_budget_reservations WHERE tenant_id = $1 ORDER BY created_at LIMIT 1")
            .bind(&tenant).fetch_one(&pool).await.unwrap();
        let stored = reopened.evidence(&tenant, &id).await.unwrap().unwrap();
        assert_eq!(stored.original_units, Some(10));
        assert_eq!(stored.accounted_units, accounted);
        let usage = stored
            .usage
            .expect("gateway persists usage even for absent or overflowing counts");
        assert_eq!(usage.input_tokens.unwrap_or(0), input);
        assert_eq!(usage.output_tokens.unwrap_or(0), output);
        pool.close().await;
    }
}

async fn settlement_cases(state: Arc<AppState>) {
    for family in ["openai", "openrouter", "anthropic"] {
        for structured in [false, true] {
            for (case, (usage, accounted, input, output)) in [
                (None, 10, 0, 0),
                (Some(json!({})), 10, 0, 0),
                (Some(Value::Null), 10, 0, 0),
                (Some(json!({"input_tokens":0,"prompt_tokens":0,"output_tokens":0,"completion_tokens":0})), 0, 0, 0),
                (Some(json!({"input_tokens":3,"prompt_tokens":3})), 10, 3, 0),
                (Some(json!({"output_tokens":15,"completion_tokens":15})), 15, 0, 15),
                (Some(json!({"input_tokens":"bad","prompt_tokens":"bad","output_tokens":15,"completion_tokens":15})), 15, 0, 15),
                (Some(json!({"input_tokens":2,"prompt_tokens":2,"output_tokens":3,"completion_tokens":3})), 5, 2, 3),
                (Some(json!({"input_tokens":u64::MAX,"prompt_tokens":u64::MAX,"output_tokens":1,"completion_tokens":1})), 10, u64::MAX, 1),
            ].into_iter().enumerate() {
                settlement_case(&state, family, structured, usage, accounted, input, output,
                    &format!("fixture-{case}-{structured}")).await;
            }
        }
    }
}

#[tokio::test]
async fn usage_settlement_memory_and_postgres() {
    // One test owns this synthetic credential for both store variants.
    let previous = std::env::var_os("MUNARIUM_USAGE_FIXTURE_KEY");
    struct Restore(Option<std::ffi::OsString>);
    impl Drop for Restore {
        fn drop(&mut self) {
            match &self.0 {
                Some(v) => std::env::set_var("MUNARIUM_USAGE_FIXTURE_KEY", v),
                None => std::env::remove_var("MUNARIUM_USAGE_FIXTURE_KEY"),
            }
        }
    }
    let _restore = Restore(previous);
    std::env::set_var("MUNARIUM_USAGE_FIXTURE_KEY", "fictional-test-key");
    settlement_cases(test_state(None).await).await;
    if let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") {
        settlement_cases(test_state(Some(url)).await).await;
    } else {
        eprintln!(
            "PostgreSQL usage settlement NOT RUN: MUNARIUM_TEST_DATABASE_URL unset; memory only"
        );
    }
}

// A logical gateway call can contain multiple physical HTTP submissions. These
// tests pin token admission separately from durable monetary attempt coverage.
#[tokio::test]
async fn dispatch_retries_cancellation_and_uncapped_policy() {
    let mut databases = vec![None];
    if let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") {
        databases.push(Some(url));
    } else {
        eprintln!("PostgreSQL dispatch tests NOT RUN: test database unset");
    }
    for database in databases {
        let state = test_state(database).await;
        for scenario in [
            "retry-success",
            "exhausted",
            "cancel",
            "denied",
            "unpolled",
            "uncapped",
        ] {
            let calls = Arc::new(AtomicUsize::new(0));
            let observed = calls.clone();
            let arrived = Arc::new(tokio::sync::Notify::new());
            let signal = arrived.clone();
            let app = axum::Router::new().route("/api/chat", axum::routing::post(move || {
                let attempt = observed.fetch_add(1, Ordering::SeqCst);
                let signal = signal.clone();
                async move {
                    signal.notify_one();
                    if scenario == "cancel" {
                        std::future::pending::<()>().await;
                    }
                    let status = if scenario == "exhausted" { StatusCode::TOO_MANY_REQUESTS }
                        else if scenario == "retry-success" && attempt == 0 { StatusCode::SERVICE_UNAVAILABLE }
                        else { StatusCode::OK };
                    (status, [("retry-after", "0")], Json(json!({
                        "done":true, "done_reason":"stop", "message":{"role":"assistant", "content":"fixture"},
                        "prompt_eval_count":2, "eval_count":1
                    })))
                }
            }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let _task = Abort(tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap()
            }));
            let tenant = format!("dispatch-{}", uuid::Uuid::new_v4().simple());
            state.providers.apply(&state, &tenant, &format!(
                "apiVersion: munarium.ioka.io/v1\nkind: ProviderConfig\nmetadata: {{ name: fixture }}\nspec:\n  provider: ollama\n  endpoint: {endpoint}\n  models: {{ fast: fixture, complete: [fixture] }}\n  budgets:\n    dailyTokens: {{ fast: 10 }}\n"
            )).await.unwrap();
            if matches!(scenario, "uncapped" | "retry-success") {
                if let Some(pool) = state.pg_pool() {
                    let price = serde_json::from_value(json!({
                        "id":"fictional-p14","provider":"ollama","model":"fixture","currency":"USD",
                        "route":munarium_providers::accounting::route_identity(&format!("{endpoint}/api/chat"),None).unwrap(),
                        "valid_from":"2020-01-01T00:00:00Z","valid_until":"2090-01-01T00:00:00Z","basis":"inclusive",
                        "rates":{"input":{"micro_units":1,"per_tokens":3},"output":{"micro_units":0,"per_tokens":1}}
                    })).unwrap();
                    munarium_store_pg::money::MoneyStore(pool.clone())
                        .add_price(&tenant, &price)
                        .await
                        .unwrap();
                }
            }
            let store = state.store_for(&tenant).await.unwrap();
            let request = serde_json::from_value(json!({"prompt":"test", "max_tokens":9,
                "tier": if scenario == "uncapped" { Value::Null } else { json!("fast") },
                "model": if scenario == "uncapped" { json!("fixture") } else { Value::Null }
            }))
            .unwrap();
            if scenario == "denied" {
                state
                    .budgets()
                    .reserve(&tenant, "fixture", "fast", 10, Some(10))
                    .await
                    .unwrap();
            }
            let future =
                complete_with_schema(&state, &tenant, store.as_ref(), "fixture", request, None);
            if scenario == "unpolled" {
                drop(future);
            } else if scenario == "cancel" {
                // Drop after the physical server observed a submission; never
                // infer zero work or refund this held reservation.
                let mut future = Box::pin(future);
                tokio::time::timeout(std::time::Duration::from_secs(10), async {
                    tokio::select! {
                        _ = arrived.notified() => {},
                        result = &mut future => panic!("unexpected result: {result:?}"),
                    }
                })
                .await
                .unwrap();
                drop(future);
            } else {
                let result = future.await;
                if matches!(scenario, "exhausted" | "denied") {
                    assert!(matches!(result, Err(KernelError::RateLimited(_))));
                } else {
                    assert!(result.is_ok(), "{scenario}: {result:?}");
                }
            }
            let physical = calls.load(Ordering::SeqCst);
            if let Some(pool) = state.pg_pool() {
                let report = munarium_store_pg::money::MoneyStore(pool.clone())
                    .report(
                        &tenant,
                        chrono::Utc::now() - chrono::Duration::hours(1),
                        chrono::Utc::now() + chrono::Duration::hours(1),
                    )
                    .await
                    .unwrap();
                assert_eq!(report.coverage.attempts, physical as u64, "{scenario}");
                let unresolved = match scenario {
                    "retry-success" => 1,
                    "exhausted" => 3,
                    "cancel" => 1,
                    _ => 0,
                };
                assert_eq!(report.coverage.unresolved, unresolved, "{scenario}");
                if matches!(scenario, "uncapped" | "retry-success") {
                    assert_eq!(report.coverage.missing_price, 0);
                    assert_eq!(report.coverage.priced, 1);
                    assert_eq!(report.known_subtotals_micro_units["USD"], "1");
                } else {
                    assert_eq!(report.coverage.missing_price, physical as u64);
                }
                if !report.attempts.is_empty() {
                    let invocation = &report.attempts[0].invocation_id;
                    assert!(report
                        .attempts
                        .iter()
                        .all(|a| &a.invocation_id == invocation));
                }
            }
            let ledger = state.budgets().ledger(&tenant).await.unwrap();
            match scenario {
                "unpolled" => {
                    assert_eq!(physical, 0);
                    assert!(ledger.is_empty());
                }
                "uncapped" => {
                    assert_eq!(physical, 1);
                    assert!(ledger.is_empty());
                }
                "denied" => {
                    assert_eq!(physical, 0);
                    assert_eq!(ledger[0].held_units, 10);
                }
                "cancel" => {
                    assert_eq!(physical, 1);
                    assert_eq!(ledger[0].held_units, 10);
                    assert_eq!(ledger[0].settled_units, 0);
                }
                "exhausted" => {
                    assert_eq!(physical, 3);
                    assert_eq!(ledger[0].settled_units, 10);
                }
                _ => {
                    assert_eq!(physical, 2);
                    assert_eq!(ledger[0].settled_units, 3);
                }
            }
        }
    }
}

#[tokio::test]
async fn monetary_embeddings_capture_only_cache_misses() {
    let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
        eprintln!("unavailable: isolated PostgreSQL not configured");
        return;
    };
    let state = test_state(Some(url)).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let app = axum::Router::new().route(
        "/api/embed",
        axum::routing::post(move || {
            count.fetch_add(1, Ordering::SeqCst);
            async { Json(json!({"embeddings":[[1.0,2.0]],"prompt_eval_count":2})) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let _server = Abort(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap()
    }));
    let tenant = format!("embed-money-{}", uuid::Uuid::new_v4());
    state.providers.apply(&state,&tenant,&format!("apiVersion: munarium.ioka.io/v1\nkind: ProviderConfig\nmetadata: {{name: fixture}}\nspec:\n  provider: ollama\n  endpoint: {endpoint}\n  models: {{embed: [fixture]}}\n")).await.unwrap();
    let ledger = state.store_for(&tenant).await.unwrap();
    for _ in 0..2 {
        let req = serde_json::from_value(json!({"inputs":["fictional input"],"model":"fixture"}))
            .unwrap();
        op_embed(&state, &tenant, ledger.as_ref(), "fixture", req)
            .await
            .unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let report = munarium_store_pg::money::MoneyStore(state.pg_pool().unwrap().clone())
        .report(
            &tenant,
            chrono::Utc::now() - chrono::Duration::hours(1),
            chrono::Utc::now() + chrono::Duration::hours(1),
        )
        .await
        .unwrap();
    assert_eq!(report.coverage.attempts, 1);
    assert_eq!(report.observed_input_tokens, "2");
    assert_eq!(report.observed_output_tokens, "0");
    assert_eq!(report.coverage.missing_price, 1);
    assert_eq!(report.coverage.missing_usage, 0);
}
