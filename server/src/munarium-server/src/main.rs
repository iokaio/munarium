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

mod admission;
#[cfg(test)]
mod admission_tests;
mod answers_api;
mod artifact_damage;
#[cfg(test)]
mod artifact_damage_tests;
mod authoring_api;
mod budget_api;
mod charts;
mod chronology_api;
mod collections_api;
mod command_recovery;
mod config;
mod console_sessions;
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
mod inference;
#[cfg(test)]
mod inference_tests;
mod ingest_api;
mod interactions;
#[cfg(test)]
mod json_persistence_tests;
mod max_tokens_api;
mod mcp;
mod mcp_generation;
mod mcp_store;
mod mcp_tools;
mod metrics;
mod middleware;
mod models;
mod money_api;
mod openapi;
mod ops;
#[cfg(test)]
mod panic_policy;
mod platform_activation;
mod platform_api;
mod platform_identity;
mod platform_records;
mod platform_tls;
mod providers_api;
mod query_api;
mod replica_fleet;
mod reports_api;
mod rest;
mod runbooks_api;
mod scale_diagnostics;
mod search_v12;
mod service;
mod sessions_api;
mod shadow_plane;
mod source_retention_api;
mod state;
mod storage_api;
mod supervision;
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

fn main() {
    // The isolated PDF extraction child (R3/B4) is this same binary under a
    // private role argument. It dispatches before anything else runs: no
    // runtime, no logging, no configuration, and nothing read from the
    // (deliberately empty) environment.
    let mut args = std::env::args_os();
    if args.nth(1).as_deref()
        == Some(std::ffi::OsStr::new(
            munarium_extract::isolated::CHILD_ROLE_ARG,
        ))
    {
        let rest: Vec<std::ffi::OsString> = args.collect();
        std::process::exit(munarium_extract::isolated::run_child(&rest));
    }

    // What `#[tokio::main]` built, with a startup error instead of a panic.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| startup_failure(format!("tokio runtime: {e}")));
    runtime.block_on(serve());
}

