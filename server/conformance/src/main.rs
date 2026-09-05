// SPDX-License-Identifier: Apache-2.0
//! mmp-conformance — fixture runner.
//!
//!   --in-process              in-memory backend (M0)
//!   --postgres <url>          munarium-store-pg, fresh tenant per run (M1)
//!   --http <base> [--token T] black-box over the REST plane (M2)
//!   --grpc <endpoint>         black-box over the direct gRPC plane (M2)
//!   --platform <base> --rw-token T --mgmt-token M
//!                             M7–M12 REST scenarios: uid contract, capability
//!                             tokens, runbook-v2 applications, compartmentalized
//!                             sessions, ingestion, removal, reports. Needs a
//!                             pg-backed server with MUNARIUM_TOKEN_SECRET set and a
//!                             FRESH tenant behind the two tokens.
//!   --cluster <baseA> --peer <baseB> [--token T]
//!                             N-replica scenarios (2026-08-17): two live
//!                             instances sharing ONE postgres and one rw token
//!                             for a FRESH tenant, both with
//!                             MUNARIUM_REGISTRY_TTL_SECS=1. Proves registry
//!                             convergence, shared idempotency, interleaved seq
//!                             allocation, and the run advisory lock.
//!
//! Passing BOTH --http and --grpc is the cross-plane parity check: the same
//! scenario set must go green on each plane.

use mmp_conformance::clients::{GrpcClientStore, RestClientStore};
use munarium_core::storage::StorageBackend;
use munarium_store_mem::MemStore;
use munarium_store_pg::PgStore;

fn print_report(label: &str, results: &[(&'static str, mmp_conformance::ScenarioResult)]) -> usize {
    let mut failed = 0;
    println!("MMP conformance — {label}");
    println!("{}", "-".repeat(56));
    for (name, result) in results {
        match result {
            Ok(()) => println!("  PASS  {name}"),
            Err(msg) => {
                failed += 1;
                println!("  FAIL  {name}\n        {msg}");
            }
        }
    }
    println!("{}", "-".repeat(56));
    println!("{} passed, {failed} failed\n", results.len() - failed);
    failed
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let in_process = args.iter().any(|a| a == "--in-process");
    let pg_url = flag(&args, "--postgres");
    let http = flag(&args, "--http");
    let grpc = flag(&args, "--grpc");
    let token = flag(&args, "--token").unwrap_or_else(|| "devtoken".into());
    let platform = flag(&args, "--platform");
    let cluster = flag(&args, "--cluster");

    if !in_process
        && pg_url.is_none()
        && http.is_none()
        && grpc.is_none()
        && platform.is_none()
        && cluster.is_none()
    {
        eprintln!(
            "usage: mmp-conformance --in-process | --postgres <url> | --http <base> [--grpc <endpoint>] [--token T] | --platform <base> --rw-token T --mgmt-token M | --cluster <baseA> --peer <baseB> [--token T]"
        );
        std::process::exit(2);
    }

    let mut failed = 0;
    if in_process {
        let store = MemStore::new();
        let results = mmp_conformance::run_all(&store).await;
        failed += print_report("in-process (munarium-store-mem)", &results);
    }
    if let Some(url) = pg_url {
        let tenant = format!("conf-{}", uuid_like());
        match PgStore::connect(&url, &tenant).await {
            Ok(store) => {
                let results = mmp_conformance::run_all(&store as &dyn StorageBackend).await;
                failed += print_report("postgres (munarium-store-pg)", &results);
            }
            Err(e) => {
                eprintln!("FAIL: could not connect/migrate postgres: {e}");
                failed += 1;
            }
        }
    }
    if let Some(base) = http {
        let store = RestClientStore::new(&base, &token);
        let results = mmp_conformance::run_all(&store as &dyn StorageBackend).await;
        failed += print_report(&format!("REST plane ({base})"), &results);
    }
    if let Some(endpoint) = grpc {
        match GrpcClientStore::connect(&endpoint, &token).await {
            Ok(store) => {
                let results = mmp_conformance::run_all(&store as &dyn StorageBackend).await;
                failed += print_report(&format!("gRPC plane ({endpoint})"), &results);
            }
            Err(e) => {
                eprintln!("FAIL: {e}");
                failed += 1;
            }
        }
    }
    if let Some(base) = platform {
        let rw = flag(&args, "--rw-token").unwrap_or_else(|| {
            eprintln!("--platform requires --rw-token");
            std::process::exit(2);
        });
        let mgmt = flag(&args, "--mgmt-token").unwrap_or_else(|| {
            eprintln!("--platform requires --mgmt-token");
            std::process::exit(2);
        });
        let env = mmp_conformance::platform::PlatformEnv::new(&base, &rw, &mgmt);
        let results = mmp_conformance::platform::run_all(&env).await;
        failed += print_report(&format!("platform surface ({base})"), &results);
    }
    if let Some(base_a) = cluster {
        let base_b = flag(&args, "--peer").unwrap_or_else(|| {
            eprintln!("--cluster requires --peer <baseB>");
            std::process::exit(2);
        });
        let env = mmp_conformance::cluster::ClusterEnv::new(&base_a, &base_b, &token);
        let results = mmp_conformance::cluster::run_all(&env).await;
        failed += print_report(&format!("cluster ({base_a} + {base_b})"), &results);
    }
    if failed > 0 {
        std::process::exit(1);
    }
}

fn uuid_like() -> String {
    format!(
        "{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}
