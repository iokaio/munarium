// SPDX-License-Identifier: Apache-2.0
//! The server's side of mirror builds: where artifacts live, who counts the
//! cost, and the tenant-scoped operations an operator can ask for.
//!
//! The coordinator ([`munarium_retrieval::mirror`]) knows how to build an
//! artifact and in what order to publish it. It deliberately knows nothing
//! about Azure, about this process's metrics format, or about which tenant is
//! asking. That is what this module supplies.
//!
//! ## Nothing here accepts an artifact hash as authority
//!
//! Every operation is reached through an authorized tenant and names a logical
//! version. An `artifact_id` appears only as an ANSWER — never as a parameter
//! that grants access to open, inspect, rebuild or delete something. A content
//! hash is a statement about bytes, and two tenants holding identical corpora
//! legitimately hold identical hashes.

use std::sync::Arc;

use munarium_core::{KernelError, Result};
use munarium_datastore::shard::OpenShard;
use munarium_datastore::store::ArtifactStore;
use munarium_datastore::verify::{Limits, ReaderCapabilities};
use munarium_retrieval::build_metrics::{BuildMetrics, BuildObserver};
use munarium_retrieval::mirror::{
    ArtifactStoreFactory, LocalStoreFactory, MirrorContext, MirrorOutcome,
};
use munarium_retrieval::RequiredVersionsPolicy;
use munarium_store_pg::artifacts::{ArtifactCatalog, BindingSlot};
use munarium_store_pg::attempts::BuildAttempts;

use munarium_api_types as dto;

use crate::error::ApiError;
use crate::state::AppState;
use sqlx::Row as _;

/// Records build cost into the process metrics.
///
/// Labels are `mode`, `outcome` and `phase` only. No tenant, collection,
/// version, artifact id or path appears, per §13.1 — a per-tenant series here
/// would multiply cardinality by the tenant count and leak the tenant list to
/// anyone who can scrape.
pub struct MetricsObserver {
    metrics: Arc<crate::metrics::Metrics>,
}

impl std::fmt::Debug for MetricsObserver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The registry holds every recorded series; printing it would put the
        // whole exposition into a log line.
        f.write_str("MetricsObserver")
    }
}

impl MetricsObserver {
    pub fn new(metrics: Arc<crate::metrics::Metrics>) -> Self {
        Self { metrics }
    }
}

impl BuildObserver for MetricsObserver {
    fn build_finished(&self, m: &BuildMetrics) {
        let base = crate::metrics::labels(&[("mode", m.mode), ("outcome", m.outcome)]);
        self.metrics.inc("munarium_index_build_total", base.clone());
        if m.chunks > 0 {
            self.metrics
                .inc_by("munarium_index_build_chunks_total", base.clone(), m.chunks);
        }
        if m.bytes > 0 {
            self.metrics
                .inc_by("munarium_index_build_bytes_total", base, m.bytes);
        }
        for (phase, seconds) in [
            ("export", m.export_seconds),
            ("seal", m.seal_seconds),
            ("publish", m.publish_seconds),
            ("total", m.total_seconds),
        ] {
            self.metrics.observe(
                "munarium_index_build_duration_seconds",
                crate::metrics::labels(&[("mode", m.mode), ("phase", phase)]),
                seconds,
            );
        }
    }
}

/// An artifact store factory over a cloud object client.
///
/// Holds the `ObjectSourceStore` rather than the raw `object_store` client, so
/// the `object_store` crate stays inside `munarium-store-objects` — the one
/// place that owns cloud client construction.
struct ObjectStoreFactory {
    store: Arc<munarium_store_objects::ObjectSourceStore>,
}

impl std::fmt::Debug for ObjectStoreFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Nothing about the account, container or credential path: this is
        // formatted on failure paths that reach logs.
        f.write_str("ObjectStoreFactory")
    }
}

impl ArtifactStoreFactory for ObjectStoreFactory {
    fn store_for_prefix(&self, prefix: &str) -> Result<Arc<dyn ArtifactStore>> {
        let s = munarium_store_objects::artifacts::ObjectArtifactStore::from_source_store(
            &self.store,
            prefix,
        )
        .map_err(|e| KernelError::Storage(format!("artifact store: {e}")))?;
        Ok(Arc::new(s))
    }
}

