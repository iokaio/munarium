// SPDX-License-Identifier: Apache-2.0
//! In-Postgres hybrid retrieval, behind `munarium_core::retrieval::RetrievalBackend`.
//!
//! - A source is identified by its LOGICAL PATH (the caller's filename), and
//!   its bytes live at that path in a `SourceStore` (Azure Blob in
//!   production, Postgres offline). The content hash is verified before
//!   commit and travels with the source as INTEGRITY, not identity — so the
//!   same bytes staged at two paths are two independently bindable sources.
//! - The chunker is deterministic and versioned (`para@1`: blank-line
//!   paragraphs packed to max_chars).
//! - The default embedder is `local-hash@1`: deterministic 256-dim feature
//!   hashing, L2-normalized — the whole pipeline runs keyless; a tenant's
//!   BYOK embedding provider swaps in per index build and is named in
//!   the manifest either way.
//! - index_version = hash(shape_ref, chunker, embedder, sorted source set);
//!   builds are side-by-side and immutable; cutover is the `active` flip;
//!   old versions keep resolving so past envelopes stay verifiable.
//! - Hybrid ranking: reciprocal rank fusion over the lexical and vector
//!   candidate lists (k configurable per shape, default 60).

use async_trait::async_trait;
use munarium_core::docintel::DocumentIntelligence;
use munarium_core::retrieval::*;
use munarium_core::sources::{SourceKey, SourceStore};
use munarium_core::{KernelError, Result};
use pgvector::Vector;
use sha2::Digest as _;
use sqlx::{PgPool, Row};
use std::sync::Arc;

mod collections;
pub mod direct;
pub mod export;
pub mod required;
pub use collections::{
    expand_query, merge_hits, merge_hits_weighted, number_query_digits, pairs_tsquery,
    select_collection_indices, tsquery_lexemes,
};
// Re-exported so this crate's public surface is unchanged by the move into
// munarium-core. Call sites are migrated to the core path deliberately, not by
// a compile error; when none remain, this line goes.
pub use munarium_core::retrieval::{
    CollectionInfo, CollectionSearchResult, ContentDemotionRule, MergeWeights, QueryExpansionRule,
    SearchParams, SourceInfo,
};
// Re-exported so this crate's surface is unchanged by the move. PgSourceStore
// is a STORAGE backend and now lives with the other ones; PgRetrieval::new
// still defaults to it so tests and `docker compose up` need no object store.
pub use munarium_store_pg::PgSourceStore;

pub const CHUNKER_VERSION: &str = "para@1";
pub const LOCAL_EMBEDDER: &str = "local-hash@1";
pub const EMBED_DIMS: usize = 256;

pub(crate) fn storage_err(e: sqlx::Error) -> KernelError {
    KernelError::Storage(e.to_string())
}

/// Reciprocal rank fusion over the lexical and vector candidate rows (each
/// row: chunk_id, source_id, source_hash, text). Shared by the legacy
/// shape-scoped search and the collection search; deterministic tie-break
/// on chunk_id.
///
/// `source_path` is left empty here and filled in by the caller, which is the
/// only layer that can resolve ids to paths.
#[allow(clippy::type_complexity)]
pub(crate) fn rrf_fuse(
    lexical: &[sqlx::postgres::PgRow],
    vector: &[sqlx::postgres::PgRow],
    k: f64,
) -> Vec<SearchHit> {
    #[derive(Default)]
    struct Acc {
        score: f64,
        lexical_rank: Option<u32>,
        vector_rank: Option<u32>,
        lexical_score: Option<f64>,
        vector_distance: Option<f64>,
        source_id: String,
        source_hash: String,
        text: String,
    }
    let mut fused: std::collections::HashMap<String, Acc> = std::collections::HashMap::new();
    let seed = |row: &sqlx::postgres::PgRow| -> Acc {
        Acc {
            source_id: row.get("source_id"),
            source_hash: row.get("source_hash"),
            text: row.get("text"),
            ..Acc::default()
        }
    };
    for (rank, row) in lexical.iter().enumerate() {
        let id: String = row.get("chunk_id");
        let entry = fused.entry(id).or_insert_with(|| seed(row));
        entry.score += 1.0 / (k + rank as f64 + 1.0);
        entry.lexical_rank = Some(rank as u32 + 1);
        entry.lexical_score = Some(row.get::<f32, _>("rank") as f64);
    }
    for (rank, row) in vector.iter().enumerate() {
        let id: String = row.get("chunk_id");
        let entry = fused.entry(id).or_insert_with(|| seed(row));
        entry.score += 1.0 / (k + rank as f64 + 1.0);
        entry.vector_rank = Some(rank as u32 + 1);
        entry.vector_distance = Some(row.get::<f64, _>("distance"));
    }
    let mut hits: Vec<SearchHit> = fused
        .into_iter()
        .map(|(chunk_id, acc)| SearchHit {
            chunk_id,
            source_id: acc.source_id,
            source_path: String::new(),
            source_content_hash: acc.source_hash,
            text: acc.text,
            score: acc.score,
            lexical_rank: acc.lexical_rank,
            vector_rank: acc.vector_rank,
            lexical_score: acc.lexical_score,
            vector_distance: acc.vector_distance,
            metadata: None,
        })
        .collect();
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.chunk_id.cmp(&b.chunk_id))
    });
    hits
}

