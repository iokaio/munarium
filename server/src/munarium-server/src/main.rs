// SPDX-License-Identifier: Apache-2.0
//! munarium-server — three listeners, one service layer, graceful shutdown.
//!   :8080  REST + /docs + /openapi.json + health (h1 + h2c)
//!   :50051 direct gRPC (tonic + health + reflection); MUNARIUM_GRPC_ADDR=disabled to turn off
//!   :9090  ops: /healthz, /readyz (real store probe), /metrics (Prometheus text)
//!
//! `munarium-server openapi` prints the OpenAPI document (CI drift check).

// Production code returns typed errors instead of panicking; tests are exempt.
// The policy, its two exemptions and the per-site record are in
// server/docs/panic-boundaries.md (P15/R32).
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented
    )
)]
// KernelError carries gate-finding vectors by design (policy rejections are the
// payload, not an anomaly), and step/invocation recorders take one argument
// per recorded dimension — both clippy defaults trade away the wrong thing here.
#![allow(clippy::result_large_err, clippy::too_many_arguments)]

mod answers_api;
mod authoring_api;
mod charts;
mod chronology_api;
mod collections_api;
mod config;
#[cfg(test)]
mod crash_recovery;
mod dashboard;
mod datastore_builds;
mod datastore_jobs;
mod datastore_serving;
#[cfg(test)]
mod docs_coverage;
mod error;
mod evidence_api;
mod evidence_hierarchy;
mod evidence_providers;
mod evidence_routes;
mod governance_api;
#[cfg(test)]
mod governance_baseline;
#[cfg(test)]
mod governance_policy_tests;
mod grpc;
mod grpc_api;
mod grpc_data;
mod grpc_platform;
mod ingest_api;
mod interactions;
#[cfg(test)]
mod json_persistence_tests;
mod max_tokens_api;
mod metrics;
mod middleware;
mod models;
mod money_api;
mod openapi;
mod ops;
#[cfg(test)]
mod panic_policy;
mod providers_api;
mod query_api;
mod reports_api;
mod rest;
mod runbooks_api;
mod search_v12;
mod service;
mod sessions_api;
mod shadow_plane;
mod state;
mod storage_api;
mod tokens_api;
#[cfg(test)]
mod v12_tests;
mod verification;
mod vocabulary_api;
#[cfg(test)]
mod wire_compat_tests;

use config::Config;
use munarium_proto::mmp::v1 as pb;
use state::AppState;

