// SPDX-License-Identifier: Apache-2.0
//! Point-in-time pins: one as_of_seq bounds facts, anchors, promises, and
//! counters together; a promise fulfilled after the pin reads back OPEN.
//!
//!   MUNARIUM_REST_URL=http://127.0.0.1:18080 MUNARIUM_TOKEN=devtoken \
//!     cargo run --example pins

use munarium_client::{dto, FactsQuery, MunariumClient, MunariumClientOptions};

#[tokio::main(flavor = "current_thread")]
async fn main() -> munarium_client::Result<()> {
    let base =
        std::env::var("MUNARIUM_REST_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".into());
    let token = std::env::var("MUNARIUM_TOKEN").unwrap_or_else(|_| "devtoken".into());
    let client = MunariumClient::rest(MunariumClientOptions::new(base).token(token))?;

    let v = client
        .commands
        .create_version(Default::default(), None)
        .await?
        .version_id;

    let fact = |s: &str, k: &str, val: &str| dto::ProposeClaimRequest {
        expected_head: None,
        claim_type: dto::ClaimTypeDto::Fact,
        subject: s.into(),
        key: k.into(),
        value: val.into(),
        scope_path: None,
        provenance: None,
        supersedes_id: None,
        entity_id: None,
        evidence: None,
        confidence: None,
        shape_ref: None,
        origin: None,
    };

    client
        .commands
        .propose_claim(&v, fact("hero", "eyes", "green"), None)
        .await?; // seq 1
    client
        .commands
        .open_promise(
            &v,
            dto::OpenPromiseRequest {
                key: "reveal".into(),
                kind: "setup".into(),
                description: "open the letter".into(),
                origin_scope: Some("ch1".into()),
                due_scope: Some("ch3".into()),
            },
            None,
        )
        .await?;
    client
        .commands
        .propose_claim(&v, fact("hero", "home", "harbor"), None)
        .await?; // seq 2
    client.commands.fulfill_promise(&v, "reveal", None).await?;

    // Pinned at 1: only the first fact; the promise reads back OPEN.
    let pinned = client
        .query
        .facts(
            &v,
            FactsQuery {
                as_of_seq: Some(1),
                ..Default::default()
            },
        )
        .await?;
    println!("facts at pin 1: {}", pinned.facts.len());
    let promises = client.query.promises(&v, Some(1), None).await?;
    println!("promise status at pin 1: {}", promises.promises[0].status);
    let head = client.query.promises(&v, None, None).await?;
    println!("promise status at head:  {}", head.promises[0].status);

    // Composition under the same pin (digests are REBUILT, never served).
    let ctx = client
        .query
        .compose_context(
            &v,
            munarium_client::ContextQuery {
                as_of_seq: Some(1),
                ..Default::default()
            },
        )
        .await?;
    println!(
        "composed at pin 1 ({} tokens):\n{}",
        ctx.estimated_tokens, ctx.text
    );
    Ok(())
}
