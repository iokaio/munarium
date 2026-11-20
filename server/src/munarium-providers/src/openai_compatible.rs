// SPDX-License-Identifier: Apache-2.0
//! The `openai-compatible` provider kind: a self-hosted inference endpoint
//! for embeddings and reranking (ClearView IR-1).
//!
//! The wire protocol is the Munarium Lens endpoint profile, which any server
//! implementing the same two routes can also pass:
//!
//! | Operation | Request | Successful reply |
//! |---|---|---|
//! | `GET  {base}/v1/models` | none | `{"data": [{"id": "<served model>"}, ...]}` |
//! | `POST {base}/v1/embeddings` | `{"model", "input": [text, ...]}` | `{"data": [{"index", "embedding": [..]}], "model"?, "usage"?}` |
//! | `POST {base}/v1/rerank` | `{"model", "query", "documents": [text, ...]}` | `{"results": [{"index", "relevance_score"}], "model"?, "usage"?}` |
//!
//! Choices this module makes where the Lens profile is still open (recorded
//! in `docs/guides/inference-endpoints.md` for Lens to align with):
//!
//! - **Base URL.** The configured endpoint is resolved once. A path ending in
//!   `/v1` is used as is; otherwise `/v1` is appended. The prefix is never
//!   applied twice, and a private path prefix (`https://host/lens`) is kept.
//! - **Discovery.** `GET /v1/models`, OpenAI list shape; only `data[].id` is
//!   read.
//! - **Authentication.** Optional `Authorization: Bearer <credential>` from
//!   `credentialRef`. Redirects are never followed.
//! - **Deadline.** Each request carries `x-munarium-deadline-ms`, the
//!   milliseconds left in the caller's single deadline, and Server stops
//!   waiting at that deadline.
//! - **Instructions.** Server applies query and passage instructions as text
//!   prefixes before sending; the request carries only `model` and `input`.
//! - **Usage.** `usage.prompt_tokens`, else `usage.total_tokens`; absent usage
//!   stays unknown.
//! - **Model identity.** A reply's `model`, when present, must equal the
//!   requested model.
//! - **Errors.** Status plus an optional JSON `code` member (Lens sends
//!   `{"code": "...", "stage": "..."}`). Only a fixed set of codes is
//!   recognized; a body is never logged, relayed or stored.

use super::*;
use munarium_core::inference::{
    validate_embeddings, validate_rerank, EmbedCall, EmbedReply, FailureKind, InferenceEndpoint,
    InferenceFailure, RerankCall, RerankReply, Usage,
};
use serde_json::{json, Value};

/// The provider kind name.
pub const KIND: &str = "openai-compatible";

/// Defaults for the optional `spec.inference` block.
pub const DEFAULT_MAX_BATCH: usize = 32;
pub const DEFAULT_MAX_REQUEST_BYTES: usize = 64 * 1024;
pub const DEFAULT_MAX_ATTEMPTS: u32 = 2;
/// A reply larger than this is refused unread (an embedding batch of 32 at
/// 1,024 dimensions is well under 1 MiB of JSON).
pub const MAX_REPLY_BYTES: usize = 16 * 1024 * 1024;

/// `spec.inference`: the endpoint's declared bounds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InferenceProfile {
    /// The embedding dimension the served embed model returns. Required when
    /// `models.embed` is set; the probe verifies it.
    #[serde(
        rename = "embedDimensions",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub embed_dimensions: Option<usize>,
    /// Inputs per embedding request (default 32).
    #[serde(rename = "maxBatch", default, skip_serializing_if = "Option::is_none")]
    pub max_batch: Option<usize>,
    /// Serialized request bytes (default 65,536). A larger request is a
    /// refusal for that request, never split or truncated mid-input.
    #[serde(
        rename = "maxRequestBytes",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub max_request_bytes: Option<usize>,
    /// Physical attempts per call within one deadline (default 2, at most 3).
    #[serde(
        rename = "maxAttempts",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub max_attempts: Option<u32>,
    /// Accept models the endpoint reports as `"retrieval_model": false` (the
    /// Lens reference backend, test doubles). Off by default: a production
    /// probe rejects them.
    #[serde(
        rename = "allowTestBackend",
        default,
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub allow_test_backend: bool,
}

