// SPDX-License-Identifier: Apache-2.0
//! Aggregate reads for the tiered-storage admin view.
//!
//! All SQL for the storage page lives here, not in `dashboard/storage.rs` —
//! the same rule the rest of the console follows, so a page can be changed
//! without touching a query and a query can be reviewed without reading HTML.
//!
//! Every read is BOUNDED and tenant-scoped. This is a page an operator loads
//! while something is wrong, so it must not become the thing that is wrong: no
//! unbounded scan, no join across every artifact, and no remote call at all.
//! Nothing here contacts L2 — the page reports what the catalog last recorded,
//! labelled as such, rather than probing an object store to render a status.

use serde::Serialize;
use sqlx::Row;

use munarium_core::Result;

use crate::state::AppState;

/// One tier's counters, as the catalog last recorded them.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TierCounts {
    pub artifacts: i64,
    pub bytes: i64,
    pub verified: i64,
    pub sealed: i64,
    pub failed: i64,
    pub retired: i64,
}

/// A node's self-report. Soft state: lagging by design, and never authoritative.
#[derive(Debug, Clone, Serialize)]
pub struct NodeRow {
    pub node_id: String,
    pub plane: String,
    pub deployment_revision: String,
    pub retrieval_mode: String,
    pub admission_state: String,
    /// How stale, in seconds. The page turns this into fresh / stale /
    /// unknown; a missing row is never rendered as healthy or as absent.
    pub seen_age_secs: i64,
    pub blocking_scopes: i64,
    pub l1_used_bytes: Option<i64>,
    pub l1_budget_bytes: Option<i64>,
    pub l0_used_bytes: Option<i64>,
    pub l0_budget_bytes: Option<i64>,
    pub local_root_health: Option<String>,
}

/// What the fleet ought to be, beside what it is.
#[derive(Debug, Clone, Serialize)]
pub struct PlaneExpectationRow {
    pub plane: String,
    pub deployment_revision: String,
    pub minimum_fresh_nodes: i32,
    pub minimum_open_nodes: i32,
    pub minimum_open_fraction: Option<f64>,
    pub required_mode: String,
    pub generation: i64,
    pub verified_age_secs: Option<i64>,
    /// Counted from the snapshots, so the page can show expected beside
    /// observed rather than implying that whoever answered is the fleet.
    pub observed_fresh: i64,
    pub observed_wrong_revision: i64,
    pub observed_warming: i64,
    pub observed_stale: i64,
}

/// One capability, as the page renders it: on or off, and WHY.
///
/// The reason travels with the flag deliberately. "artifact store: disabled" on
/// its own sends an operator looking in three possible places; "unset or
/// unrecognised" sends them to one.
#[derive(Debug, Clone, Serialize)]
pub struct CapabilityRow {
    pub name: String,
    pub enabled: bool,
    pub detail: String,
}

/// The settings this process resolved, read-only.
#[derive(Debug, Clone, Serialize)]
pub struct SettingsView {
    pub configured_mode: String,
    pub effective_mode: String,
    /// Present when configured and effective differ, or when serving cannot
    /// start. Rendered prominently: a degraded process that looks healthy is
    /// the thing this whole page exists to prevent.
    pub degraded_because: Option<String>,
    pub must_refuse_startup: bool,
    pub artifact_store: String,
    pub capabilities: Vec<CapabilityRow>,
    pub blocking: Vec<String>,
}

/// One selector row: which engine serves a scope.
#[derive(Debug, Clone, Serialize)]
pub struct RolloutRow {
    pub scope_kind: String,
    pub scope_id: String,
    pub serving: String,
    pub prewarm_staged: bool,
    pub required_versions_policy: String,
    pub generation: i64,
}

