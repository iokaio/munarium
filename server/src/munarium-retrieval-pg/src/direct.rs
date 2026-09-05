// SPDX-License-Identifier: Apache-2.0
//! The two halves of a DIRECT collection build:
//! extraction with **no transaction anywhere near it**, and a short,
//! insert-only commit.
//!
//! The existing `build_collection_index` holds one transaction around the
//! whole build — extraction, document-intelligence escalation (a slow HTTP
//! call on the worst day), embedding, and every chunk insert. That shape is
//! §7.2's named problem: a long transaction pinning a connection and holding
//! back vacuum for however long a corpus takes to extract. It stays untouched
//! because it is the live path with live behaviour; the direct build gets the
//! split shape instead, and the split is the point:
//!
//! 1. [`PgRetrieval::extract_collection_prepared`] does every slow thing —
//!    fetch, extract, chunk, embed — against ordinary reads, accumulating the
//!    prepared chunks in memory. Nothing is written.
//! 2. The CALLER computes the version identity from what was actually
//!    extracted (the `idx2-` spec hash — which is why extraction must come
//!    first: a real spec carries real per-source outcomes).
//! 3. [`PgRetrieval::commit_prepared_index`] writes the version row, the
//!    chunk rows and the extraction records in one transaction that does
//!    nothing but insert — bounded by insert speed, not by extraction.
//!
//! The same prepared chunks then feed the datastore sink, which is the other
//! half of §7.2: one extraction/embedding pass, two indexes.

use pgvector::Vector;
use sqlx::Row;

use munarium_core::retrieval::CollectionInfo;
use munarium_core::{KernelError, Result};

use crate::chunk_text;
use crate::collections::{partition_name, ChunkBatch, CHUNK_INSERT_BATCH};
use crate::{local_embed, storage_err, PgRetrieval};

/// One source of a prepared build, as identity.
#[derive(Debug, Clone)]
pub struct PreparedSource {
    pub source_id: String,
    pub filename: String,
    pub content_hash: String,
    pub media_type: String,
}

/// One extracted-and-embedded chunk, engine-neutral.
#[derive(Debug, Clone)]
pub struct BuiltChunk {
    pub chunk_id: String,
    pub source_id: String,
    pub source_path: String,
    pub source_content_hash: String,
    pub ordinal: u32,
    pub text: String,
    pub embedding: Vec<f32>,
}

/// What one source's extraction produced, for the spec's own record.
#[derive(Debug, Clone)]
pub struct PreparedExtraction {
    pub source_id: String,
    /// `extracted` | `empty` | `failed` — the datastore spec vocabulary.
    pub status: &'static str,
    pub method: Option<String>,
    pub extracted_text_sha256: Option<String>,
    /// The raw extraction result, kept so the commit can record it through
    /// the same `record_extraction` the ordinary build uses.
    pub(crate) raw: munarium_extract::Extracted,
}

/// Everything extraction produced, held in memory between the two halves.
#[derive(Debug)]
pub struct PreparedBuild {
    pub info: CollectionInfo,
    pub sources: Vec<PreparedSource>,
    pub chunks: Vec<BuiltChunk>,
    pub outcomes: Vec<PreparedExtraction>,
    pub max_chars: usize,
}