impl InferenceProfile {
    pub fn max_batch(&self) -> usize {
        self.max_batch.unwrap_or(DEFAULT_MAX_BATCH)
    }
    pub fn max_request_bytes(&self) -> usize {
        self.max_request_bytes.unwrap_or(DEFAULT_MAX_REQUEST_BYTES)
    }
    pub fn max_attempts(&self) -> u32 {
        self.max_attempts.unwrap_or(DEFAULT_MAX_ATTEMPTS)
    }
}

/// Validate an `openai-compatible` configuration. Called from
/// [`parse_provider_config`].
pub(crate) fn validate(spec: &ProviderSpec) -> std::result::Result<(), String> {
    validate_endpoint(spec.endpoint.as_deref())?;
    if !spec.models.complete.is_empty()
        || spec.models.fast.is_some()
        || spec.models.capable.is_some()
        || spec.models.frontier.is_some()
    {
        return Err(
            "openai-compatible serves embed and rerank models only; list completion models on an openai configuration"
                .into(),
        );
    }
    if spec.models.embed.is_empty() && spec.models.rerank.is_empty() {
        return Err("openai-compatible requires models.embed or models.rerank".into());
    }
    let valid_name =
        |m: &String| !m.is_empty() && m.len() <= 200 && !m.chars().any(char::is_whitespace);
    if !spec
        .models
        .embed
        .iter()
        .chain(&spec.models.rerank)
        .all(valid_name)
    {
        return Err("model names must be 1-200 characters without whitespace".into());
    }
    let profile = spec.inference.clone().unwrap_or_default();
    match profile.embed_dimensions {
        None if !spec.models.embed.is_empty() => {
            return Err("inference.embedDimensions is required with models.embed".into())
        }
        Some(0) => return Err("inference.embedDimensions must be positive".into()),
        Some(d) if d > 16_000 => return Err("inference.embedDimensions is too large".into()),
        _ => {}
    }
    if !(1..=256).contains(&profile.max_batch()) {
        return Err("inference.maxBatch must be 1-256".into());
    }
    if !(1024..=16 * 1024 * 1024).contains(&profile.max_request_bytes()) {
        return Err("inference.maxRequestBytes must be 1 KiB to 16 MiB".into());
    }
    if !(1..=3).contains(&profile.max_attempts()) {
        return Err("inference.maxAttempts must be 1-3".into());
    }
    Ok(())
}

fn validate_endpoint(endpoint: Option<&str>) -> std::result::Result<(), String> {
    let endpoint = endpoint.ok_or("openai-compatible requires an explicit endpoint")?;
    let url =
        reqwest::Url::parse(endpoint).map_err(|_| "invalid openai-compatible endpoint URL")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "openai-compatible endpoint must be an HTTP(S) base URL without credentials, query, or fragment"
                .into(),
        );
    }
    Ok(())
}

/// Resolve the configured endpoint to the `/v1` base, once.
///
/// `http://h:8787` and `http://h:8787/v1` both become `http://h:8787/v1`;
/// `https://h/lens` becomes `https://h/lens/v1`. Trailing slashes are
/// ignored. Never `/v1/v1`.
pub fn compose_base(endpoint: &str) -> std::result::Result<String, String> {
    validate_endpoint(Some(endpoint))?;
    let trimmed = endpoint.trim_end_matches('/');
    Ok(if trimmed.ends_with("/v1") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/v1")
    })
}

/// The endpoint fingerprint recorded with probes and semantic identities:
/// 16 hex digits of the resolved base. Never the address itself.
pub fn endpoint_fingerprint(base: &str) -> String {
    hex::encode(&sha2::Sha256::digest(base.as_bytes())[..8])
}

/// The HTTP client for one `openai-compatible` configuration.
#[derive(Clone)]
pub struct OpenAiCompatibleEndpoint {
    http: reqwest::Client,
    base: String,
    credential: Option<CredentialRef>,
    max_request_bytes: usize,
}

