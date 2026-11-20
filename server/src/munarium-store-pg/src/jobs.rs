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
//!
//! ClearView R4 adds a fencing generation (the job's `attempts` after a
//! claim). Lease renewal and completion require it, and a queued build's
//! writes outside this table require it too, through [`JobFence`] and
//! [`lock_generation`].

use sqlx::{PgPool, Row};

use munarium_core::{KernelError, Result};

use crate::storage_err;

/// The safe, human-readable `error` recorded when an expired final claim is
/// failed by [`BuildJobs::fail_expired_final_claims`]. It names no table,
/// node or driver detail. The machine-readable code (`attempts_exhausted`)
/// waits for the C2 hub record and is not stored or returned yet.
pub const ATTEMPTS_EXHAUSTED_MESSAGE: &str =
    "build job failed: its final attempt stopped renewing its lease before completing";

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

/// The safe message a superseded build reports when a fenced write is refused.
/// The job's own completion is refused by the same fence, so this never
/// overwrites the current holder's result; it reaches logs and the attempt's
/// failure detail.
pub const STALE_GENERATION_MESSAGE: &str =
    "build job claim was superseded; this execution may not commit or publish";

/// The authority one job claim carries (ClearView R4).
///
/// Built from the row [`BuildJobs::claim_any`] returned and the node that
/// claimed it. A queued build passes it to every write outside the job row:
/// the logical-version commit, the artifact-attempt link, the publication
/// intent and the staged binding. Each takes the job row's lock with
/// [`lock_generation`] and writes only while this claim is still current.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobFence {
    pub tenant_id: String,
    pub job_id: String,
    pub node_id: String,
    /// The job's `attempts` after this claim.
    pub generation: i32,
}

impl JobFence {
    /// The fence of a claim this node just made.
    pub fn of_claim(job: &BuildJob, node_id: &str) -> Self {
        Self {
            tenant_id: job.tenant_id.clone(),
            job_id: job.job_id.clone(),
            node_id: node_id.to_string(),
            generation: job.attempts,
        }
    }

    /// Refuse a fence for another tenant. Every fenced write goes through a
    /// tenant-bound handle, and a fence is never permission to cross it.
    pub fn require_tenant(&self, tenant_id: &str) -> Result<()> {
        if self.tenant_id != tenant_id {
            return Err(KernelError::InvalidInput(
                "a build-job fence was presented to another tenant's handle".into(),
            ));
        }
        Ok(())
    }

    /// The error a fenced write returns once its generation is retired.
    pub fn stale(&self) -> KernelError {
        KernelError::Storage(STALE_GENERATION_MESSAGE.into())
    }
}

