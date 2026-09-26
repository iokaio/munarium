// SPDX-License-Identifier: Apache-2.0
//! Process death at real executor boundaries. Private executor re-entry is
//! explicit test orchestration, not a new public resume API or scheduler.
use super::*;
use crate::crash_recovery::{available, harness, state};

async fn acquire_released_run_lock(state: &AppState, tenant: &str, run: &str) -> RunLock {
    // A completed API call or dead child has dropped its guard, but PostgreSQL
    // may still be processing the socket close. Retry only that contention;
    // database errors and a lock that remains held must still fail the test.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match acquire_run_lock(state, tenant, run).await {
                Ok(lock) => return lock,
                Err(KernelError::InvalidInput(message))
                    if message.starts_with(crate::error::RUN_LOCKED_PREFIX) =>
                {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                Err(error) => panic!("run lock acquisition failed: {error}"),
            }
        }
    })
    .await
    .expect("completed executor must release its run lock")
}

async fn drop_run_lock_and_wait(state: &AppState, mut lock: RunLock) {
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut lock._conn)
        .await
        .unwrap();
    drop(lock);
    // Closing the client socket does not synchronously release the server's
    // session lock. Observe its release before starting the next test executor;
    // do not unlock it ourselves or hide a lock that survives connection loss.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let held: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_locks WHERE pid=$1 AND locktype='advisory')",
            )
            .bind(pid)
            .fetch_one(pool(state).unwrap())
            .await
            .unwrap();
            if !held {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("dropped run connection must release its advisory lock");
}

const BOOK: &str = "apiVersion: munarium.ioka.io/v1\nkind: Runbook\nmetadata: {name: recovery, version: 1}\nspec:\n  shape: recovery@1\n  steps:\n    - resolveSources: {}\n    - buildIndex: {}\n    - verify: {}\n    - cutover: {approval: required}\n    - retireOld: {keep_versions: 2}\n";

#[test]
fn runbook_process_recovery() {
    if !available() {
        return;
    }
    let mut phases = vec![
        "step_state:3:awaiting_approval".to_string(),
        "step_event:3:awaiting_approval".to_string(),
        "step_commit:3:awaiting_approval".to_string(),
        "step_state:3:running".to_string(),
        "step_event:3:running".to_string(),
        "step_commit:3:running".to_string(),
    ];
    for ordinal in 1..=4 {
        phases.extend([
            format!("step_effect:{ordinal}"),
            format!("step_state:{ordinal}:done"),
            format!("step_event:{ordinal}:done"),
            format!("step_commit:{ordinal}:done"),
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
    if after_approval && (!interrupted || !phase.ends_with(":running")) {
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
        let uncommitted = phase.starts_with("step_state") || phase.starts_with("step_event");
        let target = phase.split(':').nth(2).unwrap_or("running");
        let expected = if uncommitted {
            match target {
                "awaiting_approval" => "pending",
                "running" => "awaiting_approval",
                _ => "running",
            }
        } else {
            target
        };
        assert_eq!(snapshot.steps[ordinal].state, expected, "phase={phase}");
        let key = format!("step-{ordinal}-{}-{target}", snapshot.steps[ordinal].name);
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM claims WHERE tenant_id=$1 AND version_id=$2 AND key=$3",
        )
        .bind(&tenant)
        .bind(&version)
        .bind(&key)
        .fetch_one(pool(&state).unwrap())
        .await
        .unwrap();
        assert_eq!(count, if uncommitted { 0 } else { 1 }, "phase={phase}");
        if uncommitted && target == "done" {
            assert!(snapshot.steps[ordinal].detail.is_none());
        }
    }
    assert_ledger(&state, &tenant, &version).await;

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
        let lock = acquire_released_run_lock(&state, &tenant, &run).await;
        let next = execute(&state, &tenant, &run, &doc, Some(&version), None)
            .await
            .unwrap();
        drop_run_lock_and_wait(&state, lock).await;
        next
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
    // Every newly completed step has exactly one durable done transition,
    // including re-entry after an uncommitted checkpoint.
    for step in &final_run.steps {
        let key = format!("step-{}-{}-done", step.ordinal, step.name);
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM claims WHERE tenant_id=$1 AND version_id=$2 AND key=$3",
        )
        .bind(&tenant)
        .bind(&version)
        .bind(key)
        .fetch_one(pool(&state).unwrap())
        .await
        .unwrap();
        assert_eq!(count, 1);
    }
    assert_ledger(&state, &tenant, &version).await;
}

async fn run_id(state: &AppState, tenant: &str) -> String {
    sqlx::query_scalar("SELECT id FROM runbook_runs WHERE tenant_id=$1")
        .bind(tenant)
        .fetch_one(pool(state).unwrap())
        .await
        .unwrap()
}

async fn assert_ledger(state: &AppState, tenant: &str, version: &str) {
    let mismatches: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM claims c FULL JOIN ledger_events e
           ON c.tenant_id=e.tenant_id AND c.version_id=e.version_id AND c.seq=e.seq
         WHERE COALESCE(c.tenant_id,e.tenant_id)=$1
           AND COALESCE(c.version_id,e.version_id)=$2
           AND (c.id IS NULL OR e.body->>'claim_id' IS DISTINCT FROM c.id)",
    )
    .bind(tenant)
    .bind(version)
    .fetch_one(pool(state).unwrap())
    .await
    .unwrap();
    assert_eq!(mismatches, 0, "claim/event correspondence survives restart");
    let (head, maximum, count, unique): (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT h.current_seq, COALESCE(MAX(c.seq),0), COUNT(c.seq), COUNT(DISTINCT c.seq)
         FROM lineage_heads h LEFT JOIN claims c
           ON c.tenant_id=h.tenant_id AND c.version_id=$2
         WHERE h.tenant_id=$1 AND h.lineage_root_id=$2 GROUP BY h.current_seq",
    )
    .bind(tenant)
    .bind(version)
    .fetch_one(pool(state).unwrap())
    .await
    .unwrap();
    assert_eq!(head, maximum);
    assert_eq!(count, unique);
}

