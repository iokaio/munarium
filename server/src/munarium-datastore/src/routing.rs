// SPDX-License-Identifier: Apache-2.0
//! `RoutingEvidence` — bounded, engine-neutral signals that one pool is
//! ABOUT a query (§6.1).
//!
//! The coordinator must never use raw per-shard relevance as a
//! cross-collection routing score: `ts_rank` and BM25 magnitudes are not
//! comparable, so the moment probes come from two engines, a raw-density
//! ranking silently routes by engine rather than by evidence. Every signal
//! here is therefore **bounded to [0, 1] and scale-free** — multiply one
//! engine's scores by a thousand and the evidence does not move, which is the
//! property the tests pin.
//!
//! ## The signals, and why each one exists
//!
//! - **`term_coverage`** — how much of the query's content vocabulary the
//!   pool's candidates hold at all. The floor signal: a pool covering two of
//!   six terms cannot be about the question.
//! - **`multi_term_fraction`** — the fraction of candidates matching at least
//!   TWO distinct content terms. This is the bounded stand-in for the raw
//!   density the PostgreSQL ranking sums: on the measured Tea Party shape
//!   (2026-08-25) the newspaper shard's pool matches "boston"+"tea" in every
//!   candidate (1.0) while the narrative pool's one relevant reprint gives it
//!   0.05, and coverage alone — which ties them — cannot see the difference.
//! - **`phrase_fraction`** — the share of candidates carrying one of the
//!   query's own adjacent content-word pairs verbatim. The measured signal
//!   that separated the George Washington letterbooks (0.73–0.87) from the
//!   travel narratives (0.11) when term density preferred the narratives.
//! - **`top_margin`** — `(top − third) / top` over the pool's own leg values,
//!   clamped. A ratio of one engine's own numbers, so it survives rescaling;
//!   used only to break ties, because a margin says "this pool has a clear
//!   winner", not "this pool is about the query".
//! - **`hit_count`** — how full the bounded probe pool came back. Final tie
//!   break before the pool name.
//!
//! ## What this is NOT, yet
//!
//! `postgres` mode keeps the existing density ranking
//! (`select_collection_indices`) untouched — its decisions are exercised by
//! the retrieval suites. RoutingEvidence becomes the routing input when
//! datastore-served probes join a fan-out, and only after the routing corpus
//! passes its own parity gate; the composite weights in [`routing_score`] are
//! the starting point that gate calibrates, not a measured endpoint. The
//! policy version exists so a recorded decision stays interpretable across
//! that calibration.
//!
//! [`routing_score`]: RoutingEvidence::routing_score

use crate::stopwords::is_stop_term;

/// Bump when a signal's definition or the composite changes.
pub const ROUTING_POLICY_VERSION: u32 = 1;

/// Bounded evidence for one pool. Every field is in `[0, 1]` except
/// `hit_count`, which is bounded by the caller's own probe size.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutingEvidence {
    pub term_coverage: f64,
    pub multi_term_fraction: f64,
    pub phrase_fraction: f64,
    pub top_margin: f64,
    pub hit_count: u32,
}

impl RoutingEvidence {
    /// The composite routing score:
    /// `(coverage + multi-term)/2 × (1 + phrase_boost × phrase_fraction)`.
    ///
    /// The phrase multiplier is the same construction the live density blend
    /// uses, for the same measured reason: strong phrase evidence must be able
    /// to overrule density-shaped signals, weak phrase evidence must barely
    /// move them.
    pub fn routing_score(&self, phrase_boost: f64) -> f64 {
        let boost = if phrase_boost.is_finite() {
            phrase_boost.max(0.0)
        } else {
            0.0
        };
        (self.term_coverage + self.multi_term_fraction) / 2.0 * (1.0 + boost * self.phrase_fraction)
    }
}

/// What one probe pool offers the computation: its candidates' texts and the
/// pool's own best/third leg values (any one engine's scale — only their ratio
/// is read).
#[derive(Debug, Clone)]
pub struct PoolSignals<'a> {
    pub pool: &'a str,
    pub texts: Vec<&'a str>,
    pub top_value: Option<f64>,
    pub third_value: Option<f64>,
}

/// The query's content terms: folded word tokens minus stop words, deduplicated
/// in order.
pub fn content_terms(query: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    word_tokens(query)
        .into_iter()
        .filter(|t| !is_stop_term(t))
        .filter(|t| seen.insert(t.clone()))
        .collect()
}

/// The query's adjacent content-word pairs, in order, deduplicated — the same
/// definition the live selection uses: a pair separated by a stop word is not
/// a phrase.
pub fn query_phrases(query: &str) -> Vec<(String, String)> {
    let tokens = word_tokens(query);
    let mut phrases = Vec::new();
    for pair in tokens.windows(2) {
        if is_stop_term(&pair[0]) || is_stop_term(&pair[1]) {
            continue;
        }
        let phrase = (pair[0].clone(), pair[1].clone());
        if !phrases.contains(&phrase) {
            phrases.push(phrase);
        }
    }
    phrases
}

