// SPDX-License-Identifier: Apache-2.0
//! Computing engine-neutral `RoutingEvidence` from a collection probe.
//!
//! The adapter half of §6.1's routing rule. `postgres` mode keeps
//! `select_collection_indices` — its density ranking is exercised by the
//! retrieval suites and raw `ts_rank` magnitudes are sound while one engine
//! produces them all. This module exists for the moment they are not: when a
//! datastore-served probe joins a fan-out, its BM25 magnitudes must never be
//! compared with `ts_rank`, and routing must read bounded, scale-free evidence
//! instead. Enabling that switch is gated on the routing corpus passing its
//! own parity gate; until then this code is exercised by its tests, which pin
//! the property that matters — on the measured routing scenarios it agrees
//! with the decisions the density blend made.

use munarium_core::retrieval::CollectionSearchResult;
use munarium_datastore::routing::{
    content_terms, evidence, query_phrases, rank, PoolSignals, RoutingEvidence,
};

/// One collection probe's evidence, computed from its bounded pool.
pub fn routing_evidence(query: &str, result: &CollectionSearchResult) -> RoutingEvidence {
    let terms = content_terms(query);
    let phrases = query_phrases(query);
    evidence_with(&terms, &phrases, result)
}

/// Rank probed collections by evidence, strongest first — the engine-neutral
/// analogue of `select_collection_indices`, over the same input shape.
pub fn rank_by_evidence(
    results: &[CollectionSearchResult],
    query: &str,
    phrase_boost: f64,
) -> Vec<usize> {
    let terms = content_terms(query);
    let phrases = query_phrases(query);
    let evidences: Vec<(&str, RoutingEvidence)> = results
        .iter()
        .map(|r| {
            (
                r.collection_name.as_str(),
                evidence_with(&terms, &phrases, r),
            )
        })
        .collect();
    rank(&evidences, phrase_boost)
}

