// SPDX-License-Identifier: Apache-2.0
//! Provider gateway wiring: per-tenant registry + REST routes + gRPC
//! ProviderService. Every model call is budget-checked and recorded as an
//! invocation-provenance event when the caller names a version (request
//! hash, provider, model, token counts, latency — never keys, never bodies).

use crate::error::{to_status, ApiError};
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use munarium_api_types as dto;
use munarium_core::provider::{CompletionRequest, EmbeddingRequest, ModelProvider};
use munarium_core::{KernelError, Result};
use munarium_proto::mmp::v1 as pb;
use munarium_providers::{
    build_provider, builtin_tier_model, default_config_doc, default_env_var, parse_provider_config,
    resolve_complete_model, resolve_credential, ModelTier, ProviderConfigDoc, RateBudget,
    DEFAULT_PROVIDER_PRIORITY,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tonic::{Request, Response, Status};

/// Reserved config name engaging the default-provider rule.
pub const DEFAULT_SELECTOR: &str = "default";

pub struct ProviderEntry {
    pub doc: ProviderConfigDoc,
    pub provider: Box<dyn ModelProvider>,
    pub budget: RateBudget,
}

#[derive(Default)]
pub struct ProviderRegistry {
    entries: RwLock<HashMap<(String, String), Arc<ProviderEntry>>>,
    /// tenant -> last table load. Entries older than MUNARIUM_REGISTRY_TTL_SECS
    /// are re-read so a config applied on ANOTHER instance converges here
    /// within the TTL (the N-replica staleness fix, 2026-08-17).
    loaded: Mutex<HashMap<String, std::time::Instant>>,
    /// (tenant, name) -> hash of the yaml the current entry was built from.
    /// A TTL reload rebuilds only changed entries — a rebuild resets the
    /// entry's in-memory RateBudget window, so unchanged configs must keep
    /// their entry.
    yaml_hashes: Mutex<HashMap<(String, String), String>>,
    /// Synthesized env-backed default configs, keyed by provider family.
    defaults: RwLock<HashMap<String, Arc<ProviderEntry>>>,
    /// Embedding cache by request hash — re-index runs are cheap when only
    /// the chunker changed. In-memory demo posture (table-backed documented);
    /// per-process on purpose: it is a pure cache, so N replicas just mean N
    /// cold caches.
    embed_cache: Mutex<HashMap<(String, String), munarium_core::provider::EmbeddingResponse>>,
}

/// Ceiling on `embed_cache` entries before it is reset. At ~1536 floats per
/// vector this is on the order of 60 MiB, which is a cache and not a leak.
const EMBED_CACHE_MAX_ENTRIES: usize = 10_000;

impl ProviderRegistry {
    pub async fn apply(&self, state: &AppState, tenant: &str, yaml: &str) -> Result<String> {
        let doc = parse_provider_config(yaml).map_err(KernelError::InvalidInput)?;
        if doc.metadata.name == DEFAULT_SELECTOR {
            return Err(KernelError::InvalidInput(
                "config name 'default' is reserved for the default-provider rule".into(),
            ));
        }
        let name = doc.metadata.name.clone();
        let entry = Arc::new(ProviderEntry {
            budget: RateBudget::new_shared(&doc.spec.budgets, state.config.replica_count),
            provider: build_provider(&doc),
            doc,
        });
        self.entries
            .write()
            .await
            .insert((tenant.to_string(), name.clone()), entry);
        // Record the applied yaml's hash so the next TTL reload sees this
        // entry as current instead of rebuilding it (and its budget window).
        self.yaml_hashes.lock().await.insert(
            (tenant.to_string(), name.clone()),
            crate::state::request_hash(yaml.as_bytes()),
        );
        if let Some(pool) = state.pg_pool() {
            sqlx::query(
                "INSERT INTO provider_configs (tenant_id, name, yaml) VALUES ($1, $2, $3)
                 ON CONFLICT (tenant_id, name) DO UPDATE SET yaml = EXCLUDED.yaml",
            )
            .bind(tenant)
            .bind(&name)
            .bind(yaml)
            .execute(pool)
            .await
            .map_err(|e| KernelError::Storage(e.to_string()))?;
        }
        Ok(name)
    }

    pub async fn get(
        &self,
        state: &AppState,
        tenant: &str,
        name: &str,
    ) -> Result<Arc<ProviderEntry>> {
        self.ensure_loaded(state, tenant).await?;
        self.entries
            .read()
            .await
            .get(&(tenant.to_string(), name.to_string()))
            .cloned()
            .ok_or_else(|| KernelError::NotFound {
                kind: "provider-config",
                id: name.to_string(),
            })
    }

    /// Resolve a provider entry. The reserved name `default` (optionally with
    /// a provider-family override) engages the default rule: anthropic first,
    /// openai second, openrouter third — the first family with a usable
    /// credential wins. Within a family, tenant-applied configs take
    /// precedence over the synthesized env-backed default.
    pub async fn resolve(
        &self,
        state: &AppState,
        tenant: &str,
        name: &str,
        provider_hint: Option<&str>,
    ) -> Result<Arc<ProviderEntry>> {
        if name != DEFAULT_SELECTOR {
            if provider_hint.is_some() {
                return Err(KernelError::InvalidInput(
                    "the provider field requires the reserved 'default' config name".into(),
                ));
            }
            return self.get(state, tenant, name).await;
        }
        if let Some(family) = provider_hint {
            return self
                .family_entry(state, tenant, family)
                .await?
                .ok_or_else(|| {
                    KernelError::Provider(format!(
                        "no usable credential for provider '{family}' (no applied config; env var '{}' not set)",
                        default_env_var(family).unwrap_or("?")
                    ))
                });
        }
        for family in DEFAULT_PROVIDER_PRIORITY {
            if let Some(entry) = self.family_entry(state, tenant, family).await? {
                return Ok(entry);
            }
        }
        Err(KernelError::Provider(
            "no default provider credential configured (checked MUNARIUM_SECRET_ANTHROPIC, \
             MUNARIUM_SECRET_OPENAI, MUNARIUM_SECRET_OPENROUTER and applied configs)"
                .into(),
        ))
    }

    /// The best usable entry for one provider family, or None when no
    /// credential resolves. Deterministic: applied configs sorted by name,
    /// then the env-backed default.
    async fn family_entry(
        &self,
        state: &AppState,
        tenant: &str,
        family: &str,
    ) -> Result<Option<Arc<ProviderEntry>>> {
        if default_env_var(family).is_none() {
            return Err(KernelError::InvalidInput(format!(
                "unsupported provider '{family}' (anthropic|openai|openrouter)"
            )));
        }
        self.ensure_loaded(state, tenant).await?;
        let mut candidates: Vec<Arc<ProviderEntry>> = self
            .entries
            .read()
            .await
            .iter()
            .filter(|((t, _), e)| t == tenant && e.doc.spec.provider == family)
            .map(|(_, e)| e.clone())
            .collect();
        candidates.sort_by(|a, b| a.doc.metadata.name.cmp(&b.doc.metadata.name));
        for c in candidates {
            if resolve_credential(&c.doc.spec.credential_ref).is_ok() {
                return Ok(Some(c));
            }
        }
        if let Some(entry) = self.default_entry(state, family).await {
            if resolve_credential(&entry.doc.spec.credential_ref).is_ok() {
                return Ok(Some(entry));
            }
        }
        Ok(None)
    }

    async fn default_entry(&self, state: &AppState, family: &str) -> Option<Arc<ProviderEntry>> {
        if let Some(e) = self.defaults.read().await.get(family) {
            return Some(e.clone());
        }
        let doc = default_config_doc(family)?;
        let entry = Arc::new(ProviderEntry {
            budget: RateBudget::new_shared(&doc.spec.budgets, state.config.replica_count),
            provider: build_provider(&doc),
            doc,
        });
        self.defaults
            .write()
            .await
            .insert(family.to_string(), entry.clone());
        Some(entry)
    }

    async fn ensure_loaded(&self, state: &AppState, tenant: &str) -> Result<()> {
        let ttl = state.config.registry_ttl_secs;
        {
            let loaded = self.loaded.lock().await;
            if let Some(at) = loaded.get(tenant) {
                if ttl == 0 || at.elapsed().as_secs() < ttl {
                    return Ok(());
                }
            }
        }
        if let Some(pool) = state.pg_pool() {
            let rows: Vec<(String,)> =
                sqlx::query_as("SELECT yaml FROM provider_configs WHERE tenant_id = $1")
                    .bind(tenant)
                    .fetch_all(pool)
                    .await
                    .map_err(|e| KernelError::Storage(e.to_string()))?;
            for (yaml,) in rows {
                let hash = crate::state::request_hash(yaml.as_bytes());
                if let Ok(doc) = parse_provider_config(&yaml) {
                    let key = (tenant.to_string(), doc.metadata.name.clone());
                    if self.yaml_hashes.lock().await.get(&key) == Some(&hash) {
                        continue; // unchanged — keep the entry and its budget window
                    }
                    let entry = Arc::new(ProviderEntry {
                        budget: RateBudget::new_shared(
                            &doc.spec.budgets,
                            state.config.replica_count,
                        ),
                        provider: build_provider(&doc),
                        doc,
                    });
                    self.entries.write().await.insert(key.clone(), entry);
                    self.yaml_hashes.lock().await.insert(key, hash);
                }
            }
        }
        self.loaded
            .lock()
            .await
            .insert(tenant.to_string(), std::time::Instant::now());
        Ok(())
    }

    /// All applied configs for a tenant, name-sorted. Free — no provider
    /// calls; used by the read-only introspection plane.
    pub async fn list(&self, state: &AppState, tenant: &str) -> Result<Vec<Arc<ProviderEntry>>> {
        self.ensure_loaded(state, tenant).await?;
        let mut entries: Vec<Arc<ProviderEntry>> = self
            .entries
            .read()
            .await
            .iter()
            .filter(|((t, _), _)| t == tenant)
            .map(|(_, e)| e.clone())
            .collect();
        entries.sort_by(|a, b| a.doc.metadata.name.cmp(&b.doc.metadata.name));
        Ok(entries)
    }

    pub async fn cached_embedding(
        &self,
        tenant: &str,
        hash: &str,
    ) -> Option<munarium_core::provider::EmbeddingResponse> {
        self.embed_cache
            .lock()
            .await
            .get(&(tenant.to_string(), hash.to_string()))
            .cloned()
    }

    pub async fn cache_embedding(
        &self,
        tenant: &str,
        hash: &str,
        resp: &munarium_core::provider::EmbeddingResponse,
    ) {
        let mut cache = self.embed_cache.lock().await;
        // Bounded. Each entry holds full float vectors, nothing evicted, and
        // the server is deployed: distinct embed requests moved process RSS
        // monotonically until a restart. A whole-cache reset at the ceiling
        // is crude but honest for a pure cache — N cold caches were already
        // the documented posture across replicas.
        if cache.len() >= EMBED_CACHE_MAX_ENTRIES {
            cache.clear();
        }
        cache.insert((tenant.to_string(), hash.to_string()), resp.clone());
    }
}

/// Records the invocation-provenance event when a version is named.
async fn record_invocation(
    store: &dyn munarium_core::storage::StorageBackend,
    version_id: &str,
    provider: &str,
    model: &str,
    request_hash: &str,
    input_tokens: u64,
    output_tokens: u64,
    latency_ms: u128,
    cache_hit: bool,
) -> Result<Option<String>> {
    let mut claim = munarium_core::storage::NewClaim::fact(
        "invocation",
        &request_hash[..16.min(request_hash.len())],
        &format!("{provider}/{model}"),
    );
    claim.evidence = Some(serde_json::json!({
        "request_hash": request_hash,
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "latency_ms": latency_ms as u64,
        "cache_hit": cache_hit,
    }));
    Ok(Some(store.append_claim(version_id, claim, None).await?.id))
}

// ---------------------------------------------------------------------------
// operations shared by both planes
// ---------------------------------------------------------------------------

/// Complete + (when a version is named) record invocation provenance. The one
/// implementation behind REST `/v1/providers/{name}/complete` and gRPC
/// `ProviderService/Complete` — provenance can never exist on one plane only.
pub async fn op_complete(
    state: &AppState,
    tenant: &str,
    store: &dyn munarium_core::storage::StorageBackend,
    name: &str,
    req: dto::CompleteRequest,
) -> Result<dto::CompleteResponse> {
    let tier = req
        .tier
        .as_deref()
        .map(ModelTier::parse)
        .transpose()
        .map_err(KernelError::InvalidInput)?;
    let entry = state
        .providers
        .resolve(state, tenant, name, req.provider.as_deref())
        .await?;
    // Resolve the lineage BEFORE spending a provider call: a bad version_id
    // must not bill the caller and then throw the completion away.
    if let Some(version_id) = req.version_id.as_deref() {
        store.head(version_id).await?;
    }
    let prompt = req.prompt.unwrap_or_default();
    // The request's own ceiling, else the tenant's `complete_default`
    // (`/v1/max-tokens`; built-in 1,024 since 2026-09-02, 512 before).
    let max_tokens = match req.max_tokens {
        Some(t) => t,
        None => {
            state
                .max_tokens
                .effective(state, tenant)
                .await?
                .complete_default
        }
    };
    entry
        .budget
        .check((prompt.len() / 4) as u64 + max_tokens as u64)?;
    let model = resolve_complete_model(&entry.doc.spec, req.model, tier)?;
    // Daily token cap (spending caps, 2026-09-01): reserve BEFORE the
    // provider call at the same estimate the rpm/tpm bucket consumes, settle
    // to the provider's actual counts after. Keyed per tier — an
    // explicit-model request names no tier and passes uncapped (this surface
    // is bearer-authenticated; the demo always sends a tier). The reserve
    // runs after the rpm/tpm check so a rate-limit refusal never burns cap.
    let mut cap_reservation = None;
    if let Some(t) = tier {
        let limit = entry.doc.spec.budgets.daily_tokens.for_tier(t);
        let estimate = (prompt.len() / 4) as u64 + max_tokens as u64;
        match state
            .budgets()
            .reserve(
                tenant,
                &entry.doc.metadata.name,
                t.as_str(),
                estimate,
                limit,
            )
            .await?
        {
            munarium_core::budget::BudgetOutcome::Unlimited => {}
            munarium_core::budget::BudgetOutcome::Granted(r) => cap_reservation = Some(r),
            munarium_core::budget::BudgetOutcome::Exhausted {
                requested,
                remaining,
                limit,
            } => {
                return Err(KernelError::RateLimited(format!(
                    "{}provider config '{}' tier '{}' has {remaining} of {limit} daily tokens \
                     left and this call needs {requested}; the window resets at midnight UTC",
                    crate::error::DAILY_CAP_PREFIX,
                    entry.doc.metadata.name,
                    t.as_str(),
                )));
            }
        }
    }
    let started = std::time::Instant::now();
    let result = entry
        .provider
        .complete(CompletionRequest {
            model: model.clone(),
            system: req.system,
            prompt,
            max_tokens,
            temperature: req.temperature,
            tools: None,
        })
        .await;
    // Settle whatever happened: actuals on success, the estimate on failure
    // (the provider may have been reached — spent, never free). A settle
    // failure must not fail a completion that already happened; the stale
    // sweep stamps the row later, in the same spent direction.
    if let Some(r) = &cap_reservation {
        let actual = result
            .as_ref()
            .ok()
            .map(|o| o.input_tokens + o.output_tokens);
        if let Err(e) = state.budgets().settle(r, actual).await {
            tracing::warn!(error = %e, "budget settle failed; reservation stands at its estimate");
        }
    }
    let family = entry.doc.spec.provider.as_str();
    state.metrics.inc(
        "munarium_provider_calls_total",
        crate::metrics::labels(&[
            ("provider", family),
            ("model", &model),
            ("kind", "complete"),
            ("outcome", if result.is_ok() { "ok" } else { "error" }),
        ]),
    );
    state.metrics.observe(
        "munarium_provider_call_duration_seconds",
        crate::metrics::labels(&[("provider", family), ("kind", "complete")]),
        started.elapsed().as_secs_f64(),
    );
    let out = result?;
    state.metrics.inc_by(
        "munarium_provider_tokens_total",
        crate::metrics::labels(&[
            ("provider", family),
            ("model", &model),
            ("direction", "input"),
        ]),
        out.input_tokens,
    );
    state.metrics.inc_by(
        "munarium_provider_tokens_total",
        crate::metrics::labels(&[
            ("provider", family),
            ("model", &model),
            ("direction", "output"),
        ]),
        out.output_tokens,
    );
    let mut invocation_event_id = None;
    if let Some(version_id) = req.version_id.as_deref() {
        invocation_event_id = record_invocation(
            store,
            version_id,
            &entry.doc.spec.provider,
            &model,
            &out.request_hash,
            out.input_tokens,
            out.output_tokens,
            started.elapsed().as_millis(),
            false,
        )
        .await?;
    }
    Ok(dto::CompleteResponse {
        text: out.text,
        stop_reason: out.stop_reason,
        input_tokens: out.input_tokens,
        output_tokens: out.output_tokens,
        provider: entry.doc.spec.provider.clone(),
        model,
        invocation_event_id,
    })
}

/// Embed (request-hash cached) + optional invocation provenance. Shared by
/// both planes.
pub async fn op_embed(
    state: &AppState,
    tenant: &str,
    store: &dyn munarium_core::storage::StorageBackend,
    name: &str,
    req: dto::EmbedRequest,
) -> Result<dto::EmbedResponse> {
    let entry = state
        .providers
        .resolve(state, tenant, name, req.provider.as_deref())
        .await?;
    if let Some(version_id) = req.version_id.as_deref() {
        store.head(version_id).await?;
    }
    let inputs = req.inputs;
    if inputs.is_empty() {
        return Err(KernelError::InvalidInput("inputs is required".into()));
    }
    let model = req
        .model
        .or_else(|| entry.doc.spec.models.embed.first().cloned())
        .ok_or(KernelError::InvalidInput(
            "no embed model given or configured".into(),
        ))?;
    let est: u64 = inputs.iter().map(|i| (i.len() / 4) as u64).sum();
    entry.budget.check(est)?;

    let pre_hash = munarium_providers::request_hash(&serde_json::json!({
        "embed": entry.doc.spec.endpoint, "model": model, "inputs": inputs,
    }));
    let started = std::time::Instant::now();
    let (out, cache_hit) = match state.providers.cached_embedding(tenant, &pre_hash).await {
        Some(hit) => (hit, true),
        None => {
            let result = entry
                .provider
                .embed(EmbeddingRequest {
                    model: model.clone(),
                    inputs,
                })
                .await;
            // Cache hits are free and never counted; only real provider
            // calls reach the metrics.
            let family = entry.doc.spec.provider.as_str();
            state.metrics.inc(
                "munarium_provider_calls_total",
                crate::metrics::labels(&[
                    ("provider", family),
                    ("model", &model),
                    ("kind", "embed"),
                    ("outcome", if result.is_ok() { "ok" } else { "error" }),
                ]),
            );
            state.metrics.observe(
                "munarium_provider_call_duration_seconds",
                crate::metrics::labels(&[("provider", family), ("kind", "embed")]),
                started.elapsed().as_secs_f64(),
            );
            let fresh = result?;
            state
                .providers
                .cache_embedding(tenant, &pre_hash, &fresh)
                .await;
            (fresh, false)
        }
    };
    let mut invocation_event_id = None;
    if let Some(version_id) = req.version_id.as_deref() {
        invocation_event_id = record_invocation(
            store,
            version_id,
            &entry.doc.spec.provider,
            &model,
            &pre_hash,
            0,
            0,
            started.elapsed().as_millis(),
            cache_hit,
        )
        .await?;
    }
    Ok(dto::EmbedResponse {
        vectors: out.vectors,
        dimensions: out.dimensions as u64,
        cache_hit,
        provider: entry.doc.spec.provider.clone(),
        model,
        invocation_event_id,
    })
}

/// Live probe of the built-in default models: one small completion per
/// provider family × tier (9 checks). Providers with no configured credential
/// are reported skipped; overall health requires every configured provider's
/// probes to pass and at least one credential present. Uses the server-level
/// env-backed default configs only — never tenant configs, never budgets.
pub async fn op_healthai(probe_max_tokens: u32) -> dto::HealthAiResponse {
    let mut handles = Vec::new();
    for family in DEFAULT_PROVIDER_PRIORITY {
        for tier in ModelTier::ALL {
            let model = builtin_tier_model(family, tier)
                .unwrap_or_default()
                .to_string();
            handles.push(tokio::spawn(async move {
                let doc = default_config_doc(family).expect("known family");
                let env = default_env_var(family).expect("known family");
                let mut check = dto::HealthAiCheck {
                    provider: family.to_string(),
                    tier: tier.as_str().to_string(),
                    model: model.clone(),
                    ok: false,
                    skipped: false,
                    latency_ms: None,
                    detail: String::new(),
                };
                if resolve_credential(&doc.spec.credential_ref).is_err() {
                    check.skipped = true;
                    check.detail = format!("credential env var '{env}' is not set");
                    return check;
                }
                let provider = build_provider(&doc);
                let started = std::time::Instant::now();
                let result = tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    provider.complete(CompletionRequest {
                        model,
                        system: None,
                        prompt: "Reply with the single word OK.".into(),
                        // Generous: reasoning models spend hidden reasoning
                        // tokens from the completion budget before any text.
                        // The caller's `healthai_probe` budget
                        // (`/v1/max-tokens`; built-in 512 since 2026-09-02,
                        // 256 before).
                        max_tokens: probe_max_tokens,
                        temperature: None,
                        tools: None,
                    }),
                )
                .await;
                check.latency_ms = Some(started.elapsed().as_millis() as u64);
                match result {
                    Ok(Ok(resp)) => {
                        if resp.text.trim().is_empty() {
                            check.detail =
                                format!("empty completion (stop_reason '{}')", resp.stop_reason);
                        } else {
                            check.ok = true;
                            check.detail = format!("ok (stop_reason '{}')", resp.stop_reason);
                        }
                    }
                    Ok(Err(e)) => {
                        check.detail = e.to_string().chars().take(300).collect();
                    }
                    Err(_) => {
                        check.detail = "timed out after 30s".into();
                    }
                }
                check
            }));
        }
    }
    let mut checks = Vec::new();
    for h in handles {
        match h.await {
            Ok(c) => checks.push(c),
            Err(e) => checks.push(dto::HealthAiCheck {
                provider: "unknown".into(),
                tier: "unknown".into(),
                model: String::new(),
                ok: false,
                skipped: false,
                latency_ms: None,
                detail: format!("probe task failed: {e}"),
            }),
        }
    }
    let probed: Vec<&dto::HealthAiCheck> = checks.iter().filter(|c| !c.skipped).collect();
    let healthy = !probed.is_empty() && probed.iter().all(|c| c.ok);
    dto::HealthAiResponse { healthy, checks }
}

