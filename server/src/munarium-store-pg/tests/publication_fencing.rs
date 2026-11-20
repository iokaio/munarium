// SPDX-License-Identifier: Apache-2.0
//! ClearView R4 publication fencing primitives, against a real PostgreSQL:
//! the job-row lock, attempt links and publication intents, and their
//! serialization with retirement of the claim.
//!
//! Tenant-scoped throughout: it never calls the global `claim_any`. Seeded
//! running rows carry `claimed_at = 'infinity'`, so no claimer or
//! expired-claim cleaner can take them, and every row is made terminal before
//! the test returns. Skips loudly when `MUNARIUM_TEST_DATABASE_URL` is unset.

use std::time::Duration;

use munarium_store_pg::attempts::{BuildAttempts, IntentState};
use munarium_store_pg::jobs::{lock_generation, BuildJobs, JobFence};
use munarium_store_pg::PgStore;

fn url() -> Option<String> {
    std::env::var("MUNARIUM_TEST_DATABASE_URL").ok()
}

async fn seed(pool: &sqlx::PgPool, tenant: &str, node: &str) -> JobFence {
    let job_id = format!("bjob-r4pub-{}", uuid::Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO index_build_jobs
             (tenant_id, job_id, kind, index_version_id, state, claimed_by, claimed_at,
              attempts, requested_by)
         VALUES ($1, $2, 'rebuild', $3, 'running', $4, 'infinity', 1, 'test-r4pub')",
    )
    .bind(tenant)
    .bind(&job_id)
    .bind(format!("idx-{job_id}"))
    .bind(node)
    .execute(pool)
    .await
    .unwrap();
    JobFence {
        tenant_id: tenant.to_string(),
        job_id,
        node_id: node.to_string(),
        generation: 1,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fenced_writes_serialize_with_retirement_of_the_claim() {
    let Some(url) = url() else {
        eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
        return;
    };
    let tenant = format!("tenant-r4pub-store-{}", uuid::Uuid::new_v4().simple());
    let store = PgStore::connect(&url, &tenant).await.unwrap();
    let pool = store.pool().clone();
    let jobs = BuildJobs::new(pool.clone());
    let attempts = BuildAttempts::new(pool.clone(), &tenant);

    // --- Link and intent while current; an intent is immutable. ------------
    let fence = seed(&pool, &tenant, "node-a").await;
    let version = format!("idx-{}", fence.job_id);
    assert!(attempts
        .link_job(&fence, "att-r4pub-1", &version)
        .await
        .unwrap());
    let digest = "a".repeat(64);
    let key = format!("v1/t/collection/c/{version}/{digest}/manifest.json");
    assert!(attempts
        .commit_publication_intent(&fence, "att-r4pub-1", &version, &digest, &key, &digest)
        .await
        .unwrap());
    assert!(
        attempts
            .commit_publication_intent(&fence, "att-r4pub-1", &version, &digest, &key, &digest)
            .await
            .unwrap(),
        "re-committing the same intent is idempotent"
    );
    let other = "b".repeat(64);
    assert!(
        attempts
            .commit_publication_intent(&fence, "att-r4pub-1", &version, &other, &key, &other)
            .await
            .is_err(),
        "a second, different intent for one attempt is refused"
    );
    assert!(
        attempts
            .commit_publication_intent(
                &fence,
                "att-r4pub-unlinked",
                &version,
                &digest,
                &key,
                &digest
            )
            .await
            .is_err(),
        "an attempt not linked to this claim cannot record an intent"
    );
    let intent = attempts
        .publication_intent("att-r4pub-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            intent.state,
            intent.manifest_key.as_str(),
            intent.job_generation
        ),
        (IntentState::Pending, key.as_str(), 1)
    );

    // A fence never crosses tenants.
    let foreign = BuildAttempts::new(pool.clone(), format!("{tenant}-other"));
    assert!(foreign
        .link_job(&fence, "att-r4pub-x", &version)
        .await
        .is_err());

    // --- Cancel waits behind a held lock, then retires the claim. ----------
    let mut tx = pool.begin().await.unwrap();
    assert!(lock_generation(&mut tx, &fence).await.unwrap());
    let cancel = {
        let jobs = jobs.clone();
        let (tenant, job_id) = (tenant.clone(), fence.job_id.clone());
        tokio::spawn(async move { jobs.cancel(&tenant, &job_id).await })
    };
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !cancel.is_finished(),
        "cancel is an UPDATE of the locked row and must wait for the fenced write"
    );
    tx.commit().await.unwrap();
    assert!(cancel.await.unwrap().unwrap(), "then the cancel commits");
    assert!(
        !attempts
            .link_job(&fence, "att-r4pub-2", &version)
            .await
            .unwrap(),
        "a cancelled claim links nothing"
    );
    assert!(!attempts
        .commit_publication_intent(&fence, "att-r4pub-1", &version, &digest, &key, &digest)
        .await
        .unwrap());

    // The outcome of the authorized intent can still be recorded, once.
    assert!(attempts
        .resolve_publication_intent("att-r4pub-1", IntentState::Published, None)
        .await
        .unwrap());
    assert!(!attempts
        .resolve_publication_intent("att-r4pub-1", IntentState::Failed, Some("late"))
        .await
        .unwrap());

    // --- Completion and reclaim each retire the generation. ----------------
    let done = seed(&pool, &tenant, "node-a").await;
    assert!(jobs
        .complete_fenced(
            &tenant,
            &done.job_id,
            "node-a",
            1,
            Ok(serde_json::json!({})),
            None
        )
        .await
        .unwrap());
    assert!(!attempts
        .link_job(&done, "att-r4pub-3", "idx-done")
        .await
        .unwrap());

    let reclaimed = seed(&pool, &tenant, "node-a").await;
    sqlx::query(
        "UPDATE index_build_jobs SET attempts = attempts + 1
          WHERE tenant_id = $1 AND job_id = $2",
    )
    .bind(&tenant)
    .bind(&reclaimed.job_id)
    .execute(&pool)
    .await
    .unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert!(
        !lock_generation(&mut tx, &reclaimed).await.unwrap(),
        "same node name, older generation: not current"
    );
    assert!(lock_generation(
        &mut tx,
        &JobFence {
            generation: 2,
            ..reclaimed.clone()
        }
    )
    .await
    .unwrap());
    let other_node = JobFence {
        node_id: "node-b".into(),
        generation: 2,
        ..reclaimed.clone()
    };
    assert!(!lock_generation(&mut tx, &other_node).await.unwrap());
    tx.rollback().await.unwrap();
    assert!(jobs.cancel(&tenant, &reclaimed.job_id).await.unwrap());
}
