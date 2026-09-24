// SPDX-License-Identifier: Apache-2.0
//! Application-process qualification only: this module is never in a library build.
use super::*;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const LIMIT: Duration = Duration::from_secs(30);
static ARMED: AtomicBool = AtomicBool::new(false);

pub(super) fn barrier(phase: &str) {
    if !ARMED.load(Ordering::Relaxed) || std::env::var("P08_PHASE").as_deref() != Ok(phase) {
        return;
    }
    let dir = PathBuf::from(std::env::var_os("P08_DIR").expect("fixture directory"));
    std::fs::write(dir.join(phase), b"reached").unwrap();
    let deadline = Instant::now() + LIMIT;
    while !dir.join("release").exists() {
        assert!(Instant::now() < deadline, "barrier timeout: {phase}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn(dir: &Path, tenant: &str, phase: &str, mode: &str) -> OwnedChild {
    OwnedChild(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "crash_recovery::child",
                "--ignored",
                "--nocapture",
            ])
            .env("P08_DIR", dir)
            .env("P08_TENANT", tenant)
            .env("P08_PHASE", phase)
            .env("P08_MODE", mode)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn owned child"),
    )
}

fn wait_marker(child: &mut OwnedChild, dir: &Path, marker: &str) {
    marker_result(child, dir, marker, LIMIT)
        .unwrap_or_else(|reason| panic!("{reason} at {marker}; retained {}", dir.display()));
}

