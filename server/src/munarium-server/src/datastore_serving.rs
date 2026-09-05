// SPDX-License-Identifier: Apache-2.0
//! Datastore serving, server side: the shared infrastructure parts,
//! the readiness warmer, the rollout selector API, and promotion.
//!
//! The §9.2 contract this file exists to keep: **a replica with any
//! datastore-selected scope is not ready until its complete serving-required
//! set is hydrated, verified and openable** — and a replica with none has no
//! datastore readiness dependency at all. Warming is asynchronous and
//! bounded; the readiness probe reads maintained state and performs no I/O.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use munarium_core::{KernelError, Result};
use munarium_datastore::hydrate::{CacheBudget, L1Cache, Residency};
use munarium_datastore::verify::{Limits, ReaderCapabilities};
use munarium_datastore::ArtifactCacheKey;
use munarium_retrieval::executor::ArtifactExecutor;
use munarium_retrieval::mirror::ArtifactStoreFactory;
use munarium_retrieval::serving::ServingPlane;
use munarium_retrieval::RequiredVersionsPolicy;
use munarium_store_pg::artifacts::{ArtifactCatalog, ArtifactState, BindingSlot};
use munarium_store_pg::rollout::{RolloutChange, RolloutSelector};

use crate::datastore_builds::tenant_path_hash;
use crate::error::ApiError;
use crate::state::AppState;
use munarium_api_types as dto;

// ---------------------------------------------------------------------------
// Shared infrastructure parts
// ---------------------------------------------------------------------------

/// The process-wide datastore machinery both the shadow plane and the serving
/// plane stand on: ONE L1 cache and ONE store factory. Two caches would be
/// two eviction ledgers over one directory — each free to delete what the
/// other believes resident.
pub struct DatastoreParts {
    pub cache: Arc<L1Cache>,
    /// The process-wide open-shard tier (L0). One per process, like the L1
    /// cache: per-request opens are the cost it exists to remove, and a
    /// per-tenant L0 would re-pay that cost once per tenant.
    pub l0: Arc<munarium_retrieval::executor::L0Cache>,
    pub stores: Arc<dyn ArtifactStoreFactory>,
    pub reader: ReaderCapabilities,
    pub limits: Limits,
}

impl std::fmt::Debug for DatastoreParts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DatastoreParts").finish_non_exhaustive()
    }
}

impl DatastoreParts {
    /// Build the parts, or say exactly why there are none. Shared by shadow
    /// and serving arming; the mode gate is the CALLER's, because which modes
    /// need parts is the caller's contract.
    pub fn build(state: &AppState) -> std::result::Result<Arc<Self>, String> {
        let local_root = std::env::var("MUNARIUM_DATASTORE_LOCAL_ROOT")
            .map_err(|_| "MUNARIUM_DATASTORE_LOCAL_ROOT is unset".to_string())?;
        let high = std::env::var("MUNARIUM_DATASTORE_L1_HIGH_WATERMARK")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8 * 1024 * 1024 * 1024u64);
        let low = std::env::var("MUNARIUM_DATASTORE_L1_LOW_WATERMARK")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(6 * 1024 * 1024 * 1024u64);
        let budget = CacheBudget::new(high, low).map_err(|e| e.to_string())?;
        let cache = L1Cache::new(std::path::Path::new(&local_root).join("l1"), budget)
            .map_err(|e| e.to_string())?;
        let stores = state.artifact_store_factory().map_err(|e| e.to_string())?;
        Ok(Arc::new(Self {
            cache: Arc::new(cache),
            l0: Arc::new(munarium_retrieval::executor::L0Cache::new(
                std::env::var("MUNARIUM_DATASTORE_L0_OPEN_SHARDS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(8),
            )),
            stores,
            reader: ReaderCapabilities::v1(),
            limits: Limits::default(),
        }))
    }

    /// A tenant-scoped executor over these parts.
    pub fn executor(&self, pool: &sqlx::PgPool, tenant: &str) -> ArtifactExecutor {
        ArtifactExecutor {
            catalog: ArtifactCatalog::new(pool.clone(), tenant),
            stores: Arc::clone(&self.stores),
            cache: Arc::clone(&self.cache),
            l0: Arc::clone(&self.l0),
            reader: self.reader.clone(),
            limits: self.limits,
            isolation_domain: tenant_path_hash(tenant),
        }
    }

