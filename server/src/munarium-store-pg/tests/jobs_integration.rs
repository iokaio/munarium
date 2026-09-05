// SPDX-License-Identifier: Apache-2.0
//! The durable build-job queue (§8.6), against a real PostgreSQL.
//!
//! The claims under test are concurrency claims — dedup under the partial
//! unique index, SKIP LOCKED claiming, lease reclaim, idempotent completion —
//! so nothing is mocked. Skips loudly when `MUNARIUM_TEST_DATABASE_URL` is
//! unset.
//!
//! ONE test function, phases in order. The queue is GLOBAL by design — a
//! builder serves every tenant — so parallel test functions would claim each
//! other's jobs, which is the same lesson the Matrix tree's conformance
//! suite learned from its own global queue: the test must own the whole
//! queue for its lifetime, or accept any claimant. Owning it is the
//! deterministic choice here.

use munarium_store_pg::jobs::{BuildJobs, EnqueueOutcome, JobKind, JobTarget};
use munarium_store_pg::PgStore;

fn url() -> Option<String> {
    std::env::var("MUNARIUM_TEST_DATABASE_URL").ok()
}

macro_rules! guard {
    () => {
        if url().is_none() {
            eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
            return;
        }
    };
}

async fn jobs(tenant: &str) -> BuildJobs {
    let store = PgStore::connect(&url().unwrap(), tenant).await.unwrap();
    BuildJobs::new(store.pool().clone())
}

fn scope<'a>(id: &'a str) -> JobTarget<'a> {
    JobTarget::Scope {
        scope_kind: "collection",
        scope_id: id,
    }
}

#[tokio::test]
async fn the_job_queue_dedups_claims_leases_and_cancels() {
    guard!();
    let run = uuid::Uuid::new_v4().simple().to_string();
    let tenant = format!("tenant-jobs-{run}");
    let q = jobs(&tenant).await;

    // --- Dedup: two enqueues for one open target are one job. -------------
    let col_dedup = format!("col-dedup-{run}");
    let first = q
        .enqueue(
            &tenant,
            JobKind::Direct,
            scope(&col_dedup),
            serde_json::json!({}),
            "test",
            None,
        )
        .await
        .unwrap();
    let EnqueueOutcome::Enqueued(first_id) = first else {
        panic!("first enqueue creates: {first:?}");
    };
    let second = q
        .enqueue(
            &tenant,
            JobKind::Direct,
            scope(&col_dedup),
            serde_json::json!({}),
            "test",
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        second,
        EnqueueOutcome::AlreadyOpen(first_id.clone()),
        "the open job is the answer, not an error and not a duplicate"
    );
    // A DIFFERENT kind for the same scope is its own job.
    let EnqueueOutcome::Enqueued(backfill_id) = q
        .enqueue(
            &tenant,
            JobKind::Backfill,
            scope(&col_dedup),
            serde_json::json!({}),
            "test",
            None,
        )
        .await
        .unwrap()
    else {
        panic!("a different kind enqueues its own job");
    };
    // Drain both so later phases see a queue they own.
    for _ in 0..2 {
        let j = q
            .claim_any("drain", 600, 3)
            .await
            .unwrap()
            .expect("pending");
        assert!(q
            .complete(
                &j.tenant_id,
                &j.job_id,
                "drain",
                Ok(serde_json::json!({})),
                None
            )
            .await
            .unwrap());
    }
    assert!(q.get(&tenant, &backfill_id).await.unwrap().unwrap().state == "succeeded");

    // --- Claim/complete: holder-only, idempotent. -------------------------
    let EnqueueOutcome::Enqueued(job_id) = q
        .enqueue(
            &tenant,
            JobKind::Rebuild,
            JobTarget::Version("idx-claim"),
            serde_json::json!({}),
            "test",
            None,
        )
        .await
        .unwrap()
    else {
        panic!("enqueue");
    };
    let claimed = q
        .claim_any("builder-1", 600, 3)
        .await
        .unwrap()
        .expect("one pending job");
    assert_eq!(claimed.job_id, job_id);
    assert_eq!(claimed.state, "running");
    assert_eq!(claimed.attempts, 1);

    assert!(
        !q.complete(
            &tenant,
            &job_id,
            "builder-2",
            Ok(serde_json::json!({"n": 1})),
            None
        )
        .await
        .unwrap(),
        "a non-holder's completion is a no-op, never a clobber"
    );
    assert!(q
        .complete(
            &tenant,
            &job_id,
            "builder-1",
            Ok(serde_json::json!({"n": 1})),
            Some("attempt-xyz")
        )
        .await
        .unwrap());
    let done = q.get(&tenant, &job_id).await.unwrap().unwrap();
    assert_eq!(done.state, "succeeded");
    assert_eq!(done.attempt_ids, vec!["attempt-xyz".to_string()]);
    assert!(
        !q.complete(&tenant, &job_id, "builder-1", Err("late".into()), None)
            .await
            .unwrap(),
        "completing twice is idempotent"
    );

    // --- Lease: a lapsed running job is re-offered, to a ceiling. ---------
    let EnqueueOutcome::Enqueued(lease_id) = q
        .enqueue(
            &tenant,
            JobKind::Rebuild,
            JobTarget::Version("idx-lease"),
            serde_json::json!({}),
            "test",
            None,
        )
        .await
        .unwrap()
    else {
        panic!("enqueue");
    };
    q.claim_any("builder-dead", 0, 3).await.unwrap().unwrap();
    let reclaimed = q
        .claim_any("builder-2", 0, 3)
        .await
        .unwrap()
        .expect("the lapsed job is offered again");
    assert_eq!(reclaimed.job_id, lease_id);
    assert_eq!(reclaimed.attempts, 2);
    assert_eq!(reclaimed.claimed_by.as_deref(), Some("builder-2"));
    q.claim_any("builder-3", 0, 3).await.unwrap().unwrap();
    assert!(
        q.claim_any("builder-4", 0, 3).await.unwrap().is_none(),
        "attempts=3 is the ceiling; the job stays for an operator, not a retry storm"
    );
    // Close it out so the cancel phase owns the queue.
    assert!(q
        .complete(
            &tenant,
            &lease_id,
            "builder-3",
            Err("poisonous".into()),
            None
        )
        .await
        .unwrap());

    // --- Cancel: closes an open job and frees its dedup slot. -------------
    let col_cancel = format!("col-cancel-{run}");
    let EnqueueOutcome::Enqueued(cancel_id) = q
        .enqueue(
            &tenant,
            JobKind::Direct,
            scope(&col_cancel),
            serde_json::json!({}),
            "test",
            None,
        )
        .await
        .unwrap()
    else {
        panic!("enqueue");
    };
    assert!(q.cancel(&tenant, &cancel_id).await.unwrap());
    assert!(!q.cancel(&tenant, &cancel_id).await.unwrap(), "idempotent");
    assert_eq!(
        q.get(&tenant, &cancel_id).await.unwrap().unwrap().state,
        "cancelled"
    );
    assert!(
        matches!(
            q.enqueue(
                &tenant,
                JobKind::Direct,
                scope(&col_cancel),
                serde_json::json!({}),
                "test",
                None,
            )
            .await
            .unwrap(),
            EnqueueOutcome::Enqueued(_)
        ),
        "cancelled is not open; the target is enqueueable again"
    );
    // Leave nothing pending for other test binaries.
    let j = q
        .claim_any("drain", 600, 3)
        .await
        .unwrap()
        .expect("pending");
    q.complete(
        &j.tenant_id,
        &j.job_id,
        "drain",
        Ok(serde_json::json!({})),
        None,
    )
    .await
    .unwrap();
}
