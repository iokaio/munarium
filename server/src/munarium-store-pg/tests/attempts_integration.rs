// SPDX-License-Identifier: Apache-2.0
//! Build-attempt leases against a real PostgreSQL.
//!
//! Single-flight and lease expiry are claims about a database and a clock, so
//! testing them anywhere else would test something else. Skips loudly when
//! `MUNARIUM_TEST_DATABASE_URL` is unset.

use munarium_store_pg::attempts::{
    reconcile_sealed, AttemptMode, AttemptState, BuildAttempts, ClaimOutcome, SealedVerdict,
};
use munarium_store_pg::PgStore;

fn url() -> Option<String> {
    std::env::var("MUNARIUM_TEST_DATABASE_URL").ok()
}

async fn attempts(tenant: &str) -> BuildAttempts {
    let url = url().expect("guarded by the caller");
    let store = PgStore::connect(&url, tenant).await.unwrap();
    BuildAttempts::new(store.pool().clone(), tenant)
}

fn unique(p: &str) -> String {
    format!("{p}-{}", uuid::Uuid::new_v4().simple())
}

const PLAN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

macro_rules! guard {
    () => {
        if url().is_none() {
            eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
            return;
        }
    };
}

/// The rule the partial unique index enforces: one running attempt per
/// (tenant, version, plan). Two nodes racing cannot both win, and the loser
/// learns WHO holds it rather than getting an opaque failure.
#[tokio::test]
async fn two_nodes_racing_one_plan_produce_one_claim() {
    guard!();
    let a = attempts("tenant-att-a").await;
    let version = unique("idx2");

    let first = a
        .claim(
            &version,
            PLAN,
            AttemptMode::Mirror,
            "node-a",
            Some("/tmp/a"),
        )
        .await
        .unwrap();
    let attempt_id = match first {
        ClaimOutcome::Claimed(id) => id,
        other => panic!("the first claim must win, got {other:?}"),
    };

    match a
        .claim(
            &version,
            PLAN,
            AttemptMode::Mirror,
            "node-b",
            Some("/tmp/b"),
        )
        .await
        .unwrap()
    {
        ClaimOutcome::AlreadyRunning { owner_node_id } => {
            assert_eq!(owner_node_id, "node-a", "the loser must learn who holds it")
        }
        other => panic!("the second claim must lose, got {other:?}"),
    }

    // A DIFFERENT plan for the same version is a different build and must not
    // be blocked -- that is how an engine upgrade proceeds while a mirror runs.
    let other_plan = "f".repeat(64);
    assert!(matches!(
        a.claim(&version, &other_plan, AttemptMode::Direct, "node-b", None)
            .await
            .unwrap(),
        ClaimOutcome::Claimed(_)
    ));

    a.mark_sealed(&attempt_id, &"a".repeat(64)).await.unwrap();
    a.mark_succeeded(&attempt_id).await.unwrap();

    // With the first attempt finished, the plan is claimable again: the index
    // is about concurrent work, not about how many times something was built.
    assert!(matches!(
        a.claim(&version, PLAN, AttemptMode::Mirror, "node-c", None)
            .await
            .unwrap(),
        ClaimOutcome::Claimed(_)
    ));
}

/// An expired lease is reclaimable. Without this a dead node would hold that
/// plan's single-flight slot forever, and the only recovery would be a manual
/// row delete.
#[tokio::test]
async fn an_expired_lease_is_reclaimed_by_the_next_claimant() {
    guard!();
    let url = url().unwrap();
    let store = PgStore::connect(&url, "tenant-att-b").await.unwrap();
    // A lease that has already elapsed by the time the next claim arrives.
    let a = BuildAttempts::new(store.pool().clone(), "tenant-att-b").with_lease_secs(1);
    let version = unique("idx2");

    let held = match a
        .claim(&version, PLAN, AttemptMode::Mirror, "dead-node", None)
        .await
        .unwrap()
    {
        ClaimOutcome::Claimed(id) => id,
        other => panic!("{other:?}"),
    };

    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;

    match a
        .claim(&version, PLAN, AttemptMode::Mirror, "live-node", None)
        .await
        .unwrap()
    {
        ClaimOutcome::Claimed(_) => {}
        other => panic!("an expired lease must be reclaimable, got {other:?}"),
    }

    // The reclaimed attempt is recorded as expired rather than deleted: an
    // attempt that was superseded reads differently from one that never was.
    let old = a.get(&held).await.unwrap().expect("the row survives");
    assert_eq!(old.state, AttemptState::Expired);
}

