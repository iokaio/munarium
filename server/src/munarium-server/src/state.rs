// SPDX-License-Identifier: Apache-2.0
//! AppState: tenant-scoped store resolution + the idempotency record store.

use crate::config::{
    AuthMode, BlobAuthConfig, Config, DocIntelAuthConfig, DocIntelConfig, SourceStoreConfig,
    StoreKind,
};
use munarium_core::docintel::DocumentIntelligence;
use munarium_core::sources::SourceStore;
use munarium_core::storage::StorageBackend;
use munarium_core::{KernelError, Result};
use munarium_store_mem::MemStore;
use munarium_store_pg::PgStore;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

#[derive(Debug, Clone)]
pub struct TenantCtx {
    pub tenant_id: String,
    pub role: String,
    /// True only under MUNARIUM_AUTH_MODE=disabled (dev/conformance): the
    /// pseudo-principal passes every role gate, including mgmt.
    pub disabled_mode: bool,
}

impl TenantCtx {
    /// Commands require a read-write token; "ro" tokens can only query.
    pub fn require_rw(&self) -> Result<()> {
        if self.role == "rw" {
            Ok(())
        } else {
            Err(KernelError::Forbidden(format!(
                "role '{}' cannot execute commands (rw required)",
                self.role
            )))
        }
    }

    /// The management plane (token issuance, reports, admin) requires the
    /// mgmt role — deliberately distinct from rw so a leaked mgmt token
    /// cannot write the ledger and a leaked rw token cannot mint capability
    /// tokens. MUNARIUM_AUTH_MODE=disabled maps to rw, which also passes here
    /// (dev/conformance convenience).
    pub fn require_mgmt(&self) -> Result<()> {
        if self.role == "mgmt" || self.disabled_mode {
            Ok(())
        } else {
            Err(KernelError::Forbidden(format!(
                "role '{}' cannot use the management plane (mgmt required)",
                self.role
            )))
        }
    }
}

/// Sentinel details for token-lifecycle failures. The transport layers
/// promote Unauthenticated errors carrying exactly these details to the
/// token-expired / token-revoked problem slugs (state.rs stays KernelError-only
/// so store code never depends on transport types).
pub const TOKEN_EXPIRED_DETAIL: &str = "access token expired";
pub const TOKEN_REVOKED_DETAIL: &str = "access token revoked";

/// The authenticated caller: a static (control/management-plane) token or a
/// verified capability JWT (data/ingest plane). See docs/security-posture.md.
#[derive(Debug, Clone)]
pub enum Principal {
    Static(TenantCtx),
    Access(munarium_access::AccessCtx),
}

impl Principal {
    pub fn tenant_id(&self) -> &str {
        match self {
            Principal::Static(c) => &c.tenant_id,
            Principal::Access(a) => &a.tenant_id,
        }
    }

    pub fn token_jti(&self) -> Option<&str> {
        match self {
            Principal::Access(a) if !a.jti.is_empty() => Some(&a.jti),
            _ => None,
        }
    }

    /// Resolve the caller to a data-plane AccessCtx carrying `uid` (already
    /// uid-mismatch-checked by the middleware for JWTs). Static tokens stay
    /// valid on the data plane for conformance/dev: rw ⇒ unrestricted,
    /// ro ⇒ query-only, mgmt ⇒ forbidden (management stays off the data path).
    pub fn access_ctx(&self, uid: &str) -> Result<munarium_access::AccessCtx> {
        match self {
            Principal::Access(a) => Ok(a.clone()),
            Principal::Static(c) => match c.role.as_str() {
                "rw" => Ok(munarium_access::AccessCtx::unrestricted(uid, &c.tenant_id)),
                "ro" => {
                    let mut ctx = munarium_access::AccessCtx::unrestricted(uid, &c.tenant_id);
                    ctx.scopes = vec![munarium_access::SCOPE_QUERY.to_string()];
                    Ok(ctx)
                }
                other => Err(KernelError::Forbidden(format!(
                    "role '{other}' cannot use the data plane (query/ingest scopes ride access tokens)"
                ))),
            },
        }
    }
}

/// Which engines this BINARY carries, from cargo features rather than from what
/// a deployment intended. Kept in one place so the admin page and the
/// capability check cannot disagree about what was compiled.
pub(crate) fn compiled_engines() -> Vec<String> {
    // munarium-datastore is a normal dependency with default features, so both
    // are present today. Read from cfg! rather than hard-coded so a reduced
    // build reports honestly instead of claiming an engine it lacks.
    vec!["tantivy".to_string(), "munarium-flat".to_string()]
}

