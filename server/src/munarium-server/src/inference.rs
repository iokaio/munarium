// SPDX-License-Identifier: Apache-2.0
//! The optional inference endpoint, server side (ClearView IR-1 to IR-4, I1,
//! I2).
//!
//! Nothing here runs unless an operator does all of the following, in order:
//! sets `MUNARIUM_INFERENCE=enabled`, applies an `openai-compatible` provider
//! configuration with a daily token cap, probes it, binds a collection to it,
//! builds the semantic index through a queued `semantic` build job, and
//! enrolls the collection. Installation alone sends nothing anywhere and
//! changes no index id (COMPAT-T4).
//!
//! - [`InferenceRuntime`] holds the deployment switch, the bounded outbound
//!   concurrency and the per-configuration HTTP clients.
//! - [`Metered`] wraps a client as the coordinator's `InferenceEndpoint`:
//!   one deadline across queueing and attempts, a reservation against the
//!   collection's tenant-owned budget before every physical attempt (zero
//!   outbound calls after a denied admission), an audit row per attempt
//!   naming the initiating caller, and reconciliation once per attempt.
//! - [`Governor`] turns a collection's binding into the coordinator's plan.
//! - The `op_*` functions are the management operations behind the REST
//!   routes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use munarium_api_types::inference as dto;
use munarium_core::budget::{BudgetEstimate, BudgetOutcome, BudgetReservation, BudgetStore};
use munarium_core::inference::{
    EmbedCall, EmbedReply, FailureKind, InferenceEndpoint, InferenceFailure, Purpose, RerankCall,
    RerankReply, Stage, StagePolicy, Usage,
};
use munarium_core::{KernelError, Result};
use munarium_providers::openai_compatible::{
    self, check_discovery, OpenAiCompatibleEndpoint, Placement, PlacementMode,
};
use munarium_providers::{parse_provider_config, ProviderConfigDoc};
use munarium_retrieval::inference::{
    rerank_identity, CollectionPlan, EmbeddingStage, InferenceGovernor, InferenceRecorder,
    RerankStage, ShadowComparison, ShadowPlan, StageEndpoint, StageEvent,
};
use munarium_retrieval::{SemanticSpace, SEMANTIC_DIMS};
use sqlx::{PgPool, Row};
use tokio::sync::Semaphore;

use crate::error::ApiError;
use crate::state::AppState;

type ApiResult<T> = std::result::Result<T, ApiError>;

/// Sentinel prefix promoting an `InvalidInput` into the
/// `inference-not-qualified` problem type (409) at the transport boundary.
pub const NOT_QUALIFIED_PREFIX: &str = "inference-not-qualified: ";

fn not_qualified(detail: impl std::fmt::Display) -> KernelError {
    KernelError::InvalidInput(format!("{NOT_QUALIFIED_PREFIX}{detail}"))
}

/// Defaults for a binding.
const DEFAULT_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_MAX_CANDIDATES: u64 = 20;
const DEFAULT_MAX_DOCUMENT_BYTES: u64 = 8_192;
/// The deadline of one build batch and of a probe.
const BUILD_BATCH_TIMEOUT: Duration = Duration::from_secs(60);
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
/// Instructions are identity, not prompts: short by construction.
const MAX_INSTRUCTION_CHARS: usize = 1_024;

// ---------------------------------------------------------------------------
// settings and runtime
// ---------------------------------------------------------------------------

/// `MUNARIUM_INFERENCE` and `MUNARIUM_INFERENCE_CONCURRENCY`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InferenceSettings {
    pub enabled: bool,
    pub concurrency: usize,
}

impl Default for InferenceSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            concurrency: 8,
        }
    }
}

impl InferenceSettings {
    /// Read once at startup. Unset is disabled; an unknown value is refused
    /// rather than silently treated as either.
    pub fn from_env() -> std::result::Result<Self, String> {
        Self::parse(
            std::env::var("MUNARIUM_INFERENCE").ok().as_deref(),
            std::env::var("MUNARIUM_INFERENCE_CONCURRENCY")
                .ok()
                .as_deref(),
        )
    }

    fn parse(switch: Option<&str>, concurrency: Option<&str>) -> std::result::Result<Self, String> {
        let enabled = match switch.map(str::trim) {
            None | Some("") | Some("disabled") => false,
            Some("enabled") => true,
            Some(_) => return Err("MUNARIUM_INFERENCE must be enabled or disabled".into()),
        };
        let concurrency = match concurrency.map(str::trim) {
            None | Some("") => 8,
            Some(raw) => match raw.parse::<usize>() {
                Ok(n) if (1..=1024).contains(&n) => n,
                _ => return Err("MUNARIUM_INFERENCE_CONCURRENCY must be 1-1024".into()),
            },
        };
        Ok(Self {
            enabled,
            concurrency,
        })
    }
}

/// The deployment's inference capability. One per process.
pub struct InferenceRuntime {
    enabled: AtomicBool,
    /// Outbound inference calls in flight in this process. A call waits for
    /// a permit no longer than its own deadline.
    permits: Arc<Semaphore>,
    pool: Option<PgPool>,
    budgets: Arc<dyn BudgetStore>,
    /// (tenant, configuration, text hash) -> client. A changed configuration
    /// text is a new key, so no client outlives the text it was built from.
    clients: std::sync::Mutex<HashMap<(String, String, String), Arc<OpenAiCompatibleEndpoint>>>,
}

