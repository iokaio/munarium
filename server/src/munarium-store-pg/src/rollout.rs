// SPDX-License-Identifier: Apache-2.0
//! The rollout selector and deployment expectations.
//!
//! Two tables that answer different questions and are scoped differently.
//!
//! `retrieval_rollout` is TENANT-scoped: which engine serves a given scope.
//! `retrieval_plane_expectations` is ENVIRONMENT-scoped, because a process
//! serves every tenant — how many processes ought to exist is not a per-tenant
//! question, and putting a tenant on it would invite one tenant's operator to
//! reason about another's capacity.

use sqlx::{PgPool, Row};

use munarium_core::{KernelError, Result};

use crate::storage_err;

/// Which engine serves one scope.
#[derive(Debug, Clone)]
pub struct RolloutEntry {
    pub scope_kind: String,
    pub scope_id: String,
    pub serving: String,
    pub prewarm_staged: bool,
    pub required_versions_policy: String,
    pub generation: i64,
}

/// What a selector write is changing, and who is changing it.
///
/// Grouped rather than passed positionally: `create` and `update` otherwise
/// take eight arguments including two adjacent strings, and a caller that
/// transposed `changed_by` and `reason` would produce an audit trail attributing
/// the change to its own justification.
#[derive(Debug, Clone)]
pub struct RolloutChange<'a> {
    pub serving: &'a str,
    pub prewarm_staged: bool,
    pub changed_by: &'a str,
    pub reason: Option<&'a str>,
}

/// Tenant-scoped access to the rollout selector.
#[derive(Debug, Clone)]
pub struct RolloutSelector {
    pool: PgPool,
    tenant_id: String,
}

impl RolloutSelector {
    pub fn new(pool: PgPool, tenant_id: impl Into<String>) -> Self {
        Self {
            pool,
            tenant_id: tenant_id.into(),
        }
    }

    /// The selector for one scope.
    ///
    /// `None` means PostgreSQL serves it. An absent row and an explicit
    /// `postgres` row mean the same thing, which is the point: a selector that
    /// failed open onto the unproven engine would be the wrong convenience.
    pub async fn get(&self, scope_kind: &str, scope_id: &str) -> Result<Option<RolloutEntry>> {
        let row = sqlx::query(
            "SELECT scope_kind, scope_id, serving, prewarm_staged,
                    required_versions_policy, generation
               FROM retrieval_rollout
              WHERE tenant_id = $1 AND scope_kind = $2 AND scope_id = $3",
        )
        .bind(&self.tenant_id)
        .bind(scope_kind)
        .bind(scope_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(row.map(Self::to_entry))
    }

    fn to_entry(r: sqlx::postgres::PgRow) -> RolloutEntry {
        RolloutEntry {
            scope_kind: r.get("scope_kind"),
            scope_id: r.get("scope_id"),
            serving: r.get("serving"),
            prewarm_staged: r.get("prewarm_staged"),
            required_versions_policy: r.get("required_versions_policy"),
            generation: r.get("generation"),
        }
    }

    /// Create the first selector row for a scope.
    ///
    /// Separate from `update` because the first write has no generation to
    /// compare against — the same reason a first binding is an insert rather
    /// than a compare-and-swap.
    pub async fn create(
        &self,
        scope_kind: &str,
        scope_id: &str,
        change: RolloutChange<'_>,
    ) -> Result<RolloutEntry> {
        validate_serving(change.serving)?;
        let inserted = sqlx::query(
            "INSERT INTO retrieval_rollout
                 (tenant_id, scope_kind, scope_id, serving, prewarm_staged,
                  generation, changed_by, reason)
             VALUES ($1,$2,$3,$4,$5,1,$6,$7)
             ON CONFLICT (tenant_id, scope_kind, scope_id) DO NOTHING
             RETURNING generation",
        )
        .bind(&self.tenant_id)
        .bind(scope_kind)
        .bind(scope_id)
        .bind(change.serving)
        .bind(change.prewarm_staged)
        .bind(change.changed_by)
        .bind(change.reason)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?;

        if inserted.is_none() {
            return Err(KernelError::InvalidInput(format!(
                "a selector row already exists for {scope_kind}/{scope_id}; changing one is a \
                 compare-and-swap against the generation you read"
            )));
        }
        self.get(scope_kind, scope_id)
            .await?
            .ok_or_else(|| KernelError::Storage("selector written but not readable".into()))
    }

    /// Change the selector, compare-and-swap on the generation the caller read.
    ///
    /// A generation mismatch returns `Ok(None)` rather than an error: someone
    /// else changing the selector concurrently is an ordinary outcome the
    /// caller re-reads and retries, not a fault to report as one.
    pub async fn update(
        &self,
        scope_kind: &str,
        scope_id: &str,
        change: RolloutChange<'_>,
        expected_generation: i64,
    ) -> Result<Option<RolloutEntry>> {
        validate_serving(change.serving)?;
        let updated = sqlx::query(
            "UPDATE retrieval_rollout
                SET serving = $4, prewarm_staged = $5, generation = generation + 1,
                    changed_by = $6, reason = $7, changed_at = now()
              WHERE tenant_id = $1 AND scope_kind = $2 AND scope_id = $3
                AND generation = $8
             RETURNING generation",
        )
        .bind(&self.tenant_id)
        .bind(scope_kind)
        .bind(scope_id)
        .bind(change.serving)
        .bind(change.prewarm_staged)
        .bind(change.changed_by)
        .bind(change.reason)
        .bind(expected_generation)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?;

        if updated.is_none() {
            return Ok(None);
        }
        self.get(scope_kind, scope_id).await
    }

    /// Every scope this tenant has routed to the datastore.
    pub async fn datastore_scopes(&self) -> Result<Vec<RolloutEntry>> {
        let rows = sqlx::query(
            "SELECT scope_kind, scope_id, serving, prewarm_staged,
                    required_versions_policy, generation
               FROM retrieval_rollout
              WHERE tenant_id = $1 AND serving = 'datastore'
              ORDER BY scope_kind, scope_id",
        )
        .bind(&self.tenant_id)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(rows.into_iter().map(Self::to_entry).collect())
    }
}

fn validate_serving(serving: &str) -> Result<()> {
    if serving != "postgres" && serving != "datastore" {
        return Err(KernelError::InvalidInput(format!(
            "serving must be 'postgres' or 'datastore', not {serving:?}"
        )));
    }
    Ok(())
}

/// What a plane's fleet ought to look like before a cutover is judged safe.
#[derive(Debug, Clone)]
pub struct PlaneExpectation {
    pub plane: String,
    pub deployment_revision: String,
    pub minimum_fresh_nodes: i32,
    pub minimum_open_nodes: i32,
    pub minimum_open_fraction: Option<f64>,
    pub required_mode: String,
    pub generation: i64,
}

/// Environment-scoped deployment expectations.
#[derive(Debug, Clone)]
pub struct PlaneExpectations {
    pool: PgPool,
    environment_id: String,
}

impl PlaneExpectations {
    pub fn new(pool: PgPool, environment_id: impl Into<String>) -> Self {
        Self {
            pool,
            environment_id: environment_id.into(),
        }
    }