// Compatibility and error paths use fresh tenants in the same disposable DB.
#[tokio::test]
async fn checkpoint_process_recovery_rollback_and_legacy() {
    if !available() {
        return;
    }
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        let tenant = format!("p08-checkpoint-{}", uuid::Uuid::new_v4().simple());
        let state = state(&tenant).await;
        let store = state.store_for(&tenant).await.unwrap();
        let version = store.create_version(None, None).await.unwrap();
        let other_version = store.create_version(None, None).await.unwrap();
        let run = "legacy-run";
        sqlx::query("INSERT INTO runbook_runs (tenant_id,id,runbook_ref,state,version_id) VALUES ($1,$2,'legacy@1','running',$3)")
            .bind(&tenant).bind(run).bind(&version).execute(pool(&state).unwrap()).await.unwrap();
        let detail = serde_json::json!({"nested": [null, {"count": 9007199254740993_u64}], "index_version": "legacy-index"});
        // Model an old writer's done checkpoint with no historical event.
        sqlx::query("INSERT INTO runbook_steps (tenant_id,run_id,ordinal,name,state,detail) VALUES ($1,$2,0,'resolveSources','done',$3),( $1,$2,1,'buildIndex','pending',NULL)")
            .bind(&tenant).bind(run).bind(&detail).execute(pool(&state).unwrap()).await.unwrap();
        let original = op_get_run(&state, &tenant, run).await.unwrap();
        for (test_tenant, test_run, ordinal, name, test_version) in [
            (tenant.as_str(), run, 99, "buildIndex", Some(version.as_str())),
            (tenant.as_str(), "absent", 1, "buildIndex", Some(version.as_str())),
            (tenant.as_str(), run, 1, "wrong-name", Some(version.as_str())),
            (tenant.as_str(), run, 1, "buildIndex", Some(other_version.as_str())),
            (tenant.as_str(), run, 1, "buildIndex", None),
            (tenant.as_str(), run, 1, "buildIndex", Some("missing-version")),
            ("p08-unrelated-tenant", run, 1, "buildIndex", Some(version.as_str())),
        ] {
            assert!(set_step(&state, test_tenant, test_run, ordinal, name,
                StepState::Done, Some(detail.clone()), test_version).await.is_err());
            assert_eq!(store.head(&version).await.unwrap(), 0);
            assert_eq!(store.head(&other_version).await.unwrap(), 0);
            assert_eq!(serde_json::to_value(op_get_run(&state, &tenant, run).await.unwrap()).unwrap(),
                serde_json::to_value(&original).unwrap());
        }
        assert_ledger(&state, &tenant, &version).await;
        // New writes on an old run do not backfill or relabel its earlier gap.
        set_step(&state, &tenant, run, 1, "buildIndex", StepState::Running,
            Some(detail.clone()), Some(&version)).await.unwrap();
        set_step(&state, &tenant, run, 1, "buildIndex", StepState::Done,
            None, Some(&version)).await.unwrap();
        let reopened = crate::crash_recovery::state(&tenant).await;
        let book = parse_runbook("apiVersion: munarium.ioka.io/v1\nkind: Runbook\nmetadata: {name: legacy, version: 1}\nspec:\n  shape: recovery@1\n  steps:\n    - resolveSources: {}\n    - buildIndex: {}\n").unwrap();
        for _ in 0..2 {
            let lock = acquire_run_lock(&reopened, &tenant, run).await.unwrap();
            assert_eq!(execute(&reopened, &tenant, run, &book, Some(&version), None).await.unwrap(), "done");
            let read = op_get_run(&reopened, &tenant, run).await.unwrap();
            assert!(read.steps.iter().all(|s| s.state == "done" && s.detail.as_ref() == Some(&detail)));
            assert_eq!(store.head(&version).await.unwrap(), 2);
            let legacy_events: i64 = sqlx::query_scalar("SELECT count(*) FROM claims WHERE tenant_id=$1 AND key='step-0-resolveSources-done'")
                .bind(&tenant).fetch_one(pool(&state).unwrap()).await.unwrap();
            assert_eq!(legacy_events, 0);
            drop_run_lock_and_wait(&reopened, lock).await;
        }
        // The unchanged wire DTO still decodes old and new checkpoints.
        let read = op_get_run(&reopened, &tenant, run).await.unwrap();
        let decoded: dto::RunStatusResponse = serde_json::from_value(serde_json::to_value(read).unwrap()).unwrap();
        assert_eq!(decoded.steps[1].detail.as_ref(), Some(&detail));
        assert_ledger(&state, &tenant, &version).await;
        // Simulate an older binary writing after the new binary: the schema
        // remains readable, but that writer cannot provide the new guarantee.
        sqlx::query("UPDATE runbook_steps SET state='failed' WHERE tenant_id=$1 AND run_id=$2 AND ordinal=1")
            .bind(&tenant).bind(run).execute(pool(&state).unwrap()).await.unwrap();
        assert_eq!(op_get_run(&reopened, &tenant, run).await.unwrap().steps[1].state, "failed");
        assert_eq!(store.head(&version).await.unwrap(), 2);
        let missing: i64 = sqlx::query_scalar("SELECT count(*) FROM claims WHERE tenant_id=$1 AND key='step-1-buildIndex-failed'")
            .bind(&tenant).fetch_one(pool(&state).unwrap()).await.unwrap();
        assert_eq!(missing, 0, "mixed writers reopen the gap; never silently repair");
        // No-version runs intentionally have no ledger history.
        sqlx::query("INSERT INTO runbook_runs (tenant_id,id,runbook_ref,state) VALUES ($1,'no-version','legacy@1','running')")
            .bind(&tenant).execute(pool(&state).unwrap()).await.unwrap();
        sqlx::query("INSERT INTO runbook_steps (tenant_id,run_id,ordinal,name,state) VALUES ($1,'no-version',0,'resolveSources','pending')")
            .bind(&tenant).execute(pool(&state).unwrap()).await.unwrap();
        set_step(&state, &tenant, "no-version", 0, "resolveSources", StepState::Running, Some(detail.clone()), None).await.unwrap();
        set_step(&state, &tenant, "no-version", 0, "resolveSources", StepState::Done, None, None).await.unwrap();
        let read = op_get_run(&reopened, &tenant, "no-version").await.unwrap();
        assert_eq!(read.steps[0].detail.as_ref(), Some(&detail));
        assert_eq!(store.head(&version).await.unwrap(), 2);
    }).await.expect("bounded checkpoint regression");
}

