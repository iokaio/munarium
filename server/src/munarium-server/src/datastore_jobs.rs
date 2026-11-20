// SPDX-License-Identifier: Apache-2.0
//! Durable build jobs, server side: enqueue over
//! REST, execute in the builder loop, poll the result.
//!
//! The request path never builds. `POST /v1/index-build-jobs` writes a row
//! and answers with an id; whichever process runs the builder loop claims and
//! executes it through the SAME ops the synchronous endpoints call — one
//! implementation, two invocation styles, which is what "changed from
//! synchronous work to job orchestration without losing audit/result
//! semantics" means in practice. The synchronous endpoints stay: a small
//! corpus built inline is a fine developer experience, and deleting them
//! would trade it for ceremony.
//!
//! The builder loop runs where `MUNARIUM_DATASTORE_BUILDER=enabled` — any
//! process with PostgreSQL and the datastore staging configuration can be a
//! builder, and a dedicated builder deployment is that flag on a process
//! whose plane is `builder`. Builds and queries then scale independently,
//! which is the phase's exit.

use std::sync::Arc;

use munarium_core::{KernelError, Result};
use munarium_store_pg::job_failure::{classify, client_failure, JobFailure};
use munarium_store_pg::jobs::{
    BuildJob, BuildJobs, ClaimPolicy, EnqueueOutcome, JobFence, JobId, JobKind, JobSignal,
    JobTarget, JobVersionOutcome, VersionCursor,
};

use crate::error::ApiError;
use crate::state::AppState;
use munarium_api_types as dto;

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

/// Execute one claimed job through the ordinary operations, returning the
/// bounded result the job row stores.
///
/// Public and free of loop machinery so a test can run a claimed job
/// deterministically — the loop adds only scheduling.
///
/// `fence` is the claim this execution runs under (ClearView R4). Every
/// effect outside the job row — a direct build's logical-version commit, each
/// artifact attempt, publication intent and `staged` binding — is written
/// under the job row's lock only while that claim is current. A superseded
/// execution commits and publishes nothing new; it may only finish an intent
/// its generation already committed. `None` runs the job with the synchronous
/// operations' authority.
///
/// `signal` is the claim's worker-local stop signal (ClearView R6). The build
/// checks it, and revalidates the claim, at each safe boundary and stops
/// there once the job is cancelled or the claim retired.
pub async fn run_one_job(
    state: &AppState,
    job: &BuildJob,
    fence: Option<&JobFence>,
    signal: Option<&JobSignal>,
) -> Result<serde_json::Value> {
    let tenant = job.tenant_id.as_str();
    match JobKind::parse(&job.kind)? {
        JobKind::Backfill => {
            let scope_id = job
                .scope_id
                .as_deref()
                .ok_or_else(|| KernelError::InvalidInput("a backfill job names a scope".into()))?;
            let report = crate::datastore_builds::op_backfill_collection_fenced(
                state, tenant, scope_id, fence, signal,
            )
            .await?;
            Ok(serde_json::json!({
                "collection_id": report.collection_id,
                "complete": report.complete,
                "versions": report.versions.len(),
            }))
        }
        JobKind::Rebuild => {
            let version = job.index_version_id.as_deref().ok_or_else(|| {
                KernelError::InvalidInput("a rebuild job names an index version".into())
            })?;
            let built = crate::datastore_builds::op_rebuild_artifact_fenced(
                state, tenant, version, fence, signal,
            )
            .await?;
            Ok(serde_json::to_value(&built).map_err(|e| KernelError::Storage(e.to_string()))?)
        }
        JobKind::Direct => {
            let collection_id = job.scope_id.as_deref().ok_or_else(|| {
                KernelError::InvalidInput("a direct job names a collection".into())
            })?;
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
            let mut ctx = state.mirror_context(tenant)?;
            ctx.job_fence = fence.cloned();
            ctx.job_signal = signal.cloned();
            let retrieval = state.retrieval_for(tenant)?;
            let outcome = retrieval
                .build_collection_direct(&ctx, collection_id, max_chars, watermark_seq)
                .await?;
            Ok(serde_json::json!({
                "index_version_id": outcome.index_version_id,
                "committed": outcome.committed,
                "expected_active": outcome.expected_active,
                "artifact": format!("{:?}", outcome.artifact),
            }))
        }
    }
}