#[tokio::main]
async fn main() {
    if std::env::args().nth(1).as_deref() == Some("openapi") {
        match serde_json::to_string_pretty(&openapi::doc()) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("openapi error: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    // MUNARIUM_LOG_FORMAT=json switches to structured JSON lines (the README
    // row was documented ahead of the code until 2026-08-17); anything else
    // keeps the human-readable default. Like MUNARIUM_LOG, this is the one
    // deliberately fail-open surface — logging must not stop a boot.
    let log = tracing_subscriber::fmt().with_env_filter(
        tracing_subscriber::EnvFilter::try_from_env("MUNARIUM_LOG")
            .unwrap_or_else(|_| "info".into()),
    );
    if std::env::var("MUNARIUM_LOG_FORMAT").as_deref() == Ok("json") {
        log.json().init();
    } else {
        log.init();
    }

    let config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("config error: {e}");
            std::process::exit(2);
        }
    };
    tracing::info!(?config.store, http = %config.http_addr, grpc = ?config.grpc_addr, instance = %config.instance_id, "starting");

    // Validate the gRPC address BEFORE any listener binds: a bad
    // MUNARIUM_GRPC_ADDR used to panic only after the REST plane was already
    // up and logged as listening — a half-started process.
    let grpc_socket: Option<std::net::SocketAddr> = match config.grpc_addr.as_deref() {
        Some(addr) => match addr.parse() {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("config error: MUNARIUM_GRPC_ADDR '{addr}' does not parse: {e}");
                std::process::exit(2);
            }
        },
        None => None,
    };

    let state = match AppState::new(config.clone()).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("startup error: {e}");
            std::process::exit(1);
        }
    };

    // One signal listener for the whole process, installed before any
    // listener binds: a platform that cannot deliver SIGTERM (or the Windows
    // equivalent) is a startup failure, not a panic inside a serving task.
    let shutdown = match Shutdown::install() {
        Ok(s) => s,
        Err(e) => startup_failure(format!("shutdown signal handler: {e}")),
    };
    let mut tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    tokio::spawn(vocabulary_api::worker(state.clone()));

    // Drain visibility: the moment a shutdown signal fires, both planes'
    // /readyz flip to 503 "draining" so load balancers stop routing here
    // while in-flight requests finish under the grace window.
    tokio::spawn({
        let state = state.clone();
        let shutdown = shutdown.clone();
        async move {
            shutdown.wait().await;
            state
                .draining
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    });

    // REST
    {
        let app = rest::router(state.clone());
        let addr = config.http_addr.clone();
        let listener = match tokio::net::TcpListener::bind(&addr).await {
            Ok(l) => l,
            Err(e) => startup_failure(format!("bind {addr}: {e}")),
        };
        tracing::info!(%addr, "REST plane listening");
        let shutdown = shutdown.clone();
        tasks.push(tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app)
                .with_graceful_shutdown(shutdown.wait())
                .await
            {
                // Logged, not panicked. The process stays up with this plane
                // stopped, as it did when the panic ended only this task
                // (dev-guide section 13, entry 30).
                tracing::error!(error = %e, "REST plane stopped with an error");
            }
        }));
    }

    // direct gRPC
    if let Some(socket) = grpc_socket {
        let (mut health_reporter, health_service) = tonic_health::server::health_reporter();
        health_reporter
            .set_serving::<pb::command_service_server::CommandServiceServer<grpc::CommandSvc>>()
            .await;
        let reflection = match tonic_reflection::server::Builder::configure()
            .register_encoded_file_descriptor_set(munarium_proto::FILE_DESCRIPTOR_SET)
            .register_encoded_file_descriptor_set(tonic_health::pb::FILE_DESCRIPTOR_SET)
            .build_v1()
        {
            Ok(r) => r,
            Err(e) => startup_failure(format!("gRPC reflection service: {e}")),
        };
        let command = pb::command_service_server::CommandServiceServer::new(grpc::CommandSvc {
            state: state.clone(),
        });
        let query = pb::query_service_server::QueryServiceServer::new(grpc::QuerySvc {
            state: state.clone(),
        });
        // Source chunks are whole-document slices; tonic's 4 MiB default
        // would reject a large chunk mid-upload.
        let ingest = pb::ingest_service_server::IngestServiceServer::new(grpc_data::IngestSvc {
            state: state.clone(),
        })
        .max_decoding_message_size(rest::MAX_SOURCE_BYTES);
        let retrieval =
            pb::retrieval_service_server::RetrievalServiceServer::new(grpc_data::RetrievalSvc {
                state: state.clone(),
            });
        let runbook =
            pb::runbook_service_server::RunbookServiceServer::new(grpc_data::RunbookSvc {
                state: state.clone(),
            });
        let provider =
            pb::provider_service_server::ProviderServiceServer::new(providers_api::ProviderSvc {
                state: state.clone(),
            });
        // platform twins (2026-08-18): the session data plane and the served
        // half of AdminService (access tokens; tenant RPCs answer
        // UNIMPLEMENTED — see proto/mmp/v1/admin.proto).
        let session =
            pb::session_service_server::SessionServiceServer::new(grpc_platform::SessionSvc {
                state: state.clone(),
            });
        let admin = pb::admin_service_server::AdminServiceServer::new(grpc_platform::AdminSvc {
            state: state.clone(),
        });
        let api = pb::server_api_service_server::ServerApiServiceServer::new(
            grpc_api::ServerApiSvc::new(state.clone()),
        )
        // Keep the full REST payload ceiling; protobuf framing and request
        // parameters have a separate small allowance.
        .max_decoding_message_size(rest::MAX_SOURCE_BYTES + 65536)
        .max_encoding_message_size(rest::MAX_SOURCE_BYTES + 65536);
        // Bind INLINE, before the spawn and before "listening" is logged —
        // the same fail-loudly shape as the REST plane. Until 2026-08-17 the
        // bind happened inside the spawned task after the log line, so an
        // occupied port produced a half-started process that answered REST
        // health checks under a false gRPC "listening" line (dev-guide §13
        // entry 4, closed).
        let listener = match tokio::net::TcpListener::bind(socket).await {
            Ok(l) => l,
            Err(e) => startup_failure(format!("bind gRPC {socket}: {e}")),
        };
        tracing::info!(addr = %socket, "direct gRPC plane listening");
        let capture_layer = middleware::GrpcCaptureLayer {
            state: state.clone(),
        };
        let shutdown = shutdown.clone();
        tasks.push(tokio::spawn(async move {
            if let Err(e) = tonic::transport::Server::builder()
                .layer(capture_layer)
                .add_service(health_service)
                .add_service(reflection)
                .add_service(command)
                .add_service(query)
                .add_service(ingest)
                .add_service(retrieval)
                .add_service(runbook)
                .add_service(provider)
                .add_service(session)
                .add_service(admin)
                .add_service(api)
                .serve_with_incoming_shutdown(
                    tokio_stream::wrappers::TcpListenerStream::new(listener),
                    shutdown.wait(),
                )
                .await
            {
                tracing::error!(error = %e, "direct gRPC plane stopped with an error");
            }
        }));
    }

    // ops
    {
        let addr = config.ops_addr.clone();
        let app = ops::router(state.clone());
        if let Ok(listener) = tokio::net::TcpListener::bind(&addr).await {
            tracing::info!(%addr, "ops listening");
            let shutdown = shutdown.clone();
            tasks.push(tokio::spawn(async move {
                if let Err(e) = axum::serve(listener, app)
                    .with_graceful_shutdown(shutdown.wait())
                    .await
                {
                    tracing::error!(error = %e, "ops plane stopped with an error");
                }
            }));
        } else {
            tracing::warn!(%addr, "ops port unavailable; continuing without it");
        }
    }

    shutdown.wait().await;
    tracing::info!("shutdown signal received; draining");
    let grace = std::time::Duration::from_secs(state.config.shutdown_grace_secs);
    let _ = tokio::time::timeout(grace, futures_join_all(tasks)).await;
}