    /// A tenant-scoped serving plane over these parts.
    pub fn serving_plane(&self, pool: &sqlx::PgPool, tenant: &str) -> Arc<ServingPlane> {
        Arc::new(ServingPlane {
            selector: RolloutSelector::new(pool.clone(), tenant),
            executor: Arc::new(self.executor(pool, tenant)),
        })
    }
}

// ---------------------------------------------------------------------------
// Readiness
// ---------------------------------------------------------------------------

/// The maintained readiness state the probe reads. No I/O in the getters —
/// §13.3's rule that the readiness endpoint reads state rather than checking
/// it.
#[derive(Debug)]
pub struct DatastoreReadiness {
    /// True when every datastore-selected scope's serving-required set is
    /// open. STARTS FALSE in datastore mode: a replica must not admit traffic
    /// on the strength of not having checked yet.
    ready: AtomicBool,
    /// How many datastore-selected scopes the last sweep saw. Zero means no
    /// datastore readiness dependency (§9.2).
    selected_scopes: AtomicI64,
    /// Why not ready — hashed scope diagnostics only, never tenant ids or
    /// scope names in clear.
    blocking: Mutex<Vec<String>>,
}

impl Default for DatastoreReadiness {
    fn default() -> Self {
        Self {
            ready: AtomicBool::new(false),
            selected_scopes: AtomicI64::new(0),
            blocking: Mutex::new(vec!["not yet swept".into()]),
        }
    }
}

impl DatastoreReadiness {
    /// Whether this replica may admit traffic, as far as the datastore is
    /// concerned. True when nothing is selected — no dependency — and true
    /// when everything selected is open.
    pub fn admits(&self) -> bool {
        self.selected_scopes.load(Ordering::Relaxed) == 0 || self.ready.load(Ordering::Relaxed)
    }

    pub fn selected_scopes(&self) -> i64 {
        self.selected_scopes.load(Ordering::Relaxed)
    }

    pub fn blocking(&self) -> Vec<String> {
        self.blocking.lock().unwrap().clone()
    }

    /// Permanent not-ready for a datastore-mode replica with no
    /// infrastructure: nothing can ever be proven open, so nothing may admit.
    pub fn mark_infrastructure_missing(&self) {
        self.record(1, vec!["datastore-infrastructure-missing".into()]);
    }

    fn record(&self, selected: i64, blocking: Vec<String>) {
        self.selected_scopes.store(selected, Ordering::Relaxed);
        self.ready.store(blocking.is_empty(), Ordering::Relaxed);
        *self.blocking.lock().unwrap() = blocking;
    }
}