/// Provider health, shared by both planes.
pub async fn op_provider_health(
    state: &AppState,
    tenant: &str,
    name: &str,
) -> Result<dto::ProviderHealthResponse> {
    let entry = state.providers.get(state, tenant, name).await?;
    let health = entry.provider.health().await?;
    Ok(dto::ProviderHealthResponse {
        healthy: health.healthy,
        provider: entry.doc.spec.provider.clone(),
        endpoint_fingerprint: health.endpoint_fingerprint,
        detail: health.detail,
    })
}

// ---------------------------------------------------------------------------
// REST
// ---------------------------------------------------------------------------

type ApiResult<T> = std::result::Result<T, ApiError>;

async fn rest_auth(
    state: &AppState,
    headers: &HeaderMap,
) -> ApiResult<(
    crate::state::TenantCtx,
    Arc<dyn munarium_core::storage::StorageBackend>,
)> {
    crate::rest::auth(state, headers).await
}

/// Resolve one config's tier models without any provider call.
fn tier_models(
    spec: &munarium_providers::ProviderSpec,
) -> (Option<String>, Option<String>, Option<String>) {
    let fast = resolve_complete_model(spec, None, Some(ModelTier::Fast)).ok();
    let capable = resolve_complete_model(spec, None, Some(ModelTier::Capable)).ok();
    let frontier = resolve_complete_model(spec, None, Some(ModelTier::Frontier)).ok();
    (fast, capable, frontier)
}

