// SPDX-License-Identifier: Apache-2.0
//! Semantic index storage (ClearView IR-2).
//!
//! A semantic index is an ordinary collection index version whose identity
//! also names a semantic embedding space, plus one companion vector per chunk
//! in `semantic_chunk_vectors` (migration 0080). The existing `VECTOR(256)`
//! columns, their HNSW indexes and the hash embedder are untouched: the
//! semantic index is a NEW index id built beside the existing one and
//! activated by the same pointer swap, so rollback is the reverse swap.
//!
//! The companion column is fixed at [`SEMANTIC_DIMS`] (the 1,024-dimension
//! first trial the design proposes). The vector leg over it is an exact scan
//! of one index version's rows: correct for any corpus size, and the place an
//! approximate index goes once I3 measurements justify one.

use munarium_core::retrieval::{IndexVersion, PreparedSearchQuery, SearchResult};
use munarium_core::{KernelError, Result};
use pgvector::Vector;
use sqlx::Row;

use crate::{storage_err, PgRetrieval};

/// The one supported semantic dimension (migration 0080's column type).
pub const SEMANTIC_DIMS: usize = 1024;

/// A semantic embedding space: what a passage vector in a semantic index
/// means. Two spaces with different identities are never compared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticSpace {
    /// `sem-` and 16 hex digits of the digest of every parameter that changes
    /// the vectors (provider configuration, endpoint fingerprint, served
    /// model, dimension, instructions, normalization, truncation).
    pub identity: String,
    pub dimensions: usize,
    /// The non-secret parameters the identity digests, recorded in the
    /// manifest so an operator can see what the index was built with.
    pub descriptor: serde_json::Value,
}

impl SemanticSpace {
    /// Derive a space from its descriptor. The identity is the digest of the
    /// descriptor's canonical (sorted-key) JSON text.
    pub fn from_descriptor(descriptor: serde_json::Value, dimensions: usize) -> Result<Self> {
        if dimensions != SEMANTIC_DIMS {
            return Err(KernelError::InvalidInput(format!(
                "semantic indexes support {SEMANTIC_DIMS} dimensions; the binding declares {dimensions}"
            )));
        }
        use sha2::Digest as _;
        // serde_json's map is ordered by key, so this text is canonical.
        let canonical = descriptor.to_string();
        let digest = hex::encode(sha2::Sha256::digest(canonical.as_bytes()));
        Ok(Self {
            identity: format!("sem-{}", &digest[..16]),
            dimensions,
            descriptor,
        })
    }

    /// The manifest block a semantic index records.
    pub fn manifest(&self) -> serde_json::Value {
        serde_json::json!({
            "identity": self.identity,
            "dims": self.dimensions,
            "descriptor": self.descriptor,
        })
    }

    /// The provenance fingerprint a search over this space reports.
    pub fn fingerprint(&self) -> String {
        format!("endpoint/{}/{}", self.identity, self.dimensions)
    }
}

/// A query vector in a semantic space, prepared by the coordinator.
#[derive(Debug, Clone)]
pub struct SemanticQuery {
    pub vector: Vec<f32>,
    /// The envelope's `provider_fingerprint` for this search.
    pub fingerprint: String,
}

/// How far a semantic index's companion vectors cover its chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticCoverage {
    pub chunks: i64,
    pub vectors: i64,
}

impl SemanticCoverage {
    pub fn complete(&self) -> bool {
        self.chunks > 0 && self.chunks == self.vectors
    }
}

impl PgRetrieval {
    /// Build (or find) the semantic index for `space`, never activated.
    ///
    /// The chunks are produced by the ordinary collection build under the
    /// semantic identity, so a repeated call with the same sources and space
    /// returns the same index id and resumes the vector fill where it stopped.
    pub async fn build_semantic_index(
        &self,
        collection_id: &str,
        max_chars: usize,
        watermark_seq: u64,
        space: &SemanticSpace,
    ) -> Result<IndexVersion> {
        self.build_collection_index_with(
            collection_id,
            max_chars,
            watermark_seq,
            false,
            Some(space),
        )
        .await
    }

