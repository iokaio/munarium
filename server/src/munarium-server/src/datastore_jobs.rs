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
use munarium_store_pg::jobs::{BuildJob, BuildJobs, EnqueueOutcome, JobKind, JobTarget};

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
pub async fn run_one_job(state: &AppState, job: &BuildJob) -> Result<serde_json::Value> {
    let tenant = job.tenant_id.as_str();
    match JobKind::parse(&job.kind)? {
        JobKind::Backfill => {
            let scope_id = job
                .scope_id
                .as_deref()
                .ok_or_else(|| KernelError::InvalidInput("a backfill job names a scope".into()))?;
            let report =
                crate::datastore_builds::op_backfill_collection(state, tenant, scope_id).await?;
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
            let built =
                crate::datastore_builds::op_rebuild_artifact(state, tenant, version).await?;
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
            let ctx = state.mirror_context(tenant)?;
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
    let lease_secs = std::env::var("MUNARIUM_DATASTORE_JOB_LEASE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(600i64)
        .max(30);

    let weak = Arc::downgrade(state);
    tokio::spawn(async move {
        let jobs = BuildJobs::new(pool);
        loop {
            let Some(state) = weak.upgrade() else { return };
            let node = state.config.instance_id.clone();
            match jobs.claim_any(&node, lease_secs, MAX_JOB_ATTEMPTS).await {
                Ok(Some(job)) => {
                    tracing::info!(job_id = %job.job_id, kind = %job.kind, "build job claimed");
                    // Keep the JOB lease alive for as long as the build runs.
                    // The attempt lease underneath is heartbeated by the
                    // mirror; the job lease was not, so any build longer than
                    // `lease_secs` was re-offered to another replica while
                    // this one was still working on it. Stopped by the guard
                    // whichever way the build ends.
                    let beat = {
                        let jobs = jobs.clone();
                        let tenant = job.tenant_id.clone();
                        let job_id = job.job_id.clone();
                        let node = node.clone();
                        let every = std::time::Duration::from_secs((lease_secs as u64 / 3).max(5));
                        AbortOnDrop(tokio::spawn(async move {
                            loop {
                                tokio::time::sleep(every).await;
                                match jobs.heartbeat(&tenant, &job_id, &node).await {
                                    Ok(true) => {}
                                    Ok(false) => {
                                        tracing::warn!(
                                            job_id = %job_id,
                                            "build job lease is no longer ours; another builder may own it"
                                        );
                                        break;
                                    }
                                    Err(e) => {
                                        tracing::warn!(error = %e, job_id = %job_id, "build job heartbeat failed")
                                    }
                                }
                            }
                        }))
                    };
                    let outcome = run_one_job(&state, &job).await.map_err(|e| e.to_string());
                    drop(beat);
                    match jobs
                        .complete(&job.tenant_id, &job.job_id, &node, outcome, None)
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

/// A job that has been claimed this many times has failed enough. Its last
/// error stands; an operator re-enqueues deliberately.
pub const MAX_JOB_ATTEMPTS: i32 = 3;

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

fn job_dto(j: BuildJob) -> dto::IndexBuildJobDto {
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
        error: j.error,
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
