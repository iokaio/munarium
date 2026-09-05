// SPDX-License-Identifier: Apache-2.0
//! BYOK provider gateway, behind `munarium_core::provider::ModelProvider`.
//!
//! The gateway speaks to LLM endpoints with the TENANT's credentials at the
//! TENANT's endpoints. Keys resolve through the `SecretResolver` seam at call
//! time and are never stored, logged, or serialized. In the deployed demo,
//! Key Vault references land as env vars (ACA) / CSI-mounted files (AKS), so
//! the `env` and `file` resolvers ARE the vault path — rotation is a vault
//! operation invisible to munarium.
//!
//! Retry: 429/5xx retried (bounded) honoring `retry-after`. Budgets: per
//! config rpm/tpm token buckets, checked before every call. Invocation
//! provenance (request hash, provider, model, token counts, latency — never
//! the key, never bodies) is the server's job on top of these responses.

use async_trait::async_trait;
use munarium_core::provider::*;
use munarium_core::{KernelError, Result};
use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// declarative ProviderConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfigDoc {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub kind: String,
    pub metadata: ProviderMeta,
    pub spec: ProviderSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderMeta {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSpec {
    /// anthropic | openai | openrouter
    pub provider: String,
    /// Endpoint override; omit for the provider default. Covers Azure
    /// OpenAI-style and vLLM/enterprise-gateway deployments.
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub models: ProviderModels,
    #[serde(rename = "credentialRef")]
    pub credential_ref: CredentialRef,
    #[serde(default)]
    pub budgets: Budgets,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderModels {
    #[serde(default)]
    pub complete: Vec<String>,
    #[serde(default)]
    pub embed: Vec<String>,
    /// Optional per-config override of the built-in "fast" tier model.
    #[serde(default)]
    pub fast: Option<String>,
    /// Optional per-config override of the built-in "capable" tier model.
    #[serde(default)]
    pub capable: Option<String>,
    /// Optional per-config override of the built-in "frontier" tier model.
    #[serde(default)]
    pub frontier: Option<String>,
}

// ---------------------------------------------------------------------------
// model tiers + provider defaults
// ---------------------------------------------------------------------------

/// The three model tiers every provider family maps onto: `fast` (the lesser,
/// cheaper model), `capable` (the stronger model), and `frontier` (the
/// family's most capable — and most expensive — model).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    Fast,
    Capable,
    Frontier,
}

impl ModelTier {
    pub const ALL: [ModelTier; 3] = [Self::Fast, Self::Capable, Self::Frontier];

    pub fn parse(s: &str) -> std::result::Result<Self, String> {
        match s {
            "fast" => Ok(Self::Fast),
            "capable" => Ok(Self::Capable),
            "frontier" => Ok(Self::Frontier),
            other => Err(format!("unknown tier '{other}' (fast|capable|frontier)")),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Capable => "capable",
            Self::Frontier => "frontier",
        }
    }
}

/// Provider selection order for the default rule: first family with a usable
/// credential wins.
pub const DEFAULT_PROVIDER_PRIORITY: [&str; 3] = ["anthropic", "openai", "openrouter"];

/// Built-in tier models per provider family (overridable per config via
/// `models.fast` / `models.capable` / `models.frontier`).
pub fn builtin_tier_model(provider: &str, tier: ModelTier) -> Option<&'static str> {
    match (provider, tier) {
        ("anthropic", ModelTier::Fast) => Some("claude-haiku-4-5"),
        ("anthropic", ModelTier::Capable) => Some("claude-sonnet-5"),
        ("anthropic", ModelTier::Frontier) => Some("claude-fable-5-1"),
        ("openai", ModelTier::Fast) => Some("gpt-5.4-mini"),
        ("openai", ModelTier::Capable) => Some("gpt-5.4"),
        ("openai", ModelTier::Frontier) => Some("gpt-5.6-sol"),
        ("openrouter", ModelTier::Fast) => Some("deepseek/deepseek-v4-flash"),
        ("openrouter", ModelTier::Capable) => Some("z-ai/glm-5.2"),
        ("openrouter", ModelTier::Frontier) => Some("z-ai/glm-5.3"),
        _ => None,
    }
}

/// Conventional env var carrying each family's default credential (the Key
/// Vault secrets surface under these names in the deployed environments).
pub fn default_env_var(provider: &str) -> Option<&'static str> {
    match provider {
        "anthropic" => Some("MUNARIUM_SECRET_ANTHROPIC"),
        "openai" => Some("MUNARIUM_SECRET_OPENAI"),
        "openrouter" => Some("MUNARIUM_SECRET_OPENROUTER"),
        _ => None,
    }
}