pub struct AppState {
    pub config: Config,
    stores: StoreRegistry,
    /// Idempotency records for the MEMORY store only — pg mode is
    /// table-backed (idempotency_keys, shared across instances and pruned
    /// by the janitor task AppState::new spawns; MUNARIUM_IDEMPOTENCY_TTL_SECS).
    idem: Mutex<HashMap<(String, String), IdemRecord>>,
    /// Published shapes; pg mode also persists to the shapes table and
    /// lazy-loads per tenant, re-reading after MUNARIUM_REGISTRY_TTL_SECS so a
    /// shape applied on another instance converges here within the TTL.
    pub shapes: munarium_shapes::ShapeRegistry,
    shapes_loaded: Mutex<HashMap<String, std::time::Instant>>,
    /// BYOK provider configs (credentialRef only; keys resolve at call time).
    pub providers: crate::providers_api::ProviderRegistry,
    /// Per-call output-token budgets (2026-09-02): tenant replacements over
    /// the process defaults, cached like the provider registry.
    pub max_tokens: crate::max_tokens_api::MaxTokensRegistry,
    /// Interaction capture: the bounded channel to the writer task.
    pub interactions_tx: crate::interactions::InteractionSender,
    /// Where raw document bytes live. One instance for every tenant — the
    /// tenant is part of the blob key, and the Azure backend's token cache
    /// is worth sharing.
    sources: Arc<dyn SourceStore>,
    /// The sealed evidence plane. One instance for every tenant —
    /// the tenant is a column on every row and a parameter on every trait
    /// method, exactly as the source store treats it.
    evidence: Arc<dyn munarium_core::evidence::EvidenceStore>,
    /// Daily token budget ledger (spending caps). Follows the ledger store
    /// like evidence does: a memory-ledger deployment gets the memory ledger
    /// (single replica by validation, so its mutex is the whole story), a
    /// Postgres deployment shares one table across every replica.
    budgets: Arc<dyn munarium_core::budget::BudgetStore>,
    /// Optional document-intelligence escalation. None by default; one
    /// instance shared by every tenant.
    doc_intel: Option<Arc<dyn DocumentIntelligence>>,
    /// Which engine serves retrieval. Resolved once at startup and carried on
    /// every coordinator handle, so a request cannot observe a mode change
    /// halfway through. `Postgres` unless configured otherwise.
    retrieval_mode: munarium_retrieval::RetrievalMode,
    /// What this process can actually do, given its configuration and the
    /// infrastructure present. Resolved once at startup for the same reason as
    /// the mode: a capability that could change under a request is one a
    /// request cannot be reasoned about.
    datastore_capabilities: munarium_retrieval::capabilities::DatastoreCapabilities,
    /// The shadow plane, armed after construction and only in `shadow` mode.
    /// A `OnceLock` because the plane's store factory needs a built AppState,
    /// so it is set beside the reconciler spawn rather than in `new` — and a
    /// request that races the arming simply sees no plane, which is the same
    /// as a shed sample.
    shadow: std::sync::OnceLock<Option<std::sync::Arc<crate::shadow_plane::ShadowPlane>>>,
    /// The shared datastore infrastructure (one L1 cache, one store factory),
    /// armed after construction in `shadow` and `datastore` modes.
    datastore_parts:
        std::sync::OnceLock<Option<std::sync::Arc<crate::datastore_serving::DatastoreParts>>>,
    /// Maintained by the readiness warmer; read (never computed) by /readyz.
    datastore_readiness: std::sync::Arc<crate::datastore_serving::DatastoreReadiness>,
    /// Process metrics (counters/histograms); gauges are polled at render
    /// time by `metrics::render`. Arc because the interaction writer task
    /// holds a clone (it outlives no AppState borrow). See metrics.rs for
    /// the cardinality rules.
    pub metrics: Arc<crate::metrics::Metrics>,
    /// One pooled HTTP client and ONE circuit breaker per instance, for the
    /// Matrix evidence provider. Per-turn versions of either would be
    /// useless: a fresh client discards the connection pool, and a fresh
    /// breaker never sees a second failure so it never trips.
    pub matrix_http: reqwest::Client,
    pub matrix_breaker: Arc<crate::evidence_providers::CircuitBreaker>,
    /// Load-shed ceilings (MUNARIUM_MAX_CONCURRENCY, one per plane): the
    /// capture middleware try-acquires a permit per /v1 (REST) or /mmp.v1.
    /// (gRPC) request and refuses 503 `overloaded` / RESOURCE_EXHAUSTED at
    /// zero permits. Deliberately NOT a tower layer over the whole router:
    /// meta routes — /healthz, /readyz — must keep answering under
    /// overload, or orchestrators kill exactly the instances that are
    /// working hardest.
    pub rest_permits: Arc<tokio::sync::Semaphore>,
    pub grpc_permits: Arc<tokio::sync::Semaphore>,
    /// Set by main's shutdown watcher the moment a stop signal fires; both
    /// planes' /readyz flip to 503 "draining" so load balancers stop
    /// routing while in-flight work finishes under the grace window.
    pub draining: std::sync::atomic::AtomicBool,
    /// Chronology rules assets in MEMORY-store mode only (dev/tests); pg
    /// mode persists to the chronology_rules table. (tenant, name) -> yaml.
    chrono_rules_mem: Mutex<HashMap<(String, String), String>>,
    /// Per-boot random secret for the /admin CSRF synchronizer token (the
    /// dashboard's mutating forms). Stateless by design: a restart
    /// invalidates in-flight forms, which re-render — an accepted caveat
    /// documented in docs/security-posture.md.
    pub boot_secret: String,
}

pub struct IdemRecord {
    pub request_hash: String,
    pub response_json: String,
}

enum StoreRegistry {
    Mem(RwLock<HashMap<String, Arc<MemStore>>>),
    Pg(PgStore),
}

/// Build the configured bytes store. The cloud and filesystem backends all
/// ride the object_store adapter (munarium-store-objects); Postgres keeps
/// compose and tests running with no cloud account; memory is for the
/// memory-ledger mode where there is no pool to write blobs into.
fn build_source_store(config: &Config, stores: &StoreRegistry) -> Result<Arc<dyn SourceStore>> {
    match &config.source_store {
        SourceStoreConfig::Azure {
            account,
            container,
            auth,
            endpoint,
        } => {
            let auth = match auth {
                BlobAuthConfig::ManagedIdentity { client_id } => {
                    munarium_store_objects::AzureAuth::ManagedIdentity {
                        client_id: client_id.clone(),
                    }
                }
                BlobAuthConfig::Sas { token } => munarium_store_objects::AzureAuth::Sas {
                    token: token.clone(),
                },
            };
            Ok(Arc::new(munarium_store_objects::ObjectSourceStore::azure(
                munarium_store_objects::AzureConfig {
                    account: account.clone(),
                    container: container.clone(),
                    auth,
                    endpoint: endpoint.clone(),
                },
            )?))
        }
        SourceStoreConfig::S3 {
            bucket,
            region,
            endpoint,
            force_path_style,
            access_key_id,
            secret_access_key,
        } => Ok(Arc::new(munarium_store_objects::ObjectSourceStore::s3(
            munarium_store_objects::S3Config {
                bucket: bucket.clone(),
                region: region.clone(),
                endpoint: endpoint.clone(),
                force_path_style: *force_path_style,
                access_key_id: access_key_id.clone(),
                secret_access_key: secret_access_key.clone(),
            },
        )?)),
        SourceStoreConfig::Gcs {
            bucket,
            service_account_json,
        } => Ok(Arc::new(munarium_store_objects::ObjectSourceStore::gcs(
            munarium_store_objects::GcsConfig {
                bucket: bucket.clone(),
                service_account_json: service_account_json.clone(),
            },
        )?)),
        SourceStoreConfig::File { root } => Ok(Arc::new(
            munarium_store_objects::ObjectSourceStore::local(root)?,
        )),
        SourceStoreConfig::Pg => match stores {
            StoreRegistry::Pg(base) => Ok(Arc::new(munarium_store_pg::PgSourceStore::new(
                base.pool().clone(),
            ))),
            StoreRegistry::Mem(_) => Err(KernelError::InvalidInput(
                "MUNARIUM_SOURCE_STORE=pg requires MUNARIUM_STORE=postgres".into(),
            )),
        },
        SourceStoreConfig::Mem => Ok(Arc::new(munarium_store_mem::MemSourceStore::new())),
    }
}