impl OpenAiCompatibleEndpoint {
    pub fn new(spec: &ProviderSpec) -> Result<Self> {
        if spec.provider != KIND {
            return Err(KernelError::InvalidInput(
                "OpenAiCompatibleEndpoint requires provider: openai-compatible".into(),
            ));
        }
        validate(spec).map_err(KernelError::InvalidInput)?;
        let base = compose_base(spec.endpoint.as_deref().unwrap_or_default())
            .map_err(KernelError::InvalidInput)?;
        let http = reqwest::Client::builder()
            .connect_timeout(PROVIDER_CONNECT_TIMEOUT)
            .timeout(PROVIDER_REQUEST_TIMEOUT)
            // A redirect could carry the bearer credential elsewhere.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| KernelError::Provider("inference HTTP client unavailable".into()))?;
        Ok(Self {
            http,
            base,
            credential: spec.credential_ref.clone(),
            max_request_bytes: spec
                .inference
                .clone()
                .unwrap_or_default()
                .max_request_bytes(),
        })
    }

    pub fn fingerprint(&self) -> String {
        endpoint_fingerprint(&self.base)
    }

    fn credential(&self) -> std::result::Result<Option<String>, InferenceFailure> {
        match &self.credential {
            None => Ok(None),
            Some(c) => resolve_credential(c).map(Some).map_err(|_| {
                InferenceFailure::new(FailureKind::Unconfigured, "credential_unavailable")
            }),
        }
    }

    fn remaining(deadline: Instant) -> std::result::Result<Duration, InferenceFailure> {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or(InferenceFailure::new(
                FailureKind::DeadlineExceeded,
                "deadline",
            ))
    }

    async fn send(
        &self,
        request: reqwest::RequestBuilder,
        deadline: Instant,
    ) -> std::result::Result<Value, InferenceFailure> {
        let remaining = Self::remaining(deadline)?;
        let mut request = request
            .timeout(remaining)
            .header("x-munarium-deadline-ms", remaining.as_millis().to_string());
        if let Some(key) = self.credential()? {
            request = request.bearer_auth(key);
        }
        let mut response = request.send().await.map_err(|e| {
            if e.is_timeout() {
                InferenceFailure::new(FailureKind::DeadlineExceeded, "timeout")
            } else {
                InferenceFailure::new(FailureKind::Unavailable, "transport")
            }
        })?;
        let status = response.status();
        let mut body = Vec::new();
        if response
            .content_length()
            .is_some_and(|n| n as usize > MAX_REPLY_BYTES)
        {
            return Err(InferenceFailure::invalid("reply_too_large"));
        }
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    if body.len() + chunk.len() > MAX_REPLY_BYTES {
                        return Err(InferenceFailure::invalid("reply_too_large"));
                    }
                    body.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(e) if e.is_timeout() => {
                    return Err(InferenceFailure::new(
                        FailureKind::DeadlineExceeded,
                        "timeout",
                    ))
                }
                Err(_) => return Err(InferenceFailure::new(FailureKind::Unavailable, "transport")),
            }
        }
        if !status.is_success() {
            let code = serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|v| v.get("code").and_then(Value::as_str).map(str::to_owned));
            return Err(classify_refusal(status.as_u16(), code.as_deref()));
        }
        serde_json::from_slice(&body).map_err(|_| InferenceFailure::invalid("malformed_json"))
    }

    fn url(&self, op: &str) -> String {
        format!("{}/{op}", self.base)
    }

    fn json_body(&self, body: &Value) -> std::result::Result<Vec<u8>, InferenceFailure> {
        let bytes = serde_json::to_vec(body)
            .map_err(|_| InferenceFailure::invalid("unencodable_request"))?;
        if bytes.len() > self.max_request_bytes {
            return Err(InferenceFailure::new(
                FailureKind::InputTooLarge,
                "request_bytes",
            ));
        }
        Ok(bytes)
    }

    fn post(&self, op: &str, bytes: Vec<u8>) -> reqwest::RequestBuilder {
        self.http
            .post(self.url(op))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(bytes)
    }

    /// The models the endpoint lists as ready (Lens lists only ready
    /// stages). Only `id` is required; the Lens members are read when present.
    pub async fn models(
        &self,
        deadline: Instant,
    ) -> std::result::Result<Vec<DiscoveredModel>, InferenceFailure> {
        let value = self
            .send(self.http.get(self.url("models")), deadline)
            .await?;
        let data = value
            .get("data")
            .and_then(Value::as_array)
            .ok_or(InferenceFailure::invalid("missing_data"))?;
        data.iter()
            .map(|m| {
                Ok(DiscoveredModel {
                    id: m
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .ok_or(InferenceFailure::invalid("missing_model_id"))?,
                    stage: m.get("stage").and_then(Value::as_str).map(str::to_owned),
                    retrieval_model: m.get("retrieval_model").and_then(Value::as_bool),
                    dimensions: m.get("dimensions").and_then(Value::as_u64),
                })
            })
            .collect()
    }

    /// The request size an embedding call would send, for admission
    /// estimates. Mirrors what [`InferenceEndpoint::embed`] serializes.
    pub fn embed_request_bytes(model: &str, inputs: &[String]) -> usize {
        serde_json::to_vec(&json!({"model": model, "input": inputs}))
            .map(|b| b.len())
            .unwrap_or(usize::MAX)
    }

    /// As [`Self::embed_request_bytes`], for a rerank call.
    pub fn rerank_request_bytes(model: &str, query: &str, documents: &[String]) -> usize {
        serde_json::to_vec(&json!({"model": model, "query": query, "documents": documents}))
            .map(|b| b.len())
            .unwrap_or(usize::MAX)
    }
}