// ---------------------------------------------------------------------------
// deterministic chunker + local embedder
// ---------------------------------------------------------------------------

/// `para@1`: split on blank lines, pack consecutive paragraphs up to
/// max_chars. Same bytes -> same chunks.
pub fn chunk_text(text: &str, max_chars: usize) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    for para in text.split("\n\n").map(str::trim).filter(|p| !p.is_empty()) {
        if !current.is_empty() && current.len() + para.len() + 2 > max_chars {
            chunks.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(para);
        while current.len() > max_chars {
            let mut split_at = current
                .char_indices()
                .take_while(|(i, _)| *i <= max_chars)
                .last()
                .map(|(i, _)| i)
                .unwrap_or(current.len());
            if split_at == 0 {
                // Forward-progress guarantee: max_chars is smaller than the
                // first character (max_chars 0, or 1 with a multi-byte lead).
                // Take exactly one character — without this the loop pushed
                // empty chunks forever.
                split_at = current
                    .chars()
                    .next()
                    .map(char::len_utf8)
                    .unwrap_or(current.len());
            }
            let rest = current.split_off(split_at);
            chunks.push(std::mem::take(&mut current));
            current = rest;
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// `local-hash@1`: deterministic bag-of-tokens feature hashing into 256 dims,
/// L2-normalized. Not semantically deep — deterministic and keyless, which is
/// what conformance and offline demos need.
pub fn local_embed(text: &str) -> Vec<f32> {
    let mut v = vec![0f32; EMBED_DIMS];
    for token in text.to_lowercase().split(|c: char| !c.is_alphanumeric()) {
        if token.is_empty() {
            continue;
        }
        let h = sha2::Sha256::digest(token.as_bytes());
        let idx = (u16::from_be_bytes([h[0], h[1]]) as usize) % EMBED_DIMS;
        let sign = if h[2] & 1 == 0 { 1.0 } else { -1.0 };
        v[idx] += sign;
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

// ---------------------------------------------------------------------------
// the backend
// ---------------------------------------------------------------------------

/// Cheap to clone (a pool handle, a tenant id, two `Arc`s): the session
/// plane fans one out per collection to search concurrently (2026-08-25).
#[derive(Clone)]
pub struct PgRetrieval {
    pub(crate) pool: PgPool,
    pub(crate) tenant_id: String,
    /// Where document bytes live. Defaults to the Postgres backend so tests
    /// and `docker compose up` need no object store; the server swaps in
    /// an `munarium-store-objects` backend (Azure/S3/GCS/file) from config.
    pub(crate) sources: Arc<dyn SourceStore>,
    /// Turns DOCX/PDF bytes into text at index time.
    pub(crate) extractors: Arc<munarium_extract::ExtractorRegistry>,
    /// Optional escalation for documents local extraction cannot read.
    /// None is the default and a complete configuration.
    pub(crate) doc_intel: Option<Arc<dyn DocumentIntelligence>>,
}

impl PgRetrieval {
    /// Postgres-backed bytes — the offline default.
    pub fn new(pool: PgPool, tenant_id: &str) -> Self {
        let sources = Arc::new(PgSourceStore::new(pool.clone()));
        Self::with_source_store(pool, tenant_id, sources)
    }

    /// Bytes in the given store (an `munarium-store-objects` backend in production).
    pub fn with_source_store(pool: PgPool, tenant_id: &str, sources: Arc<dyn SourceStore>) -> Self {
        Self {
            pool,
            tenant_id: tenant_id.to_string(),
            sources,
            extractors: Arc::new(munarium_extract::ExtractorRegistry::new()),
            doc_intel: None,
        }
    }

    /// Attach a document-intelligence provider. Without one the pipeline is
    /// complete: local extraction still runs, and documents it cannot read
    /// are recorded `empty` rather than silently contributing nothing.
    pub fn with_doc_intel(mut self, provider: Option<Arc<dyn DocumentIntelligence>>) -> Self {
        self.doc_intel = provider;
        self
    }

    pub fn source_store(&self) -> &Arc<dyn SourceStore> {
        &self.sources
    }

    /// The extractor-set version, which joins the index identity so an
    /// extractor improvement forces a rebuild instead of serving stale text.
    pub fn extractor_version(&self) -> String {
        self.extractors.version()
    }

    /// Extract a source's text, escalating to the document-intelligence
    /// provider when local extraction found nothing usable.
    ///
    /// The order is deliberate and is the cost control: local extractors are
    /// free and run on everything; the paid provider is reached only for
    /// documents that produced no text — a scan, or a PDF whose page images
    /// use an encoding no pure-Rust decoder handles. A provider failure is
    /// logged and the local (empty) result stands, so an outage at the vendor
    /// degrades the index rather than failing the build.
    pub(crate) async fn extract_source(
        &self,
        media_type: &str,
        bytes: &[u8],
    ) -> munarium_extract::Extracted {
        // Local extraction is synchronous CPU work — a PDF parse with no time
        // bound, a DOCX inflate and XML walk — and it ran on the async worker
        // that also serves /readyz, heartbeats and every other request on
        // this replica. On the blocking pool instead; the copy of the bytes
        // is what makes the closure `'static`, and it is bounded by the
        // 256 MiB source ceiling.
        let extractors = Arc::clone(&self.extractors);
        let media = media_type.to_string();
        let owned = bytes.to_vec();
        let local =
            match tokio::task::spawn_blocking(move || extractors.extract(&media, &owned)).await {
                Ok(extracted) => extracted,
                // A panic inside an extractor is a failed document, not a failed
                // build — the same rule `ExtractorRegistry::extract` applies to
                // an extractor error.
                Err(e) => munarium_extract::Extracted::empty(
                    munarium_extract::ExtractionMethod::Text,
                    format!("extraction task failed: {e}"),
                ),
            };
        if local.status == munarium_extract::ExtractionStatus::Ok {
            return local;
        }
        let Some(provider) = &self.doc_intel else {
            return local;
        };
        if !provider.supports(media_type) {
            return local;
        }
        match provider.analyze(media_type, bytes).await {
            Ok(analyzed) if !analyzed.is_empty() => {
                tracing::info!(
                    provider = provider.id(),
                    pages = analyzed.pages_analyzed,
                    "document intelligence recovered text local extraction could not"
                );
                munarium_extract::Extracted {
                    text: analyzed.text,
                    status: munarium_extract::ExtractionStatus::Ok,
                    method: munarium_extract::ExtractionMethod::Ocr,
                    pages: Vec::new(),
                    note: Some(analyzed.provider_fingerprint),
                }
            }
            Ok(analyzed) => {
                // The service read it and found nothing. That is an answer,
                // and a truer `empty` than the local one.
                tracing::info!(
                    provider = provider.id(),
                    pages = analyzed.pages_analyzed,
                    "document intelligence found no text either"
                );
                local
            }
            Err(e) => {
                tracing::warn!(
                    provider = provider.id(), error = %e,
                    "document intelligence failed; keeping the local result"
                );
                local
            }
        }
    }

    /// Record how a source's text was obtained. Visible on the source row so
    /// a document that silently contributed zero chunks — a scan with no text
    /// layer, a corrupt DOCX — is findable rather than quietly absent.
    /// Records extraction status on the sources row. Takes an executor so
    /// build paths run it on their OPEN transaction — acquiring a second pool
    /// connection while a transaction holds one deadlocks the pool at
    /// max_connections concurrent builds, and a status write outside the
    /// build transaction would survive a rolled-back build.
    pub(crate) async fn record_extraction<'e, E>(
        &self,
        ex: E,
        source_id: &str,
        extracted: &munarium_extract::Extracted,
    ) -> Result<()>
    where
        E: sqlx::Executor<'e, Database = sqlx::Postgres>,
    {
        if extracted.status != munarium_extract::ExtractionStatus::Ok {
            tracing::warn!(
                tenant = %self.tenant_id, source = %source_id,
                status = extracted.status.as_str(),
                note = extracted.note.as_deref().unwrap_or(""),
                "source produced no indexable text"
            );
        }
        sqlx::query(
            "UPDATE sources SET extraction_status = $3, extraction_method = $4
              WHERE tenant_id = $1 AND source_id = $2",
        )
        .bind(&self.tenant_id)
        .bind(source_id)
        .bind(extracted.status.as_str())
        .bind(extracted.method.as_str())
        .execute(ex)
        .await
        .map_err(storage_err)?;
        Ok(())
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Put a source at its logical path.
    ///
    /// Identity is `(tenant, filename)`. The declared hash is verified before
    /// commit (integrity), but it is NOT the key: the same bytes at two paths
    /// are two sources, because collections bind by path prefix and each must
    /// be bindable and retirable on its own.
    ///
    /// Returns `(source_id, content_hash, existed)`. `existed` is true only
    /// when this exact path already held these exact bytes — a genuine
    /// idempotent replay. Re-putting a path with NEW bytes updates it in place
    /// and reports `existed: false`, because a rebuild is now owed.
    pub async fn put_source(
        &self,
        declared_sha256: &str,
        media_type: &str,
        filename: &str,
        shape_ref: Option<&str>,
        bytes: &[u8],
    ) -> Result<(String, String, bool)> {
        let actual = hex::encode(sha2::Sha256::digest(bytes));
        if !declared_sha256.is_empty() && declared_sha256.to_lowercase() != actual {
            return Err(KernelError::InvalidInput(format!(
                "declared sha256 {declared_sha256} does not match content {actual}"
            )));
        }
        // The reserved evidence keyspace. Sealed artifacts share this
        // object store, so a document at `evidence/...` could collide with an
        // artifact's blob — and, far worse, could tempt a reader into inferring
        // authorization from the path. Authorization comes from the evidence
        // row, never from where the bytes sit; reserving the prefix is what
        // keeps that true rather than merely intended.
        //
        // This is the single chokepoint: REST `PUT /v1/sources`, the gRPC
        // `PutSource` stream and bulk intake all funnel through here.
        munarium_core::sources::refuse_reserved_document_path(filename)?;

        // Validates the path (traversal, absolute, drive-qualified, …) before
        // it can ever reach an object-store key.
        let key = SourceKey::new(&self.tenant_id, filename, &actual)?;
        let source_id = key.source_id();

        // Was this path already holding these bytes? Decided before the write
        // so the answer is about the caller's request, not the row we leave.
        let prior: Option<(String,)> = sqlx::query_as(
            "SELECT content_hash FROM sources WHERE tenant_id = $1 AND source_id = $2",
        )
        .bind(&self.tenant_id)
        .bind(&source_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?;
        let existed = prior
            .as_ref()
            .map(|(h,)| h.as_str() == actual)
            .unwrap_or(false);

        // Bytes first, metadata second. If the metadata write fails, an
        // orphaned blob is inert (nothing reads a source that has no row);
        // the reverse would leave a row pointing at bytes that never landed.
        let blob_uri = self.sources.put(&key, media_type, bytes).await?;

        sqlx::query(
            "INSERT INTO sources (tenant_id, source_id, filename, content_hash, media_type,
                                  shape_ref, blob_uri, storage_backend, bytes_len)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (tenant_id, source_id) DO UPDATE SET
                 content_hash      = EXCLUDED.content_hash,
                 media_type        = EXCLUDED.media_type,
                 shape_ref         = EXCLUDED.shape_ref,
                 blob_uri          = EXCLUDED.blob_uri,
                 storage_backend   = EXCLUDED.storage_backend,
                 bytes_len         = EXCLUDED.bytes_len,
                 -- new bytes at this path invalidate any prior extraction
                 extraction_status = NULL,
                 extraction_method = NULL",
        )
        .bind(&self.tenant_id)
        .bind(&source_id)
        .bind(filename)
        .bind(&actual)
        .bind(media_type)
        .bind(shape_ref)
        .bind(&blob_uri)
        .bind(self.sources.backend_id())
        .bind(bytes.len() as i64)
        .execute(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok((source_id, actual, existed))
    }

    /// Metadata for one source — the answer to "where did this document
    /// go, and did it index?".
    pub async fn source_info(&self, source_id: &str) -> Result<SourceInfo> {
        let row = sqlx::query(
            "SELECT source_id, filename, media_type, content_hash, bytes_len,
                    storage_backend, blob_uri, extraction_status, extraction_method,
                    created_at::text AS created_at_text
               FROM sources WHERE tenant_id = $1 AND source_id = $2",
        )
        .bind(&self.tenant_id)
        .bind(source_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?
        .ok_or_else(|| KernelError::NotFound {
            kind: "source",
            id: source_id.to_string(),
        })?;
        Ok(SourceInfo {
            source_id: row.get("source_id"),
            filename: row.get("filename"),
            media_type: row.get("media_type"),
            content_hash: row.get("content_hash"),
            bytes_len: row.get::<i64, _>("bytes_len") as u64,
            storage_backend: row.get("storage_backend"),
            blob_uri: row.get("blob_uri"),
            extraction_status: row.get("extraction_status"),
            extraction_method: row.get("extraction_method"),
            created_at: row.get("created_at_text"),
        })
    }

    /// Resolve `source_id -> filename` for a set of ids. Used when building a
    /// provenance envelope, so an answer can name the documents behind it.
    /// Source id → (current path, content hash), for provenance enrichment.
    ///
    /// The datastore serving path fills each hit's `source_content_hash` from
    /// here rather than storing it in the artifact: PostgreSQL is truth for
    /// SOURCES in every mode, and duplicating the hash into the records
    /// format would be a second copy that could drift from the first.
    pub async fn source_provenance(
        &self,
        source_ids: &[String],
    ) -> Result<std::collections::HashMap<String, (String, String)>> {
        if source_ids.is_empty() {
            return Ok(Default::default());
        }
        let rows = sqlx::query(
            "SELECT source_id, filename, content_hash FROM sources
              WHERE tenant_id = $1 AND source_id = ANY($2)",
        )
        .bind(&self.tenant_id)
        .bind(source_ids)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(rows
            .into_iter()
            .map(|r| {
                (
                    r.get::<String, _>("source_id"),
                    (
                        r.get::<String, _>("filename"),
                        r.get::<String, _>("content_hash"),
                    ),
                )
            })
            .collect())
    }

    pub(crate) async fn source_paths(
        &self,
        source_ids: &[String],
    ) -> Result<std::collections::HashMap<String, String>> {
        if source_ids.is_empty() {
            return Ok(Default::default());
        }
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT source_id, filename FROM sources WHERE tenant_id = $1 AND source_id = ANY($2)",
        )
        .bind(&self.tenant_id)
        .bind(source_ids)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(rows.into_iter().collect())
    }

    /// Fill each hit's `source_path` and build the envelope. One indexed
    /// lookup resolves every id at once. Shared by the legacy shape-scoped
    /// search and the collection search so both name sources identically.
    pub(crate) async fn envelope_for(
        &self,
        hits: &mut [SearchHit],
        index_version: String,
        watermark: u64,
    ) -> Result<ProvenanceEnvelope> {
        let mut ids: Vec<String> = hits.iter().map(|h| h.source_id.clone()).collect();
        ids.sort();
        ids.dedup();
        let paths = self.source_paths(&ids).await?;
        for hit in hits.iter_mut() {
            // A source deleted between index build and query has no path; the
            // id and hash still identify it, so report those rather than fail.
            hit.source_path = paths.get(&hit.source_id).cloned().unwrap_or_default();
        }
        let mut source_paths: Vec<String> = ids
            .iter()
            .map(|id| paths.get(id).cloned().unwrap_or_default())
            .collect();
        source_paths.retain(|p| !p.is_empty());
        let mut hashes: Vec<String> = hits.iter().map(|h| h.source_content_hash.clone()).collect();
        hashes.sort();
        hashes.dedup();
        Ok(ProvenanceEnvelope {
            chunk_ids: hits.iter().map(|h| h.chunk_id.clone()).collect(),
            source_ids: ids,
            source_paths,
            source_content_hashes: hashes,
            index_version,
            event_watermark: watermark,
            provider_fingerprint: Some(format!("local/{LOCAL_EMBEDDER}/{EMBED_DIMS}")),
        })
    }

    /// Atomic cutover: flip the active pointer to `index_id`. Scoped to
    /// LEGACY shape-scoped indexes (`collection_id IS NULL`) so a v1 cutover
    /// on a shape that a v2 collection also uses never deactivates the
    /// collection's index — the two active pointers are independent.
    pub async fn activate_index(&self, shape_ref: &str, index_id: &str) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(storage_err)?;
        sqlx::query(
            "UPDATE index_versions SET active = false, deactivated_at = now()
              WHERE tenant_id = $1 AND shape_ref = $2 AND active AND collection_id IS NULL",
        )
        .bind(&self.tenant_id)
        .bind(shape_ref)
        .execute(&mut *tx)
        .await
        .map_err(storage_err)?;
        let updated = sqlx::query(
            "UPDATE index_versions
                SET active = true, activated_at = COALESCE(activated_at, now()),
                    deactivated_at = NULL
              WHERE tenant_id = $1 AND id = $2 AND collection_id IS NULL",
        )
        .bind(&self.tenant_id)
        .bind(index_id)
        .execute(&mut *tx)
        .await
        .map_err(storage_err)?;
        if updated.rows_affected() == 0 {
            return Err(KernelError::NotFound {
                kind: "index",
                id: index_id.to_string(),
            });
        }
        tx.commit().await.map_err(storage_err)?;
        Ok(())
    }

    /// Deterministic verification of a built index: it has chunks, and a
    /// self-query over the first chunk's leading words retrieves something.
    pub async fn verify_index(&self, index_id: &str) -> Result<serde_json::Value> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM index_chunks WHERE tenant_id = $1 AND index_version_id = $2",
        )
        .bind(&self.tenant_id)
        .bind(index_id)
        .fetch_one(&self.pool)
        .await
        .map_err(storage_err)?;
        if count == 0 {
            return Err(KernelError::InvalidInput(format!(
                "index {index_id} has zero chunks"
            )));
        }
        let first: String = sqlx::query_scalar(
            "SELECT text FROM index_chunks
              WHERE tenant_id = $1 AND index_version_id = $2
              ORDER BY chunk_id LIMIT 1",
        )
        .bind(&self.tenant_id)
        .bind(index_id)
        .fetch_one(&self.pool)
        .await
        .map_err(storage_err)?;
        let probe: String = first
            .split_whitespace()
            .take(5)
            .collect::<Vec<_>>()
            .join(" ");
        let hits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM index_chunks
              WHERE tenant_id = $1 AND index_version_id = $2
                AND ts @@ plainto_tsquery('english', $3)",
        )
        .bind(&self.tenant_id)
        .bind(index_id)
        .bind(&probe)
        .fetch_one(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(serde_json::json!({ "chunks": count, "self_probe_hits": hits }))
    }

    /// Drop chunk data for inactive versions beyond the newest `keep`
    /// (manifests stay resolvable — provenance never breaks, storage does
    /// get reclaimed).
    pub async fn retire_old(&self, shape_ref: &str, keep: u32) -> Result<u64> {
        let retired = sqlx::query(
            r#"
            DELETE FROM index_chunks
             WHERE tenant_id = $1 AND index_version_id IN (
                SELECT id FROM index_versions
                 WHERE tenant_id = $1 AND shape_ref = $2 AND NOT active
                   AND collection_id IS NULL
                 ORDER BY built_at DESC OFFSET $3
             )
            "#,
        )
        .bind(&self.tenant_id)
        .bind(shape_ref)
        .bind(keep as i64)
        .execute(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(retired.rows_affected())
    }

    pub async fn source_count(&self, shape_ref: &str) -> Result<i64> {
        sqlx::query_scalar("SELECT COUNT(*) FROM sources WHERE tenant_id = $1 AND shape_ref = $2")
            .bind(&self.tenant_id)
            .bind(shape_ref)
            .fetch_one(&self.pool)
            .await
            .map_err(storage_err)
    }

    /// Side-by-side index build over every source bound to `shape_ref`.
    /// `activate` = immediate cutover (the REST /build convenience); runbook
    /// pipelines build inactive and cut over at their approval-gated step.
    /// Idempotent: an existing identical version is not rebuilt.
    pub async fn build_index(
        &self,
        shape_ref: &str,
        max_chars: usize,
        watermark_seq: u64,
        activate: bool,
    ) -> Result<IndexVersion> {
        let sources = sqlx::query(
            "SELECT source_id, filename, content_hash, media_type FROM sources
              WHERE tenant_id = $1 AND shape_ref = $2 ORDER BY source_id",
        )
        .bind(&self.tenant_id)
        .bind(shape_ref)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;
        if sources.is_empty() {
            return Err(KernelError::InvalidInput(format!(
                "no sources bound to shape '{shape_ref}' — PutSource with shape_ref first"
            )));
        }

        let source_hashes: Vec<String> = sources
            .iter()
            .map(|r| r.get::<String, _>("content_hash"))
            .collect();
        // Identity pairs id WITH hash: two sources sharing bytes are distinct
        // indexes, and re-putting one path with new bytes rebuilds.
        let source_set: Vec<String> = sources
            .iter()
            .map(|r| {
                format!(
                    "{}:{}",
                    r.get::<String, _>("source_id"),
                    r.get::<String, _>("content_hash")
                )
            })
            .collect();
        let identity = format!(
            "{shape_ref}|{CHUNKER_VERSION}|{LOCAL_EMBEDDER}|{}|{}",
            self.extractors.version(),
            source_set.join(",")
        );
        let index_id = format!(
            "idx-{}",
            &hex::encode(sha2::Sha256::digest(identity.as_bytes()))[..16]
        );

        let manifest = serde_json::json!({
            "shape_ref": shape_ref,
            "chunker": CHUNKER_VERSION,
            "extractors": self.extractors.version(),
            "embedder": { "provider": "local", "model": LOCAL_EMBEDDER, "dims": EMBED_DIMS },
            "source_set": source_hashes,
            "max_chars": max_chars,
        });

        let exists =
            sqlx::query("SELECT 1 AS one FROM index_versions WHERE tenant_id = $1 AND id = $2")
                .bind(&self.tenant_id)
                .bind(&index_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(storage_err)?
                .is_some();

        if !exists {
            let mut tx = self.pool.begin().await.map_err(storage_err)?;
            sqlx::query(
                "INSERT INTO index_versions (tenant_id, id, shape_ref, manifest, watermark_seq, active)
                 VALUES ($1, $2, $3, $4, $5, false)",
            )
            .bind(&self.tenant_id)
            .bind(&index_id)
            .bind(shape_ref)
            .bind(&manifest)
            .bind(watermark_seq as i64)
            .execute(&mut *tx)
            .await
            .map_err(storage_err)?;

            for row in &sources {
                let sid: String = row.get("source_id");
                let path: String = row.get("filename");
                let hash: String = row.get("content_hash");
                let media_type: String = row.get("media_type");
                let key = SourceKey::new(&self.tenant_id, &path, &hash)?;
                let bytes = self.sources.get(&key).await?;
                let extracted = self.extract_source(&media_type, &bytes).await;
                self.record_extraction(&mut *tx, &sid, &extracted).await?;
                let text = extracted.text;
                for (ordinal, chunk) in chunk_text(&text, max_chars).iter().enumerate() {
                    sqlx::query(
                        "INSERT INTO index_chunks
                            (tenant_id, index_version_id, chunk_id, source_id, source_hash, ordinal, text, embedding)
                         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                    )
                    .bind(&self.tenant_id)
                    .bind(&index_id)
                    // chunk_id keys on source_id, not the hash: two sources
                    // with identical bytes would otherwise collide here.
                    .bind(format!("{sid}#{ordinal}"))
                    .bind(&sid)
                    .bind(&hash)
                    .bind(ordinal as i32)
                    .bind(chunk)
                    .bind(Vector::from(local_embed(chunk)))
                    .execute(&mut *tx)
                    .await
                    .map_err(storage_err)?;
                }
            }
            tx.commit().await.map_err(storage_err)?;
        }

        // refresh the watermark on re-activation of an identical version
        sqlx::query(
            "UPDATE index_versions SET watermark_seq = $3 WHERE tenant_id = $1 AND id = $2",
        )
        .bind(&self.tenant_id)
        .bind(&index_id)
        .bind(watermark_seq as i64)
        .execute(&self.pool)
        .await
        .map_err(storage_err)?;

        if activate {
            self.activate_index(shape_ref, &index_id).await?;
        }
        self.index_version_by_id(&index_id).await
    }

    pub async fn index_version_by_id(&self, index_id: &str) -> Result<IndexVersion> {
        let row = sqlx::query(
            "SELECT id, shape_ref, manifest, watermark_seq, active
               FROM index_versions WHERE tenant_id = $1 AND id = $2",
        )
        .bind(&self.tenant_id)
        .bind(index_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?
        .ok_or_else(|| KernelError::NotFound {
            kind: "index",
            id: index_id.to_string(),
        })?;
        Ok(IndexVersion {
            id: row.get("id"),
            shape_ref: row.get("shape_ref"),
            manifest: row.get("manifest"),
            event_watermark: row.get::<i64, _>("watermark_seq") as u64,
            active: row.get("active"),
        })
    }

    /// Which index version a LEGACY shape-scoped search uses, and its
    /// watermark. Public for the same reason as `resolve_index_version`: the
    /// datastore serving path resolves the same version the PostgreSQL path
    /// would have, and only the physical artifact is the datastore's choice.
    pub async fn resolve_index(
        &self,
        shape_ref: &str,
        index_version: Option<&str>,
    ) -> Result<(String, u64)> {
        let row = match index_version {
            Some(id) => sqlx::query(
                "SELECT id, watermark_seq FROM index_versions WHERE tenant_id = $1 AND id = $2",
            )
            .bind(&self.tenant_id)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage_err)?,
            // Legacy shape-scoped resolution: the collection-scoped active
            // index (collection_id NOT NULL) is served by search_collection,
            // and its chunks live in collection_chunks, not index_chunks —
            // never resolve it here.
            None => sqlx::query(
                "SELECT id, watermark_seq FROM index_versions
                  WHERE tenant_id = $1 AND shape_ref = $2 AND active AND collection_id IS NULL",
            )
            .bind(&self.tenant_id)
            .bind(shape_ref)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage_err)?,
        };
        let row = row.ok_or_else(|| KernelError::NotFound {
            kind: "index",
            id: index_version.unwrap_or(shape_ref).to_string(),
        })?;
        Ok((row.get("id"), row.get::<i64, _>("watermark_seq") as u64))
    }
}

#[async_trait]
impl RetrievalBackend for PgRetrieval {
    async fn hybrid_search(&self, q: HybridQuery) -> Result<SearchResult> {
        let (index_id, watermark) = self
            .resolve_index(&q.shape_ref, q.index_version.as_deref())
            .await?;
        let candidate_n = 50i64;

        // lexical candidates
        // OR-semantics lexical leg — same rewrite as the collection path
        // (see collections.rs `search_collection`): rank by matched-term
        // density rather than demanding every term.
        let lexical = sqlx::query(
            "SELECT chunk_id, source_id, source_hash, text,
                    ts_rank(ts, q.q) AS rank
               FROM index_chunks,
                    (SELECT replace(plainto_tsquery('english', $3)::text,
                                    ' & ', ' | ')::tsquery AS q) q
              WHERE tenant_id = $1 AND index_version_id = $2
                AND ts @@ q.q
              ORDER BY rank DESC LIMIT $4",
        )
        .bind(&self.tenant_id)
        .bind(&index_id)
        .bind(&q.query)
        .bind(candidate_n)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;

        // vector candidates (cosine distance ascending)
        let qvec = Vector::from(local_embed(&q.query));
        let vector = sqlx::query(
            "SELECT chunk_id, source_id, source_hash, text, (embedding <=> $3) AS distance
               FROM index_chunks
              WHERE tenant_id = $1 AND index_version_id = $2
              ORDER BY embedding <=> $3 LIMIT $4",
        )
        .bind(&self.tenant_id)
        .bind(&index_id)
        .bind(&qvec)
        .bind(candidate_n)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;

        // reciprocal rank fusion, k = 60 (shape-configurable upstream)
        let mut hits = rrf_fuse(&lexical, &vector, 60.0);
        hits.truncate(if q.top_k == 0 { 10 } else { q.top_k });

        let envelope = self.envelope_for(&mut hits, index_id, watermark).await?;
        Ok(SearchResult { hits, envelope })
    }

    async fn index_version(&self, shape_ref: &str) -> Result<IndexVersion> {
        let row = sqlx::query(
            "SELECT id, shape_ref, manifest, watermark_seq, active
               FROM index_versions
              WHERE tenant_id = $1 AND shape_ref = $2 AND active AND collection_id IS NULL",
        )
        .bind(&self.tenant_id)
        .bind(shape_ref)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?
        .ok_or_else(|| KernelError::NotFound {
            kind: "index",
            id: shape_ref.to_string(),
        })?;
        Ok(IndexVersion {
            id: row.get("id"),
            shape_ref: row.get("shape_ref"),
            manifest: row.get("manifest"),
            event_watermark: row.get::<i64, _>("watermark_seq") as u64,
            active: row.get("active"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunker_is_deterministic_and_packs() {
        let text = "para one\n\npara two\n\npara three";
        let a = chunk_text(text, 2000);
        assert_eq!(a.len(), 1);
        let b = chunk_text(text, 10);
        assert!(b.len() >= 3);
        assert_eq!(chunk_text(text, 10), b);
    }

    #[test]
    fn embedder_is_deterministic_and_normalized() {
        let a = local_embed("the quick brown fox");
        let b = local_embed("the quick brown fox");
        assert_eq!(a, b);
        let norm: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
        assert_ne!(local_embed("alpha"), local_embed("omega"));
    }

    #[test]
    fn query_expansion_is_conditional_case_insensitive_and_deduplicated() {
        let rules = vec![
            QueryExpansionRule {
                when_any: vec!["visit".into(), "visited".into()],
                add_terms: vec!["journey".into(), "tour".into(), "cities".into()],
            },
            QueryExpansionRule {
                when_any: vec!["cities".into()],
                add_terms: vec!["town".into(), "place".into()],
            },
        ];
        assert_eq!(
            expand_query("What CITIES did Washington VISIT?", &rules),
            "What CITIES did Washington VISIT? journey tour town place"
        );
        assert_eq!(
            expand_query("Who commanded the army?", &rules),
            "Who commanded the army?"
        );
    }

    fn hit(
        chunk_id: &str,
        per_collection_score: f64,
        lexical_score: Option<f64>,
        vector_distance: Option<f64>,
    ) -> SearchHit {
        SearchHit {
            chunk_id: chunk_id.into(),
            source_id: format!("src-{chunk_id}"),
            source_path: format!("{chunk_id}.md"),
            source_content_hash: "0".repeat(64),
            text: String::new(),
            score: per_collection_score,
            lexical_rank: lexical_score.map(|_| 1),
            vector_rank: vector_distance.map(|_| 1),
            lexical_score,
            vector_distance,
            metadata: None,
        }
    }

    fn coll(name: &str, hits: Vec<SearchHit>) -> crate::CollectionSearchResult {
        crate::CollectionSearchResult {
            collection_id: format!("col-{name}"),
            collection_name: name.into(),
            result: munarium_core::retrieval::SearchResult {
                hits,
                envelope: munarium_core::retrieval::ProvenanceEnvelope {
                    chunk_ids: vec![],
                    source_ids: vec![],
                    source_paths: vec![],
                    source_content_hashes: vec![],
                    index_version: "idx-test".into(),
                    event_watermark: 0,
                    provider_fingerprint: None,
                },
            },
        }
    }

    /// The 2026-08-24 due-diligence starvation regression: a relevant
    /// collection's deep results (strong raw leg scores) must outrank an
    /// irrelevant collection's rank-1 (weak raw leg scores). Under the old
    /// per-collection-score merge, every collection's rank-1 tied at
    /// 1/(rrf_k+1) and top_k slots went one-per-collection.
    #[test]
    fn merge_fuses_globally_not_by_per_collection_rank() {
        let rrf_k = 60.0;
        let rank1 = 1.0 / (rrf_k + 1.0);
        let rank2 = 1.0 / (rrf_k + 2.0);
        // Relevant collection: two strong lexical matches, close vectors.
        let relevant = coll(
            "commercial",
            vec![
                hit("com-a", rank1, Some(0.9), Some(0.20)),
                hit("com-b", rank2, Some(0.7), Some(0.25)),
            ],
        );
        // Irrelevant collections: no lexical match at all — their rank-1s
        // exist only because the vector leg has no floor.
        let noise1 = coll("tax", vec![hit("tax-a", rank1, None, Some(0.90))]);
        let noise2 = coll("insurance", vec![hit("ins-a", rank1, None, Some(0.95))]);

        let merged = merge_hits(&[relevant, noise1, noise2], 3, rrf_k);
        let order: Vec<&str> = merged.iter().map(|(_, h)| h.chunk_id.as_str()).collect();
        // Both relevant docs beat every noise rank-1.
        assert_eq!(&order[..2], &["com-a", "com-b"], "got {order:?}");
        // Scores are the fused global scores, strictly ordered.
        assert!(merged.windows(2).all(|w| w[0].1.score >= w[1].1.score));
        // The old bug's signature — three identical rank-1 scores — is gone.
        assert!(merged[0].1.score > merged[2].1.score);
    }

    /// Hits with no raw leg evidence (legacy producer) sort last but are
    /// still returned deterministically.
    #[test]
    fn merge_puts_scoreless_hits_last() {
        let a = coll("with-evidence", vec![hit("ev-a", 0.016, Some(0.5), None)]);
        let b = coll("legacy", vec![hit("old-a", 0.016, None, None)]);
        let merged = merge_hits(&[a, b], 10, 60.0);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].1.chunk_id, "ev-a");
        assert_eq!(merged[1].1.chunk_id, "old-a");
        assert_eq!(merged[1].1.score, 0.0);
    }
}

/// The built-in bag-of-words embedder, behind the backend-neutral trait.
///
/// Wraps the same `local_embed` and blend the search path has always used, so
/// moving preparation up to the coordinator changes where the vector is
/// computed and not what it is. `weighted_query_embedding`'s early-exit cases
/// are preserved exactly: identical texts or weight >= 1 embed the expanded
/// query alone, weight <= 0 embeds the original alone, and only the middle
/// blends -- which matters, because embedding twice and blending is NOT
/// bit-identical to embedding once.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalHashEmbedder;

impl munarium_core::retrieval::QueryEmbedder for LocalHashEmbedder {
    fn embed(&self, text: &str) -> Vec<f32> {
        local_embed(text)
    }

    fn blend(&self, original: &str, expanded: &str, weight: f32) -> Vec<f32> {
        collections::weighted_query_embedding(original, expanded, weight)
    }

    fn fingerprint(&self) -> String {
        format!("local/{LOCAL_EMBEDDER}/{EMBED_DIMS}")
    }

    fn dimensions(&self) -> usize {
        EMBED_DIMS
    }
}
