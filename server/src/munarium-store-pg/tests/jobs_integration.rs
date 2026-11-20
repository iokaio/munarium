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
//!
//! Owning the queue also means owning the DATABASE. Other suites (the
//! `munarium-server` binary tests, for one) leave pending jobs behind in the
//! shared test database, and a global `claim_any` would take them. So the
//! test creates its own throwaway database on the configured server, lets
//! `PgStore::connect` migrate it, and drops it afterwards through a guard
//! that also runs when an assertion fails. The assertions are unchanged: the
//! queue they see is exactly the queue the test built.

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

async fn jobs(database_url: &str, tenant: &str) -> (BuildJobs, sqlx::PgPool) {
    let store = PgStore::connect(database_url, tenant).await.unwrap();
    (BuildJobs::new(store.pool().clone()), store.pool().clone())
}

/// A throwaway database on the configured server, dropped when the guard
/// goes out of scope, including while a failed assertion unwinds.
struct ScratchDatabase {
    admin_url: String,
    name: String,
    url: String,
}

impl ScratchDatabase {
    async fn create(base_url: &str) -> Self {
        use sqlx::Connection;
        let name = format!("munarium_jobs_{}", uuid::Uuid::new_v4().simple());
        let mut admin = sqlx::PgConnection::connect(base_url).await.unwrap();
        sqlx::query(&format!("CREATE DATABASE \"{name}\""))
            .execute(&mut admin)
            .await
            .unwrap();
        admin.close().await.unwrap();
        Self {
            admin_url: base_url.to_string(),
            url: with_database(base_url, &name),
            name,
        }
    }
}

impl Drop for ScratchDatabase {
    fn drop(&mut self) {
        // Drop cannot await, and the test's runtime may be unwinding, so the
        // DROP runs on its own thread and runtime. FORCE ends the test pool's
        // remaining connections (PostgreSQL 13+).
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
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("could not drop scratch database {}: {e}", self.name),
            Err(_) => eprintln!("dropping scratch database {} panicked", self.name),
        }
    }
}

/// `url` with its database path replaced by `name`, keeping the scheme,
/// authority and query parameters.
fn with_database(url: &str, name: &str) -> String {
    let (head, query) = match url.split_once('?') {
        Some((head, query)) => (head, Some(query)),
        None => (url, None),
    };
    let authority = head.find("://").expect("a postgres:// URL") + 3;
    let base = match head[authority..].find('/') {
        Some(slash) => &head[..authority + slash],
        None => head,
    };
    match query {
        Some(query) => format!("{base}/{name}?{query}"),
        None => format!("{base}/{name}"),
    }
}