/// GET /v1/providers — free introspection: every applied config plus the
/// synthesized env-backed defaults, each with the concrete model its
/// fast/capable tiers resolve to. Zero provider calls, zero tokens; the
/// disclosure counterpart to /healthai's paid probe. credentialRef is never
/// echoed — only whether it currently resolves.
#[utoipa::path(get, path = "/v1/providers",
    responses((status = 200, body = dto::ProviderListResponse)), tag = "providers")]
pub async fn list_providers(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::ProviderListResponse>> {
    let (ctx, _store) = rest_auth(&state, &headers).await?;
    Ok(Json(dto::ProviderListResponse {
        providers: op_list_providers(&state, &ctx.tenant_id).await?,
    }))
}

/// The free provider introspection shared by `GET /v1/providers` and the
/// /admin providers page (2026-08-27): applied configs first, then the
/// env-backed family defaults; no provider call, no key material.
pub async fn op_list_providers(
    state: &AppState,
    tenant: &str,
) -> Result<Vec<dto::ProviderModelsDto>> {
    let mut providers = Vec::new();
    for entry in state.providers.list(state, tenant).await? {
        let (fast, capable, frontier) = tier_models(&entry.doc.spec);
        providers.push(dto::ProviderModelsDto {
            name: entry.doc.metadata.name.clone(),
            provider: entry.doc.spec.provider.clone(),
            source: "applied".into(),
            credential_ok: resolve_credential(&entry.doc.spec.credential_ref).is_ok(),
            fast,
            capable,
            frontier,
        });
    }
    for family in DEFAULT_PROVIDER_PRIORITY {
        if let Some(doc) = default_config_doc(family) {
            let (fast, capable, frontier) = tier_models(&doc.spec);
            providers.push(dto::ProviderModelsDto {
                name: doc.metadata.name.clone(),
                provider: family.to_string(),
                source: "default".into(),
                credential_ok: resolve_credential(&doc.spec.credential_ref).is_ok(),
                fast,
                capable,
                frontier,
            });
        }
    }
    Ok(providers)
}

#[utoipa::path(post, path = "/v1/providers",
    request_body(content = String, content_type = "text/yaml",
        description = "kind: ProviderConfig — credentialRef only, never key material"),
    responses((status = 200, body = dto::ApplyProviderConfigResponse)), tag = "providers")]
