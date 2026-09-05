// SPDX-License-Identifier: Apache-2.0
//! The mirror exporter, against real committed chunks.
//!
//! Builds a real collection index through the ordinary path, then exports it
//! and checks the export against the rows it came from. Doing it any other way
//! — a hand-inserted fixture — would test the query rather than the claim that
//! an export reproduces what the index actually holds.

use munarium_retrieval_pg::PgRetrieval;
use munarium_store_pg::PgStore;

fn url() -> Option<String> {
    std::env::var("MUNARIUM_TEST_DATABASE_URL").ok()
}

macro_rules! guard {
    () => {
        if url().is_none() {
            eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
            return;
        }
    };
}

fn unique(p: &str) -> String {
    format!("{p}-{}", uuid::Uuid::new_v4().simple())
}

/// Build a real collection with `n` documents and return (retrieval, id, version).
async fn seeded_with(tenant: &str, docs: usize, max_chars: usize) -> (PgRetrieval, String, String) {
    let url = url().expect("guarded");
    let store = PgStore::connect(&url, tenant).await.unwrap();
    let r = PgRetrieval::new(store.pool().clone(), tenant);

    let name = unique("col");
    let col = r
        .ensure_collection(&name, "para", 0, &[], Some("export test"))
        .await
        .unwrap();

    for i in 0..docs {
        let body = format!(
            "Document {i}. The continental congress met in Philadelphia and debated supply.\n\n\
             A second paragraph about the destruction of the tea in Boston harbour."
        );
        let (source_id, _, _) = r
            .put_source(
                "",
                "text/markdown",
                &format!("corpus/doc-{i}.md"),
                Some("para"),
                body.as_bytes(),
            )
            .await
            .unwrap();
        r.bind_source(&col.id, &source_id, None).await.unwrap();
    }

    let version = r
        .build_collection_index(&col.id, max_chars, 1, true)
        .await
        .unwrap();
    (r, col.id, version.id)
}

async fn seeded(tenant: &str, docs: usize) -> (PgRetrieval, String, String) {
    seeded_with(tenant, docs, 400).await
}

/// The core claim: an export reproduces exactly the chunks the index holds, in
/// stable order, with the stored text and the stored embedding.
#[tokio::test]
async fn an_export_reproduces_the_committed_chunks_in_stable_order() {
    guard!();
    let (r, col, version) = seeded("tenant-export-a", 3).await;

    let mut seen = Vec::new();
    let stats = r
        .export_collection_chunks(&col, &version, |c| {
            seen.push(c);
            Ok(())
        })
        .await
        .unwrap();

    assert!(stats.complete);
    assert_eq!(stats.chunks as usize, seen.len());
    assert_eq!(stats.sources, 3, "three distinct sources");
    assert_eq!(
        stats.with_embedding, stats.chunks,
        "the built-in embedder embeds every chunk"
    );

    // Stable order, and no duplicates: the walk is keyset-paginated on the
    // primary key, so it must be a strict ascending sequence.
    let ids: Vec<&str> = seen.iter().map(|c| c.chunk_id.as_str()).collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(ids, sorted, "export order must be chunk_id ascending");
    let mut deduped = sorted.clone();
    deduped.dedup();
    assert_eq!(deduped.len(), ids.len(), "no chunk exported twice");

    // Against the rows themselves.
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM collection_chunks
          WHERE tenant_id = $1 AND collection_id = $2 AND index_version_id = $3",
    )
    .bind("tenant-export-a")
    .bind(&col)
    .bind(&version)
    .fetch_one(r.pool())
    .await
    .unwrap();
    assert_eq!(count as u64, stats.chunks, "the export covered every row");

    let first = &seen[0];
    assert!(!first.text.is_empty());
    assert_eq!(first.embedding.as_ref().unwrap().len(), 256);
    // The text hash is a statement about the bytes exported.
    use sha2::Digest as _;
    let want: [u8; 32] = sha2::Sha256::digest(first.text.as_bytes()).into();
    assert_eq!(first.text_sha256, want);
}

/// The source path is captured and frozen. This is what lets a pinned artifact
/// resolve its citations after the source has been renamed or deleted.
#[tokio::test]
async fn the_source_path_is_captured_at_export_time() {
    guard!();
    let (r, col, version) = seeded("tenant-export-b", 2).await;

    let mut paths = Vec::new();
    r.export_collection_chunks(&col, &version, |c| {
        paths.push(c.source_path);
        Ok(())
    })
    .await
    .unwrap();

    assert!(
        paths.iter().all(|p| p.starts_with("corpus/doc-")),
        "the logical path must come through, got {paths:?}"
    );
}

/// A pagination boundary is where an off-by-one hides. Enough documents to
/// cross at least one page proves the keyset walk advances rather than
/// re-reading or skipping.
#[tokio::test]
async fn the_walk_crosses_page_boundaries_without_gaps_or_repeats() {
    guard!();
    // EXPORT_PAGE is 500. A small max_chars splits each document into several
    // chunks, so 200 documents crosses the boundary without paying for 600
    // source uploads. The assertion below refuses to let this pass vacuously if
    // the chunker ever stops splitting.
    let (r, col, version) = seeded_with("tenant-export-c", 200, 48).await;

    let mut ids = Vec::new();
    let stats = r
        .export_collection_chunks(&col, &version, |c| {
            ids.push(c.chunk_id);
            Ok(())
        })
        .await
        .unwrap();

    assert!(
        stats.chunks > munarium_retrieval_pg::export::EXPORT_PAGE as u64,
        "the fixture must actually cross a page boundary, got {} chunks",
        stats.chunks
    );
    let mut deduped = ids.clone();
    deduped.sort();
    deduped.dedup();
    assert_eq!(deduped.len(), ids.len(), "a chunk was exported twice");
    assert_eq!(deduped.len() as u64, stats.chunks);

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM collection_chunks
          WHERE tenant_id = $1 AND collection_id = $2 AND index_version_id = $3",
    )
    .bind("tenant-export-c")
    .bind(&col)
    .bind(&version)
    .fetch_one(r.pool())
    .await
    .unwrap();
    assert_eq!(count as u64, stats.chunks, "no row was skipped");
}

/// A caller that stops must not be able to mistake a partial export for a whole
/// one: sealing it would produce an artifact claiming to be a version it does
/// not contain, and every checksum would still pass.
#[tokio::test]
async fn a_caller_that_stops_early_gets_an_error_not_a_partial_success() {
    guard!();
    let (r, col, version) = seeded("tenant-export-d", 3).await;

    let mut n = 0;
    let outcome = r
        .export_collection_chunks(&col, &version, |_| {
            n += 1;
            if n == 2 {
                return Err(munarium_core::KernelError::InvalidInput("stop".into()));
            }
            Ok(())
        })
        .await;

    assert!(
        outcome.is_err(),
        "stopping early must surface as an error, not as complete: false"
    );
    assert_eq!(n, 2);
}

/// An index with no chunks is not an empty artifact to build. Mirroring it
/// would seal an index that answers every query with nothing and looks
/// identical to a broken one.
#[tokio::test]
async fn an_empty_version_refuses_to_export() {
    guard!();
    let url = url().unwrap();
    let store = PgStore::connect(&url, "tenant-export-e").await.unwrap();
    let r = PgRetrieval::new(store.pool().clone(), "tenant-export-e");
    let col = r
        .ensure_collection(&unique("col"), "para", 0, &[], None)
        .await
        .unwrap();

    let err = r
        .export_collection_chunks(&col.id, "idx-nonexistent", |_| Ok(()))
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("nothing to mirror"), "{err}");
}
