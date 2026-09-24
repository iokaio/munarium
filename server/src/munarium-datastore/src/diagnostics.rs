// SPDX-License-Identifier: Apache-2.0
//! Per-leg observations, without corpus identities or authorization policy.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateDiagnostics {
    pub requested: usize,
    /// Maximum returned pool requested from the engine (including overfetch).
    pub candidate_limit: usize,
    /// Candidate pool returned by the engine, before adapter truncation.
    pub fetched: usize,
    pub accepted: usize,
    /// Adapter rejections only; engine-internal filtering is not observable.
    pub rejected: usize,
    pub rejection_reason: Option<&'static str>,
    /// Distance computations, not unique nodes or returned hits.
    pub distance_computations: Option<usize>,
    /// Unique candidates visited, when the engine exposes it.
    pub visited: Option<usize>,
    pub refill_count: usize,
    /// Hard bound on visited candidates, if known. A result limit is not one.
    pub work_limit: Option<usize>,
    /// ANN search-list size, not a bound on distance computations.
    pub search_list: Option<usize>,
    /// Whether the engine exhausted the eligible set; None means unknown.
    pub exhausted: Option<bool>,
}

impl CandidateDiagnostics {
    pub fn returned(requested: usize, fetched: usize, accepted: usize) -> Self {
        let rejected = fetched.saturating_sub(accepted);
        Self {
            requested,
            candidate_limit: requested,
            fetched,
            accepted,
            rejected,
            rejection_reason: (rejected > 0).then_some("rank_cutoff"),
            distance_computations: None,
            visited: None,
            refill_count: 0,
            work_limit: None,
            search_list: None,
            exhausted: None,
        }
    }
}

#[derive(Debug)]
pub struct CandidateBatch {
    pub candidates: Vec<crate::vector::Candidate>,
    pub diagnostics: CandidateDiagnostics,
}