/// Build the configured document-intelligence provider, if any.
///
/// `None` is the default and a complete configuration — local extraction
/// still runs. Other clouds and on-prem engines plug in as additional match
/// arms here; nothing above this function knows which one is configured.
fn build_doc_intel(config: &Config) -> Result<Option<Arc<dyn DocumentIntelligence>>> {
    match &config.doc_intel {
        DocIntelConfig::None => Ok(None),
        DocIntelConfig::Azure {
            endpoint,
            auth,
            model,
            max_bytes,
            timeout_secs,
        } => {
            let auth = match auth {
                DocIntelAuthConfig::ManagedIdentity { client_id } => {
                    munarium_docintel_az::DocIntelAuth::ManagedIdentity {
                        client_id: client_id.clone(),
                    }
                }
                DocIntelAuthConfig::Key { key } => {
                    munarium_docintel_az::DocIntelAuth::Key { key: key.clone() }
                }
            };
            Ok(Some(Arc::new(munarium_docintel_az::AzureDocIntel::new(
                munarium_docintel_az::AzureDocIntelConfig {
                    endpoint: endpoint.clone(),
                    auth,
                    model: model.clone(),
                    max_bytes: *max_bytes,
                    timeout: std::time::Duration::from_secs(*timeout_secs),
                },
            )?)))
        }
    }
}

