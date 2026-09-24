// SPDX-License-Identifier: Apache-2.0
//! Scripted provider -> shared gateway -> real budget ledger regression tests.
use super::*;
use crate::config::{AuthMode, Config, DocIntelConfig, SourceStoreConfig, StoreKind};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};

async fn test_state(database_url: Option<String>) -> Arc<AppState> {
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
