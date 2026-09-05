// SPDX-License-Identifier: Apache-2.0
//! Munarium Matrix — the library behind the `munarium-matrix` binary.
//!
//! Everything the service does lives here rather than in `main.rs`, so that a
//! build can be composed out of tree: Munarium Matrix Enterprise depends on this
//! crate, registers the adapters this repository does not carry, and ships its
//! own binary. `main.rs` is the entry point and nothing else.
//!
//! `munarium-matrix` — one binary, one role, three listeners at most.
//!
//!   :8180  REST (registry on the control role; meta on every role)
//!   :9190  ops: /healthz, /readyz, /metrics
//!
//! Startup order is the design, copied from the server because it earned it:
//! **everything checkable without side effects fails before a port is bound.**
//! Exit 2 = your environment is wrong and nothing was touched. Exit 1 = the
//! environment was plausible and the world said no (a database refused).

// `openapi::document()` is one `serde_json::json!` literal describing every
// route; each declared path deepens the macro's expansion, and the four
// metric-view routes crossed the default limit of 128.
#![recursion_limit = "1024"]
#![allow(clippy::result_large_err)]

pub mod adapters;
pub mod admin;
pub mod compat;
pub mod config;
pub mod execute;
pub mod grpc;
pub mod mcp;
pub mod metrics;
pub mod openapi;
pub mod ops;
pub mod proposals;
pub mod rest;
pub mod roles;
pub mod runtime;
pub mod state;

use config::Config;
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// The one-liner that fixes a drifted spec. Built here so the two callers
/// cannot print different advice.
fn regenerate_hint(path: &str) -> String {
    format!("regenerate it with: cargo run -q -p munarium-matrix-server --bin munarium-matrix -- openapi > {path}")
}

