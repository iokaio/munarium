// SPDX-License-Identifier: Apache-2.0
//! Engine-neutral fusion and deterministic shaping.
//!
//! Fusion here consumes **ranks**, never raw leg scores. That is not a stylistic
//! preference: PostgreSQL `ts_rank` and Tantivy BM25 are not numerically
//! comparable, and neither is a BM25 score against a cosine distance. A merge
//! that adds them produces plausible results in the wrong order — the failure
//! is silent, which is what makes it dangerous.
//!
//! The existing PostgreSQL path fuses from raw leg scores on purpose (it is
//! sound while one engine produces every score, and a 2026-08-24 regression
//! test pins the reason). This module is what has to replace that call site
//! before any multi-search can mix engines — see
//! the datastore design: "cross-engine fusion is a merge hazard".
//!
//! Every policy is named and versioned so a recorded result can be explained.

use std::collections::HashMap;

use crate::vector::Candidate;

/// The fusion policy version, recorded in diagnostics. Bump when the formula
/// or the tie-break changes, so a stored result stays interpretable.
pub const FUSION_POLICY_VERSION: u32 = 1;

/// Per-leg weights. Defaults reproduce plain RRF over both legs.
#[derive(Debug, Clone, PartialEq)]
pub struct FusionWeights {
    pub lexical: f64,
    pub vector: f64,
    /// The RRF constant. 60 is the conventional value and the one the
    /// PostgreSQL path already uses, so results stay comparable across engines.
    pub rrf_k: f64,
}

impl Default for FusionWeights {
    fn default() -> Self {
        Self {
            lexical: 1.0,
            vector: 1.0,
            rrf_k: 60.0,
        }
    }
}

/// Descending order over scores that may not be finite, as a TOTAL order.
///
/// `partial_cmp(..).unwrap_or(Equal)` is not one: a NaN compares "equal" to
/// every value while those values compare unequal to each other, which is
/// not transitive, and since Rust 1.81 `sort_by` may panic on a comparator
/// that is not a total order — and where it does not panic, the order is
/// arbitrary. A NaN can arrive from a caller-supplied measurement (pgvector's
/// `<=>` is NaN against a zero-norm vector, and the coordinator negates it
/// straight into a `Measure`). It sorts LAST, deterministically.
fn descending(a: f64, b: f64) -> std::cmp::Ordering {
    let key = |v: f64| if v.is_nan() { f64::NEG_INFINITY } else { v };
    key(b).total_cmp(&key(a))
}

/// A fused hit, with the per-leg diagnostics shadow comparison needs.
///
/// The leg ranks are carried rather than discarded because §13.2 requires
/// lexical-leg and vector-leg movement to be reported SEPARATELY from
/// post-fusion movement: "the answer changed" and "one leg changed" are
/// different findings with different causes.
#[derive(Debug, Clone, PartialEq)]
pub struct FusedHit {
    pub chunk_id: String,
    pub score: f64,
    pub lexical_rank: Option<u32>,
    pub vector_rank: Option<u32>,
    pub lexical_score: Option<f32>,
    pub vector_score: Option<f32>,
}

/// Reciprocal-rank fusion over two candidate lists.
///
/// Each list must already be in its own leg's best-first order; ranks are
/// assigned from position, so a caller that hands over an unsorted list gets a
/// wrong answer rather than an error. That is deliberate — sorting here would
/// hide an adapter returning candidates in arbitrary order, which is a defect
/// worth surfacing in the adapter's own tests.
pub fn fuse(lexical: &[Candidate], vector: &[Candidate], w: &FusionWeights) -> Vec<FusedHit> {
    let mut acc: HashMap<&str, FusedHit> = HashMap::new();

    for (i, c) in lexical.iter().enumerate() {
        let rank = i as u32 + 1;
        let e = acc.entry(&c.chunk_id).or_insert_with(|| FusedHit {
            chunk_id: c.chunk_id.clone(),
            score: 0.0,
            lexical_rank: None,
            vector_rank: None,
            lexical_score: None,
            vector_score: None,
        });
        e.lexical_rank = Some(rank);
        e.lexical_score = Some(c.score);
        e.score += w.lexical / (w.rrf_k + rank as f64);
    }
    for (i, c) in vector.iter().enumerate() {
        let rank = i as u32 + 1;
        let e = acc.entry(&c.chunk_id).or_insert_with(|| FusedHit {
            chunk_id: c.chunk_id.clone(),
            score: 0.0,
            lexical_rank: None,
            vector_rank: None,
            lexical_score: None,
            vector_score: None,
        });
        e.vector_rank = Some(rank);
        e.vector_score = Some(c.score);
        e.score += w.vector / (w.rrf_k + rank as f64);
    }

    let mut hits: Vec<FusedHit> = acc.into_values().collect();
    // Stable final ordering (§6.4): fused score descending, then chunk id.
    // The tie break is not cosmetic -- rank-1 in each leg scores identically,
    // so without it the top of a hybrid result would depend on hash iteration
    // order and no golden test could exist.
    hits.sort_by(|a, b| descending(a.score, b.score).then_with(|| a.chunk_id.cmp(&b.chunk_id)));
    hits
}