/// Synthesized server-default config for a family, backed by the conventional
/// env var. Used by the default-provider rule and /healthai; never persisted.
pub fn default_config_doc(provider: &str) -> Option<ProviderConfigDoc> {
    let env = default_env_var(provider)?;
    Some(ProviderConfigDoc {
        api_version: "munarium.ioka.io/v1".into(),
        kind: "ProviderConfig".into(),
        metadata: ProviderMeta {
            name: format!("default-{provider}"),
        },
        spec: ProviderSpec {
            provider: provider.into(),
            endpoint: None,
            models: ProviderModels::default(),
            credential_ref: CredentialRef::Env { env: env.into() },
            budgets: Budgets::default(),
        },
    })
}

/// Resolve the completion model: explicit model > requested tier (config
/// override, then built-in) > first configured model > built-in capable.
pub fn resolve_complete_model(
    spec: &ProviderSpec,
    model: Option<String>,
    tier: Option<ModelTier>,
) -> Result<String> {
    if let Some(m) = model {
        return Ok(m);
    }
    if let Some(t) = tier {
        let override_model = match t {
            ModelTier::Fast => spec.models.fast.clone(),
            ModelTier::Capable => spec.models.capable.clone(),
            ModelTier::Frontier => spec.models.frontier.clone(),
        };
        return override_model
            .or_else(|| builtin_tier_model(&spec.provider, t).map(str::to_string))
            .ok_or_else(|| {
                KernelError::InvalidInput(format!(
                    "no {} tier model for provider '{}'",
                    t.as_str(),
                    spec.provider
                ))
            });
    }
    spec.models
        .complete
        .first()
        .cloned()
        .or_else(|| builtin_tier_model(&spec.provider, ModelTier::Capable).map(str::to_string))
        .ok_or(KernelError::InvalidInput(
            "no model given or configured".into(),
        ))
}

/// Where the key lives — never the key itself.
/// YAML: `credentialRef: { env: NAME }` or `credentialRef: { file: /path }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CredentialRef {
    /// Env var name (Key Vault refs surface as env in ACA).
    Env { env: String },
    /// File path (Secrets Store CSI mount on AKS).
    File { file: String },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Budgets {
    #[serde(default)]
    pub rpm: Option<u32>,
    #[serde(default)]
    pub tpm: Option<u32>,
    /// Daily token ceilings per tier (UTC day, input + output combined),
    /// enforced against the shared store so every replica sees one ledger.
    /// Absent = unlimited, matching the house rule that an undecided policy
    /// defaults to off.
    #[serde(default, rename = "dailyTokens")]
    pub daily_tokens: DailyTokenCaps,
}

/// Per-tier daily token ceilings. A tier without a value is unlimited.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DailyTokenCaps {
    #[serde(default)]
    pub fast: Option<u64>,
    #[serde(default)]
    pub capable: Option<u64>,
    #[serde(default)]
    pub frontier: Option<u64>,
}

impl DailyTokenCaps {
    pub fn for_tier(&self, tier: ModelTier) -> Option<u64> {
        match tier {
            ModelTier::Fast => self.fast,
            ModelTier::Capable => self.capable,
            ModelTier::Frontier => self.frontier,
        }
    }
}

pub fn parse_provider_config(yaml: &str) -> std::result::Result<ProviderConfigDoc, String> {
    let doc: ProviderConfigDoc =
        serde_yaml::from_str(yaml).map_err(|e| format!("provider config yaml: {e}"))?;
    if doc.kind != "ProviderConfig" {
        return Err(format!("kind must be ProviderConfig, got '{}'", doc.kind));
    }
    match doc.spec.provider.as_str() {
        "anthropic" | "openai" | "openrouter" => {}
        other => {
            return Err(format!(
                "unsupported provider '{other}' (anthropic|openai|openrouter)"
            ))
        }
    }
    Ok(doc)
}

/// Resolves the credential at call time. Failure names the ref, never leaks
/// any material.
pub fn resolve_credential(cred: &CredentialRef) -> Result<String> {
    match cred {
        CredentialRef::Env { env } => std::env::var(env)
            .map_err(|_| KernelError::Provider(format!("credential env var '{env}' is not set"))),
        CredentialRef::File { file } => std::fs::read_to_string(file)
            .map(|s| s.trim().to_string())
            .map_err(|e| KernelError::Provider(format!("credential file '{file}': {e}"))),
    }
    .and_then(|k| {
        if k.is_empty() {
            Err(KernelError::Provider("resolved credential is empty".into()))
        } else {
            Ok(k)
        }
    })
}