/// One `GET /v1/models` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredModel {
    pub id: String,
    /// `embedding` | `reranking`, when the endpoint says (Lens does).
    pub stage: Option<String>,
    /// `false` marks a test backend that is not a retrieval model.
    pub retrieval_model: Option<bool>,
    /// The embedding dimension, when the endpoint says.
    pub dimensions: Option<u64>,
}

/// Check that a configured model is listed for a stage and fit to serve it.
/// Members the endpoint omits are not held against it; members it reports
/// must agree with the configuration.
pub fn check_discovery(
    models: &[DiscoveredModel],
    model: &str,
    stage: &str,
    dimensions: Option<usize>,
    allow_test_backend: bool,
) -> std::result::Result<(), &'static str> {
    let entry = models
        .iter()
        .find(|m| m.id == model)
        .ok_or("model_not_listed")?;
    if entry.stage.as_deref().is_some_and(|s| s != stage) {
        return Err("stage_mismatch");
    }
    if entry.retrieval_model == Some(false) && !allow_test_backend {
        return Err("not_a_retrieval_model");
    }
    if let (Some(listed), Some(declared)) = (entry.dimensions, dimensions) {
        if listed != declared as u64 {
            return Err("dimension_mismatch");
        }
    }
    Ok(())
}

fn check_model(value: &Value, requested: &str) -> std::result::Result<(), InferenceFailure> {
    match value.get("model") {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(m)) if m == requested => Ok(()),
        Some(_) => Err(InferenceFailure::invalid("model_mismatch")),
    }
}

fn usage(value: &Value) -> Usage {
    let usage = value.get("usage");
    Usage {
        input_tokens: usage
            .and_then(|u| u.get("prompt_tokens").or_else(|| u.get("total_tokens")))
            .and_then(Value::as_u64),
    }
}

/// Map a refused request to a typed, content-free failure. Only fixed codes
/// are recognized; anything else is classified by status alone.
pub fn classify_refusal(status: u16, code: Option<&str>) -> InferenceFailure {
    use FailureKind::*;
    let by_code = code.and_then(|c| {
        Some(match c {
            "inference_unavailable" => InferenceFailure::new(Unavailable, "status_503"),
            "backend_not_ready" => InferenceFailure::new(Unavailable, "not_ready"),
            "overloaded" => InferenceFailure::new(Unavailable, "overloaded"),
            "deadline_exceeded" => InferenceFailure::new(DeadlineExceeded, "endpoint_deadline"),
            "body_too_large" | "input_too_long" | "too_many_items" => {
                InferenceFailure::new(InputTooLarge, "endpoint_limit")
            }
            "model_not_found" | "unknown_model" | "unknown_profile" => {
                InferenceFailure::new(Unconfigured, "unknown_model")
            }
            "batch_too_large" => InferenceFailure::new(InputTooLarge, "endpoint_limit"),
            "inference_failed" => InferenceFailure::new(Unavailable, "inference_failed"),
            "unauthorized" | "forbidden" => InferenceFailure::new(Unavailable, "unauthorized"),
            "invalid_request" | "unsupported_media_type" => {
                InferenceFailure::new(Unavailable, "rejected_request")
            }
            _ => return None,
        })
    });
    by_code.unwrap_or(match status {
        401 | 403 => InferenceFailure::new(Unavailable, "unauthorized"),
        404 => InferenceFailure::new(Unconfigured, "route_not_found"),
        413 => InferenceFailure::new(InputTooLarge, "endpoint_limit"),
        429 => InferenceFailure::new(Unavailable, "status_429"),
        502 => InferenceFailure::new(Unavailable, "status_502"),
        503 => InferenceFailure::new(Unavailable, "status_503"),
        504 => InferenceFailure::new(DeadlineExceeded, "status_504"),
        400..=499 => InferenceFailure::new(Unavailable, "rejected_request"),
        _ => InferenceFailure::new(Unavailable, "status_5xx"),
    })
}

