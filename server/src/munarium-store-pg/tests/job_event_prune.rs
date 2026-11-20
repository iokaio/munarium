// SPDX-License-Identifier: Apache-2.0
//! Migration 0086: the build-job event ring's prune never waits on event rows
//! another transaction is already pruning, against a real PostgreSQL.
//!
//! The retention count is database-wide, so this runs in its own scratch
//! database (dropped with FORCE when the guard goes out of scope) and cannot
//! disturb another suite's ring or claim. Skips loudly when
//! `MUNARIUM_TEST_DATABASE_URL` is unset.

use munarium_store_pg::jobs::set_event_retention;
use munarium_store_pg::PgStore;

/// A throwaway database, dropped (with FORCE) on drop.
struct ScratchDatabase {
    admin_url: String,
    name: String,
    url: String,
}

impl ScratchDatabase {
    async fn create(base_url: &str) -> Self {
        use sqlx::Connection;
        let name = format!("munarium_prune_{}", uuid::Uuid::new_v4().simple());
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

const TENANT: &str = "tenant-prune";

/// Seed a running job no claimer or cleaner can take (`claimed_at` is
/// `infinity`); its insert records one event.
async fn seed(pool: &sqlx::PgPool, job_id: &str) {
    sqlx::query(
        "INSERT INTO index_build_jobs
             (tenant_id, job_id, kind, index_version_id, state, claimed_by, claimed_at,
              attempts, requested_by)
         VALUES ($1,$2,'rebuild',$2,'running','builder-p','infinity',1,'test-0086')",
    )
    .bind(TENANT)
    .bind(job_id)
    .execute(pool)
    .await
    .unwrap();
}

/// One meaningful transition (a progress phase change), which records one
/// event and prunes in the same transaction.
const ADVANCE: &str = "UPDATE index_build_jobs
    SET progress_phase = CASE WHEN progress_phase = 'sealing' THEN 'publishing'
                              ELSE 'sealing' END
  WHERE tenant_id = $1 AND job_id = $2";

async fn ring(pool: &sqlx::PgPool) -> Vec<i64> {
    sqlx::query_scalar(
        "SELECT event_id FROM index_build_job_events WHERE tenant_id = $1 ORDER BY event_id",
    )
    .bind(TENANT)
    .fetch_all(pool)
    .await
    .unwrap()
}

/// Two transactions prune the same tenant's ring at once. The first holds
/// the oldest row it is deleting; the second must not wait for it (before
/// 0086 it blocked, and two prunes that met shared rows in different orders
/// could deadlock). Once both commit, the ring is exactly what 0046 left in
/// the same interleaving, and the next event trims it back to the bound.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_prune_does_not_wait_on_rows_a_concurrent_prune_holds() {
    let Ok(base) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
        eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
        return;
    };
    let db = ScratchDatabase::create(&base).await;
    let store = PgStore::connect(&db.url, TENANT).await.unwrap();
    let pool = store.pool().clone();
    set_event_retention(&pool, 3).await.unwrap();
    seed(&pool, "bjob-prune-a").await;
    seed(&pool, "bjob-prune-b").await;
    for _ in 0..3 {
        sqlx::query(ADVANCE)
            .bind(TENANT)
            .bind("bjob-prune-a")
            .execute(&pool)
            .await
            .unwrap();
    }
    let before = ring(&pool).await;
    assert_eq!(before.len(), 3, "the ring is at its bound");

    // T1: a transition on job A prunes the oldest event and holds its lock.
    let mut t1 = pool.begin().await.unwrap();
    sqlx::query(ADVANCE)
        .bind(TENANT)
        .bind("bjob-prune-a")
        .execute(&mut *t1)
        .await
        .unwrap();

    // T2: a transition on job B prunes the same tenant meanwhile.
    let mut t2 = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL lock_timeout = '3s'")
        .execute(&mut *t2)
        .await
        .unwrap();
    let advanced = sqlx::query(ADVANCE)
        .bind(TENANT)
        .bind("bjob-prune-b")
        .execute(&mut *t2)
        .await;
    assert!(
        advanced.is_ok(),
        "the second prune waited on rows the first holds: {advanced:?}"
    );
    t2.commit().await.unwrap();
    t1.commit().await.unwrap();

    // The oldest row went with T1; T2 skipped it. Its own event and T1's
    // are both kept: one over the bound, as 0046 left this interleaving.
    let after = ring(&pool).await;
    assert_eq!(after.len(), 4, "{after:?}");
    assert!(!after.contains(&before[0]), "the oldest event was pruned");
    assert_eq!(&after[..2], &before[1..], "the newest are never deleted");

    // The next event prunes back to exactly the bound, newest kept.
    sqlx::query(ADVANCE)
        .bind(TENANT)
        .bind("bjob-prune-b")
        .execute(&pool)
        .await
        .unwrap();
    let trimmed = ring(&pool).await;
    assert_eq!(trimmed.len(), 3);
    assert_eq!(&trimmed[..2], &after[2..]);

    // A prune that rolls back leaves its rows for the next one.
    let mut t3 = pool.begin().await.unwrap();
    sqlx::query(ADVANCE)
        .bind(TENANT)
        .bind("bjob-prune-a")
        .execute(&mut *t3)
        .await
        .unwrap();
    t3.rollback().await.unwrap();
    assert_eq!(ring(&pool).await, trimmed, "nothing changed");

    pool.close().await;
    drop(store);
    drop(db);
}
