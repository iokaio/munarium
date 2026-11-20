// SPDX-License-Identifier: Apache-2.0
//! ClearView C2-T1: REST `get` and `list` of build jobs over a real
//! PostgreSQL, with a seeded historical failure, a v1.4 builder's failure
//! written during rollout, new typed failures recorded through the builder's
//! own classification, an expired final claim failed by R4 cleanup, a success
//! and a pending job. Checks exact member presence, each code against the
//! error registry, and that stored rows are left intact.
//!
//! Tenant-scoped rows only. The expired-claim cleanup is global, so its row is
//! two years old and the cleanup's lease is one year: no other test's claim
//! is old enough to be touched. Gated on `MUNARIUM_TEST_DATABASE_URL`; skips
//! loudly when it is unset.

use std::collections::BTreeSet;

use axum::body::{to_bytes, Body};
use axum::http::Request;
use munarium_store_pg::job_failure::{JobFailureCode, GENERIC_FAILURE_MESSAGE};
use munarium_store_pg::jobs::ATTEMPTS_EXHAUSTED_MESSAGE;
use serde_json::Value;
use tower::ServiceExt;

use super::*;

const FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/clearview-phase1/v14-build-jobs.json"
));
const REGISTRY: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/api/errors.md"
));

/// The codes the registry's "Build-job failure codes" table lists.
fn registered_codes() -> BTreeSet<String> {
    let section = REGISTRY
        .split("\n## Build-job failure codes")
        .nth(1)
        .expect("errors.md has a Build-job failure codes section");
    let section = section.split("\n## ").next().unwrap();
    section
        .lines()
        .filter_map(|l| l.strip_prefix("| `"))
        .filter_map(|l| l.split('`').next())
        .map(str::to_owned)
        .collect()
}