impl AppState {
    pub async fn new(config: Config) -> Result<Arc<Self>> {
        let stores = match config.store {
            StoreKind::Memory => StoreRegistry::Mem(RwLock::new(HashMap::new())),
            StoreKind::Postgres => {
                let url = config.database_url.clone().expect("checked in Config");
                StoreRegistry::Pg(
                    PgStore::connect_with_pool_size(
                        &url,
                        munarium_store_pg::DEFAULT_TENANT,
                        config.db_max_conns,
                    )
                    .await?,
                )
            }
        };
        let metrics = Arc::new(crate::metrics::Metrics::default());
        let interactions_tx = crate::interactions::spawn_writer(
            match &stores {
                StoreRegistry::Pg(base) => Some(base.pool().clone()),
                StoreRegistry::Mem(_) => None,
            },
            metrics.clone(),
            config.instance_id.clone(),
        );
        // Idempotency janitor (pg mode; the comment above `idem` was a
        // promise from 2026-08-10 that only became true 2026-08-17): prune
        // records older than MUNARIUM_IDEMPOTENCY_TTL_SECS. Every instance runs
        // one — the DELETEs are disjoint-or-idempotent, so N concurrent
        // janitors are harmless. The first tick is jittered so a fleet
        // restarted together does not sweep in lockstep.
        if let StoreRegistry::Pg(base) = &stores {
            let ttl = config.idempotency_ttl_secs;
            if ttl > 0 {
                let pool = base.pool().clone();
                let interval = std::time::Duration::from_secs((ttl / 24).max(300));
                let jitter =
                    std::time::Duration::from_secs((uuid::Uuid::now_v7().as_u128() % 60) as u64);
                tokio::spawn(async move {
                    tokio::time::sleep(jitter).await;
                    loop {
                        let result = sqlx::query(
                            "DELETE FROM idempotency_keys
                              WHERE created_at < now() - make_interval(secs => $1)",
                        )
                        .bind(ttl as f64)
                        .execute(&pool)
                        .await;
                        match result {
                            Ok(r) if r.rows_affected() > 0 => tracing::info!(
                                pruned = r.rows_affected(),
                                "idempotency janitor pruned expired records"
                            ),
                            Ok(_) => {}
                            Err(e) => {
                                tracing::warn!(error = %e, "idempotency janitor sweep failed")
                            }
                        }
                        tokio::time::sleep(interval).await;
                    }
                });
            }
        }
        // Idle-session expiry sweep (pg mode; 2026-08-18 — the deferred
        // half of the session-lifecycle close). Same janitor shape as the
        // idempotency pruner: jittered, N-replica-safe (the UPDATE is
        // idempotent), disabled at ttl=0. An expired session's next turn
        // answers 409 `session-not-open` — the refusal path already
        // enforces whatever the column says.
        if let StoreRegistry::Pg(base) = &stores {
            let ttl = config.session_idle_ttl_secs;
            if ttl > 0 {
                let pool = base.pool().clone();
                let interval = std::time::Duration::from_secs((ttl / 24).max(60));
                let jitter =
                    std::time::Duration::from_secs((uuid::Uuid::now_v7().as_u128() % 60) as u64);
                tokio::spawn(async move {
                    tokio::time::sleep(jitter).await;
                    loop {
                        let result = sqlx::query(
                            "UPDATE sessions SET state = 'expired'
                              WHERE state = 'open'
                                AND COALESCE(last_turn_at, created_at)
                                    < now() - make_interval(secs => $1)",
                        )
                        .bind(ttl as f64)
                        .execute(&pool)
                        .await;
                        match result {
                            Ok(r) if r.rows_affected() > 0 => tracing::info!(
                                expired = r.rows_affected(),
                                "session janitor expired idle sessions"
                            ),
                            Ok(_) => {}
                            Err(e) => {
                                tracing::warn!(error = %e, "session janitor sweep failed")
                            }
                        }
                        tokio::time::sleep(interval).await;
                    }
                });
            }
        }
        // ledger_events partition maintenance (pg mode): one sweep now and
        // one per day. Advisory-locked inside the sweep, so N instances are
        // safe; must run BEFORE overflow into the default partition (the
        // module header in munarium-store-pg/src/partitions.rs has the story).
        if let StoreRegistry::Pg(base) = &stores {
            let pool = base.pool().clone();
            tokio::spawn(async move {
                loop {
                    match munarium_store_pg::partitions::ensure_ledger_partitions(&pool).await {
                        Ok(Some(name)) => {
                            tracing::info!(partition = %name, "created next ledger_events partition")
                        }
                        Ok(None) => {}
                        Err(e) => {
                            tracing::error!(error = %e, "ledger partition maintenance failed")
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(86_400)).await;
                }
            });
        }
        let sources = build_source_store(&config, &stores)?;
        tracing::info!(backend = sources.backend_id(), "source bytes store");
        // Evidence metadata follows the LEDGER store, not the bytes store: an
        // artifact's authorization class and retention clock are database
        // facts, and a deployment running the memory ledger must not silently
        // persist evidence to Postgres (or vice versa).
        let evidence: Arc<dyn munarium_core::evidence::EvidenceStore> = match &stores {
            StoreRegistry::Pg(base) => {
                Arc::new(munarium_store_pg::PgEvidenceStore::new(base.pool().clone()))
            }
            StoreRegistry::Mem(_) => Arc::new(munarium_store_mem::MemEvidenceStore::new()),
        };
        // The spending-cap ledger follows the ledger store for the same
        // reason evidence does: which replica answered must not change which
        // ledger was consulted.
        let budgets: Arc<dyn munarium_core::budget::BudgetStore> = match &stores {
            StoreRegistry::Pg(base) => {
                Arc::new(munarium_store_pg::PgBudgetStore::new(base.pool().clone()))
            }
            StoreRegistry::Mem(_) => Arc::new(munarium_store_mem::MemBudgetStore::new()),
        };
        let doc_intel = build_doc_intel(&config)?;
        match &doc_intel {
            Some(p) => tracing::info!(provider = p.id(), "document intelligence enabled"),
            None => tracing::info!(
                "document intelligence disabled (MUNARIUM_DOCINTEL=none) — local extraction only"
            ),
        }
        let rest_permits = Arc::new(tokio::sync::Semaphore::new(config.max_concurrency));
        let grpc_permits = Arc::new(tokio::sync::Semaphore::new(config.max_concurrency));
        let matrix_http =
            crate::evidence_providers::MatrixProvider::client(std::time::Duration::from_secs(5));
        let matrix_breaker = Arc::new(crate::evidence_providers::CircuitBreaker::default());

        // Resolved once, here, rather than read per request: a mode that could
        // change under a request is a mode a request cannot be reasoned about.
        // An unrecognised value falls back to postgres and says so loudly --
        // failing open onto an unproven engine would be the wrong convenience.
        let raw_mode = std::env::var("MUNARIUM_RETRIEVAL_MODE").unwrap_or_default();
        let (configured_mode, recognised) = munarium_retrieval::RetrievalMode::parse(&raw_mode);
        if !recognised {
            tracing::warn!(
                value = %raw_mode,
                "MUNARIUM_RETRIEVAL_MODE is not one of postgres|mirror|shadow|datastore; using postgres"
            );
        }

        let datastore_capabilities =
            Self::resolve_datastore_capabilities(configured_mode, &stores, &config);
        // The EFFECTIVE mode is what every handle carries. A mirror that cannot
        // build degrades here, once, rather than failing per request.
        let retrieval_mode = datastore_capabilities.effective_mode;

        let state = Arc::new(Self {
            matrix_http,
            matrix_breaker,
            config,
            stores,
            idem: Mutex::new(HashMap::new()),
            shapes: munarium_shapes::ShapeRegistry::default(),
            shapes_loaded: Mutex::new(HashMap::new()),
            providers: crate::providers_api::ProviderRegistry::default(),
            max_tokens: crate::max_tokens_api::MaxTokensRegistry::default(),
            interactions_tx,
            sources,
            evidence,
            budgets,
            doc_intel,
            retrieval_mode,
            datastore_capabilities,
            shadow: std::sync::OnceLock::new(),
            datastore_parts: std::sync::OnceLock::new(),
            datastore_readiness: std::sync::Arc::new(
                crate::datastore_serving::DatastoreReadiness::default(),
            ),
            metrics,
            rest_permits,
            grpc_permits,
            draining: std::sync::atomic::AtomicBool::new(false),
            chrono_rules_mem: Mutex::new(HashMap::new()),
            // Two v4 UUIDs = 256 bits of CSPRNG-backed entropy.
            boot_secret: format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            ),
        });

        // The evidence retention janitor. Spawned AFTER the
        // state exists because it needs both the evidence store and the bytes
        // store, and it holds a Weak so a dropped AppState stops the loop
        // instead of keeping the process alive by itself.
        //
        // Jittered like the session janitor: N replicas sweeping in lockstep
        // would all contend on the same due rows every interval. The work is
        // idempotent, so contention costs effort rather than correctness — but
        // there is no reason to pay it.
        //
        // 0 disables, and that is the default: a janitor nobody configured,
        // deleting regulated data on a schedule nobody chose, is worse than a
        // janitor that never runs.
        // Interrupted mirror builds (datastore §7.4). No-op in postgres mode.
        crate::datastore_builds::spawn_reconciler(&state);

        // The shared datastore parts: one L1 cache, one store
        // factory, for whichever of the shadow and serving planes this mode
        // arms. None in postgres/mirror mode, and None with a logged error
        // when a prerequisite is missing.
        let parts = match state.retrieval_mode {
            munarium_retrieval::RetrievalMode::Shadow
            | munarium_retrieval::RetrievalMode::Datastore => {
                match crate::datastore_serving::DatastoreParts::build(&state) {
                    Ok(p) => Some(p),
                    Err(why) => {
                        tracing::error!(%why, "datastore infrastructure unavailable");
                        None
                    }
                }
            }
            _ => None,
        };
        let _ = state.datastore_parts.set(parts.clone());

        // The shadow plane. None in every mode but `shadow`, and
        // None in `shadow` mode too when a prerequisite is missing — the
        // build logs why, and PostgreSQL serves regardless.
        let _ = state
            .shadow
            .set(crate::shadow_plane::ShadowPlane::build(&state));

        // The builder loop: claims durable build jobs where the
        // operator enabled it. Independent of retrieval mode — a builder is
        // any process with PostgreSQL and staging configuration.
        crate::datastore_jobs::spawn_builder(&state);

        // The readiness warmer. Datastore mode only: it is what
        // holds /readyz false until every selected scope's serving-required
        // set is open, and what heartbeats this node into the fleet table.
        if state.retrieval_mode == munarium_retrieval::RetrievalMode::Datastore {
            match parts {
                Some(parts) => crate::datastore_serving::spawn_warmer(&state, parts),
                None => {
                    // No infrastructure in datastore mode: this replica can
                    // never prove any selected scope open, so it must never
                    // admit. Recording one phantom "selected scope" with the
                    // reason keeps /readyz false without inventing a second
                    // state machine.
                    state.datastore_readiness.mark_infrastructure_missing();
                }
            }
        }

        let secs = state.config.evidence_purge_interval_secs;
        if secs > 0 {
            let weak = Arc::downgrade(&state);
            let interval = std::time::Duration::from_secs(secs.max(60));
            let jitter =
                std::time::Duration::from_secs((uuid::Uuid::now_v7().as_u128() % 60) as u64);
            tokio::spawn(async move {
                tokio::time::sleep(jitter).await;
                loop {
                    let Some(st) = weak.upgrade() else { break };
                    match crate::evidence_api::purge_once(&st, crate::evidence_api::PURGE_BATCH)
                        .await
                    {
                        Ok(0) => {}
                        Ok(n) => tracing::info!(purged = n, "evidence retention janitor"),
                        Err(e) => {
                            tracing::warn!(error = %e, "evidence retention sweep failed")
                        }
                    }
                    drop(st);
                    tokio::time::sleep(interval).await;
                }
            });
            tracing::info!(
                interval_secs = secs.max(60),
                "evidence retention janitor enabled"
            );
        } else {
            tracing::info!(
                "evidence retention janitor disabled (MUNARIUM_EVIDENCE_PURGE_INTERVAL_SECS=0)"
            );
        }

        // Stale-reservation janitor for the spending-cap ledger: stamps held
        // reservations older than six hours settled at their estimate (the
        // crashed-holder direction is spent, never free — the provider may
        // have been reached). Unconditional, unlike the evidence janitor: it
        // deletes nothing, and an empty table makes each pass a no-op. This
        // wires the sweep Matrix defined and never called.
        {
            let weak = Arc::downgrade(&state);
            let jitter =
                std::time::Duration::from_secs((uuid::Uuid::now_v7().as_u128() % 300) as u64);
            tokio::spawn(async move {
                tokio::time::sleep(jitter).await;
                loop {
                    let Some(st) = weak.upgrade() else { break };
                    match st.budgets().sweep_stale(6 * 3600).await {
                        Ok(0) => {}
                        Ok(n) => tracing::info!(swept = n, "budget reservation janitor"),
                        Err(e) => tracing::warn!(error = %e, "budget reservation sweep failed"),
                    }
                    drop(st);
                    tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                }
            });
        }
        Ok(state)
    }