impl InferenceRuntime {
    pub fn new(
        settings: InferenceSettings,
        pool: Option<PgPool>,
        budgets: Arc<dyn BudgetStore>,
    ) -> Self {
        Self {
            enabled: AtomicBool::new(settings.enabled),
            permits: Arc::new(Semaphore::new(settings.concurrency)),
            pool,
            budgets,
            clients: Default::default(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    /// Tests switch the deployment capability without process environment.
    #[cfg(test)]
    pub(crate) fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::SeqCst);
    }

    fn pool(&self) -> Result<&PgPool> {
        self.pool.as_ref().ok_or_else(|| {
            KernelError::InvalidInput("inference requires the postgres store".into())
        })
    }

    fn client(
        &self,
        tenant: &str,
        config: &LoadedConfig,
    ) -> std::result::Result<Arc<OpenAiCompatibleEndpoint>, InferenceFailure> {
        let key = (
            tenant.to_string(),
            config.doc.metadata.name.clone(),
            config.hash.clone(),
        );
        let mut clients = self
            .clients
            .lock()
            .map_err(|_| InferenceFailure::new(FailureKind::Unavailable, "client_cache"))?;
        if let Some(c) = clients.get(&key) {
            return Ok(c.clone());
        }
        let client = Arc::new(
            OpenAiCompatibleEndpoint::new(&config.doc.spec)
                .map_err(|_| InferenceFailure::new(FailureKind::Unconfigured, "config_invalid"))?,
        );
        // Bounded by the number of configurations; drop superseded texts.
        clients.retain(|(t, n, _), _| !(t == tenant && *n == config.doc.metadata.name));
        clients.insert(key, client.clone());
        Ok(client)
    }

    /// The bound configuration, its client, and a current passing probe, or
    /// the stage state that stops it. Never calls the endpoint.
    ///
    /// SC2: the configuration's declared placement is checked against the
    /// tenant and the binding's `required` placement first. A refusal is the
    /// stage state `unconfigured` with a `placement_*` reason, so no client
    /// is handed out and nothing is sent on any path (serving, shadow, build
    /// or retry), whatever the probe says.
    async fn resolve(
        &self,
        tenant: &str,
        config_name: &str,
        required: Option<&dto::InferencePlacementRequirement>,
    ) -> Result<Resolution> {
        let config = match load_config(self.pool()?, tenant, config_name).await? {
            Some(c) => c,
            None => {
                return Ok(Resolution::failed(
                    FailureKind::Unconfigured,
                    "config_missing",
                ))
            }
        };
        let config = match config {
            Ok(c) => c,
            Err(reason) => return Ok(Resolution::failed(FailureKind::Unconfigured, reason)),
        };
        if let Err(reason) = placement_check(tenant, declared_placement(&config), required) {
            return Ok(Resolution::failed(FailureKind::Unconfigured, reason));
        }
        let client = match self.client(tenant, &config) {
            Ok(c) => c,
            Err(f) => return Ok(Resolution::failed(f.kind, f.reason)),
        };
        let fingerprint = client.fingerprint();
        let ready = if !self.enabled() {
            Err(InferenceFailure::new(
                FailureKind::Disabled,
                "deployment_disabled",
            ))
        } else {
            let probe: Option<bool> = sqlx::query_scalar(
                "SELECT passed FROM inference_probes
                  WHERE tenant_id = $1 AND provider_config = $2 AND config_hash = $3
                  ORDER BY probed_at DESC, probe_id DESC LIMIT 1",
            )
            .bind(tenant)
            .bind(config_name)
            .bind(&config.hash)
            .fetch_optional(self.pool()?)
            .await
            .map_err(storage)?;
            match probe {
                Some(true) => Ok(client),
                Some(false) => Err(InferenceFailure::new(
                    FailureKind::ProbeFailed,
                    "probe_failed",
                )),
                // Never probed, or the configuration text changed since:
                // drift expires the probe.
                None => Err(InferenceFailure::new(
                    FailureKind::ProbeFailed,
                    "no_current_probe",
                )),
            }
        };
        Ok(Resolution {
            config: Some(config),
            fingerprint: Some(fingerprint),
            ready,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn metered(
        self: &Arc<Self>,
        tenant: &str,
        config: &LoadedConfig,
        client: Arc<OpenAiCompatibleEndpoint>,
        purpose: Purpose,
        caller: &str,
        collection_id: Option<&str>,
        job_id: Option<&str>,
    ) -> Arc<dyn InferenceEndpoint> {
        let profile = config.doc.spec.inference.clone().unwrap_or_default();
        Arc::new(Metered {
            runtime: self.clone(),
            tenant: tenant.to_string(),
            config: config.doc.metadata.name.clone(),
            budgets: config.doc.spec.budgets.clone(),
            max_attempts: profile.max_attempts(),
            purpose,
            caller: caller.to_string(),
            collection_ids: collection_id
                .map(|c| vec![c.to_string()])
                .unwrap_or_default(),
            job_id: job_id.map(str::to_string),
            inner: client,
        })
    }
}

fn storage(e: sqlx::Error) -> KernelError {
    KernelError::Storage(e.to_string())
}

/// A provider configuration row, parsed, with the hash of its text.
#[derive(Clone)]
struct LoadedConfig {
    doc: ProviderConfigDoc,
    hash: String,
}

/// `None`: no such configuration. `Some(Err)`: present but unusable here.
async fn load_config(
    pool: &PgPool,
    tenant: &str,
    name: &str,
) -> Result<Option<std::result::Result<LoadedConfig, &'static str>>> {
    let yaml: Option<String> =
        sqlx::query_scalar("SELECT yaml FROM provider_configs WHERE tenant_id = $1 AND name = $2")
            .bind(tenant)
            .bind(name)
            .fetch_optional(pool)
            .await
            .map_err(storage)?;
    Ok(yaml.map(|yaml| {
        let doc = parse_provider_config(&yaml).map_err(|_| "config_invalid")?;
        if doc.spec.provider != openai_compatible::KIND {
            return Err("config_kind");
        }
        Ok(LoadedConfig {
            doc,
            hash: crate::state::request_hash(yaml.as_bytes()),
        })
    }))
}

struct Resolution {
    config: Option<LoadedConfig>,
    fingerprint: Option<String>,
    /// The client when the stage can run; otherwise why not.
    ready: std::result::Result<Arc<OpenAiCompatibleEndpoint>, InferenceFailure>,
}

impl Resolution {
    fn failed(kind: FailureKind, reason: &'static str) -> Self {
        Self {
            config: None,
            fingerprint: None,
            ready: Err(InferenceFailure::new(kind, reason)),
        }
    }
}

// ---------------------------------------------------------------------------
// placement (SC2)
// ---------------------------------------------------------------------------

/// The configuration's declared `inference.placement`, if any.
fn declared_placement(config: &LoadedConfig) -> Option<&Placement> {
    config
        .doc
        .spec
        .inference
        .as_ref()
        .and_then(|p| p.placement.as_ref())
}

/// SC2: may `tenant`'s content reach the endpoint whose declared placement
/// is `declared`, under the collection's `required` placement? `Err` names
/// a fixed reason code.
///
/// - A shared pool must list the tenant explicitly
///   (`placement_tenant_not_covered`), whether or not the binding has a
///   requirement: sharing is never implied.
/// - With no requirement, nothing more is checked. With neither a
///   requirement nor a declaration this is the Phase 6 behavior.
/// - A requirement cannot be verified against an undeclared endpoint
///   (`placement_undeclared`).
/// - `dedicated` refuses a shared pool (`placement_dedicated_required`);
///   `shared` accepts either.
/// - Every required processing rule must be declared with the same value
///   (`placement_processing_incompatible`).
pub(crate) fn placement_check(
    tenant: &str,
    declared: Option<&Placement>,
    required: Option<&dto::InferencePlacementRequirement>,
) -> std::result::Result<(), &'static str> {
    if let Some(d) = declared {
        if d.mode == PlacementMode::Shared && !d.tenants.iter().any(|t| t == tenant) {
            return Err("placement_tenant_not_covered");
        }
    }
    let Some(required) = required else {
        return Ok(());
    };
    let Some(declared) = declared else {
        return Err("placement_undeclared");
    };
    if required.isolation == PlacementMode::Dedicated.as_str()
        && declared.mode != PlacementMode::Dedicated
    {
        return Err("placement_dedicated_required");
    }
    if required
        .processing
        .iter()
        .any(|(k, v)| declared.processing.get(k) != Some(v))
    {
        return Err("placement_processing_incompatible");
    }
    Ok(())
}

/// The operator-facing sentence for a placement reason code.
fn placement_refusal(reason: &str) -> String {
    let why = match reason {
        "placement_tenant_not_covered" => {
            "the configuration's shared placement does not list this tenant"
        }
        "placement_undeclared" => {
            "the binding requires a placement but the configuration declares none"
        }
        "placement_dedicated_required" => {
            "the binding requires a dedicated endpoint but the configuration's placement is shared"
        }
        "placement_processing_incompatible" => {
            "the configuration's processing rules do not match the binding's placement requirement"
        }
        _ => "the placement check failed",
    };
    format!("{why} ({reason})")
}

/// Validate a requested placement requirement and normalize it.
fn checked_requirement(
    req: &dto::InferencePlacementRequirement,
) -> Result<dto::InferencePlacementRequirement> {
    if req.isolation != PlacementMode::Dedicated.as_str()
        && req.isolation != PlacementMode::Shared.as_str()
    {
        return Err(KernelError::InvalidInput(
            "placement.isolation must be dedicated or shared".into(),
        ));
    }
    openai_compatible::validate_processing(&req.processing).map_err(KernelError::InvalidInput)?;
    Ok(req.clone())
}

/// The requirement a bind records (SC2). An explicit request wins. An
/// omitted one keeps the binding's current requirement, so a rebind never
/// loosens placement silently; a first binding records the configuration's
/// declared placement (mode and processing rules), so a later change of
/// that declaration, such as a dedicated pool replaced by a shared one, is
/// refused until the operator binds again with an explicit requirement.
/// With no declaration and no requirement, `None`: Phase 6.
fn effective_requirement(
    requested: Option<&dto::InferencePlacementRequirement>,
    current: Option<&Binding>,
    declared: Option<&Placement>,
) -> Result<Option<dto::InferencePlacementRequirement>> {
    if let Some(r) = requested {
        return checked_requirement(r).map(Some);
    }
    if let Some(r) = current.and_then(|b| b.placement.clone()) {
        return Ok(Some(r));
    }
    if current.is_some() {
        // An existing Phase 6 binding stays unpinned until an explicit
        // requirement is given; the tenant coverage rule still applies.
        return Ok(None);
    }
    Ok(declared.map(|d| dto::InferencePlacementRequirement {
        isolation: d.mode.as_str().to_string(),
        processing: d.processing.clone(),
    }))
}

// ---------------------------------------------------------------------------
// metering (I2)
// ---------------------------------------------------------------------------

/// Failures that provably happened before a request left Server: the
/// reservation is released, never charged.
fn never_sent(f: &InferenceFailure) -> bool {
    matches!(
        f.reason,
        "credential_unavailable" | "request_bytes" | "deadline" | "unencodable_request"
    )
}

/// The coordinator's endpoint for one purpose, payer and caller.
struct Metered {
    runtime: Arc<InferenceRuntime>,
    tenant: String,
    config: String,
    budgets: munarium_providers::Budgets,
    max_attempts: u32,
    purpose: Purpose,
    caller: String,
    collection_ids: Vec<String>,
    job_id: Option<String>,
    inner: Arc<OpenAiCompatibleEndpoint>,
}

impl Metered {
    /// Reserve one attempt's estimate against the collection's tenant-owned
    /// budget. Shadow work reserves only from the shadow allocation; every
    /// other purpose from the serving share. A denial dispatches nothing.
    async fn reserve(
        &self,
        units: u64,
    ) -> std::result::Result<Option<BudgetReservation>, InferenceFailure> {
        let (scope, limit) = match self.purpose {
            Purpose::Shadow => match self.budgets.shadow_daily_tokens {
                Some(limit) => ("shadow", Some(limit)),
                None => {
                    return Err(InferenceFailure::new(
                        FailureKind::BudgetExhausted,
                        "shadow_not_authorized",
                    ))
                }
            },
            _ => ("all", self.budgets.serving_daily_limit()),
        };
        match self
            .runtime
            .budgets
            .reserve_estimated(
                &self.tenant,
                &self.config,
                scope,
                BudgetEstimate {
                    units,
                    revision: Some("inference-attempt-v1"),
                },
                limit,
            )
            .await
        {
            Ok(BudgetOutcome::Unlimited) => Ok(None),
            Ok(BudgetOutcome::Granted(r)) => Ok(Some(r)),
            Ok(BudgetOutcome::Exhausted { .. }) => Err(InferenceFailure::new(
                FailureKind::BudgetExhausted,
                if self.purpose == Purpose::Shadow {
                    "shadow_allocation"
                } else {
                    "daily_cap"
                },
            )),
            Err(_) => Err(InferenceFailure::new(
                FailureKind::Unavailable,
                "budget_store",
            )),
        }
    }

    async fn record_start(
        &self,
        stage: Stage,
        model: &str,
        estimate: u64,
        reservation: Option<&BudgetReservation>,
    ) -> std::result::Result<String, InferenceFailure> {
        let attempt = uuid::Uuid::new_v4().simple().to_string();
        let pool = self
            .runtime
            .pool()
            .map_err(|_| InferenceFailure::new(FailureKind::Unavailable, "audit_store"))?;
        sqlx::query(
            "INSERT INTO inference_attempts
                 (tenant_id, attempt_id, purpose, stage, provider_config, model, caller,
                  collection_ids, job_id, reservation_id, estimate_units)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
        )
        .bind(&self.tenant)
        .bind(&attempt)
        .bind(self.purpose.as_str())
        .bind(stage.as_str())
        .bind(&self.config)
        .bind(model)
        .bind(&self.caller)
        .bind(&self.collection_ids)
        .bind(&self.job_id)
        .bind(reservation.map(|r| r.id.clone()))
        .bind(i64::try_from(estimate).unwrap_or(i64::MAX))
        .execute(pool)
        .await
        .map_err(|_| InferenceFailure::new(FailureKind::Unavailable, "audit_store"))?;
        Ok(attempt)
    }

    /// Reconcile one attempt exactly once: settle what may have run, release
    /// what never left, and close the audit row.
    async fn finish(
        &self,
        attempt: &str,
        estimate: u64,
        reservation: Option<BudgetReservation>,
        outcome: std::result::Result<Usage, InferenceFailure>,
    ) {
        let (accounted, source, label, reason) = match &outcome {
            Ok(usage) => match usage.input_tokens {
                Some(tokens) => (Some(tokens), Some("provider_reported"), "succeeded", None),
                None => (Some(estimate), Some("missing"), "succeeded", None),
            },
            Err(f) if never_sent(f) => (Some(0), None, "not_sent", Some(f.reason)),
            // Possibly executed: spent at the estimate, never refunded.
            Err(f) => (Some(estimate), Some("missing"), "failed", Some(f.reason)),
        };
        if let Some(r) = reservation {
            let done = match &outcome {
                Err(f) if never_sent(f) => self.runtime.budgets.release(&r).await,
                _ => self.runtime.budgets.settle(&r, accounted).await,
            };
            if done.is_err() {
                tracing::warn!("inference attempt reconciliation failed; estimate retained");
            }
        }
        if let Ok(pool) = self.runtime.pool() {
            let updated = sqlx::query(
                "UPDATE inference_attempts
                    SET outcome = $3, reason = $4, accounted_units = $5, usage_source = $6,
                        finished_at = now()
                  WHERE tenant_id = $1 AND attempt_id = $2 AND finished_at IS NULL",
            )
            .bind(&self.tenant)
            .bind(attempt)
            .bind(label)
            .bind(reason)
            .bind(accounted.map(|u| i64::try_from(u).unwrap_or(i64::MAX)))
            .bind(source)
            .execute(pool)
            .await;
            if updated.is_err() {
                tracing::warn!("inference attempt outcome not persisted");
            }
        }
    }

    /// Run `op` as up to `max_attempts` physical attempts inside one
    /// deadline. Each attempt waits for a permit within the deadline,
    /// reserves, records, dispatches and reconciles.
    async fn attempts<T, F, Fut>(
        &self,
        stage: Stage,
        model: &str,
        estimate: u64,
        deadline: Instant,
        usage_of: fn(&T) -> Usage,
        op: F,
    ) -> std::result::Result<T, InferenceFailure>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = std::result::Result<T, InferenceFailure>>,
    {
        if !self.runtime.enabled() {
            return Err(InferenceFailure::new(
                FailureKind::Disabled,
                "deployment_disabled",
            ));
        }
        let mut attempt = 0;
        loop {
            attempt += 1;
            if Instant::now() >= deadline {
                return Err(InferenceFailure::new(
                    FailureKind::DeadlineExceeded,
                    "deadline",
                ));
            }
            let permit = tokio::time::timeout_at(
                deadline.into(),
                self.runtime.permits.clone().acquire_owned(),
            )
            .await
            .map_err(|_| InferenceFailure::new(FailureKind::DeadlineExceeded, "queue_deadline"))?
            .map_err(|_| InferenceFailure::new(FailureKind::Unavailable, "closed"))?;
            let reservation = self.reserve(estimate).await?;
            let id = match self
                .record_start(stage, model, estimate, reservation.as_ref())
                .await
            {
                Ok(id) => id,
                Err(f) => {
                    // No durable audit row: nothing is sent, and the
                    // reservation is refunded.
                    if let Some(r) = reservation {
                        let _ = self.runtime.budgets.release(&r).await;
                    }
                    return Err(f);
                }
            };
            let result = op().await;
            drop(permit);
            self.finish(
                &id,
                estimate,
                reservation,
                result.as_ref().map(usage_of).map_err(|f| *f),
            )
            .await;
            match result {
                Ok(v) => return Ok(v),
                Err(f) if f.retryable() && attempt < self.max_attempts => continue,
                Err(f) => return Err(f),
            }
        }
    }
}

fn estimate_units(bytes: usize) -> u64 {
    (bytes as u64).div_ceil(4).saturating_add(32)
}

#[async_trait::async_trait]
impl InferenceEndpoint for Metered {
    async fn embed(
        &self,
        call: EmbedCall<'_>,
    ) -> std::result::Result<EmbedReply, InferenceFailure> {
        let estimate = estimate_units(OpenAiCompatibleEndpoint::embed_request_bytes(
            call.model,
            call.inputs,
        ));
        self.attempts(
            Stage::Embedding,
            call.model,
            estimate,
            call.deadline,
            |r: &EmbedReply| r.usage,
            || self.inner.embed(call),
        )
        .await
    }

    async fn rerank(
        &self,
        call: RerankCall<'_>,
    ) -> std::result::Result<RerankReply, InferenceFailure> {
        let estimate = estimate_units(OpenAiCompatibleEndpoint::rerank_request_bytes(
            call.model,
            call.query,
            call.documents,
        ));
        self.attempts(
            Stage::Reranking,
            call.model,
            estimate,
            call.deadline,
            |r: &RerankReply| r.usage,
            || self.inner.rerank(call),
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// bindings
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
struct Binding {
    collection_id: String,
    revision: u64,
    state: String,
    provider_config: String,
    embedding: Option<dto::InferenceEmbeddingSettings>,
    rerank: Option<dto::InferenceRerankSettings>,
    shadow: dto::InferenceShadowSettings,
    timeout_ms: u64,
    semantic_index_id: Option<String>,
    prior_index_id: Option<String>,
    /// SC2: the placement requirement; `None` is Phase 6 (migration 0081).
    placement: Option<dto::InferencePlacementRequirement>,
}

fn json_field<T: serde::de::DeserializeOwned>(v: Option<serde_json::Value>) -> Result<Option<T>> {
    v.filter(|v| !v.is_null())
        .map(|v| {
            serde_json::from_value(v)
                .map_err(|_| KernelError::Storage("inference binding row is unreadable".into()))
        })
        .transpose()
}

fn binding_from_row(row: &sqlx::postgres::PgRow) -> Result<Binding> {
    Ok(Binding {
        collection_id: row.get("collection_id"),
        revision: row.get::<i64, _>("revision").max(0) as u64,
        state: row.get("state"),
        provider_config: row.get("provider_config"),
        embedding: json_field(row.get("embedding"))?,
        rerank: json_field(row.get("rerank"))?,
        shadow: json_field(Some(row.get("shadow")))?.unwrap_or_default(),
        timeout_ms: row.get::<i32, _>("timeout_ms").max(0) as u64,
        semantic_index_id: row.get("semantic_index_id"),
        prior_index_id: row.get("prior_index_id"),
        placement: json_field(row.get("placement"))?,
    })
}

const BINDING_COLUMNS: &str =
    "collection_id, revision, state, provider_config, embedding, rerank, \
     shadow, timeout_ms, semantic_index_id, prior_index_id, placement";

async fn load_binding(
    conn: impl sqlx::PgExecutor<'_>,
    tenant: &str,
    collection_id: &str,
    lock: bool,
) -> Result<Option<Binding>> {
    let sql = format!(
        "SELECT {BINDING_COLUMNS} FROM collection_inference_bindings
          WHERE tenant_id = $1 AND collection_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    sqlx::query(&sql)
        .bind(tenant)
        .bind(collection_id)
        .fetch_optional(conn)
        .await
        .map_err(storage)?
        .map(|r| binding_from_row(&r))
        .transpose()
}

/// Write a binding at `revision` and its history row, in `tx`.
async fn write_binding(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant: &str,
    b: &Binding,
    action: &str,
    actor: &str,
) -> Result<()> {
    let embedding = crate::error::to_json(&b.embedding, "embedding settings")?;
    let rerank = crate::error::to_json(&b.rerank, "rerank settings")?;
    let shadow = crate::error::to_json(&b.shadow, "shadow settings")?;
    let placement = crate::error::to_json(&b.placement, "placement requirement")?;
    sqlx::query(
        "INSERT INTO collection_inference_bindings
             (tenant_id, collection_id, revision, state, provider_config, embedding, rerank,
              shadow, timeout_ms, semantic_index_id, prior_index_id, updated_by, updated_at,
              placement)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12, now(), $13)
         ON CONFLICT (tenant_id, collection_id) DO UPDATE SET
             revision = EXCLUDED.revision, state = EXCLUDED.state,
             provider_config = EXCLUDED.provider_config, embedding = EXCLUDED.embedding,
             rerank = EXCLUDED.rerank, shadow = EXCLUDED.shadow,
             timeout_ms = EXCLUDED.timeout_ms, semantic_index_id = EXCLUDED.semantic_index_id,
             prior_index_id = EXCLUDED.prior_index_id, updated_by = EXCLUDED.updated_by,
             updated_at = now(), placement = EXCLUDED.placement",
    )
    .bind(tenant)
    .bind(&b.collection_id)
    .bind(b.revision as i64)
    .bind(&b.state)
    .bind(&b.provider_config)
    .bind(&embedding)
    .bind(&rerank)
    .bind(&shadow)
    .bind(b.timeout_ms as i32)
    .bind(&b.semantic_index_id)
    .bind(&b.prior_index_id)
    .bind(actor)
    .bind(&placement)
    .execute(&mut **tx)
    .await
    .map_err(storage)?;
    let mut snapshot = serde_json::json!({
        "provider_config": b.provider_config,
        "embedding": embedding,
        "rerank": rerank,
        "shadow": shadow,
        "timeout_ms": b.timeout_ms,
        "semantic_index_id": b.semantic_index_id,
        "prior_index_id": b.prior_index_id,
    });
    // A Phase 6 binding's history snapshot keeps its Phase 6 members.
    if b.placement.is_some() {
        snapshot["placement"] = placement;
    }
    sqlx::query(
        "INSERT INTO collection_inference_binding_history
             (tenant_id, collection_id, revision, action, state, binding, actor)
         VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(tenant)
    .bind(&b.collection_id)
    .bind(b.revision as i64)
    .bind(action)
    .bind(&b.state)
    .bind(&snapshot)
    .bind(actor)
    .execute(&mut **tx)
    .await
    .map_err(storage)?;
    Ok(())
}

/// The semantic space a binding's embedding stage defines on a configuration.
fn space_for(
    settings: &dto::InferenceEmbeddingSettings,
    config: &LoadedConfig,
    fingerprint: &str,
) -> std::result::Result<SemanticSpace, InferenceFailure> {
    if !config.doc.spec.models.embed.contains(&settings.model) {
        return Err(InferenceFailure::new(
            FailureKind::Unconfigured,
            "model_not_configured",
        ));
    }
    let dims = config
        .doc
        .spec
        .inference
        .as_ref()
        .and_then(|p| p.embed_dimensions)
        .ok_or(InferenceFailure::new(
            FailureKind::Unconfigured,
            "dimension_unset",
        ))?;
    SemanticSpace::from_descriptor(
        serde_json::json!({
            "provider_config": config.doc.metadata.name,
            "endpoint": fingerprint,
            "model": settings.model,
            "dims": dims,
            "query_instruction": settings.query_instruction.clone().unwrap_or_default(),
            "passage_instruction": settings.passage_instruction.clone().unwrap_or_default(),
            "normalize": "l2",
            "truncation": "refuse",
        }),
        dims,
    )
    .map_err(|_| InferenceFailure::new(FailureKind::Unconfigured, "dimension_unsupported"))
}

fn policy_of(raw: &Option<String>) -> StagePolicy {
    // Stored values were validated on write; an unreadable one is `require`.
    StagePolicy::parse(raw.as_deref()).unwrap_or_default()
}

fn unresolved_space() -> SemanticSpace {
    SemanticSpace {
        identity: "unresolved".into(),
        dimensions: 0,
        descriptor: serde_json::Value::Null,
    }
}

// ---------------------------------------------------------------------------
// governor and recorder
// ---------------------------------------------------------------------------

/// Supplies enrolled collections' plans to the coordinator, per tenant.
pub struct Governor {
    pub runtime: Arc<InferenceRuntime>,
    pub tenant: String,
}

struct StagePieces {
    resolution: Resolution,
}

impl StagePieces {
    fn endpoint(
        &self,
        g: &Governor,
        purpose: Purpose,
        caller: &str,
        collection_id: &str,
    ) -> StageEndpoint {
        match (&self.resolution.ready, &self.resolution.config) {
            (Ok(client), Some(config)) => Ok(g.runtime.metered(
                &g.tenant,
                config,
                client.clone(),
                purpose,
                caller,
                Some(collection_id),
                None,
            )),
            (Err(f), _) => Err(*f),
            (Ok(_), None) => Err(InferenceFailure::new(
                FailureKind::Unconfigured,
                "config_missing",
            )),
        }
    }

    fn embedding(
        &self,
        g: &Governor,
        b: &Binding,
        purpose: Purpose,
        caller: &str,
    ) -> Option<EmbeddingStage> {
        let settings = b.embedding.as_ref()?;
        let space = match (&self.resolution.config, &self.resolution.fingerprint) {
            (Some(config), Some(fp)) => space_for(settings, config, fp),
            _ => Err(InferenceFailure::new(
                FailureKind::Unconfigured,
                "config_missing",
            )),
        };
        let (endpoint, space) = match space {
            Ok(space) => (self.endpoint(g, purpose, caller, &b.collection_id), space),
            Err(f) => (Err(f), unresolved_space()),
        };
        // A missing/disabled endpoint reports its own state first.
        let endpoint = match (&self.resolution.ready, endpoint) {
            (Err(f), _) => Err(*f),
            (Ok(_), e) => e,
        };
        Some(EmbeddingStage {
            policy: policy_of(&settings.policy),
            endpoint,
            model: settings.model.clone(),
            space,
            query_instruction: settings.query_instruction.clone().unwrap_or_default(),
            passage_instruction: settings.passage_instruction.clone().unwrap_or_default(),
        })
    }

    fn rerank(
        &self,
        g: &Governor,
        b: &Binding,
        purpose: Purpose,
        caller: &str,
    ) -> Option<RerankStage> {
        let settings = b.rerank.as_ref()?;
        let listed = self
            .resolution
            .config
            .as_ref()
            .is_some_and(|c| c.doc.spec.models.rerank.contains(&settings.model));
        let endpoint = match self.endpoint(g, purpose, caller, &b.collection_id) {
            Ok(_) if !listed => Err(InferenceFailure::new(
                FailureKind::Unconfigured,
                "model_not_configured",
            )),
            e => e,
        };
        Some(RerankStage {
            policy: policy_of(&settings.policy),
            endpoint,
            model: settings.model.clone(),
            max_candidates: settings.max_candidates.unwrap_or(DEFAULT_MAX_CANDIDATES) as usize,
            max_document_bytes: settings
                .max_document_bytes
                .unwrap_or(DEFAULT_MAX_DOCUMENT_BYTES) as usize,
            instruction: settings.instruction.clone(),
        })
    }
}

#[async_trait::async_trait]
impl InferenceGovernor for Governor {
    async fn plan(&self, collection_id: &str, caller: &str) -> Result<Option<CollectionPlan>> {
        let pool = self.runtime.pool()?;
        let Some(b) = load_binding(pool, &self.tenant, collection_id, false).await? else {
            return Ok(None);
        };
        let serving = b.state == "enrolled";
        let shadowing = self.runtime.enabled() && (b.shadow.rerank || b.shadow.semantic_index);
        if !serving && !shadowing {
            return Ok(None);
        }
        let pieces = StagePieces {
            resolution: self
                .runtime
                .resolve(&self.tenant, &b.provider_config, b.placement.as_ref())
                .await?,
        };
        let mut shadow = ShadowPlan::default();
        // Shadow work runs only against a ready, probed endpoint: a broken
        // endpoint is a serving concern, not a stream of shadow failures.
        if shadowing && pieces.resolution.ready.is_ok() {
            if b.shadow.rerank {
                shadow.rerank = pieces.rerank(self, &b, Purpose::Shadow, caller);
            }
            if b.shadow.semantic_index {
                if let (Some(index), Some(stage)) = (
                    b.semantic_index_id.clone(),
                    pieces.embedding(self, &b, Purpose::Shadow, caller),
                ) {
                    shadow.semantic_index = Some((index, stage));
                }
            }
        }
        Ok(Some(CollectionPlan {
            policy_revision: b.revision,
            timeout: Duration::from_millis(b.timeout_ms),
            embedding: if serving {
                pieces.embedding(self, &b, Purpose::Serving, caller)
            } else {
                None
            },
            rerank: if serving {
                pieces.rerank(self, &b, Purpose::Serving, caller)
            } else {
                None
            },
            shadow,
            recorder: Arc::new(Recorder {
                pool: pool.clone(),
                tenant: self.tenant.clone(),
                caller: caller.to_string(),
            }),
        }))
    }
}

/// Writes stage events and shadow comparisons. A failed write is logged; it
/// never changes a served result.
struct Recorder {
    pool: PgPool,
    tenant: String,
    caller: String,
}

#[async_trait::async_trait]
impl InferenceRecorder for Recorder {
    async fn stage_event(&self, e: StageEvent) {
        let written = sqlx::query(
            "INSERT INTO inference_stage_events
                 (tenant_id, event_id, collection_id, stage, policy, policy_revision, outcome,
                  state, reason, caller)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
        )
        .bind(&self.tenant)
        .bind(uuid::Uuid::new_v4().simple().to_string())
        .bind(&e.collection_id)
        .bind(e.stage.as_str())
        .bind(e.policy.as_str())
        .bind(e.policy_revision as i64)
        .bind(e.outcome)
        .bind(e.failure.kind.state())
        .bind(e.failure.reason)
        .bind(&self.caller)
        .execute(&self.pool)
        .await;
        if written.is_err() {
            tracing::warn!("inference stage event not persisted");
        }
    }

    async fn shadow(&self, c: ShadowComparison) {
        let written = sqlx::query(
            "INSERT INTO inference_shadow_comparisons
                 (tenant_id, comparison_id, collection_id, kind, query_fingerprint,
                  served_index_id, candidate_index_id, outcome, reason, k, overlap_at_k,
                  top1_agrees, moved_positions, caller)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)",
        )
        .bind(&self.tenant)
        .bind(uuid::Uuid::new_v4().simple().to_string())
        .bind(&c.collection_id)
        .bind(c.kind)
        .bind(&c.query_fingerprint)
        .bind(&c.served_index_id)
        .bind(&c.candidate_index_id)
        .bind(c.outcome)
        .bind(c.reason)
        .bind(c.k as i32)
        .bind(c.overlap_at_k.map(|v| v as i32))
        .bind(c.top1_agrees)
        .bind(c.moved_positions.map(|v| v as i32))
        .bind(&self.caller)
        .execute(&self.pool)
        .await;
        if written.is_err() {
            tracing::warn!("inference shadow comparison not persisted");
        }
    }
}

// ---------------------------------------------------------------------------
// operations
// ---------------------------------------------------------------------------

fn caller_of(uid: Option<axum::Extension<crate::middleware::Uid>>, role: &str) -> String {
    uid.map(|axum::Extension(u)| u.0)
        .unwrap_or_else(|| format!("api:{role}"))
}

async fn collection_id(state: &AppState, tenant: &str, id_or_name: &str) -> Result<String> {
    let retrieval = state.retrieval_for(tenant)?;
    let info = match retrieval.collection_by_id(id_or_name).await {
        Ok(info) => info,
        Err(KernelError::NotFound { .. }) => retrieval.collection_by_name(id_or_name).await?,
        Err(e) => return Err(e),
    };
    Ok(info.id)
}

/// IR-1: probe a configuration's declared models and record the result.
pub async fn op_probe(
    state: &AppState,
    tenant: &str,
    caller: &str,
    name: &str,
) -> Result<dto::InferenceProbeResponse> {
    let runtime = &state.inference;
    let pool = runtime.pool()?;
    if !runtime.enabled() {
        return Err(not_qualified(
            "inference is disabled on this deployment (MUNARIUM_INFERENCE)",
        ));
    }
    let config = match load_config(pool, tenant, name).await? {
        None => {
            return Err(KernelError::NotFound {
                kind: "provider-config",
                id: name.to_string(),
            })
        }
        Some(Err(_)) => {
            return Err(KernelError::InvalidInput(
                "the inference probe requires an openai-compatible configuration".into(),
            ))
        }
        Some(Ok(c)) => c,
    };
    // SC2: a shared pool that does not list this tenant receives nothing,
    // not even the fixed synthetic probe inputs.
    if let Err(reason) = placement_check(tenant, declared_placement(&config), None) {
        return Err(not_qualified(placement_refusal(reason)));
    }
    let client = runtime
        .client(tenant, &config)
        .map_err(|f| KernelError::InvalidInput(format!("configuration unusable: {f}")))?;
    let spec = &config.doc.spec;
    let profile = spec.inference.clone().unwrap_or_default();
    let dims = profile.embed_dimensions;
    let embed_model = spec.models.embed.first().cloned();
    let rerank_model = spec.models.rerank.first().cloned();
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let mut checks = Vec::new();
    let mut check = |name: &str, outcome: std::result::Result<(), &'static str>| {
        checks.push(dto::InferenceProbeCheckDto {
            check: name.into(),
            passed: outcome.is_ok(),
            reason: outcome.err().map(str::to_string),
        });
    };

    match client.models(deadline).await {
        Ok(models) => {
            let mut outcome = Ok(());
            if let Some(m) = &embed_model {
                outcome = outcome.and(check_discovery(
                    &models,
                    m,
                    "embedding",
                    dims,
                    profile.allow_test_backend,
                ));
            }
            if let Some(m) = &rerank_model {
                outcome = outcome.and(check_discovery(
                    &models,
                    m,
                    "reranking",
                    None,
                    profile.allow_test_backend,
                ));
            }
            check("discovery", outcome);
        }
        Err(f) => check("discovery", Err(f.reason)),
    }
    let metered = runtime.metered(
        tenant,
        &config,
        client.clone(),
        Purpose::Probe,
        caller,
        None,
        None,
    );
    if let (Some(model), Some(dims)) = (&embed_model, dims) {
        // Content-free synthetic inputs, fixed in the source.
        let inputs = vec![
            "Munarium conformance probe: first passage.".to_string(),
            "Munarium conformance probe: second passage.".to_string(),
        ];
        let outcome = metered
            .embed(EmbedCall {
                model,
                inputs: &inputs,
                dimensions: dims,
                deadline,
            })
            .await;
        check("embedding", outcome.map(|_| ()).map_err(|f| f.reason));
    }
    if let Some(model) = &rerank_model {
        let documents = vec![
            "A conformance probe passage about orchids.".to_string(),
            "An unrelated conformance probe passage.".to_string(),
            "A third conformance probe passage.".to_string(),
        ];
        let outcome = metered
            .rerank(RerankCall {
                model,
                query: "conformance probe orchids",
                documents: &documents,
                deadline,
            })
            .await;
        check("reranking", outcome.map(|_| ()).map_err(|f| f.reason));
    }
    let passed = !checks.is_empty() && checks.iter().all(|c| c.passed);
    let probe_id = uuid::Uuid::new_v4().simple().to_string();
    let fingerprint = client.fingerprint();
    sqlx::query(
        "INSERT INTO inference_probes
             (tenant_id, probe_id, provider_config, config_hash, endpoint_fingerprint,
              embed_model, rerank_model, embed_dimensions, passed, checks, probed_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
    )
    .bind(tenant)
    .bind(&probe_id)
    .bind(name)
    .bind(&config.hash)
    .bind(&fingerprint)
    .bind(&embed_model)
    .bind(&rerank_model)
    .bind(dims.map(|d| d as i32))
    .bind(passed)
    .bind(crate::error::to_json(&checks, "probe checks")?)
    .bind(caller)
    .execute(pool)
    .await
    .map_err(storage)?;
    Ok(dto::InferenceProbeResponse {
        probe_id,
        provider_config: name.to_string(),
        passed,
        endpoint_fingerprint: fingerprint,
        embed_model,
        embed_dimensions: dims.map(|d| d as u64),
        rerank_model,
        checks,
    })
}

/// IR-2: create or change a collection's binding (compare-and-set).
pub async fn op_bind(
    state: &AppState,
    tenant: &str,
    actor: &str,
    id_or_name: &str,
    req: dto::InferenceBindingRequest,
) -> Result<dto::CollectionInferenceDto> {
    let pool = state.inference.pool()?.clone();
    let collection = collection_id(state, tenant, id_or_name).await?;
    let config = match load_config(&pool, tenant, &req.provider_config).await? {
        Some(Ok(c)) => c,
        None => {
            return Err(KernelError::NotFound {
                kind: "provider-config",
                id: req.provider_config.clone(),
            })
        }
        Some(Err(_)) => {
            return Err(KernelError::InvalidInput(
                "a collection binds an openai-compatible configuration".into(),
            ))
        }
    };
    let spec = &config.doc.spec;
    if spec.budgets.daily_total_tokens.is_none() {
        return Err(not_qualified(
            "the bound configuration must set budgets.dailyTotalTokens: the collection's tenant budget pays for its inference",
        ));
    }
    if req.embedding.is_none() && req.rerank.is_none() {
        return Err(KernelError::InvalidInput(
            "a binding configures embedding, rerank or both".into(),
        ));
    }
    let mut embedding = req.embedding.clone();
    if let Some(e) = embedding.as_mut() {
        if !spec.models.embed.contains(&e.model) {
            return Err(KernelError::InvalidInput(
                "embedding.model is not in the configuration's models.embed".into(),
            ));
        }
        let dims = spec.inference.as_ref().and_then(|p| p.embed_dimensions);
        if dims != Some(SEMANTIC_DIMS) {
            return Err(KernelError::InvalidInput(format!(
                "semantic indexes support inference.embedDimensions = {SEMANTIC_DIMS}"
            )));
        }
        for i in [&e.query_instruction, &e.passage_instruction]
            .into_iter()
            .flatten()
        {
            if i.chars().count() > MAX_INSTRUCTION_CHARS {
                return Err(KernelError::InvalidInput(
                    "instructions are at most 1,024 characters".into(),
                ));
            }
        }
        e.policy = Some(StagePolicy::parse(e.policy.as_deref())?.as_str().into());
    }
    let mut rerank = req.rerank.clone();
    if let Some(r) = rerank.as_mut() {
        if !spec.models.rerank.contains(&r.model) {
            return Err(KernelError::InvalidInput(
                "rerank.model is not in the configuration's models.rerank".into(),
            ));
        }
        let candidates = r.max_candidates.unwrap_or(DEFAULT_MAX_CANDIDATES);
        let bytes = r.max_document_bytes.unwrap_or(DEFAULT_MAX_DOCUMENT_BYTES);
        if !(2..=100).contains(&candidates) || !(256..=1_048_576).contains(&bytes) {
            return Err(KernelError::InvalidInput(
                "rerank.max_candidates must be 2-100 and max_document_bytes 256-1048576".into(),
            ));
        }
        if let Some(i) = &r.instruction {
            // The instruction sits inside the card's query template: one
            // line, so it can never forge the `<Query>:` label.
            if i.trim().is_empty()
                || i.chars().count() > MAX_INSTRUCTION_CHARS
                || i.chars().any(char::is_control)
            {
                return Err(KernelError::InvalidInput(
                    "rerank.instruction must be 1-1,024 characters with no control characters; omit it for none".into(),
                ));
            }
        }
        r.max_candidates = Some(candidates);
        r.max_document_bytes = Some(bytes);
        r.policy = Some(StagePolicy::parse(r.policy.as_deref())?.as_str().into());
    }
    let shadow = req.shadow.clone().unwrap_or_default();
    if (shadow.rerank || shadow.semantic_index) && spec.budgets.shadow_daily_tokens.is_none() {
        return Err(not_qualified(
            "shadow evaluation requires budgets.shadowDailyTokens on the bound configuration",
        ));
    }
    if (shadow.rerank && rerank.is_none()) || (shadow.semantic_index && embedding.is_none()) {
        return Err(KernelError::InvalidInput(
            "a shadow comparison needs its stage configured".into(),
        ));
    }
    let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
    if !(100..=60_000).contains(&timeout_ms) {
        return Err(KernelError::InvalidInput(
            "timeout_ms must be 100-60000".into(),
        ));
    }

    let mut tx = pool.begin().await.map_err(storage)?;
    let current = load_binding(&mut *tx, tenant, &collection, true).await?;
    let actual = current.as_ref().map_or(0, |b| b.revision);
    if actual != req.expected_revision {
        return Err(KernelError::HeadConflict {
            expected: req.expected_revision,
            actual,
        });
    }
    // SC2: refuse an endpoint that does not explicitly cover this tenant or
    // whose declared placement does not satisfy the requirement.
    let placement = effective_requirement(
        req.placement.as_ref(),
        current.as_ref(),
        declared_placement(&config),
    )?;
    if let Err(reason) = placement_check(tenant, declared_placement(&config), placement.as_ref()) {
        return Err(not_qualified(placement_refusal(reason)));
    }
    let same_space = current.as_ref().is_some_and(|c| {
        c.provider_config == req.provider_config
            && c.embedding
                .as_ref()
                .map(|e| (&e.model, &e.query_instruction, &e.passage_instruction))
                == embedding
                    .as_ref()
                    .map(|e| (&e.model, &e.query_instruction, &e.passage_instruction))
    });
    if let Some(c) = &current {
        if c.state == "enrolled" && (!same_space || c.rerank.is_some() != rerank.is_some()) {
            return Err(not_qualified(
                "roll the collection back before changing its configuration, embedding space or stages",
            ));
        }
    }
    let next = Binding {
        collection_id: collection.clone(),
        revision: actual + 1,
        state: current
            .as_ref()
            .map_or_else(|| "bound".to_string(), |c| c.state.clone()),
        provider_config: req.provider_config.clone(),
        embedding,
        rerank,
        shadow,
        timeout_ms,
        semantic_index_id: current
            .as_ref()
            .filter(|_| same_space)
            .and_then(|c| c.semantic_index_id.clone()),
        prior_index_id: current.as_ref().and_then(|c| c.prior_index_id.clone()),
        placement,
    };
    write_binding(&mut tx, tenant, &next, "bind", actor).await?;
    tx.commit().await.map_err(storage)?;
    describe(state, tenant, &collection).await
}

/// IR-4: enroll: verify the probe and the semantic index, then serve.
pub async fn op_enroll(
    state: &AppState,
    tenant: &str,
    actor: &str,
    id_or_name: &str,
    expected_revision: u64,
) -> Result<dto::CollectionInferenceDto> {
    let runtime = &state.inference;
    let pool = runtime.pool()?.clone();
    let collection = collection_id(state, tenant, id_or_name).await?;
    let retrieval = state.retrieval_for(tenant)?;
    let b = load_binding(&pool, tenant, &collection, false)
        .await?
        .ok_or_else(|| not_qualified("the collection has no inference binding"))?;
    if b.revision != expected_revision {
        return Err(KernelError::HeadConflict {
            expected: expected_revision,
            actual: b.revision,
        });
    }
    if b.state == "enrolled" {
        return Err(not_qualified("the collection is already enrolled"));
    }
    let resolution = runtime
        .resolve(tenant, &b.provider_config, b.placement.as_ref())
        .await?;
    if let Err(f) = &resolution.ready {
        return Err(not_qualified(format!(
            "the bound configuration is not ready: {}",
            f
        )));
    }
    let active = retrieval.active_collection_index(&collection).await?;
    let mut prior = b.prior_index_id.clone();
    let mut activate = None;
    if let Some(settings) = &b.embedding {
        let (Some(config), Some(fp)) = (&resolution.config, &resolution.fingerprint) else {
            return Err(not_qualified("the bound configuration is unavailable"));
        };
        let space = space_for(settings, config, fp)
            .map_err(|f| not_qualified(format!("the embedding stage is not configured: {f}")))?;
        let index = b.semantic_index_id.clone().ok_or_else(|| {
            not_qualified("build the semantic index first (a semantic build job)")
        })?;
        match retrieval.semantic_identity(&index).await? {
            Some((identity, _)) if identity == space.identity => {}
            _ => return Err(not_qualified(
                "the built semantic index does not match the binding's current space; rebuild it",
            )),
        }
        let coverage = retrieval.semantic_coverage(&collection, &index).await?;
        if !coverage.complete() {
            return Err(not_qualified(format!(
                "the semantic index is incomplete: {} of {} chunks have vectors",
                coverage.vectors, coverage.chunks
            )));
        }
        let current = active.clone().ok_or_else(|| {
            not_qualified("the collection has no active index to restore on rollback")
        })?;
        if current != index {
            if retrieval.semantic_identity(&current).await?.is_some() {
                return Err(not_qualified(
                    "the active index is not a hash index; roll back to an approved index first",
                ));
            }
            prior = Some(current);
            activate = Some(index);
        }
    }
    if let Some(index) = &activate {
        retrieval
            .activate_collection_index(&collection, index)
            .await?;
    }
    let mut tx = pool.begin().await.map_err(storage)?;
    let locked = load_binding(&mut *tx, tenant, &collection, true).await?;
    if locked.as_ref().map(|l| l.revision) != Some(expected_revision) {
        // Lost a race after activating: restore what was active.
        if let (Some(_), Some(prior)) = (&activate, &prior) {
            retrieval
                .activate_collection_index(&collection, prior)
                .await?;
        }
        return Err(KernelError::HeadConflict {
            expected: expected_revision,
            actual: locked.map_or(0, |l| l.revision),
        });
    }
    let next = Binding {
        revision: b.revision + 1,
        state: "enrolled".into(),
        prior_index_id: prior,
        ..b
    };
    write_binding(&mut tx, tenant, &next, "enroll", actor).await?;
    tx.commit().await.map_err(storage)?;
    describe(state, tenant, &collection).await
}

/// IR-4: roll back to the prior approved index. Calls no endpoint, so it
/// works with the endpoint down and with inference disabled.
pub async fn op_rollback(
    state: &AppState,
    tenant: &str,
    actor: &str,
    id_or_name: &str,
    expected_revision: u64,
) -> Result<dto::CollectionInferenceDto> {
    let pool = state.inference.pool()?.clone();
    let collection = collection_id(state, tenant, id_or_name).await?;
    let retrieval = state.retrieval_for(tenant)?;
    let mut tx = pool.begin().await.map_err(storage)?;
    let b = load_binding(&mut *tx, tenant, &collection, true)
        .await?
        .ok_or_else(|| not_qualified("the collection has no inference binding"))?;
    if b.revision != expected_revision {
        return Err(KernelError::HeadConflict {
            expected: expected_revision,
            actual: b.revision,
        });
    }
    if b.state != "enrolled" {
        return Err(not_qualified(
            "only an enrolled collection can be rolled back",
        ));
    }
    if let Some(prior) = &b.prior_index_id {
        if retrieval
            .active_collection_index(&collection)
            .await?
            .as_deref()
            != Some(prior)
        {
            retrieval
                .activate_collection_index(&collection, prior)
                .await?;
        }
    }
    let next = Binding {
        revision: b.revision + 1,
        state: "rolled_back".into(),
        ..b
    };
    write_binding(&mut tx, tenant, &next, "rollback", actor).await?;
    tx.commit().await.map_err(storage)?;
    describe(state, tenant, &collection).await
}

/// The binding, its stages' readiness (no endpoint call), the semantic
/// index's coverage and today's usage by purpose.
pub async fn describe(
    state: &AppState,
    tenant: &str,
    collection: &str,
) -> Result<dto::CollectionInferenceDto> {
    let runtime = &state.inference;
    let pool = runtime.pool()?;
    let retrieval = state.retrieval_for(tenant)?;
    let active = retrieval.active_collection_index(collection).await?;
    let Some(b) = load_binding(pool, tenant, collection, false).await? else {
        return Ok(dto::CollectionInferenceDto {
            collection_id: collection.to_string(),
            state: "unbound".into(),
            revision: 0,
            provider_config: None,
            embedding: None,
            rerank: None,
            shadow: None,
            timeout_ms: None,
            semantic_index_id: None,
            semantic_chunks: None,
            semantic_vectors: None,
            prior_index_id: None,
            active_index_id: active,
            stages: vec![],
            usage: vec![],
            placement: None,
        });
    };
    let resolution = runtime
        .resolve(tenant, &b.provider_config, b.placement.as_ref())
        .await?;
    let state_of = |space: Option<std::result::Result<SemanticSpace, InferenceFailure>>| {
        let failure = match (&resolution.ready, space) {
            (Err(f), _) => Some(*f),
            (Ok(_), Some(Err(f))) => Some(f),
            _ => None,
        };
        (
            failure.map_or("ready", |f| f.kind.state()).to_string(),
            failure.map(|f| f.reason.to_string()),
        )
    };
    let mut stages = Vec::new();
    if let Some(e) = &b.embedding {
        let space = match (&resolution.config, &resolution.fingerprint) {
            (Some(c), Some(fp)) => space_for(e, c, fp),
            _ => Err(InferenceFailure::new(
                FailureKind::Unconfigured,
                "config_missing",
            )),
        };
        let identity = space.as_ref().ok().map(|s| s.identity.clone());
        let (st, reason) = state_of(Some(space));
        stages.push(dto::InferenceStageStatusDto {
            stage: "embedding".into(),
            model: e.model.clone(),
            policy: policy_of(&e.policy).as_str().into(),
            state: st,
            reason,
            identity,
        });
    }
    if let Some(r) = &b.rerank {
        let (st, reason) = state_of(None);
        stages.push(dto::InferenceStageStatusDto {
            stage: "reranking".into(),
            model: r.model.clone(),
            policy: policy_of(&r.policy).as_str().into(),
            state: st,
            reason,
            identity: Some(rerank_identity(&r.model, r.instruction.as_deref())),
        });
    }
    let coverage = match &b.semantic_index_id {
        Some(index) => Some(retrieval.semantic_coverage(collection, index).await?),
        None => None,
    };
    let usage = sqlx::query(
        "SELECT purpose, count(*)::bigint AS attempts,
                COALESCE(sum(accounted_units), 0)::bigint AS units,
                count(*) FILTER (WHERE usage_source = 'missing')::bigint AS missing
           FROM inference_attempts
          WHERE tenant_id = $1 AND $2 = ANY(collection_ids)
            AND started_at >= (date_trunc('day', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC')
          GROUP BY purpose ORDER BY purpose",
    )
    .bind(tenant)
    .bind(collection)
    .fetch_all(pool)
    .await
    .map_err(storage)?
    .into_iter()
    .map(|r| dto::InferenceUsageDto {
        purpose: r.get("purpose"),
        attempts: r.get::<i64, _>("attempts").max(0) as u64,
        accounted_units: r.get::<i64, _>("units").max(0) as u64,
        missing_usage: r.get::<i64, _>("missing").max(0) as u64,
    })
    .collect();
    Ok(dto::CollectionInferenceDto {
        collection_id: collection.to_string(),
        state: b.state.clone(),
        revision: b.revision,
        provider_config: Some(b.provider_config.clone()),
        embedding: b.embedding.clone(),
        rerank: b.rerank.clone(),
        shadow: Some(b.shadow.clone()),
        timeout_ms: Some(b.timeout_ms),
        semantic_index_id: b.semantic_index_id.clone(),
        semantic_chunks: coverage.map(|c| c.chunks.max(0) as u64),
        semantic_vectors: coverage.map(|c| c.vectors.max(0) as u64),
        prior_index_id: b.prior_index_id.clone(),
        active_index_id: active,
        stages,
        usage,
        placement: b.placement.clone(),
    })
}

/// IR-2: the `semantic` build job. Builds (or resumes) the semantic index
/// beside the active one, fills its vectors through the collection's
/// tenant-owned budget as build work attributed to the initiating caller,
/// and records it on the binding. Never activates anything.
pub async fn run_semantic_job(
    state: &AppState,
    job: &munarium_store_pg::jobs::BuildJob,
    fence: Option<&munarium_store_pg::jobs::JobFence>,
    signal: Option<&munarium_store_pg::jobs::JobSignal>,
) -> Result<serde_json::Value> {
    let tenant = job.tenant_id.as_str();
    let runtime = &state.inference;
    let pool = runtime.pool()?.clone();
    let collection = job
        .scope_id
        .as_deref()
        .ok_or_else(|| KernelError::InvalidInput("a semantic job names a collection".into()))?;
    let caller = job
        .params
        .get("caller")
        .and_then(|v| v.as_str())
        .unwrap_or(&job.requested_by)
        .to_string();
    let max_chars = job
        .params
        .get("max_chars")
        .and_then(|v| v.as_u64())
        .unwrap_or(400) as usize;
    let watermark_seq = job
        .params
        .get("watermark_seq")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let b = load_binding(&pool, tenant, collection, false)
        .await?
        .ok_or_else(|| {
            KernelError::InvalidInput("the collection has no inference binding".into())
        })?;
    let settings = b.embedding.clone().ok_or_else(|| {
        KernelError::InvalidInput("the collection's binding has no embedding stage".into())
    })?;
    let resolution = runtime
        .resolve(tenant, &b.provider_config, b.placement.as_ref())
        .await?;
    let (config, client, fingerprint) = match (
        &resolution.ready,
        &resolution.config,
        &resolution.fingerprint,
    ) {
        (Ok(client), Some(config), Some(fp)) => (config.clone(), client.clone(), fp.clone()),
        (Err(f), _, _) => {
            return Err(KernelError::Provider(format!(
                "semantic index build: embedding stage {} ({})",
                f.kind.state(),
                f.reason
            )))
        }
        _ => {
            return Err(KernelError::Provider(
                "semantic index build: configuration unavailable".into(),
            ))
        }
    };
    let space = space_for(&settings, &config, &fingerprint)
        .map_err(|f| KernelError::InvalidInput(format!("semantic index build: {f}")))?;
    let retrieval = state.retrieval_for(tenant)?;
    let index = retrieval
        .build_semantic_index(collection, max_chars, watermark_seq, &space)
        .await?;
    let stage = EmbeddingStage {
        policy: StagePolicy::Require,
        endpoint: Ok(runtime.metered(
            tenant,
            &config,
            client,
            Purpose::Build,
            &caller,
            Some(collection),
            Some(&job.job_id),
        )),
        model: settings.model.clone(),
        space: space.clone(),
        query_instruction: settings.query_instruction.clone().unwrap_or_default(),
        passage_instruction: settings.passage_instruction.clone().unwrap_or_default(),
    };
    let batch = config
        .doc
        .spec
        .inference
        .clone()
        .unwrap_or_default()
        .max_batch();
    let coverage = retrieval
        .fill_semantic_index(
            collection,
            &index.id,
            &stage,
            batch,
            BUILD_BATCH_TIMEOUT,
            fence,
            signal,
        )
        .await?;
    // Record the index on the binding if it still defines this space.
    let mut tx = pool.begin().await.map_err(storage)?;
    if let Some(fence) = fence {
        if !munarium_store_pg::jobs::lock_generation(&mut tx, fence).await? {
            return Err(fence.stale());
        }
    }
    if let Some(current) = load_binding(&mut *tx, tenant, collection, true).await? {
        if current.embedding.as_ref() == Some(&settings)
            && current.provider_config == b.provider_config
            && current.semantic_index_id.as_deref() != Some(index.id.as_str())
        {
            let next = Binding {
                revision: current.revision + 1,
                semantic_index_id: Some(index.id.clone()),
                ..current
            };
            write_binding(&mut tx, tenant, &next, "semantic_index_built", &caller).await?;
        }
    }
    tx.commit().await.map_err(storage)?;
    Ok(serde_json::json!({
        "index_version_id": index.id,
        "semantic_identity": space.identity,
        "chunks": coverage.chunks,
        "vectors": coverage.vectors,
        "complete": coverage.complete(),
    }))
}

// ---------------------------------------------------------------------------
// REST
// ---------------------------------------------------------------------------

/// POST /v1/providers/{name}/inference-probe
#[utoipa::path(post, path = "/v1/providers/{name}/inference-probe",
    tag = "providers",
    params(("name" = String, Path, description = "an openai-compatible provider configuration")),
    responses(
        (status = 200, description = "the recorded probe; `passed` false is a recorded failure", body = dto::InferenceProbeResponse),
        (status = 409, description = "inference-not-qualified: inference is disabled on this deployment")
    ))]
pub async fn probe_provider(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uid: Option<axum::Extension<crate::middleware::Uid>>,
    Path(name): Path<String>,
) -> ApiResult<Json<dto::InferenceProbeResponse>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    let caller = caller_of(uid, &ctx.role);
    Ok(Json(
        op_probe(&state, &ctx.tenant_id, &caller, &name).await?,
    ))
}

/// GET /v1/collections/{id}/inference
#[utoipa::path(get, path = "/v1/collections/{id}/inference",
    tag = "collections",
    params(("id" = String, Path, description = "collection id or name")),
    responses((status = 200, description = "binding, stage readiness and today's usage", body = dto::CollectionInferenceDto)))]
pub async fn get_inference(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<dto::CollectionInferenceDto>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    let collection = collection_id(&state, &ctx.tenant_id, &id).await?;
    Ok(Json(describe(&state, &ctx.tenant_id, &collection).await?))
}

/// PUT /v1/collections/{id}/inference
#[utoipa::path(put, path = "/v1/collections/{id}/inference",
    tag = "collections",
    params(("id" = String, Path, description = "collection id or name")),
    request_body = dto::InferenceBindingRequest,
    responses(
        (status = 200, description = "the binding after the change", body = dto::CollectionInferenceDto),
        (status = 409, description = "head-conflict on a stale expected_revision, or inference-not-qualified")
    ))]