impl AppState {
    /// Where L2 artifacts live for this deployment.
    ///
    /// Built per call rather than cached on the state: it is used by builds and
    /// by operator actions, not on the request path, and constructing it lazily
    /// keeps a deployment that never mirrors from needing an artifact container
    /// to exist.
    pub fn artifact_store_factory(&self) -> Result<Arc<dyn ArtifactStoreFactory>> {
        let kind = std::env::var("MUNARIUM_DATASTORE_ARTIFACT_STORE")
            .unwrap_or_else(|_| "file".to_string());
        match kind.as_str() {
            "file" => {
                // A directory of its own, NOT the L1 cache root. L1 holds
                // hydrated copies that eviction may delete at any moment; L2 is
                // durable truth. One directory serving both would let the
                // evictor delete the only copy.
                let root = std::env::var("MUNARIUM_DATASTORE_ARTIFACT_ROOT")
                    .or_else(|_| {
                        std::env::var("MUNARIUM_DATASTORE_LOCAL_ROOT")
                            .map(|r| format!("{}/l2", r.trim_end_matches(['/', '\\'])))
                    })
                    .map_err(|_| {
                        KernelError::InvalidInput(
                            "MUNARIUM_DATASTORE_ARTIFACT_STORE=file needs \
                             MUNARIUM_DATASTORE_ARTIFACT_ROOT (or MUNARIUM_DATASTORE_LOCAL_ROOT, \
                             under which an l2/ directory is used)"
                                .into(),
                        )
                    })?;
                Ok(Arc::new(LocalStoreFactory::new(root)))
            }
            "az" | "s3" | "gcs" => {
                // The artifact container is a DIFFERENT container from sources,
                // sharing the account and the credential path. Reusing the
                // client would point artifacts at the sources container;
                // rebuilding the topology with a new container name reuses the
                // credential chain, which is the part worth sharing.
                let container = std::env::var("MUNARIUM_DATASTORE_ARTIFACT_CONTAINER")
                    .unwrap_or_else(|_| "indexes".to_string());
                let store = artifact_object_store(&self.config.source_store, &container)?;
                Ok(Arc::new(ObjectStoreFactory { store }))
            }
            other => Err(KernelError::InvalidInput(format!(
                "MUNARIUM_DATASTORE_ARTIFACT_STORE must be file|az|s3|gcs, got {other:?}"
            ))),
        }
    }

    /// A tenant-scoped mirror-build context.
    pub fn mirror_context(&self, tenant_id: &str) -> Result<MirrorContext> {
        let pool = self.pg_pool().ok_or_else(|| {
            KernelError::InvalidInput(
                "index artifacts require the postgres store (MUNARIUM_STORE=postgres): the \
                 catalog, bindings and attempts are all durable truth"
                    .into(),
            )
        })?;
        let staging_root = std::env::var("MUNARIUM_DATASTORE_STAGING_ROOT")
            .or_else(|_| {
                std::env::var("MUNARIUM_DATASTORE_LOCAL_ROOT")
                    .map(|r| format!("{}/staging", r.trim_end_matches(['/', '\\'])))
            })
            .map_err(|_| {
                KernelError::InvalidInput(
                    "a mirror build needs MUNARIUM_DATASTORE_STAGING_ROOT (or \
                     MUNARIUM_DATASTORE_LOCAL_ROOT, under which a staging/ directory is used): \
                     content is sealed locally before it is published"
                        .into(),
                )
            })?;

        Ok(MirrorContext {
            catalog: ArtifactCatalog::new(pool.clone(), tenant_id),
            attempts: BuildAttempts::new(pool.clone(), tenant_id),
            stores: self.artifact_store_factory()?,
            node_id: self.config.instance_id.clone(),
            staging_root: std::path::PathBuf::from(staging_root),
            artifact_prefix: std::env::var("MUNARIUM_DATASTORE_ARTIFACT_PREFIX")
                .unwrap_or_else(|_| "v1".to_string()),
            tenant_path_hash: tenant_path_hash(tenant_id),
            faults: None,
            observer: Some(Arc::new(MetricsObserver::new(self.metrics.clone()))),
            // The exact/approximate decision for direct builds, read at the
            // server edge like every other datastore env knob.
            vector_policy: munarium_retrieval::mirror::VectorPolicy::from_env(),
        })
    }
}

/// The per-tenant path element in an object key.
///
/// A hash, never the tenant id: object keys appear in storage inventories,
/// access logs, diagnostic exports and support tickets, and a container listing
/// should not enumerate the customer list. Truncated to 32 hex characters,
/// which is far beyond collision risk for a tenant population and keeps keys
/// readable.
pub fn tenant_path_hash(tenant_id: &str) -> String {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(tenant_id.as_bytes());
    hex::encode(digest)[..32].to_string()
}

