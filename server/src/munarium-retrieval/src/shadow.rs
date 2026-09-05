// SPDX-License-Identifier: Apache-2.0
//! Shadow comparison: what the two engines actually disagreed about.
//!
//! In `shadow` mode PostgreSQL answers the user and a sampled copy of the same
//! query runs against a datastore artifact. Neither the answer nor the error
//! ever comes from the shadow — its only product is the record in this module
//! (§13.2), and the record is what stage 6's per-corpus gates read.
//!
//! ## Why the legs are compared separately
//!
//! `ts_rank` and BM25 are not numerically comparable (§6.1), so a post-fusion
//! difference alone cannot say WHICH engine moved. Reporting lexical-leg and
//! vector-leg overlap separately, and only then the fused top-k, is what makes
//! a regression attributable: a fused difference with identical legs is a
//! fusion bug, a lexical difference with an identical vector leg is an
//! analyzer difference, and both moving at once is a content problem.
//!
//! ## Nothing here holds query text
//!
//! A comparison carries a [`QueryFingerprint`], never the query. These records
//! are aggregated, logged and retained; the raw question a user asked is the
//! most sensitive thing in a retrieval request, and a struct with nowhere to
//! put it cannot leak it.

use std::collections::{HashMap, HashSet};

use munarium_core::retrieval::SearchHit;

/// A stable, non-reversible identifier for one query formulation.
///
/// Truncated SHA-256 over the exact text that was searched. Two identical
/// questions fingerprint alike, which is what makes "this query class always
/// diverges" a findable pattern; nothing recovers the text from it.
///
/// The FORMULATION is fingerprinted, not the user's original: a query that
/// number-form expansion or model expansion rewrote is a different question to
/// the engine, and comparing two engines on it means comparing them on what
/// they were actually asked.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct QueryFingerprint(String);

impl QueryFingerprint {
    pub fn of(query: &str) -> Self {
        use sha2::{Digest, Sha256};
        // Whitespace-folded and lowercased first, so the same question typed
        // twice fingerprints alike. This is deliberately NOT the analyzer's
        // normalization: the analyzer is one of the things under test, and a
        // fingerprint that changed when it changed would make a comparison
        // record unjoinable across the fix it measures.
        let folded = query.split_whitespace().collect::<Vec<_>>().join(" ");
        let digest = Sha256::digest(folded.to_lowercase().as_bytes());
        Self(hex::encode(digest)[..16].to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for QueryFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// How a shadow execution ended.
///
/// A drop is not a failure. Shadow work is the first thing shed under load by
/// design (§13.2), and counting a deliberate shed as an error would make a
/// healthy busy replica look broken on the one dashboard that decides whether
/// the rollout proceeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowOutcome {
    /// Both sides ran and were compared.
    Completed,
    /// Not sampled. Recorded so the sample rate is observable rather than
    /// inferred from a missing row.
    NotSampled,
    /// Shed before it started: no permit, or the process was over budget.
    Dropped,
    /// The candidate did not finish inside the shadow deadline.
    Timeout,
    /// The candidate refused: no binding, unsupported reader, corrupt artifact.
    Rejected,
    /// The candidate failed for another reason.
    Error,
}

impl ShadowOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::NotSampled => "not_sampled",
            Self::Dropped => "dropped",
            Self::Timeout => "timeout",
            Self::Rejected => "rejected",
            Self::Error => "error",
        }
    }
}

/// One leg's agreement, computed over candidate ids in rank order.
#[derive(Debug, Clone, PartialEq)]
pub struct LegComparison {
    pub reference_count: usize,
    pub candidate_count: usize,
    /// Ids both legs returned.
    pub overlap: usize,
    /// `overlap / reference_count`, or 1.0 when the reference leg was empty
    /// AND so was the candidate — two engines agreeing that nothing matches is
    /// agreement, not a divide-by-zero.
    pub overlap_fraction: f64,
    /// Mean absolute rank movement over the overlapping ids. `None` when
    /// nothing overlapped, because the mean of no movements is not zero
    /// movement — it is no measurement, and reporting 0.0 would read as
    /// perfect agreement.
    pub mean_rank_movement: Option<f64>,
    /// The largest single rank movement, for the same reason a mean alone
    /// hides one badly moved hit among fifty still ones.
    pub max_rank_movement: Option<usize>,
}

