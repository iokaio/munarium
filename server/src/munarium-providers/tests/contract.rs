// SPDX-License-Identifier: Apache-2.0
//! Provider contract tests against an in-process mock (recorded-fixture
//! style responses). Live smokes run only with user-supplied sandbox keys
//! behind MUNARIUM_LIVE_PROVIDER_TESTS=1 — never in CI.

use axum::routing::{get, post};
use axum::{Json, Router};
use munarium_core::provider::{CompletionRequest, EmbeddingRequest, ModelProvider};
use munarium_providers::{AnthropicProvider, CredentialRef, OpenAiProvider};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

async fn spawn_mock() -> (String, Arc<AtomicU32>) {
    let attempts = Arc::new(AtomicU32::new(0));
    let attempts2 = attempts.clone();

    let app = Router::new()
        .route(
            "/v1/messages",
            post(|Json(body): Json<serde_json::Value>| async move {
                assert_eq!(body["messages"][0]["role"], "user");
                // Optional fields must be OMITTED, never sent as null — the
                // real Messages API 400s on `system: null` (found live).
                for (k, v) in body.as_object().expect("object body") {
                    assert!(!v.is_null(), "field '{k}' sent as null");
                }
                Json(serde_json::json!({
                    "content": [{ "type": "text", "text": "the harbor bell rang twice" }],
                    "stop_reason": "end_turn",
                    "usage": { "input_tokens": 21, "output_tokens": 7 }
                }))
            }),
        )
        .route(
            "/v1/models",
            get(|| async { Json(serde_json::json!({ "data": [] })) }),
        )
        .route(
            "/chat/completions",
            post(move |Json(body): Json<serde_json::Value>| {
                let attempts = attempts2.clone();
                async move {
                    // Modern OpenAI models reject `max_tokens`; the openai
                    // dialect must send `max_completion_tokens` (openrouter
                    // keeps `max_tokens`). Nulls are never sent.
                    assert!(
                        body.get("max_completion_tokens").is_some()
                            || body.get("max_tokens").is_some(),
                        "a token-cap field is required"
                    );
                    for (k, v) in body.as_object().expect("object body") {
                        assert!(!v.is_null(), "field '{k}' sent as null");
                    }
                    // first call 429s with retry-after: 0 — the client must retry
                    if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        return (
                            axum::http::StatusCode::TOO_MANY_REQUESTS,
                            [("retry-after", "0")],
                            Json(serde_json::json!({ "error": "slow down" })),
                        )
                            .into_response();
                    }
                    Json(serde_json::json!({
                        "choices": [{ "message": { "content": "42" }, "finish_reason": "stop" }],
                        "usage": { "prompt_tokens": 9, "completion_tokens": 1 }
                    }))
                    .into_response()
                }
            }),
        )
        .route(
            "/embeddings",
            post(|Json(body): Json<serde_json::Value>| async move {
                let n = body["input"].as_array().map(|a| a.len()).unwrap_or(0);
                let data: Vec<serde_json::Value> = (0..n)
                    .map(|i| serde_json::json!({ "embedding": [i as f64, 0.5, 0.25] }))
                    .collect();
                Json(serde_json::json!({ "data": data }))
            }),
        );

    use axum::response::IntoResponse;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), attempts)
}

fn test_cred() -> CredentialRef {
    std::env::set_var("MUNARIUM_TEST_PROVIDER_KEY", "sk-test-not-a-real-key");
    CredentialRef::Env {
        env: "MUNARIUM_TEST_PROVIDER_KEY".into(),
    }
}

