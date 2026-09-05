// SPDX-License-Identifier: Apache-2.0
//! Streaming export of already-committed chunks, for mirror builds.
//!
//! The first datastore artifacts are built from chunks PostgreSQL already
//! holds — the exact stored text and the exact stored embedding. Extraction,
//! OCR, chunking and embedding do **not** run again (§7.1). That is what makes
//! shadow comparison meaningful: a difference between the two engines is then
//! attributable to the engine, not to content preparation that happened twice
//! and drifted.
//!
//! ## Why this is paginated rather than one query
//!
//! A collection can hold millions of chunks, each carrying its full text and a
//! 256-dimension vector. Materializing that in one `fetch_all` would size the
//! builder's memory to the largest collection anyone ever indexes. Keyset
//! pagination on `chunk_id` keeps memory bounded by the page, and because
//! `chunk_id` is the primary key within a version, the walk is stable and
//! resumable rather than depending on an OFFSET that shifts under concurrent
//! writes.
//!
//! ## The source path is captured, not referenced
//!
//! `collection_chunks` stores no path; it lives on `sources`, which is mutable.
//! The export reads it once and freezes it into the artifact, so a historical
//! exact-version read still resolves its citations after the source has been
//! re-ingested, renamed or deleted. Freezing a mutable value is the point here,
//! not an oversight.

use sha2::Digest as _;
use sqlx::Row;

use munarium_core::{KernelError, Result};

use crate::{storage_err, PgRetrieval};

/// One chunk as it was committed, ready to become a `PreparedChunk`.
///
/// Deliberately NOT `munarium_datastore::PreparedChunk`: this crate is the
/// PostgreSQL reference implementation and must not acquire a dependency on the
/// datastore crate to describe its own rows. The coordinator maps between them.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportedChunk {
    pub chunk_id: String,
    pub source_id: String,
    /// Captured from `sources` at export time and frozen into the artifact.
    pub source_path: String,
    pub source_hash: String,
    pub ordinal: u32,
    pub text: String,
    /// SHA-256 of the text, computed here rather than stored: the chunk tables
    /// never held one, and a hash computed at export is a statement about the
    /// bytes that were exported, which is exactly the claim a citation needs.
    pub text_sha256: [u8; 32],
    /// `None` for a collection built without vectors, which is legitimate.
    pub embedding: Option<Vec<f32>>,
}

/// What a completed export covered.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportStats {
    pub chunks: u64,
    pub sources: u64,
    pub with_embedding: u64,
    /// False when the caller stopped early. A partial export must never be
    /// sealed as an artifact: it would claim to be a version it does not
    /// contain, and every checksum would still pass.
    pub complete: bool,
}

/// How many rows one page pulls. Large enough that the round trips do not
/// dominate, small enough that a page of full chunk text and 256-float vectors
/// stays well inside a builder's budget.
pub const EXPORT_PAGE: i64 = 500;

impl PgRetrieval {
    /// Stream a collection index version's committed chunks in stable order.
    ///
    /// `on_chunk` is called once per chunk, in `chunk_id` order. Returning
    /// `Err` stops the walk and the returned stats say `complete: false` — a
    /// caller that ignored that and sealed anyway would publish an artifact
    /// missing whatever came after the failure.
    pub async fn export_collection_chunks<F>(
        &self,
        collection_id: &str,
        index_version_id: &str,
        mut on_chunk: F,
    ) -> Result<ExportStats>
    where
        F: FnMut(ExportedChunk) -> Result<()>,
    {
        let mut after = String::new();
        let mut stats = ExportStats {
            chunks: 0,
            sources: 0,
            with_embedding: 0,
            complete: false,
        };
        let mut seen_sources: std::collections::HashSet<String> = std::collections::HashSet::new();

        loop {
            // LEFT JOIN, not INNER: a chunk whose source row has been deleted
            // still belongs to this immutable version, and dropping it here
            // would silently shrink the artifact relative to the index it
            // mirrors. The path falls back to the source id, which is at least
            // a true statement about where the text came from.
            let rows = sqlx::query(
                "SELECT c.chunk_id, c.source_id, c.source_hash, c.ordinal, c.text,
                        c.embedding,
                        COALESCE(s.filename, c.source_id) AS source_path
                   FROM collection_chunks c
                   LEFT JOIN sources s
                     ON s.tenant_id = c.tenant_id AND s.source_id = c.source_id
                  WHERE c.tenant_id = $1 AND c.collection_id = $2
                    AND c.index_version_id = $3 AND c.chunk_id > $4
                  ORDER BY c.chunk_id
                  LIMIT $5",
            )
            .bind(&self.tenant_id)
            .bind(collection_id)
            .bind(index_version_id)
            .bind(&after)
            .bind(EXPORT_PAGE)
            .fetch_all(&self.pool)
            .await
            .map_err(storage_err)?;

            if rows.is_empty() {
                break;
            }
            for row in rows {
                let chunk_id: String = row.get("chunk_id");
                let text: String = row.get("text");
                let embedding: Option<pgvector::Vector> = row.get("embedding");
                let ordinal: i32 = row.get("ordinal");
                let source_id: String = row.get("source_id");

                if embedding.is_some() {
                    stats.with_embedding += 1;
                }
                if seen_sources.insert(source_id.clone()) {
                    stats.sources += 1;
                }

                let chunk = ExportedChunk {
                    text_sha256: sha2::Sha256::digest(text.as_bytes()).into(),
                    chunk_id: chunk_id.clone(),
                    source_id,
                    source_path: row.get("source_path"),
                    source_hash: row.get("source_hash"),
                    ordinal: u32::try_from(ordinal).map_err(|_| {
                        KernelError::Storage(format!("chunk {chunk_id} has a negative ordinal"))
                    })?,
                    text,
                    embedding: embedding.map(|v| v.to_vec()),
                };
                on_chunk(chunk)?;
                stats.chunks += 1;
                after = chunk_id;
            }
        }

        // A version with no chunks is not an empty artifact to build; it is a
        // version that should not exist, and `verify_collection_index` already
        // refuses it. Saying so here stops a mirror sealing an empty index that
        // would answer every query with nothing.
        if stats.chunks == 0 {
            return Err(KernelError::InvalidInput(format!(
                "index {index_version_id} of collection {collection_id} has no committed chunks; \
                 there is nothing to mirror"
            )));
        }
        // A collection is either fully embedded or not embedded at all. A
        // partial vector leg would rank some chunks and silently exclude
        // others, which looks like a working hybrid search and is not.
        if stats.with_embedding != 0 && stats.with_embedding != stats.chunks {
            return Err(KernelError::Storage(format!(
                "index {index_version_id} has {} of {} chunks embedded; a partial vector leg \
                 would rank some chunks and silently exclude the rest",
                stats.with_embedding, stats.chunks
            )));
        }

        stats.complete = true;
        Ok(stats)
    }

