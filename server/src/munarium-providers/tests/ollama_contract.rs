// SPDX-License-Identifier: Apache-2.0
use axum::{
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use munarium_core::{
    provider::{CompletionRequest, EmbeddingRequest, ProviderId},
    KernelError,
};
use munarium_providers::{
    build_provider, parse_provider_config, resolve_complete_model, resolve_config_credential,
    ModelTier,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn config(endpoint: &str) -> String {
    format!("apiVersion: munarium.ioka.io/v1\nkind: ProviderConfig\nmetadata: {{ name: local }}\nspec:\n  provider: ollama\n  endpoint: {endpoint}\n  models:\n    complete: [qwen3:1.7b]\n    embed: [all-minilm:22m]\n    fast: qwen3:1.7b\n")
}
fn request(model: &str) -> CompletionRequest {
    CompletionRequest {
        model: model.into(),
        system: Some("be brief".into()),
        prompt: "hello".into(),
        max_tokens: 32,
        temperature: Some(0.0),
        tools: None,
    }
}
type Calls = Arc<Mutex<Vec<(HeaderMap, Value)>>>;
async fn mock() -> (String, Calls) {
    let calls: Calls = Arc::default();
    let capture = calls.clone();
    let app = Router::new()
        .route("/proxy/api/chat", post(move |headers: HeaderMap, Json(body): Json<Value>| {
            let capture = capture.clone();
            async move {
                let mut calls = capture.lock().unwrap();
                calls.push((headers, body.clone()));
                let model = body["model"].as_str().unwrap();
                match model {
                    "missing" => (StatusCode::NOT_FOUND, "secret prompt echoed").into_response(),
                    "rate-limited" => (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "0")], "private key").into_response(),
                    "retry" if calls.iter().filter(|(_, v)| v["model"] == "retry").count() == 1 =>
                        (StatusCode::SERVICE_UNAVAILABLE, [("retry-after", "0")], "unavailable").into_response(),
                    "error-body" => Json(json!({"error": "private key"})).into_response(),
                    "bad-json" => "not JSON: secret prompt".into_response(),
                    _ => Json(json!({"message": {"role": "assistant", "content": "hello", "thinking": "private reasoning"}, "done": true, "done_reason": "stop", "prompt_eval_count": 9, "eval_count": 2})).into_response(),
                }
            }
        }))
        .route("/proxy/api/embed", post(|headers: HeaderMap, Json(body): Json<Value>| async move {
            assert!(headers.get("authorization").is_none());
            assert_eq!(body["truncate"], false);
            assert_eq!(body["input"], json!(["first", "second"]));
            Json(json!({"embeddings": [[1.0, 0.5], [0.5, 1.0]], "prompt_eval_count": 2}))
        }))
        .route("/proxy/api/tags", get(|| async { Json(json!({"models": [{"name": "qwen3:1.7b"}, {"name": "all-minilm:22m"}]})) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/proxy/", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (endpoint, calls)
}

#[test]
fn config_requires_endpoint_and_keeps_cloud_credentials_required() {
    let valid = config("http://ollama:11434");
    let doc = parse_provider_config(&valid).unwrap();
    assert!(resolve_config_credential(&doc.spec).unwrap().is_none());
    assert!(serde_yaml::to_string(&doc)
        .unwrap()
        .find("credentialRef")
        .is_none());
    assert_eq!(
        resolve_complete_model(&doc.spec, None, None).unwrap(),
        "qwen3:1.7b"
    );
    assert_eq!(
        resolve_complete_model(&doc.spec, None, Some(ModelTier::Fast)).unwrap(),
        "qwen3:1.7b"
    );
    assert!(resolve_complete_model(&doc.spec, None, Some(ModelTier::Frontier)).is_err());
    for family in ["openai", "anthropic", "openrouter", "unknown"] {
        assert!(parse_provider_config(
            &valid.replace("provider: ollama", &format!("provider: {family}"))
        )
        .is_err());
    }
    assert!(
        parse_provider_config(&valid.replace("  endpoint: http://ollama:11434\n", "")).is_err()
    );
    for endpoint in [
        "ftp://localhost",
        "http://user:password@localhost",
        "http://localhost?q=key",
        "http://localhost#secret",
        "relative",
    ] {
        assert!(
            parse_provider_config(&config(endpoint)).is_err(),
            "{endpoint}"
        );
    }
}

#[tokio::test]
async fn native_completion_embeddings_health_and_request_identity() {
    let (endpoint, calls) = mock().await;
    let mut doc = parse_provider_config(&config(&endpoint)).unwrap();
    let provider = build_provider(&doc).unwrap();
    assert_eq!(provider.id(), ProviderId::Ollama);
    assert!(provider.health().await.unwrap().healthy);
    let answer = provider.complete(request("qwen3:1.7b")).await.unwrap();
    assert_eq!(answer.text, "hello");
    assert_eq!((answer.input_tokens, answer.output_tokens), (9, 2));
    let again = provider.complete(request("qwen3:1.7b")).await.unwrap();
    assert_eq!(answer.request_hash, again.request_hash);
    let mut changed = request("qwen3:1.7b");
    changed.max_tokens = 64;
    assert_ne!(
        answer.request_hash,
        provider.complete(changed).await.unwrap().request_hash
    );
    {
        let calls = calls.lock().unwrap();
        let (headers, body) = &calls[0];
        assert!(headers.get("authorization").is_none());
        assert_eq!(body["stream"], false);
        assert_eq!(body["think"], false);
        assert_eq!(
            body["options"],
            json!({"num_predict": 32, "temperature": 0.0})
        );
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"], "hello");
    }
    let embedding = provider
        .embed(EmbeddingRequest {
            model: "all-minilm:22m".into(),
            inputs: vec!["first".into(), "second".into()],
        })
        .await
        .unwrap();
    assert_eq!(embedding.dimensions, 2);
    assert_eq!(embedding.vectors[1], vec![0.5, 1.0]);
    doc.spec.models.frontier = Some("not-installed".into());
    assert!(
        !build_provider(&doc)
            .unwrap()
            .health()
            .await
            .unwrap()
            .healthy
    );
    let mut tools = request("qwen3:1.7b");
    tools.tools = Some(json!([{"function": {"name": "test"}}]));
    assert!(matches!(
        provider.complete(tools).await,
        Err(KernelError::InvalidInput(_))
    ));
}

#[tokio::test]
async fn retries_are_bounded_and_failures_do_not_echo_bodies() {
    let (endpoint, calls) = mock().await;
    let provider = build_provider(&parse_provider_config(&config(&endpoint)).unwrap()).unwrap();
    provider.complete(request("retry")).await.unwrap();
    for model in ["missing", "rate-limited", "error-body", "bad-json"] {
        let error = provider.complete(request(model)).await.unwrap_err();
        assert!(!error.to_string().contains("private"));
        assert!(!error.to_string().contains("secret"));
        if model == "rate-limited" {
            assert!(matches!(error, KernelError::RateLimited(_)));
        }
    }
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls
            .iter()
            .filter(|(_, v)| v["model"] == "missing")
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|(_, v)| v["model"] == "rate-limited")
            .count(),
        3
    );
    assert_eq!(
        calls.iter().filter(|(_, v)| v["model"] == "retry").count(),
        2
    );
}

#[tokio::test]
async fn proxy_credentials_resolve_at_call_time_and_fail_closed() {
    let (endpoint, calls) = mock().await;
    let mut doc = parse_provider_config(&config(&endpoint)).unwrap();
    let env = format!("MUNARIUM_OLLAMA_CONTRACT_KEY_{}", std::process::id());
    doc.spec.credential_ref = Some(munarium_providers::CredentialRef::Env { env: env.clone() });
    let provider = build_provider(&doc).unwrap();
    assert!(provider.complete(request("qwen3:1.7b")).await.is_err());
    assert!(calls.lock().unwrap().is_empty());
    for key in ["first-test-key", "rotated-test-key"] {
        std::env::set_var(&env, key);
        provider.complete(request("qwen3:1.7b")).await.unwrap();
        assert_eq!(
            calls.lock().unwrap().last().unwrap().0["authorization"],
            format!("Bearer {key}")
        );
    }
    std::env::remove_var(env);
}