// ---------------------------------------------------------------------------
// The cross-pool merge (§6.3) — the engine-neutral replacement for the
// coordinator's raw-score multi-collection merge.
// ---------------------------------------------------------------------------

/// The pooled-merge policy version, recorded in diagnostics beside
/// [`FUSION_POLICY_VERSION`]. Bump when the formula, the strata, the domain
/// composition or the tie-break changes.
pub const POOL_MERGE_POLICY_VERSION: u32 = 1;

/// One raw leg measurement, and the domain within which it may be compared.
///
/// The domain is the whole reason this type exists. A raw lexical relevance is
/// comparable across pools only while ONE engine with one analyzer produced
/// every value — PostgreSQL `ts_rank` against Tantivy BM25 is not a comparison,
/// it is a category error whose failure mode is plausible results in the wrong
/// order (decisions.md, "cross-engine fusion is a merge hazard"). Carrying the
/// domain on the measurement makes the incommensurable case DETECTABLE at the
/// merge instead of silent: values sharing a domain are ordered by value;
/// values in different domains are never numerically compared.
///
/// `value` is canonical-direction: HIGHER IS BETTER. A distance-shaped leg
/// (cosine distance, lower is better) is negated by the caller. Negation
/// preserves the ordering exactly, including the NaN case — two NaNs compare
/// `None` either way — so the adaptation costs nothing in fidelity.
#[derive(Debug, Clone, PartialEq)]
pub struct Measure {
    /// Comparability domain: an engine + analyzer identity for a lexical leg,
    /// an embedder fingerprint for a vector leg. Opaque here. Note the vector
    /// consequence — two ENGINES serving one embedder's vectors share a
    /// domain, so in a mixed-engine search only the lexical leg splits.
    pub domain: String,
    pub value: f64,
}

/// One pooled candidate, as the coordinator's adapter presents it.
///
/// Deliberately free of text, provenance and hit payloads: the neutral merge
/// ranks identities, and the caller maps the ranking back onto its own hits by
/// `ordinal`. A merge that carried document text would be a merge that could
/// leak it.
#[derive(Debug, Clone, PartialEq)]
pub struct PoolCandidate {
    /// The pool (collection) this candidate came from. Opaque label.
    pub pool: String,
    /// The caller's index for this candidate, echoed back in the ranking.
    pub ordinal: usize,
    /// Tie-break identity — the citation target.
    pub chunk_id: String,
    pub lexical: Option<Measure>,
    pub vector: Option<Measure>,
}

/// Weights and strata for the pooled merge. Field-for-field the semantics of
/// the coordinator's `MergeWeights`, restated here so this crate does not
/// depend on it.
#[derive(Debug, Clone, PartialEq)]
pub struct PoolMergeWeights {
    pub lexical: f64,
    pub vector: f64,
    pub rrf_k: f64,
    /// Multiplier on the pool-evidence leg: every candidate also receives
    /// `1/(k + rank of its pool)` where the rank is what the caller's
    /// collection selection assigned (1 = strongest evidence).
    pub pool_evidence: f64,
    /// Pool label → 1-based evidence rank. A `BTreeMap` so iteration order —
    /// and anything downstream of it — is a function of content, never of
    /// hash seeds.
    pub pool_rank: std::collections::BTreeMap<String, usize>,
    /// Pools whose candidates carry ORIGINAL-query measurements rather than
    /// the deep (expanded) search's. Raw values are only comparable within one
    /// query formulation, so these rank in their own stratum.
    pub probe_pools: std::collections::BTreeSet<String>,
    /// Multiplier on the probe stratum's leg contributions.
    pub probe_weight: f64,
}

