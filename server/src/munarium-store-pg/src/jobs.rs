// SPDX-License-Identifier: Apache-2.0
//! Durable build jobs.
//!
//! A JOB is the durable request; an ATTEMPT is one execution. The split is
//! what lets builds and queries scale independently: a request-serving node
//! enqueues and answers immediately, a builder claims and executes, and a
//! builder that dies mid-job loses nothing but its lease.
//!
//! Three rules, each learned somewhere:
//!
//! - **Claiming is `FOR UPDATE SKIP LOCKED`**, one short transaction, GLOBAL
//!   across tenants — a builder serves every tenant, and a tenant-scoped
//!   claim would need one builder per tenant to drain the queue.
//! - **A claimed job is a lease, not a title.** A `running` job whose
//!   `claimed_at` is older than the lease is re-offered, with a bounded
//!   attempt ceiling so a poisonous job cannot be retried forever. (The
//!   Matrix tree paid for this lesson first: until a late closeout, a
//!   claimed job there was a title, and a killed worker orphaned it.)
//! - **Completion is idempotent.** Completing a job that is no longer yours —
//!   the lease lapsed and someone else reclaimed it — is a no-op reported as
//!   such, never a clobber.

use sqlx::{PgPool, Row};

use munarium_core::{KernelError, Result};

use crate::storage_err;

/// What a job builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    /// Mirror every serving-required version of a scope.
    Backfill,
    /// Mirror one index version.
    Rebuild,
    /// The direct build of a collection.
    Direct,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Backfill => "backfill",
            Self::Rebuild => "rebuild",
            Self::Direct => "direct",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "backfill" => Self::Backfill,
            "rebuild" => Self::Rebuild,
            "direct" => Self::Direct,
            other => {
                return Err(KernelError::InvalidInput(format!(
                    "unknown build-job kind {other:?}"
                )))
            }
        })
    }
}

/// A job row, as callers read it.
#[derive(Debug, Clone)]
pub struct BuildJob {
    pub tenant_id: String,
    pub job_id: String,
    pub kind: String,
    pub scope_kind: Option<String>,
    pub scope_id: Option<String>,
    pub index_version_id: Option<String>,
    pub params: serde_json::Value,
    pub state: String,
    pub attempt_ids: Vec<String>,
    pub correlation_id: Option<String>,
    pub claimed_by: Option<String>,
    pub attempts: i32,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
    pub requested_by: String,
    pub created_at: String,
}

/// What an enqueue did.
#[derive(Debug, Clone, PartialEq)]
pub enum EnqueueOutcome {
    /// This call created the job.
    Enqueued(String),
    /// An open job for the same target already exists; here it is. Not an
    /// error — two callers asking for the same build want one build.
    AlreadyOpen(String),
}

/// The target of a job, by kind.
#[derive(Debug, Clone)]
pub enum JobTarget<'a> {
    Scope {
        scope_kind: &'a str,
        scope_id: &'a str,
    },
    Version(&'a str),
}

/// Access to the job queue. Tenant scoping is PER CALL on the read/enqueue
/// surface and deliberately absent from `claim_any`.
#[derive(Debug, Clone)]
pub struct BuildJobs {
    pool: PgPool,
}

