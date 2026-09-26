// SPDX-License-Identifier: Apache-2.0
//! A process must not keep advertising a surviving plane after another exits.
use crate::Shutdown;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::task::JoinSet;

type PlaneExit = (&'static str, Result<(), String>);

/// Returns true on an unexpected plane exit, panic, or failed drain. The caller
/// maps that to process exit 1 so its supervisor can restart the whole service.
pub(crate) async fn supervise(
    mut tasks: JoinSet<PlaneExit>,
    shutdown: Shutdown,
    draining: &AtomicBool,
    grace: Duration,
) -> bool {
    let mut failed = tokio::select! {
        biased;
        _ = shutdown.clone().wait() => false,
        ended = tasks.join_next() => {
            tracing::error!(?ended, "serving plane exited before shutdown");
            true
        }
    };
    draining.store(true, Ordering::Release);
    shutdown.request();
    tracing::info!(failed, "draining serving planes");
    let drained = tokio::time::timeout(grace, async {
        while let Some(ended) = tasks.join_next().await {
            if !matches!(ended, Ok((_, Ok(())))) {
                tracing::error!(?ended, "serving plane failed during drain");
                failed = true;
            }
        }
    })
    .await;
    if drained.is_err() {
        tracing::warn!("serving plane drain exceeded grace period");
        failed = true;
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    }
    failed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plane_failure_exits_the_child_process_after_draining_a_real_listener() {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "supervision::tests::child_serving_failure",
                "--ignored",
                "--nocapture",
            ])
            .env("MUNARIUM_SUPERVISION_CHILD", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert_eq!(status.code(), Some(1));
                return;
            }
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("serving supervisor left the child alive");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[tokio::test]
    #[ignore = "invoked only by the parent process fixture"]
    async fn child_serving_failure() {
        assert_eq!(
            std::env::var("MUNARIUM_SUPERVISION_CHILD").as_deref(),
            Ok("1")
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let shutdown = shutdown();
        let sibling = shutdown.clone();
        let mut tasks = JoinSet::new();
        tasks.spawn(async move {
            let result = axum::serve(listener, axum::Router::new())
                .with_graceful_shutdown(sibling.wait())
                .await;
            ("ops", result.map_err(|e| e.to_string()))
        });
        // Fault injection is confined to this test executable. The production
        // supervisor receives the same error shape as a failed serving future.
        tasks.spawn(async { ("REST", Err("fixture serving failure".into())) });
        let draining = AtomicBool::new(false);
        let failed = supervise(tasks, shutdown, &draining, Duration::from_secs(1)).await;
        assert!(draining.load(Ordering::Acquire));
        assert!(tokio::net::TcpStream::connect(address).await.is_err());
        std::process::exit(i32::from(failed));
    }

    fn shutdown() -> Shutdown {
        let (sender, receiver) = tokio::sync::watch::channel(false);
        Shutdown { sender, receiver }
    }

    #[tokio::test]
    async fn either_successful_or_failed_early_exit_drains_the_other_plane() {
        for outcome in [Ok(()), Err("fixture listener failure".into())] {
            let shutdown = shutdown();
            let sibling = shutdown.clone();
            let draining = AtomicBool::new(false);
            let mut tasks = JoinSet::new();
            tasks.spawn(async move { ("REST", outcome) });
            tasks.spawn(async move {
                sibling.wait().await;
                ("gRPC", Ok(()))
            });
            assert!(tokio::time::timeout(
                Duration::from_secs(2),
                supervise(tasks, shutdown, &draining, Duration::from_secs(1))
            )
            .await
            .unwrap());
            assert!(draining.load(Ordering::Acquire));
        }
    }

    #[tokio::test]
    async fn a_panicked_plane_is_a_process_failure() {
        let mut tasks = JoinSet::new();
        tasks.spawn(async { panic!("fixture plane panic") });
        assert!(
            supervise(
                tasks,
                shutdown(),
                &AtomicBool::new(false),
                Duration::from_secs(1)
            )
            .await
        );
    }

    #[tokio::test]
    async fn requested_shutdown_is_successful_and_closes_every_plane() {
        let shutdown = shutdown();
        let sibling = shutdown.clone();
        let mut tasks = JoinSet::new();
        tasks.spawn(async move {
            sibling.wait().await;
            ("REST", Ok(()))
        });
        shutdown.request();
        let draining = AtomicBool::new(false);
        assert!(!supervise(tasks, shutdown, &draining, Duration::from_secs(1)).await);
        assert!(draining.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn a_stuck_plane_is_aborted_after_the_grace_period() {
        let shutdown = shutdown();
        let mut tasks = JoinSet::new();
        let task = tasks.spawn(std::future::pending::<PlaneExit>());
        shutdown.request();
        assert!(
            supervise(
                tasks,
                shutdown,
                &AtomicBool::new(false),
                Duration::from_millis(10)
            )
            .await
        );
        assert!(task.is_finished());
    }
}