impl Default for PoolMergeWeights {
    fn default() -> Self {
        Self {
            lexical: 1.0,
            vector: 1.0,
            rrf_k: 60.0,
            pool_evidence: 0.0,
            pool_rank: std::collections::BTreeMap::new(),
            probe_pools: std::collections::BTreeSet::new(),
            probe_weight: 1.0,
        }
    }
}

/// What the merge decided, plus how it decided it.
#[derive(Debug, Clone, PartialEq)]
pub struct PoolMergeOutcome {
    /// `(ordinal, fused score)` in final serving order, truncated to `top_k`
    /// (0 = the historical default of 10).
    pub ranked: Vec<(usize, f64)>,
    pub diagnostics: PoolMergeDiagnostics,
}

/// The named-and-versioned record §6.3 requires: enough to explain a stored
/// ordering without re-running it.
#[derive(Debug, Clone, PartialEq)]
pub struct PoolMergeDiagnostics {
    pub policy_version: u32,
    /// Distinct lexical domains seen, in first-seen order.
    pub lexical_domains: Vec<String>,
    pub vector_domains: Vec<String>,
    /// True when any single stratum-leg held measurements from more than one
    /// domain — the case where raw values were NOT compared and per-domain
    /// rank interleaving decided instead. A caller should surface this loudly:
    /// with the rollout selector covering whole multi-search sets it is
    /// unreachable, so seeing it means the selector let a mixed set through.
    pub mixed_domain: bool,
}