/// The builder loop: claim, execute, complete, repeat. Spawned only where the
/// operator enabled it.
pub fn spawn_builder(state: &Arc<AppState>) {
    let enabled = std::env::var("MUNARIUM_DATASTORE_BUILDER")
        .map(|v| v.eq_ignore_ascii_case("enabled"))
        .unwrap_or(false);
    if !enabled {
        return;
    }
    let Some(pool) = state.pg_pool().cloned() else {
        tracing::error!("MUNARIUM_DATASTORE_BUILDER=enabled requires the postgres store");
        return;
    };
    let poll_ms = std::env::var("MUNARIUM_DATASTORE_BUILDER_POLL_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5_000u64)
        .max(250);
    // ClearView R8: opt-in tenant-fair claiming. Unset keeps the v1.4 FIFO
    // queue; an unknown value is refused rather than silently ignored.
    let policy = match std::env::var("MUNARIUM_DATASTORE_JOB_SCHEDULING") {
        Err(_) => ClaimPolicy::Fifo,
        Ok(raw) => match ClaimPolicy::parse(&raw) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(error = %e, "the builder was not started");
                return;
            }
        },
    };
    tracing::info!(scheduling = policy.as_str(), "build-job builder starting");
    // ClearView B2: the per-tenant event retention count. Unset keeps the
    // value in the database (the provisional default from migration 0046).
    let events_per_tenant = match std::env::var("MUNARIUM_DATASTORE_JOB_EVENTS_PER_TENANT") {
        Err(_) => None,
        Ok(raw) => match raw.trim().parse::<i32>() {
            Ok(n) if n >= 1 => Some(n),
            _ => {
                tracing::error!(
                    "MUNARIUM_DATASTORE_JOB_EVENTS_PER_TENANT must be a positive integer; the builder was not started"
                );
                return;
            }
        },
    };
    let lease_secs = std::env::var("MUNARIUM_DATASTORE_JOB_LEASE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(600i64)
        .max(30);

    let weak = Arc::downgrade(state);
    tokio::spawn(async move {
        if let Some(n) = events_per_tenant {
            match munarium_store_pg::jobs::set_event_retention(&pool, n).await {
                Ok(()) => tracing::info!(events_per_tenant = n, "build-job event retention set"),
                Err(e) => tracing::warn!(error = %e, "could not set build-job event retention"),
            }
        }
        let jobs = BuildJobs::new(pool);
        loop {
            let Some(state) = weak.upgrade() else { return };
            let node = state.config.instance_id.clone();
            // Fail expired final claims before claiming (ClearView R4). This
            // runs on every poll, including an empty queue, and consumes no
            // scheduling turn: a bounded batch, then the normal claim.
            match jobs
                .fail_expired_final_claims(lease_secs, MAX_JOB_ATTEMPTS, EXPIRED_CLAIM_BATCH)
                .await
            {
                Ok(failed) => {
                    for (tenant, job_id) in failed {
                        tracing::warn!(
                            tenant_id = %tenant,
                            job_id = %job_id,
                            "build job failed: final attempt's lease expired; target released"
                        );
                    }
                }
                Err(e) => tracing::warn!(error = %e, "expired-claim cleanup failed"),
            }
            match jobs
                .claim_next(&node, lease_secs, MAX_JOB_ATTEMPTS, policy)
                .await
            {
                Ok(Some(job)) => {
                    // The claim's fencing generation, captured in the claiming
                    // transaction. Every later write for this job carries it.
                    let generation = job.attempts;
                    tracing::info!(job_id = %job.job_id, kind = %job.kind, generation, "build job claimed");
                    // Keep the JOB lease alive for as long as the build runs.
                    // The attempt lease underneath is heartbeated by the
                    // mirror; the job lease was not, so any build longer than
                    // `lease_secs` was re-offered to another replica while
                    // this one was still working on it. Stopped by the guard
                    // whichever way the build ends. The renewal also drives
                    // the worker-local stop signal (ClearView R6).
                    let fence = JobFence::of_claim(&job, &node);
                    let signal = JobSignal::new();
                    let beat = spawn_lease_watch(
                        jobs.clone(),
                        fence.clone(),
                        signal.clone(),
                        std::time::Duration::from_secs((lease_secs as u64 / 3).max(5)),
                    );
                    let outcome = run_one_job(&state, &job, Some(&fence), Some(&signal))
                        .await
                        .map_err(|e| job_failure(&job, generation, &e));
                    drop(beat);
                    if let Some(reason) = signal.stopped() {
                        tracing::info!(
                            job_id = %job.job_id,
                            generation,
                            cancelled = reason == munarium_store_pg::jobs::StopReason::Cancelled,
                            "build job stopped at a safe boundary; builder slot released"
                        );
                    }
                    match jobs
                        .complete_fenced_classified(
                            &job.tenant_id,
                            &job.job_id,
                            &node,
                            generation,
                            outcome,
                            None,
                        )
                        .await
                    {
                        Ok(true) => {}
                        Ok(false) => tracing::warn!(
                            job_id = %job.job_id,
                            "job completion was a no-op; the lease lapsed and another builder owns it"
                        ),
                        Err(e) => tracing::warn!(error = %e, "job completion failed"),
                    }
                    // A non-empty queue drains without sleeping.
                    drop(state);
                    continue;
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(error = %e, "job claim failed"),
            }
            drop(state);
            tokio::time::sleep(std::time::Duration::from_millis(poll_ms)).await;
        }
    });
}

/// The failure a builder records for a job (ClearView C2): classified from
/// the typed cause, never from its text. Only the safe message and code are
/// stored; the cause itself goes to the server log.
fn job_failure(job: &BuildJob, generation: i32, e: &KernelError) -> JobFailure {
    let failure = classify(e);
    tracing::warn!(
        job_id = %job.job_id,
        generation,
        error = %e,
        error_code = failure.code.map(|c| c.as_str()).unwrap_or("unclassified"),
        "build job failed"
    );
    failure
}

/// Renew a claim's lease every `every` and drive its stop signal
/// (ClearView R6).
///
/// A refused renewal is CONFIRMED loss: the job was cancelled, or the
/// generation was retired by a reclaim, a completion or expired-claim
/// cleanup. The signal stops with the reason and the watch ends. A renewal
/// that fails with an error is not evidence either way: the lease is marked
/// uncertain and the build revalidates the claim at its next safe boundary,
/// before any further shared write. A later successful renewal clears it.
fn spawn_lease_watch(
    jobs: BuildJobs,
    fence: JobFence,
    signal: JobSignal,
    every: std::time::Duration,
) -> AbortOnDrop {
    AbortOnDrop(tokio::spawn(async move {
        loop {
            tokio::time::sleep(every).await;
            if lease_watch_once(&jobs, &fence, &signal).await {
                break;
            }
        }
    }))
}

/// One renewal of [`spawn_lease_watch`]. Returns true when the watch should
/// end because the claim is confirmed lost.
async fn lease_watch_once(jobs: &BuildJobs, fence: &JobFence, signal: &JobSignal) -> bool {
    match jobs
        .heartbeat_fenced(
            &fence.tenant_id,
            &fence.job_id,
            &fence.node_id,
            fence.generation,
        )
        .await
    {
        Ok(true) => {
            signal.set_uncertain(false);
            false
        }
        Ok(false) => {
            let reason = jobs
                .stop_reason(fence)
                .await
                .ok()
                .flatten()
                .unwrap_or(munarium_store_pg::jobs::StopReason::LeaseLost);
            tracing::warn!(
                job_id = %fence.job_id,
                cancelled = reason == munarium_store_pg::jobs::StopReason::Cancelled,
                "build job lease is no longer ours; stopping at the next safe boundary"
            );
            signal.stop(reason);
            true
        }
        Err(e) => {
            signal.set_uncertain(true);
            tracing::warn!(error = %e, job_id = %fence.job_id, "build job heartbeat failed; lease uncertain");
            false
        }
    }
}

/// A job that has been claimed this many times has failed enough. Its last
/// error stands; an operator re-enqueues deliberately.
pub const MAX_JOB_ATTEMPTS: i32 = 3;

/// How many expired final claims one builder poll fails. Bounded so cleanup
/// after a long outage cannot turn one poll into an unbounded transaction.
const EXPIRED_CLAIM_BATCH: i64 = 32;

/// A spawned task stopped when its guard goes out of scope — the job-lease
/// heartbeat, which must end on every path out of a build (success, failure,
/// panic) or it would keep extending the lease of a job nobody is working on.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

// ---------------------------------------------------------------------------
// REST
// ---------------------------------------------------------------------------

/// The REST view of a job row. Failure members follow the C2 read rule
/// ([`client_failure`]): a stored code returns its message and code; stored
/// text without a code (historical or a v1.4 builder's) returns the generic
/// message and no code. The text is never parsed.
fn job_dto(j: BuildJob) -> dto::IndexBuildJobDto {
    let (error, error_code) = client_failure(j.error.as_deref(), j.error_code.as_deref());
    dto::IndexBuildJobDto {
        job_id: j.job_id,
        kind: j.kind,
        scope_kind: j.scope_kind,
        scope_id: j.scope_id,
        index_version_id: j.index_version_id,
        state: j.state,
        attempts: j.attempts,
        claimed_by: j.claimed_by,
        result: j.result,
        error,
        error_code,
        created_at: j.created_at,
    }
}

type ApiResult<T> = std::result::Result<T, ApiError>;

/// POST /v1/index-build-jobs
#[utoipa::path(
    post,
    path = "/v1/index-build-jobs",
    tag = "index-artifacts",
    request_body = dto::IndexBuildJobRequest,
    responses(
        (status = 200, description = "the job, newly enqueued or already open for this target", body = dto::IndexBuildJobDto),
        (status = 400, description = "unknown kind, or a target that does not fit it")
    )
)]
pub async fn enqueue_job(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    crate::rest::ProblemJson(req): crate::rest::ProblemJson<dto::IndexBuildJobRequest>,
) -> ApiResult<axum::Json<dto::IndexBuildJobDto>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_rw()?;
    let pool = state.pg_pool().ok_or_else(|| {
        ApiError::from(KernelError::InvalidInput(
            "build jobs require the postgres store".into(),
        ))
    })?;
    let kind = JobKind::parse(&req.kind)?;
    let target = match (
        kind,
        req.index_version_id.as_deref(),
        req.collection_id.as_deref(),
    ) {
        (JobKind::Rebuild, Some(v), _) => JobTarget::Version(v),
        (JobKind::Rebuild, None, _) => {
            return Err(ApiError::from(KernelError::InvalidInput(
                "a rebuild job names index_version_id".into(),
            )))
        }
        (_, _, Some(c)) => JobTarget::Scope {
            scope_kind: "collection",
            scope_id: c,
        },
        (_, _, None) => {
            return Err(ApiError::from(KernelError::InvalidInput(
                "backfill and direct jobs name collection_id".into(),
            )))
        }
    };
    let mut params = serde_json::Map::new();
    if let Some(m) = req.max_chars {
        params.insert("max_chars".into(), m.into());
    }
    if let Some(w) = req.watermark_seq {
        params.insert("watermark_seq".into(), w.into());
    }

    let jobs = BuildJobs::new(pool.clone());
    let outcome = jobs
        .enqueue(
            &ctx.tenant_id,
            kind,
            target,
            serde_json::Value::Object(params),
            &format!("api:{}", ctx.role),
            req.correlation_id.as_deref(),
        )
        .await?;
    let job_id = match outcome {
        EnqueueOutcome::Enqueued(id) | EnqueueOutcome::AlreadyOpen(id) => id,
    };
    let job = jobs.get(&ctx.tenant_id, &job_id).await?.ok_or_else(|| {
        ApiError::from(KernelError::Storage("job enqueued but unreadable".into()))
    })?;
    Ok(axum::Json(job_dto(job)))
}