pub async fn apply_provider(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    yaml: String,
) -> ApiResult<Json<dto::ApplyProviderConfigResponse>> {
    let (ctx, _store) = rest_auth(&state, &headers).await?;
    ctx.require_rw()?;
    let name = state.providers.apply(&state, &ctx.tenant_id, &yaml).await?;
    Ok(Json(dto::ApplyProviderConfigResponse { config_name: name }))
}

#[utoipa::path(get, path = "/v1/providers/{name}/health",
    params(("name" = String, Path, description = "provider config name")),
    responses((status = 200, body = dto::ProviderHealthResponse)), tag = "providers")]
pub async fn provider_health(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::ProviderHealthResponse>> {
    let (ctx, _store) = rest_auth(&state, &headers).await?;
    Ok(Json(
        op_provider_health(&state, &ctx.tenant_id, &name).await?,
    ))
}

#[utoipa::path(post, path = "/v1/providers/{name}/complete",
    params(("name" = String, Path, description = "provider config name")),
    request_body = dto::CompleteRequest,
    responses((status = 200, body = dto::CompleteResponse)), tag = "providers")]
pub async fn provider_complete(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    crate::rest::ProblemJson(req): crate::rest::ProblemJson<dto::CompleteRequest>,
) -> ApiResult<Json<dto::CompleteResponse>> {
    let (ctx, store) = rest_auth(&state, &headers).await?;
    ctx.require_rw()?;
    Ok(Json(
        op_complete(&state, &ctx.tenant_id, store.as_ref(), &name, req).await?,
    ))
}