#[test]
fn with_database_replaces_only_the_path() {
    assert_eq!(
        with_database("postgres://u:p@h:5432/munarium?sslmode=disable", "x"),
        "postgres://u:p@h:5432/x?sslmode=disable"
    );
    assert_eq!(with_database("postgres://u@h", "x"), "postgres://u@h/x");
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
    let db = ScratchDatabase::create(&url().unwrap()).await;
    let (q, pool) = jobs(&db.url, &tenant).await;

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
    // --- R4 fence: a same-name reclaim supersedes the earlier claim. -------
    // A restarted replica keeps its node name, so `claimed_by` alone cannot
    // tell the two executions apart; the generation captured at claim can.
    let EnqueueOutcome::Enqueued(fence_id) = q
        .enqueue(
            &tenant,
            JobKind::Rebuild,
            JobTarget::Version("idx-fence"),
            serde_json::json!({}),
            "test",
            None,
        )
        .await
        .unwrap()
    else {
        panic!("enqueue");
    };
    let first = q.claim_any("builder-same", 0, 3).await.unwrap().unwrap();
    assert_eq!(
        (first.job_id.as_str(), first.attempts),
        (fence_id.as_str(), 1)
    );
    let second = q.claim_any("builder-same", 0, 3).await.unwrap().unwrap();
    assert_eq!(
        (second.job_id.as_str(), second.attempts),
        (fence_id.as_str(), 2),
        "the claim returns its own generation, captured in the claiming transaction"
    );
    assert!(
        !q.heartbeat_fenced(&tenant, &fence_id, "builder-same", first.attempts)
            .await
            .unwrap(),
        "the superseded claim cannot renew the lease"
    );
    assert!(
        !q.complete_fenced(
            &tenant,
            &fence_id,
            "builder-same",
            first.attempts,
            Ok(serde_json::json!({"stale": true})),
            Some("att-stale"),
        )
        .await
        .unwrap(),
        "the superseded claim cannot complete the job"
    );
    let still = q.get(&tenant, &fence_id).await.unwrap().unwrap();
    assert_eq!((still.state.as_str(), still.attempts), ("running", 2));
    assert!(still.result.is_none() && still.attempt_ids.is_empty());
    assert!(q
        .heartbeat_fenced(&tenant, &fence_id, "builder-same", second.attempts)
        .await
        .unwrap());
    assert!(q
        .complete_fenced(
            &tenant,
            &fence_id,
            "builder-same",
            second.attempts,
            Ok(serde_json::json!({"n": 2})),
            None,
        )
        .await
        .unwrap());
    assert_eq!(
        q.get(&tenant, &fence_id).await.unwrap().unwrap().result,
        Some(serde_json::json!({"n": 2}))
    );

    // --- R4 cleanup: an expired final claim fails once and frees its target.
    let EnqueueOutcome::Enqueued(exhaust_id) = q
        .enqueue(
            &tenant,
            JobKind::Rebuild,
            JobTarget::Version("idx-exhaust"),
            serde_json::json!({}),
            "test",
            None,
        )
        .await
        .unwrap()
    else {
        panic!("enqueue");
    };
    let mut last = None;
    for _ in 0..3 {
        last = q.claim_any("builder-crash", 0, 3).await.unwrap();
    }
    let last = last.expect("third claim");
    assert_eq!(
        (last.job_id.as_str(), last.attempts),
        (exhaust_id.as_str(), 3)
    );
    assert!(q.claim_any("builder-other", 0, 3).await.unwrap().is_none());
    // Two cleaners race with no other work; each job is failed exactly once.
    let (a, b) = tokio::join!(
        q.fail_expired_final_claims(0, 3, 32),
        q.fail_expired_final_claims(0, 3, 32)
    );
    let mine: Vec<_> = a
        .unwrap()
        .into_iter()
        .chain(b.unwrap())
        .filter(|(t, _)| t == &tenant)
        .collect();
    assert_eq!(mine, vec![(tenant.clone(), exhaust_id.clone())]);
    let failed = q.get(&tenant, &exhaust_id).await.unwrap().unwrap();
    assert_eq!(failed.state, "failed");
    assert_eq!(
        failed.error.as_deref(),
        Some(munarium_store_pg::jobs::ATTEMPTS_EXHAUSTED_MESSAGE)
    );
    // Late writes from the crashed holder are refused, fenced or not.
    assert!(!q
        .heartbeat_fenced(&tenant, &exhaust_id, "builder-crash", 3)
        .await
        .unwrap());
    assert!(!q
        .heartbeat(&tenant, &exhaust_id, "builder-crash")
        .await
        .unwrap());
    assert!(!q
        .complete_fenced(
            &tenant,
            &exhaust_id,
            "builder-crash",
            3,
            Ok(serde_json::json!({})),
            None
        )
        .await
        .unwrap());
    // The target is released: exactly one replacement can be enqueued.
    let replacement = q
        .enqueue(
            &tenant,
            JobKind::Rebuild,
            JobTarget::Version("idx-exhaust"),
            serde_json::json!({}),
            "test",
            None,
        )
        .await
        .unwrap();
    let EnqueueOutcome::Enqueued(replacement_id) = replacement else {
        panic!("the released target enqueues a replacement: {replacement:?}");
    };
    assert_eq!(
        q.enqueue(
            &tenant,
            JobKind::Rebuild,
            JobTarget::Version("idx-exhaust"),
            serde_json::json!({}),
            "test",
            None,
        )
        .await
        .unwrap(),
        EnqueueOutcome::AlreadyOpen(replacement_id.clone())
    );
    let r = q
        .claim_any("drain", 600, 3)
        .await
        .unwrap()
        .expect("replacement");
    assert_eq!(r.job_id, replacement_id);
    assert!(q
        .complete_fenced(
            &tenant,
            &r.job_id,
            "drain",
            r.attempts,
            Ok(serde_json::json!({})),
            None
        )
        .await
        .unwrap());

    // --- R4 cleanup races a valid renewal: exactly one transition wins. ----
    let EnqueueOutcome::Enqueued(race_id) = q
        .enqueue(
            &tenant,
            JobKind::Rebuild,
            JobTarget::Version("idx-race"),
            serde_json::json!({}),
            "test",
            None,
        )
        .await
        .unwrap()
    else {
        panic!("enqueue");
    };
    let mut held = None;
    for _ in 0..3 {
        held = q.claim_any("builder-race", 0, 3).await.unwrap();
    }
    let held = held.expect("final claim");
    assert_eq!((held.job_id.as_str(), held.attempts), (race_id.as_str(), 3));
    // Let the lease (1 s below) lapse, then renew and clean concurrently.
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let (renewed, cleaned) = tokio::join!(
        q.heartbeat_fenced(&tenant, &race_id, "builder-race", 3),
        q.fail_expired_final_claims(1, 3, 32)
    );
    let renewed = renewed.unwrap();
    let cleaned = cleaned
        .unwrap()
        .iter()
        .any(|(t, j)| t == &tenant && j == &race_id);
    let state = q.get(&tenant, &race_id).await.unwrap().unwrap().state;
    assert!(
        (renewed && !cleaned && state == "running") || (!renewed && cleaned && state == "failed"),
        "exactly one of renewal and cleanup takes effect: renewed={renewed} cleaned={cleaned} state={state}"
    );
    if renewed {
        assert!(q
            .complete_fenced(
                &tenant,
                &race_id,
                "builder-race",
                3,
                Ok(serde_json::json!({})),
                None
            )
            .await
            .unwrap());
    }

    // --- R6: cancel before claim, and the claim's run record. ------------
    // R6-T1 row 1: cancel wins before any claim. The v1.4 response and
    // state, no worker, no run record, and a recorded, empty history (the
    // job started no version). A repeated cancel is `false` and changes
    // nothing.
    let EnqueueOutcome::Enqueued(early_id) = q
        .enqueue(
            &tenant,
            JobKind::Backfill,
            scope("col-r6-early"),
            serde_json::json!({}),
            "test",
            None,
        )
        .await
        .unwrap()
    else {
        panic!("enqueue");
    };
    assert!(q.cancel(&tenant, &early_id).await.unwrap(), "ok: true");
    let early = q.get(&tenant, &early_id).await.unwrap().unwrap();
    assert_eq!(
        (
            early.state.as_str(),
            early.attempts,
            early.claimed_by.as_deref()
        ),
        ("cancelled", 0, None)
    );
    assert!(
        q.claim_any("builder-r6", 600, 3).await.unwrap().is_none(),
        "a cancelled job is never claimed"
    );
    let history = q
        .version_history(
            &tenant,
            &munarium_store_pg::jobs::JobId::parse(&early_id).unwrap(),
            None,
            50,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        history.history,
        munarium_store_pg::jobs::HistoryStatus::Recorded
    );
    assert!(history.versions.is_empty() && history.next_after.is_none());
    assert!(
        !q.cancel(&tenant, &early_id).await.unwrap(),
        "repeat: ok false"
    );
    assert_eq!(
        q.get(&tenant, &early_id).await.unwrap().unwrap().state,
        "cancelled"
    );
    // The target is free at once.
    let EnqueueOutcome::Enqueued(again_id) = q
        .enqueue(
            &tenant,
            JobKind::Backfill,
            scope("col-r6-early"),
            serde_json::json!({}),
            "test",
            None,
        )
        .await
        .unwrap()
    else {
        panic!("the cancelled target enqueues again at once");
    };
    // Every claim records its generation and node in the claiming
    // transaction, so the history of a fenced job is complete.
    let c1 = q.claim_any("builder-r6", 0, 3).await.unwrap().unwrap();
    let c2 = q.claim_any("builder-r6b", 0, 3).await.unwrap().unwrap();
    assert_eq!(
        (c1.job_id.as_str(), c2.job_id.as_str()),
        (again_id.as_str(), again_id.as_str())
    );
    let runs: Vec<(i32, String)> = sqlx::query_as(
        "SELECT job_generation, node_id FROM index_build_job_runs
          WHERE tenant_id = $1 AND job_id = $2 ORDER BY job_generation",
    )
    .bind(&tenant)
    .bind(&again_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        runs,
        vec![
            (1, "builder-r6".to_string()),
            (2, "builder-r6b".to_string())
        ]
    );
    let fence2 = munarium_store_pg::jobs::JobFence::of_claim(&c2, "builder-r6b");
    let fence1 = munarium_store_pg::jobs::JobFence::of_claim(&c1, "builder-r6");
    assert!(q.is_current(&fence2).await.unwrap());
    assert_eq!(
        q.stop_reason(&fence1).await.unwrap(),
        Some(munarium_store_pg::jobs::StopReason::LeaseLost),
        "a reclaimed generation reports lease loss"
    );
    assert!(q.cancel(&tenant, &again_id).await.unwrap());
    assert_eq!(
        q.stop_reason(&fence2).await.unwrap(),
        Some(munarium_store_pg::jobs::StopReason::Cancelled)
    );
    assert!(
        !q.complete_fenced(
            &tenant,
            &again_id,
            "builder-r6b",
            2,
            Ok(serde_json::json!({})),
            None
        )
        .await
        .unwrap(),
        "a cancelled job's holder cannot complete it"
    );

    // --- R8 / B1: fair claiming, and the v1.4 FIFO default. ---------------
    fair_claiming_phases(&q, &pool, &run).await;
}