pub fn request_hash(parts: &serde_json::Value) -> String {
    hex::encode(sha2::Sha256::digest(parts.to_string().as_bytes()))
}

// ---------------------------------------------------------------------------
// rate budget (token bucket)
// ---------------------------------------------------------------------------

pub struct RateBudget {
    rpm: Option<u32>,
    tpm: Option<u32>,
    state: std::sync::Mutex<BudgetState>,
}

struct BudgetState {
    window_start: Instant,
    requests: u32,
    tokens: u64,
}

impl RateBudget {
    pub fn new(b: &Budgets) -> Self {
        Self::new_shared(b, 1)
    }

    /// Budget for one of `replicas` instances sharing a configured ceiling:
    /// each instance enforces ceil(limit / replicas), so the CLUSTER honors
    /// the configured rpm/tpm rather than multiplying it by the instance
    /// count. This is a per-window approximation — uneven load balancing
    /// under-uses the budget, and a restarted instance resets its window —
    /// documented in docs/ops/clustering.md.
    pub fn new_shared(b: &Budgets, replicas: u32) -> Self {
        let div = replicas.max(1);
        let share = |v: Option<u32>| v.map(|x| x.div_ceil(div).max(1));
        Self {
            rpm: share(b.rpm),
            tpm: share(b.tpm),
            state: std::sync::Mutex::new(BudgetState {
                window_start: Instant::now(),
                requests: 0,
                tokens: 0,
            }),
        }
    }

