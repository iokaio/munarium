// SPDX-License-Identifier: Apache-2.0
//! Hybrid search — and the ProvenanceEnvelope every answer carries. Render
//! it; don't hide it. Requires the postgres store and a built index.
//!
//!   MUNARIUM_REST_URL=http://127.0.0.1:18080 MUNARIUM_TOKEN=devtoken \
//!     cargo run --example retrieval_envelope -- support-tickets@1 "printer"

use munarium_client::{dto, MunariumClient, MunariumClientOptions};

#[tokio::main(flavor = "current_thread")]
async fn main() -> munarium_client::Result<()> {
    let base =
        std::env::var("MUNARIUM_REST_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".into());
    let token = std::env::var("MUNARIUM_TOKEN").unwrap_or_else(|_| "devtoken".into());
    let mut args = std::env::args().skip(1);
    let shape_ref = args.next().unwrap_or_else(|| "support-tickets@1".into());
    let query = args.next().unwrap_or_else(|| "printer".into());

    let client = MunariumClient::rest(MunariumClientOptions::new(base).token(token))?;
    let resp = client
        .retrieval
        .search(dto::SearchRequest {
            query: Some(query),
            shape_ref: Some(shape_ref),
            top_k: Some(5),
            ..Default::default()
        })
        .await?;

    for hit in &resp.hits {
        println!(
            "[{:.3}] {} — {}",
            hit.score,
            hit.chunk_id,
            hit.text.chars().take(80).collect::<String>()
        );
    }
    // The envelope makes the answer reproducible: which chunks, which
    // sources, which immutable index, at which ledger watermark.
    let e = &resp.envelope;
    println!(
        "\nProvenanceEnvelope: index {} @ watermark {} — {} chunks from {} sources{}",
        e.index_version,
        e.event_watermark,
        e.chunk_ids.len(),
        e.source_ids.len(),
        e.provider_fingerprint
            .as_deref()
            .map(|f| format!(" (embeddings: {f})"))
            .unwrap_or_default()
    );
    // A hash never told you WHICH document answered; the path does.
    for (path, hash) in e.source_paths.iter().zip(&e.source_content_hashes) {
        println!("  {path}  ({})", &hash[..12.min(hash.len())]);
    }
    Ok(())
}
