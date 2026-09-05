// SPDX-License-Identifier: Apache-2.0
//! munarium-client-conformance — the official Rust client, proven against the
//! MMP conformance scenarios.
//!
//! ```text
//!   --rest <base>   run the 7 wire scenarios through MunariumClient::rest
//!   --grpc <url>    run the 7 wire scenarios through MunariumClient::grpc
//!   --token T       bearer token (default devtoken)
//!   --smoke         additionally run the per-plane smokes (ingest streaming,
//!                   retrieval + envelope, shapes, runbooks approve flow,
//!                   providers). Requires the postgres store; --rest must be
//!                   given (index builds are REST-only).
//!   --mgmt-token M  additionally run the platform smokes (sessions,
//!                   tokens, reports, authoring, bulk upload, SSE turns) —
//!                   needs a mgmt static token on the SAME tenant as --token
//!                   and MUNARIUM_TOKEN_SECRET configured server-side.
//!   --mgmt-env      read the mgmt token from $MUNARIUM_MGMT_TOKEN instead of
//!                   the flag (explicit, because these smokes WRITE).
//! ```
//!
//! The scenarios in `scenarios.rs` are written against the client API and
//! follow `contract/conformance/SCENARIOS.md`, the same text the Python, .NET
//! and Java suites follow. Nothing in this crate depends on the server tree.

mod platform_smoke;
mod scenarios;
mod smoke;

use munarium_client::{MunariumClient, MunariumClientOptions};

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn print_report(label: &str, results: &[(&'static str, scenarios::ScenarioResult)]) -> usize {
    let mut failed = 0;
    println!("munarium-client conformance — {label}");
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

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = flag(&args, "--rest").or_else(|| {
        std::env::var("MUNARIUM_REST_URL")
            .ok()
            .filter(|_| args.iter().any(|a| a == "--rest-env"))
    });
    let grpc = flag(&args, "--grpc");
    let token = flag(&args, "--token")
        .or_else(|| std::env::var("MUNARIUM_TOKEN").ok())
        .unwrap_or_else(|| "devtoken".into());
    let smoke = args.iter().any(|a| a == "--smoke");
    // The platform smokes WRITE (runbooks, ingests, token mint/revoke,
    // a permanent soft-removal), so they never run off ambient environment
    // alone: --mgmt-token is explicit, and --mgmt-env explicitly opts into
    // reading $MUNARIUM_MGMT_TOKEN (mirroring --rest-env).
    let mgmt_token = flag(&args, "--mgmt-token").or_else(|| {
        std::env::var("MUNARIUM_MGMT_TOKEN")
            .ok()
            .filter(|_| args.iter().any(|a| a == "--mgmt-env"))
    });

    if rest.is_none() && grpc.is_none() {
        eprintln!(
            "usage: munarium-client-conformance [--rest <base>] [--grpc <url>] [--token T] [--smoke]"
        );
        std::process::exit(2);
    }

    let mut failed = 0;
    let mut rest_client: Option<MunariumClient> = None;
    let mut grpc_client: Option<MunariumClient> = None;

    if let Some(base) = &rest {
        let client = MunariumClient::rest(
            MunariumClientOptions::new(base.clone())
                .token(token.clone())
                .uid("conformance"),
        )
        .expect("rest client builds");
        let results = scenarios::run_all(&client).await;
        failed += print_report(&format!("REST ({base})"), &results);
        rest_client = Some(client);
    }
    if let Some(url) = &grpc {
        match MunariumClient::grpc(
            MunariumClientOptions::new(url.clone())
                .token(token.clone())
                .uid("conformance"),
        )
        .await
        {
            Ok(client) => {
                let results = scenarios::run_all(&client).await;
                failed += print_report(&format!("gRPC ({url})"), &results);
                grpc_client = Some(client);
            }
            Err(e) => {
                eprintln!("FAIL: gRPC connect: {e}");
                failed += 1;
            }
        }
    }

    if smoke {
        match &rest_client {
            Some(rest) => {
                failed += smoke::run(rest, grpc_client.as_ref()).await;
            }
            None => {
                eprintln!("FAIL: --smoke requires --rest (index builds are REST-only)");
                failed += 1;
            }
        }
    }

    if let Some(mgmt) = &mgmt_token {
        match &rest {
            Some(base) => {
                failed += platform_smoke::run(base, grpc.as_deref(), &token, mgmt).await;
            }
            None => {
                eprintln!(
                    "FAIL: --mgmt-token requires --rest (the platform surface is REST-first)"
                );
                failed += 1;
            }
        }
    }

    if failed > 0 {
        std::process::exit(1);
    }
}