/// A heartbeat from the owner extends the lease; a heartbeat for an attempt
/// that is no longer this node's returns false. A builder ignoring that would
/// keep working on an attempt someone else has taken over -- two builders
/// publishing one plan is exactly what the lease prevents.
#[tokio::test]
async fn a_heartbeat_extends_only_the_owners_lease() {
    guard!();
    let a = attempts("tenant-att-c").await;
    let version = unique("idx2");
    let id = match a
        .claim(&version, PLAN, AttemptMode::Mirror, "node-a", None)
        .await
        .unwrap()
    {
        ClaimOutcome::Claimed(id) => id,
        other => panic!("{other:?}"),
    };

    assert!(a.heartbeat(&id, "node-a").await.unwrap());
    assert!(
        !a.heartbeat(&id, "node-b").await.unwrap(),
        "another node must not be able to extend a lease it does not hold"
    );

    a.mark_cancelled(&id).await.unwrap();
    assert!(
        !a.heartbeat(&id, "node-a").await.unwrap(),
        "a terminal attempt has no lease to extend"
    );
}

/// A terminal attempt cannot move again. Silently allowing it would hide two
/// code paths believing they own the same build.
#[tokio::test]
async fn a_terminal_attempt_cannot_transition_again() {
    guard!();
    let a = attempts("tenant-att-d").await;
    let version = unique("idx2");
    let id = match a
        .claim(&version, PLAN, AttemptMode::Mirror, "node-a", None)
        .await
        .unwrap()
    {
        ClaimOutcome::Claimed(id) => id,
        other => panic!("{other:?}"),
    };

    a.mark_failed(&id, "export_failed", "the source read failed")
        .await
        .unwrap();
    assert!(a.mark_succeeded(&id).await.is_err());
    assert!(a.mark_converged(&id, &"a".repeat(64)).await.is_err());
}

/// `converged` is a distinct terminal state. A rebuild that found an identical
/// artifact did the right thing, and counting it as a failure would make a
/// healthy deployment look unhealthy on every dashboard.
#[tokio::test]
async fn converged_is_recorded_separately_from_failed() {
    guard!();
    let a = attempts("tenant-att-e").await;
    let version = unique("idx2");
    let id = match a
        .claim(&version, PLAN, AttemptMode::Backfill, "node-a", None)
        .await
        .unwrap()
    {
        ClaimOutcome::Claimed(id) => id,
        other => panic!("{other:?}"),
    };
    a.mark_converged(&id, &"c".repeat(64)).await.unwrap();
    let row = a.get(&id).await.unwrap().unwrap();
    assert_eq!(row.state, AttemptState::Converged);
    assert_eq!(row.artifact_id.as_deref(), Some("c".repeat(64).as_str()));
}

/// A sealed attempt is mid-flight, not finished, and the reconciler decides its
/// fate from what it can observe. A `sealed` row is never read as "L2 exists".
#[tokio::test]
async fn a_sealed_attempt_is_listed_for_reconciliation_and_judged_by_the_rule() {
    guard!();
    let a = attempts("tenant-att-f").await;
    let version = unique("idx2");
    let id = match a
        .claim(
            &version,
            PLAN,
            AttemptMode::Mirror,
            "node-a",
            Some("/tmp/x"),
        )
        .await
        .unwrap()
    {
        ClaimOutcome::Claimed(id) => id,
        other => panic!("{other:?}"),
    };
    a.mark_sealed(&id, &"b".repeat(64)).await.unwrap();

    let pending = a.sealed_awaiting_publication().await.unwrap();
    let row = pending
        .iter()
        .find(|r| r.attempt_id == id)
        .expect("a sealed attempt awaits publication");
    assert!(!row.lease_expired, "freshly sealed, so the lease is live");

    assert_eq!(
        reconcile_sealed(row, "node-a", true),
        SealedVerdict::Resume,
        "ours, fresh, with its staging directory present"
    );
    assert_eq!(
        reconcile_sealed(row, "node-a", false),
        SealedVerdict::Abandon {
            reason: "staging_lost"
        },
        "the content is gone, so there is nothing to resume"
    );

    // expire_stale must NOT touch a sealed attempt: its fate needs an
    // observation this query cannot make.
    a.expire_stale().await.unwrap();
    assert_eq!(
        a.get(&id).await.unwrap().unwrap().state,
        AttemptState::Sealed
    );
}

/// Failure detail is bounded. This column is rendered by an admin page and
/// copied into every backup, so an unbounded error string would be a slow leak
/// of whatever the failure happened to contain.
#[tokio::test]
async fn failure_detail_is_truncated() {
    guard!();
    let a = attempts("tenant-att-g").await;
    let version = unique("idx2");
    let id = match a
        .claim(&version, PLAN, AttemptMode::Mirror, "node-a", None)
        .await
        .unwrap()
    {
        ClaimOutcome::Claimed(id) => id,
        other => panic!("{other:?}"),
    };
    a.mark_failed(&id, "export_failed", &"x".repeat(5_000))
        .await
        .unwrap();
    let stored: String = sqlx::query_scalar(
        "SELECT failure_detail FROM index_build_attempts WHERE tenant_id = $1 AND attempt_id = $2",
    )
    .bind("tenant-att-g")
    .bind(&id)
    .fetch_one(
        PgStore::connect(&url().unwrap(), "tenant-att-g")
            .await
            .unwrap()
            .pool(),
    )
    .await
    .unwrap();
    assert_eq!(stored.len(), 500);
}
