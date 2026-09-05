// SPDX-License-Identifier: Apache-2.0
//! The head-conflict write loop: expected_head, fresh idempotency keys per
//! attempt, and disputed-is-not-an-error.
//!
//!   MUNARIUM_REST_URL=http://127.0.0.1:18080 MUNARIUM_TOKEN=devtoken \
//!     cargo run --example write_loop

use munarium_client::{dto, ClaimOutcome, MunariumClient, MunariumClientOptions, WriteLoopOptions};

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

    // The loop reads head, builds the request against it, and retries only on
    // head-conflict — with a FRESH idempotency key per rebuilt attempt.
    let outcome = client
        .propose_claim_with_retry(
            &v,
            |_head| dto::ProposeClaimRequest {
                expected_head: None, // the loop fills this in
                claim_type: dto::ClaimTypeDto::Fact,
                subject: "hero".into(),
                key: "eyes".into(),
                value: "green".into(),
                scope_path: Some("ch1".into()),
                provenance: None,
                supersedes_id: None,
                entity_id: None,
                evidence: None,
                confidence: None,
                shape_ref: None,
                origin: None,
            },
            WriteLoopOptions::default(),
        )
        .await?;
    println!(
        "accepted: {} (seq {})",
        outcome.claim.normalized_text, outcome.claim.seq
    );

    // A conflicting plain claim is SUCCESS with status disputed — the
    // governance record, not an error.
    let disputed = client
        .propose_claim_with_retry(
            &v,
            |_head| dto::ProposeClaimRequest {
                expected_head: None,
                claim_type: dto::ClaimTypeDto::Fact,
                subject: "hero".into(),
                key: "eyes".into(),
                value: "blue".into(),
                scope_path: Some("ch2".into()),
                provenance: None,
                supersedes_id: None,
                entity_id: None,
                evidence: None,
                confidence: None,
                shape_ref: None,
                origin: None,
            },
            WriteLoopOptions::default(),
        )
        .await?;
    assert!(disputed.is_disputed(), "conflict must be recorded disputed");
    for f in disputed.findings() {
        println!("finding: {} — {}", f.rule_id, f.message);
    }
    Ok(())
}