fn marker_result(
    child: &mut OwnedChild,
    dir: &Path,
    marker: &str,
    limit: Duration,
) -> std::result::Result<(), &'static str> {
    let deadline = Instant::now() + limit;
    while !dir.join(marker).exists() {
        if child.0.try_wait().unwrap().is_some() {
            return Err("child exited");
        }
        if Instant::now() >= deadline {
            return Err("marker timeout");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

#[test]
fn harness_controls_without_database() {
    let dir = std::env::temp_dir().join(format!("p08-control-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir(&dir).unwrap();
    let mut child = spawn(&dir, "unused", "probe", "probe");
    wait_marker(&mut child, &dir, "probe");
    assert_eq!(
        marker_result(&mut child, &dir, "missing", Duration::from_millis(50)),
        Err("marker timeout")
    );
    std::fs::write(dir.join("release"), b"continue").unwrap();
    wait_exit(&mut child, true);
    assert_eq!(
        marker_result(&mut child, &dir, "missing", LIMIT),
        Err("child exited")
    );
    for name in ["probe", "release"] {
        std::fs::remove_file(dir.join(name)).unwrap();
    }
    std::fs::remove_dir(dir).unwrap();
}

fn wait_exit(child: &mut OwnedChild, success: bool) {
    let deadline = Instant::now() + LIMIT;
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert_eq!(status.success(), success, "unexpected child exit: {status}");
            break;
        }
        assert!(Instant::now() < deadline, "child exit timeout");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn ledger_process_recovery() {
    if std::env::var_os("MUNARIUM_TEST_DATABASE_URL").is_none() {
        eprintln!("UNAVAILABLE: P08 requires MUNARIUM_TEST_DATABASE_URL (disposable database)");
        return;
    }
    for phase in ["before_commit", "after_commit", "before_reply"] {
        for crash in [false, true] {
            let tenant = format!("p08-{}", uuid::Uuid::new_v4().simple());
            let dir = std::env::temp_dir().join(&tenant);
            std::fs::create_dir(&dir).unwrap();
            eprintln!(
                "P08 phase={phase} crash={crash} run={tenant} markers={}",
                dir.display()
            );
            let mut child = spawn(&dir, &tenant, phase, "write");
            wait_marker(&mut child, &dir, phase);
            if crash {
                child.0.kill().expect("terminate only owned child");
            } else {
                std::fs::write(dir.join("release"), b"continue").unwrap();
            }
            wait_exit(&mut child, !crash);
            assert_eq!(dir.join("reply").exists(), !crash);
            let committed = !crash || phase != "before_commit";
            let mut reader = spawn(
                &dir,
                &tenant,
                "disabled",
                if committed { "full" } else { "baseline" },
            );
            wait_exit(&mut reader, true);
            // Only files created by this scenario are removed; failures retain evidence.
            for name in [phase, "release", "reply", "version", "baseline"] {
                let path = dir.join(name);
                if path.exists() {
                    std::fs::remove_file(path).unwrap();
                }
            }
            std::fs::remove_dir(&dir).unwrap();
        }
    }
}

#[test]
#[ignore = "child fixture; invoked only by ledger_process_recovery"]
fn child() {
    if std::env::var("P08_MODE").as_deref() == Ok("probe") {
        ARMED.store(true, Ordering::Relaxed);
        barrier("probe");
        return;
    }
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        tokio::time::timeout(LIMIT, child_work())
            .await
            .expect("bounded database operations");
    });
}

async fn child_work() {
    let dir = PathBuf::from(std::env::var_os("P08_DIR").expect("parent invocation required"));
    let tenant = std::env::var("P08_TENANT").unwrap();
    let url = std::env::var("MUNARIUM_TEST_DATABASE_URL").expect("test database required");
    // Do not print database errors: connection diagnostics may contain credentials.
    let store = PgStore::connect(&url, &tenant)
        .await
        .unwrap_or_else(|_| panic!("fixture connect failed"));
    let mode = std::env::var("P08_MODE").unwrap();
    if mode == "write" {
        let version = store.create_version(None, None).await.unwrap();
        // Baseline acknowledgement happens before arming the batch barrier.
        let baseline = store
            .append_claim(&version, NewClaim::fact("hero", "eyes", "green"), Some(0))
            .await
            .unwrap();
        std::fs::write(dir.join("version"), &version).unwrap();
        std::fs::write(dir.join("baseline"), serde_json::to_vec(&baseline).unwrap()).unwrap();
        ARMED.store(true, Ordering::Relaxed);
        let mut correction = NewClaim::fact("hero", "eyes", "blue");
        correction.claim_type = ClaimType::Correction;
        correction.supersedes_id = Some(baseline.id);
        let mut disputed = NewClaim::fact("hero", "height", "tall");
        disputed.status = ClaimStatus::Disputed;
        let result = store
            .append_claims(
                &version,
                vec![
                    correction,
                    disputed,
                    NewClaim::fact("hero", "name", "Ansel"),
                ],
                Some(1),
            )
            .await
            .unwrap();
        barrier("before_reply");
        std::fs::write(dir.join("reply"), serde_json::to_vec(&result).unwrap()).unwrap();
    } else {
        let version = std::fs::read_to_string(dir.join("version")).unwrap();
        let baseline: Claim =
            serde_json::from_slice(&std::fs::read(dir.join("baseline")).unwrap()).unwrap();
        let full = mode == "full";
        let expected = if full { 4 } else { 1 };
        assert_eq!(store.head(&version).await.unwrap(), expected);
        let pinned = store
            .slice_facts(
                &version,
                &FactQuery {
                    as_of_seq: Some(1),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(&pinned).unwrap(),
            serde_json::json!([baseline])
        );
        let visible = store
            .slice_facts(&version, &FactQuery::default())
            .await
            .unwrap();
        let mut values: Vec<_> = visible.iter().map(|c| c.normalized_text()).collect();
        values.sort();
        assert_eq!(
            values,
            if full {
                vec!["hero.eyes=blue", "hero.name=Ansel"]
            } else {
                vec!["hero.eyes=green"]
            }
        );
        // Independent SQL oracle checks every event/projection, including disputed
        // and superseded rows, and the allocation head (not just resolved facts).
        let rows = sqlx::query("SELECT c.*, e.body FROM claims c FULL JOIN ledger_events e ON c.tenant_id=e.tenant_id AND c.version_id=e.version_id AND c.seq=e.seq WHERE COALESCE(c.tenant_id,e.tenant_id)=$1 ORDER BY COALESCE(c.seq,e.seq)")
            .bind(&tenant).fetch_all(store.pool()).await.unwrap();
        assert_eq!(rows.len(), expected as usize);
        let mut previous = 0;
        for row in &rows {
            let claim = row_to_claim(row).unwrap();
            assert!(claim.seq > previous);
            previous = claim.seq;
            let event: serde_json::Value = row.get("body");
            assert_eq!(event["claim_id"], claim.id);
            assert_eq!(event["normalized"], claim.normalized_text());
            assert_eq!(event["status"], status_str(claim.status));
            assert_eq!(
                event["supersedes_id"],
                serde_json::json!(claim.supersedes_id)
            );
        }
        let head: i64 = sqlx::query_scalar(
            "SELECT current_seq FROM lineage_heads WHERE tenant_id=$1 AND lineage_root_id=$2",
        )
        .bind(&tenant)
        .bind(&version)
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(head as u64, previous);
        if dir.join("reply").exists() {
            let reply: Vec<Claim> =
                serde_json::from_slice(&std::fs::read(dir.join("reply")).unwrap()).unwrap();
            for claim in reply {
                assert_eq!(
                    serde_json::to_value(store.get_claim(&claim.id).await.unwrap().unwrap())
                        .unwrap(),
                    serde_json::to_value(claim).unwrap()
                );
            }
        }
        // Re-entry proves the killed transaction released its lock and the head
        // remains usable. No assertion about gapless global identity allocation.
        store
            .append_claim(
                &version,
                NewClaim::fact("hero", "next", "ok"),
                Some(expected),
            )
            .await
            .unwrap();
    }
    store.pool().close().await;
}