/// Build an object client for the artifact container.
///
/// The topology is re-derived from the source-store configuration with the
/// container replaced: the credential path — managed identity, SAS, the ambient
/// AWS chain — is the part worth sharing, and reusing the source client itself
/// would point artifacts at the sources container.
fn artifact_object_store(
    source: &crate::config::SourceStoreConfig,
    container: &str,
) -> Result<Arc<munarium_store_objects::ObjectSourceStore>> {
    use crate::config::{BlobAuthConfig, SourceStoreConfig as S};
    let store = match source {
        S::Azure {
            account,
            auth,
            endpoint,
            ..
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
            munarium_store_objects::ObjectSourceStore::azure(munarium_store_objects::AzureConfig {
                account: account.clone(),
                container: container.to_string(),
                auth,
                endpoint: endpoint.clone(),
            })?
        }
        S::S3 {
            region,
            endpoint,
            force_path_style,
            access_key_id,
            secret_access_key,
            ..
        } => munarium_store_objects::ObjectSourceStore::s3(munarium_store_objects::S3Config {
            bucket: container.to_string(),
            region: region.clone(),
            endpoint: endpoint.clone(),
            force_path_style: *force_path_style,
            access_key_id: access_key_id.clone(),
            secret_access_key: secret_access_key.clone(),
        })?,
        S::Gcs {
            service_account_json,
            ..
        } => munarium_store_objects::ObjectSourceStore::gcs(munarium_store_objects::GcsConfig {
            bucket: container.to_string(),
            service_account_json: service_account_json.clone(),
        })?,
        // `file`, `pg` and `mem` source stores have no cloud credential path to
        // share. Refusing is better than silently falling back to local files,
        // which would put durable truth on a disk the next restart discards --
        // and a local posture is available deliberately, as
        // MUNARIUM_DATASTORE_ARTIFACT_STORE=file.
        other => {
            return Err(KernelError::InvalidInput(format!(
                "MUNARIUM_DATASTORE_ARTIFACT_STORE names a cloud object store but the source store is {other:?}, which has no cloud client to share a credential path with;                  set MUNARIUM_DATASTORE_ARTIFACT_STORE=file for a local posture"
            )))
        }
    };
    Ok(Arc::new(store))
}

// ---------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------

pub async fn op_artifact_status(
    state: &AppState,
    tenant_id: &str,
    index_version_id: &str,
) -> Result<dto::IndexArtifactStatusDto> {
    let ctx = state.mirror_context(tenant_id)?;
    let artifacts = ctx
        .catalog
        .artifacts_for_version(index_version_id)
        .await?
        .into_iter()
        .map(|a| dto::IndexArtifactDto {
            artifact_id: a.artifact_id,
            engine_id: a.engine_id,
            state: a.state.as_str().to_string(),
            format_version: a.format_version,
            bytes_len: a.bytes_len,
            file_count: a.file_count,
            artifact_plan_sha256: a.artifact_plan_sha256,
        })
        .collect();

    let mut bindings = Vec::new();
    for slot in [
        BindingSlot::Staged,
        BindingSlot::Shadow,
        BindingSlot::Serving,
    ] {
        if let Some(b) = ctx.catalog.binding(index_version_id, slot).await? {
            bindings.push(dto::IndexArtifactBindingDto {
                slot: slot.as_str().to_string(),
                artifact_id: b.artifact_id,
                generation: b.generation,
            });
        }
    }

    Ok(dto::IndexArtifactStatusDto {
        index_version_id: index_version_id.to_string(),
        artifacts,
        bindings,
    })
}