/// Lock the fence's job row for the rest of `conn`'s transaction and report
/// whether the fence is still current: `running`, `attempts = generation` and
/// `claimed_by = node`.
///
/// `FOR UPDATE` on the row every retirement writes. Cancel, completion,
/// reclaim and expired-claim cleanup are all UPDATEs of this row, so a
/// retirement that commits first is seen here, and one that comes later waits
/// until the caller's write has committed. Call it inside a transaction; on
/// `false`, write nothing and roll back.
pub async fn lock_generation(conn: &mut sqlx::PgConnection, fence: &JobFence) -> Result<bool> {
    let row = sqlx::query(
        "SELECT state, attempts, claimed_by FROM index_build_jobs
          WHERE tenant_id = $1 AND job_id = $2
          FOR UPDATE",
    )
    .bind(&fence.tenant_id)
    .bind(&fence.job_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(storage_err)?;
    Ok(row.is_some_and(|r| {
        r.get::<String, _>("state") == "running"
            && r.get::<i32, _>("attempts") == fence.generation
            && r.get::<Option<String>, _>("claimed_by").as_deref() == Some(fence.node_id.as_str())
    }))
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
    ///
    /// The returned job is the row as THIS claim wrote it, captured with
    /// `RETURNING` inside the claiming transaction. Its `attempts` is the
    /// claim's fencing generation (ClearView R4): pass it to
    /// [`heartbeat_fenced`](Self::heartbeat_fenced) and
    /// [`complete_fenced`](Self::complete_fenced). Reading the row after
    /// commit, as v1.4 did, could observe a later claimant's generation.
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
        let claimed = sqlx::query(
            "UPDATE index_build_jobs
                SET state = 'running', claimed_by = $3, claimed_at = now(),
                    attempts = attempts + 1, updated_at = now()
              WHERE tenant_id = $1 AND job_id = $2
          RETURNING tenant_id, job_id, kind, scope_kind, scope_id, index_version_id,
                    params, state, attempt_ids, correlation_id, claimed_by, attempts,
                    result, error, requested_by, created_at::text AS created_at_text",
        )
        .bind(&tenant)
        .bind(&job_id)
        .bind(node_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage_err)?;
        let job = row_to_job(claimed);
        tx.commit().await.map_err(storage_err)?;
        Ok(Some(job))
    }

    /// Move expired FINAL claims to `failed` and release their targets.
    ///
    /// A running job whose lease lapsed with `attempts >= max_attempts` is
    /// never re-offered by [`claim_any`](Self::claim_any). In v1.4 it stayed
    /// `running` forever and held the open-target index, so the target could
    /// not be enqueued again. Each row is rechecked under its row lock (lease,
    /// state and ceiling), so a lease renewal that commits first wins and the
    /// row is skipped; a renewal that arrives second finds the job `failed`.
    /// Rows locked by another cleaner are skipped, so concurrent cleaners
    /// fail each job once. Committed build effects are left for recovery.
    ///
    /// Global across tenants, like claiming. Returns `(tenant_id, job_id)`
    /// for each job this call failed.
    pub async fn fail_expired_final_claims(
        &self,
        lease_secs: i64,
        max_attempts: i32,
        limit: i64,
    ) -> Result<Vec<(String, String)>> {
        let rows = sqlx::query(
            "WITH expired AS (
                 SELECT tenant_id, job_id FROM index_build_jobs
                  WHERE state = 'running'
                    AND attempts >= $2
                    AND claimed_at < now() - make_interval(secs => $1)
                  ORDER BY claimed_at
                  FOR UPDATE SKIP LOCKED
                  LIMIT $3)
             UPDATE index_build_jobs j
                SET state = 'failed', error = $4, updated_at = now()
               FROM expired e
              WHERE j.tenant_id = e.tenant_id AND j.job_id = e.job_id
          RETURNING j.tenant_id, j.job_id",
        )
        .bind(lease_secs as f64)
        .bind(max_attempts)
        .bind(limit.clamp(1, 1000))
        .bind(ATTEMPTS_EXHAUSTED_MESSAGE)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(rows
            .into_iter()
            .map(|r| (r.get("tenant_id"), r.get("job_id")))
            .collect())
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
        self.renew(tenant_id, job_id, node_id, None).await
    }

    /// [`heartbeat`](Self::heartbeat) fenced by the claim's generation.
    ///
    /// Also requires `attempts = generation`, so a holder whose lease lapsed
    /// and was reclaimed under the same node name (a restarted StatefulSet
    /// pod, a fixed `MUNARIUM_INSTANCE_ID`) is refused instead of renewing a
    /// lease that now belongs to the newer claim.
    pub async fn heartbeat_fenced(
        &self,
        tenant_id: &str,
        job_id: &str,
        node_id: &str,
        generation: i32,
    ) -> Result<bool> {
        self.renew(tenant_id, job_id, node_id, Some(generation))
            .await
    }

    async fn renew(
        &self,
        tenant_id: &str,
        job_id: &str,
        node_id: &str,
        generation: Option<i32>,
    ) -> Result<bool> {
        let updated = sqlx::query(
            "UPDATE index_build_jobs
                SET claimed_at = now(), updated_at = now()
              WHERE tenant_id = $1 AND job_id = $2
                AND state = 'running' AND claimed_by = $3
                AND ($4::int IS NULL OR attempts = $4)",
        )
        .bind(tenant_id)
        .bind(job_id)
        .bind(node_id)
        .bind(generation)
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
        self.finish(tenant_id, job_id, node_id, None, outcome, attempt_id)
            .await
    }

    /// [`complete`](Self::complete) fenced by the claim's generation: a
    /// holder whose claim was superseded, even under the same node name,
    /// gets `false` and records nothing.
    pub async fn complete_fenced(
        &self,
        tenant_id: &str,
        job_id: &str,
        node_id: &str,
        generation: i32,
        outcome: std::result::Result<serde_json::Value, String>,
        attempt_id: Option<&str>,
    ) -> Result<bool> {
        self.finish(
            tenant_id,
            job_id,
            node_id,
            Some(generation),
            outcome,
            attempt_id,
        )
        .await
    }

    async fn finish(
        &self,
        tenant_id: &str,
        job_id: &str,
        node_id: &str,
        generation: Option<i32>,
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
                AND state = 'running' AND claimed_by = $3
                AND ($8::int IS NULL OR attempts = $8)",
        )
        .bind(tenant_id)
        .bind(job_id)
        .bind(node_id)
        .bind(state)
        .bind(result)
        .bind(error)
        .bind(attempt_id)
        .bind(generation)
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