    pub async fn get(&self, plane: &str, revision: &str) -> Result<Option<PlaneExpectation>> {
        let row = sqlx::query(
            "SELECT plane, deployment_revision, minimum_fresh_nodes, minimum_open_nodes,
                    minimum_open_fraction, required_mode, generation
               FROM retrieval_plane_expectations
              WHERE environment_id = $1 AND plane = $2 AND deployment_revision = $3",
        )
        .bind(&self.environment_id)
        .bind(plane)
        .bind(revision)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(row.map(|r| PlaneExpectation {
            plane: r.get("plane"),
            deployment_revision: r.get("deployment_revision"),
            minimum_fresh_nodes: r.get("minimum_fresh_nodes"),
            minimum_open_nodes: r.get("minimum_open_nodes"),
            minimum_open_fraction: r.get("minimum_open_fraction"),
            required_mode: r.get("required_mode"),
            generation: r.get("generation"),
        }))
    }

    /// Record what a plane's fleet ought to be.
    ///
    /// The database refuses a zero minimum and a fraction without a floor
    /// (migration 0026's CHECK constraints), so no code path can write one.
    /// What this adds is WHO said so and WHY, which is what makes a later
    /// cutover auditable rather than merely permitted.
    #[allow(clippy::too_many_arguments)]
    pub async fn record(
        &self,
        plane: &str,
        revision: &str,
        minimum_fresh_nodes: i32,
        minimum_open_nodes: i32,
        minimum_open_fraction: Option<f64>,
        required_mode: &str,
        observed_min_replicas: Option<i32>,
        actor: &str,
        reason: Option<&str>,
    ) -> Result<PlaneExpectation> {
        sqlx::query(
            "INSERT INTO retrieval_plane_expectations
                 (environment_id, plane, deployment_revision, minimum_fresh_nodes,
                  minimum_open_nodes, minimum_open_fraction, required_mode,
                  observed_min_replicas, generation, actor, reason, verified_at,
                  verification_source)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,1,$9,$10, now(), 'operator')
             ON CONFLICT (environment_id, plane, deployment_revision) DO UPDATE
                SET minimum_fresh_nodes = EXCLUDED.minimum_fresh_nodes,
                    minimum_open_nodes  = EXCLUDED.minimum_open_nodes,
                    minimum_open_fraction = EXCLUDED.minimum_open_fraction,
                    required_mode = EXCLUDED.required_mode,
                    observed_min_replicas = EXCLUDED.observed_min_replicas,
                    generation = retrieval_plane_expectations.generation + 1,
                    actor = EXCLUDED.actor,
                    reason = EXCLUDED.reason,
                    changed_at = now(),
                    verified_at = now()",
        )
        .bind(&self.environment_id)
        .bind(plane)
        .bind(revision)
        .bind(minimum_fresh_nodes)
        .bind(minimum_open_nodes)
        .bind(minimum_open_fraction)
        .bind(required_mode)
        .bind(observed_min_replicas)
        .bind(actor)
        .bind(reason)
        .execute(&self.pool)
        .await
        .map_err(storage_err)?;

        self.get(plane, revision)
            .await?
            .ok_or_else(|| KernelError::Storage("expectation written but not readable".into()))
    }
}
