// SPDX-License-Identifier: Apache-2.0
//! Transport-neutral inference stages (ClearView IR-0).
//!
//! An enrolled collection can send two kinds of work to an inference
//! endpoint: query and passage **embeddings** for a semantic vector leg, and
//! **reranking** of fused candidates. This module is the vocabulary both the
//! retrieval coordinator and the provider gateway speak. It holds no HTTP, no
//! SQL and no credentials: the gateway implements [`InferenceEndpoint`], the
//! coordinator calls it, and neither knows how the other works.
//!
//! The reply rules are enforced here, once, so that every endpoint (Munarium
//! Lens or any server passing the generic probe) is held to the same contract:
//!
//! - an embedding reply must be a bijection onto the inputs, at exactly the
//!   declared dimension, with finite values ([`validate_embeddings`]);
//! - a rerank reply must be a full permutation of the documents sent, with
//!   finite scores. Server uses the ORDER only: ties keep the original
//!   position, and a score is never blended with a fusion score
//!   ([`validate_rerank`], [`permute_prefix`]).
//!
//! Failures are typed and content-free. A [`InferenceFailure`] carries a
//! [`FailureKind`] and a short fixed reason code, never request text, passage
//! text, endpoint addresses or response bodies.

use crate::KernelError;
use std::time::Instant;

/// The two inference stages. The wire names match the Lens problem bodies
/// (`"stage": "embedding" | "reranking"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    Embedding,
    Reranking,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Embedding => "embedding",
            Stage::Reranking => "reranking",
        }
    }
}

/// What an enrolled read does when a configured stage cannot run (I1).
///
/// `Require` is the default: an omitted policy never permits a fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StagePolicy {
    /// The stage's failure is a typed refusal of the governed read.
    #[default]
    Require,
    /// Explicit operator selection: the stage's documented, disclosed fallback
    /// (lexical-only search for the embedder, the original fused order for
    /// the reranker).
    Prefer,
}

impl StagePolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            StagePolicy::Require => "require",
            StagePolicy::Prefer => "prefer",
        }
    }

    /// Parse an explicit value. `None` (omitted) is `Require`; anything other
    /// than the two names is an error, never a silent default.
    pub fn parse(raw: Option<&str>) -> Result<Self, KernelError> {
        match raw {
            None | Some("require") => Ok(StagePolicy::Require),
            Some("prefer") => Ok(StagePolicy::Prefer),
            Some(_) => Err(KernelError::InvalidInput(
                "stage policy must be \"require\" or \"prefer\"".into(),
            )),
        }
    }
}

/// Why endpoint work happened. Each purpose is metered separately (I2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Purpose {
    /// Work whose result a caller receives.
    Serving,
    /// Explicitly authorized shadow comparison. Never affects a response.
    Shadow,
    /// Index build work (passage embeddings), usually queued.
    Build,
    /// An operator's conformance probe.
    Probe,
}

impl Purpose {
    pub fn as_str(self) -> &'static str {
        match self {
            Purpose::Serving => "serving",
            Purpose::Shadow => "shadow",
            Purpose::Build => "build",
            Purpose::Probe => "probe",
        }
    }
}

/// The diagnostic class of a stage that could not run (design 8.9).
///
/// Together with "ready" these are the states an operator report and a
/// refusal distinguish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// The deployment has inference switched off (`MUNARIUM_INFERENCE`).
    Disabled,
    /// The binding names no usable provider configuration, or a stage
    /// identity that does not match the index being read.
    Unconfigured,
    /// No current passing conformance probe for the bound configuration
    /// (never probed, failed, or expired by configuration drift).
    ProbeFailed,
    /// Transport failure, an endpoint refusal, or an overloaded endpoint.
    Unavailable,
    /// The reply broke the response contract.
    InvalidResponse,
    /// The request deadline expired before the stage completed.
    DeadlineExceeded,
    /// The responsible tenant budget could not admit the work.
    BudgetExhausted,
    /// The input exceeded a Server bound or the model's limit. A refusal for
    /// this request; Server never truncates.
    InputTooLarge,
}