impl PgRetrieval {
    /// The slow half: fetch, extract, chunk and embed every bound source —
    /// with no transaction open anywhere.
    pub async fn extract_collection_prepared(
        &self,
        collection_id: &str,
        max_chars: usize,
    ) -> Result<PreparedBuild> {
        let info = self.collection_by_id(collection_id).await?;
        let rows = sqlx::query(
            "SELECT s.source_id, s.filename, s.content_hash, s.media_type
               FROM collection_sources cs
               JOIN sources s
                 ON s.tenant_id = cs.tenant_id AND s.source_id = cs.source_id
              WHERE cs.tenant_id = $1 AND cs.collection_id = $2
              ORDER BY s.source_id",
        )
        .bind(&self.tenant_id)
        .bind(collection_id)
        .fetch_all(self.pool())
        .await
        .map_err(storage_err)?;
        if rows.is_empty() {
            return Err(KernelError::InvalidInput(format!(
                "no sources bound to collection '{}' — ingest or bind sources first",
                info.name
            )));
        }

        let sources: Vec<PreparedSource> = rows
            .iter()
            .map(|r| PreparedSource {
                source_id: r.get("source_id"),
                filename: r.get("filename"),
                content_hash: r.get("content_hash"),
                media_type: r.get("media_type"),
            })
            .collect();

        let mut chunks = Vec::new();
        let mut outcomes = Vec::with_capacity(sources.len());
        for s in &sources {
            let key = munarium_core::sources::SourceKey::new(
                &self.tenant_id,
                &s.filename,
                &s.content_hash,
            )?;
            let bytes = self.source_store().get(&key).await?;
            let extracted = self.extract_source(&s.media_type, &bytes).await;

            use sha2::Digest as _;
            let status = match extracted.status {
                munarium_extract::ExtractionStatus::Ok => "extracted",
                munarium_extract::ExtractionStatus::Empty => "empty",
                munarium_extract::ExtractionStatus::Failed => "failed",
            };
            outcomes.push(PreparedExtraction {
                source_id: s.source_id.clone(),
                status,
                method: Some(extracted.method.as_str().to_string()),
                extracted_text_sha256: (!extracted.text.is_empty())
                    .then(|| hex::encode(sha2::Sha256::digest(extracted.text.as_bytes()))),
                raw: extracted.clone(),
            });

            for (ordinal, chunk) in chunk_text(&extracted.text, max_chars).iter().enumerate() {
                chunks.push(BuiltChunk {
                    chunk_id: format!("{}#{ordinal}", s.source_id),
                    source_id: s.source_id.clone(),
                    source_path: s.filename.clone(),
                    source_content_hash: s.content_hash.clone(),
                    ordinal: ordinal as u32,
                    text: chunk.clone(),
                    embedding: local_embed(chunk),
                });
            }
        }

        Ok(PreparedBuild {
            info,
            sources,
            chunks,
            outcomes,
            max_chars,
        })
    }

    /// The short half: write the version row, every chunk row and the
    /// extraction records in ONE insert-only transaction.
    ///
    /// Idempotent by version id: an existing row means another builder (or a
    /// retry) already committed this identity, and the chunks it committed
    /// are this build's chunks by construction — the id is a hash of the
    /// inputs that produced them.
    pub async fn commit_prepared_index(
        &self,
        collection_id: &str,
        index_id: &str,
        manifest: &serde_json::Value,
        watermark_seq: u64,
        build: &PreparedBuild,
    ) -> Result<bool> {
        let mut tx = self.pool().begin().await.map_err(storage_err)?;
        // The existence check IS the insert: `ON CONFLICT DO NOTHING` makes
        // two builders racing on one identity converge — the loser sees zero
        // rows and reports `false`, exactly as a retry after a completed
        // commit does — where a separate SELECT-then-INSERT turned the race
        // into a primary-key violation for a build that had in fact converged.
        let inserted = sqlx::query(
            "INSERT INTO index_versions
                (tenant_id, id, shape_ref, collection_id, manifest, watermark_seq, active)
             VALUES ($1, $2, $3, $4, $5, $6, false)
             ON CONFLICT (tenant_id, id) DO NOTHING",
        )
        .bind(&self.tenant_id)
        .bind(index_id)
        .bind(&build.info.shape_ref)
        .bind(collection_id)
        .bind(manifest)
        .bind(watermark_seq as i64)
        .execute(&mut *tx)
        .await
        .map_err(storage_err)?
        .rows_affected();
        if inserted == 0 {
            // Dropping `tx` rolls the (empty) transaction back.
            return Ok(false);
        }

        for outcome in &build.outcomes {
            self.record_extraction(&mut *tx, &outcome.source_id, &outcome.raw)
                .await?;
        }

        let mut batch = ChunkBatch::default();
        for c in &build.chunks {
            batch.push(
                c.chunk_id.clone(),
                &c.source_id,
                &c.source_content_hash,
                c.ordinal as i32,
                &c.text,
                Vector::from(c.embedding.clone()),
            );
            if batch.len() >= CHUNK_INSERT_BATCH {
                batch
                    .flush(&mut tx, &self.tenant_id, collection_id, index_id)
                    .await?;
            }
        }
        batch
            .flush(&mut tx, &self.tenant_id, collection_id, index_id)
            .await?;
        tx.commit().await.map_err(storage_err)?;

        // The same post-commit maintenance the ordinary build performs, for
        // the same measured reasons (§13.5 entries 20-21): stop-term
        // statistics, then fresh planner statistics for the partition.
        self.record_lexeme_frequency(collection_id, index_id)
            .await?;
        let partition = partition_name(collection_id)?;
        sqlx::query(&format!("VACUUM (ANALYZE) {partition}"))
            .execute(self.pool())
            .await
            .map_err(storage_err)?;
        Ok(true)
    }

