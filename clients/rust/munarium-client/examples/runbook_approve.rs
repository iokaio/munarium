// SPDX-License-Identifier: Apache-2.0
//! The runbook approve flow: run → awaiting_approval → approve → done.
//! Requires the postgres store, an applied shape, and an applied runbook
//! (see server/runbooks/pipelines/tickets-reindex.yaml).
//!
//!   MUNARIUM_REST_URL=http://127.0.0.1:18080 MUNARIUM_TOKEN=devtoken \
//!     cargo run --example runbook_approve -- tickets-reindex

use munarium_client::{MunariumClient, MunariumClientOptions};

#[tokio::main(flavor = "current_thread")]
async fn main() -> munarium_client::Result<()> {
    let base =
        std::env::var("MUNARIUM_REST_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".into());
    let token = std::env::var("MUNARIUM_TOKEN").unwrap_or_else(|_| "devtoken".into());
    let runbook = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "tickets-reindex".into());

    let client = MunariumClient::rest(MunariumClientOptions::new(base).token(token))?;
    let run = client.runbooks.run_runbook(&runbook, None).await?;
    println!("run {} started: {}", run.run_id, run.state);

    let status = client.runbooks.get_run(&run.run_id).await?;
    for step in &status.steps {
        println!("  step {} {} — {}", step.ordinal, step.name, step.state);
    }

    if status.state == "awaiting_approval" {
        let awaiting = status
            .steps
            .iter()
            .find(|s| s.state == "awaiting_approval")
            .expect("a step is awaiting approval");
        println!("approving step {} ({})...", awaiting.ordinal, awaiting.name);
        let done = client
            .runbooks
            .approve_step(&run.run_id, awaiting.ordinal)
            .await?;
        println!("run {}: {}", done.run_id, done.state);
    }
    Ok(())
}
