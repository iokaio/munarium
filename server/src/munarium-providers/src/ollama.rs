// SPDX-License-Identifier: Apache-2.0
//! Native Ollama dialect. Local endpoints are explicit and credentials optional.
use super::*;
use serde_json::{json, Value};

pub(crate) fn validate_endpoint(endpoint: Option<&str>) -> std::result::Result<(), String> {
    let endpoint = endpoint.ok_or("ollama requires an explicit endpoint")?;
    let url = reqwest::Url::parse(endpoint).map_err(|_| "invalid ollama endpoint URL")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "ollama endpoint must be an HTTP(S) base URL without credentials, query, or fragment"
                .into(),
        );
    }
    Ok(())
}

pub struct OllamaProvider {
    endpoint: String,
    spec: ProviderSpec,
    http: reqwest::Client,
}

impl OllamaProvider {
    pub fn new(spec: &ProviderSpec) -> Result<Self> {
        validate_endpoint(spec.endpoint.as_deref()).map_err(KernelError::InvalidInput)?;
        if spec.provider != "ollama" {
            return Err(KernelError::InvalidInput(
                "OllamaProvider requires provider: ollama".into(),
            ));
        }
        Ok(Self {
            endpoint: spec
                .endpoint
                .as_deref()
                .unwrap_or_default()
                .trim_end_matches('/')
                .into(),
            spec: spec.clone(),
            http: http_client(),
        })
    }

    async fn request(&self, path: &str, body: Option<&Value>) -> Result<Value> {
        let key = resolve_config_credential(&self.spec)?;
        let url = format!("{}{path}", self.endpoint);
        let response = send_with_retry_impl(
            || {
                let mut request = if let Some(body) = body {
                    self.http.post(&url).json(body)
                } else {
                    self.http.get(&url)
                };
                if let Some(key) = &key {
                    request = request.bearer_auth(key);
                }
                request
            },
            2,
            true,
        )
        .await?;
        let value: Value = response
            .json()
            .await
            .map_err(|_| KernelError::Provider("invalid Ollama JSON response".into()))?;
        if value.get("error").is_some() {
            // Remote errors can echo prompts or proxy credentials; never relay bodies.
            return Err(KernelError::Provider(
                "Ollama returned an error body".into(),
            ));
        }
        Ok(value)
    }
}

fn bad_response() -> KernelError {
    KernelError::Provider("invalid Ollama response shape".into())
}

fn parse_completion(value: &Value, request_hash: String) -> Result<CompletionResponse> {
    if value["done"] != true {
        return Err(bad_response());
    }
    if value["message"]
        .get("tool_calls")
        .is_some_and(|calls| !calls.is_null() && calls != &json!([]))
    {
        return Err(KernelError::Provider(
            "Ollama tool-call responses are not supported".into(),
        ));
    }
    let reason = value["done_reason"].as_str().ok_or_else(bad_response)?;
    let stop_reason = match reason {
        "stop" => "stop",
        "length" | "max_tokens" => "length",
        _ => return Err(bad_response()),
    };
    let text = value["message"]["content"]
        .as_str()
        .ok_or_else(bad_response)?;
    if text.trim().is_empty() && stop_reason != "length" {
        return Err(bad_response());
    }
    Ok(CompletionResponse {
        text: text.into(),
        stop_reason: stop_reason.into(),
        input_tokens: value["prompt_eval_count"]
            .as_u64()
            .ok_or_else(bad_response)?,
        output_tokens: value["eval_count"].as_u64().ok_or_else(bad_response)?,
        request_hash,
    })
}

fn parse_embeddings(
    value: &Value,
    count: usize,
    request_hash: String,
) -> Result<EmbeddingResponse> {
    let vectors: Vec<Vec<f32>> =
        serde_json::from_value(value["embeddings"].clone()).map_err(|_| bad_response())?;
    let dimensions = vectors.first().map_or(0, Vec::len);
    if vectors.len() != count
        || dimensions == 0
        || vectors
            .iter()
            .any(|v| v.len() != dimensions || v.iter().any(|x| !x.is_finite()))
    {
        return Err(bad_response());
    }
    Ok(EmbeddingResponse {
        vectors,
        dimensions,
        request_hash,
    })
}