    /// Check-and-consume one request + an estimated token load.
    pub fn check(&self, estimated_tokens: u64) -> Result<()> {
        let mut s = self.state.lock().expect("budget lock");
        if s.window_start.elapsed() >= Duration::from_secs(60) {
            s.window_start = Instant::now();
            s.requests = 0;
            s.tokens = 0;
        }
        if let Some(rpm) = self.rpm {
            if s.requests + 1 > rpm {
                return Err(KernelError::RateLimited(format!(
                    "rpm budget {rpm} exhausted"
                )));
            }
        }
        if let Some(tpm) = self.tpm {
            if s.tokens + estimated_tokens > tpm as u64 {
                return Err(KernelError::RateLimited(format!(
                    "tpm budget {tpm} exhausted"
                )));
            }
        }
        s.requests += 1;
        s.tokens += estimated_tokens;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// HTTP core with bounded retry honoring retry-after
// ---------------------------------------------------------------------------

/// How long a single provider request may take, end to end. A long
/// completion is tens of seconds; five minutes is far past anything a turn
/// should wait, and the point is the bound itself: a provider that accepts
/// the connection and then never answers held the session turn, its database
/// connections and its SSE stream open forever, because `reqwest::Client::new()`
/// sets no timeout at all.
const PROVIDER_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
const PROVIDER_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(PROVIDER_CONNECT_TIMEOUT)
        .timeout(PROVIDER_REQUEST_TIMEOUT)
        .build()
        // The builder fails only on a TLS backend that cannot initialise,
        // which is a broken binary rather than a runtime condition; the
        // client is built once at provider construction.
        .expect("reqwest client with timeouts")
}

async fn send_with_retry(
    builder: impl Fn() -> reqwest::RequestBuilder,
    max_retries: u32,
) -> Result<reqwest::Response> {
    let mut attempt = 0;
    loop {
        let resp = builder()
            .send()
            .await
            .map_err(|e| KernelError::Provider(format!("request failed: {e}")))?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let retryable = status.as_u16() == 429 || status.is_server_error();
        if !retryable || attempt >= max_retries {
            let body = resp.text().await.unwrap_or_default();
            let detail = format!(
                "provider returned {status}: {}",
                body.chars().take(300).collect::<String>()
            );
            // An exhausted upstream rate limit surfaces as OUR 429, not a
            // 502: the caller's recovery is "slow down", and flattening it
            // into provider-error loses exactly that signal (spending-caps
            // batch, 2026-09-01).
            return Err(if status.as_u16() == 429 {
                KernelError::RateLimited(detail)
            } else {
                KernelError::Provider(detail)
            });
        }
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(1 + attempt as u64); // linear-ish backoff without a rand dep
        tokio::time::sleep(Duration::from_secs(retry_after.min(30))).await;
        attempt += 1;
    }
}

fn extract_str(v: &serde_json::Value, path: &[&str]) -> String {
    let mut cur = v;
    for p in path {
        cur = &cur[*p];
    }
    cur.as_str().unwrap_or_default().to_string()
}

/// Join every `text` block of an Anthropic `content` array.
///
/// Reading `content[0].text` is wrong: the Messages API returns a LIST of
/// blocks and a text block need not come first. When the model leads with a
/// non-text block — extended `thinking`, or `tool_use` — block 0 carries no
/// `text`, so first-block extraction silently yields "" while `usage` still
/// reports hundreds of output tokens. Measured against the deployed demo on
/// 2026-08-20: claude-sonnet-5 answered a support question with 544-632
/// output tokens and the caller received an empty answer beside its
/// citations. Concatenating the text blocks is also what makes a
/// thinking-then-answer response come through whole.
fn anthropic_text(v: &serde_json::Value) -> String {
    match v["content"].as_array() {
        Some(blocks) => blocks
            .iter()
            // A block is text when it says so, or (defensively, for older
            // shapes) when it carries `text` and names no other type.
            .filter(|b| b["type"] == "text" || (b["type"].is_null() && b["text"].is_string()))
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join(""),
        // Non-array content (or absent) — fall back to the old path so a
        // hand-rolled fixture or a future shape still yields something.
        None => v["content"]["text"]
            .as_str()
            .or_else(|| v["content"].as_str())
            .unwrap_or_default()
            .to_string(),
    }
}

// ---------------------------------------------------------------------------
// Anthropic (Messages API)
// ---------------------------------------------------------------------------

pub struct AnthropicProvider {
    pub endpoint: String,
    pub cred: CredentialRef,
    http: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(endpoint: Option<&str>, cred: CredentialRef) -> Self {
        Self {
            endpoint: endpoint
                .unwrap_or("https://api.anthropic.com")
                .trim_end_matches('/')
                .into(),
            cred,
            http: http_client(),
        }
    }
}

#[async_trait]
impl ModelProvider for AnthropicProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Anthropic
    }

    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse> {
        let key = resolve_credential(&self.cred)?;
        // Optional fields are OMITTED when absent — the Messages API rejects
        // explicit nulls (`system: Input should be a valid array`, found live
        // by /healthai 2026-08-09).
        let mut body = serde_json::json!({
            "model": req.model,
            "max_tokens": req.max_tokens.max(1),
            "messages": [{ "role": "user", "content": req.prompt }],
        });
        if let Some(system) = &req.system {
            body["system"] = serde_json::json!(system);
        }
        if let Some(t) = req.temperature {
            body["temperature"] = serde_json::json!(t);
        }
        let hash = request_hash(&serde_json::json!({"anthropic": &self.endpoint, "body": &body}));
        let url = format!("{}/v1/messages", self.endpoint);
        let resp = send_with_retry(
            || {
                self.http
                    .post(&url)
                    .header("x-api-key", &key)
                    .header("anthropic-version", "2023-06-01")
                    .json(&body)
            },
            2,
        )
        .await?;
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| KernelError::Provider(format!("bad response: {e}")))?;
        Ok(CompletionResponse {
            text: anthropic_text(&v),
            stop_reason: extract_str(&v, &["stop_reason"]),
            input_tokens: v["usage"]["input_tokens"].as_u64().unwrap_or(0),
            output_tokens: v["usage"]["output_tokens"].as_u64().unwrap_or(0),
            request_hash: hash,
        })
    }

    async fn embed(&self, _req: EmbeddingRequest) -> Result<EmbeddingResponse> {
        Err(KernelError::Provider(
            "anthropic does not expose an embeddings API".into(),
        ))
    }

    async fn health(&self) -> Result<ProviderHealth> {
        let key = resolve_credential(&self.cred)?;
        let url = format!("{}/v1/models", self.endpoint);
        let resp = self
            .http
            .get(&url)
            .header("x-api-key", &key)
            .header("anthropic-version", "2023-06-01")
            .send()
            .await
            .map_err(|e| KernelError::Provider(format!("unreachable: {e}")))?;
        Ok(ProviderHealth {
            healthy: resp.status().is_success(),
            endpoint_fingerprint: fingerprint(&self.endpoint),
            detail: format!("GET /v1/models -> {}", resp.status()),
        })
    }
}

// ---------------------------------------------------------------------------
// OpenAI (Chat Completions + Embeddings; base-URL override)
// ---------------------------------------------------------------------------

