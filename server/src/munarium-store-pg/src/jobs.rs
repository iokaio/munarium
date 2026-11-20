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
#[derive(Debug, Clone, PartialEq)]
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

/// Record the generation a fenced builder just claimed (ClearView R6). In
/// the claiming transaction, so a committed claim always has its run row and
/// a rolled-back one never does. A generation without a run row was claimed
/// by a v1.4 builder, and its per-version history is reported unavailable.
async fn record_run(conn: &mut sqlx::PgConnection, job: &BuildJob, node_id: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO index_build_job_runs (tenant_id, job_id, job_generation, node_id)
         VALUES ($1,$2,$3,$4)
         ON CONFLICT (tenant_id, job_id, job_generation) DO NOTHING",
    )
    .bind(&job.tenant_id)
    .bind(&job.job_id)
    .bind(job.attempts)
    .bind(node_id)
    .execute(&mut *conn)
    .await
    .map_err(storage_err)?;
    Ok(())
}

/// Why a worker-local [`JobSignal`] stopped its build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The job was cancelled.
    Cancelled,
    /// The claim's generation was retired some other way: the lease lapsed
    /// and the job was reclaimed, completed, or failed by cleanup.
    LeaseLost,
}

type StopHook = Box<dyn FnOnce(StopReason) + Send>;

#[derive(Default)]
struct SignalInner {
    /// 0 = running, 1 = cancelled, 2 = lease lost.
    stop: std::sync::atomic::AtomicU8,
    /// The last lease renewal failed with an error, so whether the claim is
    /// still current is unknown until it is revalidated.
    uncertain: std::sync::atomic::AtomicBool,
    hooks: std::sync::Mutex<Vec<StopHook>>,
}

/// A worker-local stop signal for one job claim (ClearView R6).
///
/// The builder signals it on CONFIRMED loss of the claim: a cancel, or a
/// lease renewal refused because the generation was retired. A renewal that
/// fails with an error only marks the lease [`uncertain`](Self::uncertain);
/// the build then revalidates against PostgreSQL at its next safe boundary
/// instead of stopping on a guess, and every shared write still rechecks the
/// claim under the job-row lock. Builds check the signal at their safe
/// boundaries (between sources, between versions, before seal and before
/// upload) and stop there, which releases the builder slot.
///
/// [`on_stop`](Self::on_stop) registers work to run when the signal stops.
/// Its intended consumer is the opt-in R3 PDF child process, which should
/// register a hook that kills and reaps the child; that wiring is a
/// follow-up. A legacy in-process parse cannot be interrupted and holds its
/// slot until it returns, after which the next boundary stops the build.
#[derive(Clone, Default)]
pub struct JobSignal {
    inner: std::sync::Arc<SignalInner>,
}

impl std::fmt::Debug for JobSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobSignal")
            .field("stopped", &self.stopped())
            .field("uncertain", &self.uncertain())
            .finish()
    }
}

impl JobSignal {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stop the build. The first reason wins; hooks run once, on this call.
    pub fn stop(&self, reason: StopReason) {
        use std::sync::atomic::Ordering;
        let code = match reason {
            StopReason::Cancelled => 1,
            StopReason::LeaseLost => 2,
        };
        if self
            .inner
            .stop
            .compare_exchange(0, code, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        let hooks =
            std::mem::take(&mut *self.inner.hooks.lock().unwrap_or_else(|e| e.into_inner()));
        for hook in hooks {
            hook(reason);
        }
    }

    /// Why the build was stopped, or `None` while it may continue.
    pub fn stopped(&self) -> Option<StopReason> {
        match self.inner.stop.load(std::sync::atomic::Ordering::SeqCst) {
            0 => None,
            1 => Some(StopReason::Cancelled),
            _ => Some(StopReason::LeaseLost),
        }
    }

    /// Record whether the last lease renewal's outcome is unknown.
    pub fn set_uncertain(&self, uncertain: bool) {
        self.inner
            .uncertain
            .store(uncertain, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn uncertain(&self) -> bool {
        self.inner
            .uncertain
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Run `hook` when the signal stops, or now if it already has.
    pub fn on_stop(&self, hook: impl FnOnce(StopReason) + Send + 'static) {
        let mut hooks = self.inner.hooks.lock().unwrap_or_else(|e| e.into_inner());
        match self.stopped() {
            Some(reason) => {
                drop(hooks);
                hook(reason);
            }
            None => hooks.push(Box::new(hook)),
        }
    }
}

/// How much of a job's per-version history was recorded (ClearView R6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryStatus {
    /// Every claimed generation was claimed by a fenced builder, so the
    /// versions listed are everything the job touched. A job never claimed
    /// has a recorded, empty history: it started no version.
    Recorded,
    /// Some generations were claimed by a builder that kept no history.
    Partial,
    /// No claimed generation recorded history: a v1.4 job. An empty page
    /// means "unknown", never "nothing".
    Unavailable,
}

impl HistoryStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Recorded => "recorded",
            Self::Partial => "partial",
            Self::Unavailable => "unavailable",
        }
    }
}