#[tokio::test]
async fn approval_process_recovery_two_instances() {
    if !available() {
        return;
    }
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        let tenant = format!("p08-approval-{}", uuid::Uuid::new_v4().simple());
        let first = state(&tenant).await;
        let second = state(&tenant).await;
        let store = first.store_for(&tenant).await.unwrap();
        let version = store.create_version(None, None).await.unwrap();
        op_apply_shape(&first, &tenant,
            "apiVersion: munarium.ioka.io/v1\nkind: Shape\nmetadata: {name: recovery, version: 1}\nspec:\n  fact:\n    schema: {type: object}\n", None, None).await.unwrap();
        first.retrieval_for(&tenant).unwrap().put_source(
            "", "text/plain", "recovery/approval.txt", Some("recovery@1"),
            b"A fictional approval fixture.",
        ).await.unwrap();
        op_apply_runbook(&first, &tenant, BOOK).await.unwrap();
        let (run, status) = op_run_runbook(&first, &tenant, "recovery@1", Some(&version)).await.unwrap();
        assert_eq!(status, "awaiting_approval");
        let lock = acquire_released_run_lock(&first, &tenant, &run).await;
        assert!(op_approve_step(&second, &tenant, &run, 3).await.is_err());
        assert_eq!(op_get_run(&first, &tenant, &run).await.unwrap().steps[3].state, "awaiting_approval");
        drop_run_lock_and_wait(&first, lock).await;
        // Independent pools represent two replicas racing the same approval.
        let (a, b) = tokio::join!(op_approve_step(&first, &tenant, &run, 3), op_approve_step(&second, &tenant, &run, 3));
        assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
        let before = store.head(&version).await.unwrap();
        assert!(op_approve_step(&second, &tenant, &run, 3).await.is_err());
        assert_eq!(store.head(&version).await.unwrap(), before);
        let running: i64 = sqlx::query_scalar("SELECT count(*) FROM claims WHERE tenant_id=$1 AND key='step-3-cutover-running'")
            .bind(&tenant).fetch_one(pool(&first).unwrap()).await.unwrap();
        assert_eq!(running, 1);
        assert_ledger(&first, &tenant, &version).await;
    }).await.expect("bounded competing approval regression");
}
