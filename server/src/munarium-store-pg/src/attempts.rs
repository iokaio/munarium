// SPDX-License-Identifier: Apache-2.0
//! Build attempts: pre-seal state, ownership leases, and reconciliation.
//!
//! This replaces a long-held advisory lock, and the difference is the point
//! (§7.1). A lock held across extraction and index construction pins a pool
//! connection for the whole build, cannot be seen by anyone, and — if the
//! holder dies — is released by the database at a moment nobody chose. A lease
//! expires on a schedule, is visible to the reconciler and to `/admin`, and
//! costs one row.
//!
//! The state machine, and why each state exists:
//!
//! ```text
//!   running ──seal──▶ sealed ──publish──▶ succeeded
//!      │                 │
//!      │                 └──abandoned──▶ failed
//!      ├──lease expiry──▶ expired
//!      ├──identical artifact found──▶ converged
//!      └──cancel──▶ cancelled
//! ```
//!
//! `converged` is its own state rather than a flavour of `failed`: an attempt
//! that discovered an identical artifact already catalogued did exactly the
//! right thing, and recording it as a failure would make a healthy rebuild look
//! like an incident on every dashboard that counts failures.

use sqlx::{PgPool, Row};

use munarium_core::{KernelError, Result};

use crate::storage_err;

/// Why a build is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptMode {
    /// Export already-committed PostgreSQL chunks. No extraction, no embedding.
    Mirror,
    /// Build from one prepared-chunk stream, fanned to configured sinks.
    Direct,
    /// Fill in a serving-required version that has no artifact yet.
    Backfill,
}

impl AttemptMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mirror => "mirror",
            Self::Direct => "direct",
            Self::Backfill => "backfill",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptState {
    Running,
    Sealed,
    Succeeded,
    Converged,
    Failed,
    Cancelled,
    Expired,
}

impl AttemptState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Sealed => "sealed",
            Self::Succeeded => "succeeded",
            Self::Converged => "converged",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "running" => Self::Running,
            "sealed" => Self::Sealed,
            "succeeded" => Self::Succeeded,
            "converged" => Self::Converged,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "expired" => Self::Expired,
            other => {
                return Err(KernelError::InvalidInput(format!(
                    "unknown attempt state {other:?}"
                )))
            }
        })
    }

    /// Whether the attempt is over. The reconciler only looks at the rest.
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Running | Self::Sealed)
    }
}

#[derive(Debug, Clone)]
pub struct AttemptRow {
    pub attempt_id: String,
    pub index_version_id: String,
    pub artifact_plan_sha256: String,
    pub mode: String,
    pub state: AttemptState,
    pub owner_node_id: String,
    pub attempt_no: i32,
    pub l1_staging_path: Option<String>,
    pub artifact_id: Option<String>,
    pub lease_expired: bool,
}

/// What starting an attempt produced.
///
/// A refusal is not an error: another node already building this exact plan is
/// the single-flight rule working, and a caller that treated it as a failure
/// would retry into the same refusal.
#[derive(Debug, Clone, PartialEq)]
pub enum ClaimOutcome {
    Claimed(String),
    /// Someone else holds an unexpired lease for this (version, plan).
    AlreadyRunning {
        owner_node_id: String,
    },
}

/// Tenant-scoped access to build attempts.
#[derive(Debug, Clone)]
pub struct BuildAttempts {
    pool: PgPool,
    tenant_id: String,
    lease_secs: i64,
}

/// Default lease. Long enough that a slow build heartbeats comfortably within
/// it, short enough that a dead node's work is reclaimable in minutes rather
/// than hours.
pub const DEFAULT_LEASE_SECS: i64 = 300;

impl BuildAttempts {
    pub fn new(pool: PgPool, tenant_id: impl Into<String>) -> Self {
        Self {
            pool,
            tenant_id: tenant_id.into(),
            lease_secs: DEFAULT_LEASE_SECS,
        }
    }

    pub fn with_lease_secs(mut self, secs: i64) -> Self {
        self.lease_secs = secs.max(1);
        self
    }