async fn futures_join_all(tasks: Vec<tokio::task::JoinHandle<()>>) {
    for t in tasks {
        let _ = t.await;
    }
}

/// Report a startup failure the way `AppState::new` failures already are: one
/// `startup error:` line on stderr and exit status 1, with no panic backtrace.
/// Configuration errors keep exit status 2 (P15/R32).
fn startup_failure(message: impl std::fmt::Display) -> ! {
    eprintln!("startup error: {message}");
    std::process::exit(1);
}

/// Resolve on ANY shutdown request the platform can send. Until 2026-08-17
/// this awaited only ctrl_c (SIGINT) — Kubernetes and Container Apps send
/// SIGTERM, so the graceful drain (MUNARIUM_SHUTDOWN_GRACE_SECS) never fired
/// under an orchestrator and every rolling restart was a hard kill. The
/// Windows arm keeps the dev loop honest with the same select shape.
///
/// The handler is installed ONCE, by `install`, and fanned out to every plane
/// through a watch channel. Each plane used to install its own, and an
/// installation failure was an `expect` inside a serving task (P15/R32).
#[derive(Clone)]
struct Shutdown(tokio::sync::watch::Receiver<bool>);

impl Shutdown {
    fn install() -> std::io::Result<Self> {
        let (fired, rx) = tokio::sync::watch::channel(false);
        #[cfg(unix)]
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        #[cfg(windows)]
        let mut shut = tokio::signal::windows::ctrl_shutdown()?;
        tokio::spawn(async move {
            #[cfg(unix)]
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            #[cfg(windows)]
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = shut.recv() => {}
            }
            let _ = fired.send(true);
        });
        Ok(Self(rx))
    }

    /// Resolves once shutdown has been requested. A listener task that ended
    /// without sending also counts: nothing could signal shutdown any more.
    async fn wait(mut self) {
        let _ = self.0.wait_for(|fired| *fired).await;
    }
}