/// Fuse pooled candidates across collections, engine-neutrally.
///
/// The algorithm is the coordinator's measured 2026-08-24/25 design, restated
/// over domain-tagged measurements:
///
/// 1. Per stratum (deep, then probe) and per leg, order candidates globally —
///    by raw value WITHIN a domain (descending, chunk-id tie-break). This is
///    the starvation fix: a relevant pool's rank-2 outranks an irrelevant
///    pool's rank-1 because the raw measurements say so, where per-pool RRF
///    ranks tie every rank-1 at 1/(k+1).
/// 2. Fused score = RRF over those global leg ranks, weighted per leg and per
///    stratum, plus the pool-evidence leg.
/// 3. Final order: fused score descending, chunk id, then input order.
///
/// **When one stratum-leg holds two domains** the values are never compared:
/// each domain is ordered internally, and the leg's global order interleaves
/// them by per-domain rank (rank, then domain, then chunk id). Interleaving
/// two globally-ordered lists is not the per-collection tie that starved —
/// that failure needed one list per COLLECTION with pool count near `top_k` —
/// and it is the strongest statement available without comparing numbers that
/// share no scale. The diagnostics flag it, because the selector contract says
/// it should never happen.
pub fn fuse_pools(
    candidates: &[PoolCandidate],
    top_k: usize,
    w: &PoolMergeWeights,
) -> PoolMergeOutcome {
    let k = if w.rrf_k > 0.0 { w.rrf_k } else { 60.0 };
    let is_probe = |c: &PoolCandidate| w.probe_pools.contains(&c.pool);

    let mut fused = vec![0.0f64; candidates.len()];
    let mut lexical_domains: Vec<String> = Vec::new();
    let mut vector_domains: Vec<String> = Vec::new();
    let mut mixed = false;

    // Accumulation order is load-bearing for float identity with the
    // coordinator's historical merge: deep lexical, deep vector, probe
    // lexical, probe vector, then pool evidence. Float addition is not
    // associative, and "byte-identical in postgres mode" is the gate.
    for probe_stratum in [false, true] {
        let stratum_weight = if probe_stratum { w.probe_weight } else { 1.0 };
        for leg in [Leg::Lexical, Leg::Vector] {
            let leg_weight = match leg {
                Leg::Lexical => w.lexical,
                Leg::Vector => w.vector,
            };
            fn pick(c: &PoolCandidate, leg: Leg) -> Option<&Measure> {
                match leg {
                    Leg::Lexical => c.lexical.as_ref(),
                    Leg::Vector => c.vector.as_ref(),
                }
            }
            let members: Vec<usize> = (0..candidates.len())
                .filter(|&i| {
                    is_probe(&candidates[i]) == probe_stratum && pick(&candidates[i], leg).is_some()
                })
                .collect();
            if members.is_empty() {
                continue;
            }
            let seen = match leg {
                Leg::Lexical => &mut lexical_domains,
                Leg::Vector => &mut vector_domains,
            };
            for &i in &members {
                let d = &pick(&candidates[i], leg).unwrap().domain;
                if !seen.contains(d) {
                    seen.push(d.clone());
                }
            }
            let mut stratum_domains: Vec<&str> = members
                .iter()
                .map(|&i| pick(&candidates[i], leg).unwrap().domain.as_str())
                .collect();
            stratum_domains.sort_unstable();
            stratum_domains.dedup();

            let by_value = |a: &usize, b: &usize| {
                // Every member of a stratum has this leg (that is what a
                // stratum is), so the `None` arm is unreachable; it sorts last
                // rather than panicking on the off chance.
                let (va, vb) = (
                    pick(&candidates[*a], leg).map_or(f64::NAN, |x| x.value),
                    pick(&candidates[*b], leg).map_or(f64::NAN, |x| x.value),
                );
                descending(va, vb).then(candidates[*a].chunk_id.cmp(&candidates[*b].chunk_id))
            };

            let ordered: Vec<usize> = if stratum_domains.len() <= 1 {
                // The single-domain path: raw values order the leg globally.
                // Operation-identical to the historical merge.
                let mut m = members;
                m.sort_by(by_value);
                m
            } else {
                // Two or more domains in one stratum-leg: refuse the numeric
                // comparison, interleave by per-domain rank.
                mixed = true;
                let mut with_rank: Vec<(usize, usize)> = Vec::with_capacity(members.len());
                for domain in &stratum_domains {
                    let mut own: Vec<usize> = members
                        .iter()
                        .copied()
                        .filter(|&i| pick(&candidates[i], leg).unwrap().domain == *domain)
                        .collect();
                    own.sort_by(by_value);
                    with_rank.extend(own.into_iter().enumerate());
                }
                with_rank.sort_by(|&(ra, a), &(rb, b)| {
                    ra.cmp(&rb)
                        .then_with(|| {
                            pick(&candidates[a], leg)
                                .unwrap()
                                .domain
                                .cmp(&pick(&candidates[b], leg).unwrap().domain)
                        })
                        .then(candidates[a].chunk_id.cmp(&candidates[b].chunk_id))
                });
                with_rank.into_iter().map(|(_, i)| i).collect()
            };

            for (rank, &i) in ordered.iter().enumerate() {
                fused[i] += stratum_weight * leg_weight / (k + rank as f64 + 1.0);
            }
        }
    }

    if w.pool_evidence > 0.0 {
        for (i, c) in candidates.iter().enumerate() {
            if let Some(&rank) = w.pool_rank.get(&c.pool) {
                fused[i] += w.pool_evidence / (k + rank.max(1) as f64);
            }
        }
    }

    let mut ranked: Vec<(usize, f64)> = fused.into_iter().enumerate().collect();
    // A stable sort with the chunk-id tie-break, so a full tie preserves the
    // caller's input order — the same guarantee the historical merge gave.
    ranked.sort_by(|a, b| {
        descending(a.1, b.1).then(candidates[a.0].chunk_id.cmp(&candidates[b.0].chunk_id))
    });
    ranked.truncate(if top_k == 0 { 10 } else { top_k });

    PoolMergeOutcome {
        ranked,
        diagnostics: PoolMergeDiagnostics {
            policy_version: POOL_MERGE_POLICY_VERSION,
            lexical_domains,
            vector_domains,
            mixed_domain: mixed,
        },
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Leg {
    Lexical,
    Vector,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(id: &str, score: f32) -> Candidate {
        Candidate {
            chunk_id: id.into(),
            score,
        }
    }

    #[test]
    fn a_chunk_in_both_legs_outranks_one_in_either() {
        let lex = vec![c("a", 9.0), c("b", 8.0)];
        let vec_ = vec![c("b", 0.1), c("c", 0.2)];
        let hits = fuse(&lex, &vec_, &FusionWeights::default());
        assert_eq!(hits[0].chunk_id, "b", "b is the only chunk in both legs");
        assert_eq!(hits[0].lexical_rank, Some(2));
        assert_eq!(hits[0].vector_rank, Some(1));
    }

    #[test]
    fn leg_diagnostics_survive_for_shadow_comparison() {
        let hits = fuse(&[c("a", 9.0)], &[c("a", 0.25)], &FusionWeights::default());
        assert_eq!(hits[0].lexical_score, Some(9.0));
        assert_eq!(hits[0].vector_score, Some(0.25));
    }

    /// The property that makes fusion engine-neutral: only the ORDER of a leg
    /// matters, never the magnitudes. Rescaling one leg's scores by any factor
    /// leaves the fused result identical -- which is exactly why a Tantivy leg
    /// can replace a Postgres one without recalibration.
    #[test]
    fn rescaling_a_legs_scores_does_not_change_the_fusion() {
        let lex = vec![c("a", 9.0), c("b", 8.0), c("c", 7.0)];
        let rescaled = vec![c("a", 900.0), c("b", 0.8), c("c", 0.07)];
        let vec_ = vec![c("c", 0.1)];
        let w = FusionWeights::default();
        let a: Vec<String> = fuse(&lex, &vec_, &w)
            .into_iter()
            .map(|h| h.chunk_id)
            .collect();
        let b: Vec<String> = fuse(&rescaled, &vec_, &w)
            .into_iter()
            .map(|h| h.chunk_id)
            .collect();
        assert_eq!(a, b);
    }

    #[test]
    fn equal_scores_break_on_chunk_id_not_hash_order() {
        // Two chunks at rank 1 of one leg each: identical fused scores.
        let hits = fuse(&[c("z", 1.0)], &[c("a", 0.0)], &FusionWeights::default());
        assert_eq!(hits[0].chunk_id, "a");
        assert_eq!(hits[1].chunk_id, "z");
        assert!((hits[0].score - hits[1].score).abs() < f64::EPSILON);
    }

    #[test]
    fn an_empty_leg_is_not_an_error_and_contributes_nothing() {
        let hits = fuse(&[c("a", 1.0)], &[], &FusionWeights::default());
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].vector_rank, None);
        assert!(fuse(&[], &[], &FusionWeights::default()).is_empty());
    }

    #[test]
    fn weights_can_silence_a_leg_entirely() {
        let w = FusionWeights {
            vector: 0.0,
            ..FusionWeights::default()
        };
        let hits = fuse(&[c("lex", 1.0)], &[c("vec", 0.0)], &w);
        assert_eq!(hits[0].chunk_id, "lex");
        // Still PRESENT, with its rank recorded -- silenced is not the same as
        // dropped, and a diagnostic that vanished would hide why it lost.
        assert!(hits.iter().any(|h| h.chunk_id == "vec"));
        assert_eq!(hits[1].score, 0.0);
    }
}