/// Everything the page renders, gathered in one pass.
#[derive(Debug, Clone, Serialize)]
pub struct StorageSnapshot {
    /// What this process is configured to do and what it can actually do.
    pub settings: SettingsView,
    /// The mode THIS process is running, which is not necessarily what the
    /// fleet is running — hence the node table below it.
    pub this_replica_mode: String,
    pub truth: TierCounts,
    pub bindings: i64,
    pub attempts_running: i64,
    pub attempts_expired: i64,
    pub rollout_datastore_scopes: i64,
    pub rollout: Vec<RolloutRow>,
    pub expectations: Vec<PlaneExpectationRow>,
    pub nodes: Vec<NodeRow>,
    /// When the durable half was read, distinct from each node's own
    /// `seen_age_secs`. Two clocks, shown separately, so "fresh page, stale
    /// node" is legible rather than confusing.
    pub read_at: String,
    /// This replica's datastore admission state (§9.2): whether it would
    /// admit traffic, how many scopes bind it, and the hashed blockers.
    pub readiness: ReadinessView,
    /// This replica's shadow sampling and comparison counters. `None` when the
    /// process has no shadow plane — every mode but `shadow`, or a `shadow`
    /// process whose prerequisites were missing at startup, which is exactly
    /// the state an operator turning shadow mode on needs to be able to SEE.
    pub shadow: Option<ShadowView>,
}

/// This replica's datastore admission state, read from maintained state.
#[derive(Debug, Clone, Serialize)]
pub struct ReadinessView {
    pub admits: bool,
    pub selected_scopes: i64,
    pub blocking: Vec<String>,
}

/// The shadow plane's counters, per §13.4: sampling, dropped/timeout counts
/// and comparison outcomes, with no query text anywhere near them.
#[derive(Debug, Clone, Serialize)]
pub struct ShadowView {
    /// One request in N is sampled; 0 = configured off.
    pub sample_one_in: u64,
    pub completed: u64,
    pub not_sampled: u64,
    pub dropped: u64,
    pub timeout: u64,
    pub rejected: u64,
    pub error: u64,
    /// Non-zero is a finding, not a rate — a text-hash or provenance mismatch
    /// no tolerance band may absorb.
    pub corrupting: u64,
    pub mean_fused_overlap: Option<f64>,
}

/// How many rows the page will render per table. A bounded page stays useful
/// during an incident; an unbounded one becomes part of it.
const MAX_ROWS: i64 = 200;
/// Beyond this a snapshot is stale; beyond twice this it is unknown. Derived
/// from the heartbeat interval by the caller in a later slice — fixed here so
/// the page has one definition rather than three.
pub const STALE_AFTER_SECS: i64 = 120;