#[utoipa::path(post, path = "/v1/providers/{name}/embed",
    params(("name" = String, Path, description = "provider config name")),
    request_body = dto::EmbedRequest,
    responses((status = 200, body = dto::EmbedResponse)), tag = "providers")]
pub async fn provider_embed(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    crate::rest::ProblemJson(req): crate::rest::ProblemJson<dto::EmbedRequest>,
) -> ApiResult<Json<dto::EmbedResponse>> {
    let (ctx, store) = rest_auth(&state, &headers).await?;
    ctx.require_rw()?;
    Ok(Json(
        op_embed(&state, &ctx.tenant_id, store.as_ref(), &name, req).await?,
    ))
}

/// GET /healthai — live probe of all nine built-in default models (three
/// provider families × three tiers). Authenticated (any role): each call spends
/// real provider tokens, so it must not be drive-by reachable like /healthz.
#[utoipa::path(get, path = "/healthai",
    responses((status = 200, body = dto::HealthAiResponse)), tag = "providers")]
pub async fn healthai(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::HealthAiResponse>> {
    let (ctx, _store) = rest_auth(&state, &headers).await?;
    let probe = state
        .max_tokens
        .effective(&state, &ctx.tenant_id)
        .await?
        .healthai_probe;
    Ok(Json(op_healthai(probe).await))
}

// ---------------------------------------------------------------------------
// gRPC ProviderService
// ---------------------------------------------------------------------------

pub struct ProviderSvc {
    pub state: Arc<AppState>,
}

#[tonic::async_trait]
impl pb::provider_service_server::ProviderService for ProviderSvc {
    async fn apply_provider_config(
        &self,
        req: Request<pb::ApplyProviderConfigRequest>,
    ) -> std::result::Result<Response<pb::ApplyProviderConfigResponse>, Status> {
        let ctx = crate::grpc::authenticate(&self.state, &req).await?;
        ctx.require_rw_pub()?;
        let inner = req.into_inner();
        let name = self
            .state
            .providers
            .apply(&self.state, &ctx.tenant_id, &inner.yaml)
            .await
            .map_err(|e| to_status(&e))?;
        Ok(Response::new(pb::ApplyProviderConfigResponse {
            config_name: name,
        }))
    }

    async fn provider_health(
        &self,
        req: Request<pb::ProviderHealthRequest>,
    ) -> std::result::Result<Response<pb::ProviderHealthResponse>, Status> {
        let ctx = crate::grpc::authenticate(&self.state, &req).await?;
        let inner = req.into_inner();
        let health = op_provider_health(&self.state, &ctx.tenant_id, &inner.config_name)
            .await
            .map_err(|e| to_status(&e))?;
        Ok(Response::new(pb::ProviderHealthResponse {
            healthy: health.healthy,
            provider: health.provider,
            endpoint_fingerprint: health.endpoint_fingerprint,
            detail: health.detail,
        }))
    }

    async fn complete(
        &self,
        req: Request<pb::CompleteRequest>,
    ) -> std::result::Result<Response<pb::CompleteResponse>, Status> {
        let ctx = crate::grpc::authenticate(&self.state, &req).await?;
        ctx.require_rw_pub()?;
        let inner = req.into_inner();
        let out = op_complete(
            &self.state,
            &ctx.tenant_id,
            ctx.store.as_ref(),
            &inner.config_name,
            dto::CompleteRequest {
                model: crate::grpc::none_if_empty(&inner.model),
                provider: crate::grpc::none_if_empty(&inner.provider),
                tier: crate::grpc::none_if_empty(&inner.tier),
                system: crate::grpc::none_if_empty(&inner.system),
                prompt: Some(inner.prompt),
                max_tokens: if inner.max_tokens == 0 {
                    None
                } else {
                    Some(inner.max_tokens)
                },
                temperature: if inner.temperature != 0.0 {
                    Some(inner.temperature)
                } else {
                    None
                },
                version_id: crate::grpc::none_if_empty(&inner.version_id),
            },
        )
        .await
        .map_err(|e| to_status(&e))?;
        Ok(Response::new(pb::CompleteResponse {
            text: out.text,
            stop_reason: out.stop_reason,
            input_tokens: out.input_tokens,
            output_tokens: out.output_tokens,
            provider: out.provider,
            model: out.model,
            invocation_event_id: out.invocation_event_id.unwrap_or_default(),
        }))
    }

    async fn embed(
        &self,
        req: Request<pb::EmbedRequest>,
    ) -> std::result::Result<Response<pb::EmbedResponse>, Status> {
        let ctx = crate::grpc::authenticate(&self.state, &req).await?;
        ctx.require_rw_pub()?;
        let inner = req.into_inner();
        let out = op_embed(
            &self.state,
            &ctx.tenant_id,
            ctx.store.as_ref(),
            &inner.config_name,
            dto::EmbedRequest {
                model: crate::grpc::none_if_empty(&inner.model),
                provider: crate::grpc::none_if_empty(&inner.provider),
                inputs: inner.inputs,
                version_id: crate::grpc::none_if_empty(&inner.version_id),
            },
        )
        .await
        .map_err(|e| to_status(&e))?;
        Ok(Response::new(pb::EmbedResponse {
            vectors: out
                .vectors
                .into_iter()
                .map(|v| pb::embed_response::Vector { values: v })
                .collect(),
            dimensions: out.dimensions as u32,
            cache_hit: out.cache_hit,
            provider: out.provider,
            model: out.model,
            invocation_event_id: out.invocation_event_id.unwrap_or_default(),
        }))
    }
}
