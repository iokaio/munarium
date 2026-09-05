// SPDX-License-Identifier: Apache-2.0
//! The record store: the answer to "what did this hit actually say".
//!
//! Holds the stable source path, source id, node id, ordinal, text hash and
//! text for every chunk, so a historical exact-version read is answerable from
//! the artifact alone — **without consulting mutable source metadata** (§5.2).
//! That is the point: an artifact pinned by a session must still resolve its
//! citations after the source has been re-ingested, renamed, or deleted.
//!
//! Format `munarium-records@1`: a length-prefixed JSON-Lines body plus a
//! fixed-width offset index. Chosen for v1 because it is trivially verifiable
//! and debuggable by eye; the manifest names the format, so replacing it later
//! is a new physical artifact of the same logical version rather than a
//! migration.

use serde::{Deserialize, Serialize};

use crate::Error;

/// One stored chunk, as it will be returned with a hit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChunkRecord {
    pub chunk_id: String,
    pub source_id: String,
    pub source_path: String,
    pub node_id: Option<String>,
    pub ordinal: u32,
    pub text: String,
    /// Lowercase hex of the chunk text's SHA-256, so a hit can be shown to be
    /// the bytes that were indexed rather than a later edit of them.
    pub text_sha256: String,
}

/// The serialized pair: `chunks.bin` (bodies) and `chunks.idx` (offsets).
pub struct RecordsBlob {
    pub body: Vec<u8>,
    pub index: Vec<u8>,
}

/// Serialize records in the given order. Order is preserved exactly: the
/// ordinal is data, not a position, but the offset index addresses by position
/// and callers rely on stable iteration.
pub fn write_records(records: &[ChunkRecord]) -> Result<RecordsBlob, Error> {
    let mut body = Vec::new();
    let mut index = Vec::with_capacity(records.len() * 8);
    for r in records {
        let offset = body.len() as u64;
        index.extend_from_slice(&offset.to_le_bytes());
        let line = serde_json::to_vec(r).map_err(|e| Error::Invalid(e.to_string()))?;
        if line.contains(&b'\n') {
            // serde_json never emits a bare newline inside a compact document,
            // so this is a corrupt-encoder check rather than an input check --
            // but a newline here would silently shift every later offset.
            return Err(Error::Invalid(format!(
                "record {} serialized with an embedded newline",
                r.chunk_id
            )));
        }
        body.extend_from_slice(&line);
        body.push(b'\n');
    }
    Ok(RecordsBlob { body, index })
}

/// Read every record back. Validates the index against the body rather than
/// trusting either: both arrive from an untrusted store.
pub fn read_records(body: &[u8], index: &[u8]) -> Result<Vec<ChunkRecord>, Error> {
    if !index.len().is_multiple_of(8) {
        return Err(Error::Integrity(format!(
            "record index is {} bytes, not a multiple of 8",
            index.len()
        )));
    }
    let count = index.len() / 8;
    let mut out = Vec::with_capacity(count.min(4096));
    for i in 0..count {
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&index[i * 8..i * 8 + 8]);
        let start = u64::from_le_bytes(buf) as usize;
        if start > body.len() {
            return Err(Error::Integrity(format!(
                "record {i} starts at {start}, past the {}-byte body",
                body.len()
            )));
        }
        let rest = &body[start..];
        let end = rest
            .iter()
            .position(|b| *b == b'\n')
            .ok_or_else(|| Error::Integrity(format!("record {i} has no terminator")))?;
        let record: ChunkRecord = serde_json::from_slice(&rest[..end])
            .map_err(|e| Error::Integrity(format!("record {i} does not parse: {e}")))?;
        out.push(record);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: &str) -> ChunkRecord {
        ChunkRecord {
            chunk_id: id.into(),
            source_id: "src-1".into(),
            source_path: "a/b.md".into(),
            node_id: None,
            ordinal: 0,
            text: format!("body of {id}"),
            text_sha256: "0".repeat(64),
        }
    }

    #[test]
    fn records_round_trip_in_order() {
        let records = vec![rec("c1"), rec("c2"), rec("c3")];
        let blob = write_records(&records).unwrap();
        let back = read_records(&blob.body, &blob.index).unwrap();
        assert_eq!(back, records);
    }

    #[test]
    fn an_empty_set_round_trips() {
        let blob = write_records(&[]).unwrap();
        assert!(read_records(&blob.body, &blob.index).unwrap().is_empty());
    }

    #[test]
    fn a_truncated_index_is_refused_rather_than_guessed() {
        let blob = write_records(&[rec("c1")]).unwrap();
        let err = read_records(&blob.body, &blob.index[..7]).unwrap_err();
        assert!(err.to_string().contains("multiple of 8"), "{err}");
    }

    #[test]
    fn an_offset_past_the_body_is_refused() {
        let blob = write_records(&[rec("c1")]).unwrap();
        let bad = 9_999u64.to_le_bytes().to_vec();
        let err = read_records(&blob.body, &bad).unwrap_err();
        assert!(err.to_string().contains("past the"), "{err}");
    }

    /// Corruption inside a body must surface as an error, not as a record with
    /// plausible-looking wrong content.
    #[test]
    fn a_corrupt_body_is_refused() {
        let blob = write_records(&[rec("c1")]).unwrap();
        let mut body = blob.body.clone();
        body[3] = b'!';
        assert!(read_records(&body, &blob.index).is_err());
    }

    /// Unicode text must survive byte-for-byte: a citation quotes what was
    /// indexed, and a normalizing round trip would make the quote a paraphrase.
    #[test]
    fn unicode_text_survives_the_round_trip() {
        let mut r = rec("c1");
        r.text = "Café 東京 — “quoted”…".to_string();
        let blob = write_records(std::slice::from_ref(&r)).unwrap();
        let back = read_records(&blob.body, &blob.index).unwrap();
        assert_eq!(back[0].text, r.text);
    }
}