impl LegComparison {
    /// Compare two ranked id lists. Position in the slice IS the rank.
    pub fn of(reference: &[String], candidate: &[String]) -> Self {
        let cand_rank: HashMap<&str, usize> = candidate
            .iter()
            .enumerate()
            .map(|(i, id)| (id.as_str(), i))
            .collect();

        let mut movements = Vec::new();
        for (i, id) in reference.iter().enumerate() {
            if let Some(&j) = cand_rank.get(id.as_str()) {
                movements.push(i.abs_diff(j));
            }
        }
        let overlap = movements.len();
        let overlap_fraction = if reference.is_empty() {
            if candidate.is_empty() {
                1.0
            } else {
                0.0
            }
        } else {
            overlap as f64 / reference.len() as f64
        };

        Self {
            reference_count: reference.len(),
            candidate_count: candidate.len(),
            overlap,
            overlap_fraction,
            mean_rank_movement: (!movements.is_empty())
                .then(|| movements.iter().sum::<usize>() as f64 / movements.len() as f64),
            max_rank_movement: movements.iter().copied().max(),
        }
    }
}

/// What the answer keys say, when there is one.
#[derive(Debug, Clone, PartialEq)]
pub struct RelevanceMovement {
    /// 0-based rank of the first relevant hit in the reference, if any.
    pub reference_first_relevant: Option<usize>,
    pub candidate_first_relevant: Option<usize>,
}

impl RelevanceMovement {
    /// Positive means the candidate found the first relevant hit LATER — a
    /// regression. `None` when either side found none, because "moved from
    /// nowhere to rank 3" is not a movement, it is a different event and
    /// collapsing them would hide a fix as readily as a regression.
    pub fn delta(&self) -> Option<i64> {
        match (self.reference_first_relevant, self.candidate_first_relevant) {
            (Some(r), Some(c)) => Some(c as i64 - r as i64),
            _ => None,
        }
    }
}

/// Where two result sets disagree about identity rather than order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IdentityDelta {
    pub missing_sources: Vec<String>,
    pub extra_sources: Vec<String>,
    pub missing_chunks: Vec<String>,
    pub extra_chunks: Vec<String>,
    /// Chunks both sides returned whose text hash differs. This is the serious
    /// one: the same citation resolving to different bytes means one of the
    /// two is serving content that is not what it claims.
    pub text_hash_mismatches: Vec<String>,
}

impl IdentityDelta {
    /// Whether anything here invalidates a citation.
    ///
    /// A missing or extra hit is a ranking difference and is expected while the
    /// analyzers differ. A text-hash mismatch is not: it means one engine
    /// resolved a chunk id to different content, which no tolerance band should
    /// ever accept.
    pub fn is_corrupting(&self) -> bool {
        !self.text_hash_mismatches.is_empty()
    }
}

/// Per-phase timing, in milliseconds.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PhaseLatency {
    pub lexical_ms: f64,
    pub vector_ms: f64,
    pub fusion_ms: f64,
    pub total_ms: f64,
}

/// One sampled comparison (§13.2).
#[derive(Debug, Clone, PartialEq)]
pub struct ShadowComparison {
    pub query_fingerprint: QueryFingerprint,
    /// The LOGICAL version both sides answered. Identical by construction —
    /// a shadow that compared two different corpus versions would measure the
    /// corpus, not the engine.
    pub reference_version: String,
    /// The physical artifact the candidate used, and the engine that read it.
    pub candidate_artifact_id: Option<String>,
    pub candidate_engine: Option<String>,
    pub outcome: ShadowOutcome,
    /// Present only when `outcome` is `Completed`.
    pub lexical: Option<LegComparison>,
    pub vector: Option<LegComparison>,
    pub fused: Option<LegComparison>,
    pub relevance: Option<RelevanceMovement>,
    pub identity: IdentityDelta,
    /// True when the two sides disagreed about a hit's provenance envelope.
    pub provenance_mismatch: bool,
    pub reference_latency: PhaseLatency,
    pub candidate_latency: PhaseLatency,
}

