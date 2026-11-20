// SPDX-License-Identifier: Apache-2.0
//! ClearView C2 at the store: the `error_code` column (migration 0047).
//!
//! A classified failure stores its code with its safe message; the
//! v1.4-shaped `complete` API and a success store none; and the column
//! refuses a code outside the registered syntax. The REST read rule and the
//! expired-claim cleanup are exercised by `munarium-server`'s C2-T1 test.
//!
//! Tenant-scoped rows that are never claimable by another test's global
//! `claim_any` (their claims are fresh and below no ceiling a test uses for
//! an expired lease). Skips loudly when `MUNARIUM_TEST_DATABASE_URL` is unset.

use munarium_core::KernelError;
use munarium_store_pg::job_failure::{classify, JobFailure, JobFailureCode, CODE_MAX_LEN};
use munarium_store_pg::jobs::BuildJobs;
use munarium_store_pg::PgStore;

async fn seed_running(pool: &sqlx::PgPool, tenant: &str, job_id: &str) {
    sqlx::query(
        "INSERT INTO index_build_jobs
             (tenant_id, job_id, kind, index_version_id, state, claimed_by, claimed_at,
              attempts, requested_by)
         VALUES ($1, $2, 'rebuild', 'iv-' || $2, 'running', 'builder-c2', 'infinity', 1, 't')",
    )
    .bind(tenant)
    .bind(job_id)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn error_codes_are_stored_only_for_classified_failures() {
    let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
        eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
        return;
    };
    let tenant = format!("tenant-c2-store-{}", uuid::Uuid::new_v4().simple());
    let store = PgStore::connect(&url, &tenant).await.unwrap();
    let pool = store.pool().clone();
    let jobs = BuildJobs::new(pool.clone());

    for id in [
        "c2-classified",
        "c2-unclassified",
        "c2-legacy",
        "c2-ok",
        "c2-bad",
    ] {
        seed_running(&pool, &tenant, id).await;
    }
    let classified = classify(&KernelError::Storage(
        "duplicate key value violates unique constraint \"index_versions_pkey\"".into(),
    ));
    let recorded = [
        jobs.complete_fenced_classified(
            &tenant,
            "c2-classified",
            "builder-c2",
            1,
            Err(classified),
            None,
        )
        .await
        .unwrap(),
        jobs.complete_fenced_classified(
            &tenant,
            "c2-unclassified",
            "builder-c2",
            1,
            Err(classify(&KernelError::Forbidden("role".into()))),
            None,
        )
        .await
        .unwrap(),
        jobs.complete_fenced(
            &tenant,
            "c2-legacy",
            "builder-c2",
            1,
            Err("legacy caller text".into()),
            None,
        )
        .await
        .unwrap(),
        jobs.complete_fenced_classified(
            &tenant,
            "c2-ok",
            "builder-c2",
            1,
            Ok(serde_json::json!({})),
            None,
        )
        .await
        .unwrap(),
    ];
    // A code outside the syntax is refused by the column, so nothing a
    // future caller does can store one.
    let too_long = "a".repeat(CODE_MAX_LEN + 1);
    let mut refused = Vec::new();
    for bad in [
        "Storage_Error",
        "storage-error",
        "1code",
        "",
        too_long.as_str(),
    ] {
        let r = sqlx::query(
            "UPDATE index_build_jobs SET error_code = $3 WHERE tenant_id = $1 AND job_id = $2",
        )
        .bind(&tenant)
        .bind("c2-bad")
        .bind(bad)
        .execute(&pool)
        .await;
        refused.push((bad.to_owned(), r.is_err()));
    }
    let longest_ok = sqlx::query(
        "UPDATE index_build_jobs SET error_code = $3 WHERE tenant_id = $1 AND job_id = $2",
    )
    .bind(&tenant)
    .bind("c2-bad")
    .bind("a".repeat(CODE_MAX_LEN))
    .execute(&pool)
    .await;

    let read = |id: &'static str| {
        let jobs = jobs.clone();
        let tenant = tenant.clone();
        async move { jobs.get(&tenant, id).await.unwrap().unwrap() }
    };
    let rows = [
        read("c2-classified").await,
        read("c2-unclassified").await,
        read("c2-legacy").await,
        read("c2-ok").await,
    ];
    let listed = jobs.list(&tenant, 50).await.unwrap();
    sqlx::query("DELETE FROM index_build_jobs WHERE tenant_id = $1")
        .bind(&tenant)
        .execute(&pool)
        .await
        .unwrap();

    assert_eq!(recorded, [true; 4]);
    let [classified, unclassified, legacy, ok] = rows;
    assert_eq!(classified.state, "failed");
    assert_eq!(classified.error_code.as_deref(), Some("storage_error"));
    assert_eq!(
        classified.error.as_deref(),
        Some(JobFailureCode::StorageError.message())
    );
    assert_eq!(unclassified.error_code, None);
    assert_eq!(
        unclassified.error,
        Some(JobFailure::unclassified().message),
        "an unclassified cause stores the generic message, not its text"
    );
    assert_eq!(
        (legacy.error.as_deref(), legacy.error_code.as_deref()),
        (Some("legacy caller text"), None)
    );
    assert_eq!(
        (ok.state.as_str(), ok.error, ok.error_code),
        ("succeeded", None, None)
    );
    let in_list = listed.iter().find(|j| j.job_id == "c2-classified").unwrap();
    assert_eq!(in_list.error_code.as_deref(), Some("storage_error"));
    for (bad, was_refused) in refused {
        assert!(was_refused, "the column accepted {bad:?}");
    }
    assert!(longest_ok.is_ok(), "{longest_ok:?}");
}
