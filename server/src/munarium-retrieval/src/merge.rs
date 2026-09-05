// SPDX-License-Identifier: Apache-2.0
//! The cross-collection merge, adapted onto the engine-neutral fusion.
//!
//! This is the fusion gate from the datastore design ("cross-engine
//! fusion is a merge hazard"): the raw-score `merge_hits_weighted` in
//! `munarium-retrieval-pg` was correct while one engine produced every score,
//! and had to be replaced by the engine-neutral pooled merge BEFORE any
//! mixed-engine multi-search could exist. The replacement is verified by
//! equivalence: in `postgres` mode — every measurement in one domain — the
//! pooled merge must produce the identical order and identical scores, and the
//! tests below prove it against the historical implementation as an oracle.
//!
//! What changes when engines mix: every measurement carries a comparability
//! domain, raw values are only compared within a domain, and a mixed
//! stratum-leg interleaves per-domain ranks instead of comparing magnitudes.
//! The rollout selector's unit covers a whole multi-search set, so a mixed
//! merge should be unreachable; if one happens anyway it is logged loudly
//! rather than silently mis-ordered.

use munarium_core::retrieval::{CollectionSearchResult, MergeWeights, SearchHit};
use munarium_datastore::fusion::{fuse_pools, Measure, PoolCandidate, PoolMergeWeights};

/// The lexical comparability domain for measurements PostgreSQL produced.
/// Versioned like the analyzer it names: `ts_rank` over the `english`
/// configuration. A different engine, or a changed analyzer, is a different
/// domain.
pub const PG_LEXICAL_DOMAIN: &str = "postgresql/ts_rank/english@1";

/// The vector comparability domain: the EMBEDDER, not the engine. Cosine
/// distances under one embedder are comparable whichever engine computed them,
/// which is why a mixed-engine search splits only its lexical leg.
pub const LOCAL_VECTOR_DOMAIN: &str = "local/local-hash@1/256";

/// Merge per-collection results into one globally fused list, unweighted.
pub fn merge_hits(
    results: &[CollectionSearchResult],
    top_k: usize,
    rrf_k: f64,
) -> Vec<(String, SearchHit)> {
    merge_hits_weighted(results, top_k, rrf_k, &MergeWeights::default())
}