/// Gather the storage snapshot for one tenant.
pub async fn op_storage_snapshot(state: &AppState, tenant: &str) -> Result<StorageSnapshot> {
    let Some(pool) = state.pg_pool() else {
        // Memory-store deployments have no catalog at all. An empty snapshot
        // with the real mode is the honest answer; inventing zeros for tables
        // that do not exist would look like a healthy empty deployment.
        return Ok(StorageSnapshot {
            settings: settings_view(state),
            this_replica_mode: state.retrieval_mode_str().to_string(),
            truth: TierCounts::default(),
            bindings: 0,
            attempts_running: 0,
            attempts_expired: 0,
            rollout_datastore_scopes: 0,
            rollout: Vec::new(),
            expectations: Vec::new(),
            nodes: Vec::new(),
            read_at: chrono::Utc::now().to_rfc3339(),
            readiness: readiness_view(state),
            shadow: shadow_view(state),
        });
    };

    let truth_row = sqlx::query(
        "SELECT COUNT(*) AS artifacts,
                -- ::BIGINT because SUM(bigint) is NUMERIC in PostgreSQL, which a
                -- BIGINT get() PANICS on -- found live on the demo's first
                -- /admin/storage hit against a pg-backed deployment (the
                -- memory-store tests could never see it; the Matrix tree
                -- recorded the same species from its own live tier).
                COALESCE(SUM(bytes_len), 0)::BIGINT AS bytes,
                COUNT(*) FILTER (WHERE state = 'verified') AS verified,
                COUNT(*) FILTER (WHERE state = 'sealed')   AS sealed,
                COUNT(*) FILTER (WHERE state = 'failed')   AS failed,
                COUNT(*) FILTER (WHERE state = 'retired')  AS retired
           FROM index_artifacts
          WHERE tenant_id = $1",
    )
    .bind(tenant)
    .fetch_one(pool)
    .await
    .map_err(store_err)?;

    let truth = TierCounts {
        artifacts: truth_row.get("artifacts"),
        bytes: truth_row.get("bytes"),
        verified: truth_row.get("verified"),
        sealed: truth_row.get("sealed"),
        failed: truth_row.get("failed"),
        retired: truth_row.get("retired"),
    };

    let bindings: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM index_artifact_bindings WHERE tenant_id = $1")
            .bind(tenant)
            .fetch_one(pool)
            .await
            .map_err(store_err)?;

    let attempts = sqlx::query(
        "SELECT COUNT(*) FILTER (WHERE state = 'running' AND lease_expires_at > now()) AS running,
                COUNT(*) FILTER (WHERE state = 'running' AND lease_expires_at <= now()) AS expired
           FROM index_build_attempts
          WHERE tenant_id = $1",
    )
    .bind(tenant)
    .fetch_one(pool)
    .await
    .map_err(store_err)?;

    let rollout_rows = sqlx::query(
        "SELECT scope_kind, scope_id, serving, prewarm_staged,
                required_versions_policy, generation
           FROM retrieval_rollout
          WHERE tenant_id = $1
          ORDER BY scope_kind, scope_id
          LIMIT $2",
    )
    .bind(tenant)
    .bind(MAX_ROWS)
    .fetch_all(pool)
    .await
    .map_err(store_err)?;

    let rollout: Vec<RolloutRow> = rollout_rows
        .into_iter()
        .map(|r| RolloutRow {
            scope_kind: r.get("scope_kind"),
            scope_id: r.get("scope_id"),
            serving: r.get("serving"),
            prewarm_staged: r.get("prewarm_staged"),
            required_versions_policy: r.get("required_versions_policy"),
            generation: r.get("generation"),
        })
        .collect();
    let rollout_datastore_scopes =
        rollout.iter().filter(|r| r.serving == "datastore").count() as i64;

    // Node snapshots are environment-scoped, not tenant-scoped: a process
    // serves every tenant. They carry no tenant identifier at all, which is
    // deliberate -- a shared table must not name one tenant's scopes to
    // another tenant's operator.
    let env = state.deployment_environment_id();
    let node_rows = sqlx::query(
        "SELECT node_id, plane, deployment_revision, retrieval_mode, admission_state,
                EXTRACT(EPOCH FROM (now() - last_seen_at))::BIGINT AS seen_age,
                COALESCE(array_length(blocking_scope_hashes, 1), 0)::BIGINT AS blocking,
                l1_used_bytes, l1_budget_bytes, l0_used_bytes, l0_budget_bytes,
                local_root_health
           FROM retrieval_node_snapshots
          WHERE environment_id = $1
          ORDER BY plane, node_id
          LIMIT $2",
    )
    .bind(&env)
    .bind(MAX_ROWS)
    .fetch_all(pool)
    .await
    .map_err(store_err)?;

    let nodes: Vec<NodeRow> = node_rows
        .into_iter()
        .map(|r| NodeRow {
            node_id: r.get("node_id"),
            plane: r.get("plane"),
            deployment_revision: r.get("deployment_revision"),
            retrieval_mode: r.get("retrieval_mode"),
            admission_state: r.get("admission_state"),
            seen_age_secs: r.get("seen_age"),
            blocking_scopes: r.get("blocking"),
            l1_used_bytes: r.get("l1_used_bytes"),
            l1_budget_bytes: r.get("l1_budget_bytes"),
            l0_used_bytes: r.get("l0_used_bytes"),
            l0_budget_bytes: r.get("l0_budget_bytes"),
            local_root_health: r.get("local_root_health"),
        })
        .collect();

    let exp_rows = sqlx::query(
        "SELECT plane, deployment_revision, minimum_fresh_nodes, minimum_open_nodes,
                minimum_open_fraction, required_mode, generation,
                EXTRACT(EPOCH FROM (now() - verified_at))::BIGINT AS verified_age
           FROM retrieval_plane_expectations
          WHERE environment_id = $1
          ORDER BY plane, deployment_revision
          LIMIT $2",
    )
    .bind(&env)
    .bind(MAX_ROWS)
    .fetch_all(pool)
    .await
    .map_err(store_err)?;

    let expectations: Vec<PlaneExpectationRow> = exp_rows
        .into_iter()
        .map(|r| {
            let plane: String = r.get("plane");
            let revision: String = r.get("deployment_revision");
            // Counted from the snapshots already loaded rather than with
            // another query: the page must show EXPECTED beside OBSERVED, and
            // a node on the wrong revision satisfies neither count.
            let matching = nodes
                .iter()
                .filter(|n| n.plane == plane && n.seen_age_secs <= STALE_AFTER_SECS);
            let observed_fresh = matching
                .clone()
                .filter(|n| n.deployment_revision == revision)
                .count() as i64;
            let observed_wrong_revision = matching
                .clone()
                .filter(|n| n.deployment_revision != revision)
                .count() as i64;
            let observed_warming = nodes
                .iter()
                .filter(|n| {
                    n.plane == plane
                        && n.deployment_revision == revision
                        && n.seen_age_secs <= STALE_AFTER_SECS
                        && n.admission_state == "warming"
                })
                .count() as i64;
            let observed_stale = nodes
                .iter()
                .filter(|n| n.plane == plane && n.seen_age_secs > STALE_AFTER_SECS)
                .count() as i64;
            PlaneExpectationRow {
                plane,
                deployment_revision: revision,
                minimum_fresh_nodes: r.get("minimum_fresh_nodes"),
                minimum_open_nodes: r.get("minimum_open_nodes"),
                minimum_open_fraction: r.get("minimum_open_fraction"),
                required_mode: r.get("required_mode"),
                generation: r.get("generation"),
                verified_age_secs: r.get("verified_age"),
                observed_fresh,
                observed_wrong_revision,
                observed_warming,
                observed_stale,
            }
        })
        .collect();

    Ok(StorageSnapshot {
        settings: settings_view(state),
        this_replica_mode: state.retrieval_mode_str().to_string(),
        truth,
        bindings,
        attempts_running: attempts.get("running"),
        attempts_expired: attempts.get("expired"),
        rollout_datastore_scopes,
        rollout,
        expectations,
        nodes,
        read_at: chrono::Utc::now().to_rfc3339(),
        readiness: readiness_view(state),
        shadow: shadow_view(state),
    })
}

