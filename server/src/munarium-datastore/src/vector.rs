// SPDX-License-Identifier: Apache-2.0
//! Exact cosine vector search.
//!
//! Not a placeholder for a "real" engine. It provides four things at once
//! (§6.2): a fast path for small indexes, a **deterministic correctness
//! oracle** every approximate engine is measured against, the means to compute
//! recall for one, and a deployable fallback below the graph threshold. An
//! approximate index whose recall nobody can compute is an approximate index
//! nobody can promote.

use crate::Error;

/// A candidate from one leg, before fusion.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub chunk_id: String,
    /// Leg-native score. Lexical: higher is better. Vector: cosine DISTANCE,
    /// lower is better. The two are never compared numerically -- fusion works
    /// on ranks precisely because these scales are incommensurable.
    pub score: f32,
}

/// Produces vector candidates. Separate from the lexical trait so either
/// adapter can be replaced without touching the other, and so no adapter can
/// hide fusion inside a monolithic `search`.
pub trait VectorIndex: Send + Sync {
    fn vector_candidates(&self, embedding: &[f32], limit: usize) -> Result<Vec<Candidate>, Error>;
    fn dimensions(&self) -> usize;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Every vector held contiguously, scanned in full for each query.
#[derive(Debug, Clone, Default)]
pub struct FlatVectorIndex {
    dims: usize,
    chunk_ids: Vec<String>,
    data: Vec<f32>,
}

impl FlatVectorIndex {
    pub fn new(dims: usize) -> Self {
        Self {
            dims,
            chunk_ids: Vec::new(),
            data: Vec::new(),
        }
    }

    /// Vectors are stored **as given**, not re-normalized.
    ///
    /// Normalization is declared in the BuildSpec's embedder block, so it is
    /// part of the logical identity; silently normalizing here would make the
    /// artifact disagree with the spec that names it, and would change results
    /// for any embedder that legitimately does not normalize.
    pub fn push(&mut self, chunk_id: impl Into<String>, embedding: &[f32]) -> Result<(), Error> {
        if embedding.len() != self.dims {
            return Err(Error::Invalid(format!(
                "embedding has {} dimensions, index expects {}",
                embedding.len(),
                self.dims
            )));
        }
        if embedding.iter().any(|v| !v.is_finite()) {
            // A NaN would make every comparison false and quietly sort the
            // vector to wherever the algorithm happens to leave it.
            return Err(Error::Invalid(format!(
                "embedding for {:?} holds a non-finite value",
                chunk_id.into()
            )));
        }
        self.chunk_ids.push(chunk_id.into());
        self.data.extend_from_slice(embedding);
        Ok(())
    }

    /// Iterate `(chunk_id, embedding)` pairs in insertion order.
    ///
    /// Exists for the seal path: the writer stages every embedding here
    /// whatever engine the plan names, and an approximate build reads the
    /// staged rows back out. Also what a recall test iterates.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &[f32])> + '_ {
        let dims = self.dims.max(1);
        self.chunk_ids
            .iter()
            .map(String::as_str)
            .zip(self.data.chunks(dims))
    }

    /// Serialize: `f32` little-endian, preceded by the id table.
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        out.extend_from_slice(&(self.dims as u64).to_le_bytes());
        out.extend_from_slice(&(self.chunk_ids.len() as u64).to_le_bytes());
        for id in &self.chunk_ids {
            let b = id.as_bytes();
            out.extend_from_slice(&(b.len() as u32).to_le_bytes());
            out.extend_from_slice(b);
        }
        for v in &self.data {
            out.extend_from_slice(&v.to_le_bytes());
        }
        Ok(out)
    }

    /// Parse, checking every length against what remains rather than trusting
    /// the header: these bytes come from an untrusted store, and a declared
    /// count is an allocation instruction until it is bounded.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let bad = |what: &str| Error::Integrity(format!("vector index: {what}"));
        if bytes.len() < 16 {
            return Err(bad("shorter than its header"));
        }
        let dims = u64::from_le_bytes(bytes[0..8].try_into().unwrap()) as usize;
        let count = u64::from_le_bytes(bytes[8..16].try_into().unwrap()) as usize;
        if dims == 0 {
            return Err(bad("declares zero dimensions"));
        }
        let mut pos = 16;
        let mut chunk_ids = Vec::with_capacity(count.min(4096));
        for i in 0..count {
            if pos + 4 > bytes.len() {
                return Err(bad(&format!("truncated before id {i}")));
            }
            let len = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
            pos += 4;
            if pos + len > bytes.len() {
                return Err(bad(&format!("id {i} claims {len} bytes past the end")));
            }
            chunk_ids.push(
                String::from_utf8(bytes[pos..pos + len].to_vec())
                    .map_err(|_| bad(&format!("id {i} is not UTF-8")))?,
            );
            pos += len;
        }
        let want = count
            .checked_mul(dims)
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| bad("declared size overflows"))?;
        if bytes.len() - pos != want {
            return Err(bad(&format!(
                "body is {} bytes, expected {want} for {count}x{dims}",
                bytes.len() - pos
            )));
        }
        let mut data = Vec::with_capacity(count * dims);
        let (quads, _) = bytes[pos..].as_chunks::<4>();
        for c in quads {
            data.push(f32::from_le_bytes(*c));
        }
        Ok(Self {
            dims,
            chunk_ids,
            data,
        })
    }
}

fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        // A zero vector has no direction, so it has no cosine similarity to
        // anything. Returning the maximum distance keeps it last instead of
        // producing a NaN that would poison the sort.
        return 1.0;
    }
    1.0 - (dot / (na.sqrt() * nb.sqrt()))
}

impl VectorIndex for FlatVectorIndex {
    fn vector_candidates(&self, embedding: &[f32], limit: usize) -> Result<Vec<Candidate>, Error> {
        if embedding.len() != self.dims {
            return Err(Error::Invalid(format!(
                "query has {} dimensions, index holds {}",
                embedding.len(),
                self.dims
            )));
        }
        let mut scored: Vec<Candidate> = self
            .chunk_ids
            .iter()
            .enumerate()
            .map(|(i, id)| Candidate {
                chunk_id: id.clone(),
                score: cosine_distance(embedding, &self.data[i * self.dims..(i + 1) * self.dims]),
            })
            .collect();
        // Distance ascending, then chunk id -- a deterministic tie break, so
        // two equally close chunks always come back in the same order and a
        // golden test is possible at all.
        scored.sort_by(|a, b| {
            a.score
                .partial_cmp(&b.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.chunk_id.cmp(&b.chunk_id))
        });
        scored.truncate(limit);
        Ok(scored)
    }

    fn dimensions(&self) -> usize {
        self.dims
    }

    fn len(&self) -> usize {
        self.chunk_ids.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> FlatVectorIndex {
        let mut ix = FlatVectorIndex::new(3);
        ix.push("a", &[1.0, 0.0, 0.0]).unwrap();
        ix.push("b", &[0.0, 1.0, 0.0]).unwrap();
        ix.push("c", &[1.0, 1.0, 0.0]).unwrap();
        ix
    }

    #[test]
    fn nearest_first_by_cosine_distance() {
        let got = index().vector_candidates(&[1.0, 0.0, 0.0], 3).unwrap();
        assert_eq!(got[0].chunk_id, "a");
        assert!(got[0].score < got[1].score);
    }

    #[test]
    fn ties_break_on_chunk_id_so_results_are_reproducible() {
        let mut ix = FlatVectorIndex::new(2);
        ix.push("z", &[1.0, 0.0]).unwrap();
        ix.push("a", &[1.0, 0.0]).unwrap();
        let got = ix.vector_candidates(&[1.0, 0.0], 2).unwrap();
        assert_eq!(got[0].chunk_id, "a");
        assert_eq!(got[1].chunk_id, "z");
    }

    #[test]
    fn a_zero_vector_sorts_last_instead_of_producing_nan() {
        let mut ix = FlatVectorIndex::new(2);
        ix.push("zero", &[0.0, 0.0]).unwrap();
        ix.push("real", &[1.0, 0.0]).unwrap();
        let got = ix.vector_candidates(&[1.0, 0.0], 2).unwrap();
        assert_eq!(got[0].chunk_id, "real");
        assert!(got[1].score.is_finite());
    }

    #[test]
    fn dimension_mismatches_are_refused_both_ways() {
        let mut ix = FlatVectorIndex::new(3);
        assert!(ix.push("a", &[1.0, 0.0]).is_err());
        ix.push("a", &[1.0, 0.0, 0.0]).unwrap();
        assert!(ix.vector_candidates(&[1.0, 0.0], 1).is_err());
    }

    #[test]
    fn non_finite_embeddings_are_refused_at_the_door() {
        let mut ix = FlatVectorIndex::new(2);
        assert!(ix.push("nan", &[f32::NAN, 0.0]).is_err());
        assert!(ix.push("inf", &[f32::INFINITY, 0.0]).is_err());
    }

    #[test]
    fn serialization_round_trips_exactly() {
        let ix = index();
        let bytes = ix.to_bytes().unwrap();
        let back = FlatVectorIndex::from_bytes(&bytes).unwrap();
        assert_eq!(back.dimensions(), 3);
        assert_eq!(back.len(), 3);
        assert_eq!(
            back.vector_candidates(&[1.0, 0.0, 0.0], 3).unwrap(),
            ix.vector_candidates(&[1.0, 0.0, 0.0], 3).unwrap()
        );
    }

    #[test]
    fn truncated_or_lying_bytes_are_refused() {
        let bytes = index().to_bytes().unwrap();
        assert!(FlatVectorIndex::from_bytes(&bytes[..10]).is_err());
        assert!(FlatVectorIndex::from_bytes(&bytes[..bytes.len() - 4]).is_err());
        let mut lying = bytes.clone();
        lying[8..16].copy_from_slice(&9_999u64.to_le_bytes());
        assert!(FlatVectorIndex::from_bytes(&lying).is_err());
    }

    /// Vectors are stored as given. The BuildSpec declares whether the embedder
    /// normalizes, so normalizing here would make the artifact contradict the
    /// spec whose hash names it.
    #[test]
    fn unnormalized_vectors_are_not_silently_normalized() {
        let mut ix = FlatVectorIndex::new(2);
        ix.push("big", &[3.0, 4.0]).unwrap();
        let back = FlatVectorIndex::from_bytes(&ix.to_bytes().unwrap()).unwrap();
        assert_eq!(back.data, vec![3.0, 4.0]);
    }
}