/// Re-verify every catalogued artifact of a version, against its stored bytes.
///
/// The check is the real one: fetch the canonical L2 manifest, hash it against
/// the artifact id, then open the artifact — verifying every component and
/// running its probes. Reading the catalog's `artifact_manifest` projection
/// would verify the database against itself.
pub async fn op_verify_artifacts(
    state: &AppState,
    tenant_id: &str,
    index_version_id: &str,
) -> Result<Vec<dto::IndexArtifactVerifyDto>> {
    let ctx = state.mirror_context(tenant_id)?;
    let rows = ctx.catalog.artifacts_for_version(index_version_id).await?;
    if rows.is_empty() {
        return Err(KernelError::NotFound {
            kind: "index artifact",
            id: index_version_id.to_string(),
        });
    }

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let store = ctx.stores.store_for_prefix(&row.artifact_uri)?;
        let artifact_id = row.artifact_id.clone();
        // Opening an artifact is CPU- and IO-bound synchronous work: hashing
        // every component and materializing a lexical index. It runs on the
        // blocking pool so a verify cannot stall the async runtime.
        let opened = tokio::task::spawn_blocking(move || {
            OpenShard::open(
                store.as_ref(),
                &artifact_id,
                &ReaderCapabilities::v1(),
                &Limits::default(),
            )
            .map(|_| ())
        })
        .await
        .map_err(|e| KernelError::Storage(format!("verify task: {e}")))?;

        out.push(match opened {
            Ok(()) => dto::IndexArtifactVerifyDto {
                artifact_id: row.artifact_id,
                verified: true,
                detail: None,
            },
            Err(e) => {
                let mut detail = e.to_string();
                detail.truncate(500);
                dto::IndexArtifactVerifyDto {
                    artifact_id: row.artifact_id,
                    verified: false,
                    detail: Some(detail),
                }
            }
        });
    }
    Ok(out)
}

/// Mirror one index version.
pub async fn op_rebuild_artifact(
    state: &AppState,
    tenant_id: &str,
    index_version_id: &str,
) -> Result<dto::IndexArtifactBuildDto> {
    let ctx = state.mirror_context(tenant_id)?;
    let outcome = state
        .retrieval_for(tenant_id)?
        .rebuild_version(&ctx, index_version_id)
        .await?;
    Ok(build_result(index_version_id, outcome))
}

/// Bind a verified artifact into the `staged` or `shadow` slot.
///
/// The slot restriction is enforced twice on purpose: here, so the API's
/// contract is explicit, and inside `ArtifactCatalog::rebind`, so no future
/// caller acquires a serving-slot bypass by linking against the catalog.
pub async fn op_bind_artifact(
    state: &AppState,
    tenant_id: &str,
    index_version_id: &str,
    req: &dto::IndexArtifactBindRequest,
    actor: &str,
) -> Result<dto::IndexArtifactBindingDto> {
    let slot = BindingSlot::parse(&req.slot)?;
    if slot == BindingSlot::Serving {
        return Err(KernelError::InvalidInput(
            "the serving slot is changed by promotion, not by bind; this operation accepts staged and shadow only"
                .into(),
        ));
    }
    let ctx = state.mirror_context(tenant_id)?;
    let bound = match req.expected_generation {
        None => {
            ctx.catalog
                .bind_new(
                    index_version_id,
                    slot,
                    &req.artifact_id,
                    actor,
                    req.reason.as_deref(),
                )
                .await?
        }
        Some(expected) => {
            ctx.catalog
                .rebind(
                    index_version_id,
                    slot,
                    &req.artifact_id,
                    expected,
                    actor,
                    req.reason.as_deref(),
                )
                .await?
        }
    };
    Ok(dto::IndexArtifactBindingDto {
        slot: bound.slot.as_str().to_string(),
        artifact_id: bound.artifact_id,
        generation: bound.generation,
    })
}

