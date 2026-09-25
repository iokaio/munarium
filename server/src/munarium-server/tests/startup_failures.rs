// SPDX-License-Identifier: Apache-2.0
//! Startup failures end the process with one `startup error:` line and exit
//! status 1, never a panic (P15/R32). A listener that cannot bind is the
//! realistic case: before P15 an occupied port panicked `main` with exit
//! status 101 and a backtrace hint instead of an operator-readable error.

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// The server binary with a minimal, isolated configuration: memory store, no
/// authentication, nothing inherited from the caller's MUNARIUM_* variables.
/// The rest of the environment is kept (Windows sockets need `SystemRoot`).
fn server() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_munarium-server"));
    for (key, _) in std::env::vars() {
        if key.starts_with("MUNARIUM_") {
            cmd.env_remove(key);
        }
    }
    cmd.env("MUNARIUM_STORE", "memory")
        .env("MUNARIUM_AUTH_MODE", "disabled")
        .env("MUNARIUM_OPS_ADDR", "127.0.0.1:0");
    cmd
}

fn assert_startup_error(out: &Output, expected: &str) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    assert!(stderr.contains(expected), "stderr: {stderr}");
    assert!(!stderr.contains("panicked"), "stderr: {stderr}");
}

#[test]
fn an_occupied_rest_port_is_a_startup_error_not_a_panic() {
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = occupied.local_addr().unwrap();
    let out = server()
        .env("MUNARIUM_HTTP_ADDR", addr.to_string())
        .env("MUNARIUM_GRPC_ADDR", "disabled")
        .output()
        .unwrap();
    assert_startup_error(&out, &format!("startup error: bind {addr}"));
}

#[test]
fn an_occupied_grpc_port_is_a_startup_error_not_a_panic() {
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = occupied.local_addr().unwrap();
    let out = server()
        .env("MUNARIUM_HTTP_ADDR", "127.0.0.1:0")
        .env("MUNARIUM_GRPC_ADDR", addr.to_string())
        .output()
        .unwrap();
    assert_startup_error(&out, &format!("startup error: bind gRPC {addr}"));
}

/// Control: with free ports the same configuration starts and serves.
#[test]
fn free_ports_start_the_rest_plane() {
    let mut child = server()
        .env("MUNARIUM_HTTP_ADDR", "127.0.0.1:0")
        .env("MUNARIUM_GRPC_ADDR", "disabled")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if line.contains("REST plane listening") {
                let _ = tx.send(());
                return;
            }
        }
    });
    let started = rx.recv_timeout(Duration::from_secs(60));
    let _ = child.kill();
    let _ = child.wait();
    assert!(started.is_ok(), "the REST plane never reported listening");
}
