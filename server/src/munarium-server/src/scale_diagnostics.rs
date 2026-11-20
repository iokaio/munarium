// SPDX-License-Identifier: Apache-2.0
//! Replica, pool and queue diagnostics (ClearView SCALE-3).
//!
//! One snapshot answers "what is this replica, how full are its pools, and is
//! the build queue backing up" for three readers:
//!
//! - `/metrics` on the ops plane: process gauges plus the build-job backlog
//!   across every tenant. Counts only, no tenant or job labels.
//! - `GET /v1/reports/scale` (mgmt role): this process's identity and pools,
//!   and the caller's own tenant's backlog.
//! - The console Health page, from the same report.
//!
//! The backlog read is the only database work here. It is read-only, bounded
//! by [`QUEUE_READ_TIMEOUT`], and uses the claim index on
//! `index_build_jobs (state, created_at)`. A failed or timed-out read is
//! reported as unavailable, never as an empty queue (the U5 rule).
//!
//! Live fleet membership comes from the replica heartbeats
//! (`replica_fleet.rs`, migration 0090): the report sets `observed_replicas`
//! to the live heartbeats of this replica's deployment environment beside the
//! configured `MUNARIUM_REPLICA_COUNT`, and warns when more are live than
//! configured. A failed heartbeat read is `unavailable` with a null count,
//! never 0. A v1.4 process writes no heartbeat, so it is not counted; fewer
//! live than configured is therefore shown but not a warning. The memory
//! store is single-replica by validation and reports itself.

use crate::state::AppState;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use munarium_core::{KernelError, Result};
use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;
use utoipa::ToSchema;

/// Upper bound on one backlog read. A scrape or page load must not wait on
/// a saturated pool, which is exactly when an operator is looking.
pub const QUEUE_READ_TIMEOUT: Duration = Duration::from_secs(2);

/// Open durable build jobs, as read from `index_build_jobs`.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct BuildQueueBacklog {
    /// Jobs waiting for a builder (`state = 'pending'`).
    pub pending: i64,
    /// Jobs claimed by a builder (`state = 'running'`), including any whose
    /// lease has lapsed and that a builder may reclaim.
    pub running: i64,
    /// Seconds since the oldest pending job was enqueued; null when nothing
    /// is pending.
    pub oldest_pending_age_seconds: Option<f64>,
}

/// Read the backlog for one tenant, or across every tenant when `tenant` is
/// `None` (the ops-plane metric). Read-only; bounded by
/// [`QUEUE_READ_TIMEOUT`], including the wait for a pool connection.
pub async fn read_build_queue<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    tenant: Option<&str>,
) -> Result<BuildQueueBacklog> {
    let query = sqlx::query_as::<_, (i64, i64, Option<f64>)>(
        "SELECT count(*) FILTER (WHERE state = 'pending'),
                count(*) FILTER (WHERE state = 'running'),
                EXTRACT(EPOCH FROM now() - min(created_at) FILTER (WHERE state = 'pending'))::float8
           FROM index_build_jobs
          WHERE state IN ('pending', 'running')
            AND ($1::text IS NULL OR tenant_id = $1)",
    )
    .bind(tenant)
    .fetch_one(exec);
    let (pending, running, age) = tokio::time::timeout(QUEUE_READ_TIMEOUT, query)
        .await
        .map_err(|_| KernelError::Storage("build queue read timed out".into()))?
        .map_err(|e| KernelError::Storage(e.to_string()))?;
    Ok(BuildQueueBacklog {
        pending,
        running,
        // Clock skew between the database and an enqueuing transaction can
        // make a just-created row look a few microseconds in the future.
        oldest_pending_age_seconds: age.map(|a| a.max(0.0)),
    })
}

/// The SQL connection pool of this process (PostgreSQL store only).
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct DbPoolUsage {
    /// `MUNARIUM_DB_MAX_CONNS`: the most connections this process opens.
    pub max_connections: u32,
    /// Connections open now.
    pub open: u32,
    /// Open connections not checked out.
    pub idle: u32,
    /// Open connections checked out by work in progress.
    pub in_use: u32,
}