impl FailureKind {
    /// The stage state an operator report and a refusal disclose.
    pub fn state(self) -> &'static str {
        match self {
            FailureKind::Disabled => "disabled",
            FailureKind::Unconfigured => "unconfigured",
            FailureKind::ProbeFailed => "probe_failed",
            FailureKind::Unavailable
            | FailureKind::InvalidResponse
            | FailureKind::DeadlineExceeded
            | FailureKind::InputTooLarge => "unavailable",
            FailureKind::BudgetExhausted => "budget_exhausted",
        }
    }
}

/// A typed, content-free stage failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InferenceFailure {
    pub kind: FailureKind,
    /// A fixed, lowercase reason code (`transport`, `status_503`,
    /// `duplicate_index`, ...). Never derived from request or response text.
    pub reason: &'static str,
}

impl InferenceFailure {
    pub const fn new(kind: FailureKind, reason: &'static str) -> Self {
        Self { kind, reason }
    }

    pub const fn invalid(reason: &'static str) -> Self {
        Self::new(FailureKind::InvalidResponse, reason)
    }

    /// Whether a fresh attempt within the same deadline could succeed: a
    /// transport failure or an endpoint that said it is busy or not ready.
    /// A refusal of the request itself is never retried.
    pub fn retryable(&self) -> bool {
        self.kind == FailureKind::Unavailable
            && matches!(
                self.reason,
                "transport"
                    | "status_429"
                    | "status_502"
                    | "status_503"
                    | "overloaded"
                    | "not_ready"
            )
    }
}

impl std::fmt::Display for InferenceFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.kind.state(), self.reason)
    }
}

/// Usage the endpoint reported, when it reported any. `None` is unknown and
/// stays unknown in accounting; it is never read as zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Usage {
    pub input_tokens: Option<u64>,
}

/// One embedding call: an ordered batch, the exact served model and the
/// dimension the binding declared.
#[derive(Debug, Clone, Copy)]
pub struct EmbedCall<'a> {
    pub model: &'a str,
    pub inputs: &'a [String],
    pub dimensions: usize,
    pub deadline: Instant,
}

/// A validated embedding reply: one vector per input, in input order.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbedReply {
    pub vectors: Vec<Vec<f32>>,
    pub usage: Usage,
}

/// One rerank call: the query and the ordered candidate passages.
#[derive(Debug, Clone, Copy)]
pub struct RerankCall<'a> {
    pub model: &'a str,
    pub query: &'a str,
    pub documents: &'a [String],
    pub deadline: Instant,
}

/// A validated rerank reply.
#[derive(Debug, Clone, PartialEq)]
pub struct RerankReply {
    /// Original positions, best first. A full permutation of `0..n`.
    pub order: Vec<usize>,
    pub usage: Usage,
}