    /// The same, for the legacy shape-scoped `index_chunks` table.
    ///
    /// Kept separate rather than parameterized over a table name: the two
    /// tables have different keys and different histories, and a string
    /// substituted into a FROM clause is a habit worth not starting.
    pub async fn export_legacy_chunks<F>(
        &self,
        index_version_id: &str,
        mut on_chunk: F,
    ) -> Result<ExportStats>
    where
        F: FnMut(ExportedChunk) -> Result<()>,
    {
        let mut after = String::new();
        let mut stats = ExportStats {
            chunks: 0,
            sources: 0,
            with_embedding: 0,
            complete: false,
        };
        let mut seen_sources: std::collections::HashSet<String> = std::collections::HashSet::new();

        loop {
            let rows = sqlx::query(
                "SELECT c.chunk_id, c.source_id, c.source_hash, c.ordinal, c.text,
                        c.embedding,
                        COALESCE(s.filename, c.source_id) AS source_path
                   FROM index_chunks c
                   LEFT JOIN sources s
                     ON s.tenant_id = c.tenant_id AND s.source_id = c.source_id
                  WHERE c.tenant_id = $1 AND c.index_version_id = $2 AND c.chunk_id > $3
                  ORDER BY c.chunk_id
                  LIMIT $4",
            )
            .bind(&self.tenant_id)
            .bind(index_version_id)
            .bind(&after)
            .bind(EXPORT_PAGE)
            .fetch_all(&self.pool)
            .await
            .map_err(storage_err)?;

            if rows.is_empty() {
                break;
            }
            for row in rows {
                let chunk_id: String = row.get("chunk_id");
                let text: String = row.get("text");
                let embedding: Option<pgvector::Vector> = row.get("embedding");
                let ordinal: i32 = row.get("ordinal");
                let source_id: String = row.get("source_id");

                if embedding.is_some() {
                    stats.with_embedding += 1;
                }
                if seen_sources.insert(source_id.clone()) {
                    stats.sources += 1;
                }
                on_chunk(ExportedChunk {
                    text_sha256: sha2::Sha256::digest(text.as_bytes()).into(),
                    chunk_id: chunk_id.clone(),
                    source_id,
                    source_path: row.get("source_path"),
                    source_hash: row.get("source_hash"),
                    ordinal: u32::try_from(ordinal).map_err(|_| {
                        KernelError::Storage(format!("chunk {chunk_id} has a negative ordinal"))
                    })?,
                    text,
                    embedding: embedding.map(|v| v.to_vec()),
                })?;
                stats.chunks += 1;
                after = chunk_id;
            }
        }

        if stats.chunks == 0 {
            return Err(KernelError::InvalidInput(format!(
                "legacy index {index_version_id} has no committed chunks; nothing to mirror"
            )));
        }
        if stats.with_embedding != 0 && stats.with_embedding != stats.chunks {
            return Err(KernelError::Storage(format!(
                "legacy index {index_version_id} has {} of {} chunks embedded",
                stats.with_embedding, stats.chunks
            )));
        }
        stats.complete = true;
        Ok(stats)
    }
}

/// One source as the index version recorded it, for a reconstructed `BuildSpec`.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportedSource {
    pub source_id: String,
    pub logical_path: String,
    pub content_sha256: String,
    /// `None` when the `sources` row is gone. The caller records the media type
    /// as unknown rather than substituting a plausible one: a reconstructed
    /// spec may be incomplete, and a guess dressed as a record is worse than an
    /// acknowledged blank.
    pub media_type: Option<String>,
}

