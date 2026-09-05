// SPDX-License-Identifier: Apache-2.0
//! The shadow candidate: the executor pointed at the `shadow` slot, and the
//! §13.2 comparison of its answer against the reference engine's.
//!
//! The mechanics — resolve, hydrate, open, run both legs — live in
//! [`crate::executor`], shared with the stage 6 serving path; what is
//! shadow-specific is the slot (`shadow`), the residency (`Opportunistic`, a
//! cache guest that can never displace the serving-required set), and the
//! comparison itself.

use std::sync::Arc;

use munarium_core::retrieval::{PreparedSearchQuery, SearchHit, SearchResult};
use munarium_datastore::hydrate::Residency;
use munarium_store_pg::artifacts::BindingSlot;

use crate::executor::{ArtifactExecution, ArtifactExecutor, ExecutionOutcome, TextPayload};
use crate::shadow::{compare, ComparisonInput, PhaseLatency, ShadowComparison};

/// Run the shadow candidate for one version: the executor, at the shadow
/// slot's posture.
pub async fn execute_candidate(
    executor: &ArtifactExecutor,
    index_version_id: &str,
    prepared: &Arc<PreparedSearchQuery>,
) -> ExecutionOutcome {
    executor
        .execute(
            index_version_id,
            BindingSlot::Shadow,
            Residency::Opportunistic,
            TextPayload::Identity,
            prepared,
        )
        .await
}

/// Assemble the §13.2 comparison from the two sides.
///
/// Each side's leg lists are its FUSED hits that carry that leg's rank, in
/// rank order — derived the same way from both, which is what makes the leg
/// comparison symmetric. The reference's `source_content_hash` is replaced by
/// the sha256 of its chunk TEXT so the corruption check compares like with
/// like (see [`ArtifactExecution::hits`]).
pub fn comparison(
    query: &str,
    reference_version: &str,
    reference: &SearchResult,
    candidate: &ArtifactExecution,
    reference_latency: PhaseLatency,
    relevant_chunks: Option<&std::collections::HashSet<String>>,
) -> ShadowComparison {
    use sha2::{Digest, Sha256};

    let leg = |hits: &[SearchHit], rank_of: fn(&SearchHit) -> Option<u32>| -> Vec<String> {
        let mut with_rank: Vec<(u32, String)> = hits
            .iter()
            .filter_map(|h| rank_of(h).map(|r| (r, h.chunk_id.clone())))
            .collect();
        with_rank.sort();
        with_rank.into_iter().map(|(_, id)| id).collect()
    };

    let normalized_reference: Vec<SearchHit> = reference
        .hits
        .iter()
        .map(|h| {
            let mut h = h.clone();
            h.source_content_hash = hex::encode(Sha256::digest(h.text.as_bytes()));
            h.text = String::new();
            h
        })
        .collect();

    compare(ComparisonInput {
        query,
        reference_version,
        candidate_artifact_id: &candidate.artifact_id,
        candidate_engine: &candidate.engine,
        reference_lexical: &leg(&normalized_reference, |h| h.lexical_rank),
        candidate_lexical: &leg(&candidate.hits, |h| h.lexical_rank),
        reference_vector: &leg(&normalized_reference, |h| h.vector_rank),
        candidate_vector: &leg(&candidate.hits, |h| h.vector_rank),
        reference_hits: &normalized_reference,
        candidate_hits: &candidate.hits,
        relevant_chunks,
        reference_latency,
        candidate_latency: candidate.latency.clone(),
    })
}
