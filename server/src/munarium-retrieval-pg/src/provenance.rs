// SPDX-License-Identifier: Apache-2.0
//! Locations describe the exact extracted text of a pinned index, not mutable files.
use crate::{chunk_text, storage_err, PgRetrieval};
use munarium_core::{retrieval::SearchHit, Result};
use munarium_extract::{Extracted, ExtractionMethod};
use serde_json::{json, Value};
use sqlx::Row;
use std::collections::HashMap;

/// Only exact, ordered matches get offsets. Paragraph whitespace normalization
/// can make a packed chunk non-contiguous in the extracted text; never invent
/// a location in that case. UTF-8 bytes and Unicode scalar offsets are explicit.
pub fn locations(extracted: &Extracted, max_chars: usize) -> Vec<Value> {
    let mut cursor = 0;
    chunk_text(&extracted.text, max_chars).iter().map(|chunk| {
        let found = extracted.text[cursor..].find(chunk).map(|n| cursor + n);
        // After an unmatched normalized chunk, later repeated text could refer
        // to that earlier chunk. Stop mapping rather than guess its position.
        if found.is_none() { cursor = extracted.text.len(); }
        let location = found.map(|start| {
            let end = start + chunk.len();
            cursor = end;
            let blocks: Vec<usize> = extracted.pages.iter().filter(|p| p.start < end && p.end > start).map(|p| p.number).collect();
            json!({"utf8_start": start, "utf8_length": chunk.len(),
                "character_start": extracted.text[..start].chars().count(), "character_length": chunk.chars().count(),
                "unit": if extracted.method == ExtractionMethod::Docx { "paragraph" } else { "page" },
                "numbers": blocks})
        });
        json!({"location": location, "extraction_method": extracted.method.as_str()})
    }).collect()
}

impl PgRetrieval {
    pub(crate) async fn record_chunk_provenance(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        index: &str,
        source: &str,
        path: &str,
        extracted: &Extracted,
        max_chars: usize,
    ) -> Result<()> {
        let rows: Vec<Value> = locations(extracted, max_chars)
            .into_iter()
            .enumerate()
            .map(|(n, metadata)| json!({"chunk_id": format!("{source}#{n}"), "metadata": metadata}))
            .collect();
        sqlx::query("INSERT INTO chunk_provenance (tenant_id, index_version_id, chunk_id, source_path, metadata)
            SELECT $1, $2, r.chunk_id, $3, r.metadata FROM jsonb_to_recordset($4) AS r(chunk_id text, metadata jsonb)
            ON CONFLICT DO NOTHING")
            .bind(&self.tenant_id).bind(index).bind(path).bind(json!(rows)).execute(&mut **tx).await.map_err(storage_err)?;
        Ok(())
    }

    pub async fn enrich_chunk_provenance(&self, index: &str, hits: &mut [SearchHit]) -> Result<()> {
        let ids: Vec<&str> = hits.iter().map(|h| h.chunk_id.as_str()).collect();
        let rows = sqlx::query(
            "SELECT chunk_id, source_path, metadata FROM chunk_provenance
            WHERE tenant_id = $1 AND index_version_id = $2 AND chunk_id = ANY($3)",
        )
        .bind(&self.tenant_id)
        .bind(index)
        .bind(ids)
        .fetch_all(self.pool())
        .await
        .map_err(storage_err)?;
        let by_id: HashMap<String, (String, Value)> = rows
            .iter()
            .map(|r| (r.get("chunk_id"), (r.get("source_path"), r.get("metadata"))))
            .collect();
        for hit in hits {
            if let Some((path, metadata)) = by_id.get(&hit.chunk_id) {
                hit.source_path = path.clone();
                hit.metadata = Some(metadata.clone());
            }
        }
        Ok(())
    }

    /// Compatibility for artifacts built before source hashes were recorded.
    /// Read ONLY the pinned chunk rows; never substitute current source hashes.
    pub async fn pinned_source_hashes(
        &self,
        index: &str,
        collection: Option<&str>,
        ids: &[String],
    ) -> Result<HashMap<String, String>> {
        let rows: Vec<(String, String)> = if let Some(collection) = collection {
            sqlx::query_as("SELECT DISTINCT source_id, source_hash FROM collection_chunks WHERE tenant_id=$1 AND collection_id=$2 AND index_version_id=$3 AND source_id=ANY($4)")
                .bind(&self.tenant_id).bind(collection).bind(index).bind(ids).fetch_all(self.pool()).await
        } else {
            sqlx::query_as("SELECT DISTINCT source_id, source_hash FROM index_chunks WHERE tenant_id=$1 AND index_version_id=$2 AND source_id=ANY($3)")
                .bind(&self.tenant_id).bind(index).bind(ids).fetch_all(self.pool()).await
        }.map_err(storage_err)?;
        let mut hashes = HashMap::new();
        for (id, hash) in rows {
            if hashes
                .insert(id, hash.clone())
                .is_some_and(|old| old != hash)
            {
                return Err(munarium_core::KernelError::Storage(
                    "pinned index has conflicting source hashes".into(),
                ));
            }
        }
        Ok(hashes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use munarium_extract::{ExtractionStatus, PageSpan};
    #[test]
    fn unicode_and_repeated_passages_get_ordered_locations_and_docx_is_not_a_page() {
        let extracted = Extracted {
            text: "écho\n\nécho".into(),
            method: ExtractionMethod::Docx,
            status: ExtractionStatus::Ok,
            pages: vec![
                PageSpan {
                    number: 1,
                    start: 0,
                    end: 5,
                },
                PageSpan {
                    number: 2,
                    start: 7,
                    end: 12,
                },
            ],
            note: None,
        };
        let mapped = locations(&extracted, 5);
        assert_eq!(mapped[0]["location"]["character_length"], 4);
        assert_eq!(mapped[1]["location"]["utf8_start"], 7);
        assert_eq!(mapped[1]["location"]["character_start"], 6);
        assert_eq!(mapped[1]["location"]["unit"], "paragraph");
        assert_eq!(mapped[1]["location"]["numbers"], json!([2]));
    }
    #[test]
    fn normalized_packing_does_not_invent_offsets() {
        let extracted = Extracted {
            text: "First  \n\n  Second".into(),
            method: ExtractionMethod::Text,
            status: ExtractionStatus::Ok,
            pages: vec![],
            note: None,
        };
        assert!(locations(&extracted, 100)[0]["location"].is_null());
    }
}