/// GET /v1/index-build-jobs/{job_id}
#[utoipa::path(
    get,
    path = "/v1/index-build-jobs/{job_id}",
    tag = "index-artifacts",
    params(("job_id" = String, Path, description = "build job id")),
    responses(
        (status = 200, description = "the job's state and bounded result", body = dto::IndexBuildJobDto),
        (status = 404, description = "no such job")
    )
)]
pub async fn get_job(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path(job_id): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
) -> ApiResult<axum::Json<dto::IndexBuildJobDto>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    let pool = state.pg_pool().ok_or_else(|| {
        ApiError::from(KernelError::InvalidInput(
            "build jobs require the postgres store".into(),
        ))
    })?;
    let job = BuildJobs::new(pool.clone())
        .get(&ctx.tenant_id, &job_id)
        .await?
        .ok_or(KernelError::NotFound {
            kind: "build job",
            id: job_id,
        })?;
    Ok(axum::Json(job_dto(job)))
}

/// GET /v1/index-build-jobs
#[utoipa::path(
    get,
    path = "/v1/index-build-jobs",
    tag = "index-artifacts",
    responses((status = 200, description = "this tenant's recent jobs, newest first", body = dto::IndexBuildJobListResponse))
)]
pub async fn list_jobs(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> ApiResult<axum::Json<dto::IndexBuildJobListResponse>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    let pool = state.pg_pool().ok_or_else(|| {
        ApiError::from(KernelError::InvalidInput(
            "build jobs require the postgres store".into(),
        ))
    })?;
    let jobs = BuildJobs::new(pool.clone())
        .list(&ctx.tenant_id, 50)
        .await?;
    Ok(axum::Json(dto::IndexBuildJobListResponse {
        jobs: jobs.into_iter().map(job_dto).collect(),
    }))
}

