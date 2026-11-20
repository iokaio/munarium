// SPDX-License-Identifier: Apache-2.0
//! SCALE-2: a draining builder stops claiming, and a build stopped for the
//! drain releases its claim instead of failing the job.
//!
//! Claims are global across tenants, so the database tests run against their
//! own scratch database: no other suite's pending job can be claimed here,
//! and nothing these tests release can be claimed by another suite. They skip
//! loudly when `MUNARIUM_TEST_DATABASE_URL` is unset.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use munarium_store_pg::jobs::{
    BuildJobs, ClaimPolicy, EnqueueOutcome, JobFence, JobKind, JobSignal, JobTarget, StopReason,
};

use super::*;
use crate::config::{AuthMode, Config, DocIntelConfig, SourceStoreConfig, StoreKind};

fn config(store: StoreKind, database_url: Option<String>) -> Config {
    let pg = store == StoreKind::Postgres;
    Config {
        http_addr: "127.0.0.1:0".into(),
        grpc_addr: None,
        ops_addr: "127.0.0.1:0".into(),
        store,
        database_url,
        platform: None,
        auth: AuthMode::Disabled,
        shutdown_grace_secs: 1,
        token_secret: None,
        token_ttl_secs: 3600,
        require_uid: false,
        interaction_body_max: 32768,
        token_revocation_check: false,
        matrix_base_url: None,
        matrix_admin_url: None,
        max_concurrency: 4,
        db_max_conns: 4,
        idempotency_ttl_secs: 0,
        replica_count: 1,
        registry_ttl_secs: 15,
        session_idle_ttl_secs: 0,
        evidence_purge_interval_secs: 0,
        managed_provider_diagnostics: false,
        max_tokens: Default::default(),
        instance_id: "builder-drain-test".into(),
        source_store: if pg {
            SourceStoreConfig::Pg
        } else {
            SourceStoreConfig::Mem
        },
        doc_intel: DocIntelConfig::None,
    }
}

async fn memory_state() -> Arc<AppState> {
    AppState::new(config(StoreKind::Memory, None))
        .await
        .unwrap()
}

/// A throwaway database that owns its whole job queue, dropped (with FORCE)
/// when the guard goes out of scope, including while an assertion unwinds.
struct ScratchDatabase {
    admin_url: String,
    name: String,
    url: String,
}

impl ScratchDatabase {
    async fn create(base_url: &str) -> Self {
        use sqlx::Connection;
        let name = format!("munarium_drain_{}", uuid::Uuid::new_v4().simple());
        let mut admin = sqlx::PgConnection::connect(base_url).await.unwrap();
        sqlx::query(&format!("CREATE DATABASE \"{name}\""))
            .execute(&mut admin)
            .await
            .unwrap();
        admin.close().await.unwrap();
        let (head, query) = match base_url.split_once('?') {
            Some((h, q)) => (h, Some(q)),
            None => (base_url, None),
        };
        let authority = head.find("://").unwrap() + 3;
        let base = match head[authority..].find('/') {
            Some(slash) => &head[..authority + slash],
            None => head,
        };
        let url = match query {
            Some(q) => format!("{base}/{name}?{q}"),
            None => format!("{base}/{name}"),
        };
        Self {
            admin_url: base_url.to_string(),
            name,
            url,
        }
    }
}

impl Drop for ScratchDatabase {
    fn drop(&mut self) {
        let admin_url = self.admin_url.clone();
        let name = self.name.clone();
        let outcome = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            rt.block_on(async move {
                use sqlx::Connection;
                let mut admin = sqlx::PgConnection::connect(&admin_url).await?;
                sqlx::query(&format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)"))
                    .execute(&mut admin)
                    .await?;
                admin.close().await
            })
            .map_err(|e| e.to_string())
        })
        .join();
        if !matches!(outcome, Ok(Ok(()))) {
            eprintln!("could not drop scratch database {}", self.name);
        }
    }
}

async fn enqueue(jobs: &BuildJobs, tenant: &str, version: &str) -> String {
    match jobs
        .enqueue(
            tenant,
            JobKind::Rebuild,
            JobTarget::Version(version),
            serde_json::json!({}),
            "drain-test",
            None,
        )
        .await
        .unwrap()
    {
        EnqueueOutcome::Enqueued(id) => id,
        other => panic!("expected a new job, got {other:?}"),
    }
}

async fn job_state(jobs: &BuildJobs, tenant: &str, job_id: &str) -> (String, i32) {
    let j = jobs.get(tenant, job_id).await.unwrap().unwrap();
    (j.state, j.attempts)
}

const SETTINGS: BuilderSettings = BuilderSettings {
    poll_ms: 60_000,
    policy: ClaimPolicy::Fifo,
    events_per_tenant: None,
    lease_secs: 600,
};