async fn get(router: &axum::Router, path: &str) -> (u16, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(path)
                .header("authorization", "Bearer reader")
                .header("x-munarium-uid", "c2-t1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn members(v: &Value) -> BTreeSet<&str> {
    v.as_object().unwrap().keys().map(String::as_str).collect()
}

#[tokio::test]
async fn c2_t1_get_and_list_return_safe_messages_and_registered_codes() {
    let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
        eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
        return;
    };
    let tenant = format!("tenant-c2-t1-{}", uuid::Uuid::new_v4().simple());
    let state = crate::providers_api::usage_tests::test_state_with_auth(
        Some(url),
        crate::config::AuthMode::Static(vec![("reader".into(), tenant.clone(), "ro".into())]),
    )
    .await;
    let pool = state.pg_pool().unwrap().clone();
    let jobs = BuildJobs::new(pool.clone());
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let fixture_error = |case: &str| -> String {
        fixture["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["case"] == case)
            .unwrap()["error"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let driver_text = fixture_error("failed_historical_storage_text");
    let v14_text = fixture_error("failed_v14_writer_free_text");

    // Seed a row as a claim of `node` would leave it. `age` is how long ago
    // the claim was made, as a PostgreSQL interval.
    let seed = |job_id: &'static str,
                kind: &'static str,
                state: &'static str,
                attempts: i32,
                age: &'static str,
                error: Option<String>| {
        let pool = pool.clone();
        let tenant = tenant.clone();
        async move {
            sqlx::query(
                "INSERT INTO index_build_jobs
                     (tenant_id, job_id, kind, index_version_id, state, claimed_by,
                      claimed_at, attempts, error, requested_by)
                 VALUES ($1, $2, $3, 'iv-' || $2, $4,
                         CASE WHEN $4 = 'pending' THEN NULL ELSE 'builder-c2' END,
                         CASE WHEN $4 = 'pending' THEN NULL ELSE now() - $5::interval END,
                         $6, $7, 'operator-c2')",
            )
            .bind(&tenant)
            .bind(job_id)
            .bind(kind)
            .bind(state)
            .bind(age)
            .bind(attempts)
            .bind(error)
            .execute(&pool)
            .await
            .unwrap();
        }
    };

    // 1. Historical failure: raw driver text, written before migration 0047.
    seed(
        "bjob-c2-historical",
        "rebuild",
        "failed",
        1,
        "2 hours",
        Some(driver_text.clone()),
    )
    .await;

    // 2. A v1.4 builder's failure during rollout: v1.4's exact completion
    //    statement, which does not know the error_code column.
    seed(
        "bjob-c2-v14-writer",
        "rebuild",
        "running",
        1,
        "1 second",
        None,
    )
    .await;
    let v14 = sqlx::query(
        "UPDATE index_build_jobs
            SET state = $4, result = $5, error = $6,
                attempt_ids = CASE WHEN $7::text IS NULL THEN attempt_ids
                                   ELSE array_append(attempt_ids, $7) END,
                updated_at = now()
          WHERE tenant_id = $1 AND job_id = $2
            AND state = 'running' AND claimed_by = $3",
    )
    .bind(&tenant)
    .bind("bjob-c2-v14-writer")
    .bind("builder-c2")
    .bind("failed")
    .bind(None::<Value>)
    .bind(&v14_text)
    .bind(None::<String>)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(v14.rows_affected(), 1);

    // 3. New typed failures through the builder's own path: execute, then
    //    record with the call site's classification.
    seed("bjob-c2-typed", "backfill", "running", 1, "1 second", None).await;
    let job = jobs.get(&tenant, "bjob-c2-typed").await.unwrap().unwrap();
    let fence = JobFence::of_claim(&job, "builder-c2");
    let err = run_one_job(&state, &job, Some(&fence), None)
        .await
        .expect_err("a backfill without a scope fails");
    assert!(matches!(err, KernelError::InvalidInput(_)), "{err}");
    let cause_text = err.to_string();
    assert!(jobs
        .complete_fenced_classified(
            &tenant,
            "bjob-c2-typed",
            "builder-c2",
            fence.generation,
            Err(job_failure(&job, fence.generation, &err)),
            None,
        )
        .await
        .unwrap());

    seed("bjob-c2-storage", "rebuild", "running", 1, "1 second", None).await;
    let job = jobs.get(&tenant, "bjob-c2-storage").await.unwrap().unwrap();
    let storage = KernelError::Storage(driver_text.clone());
    assert!(jobs
        .complete_fenced_classified(
            &tenant,
            "bjob-c2-storage",
            "builder-c2",
            1,
            Err(job_failure(&job, 1, &storage)),
            None,
        )
        .await
        .unwrap());

    // 4. An expired final claim, failed by R4 cleanup.
    seed("bjob-c2-expired", "rebuild", "running", 3, "2 years", None).await;
    let year = 365 * 24 * 3600;
    let mut cleaned = false;
    for _ in 0..64 {
        let failed = jobs
            .fail_expired_final_claims(year, MAX_JOB_ATTEMPTS, 32)
            .await
            .unwrap();
        cleaned |= failed
            .iter()
            .any(|(t, j)| t == &tenant && j == "bjob-c2-expired");
        if cleaned || failed.is_empty() {
            break;
        }
    }
    assert!(cleaned, "cleanup failed the expired final claim");

    // 5. Success, and a pending job.
    seed("bjob-c2-ok", "rebuild", "running", 1, "1 second", None).await;
    assert!(jobs
        .complete_fenced_classified(
            &tenant,
            "bjob-c2-ok",
            "builder-c2",
            1,
            Ok(serde_json::json!({"index_version_id": "iv-c2-t1"})),
            None,
        )
        .await
        .unwrap());
    seed(
        "bjob-c2-pending",
        "rebuild",
        "pending",
        0,
        "0 seconds",
        None,
    )
    .await;

    // Read everything over REST and from the store, then remove this
    // tenant's rows before any assertion.
    let router = crate::rest::router(state.clone());
    let ids = [
        "bjob-c2-historical",
        "bjob-c2-v14-writer",
        "bjob-c2-typed",
        "bjob-c2-storage",
        "bjob-c2-expired",
        "bjob-c2-ok",
        "bjob-c2-pending",
    ];
    let mut got = std::collections::BTreeMap::new();
    let mut stored = std::collections::BTreeMap::new();
    for id in ids {
        got.insert(
            id,
            get(&router, &format!("/v1/index-build-jobs/{id}")).await,
        );
        stored.insert(id, jobs.get(&tenant, id).await.unwrap().unwrap());
    }
    let (list_status, list) = get(&router, "/v1/index-build-jobs").await;
    sqlx::query("DELETE FROM index_build_jobs WHERE tenant_id = $1")
        .bind(&tenant)
        .execute(&pool)
        .await
        .unwrap();

    assert_eq!(list_status, 200, "{list}");
    let listed: std::collections::BTreeMap<&str, &Value> = list["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| (j["job_id"].as_str().unwrap(), j))
        .collect();
    assert_eq!(listed.len(), ids.len());
    let registry = registered_codes();
    let base: BTreeSet<&str> = ["job_id", "kind", "index_version_id", "state", "attempts"]
        .into_iter()
        .collect();

    for id in ids {
        let (status, dto) = &got[id];
        assert_eq!(*status, 200, "{id}: {dto}");
        assert_eq!(&dto, &listed[id], "{id}: get and list agree");
        let mut want: BTreeSet<&str> = base.iter().copied().collect();
        want.insert("created_at");
        let (state, error, code): (&str, Option<&str>, Option<&str>) = match id {
            "bjob-c2-historical" | "bjob-c2-v14-writer" => {
                ("failed", Some(GENERIC_FAILURE_MESSAGE), None)
            }
            "bjob-c2-typed" => (
                "failed",
                Some(JobFailureCode::InvalidInput.message()),
                Some("invalid_input"),
            ),
            "bjob-c2-storage" => (
                "failed",
                Some(JobFailureCode::StorageError.message()),
                Some("storage_error"),
            ),
            "bjob-c2-expired" => (
                "failed",
                Some(ATTEMPTS_EXHAUSTED_MESSAGE),
                Some("attempts_exhausted"),
            ),
            "bjob-c2-ok" => ("succeeded", None, None),
            "bjob-c2-pending" => ("pending", None, None),
            _ => unreachable!(),
        };
        if state != "pending" {
            want.insert("claimed_by");
        }
        if state == "succeeded" {
            want.insert("result");
        }
        if error.is_some() {
            want.insert("error");
        }
        if code.is_some() {
            want.insert("error_code");
        }
        assert_eq!(members(dto), want, "{id}: exact members of {dto}");
        assert_eq!(dto["state"], state, "{id}");
        assert_eq!(dto["error"].as_str(), error, "{id}");
        assert_eq!(dto["error_code"].as_str(), code, "{id}");
        if let Some(code) = code {
            assert!(registry.contains(code), "{id}: {code} is not registered");
            assert!(
                JobFailureCode::ALL.iter().any(|c| c.as_str() == code),
                "{id}: {code} is not a typed code"
            );
        }
        // Nothing a client sees carries the cause's own text.
        let text = dto.to_string();
        assert!(!text.contains("index_versions_pkey"), "{id}: {text}");
        assert!(!text.contains(&v14_text), "{id}: {text}");
        assert!(!text.contains(&cause_text), "{id}: {text}");
    }

    // The fixture's recorded v1.5 expectation for its expired final claim.
    let after = fixture["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["case"] == "running_expired_final_claim")
        .unwrap()["expected_v15_after_cleanup"]
        .as_object()
        .unwrap();
    for (k, v) in after {
        assert_eq!(&got["bjob-c2-expired"].1[k], v, "expired claim: {k}");
    }

    // Stored rows are intact: historical and rollout text stays in the
    // table, with no code; new failures store only the safe message.
    assert_eq!(
        stored["bjob-c2-historical"].error,
        Some(driver_text.clone())
    );
    assert_eq!(stored["bjob-c2-v14-writer"].error, Some(v14_text.clone()));
    for id in ["bjob-c2-historical", "bjob-c2-v14-writer", "bjob-c2-ok"] {
        assert_eq!(stored[id].error_code, None, "{id}");
    }
    assert_eq!(
        stored["bjob-c2-storage"].error.as_deref(),
        Some(JobFailureCode::StorageError.message())
    );
    assert_eq!(stored["bjob-c2-ok"].error, None);
}