#[async_trait]
impl InferenceEndpoint for OpenAiCompatibleEndpoint {
    async fn embed(
        &self,
        call: EmbedCall<'_>,
    ) -> std::result::Result<EmbedReply, InferenceFailure> {
        let body = self.json_body(&json!({"model": call.model, "input": call.inputs}))?;
        let value = self
            .send(self.post("embeddings", body), call.deadline)
            .await?;
        check_model(&value, call.model)?;
        let data = value
            .get("data")
            .and_then(Value::as_array)
            .ok_or(InferenceFailure::invalid("missing_data"))?;
        let mut rows = Vec::with_capacity(data.len());
        for row in data {
            let index = row
                .get("index")
                .and_then(Value::as_i64)
                .ok_or(InferenceFailure::invalid("missing_index"))?;
            let values = row
                .get("embedding")
                .and_then(Value::as_array)
                .ok_or(InferenceFailure::invalid("missing_embedding"))?
                .iter()
                .map(|v| v.as_f64().ok_or(InferenceFailure::invalid("nonfinite")))
                .collect::<std::result::Result<Vec<f64>, _>>()?;
            rows.push((index, values));
        }
        Ok(EmbedReply {
            vectors: validate_embeddings(rows, call.inputs.len(), call.dimensions)?,
            usage: usage(&value),
        })
    }

    async fn rerank(
        &self,
        call: RerankCall<'_>,
    ) -> std::result::Result<RerankReply, InferenceFailure> {
        let body = self.json_body(
            &json!({"model": call.model, "query": call.query, "documents": call.documents}),
        )?;
        let value = self.send(self.post("rerank", body), call.deadline).await?;
        check_model(&value, call.model)?;
        let results = value
            .get("results")
            .and_then(Value::as_array)
            .ok_or(InferenceFailure::invalid("missing_results"))?;
        let mut scored = Vec::with_capacity(results.len());
        for r in results {
            let index = r
                .get("index")
                .and_then(Value::as_i64)
                .ok_or(InferenceFailure::invalid("missing_index"))?;
            let score = r
                .get("relevance_score")
                .and_then(Value::as_f64)
                .ok_or(InferenceFailure::invalid("nonfinite"))?;
            scored.push((index, score));
        }
        // Server reads the order only; returned passage text, if any, is
        // ignored and never used in place of the authorized originals.
        Ok(RerankReply {
            order: validate_rerank(scored, call.documents.len())?,
            usage: usage(&value),
        })
    }
}

/// The `ModelProvider` face of an `openai-compatible` configuration: the
/// providers API's embed and health operations. Completion is not served by
/// this kind.
pub struct OpenAiCompatibleProvider {
    endpoint: OpenAiCompatibleEndpoint,
    dimensions: usize,
}

impl OpenAiCompatibleProvider {
    pub fn new(spec: &ProviderSpec) -> Result<Self> {
        Ok(Self {
            endpoint: OpenAiCompatibleEndpoint::new(spec)?,
            dimensions: spec
                .inference
                .as_ref()
                .and_then(|p| p.embed_dimensions)
                .unwrap_or(0),
        })
    }
}

#[async_trait]
impl ModelProvider for OpenAiCompatibleProvider {
    fn id(&self) -> ProviderId {
        ProviderId::OpenaiCompatible
    }

    async fn complete(&self, _req: CompletionRequest) -> Result<CompletionResponse> {
        Err(KernelError::InvalidInput(
            "an openai-compatible configuration serves embed and rerank only".into(),
        ))
    }

    async fn embed(&self, req: EmbeddingRequest) -> Result<EmbeddingResponse> {
        let hash = request_hash(
            &json!({"embed": self.endpoint.fingerprint(), "model": req.model, "input": req.inputs}),
        );
        let reply = self
            .endpoint
            .embed(EmbedCall {
                model: &req.model,
                inputs: &req.inputs,
                dimensions: self.dimensions,
                deadline: Instant::now() + PROVIDER_REQUEST_TIMEOUT,
            })
            .await
            .map_err(|f| KernelError::Provider(format!("inference endpoint: {f}")))?;
        Ok(EmbeddingResponse {
            vectors: reply.vectors,
            dimensions: self.dimensions,
            request_hash: hash,
        })
    }