/// A draining builder claims nothing and returns. The control half shows
/// the same loop does claim when not draining, and that an idle loop leaves
/// its (one-minute) poll sleep as soon as the drain starts.
#[tokio::test]
async fn a_draining_builder_claims_nothing_and_an_idle_one_wakes_to_drain() {
    let Ok(base) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
        eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
        return;
    };
    let db = ScratchDatabase::create(&base).await;
    let tenant = "tenant-scale2-drain";
    let state = AppState::new(config(StoreKind::Postgres, Some(db.url.clone())))
        .await
        .unwrap();
    let pool = state.pg_pool().cloned().unwrap();
    let jobs = BuildJobs::new(pool.clone());

    // Draining before the loop starts: the queued job is never claimed.
    let held = enqueue(&jobs, tenant, "iv-drain-held").await;
    state.draining.store(true, Ordering::Release);
    let handle = spawn_builder_with(&state, pool.clone(), SETTINGS);
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("a draining builder returns")
        .unwrap();
    assert_eq!(
        job_state(&jobs, tenant, &held).await,
        ("pending".to_string(), 0),
        "a draining builder claimed a job"
    );

    // Control: not draining, the same loop claims it (the version does not
    // exist, so the build fails and the failure is recorded).
    state.draining.store(false, Ordering::Release);
    let handle = spawn_builder_with(&state, pool.clone(), SETTINGS);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let (st, attempts) = job_state(&jobs, tenant, &held).await;
        if st != "pending" && st != "running" {
            assert_eq!((st.as_str(), attempts), ("failed", 1));
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the builder never ran the job"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // The loop is now idle in a one-minute poll; the drain wakes it.
    tokio::time::sleep(Duration::from_millis(300)).await;
    state.draining.store(true, Ordering::Release);
    tokio::time::timeout(Duration::from_secs(3), handle)
        .await
        .expect("an idle builder leaves its poll sleep when draining")
        .unwrap();
    drop(state);
    pool.close().await;
    drop(db);
}