#[cfg(test)]
mod pool_tests {
    use super::*;

    const LEX: &str = "postgresql/ts_rank@1";
    const VEC: &str = "local/local-hash@1/256";

    fn cand(
        pool: &str,
        ordinal: usize,
        chunk: &str,
        lexical: Option<f64>,
        distance: Option<f64>,
    ) -> PoolCandidate {
        PoolCandidate {
            pool: pool.into(),
            ordinal,
            chunk_id: chunk.into(),
            lexical: lexical.map(|v| Measure {
                domain: LEX.into(),
                value: v,
            }),
            // Canonical direction: the adapter negates a distance.
            vector: distance.map(|d| Measure {
                domain: VEC.into(),
                value: -d,
            }),
        }
    }

    /// The 2026-08-24 due-diligence starvation regression, restated over the
    /// neutral shape: a relevant pool's rank-2 (strong raw measurements) must
    /// outrank an irrelevant pool's rank-1 (weak raw measurements). Under a
    /// per-pool-rank merge every rank-1 ties at 1/(k+1) and top_k slots go one
    /// per pool.
    #[test]
    fn pooled_merge_fuses_globally_not_by_per_pool_rank() {
        let candidates = vec![
            cand("commercial", 0, "com-a", Some(0.9), Some(0.20)),
            cand("commercial", 1, "com-b", Some(0.7), Some(0.25)),
            cand("tax", 2, "tax-a", None, Some(0.90)),
            cand("insurance", 3, "ins-a", None, Some(0.95)),
        ];
        let out = fuse_pools(&candidates, 3, &PoolMergeWeights::default());
        let order: Vec<usize> = out.ranked.iter().map(|&(o, _)| o).collect();
        assert_eq!(
            &order[..2],
            &[0, 1],
            "both relevant docs beat every noise rank-1"
        );
        assert!(out.ranked.windows(2).all(|w| w[0].1 >= w[1].1));
        assert!(
            out.ranked[0].1 > out.ranked[2].1,
            "the old bug's signature — identical rank-1 scores — must be gone"
        );
        assert!(!out.diagnostics.mixed_domain);
    }