/// One admission pool on one plane (R7/B3, `docs/ops/admission.md`).
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct AdmissionPoolUsage {
    /// `rest` or `grpc`.
    pub plane: String,
    /// `ordinary`, or `reserve` / `heavy` when configured.
    pub pool: String,
    pub capacity: u64,
    pub in_use: u64,
}

/// The caller's tenant's build backlog, or why it could not be read.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct BuildQueueReport {
    /// `observed`; `unavailable` when the read failed or timed out (the
    /// counts are then null, never 0); `not_applicable` on the memory store,
    /// which has no durable job queue.
    pub status: String,
    /// The counts, present only when `status` is `observed`.
    pub backlog: Option<BuildQueueBacklog>,
    /// A short reason when `status` is `unavailable`.
    pub reason: Option<String>,
}

/// Live Server processes sharing this replica's database and deployment
/// environment, from their heartbeats.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct ReplicaMembership {
    /// `observed`, or `unavailable` when the heartbeat read failed or timed
    /// out (the count is then null, never 0).
    pub status: String,
    /// `MUNARIUM_DEPLOYMENT_ENVIRONMENT_ID`: the scope of the heartbeats.
    pub environment_id: String,
    /// True when more replicas are live than `replica_count` allows: the
    /// provider limits are then divided too few ways. Null when unavailable.
    pub exceeds_configured: Option<bool>,
    /// The live replicas, ordered by instance id. Empty when unavailable.
    pub replicas: Vec<crate::replica_fleet::LiveReplica>,
    /// A short reason when `status` is `unavailable`.
    pub reason: Option<String>,
}

/// GET /v1/reports/scale.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct ScaleReport {
    /// The replica that answered: `MUNARIUM_INSTANCE_ID`, else the host name.
    pub instance_id: String,
    /// Server version of this process.
    pub version: String,
    /// `MUNARIUM_REPLICA_COUNT` this process runs with: the divisor of each
    /// provider RPM/TPM limit. 1 is also the default when it is unset.
    pub replica_count: u32,
    /// Live Server processes (unexpired heartbeats) sharing the database and
    /// deployment environment. Null when the heartbeat read failed: unknown,
    /// never 0. v1.4 processes write no heartbeat and are not counted.
    pub observed_replicas: Option<u32>,
    /// Whether this process runs the durable build-job loop
    /// (`MUNARIUM_DATASTORE_BUILDER=enabled`).
    pub builder_enabled: bool,
    /// Null on the memory store.
    pub db_pool: Option<DbPoolUsage>,
    /// Per plane and pool; `MUNARIUM_MAX_CONCURRENCY` with the optional
    /// control reserve and heavy bound.
    pub admission: Vec<AdmissionPoolUsage>,
    /// The caller's tenant only. `/metrics` carries the all-tenant backlog.
    pub build_queue: BuildQueueReport,
    /// The live replicas and the warning when they exceed `replica_count`.
    pub replica_membership: ReplicaMembership,
}

/// Classify a heartbeat read against the configured count.
pub fn membership_report(
    environment_id: String,
    configured: u32,
    read: Result<Vec<crate::replica_fleet::LiveReplica>>,
) -> ReplicaMembership {
    match read {
        Ok(replicas) => ReplicaMembership {
            status: "observed".into(),
            environment_id,
            exceeds_configured: Some(replicas.len() as u64 > u64::from(configured)),
            replicas,
            reason: None,
        },
        Err(e) => {
            tracing::warn!(error = %e, "replica heartbeat read failed");
            ReplicaMembership {
                status: "unavailable".into(),
                environment_id,
                exceeds_configured: None,
                replicas: Vec::new(),
                reason: Some(match e {
                    KernelError::Storage(m) if m == "replica heartbeat read timed out" => m,
                    _ => "replica heartbeat read failed".into(),
                }),
            }
        }
    }
}

fn builder_enabled() -> bool {
    std::env::var("MUNARIUM_DATASTORE_BUILDER")
        .map(|v| v.eq_ignore_ascii_case("enabled"))
        .unwrap_or(false)
}