/// Promote the `staged` binding into `serving` (§7.3).
///
/// Before the catalog's atomic CAS runs, the FLEET gate is evaluated here:
/// for every `retrieval_plane_expectations` row in this environment, the node
/// snapshots must show at least the expected number of fresh, ready nodes at
/// the required plane and revision. Expectations are deployment truth written
/// by automation; NO rows means no fleet contract has been declared, and the
/// promotion proceeds on the catalog checks alone — a single-replica dev
/// posture the plan explicitly allows, with the production posture being that
/// automation declares its expectations.
///
/// The gate proves two things per expectation row: the fleet is FRESH and
/// ready (node snapshots), and — when `minimum_open_nodes` asks for it — the
/// staged candidate is already OPEN on at least that many distinct nodes
/// (residency snapshots, which the readiness warmer writes every sweep).
/// That is §7.3 step 2's cutover condition: a promotion must never make an
/// artifact serving-required before the fleet holds it.
pub async fn op_promote_artifact(
    state: &AppState,
    tenant_id: &str,
    index_version_id: &str,
    req: &dto::IndexArtifactPromoteRequest,
    actor: &str,
) -> Result<dto::IndexArtifactBindingDto> {
    let pool = state
        .pg_pool()
        .ok_or_else(|| KernelError::InvalidInput("promotion requires the postgres store".into()))?;

    let env = state.deployment_environment_id();
    let expectations = sqlx::query(
        "SELECT plane, deployment_revision, minimum_fresh_nodes, minimum_open_nodes,
                required_mode
           FROM retrieval_plane_expectations WHERE environment_id = $1",
    )
    .bind(&env)
    .fetch_all(pool)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    for exp in &expectations {
        let plane: String = exp.get("plane");
        let revision: String = exp.get("deployment_revision");
        let minimum: i32 = exp.get("minimum_fresh_nodes");
        let required_mode: String = exp.get("required_mode");
        let fresh: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM retrieval_node_snapshots
              WHERE environment_id = $1 AND plane = $2 AND deployment_revision = $3
                AND retrieval_mode = $4 AND admission_state = 'ready'
                AND last_seen_at > now() - make_interval(secs => $5)",
        )
        .bind(&env)
        .bind(&plane)
        .bind(&revision)
        .bind(&required_mode)
        .bind(crate::storage_api::STALE_AFTER_SECS as f64)
        .fetch_one(pool)
        .await
        .map_err(|e| KernelError::Storage(e.to_string()))?;
        if fresh < minimum as i64 {
            return Err(KernelError::InvalidInput(format!(
                "plane {plane}@{revision} expects {minimum} fresh ready node(s) in mode                  {required_mode}; observed {fresh}. Promotion waits for the fleet, not the other way around"
            )));
        }
    }

    let ctx = state.mirror_context(tenant_id)?;

    // The staged-open half of the fleet gate needs the staged ARTIFACT id,
    // read before the CAS. A rebind between this read and the CAS is caught
    // by the CAS itself (the caller supplied the staged generation), so the
    // read does not need its own lock.
    let staged = ctx
        .catalog
        .binding(index_version_id, BindingSlot::Staged)
        .await?
        .ok_or_else(|| {
            KernelError::InvalidInput(format!(
                "no staged binding for {index_version_id}; there is nothing to promote"
            ))
        })?;
    for exp in &expectations {
        let plane: String = exp.get("plane");
        let revision: String = exp.get("deployment_revision");
        let minimum_open: i32 = exp
            .try_get::<Option<i32>, _>("minimum_open_nodes")
            .ok()
            .flatten()
            .unwrap_or(0);
        if minimum_open <= 0 {
            continue;
        }
        let open: i64 = sqlx::query_scalar(
            "SELECT COUNT(DISTINCT r.node_id)
               FROM index_artifact_residency_snapshots r
               JOIN retrieval_node_snapshots n
                 ON n.environment_id = r.environment_id AND n.node_id = r.node_id
              WHERE r.environment_id = $1 AND r.tenant_id = $2
                AND r.index_version_id = $3 AND r.artifact_id = $4
                AND r.residency_state = 'open'
                AND r.last_seen_at > now() - make_interval(secs => $5)
                AND n.plane = $6 AND n.deployment_revision = $7",
        )
        .bind(&env)
        .bind(tenant_id)
        .bind(index_version_id)
        .bind(&staged.artifact_id)
        .bind(crate::storage_api::STALE_AFTER_SECS as f64)
        .bind(&plane)
        .bind(&revision)
        .fetch_one(pool)
        .await
        .map_err(|e| KernelError::Storage(e.to_string()))?;
        if open < minimum_open as i64 {
            return Err(KernelError::InvalidInput(format!(
                "plane {plane}@{revision} expects the staged candidate open on {minimum_open} \
                 node(s); observed {open}. Prewarm first (prewarm_staged=true), then promote"
            )));
        }
    }

    let bound = ctx
        .catalog
        .promote_staged(
            index_version_id,
            req.expected_staged_generation,
            req.expected_serving_generation,
            actor,
            req.reason.as_deref(),
        )
        .await?;
    Ok(dto::IndexArtifactBindingDto {
        slot: bound.slot.as_str().to_string(),
        artifact_id: bound.artifact_id,
        generation: bound.generation,
    })
}