    /// A NaN measurement (pgvector's `<=>` against a zero-norm vector, negated
    /// by the adapter) must neither panic the sort nor scramble it: the
    /// comparator is a total order, and NaN sorts last within its leg. Many
    /// candidates, because `sort_by` only detects a broken order on inputs
    /// large enough to exercise its merge paths.
    #[test]
    fn a_nan_measurement_sorts_last_and_never_panics() {
        let mut candidates: Vec<PoolCandidate> = (0..64)
            .map(|i| {
                cand(
                    "p",
                    i,
                    &format!("c{i:02}"),
                    Some(1.0 - i as f64 / 100.0),
                    Some(0.1 + i as f64 / 100.0),
                )
            })
            .collect();
        // NaN in the vector leg of a candidate with the STRONGEST lexical
        // measurement, plus one NaN in the lexical leg.
        candidates[0].vector = Some(Measure {
            domain: VEC.into(),
            value: f64::NAN,
        });
        candidates[7].lexical = Some(Measure {
            domain: LEX.into(),
            value: f64::NAN,
        });
        let out = fuse_pools(&candidates, 64, &PoolMergeWeights::default());
        assert_eq!(out.ranked.len(), 64);
        assert!(out.ranked.iter().all(|(_, s)| s.is_finite()));
        assert!(out.ranked.windows(2).all(|w| w[0].1 >= w[1].1));
        // Deterministic: the same input twice is the same order.
        let again = fuse_pools(&candidates, 64, &PoolMergeWeights::default());
        assert_eq!(out.ranked, again.ranked);
        // The lexical-NaN candidate lost its lexical leg's contribution — it
        // ranked last on that leg — while its vector leg still counts.
        let c7 = out.ranked.iter().find(|(o, _)| *o == 7).unwrap().1;
        let c8 = out.ranked.iter().find(|(o, _)| *o == 8).unwrap().1;
        assert!(c7 < c8, "NaN lexical ranks below a finite neighbour");
    }

    /// A candidate with no measurements at all (a legacy producer) sorts last,
    /// deterministically, at score zero — returned, never dropped.
    #[test]
    fn measurement_free_candidates_sort_last() {
        let candidates = vec![
            cand("legacy", 0, "old-a", None, None),
            cand("evidence", 1, "ev-a", Some(0.5), None),
        ];
        let out = fuse_pools(&candidates, 10, &PoolMergeWeights::default());
        assert_eq!(out.ranked.len(), 2);
        assert_eq!(out.ranked[0].0, 1);
        assert_eq!(out.ranked[1], (0, 0.0));
    }

