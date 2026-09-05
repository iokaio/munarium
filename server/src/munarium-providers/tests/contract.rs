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
        msg.contains("MUNARIUM_NEVER_SET_KEY_VAR"),
        "must name the ref: {msg}"
    );
    assert!(!msg.contains("sk-"), "must never leak key material");
}