/// Compute one pool's evidence.
pub fn evidence(
    terms: &[String],
    phrases: &[(String, String)],
    signals: &PoolSignals<'_>,
) -> RoutingEvidence {
    let candidate_tokens: Vec<Vec<String>> = signals.texts.iter().map(|t| word_tokens(t)).collect();

    let mut covered = 0usize;
    for term in terms {
        if candidate_tokens.iter().any(|toks| toks.contains(term)) {
            covered += 1;
        }
    }
    let term_coverage = if terms.is_empty() {
        0.0
    } else {
        covered as f64 / terms.len() as f64
    };

    let multi = candidate_tokens
        .iter()
        .filter(|toks| {
            let mut distinct = 0usize;
            for term in terms {
                if toks.contains(term) {
                    distinct += 1;
                    if distinct >= 2 {
                        return true;
                    }
                }
            }
            false
        })
        .count();
    let multi_term_fraction = if candidate_tokens.is_empty() {
        0.0
    } else {
        multi as f64 / candidate_tokens.len() as f64
    };

    let with_phrase = candidate_tokens
        .iter()
        .filter(|toks| {
            toks.windows(2)
                .any(|pair| phrases.iter().any(|(a, b)| pair[0] == *a && pair[1] == *b))
        })
        .count();
    let phrase_fraction = if candidate_tokens.is_empty() {
        0.0
    } else {
        with_phrase as f64 / candidate_tokens.len() as f64
    };

    let top_margin = match (signals.top_value, signals.third_value) {
        (Some(top), Some(third)) if top.is_finite() && third.is_finite() && top > 0.0 => {
            ((top - third) / top).clamp(0.0, 1.0)
        }
        _ => 0.0,
    };

    RoutingEvidence {
        term_coverage,
        multi_term_fraction,
        phrase_fraction,
        top_margin,
        hit_count: signals.texts.len() as u32,
    }
}

/// Rank pools by evidence, strongest first. Returns indices into the input.
///
/// Deterministic ties: composite score, then top margin, then hit count, then
/// pool name — so a golden test can exist, and a re-run cannot reorder equals.
pub fn rank(pools: &[(&str, RoutingEvidence)], phrase_boost: f64) -> Vec<usize> {
    let mut indices: Vec<usize> = (0..pools.len())
        .filter(|&i| pools[i].1.hit_count > 0)
        .collect();
    indices.sort_by(|&a, &b| {
        let (sa, sb) = (
            pools[a].1.routing_score(phrase_boost),
            pools[b].1.routing_score(phrase_boost),
        );
        sb.partial_cmp(&sa)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                pools[b]
                    .1
                    .top_margin
                    .partial_cmp(&pools[a].1.top_margin)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(pools[b].1.hit_count.cmp(&pools[a].1.hit_count))
            .then(pools[a].0.cmp(pools[b].0))
    });
    indices
}