/// What one job did to one index version, from the durable records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobVersionOutcome {
    pub index_version_id: String,
    /// Position in the job's selected set, when the job selected it.
    pub selected_ordinal: Option<i32>,
    /// The generation that committed this logical version (a direct build).
    pub committed_generation: Option<i32>,
    /// `reused` or `deferred`: what the job found without publishing.
    pub disposition: Option<String>,
    /// How many artifact attempts the job linked to this version.
    pub attempt_count: i64,
    /// The newest linked attempt and its generation and state.
    pub attempt_id: Option<String>,
    pub attempt_generation: Option<i32>,
    pub attempt_state: Option<String>,
    /// The newest attempt's publication intent: `pending` (unresolved, so the
    /// manifest outcome is unknown), `published`, `converged` or `failed`.
    pub intent_state: Option<String>,
    pub artifact_id: Option<String>,
    /// The catalog state of the intent's artifact, when it was catalogued.
    pub artifact_state: Option<String>,
    /// Whether the version's `staged` slot binds the intent's artifact.
    pub staged_bound: bool,
}

/// One page of a job's per-version history.
#[derive(Debug, Clone, PartialEq)]
pub struct JobVersionHistory {
    pub job: BuildJob,
    pub history: HistoryStatus,
    pub versions: Vec<JobVersionOutcome>,
    /// The last version on this page, when more may follow.
    pub next_after: Option<String>,
}

/// How a builder picks its next job (ClearView R8 / B1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClaimPolicy {
    /// The v1.4 global queue: oldest eligible job first, across tenants.
    #[default]
    Fifo,
    /// Equal claim turns for every tenant with eligible work; within a
    /// tenant, its oldest eligible job. Opt-in.
    TenantFair,
}

impl ClaimPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fifo => "fifo",
            Self::TenantFair => "tenant_fair",
        }
    }

    /// Parse the `MUNARIUM_DATASTORE_JOB_SCHEDULING` value.
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "fifo" => Ok(Self::Fifo),
            "tenant_fair" => Ok(Self::TenantFair),
            other => Err(KernelError::InvalidInput(format!(
                "unknown build-job scheduling policy {other:?}; expected fifo or tenant_fair"
            ))),
        }
    }
}

/// The stage a queued build has reached (ClearView R9). Closed: a reader
/// can rely on these values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressPhase {
    /// A direct build extracting, chunking and embedding its sources.
    Extracting,
    /// A backfill moving between the versions it selected.
    Versions,
    /// Sealing an artifact in local staging.
    Sealing,
    /// Deciding to publish: the publication intent.
    Publishing,
    /// Uploading components and the manifest.
    Uploading,
}

impl ProgressPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Extracting => "extracting",
            Self::Versions => "versions",
            Self::Sealing => "sealing",
            Self::Publishing => "publishing",
            Self::Uploading => "uploading",
        }
    }
}

/// A job's current progress, as the holder last recorded it.
#[derive(Debug, Clone, PartialEq)]
pub struct JobProgress {
    pub job: BuildJob,
    pub phase: Option<String>,
    /// Sources for a direct build, versions for a backfill. Never decreases
    /// within a claim.
    pub units_done: Option<i64>,
    /// Unknown is `None`, never zero.
    pub units_total: Option<i64>,
    pub last_advance_at: Option<String>,
}

/// One recorded transition of a job (ClearView R9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobEvent {
    /// 1, 2, 3, ... for this job, in commit order.
    pub seq: i64,
    /// `enqueued`, `claimed`, `state` or `progress`.
    pub kind: String,
    pub from_state: Option<String>,
    pub to_state: String,
    pub generation: i32,
    pub node_id: Option<String>,
    pub phase: Option<String>,
    pub recorded_at: String,
}