    /// The semantic identity and dimension an index version records, or
    /// `None` for a hash index (every v1.4 index).
    pub async fn semantic_identity(&self, index_id: &str) -> Result<Option<(String, usize)>> {
        let row = sqlx::query(
            "SELECT manifest->'semantic'->>'identity' AS identity,
                    (manifest->'semantic'->>'dims')::int AS dims
               FROM index_versions WHERE tenant_id = $1 AND id = $2",
        )
        .bind(&self.tenant_id)
        .bind(index_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(row.and_then(|r| {
            let identity: Option<String> = r.get("identity");
            let dims: Option<i32> = r.get("dims");
            Some((identity?, usize::try_from(dims?).ok()?))
        }))
    }

    /// Up to `limit` chunks of a semantic index that have no vector yet, in
    /// chunk order. The build loop embeds these and writes them back.
    pub async fn semantic_chunks_pending(
        &self,
        collection_id: &str,
        index_id: &str,
        limit: i64,
    ) -> Result<Vec<(String, String)>> {
        let rows = sqlx::query(
            "SELECT c.chunk_id, c.text FROM collection_chunks c
              WHERE c.tenant_id = $1 AND c.collection_id = $2 AND c.index_version_id = $3
                AND NOT EXISTS (
                    SELECT 1 FROM semantic_chunk_vectors v
                     WHERE v.tenant_id = c.tenant_id AND v.index_version_id = c.index_version_id
                       AND v.chunk_id = c.chunk_id)
              ORDER BY c.chunk_id LIMIT $4",
        )
        .bind(&self.tenant_id)
        .bind(collection_id)
        .bind(index_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(rows
            .into_iter()
            .map(|r| (r.get("chunk_id"), r.get("text")))
            .collect())
    }

    /// Write one batch of validated vectors. Under a build-job fence the write
    /// commits only while the claim is current (ClearView R4); a superseded
    /// execution writes nothing.
    pub async fn write_semantic_vectors(
        &self,
        collection_id: &str,
        index_id: &str,
        rows: &[(String, Vec<f32>)],
        fence: Option<&munarium_store_pg::jobs::JobFence>,
    ) -> Result<()> {
        if rows.iter().any(|(_, v)| v.len() != SEMANTIC_DIMS) {
            return Err(KernelError::InvalidInput(
                "semantic vectors must have the supported dimension".into(),
            ));
        }
        let mut tx = self.pool.begin().await.map_err(storage_err)?;
        if let Some(fence) = fence {
            fence.require_tenant(&self.tenant_id)?;
            if !munarium_store_pg::jobs::lock_generation(&mut tx, fence).await? {
                return Err(fence.stale());
            }
        }
        let ids: Vec<String> = rows.iter().map(|(id, _)| id.clone()).collect();
        let vectors: Vec<Vector> = rows.iter().map(|(_, v)| Vector::from(v.clone())).collect();
        sqlx::query(
            "INSERT INTO semantic_chunk_vectors
                 (tenant_id, collection_id, index_version_id, chunk_id, embedding)
             SELECT $1, $2, $3, u.chunk_id, u.embedding
               FROM UNNEST($4::text[], $5::vector[]) AS u(chunk_id, embedding)
             ON CONFLICT (tenant_id, index_version_id, chunk_id) DO NOTHING",
        )
        .bind(&self.tenant_id)
        .bind(collection_id)
        .bind(index_id)
        .bind(&ids)
        .bind(&vectors)
        .execute(&mut *tx)
        .await
        .map_err(storage_err)?;
        tx.commit().await.map_err(storage_err)
    }

    /// Chunks and companion vectors of one semantic index.
    pub async fn semantic_coverage(
        &self,
        collection_id: &str,
        index_id: &str,
    ) -> Result<SemanticCoverage> {
        let row = sqlx::query(
            "SELECT
                (SELECT count(*) FROM collection_chunks
                  WHERE tenant_id = $1 AND collection_id = $2 AND index_version_id = $3) AS chunks,
                (SELECT count(*) FROM semantic_chunk_vectors
                  WHERE tenant_id = $1 AND collection_id = $2 AND index_version_id = $3) AS vectors",
        )
        .bind(&self.tenant_id)
        .bind(collection_id)
        .bind(index_id)
        .fetch_one(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(SemanticCoverage {
            chunks: row.get("chunks"),
            vectors: row.get("vectors"),
        })
    }

    /// Search one collection's semantic index: the ordinary lexical leg and
    /// fusion, with the vector leg over the companion vectors.
    pub async fn search_collection_semantic(
        &self,
        collection_id: &str,
        prepared: &PreparedSearchQuery,
        index_id: &str,
        query: &SemanticQuery,
    ) -> Result<SearchResult> {
        if query.vector.len() != SEMANTIC_DIMS {
            return Err(KernelError::InvalidInput(
                "a semantic query vector must have the supported dimension".into(),
            ));
        }
        self.search_collection_leg(collection_id, prepared, Some(index_id), Some(query))
            .await
    }

    /// The semantic vector leg's candidates, with the same columns and
    /// content-demotion rules as the hash leg.
    pub(crate) async fn semantic_vector_leg(
        &self,
        collection_id: &str,
        index_id: &str,
        query: &SemanticQuery,
        limit: i64,
        demotions_json: &serde_json::Value,
    ) -> Result<Vec<sqlx::postgres::PgRow>> {
        sqlx::query(
            "WITH rules AS (
                SELECT (rule->>'vector_distance_penalty')::double precision AS penalty,
                       CASE WHEN rule->>'match' = 'phrase'
                            THEN phraseto_tsquery('english', rule->>'contains') END AS phrase,
                       CASE WHEN rule->>'match' = 'phrase'
                            THEN NULL ELSE lower(rule->>'contains') END AS marker
                  FROM jsonb_array_elements($6::jsonb) AS rule
             )
             SELECT candidates.chunk_id, candidates.source_id, candidates.source_hash, candidates.text,
                    candidates.raw_distance + COALESCE((
                        SELECT MAX(rules.penalty)
                          FROM rules
                         WHERE (rules.phrase IS NOT NULL AND candidates.ts @@ rules.phrase)
                            OR (rules.marker IS NOT NULL
                                AND strpos(lower(candidates.text), rules.marker) > 0)
                    ), 0.0::double precision) AS distance
               FROM (
                    SELECT c.chunk_id, c.source_id, c.source_hash, c.text, c.ts,
                           (v.embedding <=> $4) AS raw_distance
                      FROM semantic_chunk_vectors v
                      JOIN collection_chunks c
                        ON c.tenant_id = v.tenant_id AND c.collection_id = v.collection_id
                       AND c.index_version_id = v.index_version_id AND c.chunk_id = v.chunk_id
                     WHERE v.tenant_id = $1 AND v.collection_id = $2 AND v.index_version_id = $3
                     ORDER BY v.embedding <=> $4, v.chunk_id
                     LIMIT $5
               ) AS candidates
              ORDER BY distance, candidates.chunk_id",
        )
        .bind(&self.tenant_id)
        .bind(collection_id)
        .bind(index_id)
        .bind(Vector::from(query.vector.clone()))
        .bind(limit)
        .bind(demotions_json)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_identity_digests_every_descriptor_parameter() {
        let a = SemanticSpace::from_descriptor(
            serde_json::json!({"model": "m", "query_instruction": "q"}),
            SEMANTIC_DIMS,
        )
        .unwrap();
        let b = SemanticSpace::from_descriptor(
            serde_json::json!({"query_instruction": "q", "model": "m"}),
            SEMANTIC_DIMS,
        )
        .unwrap();
        let c = SemanticSpace::from_descriptor(
            serde_json::json!({"model": "m", "query_instruction": "r"}),
            SEMANTIC_DIMS,
        )
        .unwrap();
        assert_eq!(a.identity, b.identity, "key order is not identity");
        assert_ne!(
            a.identity, c.identity,
            "an instruction change is a new space"
        );
        assert!(a.identity.starts_with("sem-") && a.identity.len() == 20);
        assert!(SemanticSpace::from_descriptor(serde_json::json!({}), 768).is_err());
    }
}