    /// Compare-and-swap activation (§7.3 logical activation, steps 2–6): flip
    /// the active pointer to `index_id` only if the CURRENT active version is
    /// still `expected_active` (`None` = "no version was active").
    ///
    /// A failed comparison is the superseded-build outcome: `Ok(false)`, the
    /// pointer untouched, the built version still valid — the caller reports
    /// it rather than deleting anything.
    pub async fn activate_collection_index_cas(
        &self,
        collection_id: &str,
        index_id: &str,
        expected_active: Option<&str>,
    ) -> Result<bool> {
        let mut tx = self.pool().begin().await.map_err(storage_err)?;
        // Serialize activations per collection with an advisory lock. The
        // `FOR UPDATE` below locks the CURRENT active row, which is enough
        // when one exists — but when nothing is active it locks nothing, and
        // two concurrent CAS calls with `expected_active = None` would both
        // compare equal, both proceed, and both report `Ok(true)` while only
        // the last writer's version stayed active.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1 || ':activate:' || $2))")
            .bind(&self.tenant_id)
            .bind(collection_id)
            .execute(&mut *tx)
            .await
            .map_err(storage_err)?;
        // Lock the collection's version rows so a concurrent activation
        // serializes against this comparison rather than racing it.
        let current: Option<String> = sqlx::query_scalar(
            "SELECT id FROM index_versions
              WHERE tenant_id = $1 AND collection_id = $2 AND active
              FOR UPDATE",
        )
        .bind(&self.tenant_id)
        .bind(collection_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage_err)?;
        if current.as_deref() != expected_active {
            return Ok(false);
        }

        sqlx::query(
            "UPDATE index_versions SET active = false, deactivated_at = now()
              WHERE tenant_id = $1 AND collection_id = $2 AND active",
        )
        .bind(&self.tenant_id)
        .bind(collection_id)
        .execute(&mut *tx)
        .await
        .map_err(storage_err)?;
        let updated = sqlx::query(
            "UPDATE index_versions
                SET active = true, activated_at = COALESCE(activated_at, now()),
                    deactivated_at = NULL
              WHERE tenant_id = $1 AND id = $2 AND collection_id = $3",
        )
        .bind(&self.tenant_id)
        .bind(index_id)
        .bind(collection_id)
        .execute(&mut *tx)
        .await
        .map_err(storage_err)?;
        if updated.rows_affected() == 0 {
            return Err(KernelError::NotFound {
                kind: "index version",
                id: index_id.to_string(),
            });
        }
        tx.commit().await.map_err(storage_err)?;
        Ok(true)
    }

    /// The current active version of a collection, if any — what a direct
    /// build records as `expected_active_version` before it starts.
    pub async fn current_active_collection_index(
        &self,
        collection_id: &str,
    ) -> Result<Option<String>> {
        sqlx::query_scalar(
            "SELECT id FROM index_versions
              WHERE tenant_id = $1 AND collection_id = $2 AND active",
        )
        .bind(&self.tenant_id)
        .bind(collection_id)
        .fetch_optional(self.pool())
        .await
        .map_err(storage_err)
    }
}