#[tokio::test]
async fn legacy_provider_defaults_preserve_structured_override_and_charge_conservatively() {
    use munarium_core::provider::{
        CompletionResponse, EmbeddingResponse, ProviderHealth, ProviderId, UsageSource,
    };
    struct Legacy(AtomicU32);
    #[async_trait::async_trait]
    impl ModelProvider for Legacy {
        fn id(&self) -> ProviderId {
            ProviderId::Openai
        }
        async fn complete(
            &self,
            req: CompletionRequest,
        ) -> munarium_core::Result<CompletionResponse> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(CompletionResponse {
                text: req.prompt,
                stop_reason: "stop".into(),
                input_tokens: 0,
                output_tokens: 0,
                request_hash: "fixture".into(),
            })
        }
        async fn complete_structured(
            &self,
            mut req: CompletionRequest,
            schema: serde_json::Value,
        ) -> munarium_core::Result<CompletionResponse> {
            assert_eq!(schema["type"], "object");
            req.prompt = "structured override".into();
            self.complete(req).await
        }
        async fn embed(&self, _: EmbeddingRequest) -> munarium_core::Result<EmbeddingResponse> {
            unreachable!()
        }
        async fn health(&self) -> munarium_core::Result<ProviderHealth> {
            unreachable!()
        }
    }
    let provider = Legacy(AtomicU32::new(0));
    let req = CompletionRequest {
        model: "fixture".into(),
        system: None,
        prompt: "ordinary".into(),
        max_tokens: 9,
        temperature: None,
        tools: None,
    };
    let ordinary = provider.complete_detailed(req.clone()).await.unwrap();
    let structured = provider
        .complete_structured_detailed(req, serde_json::json!({"type":"object"}))
        .await
        .unwrap();
    assert_eq!(ordinary.response.text, "ordinary");
    assert_eq!(structured.response.text, "structured override");
    for out in [ordinary, structured] {
        assert_eq!(out.usage.source, UsageSource::LegacyUnverified);
        assert_eq!(out.usage.accounted_units(10).unwrap(), 10);
    }
    assert_eq!(provider.0.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn usage_evidence_preserves_missing_partial_malformed_and_explicit_zero() {
    use munarium_core::provider::{UsageEvidence, UsageSource};
    use serde_json::{json, Value};
    let cases = [
        (None, None, None, UsageSource::Missing),
        (Some(json!({})), None, None, UsageSource::Missing),
        (Some(Value::Null), None, None, UsageSource::Malformed),
        (
            Some(json!({"input":0,"output":0})),
            Some(0),
            Some(0),
            UsageSource::ProviderReported,
        ),
        (
            Some(json!({"input":3})),
            Some(3),
            None,
            UsageSource::ProviderReported,
        ),
        (
            Some(json!({"output":7})),
            None,
            Some(7),
            UsageSource::ProviderReported,
        ),
        (
            Some(json!({"input":null,"output":7})),
            None,
            Some(7),
            UsageSource::Malformed,
        ),
        (
            Some(json!({"input":"3","output":7})),
            None,
            Some(7),
            UsageSource::Malformed,
        ),
        (
            Some(json!({"input":-1,"output":7})),
            None,
            Some(7),
            UsageSource::Malformed,
        ),
        (
            Some(json!({"input":1.5,"output":7})),
            None,
            Some(7),
            UsageSource::Malformed,
        ),
        (
            Some(serde_json::from_str(r#"{"input":18446744073709551616,"output":7}"#).unwrap()),
            None,
            Some(7),
            UsageSource::Malformed,
        ),
    ];
    for (usage, input_tokens, output_tokens, source) in cases {
        let mut body = json!({"choices":[{"message":{"content":"fixture"},"finish_reason":"stop"}],
            "content":[{"type":"text","text":"fixture"}],"stop_reason":"end_turn"});
        if let Some(mut usage) = usage {
            if let Some(obj) = usage.as_object_mut() {
                if let Some(v) = obj.remove("input") {
                    obj.insert("prompt_tokens".into(), v.clone());
                    obj.insert("input_tokens".into(), v);
                }
                if let Some(v) = obj.remove("output") {
                    obj.insert("completion_tokens".into(), v.clone());
                    obj.insert("output_tokens".into(), v);
                }
            }
            body["usage"] = usage;
        }
        let calls = Arc::new(AtomicU32::new(0));
        let observed = calls.clone();
        let handler = move || {
            observed.fetch_add(1, Ordering::SeqCst);
            let body = body.clone();
            async move { Json(body) }
        };
        let app = Router::new()
            .route("/chat/completions", post(handler.clone()))
            .route("/v1/messages", post(handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let providers: Vec<Box<dyn ModelProvider>> = vec![
            Box::new(OpenAiProvider::new(Some(&endpoint), test_cred())),
            Box::new(OpenAiProvider::openrouter(Some(&endpoint), test_cred())),
            Box::new(AnthropicProvider::new(Some(&endpoint), test_cred())),
        ];
        for provider in providers {
            let req = CompletionRequest {
                model: "fixture".into(),
                system: None,
                prompt: "test".into(),
                max_tokens: 9,
                temperature: None,
                tools: None,
            };
            let expected = UsageEvidence {
                input_tokens,
                output_tokens,
                source,
            };
            let detailed = provider.complete_detailed(req.clone()).await.unwrap();
            let legacy = provider.complete(req.clone()).await.unwrap();
            let structured = provider
                .complete_structured_detailed(req.clone(), json!({"type":"object"}))
                .await
                .unwrap();
            let legacy_structured = provider
                .complete_structured(req, json!({"type":"object"}))
                .await
                .unwrap();
            assert_eq!(detailed.usage, expected);
            assert_eq!(structured.usage, expected);
            assert_eq!(legacy.request_hash, detailed.response.request_hash);
            assert_eq!(
                legacy_structured.request_hash,
                structured.response.request_hash
            );
            assert_ne!(legacy.request_hash, legacy_structured.request_hash);
            assert_eq!(legacy.input_tokens, input_tokens.unwrap_or(0));
            assert_eq!(legacy.output_tokens, output_tokens.unwrap_or(0));
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            12,
            "one dispatch per method call"
        );
        task.abort();
    }
}

#[tokio::test]
async fn configured_openrouter_route_is_explicit_and_legacy_routes_are_unchanged() {
    let recorded = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
    let sink = recorded.clone();
    let app = Router::new().route("/chat/completions", post(move |Json(body): Json<serde_json::Value>| {
        sink.lock().unwrap().push(body);
        async { Json(serde_json::json!({"choices":[{"message":{"content":"fixture"},"finish_reason":"stop"}]})) }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    for downstream in [None, Some("fixture/vendor".to_string())] {
        let mut provider = OpenAiProvider::openrouter(Some(&endpoint), test_cred());
        provider.downstream_provider = downstream;
        provider
            .complete(CompletionRequest {
                model: "fixture/model".into(),
                system: None,
                prompt: "fixture".into(),
                max_tokens: 64,
                temperature: None,
                tools: None,
            })
            .await
            .unwrap();
    }
    let calls = recorded.lock().unwrap();
    assert!(calls[0].get("provider").is_none());
    assert_eq!(
        calls[1]["provider"],
        serde_json::json!({"only":["fixture/vendor"],"allow_fallbacks":false,"require_parameters":true,"data_collection":"deny"})
    );
    assert_eq!(calls[1]["max_tokens"], 64);
    assert!(calls[1].get("max_completion_tokens").is_none());
    server.abort();
}

#[tokio::test]
async fn anthropic_messages_dialect() {
    let (base, _) = spawn_mock().await;
    let p = AnthropicProvider::new(Some(&base), test_cred());
    let out = p
        .complete(CompletionRequest {
            model: "claude-sonnet-4-6".into(),
            system: Some("be brief".into()),
            prompt: "what rang?".into(),
            max_tokens: 64,
            temperature: None,
            tools: None,
        })
        .await
        .expect("complete");
    assert_eq!(out.text, "the harbor bell rang twice");
    assert_eq!(out.stop_reason, "end_turn");
    assert_eq!((out.input_tokens, out.output_tokens), (21, 7));
    assert!(!out.request_hash.is_empty());

    let health = p.health().await.expect("health");
    assert!(health.healthy);
}

#[tokio::test]
async fn structured_completion_is_scoped_to_the_request_in_each_hosted_dialect() {
    let recorded = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
    let sink = recorded.clone();
    let handler = move |Json(body): Json<serde_json::Value>| {
        sink.lock().unwrap().push(body);
        async {
            Json(
                serde_json::json!({"choices":[{"message":{"content":"fixture"},"finish_reason":"stop"}],
            "content":[{"type":"text","text":"fixture"}],"stop_reason":"end_turn","usage":{}}),
            )
        }
    };
    let app = Router::new()
        .route("/chat/completions", post(handler.clone()))
        .route("/v1/messages", post(handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let schema = serde_json::json!({"type":"object","additionalProperties":false,"required":["answer"],"properties":{"answer":{"type":"string"}}});
    let providers: Vec<Box<dyn ModelProvider>> = vec![
        Box::new(OpenAiProvider::new(Some(&endpoint), test_cred())),
        Box::new(OpenAiProvider::openrouter(Some(&endpoint), test_cred())),
        Box::new(AnthropicProvider::new(Some(&endpoint), test_cred())),
    ];
    for provider in providers {
        let request = CompletionRequest {
            model: "fixture".into(),
            system: None,
            prompt: "Return JSON".into(),
            max_tokens: 64,
            temperature: None,
            tools: None,
        };
        let before = provider.complete(request.clone()).await.unwrap();
        let structured = provider
            .complete_structured(request.clone(), schema.clone())
            .await
            .unwrap();
        let after = provider.complete(request).await.unwrap();
        assert_eq!(before.request_hash, after.request_hash);
        assert_ne!(before.request_hash, structured.request_hash);
    }
    let calls = recorded.lock().unwrap();
    for i in [0, 2, 3, 5, 6, 8] {
        assert!(calls[i].get("response_format").is_none());
        assert!(calls[i].get("output_config").is_none());
    }
    for i in [1, 4] {
        assert_eq!(calls[i]["response_format"]["json_schema"]["schema"], schema);
        assert_eq!(calls[i]["response_format"]["json_schema"]["strict"], true);
    }
    assert_eq!(calls[7]["output_config"]["format"]["schema"], schema);
    server.abort();
}

#[tokio::test]
async fn openai_dialect_with_retry_after() {
    let (base, attempts) = spawn_mock().await;
    let p = OpenAiProvider::new(Some(&base), test_cred());
    let out = p
        .complete(CompletionRequest {
            model: "gpt-5-mini".into(),
            system: None,
            prompt: "meaning of life?".into(),
            max_tokens: 8,
            temperature: Some(0.0),
            tools: None,
        })
        .await
        .expect("complete after retry");
    assert_eq!(out.text, "42");
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        2,
        "429 must be retried exactly once here"
    );
}

#[tokio::test]
async fn openai_embeddings_shape() {
    let (base, _) = spawn_mock().await;
    let p = OpenAiProvider::new(Some(&base), test_cred());
    let out = p
        .embed(EmbeddingRequest {
            model: "text-embedding-3-small".into(),
            inputs: vec!["alpha".into(), "beta".into()],
        })
        .await
        .expect("embed");
    assert_eq!(out.vectors.len(), 2);
    assert_eq!(out.dimensions, 3);
    assert_eq!(out.vectors[1][0], 1.0);
}

// ---------------------------------------------------------------------------
// Live smokes — the tests the header's policy line promises. Gated twice with
// the vacuous-skip pattern (return early): first on MUNARIUM_LIVE_PROVIDER_TESTS
// =1, then on the family's conventional key var (MUNARIUM_SECRET_<FAMILY>, the
// same names the deployed environments use). `cargo test` without keys stays
// green and free; CI never sets the gate, so these never run there by
// construction. Assertions cover transport shape only — non-empty text and
// usage accounting — never model output content (models change; the dialect
// contract does not). Each smoke is one minimal paid call.
// ---------------------------------------------------------------------------

fn live_cred(key_var: &str) -> Option<CredentialRef> {
    if std::env::var("MUNARIUM_LIVE_PROVIDER_TESTS").as_deref() != Ok("1") {
        return None;
    }
    std::env::var(key_var).ok()?;
    Some(CredentialRef::Env {
        env: key_var.into(),
    })
}

fn live_prompt() -> CompletionRequest {
    CompletionRequest {
        model: String::new(), // caller sets the family's fast tier model
        system: None,
        prompt: "Reply with the single word: ready".into(),
        max_tokens: 16,
        temperature: Some(0.0),
        tools: None,
    }
}

#[tokio::test]
async fn live_anthropic_smoke() {
    let Some(cred) = live_cred("MUNARIUM_SECRET_ANTHROPIC") else {
        return;
    };
    let p = AnthropicProvider::new(None, cred);
    let mut req = live_prompt();
    req.model =
        munarium_providers::builtin_tier_model("anthropic", munarium_providers::ModelTier::Fast)
            .unwrap()
            .into();
    let out = p.complete(req).await.expect("live anthropic completion");
    assert!(!out.text.trim().is_empty(), "empty completion text");
    assert!(out.input_tokens > 0, "usage.input_tokens not reported");
    assert!(out.output_tokens > 0, "usage.output_tokens not reported");
}

#[tokio::test]
async fn live_openai_smoke() {
    let Some(cred) = live_cred("MUNARIUM_SECRET_OPENAI") else {
        return;
    };
    let p = OpenAiProvider::new(None, cred.clone());
    let mut req = live_prompt();
    req.model =
        munarium_providers::builtin_tier_model("openai", munarium_providers::ModelTier::Fast)
            .unwrap()
            .into();
    let out = p.complete(req).await.expect("live openai completion");
    assert!(!out.text.trim().is_empty(), "empty completion text");
    assert!(out.input_tokens > 0, "usage.prompt_tokens not reported");

    // The one live embedding call: openai is the family the retrieval tier
    // would use for provider-backed embeddings.
    let emb = OpenAiProvider::new(None, cred)
        .embed(EmbeddingRequest {
            model: "text-embedding-3-small".into(),
            inputs: vec!["alpha".into()],
        })
        .await
        .expect("live openai embedding");
    assert_eq!(emb.vectors.len(), 1);
    assert!(emb.dimensions > 0, "embedding dimensions not reported");
}

#[tokio::test]
async fn live_openrouter_smoke() {
    let Some(cred) = live_cred("MUNARIUM_SECRET_OPENROUTER") else {
        return;
    };
    let p = OpenAiProvider::openrouter(None, cred);
    let mut req = live_prompt();
    req.model =
        munarium_providers::builtin_tier_model("openrouter", munarium_providers::ModelTier::Fast)
            .unwrap()
            .into();
    let out = p.complete(req).await.expect("live openrouter completion");
    assert!(!out.text.trim().is_empty(), "empty completion text");
}

#[tokio::test]
async fn missing_credential_fails_closed_before_any_network() {
    let p = AnthropicProvider::new(
        Some("http://127.0.0.1:1"), // nothing listens here — must not matter
        CredentialRef::Env {
            env: "MUNARIUM_NEVER_SET_KEY_VAR".into(),
        },
    );
    let err = p
        .complete(CompletionRequest {
            model: "m".into(),
            system: None,
            prompt: "p".into(),
            max_tokens: 1,
            temperature: None,
            tools: None,
        })
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("credential unavailable (environment)"),
        "must identify the failure category before trying HTTP: {msg}"
    );
    assert!(
        !msg.contains("MUNARIUM_NEVER_SET_KEY_VAR"),
        "credential references are operator-private"
    );
    assert!(!msg.contains("sk-"), "must never leak key material");
}