/// POST /v1/index-build-jobs/{job_id}/cancel
#[utoipa::path(
    post,
    path = "/v1/index-build-jobs/{job_id}/cancel",
    tag = "index-artifacts",
    params(("job_id" = String, Path, description = "build job id")),
    responses(
        (status = 200, description = "whether the job was open and is now cancelled", body = dto::OkResponse)
    )
)]
pub async fn cancel_job(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path(job_id): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
) -> ApiResult<axum::Json<dto::OkResponse>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_rw()?;
    let pool = state.pg_pool().ok_or_else(|| {
        ApiError::from(KernelError::InvalidInput(
            "build jobs require the postgres store".into(),
        ))
    })?;
    let cancelled = BuildJobs::new(pool.clone())
        .cancel(&ctx.tenant_id, &job_id)
        .await?;
    Ok(axum::Json(dto::OkResponse { ok: cancelled }))
}

/// The `manifest` member of a version outcome, from its intent state.
fn manifest_label(intent_state: Option<&str>) -> &'static str {
    match intent_state {
        None => "none",
        Some("pending") => "unknown",
        Some("published") => "published",
        Some("converged") => "converged",
        Some("failed") => "failed",
        // A state this binary does not know is not evidence of anything.
        Some(_) => "unknown",
    }
}

fn version_dto(v: JobVersionOutcome) -> dto::IndexBuildJobVersionDto {
    dto::IndexBuildJobVersionDto {
        logical_version: if v.committed_generation.is_some() {
            "committed".into()
        } else {
            "not_committed".into()
        },
        manifest: manifest_label(v.intent_state.as_deref()).into(),
        index_version_id: v.index_version_id,
        selected_ordinal: v.selected_ordinal,
        committed_generation: v.committed_generation,
        disposition: v.disposition,
        attempts: v.attempt_count,
        attempt_id: v.attempt_id,
        attempt_generation: v.attempt_generation,
        attempt_state: v.attempt_state,
        artifact_id: v.artifact_id,
        catalog: v.artifact_state,
        staged_bound: v.staged_bound,
    }
}