impl PgRetrieval {
    /// The distinct sources an index version was built from, in stable order.
    ///
    /// Read from the version's own committed chunks rather than from the
    /// collection's CURRENT bindings: a source bound or unbound after the build
    /// is not part of what this version contains, and the artifact must
    /// describe the version it mirrors.
    pub async fn exported_sources(
        &self,
        collection_id: &str,
        index_version_id: &str,
    ) -> Result<Vec<ExportedSource>> {
        let rows = sqlx::query(
            "SELECT DISTINCT c.source_id, c.source_hash,
                    COALESCE(s.filename, c.source_id) AS logical_path,
                    s.media_type
               FROM collection_chunks c
               LEFT JOIN sources s
                 ON s.tenant_id = c.tenant_id AND s.source_id = c.source_id
              WHERE c.tenant_id = $1 AND c.collection_id = $2 AND c.index_version_id = $3
              ORDER BY c.source_id",
        )
        .bind(&self.tenant_id)
        .bind(collection_id)
        .bind(index_version_id)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;

        Ok(rows
            .into_iter()
            .map(|r| ExportedSource {
                source_id: r.get("source_id"),
                logical_path: r.get("logical_path"),
                content_sha256: r.get("source_hash"),
                media_type: r.get("media_type"),
            })
            .collect())
    }

    /// The legacy shape-scoped twin.
    pub async fn exported_legacy_sources(
        &self,
        index_version_id: &str,
    ) -> Result<Vec<ExportedSource>> {
        let rows = sqlx::query(
            "SELECT DISTINCT c.source_id, c.source_hash,
                    COALESCE(s.filename, c.source_id) AS logical_path,
                    s.media_type
               FROM index_chunks c
               LEFT JOIN sources s
                 ON s.tenant_id = c.tenant_id AND s.source_id = c.source_id
              WHERE c.tenant_id = $1 AND c.index_version_id = $2
              ORDER BY c.source_id",
        )
        .bind(&self.tenant_id)
        .bind(index_version_id)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;

        Ok(rows
            .into_iter()
            .map(|r| ExportedSource {
                source_id: r.get("source_id"),
                logical_path: r.get("logical_path"),
                content_sha256: r.get("source_hash"),
                media_type: r.get("media_type"),
            })
            .collect())
    }
}

/// What an existing index version recorded about itself.
///
/// Read from the version row and its own committed chunks, never from the
/// collection's current configuration: a mirror describes the version it is
/// mirroring, and today's extractor set or embedder is a statement about
/// today's builds.
#[derive(Debug, Clone, PartialEq)]
pub struct VersionFacts {
    pub shape_ref: String,
    pub watermark_seq: u64,
    /// The extractor-set version recorded in the version's manifest. `None`
    /// when the manifest predates that field, in which case the reconstructed
    /// spec says `unknown` rather than substituting the current one.
    pub recorded_extractor_version: Option<String>,
    /// Whether this version's chunks carry embeddings at all. A mirror must
    /// know before it starts: a writer declaring vectors refuses the first
    /// chunk that has none, and one declaring none refuses the first that has.
    pub embedded: bool,
    /// `None` for a legacy shape-scoped version.
    pub collection_id: Option<String>,
}

impl PgRetrieval {
    /// Read what an index version recorded about itself.
    pub async fn version_facts(&self, index_version_id: &str) -> Result<VersionFacts> {
        let row = sqlx::query(
            "SELECT shape_ref, watermark_seq, manifest, collection_id
               FROM index_versions WHERE tenant_id = $1 AND id = $2",
        )
        .bind(&self.tenant_id)
        .bind(index_version_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?
        .ok_or_else(|| KernelError::NotFound {
            kind: "index version",
            id: index_version_id.to_string(),
        })?;

        let manifest: serde_json::Value = row.get("manifest");
        let collection_id: Option<String> = row.get("collection_id");
        let watermark: i64 = row.get("watermark_seq");

        let embedded: bool = match &collection_id {
            Some(cid) => sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM collection_chunks
                                WHERE tenant_id = $1 AND collection_id = $2
                                  AND index_version_id = $3 AND embedding IS NOT NULL)",
            )
            .bind(&self.tenant_id)
            .bind(cid)
            .bind(index_version_id)
            .fetch_one(&self.pool)
            .await
            .map_err(storage_err)?,
            None => sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM index_chunks
                                WHERE tenant_id = $1 AND index_version_id = $2
                                  AND embedding IS NOT NULL)",
            )
            .bind(&self.tenant_id)
            .bind(index_version_id)
            .fetch_one(&self.pool)
            .await
            .map_err(storage_err)?,
        };

        Ok(VersionFacts {
            shape_ref: row.get("shape_ref"),
            watermark_seq: u64::try_from(watermark).map_err(|_| {
                KernelError::Storage(format!(
                    "version {index_version_id} has a negative watermark"
                ))
            })?,
            recorded_extractor_version: manifest
                .get("extractors")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            embedded,
            collection_id,
        })
    }
}