/// Process pool usage, or `None` on the memory store.
pub fn db_pool_usage(state: &AppState) -> Option<DbPoolUsage> {
    state.pg_pool().map(|p| {
        let open = p.size();
        let idle = u32::try_from(p.num_idle()).unwrap_or(u32::MAX);
        DbPoolUsage {
            max_connections: state.config.db_max_conns,
            open,
            idle,
            in_use: open.saturating_sub(idle),
        }
    })
}

/// Admission pools of both planes, in plane then pool order.
pub fn admission_usage(state: &AppState) -> Vec<AdmissionPoolUsage> {
    [&state.admission.rest, &state.admission.grpc]
        .into_iter()
        .flat_map(|plane| {
            plane
                .usage()
                .into_iter()
                .map(move |(pool, capacity, in_use)| AdmissionPoolUsage {
                    plane: plane.plane.to_string(),
                    pool: pool.to_string(),
                    capacity: capacity as u64,
                    in_use: in_use as u64,
                })
        })
        .collect()
}

/// Classify a backlog read for the report.
pub fn queue_report(read: Option<Result<BuildQueueBacklog>>) -> BuildQueueReport {
    match read {
        None => BuildQueueReport {
            status: "not_applicable".into(),
            backlog: None,
            reason: Some("the memory store has no durable build queue".into()),
        },
        Some(Ok(backlog)) => BuildQueueReport {
            status: "observed".into(),
            backlog: Some(backlog),
            reason: None,
        },
        Some(Err(e)) => {
            tracing::warn!(error = %e, "build queue diagnostics read failed");
            BuildQueueReport {
                status: "unavailable".into(),
                backlog: None,
                reason: Some(match e {
                    KernelError::Storage(m) if m == "build queue read timed out" => m,
                    _ => "build queue read failed".into(),
                }),
            }
        }
    }
}

/// The report for `tenant`, read now.
pub async fn op_scale_report(state: &AppState, tenant: &str) -> ScaleReport {
    let queue = match state.pg_pool() {
        Some(pool) => Some(read_build_queue(pool, Some(tenant)).await),
        None => None,
    };
    let membership = membership_report(
        state.fleet.environment(),
        state.config.replica_count,
        crate::replica_fleet::live_replicas(state).await,
    );
    ScaleReport {
        instance_id: state.config.instance_id.clone(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        replica_count: state.config.replica_count,
        observed_replicas: (membership.status == "observed")
            .then(|| u32::try_from(membership.replicas.len()).unwrap_or(u32::MAX)),
        builder_enabled: builder_enabled(),
        db_pool: db_pool_usage(state),
        admission: admission_usage(state),
        build_queue: queue_report(queue),
        replica_membership: membership,
    }
}

/// GET /v1/reports/scale — this replica's identity, pools and admission,
/// and the caller's tenant's build-job backlog (SCALE-3).
#[utoipa::path(get, path = "/v1/reports/scale",
    responses((status = 200, body = ScaleReport)), tag = "reports")]
pub async fn scale_report(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> std::result::Result<Json<ScaleReport>, crate::error::ApiError> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    Ok(Json(op_scale_report(&state, &ctx.tenant_id).await))
}