/// Mirror every serving-required version of a collection.
pub async fn op_backfill_collection(
    state: &AppState,
    tenant_id: &str,
    collection_id: &str,
) -> Result<dto::IndexArtifactBackfillResponse> {
    let ctx = state.mirror_context(tenant_id)?;

    // The policy comes from the scope's own rollout row when it has one, so a
    // backfill covers exactly what the selector will later demand. A scope with
    // no row has not been configured, and the default is the safe one.
    let selector = munarium_store_pg::rollout::RolloutSelector::new(
        state
            .pg_pool()
            .ok_or_else(|| KernelError::InvalidInput("the postgres store is required".into()))?
            .clone(),
        tenant_id,
    );
    let policy = match selector.get("collection", collection_id).await? {
        Some(entry) => RequiredVersionsPolicy::parse(&entry.required_versions_policy)?,
        None => RequiredVersionsPolicy::ActivePinnedAndHorizon,
    };
    // The SAME horizon retention and eviction use. Two of them computing it
    // separately is how a version becomes evictable while a session still
    // holds it.
    let horizon =
        i64::try_from(state.datastore_capabilities().pin_horizon_secs).unwrap_or(i64::MAX);

    let report = state
        .retrieval_for(tenant_id)?
        .backfill_collection(&ctx, collection_id, policy, horizon)
        .await?;

    Ok(dto::IndexArtifactBackfillResponse {
        collection_id: report.scope_id.clone(),
        policy: report.policy.to_string(),
        complete: report.is_complete(),
        versions: report
            .versions
            .iter()
            .map(|v| dto::IndexArtifactBackfillVersionDto {
                index_version_id: v.index_version_id.clone(),
                reason: match v.reason {
                    munarium_retrieval::RequiredReason::Active => "active".into(),
                    munarium_retrieval::RequiredReason::WithinHorizon => "within_horizon".into(),
                },
                outcome: match &v.result {
                    Ok(o) => outcome_label(o).to_string(),
                    Err(_) => "failed".to_string(),
                },
                error: v.result.as_ref().err().map(|e| {
                    let mut m = e.clone();
                    m.truncate(500);
                    m
                }),
            })
            .collect(),
    })
}

fn outcome_label(o: &MirrorOutcome) -> &'static str {
    match o {
        MirrorOutcome::Published { .. } => "published",
        MirrorOutcome::Converged { .. } => "converged",
        MirrorOutcome::AlreadyBuilt { .. } => "already_built",
        MirrorOutcome::AlreadyRunning { .. } => "deferred",
    }
}

fn build_result(index_version_id: &str, outcome: MirrorOutcome) -> dto::IndexArtifactBuildDto {
    let label = outcome_label(&outcome).to_string();
    match outcome {
        MirrorOutcome::Published {
            artifact_id,
            chunks,
            bound_staged,
        } => dto::IndexArtifactBuildDto {
            index_version_id: index_version_id.to_string(),
            outcome: label,
            artifact_id: Some(artifact_id),
            chunks,
            bound_staged,
        },
        MirrorOutcome::Converged { artifact_id } | MirrorOutcome::AlreadyBuilt { artifact_id } => {
            dto::IndexArtifactBuildDto {
                index_version_id: index_version_id.to_string(),
                outcome: label,
                artifact_id: Some(artifact_id),
                chunks: 0,
                bound_staged: false,
            }
        }
        MirrorOutcome::AlreadyRunning { .. } => dto::IndexArtifactBuildDto {
            index_version_id: index_version_id.to_string(),
            outcome: label,
            // Deliberately not the holder's node id: it is a hostname-shaped
            // internal identifier and this response crosses a tenant boundary.
            artifact_id: None,
            chunks: 0,
            bound_staged: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tenant never appears in an object key in clear. A container listing
    /// or an access log should not enumerate the customer list.
    #[test]
    fn the_tenant_path_element_is_a_hash() {
        let h = tenant_path_hash("acme-corp");
        assert_eq!(h.len(), 32);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(!h.contains("acme"));
        assert_eq!(h, tenant_path_hash("acme-corp"), "and it is stable");
        assert_ne!(h, tenant_path_hash("acme-corp2"));
    }

    /// A `pg` or `mem` source store has no cloud credential path to borrow.
    /// Refusing beats falling back to local files, which would put durable
    /// truth on a disk the next restart discards.
    #[test]
    fn a_cloud_artifact_store_over_a_non_cloud_source_store_is_refused() {
        assert!(artifact_object_store(&crate::config::SourceStoreConfig::Pg, "indexes").is_err());
        assert!(artifact_object_store(&crate::config::SourceStoreConfig::Mem, "indexes").is_err());
    }
}

// ---------------------------------------------------------------------------
// REST handlers
// ---------------------------------------------------------------------------
//
// Reads need only a valid tenant token; the three that do work need `rw`.
// A rebuild or a backfill spends real CPU, real object-storage writes and real
// build slots, so it is a write even though it changes nothing a query can see.

type ApiResult<T> = std::result::Result<T, ApiError>;

/// GET /v1/index-artifacts/{index_version_id}
#[utoipa::path(
    get,
    path = "/v1/index-artifacts/{index_version_id}",
    tag = "index-artifacts",
    params(("index_version_id" = String, Path, description = "logical index version id")),
    responses((status = 200, description = "artifacts and bindings for this version", body = dto::IndexArtifactStatusDto))
)]
pub async fn artifact_status(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path(index_version_id): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
) -> ApiResult<axum::Json<dto::IndexArtifactStatusDto>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    Ok(axum::Json(
        op_artifact_status(&state, &ctx.tenant_id, &index_version_id).await?,
    ))
}