    /// Start an attempt, or report that someone else is already building this.
    ///
    /// Single-flight is enforced by the partial unique index on
    /// `(tenant, version, plan) WHERE state = 'running'`, so two nodes racing
    /// cannot both win — the database decides, not a read-then-write.
    ///
    /// A lease that has EXPIRED does not block a new attempt: the row is moved
    /// to `expired` first, which is what makes a dead node's work reclaimable
    /// rather than a permanent lock on that plan.
    pub async fn claim(
        &self,
        index_version_id: &str,
        artifact_plan_sha256: &str,
        mode: AttemptMode,
        owner_node_id: &str,
        l1_staging_path: Option<&str>,
    ) -> Result<ClaimOutcome> {
        // Reclaim first. Done as its own statement rather than folded into the
        // insert so that the transition is visible in the row's history: an
        // attempt that was expired and superseded reads differently from one
        // that never existed.
        self.expire_stale().await?;

        let attempt_id = format!("att-{}", uuid::Uuid::new_v4().simple());
        let inserted = sqlx::query(
            "INSERT INTO index_build_attempts
                 (tenant_id, attempt_id, index_version_id, artifact_plan_sha256, mode, state,
                  owner_node_id, lease_expires_at, attempt_no, l1_staging_path)
             VALUES ($1,$2,$3,$4,$5,'running',$6, now() + make_interval(secs => $7),
                     COALESCE((SELECT MAX(attempt_no) + 1 FROM index_build_attempts
                                WHERE tenant_id = $1 AND index_version_id = $3
                                  AND artifact_plan_sha256 = $4), 1),
                     $8)
             ON CONFLICT DO NOTHING
             RETURNING attempt_id",
        )
        .bind(&self.tenant_id)
        .bind(&attempt_id)
        .bind(index_version_id)
        .bind(artifact_plan_sha256)
        .bind(mode.as_str())
        .bind(owner_node_id)
        .bind(self.lease_secs as f64)
        .bind(l1_staging_path)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?;

        match inserted {
            Some(_) => Ok(ClaimOutcome::Claimed(attempt_id)),
            None => {
                let holder: Option<String> = sqlx::query_scalar(
                    "SELECT owner_node_id FROM index_build_attempts
                      WHERE tenant_id = $1 AND index_version_id = $2
                        AND artifact_plan_sha256 = $3 AND state = 'running'",
                )
                .bind(&self.tenant_id)
                .bind(index_version_id)
                .bind(artifact_plan_sha256)
                .fetch_optional(&self.pool)
                .await
                .map_err(storage_err)?;
                Ok(ClaimOutcome::AlreadyRunning {
                    owner_node_id: holder.unwrap_or_else(|| "unknown".into()),
                })
            }
        }
    }