pub async fn put_inference(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uid: Option<axum::Extension<crate::middleware::Uid>>,
    Path(id): Path<String>,
    crate::rest::ProblemJson(req): crate::rest::ProblemJson<dto::InferenceBindingRequest>,
) -> ApiResult<Json<dto::CollectionInferenceDto>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    let actor = caller_of(uid, &ctx.role);
    Ok(Json(
        op_bind(&state, &ctx.tenant_id, &actor, &id, req).await?,
    ))
}

/// POST /v1/collections/{id}/inference/enroll
#[utoipa::path(post, path = "/v1/collections/{id}/inference/enroll",
    tag = "collections",
    params(("id" = String, Path, description = "collection id or name")),
    request_body = dto::InferenceRevisionRequest,
    responses(
        (status = 200, description = "enrolled: the semantic index is active and the stages serve", body = dto::CollectionInferenceDto),
        (status = 409, description = "head-conflict, or inference-not-qualified with the unmet condition")
    ))]
pub async fn enroll_inference(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uid: Option<axum::Extension<crate::middleware::Uid>>,
    Path(id): Path<String>,
    crate::rest::ProblemJson(req): crate::rest::ProblemJson<dto::InferenceRevisionRequest>,
) -> ApiResult<Json<dto::CollectionInferenceDto>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    let actor = caller_of(uid, &ctx.role);
    Ok(Json(
        op_enroll(&state, &ctx.tenant_id, &actor, &id, req.expected_revision).await?,
    ))
}