/// One exposition sample: metric name, rendered labels (may be empty) and
/// value.
pub type Sample = (&'static str, String, String);

/// The ops-plane samples for the all-tenant backlog. With no PostgreSQL
/// store there is no queue and nothing is emitted. A failed read emits
/// `munarium_index_build_queue_up 0` and omits the counts; with nothing
/// pending the oldest-pending age is 0.
pub fn queue_exposition(read: Option<&Result<BuildQueueBacklog>>) -> Vec<Sample> {
    let Some(read) = read else {
        return Vec::new();
    };
    let state = |s: &str| crate::metrics::labels(&[("state", s)]);
    match read {
        Ok(b) => vec![
            ("munarium_index_build_queue_up", String::new(), "1".into()),
            (
                "munarium_index_build_jobs",
                state("pending"),
                b.pending.to_string(),
            ),
            (
                "munarium_index_build_jobs",
                state("running"),
                b.running.to_string(),
            ),
            (
                "munarium_index_build_oldest_pending_seconds",
                String::new(),
                b.oldest_pending_age_seconds.unwrap_or(0.0).to_string(),
            ),
        ],
        Err(_) => vec![("munarium_index_build_queue_up", String::new(), "0".into())],
    }
}

/// The console Health page section (U5: a failed read shows unavailable).
pub fn health_section(report: &ScaleReport) -> String {
    use crate::charts::{esc, tile};
    let (pool_value, pool_sub) = match &report.db_pool {
        Some(p) => (
            format!("{} / {}", p.in_use, p.max_connections),
            format!("in use / max; {} open, {} idle", p.open, p.idle),
        ),
        None => ("n/a".into(), "memory store: no SQL pool".into()),
    };
    let mut note = String::new();
    let (pending, running, age) =
        match (&report.build_queue.status[..], &report.build_queue.backlog) {
            ("observed", Some(b)) => (
                b.pending.to_string(),
                b.running.to_string(),
                b.oldest_pending_age_seconds
                    .map(|s| format!("{s:.0} s"))
                    .unwrap_or_else(|| "none pending".into()),
            ),
            ("not_applicable", _) => ("n/a".into(), "n/a".into(), "n/a".into()),
            _ => {
                note = format!(
                    r#"<div class="notice">build queue unavailable: {}</div>"#,
                    esc(report
                        .build_queue
                        .reason
                        .as_deref()
                        .unwrap_or("read failed"))
                );
                let u = "unavailable".to_string();
                (u.clone(), u.clone(), u)
            }
        };
    let m = &report.replica_membership;
    let (live_value, live_sub) = match report.observed_replicas {
        Some(n) => (
            format!("{n} / {}", report.replica_count),
            "live / configured".to_string(),
        ),
        None => (
            "unavailable".to_string(),
            format!("configured {}", report.replica_count),
        ),
    };
    if m.status != "observed" {
        note.push_str(&format!(
            r#"<div class="notice">replica membership unavailable: {}</div>"#,
            esc(m.reason.as_deref().unwrap_or("read failed"))
        ));
    } else if m.exceeds_configured == Some(true) {
        note.push_str(&format!(
            r#"<div class="notice" role="alert">warning: {} replicas are live but MUNARIUM_REPLICA_COUNT is {}. Each replica divides the provider limits by {}, so the fleet together can exceed them. Set the count to the number of Server processes.</div>"#,
            m.replicas.len(),
            report.replica_count,
            report.replica_count
        ));
    }
    let members = if m.status == "observed" {
        let rows: Vec<Vec<String>> = m
            .replicas
            .iter()
            .map(|r| {
                vec![
                    esc(&r.instance_id),
                    esc(&r.version),
                    esc(&r.plane),
                    r.replica_count.to_string(),
                    esc(&r.capabilities.join(", ")),
                    format!("{:.0} s", r.seconds_since_heartbeat),
                ]
            })
            .collect();
        format!(
            r#"<div class="card">{}</div>"#,
            crate::dashboard::html_table(
                &[
                    "live replica",
                    "version",
                    "plane",
                    "configured count",
                    "capabilities",
                    "last heartbeat"
                ],
                &rows
            )
        )
    } else {
        String::new()
    };
    let rows: Vec<Vec<String>> = report
        .admission
        .iter()
        .map(|a| {
            vec![
                esc(&a.plane),
                esc(&a.pool),
                a.in_use.to_string(),
                a.capacity.to_string(),
            ]
        })
        .collect();
    format!(
        r#"<h2>replicas, pools and queue</h2><div class="tiles">{}{}{}{}{}{}</div>{note}
{members}<div class="card">{}</div>
<div class="notice">Configured count is this process's <code>MUNARIUM_REPLICA_COUNT</code>, the provider-limit divisor. Live replicas are the unexpired heartbeats in deployment environment <code>{}</code>; a v1.4 process writes none and is not counted, so fewer live than configured is expected while v1.4 processes still run. The build queue counts this tenant's jobs; <code>/metrics</code> carries every tenant's. Read the same data from <code>GET /v1/reports/scale</code>.</div>"#,
        tile("replicas", &live_value, &live_sub),
        tile(
            "builder",
            if report.builder_enabled {
                "enabled"
            } else {
                "off"
            },
            &report.instance_id
        ),
        tile("db pool", &pool_value, &pool_sub),
        tile("jobs pending", &pending, "this tenant"),
        tile("jobs running", &running, "this tenant"),
        tile("oldest pending", &age, "this tenant"),
        crate::dashboard::html_table(&["plane", "admission pool", "in use", "capacity"], &rows),
        esc(&m.environment_id),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(queue: Option<Result<BuildQueueBacklog>>) -> ScaleReport {
        let membership = membership_report("env".into(), 2, Ok(Vec::new()));
        ScaleReport {
            instance_id: "pod-<a>".into(),
            version: "x".into(),
            replica_count: 2,
            observed_replicas: Some(0),
            builder_enabled: false,
            db_pool: Some(DbPoolUsage {
                max_connections: 10,
                open: 4,
                idle: 1,
                in_use: 3,
            }),
            admission: vec![AdmissionPoolUsage {
                plane: "rest".into(),
                pool: "ordinary".into(),
                capacity: 8,
                in_use: 5,
            }],
            build_queue: queue_report(queue),
            replica_membership: membership,
        }
    }

    fn live(instance: &str) -> crate::replica_fleet::LiveReplica {
        crate::replica_fleet::LiveReplica {
            replica_id: format!("r-{instance}"),
            instance_id: instance.into(),
            version: "1.5.0".into(),
            plane: "rest".into(),
            replica_count: 2,
            capabilities: vec!["console-sessions".into()],
            seconds_since_heartbeat: 3.2,
        }
    }

    /// The membership rule (SCALE-3): live against configured, a warning
    /// only when more are live, and a failed read unavailable, never 0.
    #[test]
    fn membership_warns_on_excess_and_a_failed_read_is_unavailable() {
        let two = membership_report("env".into(), 2, Ok(vec![live("a"), live("b")]));
        assert_eq!(two.exceeds_configured, Some(false));
        let one = membership_report("env".into(), 2, Ok(vec![live("a")]));
        assert_eq!(
            one.exceeds_configured,
            Some(false),
            "a shortfall is no warning"
        );
        let three = membership_report("env".into(), 2, Ok(vec![live("a"), live("b"), live("<c>")]));
        assert_eq!(three.exceeds_configured, Some(true));
        let mut r = report(None);
        r.observed_replicas = Some(3);
        r.replica_membership = three;
        let html = health_section(&r);
        assert!(html.contains("3 / 2"), "{html}");
        assert!(html.contains("warning: 3 replicas are live"), "{html}");
        assert!(html.contains("&lt;c&gt;"), "{html}");
        r.observed_replicas = Some(2);
        r.replica_membership = two;
        let html = health_section(&r);
        assert!(
            html.contains("2 / 2") && !html.contains("warning:"),
            "{html}"
        );

        let failed = membership_report(
            "env".into(),
            2,
            Err(KernelError::Storage("connection refused <db>".into())),
        );
        assert_eq!(failed.status, "unavailable");
        assert_eq!(failed.exceeds_configured, None);
        assert!(failed.replicas.is_empty());
        assert_eq!(
            failed.reason.as_deref(),
            Some("replica heartbeat read failed")
        );
        let mut r = report(None);
        r.observed_replicas = None;
        r.replica_membership = failed;
        let json = serde_json::to_value(&r).unwrap();
        assert!(json["observed_replicas"].is_null(), "{json}");
        assert!(json["replica_membership"]["exceeds_configured"].is_null());
        let html = health_section(&r);
        assert!(html.contains("replica membership unavailable"), "{html}");
        assert!(
            !html.contains("0 / 2") && !html.contains("&lt;db&gt;"),
            "{html}"
        );
        assert!(!html.contains("live replica"), "{html}");
    }

    #[test]
    fn a_failed_backlog_read_is_unavailable_never_zero() {
        let failed = Err(KernelError::Storage("connection refused <db>".into()));
        let r = report(Some(failed));
        assert_eq!(r.build_queue.status, "unavailable");
        assert_eq!(r.build_queue.backlog, None);
        // The API reason is generic; the detail stays in the log.
        assert_eq!(
            r.build_queue.reason.as_deref(),
            Some("build queue read failed")
        );
        let json = serde_json::to_value(&r).unwrap();
        assert!(json["build_queue"]["backlog"].is_null(), "{json}");
        let html = health_section(&r);
        assert!(html.contains("build queue unavailable"), "{html}");
        assert!(!html.contains(r#"<div class="value">0</div>"#), "{html}");
        assert_eq!(
            html.matches(r#"<div class="value">unavailable</div>"#)
                .count(),
            3
        );
        assert!(!html.contains("<db>"), "{html}");

        let timed_out = Err(KernelError::Storage("build queue read timed out".into()));
        let r = report(Some(timed_out));
        assert_eq!(
            r.build_queue.reason.as_deref(),
            Some("build queue read timed out")
        );
    }

    #[test]
    fn an_empty_backlog_is_zero_and_the_memory_store_is_not_applicable() {
        let empty = BuildQueueBacklog {
            pending: 0,
            running: 0,
            oldest_pending_age_seconds: None,
        };
        let r = report(Some(Ok(empty)));
        assert_eq!(r.build_queue.status, "observed");
        let html = health_section(&r);
        assert_eq!(
            html.matches(r#"<div class="value">0</div>"#).count(),
            2,
            "{html}"
        );
        assert!(html.contains("none pending"), "{html}");
        assert!(!html.contains("unavailable"), "{html}");

        let r = report(None);
        assert_eq!(r.build_queue.status, "not_applicable");
        let html = health_section(&r);
        assert!(
            !html.contains("unavailable") && html.contains("n/a"),
            "{html}"
        );
    }

    #[test]
    fn the_section_escapes_identity_and_shows_pools() {
        let r = report(Some(Ok(BuildQueueBacklog {
            pending: 7,
            running: 2,
            oldest_pending_age_seconds: Some(93.4),
        })));
        let html = health_section(&r);
        assert!(html.contains("pod-&lt;a&gt;"), "{html}");
        assert!(html.contains("3 / 10"), "{html}");
        assert!(html.contains("93 s"), "{html}");
        assert!(
            html.contains("<td>ordinary</td><td>5</td><td>8</td>"),
            "{html}"
        );
    }

    #[test]
    fn the_exposition_omits_counts_when_the_read_failed() {
        assert!(queue_exposition(None).is_empty());
        let failed: Result<BuildQueueBacklog> = Err(KernelError::Storage("x".into()));
        assert_eq!(
            queue_exposition(Some(&failed)),
            vec![(
                "munarium_index_build_queue_up",
                String::new(),
                "0".to_string()
            )]
        );
        let ok: Result<BuildQueueBacklog> = Ok(BuildQueueBacklog {
            pending: 3,
            running: 1,
            oldest_pending_age_seconds: Some(12.5),
        });
        let lines = queue_exposition(Some(&ok));
        assert_eq!(
            lines[0],
            ("munarium_index_build_queue_up", String::new(), "1".into())
        );
        assert_eq!(
            (lines[1].1.as_str(), lines[1].2.as_str()),
            ("state=\"pending\"", "3")
        );
        assert_eq!(
            (lines[2].1.as_str(), lines[2].2.as_str()),
            ("state=\"running\"", "1")
        );
        assert_eq!(lines[3].2, "12.5");
    }
}

#[cfg(test)]
mod pg_tests {
    //! SCALE-3 backlog reads and the platform-v1 count guard against a real
    //! PostgreSQL. Skipped without `MUNARIUM_TEST_DATABASE_URL`.
    //!
    //! Seeded job rows never commit: they are inserted and read inside one
    //! transaction that rolls back, so no other test's `claim_any` can see or
    //! take them.
    use super::*;
    use crate::config::AuthMode;
    use tower::ServiceExt;

    fn url() -> Option<String> {
        let url = std::env::var("MUNARIUM_TEST_DATABASE_URL").ok();
        if url.is_none() {
            eprintln!("unavailable: isolated PostgreSQL required");
        }
        url
    }

    async fn state(url: String, tenant: &str) -> Arc<AppState> {
        crate::providers_api::usage_tests::test_state_with_auth(
            Some(url),
            AuthMode::Static(vec![
                ("admin".into(), tenant.into(), "mgmt".into()),
                ("writer".into(), tenant.into(), "rw".into()),
            ]),
        )
        .await
    }

    async fn seed(tx: &mut sqlx::PgConnection, tenant: &str, job: &str, state: &str, age: f64) {
        sqlx::query(
            "INSERT INTO index_build_jobs
                 (tenant_id, job_id, kind, scope_kind, scope_id, state, claimed_by,
                  claimed_at, requested_by, created_at)
             VALUES ($1, $2, 'backfill', 'collection', $2, $3,
                     CASE WHEN $3 = 'running' THEN 'scale-test' END,
                     CASE WHEN $3 = 'running' THEN now() END,
                     'scale-test', now() - make_interval(secs => $4))",
        )
        .bind(tenant)
        .bind(job)
        .bind(state)
        .bind(age)
        .execute(tx)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn the_backlog_counts_open_jobs_per_tenant_and_ages_the_oldest_pending() {
        let Some(url) = url() else { return };
        let tenant = format!("scale-{}", uuid::Uuid::new_v4());
        let other = format!("scale-other-{}", uuid::Uuid::new_v4());
        let state = state(url, &tenant).await;
        let pool = state.pg_pool().unwrap();

        let mut tx = pool.begin().await.unwrap();
        seed(&mut tx, &tenant, "p-old", "pending", 120.0).await;
        seed(&mut tx, &tenant, "p-new", "pending", 5.0).await;
        seed(&mut tx, &tenant, "r-1", "running", 300.0).await;
        seed(&mut tx, &tenant, "done", "succeeded", 900.0).await;
        seed(&mut tx, &tenant, "gone", "failed", 900.0).await;
        seed(&mut tx, &other, "p-other", "pending", 4000.0).await;

        let mine = read_build_queue(&mut *tx, Some(&tenant)).await.unwrap();
        assert_eq!((mine.pending, mine.running), (2, 1));
        let age = mine.oldest_pending_age_seconds.unwrap();
        assert!((119.0..200.0).contains(&age), "oldest pending age {age}");

        let theirs = read_build_queue(&mut *tx, Some(&other)).await.unwrap();
        assert_eq!((theirs.pending, theirs.running), (1, 0));
        assert!(theirs.oldest_pending_age_seconds.unwrap() >= 3999.0);

        // The all-tenant read includes both tenants' rows (and any other
        // test's committed rows, so only lower bounds hold).
        let all = read_build_queue(&mut *tx, None).await.unwrap();
        assert!(all.pending >= 3 && all.running >= 1, "{all:?}");
        assert!(all.oldest_pending_age_seconds.unwrap() >= 3999.0);
        tx.rollback().await.unwrap();

        // Nothing committed: the tenant reads an observed, empty queue.
        let empty = read_build_queue(pool, Some(&tenant)).await.unwrap();
        assert_eq!(
            empty,
            BuildQueueBacklog {
                pending: 0,
                running: 0,
                oldest_pending_age_seconds: None
            }
        );
    }

    #[tokio::test]
    async fn the_report_route_is_mgmt_only_and_reports_observed_values() {
        let Some(url) = url() else { return };
        let tenant = format!("scale-{}", uuid::Uuid::new_v4());
        let state = state(url, &tenant).await;
        let router = crate::rest::router(state.clone());
        let get = |token: &str| {
            axum::http::Request::get("/v1/reports/scale")
                .header("authorization", format!("Bearer {token}"))
                .body(axum::body::Body::empty())
                .unwrap()
        };
        let denied = router.clone().oneshot(get("writer")).await.unwrap();
        assert_eq!(denied.status(), axum::http::StatusCode::FORBIDDEN);

        let ok = router.oneshot(get("admin")).await.unwrap();
        assert_eq!(ok.status(), axum::http::StatusCode::OK);
        let bytes = axum::body::to_bytes(ok.into_body(), 1_000_000)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["instance_id"], "usage-test");
        assert_eq!(body["replica_count"], 1);
        // Observed from the heartbeats (none written here), never null on a
        // successful read; the membership tests are in replica_fleet.rs.
        assert!(body["observed_replicas"].is_u64(), "{body}");
        assert_eq!(body["replica_membership"]["status"], "observed");
        assert_eq!(body["db_pool"]["max_connections"], 4);
        assert_eq!(body["build_queue"]["status"], "observed");
        assert_eq!(body["build_queue"]["backlog"]["pending"], 0);
        let planes: Vec<_> = body["admission"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| (a["plane"].as_str().unwrap(), a["pool"].as_str().unwrap()))
            .collect();
        assert_eq!(planes, vec![("rest", "ordinary"), ("grpc", "ordinary")]);
        // The request itself holds one REST slot while the report is read.
        assert_eq!(body["admission"][0]["in_use"], 1);
    }

    #[tokio::test]
    async fn the_metrics_scrape_carries_the_all_tenant_backlog() {
        let Some(url) = url() else { return };
        let state = state(url, &format!("scale-{}", uuid::Uuid::new_v4())).await;
        let resp = crate::ops::router(state)
            .oneshot(
                axum::http::Request::get("/metrics")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), 1_000_000)
            .await
            .unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        for line in [
            "munarium_index_build_queue_up 1",
            "munarium_db_pool_max_connections 4",
            "munarium_replica_count 1",
            "# TYPE munarium_index_build_jobs gauge",
            "munarium_index_build_jobs{state=\"pending\"} ",
            "munarium_index_build_jobs{state=\"running\"} ",
            "munarium_index_build_oldest_pending_seconds ",
        ] {
            assert!(body.contains(line), "missing {line}:\n{body}");
        }
    }

    #[tokio::test]
    async fn a_saturated_pool_times_out_as_unavailable() {
        let Some(url) = url() else { return };
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .unwrap();
        let _held = pool.acquire().await.unwrap();
        let started = std::time::Instant::now();
        let read = read_build_queue(&pool, None).await;
        assert!(started.elapsed() < QUEUE_READ_TIMEOUT + Duration::from_secs(2));
        let report = queue_report(Some(read));
        assert_eq!(report.status, "unavailable");
        assert_eq!(report.reason.as_deref(), Some("build queue read timed out"));
        assert_eq!(report.backlog, None);
    }

    #[tokio::test]
    async fn platform_v1_refuses_a_count_above_one_even_with_shared_storage() {
        let Some(url) = url() else { return };
        let state = state(url, &format!("scale-{}", uuid::Uuid::new_v4())).await;
        let mut config = state.config.clone();
        config.source_store = crate::config::SourceStoreConfig::Pg;
        config.replica_count = 2;
        config.platform = Some(crate::platform_api::PlatformConfig {
            certificate_file: "unused.crt".into(),
            private_key_file: "unused.key".into(),
            client_ca_file: "unused-ca.crt".into(),
            peers: Default::default(),
            tenants: Default::default(),
        });
        let err = AppState::new(config)
            .await
            .err()
            .expect("count 2 must be refused");
        assert!(
            err.to_string()
                .contains("platform-v1 requires one instance"),
            "{err}"
        );
    }
}

#[cfg(test)]
mod unreachable_tests {
    use super::*;

    #[tokio::test]
    async fn an_unreachable_database_is_unavailable_never_an_empty_queue() {
        // Port 1 on loopback refuses at once; no database is needed.
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://nobody:nothing@127.0.0.1:1/none")
            .unwrap();
        let report = queue_report(Some(read_build_queue(&pool, Some("t")).await));
        assert_eq!(report.status, "unavailable");
        assert_eq!(report.backlog, None);
        let exposition = queue_exposition(Some(&read_build_queue(&pool, None).await));
        assert_eq!(
            exposition,
            vec![(
                "munarium_index_build_queue_up",
                String::new(),
                "0".to_string()
            )]
        );
    }
}