fn readiness_view(state: &AppState) -> ReadinessView {
    let r = state.datastore_readiness();
    ReadinessView {
        admits: r.admits(),
        selected_scopes: r.selected_scopes(),
        blocking: r.blocking(),
    }
}

/// This replica's shadow counters, or `None` when it has no plane.
fn shadow_view(state: &AppState) -> Option<ShadowView> {
    state.shadow_plane().map(|plane| {
        let stats = plane.stats().snapshot();
        ShadowView {
            sample_one_in: plane.sample_one_in(),
            completed: stats.completed,
            not_sampled: stats.not_sampled,
            dropped: stats.dropped,
            timeout: stats.timeout,
            rejected: stats.rejected,
            error: stats.error,
            corrupting: stats.corrupting,
            mean_fused_overlap: stats.mean_fused_overlap,
        }
    })
}

/// Project the resolved capabilities into what the page renders.
///
/// No I/O: these were resolved once at startup, so rendering them cannot fail
/// and cannot disagree with what the process is actually doing.
fn settings_view(state: &AppState) -> SettingsView {
    let c = state.datastore_capabilities();
    SettingsView {
        configured_mode: c.configured_mode.as_str().to_string(),
        effective_mode: c.effective_mode.as_str().to_string(),
        degraded_because: c.degraded_because.clone(),
        must_refuse_startup: c.must_refuse_startup(),
        artifact_store: c.artifact_store.as_str().to_string(),
        capabilities: c
            .capabilities
            .iter()
            .map(|x| CapabilityRow {
                name: x.name.to_string(),
                enabled: x.enabled,
                detail: x.detail.clone(),
            })
            .collect(),
        blocking: c.blocking.iter().map(|e| e.to_string()).collect(),
    }
}

fn store_err(e: sqlx::Error) -> munarium_core::KernelError {
    munarium_core::KernelError::Storage(e.to_string())
}