fn evidence_with(
    terms: &[String],
    phrases: &[(String, String)],
    result: &CollectionSearchResult,
) -> RoutingEvidence {
    let texts: Vec<&str> = result.result.hits.iter().map(|h| h.text.as_str()).collect();

    // The margin ratio wants ONE leg's values, higher-is-better. Lexical
    // scores qualify directly. A vector-only pool's distances are converted
    // through 1/(1+d) — positive and order-preserving — so its margin means
    // the same thing; the two legs are never mixed in one ratio.
    let mut lexical: Vec<f64> = result
        .result
        .hits
        .iter()
        .filter_map(|h| h.lexical_score)
        .filter(|v| v.is_finite())
        .collect();
    lexical.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));

    let (top, third) = if !lexical.is_empty() {
        (lexical.first().copied(), lexical.get(2).copied())
    } else {
        let mut vector: Vec<f64> = result
            .result
            .hits
            .iter()
            .filter_map(|h| h.vector_distance)
            .filter(|v| v.is_finite())
            .map(|d| 1.0 / (1.0 + d.max(0.0)))
            .collect();
        vector.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        (vector.first().copied(), vector.get(2).copied())
    };

    evidence(
        terms,
        phrases,
        &PoolSignals {
            pool: &result.collection_name,
            texts,
            top_value: top,
            third_value: third,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use munarium_core::retrieval::{ProvenanceEnvelope, SearchHit, SearchResult};

    fn probe_with_texts(
        name: &str,
        lexical: &[f64],
        vector: &[f64],
        texts: &[&str],
    ) -> CollectionSearchResult {
        let count = lexical.len().max(vector.len()).max(texts.len());
        let hits = (0..count)
            .map(|index| SearchHit {
                chunk_id: format!("{name}-{index}"),
                source_id: format!("source-{name}-{index}"),
                source_path: format!("{name}/{index}.md"),
                source_content_hash: "hash".into(),
                text: texts.get(index).copied().unwrap_or("probe").into(),
                score: 0.0,
                lexical_rank: lexical.get(index).map(|_| index as u32 + 1),
                vector_rank: vector.get(index).map(|_| index as u32 + 1),
                lexical_score: lexical.get(index).copied(),
                vector_distance: vector.get(index).copied(),
                metadata: None,
            })
            .collect();
        CollectionSearchResult {
            collection_id: format!("id-{name}"),
            collection_name: name.into(),
            result: SearchResult {
                hits,
                envelope: ProvenanceEnvelope {
                    chunk_ids: Vec::new(),
                    source_ids: Vec::new(),
                    source_paths: Vec::new(),
                    source_content_hashes: Vec::new(),
                    index_version: "index".into(),
                    event_watermark: 0,
                    provider_fingerprint: None,
                },
            },
        }
    }

    /// The George Washington routing scenario (measured 2026-08-25), decided
    /// by evidence instead of density: the letterbooks — whose pools carry the
    /// subject's own name as a phrase — outrank the narrative that merely uses
    /// the query's words, at the same phrase boost the live runbook ships.
    #[test]
    fn evidence_routes_the_george_washington_scenario_like_the_measured_selection() {
        let probes = vec![
            probe_with_texts(
                "narrative",
                &[0.52, 0.51, 0.50],
                &[0.9],
                &[
                    "The canal boats reach the city of Washington; we visit the yards.",
                    "Through many cities did our narrator visit on the way to Washington.",
                    "A description of Washington city, which travellers visit often.",
                ],
            ),
            probe_with_texts(
                "letterbook-b",
                &[0.17, 0.17, 0.16],
                &[0.9],
                &[
                    "# George Washington Papers, Series 2: Letterbook 17",
                    "George Washington to Henry Knox, October 1789.",
                    "Washington left New York on his tour and lodged at Rye.",
                ],
            ),
            probe_with_texts(
                "letterbook-a",
                &[0.18, 0.17, 0.17],
                &[0.9],
                &[
                    "# George Washington Papers, Series 4: General Correspondence",
                    "George Washington to Israel Putnam, May 21, 1776.",
                    "Orders for the march to the North River.",
                ],
            ),
        ];
        let ranked = rank_by_evidence(&probes, "What cities did George Washington visit?", 3.0);
        let names: Vec<&str> = ranked
            .iter()
            .map(|&i| probes[i].collection_name.as_str())
            .collect();
        // The measured property is letterbooks-over-narrative. The order
        // WITHIN the letterbooks legitimately differs from the density
        // blend's (density had a > b by raw magnitude; bounded evidence
        // breaks their tie on margin) — asserting it would pin an accident.
        let mut top: Vec<&str> = names[..2].to_vec();
        top.sort_unstable();
        assert_eq!(
            top,
            vec!["letterbook-a", "letterbook-b"],
            "the subject's own collections lead: {names:?}"
        );
        assert_eq!(names[2], "narrative");
    }

    /// The Tea Party routing scenario (measured 2026-08-25): the phrase is
    /// later coinage, so weak phrase evidence must not override the density
    /// signal — carried here by the multi-term fraction, since every newspaper
    /// candidate matches boston+tea and almost no narrative candidate matches
    /// two terms at all.
    #[test]
    fn evidence_routes_the_tea_party_scenario_like_the_measured_selection() {
        let mut narrative_texts = vec!["The city and its press, described at length."; 19];
        narrative_texts.push("Reprinting what the colonial newspapers said of it.");
        let probes = vec![
            probe_with_texts("narrative", &[0.13, 0.12, 0.11], &[0.8], &narrative_texts),
            probe_with_texts(
                "newspaper",
                &[0.16, 0.15, 0.14],
                &[0.8],
                &["Boston, December 20. The tea was destroyed last Thursday."; 20],
            ),
        ];
        let query = "How did colonial newspapers report the Boston Tea Party?";
        for boost in [3.0, 0.0] {
            let ranked = rank_by_evidence(&probes, query, boost);
            assert_eq!(
                probes[ranked[0]].collection_name, "newspaper",
                "density-shaped evidence leads at boost {boost}"
            );
        }
    }

    /// A vector-only pool still gets a margin, through the order-preserving
    /// distance conversion — and an empty pool is excluded.
    #[test]
    fn vector_only_pools_have_evidence_and_empty_pools_are_excluded() {
        let probes = vec![
            probe_with_texts("vec-only", &[], &[0.10, 0.20, 0.90], &["tea in boston"; 3]),
            probe_with_texts("empty", &[], &[], &[]),
        ];
        let e = routing_evidence("boston tea", &probes[0]);
        assert!(e.top_margin > 0.0, "distances 0.1 vs 0.9 leave a margin");
        assert_eq!(rank_by_evidence(&probes, "boston tea", 3.0), vec![0]);
    }
}