pub struct OpenAiProvider {
    pub endpoint: String,
    pub cred: CredentialRef,
    /// openrouter specialization: extra attribution headers.
    pub extra_headers: Vec<(String, String)>,
    pub provider_id: ProviderId,
    http: reqwest::Client,
}

impl OpenAiProvider {
    pub fn new(endpoint: Option<&str>, cred: CredentialRef) -> Self {
        Self {
            endpoint: endpoint
                .unwrap_or("https://api.openai.com/v1")
                .trim_end_matches('/')
                .into(),
            cred,
            extra_headers: Vec::new(),
            provider_id: ProviderId::Openai,
            http: http_client(),
        }
    }

    pub fn openrouter(endpoint: Option<&str>, cred: CredentialRef) -> Self {
        Self {
            endpoint: endpoint
                .unwrap_or("https://openrouter.ai/api/v1")
                .trim_end_matches('/')
                .into(),
            cred,
            extra_headers: vec![
                ("HTTP-Referer".into(), "https://munarium.ioka.io".into()),
                ("X-Title".into(), "munarium-server".into()),
            ],
            provider_id: ProviderId::Openrouter,
            http: http_client(),
        }
    }

    fn authed(&self, rb: reqwest::RequestBuilder, key: &str) -> reqwest::RequestBuilder {
        let mut rb = rb.bearer_auth(key);
        for (k, v) in &self.extra_headers {
            rb = rb.header(k, v);
        }
        rb
    }
}

/// Decode one Chat Completions response body. Separated from the HTTP path
/// so the shapes that matter are pinned without a server.
///
/// The same class of defect the Anthropic arm fixed: an in-band error object
/// (OpenRouter answers 200 with `{"error": ...}` for an upstream failure), a
/// refusal (`content: null` beside `refusal`), a tool-call-only message, or
/// an empty `choices` array all used to read as an empty string — tokens
/// billed, the turn proceeding on a blank answer. Each is a provider error
/// with its reason in the message.
///
/// The one `content: null` shape that is NOT an error: beside a truncation
/// `finish_reason` (`length` / `max_tokens`) with no refusal, which is what
/// a reasoning model returns when its hidden reasoning spent the whole
/// completion budget. That decodes as EMPTY TEXT with the truncation stop
/// reason, because the session turn's truncation-aware retry
/// (`completion.maxTokens`, 2026-09-01 — added when z-ai/glm-5.3 exhausted
/// the default this way) owns that case and re-asks at 4x. Failing it here
/// would turn every reasoning-exhausted frontier turn into a 502 with no
/// retry, which is the behaviour the 2026-09-02 review's stricter decoding
/// would otherwise have regressed at merge.
fn parse_openai_completion(v: &serde_json::Value, hash: String) -> Result<CompletionResponse> {
    if let Some(err) = v.get("error").filter(|e| e.is_object()) {
        let message = err["message"].as_str().unwrap_or("unspecified");
        return Err(KernelError::Provider(format!(
            "provider returned an error body: {message}"
        )));
    }
    let choice = &v["choices"][0];
    if choice.is_null() {
        return Err(KernelError::Provider(
            "provider response carries no choices".into(),
        ));
    }
    let finish_reason = choice["finish_reason"].as_str().unwrap_or_default();
    let truncated = matches!(finish_reason, "length" | "max_tokens");
    let refusal = choice["message"]["refusal"].as_str();
    let text = match choice["message"]["content"].as_str() {
        Some(t) => t.to_string(),
        None if truncated && refusal.is_none() => String::new(),
        None => {
            let why = refusal
                .map(|r| format!("refusal: {r}"))
                .unwrap_or_else(|| format!("no text content (finish_reason={finish_reason:?})"));
            return Err(KernelError::Provider(format!(
                "provider returned no completion text: {why}"
            )));
        }
    };
    Ok(CompletionResponse {
        text,
        stop_reason: finish_reason.to_string(),
        input_tokens: v["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
        output_tokens: v["usage"]["completion_tokens"].as_u64().unwrap_or(0),
        request_hash: hash,
    })
}

#[async_trait]
impl ModelProvider for OpenAiProvider {
    fn id(&self) -> ProviderId {
        self.provider_id
    }

    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse> {
        let key = resolve_credential(&self.cred)?;
        let mut messages = Vec::new();
        if let Some(system) = &req.system {
            messages.push(serde_json::json!({ "role": "system", "content": system }));
        }
        messages.push(serde_json::json!({ "role": "user", "content": req.prompt }));
        // OpenAI's current models reject `max_tokens` and require
        // `max_completion_tokens` (found live by /healthai 2026-08-09 on
        // gpt-5.4); OpenRouter keeps the OpenAI-compatible `max_tokens`.
        // Optional fields are omitted when absent, never sent as null.
        let max_tokens_field = match self.provider_id {
            ProviderId::Openai => "max_completion_tokens",
            _ => "max_tokens",
        };
        let mut body = serde_json::json!({
            "model": req.model,
            "messages": messages,
        });
        body[max_tokens_field] = serde_json::json!(req.max_tokens.max(1));
        if let Some(t) = req.temperature {
            body["temperature"] = serde_json::json!(t);
        }
        let hash = request_hash(&serde_json::json!({"openai": &self.endpoint, "body": &body}));
        let url = format!("{}/chat/completions", self.endpoint);
        let resp =
            send_with_retry(|| self.authed(self.http.post(&url), &key).json(&body), 2).await?;
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| KernelError::Provider(format!("bad response: {e}")))?;
        parse_openai_completion(&v, hash)
    }

    async fn embed(&self, req: EmbeddingRequest) -> Result<EmbeddingResponse> {
        let key = resolve_credential(&self.cred)?;
        let body = serde_json::json!({ "model": req.model, "input": req.inputs });
        let hash = request_hash(&serde_json::json!({"embed": &self.endpoint, "body": &body}));
        let url = format!("{}/embeddings", self.endpoint);
        let resp =
            send_with_retry(|| self.authed(self.http.post(&url), &key).json(&body), 2).await?;
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| KernelError::Provider(format!("bad response: {e}")))?;
        let vectors: Vec<Vec<f32>> = v["data"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .map(|d| {
                d["embedding"]
                    .as_array()
                    .unwrap_or(&Vec::new())
                    .iter()
                    .filter_map(|x| x.as_f64().map(|f| f as f32))
                    .collect()
            })
            .collect();
        let dims = vectors.first().map(|v| v.len()).unwrap_or(0);
        Ok(EmbeddingResponse {
            vectors,
            dimensions: dims,
            request_hash: hash,
        })
    }

    async fn health(&self) -> Result<ProviderHealth> {
        let key = resolve_credential(&self.cred)?;
        let url = format!("{}/models", self.endpoint);
        let resp = self
            .authed(self.http.get(&url), &key)
            .send()
            .await
            .map_err(|e| KernelError::Provider(format!("unreachable: {e}")))?;
        Ok(ProviderHealth {
            healthy: resp.status().is_success(),
            endpoint_fingerprint: fingerprint(&self.endpoint),
            detail: format!("GET /models -> {}", resp.status()),
        })
    }
}