#[async_trait]
impl ModelProvider for OllamaProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Ollama
    }

    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse> {
        if req.tools.as_ref().is_some_and(|tools| tools != &json!([])) {
            return Err(KernelError::InvalidInput(
                "Ollama tool requests are not supported by the completion contract".into(),
            ));
        }
        let mut messages = Vec::new();
        if let Some(system) = req.system {
            messages.push(json!({"role": "system", "content": system}));
        }
        messages.push(json!({"role": "user", "content": req.prompt}));
        let mut options = json!({"num_predict": req.max_tokens.max(1)});
        if let Some(temperature) = req.temperature {
            options["temperature"] = json!(temperature);
        }
        let body = json!({"model": req.model, "messages": messages, "stream": false, "think": false, "options": options});
        let hash =
            request_hash(&json!({"ollama": self.endpoint, "operation": "chat", "body": body}));
        let value = self.request("/api/chat", Some(&body)).await?;
        parse_completion(&value, hash)
    }

    async fn embed(&self, req: EmbeddingRequest) -> Result<EmbeddingResponse> {
        if req.inputs.is_empty() {
            return Err(KernelError::InvalidInput("inputs is required".into()));
        }
        let count = req.inputs.len();
        let body = json!({"model": req.model, "input": req.inputs, "truncate": false});
        let hash =
            request_hash(&json!({"ollama": self.endpoint, "operation": "embed", "body": body}));
        let value = self.request("/api/embed", Some(&body)).await?;
        parse_embeddings(&value, count, hash)
    }

    async fn health(&self) -> Result<ProviderHealth> {
        let tags = self.request("/api/tags", None).await?;
        let installed = tags["models"].as_array().ok_or_else(bad_response)?;
        let names: Vec<&str> = installed
            .iter()
            .map(|m| m["name"].as_str().ok_or_else(bad_response))
            .collect::<Result<_>>()?;
        let models = &self.spec.models;
        let configured = models
            .complete
            .iter()
            .chain(&models.embed)
            .chain(models.fast.iter())
            .chain(models.capable.iter())
            .chain(models.frontier.iter());
        let missing = configured
            .filter(|model| {
                !names
                    .iter()
                    .any(|name| *name == model.as_str() || *name == format!("{model}:latest"))
            })
            .count();
        Ok(ProviderHealth {
            healthy: missing == 0,
            endpoint_fingerprint: fingerprint(&self.endpoint),
            detail: if missing == 0 {
                "Ollama reachable; configured models installed (inference not probed)".into()
            } else {
                format!("Ollama reachable; {missing} configured model entries missing; pull them explicitly")
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stalled_endpoint_has_a_request_deadline() {
        let app = axum::Router::new().route(
            "/api/chat",
            axum::routing::post(|| async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                axum::Json(json!({}))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let doc = parse_provider_config(&format!("apiVersion: munarium.ioka.io/v1\nkind: ProviderConfig\nmetadata: {{name: timeout}}\nspec:\n  provider: ollama\n  endpoint: {endpoint}\n")).unwrap();
        let mut provider = OllamaProvider::new(&doc.spec).unwrap();
        provider.http = reqwest::Client::builder()
            .timeout(Duration::from_millis(30))
            .build()
            .unwrap();
        let result = tokio::time::timeout(
            Duration::from_millis(500),
            provider.complete(CompletionRequest {
                model: "test".into(),
                system: None,
                prompt: "test".into(),
                max_tokens: 1,
                temperature: None,
                tools: None,
            }),
        )
        .await
        .expect("request timeout must expire before the outer deadline");
        assert!(matches!(result, Err(KernelError::Provider(_))));
    }

    #[test]
    fn rejects_malformed_completion_and_preserves_truncation() {
        let base = json!({"done": true, "done_reason": "stop", "message": {"content": "ok", "thinking": "hidden"}, "prompt_eval_count": 7, "eval_count": 2});
        let answer = parse_completion(&base, "hash".into()).unwrap();
        assert_eq!(answer.text, "ok");
        assert_eq!((answer.input_tokens, answer.output_tokens), (7, 2));
        for patch in [
            json!({"done": false}),
            json!({"message": {"content": ""}}),
            json!({"eval_count": null}),
            json!({"done_reason": "unknown"}),
            json!({"message": {"content": "ok", "tool_calls": [{}]}}),
        ] {
            let mut bad = base.clone();
            for (key, value) in patch.as_object().unwrap() {
                bad[key] = value.clone();
            }
            assert!(parse_completion(&bad, "h".into()).is_err());
        }
        let mut truncated = base;
        truncated["done_reason"] = json!("length");
        truncated["message"]["content"] = json!("");
        assert_eq!(
            parse_completion(&truncated, "h".into())
                .unwrap()
                .stop_reason,
            "length"
        );
    }

    #[test]
    fn embeddings_reject_wrong_counts_ragged_non_numeric_and_overflow() {
        for vectors in [
            json!([]),
            json!([[1.0]]),
            json!([[], []]),
            json!([[1.0], [1.0, 2.0]]),
            json!([["1"], [2]]),
            json!([[1e100], [1]]),
        ] {
            assert!(parse_embeddings(&json!({"embeddings": vectors}), 2, "h".into()).is_err());
        }
        assert_eq!(
            parse_embeddings(
                &json!({"embeddings": [[1.0, 0.0], [0.0, 1.0]]}),
                2,
                "h".into()
            )
            .unwrap()
            .dimensions,
            2
        );
    }
}
