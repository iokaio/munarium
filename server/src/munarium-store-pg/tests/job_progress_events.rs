// SPDX-License-Identifier: Apache-2.0
//! ClearView R9: fenced job progress and the transactional, per-tenant
//! bounded event ring (B2), against a real PostgreSQL.
//!
//! ONE test function: the retention count is database-wide, and this test
//! lowers it temporarily, so nothing else in this binary may run beside it.
//! It restores the provisional default before returning. Every job row is
//! seeded tenant-scoped, running rows with `claimed_at = 'infinity'`, and left
//! terminal, so no claimer or cleaner can take one. Skips loudly when
//! `MUNARIUM_TEST_DATABASE_URL` is unset.

use munarium_store_pg::jobs::{
    event_retention, set_event_retention, BuildJobs, JobFence, ProgressPhase,
    DEFAULT_EVENTS_PER_TENANT,
};
use munarium_store_pg::PgStore;

fn url() -> Option<String> {
    std::env::var("MUNARIUM_TEST_DATABASE_URL").ok()
}

async fn seed_running(pool: &sqlx::PgPool, tenant: &str, job_id: &str, node: &str) -> JobFence {
    sqlx::query(
        "INSERT INTO index_build_jobs
             (tenant_id, job_id, kind, index_version_id, state, claimed_by, claimed_at,
              attempts, requested_by)
         VALUES ($1,$2,'rebuild',$2,'running',$3,'infinity',1,'test-r9')",
    )
    .bind(tenant)
    .bind(job_id)
    .bind(node)
    .execute(pool)
    .await
    .unwrap();
    JobFence {
        tenant_id: tenant.into(),
        job_id: job_id.into(),
        node_id: node.into(),
        generation: 1,
    }
}

async fn event_kinds(pool: &sqlx::PgPool, tenant: &str, job_id: &str) -> Vec<(i64, String)> {
    sqlx::query_as(
        "SELECT job_event_seq, kind FROM index_build_job_events
          WHERE tenant_id = $1 AND job_id = $2 ORDER BY job_event_seq",
    )
    .bind(tenant)
    .bind(job_id)
    .fetch_all(pool)
    .await
    .unwrap()
}