fn fingerprint(endpoint: &str) -> String {
    hex::encode(&sha2::Sha256::digest(endpoint.as_bytes())[..8])
}

/// Factory from a validated config doc.
pub fn build_provider(doc: &ProviderConfigDoc) -> Box<dyn ModelProvider> {
    let endpoint = doc.spec.endpoint.as_deref();
    let cred = doc.spec.credential_ref.clone();
    match doc.spec.provider.as_str() {
        "anthropic" => Box::new(AnthropicProvider::new(endpoint, cred)),
        "openrouter" => Box::new(OpenAiProvider::openrouter(endpoint, cred)),
        _ => Box::new(OpenAiProvider::new(endpoint, cred)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_parses_and_rejects() {
        let yaml = r#"
apiVersion: munarium.ioka.io/v1
kind: ProviderConfig
metadata: { name: primary-anthropic }
spec:
  provider: anthropic
  models: { complete: [claude-sonnet-4-6] }
  credentialRef: { env: MUNARIUM_SECRET_ANTHROPIC }
  budgets: { rpm: 300, tpm: 200000 }
"#;
        let doc = parse_provider_config(yaml).expect("parses");
        assert_eq!(doc.metadata.name, "primary-anthropic");
        assert!(matches!(doc.spec.credential_ref, CredentialRef::Env { .. }));

        let bad = yaml.replace("anthropic", "watsonx");
        assert!(parse_provider_config(&bad).is_err());
    }

    #[test]
    fn tier_resolution_order() {
        let doc = default_config_doc("anthropic").expect("anthropic default");
        // explicit model wins
        assert_eq!(
            resolve_complete_model(&doc.spec, Some("my-model".into()), Some(ModelTier::Fast))
                .unwrap(),
            "my-model"
        );
        // tier falls to the built-in defaults
        assert_eq!(
            resolve_complete_model(&doc.spec, None, Some(ModelTier::Fast)).unwrap(),
            "claude-haiku-4-5"
        );
        assert_eq!(
            resolve_complete_model(&doc.spec, None, Some(ModelTier::Capable)).unwrap(),
            "claude-sonnet-5"
        );
        // no model, no tier: first configured, else built-in capable
        assert_eq!(
            resolve_complete_model(&doc.spec, None, None).unwrap(),
            "claude-sonnet-5"
        );
        // config override beats the built-in tier default
        let mut spec = doc.spec.clone();
        spec.models.fast = Some("claude-haiku-9".into());
        assert_eq!(
            resolve_complete_model(&spec, None, Some(ModelTier::Fast)).unwrap(),
            "claude-haiku-9"
        );
        // first-configured beats built-in capable when nothing is requested
        spec.models.complete = vec!["pinned-model".into()];
        assert_eq!(
            resolve_complete_model(&spec, None, None).unwrap(),
            "pinned-model"
        );
    }

    #[test]
    fn openai_decoder_pins_the_error_and_truncation_shapes() {
        let parse = |v: serde_json::Value| parse_openai_completion(&v, "h".into());
        let ok = parse(serde_json::json!({
            "choices": [{"message": {"content": "hi"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 3, "completion_tokens": 1}
        }))
        .expect("a normal completion decodes");
        assert_eq!(ok.text, "hi");
        assert_eq!(ok.stop_reason, "stop");
        assert_eq!((ok.input_tokens, ok.output_tokens), (3, 1));
        // In-band error body: OpenRouter answers 200 with `{"error": ...}`.
        let e = parse(serde_json::json!({"error": {"message": "upstream down"}}))
            .unwrap_err()
            .to_string();
        assert!(e.contains("upstream down"), "{e}");
        // No choices at all.
        assert!(parse(serde_json::json!({"choices": []})).is_err());
        // A refusal is a provider error carrying the reason.
        let e = parse(serde_json::json!({
            "choices": [{"message": {"content": null, "refusal": "no"}, "finish_reason": "stop"}]
        }))
        .unwrap_err()
        .to_string();
        assert!(e.contains("refusal: no"), "{e}");
        // Null content with a NORMAL stop is still an error: tokens billed,
        // no answer, and no retry would change it.
        assert!(parse(serde_json::json!({
            "choices": [{"message": {"content": null}, "finish_reason": "stop"}]
        }))
        .is_err());
        // Reasoning exhausted the budget: `content: null` beside
        // `finish_reason: length` is EMPTY TEXT with the truncation stop
        // reason, so the session turn's 4x retry fires instead of a 502.
        let exhausted = parse(serde_json::json!({
            "choices": [{"message": {"content": null}, "finish_reason": "length"}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 1024}
        }))
        .expect("a truncated reasoning-only completion is not a provider error");
        assert_eq!(exhausted.text, "");
        assert_eq!(exhausted.stop_reason, "length");
        assert_eq!(exhausted.output_tokens, 1024);
    }

    #[test]
    fn tier_and_default_tables_cover_all_families() {
        for family in DEFAULT_PROVIDER_PRIORITY {
            for tier in ModelTier::ALL {
                assert!(
                    builtin_tier_model(family, tier).is_some(),
                    "{family} {} missing from the builtin table",
                    tier.as_str()
                );
            }
            assert!(default_env_var(family).is_some());
            let doc = default_config_doc(family).expect("default doc");
            assert_eq!(doc.spec.provider, family);
            assert_eq!(doc.metadata.name, format!("default-{family}"));
        }
        assert!(ModelTier::parse("fast").is_ok());
        assert!(ModelTier::parse("capable").is_ok());
        assert!(ModelTier::parse("frontier").is_ok());
        assert!(ModelTier::parse("huge").is_err());
        for tier in ModelTier::ALL {
            assert_eq!(ModelTier::parse(tier.as_str()).unwrap(), tier);
        }
    }

    #[test]
    fn frontier_tier_resolves_the_top_models() {
        // The 2026-09-01 frontier tier: the models are load-bearing — the
        // demo's "Frontier" selector and the dev caps are sized against them.
        assert_eq!(
            builtin_tier_model("anthropic", ModelTier::Frontier),
            Some("claude-fable-5-1")
        );
        assert_eq!(
            builtin_tier_model("openai", ModelTier::Frontier),
            Some("gpt-5.6-sol")
        );
        assert_eq!(
            builtin_tier_model("openrouter", ModelTier::Frontier),
            Some("z-ai/glm-5.3")
        );
        // Per-config override beats the builtin, same as the other tiers.
        let mut spec = default_config_doc("anthropic").expect("default doc").spec;
        spec.models.frontier = Some("claude-fable-6".into());
        assert_eq!(
            resolve_complete_model(&spec, None, Some(ModelTier::Frontier)).unwrap(),
            "claude-fable-6"
        );
    }

    #[test]
    fn daily_token_caps_parse_and_select() {
        let yaml = r#"
apiVersion: munarium.ioka.io/v1
kind: ProviderConfig
metadata: { name: capped }
spec:
  provider: anthropic
  credentialRef: { env: MUNARIUM_SECRET_ANTHROPIC }
  budgets:
    rpm: 30
    dailyTokens: { fast: 5000000, capable: 3000000, frontier: 1000000 }
"#;
        let doc = parse_provider_config(yaml).expect("parses");
        let caps = &doc.spec.budgets.daily_tokens;
        assert_eq!(caps.for_tier(ModelTier::Fast), Some(5_000_000));
        assert_eq!(caps.for_tier(ModelTier::Capable), Some(3_000_000));
        assert_eq!(caps.for_tier(ModelTier::Frontier), Some(1_000_000));
        // Absent = unlimited, the honest default for an undecided policy.
        let uncapped = default_config_doc("anthropic").expect("default doc");
        for tier in ModelTier::ALL {
            assert_eq!(uncapped.spec.budgets.daily_tokens.for_tier(tier), None);
        }
    }

    #[test]
    fn credential_resolution_never_silently_passes() {
        let missing = CredentialRef::Env {
            env: "MUNARIUM_TEST_NOT_SET_EVER".into(),
        };
        assert!(resolve_credential(&missing).is_err());
    }

    #[test]
    fn budget_window_enforces() {
        let b = RateBudget::new(&Budgets {
            rpm: Some(2),
            tpm: Some(100),
            ..Default::default()
        });
        assert!(b.check(10).is_ok());
        assert!(b.check(10).is_ok());
        assert!(
            matches!(b.check(10), Err(KernelError::RateLimited(_))),
            "3rd request over rpm"
        );
        let b = RateBudget::new(&Budgets {
            rpm: None,
            tpm: Some(50),
            ..Default::default()
        });
        assert!(b.check(40).is_ok());
        assert!(
            matches!(b.check(20), Err(KernelError::RateLimited(_))),
            "tpm exceeded"
        );
    }
}

#[cfg(test)]
mod anthropic_content_tests {
    use super::anthropic_text;
    use serde_json::json;

    /// The Messages API returns a LIST of content blocks. Reading only
    /// `content[0].text` loses the answer whenever the model leads with a
    /// non-text block, which is how a 600-output-token completion reached a
    /// caller as an empty string (deployed demo, 2026-08-20).
    #[test]
    fn text_survives_a_leading_non_text_block() {
        let thinking_first = json!({"content": [
            {"type": "thinking", "thinking": "weighing the KB against the release note"},
            {"type": "text", "text": "The KB article is superseded."}
        ]});
        assert_eq!(
            anthropic_text(&thinking_first),
            "The KB article is superseded."
        );

        let tool_first = json!({"content": [
            {"type": "tool_use", "id": "tu_1", "name": "search", "input": {}},
            {"type": "text", "text": "Answer."}
        ]});
        assert_eq!(anthropic_text(&tool_first), "Answer.");
    }

    #[test]
    fn multiple_text_blocks_join_in_order() {
        let split = json!({"content": [
            {"type": "text", "text": "first. "},
            {"type": "thinking", "thinking": "..."},
            {"type": "text", "text": "second."}
        ]});
        assert_eq!(anthropic_text(&split), "first. second.");
    }

    #[test]
    fn ordinary_and_degenerate_shapes() {
        // the common case
        assert_eq!(
            anthropic_text(&json!({"content": [{"type": "text", "text": "hello"}]})),
            "hello"
        );
        // nothing but non-text blocks -> empty, but honestly empty
        assert_eq!(
            anthropic_text(&json!({"content": [{"type": "thinking", "thinking": "x"}]})),
            ""
        );
        // absent / unexpected content must not panic
        assert_eq!(anthropic_text(&json!({})), "");
        assert_eq!(anthropic_text(&json!({"content": "plain"})), "plain");
    }
}
