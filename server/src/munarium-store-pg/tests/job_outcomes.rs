// SPDX-License-Identifier: Apache-2.0
//! ClearView R6: a job's per-version history distinguishes "recorded and
//! empty" from "never recorded", against a real PostgreSQL.
//!
//! The v1.4 fixture's terminal rows (`server/tests/fixtures/clearview-phase1/
//! v14-build-jobs.json`) were claimed by a v1.4 builder, which kept no run
//! records, so their history must read as unavailable, never as an empty page
//! meaning "it did nothing". Rows are seeded tenant-scoped and only in
//! terminal states, so no claimer can take them. Skips loudly when
//! `MUNARIUM_TEST_DATABASE_URL` is unset.

use munarium_store_pg::jobs::{BuildJobs, HistoryStatus, JobId, VersionCursor};
use munarium_store_pg::PgStore;

const FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/clearview-phase1/v14-build-jobs.json"
));

fn url() -> Option<String> {
    std::env::var("MUNARIUM_TEST_DATABASE_URL").ok()
}

async fn seed_terminal(
    pool: &sqlx::PgPool,
    tenant: &str,
    job_id: &str,
    state: &str,
    attempts: i32,
) {
    sqlx::query(
        "INSERT INTO index_build_jobs
             (tenant_id, job_id, kind, scope_kind, scope_id, params, state,
              claimed_by, claimed_at, attempts, requested_by)
         VALUES ($1,$2,'backfill','collection','col-r6-history','{}'::jsonb,$3,
                 'builder-a', now(), $4, 'operator-fixture')",
    )
    .bind(tenant)
    .bind(job_id)
    .bind(state)
    .bind(attempts)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn a_v14_job_reports_history_unavailable_never_an_empty_success() {
    if url().is_none() {
        eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
        return;
    }
    let fixture: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
    let tenant = format!("tenant-r6-history-{}", uuid::Uuid::new_v4().simple());
    let store = PgStore::connect(&url().unwrap(), &tenant).await.unwrap();
    let pool = store.pool().clone();
    let jobs = BuildJobs::new(pool.clone());

    // The fixture's terminal, claimed rows, exactly as v1.4 stored them.
    let terminal: Vec<&serde_json::Value> = fixture["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| {
            matches!(
                r["state"].as_str(),
                Some("succeeded" | "failed" | "cancelled")
            ) && r["attempts"].as_i64().unwrap() > 0
        })
        .collect();
    assert!(terminal.len() >= 3, "fixture lost its terminal rows");
    for row in &terminal {
        seed_terminal(
            &pool,
            &tenant,
            row["job_id"].as_str().unwrap(),
            row["state"].as_str().unwrap(),
            row["attempts"].as_i64().unwrap() as i32,
        )
        .await;
    }
    for row in &terminal {
        let id = row["job_id"].as_str().unwrap();
        let h = jobs
            .version_history(&tenant, &JobId::parse(id).unwrap(), None, 50)
            .await
            .unwrap()
            .expect("the tenant's job");
        assert_eq!(
            h.history,
            HistoryStatus::Unavailable,
            "{}: a v1.4 job's empty page is unknown, not nothing",
            row["case"]
        );
        assert!(h.versions.is_empty());
        assert_eq!(
            h.job.state,
            row["state"].as_str().unwrap(),
            "state unchanged"
        );
    }

    // Partial: generation 1 by a v1.4 builder, generation 2 by a fenced one.
    seed_terminal(&pool, &tenant, "bjob-r6-partial", "failed", 2).await;
    sqlx::query(
        "INSERT INTO index_build_job_runs (tenant_id, job_id, job_generation, node_id)
         VALUES ($1, 'bjob-r6-partial', 2, 'builder-b')",
    )
    .bind(&tenant)
    .execute(&pool)
    .await
    .unwrap();
    let h = jobs
        .version_history(&tenant, &JobId::parse("bjob-r6-partial").unwrap(), None, 50)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(h.history, HistoryStatus::Partial);

    // Another tenant cannot read it, even knowing the id.
    assert!(jobs
        .version_history(
            "tenant-r6-someone-else",
            &JobId::parse("bjob-r6-partial").unwrap(),
            None,
            50
        )
        .await
        .unwrap()
        .is_none());

    // Recorded: every generation has its run record. Paging is by version id
    // and each page reports where the next one starts.
    seed_terminal(&pool, &tenant, "bjob-r6-recorded", "cancelled", 1).await;
    sqlx::query(
        "INSERT INTO index_build_job_runs (tenant_id, job_id, job_generation, node_id)
         VALUES ($1, 'bjob-r6-recorded', 1, 'builder-b')",
    )
    .bind(&tenant)
    .execute(&pool)
    .await
    .unwrap();
    for (i, v) in ["iv-r6-a", "iv-r6-b", "iv-r6-c"].iter().enumerate() {
        sqlx::query(
            "INSERT INTO index_build_job_versions
                 (tenant_id, job_id, index_version_id, ordinal, job_generation)
             VALUES ($1, 'bjob-r6-recorded', $2, $3, 1)",
        )
        .bind(&tenant)
        .bind(v)
        .bind(i as i32)
        .execute(&pool)
        .await
        .unwrap();
    }
    let first = jobs
        .version_history(&tenant, &JobId::parse("bjob-r6-recorded").unwrap(), None, 2)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.history, HistoryStatus::Recorded);
    let ids: Vec<_> = first
        .versions
        .iter()
        .map(|v| v.index_version_id.as_str())
        .collect();
    assert_eq!(ids, ["iv-r6-a", "iv-r6-b"]);
    assert_eq!(first.next_after.as_deref(), Some("iv-r6-b"));
    let second = jobs
        .version_history(
            &tenant,
            &JobId::parse("bjob-r6-recorded").unwrap(),
            first
                .next_after
                .as_deref()
                .map(|v| VersionCursor::parse(v).unwrap())
                .as_ref(),
            2,
        )
        .await
        .unwrap()
        .unwrap();
    let ids: Vec<_> = second
        .versions
        .iter()
        .map(|v| v.index_version_id.as_str())
        .collect();
    assert_eq!(ids, ["iv-r6-c"]);
    assert!(second.next_after.is_none());
    // An untouched selected version: no attempt, no intent, nothing bound.
    let c = &second.versions[0];
    assert_eq!(
        (
            c.selected_ordinal,
            c.attempt_count,
            c.intent_state.as_deref(),
            c.staged_bound
        ),
        (Some(2), 0, None, false)
    );
}
