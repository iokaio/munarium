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

// Production code returns typed errors instead of panicking; tests are exempt.
// The policy, its two exemptions and the per-site record are in
// server/docs/panic-boundaries.md (P15/R32).
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented
    )
)]

use async_trait::async_trait;
use munarium_core::provider::*;
use munarium_core::{KernelError, Result};
use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use std::time::{Duration, Instant};

pub mod accounting;
mod ollama;
pub use ollama::OllamaProvider;

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
    /// anthropic | openai | openrouter | ollama
    pub provider: String,
    /// Endpoint override; omit for the provider default. Covers Azure
    /// OpenAI-style and vLLM/enterprise-gateway deployments.
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub models: ProviderModels,
    /// Optional only for Ollama. Existing providers require a secret reference.
    #[serde(
        rename = "credentialRef",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub credential_ref: Option<CredentialRef>,
    /// An operator-chosen public label, never derived from secret material or
    /// its location. Disclosed only by the management diagnostics surface.
    #[serde(default, rename = "credentialAlias")]
    pub credential_alias: Option<String>,
    /// Optional explicit OpenRouter downstream. When set, fallback is disabled.
    #[serde(
        default,
        rename = "openrouterProvider",
        skip_serializing_if = "Option::is_none"
    )]
    pub openrouter_provider: Option<String>,
    /// Native structured-output policy. Omitted preserves existing requests.
    #[serde(default, rename = "structuredOutput")]
    pub structured_output: StructuredOutputPolicy,
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

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuredOutputMode {
    #[default]
    Compatibility,
    Native,
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructuredOutputPolicy {
    #[serde(default)]
    pub default: StructuredOutputMode,
    #[serde(default)]
    pub models: std::collections::BTreeMap<String, StructuredOutputMode>,
}

impl StructuredOutputPolicy {
    /// Refuse before admission or network submission. No cost-incurring retry
    /// or weaker schema mode is implied by an unsupported capability.
    pub fn require_native(&self, model: &str) -> Result<()> {
        match self.models.get(model).unwrap_or(&self.default) {
            StructuredOutputMode::Compatibility | StructuredOutputMode::Native => Ok(()),
            StructuredOutputMode::Unsupported | StructuredOutputMode::Unknown => {
                Err(KernelError::InvalidInput(
                    "native structured output is not enabled for the resolved model".into(),
                ))
            }
        }
    }
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
            credential_alias: None,
            provider: provider.into(),
            endpoint: None,
            models: ProviderModels::default(),
            credential_ref: Some(CredentialRef::Env { env: env.into() }),
            openrouter_provider: None,
            structured_output: StructuredOutputPolicy::default(),
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
    /// Opt-in shared UTC-day ceiling across completion and embedding HTTP
    /// attempts, including retries and requests without a tier. Cache hits
    /// create no attempt. Independent of the legacy per-tier logical cap.
    #[serde(default, rename = "dailyTotalTokens")]
    pub daily_total_tokens: Option<u64>,
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
    if doc
        .spec
        .budgets
        .daily_total_tokens
        .is_some_and(|n| n > i64::MAX as u64)
    {
        return Err("dailyTotalTokens must fit a nonnegative signed 64-bit integer".into());
    }
    if doc.spec.credential_alias.as_ref().is_some_and(|alias| {
        alias.is_empty()
            || alias.len() > 64
            || !alias
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    }) {
        return Err(
            "credentialAlias must be 1-64 letters, digits, dots, underscores or dashes".into(),
        );
    }
    if let Some(slug) = &doc.spec.openrouter_provider {
        if doc.spec.provider != "openrouter"
            || slug.is_empty()
            || slug.len() > 100
            || !slug
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/'))
        {
            return Err("openrouterProvider requires one valid downstream slug on an OpenRouter configuration".into());
        }
    }
    match doc.spec.provider.as_str() {
        "anthropic" | "openai" | "openrouter" => {
            if doc.spec.credential_ref.is_none() {
                return Err("credentialRef is required for this provider".into());
            }
        }
        "ollama" => ollama::validate_endpoint(doc.spec.endpoint.as_deref())?,
        other => {
            return Err(format!(
                "unsupported provider '{other}' (anthropic|openai|openrouter|ollama)"
            ))
        }
    }
    Ok(doc)
}

/// A local Ollama endpoint needs no credential. A configured proxy credential
/// still must resolve, and cloud providers still require one.
pub fn resolve_config_credential(spec: &ProviderSpec) -> Result<Option<String>> {
    match &spec.credential_ref {
        Some(reference) => resolve_credential(reference).map(Some),
        None if spec.provider == "ollama" => Ok(None),
        None => Err(KernelError::InvalidInput(
            "credentialRef is required for this provider".into(),
        )),
    }
}

/// Resolves the credential at call time. Errors disclose neither its reference
/// nor its material; operator diagnostics use a separately supplied alias.
pub fn resolve_credential(cred: &CredentialRef) -> Result<String> {
    match cred {
        CredentialRef::Env { env } => std::env::var(env).map_err(|_| {
            KernelError::Provider("provider credential unavailable (environment)".into())
        }),
        CredentialRef::File { file } => std::fs::read_to_string(file)
            .map(|s| s.trim().to_string())
            .map_err(|_| KernelError::Provider("provider credential unavailable (file)".into())),
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
    ///
    /// The arithmetic is checked: an estimate that does not fit alongside the
    /// window's total exceeds any tpm limit, and without a limit the totals
    /// saturate. A poisoned lock refuses the request (a storage-class error,
    /// not a 429) rather than panicking or admitting it unmetered (P15/R32).
    pub fn check(&self, estimated_tokens: u64) -> Result<()> {
        let mut s = self
            .state
            .lock()
            .map_err(|_| KernelError::Storage("provider rate budget lock is poisoned".into()))?;
        if s.window_start.elapsed() >= Duration::from_secs(60) {
            s.window_start = Instant::now();
            s.requests = 0;
            s.tokens = 0;
        }
        if let Some(rpm) = self.rpm {
            if s.requests >= rpm {
                return Err(KernelError::RateLimited(format!(
                    "rpm budget {rpm} exhausted"
                )));
            }
        }
        if let Some(tpm) = self.tpm {
            if s.tokens
                .checked_add(estimated_tokens)
                .is_none_or(|total| total > u64::from(tpm))
            {
                return Err(KernelError::RateLimited(format!(
                    "tpm budget {tpm} exhausted"
                )));
            }
        }
        s.requests = s.requests.saturating_add(1);
        s.tokens = s.tokens.saturating_add(estimated_tokens);
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

// The builder fails only on a TLS backend that cannot initialise, which is a
// broken binary rather than a runtime condition; the client is built once at
// provider construction, and the public constructors that call this are
// infallible. Falling back to a default client would silently drop the
// timeouts above, so this is one of the two registered panic exemptions
// (server/docs/panic-boundaries.md).
#[expect(
    clippy::expect_used,
    reason = "TLS backend initialisation failure is a broken binary; a default client would drop the timeouts"
)]
fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(PROVIDER_CONNECT_TIMEOUT)
        .timeout(PROVIDER_REQUEST_TIMEOUT)
        .build()
        .expect("reqwest client with timeouts")
}

async fn send_with_retry(
    builder: impl Fn() -> reqwest::RequestBuilder,
    max_retries: u32,
) -> Result<reqwest::Response> {
    send_with_retry_impl(builder, max_retries).await
}

async fn send_with_retry_impl(
    builder: impl Fn() -> reqwest::RequestBuilder,
    max_retries: u32,
) -> Result<reqwest::Response> {
    let mut attempt = 0;
    loop {
        let request = builder();
        let url = request
            .try_clone()
            .and_then(|r| r.build().ok())
            .map(|r| r.url().to_string())
            .ok_or_else(|| KernelError::Provider("cannot prepare provider request".into()))?;
        accounting::begin(&url).await?;
        let resp = request
            .send()
            .await
            .map_err(|e| KernelError::Provider(format!("request failed: {}", e.without_url())))?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let retryable = status.as_u16() == 429 || status.is_server_error();
        if !retryable || attempt >= max_retries {
            // Upstream error bodies can echo authorization headers, prompts or
            // proxy configuration. Retain the status, never the raw body.
            let detail = format!("provider returned {status}");
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

/// Keep individually valid counts even when another field is malformed.
/// Missing fields and malformed values must never become observed zeros.
fn usage_evidence(value: &serde_json::Value, input: &str, output: &str) -> UsageEvidence {
    let usage = value.get("usage");
    let input_value = usage.and_then(|v| v.get(input));
    let output_value = usage.and_then(|v| v.get(output));
    let input_tokens = input_value.and_then(serde_json::Value::as_u64);
    let output_tokens = output_value.and_then(serde_json::Value::as_u64);
    let malformed = usage.is_some_and(|v| !v.is_object())
        || input_value.is_some_and(|v| v.as_u64().is_none())
        || output_value.is_some_and(|v| v.as_u64().is_none());
    UsageEvidence {
        input_tokens,
        output_tokens,
        source: if malformed {
            UsageSource::Malformed
        } else if input_tokens.is_some() || output_tokens.is_some() {
            UsageSource::ProviderReported
        } else {
            UsageSource::Missing
        },
    }
}

#[derive(Clone)]
pub struct AnthropicProvider {
    pub endpoint: String,
    pub cred: CredentialRef,
    output_schema: Option<serde_json::Value>,
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
            output_schema: None,
            http: http_client(),
        }
    }
}

#[async_trait]
impl ModelProvider for AnthropicProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Anthropic
    }

    async fn complete_structured(
        &self,
        req: CompletionRequest,
        schema: serde_json::Value,
    ) -> Result<CompletionResponse> {
        Ok(self
            .complete_structured_detailed(req, schema)
            .await?
            .response)
    }

    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse> {
        Ok(self.complete_detailed(req).await?.response)
    }

    async fn complete_structured_detailed(
        &self,
        req: CompletionRequest,
        schema: serde_json::Value,
    ) -> Result<DetailedCompletionResponse> {
        let mut request_provider = self.clone();
        request_provider.output_schema = Some(schema);
        request_provider.complete_detailed(req).await
    }

    async fn complete_detailed(
        &self,
        req: CompletionRequest,
    ) -> Result<DetailedCompletionResponse> {
        let key = resolve_credential(&self.cred)?;
        // Optional fields are OMITTED when absent — the Messages API rejects
        // explicit nulls (`system: Input should be a valid array`, found live
        // by /healthai 2026-08-09).
        let mut body = serde_json::json!({
            "model": req.model,
            "max_tokens": req.max_tokens.max(1),
            "messages": [{ "role": "user", "content": req.prompt }],
        });
        if let Some(schema) = &self.output_schema {
            body["output_config"] =
                serde_json::json!({"format":{"type":"json_schema","schema":schema}});
        }
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
            .map_err(|e| KernelError::Provider(format!("bad response: {}", e.without_url())))?;
        let usage = usage_evidence(&v, "input_tokens", "output_tokens");
        accounting::finish("anthropic", &v, false).await?;
        Ok(DetailedCompletionResponse {
            usage,
            response: CompletionResponse {
                text: anthropic_text(&v),
                stop_reason: extract_str(&v, &["stop_reason"]),
                input_tokens: usage.input_tokens.unwrap_or(0),
                output_tokens: usage.output_tokens.unwrap_or(0),
                request_hash: hash,
            },
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
            .map_err(|e| KernelError::Provider(format!("unreachable: {}", e.without_url())))?;
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

#[derive(Clone)]
pub struct OpenAiProvider {
    pub endpoint: String,
    pub cred: CredentialRef,
    /// openrouter specialization: extra attribution headers.
    pub extra_headers: Vec<(String, String)>,
    pub provider_id: ProviderId,
    pub downstream_provider: Option<String>,
    output_schema: Option<serde_json::Value>,
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
            downstream_provider: None,
            output_schema: None,
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
            downstream_provider: None,
            output_schema: None,
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
fn parse_openai_completion(
    v: &serde_json::Value,
    hash: String,
) -> Result<DetailedCompletionResponse> {
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
    let usage = usage_evidence(v, "prompt_tokens", "completion_tokens");
    Ok(DetailedCompletionResponse {
        usage,
        response: CompletionResponse {
            text,
            stop_reason: finish_reason.to_string(),
            input_tokens: usage.input_tokens.unwrap_or(0),
            output_tokens: usage.output_tokens.unwrap_or(0),
            request_hash: hash,
        },
    })
}

#[async_trait]
impl ModelProvider for OpenAiProvider {
    fn id(&self) -> ProviderId {
        self.provider_id
    }

    async fn complete_structured(
        &self,
        req: CompletionRequest,
        schema: serde_json::Value,
    ) -> Result<CompletionResponse> {
        Ok(self
            .complete_structured_detailed(req, schema)
            .await?
            .response)
    }

    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse> {
        Ok(self.complete_detailed(req).await?.response)
    }

    async fn complete_structured_detailed(
        &self,
        req: CompletionRequest,
        schema: serde_json::Value,
    ) -> Result<DetailedCompletionResponse> {
        let mut request_provider = self.clone();
        request_provider.output_schema = Some(schema);
        request_provider.complete_detailed(req).await
    }

    async fn complete_detailed(
        &self,
        req: CompletionRequest,
    ) -> Result<DetailedCompletionResponse> {
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
        if let Some(schema) = &self.output_schema {
            body["response_format"] = serde_json::json!({"type":"json_schema","json_schema":{"name":"munarium_response","strict":true,"schema":schema}});
        }
        if let Some(slug) = &self.downstream_provider {
            body["provider"] = serde_json::json!({"only":[slug],"allow_fallbacks":false,"require_parameters":true,"data_collection":"deny"});
        }
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
            .map_err(|e| KernelError::Provider(format!("bad response: {}", e.without_url())))?;
        accounting::finish("openai", &v, false).await?;
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
            .map_err(|e| KernelError::Provider(format!("bad response: {}", e.without_url())))?;
        accounting::finish("openai", &v, true).await?;
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
            .map_err(|e| KernelError::Provider(format!("unreachable: {}", e.without_url())))?;
        Ok(ProviderHealth {
            healthy: resp.status().is_success(),
            endpoint_fingerprint: fingerprint(&self.endpoint),
            detail: format!("GET /models -> {}", resp.status()),
        })
    }
}

fn fingerprint(endpoint: &str) -> String {
    if accounting::route_identity(endpoint, None).is_none() {
        return "unavailable".into();
    }
    hex::encode(&sha2::Sha256::digest(endpoint.as_bytes())[..8])
}

/// Factory from a validated config doc.
pub fn build_provider(doc: &ProviderConfigDoc) -> Result<Box<dyn ModelProvider>> {
    let endpoint = doc.spec.endpoint.as_deref();
    if doc.spec.provider == "ollama" {
        return Ok(Box::new(OllamaProvider::new(&doc.spec)?));
    }
    let cred = doc.spec.credential_ref.clone().ok_or_else(|| {
        KernelError::InvalidInput("credentialRef is required for this provider".into())
    })?;
    Ok(match doc.spec.provider.as_str() {
        "anthropic" => Box::new(AnthropicProvider::new(endpoint, cred)),
        "openrouter" => {
            let mut provider = OpenAiProvider::openrouter(endpoint, cred);
            provider.downstream_provider = doc.spec.openrouter_provider.clone();
            Box::new(provider)
        }
        "openai" => Box::new(OpenAiProvider::new(endpoint, cred)),
        _ => return Err(KernelError::InvalidInput("unsupported provider".into())),
    })
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
        assert!(matches!(
            doc.spec.credential_ref,
            Some(CredentialRef::Env { .. })
        ));

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
        let parse =
            |v: serde_json::Value| parse_openai_completion(&v, "h".into()).map(|out| out.response);
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
    fn diagnostics_require_explicit_safe_labels_and_never_hash_credential_urls() {
        for (alias, valid) in [
            ("", false),
            ("bad label", false),
            ("folder/key", false),
            (&"x".repeat(65), false),
            ("public-label.1", true),
            (&"x".repeat(64), true),
        ] {
            let yaml = format!("apiVersion: munarium.ioka.io/v1\nkind: ProviderConfig\nmetadata: {{name: fixture}}\nspec:\n  provider: ollama\n  endpoint: http://127.0.0.1:11434\n  credentialAlias: '{alias}'\n");
            assert_eq!(parse_provider_config(&yaml).is_ok(), valid);
        }
        assert_eq!(
            fingerprint("https://user:fictional-secret@example.invalid"),
            "unavailable"
        );
        assert_eq!(
            fingerprint("https://example.invalid?key=fictional-secret"),
            "unavailable"
        );
        assert_eq!(
            fingerprint("https://example.invalid"),
            hex::encode(&sha2::Sha256::digest(b"https://example.invalid")[..8])
        );
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

    #[test]
    fn budget_arithmetic_cannot_overflow_into_a_bypass() {
        // P15/R32: `s.tokens + estimated_tokens` overflowed while the lock
        // was held — a panic in debug builds, and in release a wrapped sum
        // that admitted the request under a tpm limit.
        let b = RateBudget::new(&Budgets {
            tpm: Some(100),
            ..Default::default()
        });
        assert!(b.check(10).is_ok());
        assert!(matches!(
            b.check(u64::MAX),
            Err(KernelError::RateLimited(_))
        ));
        assert!(b.check(90).is_ok(), "tokens == tpm is still admitted");
        assert!(matches!(b.check(1), Err(KernelError::RateLimited(_))));
        // Without a tpm limit a huge estimate is admitted and saturates.
        let b = RateBudget::new(&Budgets::default());
        assert!(b.check(u64::MAX).is_ok());
        assert!(b.check(u64::MAX).is_ok());
    }

    #[test]
    fn a_poisoned_budget_refuses_rather_than_panicking() {
        let b = std::sync::Arc::new(RateBudget::new(&Budgets {
            rpm: Some(10),
            ..Default::default()
        }));
        let held = b.clone();
        let _ = std::thread::spawn(move || {
            let _guard = held.state.lock().unwrap();
            panic!("poison the budget");
        })
        .join();
        assert!(matches!(b.check(1), Err(KernelError::Storage(_))));
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