/// POST /v1/collections/{id}/inference/rollback
#[utoipa::path(post, path = "/v1/collections/{id}/inference/rollback",
    tag = "collections",
    params(("id" = String, Path, description = "collection id or name")),
    request_body = dto::InferenceRevisionRequest,
    responses(
        (status = 200, description = "rolled back: the prior approved index is active; no endpoint is called", body = dto::CollectionInferenceDto),
        (status = 409, description = "head-conflict, or inference-not-qualified when not enrolled")
    ))]
pub async fn rollback_inference(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uid: Option<axum::Extension<crate::middleware::Uid>>,
    Path(id): Path<String>,
    crate::rest::ProblemJson(req): crate::rest::ProblemJson<dto::InferenceRevisionRequest>,
) -> ApiResult<Json<dto::CollectionInferenceDto>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    let actor = caller_of(uid, &ctx.role);
    Ok(Json(
        op_rollback(&state, &ctx.tenant_id, &actor, &id, req.expected_revision).await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_switch_is_off_unless_enabled_and_refuses_unknown_values() {
        assert!(!InferenceSettings::parse(None, None).unwrap().enabled);
        assert!(
            !InferenceSettings::parse(Some("disabled"), None)
                .unwrap()
                .enabled
        );
        assert!(
            InferenceSettings::parse(Some("enabled"), None)
                .unwrap()
                .enabled
        );
        assert!(InferenceSettings::parse(Some("on"), None).is_err());
        assert_eq!(
            InferenceSettings::parse(None, Some("3"))
                .unwrap()
                .concurrency,
            3
        );
        assert!(InferenceSettings::parse(None, Some("0")).is_err());
    }

    #[test]
    fn only_unsent_failures_are_refunded() {
        let f = |reason| InferenceFailure::new(FailureKind::Unavailable, reason);
        assert!(never_sent(&f("request_bytes")));
        assert!(never_sent(&f("credential_unavailable")));
        assert!(!never_sent(&f("transport")));
        assert!(!never_sent(&f("status_503")));
    }
}