/// One page of a job's events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobEventPage {
    pub events: Vec<JobEvent>,
    /// Events after the cursor were pruned by retention (or were recorded
    /// before the event ring existed), so this page does not continue the
    /// history without a hole. Also true when no event after the cursor
    /// survives at all.
    pub gap: bool,
    /// The last sequence number this page covers; pass it back to continue.
    pub through: i64,
}

/// The provisional per-tenant event retention count (ClearView B2). Not
/// measured yet; see migration 0046 and the Datastore guide.
pub const DEFAULT_EVENTS_PER_TENANT: i32 = 1024;

/// Set the per-tenant event retention count. Database-wide: every writer's
/// triggers read it.
pub async fn set_event_retention(pool: &PgPool, events_per_tenant: i32) -> Result<()> {
    if events_per_tenant < 1 {
        return Err(KernelError::InvalidInput(
            "the build-job event retention count must be at least 1".into(),
        ));
    }
    sqlx::query(
        "INSERT INTO index_build_job_event_retention (singleton, events_per_tenant)
         VALUES (true, $1)
         ON CONFLICT (singleton)
         DO UPDATE SET events_per_tenant = EXCLUDED.events_per_tenant, updated_at = now()",
    )
    .bind(events_per_tenant)
    .execute(pool)
    .await
    .map_err(storage_err)?;
    Ok(())
}