/// One warmer sweep: enumerate every datastore-selected scope across tenants,
/// hydrate-and-open its serving-required set, prewarm `staged` bindings where
/// asked, and record readiness. Returns the blocking diagnostics.
///
/// The sweep also HEARTBEATS this node into `retrieval_node_snapshots`, which
/// is what makes the admin fleet table and the promotion gate's observed
/// counts real rather than perpetually `unknown`.
pub async fn warm_once(
    state: &AppState,
    parts: &Arc<DatastoreParts>,
    opened: &mut HashSet<ArtifactCacheKey>,
) -> Result<()> {
    let Some(pool) = state.pg_pool() else {
        return Err(KernelError::InvalidInput(
            "datastore readiness requires the postgres store".into(),
        ));
    };

    // Environment-scoped enumeration across tenants. Raw SQL rather than the
    // tenant-scoped selector, because readiness is a PROCESS property: this
    // replica serves every tenant, so every tenant's selected scopes bind it.
    let rows = sqlx::query_as::<_, (String, String, String, String, bool)>(
        "SELECT tenant_id, scope_kind, scope_id, required_versions_policy, prewarm_staged
           FROM retrieval_rollout WHERE serving = 'datastore' OR prewarm_staged",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;

    let horizon =
        i64::try_from(state.datastore_capabilities().pin_horizon_secs).unwrap_or(i64::MAX);
    let mut selected = 0i64;
    let mut blocking: Vec<String> = Vec::new();

    for (tenant, scope_kind, scope_id, policy, prewarm) in &rows {
        let selector = RolloutSelector::new(pool.clone(), tenant);
        let entry = match selector.get(scope_kind, scope_id).await? {
            Some(e) => e,
            None => continue,
        };
        let is_selected = entry.serving == "datastore";
        if is_selected {
            selected += 1;
        }

        // Hashed diagnostic key: enough for an operator to correlate with
        // their own selector rows, nothing for anyone else.
        let scope_hash = {
            use sha2::Digest as _;
            let d = sha2::Sha256::digest(format!("{tenant}/{scope_kind}/{scope_id}").as_bytes());
            hex::encode(d)[..12].to_string()
        };

        let retrieval = state.retrieval_for(tenant)?;
        let parsed_policy = RequiredVersionsPolicy::parse(policy)?;
        let required = match scope_kind.as_str() {
            "collection" => {
                retrieval
                    .reference()
                    .required_versions(scope_id, parsed_policy, horizon)
                    .await
            }
            "shape" => {
                retrieval
                    .reference()
                    .required_versions_for_shape(scope_id, parsed_policy, horizon)
                    .await
            }
            other => {
                // A scope kind the warmer cannot prove open must not be
                // admitted on hope; refusing readiness is the honest posture.
                if is_selected {
                    blocking.push(format!("{scope_hash}:unsupported-scope-kind-{other}"));
                }
                continue;
            }
        };
        let required = match required {
            Ok(r) => r,
            Err(e) => {
                if is_selected {
                    blocking.push(format!("{scope_hash}:required-versions:{e}"));
                }
                continue;
            }
        };

        let catalog = ArtifactCatalog::new(pool.clone(), tenant);
        for version in &required {
            // The serving set gates readiness; the staged set prewarms and
            // never does (§9.1).
            for (slot, residency, gate) in [
                (
                    BindingSlot::Serving,
                    Residency::ServingRequired,
                    is_selected,
                ),
                (BindingSlot::Staged, Residency::StagedPrewarm, false),
            ] {
                if slot == BindingSlot::Staged && !(*prewarm || is_selected) {
                    continue;
                }
                let binding = match catalog.binding(&version.index_version_id, slot).await? {
                    Some(b) => b,
                    None => {
                        if gate {
                            blocking.push(format!(
                                "{scope_hash}:{}:no-serving-binding",
                                short(&version.index_version_id)
                            ));
                        }
                        continue;
                    }
                };
                let row = match catalog
                    .artifact(&version.index_version_id, &binding.artifact_id)
                    .await?
                {
                    Some(r) if r.state == ArtifactState::Verified => r,
                    _ => {
                        if gate {
                            blocking.push(format!(
                                "{scope_hash}:{}:artifact-not-verified",
                                short(&version.index_version_id)
                            ));
                        }
                        continue;
                    }
                };

                let key = ArtifactCacheKey::new(
                    tenant_path_hash(tenant),
                    version.index_version_id.clone(),
                    binding.artifact_id.clone(),
                );
                let env = state.deployment_environment_id();
                let node = state.config.instance_id.clone();
                if let Some(resident) = parts.cache.resident(&key) {
                    if opened.contains(&key) {
                        // Still resident and previously proven open: refresh
                        // the residency row so its freshness reflects THIS
                        // sweep, not the sweep that first opened it.
                        record_residency(
                            pool,
                            &env,
                            &node,
                            tenant,
                            &version.index_version_id,
                            &binding.artifact_id,
                            &row.engine_id,
                            "open",
                            Some(resident.bytes as i64),
                        )
                        .await;
                        continue;
                    }
                }
                let store = match parts.stores.store_for_prefix(&row.artifact_uri) {
                    Ok(s) => s,
                    Err(e) => {
                        if gate {
                            blocking.push(format!("{scope_hash}:store:{e}"));
                        }
                        continue;
                    }
                };
                let cache = Arc::clone(&parts.cache);
                let reader = parts.reader.clone();
                let limits = parts.limits;
                let artifact_id = binding.artifact_id.clone();
                let key_for_task = key.clone();
                let outcome = tokio::task::spawn_blocking(move || {
                    let resident = cache.hydrate(
                        &key_for_task,
                        store.as_ref(),
                        &reader,
                        &limits,
                        residency,
                    )?;
                    // Openable is part of the admission bar (§9.2): hydrated
                    // bytes that cannot open are not capacity.
                    let local = munarium_datastore::store::LocalFileStore::new(&resident.path)?;
                    munarium_datastore::shard::OpenShard::open(
                        &local,
                        &artifact_id,
                        &reader,
                        &limits,
                    )
                    .map(|_| ())
                })
                .await;
                match outcome {
                    Ok(Ok(())) => {
                        let bytes = parts.cache.resident(&key).map(|r| r.bytes as i64);
                        record_residency(
                            pool,
                            &env,
                            &node,
                            tenant,
                            &version.index_version_id,
                            &binding.artifact_id,
                            &row.engine_id,
                            "open",
                            bytes,
                        )
                        .await;
                        opened.insert(key);
                    }
                    Ok(Err(e)) => {
                        let residency = match &e {
                            munarium_datastore::Error::Integrity(_) => "quarantined",
                            _ => "hydrating",
                        };
                        record_residency(
                            pool,
                            &env,
                            &node,
                            tenant,
                            &version.index_version_id,
                            &binding.artifact_id,
                            &row.engine_id,
                            residency,
                            None,
                        )
                        .await;
                        if gate {
                            blocking.push(format!(
                                "{scope_hash}:{}:{e}",
                                short(&version.index_version_id)
                            ));
                        }
                    }
                    Err(e) => {
                        if gate {
                            blocking.push(format!("{scope_hash}:task:{e}"));
                        }
                    }
                }
            }
        }
    }

    state.datastore_readiness().record(selected, blocking);
    heartbeat(state, parts, pool).await;
    Ok(())
}

fn short(version: &str) -> String {
    version.chars().take(12).collect()
}

/// Upsert this node's row in `retrieval_node_snapshots`. Failure is logged,
/// never fatal: a heartbeat that could take readiness down would invert its
/// purpose.
async fn heartbeat(state: &AppState, parts: &Arc<DatastoreParts>, pool: &sqlx::PgPool) {
    let readiness = state.datastore_readiness();
    let admission = if readiness.admits() {
        "ready"
    } else {
        "warming"
    };
    let result = sqlx::query(
        // started_at rides the INSERT only (the conflict path leaves it): the
        // first successful heartbeat is a fine approximation of process
        // start. The column is NOT NULL with no default, and omitting it
        // meant a FRESH node row could never insert — heartbeats had been
        // failing on every new replica since 0026, masked by tests that
        // insert their own rows and a fleet gate that treats zero reporting
        // nodes permissively.
        "INSERT INTO retrieval_node_snapshots
             (environment_id, node_id, plane, deployment_revision, retrieval_mode,
              compiled_engines, format_min, format_max, admission_state,
              blocking_scope_hashes, l1_used_bytes, l1_budget_bytes, local_root_health,
              started_at, last_seen_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13, now(), now())
         ON CONFLICT (environment_id, node_id) DO UPDATE SET
              plane = EXCLUDED.plane,
              deployment_revision = EXCLUDED.deployment_revision,
              retrieval_mode = EXCLUDED.retrieval_mode,
              admission_state = EXCLUDED.admission_state,
              blocking_scope_hashes = EXCLUDED.blocking_scope_hashes,
              l1_used_bytes = EXCLUDED.l1_used_bytes,
              l1_budget_bytes = EXCLUDED.l1_budget_bytes,
              local_root_health = EXCLUDED.local_root_health,
              last_seen_at = now()",
    )
    .bind(state.deployment_environment_id())
    .bind(&state.config.instance_id)
    .bind(std::env::var("MUNARIUM_DEPLOYMENT_PLANE").unwrap_or_else(|_| "rest".into()))
    .bind(std::env::var("MUNARIUM_DEPLOYMENT_REVISION").unwrap_or_else(|_| "local".into()))
    .bind(state.retrieval_mode_str())
    // The SAME answer the admin page and the capability check give: this row
    // used to hard-code a third engine list (`flat-cosine`, a name no code
    // path produces) that disagreed with `compiled_engines()`.
    .bind(crate::state::compiled_engines())
    .bind(1i32)
    .bind(1i32)
    .bind(admission)
    .bind(readiness.blocking())
    .bind(parts.cache.used_bytes() as i64)
    .bind(0i64)
    .bind("ok")
    .execute(pool)
    .await;
    if let Err(e) = result {
        tracing::warn!(error = %e, "node heartbeat failed");
    }
}

/// Upsert one artifact's residency on THIS node. Failure is logged, never
/// fatal — the row is observability and gate evidence, not serving state.
#[allow(clippy::too_many_arguments)]
async fn record_residency(
    pool: &sqlx::PgPool,
    environment_id: &str,
    node_id: &str,
    tenant: &str,
    index_version_id: &str,
    artifact_id: &str,
    engine_id: &str,
    state: &str,
    local_bytes: Option<i64>,
) {
    let result = sqlx::query(
        "INSERT INTO index_artifact_residency_snapshots
             (environment_id, node_id, tenant_id, index_version_id, artifact_id,
              engine_id, residency_state, local_bytes, last_seen_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8, now())
         ON CONFLICT (environment_id, node_id, tenant_id, index_version_id, artifact_id)
         DO UPDATE SET residency_state = EXCLUDED.residency_state,
                       engine_id = EXCLUDED.engine_id,
                       local_bytes = EXCLUDED.local_bytes,
                       last_seen_at = now()",
    )
    .bind(environment_id)
    .bind(node_id)
    .bind(tenant)
    .bind(index_version_id)
    .bind(artifact_id)
    .bind(engine_id)
    .bind(state)
    .bind(local_bytes)
    .execute(pool)
    .await;
    if let Err(e) = result {
        tracing::warn!(error = %e, "residency snapshot upsert failed");
    }
}

/// The warmer loop: sweep, record, repeat. Spawned only in `datastore` mode.
pub fn spawn_warmer(state: &Arc<AppState>, parts: Arc<DatastoreParts>) {
    let interval_ms = std::env::var("MUNARIUM_DATASTORE_ROLLOUT_REFRESH_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(15_000u64)
        .max(1_000);
    let startup_deadline_ms = std::env::var("MUNARIUM_DATASTORE_STARTUP_HYDRATE_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120_000u64);

    let weak = Arc::downgrade(state);
    tokio::spawn(async move {
        let started = std::time::Instant::now();
        let mut alerted = false;
        let mut opened: HashSet<ArtifactCacheKey> = HashSet::new();
        loop {
            let Some(state) = weak.upgrade() else { return };
            if let Err(e) = warm_once(&state, &parts, &mut opened).await {
                tracing::warn!(error = %e, "datastore readiness sweep failed");
            }
            let readiness = state.datastore_readiness();
            if !readiness.admits()
                && !alerted
                && started.elapsed().as_millis() as u64 > startup_deadline_ms
            {
                // The deadline is an ALERT, not permission to admit traffic
                // (§9.2): the replica stays unready and keeps trying.
                alerted = true;
                tracing::error!(
                    blocking = ?readiness.blocking(),
                    "datastore startup hydration exceeded its deadline; replica remains unready"
                );
            }
            drop(state);
            tokio::time::sleep(std::time::Duration::from_millis(interval_ms)).await;
        }
    });
}

// ---------------------------------------------------------------------------
// The rollout selector API
// ---------------------------------------------------------------------------

/// Read one scope's selector row.
pub async fn op_rollout_get(
    state: &AppState,
    tenant: &str,
    scope_kind: &str,
    scope_id: &str,
) -> Result<Option<dto::RetrievalRolloutDto>> {
    let pool = require_pool(state)?;
    let selector = RolloutSelector::new(pool.clone(), tenant);
    Ok(selector
        .get(scope_kind, scope_id)
        .await?
        .map(|e| dto::RetrievalRolloutDto {
            scope_kind: e.scope_kind,
            scope_id: e.scope_id,
            serving: e.serving,
            prewarm_staged: e.prewarm_staged,
            required_versions_policy: e.required_versions_policy,
            generation: e.generation,
        }))
}

/// Create or update one scope's selector row — the operator's routing and
/// ROLLBACK lever.
///
/// Selecting `datastore` runs the §9.1 completeness gate first: every
/// serving-required version must hold a VERIFIED artifact in its `serving`
/// binding, or the CAS is refused naming what is missing. Rolling BACK to
/// `postgres` is deliberately ungated — the way out must never be harder than
/// the way in.
pub async fn op_rollout_set(
    state: &AppState,
    tenant: &str,
    req: &dto::RetrievalRolloutSetRequest,
    actor: &str,
) -> Result<dto::RetrievalRolloutDto> {
    let pool = require_pool(state)?;
    match req.serving.as_str() {
        "postgres" | "datastore" => {}
        other => {
            return Err(KernelError::InvalidInput(format!(
                "serving must be postgres or datastore, not {other:?}"
            )))
        }
    }
    let policy = match req.required_versions_policy.as_deref() {
        None => RequiredVersionsPolicy::ActivePinnedAndHorizon,
        Some(p) => {
            let parsed = RequiredVersionsPolicy::parse(p)?;
            if parsed != RequiredVersionsPolicy::ActivePinnedAndHorizon {
                // The schema stores a per-scope policy, but the selector's
                // write path does not set it yet; accepting one here and
                // silently keeping the default would be the configuration-
                // that-reads-as-though-it-does-something defect the stage 4
                // audit was about.
                return Err(KernelError::InvalidInput(format!(
                    "required_versions_policy {p:?} is not yet settable per scope; the default active_pinned_and_horizon applies"
                )));
            }
            parsed
        }
    };

    if req.serving == "datastore" {
        let retrieval = state.retrieval_for(tenant)?;
        let horizon =
            i64::try_from(state.datastore_capabilities().pin_horizon_secs).unwrap_or(i64::MAX);
        let required = match req.scope_kind.as_str() {
            "collection" => {
                retrieval
                    .reference()
                    .required_versions(&req.scope_id, policy, horizon)
                    .await?
            }
            "shape" => {
                retrieval
                    .reference()
                    .required_versions_for_shape(&req.scope_id, policy, horizon)
                    .await?
            }
            other => {
                return Err(KernelError::InvalidInput(format!(
                    "scope kind {other:?} cannot be datastore-served; the selector routes \
                     collections and legacy shapes"
                )))
            }
        };
        if required.is_empty() {
            return Err(KernelError::InvalidInput(format!(
                "{} {} has no serving-required versions (no active index); there is \
                 nothing the datastore could serve",
                req.scope_kind, req.scope_id
            )));
        }
        let catalog = ArtifactCatalog::new(pool.clone(), tenant);
        let mut missing = Vec::new();
        for version in &required {
            match catalog
                .binding(&version.index_version_id, BindingSlot::Serving)
                .await?
            {
                None => missing.push(format!("{}: no serving binding", version.index_version_id)),
                Some(b) => match catalog
                    .artifact(&version.index_version_id, &b.artifact_id)
                    .await?
                {
                    Some(row) if row.state == ArtifactState::Verified => {}
                    Some(row) => missing.push(format!(
                        "{}: artifact {} is {}",
                        version.index_version_id,
                        b.artifact_id,
                        row.state.as_str()
                    )),
                    None => missing.push(format!(
                        "{}: bound artifact {} is not catalogued",
                        version.index_version_id, b.artifact_id
                    )),
                },
            }
        }
        if !missing.is_empty() {
            return Err(KernelError::InvalidInput(format!(
                "serving-required set incomplete; backfill and bind before selecting: {}",
                missing.join("; ")
            )));
        }
    }

    let selector = RolloutSelector::new(pool.clone(), tenant);
    let change = RolloutChange {
        serving: &req.serving,
        prewarm_staged: req.prewarm_staged,
        changed_by: actor,
        reason: req.reason.as_deref(),
    };
    let entry = match req.expected_generation {
        None => {
            selector
                .create(&req.scope_kind, &req.scope_id, change)
                .await?
        }
        Some(expected) => selector
            .update(&req.scope_kind, &req.scope_id, change, expected)
            .await?
            .ok_or_else(|| {
                KernelError::InvalidInput(format!(
                    "selector for {}/{} is not at generation {expected}; re-read and decide again",
                    req.scope_kind, req.scope_id
                ))
            })?,
    };
    Ok(dto::RetrievalRolloutDto {
        scope_kind: entry.scope_kind,
        scope_id: entry.scope_id,
        serving: entry.serving,
        prewarm_staged: entry.prewarm_staged,
        required_versions_policy: entry.required_versions_policy,
        generation: entry.generation,
    })
}

fn require_pool(state: &AppState) -> Result<&sqlx::PgPool> {
    state.pg_pool().ok_or_else(|| {
        KernelError::InvalidInput("the rollout selector requires the postgres store".into())
    })
}

// ---------------------------------------------------------------------------
// REST handlers
// ---------------------------------------------------------------------------

type ApiResult<T> = std::result::Result<T, ApiError>;

/// GET /v1/retrieval-rollout/{scope_kind}/{scope_id}
#[utoipa::path(
    get,
    path = "/v1/retrieval-rollout/{scope_kind}/{scope_id}",
    tag = "retrieval-rollout",
    params(
        ("scope_kind" = String, Path, description = "collection | shape"),
        ("scope_id" = String, Path, description = "the scope's id")
    ),
    responses(
        (status = 200, description = "the selector row", body = dto::RetrievalRolloutDto),
        (status = 404, description = "no row: PostgreSQL serves this scope")
    )
)]
pub async fn rollout_get(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path((scope_kind, scope_id)): axum::extract::Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> ApiResult<axum::Json<dto::RetrievalRolloutDto>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    match op_rollout_get(&state, &ctx.tenant_id, &scope_kind, &scope_id).await? {
        Some(row) => Ok(axum::Json(row)),
        None => Err(ApiError::from(KernelError::NotFound {
            kind: "rollout entry",
            id: format!("{scope_kind}/{scope_id}"),
        })),
    }
}

/// PUT /v1/retrieval-rollout
#[utoipa::path(
    put,
    path = "/v1/retrieval-rollout",
    tag = "retrieval-rollout",
    request_body = dto::RetrievalRolloutSetRequest,
    responses(
        (status = 200, description = "the selector row after the change", body = dto::RetrievalRolloutDto),
        (status = 400, description = "incomplete serving-required set, bad serving value, or a stale generation")
    )
)]
pub async fn rollout_set(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    crate::rest::ProblemJson(req): crate::rest::ProblemJson<dto::RetrievalRolloutSetRequest>,
) -> ApiResult<axum::Json<dto::RetrievalRolloutDto>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_rw()?;
    Ok(axum::Json(
        op_rollout_set(&state, &ctx.tenant_id, &req, &format!("api:{}", ctx.role)).await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The admission truth table: no selected scopes = no dependency; selected
    /// scopes admit only when the sweep found nothing blocking; and the
    /// pre-sweep default NEVER admits a selected fleet on the strength of not
    /// having checked.
    #[test]
    fn admission_follows_the_sweep() {
        let r = DatastoreReadiness::default();
        // Before any sweep: the default records a blocker, but no selected
        // scopes -- a postgres-mode process must admit trivially.
        assert!(r.admits(), "no selected scopes means no dependency");

        r.record(2, vec!["abc:no-serving-binding".into()]);
        assert!(!r.admits(), "a blocked selected scope withdraws admission");
        assert_eq!(r.selected_scopes(), 2);

        r.record(2, Vec::new());
        assert!(r.admits(), "everything open admits");

        r.record(0, Vec::new());
        assert!(r.admits(), "deselection removes the dependency");
    }

    /// Missing infrastructure is a permanent, legible refusal.
    #[test]
    fn missing_infrastructure_never_admits() {
        let r = DatastoreReadiness::default();
        r.mark_infrastructure_missing();
        assert!(!r.admits());
        assert_eq!(
            r.blocking(),
            vec!["datastore-infrastructure-missing".to_string()]
        );
    }
}