impl ShadowComparison {
    /// A record for a shadow that never produced results.
    ///
    /// Recorded rather than skipped: a drop rate that is invisible looks like a
    /// sample rate, and the difference decides whether a parity window means
    /// anything.
    pub fn unrun(
        query_fingerprint: QueryFingerprint,
        reference_version: impl Into<String>,
        outcome: ShadowOutcome,
    ) -> Self {
        Self {
            query_fingerprint,
            reference_version: reference_version.into(),
            candidate_artifact_id: None,
            candidate_engine: None,
            outcome,
            lexical: None,
            vector: None,
            fused: None,
            relevance: None,
            identity: IdentityDelta::default(),
            provenance_mismatch: false,
            reference_latency: PhaseLatency::default(),
            candidate_latency: PhaseLatency::default(),
        }
    }

    /// Whether this comparison found something no tolerance band may absorb.
    pub fn is_corrupting(&self) -> bool {
        self.identity.is_corrupting() || self.provenance_mismatch
    }
}

/// The two sides of one comparison, as the caller collected them.
pub struct ComparisonInput<'a> {
    pub query: &'a str,
    pub reference_version: &'a str,
    pub candidate_artifact_id: &'a str,
    pub candidate_engine: &'a str,
    /// Ranked chunk ids from each side's lexical leg, before fusion.
    pub reference_lexical: &'a [String],
    pub candidate_lexical: &'a [String],
    /// The same for the vector leg. Empty on both sides is legitimate — a
    /// lexical-only collection has no vector leg to disagree about.
    pub reference_vector: &'a [String],
    pub candidate_vector: &'a [String],
    /// The final, fused, shaped hits each side would have returned.
    pub reference_hits: &'a [SearchHit],
    pub candidate_hits: &'a [SearchHit],
    /// Chunk ids an answer key marks relevant, if this query has one.
    pub relevant_chunks: Option<&'a HashSet<String>>,
    pub reference_latency: PhaseLatency,
    pub candidate_latency: PhaseLatency,
}

/// Compute the comparison. Pure: no clock, no I/O, no randomness.
pub fn compare(input: ComparisonInput<'_>) -> ShadowComparison {
    let ref_ids: Vec<String> = input
        .reference_hits
        .iter()
        .map(|h| h.chunk_id.clone())
        .collect();
    let cand_ids: Vec<String> = input
        .candidate_hits
        .iter()
        .map(|h| h.chunk_id.clone())
        .collect();

    let ref_by_id: HashMap<&str, &SearchHit> = input
        .reference_hits
        .iter()
        .map(|h| (h.chunk_id.as_str(), h))
        .collect();

    // Identity, over the FUSED sets: what a caller would actually have cited.
    let ref_set: HashSet<&str> = ref_ids.iter().map(String::as_str).collect();
    let cand_set: HashSet<&str> = cand_ids.iter().map(String::as_str).collect();
    let mut identity = IdentityDelta {
        missing_chunks: sorted_diff(&ref_set, &cand_set),
        extra_chunks: sorted_diff(&cand_set, &ref_set),
        ..Default::default()
    };

    let ref_sources: HashSet<&str> = input
        .reference_hits
        .iter()
        .map(|h| h.source_id.as_str())
        .collect();
    let cand_sources: HashSet<&str> = input
        .candidate_hits
        .iter()
        .map(|h| h.source_id.as_str())
        .collect();
    identity.missing_sources = sorted_diff(&ref_sources, &cand_sources);
    identity.extra_sources = sorted_diff(&cand_sources, &ref_sources);

    // The serious check, over hits BOTH sides returned: does one chunk id
    // resolve to two different documents?
    let mut provenance_mismatch = false;
    for cand in input.candidate_hits {
        let Some(reference) = ref_by_id.get(cand.chunk_id.as_str()) else {
            continue;
        };
        if reference.source_content_hash != cand.source_content_hash {
            identity.text_hash_mismatches.push(cand.chunk_id.clone());
        }
        // Same chunk id, different document identity or path: the citation
        // would point somewhere else entirely.
        if reference.source_id != cand.source_id || reference.source_path != cand.source_path {
            provenance_mismatch = true;
        }
    }
    identity.text_hash_mismatches.sort();

    let relevance = input.relevant_chunks.map(|keys| RelevanceMovement {
        reference_first_relevant: first_relevant(&ref_ids, keys),
        candidate_first_relevant: first_relevant(&cand_ids, keys),
    });

    ShadowComparison {
        query_fingerprint: QueryFingerprint::of(input.query),
        reference_version: input.reference_version.to_string(),
        candidate_artifact_id: Some(input.candidate_artifact_id.to_string()),
        candidate_engine: Some(input.candidate_engine.to_string()),
        outcome: ShadowOutcome::Completed,
        lexical: Some(LegComparison::of(
            input.reference_lexical,
            input.candidate_lexical,
        )),
        vector: Some(LegComparison::of(
            input.reference_vector,
            input.candidate_vector,
        )),
        fused: Some(LegComparison::of(&ref_ids, &cand_ids)),
        relevance,
        identity,
        provenance_mismatch,
        reference_latency: input.reference_latency,
        candidate_latency: input.candidate_latency,
    }
}