    /// The probe stratum ranks apart from the deep stratum, and probe_weight
    /// scales only the probe side — the measured 2026-08-25 design.
    #[test]
    fn probe_pools_rank_in_their_own_stratum() {
        let candidates = vec![
            cand("deep-letterbook", 0, "deep-letterbook-0", Some(0.030), None),
            cand("deep-letterbook", 1, "deep-letterbook-1", Some(0.028), None),
            cand("probe-narrative", 2, "probe-narrative-0", Some(0.20), None),
            cand("probe-narrative", 3, "probe-narrative-1", Some(0.19), None),
        ];
        // Raw comparison, no strata: the probe pool's original-query scores
        // dominate wholesale.
        let raw = fuse_pools(&candidates, 4, &PoolMergeWeights::default());
        assert_eq!(
            raw.ranked.iter().map(|&(o, _)| o).collect::<Vec<_>>(),
            vec![2, 3, 0, 1]
        );
        // Strata plus the evidence leg arbitrating: the letterbook leads.
        let weights = PoolMergeWeights {
            pool_evidence: 1.0,
            pool_rank: [
                ("deep-letterbook".to_string(), 1),
                ("probe-narrative".to_string(), 20),
            ]
            .into_iter()
            .collect(),
            probe_pools: ["probe-narrative".to_string()].into_iter().collect(),
            ..Default::default()
        };
        let strata = fuse_pools(&candidates, 4, &weights);
        assert_eq!(
            strata.ranked.iter().map(|&(o, _)| o).collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        // probe_weight scales only the probe stratum's contribution.
        let half = PoolMergeWeights {
            probe_weight: 0.5,
            ..weights
        };
        let scaled = fuse_pools(&candidates, 4, &half);
        let narrative_top = scaled
            .ranked
            .iter()
            .find(|&&(o, _)| o == 2)
            .map(|&(_, s)| s)
            .unwrap();
        let expected = 0.5 / 61.0 + 1.0 / 80.0;
        assert!((narrative_top - expected).abs() < 1e-12);
    }

    /// top_k zero keeps the historical default of ten rather than returning
    /// nothing — the coordinator's callers rely on it.
    #[test]
    fn top_k_zero_is_the_historical_default_of_ten() {
        let candidates: Vec<PoolCandidate> = (0..15)
            .map(|i| {
                cand(
                    "p",
                    i,
                    &format!("c{i:02}"),
                    Some(1.0 - i as f64 * 0.01),
                    None,
                )
            })
            .collect();
        assert_eq!(
            fuse_pools(&candidates, 0, &PoolMergeWeights::default())
                .ranked
                .len(),
            10
        );
    }

    /// Two domains in one leg are never numerically compared. Each domain's
    /// internal order is preserved, the interleave goes by per-domain rank,
    /// and the diagnostics say it happened.
    #[test]
    fn mixed_domains_interleave_by_rank_and_are_flagged() {
        let mut candidates = vec![
            // Postgres pool: raw ts_rank magnitudes ~0.2.
            cand("pg-pool", 0, "pg-a", Some(0.20), None),
            cand("pg-pool", 1, "pg-b", Some(0.15), None),
        ];
        // Tantivy pool: BM25 magnitudes ~8 — wildly larger, meaninglessly so.
        for (ordinal, (chunk, score)) in [("tv-a", 8.0f64), ("tv-b", 6.0)].iter().enumerate() {
            candidates.push(PoolCandidate {
                pool: "tv-pool".into(),
                ordinal: ordinal + 2,
                chunk_id: (*chunk).into(),
                lexical: Some(Measure {
                    domain: "tantivy/bm25/munarium-en@1".into(),
                    value: *score,
                }),
                vector: None,
            });
        }
        let out = fuse_pools(&candidates, 4, &PoolMergeWeights::default());
        assert!(out.diagnostics.mixed_domain);
        assert_eq!(out.diagnostics.lexical_domains.len(), 2);

        let order: Vec<usize> = out.ranked.iter().map(|&(o, _)| o).collect();
        // Raw comparison would put both Tantivy hits first (8.0 and 6.0 beat
        // 0.20). Rank interleave alternates the domains' rank-1s, then their
        // rank-2s — magnitudes never decided anything.
        assert_eq!(order, vec![0, 2, 1, 3]);
        // And within each domain the internal order held.
        let pg_first = order.iter().position(|&o| o == 0).unwrap();
        let pg_second = order.iter().position(|&o| o == 1).unwrap();
        assert!(pg_first < pg_second);
    }

    /// A single-domain merge reports its domains and does not flag mixing —
    /// what every postgres-mode request looks like.
    #[test]
    fn a_single_domain_merge_is_not_flagged() {
        let candidates = vec![
            cand("a", 0, "a-0", Some(0.5), Some(0.3)),
            cand("b", 1, "b-0", Some(0.4), Some(0.2)),
        ];
        let out = fuse_pools(&candidates, 5, &PoolMergeWeights::default());
        assert!(!out.diagnostics.mixed_domain);
        assert_eq!(out.diagnostics.lexical_domains, vec![LEX.to_string()]);
        assert_eq!(out.diagnostics.vector_domains, vec![VEC.to_string()]);
        assert_eq!(out.diagnostics.policy_version, POOL_MERGE_POLICY_VERSION);
    }

    /// The vector-domain consequence spelled out: two engines serving ONE
    /// embedder's vectors share a domain, so a mixed-engine search whose
    /// vector legs agree on the embedder splits only the lexical leg.
    #[test]
    fn a_shared_embedder_keeps_the_vector_leg_single_domain() {
        let candidates = vec![
            cand("pg-pool", 0, "pg-a", None, Some(0.2)),
            PoolCandidate {
                pool: "tv-pool".into(),
                ordinal: 1,
                chunk_id: "tv-a".into(),
                lexical: None,
                vector: Some(Measure {
                    domain: VEC.into(),
                    value: -0.1,
                }),
            },
        ];
        let out = fuse_pools(&candidates, 5, &PoolMergeWeights::default());
        assert!(!out.diagnostics.mixed_domain);
        // And the raw distances DID decide: the closer vector leads.
        assert_eq!(out.ranked[0].0, 1);
    }
}
