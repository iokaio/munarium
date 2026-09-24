// SPDX-License-Identifier: Apache-2.0
//! Included only by test targets. Never a production feature or configuration.
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const LIMIT: Duration = Duration::from_secs(45);

pub fn dir() -> PathBuf {
    PathBuf::from(std::env::var_os("P08_DIR").expect("parent fixture invocation required"))
}

pub fn setting(name: &str) -> String {
    std::env::var(name).expect("parent fixture setting required")
}

pub fn barrier(phase: &str) {
    if std::env::var("P08_MODE").as_deref() != Ok("write")
        || std::env::var("P08_PHASE").as_deref() != Ok(phase)
    {
        return;
    }
    std::fs::write(dir().join("reached.tmp"), phase).unwrap();
    std::fs::rename(dir().join("reached.tmp"), dir().join("reached")).unwrap();
    let deadline = Instant::now() + LIMIT;
    while !dir().join("release").exists() {
        assert!(Instant::now() < deadline, "barrier timeout: {phase}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let deadline = Instant::now() + LIMIT;
        loop {
            match self.0.try_wait() {
                Ok(Some(_)) => return,
                Err(error) => {
                    eprintln!("owned child cleanup failed: {error}");
                    return;
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => {
                    eprintln!("owned child {} did not exit after termination", self.0.id());
                    return;
                }
            }
        }
    }
}

fn wait_exit(child: &mut OwnedChild, success: bool) {
    let deadline = Instant::now() + LIMIT;
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert_eq!(status.success(), success, "unexpected child exit: {status}");
            return;
        }
        assert!(Instant::now() < deadline, "child exit timeout");
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub fn run(entry: &str, case: &str, phase: &str, crash: bool, run_id: &str) {
    let root = std::env::temp_dir().canonicalize().unwrap();
    assert!(run_id.starts_with("p08-"));
    assert!(run_id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-'));
    let dir = root.join(run_id);
    std::fs::create_dir(&dir).unwrap();
    eprintln!(
        "P08 case={case} phase={phase} crash={crash} evidence={}",
        dir.display()
    );
    let spawn = |mode: &str| {
        OwnedChild(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", entry, "--ignored", "--nocapture"])
                .env("P08_DIR", &dir)
                .env("P08_TENANT", run_id)
                .env("P08_CASE", case)
                .env("P08_PHASE", phase)
                .env("P08_CRASH", if crash { "yes" } else { "no" })
                .env("P08_MODE", mode)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        )
    };
    wait_exit(&mut spawn("setup"), true);
    let mut writer = spawn("write");
    let deadline = Instant::now() + LIMIT;
    while !dir.join("reached").exists() {
        assert!(
            writer.0.try_wait().unwrap().is_none(),
            "writer exited before {phase}"
        );
        assert!(Instant::now() < deadline, "marker timeout: {case}/{phase}");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(std::fs::read_to_string(dir.join("reached")).unwrap(), phase);
    // A separate observer also checks the live lock/lease and pre-reply state.
    wait_exit(&mut spawn("observe"), true);
    if crash {
        writer.0.kill().unwrap();
    } else {
        std::fs::write(dir.join("release"), b"continue").unwrap();
    }
    wait_exit(&mut writer, !crash);
    assert_eq!(dir.join("reply").exists(), !crash);
    wait_exit(&mut spawn("recover"), true);
    // Canonical, run-owned directory only. Failure above retains all evidence.
    assert_eq!(dir.canonicalize().unwrap().parent(), Some(root.as_path()));
    std::fs::remove_dir_all(&dir).unwrap();
}