/// POST /v1/index-artifacts/{index_version_id}/verify
#[utoipa::path(
    post,
    path = "/v1/index-artifacts/{index_version_id}/verify",
    tag = "index-artifacts",
    params(("index_version_id" = String, Path, description = "logical index version id")),
    responses(
        (status = 200, description = "each artifact re-verified against its stored bytes", body = dto::IndexArtifactVerifyResponse),
        (status = 404, description = "no artifact is catalogued for this version")
    )
)]
pub async fn verify_artifacts(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path(index_version_id): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
) -> ApiResult<axum::Json<dto::IndexArtifactVerifyResponse>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_rw()?;
    Ok(axum::Json(dto::IndexArtifactVerifyResponse {
        results: op_verify_artifacts(&state, &ctx.tenant_id, &index_version_id).await?,
        index_version_id,
    }))
}

/// POST /v1/index-artifacts/{index_version_id}/rebuild
#[utoipa::path(
    post,
    path = "/v1/index-artifacts/{index_version_id}/rebuild",
    tag = "index-artifacts",
    params(("index_version_id" = String, Path, description = "logical index version id")),
    responses(
        (status = 200, description = "what the build did", body = dto::IndexArtifactBuildDto),
        (status = 404, description = "no such index version")
    )
)]
pub async fn rebuild_artifact(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path(index_version_id): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
) -> ApiResult<axum::Json<dto::IndexArtifactBuildDto>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_rw()?;
    Ok(axum::Json(
        op_rebuild_artifact(&state, &ctx.tenant_id, &index_version_id).await?,
    ))
}

/// POST /v1/collections/{collection_id}/activate-index
///
/// The §7.3 logical activation the direct-build path was missing on the wire:
/// jobs build and the promote endpoint moves bindings, but until this route
/// the final CAS on the ACTIVE pointer existed only as a library call. For a
/// datastore-routed collection the guard refuses a version with no verified
/// serving binding, so the order is build -> promote -> activate, and running
/// it out of order fails loudly.
#[utoipa::path(
    post,
    path = "/v1/collections/{collection_id}/activate-index",
    tag = "index-artifacts",
    params(("collection_id" = String, Path, description = "collection id")),
    request_body = dto::CollectionIndexActivateRequest,
    responses(
        (status = 200, description = "the CAS outcome and the resulting active version", body = dto::CollectionIndexActivateResponse),
        (status = 400, description = "no serving binding on a datastore-routed collection, or invalid input")
    )
)]
pub async fn activate_collection_index(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path(collection_id): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
    crate::rest::ProblemJson(req): crate::rest::ProblemJson<dto::CollectionIndexActivateRequest>,
) -> ApiResult<axum::Json<dto::CollectionIndexActivateResponse>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_rw()?;
    let retrieval = state.retrieval_for(&ctx.tenant_id)?;
    let activated = retrieval
        .activate_collection_index_cas(
            &collection_id,
            &req.index_version_id,
            req.expected_active.as_deref(),
        )
        .await?;
    let active = retrieval.active_collection_index(&collection_id).await?;
    Ok(axum::Json(dto::CollectionIndexActivateResponse {
        activated,
        active,
    }))
}

/// POST /v1/index-artifacts/{index_version_id}/bind
#[utoipa::path(
    post,
    path = "/v1/index-artifacts/{index_version_id}/bind",
    tag = "index-artifacts",
    params(("index_version_id" = String, Path, description = "logical index version id")),
    request_body = dto::IndexArtifactBindRequest,
    responses(
        (status = 200, description = "the slot's new occupant and generation", body = dto::IndexArtifactBindingDto),
        (status = 404, description = "no such artifact"),
        (status = 400, description = "serving slot, unverified artifact, occupied slot without an expected generation, or a stale generation")
    )
)]
pub async fn bind_artifact(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path(index_version_id): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
    crate::rest::ProblemJson(req): crate::rest::ProblemJson<dto::IndexArtifactBindRequest>,
) -> ApiResult<axum::Json<dto::IndexArtifactBindingDto>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_rw()?;
    Ok(axum::Json(
        op_bind_artifact(
            &state,
            &ctx.tenant_id,
            &index_version_id,
            &req,
            &format!("api:{}", ctx.role),
        )
        .await?,
    ))
}

