// SPDX-License-Identifier: Apache-2.0
//! ClearView Phase 1 baseline: the v1.4 build-job fixture, seeded into a real
//! PostgreSQL and read back through the v1.4 job store.
//!
//! The fixture (`server/tests/fixtures/clearview-phase1/v14-build-jobs.json`)
//! records stored rows and what v1.4.0 does with them. This test pins the
//! store half: every row reads back field for field, `list` orders newest
//! first, and the three R4 defects the plan names are observable. When R4
//! lands, the `changes_under: R4` observations flip deliberately and this
//! test is updated with the fence, never weakened to pass.
//!
//! Tenant-scoped throughout: it never calls the global `claim_any`, so it
//! cannot take another test's queued jobs. Skips loudly when
//! `MUNARIUM_TEST_DATABASE_URL` is unset.

use munarium_store_pg::jobs::{BuildJob, BuildJobs, EnqueueOutcome, JobKind, JobTarget};
use munarium_store_pg::PgStore;

const FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/clearview-phase1/v14-build-jobs.json"
));

fn url() -> Option<String> {
    std::env::var("MUNARIUM_TEST_DATABASE_URL").ok()
}

fn s(v: &serde_json::Value) -> Option<String> {
    v.as_str().map(str::to_owned)
}

async fn seed(pool: &sqlx::PgPool, tenant: &str, row: &serde_json::Value) {
    let attempt_ids: Vec<String> = row["attempt_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap().to_owned())
        .collect();
    let result = (!row["result"].is_null()).then(|| row["result"].clone());
    sqlx::query(
        "INSERT INTO index_build_jobs
            (tenant_id, job_id, kind, scope_kind, scope_id, index_version_id, params,
             state, attempt_ids, correlation_id, claimed_by, claimed_at, attempts,
             result, error, requested_by, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11,
                 CASE WHEN $12::float8 IS NULL THEN NULL
                      ELSE now() - make_interval(secs => $12::float8) END,
                 $13, $14, $15, $16, $17::timestamptz, now())",
    )
    .bind(tenant)
    .bind(s(&row["job_id"]))
    .bind(s(&row["kind"]))
    .bind(s(&row["scope_kind"]))
    .bind(s(&row["scope_id"]))
    .bind(s(&row["index_version_id"]))
    .bind(row["params"].clone())
    .bind(s(&row["state"]))
    .bind(attempt_ids)
    .bind(s(&row["correlation_id"]))
    .bind(s(&row["claimed_by"]))
    .bind(row["claimed_age_secs"].as_f64())
    .bind(row["attempts"].as_i64().unwrap() as i32)
    .bind(result)
    .bind(s(&row["error"]))
    .bind(s(&row["requested_by"]))
    .bind(s(&row["created_at"]))
    .execute(pool)
    .await
    .unwrap();
}

/// created_at is text in the session's zone; compare the instant, so the
/// test does not depend on (or alter) the server's TimeZone setting.
async fn same_instant(pool: &sqlx::PgPool, got: &str, want: &str) -> bool {
    sqlx::query_scalar("SELECT $1::timestamptz = $2::timestamptz")
        .bind(got)
        .bind(want)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn assert_row(job: &BuildJob, row: &serde_json::Value) {
    let case = &row["case"];
    assert_eq!(Some(job.job_id.clone()), s(&row["job_id"]), "{case}");
    assert_eq!(Some(job.kind.clone()), s(&row["kind"]), "{case}");
    assert_eq!(job.scope_kind, s(&row["scope_kind"]), "{case}");
    assert_eq!(job.scope_id, s(&row["scope_id"]), "{case}");
    assert_eq!(job.index_version_id, s(&row["index_version_id"]), "{case}");
    assert_eq!(job.params, row["params"], "{case}");
    assert_eq!(Some(job.state.clone()), s(&row["state"]), "{case}");
    let ids: Vec<serde_json::Value> = job.attempt_ids.iter().map(|a| a.as_str().into()).collect();
    assert_eq!(serde_json::Value::Array(ids), row["attempt_ids"], "{case}");
    assert_eq!(job.correlation_id, s(&row["correlation_id"]), "{case}");
    assert_eq!(job.claimed_by, s(&row["claimed_by"]), "{case}");
    assert_eq!(
        i64::from(job.attempts),
        row["attempts"].as_i64().unwrap(),
        "{case}"
    );
    assert_eq!(
        job.result.clone().unwrap_or(serde_json::Value::Null),
        row["result"],
        "{case}"
    );
    assert_eq!(
        job.error,
        s(&row["error"]),
        "{case}: the stored error is read back verbatim"
    );
    // C2: a row written before migration 0047 has no code, and the store
    // never derives one from its text. The REST view of these rows is the
    // fixture's `expected_v15_dto` (checked in munarium-server).
    assert_eq!(job.error_code, None, "{case}: no stored error_code");
    assert_eq!(
        Some(job.requested_by.clone()),
        s(&row["requested_by"]),
        "{case}"
    );
}

#[tokio::test]
async fn v14_job_fixture_reads_back_and_shows_the_r4_baseline() {
    if url().is_none() {
        eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
        return;
    }
    let fixture: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
    let run = uuid::Uuid::new_v4().simple().to_string();
    let tenant = format!("tenant-v14-fixture-{run}");
    let store = PgStore::connect(&url().unwrap(), &tenant).await.unwrap();
    let pool = store.pool().clone();
    let jobs = BuildJobs::new(pool.clone());

    let rows = fixture["rows"].as_array().unwrap();
    for row in rows {
        seed(&pool, &tenant, row).await;
    }

    // Collect everything first, then remove this tenant's rows before any
    // assertion: the pending and live-lease rows are claimable, and a
    // leftover would be taken by another test's global claim_any.
    let mut read_back = Vec::new();
    for row in rows {
        let job = jobs
            .get(&tenant, row["job_id"].as_str().unwrap())
            .await
            .unwrap();
        let instant_ok = match &job {
            Some(j) => {
                same_instant(&pool, &j.created_at, row["created_at"].as_str().unwrap()).await
            }
            None => false,
        };
        read_back.push((job, instant_ok));
    }
    let listed = jobs.list(&tenant, 50).await.unwrap();

    // R4 baseline: the expired final claim still holds its open target.
    let held = jobs
        .enqueue(
            &tenant,
            JobKind::Rebuild,
            JobTarget::Version("iv-fixture-expired"),
            serde_json::json!({}),
            "operator-fixture",
            None,
        )
        .await
        .unwrap();
    // R4: the fenced heartbeat refuses a stale generation on the same row
    // and node name that the v1.4 heartbeat accepts.
    let fenced_stale = jobs
        .heartbeat_fenced(&tenant, "job-v14-running-expired-final", "builder-a", 2)
        .await
        .unwrap();
    // R4 baseline: a replica reusing the node name renews a lapsed final
    // claim it never held. Last, because it moves claimed_at.
    let renewed = jobs
        .heartbeat(&tenant, "job-v14-running-expired-final", "builder-a")
        .await
        .unwrap();

    sqlx::query("DELETE FROM index_build_jobs WHERE tenant_id = $1")
        .bind(&tenant)
        .execute(&pool)
        .await
        .unwrap();

    for (row, (job, instant_ok)) in rows.iter().zip(&read_back) {
        let job = job.as_ref().expect("seeded row is readable");
        assert_row(job, row);
        assert!(
            instant_ok,
            "{}: created_at {} differs from the fixture",
            row["case"], job.created_at
        );
    }
    let order: Vec<&str> = listed.iter().map(|j| j.job_id.as_str()).collect();
    let expected: Vec<&str> = fixture["expected_v14_list_order"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(order, expected);
    assert_eq!(
        held,
        EnqueueOutcome::AlreadyOpen("job-v14-running-expired-final".into()),
        "v1.4 baseline: an expired final claim occupies the open-target index"
    );
    assert!(
        !fenced_stale,
        "R4: a stale generation cannot renew the lease under the same node name"
    );
    assert!(
        renewed,
        "v1.4 baseline: a same-name unfenced heartbeat renews the lapsed final claim"
    );
}