impl BuildJobs {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Enqueue a job, deduplicating against open jobs for the same target.
    pub async fn enqueue(
        &self,
        tenant_id: &str,
        kind: JobKind,
        target: JobTarget<'_>,
        params: serde_json::Value,
        requested_by: &str,
        correlation_id: Option<&str>,
    ) -> Result<EnqueueOutcome> {
        let (scope_kind, scope_id, version) = match target {
            JobTarget::Scope {
                scope_kind,
                scope_id,
            } => (Some(scope_kind), Some(scope_id), None),
            JobTarget::Version(v) => (None, None, Some(v)),
        };
        let job_id = format!("bjob-{}", uuid::Uuid::new_v4().simple());
        let inserted = sqlx::query(
            "INSERT INTO index_build_jobs
                 (tenant_id, job_id, kind, scope_kind, scope_id, index_version_id,
                  params, requested_by, correlation_id)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
             ON CONFLICT (tenant_id, kind,
                          COALESCE(scope_kind, ''), COALESCE(scope_id, ''),
                          COALESCE(index_version_id, ''))
                 WHERE state IN ('pending', 'running')
             DO NOTHING
             RETURNING job_id",
        )
        .bind(tenant_id)
        .bind(&job_id)
        .bind(kind.as_str())
        .bind(scope_kind)
        .bind(scope_id)
        .bind(version)
        .bind(&params)
        .bind(requested_by)
        .bind(correlation_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?;

        if inserted.is_some() {
            return Ok(EnqueueOutcome::Enqueued(job_id));
        }
        // The open job that beat us. Read it back rather than guessing.
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT job_id FROM index_build_jobs
              WHERE tenant_id = $1 AND kind = $2
                AND COALESCE(scope_kind,'') = COALESCE($3,'')
                AND COALESCE(scope_id,'') = COALESCE($4,'')
                AND COALESCE(index_version_id,'') = COALESCE($5,'')
                AND state IN ('pending','running')
              ORDER BY created_at LIMIT 1",
        )
        .bind(tenant_id)
        .bind(kind.as_str())
        .bind(scope_kind)
        .bind(scope_id)
        .bind(version)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?;
        match existing {
            Some(id) => Ok(EnqueueOutcome::AlreadyOpen(id)),
            // The open job completed between our insert and this read; retry
            // once by recursing would be elegant and unbounded — one plain
            // re-insert attempt is enough, and a second conflict is a real
            // race worth surfacing.
            None => Err(KernelError::Storage(
                "job enqueue lost a race with a completing duplicate; retry".into(),
            )),
        }
    }

