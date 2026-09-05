// SPDX-License-Identifier: Apache-2.0
//! Bulk upload sessions: declare a manifest, upload only what the server
//! says it is owed, and finalize with server-side verification. An identical
//! re-run uploads zero bytes. Requires an ingest-scoped token (or rw) and
//! an applied runbook whose `sources:` matchers cover the filenames.
//!
//!   MUNARIUM_REST_URL=http://127.0.0.1:18080 MUNARIUM_TOKEN=devtoken \
//!   MUNARIUM_UID=loader cargo run --example bulk_upload

use base64::Engine as _;
use munarium_client::{dto, MunariumClient, MunariumClientOptions};
use sha2::Digest as _;

fn entry(filename: &str, text: &str) -> dto::BulkManifestEntry {
    dto::BulkManifestEntry {
        filename: filename.into(),
        sha256: hex::encode(sha2::Sha256::digest(text.as_bytes())),
        bytes_len: text.len() as u64,
        media_type: "text/markdown".into(),
    }
}

fn file(filename: &str, text: &str) -> dto::IngestFileRequest {
    dto::IngestFileRequest {
        filename: filename.into(),
        media_type: "text/markdown".into(),
        content_base64: base64::engine::general_purpose::STANDARD.encode(text),
        sha256: None,
        collections: None,
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> munarium_client::Result<()> {
    let base =
        std::env::var("MUNARIUM_REST_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".into());
    let token = std::env::var("MUNARIUM_TOKEN").unwrap_or_else(|_| "devtoken".into());
    let uid = std::env::var("MUNARIUM_UID").unwrap_or_else(|_| "loader".into());
    let client = MunariumClient::rest(MunariumClientOptions::new(base).token(token).uid(uid))?;

    let docs = [
        ("bulkdocs/alpha.md", "Alpha: the treaty was signed."),
        ("bulkdocs/beta.md", "Beta: the harbor closed in March."),
        ("bulkdocs/gamma.md", "Gamma: the assembly dissolved."),
    ];

    // 1. Open with the full manifest; the server diffs against stored
    //    sources and answers with the upload work list.
    let open = client
        .ingest
        .bulk_open(dto::BulkOpenRequest {
            files: docs.iter().map(|(n, t)| entry(n, t)).collect(),
            label: Some("example".into()),
        })
        .await?;
    println!(
        "session {}: {} total, {} already present, {} needed",
        open.bulk_id,
        open.total,
        open.already_present,
        open.needed.len()
    );

    // 2. Upload ONLY what is owed (≤500 files per chunk).
    if !open.needed.is_empty() {
        let chunk: Vec<_> = docs
            .iter()
            .filter(|(n, _)| open.needed.iter().any(|need| need == n))
            .map(|(n, t)| file(n, t))
            .collect();
        let resp = client.ingest.bulk_chunk(&open.bulk_id, chunk).await?;
        println!(
            "chunk: {} stored, {} failed, {} pending",
            resp.stored, resp.failed, resp.pending
        );
        for r in resp.results.iter().filter(|r| r.error.is_some()) {
            println!("  FAILED {}: {:?}", r.filename, r.error);
        }
    }

    // 3. Finalize: the server verifies every manifest entry is stored and
    //    hash-matched; `incomplete` names exactly what is still owed.
    let done = client.ingest.bulk_complete(&open.bulk_id).await?;
    println!(
        "complete: {} ({} stored, {} missing, {} mismatched)",
        done.status, done.stored, done.missing_count, done.mismatched_count
    );
    Ok(())
}