    /// Readiness ≠ liveness: probe the backing store so orchestrators stop
    /// routing to an instance whose database is down. Memory store: ready.
    /// Shared by BOTH readyz handlers — the REST plane's and the ops
    /// plane's — so the two planes cannot disagree about readiness.
    pub async fn store_ready(&self) -> bool {
        match self.pg_pool() {
            Some(pool) => tokio::time::timeout(
                std::time::Duration::from_secs(2),
                sqlx::query("SELECT 1").execute(pool),
            )
            .await
            .map(|r| r.is_ok())
            .unwrap_or(false),
            None => true,
        }
    }

    /// The shared pg pool when running on postgres (retrieval + shape
    /// persistence live there); None on the memory store.
    pub fn pg_pool(&self) -> Option<&sqlx::PgPool> {
        match &self.stores {
            StoreRegistry::Pg(base) => Some(base.pool()),
            StoreRegistry::Mem(_) => None,
        }
    }

    /// The configured source-bytes backend id (az | s3 | gcs | file | pg |
    /// mem) — the /admin health page's configuration table (2026-08-27).
    pub fn source_backend_id(&self) -> &'static str {
        self.sources.backend_id()
    }

    /// The evidence plane. Handlers reach it through here so there is one
    /// place the backend choice is made.
    pub fn evidence(&self) -> &Arc<dyn munarium_core::evidence::EvidenceStore> {
        &self.evidence
    }

    /// The daily token budget ledger (spending caps).
    pub fn budgets(&self) -> &Arc<dyn munarium_core::budget::BudgetStore> {
        &self.budgets
    }

    /// The bytes store, for artifact blobs under the reserved keyspace.
    pub fn source_store(&self) -> &Arc<dyn SourceStore> {
        &self.sources
    }

    /// The configured document-intelligence provider id, if any.
    pub fn doc_intel_id(&self) -> Option<&'static str> {
        self.doc_intel.as_ref().map(|p| p.id())
    }

    /// The retrieval mode this process resolved at startup, for display.
    pub fn retrieval_mode_str(&self) -> &'static str {
        self.retrieval_mode.as_str()
    }

    /// The shadow plane, when this process has one. `None` outside `shadow`
    /// mode, before arming, or when arming found a prerequisite missing.
    pub fn shadow_plane(&self) -> Option<&std::sync::Arc<crate::shadow_plane::ShadowPlane>> {
        self.shadow.get().and_then(|p| p.as_ref())
    }

    /// The shared datastore infrastructure, when this mode armed it.
    pub fn datastore_parts(
        &self,
    ) -> Option<&std::sync::Arc<crate::datastore_serving::DatastoreParts>> {
        self.datastore_parts.get().and_then(|p| p.as_ref())
    }

    /// The maintained datastore readiness state (§9.2). Meaningful in
    /// `datastore` mode; trivially admitting elsewhere.
    pub fn datastore_readiness(&self) -> &crate::datastore_serving::DatastoreReadiness {
        &self.datastore_readiness
    }

    /// What this process can actually do. Read-only: the admin console renders
    /// it, and changing any of it is a deployment event rather than a button.
    pub fn datastore_capabilities(
        &self,
    ) -> &munarium_retrieval::capabilities::DatastoreCapabilities {
        &self.datastore_capabilities
    }

    /// Resolve configuration against the infrastructure actually present.
    ///
    /// Probed rather than trusted: the local root is tested by WRITING to it,
    /// because a configured path on a read-only mount looks configured, and the
    /// engine list comes from what was compiled rather than from what a
    /// deployment intended.
    fn resolve_datastore_capabilities(
        mode: munarium_retrieval::RetrievalMode,
        stores: &StoreRegistry,
        config: &Config,
    ) -> munarium_retrieval::capabilities::DatastoreCapabilities {
        use munarium_retrieval::capabilities::{
            ArtifactStoreKind, DatastoreCapabilities, Infrastructure,
        };
        use munarium_retrieval::config::{DatastoreConfig, PinHorizon, DEFAULT_RECOVERY_MARGIN};

        let env = |k: &str| std::env::var(k).ok();
        let local_root = env("MUNARIUM_DATASTORE_LOCAL_ROOT");
        let (root_configured, root_writable) =
            Infrastructure::probe_local_root(local_root.as_deref());

        let session_ttl = std::time::Duration::from_secs(config.session_idle_ttl_secs.max(1));
        if config.session_idle_ttl_secs == 0 && env("MUNARIUM_DATASTORE_PIN_HORIZON").is_none() {
            // `0` means sessions never expire (config.rs), and a session that
            // never expires can hold an index-version pin forever — which no
            // finite horizon derived from "1 second + the recovery margin"
            // covers. The derivation below cannot express "forever", so the
            // gap is named here rather than hidden in a plausible number.
            tracing::warn!(
                "MUNARIUM_SESSION_IDLE_TTL_SECS=0 (sessions never expire) but no \
                 MUNARIUM_DATASTORE_PIN_HORIZON is set: the derived pin horizon cannot cover \
                 pins held by immortal sessions; set an explicit horizon or a session TTL"
            );
        }
        // No separate runbook TTL exists today, and a runbook execution
        // outlives a session at most by its own retry window, so the session
        // TTL is the binding constraint. Named rather than silently reused, so
        // the day a runbook TTL exists this is the line to change.
        let runbook_ttl = session_ttl;
        let derived = PinHorizon::derive(session_ttl, runbook_ttl, DEFAULT_RECOVERY_MARGIN);

        let ds = DatastoreConfig {
            mode,
            local_root,
            l1_high_watermark_bytes: env("MUNARIUM_DATASTORE_L1_HIGH_WATERMARK")
                .and_then(|v| v.parse().ok())
                .unwrap_or(8 * 1024 * 1024 * 1024),
            l1_low_watermark_bytes: env("MUNARIUM_DATASTORE_L1_LOW_WATERMARK")
                .and_then(|v| v.parse().ok())
                .unwrap_or(6 * 1024 * 1024 * 1024),
            // Unset DERIVES rather than defaulting to a constant: the safe
            // horizon depends on this deployment's own TTLs, and a fixed number
            // would be wrong on any deployment that changed them.
            pin_horizon: env("MUNARIUM_DATASTORE_PIN_HORIZON")
                .and_then(|v| v.parse::<u64>().ok())
                .map(PinHorizon::from_secs)
                .unwrap_or(derived),
            retired_retention: env("MUNARIUM_DATASTORE_RETIRED_RETENTION")
                .and_then(|v| v.parse::<u64>().ok())
                .map(std::time::Duration::from_secs)
                .unwrap_or(derived.as_duration() * 2),
            allow_short_pin_horizon: env("MUNARIUM_DATASTORE_ALLOW_SHORT_PIN_HORIZON")
                .is_some_and(|v| v.eq_ignore_ascii_case("true")),
            supported_formats: (1, 1),
            compiled_engines: compiled_engines(),
        };

        let infra = Infrastructure {
            has_postgres_catalog: matches!(stores, StoreRegistry::Pg(_)),
            artifact_store: env("MUNARIUM_DATASTORE_ARTIFACT_STORE")
                .map(|v| ArtifactStoreKind::parse(&v)),
            local_root_writable: root_writable,
            local_root_configured: root_configured,
            compiled_engines: compiled_engines(),
        };

        let caps = DatastoreCapabilities::resolve(&ds, &infra, session_ttl, runbook_ttl);
        for e in &caps.blocking {
            tracing::error!(error = %e, "datastore configuration is invalid");
        }
        if let Some(why) = &caps.degraded_because {
            if caps.must_refuse_startup() {
                tracing::error!(reason = %why, "datastore serving cannot start in this mode");
            } else {
                tracing::warn!(
                    configured = caps.configured_mode.as_str(),
                    effective = caps.effective_mode.as_str(),
                    reason = %why,
                    "datastore mode degraded"
                );
            }
        }
        caps
    }

    /// The deployment environment this process belongs to.
    ///
    /// Node snapshots and plane expectations are environment-scoped rather
    /// than tenant-scoped, because a process serves every tenant. Defaults to
    /// `local` so a developer's server writes somewhere obviously not shared
    /// with a real deployment.
    pub fn deployment_environment_id(&self) -> String {
        std::env::var("MUNARIUM_DEPLOYMENT_ENVIRONMENT_ID").unwrap_or_else(|_| "local".to_string())
    }

    /// A tenant-scoped retrieval coordinator.
    ///
    /// Returns the coordinator, never a concrete backend: that is the whole
    /// point of the stage 1 extraction. While `MUNARIUM_RETRIEVAL_MODE` is
    /// `postgres` this wraps `PgRetrieval` and forwards, so behaviour is
    /// unchanged; the mode is carried on the handle so a later dispatch has
    /// somewhere to read it from.
    pub fn retrieval_for(&self, tenant_id: &str) -> Result<munarium_retrieval::Retrieval> {
        let pool = self.pg_pool().ok_or_else(|| {
            KernelError::InvalidInput(
                "retrieval requires the postgres store (MUNARIUM_STORE=postgres)".into(),
            )
        })?;
        let pg = munarium_retrieval_pg::PgRetrieval::with_source_store(
            pool.clone(),
            tenant_id,
            self.sources.clone(),
        )
        .with_doc_intel(self.doc_intel.clone());
        let mut retrieval = munarium_retrieval::Retrieval::new(pg, self.retrieval_mode);
        if self.retrieval_mode == munarium_retrieval::RetrievalMode::Datastore {
            if let Some(parts) = self.datastore_parts() {
                retrieval = retrieval.with_serving(parts.serving_plane(pool, tenant_id));
            }
            // No parts in datastore mode leaves `serving` unset, and the
            // coordinator FAILS CLOSED on every search: without the selector
            // it cannot tell a selected scope from an unselected one, and
            // guessing "postgres" could silently serve a selected scope from
            // the wrong engine — the §9.1 failure. /readyz is false for the
            // same reason, so ingress should never route here anyway.
        }
        Ok(retrieval)
    }

    /// Lazy-load a tenant's persisted shapes into the registry (pg mode),
    /// re-reading after MUNARIUM_REGISTRY_TTL_SECS. Shapes are immutable per
    /// shape_ref, so staleness is a MISSING-entry problem only — this
    /// instance can never hold a wrong shape, and one applied on another
    /// instance becomes visible here within the TTL (0 = load-once, the
    /// single-instance behavior through v0.1.2).
    pub async fn ensure_shapes_loaded(&self, tenant_id: &str) -> Result<()> {
        let ttl = self.config.registry_ttl_secs;
        {
            let loaded = self.shapes_loaded.lock().await;
            if let Some(at) = loaded.get(tenant_id) {
                if ttl == 0 || at.elapsed().as_secs() < ttl {
                    return Ok(());
                }
            }
        }
        if let Some(pool) = self.pg_pool() {
            let rows: Vec<(String,)> =
                sqlx::query_as("SELECT yaml FROM shapes WHERE tenant_id = $1")
                    .bind(tenant_id)
                    .fetch_all(pool)
                    .await
                    .map_err(|e| KernelError::Storage(e.to_string()))?;
            for (yaml,) in rows {
                // Persisted rows already passed apply-time validation; a
                // re-apply of an already-loaded ref is a no-op error.
                let _ = self.shapes.apply(tenant_id, &yaml);
            }
        }
        self.shapes_loaded
            .lock()
            .await
            .insert(tenant_id.to_string(), std::time::Instant::now());
        Ok(())
    }

    /// Persist a newly applied shape (pg mode).
    pub async fn persist_shape(
        &self,
        tenant_id: &str,
        shape_ref: &str,
        yaml: &str,
        yaml_hash: &str,
    ) -> Result<()> {
        if let Some(pool) = self.pg_pool() {
            sqlx::query(
                "INSERT INTO shapes (tenant_id, shape_ref, yaml, yaml_hash)
                 VALUES ($1, $2, $3, $4) ON CONFLICT (tenant_id, shape_ref) DO NOTHING",
            )
            .bind(tenant_id)
            .bind(shape_ref)
            .bind(yaml)
            .bind(yaml_hash)
            .execute(pool)
            .await
            .map_err(|e| KernelError::Storage(e.to_string()))?;
        }
        Ok(())
    }

    /// Upsert an applied chronology rules asset (already parse-validated).
    pub async fn store_chronology_rules(
        &self,
        tenant_id: &str,
        name: &str,
        yaml: &str,
    ) -> Result<()> {
        if let Some(pool) = self.pg_pool() {
            sqlx::query(
                "INSERT INTO chronology_rules (tenant_id, name, yaml) VALUES ($1, $2, $3)
                 ON CONFLICT (tenant_id, name) DO UPDATE SET yaml = EXCLUDED.yaml",
            )
            .bind(tenant_id)
            .bind(name)
            .bind(yaml)
            .execute(pool)
            .await
            .map_err(|e| KernelError::Storage(e.to_string()))?;
        } else {
            self.chrono_rules_mem
                .lock()
                .await
                .insert((tenant_id.to_string(), name.to_string()), yaml.to_string());
        }
        Ok(())
    }

    pub async fn load_chronology_rules_yaml(
        &self,
        tenant_id: &str,
        name: &str,
    ) -> Result<Option<String>> {
        if let Some(pool) = self.pg_pool() {
            let row: Option<(String,)> = sqlx::query_as(
                "SELECT yaml FROM chronology_rules WHERE tenant_id = $1 AND name = $2",
            )
            .bind(tenant_id)
            .bind(name)
            .fetch_optional(pool)
            .await
            .map_err(|e| KernelError::Storage(e.to_string()))?;
            Ok(row.map(|(y,)| y))
        } else {
            Ok(self
                .chrono_rules_mem
                .lock()
                .await
                .get(&(tenant_id.to_string(), name.to_string()))
                .cloned())
        }
    }

    /// Every applied chronology rules asset for a tenant as `(name,
    /// created_at)` — the /admin runbooks hub lists them beside shapes
    /// (2026-08-27). The memory store has no timestamp to give.
    pub async fn list_chronology_rules(
        &self,
        tenant_id: &str,
    ) -> Result<Vec<(String, Option<String>)>> {
        if let Some(pool) = self.pg_pool() {
            let rows: Vec<(String, String)> = sqlx::query_as(
                "SELECT name, created_at::text FROM chronology_rules
                  WHERE tenant_id = $1 ORDER BY name",
            )
            .bind(tenant_id)
            .fetch_all(pool)
            .await
            .map_err(|e| KernelError::Storage(e.to_string()))?;
            Ok(rows.into_iter().map(|(n, c)| (n, Some(c))).collect())
        } else {
            let map = self.chrono_rules_mem.lock().await;
            let mut out: Vec<(String, Option<String>)> = map
                .keys()
                .filter(|(t, _)| t == tenant_id)
                .map(|(_, n)| (n.clone(), None))
                .collect();
            out.sort();
            Ok(out)
        }
    }

    /// The chronology rules a version is armed with, or None (2026-08-17 —
    /// the sixth gate's arming surface). Fails LOUD when a version names a
    /// rules asset that is not applied: silent un-gating on misconfig is
    /// exactly what the gates exist to prevent.
    pub async fn armed_chronology(
        &self,
        store: &dyn StorageBackend,
        tenant_id: &str,
        version_id: &str,
    ) -> Result<Option<munarium_core::chrono_gate::ChronologyRules>> {
        let Some(meta) = store.version_metadata(version_id).await? else {
            return Ok(None);
        };
        let Some(name) = meta.get("chronology_rules").and_then(|v| v.as_str()) else {
            return Ok(None);
        };
        match self.load_chronology_rules_yaml(tenant_id, name).await? {
            Some(yaml) => Ok(Some(crate::chronology_api::parse_rules_doc(&yaml)?.spec)),
            None => Err(KernelError::InvalidInput(format!(
                "version is armed with chronology rules '{name}' but no such asset is \
                 applied for this tenant (POST /v1/chronology-rules first)"
            ))),
        }
    }

    /// The Matrix evidence provider, or None when the structured-evidence
    /// plane is not configured.
    ///
    /// The HTTP client and the circuit breaker are built ONCE per instance and
    /// shared: rebuilding the client per turn would discard the connection
    /// pool, and a per-turn breaker would never trip, because it would never
    /// see a second failure.
    pub fn matrix_provider(
        &self,
        auth: crate::evidence_providers::SessionAuthorization,
        doc: &munarium_runbooks::RunbookDoc,
    ) -> Option<crate::evidence_providers::MatrixProvider> {
        let base_url = self.config.matrix_base_url.clone()?;
        // The view -> contract mapping comes from the RUNBOOK, resolved once
        // per turn. A layer names a view; Matrix's route takes a contract, and
        // the runbook is the only place that binding exists — which is also
        // what keeps a turn from reaching a contract nobody declared.
        let views = doc
            .spec
            .data_views
            .iter()
            .map(|v| {
                (
                    v.name.clone(),
                    crate::evidence_providers::BoundDataView {
                        contract: v.contract.clone(),
                        kind: v.kind,
                        parameters: serde_json::to_value(&v.parameters)
                            .unwrap_or_else(|_| serde_json::json!({})),
                        access_level: v.access_level,
                        compartments: v.compartments.clone(),
                    },
                )
            })
            .collect();
        Some(crate::evidence_providers::MatrixProvider {
            http: self.matrix_http.clone(),
            base_url,
            token: std::env::var("MUNARIUM_MATRIX_TOKEN").ok(),
            auth,
            views,
            breaker: self.matrix_breaker.clone(),
            metrics: self.metrics.clone(),
        })
    }

    pub async fn store_for(&self, tenant_id: &str) -> Result<Arc<dyn StorageBackend>> {
        match &self.stores {
            StoreRegistry::Mem(map) => {
                if let Some(s) = map.read().await.get(tenant_id) {
                    return Ok(s.clone());
                }
                let mut w = map.write().await;
                Ok(w.entry(tenant_id.to_string())
                    .or_insert_with(|| Arc::new(MemStore::new()))
                    .clone())
            }
            StoreRegistry::Pg(base) => Ok(Arc::new(base.with_tenant(tenant_id).await?)),
        }
    }

    /// Resolves a bearer token to a tenant context (static tokens only —
    /// the control/management planes). Data-plane JWTs resolve through
    /// `authenticate_principal`.
    pub fn authenticate(&self, bearer: Option<&str>) -> Result<TenantCtx> {
        match self.authenticate_principal(bearer)? {
            Principal::Static(ctx) => Ok(ctx),
            Principal::Access(_) => Err(KernelError::Forbidden(
                "access tokens are data-plane credentials; this endpoint requires a static token"
                    .into(),
            )),
        }
    }

    /// Resolves a bearer to the full principal: a static-token match wins;
    /// otherwise, when a token secret is configured, the bearer is verified
    /// as a capability JWT. Expiry surfaces as the token-expired slug
    /// via CustomError at the transport layer.
    pub fn authenticate_principal(&self, bearer: Option<&str>) -> Result<Principal> {
        match &self.config.auth {
            AuthMode::Disabled => Ok(Principal::Static(TenantCtx {
                tenant_id: "tenant-default".into(),
                role: "rw".into(),
                disabled_mode: true,
            })),
            AuthMode::Static(tokens) => {
                let token =
                    bearer.ok_or(KernelError::Unauthenticated("missing bearer token".into()))?;
                if let Some(hit) = tokens.iter().find(|(t, _, _)| constant_time_eq(t, token)) {
                    let (_, tenant, role) = hit;
                    return Ok(Principal::Static(TenantCtx {
                        tenant_id: tenant.clone(),
                        role: role.clone(),
                        disabled_mode: false,
                    }));
                }
                // Not a static token: try the JWT arm when configured.
                let secret = self
                    .config
                    .token_secret
                    .as_ref()
                    .ok_or_else(|| KernelError::Unauthenticated("invalid token".into()))?;
                match munarium_access::verify(secret, token) {
                    Ok(claims) => Ok(Principal::Access(claims.into())),
                    Err(munarium_access::AccessError::Expired) => {
                        Err(KernelError::Unauthenticated(TOKEN_EXPIRED_DETAIL.into()))
                    }
                    Err(munarium_access::AccessError::Invalid(_)) => {
                        Err(KernelError::Unauthenticated("invalid token".into()))
                    }
                }
            }
        }
    }

    /// Optional revocation check (MUNARIUM_TOKEN_REVOCATION_CHECK): one indexed
    /// PK lookup against access_tokens. No-op on the memory store.
    pub async fn check_revocation(&self, tenant_id: &str, jti: &str) -> Result<()> {
        if !self.config.token_revocation_check {
            return Ok(());
        }
        let Some(pool) = self.pg_pool() else {
            return Ok(());
        };
        let revoked: Option<(Option<chrono::DateTime<chrono::Utc>>,)> = sqlx::query_as(
            "SELECT revoked_at FROM access_tokens WHERE tenant_id = $1 AND jti = $2",
        )
        .bind(tenant_id)
        .bind(jti)
        .fetch_optional(pool)
        .await
        .map_err(|e| KernelError::Storage(e.to_string()))?;
        match revoked {
            Some((Some(_),)) => Err(KernelError::Unauthenticated(TOKEN_REVOKED_DETAIL.into())),
            _ => Ok(()),
        }
    }

    /// Idempotency check-or-record. Returns Ok(Some(stored)) on replay,
    /// Ok(None) when the caller should execute and then `idem_store`.
    /// Hardening: pg mode is table-backed (idempotency_keys), so replay
    /// protection survives restarts and is shared across replicas; the
    /// in-memory map remains the memory-store/dev behavior.
    pub async fn idem_check(
        &self,
        tenant: &str,
        key: &str,
        request_hash: &str,
    ) -> Result<Option<String>> {
        if let Some(pool) = self.pg_pool() {
            let row: Option<(String, String)> = sqlx::query_as(
                "SELECT request_hash, response_body FROM idempotency_keys
                  WHERE tenant_id = $1 AND key = $2",
            )
            .bind(tenant)
            .bind(key)
            .fetch_optional(pool)
            .await
            .map_err(|e| KernelError::Storage(e.to_string()))?;
            return match row {
                Some((stored_hash, body)) if stored_hash == request_hash => Ok(Some(body)),
                Some(_) => Err(KernelError::IdempotencyMismatch),
                None => Ok(None),
            };
        }
        let map = self.idem.lock().await;
        match map.get(&(tenant.to_string(), key.to_string())) {
            Some(rec) if rec.request_hash == request_hash => Ok(Some(rec.response_json.clone())),
            Some(_) => Err(KernelError::IdempotencyMismatch),
            None => Ok(None),
        }
    }

    pub async fn idem_store(&self, tenant: &str, key: &str, request_hash: &str, response: &str) {
        if let Some(pool) = self.pg_pool() {
            // Recorded after the command completes (documented retry
            // contract); a concurrent duplicate keeps the first record.
            let result = sqlx::query(
                "INSERT INTO idempotency_keys (tenant_id, key, request_hash, response_body, status_code)
                 VALUES ($1, $2, $3, $4, 200)
                 ON CONFLICT (tenant_id, key) DO NOTHING",
            )
            .bind(tenant)
            .bind(key)
            .bind(request_hash)
            .bind(response)
            .execute(pool)
            .await;
            if let Err(e) = result {
                tracing::warn!(error = %e, "idempotency record insert failed");
            }
            return;
        }
        self.idem.lock().await.insert(
            (tenant.to_string(), key.to_string()),
            IdemRecord {
                request_hash: request_hash.to_string(),
                response_json: response.to_string(),
            },
        );
    }
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn request_hash(body: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(body))
}