/// Move a job through `n` meaningful transitions (progress phase changes).
async fn churn(jobs: &BuildJobs, fence: &JobFence, n: usize) {
    let phases = [ProgressPhase::Sealing, ProgressPhase::Publishing];
    for i in 0..n {
        assert!(jobs
            .record_progress(fence, phases[i % 2], None, None)
            .await
            .unwrap());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn progress_is_fenced_and_events_are_transactional_and_bounded_per_tenant() {
    if url().is_none() {
        eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
        return;
    }
    // The body lowers the database-wide retention count. Run it on its own
    // task so the default is restored even when an assertion fails, and
    // other tests never see the lowered bound afterwards.
    let outcome = tokio::spawn(scenario()).await;
    let store = PgStore::connect(&url().unwrap(), "tenant-r9-restore")
        .await
        .unwrap();
    set_event_retention(store.pool(), DEFAULT_EVENTS_PER_TENANT)
        .await
        .unwrap();
    if let Err(e) = outcome {
        std::panic::resume_unwind(e.into_panic());
    }
}

async fn scenario() {
    let run = uuid::Uuid::new_v4().simple().to_string();
    let ta = format!("tenant-r9-a-{run}");
    let tb = format!("tenant-r9-b-{run}");
    let store = PgStore::connect(&url().unwrap(), &ta).await.unwrap();
    let pool = store.pool().clone();
    let jobs = BuildJobs::new(pool.clone());
    assert_eq!(
        event_retention(&pool).await.unwrap(),
        DEFAULT_EVENTS_PER_TENANT,
        "the provisional default"
    );

    // --- Progress: fenced, monotonic, unknown total stays unknown. -------
    let f1 = seed_running(&pool, &ta, "bjob-r9-progress", "builder-same").await;
    assert!(jobs
        .record_progress(&f1, ProgressPhase::Extracting, Some(0), None)
        .await
        .unwrap());
    let p = jobs.progress(&ta, &f1.job_id).await.unwrap().unwrap();
    assert_eq!(
        (p.phase.as_deref(), p.units_done, p.units_total),
        (Some("extracting"), Some(0), None),
        "an unknown total is absent, never zero"
    );
    let first_advance = p.last_advance_at.clone().expect("advanced");
    assert!(jobs
        .record_progress(&f1, ProgressPhase::Extracting, Some(3), Some(5))
        .await
        .unwrap());
    assert!(jobs
        .record_progress(&f1, ProgressPhase::Extracting, Some(2), None)
        .await
        .unwrap());
    let p = jobs.progress(&ta, &f1.job_id).await.unwrap().unwrap();
    assert_eq!(
        (p.units_done, p.units_total),
        (Some(3), Some(5)),
        "counters never decrease and None keeps the total"
    );
    assert!(p.last_advance_at.unwrap() >= first_advance);
    // A reclaim under the SAME node name retires generation 1.
    sqlx::query(
        "UPDATE index_build_jobs SET attempts = 2, claimed_at = 'infinity'
          WHERE tenant_id = $1 AND job_id = $2",
    )
    .bind(&ta)
    .bind(&f1.job_id)
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        !jobs
            .record_progress(&f1, ProgressPhase::Uploading, Some(5), Some(5))
            .await
            .unwrap(),
        "a stale holder cannot write progress"
    );
    let p = jobs.progress(&ta, &f1.job_id).await.unwrap().unwrap();
    assert_eq!(
        (p.phase.as_deref(), p.units_done, p.job.attempts),
        (Some("extracting"), Some(3), 2),
        "the refused write changed nothing"
    );
    let other_node = JobFence {
        node_id: "builder-other".into(),
        generation: 2,
        ..f1.clone()
    };
    assert!(!jobs
        .record_progress(&other_node, ProgressPhase::Uploading, None, None)
        .await
        .unwrap());
    let f2 = JobFence {
        generation: 2,
        ..f1.clone()
    };
    assert!(jobs
        .record_progress(&f2, ProgressPhase::Uploading, None, None)
        .await
        .unwrap());
    // A cancelled job takes no more progress.
    assert!(jobs.cancel(&ta, &f1.job_id).await.unwrap());
    assert!(!jobs
        .record_progress(&f2, ProgressPhase::Uploading, Some(5), None)
        .await
        .unwrap());

    // --- Events: by trigger, with the change that caused them. ------------
    let fe = seed_running(&pool, &ta, "bjob-r9-events", "builder-e").await;
    // Insert: one `enqueued` event (state was inserted as running).
    assert_eq!(
        event_kinds(&pool, &ta, &fe.job_id).await,
        vec![(1, "enqueued".to_string())]
    );
    // Heartbeat only: no event.
    assert!(jobs
        .heartbeat_fenced(&ta, &fe.job_id, "builder-e", 1)
        .await
        .unwrap());
    // Counter only: no event. A phase change: one event.
    assert!(jobs
        .record_progress(&fe, ProgressPhase::Extracting, Some(1), None)
        .await
        .unwrap());
    assert!(jobs
        .record_progress(&fe, ProgressPhase::Extracting, Some(2), None)
        .await
        .unwrap());
    assert_eq!(
        event_kinds(&pool, &ta, &fe.job_id).await,
        vec![(1, "enqueued".to_string()), (2, "progress".to_string())]
    );
    // A rolled-back transition leaves no event.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query(
        "UPDATE index_build_jobs SET state = 'failed' WHERE tenant_id = $1 AND job_id = $2",
    )
    .bind(&ta)
    .bind(&fe.job_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(event_kinds(&pool, &ta, &fe.job_id).await.len(), 2);
    // Completion, then a replayed completion and a late cancel: one event.
    assert!(jobs
        .complete_fenced(
            &ta,
            &fe.job_id,
            "builder-e",
            1,
            Ok(serde_json::json!({})),
            None
        )
        .await
        .unwrap());
    assert!(!jobs
        .complete_fenced(
            &ta,
            &fe.job_id,
            "builder-e",
            1,
            Ok(serde_json::json!({})),
            None
        )
        .await
        .unwrap());
    assert!(!jobs.cancel(&ta, &fe.job_id).await.unwrap());
    let page = jobs.events(&ta, &fe.job_id, 0, 50).await.unwrap().unwrap();
    assert!(!page.gap);
    let last = page.events.last().unwrap();
    assert_eq!(
        (
            last.seq,
            last.kind.as_str(),
            last.from_state.as_deref(),
            last.to_state.as_str()
        ),
        (3, "state", Some("running"), "succeeded")
    );
    assert_eq!(page.through, 3);
    // Another tenant cannot read it.
    assert!(jobs.events(&tb, &fe.job_id, 0, 50).await.unwrap().is_none());

    // --- B2-T1: per-tenant retention, interleaved tenants. -----------------
    set_event_retention(&pool, 4).await.unwrap();
    let fa = seed_running(&pool, &ta, "bjob-r9-ring-a", "builder-r").await;
    let fb = seed_running(&pool, &tb, "bjob-r9-ring-b", "builder-r").await;
    for _ in 0..3 {
        churn(&jobs, &fa, 2).await;
        churn(&jobs, &fb, 0).await;
    }
    churn(&jobs, &fb, 2).await;
    // A: the fe job's 3 + progress job's events + fa's 7: A's tenant ring
    // keeps exactly its newest 4, which are fa's 4..7.
    let a_all: Vec<(String, i64)> = sqlx::query_as(
        "SELECT job_id, job_event_seq FROM index_build_job_events
          WHERE tenant_id = $1 ORDER BY event_id",
    )
    .bind(&ta)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        a_all,
        (4..=7)
            .map(|s| ("bjob-r9-ring-a".to_string(), s))
            .collect::<Vec<_>>(),
        "A retains exactly its newest events"
    );
    assert_eq!(
        event_kinds(&pool, &tb, &fb.job_id).await.len(),
        3,
        "B, below the bound, keeps its whole history"
    );
    // Both jobs finish; their rows keep the final state whatever the ring
    // retained.
    assert!(jobs
        .complete_fenced(
            &ta,
            &fa.job_id,
            "builder-r",
            1,
            Ok(serde_json::json!({"a": 1})),
            None
        )
        .await
        .unwrap());
    assert!(jobs
        .complete_fenced(
            &tb,
            &fb.job_id,
            "builder-r",
            1,
            Err("b failed".into()),
            None
        )
        .await
        .unwrap());
    assert_eq!(
        jobs.get(&ta, &fe.job_id).await.unwrap().unwrap().state,
        "succeeded"
    );
    assert_eq!(
        jobs.get(&ta, &fa.job_id).await.unwrap().unwrap().state,
        "succeeded"
    );
    assert_eq!(
        jobs.get(&tb, &fb.job_id).await.unwrap().unwrap().state,
        "failed"
    );

    // --- B2-T2: paging across the retention boundary. ---------------------
    // fa now has events 5..8 retained (8 = succeeded).
    let p1 = jobs.events(&ta, &fa.job_id, 0, 2).await.unwrap().unwrap();
    assert!(p1.gap, "the first page starts after pruned history");
    assert_eq!(p1.events.iter().map(|e| e.seq).collect::<Vec<_>>(), [5, 6]);
    let p2 = jobs
        .events(&ta, &fa.job_id, p1.through, 2)
        .await
        .unwrap()
        .unwrap();
    assert!(!p2.gap, "a current cursor continues without a hole");
    assert_eq!(p2.events.iter().map(|e| e.seq).collect::<Vec<_>>(), [7, 8]);
    let p3 = jobs
        .events(&ta, &fa.job_id, p2.through, 2)
        .await
        .unwrap()
        .unwrap();
    assert!(
        p3.events.is_empty() && !p3.gap && p3.through == 8,
        "caught up"
    );
    // An expired cursor: its next events were pruned.
    let expired = jobs.events(&ta, &fa.job_id, 2, 2).await.unwrap().unwrap();
    assert!(expired.gap);
    // Pruning between page reads, with concurrent commits from another job
    // of the same tenant: the next page reports the gap, including when no
    // event of this job survives at all.
    let fc = seed_running(&pool, &ta, "bjob-r9-ring-c", "builder-r").await;
    let held = jobs.events(&ta, &fc.job_id, 0, 1).await.unwrap().unwrap();
    assert_eq!((held.events.len(), held.gap), (1, false));
    let fd = seed_running(&pool, &ta, "bjob-r9-ring-d", "builder-r").await;
    churn(&jobs, &fc, 1).await;
    churn(&jobs, &fd, 6).await;
    let after_prune = jobs
        .events(&ta, &fc.job_id, held.through, 10)
        .await
        .unwrap()
        .unwrap();
    assert!(
        after_prune.events.is_empty() && after_prune.gap,
        "every later event of this job was pruned: an explicit gap, not an empty success"
    );
    assert_eq!(after_prune.through, 2);
    for f in [&fc, &fd] {
        assert!(jobs.cancel(&ta, &f.job_id).await.unwrap());
    }

    set_event_retention(&pool, DEFAULT_EVENTS_PER_TENANT)
        .await
        .unwrap();
    assert!(set_event_retention(&pool, 0).await.is_err());
}
