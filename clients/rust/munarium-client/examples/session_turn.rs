// SPDX-License-Identifier: Apache-2.0
//! Multiturn sessions with a live progress stream: open a session on a
//! runbook, take one streaming turn (progress events as retrieval/merge/
//! completion stages land, then the full answer), and close the session.
//! Requires an applied v2 runbook with collections + an active index.
//!
//!   MUNARIUM_REST_URL=http://127.0.0.1:18080 MUNARIUM_TOKEN=devtoken \
//!   MUNARIUM_UID=user-1 cargo run --example session_turn -- ent-support "vacation policy"

use futures_util::StreamExt;
use munarium_client::{dto, MunariumClient, MunariumClientOptions, TurnStreamEvent};

#[tokio::main(flavor = "current_thread")]
async fn main() -> munarium_client::Result<()> {
    let base =
        std::env::var("MUNARIUM_REST_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".into());
    let token = std::env::var("MUNARIUM_TOKEN").unwrap_or_else(|_| "devtoken".into());
    let uid = std::env::var("MUNARIUM_UID").unwrap_or_else(|_| "example-user".into());
    let mut args = std::env::args().skip(1);
    let runbook = args.next().unwrap_or_else(|| "ent-support".into());
    let query = args.next().unwrap_or_else(|| "vacation".into());

    let client = MunariumClient::rest(MunariumClientOptions::new(base).token(token).uid(uid))?;

    let session = client.sessions.create(&runbook).await?;
    println!(
        "session {} on {} (permitted: {:?})",
        session.session_id, session.runbook_ref, session.permitted_collections
    );

    // The streaming turn: progress events narrate the stages, `done`
    // carries the same TurnResponse the unary route returns.
    let mut stream = client
        .sessions
        .turn_stream(
            &session.session_id,
            dto::TurnRequest {
                query,
                ..Default::default()
            },
        )
        .await?;
    while let Some(event) = stream.next().await {
        match event? {
            TurnStreamEvent::Progress(p) => println!("  … {p:?}"),
            TurnStreamEvent::Done(turn) => {
                println!(
                    "turn {} searched {:?}",
                    turn.ordinal, turn.collections_searched
                );
                for hit in turn.hits.iter().take(3) {
                    println!(
                        "  [{}] {} — {:.3}",
                        hit.collection, hit.source_path, hit.score
                    );
                }
                if let Some(c) = &turn.completion {
                    println!("  {} ({}): {}", c.provider, c.model, c.text);
                }
            }
        }
    }

    let closed = client.sessions.close(&session.session_id).await?;
    println!("session closed: {}", closed.state);
    Ok(())
}