/// Folded word tokens: alphanumeric runs, lowercased — the routing-signal
/// tokenizer, deliberately simple. It feeds a bounded ABOUTNESS measure, not
/// an index; the index's analyzer lives in `lexical.rs` and answers a harder
/// question.
fn word_tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signals<'a>(
        pool: &'a str,
        texts: &'a [&'a str],
        top: Option<f64>,
        third: Option<f64>,
    ) -> PoolSignals<'a> {
        PoolSignals {
            pool,
            texts: texts.to_vec(),
            top_value: top,
            third_value: third,
        }
    }

    /// Every signal stays in [0, 1] whatever the inputs — including hostile
    /// leg values. Bounded is the whole contract.
    #[test]
    fn signals_are_bounded_whatever_the_inputs() {
        let terms = content_terms("boston tea party");
        let phrases = query_phrases("boston tea party");
        for (top, third) in [
            (Some(f64::NAN), Some(1.0)),
            (Some(f64::INFINITY), Some(0.0)),
            (Some(-5.0), Some(-50.0)),
            (Some(1e300), Some(1e-300)),
            (None, None),
        ] {
            let e = evidence(
                &terms,
                &phrases,
                &signals("p", &["the tea was destroyed in boston"], top, third),
            );
            for (name, v) in [
                ("coverage", e.term_coverage),
                ("multi", e.multi_term_fraction),
                ("phrase", e.phrase_fraction),
                ("margin", e.top_margin),
            ] {
                assert!(
                    (0.0..=1.0).contains(&v),
                    "{name} = {v} for {top:?}/{third:?}"
                );
            }
        }
    }

    /// The property raw density cannot have: rescaling an engine's leg values
    /// by any factor leaves the evidence identical, because only the ratio is
    /// read. This is what makes a Tantivy probe and a PostgreSQL probe
    /// rankable side by side at all.
    #[test]
    fn rescaling_leg_values_does_not_move_the_evidence() {
        let terms = content_terms("continental congress supply");
        let phrases = query_phrases("continental congress supply");
        let texts = ["the continental congress debated supply"];
        let a = evidence(
            &terms,
            &phrases,
            &signals("p", &texts, Some(0.16), Some(0.14)),
        );
        let b = evidence(
            &terms,
            &phrases,
            &signals("p", &texts, Some(160.0), Some(140.0)),
        );
        // Scale-free to floating-point precision: a ratio of rescaled values
        // differs in the last ulp, and demanding bit identity here would fail
        // on arithmetic, not on the property under test.
        assert_eq!(a.term_coverage, b.term_coverage);
        assert_eq!(a.multi_term_fraction, b.multi_term_fraction);
        assert_eq!(a.phrase_fraction, b.phrase_fraction);
        assert!((a.top_margin - b.top_margin).abs() < 1e-9);
    }

    /// The George Washington shape (measured 2026-08-25): a pool that USES the
    /// query's words constantly ties or beats the subject's own pool on
    /// coverage and multi-term density — and the phrase fraction is what
    /// separates being ABOUT the subject from talking near it.
    #[test]
    fn phrase_evidence_separates_about_from_near() {
        let query = "What cities did George Washington visit?";
        let terms = content_terms(query);
        let phrases = query_phrases(query);

        // The narrative: dense in the vocabulary, phrase-free.
        let narrative_texts = ["the cities we visit, as washington described the town"; 8];
        // The letterbook: the same density, and the subject's own name intact.
        let letterbook_texts = ["george washington left the city to visit the north river"; 8];

        let narrative = evidence(
            &terms,
            &phrases,
            &signals("narrative", &narrative_texts, None, None),
        );
        let letterbook = evidence(
            &terms,
            &phrases,
            &signals("letterbook", &letterbook_texts, None, None),
        );
        assert!(narrative.term_coverage >= letterbook.term_coverage - f64::EPSILON);
        assert!(narrative.phrase_fraction < 0.01, "no adjacent pair");
        assert!(letterbook.phrase_fraction > 0.99, "every text carries one");

        let ranked = rank(&[("narrative", narrative), ("letterbook", letterbook)], 3.0);
        assert_eq!(ranked[0], 1, "the letterbook leads at boost 3");
    }

    /// The Tea Party shape (measured 2026-08-25): the phrase is later coinage,
    /// so weak phrase evidence must not override the density-shaped signal —
    /// and `multi_term_fraction` is the bounded signal that carries it, where
    /// coverage alone would tie the pools.
    #[test]
    fn multi_term_density_decides_when_phrases_are_absent() {
        let query = "How did colonial newspapers report the Boston Tea Party?";
        let terms = content_terms(query);
        let phrases = query_phrases(query);

        let mut narrative_texts = vec!["the city and its press, described at length"; 19];
        narrative_texts.push("reprinting what the colonial newspapers said of it");
        let newspaper_texts = ["boston, december 20: the tea was destroyed last thursday"; 20];

        let narrative = evidence(
            &terms,
            &phrases,
            &signals("narrative", &narrative_texts, Some(0.13), Some(0.11)),
        );
        let newspaper = evidence(
            &terms,
            &phrases,
            &signals("newspaper", &newspaper_texts, Some(0.16), Some(0.14)),
        );
        // Coverage ties them (each pool holds two of the six content terms);
        // the multi-term fraction does not.
        assert!((narrative.term_coverage - newspaper.term_coverage).abs() < 1e-9);
        assert!(newspaper.multi_term_fraction > 0.9);
        assert!(narrative.multi_term_fraction < 0.1);

        let pools = [("narrative", narrative), ("newspaper", newspaper)];
        assert_eq!(rank(&pools, 3.0)[0], 1, "the newspaper shard leads");
        assert_eq!(rank(&pools, 0.0)[0], 1, "with the phrase signal off too");
    }

    /// An empty pool is excluded from the ranking rather than ranked last —
    /// the live selection does the same, and a route to an empty pool is a
    /// route to nothing.
    #[test]
    fn empty_pools_are_excluded() {
        let terms = content_terms("anything");
        let e_empty = evidence(&terms, &[], &signals("empty", &[], None, None));
        let e_full = evidence(
            &terms,
            &[],
            &signals("full", &["anything at all"], None, None),
        );
        let pools = [("empty", e_empty), ("full", e_full)];
        assert_eq!(rank(&pools, 3.0), vec![1]);
    }

    /// Ties break on margin, then hit count, then name — deterministically.
    #[test]
    fn ties_break_deterministically() {
        let terms = content_terms("alpha beta");
        let texts = ["alpha beta gamma"];
        let with_margin = evidence(&terms, &[], &signals("m", &texts, Some(1.0), Some(0.5)));
        let without = evidence(&terms, &[], &signals("n", &texts, Some(1.0), Some(1.0)));
        let pools = [("zeta", without.clone()), ("m", with_margin)];
        assert_eq!(rank(&pools, 0.0), vec![1, 0], "margin first");

        let same = [("b", without.clone()), ("a", without)];
        assert_eq!(rank(&same, 0.0), vec![1, 0], "then the pool name");
    }

    /// The phrase definition matches the live one: a pair separated by a stop
    /// word is not a phrase.
    #[test]
    fn phrases_are_adjacent_content_words_only() {
        let phrases = query_phrases("What cities did George Washington visit?");
        assert_eq!(
            phrases,
            vec![
                ("george".to_string(), "washington".to_string()),
                ("washington".to_string(), "visit".to_string()),
            ]
        );
    }
}