/// The `job_id` path segment of the ClearView job reads (R12). Validated
/// before any work; the schema declares the same syntax as
/// [`munarium_store_pg::jobs::OPAQUE_ID_PATTERN`], which a test checks.
#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Path)]
pub struct JobPath {
    /// Build job id.
    #[param(
        pattern = "^[A-Za-z0-9][A-Za-z0-9._:-]{0,199}$",
        min_length = 1,
        max_length = 200
    )]
    pub job_id: String,
}

impl JobPath {
    fn parse(&self) -> ApiResult<JobId> {
        Ok(JobId::parse(&self.job_id)?)
    }
}

/// Paging for the job history reads.
#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct JobPage {
    /// The `next_cursor` of the previous page.
    pub cursor: Option<String>,
    /// Page size, 1 to 200 (default 50).
    #[param(value_type = Option<u32>, minimum = 1, maximum = 200)]
    pub limit: Option<String>,
}

impl JobPage {
    fn limit(&self) -> ApiResult<i64> {
        match self.limit.as_deref() {
            None => Ok(50),
            Some(raw) => match raw.parse::<i64>() {
                Ok(n) if (1..=200).contains(&n) => Ok(n),
                _ => Err(ApiError::from(KernelError::InvalidInput(
                    "limit must be an integer from 1 to 200".into(),
                ))),
            },
        }
    }
}

/// GET /v1/index-build-jobs/{job_id}/progress
#[utoipa::path(
    get,
    path = "/v1/index-build-jobs/{job_id}/progress",
    tag = "index-artifacts",
    params(JobPath),
    responses(
        (status = 200, description = "the job's fenced progress for its current claim", body = dto::IndexBuildJobProgressDto),
        (status = 400, description = "a malformed job id, or the memory store"),
        (status = 404, description = "no such job")
    )
)]
pub async fn get_job_progress(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path(path): axum::extract::Path<JobPath>,
    headers: axum::http::HeaderMap,
) -> ApiResult<axum::Json<dto::IndexBuildJobProgressDto>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    let job_id = path.parse()?;
    let pool = state.pg_pool().ok_or_else(|| {
        ApiError::from(KernelError::InvalidInput(
            "build jobs require the postgres store".into(),
        ))
    })?;
    let p = BuildJobs::new(pool.clone())
        .progress(&ctx.tenant_id, &job_id)
        .await?
        .ok_or(KernelError::NotFound {
            kind: "build job",
            id: job_id.to_string(),
        })?;
    Ok(axum::Json(dto::IndexBuildJobProgressDto {
        job_id: p.job.job_id,
        state: p.job.state,
        generation: p.job.attempts,
        phase: p.phase,
        units_done: p.units_done,
        units_total: p.units_total,
        last_advance_at: p.last_advance_at,
    }))
}

/// The opaque event cursor: the last sequence read, bound to the tenant and
/// job it was issued for (ClearView B2-T2). Not a secret; the binding stops
/// a cursor from another tenant or job from being accepted silently.
fn event_cursor_tag(tenant_id: &str, job_id: &str) -> String {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(format!("{tenant_id}\0{job_id}").as_bytes());
    hex::encode(&digest[..8])
}

fn encode_event_cursor(tenant_id: &str, job_id: &str, through: i64) -> String {
    format!("je1.{through}.{}", event_cursor_tag(tenant_id, job_id))
}

fn decode_event_cursor(tenant_id: &str, job_id: &str, cursor: &str) -> ApiResult<i64> {
    let bad = || {
        ApiError::from(KernelError::InvalidInput(
            "cursor is not an event cursor issued for this job".into(),
        ))
    };
    let mut parts = cursor.split('.');
    let (Some("je1"), Some(seq), Some(tag), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(bad());
    };
    let seq: i64 = seq.parse().map_err(|_| bad())?;
    if seq < 0 || tag != event_cursor_tag(tenant_id, job_id) {
        return Err(bad());
    }
    Ok(seq)
}

