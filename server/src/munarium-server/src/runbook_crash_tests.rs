// SPDX-License-Identifier: Apache-2.0
//! Process death at real executor boundaries. Private executor re-entry is
//! explicit test orchestration, not a new public resume API or scheduler.
use super::*;
use crate::crash_recovery::{available, harness, state};

const BOOK: &str = "apiVersion: munarium.ioka.io/v1\nkind: Runbook\nmetadata: {name: recovery, version: 1}\nspec:\n  shape: recovery@1\n  steps:\n    - resolveSources: {}\n    - buildIndex: {}\n    - verify: {}\n    - cutover: {approval: required}\n    - retireOld: {keep_versions: 2}\n";

#[test]
fn runbook_process_recovery() {
    if !available() {
        return;
    }
    let mut phases = vec![
        "step_state:3:awaiting_approval".to_string(),
        "step_event:3:awaiting_approval".to_string(),
    ];
    for ordinal in 1..=4 {
        phases.extend([
            format!("step_effect:{ordinal}"),
            format!("step_state:{ordinal}:done"),
            format!("step_event:{ordinal}:done"),
        ]);
    }
    for phase in phases {
        for crash in [false, true] {
            harness::run(
                "runbooks_api::crash_tests::child",
                "runbook",
                &phase,
                crash,
                &format!("p08-runbook-{}", uuid::Uuid::new_v4().simple()),
            );
        }
    }
}

#[test]
#[ignore = "owned child fixture"]
fn child() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        tokio::time::timeout(harness::LIMIT, work())
            .await
            .expect("bounded runbook fixture");
    });
}