/// POST /v1/index-artifacts/{index_version_id}/promote
#[utoipa::path(
    post,
    path = "/v1/index-artifacts/{index_version_id}/promote",
    tag = "index-artifacts",
    params(("index_version_id" = String, Path, description = "logical index version id")),
    request_body = dto::IndexArtifactPromoteRequest,
    responses(
        (status = 200, description = "the serving slot after the promotion", body = dto::IndexArtifactBindingDto),
        (status = 400, description = "no staged binding, stale generations, unverified artifact, or an unmet fleet expectation")
    )
)]
pub async fn promote_artifact(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path(index_version_id): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
    crate::rest::ProblemJson(req): crate::rest::ProblemJson<dto::IndexArtifactPromoteRequest>,
) -> ApiResult<axum::Json<dto::IndexArtifactBindingDto>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_rw()?;
    Ok(axum::Json(
        op_promote_artifact(
            &state,
            &ctx.tenant_id,
            &index_version_id,
            &req,
            &format!("api:{}", ctx.role),
        )
        .await?,
    ))
}

/// POST /v1/index-artifacts/backfill
#[utoipa::path(
    post,
    path = "/v1/index-artifacts/backfill",
    tag = "index-artifacts",
    request_body = dto::IndexArtifactBackfillRequest,
    responses((status = 200, description = "per-version outcomes and whether the scope is now complete", body = dto::IndexArtifactBackfillResponse))
)]
pub async fn backfill(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    crate::rest::ProblemJson(req): crate::rest::ProblemJson<dto::IndexArtifactBackfillRequest>,
) -> ApiResult<axum::Json<dto::IndexArtifactBackfillResponse>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_rw()?;
    Ok(axum::Json(
        op_backfill_collection(&state, &ctx.tenant_id, &req.collection_id).await?,
    ))
}

// ---------------------------------------------------------------------------
// The reconciler task
// ---------------------------------------------------------------------------

/// How often this process reconciles its own interrupted builds. Bounded below
/// so a misconfiguration cannot turn the reconciler into a busy loop against
/// the catalog.
const MIN_RECONCILE_INTERVAL_SECS: u64 = 30;

/// Start the interval reconciler for interrupted builds (§7.4).
///
/// Runs once at startup and then on an interval, because the case it exists for
/// is a process that died mid-publication: the successor must find that work
/// before its lease lapses, and waiting a full interval to look would waste most
/// of the window.
///
/// Only in modes that build. In `postgres` mode nothing writes an attempt row,
/// so a reconciler would be a timer that queries an empty table forever.
pub fn spawn_reconciler(state: &Arc<AppState>) {
    if state.retrieval_mode_str() == "postgres" {
        return;
    }
    let Some(pool) = state.pg_pool().cloned() else {
        return;
    };
    let secs = std::env::var("MUNARIUM_DATASTORE_RECONCILE_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(60)
        .max(MIN_RECONCILE_INTERVAL_SECS);

    let weak = Arc::downgrade(state);
    let node_id = state.config.instance_id.clone();
    tokio::spawn(async move {
        loop {
            let Some(st) = weak.upgrade() else { break };
            match munarium_store_pg::attempts::tenants_with_sealed_attempts(&pool, &node_id).await {
                Ok(tenants) => {
                    for tenant in tenants {
                        match st.mirror_context(&tenant) {
                            Ok(ctx) => {
                                match munarium_retrieval::mirror::reconcile_attempts(&ctx).await {
                                    Ok(r) if r == Default::default() => {}
                                    Ok(r) => tracing::info!(
                                        resumed = r.resumed,
                                        abandoned = r.abandoned,
                                        expired = r.expired,
                                        left_alone = r.left_alone,
                                        "build reconciler"
                                    ),
                                    Err(e) => {
                                        tracing::warn!(error = %e, "build reconciliation failed")
                                    }
                                }
                            }
                            // A tenant whose context cannot be built (no
                            // staging root, no artifact store) is a
                            // configuration problem, not a per-tenant one, and
                            // it will be the same next tick. Warn and continue
                            // rather than stopping the sweep for everyone.
                            Err(e) => tracing::warn!(error = %e, "no build context for a tenant"),
                        }
                    }
                }
                Err(e) => tracing::warn!(error = %e, "could not list interrupted builds"),
            }
            drop(st);
            tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
        }
    });
    tracing::info!(interval_secs = secs, "build reconciler enabled");
}