/// The per-tenant event retention count in force.
pub async fn event_retention(pool: &PgPool) -> Result<i32> {
    let n: Option<i32> =
        sqlx::query_scalar("SELECT events_per_tenant FROM index_build_job_event_retention")
            .fetch_optional(pool)
            .await
            .map_err(storage_err)?;
    Ok(n.unwrap_or(DEFAULT_EVENTS_PER_TENANT))
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
        self.claim_next(node_id, lease_secs, max_attempts, ClaimPolicy::Fifo)
            .await
    }

    /// [`claim_any`](Self::claim_any) under an explicit scheduling policy
    /// (ClearView R8). [`ClaimPolicy::Fifo`] is exactly the v1.4 global
    /// queue. [`ClaimPolicy::TenantFair`] gives each tenant with eligible
    /// work an equal turn: the tenant served least recently (never-served
    /// first) claims its oldest eligible job, ties broken by job id. Both
    /// policies claim pending jobs and reclaim expired leases with attempts
    /// remaining, and both return the generation captured in the claiming
    /// transaction.
    ///
    /// In fair mode the tenant's turn row is locked with the job row and
    /// advanced in the same transaction, so a turn is one committed claim: a
    /// rolled-back claim leaves it available, and two concurrent builders
    /// never spend one tenant's turn twice (the second skips that tenant for
    /// this claim). A candidate locked by another transaction is passed over
    /// for this claim, as in FIFO mode.
    pub async fn claim_next(
        &self,
        node_id: &str,
        lease_secs: i64,
        max_attempts: i32,
        policy: ClaimPolicy,
    ) -> Result<Option<BuildJob>> {
        let mut tx = self.pool.begin().await.map_err(storage_err)?;
        let select = match policy {
            ClaimPolicy::Fifo => {
                "SELECT tenant_id, job_id FROM index_build_jobs
                  WHERE (state = 'pending'
                         OR (state = 'running'
                             AND claimed_at < now() - make_interval(secs => $1)
                             AND attempts < $2))
                  ORDER BY created_at
                  FOR UPDATE SKIP LOCKED
                  LIMIT 1"
            }
            ClaimPolicy::TenantFair => {
                "SELECT j.tenant_id, j.job_id
                   FROM index_build_jobs j
                   JOIN index_build_tenant_turns t ON t.tenant_id = j.tenant_id
                  WHERE (j.state = 'pending'
                         OR (j.state = 'running'
                             AND j.claimed_at < now() - make_interval(secs => $1)
                             AND j.attempts < $2))
                  ORDER BY t.last_served_seq ASC NULLS FIRST, j.created_at, j.job_id
                  FOR UPDATE OF j, t SKIP LOCKED
                  LIMIT 1"
            }
        };
        let row = sqlx::query(select)
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
                    attempts = attempts + 1, updated_at = now(),
                    progress_phase = NULL, progress_units_done = NULL,
                    progress_units_total = NULL, progress_advanced_at = NULL
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
        record_run(&mut tx, &job, node_id).await?;
        if policy == ClaimPolicy::TenantFair {
            // The turn advances with the claim, in its transaction.
            sqlx::query(
                "UPDATE index_build_tenant_turns
                    SET last_served_seq = nextval('index_build_tenant_turn_seq'),
                        last_served_at = now()
                  WHERE tenant_id = $1",
            )
            .bind(&job.tenant_id)
            .execute(&mut *tx)
            .await
            .map_err(storage_err)?;
        }
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

    /// Cancel a job that has not finished.
    ///
    /// Commits the `cancelled` state at once, which v1.4 readers see
    /// unchanged, and releases the open target so the same build can be
    /// enqueued again immediately. It is an UPDATE of the job row, so it
    /// takes the same row lock as every fenced write ([`lock_generation`]):
    /// a fenced write that commits first is kept, and one that comes later
    /// finds the job `cancelled` and writes nothing (ClearView R6). The
    /// running worker learns of it at its next safe boundary
    /// ([`BuildJobs::is_current`] or a refused fenced write) and stops; a
    /// worker that never comes back needs no acknowledgment. Returns false
    /// for a job that had already finished, or a repeated cancel.
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

    /// Whether a claim is still current, without taking the row lock: the
    /// revalidation a build makes at a safe boundary, where a cancel may have
    /// committed or a lease renewal failed with an error. A shared write
    /// still takes [`lock_generation`] in its own transaction; this only
    /// decides whether to keep working.
    pub async fn is_current(&self, fence: &JobFence) -> Result<bool> {
        let current: Option<bool> = sqlx::query_scalar(
            "SELECT state = 'running' AND attempts = $3 AND claimed_by IS NOT DISTINCT FROM $4
               FROM index_build_jobs
              WHERE tenant_id = $1 AND job_id = $2",
        )
        .bind(&fence.tenant_id)
        .bind(&fence.job_id)
        .bind(fence.generation)
        .bind(&fence.node_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(current == Some(true))
    }

    /// Record progress for the claim `fence` holds (ClearView R9).
    ///
    /// Fenced like completion: the row must be `running` with this node and
    /// generation, so a stale holder, even under the same node name, writes
    /// nothing and gets `false`. `units_done` never decreases within a claim
    /// (a smaller value keeps the larger one), `None` leaves a counter as it
    /// is, and an unknown total stays NULL. `last_advance_at` moves when the
    /// phase changes or the count grows. A phase change is recorded in the
    /// event ring by trigger; a count change is not.
    pub async fn record_progress(
        &self,
        fence: &JobFence,
        phase: ProgressPhase,
        units_done: Option<u64>,
        units_total: Option<u64>,
    ) -> Result<bool> {
        let done = units_done.map(|v| i64::try_from(v).unwrap_or(i64::MAX));
        let total = units_total.map(|v| i64::try_from(v).unwrap_or(i64::MAX));
        let updated = sqlx::query(
            "UPDATE index_build_jobs
                SET progress_advanced_at = CASE
                        WHEN progress_phase IS DISTINCT FROM $5
                          OR ($6::bigint IS NOT NULL
                              AND $6 > COALESCE(progress_units_done, -1))
                        THEN now() ELSE progress_advanced_at END,
                    progress_phase = $5,
                    progress_units_done = CASE
                        WHEN $6::bigint IS NULL THEN progress_units_done
                        ELSE GREATEST(COALESCE(progress_units_done, 0), $6) END,
                    progress_units_total = COALESCE($7::bigint, progress_units_total)
              WHERE tenant_id = $1 AND job_id = $2
                AND state = 'running' AND claimed_by = $3 AND attempts = $4",
        )
        .bind(&fence.tenant_id)
        .bind(&fence.job_id)
        .bind(&fence.node_id)
        .bind(fence.generation)
        .bind(phase.as_str())
        .bind(done)
        .bind(total)
        .execute(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(updated.rows_affected() == 1)
    }

    /// A job's recorded progress. Tenant-scoped; `None` for no such job.
    pub async fn progress(&self, tenant_id: &str, job_id: &str) -> Result<Option<JobProgress>> {
        let Some(job) = self.get(tenant_id, job_id).await? else {
            return Ok(None);
        };
        let row = sqlx::query(
            "SELECT progress_phase, progress_units_done, progress_units_total,
                    progress_advanced_at::text AS advanced
               FROM index_build_jobs WHERE tenant_id = $1 AND job_id = $2",
        )
        .bind(tenant_id)
        .bind(job_id)
        .fetch_one(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(Some(JobProgress {
            job,
            phase: row.get("progress_phase"),
            units_done: row.get("progress_units_done"),
            units_total: row.get("progress_units_total"),
            last_advance_at: row.get("advanced"),
        }))
    }

    /// One page of a job's events after sequence `after` (0 = from the
    /// start), oldest first (ClearView R9). Tenant-scoped; `None` for no such
    /// job. Read in one repeatable-read snapshot, so the job's event count and
    /// the surviving events agree even while retention prunes and other
    /// transitions commit; the next page continues from `through`.
    pub async fn events(
        &self,
        tenant_id: &str,
        job_id: &str,
        after: i64,
        limit: i64,
    ) -> Result<Option<JobEventPage>> {
        let after = after.max(0);
        let limit = limit.clamp(1, 200);
        let mut tx = self.pool.begin().await.map_err(storage_err)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(storage_err)?;
        let head: Option<Option<i64>> = sqlx::query_scalar(
            "SELECT event_seq FROM index_build_jobs WHERE tenant_id = $1 AND job_id = $2",
        )
        .bind(tenant_id)
        .bind(job_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage_err)?;
        let Some(head) = head else {
            return Ok(None);
        };
        let rows = sqlx::query(
            "SELECT job_event_seq, kind, from_state, to_state, job_generation, node_id,
                    progress_phase, recorded_at::text AS recorded
               FROM index_build_job_events
              WHERE tenant_id = $1 AND job_id = $2 AND job_event_seq > $3
              ORDER BY job_event_seq
              LIMIT $4",
        )
        .bind(tenant_id)
        .bind(job_id)
        .bind(after)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await
        .map_err(storage_err)?;
        tx.commit().await.map_err(storage_err)?;
        let events: Vec<JobEvent> = rows
            .into_iter()
            .map(|r| JobEvent {
                seq: r.get("job_event_seq"),
                kind: r.get("kind"),
                from_state: r.get("from_state"),
                to_state: r.get("to_state"),
                generation: r.get("job_generation"),
                node_id: r.get("node_id"),
                phase: r.get("progress_phase"),
                recorded_at: r.get("recorded"),
            })
            .collect();
        // A job with no recorded numbering predates the ring entirely.
        let head = head.unwrap_or(1);
        let gap = match events.first() {
            Some(first) => first.seq > after + 1,
            None => head > after,
        };
        let through = match events.last() {
            Some(last) => last.seq,
            // Nothing survives after the cursor: the reader has seen (or
            // lost) everything up to the head.
            None => head.max(after),
        };
        Ok(Some(JobEventPage {
            events,
            gap,
            through,
        }))
    }

    /// Why a claim is no longer current: `Cancelled` when the job is
    /// `cancelled`, otherwise `LeaseLost`. `None` while it is current.
    pub async fn stop_reason(&self, fence: &JobFence) -> Result<Option<StopReason>> {
        if self.is_current(fence).await? {
            return Ok(None);
        }
        let state: Option<String> = sqlx::query_scalar(
            "SELECT state FROM index_build_jobs WHERE tenant_id = $1 AND job_id = $2",
        )
        .bind(&fence.tenant_id)
        .bind(&fence.job_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(Some(if state.as_deref() == Some("cancelled") {
            StopReason::Cancelled
        } else {
            StopReason::LeaseLost
        }))
    }

    /// A job's per-version outcomes, one page, ordered by version id
    /// (ClearView R6). Tenant-scoped; `None` when the tenant has no such job.
    ///
    /// Assembled from the durable records a fenced build writes: the selected
    /// set and dispositions, logical-version commits, artifact-attempt links
    /// and publication intents, with the catalog row and `staged` binding they
    /// name. A pending intent is reported as pending: its manifest outcome is
    /// unknown until the worker or the reconciler resolves it. Read-only; it
    /// never starts recovery.
    pub async fn version_history(
        &self,
        tenant_id: &str,
        job_id: &str,
        after: Option<&str>,
        limit: i64,
    ) -> Result<Option<JobVersionHistory>> {
        let Some(job) = self.get(tenant_id, job_id).await? else {
            return Ok(None);
        };
        let recorded: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM index_build_job_runs
              WHERE tenant_id = $1 AND job_id = $2
                AND job_generation BETWEEN 1 AND $3",
        )
        .bind(tenant_id)
        .bind(job_id)
        .bind(job.attempts)
        .fetch_one(&self.pool)
        .await
        .map_err(storage_err)?;
        let history = if job.attempts <= 0 || recorded >= i64::from(job.attempts) {
            HistoryStatus::Recorded
        } else if recorded == 0 {
            HistoryStatus::Unavailable
        } else {
            HistoryStatus::Partial
        };
        let limit = limit.clamp(1, 200);
        let rows = sqlx::query(
            "WITH v AS (
                 SELECT index_version_id FROM index_build_job_versions
                  WHERE tenant_id = $1 AND job_id = $2
                 UNION
                 SELECT index_version_id FROM index_version_job_commits
                  WHERE tenant_id = $1 AND job_id = $2
                 UNION
                 SELECT index_version_id FROM index_build_attempt_jobs
                  WHERE tenant_id = $1 AND job_id = $2)
             SELECT v.index_version_id,
                    sel.ordinal, sel.disposition,
                    c.job_generation AS committed_generation,
                    (SELECT count(*) FROM index_build_attempt_jobs n
                      WHERE n.tenant_id = $1 AND n.job_id = $2
                        AND n.index_version_id = v.index_version_id) AS attempt_count,
                    a.attempt_id, a.job_generation AS attempt_generation,
                    att.state AS attempt_state,
                    i.state AS intent_state, i.artifact_id,
                    art.state AS artifact_state,
                    COALESCE(b.artifact_id = i.artifact_id, false) AS staged_bound
               FROM v
               LEFT JOIN index_build_job_versions sel
                 ON sel.tenant_id = $1 AND sel.job_id = $2
                AND sel.index_version_id = v.index_version_id
               LEFT JOIN index_version_job_commits c
                 ON c.tenant_id = $1 AND c.index_version_id = v.index_version_id
                AND c.job_id = $2
               LEFT JOIN LATERAL (
                     SELECT l.attempt_id, l.job_generation FROM index_build_attempt_jobs l
                      WHERE l.tenant_id = $1 AND l.job_id = $2
                        AND l.index_version_id = v.index_version_id
                      ORDER BY l.job_generation DESC, l.linked_at DESC
                      LIMIT 1) a ON true
               LEFT JOIN index_build_attempts att
                 ON att.tenant_id = $1 AND att.attempt_id = a.attempt_id
               LEFT JOIN index_build_publication_intents i
                 ON i.tenant_id = $1 AND i.attempt_id = a.attempt_id
               LEFT JOIN index_artifacts art
                 ON art.tenant_id = $1 AND art.index_version_id = v.index_version_id
                AND art.artifact_id = i.artifact_id
               LEFT JOIN index_artifact_bindings b
                 ON b.tenant_id = $1 AND b.index_version_id = v.index_version_id
                AND b.slot = 'staged'
              WHERE $3::text IS NULL OR v.index_version_id > $3
              ORDER BY v.index_version_id
              LIMIT $4",
        )
        .bind(tenant_id)
        .bind(job_id)
        .bind(after)
        .bind(limit + 1)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;
        let mut versions: Vec<JobVersionOutcome> = rows
            .into_iter()
            .map(|r| JobVersionOutcome {
                index_version_id: r.get("index_version_id"),
                selected_ordinal: r.get("ordinal"),
                committed_generation: r.get("committed_generation"),
                disposition: r.get("disposition"),
                attempt_count: r.get("attempt_count"),
                attempt_id: r.get("attempt_id"),
                attempt_generation: r.get("attempt_generation"),
                attempt_state: r.get("attempt_state"),
                intent_state: r.get("intent_state"),
                artifact_id: r.get("artifact_id"),
                artifact_state: r.get("artifact_state"),
                staged_bound: r.get("staged_bound"),
            })
            .collect();
        let next_after = if versions.len() as i64 > limit {
            versions.truncate(limit as usize);
            versions.last().map(|v| v.index_version_id.clone())
        } else {
            None
        };
        Ok(Some(JobVersionHistory {
            job,
            history,
            versions,
            next_after,
        }))
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