/// [`merge_hits`] with per-leg weights, the collection-evidence leg and the
/// probe stratum — the same contract the historical merge carried, now
/// executed by `munarium_datastore::fusion::fuse_pools`.
///
/// Every hit today is PostgreSQL-produced, so the adapter tags every lexical
/// measurement [`PG_LEXICAL_DOMAIN`] and every vector measurement
/// [`LOCAL_VECTOR_DOMAIN`]: one domain per leg, which is the case the
/// equivalence tests pin to the historical behaviour. stage 6's dispatch will
/// tag per-collection domains from the serving binding, at which point a mixed
/// set becomes representable — and detected, rather than silently fused.
pub fn merge_hits_weighted(
    results: &[CollectionSearchResult],
    top_k: usize,
    rrf_k: f64,
    weights: &MergeWeights,
) -> Vec<(String, SearchHit)> {
    // Flattened in input order, exactly as the historical merge did — the
    // ordinal is the tie-of-ties preserver.
    let flat: Vec<(&str, &SearchHit)> = results
        .iter()
        .flat_map(|r| {
            r.result
                .hits
                .iter()
                .map(move |h| (r.collection_name.as_str(), h))
        })
        .collect();

    let candidates: Vec<PoolCandidate> = flat
        .iter()
        .enumerate()
        .map(|(ordinal, (pool, hit))| PoolCandidate {
            pool: (*pool).to_string(),
            ordinal,
            chunk_id: hit.chunk_id.clone(),
            lexical: hit.lexical_score.map(|v| Measure {
                domain: PG_LEXICAL_DOMAIN.into(),
                value: v,
            }),
            // Negated: the pooled merge is canonical higher-is-better, and
            // negation preserves the ordering distance gave, ties included.
            vector: hit.vector_distance.map(|d| Measure {
                domain: LOCAL_VECTOR_DOMAIN.into(),
                value: -d,
            }),
        })
        .collect();

    let pooled = PoolMergeWeights {
        lexical: weights.lexical,
        vector: weights.vector,
        rrf_k,
        pool_evidence: weights.collection_evidence,
        pool_rank: weights
            .collection_rank
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect(),
        probe_pools: weights.probe_collections.iter().cloned().collect(),
        probe_weight: weights.probe_weight,
    };

    let outcome = fuse_pools(&candidates, top_k, &pooled);
    if outcome.diagnostics.mixed_domain {
        // Unreachable while the selector covers whole multi-search sets. If it
        // fires, the ordering above was decided by rank interleave rather than
        // raw measurement — sound, but a state someone must go look at.
        tracing::warn!(
            lexical_domains = ?outcome.diagnostics.lexical_domains,
            vector_domains = ?outcome.diagnostics.vector_domains,
            "cross-collection merge saw measurements from more than one domain"
        );
    }

    outcome
        .ranked
        .into_iter()
        .map(|(ordinal, score)| {
            let (pool, hit) = flat[ordinal];
            let mut hit = hit.clone();
            hit.score = score;
            (pool.to_string(), hit)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use munarium_core::retrieval::{ProvenanceEnvelope, SearchResult};

    fn hit(chunk_id: &str, lexical_score: Option<f64>, vector_distance: Option<f64>) -> SearchHit {
        SearchHit {
            chunk_id: chunk_id.into(),
            source_id: format!("src-{chunk_id}"),
            source_path: format!("{chunk_id}.md"),
            source_content_hash: "0".repeat(64),
            text: String::new(),
            score: 0.0,
            lexical_rank: lexical_score.map(|_| 1),
            vector_rank: vector_distance.map(|_| 1),
            lexical_score,
            vector_distance,
            metadata: None,
        }
    }

    fn coll(name: &str, hits: Vec<SearchHit>) -> CollectionSearchResult {
        CollectionSearchResult {
            collection_id: format!("col-{name}"),
            collection_name: name.into(),
            result: SearchResult {
                hits,
                envelope: ProvenanceEnvelope {
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

    /// Equal to the historical implementation, order AND score, on every case
    /// in a battery built to hit its edges: ties, missing legs, probe strata,
    /// the evidence leg, truncation, the top_k=0 default, scoreless hits, and
    /// a many-collection starvation shape.
    ///
    /// The oracle is `munarium_retrieval_pg::merge_hits_weighted` — the code
    /// this module replaces, kept precisely so this equivalence stays
    /// checkable. "No top-k change in postgres mode" is the gate the decision
    /// log set for this swap, and equality of every score is a stronger
    /// statement than equality of the top k.
    #[test]
    fn the_pooled_merge_is_byte_identical_to_the_historical_merge() {
        let cases: Vec<(Vec<CollectionSearchResult>, usize, f64, MergeWeights)> = vec![
            // The starvation regression shape.
            (
                vec![
                    coll(
                        "commercial",
                        vec![
                            hit("com-a", Some(0.9), Some(0.20)),
                            hit("com-b", Some(0.7), Some(0.25)),
                        ],
                    ),
                    coll("tax", vec![hit("tax-a", None, Some(0.90))]),
                    coll("insurance", vec![hit("ins-a", None, Some(0.95))]),
                ],
                3,
                60.0,
                MergeWeights::default(),
            ),
            // Scoreless legacy hits.
            (
                vec![
                    coll("with-evidence", vec![hit("ev-a", Some(0.5), None)]),
                    coll("legacy", vec![hit("old-a", None, None)]),
                ],
                10,
                60.0,
                MergeWeights::default(),
            ),
            // Exact ties across collections, resolved by chunk id.
            (
                vec![
                    coll("z-pool", vec![hit("zzz", Some(0.5), None)]),
                    coll("a-pool", vec![hit("aaa", Some(0.5), None)]),
                ],
                5,
                60.0,
                MergeWeights::default(),
            ),
            // The evidence leg reordering, measured 2026-08-25.
            (
                vec![
                    coll(
                        "narrative",
                        vec![
                            hit("nar-0", Some(0.30), None),
                            hit("nar-1", Some(0.29), None),
                        ],
                    ),
                    coll(
                        "letterbook",
                        vec![
                            hit("let-0", Some(0.20), None),
                            hit("let-1", Some(0.19), None),
                        ],
                    ),
                    coll("fragments", vec![hit("fra-0", None, Some(0.1))]),
                ],
                5,
                60.0,
                MergeWeights {
                    lexical: 1.0,
                    vector: 0.3,
                    collection_evidence: 2.0,
                    collection_rank: [("letterbook".to_string(), 1), ("narrative".to_string(), 2)]
                        .into_iter()
                        .collect(),
                    ..Default::default()
                },
            ),
            // Probe strata with a scaled probe weight.
            (
                vec![
                    coll(
                        "deep-letterbook",
                        vec![
                            hit("deep-letterbook-0", Some(0.030), None),
                            hit("deep-letterbook-1", Some(0.028), None),
                        ],
                    ),
                    coll(
                        "probe-narrative",
                        vec![
                            hit("probe-narrative-0", Some(0.20), None),
                            hit("probe-narrative-1", Some(0.19), None),
                        ],
                    ),
                ],
                4,
                60.0,
                MergeWeights {
                    collection_evidence: 1.0,
                    collection_rank: [
                        ("deep-letterbook".to_string(), 1),
                        ("probe-narrative".to_string(), 20),
                    ]
                    .into_iter()
                    .collect(),
                    probe_collections: ["probe-narrative".to_string()].into_iter().collect(),
                    probe_weight: 0.5,
                    ..Default::default()
                },
            ),
            // top_k = 0 must keep the historical default of 10.
            (
                vec![coll(
                    "wide",
                    (0..15)
                        .map(|i| hit(&format!("c{i:02}"), Some(1.0 - i as f64 * 0.01), None))
                        .collect(),
                )],
                0,
                60.0,
                MergeWeights::default(),
            ),
            // rrf_k = 0 falls back to 60 in both implementations.
            (
                vec![coll("k", vec![hit("k-a", Some(0.4), Some(0.2))])],
                5,
                0.0,
                MergeWeights::default(),
            ),
            // Many collections, small top_k — the starvation regime itself.
            (
                (0..12)
                    .map(|i| {
                        coll(
                            &format!("pool-{i:02}"),
                            vec![
                                hit(
                                    &format!("p{i:02}-0"),
                                    Some(0.1 + i as f64 * 0.01),
                                    Some(0.5),
                                ),
                                hit(&format!("p{i:02}-1"), Some(0.05), Some(0.6)),
                            ],
                        )
                    })
                    .collect(),
                5,
                60.0,
                MergeWeights::default(),
            ),
        ];

        for (i, (results, top_k, rrf_k, weights)) in cases.iter().enumerate() {
            let old = munarium_retrieval_pg::merge_hits_weighted(results, *top_k, *rrf_k, weights);
            let new = merge_hits_weighted(results, *top_k, *rrf_k, weights);
            assert_eq!(old.len(), new.len(), "case {i}: length");
            for (j, (o, n)) in old.iter().zip(new.iter()).enumerate() {
                assert_eq!(o.0, n.0, "case {i} hit {j}: collection");
                assert_eq!(o.1.chunk_id, n.1.chunk_id, "case {i} hit {j}: chunk");
                assert_eq!(
                    o.1.score.to_bits(),
                    n.1.score.to_bits(),
                    "case {i} hit {j}: score must be BIT-identical, got {} vs {}",
                    o.1.score,
                    n.1.score
                );
            }
        }
    }

    /// The 2026-08-24 starvation regression, guarded here against the NEW
    /// implementation — the pinned test must not die with the code it pinned.
    #[test]
    fn merge_fuses_globally_not_by_per_collection_rank() {
        let rrf_k = 60.0;
        let relevant = coll(
            "commercial",
            vec![
                hit("com-a", Some(0.9), Some(0.20)),
                hit("com-b", Some(0.7), Some(0.25)),
            ],
        );
        let noise1 = coll("tax", vec![hit("tax-a", None, Some(0.90))]);
        let noise2 = coll("insurance", vec![hit("ins-a", None, Some(0.95))]);

        let merged = merge_hits(&[relevant, noise1, noise2], 3, rrf_k);
        let order: Vec<&str> = merged.iter().map(|(_, h)| h.chunk_id.as_str()).collect();
        assert_eq!(&order[..2], &["com-a", "com-b"], "got {order:?}");
        assert!(merged.windows(2).all(|w| w[0].1.score >= w[1].1.score));
        assert!(merged[0].1.score > merged[2].1.score);
    }

    /// Scoreless hits sort last but are still returned deterministically.
    #[test]
    fn merge_puts_scoreless_hits_last() {
        let a = coll("with-evidence", vec![hit("ev-a", Some(0.5), None)]);
        let b = coll("legacy", vec![hit("old-a", None, None)]);
        let merged = merge_hits(&[a, b], 10, 60.0);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].1.chunk_id, "ev-a");
        assert_eq!(merged[1].1.chunk_id, "old-a");
        assert_eq!(merged[1].1.score, 0.0);
    }
}