/// GET /v1/index-build-jobs/{job_id}/events
#[utoipa::path(
    get,
    path = "/v1/index-build-jobs/{job_id}/events",
    tag = "index-artifacts",
    params(JobPath, JobPage),
    responses(
        (status = 200, description = "one page of the job's recorded transitions, oldest first; `gap` marks pruned history", body = dto::IndexBuildJobEventsResponse),
        (status = 400, description = "a malformed job id, cursor or limit, a cursor issued for another tenant or job, or the memory store"),
        (status = 404, description = "no such job")
    )
)]
pub async fn list_job_events(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path(path): axum::extract::Path<JobPath>,
    axum::extract::Query(page): axum::extract::Query<JobPage>,
    headers: axum::http::HeaderMap,
) -> ApiResult<axum::Json<dto::IndexBuildJobEventsResponse>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    let job = path.parse()?;
    let pool = state.pg_pool().ok_or_else(|| {
        ApiError::from(KernelError::InvalidInput(
            "build jobs require the postgres store".into(),
        ))
    })?;
    let limit = page.limit()?;
    let job_id = job.to_string();
    let after = match page.cursor.as_deref() {
        None => 0,
        Some(c) => decode_event_cursor(&ctx.tenant_id, &job_id, c)?,
    };
    let events = BuildJobs::new(pool.clone())
        .events(&ctx.tenant_id, &job, after, limit)
        .await?
        .ok_or(KernelError::NotFound {
            kind: "build job",
            id: job_id.clone(),
        })?;
    Ok(axum::Json(dto::IndexBuildJobEventsResponse {
        next_cursor: encode_event_cursor(&ctx.tenant_id, &job_id, events.through),
        job_id,
        gap: events.gap,
        events: events
            .events
            .into_iter()
            .map(|e| dto::IndexBuildJobEventDto {
                seq: e.seq,
                kind: e.kind,
                from_state: e.from_state,
                to_state: e.to_state,
                generation: e.generation,
                claimed_by: e.node_id,
                phase: e.phase,
                recorded_at: e.recorded_at,
            })
            .collect(),
    }))
}

/// GET /v1/index-build-jobs/{job_id}/versions
#[utoipa::path(
    get,
    path = "/v1/index-build-jobs/{job_id}/versions",
    tag = "index-artifacts",
    params(JobPath, JobPage),
    responses(
        (status = 200, description = "one page of the job's per-version outcomes; `history` says whether an empty page means nothing or unknown", body = dto::IndexBuildJobVersionsResponse),
        (status = 400, description = "a malformed job id, cursor or limit, or the memory store"),
        (status = 404, description = "no such job")
    )
)]
pub async fn list_job_versions(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path(path): axum::extract::Path<JobPath>,
    axum::extract::Query(page): axum::extract::Query<JobPage>,
    headers: axum::http::HeaderMap,
) -> ApiResult<axum::Json<dto::IndexBuildJobVersionsResponse>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    let job_id = path.parse()?;
    let cursor = page
        .cursor
        .as_deref()
        .map(VersionCursor::parse)
        .transpose()?;
    let pool = state.pg_pool().ok_or_else(|| {
        ApiError::from(KernelError::InvalidInput(
            "build jobs require the postgres store".into(),
        ))
    })?;
    let limit = page.limit()?;
    let history = BuildJobs::new(pool.clone())
        .version_history(&ctx.tenant_id, &job_id, cursor.as_ref(), limit)
        .await?
        .ok_or(KernelError::NotFound {
            kind: "build job",
            id: job_id.to_string(),
        })?;
    Ok(axum::Json(dto::IndexBuildJobVersionsResponse {
        job_id: history.job.job_id,
        state: history.job.state,
        history: history.history.as_str().into(),
        versions: history.versions.into_iter().map(version_dto).collect(),
        next_cursor: history.next_after,
    }))
}

#[cfg(test)]
#[path = "datastore_jobs_c2_tests.rs"]
mod c2_tests;

#[cfg(test)]
mod tests {
    //! v1.4 job DTO baseline for ClearView Phase 1 (R4, R5/C2). The fixture
    //! records each stored row and the exact JSON v1.4.0 returned for it.
    //! C2 changes the failure rows' expectations deliberately: those rows are
    //! marked `generic` and carry `expected_v15_dto` beside the v1.4 one. Every
    //! row marked `unchanged` must keep its v1.4 bytes.
    use super::*;