async fn enqueue_n(q: &BuildJobs, tenant: &str, prefix: &str, n: usize) -> Vec<String> {
    let mut ids = Vec::new();
    for i in 0..n {
        match q
            .enqueue(
                tenant,
                JobKind::Rebuild,
                JobTarget::Version(&format!("iv-{prefix}-{i}")),
                serde_json::json!({}),
                "test",
                None,
            )
            .await
            .unwrap()
        {
            EnqueueOutcome::Enqueued(id) => ids.push(id),
            other => panic!("enqueue: {other:?}"),
        }
    }
    ids
}

async fn done(q: &BuildJobs, j: &munarium_store_pg::jobs::BuildJob, node: &str) {
    assert!(q
        .complete_fenced(
            &j.tenant_id,
            &j.job_id,
            node,
            j.attempts,
            Ok(serde_json::json!({})),
            None
        )
        .await
        .unwrap());
}

async fn last_served(pool: &sqlx::PgPool, tenant: &str) -> Option<i64> {
    sqlx::query_scalar("SELECT last_served_seq FROM index_build_tenant_turns WHERE tenant_id = $1")
        .bind(tenant)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// B1-T1 and B1-T2, in the single function that owns the global queue.
async fn fair_claiming_phases(q: &BuildJobs, pool: &sqlx::PgPool, run: &str) {
    use munarium_store_pg::jobs::ClaimPolicy::{Fifo, TenantFair};
    let a = format!("tenant-fair-a-{run}");
    let b = format!("tenant-fair-b-{run}");
    let c = format!("tenant-fair-c-{run}");

    // B1-T1: A has a much larger, older backlog; B and C three each.
    let a_ids = enqueue_n(q, &a, "a", 6).await;
    let b_ids = enqueue_n(q, &b, "b", 3).await;
    let c_ids = enqueue_n(q, &c, "c", 3).await;
    let mut order = Vec::new();
    for _ in 0..9 {
        let j = q
            .claim_next("builder-fair", 600, 3, TenantFair)
            .await
            .unwrap()
            .expect("eligible work");
        order.push((j.tenant_id.clone(), j.job_id.clone()));
        done(q, &j, "builder-fair").await;
    }
    let expected: Vec<(String, String)> = (0..3)
        .flat_map(|round| {
            [
                (a.clone(), a_ids[round].clone()),
                (b.clone(), b_ids[round].clone()),
                (c.clone(), c_ids[round].clone()),
            ]
        })
        .collect();
    assert_eq!(
        order, expected,
        "one claim per tenant per round, each tenant's own oldest first"
    );
    // Only A has work left: it is served, and its order is still FIFO.
    for id in &a_ids[3..] {
        let j = q
            .claim_next("builder-fair", 600, 3, TenantFair)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&j.job_id, id);
        done(q, &j, "builder-fair").await;
    }
    assert!(q
        .claim_next("builder-fair", 600, 3, TenantFair)
        .await
        .unwrap()
        .is_none());

    // B1-T2: competing builders never spend one tenant's turn twice.
    let a2 = enqueue_n(q, &a, "a2", 2).await;
    let b2 = enqueue_n(q, &b, "b2", 2).await;
    let (x, y) = tokio::join!(
        q.claim_next("builder-x", 600, 3, TenantFair),
        q.claim_next("builder-y", 600, 3, TenantFair)
    );
    let (x, y) = (x.unwrap(), y.unwrap());
    let claimed: Vec<_> = [&x, &y].iter().filter_map(|j| j.as_ref()).collect();
    assert!(
        !claimed.is_empty(),
        "at least one concurrent claim succeeds"
    );
    if claimed.len() == 2 {
        assert_ne!(
            claimed[0].tenant_id, claimed[1].tenant_id,
            "two concurrent claims took two different tenants' turns"
        );
    }
    for (j, node) in [(&x, "builder-x"), (&y, "builder-y")] {
        if let Some(j) = j {
            done(q, j, node).await;
        }
    }
    // Drain the rest of this phase in fair order.
    while let Some(j) = q
        .claim_next("builder-fair", 600, 3, TenantFair)
        .await
        .unwrap()
    {
        assert!(
            a2.contains(&j.job_id) || b2.contains(&j.job_id),
            "only this phase's jobs are eligible"
        );
        done(q, &j, "builder-fair").await;
    }

    // A locked candidate is passed over, and progress resumes when it clears.
    let a3 = enqueue_n(q, &a, "a3", 1).await;
    let b3 = enqueue_n(q, &b, "b3", 1).await;
    let mut holder = pool.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM index_build_tenant_turns WHERE tenant_id = $1 FOR UPDATE")
        .bind(&a)
        .execute(&mut *holder)
        .await
        .unwrap();
    let first = q
        .claim_next("builder-fair", 600, 3, TenantFair)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.job_id, b3[0], "A's turn row is locked; B goes");
    done(q, &first, "builder-fair").await;
    holder.rollback().await.unwrap();
    let second = q
        .claim_next("builder-fair", 600, 3, TenantFair)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.job_id, a3[0], "A is served once the lock clears");
    done(q, &second, "builder-fair").await;

    // A rolled-back claim leaves the turn available.
    let a4 = enqueue_n(q, &a, "a4", 1).await;
    let before = last_served(pool, &a).await;
    let mut rolled = pool.begin().await.unwrap();
    sqlx::query(
        "UPDATE index_build_jobs SET state = 'running', claimed_by = 'builder-gone',
                claimed_at = now(), attempts = attempts + 1
          WHERE tenant_id = $1 AND job_id = $2",
    )
    .bind(&a)
    .bind(&a4[0])
    .execute(&mut *rolled)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE index_build_tenant_turns
            SET last_served_seq = nextval('index_build_tenant_turn_seq')
          WHERE tenant_id = $1",
    )
    .bind(&a)
    .execute(&mut *rolled)
    .await
    .unwrap();
    rolled.rollback().await.unwrap();
    assert_eq!(
        last_served(pool, &a).await,
        before,
        "rollback kept the turn"
    );
    let next = q
        .claim_next("builder-fair", 600, 3, TenantFair)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (next.job_id.as_str(), next.attempts),
        (a4[0].as_str(), 1),
        "A still has its turn and the job its first generation"
    );
    // A reclaimed lease in fair mode: a new generation, the old one fenced.
    let reclaimed = q
        .claim_next("builder-fair2", 0, 3, TenantFair)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (reclaimed.job_id.as_str(), reclaimed.attempts),
        (a4[0].as_str(), 2),
        "the expired lease is reclaimed"
    );
    assert!(!q
        .heartbeat_fenced(&a, &a4[0], "builder-fair", 1)
        .await
        .unwrap());
    done(q, &reclaimed, "builder-fair2").await;

    // Expired-claim cleanup consumes no turn.
    let a5 = enqueue_n(q, &a, "a5", 1).await;
    let mut held = None;
    for _ in 0..3 {
        held = q.claim_next("builder-crash", 0, 3, Fifo).await.unwrap();
    }
    assert_eq!(held.unwrap().job_id, a5[0]);
    let before = last_served(pool, &a).await;
    let failed = q.fail_expired_final_claims(0, 3, 32).await.unwrap();
    assert!(failed.contains(&(a.clone(), a5[0].clone())));
    assert_eq!(last_served(pool, &a).await, before, "cleanup is not a turn");

    // Fair scheduling disabled: the v1.4 FIFO queue, oldest first across
    // tenants, never reading or advancing turns. The fixture's pending row
    // (created 2026-10-01) is older than anything enqueued now.
    let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/clearview-phase1/v14-build-jobs.json"
    )))
    .unwrap();
    let pending = fixture["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["case"] == "pending")
        .unwrap();
    let f_tenant = format!("tenant-fair-fixture-{run}");
    sqlx::query(
        "INSERT INTO index_build_jobs
             (tenant_id, job_id, kind, scope_kind, scope_id, params, state, attempts,
              requested_by, created_at)
         VALUES ($1,$2,$3,$4,$5,$6,'pending',0,$7,$8::timestamptz)",
    )
    .bind(&f_tenant)
    .bind(pending["job_id"].as_str().unwrap())
    .bind(pending["kind"].as_str().unwrap())
    .bind(pending["scope_kind"].as_str().unwrap())
    .bind(pending["scope_id"].as_str().unwrap())
    .bind(pending["params"].clone())
    .bind(pending["requested_by"].as_str().unwrap())
    .bind(pending["created_at"].as_str().unwrap())
    .execute(pool)
    .await
    .unwrap();
    let fa = enqueue_n(q, &a, "fifo-a", 3).await;
    let fb = enqueue_n(q, &b, "fifo-b", 1).await;
    let turns_before = (last_served(pool, &a).await, last_served(pool, &b).await);
    let mut fifo = Vec::new();
    while let Some(j) = q.claim_any("builder-fifo", 600, 3).await.unwrap() {
        fifo.push(j.job_id.clone());
        done(q, &j, "builder-fifo").await;
    }
    let mut expected = vec![pending["job_id"].as_str().unwrap().to_string()];
    expected.extend(fa);
    expected.extend(fb);
    assert_eq!(fifo, expected, "the v1.4 global FIFO order");
    assert_eq!(
        (last_served(pool, &a).await, last_served(pool, &b).await),
        turns_before,
        "FIFO mode never advances turns"
    );
}