/// A build stopped for the drain releases its claim, and the next builder
/// reclaims it under a new generation; the drained holder can no longer
/// write. A build that finished first, or failed for its own reason, keeps
/// its recorded outcome. A release by a superseded fence changes nothing.
#[tokio::test]
async fn a_build_stopped_for_the_drain_releases_its_claim() {
    let Ok(base) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
        eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
        return;
    };
    let db = ScratchDatabase::create(&base).await;
    let tenant = "tenant-scale2-release";
    let store = munarium_store_pg::PgStore::connect(&db.url, tenant)
        .await
        .unwrap();
    let pool = store.pool().clone();
    let jobs = BuildJobs::new(pool.clone());
    let claim = |node: &'static str| {
        let jobs = jobs.clone();
        async move {
            jobs.claim_next(node, 600, MAX_JOB_ATTEMPTS, ClaimPolicy::Fifo)
                .await
                .unwrap()
                .expect("a claimable job")
        }
    };
    let draining = || {
        let s = JobSignal::new();
        s.stop(StopReason::Draining);
        s
    };

    // Stopped at a safe boundary for the drain: released, not failed.
    let released = enqueue(&jobs, tenant, "iv-drain-released").await;
    let job = claim("builder-a").await;
    assert_eq!(job.job_id, released);
    let fence = JobFence::of_claim(&job, "builder-a");
    let settled = settle_claim(&jobs, &job, &fence, &draining(), Err(fence.stale()))
        .await
        .unwrap();
    assert_eq!(settled, Settled::Released);
    assert_eq!(
        job_state(&jobs, tenant, &released).await,
        ("running".to_string(), 1),
        "a released claim is not a failure"
    );
    // The next builder reclaims it at once, under generation 2.
    let again = claim("builder-b").await;
    assert_eq!(
        (again.job_id.as_str(), again.attempts),
        (released.as_str(), 2)
    );
    assert_eq!(again.claimed_by.as_deref(), Some("builder-b"));
    // The drained holder is fenced out.
    assert!(!jobs.is_current(&fence).await.unwrap());
    assert!(!jobs
        .complete_fenced(
            &job.tenant_id,
            &job.job_id,
            "builder-a",
            1,
            Ok(serde_json::json!({})),
            None
        )
        .await
        .unwrap());
    // A release by the superseded fence changes nothing.
    assert!(!jobs.release_fenced(&fence).await.unwrap());
    let settled = settle_claim(&jobs, &job, &fence, &draining(), Err(fence.stale()))
        .await
        .unwrap();
    assert_eq!(settled, Settled::NotOurs);
    let current = JobFence::of_claim(&again, "builder-b");
    assert!(jobs.is_current(&current).await.unwrap());
    assert!(jobs
        .complete_fenced(
            tenant,
            &released,
            "builder-b",
            2,
            Ok(serde_json::json!({})),
            None
        )
        .await
        .unwrap());

    // Finished before reaching a boundary: the outcome is recorded.
    let finished = enqueue(&jobs, tenant, "iv-drain-finished").await;
    let job = claim("builder-a").await;
    assert_eq!(job.job_id, finished);
    let fence = JobFence::of_claim(&job, "builder-a");
    let settled = settle_claim(
        &jobs,
        &job,
        &fence,
        &draining(),
        Ok(serde_json::json!({"ok": 1})),
    )
    .await
    .unwrap();
    assert_eq!(settled, Settled::Recorded);
    assert_eq!(
        job_state(&jobs, tenant, &finished).await,
        ("succeeded".to_string(), 1)
    );

    // A genuine failure during the drain is recorded, not released.
    let broken = enqueue(&jobs, tenant, "iv-drain-broken").await;
    let job = claim("builder-a").await;
    assert_eq!(job.job_id, broken);
    let fence = JobFence::of_claim(&job, "builder-a");
    let settled = settle_claim(
        &jobs,
        &job,
        &fence,
        &draining(),
        Err(KernelError::InvalidInput("a real failure".into())),
    )
    .await
    .unwrap();
    assert_eq!(settled, Settled::Recorded);
    assert_eq!(
        job_state(&jobs, tenant, &broken).await,
        ("failed".to_string(), 1)
    );

    // The stale error without a drain (a cancel or lost lease) is still
    // completed through the fence, as before SCALE-2.
    let lost = enqueue(&jobs, tenant, "iv-drain-lost").await;
    let job = claim("builder-a").await;
    assert_eq!(job.job_id, lost);
    let fence = JobFence::of_claim(&job, "builder-a");
    let signal = JobSignal::new();
    signal.stop(StopReason::LeaseLost);
    assert!(jobs.cancel(tenant, &lost).await.unwrap());
    let settled = settle_claim(&jobs, &job, &fence, &signal, Err(fence.stale()))
        .await
        .unwrap();
    assert_eq!(settled, Settled::NotOurs);
    assert_eq!(
        job_state(&jobs, tenant, &lost).await,
        ("cancelled".to_string(), 1)
    );

    // The final claim, released by a drain, is failed by the next poll's
    // cleanup exactly as a crashed final claim is (R4): no fourth claim.
    let last = enqueue(&jobs, tenant, "iv-drain-last").await;
    for generation in 1..=MAX_JOB_ATTEMPTS {
        let node = if generation % 2 == 0 {
            "builder-b"
        } else {
            "builder-a"
        };
        let job = claim(node).await;
        assert_eq!(
            (job.job_id.as_str(), job.attempts),
            (last.as_str(), generation)
        );
        let fence = JobFence::of_claim(&job, node);
        assert_eq!(
            settle_claim(&jobs, &job, &fence, &draining(), Err(fence.stale()))
                .await
                .unwrap(),
            Settled::Released
        );
    }
    assert!(jobs
        .claim_next("builder-c", 600, MAX_JOB_ATTEMPTS, ClaimPolicy::Fifo)
        .await
        .unwrap()
        .is_none());
    let failed = jobs
        .fail_expired_final_claims(600, MAX_JOB_ATTEMPTS, 10)
        .await
        .unwrap();
    assert_eq!(failed, vec![(tenant.to_string(), last.clone())]);
    pool.close().await;
    drop(db);
}

/// The drain watch stops an in-flight build's signal with `Draining` once
/// the process drains, and not before.
#[tokio::test]
async fn the_drain_watch_stops_the_build_signal() {
    let state = memory_state().await;
    let signal = JobSignal::new();
    let watch = spawn_drain_watch(Arc::downgrade(&state), signal.clone());
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(signal.stopped(), None);
    state.draining.store(true, Ordering::Release);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while signal.stopped().is_none() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the drain never reached the build"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(signal.stopped(), Some(StopReason::Draining));
    drop(watch);
}

/// Shutdown waits for the builder loop within the grace period, and aborts
/// one that outlives it. Without a builder it returns at once.
#[tokio::test]
async fn shutdown_waits_for_the_builder_within_the_grace_period() {
    let state = memory_state().await;
    assert!(!has_builder(&state));
    assert!(drain_builder(&state, Duration::from_millis(10)).await);
    assert!(state.draining.load(Ordering::Acquire));

    let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = finished.clone();
    *state.builder_task.lock().unwrap() = Some(tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        flag.store(true, Ordering::Release);
    }));
    assert!(has_builder(&state));
    assert!(drain_builder(&state, Duration::from_secs(5)).await);
    assert!(finished.load(Ordering::Acquire), "shutdown did not wait");
    assert!(!has_builder(&state));

    let stuck = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = stuck.clone();
    *state.builder_task.lock().unwrap() = Some(tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(3600)).await;
        flag.store(true, Ordering::Release);
    }));
    let started = tokio::time::Instant::now();
    assert!(!drain_builder(&state, Duration::from_millis(200)).await);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(!stuck.load(Ordering::Acquire));
}
