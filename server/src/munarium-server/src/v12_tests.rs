// SPDX-License-Identifier: Apache-2.0
//! Versioned vocabulary and citation contracts against an isolated PostgreSQL tenant.
use crate::{
    config::{AuthMode, Config, DocIntelConfig, SourceStoreConfig, StoreKind},
    state::AppState,
};
use axum::{
    body::Body,
    http::{Request, StatusCode},
    Json,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    token: &str,
    body: Value,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {token}"))
        .header("X-Munarium-Uid", "fixture-user")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1_000_000)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn collection_vocabulary_generation_authorization_revisions_and_query_application() {
    let Ok(database_url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
        eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
        return;
    };
    let tenant = format!("vocabulary-{}", uuid::Uuid::new_v4().simple());
    let state = AppState::new(Config {
        http_addr: "127.0.0.1:0".into(),
        grpc_addr: None,
        ops_addr: "127.0.0.1:0".into(),
        store: StoreKind::Postgres,
        database_url: Some(database_url),
        auth: AuthMode::Static(vec![
            ("fixture-rw".into(), tenant.clone(), "rw".into()),
            ("fixture-ro".into(), tenant.clone(), "ro".into()),
        ]),
        shutdown_grace_secs: 1,
        token_secret: Some(vec![47; 32]),
        token_ttl_secs: 3600,
        require_uid: true,
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
        max_tokens: munarium_api_types::MaxTokensBudgets::default(),
        instance_id: "routing-test".into(),
        source_store: SourceStoreConfig::Pg,
        doc_intel: DocIntelConfig::None,
    })
    .await
    .unwrap();
    let calls = Arc::new(Mutex::new(Vec::<Value>::new()));
    let output = Arc::new(Mutex::new(
        json!({"groups":[["purchase order","PO"]]}).to_string(),
    ));
    let capture = calls.clone();
    let reply = output.clone();
    let provider = axum::Router::new().route("/api/chat", axum::routing::post(move |Json(body): Json<Value>| {
        let capture = capture.clone(); let reply = reply.clone();
        async move { capture.lock().unwrap().push(body.clone()); Json(json!({"model":body["model"],"done":true,"done_reason":"stop",
            "message":{"role":"assistant","content":reply.lock().unwrap().clone()},"prompt_eval_count":20,"eval_count":12})) }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, provider).await.unwrap() });
    state.providers.apply(&state, &tenant, &format!("apiVersion: munarium.ioka.io/v1\nkind: ProviderConfig\nmetadata: {{name: fixture}}\nspec:\n  provider: ollama\n  endpoint: {endpoint}\n  models:\n    complete: [fixture-model]\n    fast: fixture-model\n")).await.unwrap();
    let retrieval = state.retrieval_for(&tenant).unwrap();
    let collection = retrieval
        .ensure_collection("manuals", "text@1", 1, &["manuals".into()], None)
        .await
        .unwrap();
    let other = retrieval
        .ensure_collection("private", "text@1", 2, &["private".into()], None)
        .await
        .unwrap();
    let text = "A purchase order requires supervisor approval before goods are ordered.";
    let (source, hash, _) = retrieval
        .put_source(
            "",
            "text/plain",
            "manuals/ordering.txt",
            None,
            text.as_bytes(),
        )
        .await
        .unwrap();
    retrieval
        .bind_source(&collection.id, &source, None)
        .await
        .unwrap();
    let (foreign, _, _) = retrieval
        .put_source(
            "",
            "text/plain",
            "private/private.txt",
            None,
            b"Private unrelated material",
        )
        .await
        .unwrap();
    retrieval
        .bind_source(&other.id, &foreign, None)
        .await
        .unwrap();
    let index = retrieval
        .build_collection_index(&collection.id, 500, 1, true)
        .await
        .unwrap();
    let (token, _) = munarium_access::issue(
        &[47; 32],
        "fixture-user",
        &tenant,
        1,
        vec!["manuals".into()],
        vec!["vocabulary".into()],
        None,
        600,
        "vocab-fixture".into(),
    )
    .unwrap();
    let (query, _) = munarium_access::issue(
        &[47; 32],
        "fixture-user",
        &tenant,
        1,
        vec!["manuals".into()],
        vec!["query".into()],
        None,
        600,
        "query-fixture".into(),
    )
    .unwrap();
    let app = crate::rest::router(state.clone());
    let path = format!("/v1.2/collections/{}/vocabulary", collection.id);
    let (status, mut defaults) = call(
        &app,
        "GET",
        "/v1.2/vocabulary-settings",
        "fixture-rw",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(defaults["auto_generate"], true);
    assert_eq!(defaults["sampling"]["document_count"], 12);
    defaults["provider"] = json!("fixture");
    let (status, configured) = call(
        &app,
        "PUT",
        "/v1.2/vocabulary-settings",
        "fixture-rw",
        defaults.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{configured}");
    assert_eq!(
        call(
            &app,
            "PUT",
            "/v1.2/vocabulary-settings",
            "fixture-rw",
            defaults
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(&app, "GET", &path, "fixture-ro", Value::Null).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "GET", &path, &query, Value::Null).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("/v1.2/collections/{}/vocabulary", other.id),
            &token,
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &app,
            "GET",
            &(path.clone() + "/revision"),
            &query,
            Value::Null
        )
        .await
        .1["revision"],
        0
    );
    let refresh = path.clone() + "/refresh";
    assert_eq!(
        call(
            &app,
            "POST",
            &refresh,
            &token,
            json!({"revision":0,"source_ids":[foreign]})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert!(calls.lock().unwrap().is_empty());
    let (status, generated) = call(
        &app,
        "POST",
        &refresh,
        &token,
        json!({"revision":0,"source_ids":[source]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{generated}");
    assert_eq!(generated["origin"], "generated");
    assert_eq!(generated["groups"], json!([["purchase order", "PO"]]));
    assert_eq!(calls.lock().unwrap().len(), 1);
    let (status, found) = call(
        &app,
        "POST",
        "/v1.2/search",
        &query,
        json!({"collection":collection.id,"query":"PO","index_version":index.id,"top_k":4}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{found}");
    assert!(found["expanded_query"]
        .as_str()
        .unwrap()
        .contains("purchase order"));
    assert_eq!(found["hits"][0]["source_content_hash"], hash);
    assert_eq!(found["hits"][0]["metadata"]["location"]["utf8_start"], 0);
    let revision = generated["revision"].as_i64().unwrap();
    let (status, disabled) = call(
        &app,
        "PATCH",
        &path,
        &token,
        json!({"revision":revision,"enabled":false}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{disabled}");
    assert_eq!(disabled["groups"], generated["groups"]);
    assert_eq!(
        call(
            &app,
            "PATCH",
            &path,
            &token,
            json!({"revision":revision,"enabled":true})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let (_, plain) = call(
        &app,
        "POST",
        "/v1.2/search",
        &query,
        json!({"collection":collection.id,"query":"PO","index_version":index.id}),
    )
    .await;
    assert_eq!(plain["expanded_query"], "PO");
    let (status, manual) = call(&app,"PUT",&path,&token,json!({"revision":disabled["revision"],"enabled":true,"auto_generate":false,"sampling":null,"groups":[["purchase order","procurement request"]]})).await;
    assert_eq!(status, StatusCode::OK, "{manual}");
    assert_eq!(manual["origin"], "manual");
    *output.lock().unwrap() =
        json!({"groups":[["unrelated topic","unrelated synonym"]]}).to_string();
    assert!(call(
        &app,
        "POST",
        &refresh,
        &token,
        json!({"revision":manual["revision"]})
    )
    .await
    .0
    .is_server_error());
    let (_, retained) = call(&app, "GET", &path, &token, Value::Null).await;
    assert_eq!(retained["groups"], manual["groups"]);
    assert_eq!(retained["status"], "generation-failed");
    let answer_body = json!({"question":"Who approves a purchase order?","expected_provider":"ollama","expected_model":"fixture-model",
        "sources":[{"id":"c1","collection":collection.id,"index_version":index.id,"source_id":source,"source_path":"manuals/ordering.txt","source_content_hash":hash,"text":text}]});
    *output.lock().unwrap() = json!({"status":"supported","answer":"A supervisor must approve the purchase order before ordering.","citations":[{"id":"c1","quote":text}]}).to_string();
    let count = calls.lock().unwrap().len();
    let mut bad = answer_body.clone();
    bad["expected_provider"] = json!("openai");
    assert_eq!(
        call(&app, "POST", "/v1.2/answers", &query, bad).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(calls.lock().unwrap().len(), count);
    let (status, answer) = call(&app, "POST", "/v1.2/answers", &query, answer_body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(answer["references"][0]["source_id"], source);
    assert_eq!(
        answer["references"][0]["metadata"]["location"]["utf8_start"],
        0
    );
    assert!(answer["references"][0].get("url").is_none());
    let mut corrupt = answer_body;
    corrupt["sources"][0]["source_content_hash"] = json!("wrong");
    let count = calls.lock().unwrap().len();
    assert!(call(&app, "POST", "/v1.2/answers", &query, corrupt)
        .await
        .0
        .is_client_error());
    assert_eq!(calls.lock().unwrap().len(), count);
    // The default-on ingest worker creates a vocabulary for a newly bound
    // collection, while retaining an explicitly edited vocabulary.
    call(
        &app,
        "PATCH",
        &format!("/v1.2/collections/{}/vocabulary", other.id),
        "fixture-rw",
        json!({"revision":0,"auto_generate":false}),
    )
    .await;
    let automatic = retrieval
        .ensure_collection("automatic", "text@1", 1, &["automatic".into()], None)
        .await
        .unwrap();
    retrieval
        .bind_source(&automatic.id, &source, None)
        .await
        .unwrap();
    sqlx::query("UPDATE collection_sources SET bound_at=now()-interval '2 minutes' WHERE tenant_id=$1 AND collection_id=$2")
        .bind(&tenant).bind(&automatic.id).execute(state.pg_pool().unwrap()).await.unwrap();
    *output.lock().unwrap() = json!({"groups":[["purchase order","PO"]]}).to_string();
    crate::vocabulary_api::automatic_batch(&state, &tenant)
        .await
        .unwrap();
    let automatic_result = crate::vocabulary_api::load(&state, &tenant, &automatic.id)
        .await
        .unwrap();
    assert_eq!(automatic_result.origin, "generated");
    assert_eq!(automatic_result.sampled_sources, vec![source.clone()]);
    assert_eq!(automatic_result.groups, vec![vec!["purchase order", "PO"]]);
    // Durable reads do not depend on an application-side dictionary.
    assert_eq!(
        crate::vocabulary_api::load(&state, &tenant, &collection.id)
            .await
            .unwrap()
            .revision,
        retained["revision"].as_i64().unwrap()
    );
    task.abort();
}