/// An inference endpoint, as the coordinator sees it.
///
/// Implementations own transport, credentials, admission and metering. The
/// replies they return must already have passed [`validate_embeddings`] or
/// [`validate_rerank`]; the coordinator trusts nothing else about them.
#[async_trait::async_trait]
pub trait InferenceEndpoint: Send + Sync {
    async fn embed(&self, call: EmbedCall<'_>) -> Result<EmbedReply, InferenceFailure>;
    async fn rerank(&self, call: RerankCall<'_>) -> Result<RerankReply, InferenceFailure>;
}

/// Validate raw embedding rows `(index, values)` against the request.
///
/// The reply must account for every input exactly once, at the declared
/// dimension, with values that are finite as `f32`. Rows may arrive in any
/// order; the result is restored to input order from the validated indices.
pub fn validate_embeddings(
    rows: Vec<(i64, Vec<f64>)>,
    expected: usize,
    dimensions: usize,
) -> Result<Vec<Vec<f32>>, InferenceFailure> {
    if rows.len() != expected {
        return Err(InferenceFailure::invalid("count_mismatch"));
    }
    let mut slots: Vec<Option<Vec<f32>>> = vec![None; expected];
    for (index, values) in rows {
        let slot = usize::try_from(index)
            .ok()
            .and_then(|i| slots.get_mut(i))
            .ok_or(InferenceFailure::invalid("index_out_of_range"))?;
        if slot.is_some() {
            return Err(InferenceFailure::invalid("duplicate_index"));
        }
        if values.len() != dimensions {
            return Err(InferenceFailure::invalid("dimension_mismatch"));
        }
        let mut vector = Vec::with_capacity(dimensions);
        for v in values {
            let narrowed = v as f32;
            if !v.is_finite() || !narrowed.is_finite() {
                return Err(InferenceFailure::invalid("nonfinite"));
            }
            vector.push(narrowed);
        }
        *slot = Some(vector);
    }
    // Every slot is filled: `expected` distinct in-range indices were seen.
    slots
        .into_iter()
        .map(|v| v.ok_or(InferenceFailure::invalid("missing_index")))
        .collect()
}

/// Validate raw rerank results `(index, relevance_score)` and derive the order.
///
/// The results must be a full permutation of `0..expected` with finite
/// scores. The returned order sorts by score, highest first, and keeps the
/// original position on ties, so the result is deterministic for a given set
/// of scores whatever order the endpoint listed them in.
pub fn validate_rerank(
    results: Vec<(i64, f64)>,
    expected: usize,
) -> Result<Vec<usize>, InferenceFailure> {
    if results.len() != expected {
        return Err(InferenceFailure::invalid("count_mismatch"));
    }
    let mut scores: Vec<Option<f64>> = vec![None; expected];
    for (index, score) in results {
        let slot = usize::try_from(index)
            .ok()
            .and_then(|i| scores.get_mut(i))
            .ok_or(InferenceFailure::invalid("index_out_of_range"))?;
        if slot.is_some() {
            return Err(InferenceFailure::invalid("duplicate_index"));
        }
        if !score.is_finite() {
            return Err(InferenceFailure::invalid("nonfinite"));
        }
        *slot = Some(score);
    }
    let mut scored = Vec::with_capacity(expected);
    for (i, s) in scores.into_iter().enumerate() {
        scored.push((i, s.ok_or(InferenceFailure::invalid("missing_index"))?));
    }
    // Stable sort, descending score: equal scores keep ascending position.
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    Ok(scored.into_iter().map(|(i, _)| i).collect())
}

/// Apply a rerank order to the scored prefix of `items`.
///
/// Permutation only: `order` reorders `items[..order.len()]`, and the
/// unscored tail follows in its original order. Nothing is added, dropped or
/// rescored. `None` when `order` is not a permutation of the prefix (the
/// caller treats that as a failed stage, never as a partial reorder).
pub fn permute_prefix<T>(items: Vec<T>, order: &[usize]) -> Option<Vec<T>> {
    let n = order.len();
    if n > items.len() {
        return None;
    }
    let mut seen = vec![false; n];
    for &i in order {
        if i >= n || std::mem::replace(&mut seen[i], true) {
            return None;
        }
    }
    let mut slots: Vec<Option<T>> = items.into_iter().map(Some).collect();
    let mut out = Vec::with_capacity(slots.len());
    for &i in order {
        out.push(slots[i].take()?);
    }
    out.extend(slots.into_iter().skip(n).flatten());
    Some(out)
}

/// Blend two embeddings by weight and L2-normalize: the semantic-space
/// counterpart of the hash embedder's blend. `weight` 0 is the original
/// query, 1 the expanded one.
pub fn blend_normalized(original: &[f32], expanded: &[f32], weight: f32) -> Vec<f32> {
    let w = weight.clamp(0.0, 1.0);
    let mut out: Vec<f32> = original
        .iter()
        .zip(expanded)
        .map(|(a, b)| (1.0 - w) * a + w * b)
        .collect();
    normalize(&mut out);
    out
}

/// L2-normalize in place. A zero vector stays zero.
pub fn normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 && norm.is_finite() {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// Sentinel prefix promoting a [`KernelError::Provider`] into the
/// `inference-stage-unavailable` problem type at the transport boundary, the
/// same promote-at-the-boundary pattern the daily cap uses.
pub const REFUSAL_PREFIX: &str = "inference-stage-unavailable: ";

/// The typed refusal of a governed read whose required stage could not run.
/// Content-free: the stage, its state and reason code, and the policy
/// revision that required it.
pub fn refusal(stage: Stage, failure: InferenceFailure, policy_revision: u64) -> KernelError {
    KernelError::Provider(format!(
        "{REFUSAL_PREFIX}stage={} state={} reason={} policy=require policy_revision={policy_revision}",
        stage.as_str(),
        failure.kind.state(),
        failure.reason,
    ))
}

/// The detail of a refusal built by [`refusal`], if `e` is one.
pub fn refusal_detail(e: &KernelError) -> Option<&str> {
    match e {
        KernelError::Provider(msg) => msg.strip_prefix(REFUSAL_PREFIX),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(dims: usize, order: &[i64]) -> Vec<(i64, Vec<f64>)> {
        order.iter().map(|&i| (i, vec![i as f64; dims])).collect()
    }

    #[test]
    fn embeddings_are_restored_to_input_order() {
        let out = validate_embeddings(rows(3, &[2, 0, 1]), 3, 3).unwrap();
        assert_eq!(out[0], vec![0.0; 3]);
        assert_eq!(out[2], vec![2.0; 3]);
    }

    #[test]
    fn embedding_contract_violations_are_typed() {
        let bad = |rows, n, d| validate_embeddings(rows, n, d).unwrap_err().reason;
        assert_eq!(bad(rows(2, &[0]), 2, 2), "count_mismatch");
        assert_eq!(bad(rows(2, &[0, 0]), 2, 2), "duplicate_index");
        assert_eq!(bad(rows(2, &[0, 2]), 2, 2), "index_out_of_range");
        assert_eq!(bad(rows(2, &[0, -1]), 2, 2), "index_out_of_range");
        assert_eq!(bad(rows(3, &[0, 1]), 2, 2), "dimension_mismatch");
        assert_eq!(bad(vec![(0, vec![f64::NAN])], 1, 1), "nonfinite");
        // Finite as f64, infinite as f32: refused, never stored as inf.
        assert_eq!(bad(vec![(0, vec![1e300])], 1, 1), "nonfinite");
        assert_eq!(
            validate_embeddings(rows(2, &[0]), 2, 2).unwrap_err().kind,
            FailureKind::InvalidResponse
        );
    }

    #[test]
    fn rerank_order_is_by_score_with_stable_ties() {
        // Listed in a scrambled order; 1 and 3 tie and keep position order.
        let order = validate_rerank(vec![(3, 0.5), (0, 0.1), (2, 0.9), (1, 0.5)], 4).unwrap();
        assert_eq!(order, vec![2, 1, 3, 0]);
    }

    #[test]
    fn rerank_contract_violations_are_typed() {
        let bad = |r: Vec<(i64, f64)>, n| validate_rerank(r, n).unwrap_err().reason;
        assert_eq!(bad(vec![(0, 1.0)], 2), "count_mismatch");
        assert_eq!(bad(vec![(0, 1.0), (0, 0.5)], 2), "duplicate_index");
        assert_eq!(bad(vec![(0, 1.0), (5, 0.5)], 2), "index_out_of_range");
        assert_eq!(bad(vec![(0, 1.0), (1, f64::INFINITY)], 2), "nonfinite");
    }

    #[test]
    fn permutation_reorders_only_the_prefix() {
        let out = permute_prefix(vec!["a", "b", "c", "d"], &[1, 0]).unwrap();
        assert_eq!(out, vec!["b", "a", "c", "d"]);
        assert!(permute_prefix(vec!["a", "b"], &[0, 0]).is_none());
        assert!(permute_prefix(vec!["a"], &[0, 1]).is_none());
        assert_eq!(permute_prefix(vec![1, 2], &[]).unwrap(), vec![1, 2]);
    }

    #[test]
    fn policy_defaults_to_require_and_rejects_unknown_values() {
        assert_eq!(StagePolicy::parse(None).unwrap(), StagePolicy::Require);
        assert_eq!(StagePolicy::default(), StagePolicy::Require);
        assert_eq!(
            StagePolicy::parse(Some("prefer")).unwrap(),
            StagePolicy::Prefer
        );
        assert!(StagePolicy::parse(Some("Prefer")).is_err());
    }

    #[test]
    fn a_refusal_is_content_free_and_recognized() {
        let e = refusal(
            Stage::Reranking,
            InferenceFailure::new(FailureKind::Unavailable, "status_503"),
            7,
        );
        let detail = refusal_detail(&e).unwrap();
        assert_eq!(
            detail,
            "stage=reranking state=unavailable reason=status_503 policy=require policy_revision=7"
        );
        assert!(refusal_detail(&KernelError::Provider("other".into())).is_none());
    }

    #[test]
    fn blend_is_normalized() {
        let v = blend_normalized(&[1.0, 0.0], &[0.0, 1.0], 0.5);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
    }
}