    const FIXTURE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/clearview-phase1/v14-build-jobs.json"
    ));

    fn opt(v: &serde_json::Value, k: &str) -> Option<String> {
        v[k].as_str().map(str::to_owned)
    }

    fn job_from_row(row: &serde_json::Value) -> BuildJob {
        BuildJob {
            tenant_id: "tenant-fixture".into(),
            job_id: opt(row, "job_id").unwrap(),
            kind: opt(row, "kind").unwrap(),
            scope_kind: opt(row, "scope_kind"),
            scope_id: opt(row, "scope_id"),
            index_version_id: opt(row, "index_version_id"),
            params: row["params"].clone(),
            state: opt(row, "state").unwrap(),
            attempt_ids: row["attempt_ids"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a.as_str().unwrap().to_owned())
                .collect(),
            correlation_id: opt(row, "correlation_id"),
            claimed_by: opt(row, "claimed_by"),
            attempts: row["attempts"].as_i64().unwrap() as i32,
            result: (!row["result"].is_null()).then(|| row["result"].clone()),
            error: opt(row, "error"),
            // Every fixture row predates migration 0047.
            error_code: None,
            requested_by: opt(row, "requested_by").unwrap(),
            created_at: opt(row, "created_at").unwrap(),
        }
    }

    /// The DTO a fixture row must serialize to now: its v1.4 bytes unless
    /// C2 changed it (`generic`), and then only the `error` text may differ.
    fn expected_now(row: &serde_json::Value) -> &serde_json::Value {
        match row["v15"].as_str().unwrap() {
            "unchanged" | "unchanged until cleanup" => &row["expected_v14_dto"],
            "generic" => {
                let (v14, v15) = (
                    row["expected_v14_dto"].as_object().unwrap(),
                    row["expected_v15_dto"].as_object().unwrap(),
                );
                assert_eq!(
                    v14.keys().collect::<Vec<_>>(),
                    v15.keys().collect::<Vec<_>>(),
                    "case {}: C2 adds or removes no member here",
                    row["case"]
                );
                for (k, v) in v14 {
                    if k != "error" {
                        assert_eq!(&v15[k], v, "case {}: only error changes", row["case"]);
                    }
                }
                assert_eq!(
                    v15["error"].as_str(),
                    Some(munarium_store_pg::job_failure::GENERIC_FAILURE_MESSAGE)
                );
                &row["expected_v15_dto"]
            }
            other => panic!("case {}: unknown v15 marker {other}", row["case"]),
        }
    }

    #[test]
    fn v14_job_fixture_serializes_to_the_recorded_dtos() {
        let fixture: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let rows = fixture["rows"].as_array().unwrap();
        assert!(rows.len() >= 7, "fixture lost rows");
        let generic = rows.iter().filter(|r| r["v15"] == "generic").count();
        assert_eq!(generic, 2, "C2 changes exactly the two stored failures");
        for row in rows {
            let got = serde_json::to_value(job_dto(job_from_row(row))).unwrap();
            assert_eq!(
                &got,
                expected_now(row),
                "case {} does not serialize as recorded",
                row["case"]
            );
            // C2: no stored row without a code gains one.
            assert!(got.get("error_code").is_none(), "case {}", row["case"]);
        }
        // Success keeps every failure member omitted (C2-T1's existing rule).
        let ok = rows.iter().find(|r| r["case"] == "succeeded").unwrap();
        let ok = serde_json::to_value(job_dto(job_from_row(ok))).unwrap();
        assert!(ok.get("error").is_none() && ok.get("error_code").is_none());
    }

    /// ClearView R6: the lease watch stops the worker-local signal only on
    /// CONFIRMED loss, with the reason, and marks an erroring renewal as
    /// uncertain rather than lost. Tenant-scoped rows, never claimable.
    #[tokio::test]
    async fn the_lease_watch_signals_confirmed_loss_and_marks_errors_uncertain() {
        let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
            eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
            return;
        };
        let tenant = format!("tenant-r6-watch-{}", uuid::Uuid::new_v4().simple());
        let store = munarium_store_pg::PgStore::connect(&url, &tenant)
            .await
            .unwrap();
        let pool = store.pool().clone();
        let jobs = BuildJobs::new(pool.clone());
        let seed = |job_id: &'static str| {
            let pool = pool.clone();
            let tenant = tenant.clone();
            async move {
                sqlx::query(
                    "INSERT INTO index_build_jobs
                         (tenant_id, job_id, kind, index_version_id, state, claimed_by,
                          claimed_at, attempts, requested_by)
                     VALUES ($1,$2,'rebuild','iv-watch','running','builder-w','infinity',1,'t')",
                )
                .bind(&tenant)
                .bind(job_id)
                .execute(&pool)
                .await
                .unwrap();
                JobFence {
                    tenant_id: tenant,
                    job_id: job_id.into(),
                    node_id: "builder-w".into(),
                    generation: 1,
                }
            }
        };

        // Current: renewal succeeds, nothing stops.
        let fence = seed("bjob-watch-cancel").await;
        let signal = JobSignal::new();
        signal.set_uncertain(true);
        assert!(!lease_watch_once(&jobs, &fence, &signal).await);
        assert_eq!((signal.stopped(), signal.uncertain()), (None, false));
        // Cancelled: the renewal is refused and the reason is Cancelled.
        assert!(jobs.cancel(&tenant, &fence.job_id).await.unwrap());
        assert!(lease_watch_once(&jobs, &fence, &signal).await);
        assert_eq!(
            signal.stopped(),
            Some(munarium_store_pg::jobs::StopReason::Cancelled)
        );

        // Reclaimed under the same node name: LeaseLost.
        let fence = seed("bjob-watch-reclaim").await;
        sqlx::query(
            "UPDATE index_build_jobs SET attempts = 2 WHERE tenant_id = $1 AND job_id = $2",
        )
        .bind(&tenant)
        .bind(&fence.job_id)
        .execute(&pool)
        .await
        .unwrap();
        let signal = JobSignal::new();
        assert!(lease_watch_once(&jobs, &fence, &signal).await);
        assert_eq!(
            signal.stopped(),
            Some(munarium_store_pg::jobs::StopReason::LeaseLost)
        );
        sqlx::query("UPDATE index_build_jobs SET state = 'cancelled' WHERE tenant_id = $1")
            .bind(&tenant)
            .execute(&pool)
            .await
            .unwrap();

        // A renewal that errors is not loss: uncertain, still running.
        let closed = BuildJobs::new(pool.clone());
        pool.close().await;
        let signal = JobSignal::new();
        assert!(!lease_watch_once(&closed, &fence, &signal).await);
        assert_eq!((signal.stopped(), signal.uncertain()), (None, true));
    }

    /// ClearView B2-T2: an event cursor is bound to the tenant and job it
    /// was issued for; one from another tenant or job, or a malformed one,
    /// is refused rather than read as a position.
    #[test]
    fn event_cursors_are_bound_to_their_tenant_and_job() {
        let c = encode_event_cursor("tenant-a", "bjob-1", 7);
        assert_eq!(decode_event_cursor("tenant-a", "bjob-1", &c).unwrap(), 7);
        assert!(decode_event_cursor("tenant-b", "bjob-1", &c).is_err());
        assert!(decode_event_cursor("tenant-a", "bjob-2", &c).is_err());
        for bad in [
            "",
            "7",
            "je1.7",
            "je1.-1.x",
            "je1.x.y",
            "je2.7.abc",
            "je1.7.abc.d",
        ] {
            assert!(
                decode_event_cursor("tenant-a", "bjob-1", bad).is_err(),
                "{bad:?}"
            );
        }
    }

    /// R12-T1: every id Server has issued, and every fixture id, round-trips
    /// through the new validator; malformed ids are rejected before work;
    /// and the generated OpenAPI declares exactly the validator's syntax.
    #[test]
    fn job_ids_validate_at_the_new_boundaries_and_match_the_schema() {
        let fixture: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let mut issued: Vec<String> = fixture["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["job_id"].as_str().unwrap().to_string())
            .collect();
        issued.push(format!("bjob-{}", uuid::Uuid::new_v4().simple()));
        for id in &issued {
            assert_eq!(JobId::parse(id).unwrap().as_str(), id, "issued id {id}");
        }
        for v in [
            "idx-0123abcd",
            "idx2-9f86d081884c7d65",
            "iv-fixture-expired",
        ] {
            assert_eq!(VersionCursor::parse(v).unwrap().as_str(), v);
        }
        let long = "a".repeat(201);
        for bad in [
            "",
            "-leading",
            ".x",
            "a b",
            "a/b",
            "a%2Fb",
            "job\u{e9}",
            "a\n",
            long.as_str(),
        ] {
            assert!(JobId::parse(bad).is_err(), "{bad:?}");
            assert!(VersionCursor::parse(bad).is_err(), "{bad:?}");
        }
        assert!(JobId::parse(&"a".repeat(200)).is_ok());

        let doc = serde_json::to_value(crate::openapi::doc()).unwrap();
        for route in ["versions", "progress", "events"] {
            let path = format!("/v1/index-build-jobs/{{job_id}}/{route}");
            let params = doc["paths"][&path]["get"]["parameters"].as_array().unwrap();
            let job = params.iter().find(|p| p["name"] == "job_id").unwrap();
            assert_eq!(job["in"], "path");
            let schema = &job["schema"];
            assert_eq!(
                schema["pattern"].as_str(),
                Some(munarium_store_pg::jobs::OPAQUE_ID_PATTERN),
                "{path}"
            );
            assert_eq!(schema["maxLength"].as_u64(), Some(200), "{path}");
            assert_eq!(schema["minLength"].as_u64(), Some(1), "{path}");
            let limit = params
                .iter()
                .find(|p| p["name"] == "limit")
                .map(|p| &p["schema"]);
            if let Some(limit) = limit {
                assert_eq!(
                    (limit["minimum"].as_f64(), limit["maximum"].as_f64()),
                    (Some(1.0), Some(200.0))
                );
            }
        }
        assert_eq!(munarium_store_pg::jobs::OPAQUE_ID_MAX_LEN, 200);
    }

    #[test]
    fn job_page_limits_are_checked() {
        let page = |l: &str| JobPage {
            cursor: None,
            limit: Some(l.into()),
        };
        assert_eq!(JobPage::default().limit().unwrap(), 50);
        assert_eq!(page("1").limit().unwrap(), 1);
        assert_eq!(page("200").limit().unwrap(), 200);
        for bad in ["0", "201", "-1", "x", ""] {
            assert!(page(bad).limit().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn v14_job_list_wraps_dtos_in_a_jobs_array() {
        let fixture: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let rows = fixture["rows"].as_array().unwrap();
        let list = dto::IndexBuildJobListResponse {
            jobs: rows.iter().map(|r| job_dto(job_from_row(r))).collect(),
        };
        let got = serde_json::to_value(list).unwrap();
        let obj = got.as_object().unwrap();
        assert_eq!(obj.len(), 1, "list response grew a member: {got}");
        let expected: Vec<_> = rows.iter().map(|r| expected_now(r).clone()).collect();
        assert_eq!(got["jobs"], serde_json::Value::Array(expected));
    }
}