    async fn health(&self) -> Result<ProviderHealth> {
        let outcome = self
            .endpoint
            .models(Instant::now() + PROVIDER_CONNECT_TIMEOUT)
            .await;
        Ok(ProviderHealth {
            healthy: outcome.is_ok(),
            endpoint_fingerprint: self.endpoint.fingerprint(),
            detail: match outcome {
                Ok(models) => format!("GET /v1/models -> {} model(s)", models.len()),
                Err(f) => format!("GET /v1/models -> {f}"),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_v1_prefix_is_applied_exactly_once() {
        for (input, base) in [
            ("http://127.0.0.1:8787", "http://127.0.0.1:8787/v1"),
            ("http://127.0.0.1:8787/", "http://127.0.0.1:8787/v1"),
            ("http://127.0.0.1:8787/v1", "http://127.0.0.1:8787/v1"),
            ("http://127.0.0.1:8787/v1/", "http://127.0.0.1:8787/v1"),
            (
                "https://gateway.example/lens",
                "https://gateway.example/lens/v1",
            ),
            (
                "https://gateway.example/lens/v1",
                "https://gateway.example/lens/v1",
            ),
        ] {
            assert_eq!(compose_base(input).unwrap(), base, "{input}");
        }
        for bad in [
            "ftp://h",
            "http://user:pw@h",
            "http://h/?q=1",
            "http://h/#f",
            "not a url",
        ] {
            assert!(compose_base(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn refusals_are_classified_from_fixed_codes_and_status() {
        let c = |s, code| classify_refusal(s, code);
        assert_eq!(c(503, Some("inference_unavailable")).reason, "status_503");
        assert!(c(503, Some("inference_unavailable")).retryable());
        assert_eq!(
            c(413, Some("body_too_large")).kind,
            FailureKind::InputTooLarge
        );
        assert!(!c(413, None).retryable());
        assert_eq!(c(400, Some("invalid_request")).reason, "rejected_request");
        assert!(!c(400, Some("invalid_request")).retryable());
        // An unrecognized code is never echoed; the status decides.
        assert_eq!(c(503, Some("secret text")).reason, "status_503");
        assert_eq!(c(504, None).kind, FailureKind::DeadlineExceeded);
        assert_eq!(c(401, None).reason, "unauthorized");
        // The LENS-1 refusal codes.
        assert_eq!(
            c(404, Some("model_not_found")).kind,
            FailureKind::Unconfigured
        );
        assert_eq!(
            c(413, Some("batch_too_large")).kind,
            FailureKind::InputTooLarge
        );
        assert_eq!(
            c(413, Some("input_too_long")).kind,
            FailureKind::InputTooLarge
        );
        assert!(c(503, Some("overloaded")).retryable());
        assert!(!c(500, Some("inference_failed")).retryable());
    }

    #[test]
    fn discovery_rejects_test_backends_unless_allowed() {
        let models = vec![
            DiscoveredModel {
                id: "lens-reference/embedding-v1-1024d".into(),
                stage: Some("embedding".into()),
                retrieval_model: Some(false),
                dimensions: Some(1024),
            },
            DiscoveredModel {
                id: "plain".into(),
                stage: None,
                retrieval_model: None,
                dimensions: None,
            },
        ];
        let m = "lens-reference/embedding-v1-1024d";
        assert_eq!(
            check_discovery(&models, m, "embedding", Some(1024), false),
            Err("not_a_retrieval_model")
        );
        assert_eq!(
            check_discovery(&models, m, "embedding", Some(1024), true),
            Ok(())
        );
        assert_eq!(
            check_discovery(&models, m, "embedding", Some(768), true),
            Err("dimension_mismatch")
        );
        assert_eq!(
            check_discovery(&models, m, "reranking", None, true),
            Err("stage_mismatch")
        );
        assert_eq!(
            check_discovery(&models, "absent", "embedding", None, true),
            Err("model_not_listed")
        );
        assert_eq!(
            check_discovery(&models, "plain", "reranking", None, false),
            Ok(())
        );
    }
}