pub async fn run() {
    // Step 1: the `openapi` argv short-circuit, BEFORE config. CI regenerates
    // the spec from a checkout with no database and no environment; if this
    // needed Config::from_env the drift check would need a fake environment
    // and would rot.
    if std::env::args().nth(1).as_deref() == Some("openapi") {
        let doc = serde_json::to_string_pretty(&openapi::document()).expect("serialize");

        // `openapi --check <path>` compares the committed copy against what
        // this build generates, and exits 1 on drift.
        //
        // The comparison lives HERE rather than in the test script because a
        // shell is the wrong place for it: the document contains em-dashes,
        // and PowerShell's pipeline capture and `Get-Content` do not
        // round-trip UTF-8 the way a redirect writes it — a text comparison in
        // the runner reported drift between two files `cmp` calls identical.
        // Rust reads the bytes and parses the JSON, so the check compares
        // MEANING rather than whatever the shell made of the encoding.
        if std::env::args().nth(2).as_deref() == Some("--check") {
            let path = std::env::args().nth(3).unwrap_or_default();
            let committed = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("cannot read {path}: {e}");
                    eprintln!("{}", regenerate_hint(&path));
                    std::process::exit(1);
                }
            };
            let a: serde_json::Value = serde_json::from_str(&doc).expect("own document parses");
            match serde_json::from_str::<serde_json::Value>(&committed) {
                Ok(b) if a == b => {
                    println!("openapi: {path} matches this build");
                    return;
                }
                Ok(_) => {
                    eprintln!("openapi: {path} has DRIFTED from the code");
                    eprintln!("{}", regenerate_hint(&path));
                    std::process::exit(1);
                }
                Err(e) => {
                    eprintln!("openapi: {path} is not valid JSON: {e}");
                    std::process::exit(1);
                }
            }
        }

        println!("{doc}");
        return;
    }

    // Step 2: the TLS crypto provider, before anything can open a connection.
    // rustls 0.23 refuses to guess when more than one provider feature is
    // enabled, and this workspace has two — see the function's own docs.
    munarium_matrix_adapter::install_crypto_provider();

    // Step 4: tracing.
    let filter = std::env::var("MUNARIUM_MATRIX_LOG").unwrap_or_else(|_| "info".into());
    let json = std::env::var("MUNARIUM_MATRIX_LOG_FORMAT").as_deref() == Ok("json");
    let sub = tracing_subscriber::fmt().with_env_filter(filter);
    if json {
        sub.json().init();
    } else {
        sub.init();
    }

    // Step 3: configuration. Exit 2 means nothing was touched.
    let config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "config error");
            std::process::exit(2);
        }
    };
    tracing::info!(
        role = config.role.as_str(),
        instance = %config.instance_id,
        contract = munarium_matrix_core::CONTRACT_VERSION,
        "munarium-matrix starting"
    );

    // Step 4: the store. Exit 1 — the environment was plausible, the world said no.
    let url = config.database_url.clone().expect("checked in Config");
    let store = match munarium_matrix_store::MatrixStore::connect(&url, config.db_max_conns).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "startup error: database");
            std::process::exit(1);
        }
    };
    if let Err(e) = store.migrate().await {
        tracing::error!(error = %e, "startup error: migrations");
        std::process::exit(1);
    }
    tracing::info!("store ready (schema matrix)");

    // Step 4b: the lockstep check. A MAJOR mismatch stops the process here,
    // where the message is unmissable, rather than at the first seal.
    let server_version = match (&config.server_url, &config.server_token_ref) {
        (Some(url), token_ref) => {
            let token = token_ref
                .as_deref()
                .and_then(|r| config::resolve_secret(r).ok())
                .unwrap_or_default();
            match munarium_matrix_server_client::HttpServerClient::new_http1(
                url,
                &token,
                std::time::Duration::from_secs(10),
            ) {
                Ok(client) => {
                    use munarium_matrix_server_client::ServerClient as _;
                    match client.server_version().await {
                        Ok(v) => Some(v),
                        Err(e) => {
                            tracing::warn!(error = %e, "could not read the server version");
                            None
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "could not build the server client");
                    None
                }
            }
        }
        _ => {
            tracing::info!("MUNARIUM_MATRIX_SERVER_URL is unset; skipping the lockstep check");
            None
        }
    };
    let verdict = compat::compare(&config.target_server_version, server_version.as_deref());
    if verdict.is_fatal() {
        tracing::error!(
            target_version = %config.target_server_version,
            server_version = ?server_version,
            verdict = %verdict,
            "startup error: major server version mismatch — the wire contract differs"
        );
        std::process::exit(1);
    }
    match verdict {
        compat::Compatibility::MajorMismatch => unreachable!("handled by is_fatal above"),
        compat::Compatibility::MinorDrift => tracing::warn!(
            target_version = %config.target_server_version,
            server_version = ?server_version,
            "server minor version is NEWER than this build targets; additive by the              compatibility rule, so the extra surface is simply unused"
        ),
        // Deliberately `error!` and not `warn!`, though it is non-fatal. The
        // server is missing surface this build calls, so the first symptom
        // would otherwise be a 404 at seal time — far from the cause, which is
        // exactly what this check exists to prevent.
        compat::Compatibility::MinorBehind => tracing::error!(
            target_version = %config.target_server_version,
            server_version = ?server_version,
            "server minor version is OLDER than this build targets: it may not have routes              this build calls (the evidence plane arrived in 0.5.0). Starting anyway — the              runtime fails closed with a typed refusal — but deploy a matching server"
        ),
        compat::Compatibility::Exact => {
            tracing::info!(server_version = ?server_version, "server lockstep confirmed")
        }
        compat::Compatibility::Unknown => {}
    }

    let state = state::AppState::with_server_version(config, store, server_version);

    // Step 5: the listeners.
    let rest_addr = state.config.http_addr.clone();
    let ops_addr = state.config.ops_addr.clone();

    let rest_listener = match tokio::net::TcpListener::bind(&rest_addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, addr = %rest_addr, "startup error: REST bind");
            std::process::exit(1);
        }
    };
    tracing::info!(addr = %rest_addr, "REST plane listening");

    let ops_state = state.clone();
    // An ops bind failure is a WARNING, not a death: losing the scrape target
    // must not take down a healthy data plane.
    match tokio::net::TcpListener::bind(&ops_addr).await {
        Ok(l) => {
            tracing::info!(addr = %ops_addr, "ops plane listening");
            tokio::spawn(async move {
                let _ = axum::serve(l, ops::router(ops_state)).await;
            });
        }
        Err(e) => tracing::warn!(error = %e, addr = %ops_addr, "ops plane not bound"),
    }

    // Step 5a: the gRPC data plane, on its own port, only on a role that
    // serves the query plane. A parse failure is fatal BEFORE the role loops
    // start, like the REST bind — a misconfigured listener must not become a
    // worker that quietly claims jobs.
    let grpc_shutdown = Arc::new(tokio::sync::Notify::new());
    let grpc_socket: Option<std::net::SocketAddr> = match state.config.grpc_addr.as_deref() {
        None => None,
        Some(addr) => match addr.parse() {
            Ok(a) => Some(a),
            Err(e) => {
                tracing::error!(error = %e, addr = %addr, "startup error: MUNARIUM_MATRIX_GRPC_ADDR does not parse");
                std::process::exit(1);
            }
        },
    };
    if let Some(addr) = grpc_socket {
        if state.config.role.serves_query() {
            let grpc_state = state.clone();
            let notify = grpc_shutdown.clone();
            tracing::info!(addr = %addr, "gRPC plane listening");
            tokio::spawn(async move {
                if let Err(e) =
                    grpc::serve(grpc_state, addr, async move { notify.notified().await }).await
                {
                    // FATAL, like a REST bind failure: a plane the config asked
                    // for and did not get is a misconfiguration, and a process
                    // that keeps serving REST while its gRPC listener is dead
                    // reports healthy to every probe that does not speak gRPC.
                    // tonic's Display is the bare "transport error"; the cause
                    // (the bind refusal, usually) is in the source chain. Found
                    // 2026-08-29 on a Windows dev box where 50151 sat inside a
                    // Hyper-V excluded port range.
                    let cause = std::error::Error::source(&e)
                        .map(|c| c.to_string())
                        .unwrap_or_default();
                    tracing::error!(error = %e, cause = %cause, addr = %addr, "startup error: gRPC plane");
                    std::process::exit(1);
                }
            });
        } else {
            tracing::info!(
                role = ?state.config.role,
                "gRPC plane not served: this role does not serve the query plane"
            );
        }
    }

    // Step 5b: the role loops. Spawned AFTER the listeners bind, so a port
    // clash kills the process before a worker can claim a job it would then
    // abandon.
    let workers = roles::spawn(state.clone());

    let shutdown_state = state.clone();
    let server = axum::serve(rest_listener, rest::router(state.clone())).with_graceful_shutdown(
        async move {
            shutdown_signal().await;
            // Flip readiness FIRST, then drain: a load balancer needs a
            // window to stop routing before in-flight work finishes.
            shutdown_state.draining.store(true, Ordering::Relaxed);
            grpc_shutdown.notify_waiters();
            tracing::info!("draining");
        },
    );

    if let Err(e) = server.await {
        tracing::error!(error = %e, "REST plane stopped");
        std::process::exit(1);
    }

    // The REST plane has drained. Give the role loops their window to finish
    // the job each is holding and exit on the `draining` flag they poll. This
    // is bounded on purpose: a worker wedged on an unresponsive source must not
    // hold the process open forever, and the checkpoint discipline means the
    // worst case of a hard stop is repeated work, never lost work.
    let grace = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        for handle in workers {
            let _ = handle.await;
        }
    })
    .await;
    if grace.is_err() {
        tracing::warn!(
            "role loops did not finish within the drain window; exiting anyway.              In-flight work will be re-claimed after its lease expires."
        );
    }
    tracing::info!("stopped");
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