async fn work() {
    let tenant = harness::setting("P08_TENANT");
    let state = state(&tenant).await;
    let store = state.store_for(&tenant).await.unwrap();
    let retrieval = state.retrieval_for(&tenant).unwrap();
    let phase = harness::setting("P08_PHASE");
    let ordinal: usize = phase.split(':').nth(1).unwrap().parse().unwrap();
    let after_approval = ordinal >= 3 && !phase.ends_with("awaiting_approval");
    let mode = harness::setting("P08_MODE");
    if mode == "setup" {
        op_apply_shape(&state, &tenant,
            "apiVersion: munarium.ioka.io/v1\nkind: Shape\nmetadata: {name: recovery, version: 1}\nspec:\n  fact:\n    schema: {type: object}\n", None, None).await.unwrap();
        for edition in 0..2 {
            retrieval
                .put_source(
                    "",
                    "text/plain",
                    "recovery/story.txt",
                    Some("recovery@1"),
                    format!("Fictional obsolete edition {edition}.").as_bytes(),
                )
                .await
                .unwrap();
            let obsolete = retrieval
                .build_index("recovery@1", 2000, 0, false)
                .await
                .unwrap();
            if edition == 0 {
                std::fs::write(harness::dir().join("obsolete-index"), obsolete.id).unwrap();
            }
        }
        retrieval
            .put_source(
                "",
                "text/plain",
                "recovery/story.txt",
                Some("recovery@1"),
                b"The fictional library opens at noon.",
            )
            .await
            .unwrap();
        let old = retrieval
            .build_index("recovery@1", 2000, 0, true)
            .await
            .unwrap();
        std::fs::write(harness::dir().join("old-index"), old.id).unwrap();
        // The run must actually build/cut over new content, and retirement must
        // delete an obsolete generation; no-op effects are not crash evidence.
        retrieval
            .put_source(
                "",
                "text/plain",
                "recovery/story.txt",
                Some("recovery@1"),
                b"The fictional library now opens at dawn.",
            )
            .await
            .unwrap();
        let version = store.create_version(None, None).await.unwrap();
        std::fs::write(harness::dir().join("version"), &version).unwrap();
        op_apply_runbook(&state, &tenant, BOOK).await.unwrap();
        if after_approval {
            let (_, status) = op_run_runbook(&state, &tenant, "recovery@1", Some(&version))
                .await
                .unwrap();
            assert_eq!(status, "awaiting_approval");
        }
        return;
    }
    let version = std::fs::read_to_string(harness::dir().join("version")).unwrap();
    if mode == "write" {
        if after_approval {
            let run = run_id(&state, &tenant).await;
            assert_eq!(
                op_approve_step(&state, &tenant, &run, 3).await.unwrap(),
                "done"
            );
        } else {
            assert_eq!(
                op_run_runbook(&state, &tenant, "recovery@1", Some(&version))
                    .await
                    .unwrap()
                    .1,
                "awaiting_approval"
            );
        }
        std::fs::write(harness::dir().join("reply"), b"completed").unwrap();
        return;
    }
    let run = run_id(&state, &tenant).await;
    let snapshot = op_get_run(&state, &tenant, &run).await.unwrap();
    let interrupted = mode == "observe" || harness::setting("P08_CRASH") == "yes";
    let old = std::fs::read_to_string(harness::dir().join("old-index")).unwrap();
    let obsolete = std::fs::read_to_string(harness::dir().join("obsolete-index")).unwrap();
    let versions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM index_versions WHERE tenant_id=$1")
            .bind(&tenant)
            .fetch_one(pool(&state).unwrap())
            .await
            .unwrap();
    assert_eq!(
        versions, 4,
        "build effect created the new generation before its checkpoint"
    );
    if after_approval {
        let built = snapshot.steps[1].detail.as_ref().unwrap()["index_version"]
            .as_str()
            .unwrap();
        assert_ne!(built, old);
        assert_eq!(
            retrieval.index_version("recovery@1").await.unwrap().id,
            built,
            "cutover effect is already visible even before its done checkpoint"
        );
    }
    let obsolete_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM index_chunks WHERE tenant_id=$1 AND index_version_id=$2",
    )
    .bind(&tenant)
    .bind(&obsolete)
    .fetch_one(pool(&state).unwrap())
    .await
    .unwrap();
    assert_eq!(
        obsolete_rows,
        if ordinal == 4 || (!interrupted && after_approval) {
            0
        } else {
            1
        },
        "retirement really removes obsolete chunk data"
    );
    if interrupted {
        let expected = if phase.starts_with("step_effect") {
            "running"
        } else if phase.ends_with("awaiting_approval") {
            "awaiting_approval"
        } else {
            "done"
        };
        assert_eq!(snapshot.steps[ordinal].state, expected);
        let key = format!("step-{ordinal}-{}-{expected}", snapshot.steps[ordinal].name);
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM claims WHERE tenant_id=$1 AND version_id=$2 AND key=$3",
        )
        .bind(&tenant)
        .bind(&version)
        .bind(&key)
        .fetch_one(pool(&state).unwrap())
        .await
        .unwrap();
        assert_eq!(
            count,
            if phase.starts_with("step_state") {
                0
            } else {
                1
            },
            "state/event gap characterization"
        );
    }
    if mode == "observe" {
        assert!(
            matches!(
                acquire_run_lock(&state, &tenant, &run).await,
                Err(KernelError::InvalidInput(_))
            ),
            "live executor owns the advisory lock"
        );
        return;
    }
    // Reopen alone does not advance steps. No scheduler is implied.
    assert_eq!(
        serde_json::to_value(op_get_run(&state, &tenant, &run).await.unwrap()).unwrap(),
        serde_json::to_value(&snapshot).unwrap()
    );
    retrieval.index_version_by_id(&old).await.unwrap();
    if !after_approval {
        assert_eq!(
            retrieval.index_version("recovery@1").await.unwrap().id,
            old,
            "unapproved run cannot activate an index"
        );
    }
    let doc = load_runbook(&state, &tenant, "recovery@1").await.unwrap();
    let next = {
        let _lock = acquire_run_lock(&state, &tenant, &run)
            .await
            .expect("dead process released lock");
        execute(&state, &tenant, &run, &doc, Some(&version), None)
            .await
            .unwrap()
    };
    if snapshot.steps[3].state != "done" {
        assert_eq!(
            next, "awaiting_approval",
            "explicit re-entry does not imply approval"
        );
        assert_eq!(
            op_approve_step(&state, &tenant, &run, 3).await.unwrap(),
            "done"
        );
    } else {
        assert_eq!(next, "done");
    }
    let final_run = op_get_run(&state, &tenant, &run).await.unwrap();
    assert!(final_run.steps.iter().all(|s| s.state == "done"));
    let built = final_run.steps[1].detail.as_ref().unwrap()["index_version"]
        .as_str()
        .unwrap();
    assert_eq!(
        retrieval.index_version("recovery@1").await.unwrap().id,
        built
    );
    let versions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM index_versions WHERE tenant_id=$1")
            .bind(&tenant)
            .fetch_one(pool(&state).unwrap())
            .await
            .unwrap();
    assert_eq!(
        versions, 4,
        "build re-entry reuses the same content generation"
    );
    let pinned_text: String = sqlx::query_scalar(
        "SELECT text FROM index_chunks WHERE tenant_id=$1 AND index_version_id=$2",
    )
    .bind(&tenant)
    .bind(&old)
    .fetch_one(pool(&state).unwrap())
    .await
    .unwrap();
    assert_eq!(
        pinned_text, "The fictional library opens at noon.",
        "previous generation remains within the retention window"
    );
    // Re-entry skips done steps. A missing done transition remains missing;
    // do not manufacture a recovery contract by repairing it inside the test.
    if interrupted && phase.starts_with("step_state") && phase.ends_with(":done") {
        let key = format!("step-{ordinal}-{}-done", snapshot.steps[ordinal].name);
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM claims WHERE tenant_id=$1 AND version_id=$2 AND key=$3",
        )
        .bind(&tenant)
        .bind(&version)
        .bind(key)
        .fetch_one(pool(&state).unwrap())
        .await
        .unwrap();
        assert_eq!(
            count, 0,
            "re-entry does not reconstruct missing transition history"
        );
    }
}

async fn run_id(state: &AppState, tenant: &str) -> String {
    sqlx::query_scalar("SELECT id FROM runbook_runs WHERE tenant_id=$1")
        .bind(tenant)
        .fetch_one(pool(state).unwrap())
        .await
        .unwrap()
}
