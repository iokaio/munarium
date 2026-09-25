// SPDX-License-Identifier: Apache-2.0
//! Optional async observer at the actual HTTP retry boundary. Task-local scope
//! binds a tenant invocation without changing the legacy ModelProvider trait.
use munarium_core::money::MoneyUsage;
use munarium_core::{KernelError, Result};
use serde_json::Value;
use std::sync::Arc;

#[async_trait::async_trait]
pub trait AttemptObserver: Send + Sync {
    async fn begin(&self, route: &str) -> Result<String>;
    async fn finish(&self, attempt: &str, usage: MoneyUsage) -> Result<()>;
}

pub struct Context {
    pub observer: Arc<dyn AttemptObserver>,
    pub routing: Option<String>,
    current: tokio::sync::Mutex<Option<String>>,
}

impl Context {
    pub fn new(observer: Arc<dyn AttemptObserver>, routing: Option<String>) -> Self {
        Self {
            observer,
            routing,
            current: tokio::sync::Mutex::new(None),
        }
    }
}

tokio::task_local! { pub static CONTEXT: Context; }

pub(crate) async fn begin(url: &str) -> Result<()> {
    let Some((observer, routing)) = CONTEXT
        .try_with(|c| (c.observer.clone(), c.routing.clone()))
        .ok()
    else {
        return Ok(());
    };
    let route =
        route_identity(url, routing.as_deref()).unwrap_or_else(|| "unpriceable-route".into());
    let id = observer.begin(&route).await?;
    CONTEXT
        .with(|c| c.current.try_lock().map(|mut current| *current = Some(id)))
        .map_err(|_| KernelError::Provider("concurrent accounting attempt".into()))?;
    Ok(())
}

/// Hash an effective route only when its URL carries no userinfo or arbitrary
/// query values that could be credentials. Unknown routes cannot match a tariff.
pub fn route_identity(url: &str, routing: Option<&str>) -> Option<String> {
    let parsed = reqwest::Url::parse(url).ok()?;
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
        || parsed.query_pairs().any(|(key, _)| key != "api-version")
    {
        return None;
    }
    // Sorted insertion also survives serde_json/preserve_order unification.
    let parts = std::collections::BTreeMap::from([("routing", routing), ("url", Some(url))]);
    Some(super::request_hash(&serde_json::to_value(parts).ok()?))
}

pub(crate) async fn finish(provider: &str, value: &Value, embedding: bool) -> Result<()> {
    let Some((observer, id)) = CONTEXT
        .try_with(|c| {
            (
                c.observer.clone(),
                c.current.try_lock().ok().and_then(|mut c| c.take()),
            )
        })
        .ok()
    else {
        return Ok(());
    };
    if let Some(id) = id {
        observer
            .finish(&id, usage(provider, value, embedding))
            .await?;
    }
    Ok(())
}