async fn serve() {
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
    // Validated with the rest of the configuration, before anything binds;
    // AppState reads it again when it builds the extractor registry.
    if let Err(e) = config::pdf_extraction_from_env() {
        eprintln!("config error: {e}");
        std::process::exit(2);
    }
    // The console login (ClearView U10). `legacy` is the v1.4 default; the
    // switch is an explicit operator decision (docs/design/console-sessions.md).
    match console_sessions::ConsoleSessionSettings::from_env() {
        Ok(s) if s.sessions() => tracing::info!(
            idle_secs = s.idle_secs,
            max_secs = s.max_secs,
            secure_cookie = s.secure_cookie,
            "console sessions enabled (MUNARIUM_CONSOLE_SESSIONS=enabled)"
        ),
        Ok(_) => {}
        Err(e) => {
            eprintln!("config error: {e}");
            std::process::exit(2);
        }
    }
    // The MCP endpoint (ClearView A1-A9). Off unless MUNARIUM_MCP=enabled;
    // every MCP setting is validated either way, and an enabled endpoint is
    // checked against the rest of the configuration before anything binds.
    match mcp::McpSettings::from_env().and_then(|s| s.check_compatible(&config).map(|_| s)) {
        Ok(s) if s.enabled => tracing::info!(
            paid_tools = s.paid_tools,
            execution = ?s.execution,
            policies = s.generation.len(),
            "MCP endpoint enabled (MUNARIUM_MCP=enabled): POST /mcp on the REST listener"
        ),
        Ok(_) => {}
        Err(e) => {
            eprintln!("config error: {e}");
            std::process::exit(2);
        }
    }
    tracing::info!(?config.store, http = %config.http_addr, grpc = ?config.grpc_addr, instance = %config.instance_id, "starting");

    if std::env::args().nth(1).as_deref() == Some("artifact-damage") {
        let args: Vec<String> = std::env::args().skip(2).collect();
        std::process::exit(artifact_damage::run(&config, &args).await);
    }

    if std::env::args().nth(1).as_deref() == Some("console-fleet") {
        let args: Vec<String> = std::env::args().skip(2).collect();
        std::process::exit(replica_fleet::run(&config, &args).await);
    }

    if std::env::args().nth(1).as_deref() == Some("platform-enroll") {
        let settings = config.platform.as_ref().unwrap_or_else(|| {
            startup_failure("platform-enroll requires platform-v1 configuration")
        });
        let pg = if config.store == config::StoreKind::Postgres {
            let url = config
                .database_url
                .as_deref()
                .unwrap_or_else(|| startup_failure("platform enrollment requires a database"));
            Some(
                munarium_store_pg::PgStore::connect(url, munarium_store_pg::DEFAULT_TENANT)
                    .await
                    .unwrap_or_else(|_| {
                        startup_failure("platform enrollment database unavailable")
                    }),
            )
        } else {
            None
        };
        if let Err(e) = platform_api::initialize_checkpoints(settings, pg.as_ref()).await {
            startup_failure(e);
        }
        println!("Platform enrollment checkpoints created; no governing artifact activated.");
        return;
    }

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

    // Admission by work class (R7/B3). Validated with the other settings,
    // before any listener binds: a reserve that leaves no ordinary slot is a
    // config error, not a server that sheds everything.
    let admission = match admission::AdmissionConfig::from_env(config.max_concurrency) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("config error: {e}");
            std::process::exit(2);
        }
    };
    if admission != admission::AdmissionConfig::legacy(config.max_concurrency) {
        tracing::info!(
            ceiling = admission.ceiling,
            control_reserve = admission.control_reserve,
            heavy_limit = admission.heavy_limit,
            "admission by work class enabled (per plane)"
        );
    }

    let state = match AppState::new_with_admission(config.clone(), admission).await {
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
    let mut grpc_server = tonic::transport::Server::builder();
    if let Some(settings) = &config.platform {
        let tls = platform_tls::grpc_tls(settings)
            .unwrap_or_else(|_| startup_failure("invalid platform gRPC TLS configuration"));
        grpc_server = grpc_server
            .tls_config(tls)
            .unwrap_or_else(|_| startup_failure("invalid platform gRPC TLS configuration"));
    }
    let mut tasks = tokio::task::JoinSet::new();
    tokio::spawn(vocabulary_api::worker(state.clone()));
    // The replica heartbeat (ClearView SCALE-3 and LOGIN1): written now and
    // every 15 s, so the scale report and the console cutover see this
    // process. Never fatal.
    replica_fleet::spawn_heartbeat(&state);

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
        let shutdown = shutdown.clone();
        if let Some(settings) = config.platform.clone() {
            let listener = platform_tls::MtlsListener::new(listener, settings)
                .unwrap_or_else(|_| startup_failure("invalid platform REST TLS configuration"));
            tracing::info!(%addr, "REST plane listening");
            tasks.spawn(async move {
                let result = axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<platform_tls::PeerInfo>(),
                )
                .with_graceful_shutdown(shutdown.wait())
                .await;
                ("REST", result.map_err(|e| e.to_string()))
            });
        } else {
            tracing::info!(%addr, "REST plane listening");
            tasks.spawn(async move {
                let result = axum::serve(listener, app)
                    .with_graceful_shutdown(shutdown.wait())
                    .await;
                ("REST", result.map_err(|e| e.to_string()))
            });
        }
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
        tasks.spawn(async move {
            let result = grpc_server
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
                .await;
            ("gRPC", result.map_err(|e| e.to_string()))
        });
    }

    // ops
    {
        let addr = config.ops_addr.clone();
        let app = ops::router(state.clone());
        if let Ok(listener) = tokio::net::TcpListener::bind(&addr).await {
            tracing::info!(%addr, "ops listening");
            let shutdown = shutdown.clone();
            tasks.spawn(async move {
                let result = axum::serve(listener, app)
                    .with_graceful_shutdown(shutdown.wait())
                    .await;
                ("ops", result.map_err(|e| e.to_string()))
            });
        } else {
            tracing::warn!(%addr, "ops port unavailable; continuing without it");
        }
    }

    let grace = std::time::Duration::from_secs(state.config.shutdown_grace_secs);
    // SCALE-2: the builder loop, where enabled, stops claiming on the same
    // signal and finishes or releases its claim within the same grace window,
    // while the planes drain. A builder still working at the deadline is
    // aborted and its claim lapses as after a crash.
    let builder = {
        let state = state.clone();
        let shutdown = shutdown.clone();
        async move {
            if datastore_jobs::has_builder(&state) {
                shutdown.wait().await;
                datastore_jobs::drain_builder(&state, grace).await;
            }
        }
    };
    let (failed, ()) = tokio::join!(
        supervision::supervise(tasks, shutdown, &state.draining, grace),
        builder
    );
    if failed {
        std::process::exit(1);
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
struct Shutdown {
    receiver: tokio::sync::watch::Receiver<bool>,
    sender: tokio::sync::watch::Sender<bool>,
}

impl Shutdown {
    fn install() -> std::io::Result<Self> {
        let (fired, rx) = tokio::sync::watch::channel(false);
        let sender = fired.clone();
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
        Ok(Self {
            receiver: rx,
            sender,
        })
    }

    fn request(&self) {
        self.sender.send_replace(true);
    }

    /// Resolves once a platform signal or the serving supervisor requests shutdown.
    async fn wait(mut self) {
        let _ = self.receiver.wait_for(|fired| *fired).await;
    }
}
