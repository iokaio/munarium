// SPDX-License-Identifier: Apache-2.0
//! Content-addressed ingest: stream chunks, verify the declared hash, record
//! the ingest event. Requires the postgres store.
//!
//!   MUNARIUM_REST_URL=http://127.0.0.1:18080 MUNARIUM_TOKEN=devtoken \
//!     cargo run --example ingest_stream

use munarium_client::{chunks_from_vec, dto, MunariumClient, MunariumClientOptions, SourceMeta};
use sha2::Digest;

#[tokio::main(flavor = "current_thread")]
async fn main() -> munarium_client::Result<()> {
    let base =
        std::env::var("MUNARIUM_REST_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".into());
    let token = std::env::var("MUNARIUM_TOKEN").unwrap_or_else(|_| "devtoken".into());
    let client = MunariumClient::rest(MunariumClientOptions::new(base).token(token))?;

    let bytes = b"ticket-1: the printer prints nothing but sadness".to_vec();
    let declared = hex::encode(sha2::Sha256::digest(&bytes));

    // A replayable chunk SOURCE: the transport calls it once per attempt,
    // so a transient failure retries with a fresh stream.
    let source = chunks_from_vec(bytes.chunks(16).map(|c| c.to_vec()).collect());
    let resp = client
        .ingest
        .put_source(
            SourceMeta {
                declared_sha256: declared.clone(),
                media_type: Some("text/plain".into()),
                filename: Some("ticket-1.txt".into()),
                shape_ref: None,
            },
            source,
        )
        .await?;
    assert_eq!(resp.content_hash, declared, "server verifies before commit");
    println!(
        "stored {} bytes at {} (already_existed: {})",
        resp.bytes_len, resp.content_hash, resp.already_existed
    );

    let v = client
        .commands
        .create_version(Default::default(), None)
        .await?
        .version_id;
    let recorded = client
        .ingest
        .record_ingest(
            &v,
            dto::RecordIngestRequest {
                content_hash: resp.content_hash,
                shape_ref: None,
            },
        )
        .await?;
    println!(
        "ingest recorded: event {} at seq {}",
        recorded.event_id, recorded.seq
    );
    Ok(())
}