    /// Extend the lease. Called periodically by the node doing the work.
    ///
    /// Returns `false` when the attempt is no longer this node's to extend —
    /// it expired and was reclaimed, or was cancelled. A builder that ignored
    /// that would keep working on an attempt someone else has taken over, and
    /// two builders publishing one plan is the situation the lease prevents.
    pub async fn heartbeat(&self, attempt_id: &str, owner_node_id: &str) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE index_build_attempts
                SET last_heartbeat_at = now(),
                    lease_expires_at = now() + make_interval(secs => $4)
              WHERE tenant_id = $1 AND attempt_id = $2 AND owner_node_id = $3
                AND state IN ('running', 'sealed')",
        )
        .bind(&self.tenant_id)
        .bind(attempt_id)
        .bind(owner_node_id)
        .bind(self.lease_secs as f64)
        .execute(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(done.rows_affected() == 1)
    }

    /// Record that content is sealed and publication is beginning.
    pub async fn mark_sealed(&self, attempt_id: &str, artifact_id: &str) -> Result<()> {
        self.transition(
            attempt_id,
            AttemptState::Sealed,
            Some(artifact_id),
            None,
            None,
        )
        .await
    }

    pub async fn mark_succeeded(&self, attempt_id: &str) -> Result<()> {
        self.transition(attempt_id, AttemptState::Succeeded, None, None, None)
            .await
    }

    /// The attempt found an identical artifact already catalogued.
    pub async fn mark_converged(&self, attempt_id: &str, artifact_id: &str) -> Result<()> {
        self.transition(
            attempt_id,
            AttemptState::Converged,
            Some(artifact_id),
            None,
            None,
        )
        .await
    }

    pub async fn mark_failed(&self, attempt_id: &str, code: &str, detail: &str) -> Result<()> {
        // Bounded: this column is rendered by an admin page and copied into
        // every backup, so it must never carry source or query text.
        let detail: String = detail.chars().take(500).collect();
        self.transition(
            attempt_id,
            AttemptState::Failed,
            None,
            Some(code),
            Some(&detail),
        )
        .await
    }

    pub async fn mark_cancelled(&self, attempt_id: &str) -> Result<()> {
        self.transition(attempt_id, AttemptState::Cancelled, None, None, None)
            .await
    }

    async fn transition(
        &self,
        attempt_id: &str,
        to: AttemptState,
        artifact_id: Option<&str>,
        code: Option<&str>,
        detail: Option<&str>,
    ) -> Result<()> {
        // Only a non-terminal attempt may move. A terminal one moving again
        // would mean two code paths believe they own the same build, and
        // silently allowing it would hide that.
        let done = sqlx::query(
            "UPDATE index_build_attempts
                SET state = $3,
                    artifact_id = COALESCE($4, artifact_id),
                    failure_code = COALESCE($5, failure_code),
                    failure_detail = COALESCE($6, failure_detail),
                    sealed_at = CASE WHEN $3 = 'sealed' THEN now() ELSE sealed_at END,
                    finished_at = CASE WHEN $3 IN ('succeeded','converged','failed','cancelled','expired')
                                       THEN now() ELSE finished_at END
              WHERE tenant_id = $1 AND attempt_id = $2
                AND state IN ('running', 'sealed')",
        )
        .bind(&self.tenant_id)
        .bind(attempt_id)
        .bind(to.as_str())
        .bind(artifact_id)
        .bind(code)
        .bind(detail)
        .execute(&self.pool)
        .await
        .map_err(storage_err)?;
        if done.rows_affected() == 0 {
            return Err(KernelError::InvalidInput(format!(
                "attempt {attempt_id} is already in a terminal state, or is not this tenant's; \
                 it cannot move to {}",
                to.as_str()
            )));
        }
        Ok(())
    }

    pub async fn get(&self, attempt_id: &str) -> Result<Option<AttemptRow>> {
        let row = sqlx::query(
            "SELECT attempt_id, index_version_id, artifact_plan_sha256, mode, state,
                    owner_node_id, attempt_no, l1_staging_path, artifact_id,
                    (lease_expires_at <= now()) AS lease_expired
               FROM index_build_attempts
              WHERE tenant_id = $1 AND attempt_id = $2",
        )
        .bind(&self.tenant_id)
        .bind(attempt_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?;
        row.map(Self::to_row).transpose()
    }

    fn to_row(r: sqlx::postgres::PgRow) -> Result<AttemptRow> {
        Ok(AttemptRow {
            attempt_id: r.get("attempt_id"),
            index_version_id: r.get("index_version_id"),
            artifact_plan_sha256: r.get("artifact_plan_sha256"),
            mode: r.get("mode"),
            state: AttemptState::parse(r.get::<String, _>("state").as_str())?,
            owner_node_id: r.get("owner_node_id"),
            attempt_no: r.get("attempt_no"),
            l1_staging_path: r.get("l1_staging_path"),
            artifact_id: r.get("artifact_id"),
            lease_expired: r.get("lease_expired"),
        })
    }

    /// Move `running` attempts whose lease has passed to `expired`.
    ///
    /// Only `running`. A `sealed` attempt has content on disk somewhere and its
    /// fate is decided by [`Self::reconcile_sealed`], which needs to know
    /// whether the staging directory still exists — a question this query
    /// cannot answer.
    pub async fn expire_stale(&self) -> Result<u64> {
        let done = sqlx::query(
            "UPDATE index_build_attempts
                SET state = 'expired', finished_at = now(),
                    failure_code = COALESCE(failure_code, 'lease_expired')
              WHERE tenant_id = $1 AND state = 'running' AND lease_expires_at <= now()",
        )
        .bind(&self.tenant_id)
        .execute(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(done.rows_affected())
    }

    /// Attempts stuck at `sealed`: content exists but publication never
    /// completed.
    pub async fn sealed_awaiting_publication(&self) -> Result<Vec<AttemptRow>> {
        let rows = sqlx::query(
            "SELECT attempt_id, index_version_id, artifact_plan_sha256, mode, state,
                    owner_node_id, attempt_no, l1_staging_path, artifact_id,
                    (lease_expires_at <= now()) AS lease_expired
               FROM index_build_attempts
              WHERE tenant_id = $1 AND state = 'sealed'
              ORDER BY sealed_at",
        )
        .bind(&self.tenant_id)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;
        rows.into_iter().map(Self::to_row).collect()
    }

    /// A `sealed` attempt on this exact plan whose lease has not lapsed.
    ///
    /// The partial unique index enforces single-flight over `running` only, so
    /// an attempt that sealed and then failed before publishing drops out of it
    /// and a second builder can claim the same plan. That second build would
    /// duplicate work and — because the lexical engine is not byte-deterministic
    /// — produce a SECOND artifact id for one plan, leaving two verified
    /// artifacts where the convergence rule expected one.
    ///
    /// A caller checks this before claiming, and defers. An EXPIRED lease is
    /// deliberately not reported: its owner is gone, the reconciler will abandon
    /// it, and blocking on it would turn a dead node into a permanent lock on
    /// that plan.
    pub async fn sealed_for_plan(
        &self,
        index_version_id: &str,
        artifact_plan_sha256: &str,
    ) -> Result<Option<AttemptRow>> {
        let row = sqlx::query(
            "SELECT attempt_id, index_version_id, artifact_plan_sha256, mode, state,
                    owner_node_id, attempt_no, l1_staging_path, artifact_id,
                    (lease_expires_at <= now()) AS lease_expired
               FROM index_build_attempts
              WHERE tenant_id = $1 AND index_version_id = $2
                AND artifact_plan_sha256 = $3 AND state = 'sealed'
                AND lease_expires_at > now()
              ORDER BY sealed_at
              LIMIT 1",
        )
        .bind(&self.tenant_id)
        .bind(index_version_id)
        .bind(artifact_plan_sha256)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?;
        row.map(Self::to_row).transpose()
    }
}

/// Tenants with a `sealed` attempt owned by this node.
///
/// The one query in this module that is NOT tenant-scoped, and it is
/// deliberately a free function rather than a method on the tenant-bound
/// handle so it cannot be reached from a request path. Its only caller is the
/// startup/interval reconciler, which then does all its real work through a
/// per-tenant handle.
///
/// Scoped to this node's OWN attempts: another node's sealed work is that
/// node's to resume, and once its lease lapses any node may abandon it — which
/// is a separate sweep, not this enumeration.
pub async fn tenants_with_sealed_attempts(
    pool: &PgPool,
    owner_node_id: &str,
) -> Result<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT tenant_id FROM index_build_attempts
          WHERE state = 'sealed' AND owner_node_id = $1
          ORDER BY tenant_id",
    )
    .bind(owner_node_id)
    .fetch_all(pool)
    .await
    .map_err(storage_err)?;
    Ok(rows.into_iter().map(|(t,)| t).collect())
}

