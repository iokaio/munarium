// SPDX-License-Identifier: Apache-2.0
//! munarium-server — three listeners, one service layer, graceful shutdown.
//!   :8080  REST + /docs + /openapi.json + health (h1 + h2c)
//!   :50051 direct gRPC (tonic + health + reflection); MUNARIUM_GRPC_ADDR=disabled to turn off
//!   :9090  ops: /healthz, /readyz (real store probe), /metrics (Prometheus text)
//!
//! `munarium-server openapi` prints the OpenAPI document (CI drift check).

// KernelError carries gate-finding vectors by design (policy rejections are the
// payload, not an anomaly), and step/invocation recorders take one argument
// per recorded dimension — both clippy defaults trade away the wrong thing here.
#![allow(clippy::result_large_err, clippy::too_many_arguments)]

mod authoring_api;
mod charts;
mod chronology_api;
mod collections_api;
mod config;
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
mod grpc;
mod grpc_data;
mod grpc_platform;
mod ingest_api;
mod interactions;
mod max_tokens_api;
mod metrics;
mod middleware;
mod models;
mod openapi;
mod ops;
mod providers_api;
mod reports_api;
mod rest;
mod runbooks_api;
mod service;
mod sessions_api;
mod shadow_plane;
mod state;
mod storage_api;
mod tokens_api;
mod verification;

use config::Config;
use munarium_proto::mmp::v1 as pb;
use state::AppState;

#[tokio::main]
async fn main() {
    if std::env::args().nth(1).as_deref() == Some("openapi") {
        println!(
            "{}",
            serde_json::to_string_pretty(&openapi::doc()).expect("openapi serializes")
        );
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

    let shutdown = shutdown_signal();
    let mut tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    // Drain visibility: the moment a shutdown signal fires, both planes'
    // /readyz flip to 503 "draining" so load balancers stop routing here
    // while in-flight requests finish under the grace window.
    tokio::spawn({
        let state = state.clone();
        async move {
            shutdown_signal().await;
            state
                .draining
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    });

    // REST
    {
        let app = rest::router(state.clone());
        let addr = config.http_addr.clone();
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .unwrap_or_else(|e| panic!("bind {addr}: {e}"));
        tracing::info!(%addr, "REST plane listening");
        tasks.push(tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown_signal())
                .await
                .expect("rest server");
        }));
    }

    // direct gRPC
    if let Some(socket) = grpc_socket {
        let (mut health_reporter, health_service) = tonic_health::server::health_reporter();
        health_reporter
            .set_serving::<pb::command_service_server::CommandServiceServer<grpc::CommandSvc>>()
            .await;
        let reflection = tonic_reflection::server::Builder::configure()
            .register_encoded_file_descriptor_set(munarium_proto::FILE_DESCRIPTOR_SET)
            .register_encoded_file_descriptor_set(tonic_health::pb::FILE_DESCRIPTOR_SET)
            .build_v1()
            .expect("reflection");
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
        // Bind INLINE, before the spawn and before "listening" is logged —
        // the same fail-loudly shape as the REST plane. Until 2026-08-17 the
        // bind happened inside the spawned task after the log line, so an
        // occupied port produced a half-started process that answered REST
        // health checks under a false gRPC "listening" line (dev-guide §13
        // entry 4, closed).
        let listener = tokio::net::TcpListener::bind(socket)
            .await
            .unwrap_or_else(|e| panic!("bind gRPC {socket}: {e}"));
        tracing::info!(addr = %socket, "direct gRPC plane listening");
        let capture_layer = middleware::GrpcCaptureLayer {
            state: state.clone(),
        };
        tasks.push(tokio::spawn(async move {
            tonic::transport::Server::builder()
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
                .serve_with_incoming_shutdown(
                    tokio_stream::wrappers::TcpListenerStream::new(listener),
                    shutdown_signal(),
                )
                .await
                .expect("grpc server");
        }));
    }

    // ops
    {
        let addr = config.ops_addr.clone();
        let app = ops::router(state.clone());
        if let Ok(listener) = tokio::net::TcpListener::bind(&addr).await {
            tracing::info!(%addr, "ops listening");
            tasks.push(tokio::spawn(async move {
                axum::serve(listener, app)
                    .with_graceful_shutdown(shutdown_signal())
                    .await
                    .expect("ops server");
            }));
        } else {
            tracing::warn!(%addr, "ops port unavailable; continuing without it");
        }
    }

    shutdown.await;
    tracing::info!("shutdown signal received; draining");
    let grace = std::time::Duration::from_secs(state.config.shutdown_grace_secs);
    let _ = tokio::time::timeout(grace, futures_join_all(tasks)).await;
}

async fn futures_join_all(tasks: Vec<tokio::task::JoinHandle<()>>) {
    for t in tasks {
        let _ = t.await;
    }
}

/// Resolve on ANY shutdown request the platform can send. Until 2026-08-17
/// this awaited only ctrl_c (SIGINT) — Kubernetes and Container Apps send
/// SIGTERM, so the graceful drain (MUNARIUM_SHUTDOWN_GRACE_SECS) never fired
/// under an orchestrator and every rolling restart was a hard kill. The
/// Windows arm keeps the dev loop honest with the same select shape.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler installs");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(windows)]
    {
        let mut shut =
            tokio::signal::windows::ctrl_shutdown().expect("ctrl-shutdown handler installs");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = shut.recv() => {}
        }
    }
}