/// Keep only numeric accounting evidence. Never persist response content,
/// provider error text, prompts, credential references or arbitrary JSON.
pub fn usage(provider: &str, value: &Value, embedding: bool) -> MoneyUsage {
    if provider == "ollama" {
        return MoneyUsage {
            input: value["prompt_eval_count"].as_u64(),
            output: if embedding {
                Some(0)
            } else {
                value["eval_count"].as_u64()
            },
            ..Default::default()
        };
    }
    let Some(u) = value["usage"].as_object() else {
        return MoneyUsage::default();
    };
    let anthropic = provider == "anthropic";
    let mut result = MoneyUsage {
        input: u
            .get(if anthropic {
                "input_tokens"
            } else {
                "prompt_tokens"
            })
            .and_then(Value::as_u64),
        output: if embedding {
            Some(0)
        } else {
            u.get(if anthropic {
                "output_tokens"
            } else {
                "completion_tokens"
            })
            .and_then(Value::as_u64)
        },
        ..Default::default()
    };
    if embedding && !anthropic {
        result.input = u.get("prompt_tokens").and_then(Value::as_u64);
    }
    if anthropic {
        // These optional counts are additional to Anthropic's input_tokens.
        let optional = |key: &str| match u.get(key) {
            None => Some(0),
            Some(v) => v.as_u64(),
        };
        result.cache_read = optional("cache_read_input_tokens");
        result.cache_write = optional("cache_creation_input_tokens");
        result.input = result
            .input
            .zip(result.cache_read)
            .zip(result.cache_write)
            .and_then(|((i, r), w)| i.checked_add(r)?.checked_add(w));
    } else {
        result.cache_read = value["usage"]["prompt_tokens_details"]["cached_tokens"].as_u64();
        result.reasoning = value["usage"]["completion_tokens_details"]["reasoning_tokens"].as_u64();
        result.cache_write = Some(0);
    }
    // Unknown categories never silently become free. Details with a nonzero
    // unrecognized category require explicit reconciliation and a suitable tariff.
    let known = if anthropic {
        &[
            "input_tokens",
            "output_tokens",
            "cache_read_input_tokens",
            "cache_creation_input_tokens",
        ][..]
    } else {
        &[
            "prompt_tokens",
            "completion_tokens",
            "total_tokens",
            "prompt_tokens_details",
            "completion_tokens_details",
        ][..]
    };
    result.unknown_categories = u.keys().any(|k| !known.contains(&k.as_str()));
    for (key, allowed) in [
        ("prompt_tokens_details", "cached_tokens"),
        ("completion_tokens_details", "reasoning_tokens"),
    ] {
        if let Some(details) = u.get(key) {
            match details.as_object() {
                Some(details) => {
                    result.unknown_categories |= details
                        .iter()
                        .any(|(k, v)| k != allowed && v.as_u64() != Some(0))
                }
                None => result.unknown_categories = true,
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn a_failed_durable_begin_prevents_submission() {
        struct Refuse;
        #[async_trait::async_trait]
        impl AttemptObserver for Refuse {
            async fn begin(&self, _: &str) -> Result<String> {
                Err(KernelError::Storage("fictional accounting failure".into()))
            }
            async fn finish(&self, _: &str, _: MoneyUsage) -> Result<()> {
                panic!("no attempt was sent")
            }
        }
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = calls.clone();
        let app = axum::Router::new().route(
            "/",
            axum::routing::post(move || {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { "unused" }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::new();
        let result = CONTEXT
            .scope(
                Context::new(Arc::new(Refuse), None),
                super::super::send_with_retry(|| client.post(&endpoint), 2),
            )
            .await;
        server.abort();
        assert!(matches!(result, Err(KernelError::Storage(_))));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn numeric_evidence_preserves_unknowns_and_inclusive_categories() {
        let v = json!({"usage":{"prompt_tokens":10,"completion_tokens":20,"total_tokens":30,
            "prompt_tokens_details":{"cached_tokens":4},"completion_tokens_details":{"reasoning_tokens":8}}});
        let u = usage("openai", &v, false);
        assert_eq!(
            (u.input, u.output, u.cache_read, u.reasoning),
            (Some(10), Some(20), Some(4), Some(8))
        );
        assert!(!u.unknown_categories);
        let u = usage(
            "anthropic",
            &json!({"usage":{"input_tokens":3,"output_tokens":4,"cache_read_input_tokens":5,"cache_creation_input_tokens":2}}),
            false,
        );
        assert_eq!(u.input, Some(10));
        assert!(
            usage(
                "openai",
                &json!({"usage":{"prompt_tokens":1,"completion_tokens":2,"context_tokens":3}}),
                false
            )
            .unknown_categories
        );
        assert!(usage("openai",&json!({"usage":{"prompt_tokens":1,"completion_tokens":2,"completion_tokens_details":{"audio_tokens":3}}}),false).unknown_categories);
        assert_eq!(
            usage("openai", &json!({"usage":{"prompt_tokens":-1}}), false).input,
            None
        );
        assert_eq!(usage("openai", &json!({}), false), MoneyUsage::default());
        assert_eq!(
            usage(
                "openai",
                &json!({"usage":{"prompt_tokens":7,"total_tokens":7}}),
                true
            )
            .output,
            Some(0)
        );
        assert_eq!(
            usage(
                "ollama",
                &json!({"prompt_eval_count":0,"eval_count":0}),
                false
            )
            .input,
            Some(0)
        );
        assert_eq!(
            usage(
                "anthropic",
                &json!({"usage":{"input_tokens":u64::MAX,"cache_read_input_tokens":1}}),
                false
            )
            .input,
            None
        );
    }

    #[test]
    fn route_identity_separates_routing_without_hashing_credentials() {
        assert_ne!(
            route_identity("http://localhost/api/chat", None),
            route_identity("http://localhost/api/embed", None)
        );
        assert_ne!(
            route_identity("https://example.invalid/chat", None),
            route_identity("https://example.invalid/chat", Some("fictional-downstream"))
        );
        for url in [
            "https://user:secret@example.invalid/chat",
            "https://example.invalid/chat?key=sentinel",
        ] {
            assert_eq!(route_identity(url, None), None);
        }
        assert!(
            route_identity("https://example.invalid/chat?api-version=2026-01-01", None).is_some()
        );
    }
}