/// What the reconciler decided about a `sealed`-but-unpublished attempt.
///
/// The rule (§7.4): resume only if the OWNING node still has the staging
/// directory AND its lease is fresh. Otherwise abandon. **A `sealed` row is
/// never read as "L2 exists"** — it means content was sealed locally, which is
/// a different claim entirely, and treating it as publication would advertise
/// an artifact that may have no bytes anywhere.
#[derive(Debug, Clone, PartialEq)]
pub enum SealedVerdict {
    /// This node owns it, the lease is fresh, the staging directory is there.
    Resume,
    /// Someone else owns it and is still alive. Leave it alone.
    NotOurs { owner_node_id: String },
    /// The lease has expired or the staging directory is gone.
    Abandon { reason: &'static str },
}

/// Decide what to do with one sealed attempt.
///
/// Pure, so the decision can be tested without a filesystem or a clock. The
/// caller supplies what it observed; this only encodes the rule.
pub fn reconcile_sealed(
    attempt: &AttemptRow,
    this_node_id: &str,
    staging_dir_present: bool,
) -> SealedVerdict {
    if attempt.owner_node_id != this_node_id {
        if attempt.lease_expired {
            // Its owner is gone. Any node may abandon it: leaving it forever
            // would block the single-flight index on that plan.
            return SealedVerdict::Abandon {
                reason: "publication_abandoned",
            };
        }
        return SealedVerdict::NotOurs {
            owner_node_id: attempt.owner_node_id.clone(),
        };
    }
    if attempt.lease_expired {
        return SealedVerdict::Abandon {
            reason: "publication_abandoned",
        };
    }
    if !staging_dir_present {
        // Ours and fresh, but the content is gone -- a restart with an
        // ephemeral filesystem, which Container Apps does on every scale event.
        return SealedVerdict::Abandon {
            reason: "staging_lost",
        };
    }
    SealedVerdict::Resume
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attempt(owner: &str, expired: bool) -> AttemptRow {
        AttemptRow {
            attempt_id: "att-1".into(),
            index_version_id: "idx2-v".into(),
            artifact_plan_sha256: "p".repeat(64),
            mode: "mirror".into(),
            state: AttemptState::Sealed,
            owner_node_id: owner.into(),
            attempt_no: 1,
            l1_staging_path: Some("/tmp/att-1".into()),
            artifact_id: Some("a".repeat(64)),
            lease_expired: expired,
        }
    }

    #[test]
    fn our_fresh_attempt_with_its_staging_directory_resumes() {
        assert_eq!(
            reconcile_sealed(&attempt("node-a", false), "node-a", true),
            SealedVerdict::Resume
        );
    }

    /// Ours and fresh, but the content is gone: Container Apps discards the
    /// filesystem on every scale event, so this is the common case, not a rare
    /// one.
    #[test]
    fn a_lost_staging_directory_abandons_even_when_the_lease_is_fresh() {
        assert_eq!(
            reconcile_sealed(&attempt("node-a", false), "node-a", false),
            SealedVerdict::Abandon {
                reason: "staging_lost"
            }
        );
    }

    /// Another node's live attempt is left alone. Abandoning it would race a
    /// publication that is still in progress.
    #[test]
    fn another_live_nodes_attempt_is_left_alone() {
        assert_eq!(
            reconcile_sealed(&attempt("node-b", false), "node-a", true),
            SealedVerdict::NotOurs {
                owner_node_id: "node-b".into()
            }
        );
    }

    /// Once the owner's lease expires, any node may abandon it -- otherwise a
    /// dead node's sealed attempt would block that plan's single-flight index
    /// forever.
    #[test]
    fn a_dead_nodes_attempt_is_abandoned_by_anyone() {
        assert_eq!(
            reconcile_sealed(&attempt("node-b", true), "node-a", false),
            SealedVerdict::Abandon {
                reason: "publication_abandoned"
            }
        );
        assert_eq!(
            reconcile_sealed(&attempt("node-b", true), "node-a", true),
            SealedVerdict::Abandon {
                reason: "publication_abandoned"
            },
            "a staging directory we can see is not ours to trust"
        );
    }

    #[test]
    fn terminal_states_are_classified_correctly() {
        for s in [
            AttemptState::Succeeded,
            AttemptState::Converged,
            AttemptState::Failed,
            AttemptState::Cancelled,
            AttemptState::Expired,
        ] {
            assert!(s.is_terminal(), "{s:?}");
        }
        assert!(!AttemptState::Running.is_terminal());
        assert!(
            !AttemptState::Sealed.is_terminal(),
            "sealed is mid-flight: content exists but publication has not finished"
        );
    }
}