fn sorted_diff(a: &HashSet<&str>, b: &HashSet<&str>) -> Vec<String> {
    let mut v: Vec<String> = a.difference(b).map(|s| (*s).to_string()).collect();
    v.sort();
    v
}

fn first_relevant(ids: &[String], keys: &HashSet<String>) -> Option<usize> {
    ids.iter().position(|id| keys.contains(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(chunk: &str, source: &str, hash: &str) -> SearchHit {
        SearchHit {
            chunk_id: chunk.into(),
            source_id: source.into(),
            source_path: format!("corpus/{source}.md"),
            source_content_hash: hash.into(),
            text: "text".into(),
            score: 1.0,
            lexical_rank: None,
            vector_rank: None,
            lexical_score: None,
            vector_distance: None,
            metadata: None,
        }
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    /// The record carries a fingerprint and has nowhere to put the query.
    /// Asserted on the rendered form, because a field added later would be
    /// caught here rather than in a log nobody reads.
    #[test]
    fn a_comparison_never_carries_query_text() {
        let secret = "what is the settlement amount for Copperline";
        let c = ShadowComparison::unrun(
            QueryFingerprint::of(secret),
            "idx-1",
            ShadowOutcome::Dropped,
        );
        let rendered = format!("{c:?}");
        assert!(!rendered.contains("Copperline"), "{rendered}");
        assert!(!rendered.to_lowercase().contains("settlement"));
        assert_eq!(c.query_fingerprint.as_str().len(), 16);
    }

    /// The fingerprint folds whitespace and case so the same question asked
    /// twice joins, and differs for a different question.
    #[test]
    fn the_fingerprint_is_stable_and_discriminating() {
        assert_eq!(
            QueryFingerprint::of("What did  Washington write?"),
            QueryFingerprint::of("what did washington write?")
        );
        assert_ne!(
            QueryFingerprint::of("what did washington write?"),
            QueryFingerprint::of("what did adams write?")
        );
    }

    /// Two engines agreeing that nothing matches is agreement. A reference leg
    /// with no hits must not divide by zero, and must not read as total
    /// disagreement.
    #[test]
    fn two_empty_legs_agree_and_an_empty_reference_with_hits_does_not() {
        let empty = LegComparison::of(&[], &[]);
        assert_eq!(empty.overlap_fraction, 1.0);
        assert_eq!(empty.mean_rank_movement, None);

        let appeared = LegComparison::of(&[], &ids(&["a"]));
        assert_eq!(appeared.overlap_fraction, 0.0);
        assert_eq!(appeared.candidate_count, 1);
    }

    /// No overlap means no measurement, not zero movement. Reporting 0.0 here
    /// would read as perfect agreement on a leg that agreed about nothing.
    #[test]
    fn no_overlap_reports_no_movement_rather_than_zero() {
        let c = LegComparison::of(&ids(&["a", "b"]), &ids(&["c", "d"]));
        assert_eq!(c.overlap, 0);
        assert_eq!(c.overlap_fraction, 0.0);
        assert_eq!(c.mean_rank_movement, None);
        assert_eq!(c.max_rank_movement, None);
    }

    /// Movement is measured per id, and the max is reported beside the mean
    /// because one badly moved hit among many still ones is the interesting
    /// case a mean hides.
    #[test]
    fn rank_movement_reports_both_the_mean_and_the_worst() {
        // a: 0->0 (0), b: 1->3 (2), c: 2->1 (1)
        let c = LegComparison::of(&ids(&["a", "b", "c"]), &ids(&["a", "c", "x", "b"]));
        assert_eq!(c.overlap, 3);
        assert_eq!(c.max_rank_movement, Some(2));
        assert!((c.mean_rank_movement.unwrap() - 1.0).abs() < 1e-9);
    }

    /// The legs are compared independently of the fused set, which is what
    /// makes a difference attributable to an engine rather than to fusion.
    #[test]
    fn identical_legs_with_a_different_fusion_isolates_the_fusion() {
        let lex = ids(&["a", "b"]);
        let vec_ = ids(&["b", "a"]);
        let c = compare(ComparisonInput {
            query: "q",
            reference_version: "idx-1",
            candidate_artifact_id: "art",
            candidate_engine: "tantivy",
            reference_lexical: &lex,
            candidate_lexical: &lex,
            reference_vector: &vec_,
            candidate_vector: &vec_,
            reference_hits: &[hit("a", "s1", "h1"), hit("b", "s2", "h2")],
            candidate_hits: &[hit("b", "s2", "h2"), hit("a", "s1", "h1")],
            relevant_chunks: None,
            reference_latency: PhaseLatency::default(),
            candidate_latency: PhaseLatency::default(),
        });
        assert_eq!(c.lexical.as_ref().unwrap().overlap_fraction, 1.0);
        assert_eq!(c.lexical.as_ref().unwrap().max_rank_movement, Some(0));
        assert_eq!(c.vector.as_ref().unwrap().max_rank_movement, Some(0));
        // Both legs identical, the fused order swapped: fusion moved it.
        assert_eq!(c.fused.as_ref().unwrap().max_rank_movement, Some(1));
        assert!(!c.is_corrupting());
    }

    /// One chunk id resolving to different bytes is corrupting, and no
    /// tolerance band may absorb it.
    #[test]
    fn a_text_hash_mismatch_on_a_shared_chunk_is_corrupting() {
        let c = compare(ComparisonInput {
            query: "q",
            reference_version: "idx-1",
            candidate_artifact_id: "art",
            candidate_engine: "tantivy",
            reference_lexical: &ids(&["a"]),
            candidate_lexical: &ids(&["a"]),
            reference_vector: &[],
            candidate_vector: &[],
            reference_hits: &[hit("a", "s1", "hash-one")],
            candidate_hits: &[hit("a", "s1", "hash-two")],
            relevant_chunks: None,
            reference_latency: PhaseLatency::default(),
            candidate_latency: PhaseLatency::default(),
        });
        assert_eq!(c.identity.text_hash_mismatches, vec!["a".to_string()]);
        assert!(c.is_corrupting());
    }

    /// The same chunk id pointing at a different document is a provenance
    /// mismatch even when the bytes happen to hash alike.
    #[test]
    fn a_shared_chunk_id_on_a_different_document_is_a_provenance_mismatch() {
        let c = compare(ComparisonInput {
            query: "q",
            reference_version: "idx-1",
            candidate_artifact_id: "art",
            candidate_engine: "tantivy",
            reference_lexical: &ids(&["a"]),
            candidate_lexical: &ids(&["a"]),
            reference_vector: &[],
            candidate_vector: &[],
            reference_hits: &[hit("a", "s1", "h")],
            candidate_hits: &[hit("a", "s2", "h")],
            relevant_chunks: None,
            reference_latency: PhaseLatency::default(),
            candidate_latency: PhaseLatency::default(),
        });
        assert!(c.provenance_mismatch);
        assert!(c.is_corrupting());
    }

    /// Identity is reported over sources as well as chunks: a candidate that
    /// returns the same document through different chunks has not lost the
    /// answer, and a report that only counted chunks would say it had.
    #[test]
    fn missing_and_extra_are_reported_for_sources_and_chunks_separately() {
        let c = compare(ComparisonInput {
            query: "q",
            reference_version: "idx-1",
            candidate_artifact_id: "art",
            candidate_engine: "tantivy",
            reference_lexical: &[],
            candidate_lexical: &[],
            reference_vector: &[],
            candidate_vector: &[],
            reference_hits: &[hit("a#1", "s1", "h")],
            candidate_hits: &[hit("a#2", "s1", "h")],
            relevant_chunks: None,
            reference_latency: PhaseLatency::default(),
            candidate_latency: PhaseLatency::default(),
        });
        assert_eq!(c.identity.missing_chunks, vec!["a#1".to_string()]);
        assert_eq!(c.identity.extra_chunks, vec!["a#2".to_string()]);
        assert!(c.identity.missing_sources.is_empty(), "same document");
        assert!(c.identity.extra_sources.is_empty());
        assert!(
            !c.is_corrupting(),
            "a chunk-level difference is a ranking difference"
        );
    }

    /// A first-relevant movement is only a movement when both sides found one.
    #[test]
    fn first_relevant_movement_needs_both_sides_to_have_found_something() {
        let keys: HashSet<String> = ["gold".to_string()].into_iter().collect();
        let both = RelevanceMovement {
            reference_first_relevant: Some(0),
            candidate_first_relevant: Some(3),
        };
        assert_eq!(both.delta(), Some(3), "the candidate found it later");

        let vanished = RelevanceMovement {
            reference_first_relevant: Some(0),
            candidate_first_relevant: None,
        };
        assert_eq!(vanished.delta(), None, "not a movement, a different event");

        let c = compare(ComparisonInput {
            query: "q",
            reference_version: "idx-1",
            candidate_artifact_id: "art",
            candidate_engine: "tantivy",
            reference_lexical: &[],
            candidate_lexical: &[],
            reference_vector: &[],
            candidate_vector: &[],
            reference_hits: &[hit("noise", "s1", "h"), hit("gold", "s2", "h")],
            candidate_hits: &[hit("gold", "s2", "h")],
            relevant_chunks: Some(&keys),
            reference_latency: PhaseLatency::default(),
            candidate_latency: PhaseLatency::default(),
        });
        let r = c.relevance.unwrap();
        assert_eq!(r.reference_first_relevant, Some(1));
        assert_eq!(r.candidate_first_relevant, Some(0));
        assert_eq!(r.delta(), Some(-1), "the candidate found it sooner");
    }

    /// A drop is recorded, not skipped. A drop rate that is invisible looks
    /// like a sample rate.
    #[test]
    fn an_unrun_shadow_still_produces_a_record() {
        for outcome in [
            ShadowOutcome::NotSampled,
            ShadowOutcome::Dropped,
            ShadowOutcome::Timeout,
            ShadowOutcome::Rejected,
            ShadowOutcome::Error,
        ] {
            let c = ShadowComparison::unrun(QueryFingerprint::of("q"), "idx-1", outcome);
            assert_eq!(c.outcome, outcome);
            assert!(c.fused.is_none(), "nothing was compared");
            assert!(
                !c.is_corrupting(),
                "a shadow that did not run found nothing"
            );
        }
    }
}