    /// Claim the next runnable job: pending FIFO, plus lease-lapsed running
    /// jobs with attempts to spare. One short SKIP LOCKED transaction.
    pub async fn claim_any(
        &self,
        node_id: &str,
        lease_secs: i64,
        max_attempts: i32,
    ) -> Result<Option<BuildJob>> {
        let mut tx = self.pool.begin().await.map_err(storage_err)?;
        let row = sqlx::query(
            "SELECT tenant_id, job_id FROM index_build_jobs
              WHERE (state = 'pending'
                     OR (state = 'running'
                         AND claimed_at < now() - make_interval(secs => $1)
                         AND attempts < $2))
              ORDER BY created_at
              FOR UPDATE SKIP LOCKED
              LIMIT 1",
        )
        .bind(lease_secs as f64)
        .bind(max_attempts)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage_err)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let tenant: String = row.get("tenant_id");
        let job_id: String = row.get("job_id");
        sqlx::query(
            "UPDATE index_build_jobs
                SET state = 'running', claimed_by = $3, claimed_at = now(),
                    attempts = attempts + 1, updated_at = now()
              WHERE tenant_id = $1 AND job_id = $2",
        )
        .bind(&tenant)
        .bind(&job_id)
        .bind(node_id)
        .execute(&mut *tx)
        .await
        .map_err(storage_err)?;
        tx.commit().await.map_err(storage_err)?;
        self.get(&tenant, &job_id).await
    }

    /// Extend the holder's lease: `claimed_at` moves to now, so `claim_any`'s
    /// lapse test measures from the last heartbeat rather than from the
    /// claim. Returns false when the job is no longer this node's `running`
    /// job — the lease lapsed and was re-offered, or it was cancelled — which
    /// the builder treats as "stop; the result would be recorded on someone
    /// else's attempt".
    ///
    /// Without this a claimed job was a title, not a lease: a build longer
    /// than the lease (the newspaper batches are exactly that) was re-offered
    /// to a second replica mid-flight, the first builder's `complete` became
    /// a no-op, and its real outcome was lost.
    pub async fn heartbeat(&self, tenant_id: &str, job_id: &str, node_id: &str) -> Result<bool> {
        let updated = sqlx::query(
            "UPDATE index_build_jobs
                SET claimed_at = now(), updated_at = now()
              WHERE tenant_id = $1 AND job_id = $2
                AND state = 'running' AND claimed_by = $3",
        )
        .bind(tenant_id)
        .bind(job_id)
        .bind(node_id)
        .execute(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(updated.rows_affected() == 1)
    }

    /// Record a terminal outcome, idempotently and only for the holder.
    ///
    /// Returns false when the job was not this claimant's `running` job any
    /// more — the lease lapsed and someone else took it, or it was cancelled.
    /// The caller logs and moves on; clobbering the new holder's state would
    /// be worse than losing this report.
    pub async fn complete(
        &self,
        tenant_id: &str,
        job_id: &str,
        node_id: &str,
        outcome: std::result::Result<serde_json::Value, String>,
        attempt_id: Option<&str>,
    ) -> Result<bool> {
        let (state, result, error) = match outcome {
            Ok(v) => ("succeeded", Some(v), None),
            Err(e) => ("failed", None, Some(e)),
        };
        let updated = sqlx::query(
            "UPDATE index_build_jobs
                SET state = $4, result = $5, error = $6,
                    attempt_ids = CASE WHEN $7::text IS NULL THEN attempt_ids
                                       ELSE array_append(attempt_ids, $7) END,
                    updated_at = now()
              WHERE tenant_id = $1 AND job_id = $2
                AND state = 'running' AND claimed_by = $3",
        )
        .bind(tenant_id)
        .bind(job_id)
        .bind(node_id)
        .bind(state)
        .bind(result)
        .bind(error)
        .bind(attempt_id)
        .execute(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(updated.rows_affected() == 1)
    }

    /// Cancel a job that has not finished. A running job's executing attempt
    /// keeps its own lease semantics; cancellation stops the JOB from being
    /// re-offered, which is what an operator asking to stop actually needs.
    pub async fn cancel(&self, tenant_id: &str, job_id: &str) -> Result<bool> {
        let updated = sqlx::query(
            "UPDATE index_build_jobs
                SET state = 'cancelled', updated_at = now()
              WHERE tenant_id = $1 AND job_id = $2 AND state IN ('pending','running')",
        )
        .bind(tenant_id)
        .bind(job_id)
        .execute(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(updated.rows_affected() == 1)
    }

    pub async fn get(&self, tenant_id: &str, job_id: &str) -> Result<Option<BuildJob>> {
        let row = sqlx::query(
            "SELECT tenant_id, job_id, kind, scope_kind, scope_id, index_version_id,
                    params, state, attempt_ids, correlation_id, claimed_by, attempts,
                    result, error, requested_by, created_at::text AS created_at_text
               FROM index_build_jobs
              WHERE tenant_id = $1 AND job_id = $2",
        )
        .bind(tenant_id)
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(row.map(row_to_job))
    }

    /// A tenant's recent jobs, newest first, bounded.
    pub async fn list(&self, tenant_id: &str, limit: i64) -> Result<Vec<BuildJob>> {
        let rows = sqlx::query(
            "SELECT tenant_id, job_id, kind, scope_kind, scope_id, index_version_id,
                    params, state, attempt_ids, correlation_id, claimed_by, attempts,
                    result, error, requested_by, created_at::text AS created_at_text
               FROM index_build_jobs
              WHERE tenant_id = $1
              ORDER BY created_at DESC
              LIMIT $2",
        )
        .bind(tenant_id)
        .bind(limit.clamp(1, 200))
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(rows.into_iter().map(row_to_job).collect())
    }
}

fn row_to_job(r: sqlx::postgres::PgRow) -> BuildJob {
    BuildJob {
        tenant_id: r.get("tenant_id"),
        job_id: r.get("job_id"),
        kind: r.get("kind"),
        scope_kind: r.get("scope_kind"),
        scope_id: r.get("scope_id"),
        index_version_id: r.get("index_version_id"),
        params: r.get("params"),
        state: r.get("state"),
        attempt_ids: r.get("attempt_ids"),
        correlation_id: r.get("correlation_id"),
        claimed_by: r.get("claimed_by"),
        attempts: r.get("attempts"),
        result: r.get("result"),
        error: r.get("error"),
        requested_by: r.get("requested_by"),
        created_at: r.get("created_at_text"),
    }
}
