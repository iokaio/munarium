// SPDX-License-Identifier: Apache-2.0
//! Datastore serving: the `datastore` mode's dispatch, and the
//! no-fallback contract it keeps.
//!
//! The rules are §9.1's, made code:
//!
//! - The rollout selector decides WHICH scopes the datastore serves. An
//!   unselected scope — no row, or a row saying `postgres` — continues on
//!   PostgreSQL by policy. That is routing, not fallback.
//! - Once a scope IS selected, every failure past that point is an explicit
//!   [`KernelError::DatastoreUnavailable`]: a missing serving binding, an
//!   unverified artifact, a quarantined cache key, an unopenable component.
//!   **Nothing here ever retries the query against PostgreSQL.** A silent
//!   per-request fallback would make the datastore look healthy exactly when
//!   it is broken, and would serve two different engines' answers under one
//!   scope without anyone choosing that. Rollback is an operator's selector
//!   change — observable, audited, deliberate.
//! - The VERSION being served is resolved from PostgreSQL's control plane
//!   (the active pointer or the caller's pin) exactly as the PostgreSQL path
//!   resolves it; the datastore chooses only the PHYSICAL artifact, through
//!   the `serving` binding. An exact-version request either opens that exact
//!   verified artifact or fails; it never substitutes a newer version.
//!
//! Source provenance is enriched from PostgreSQL at answer time: the artifact
//! records carry each chunk's text and build-time path, and PostgreSQL is
//! truth for source content hashes in every mode. Duplicating the hash into
//! the records format would be a second copy free to drift from the first.

use std::sync::Arc;

use munarium_core::retrieval::{PreparedSearchQuery, ProvenanceEnvelope, SearchResult};
use munarium_core::{KernelError, Result};
use munarium_datastore::hydrate::Residency;
use munarium_retrieval_pg::PgRetrieval;
use munarium_store_pg::artifacts::BindingSlot;
use munarium_store_pg::rollout::RolloutSelector;

use crate::executor::{ArtifactExecutor, ExecutionOutcome, TextPayload};

/// The serving-side machinery for one tenant: the selector that routes and
/// the executor that answers.
pub struct ServingPlane {
    pub selector: RolloutSelector,
    pub executor: Arc<ArtifactExecutor>,
}

impl std::fmt::Debug for ServingPlane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServingPlane").finish_non_exhaustive()
    }
}

impl ServingPlane {
    /// Whether the selector routes this scope to the datastore.
    ///
    /// A selector read per search rather than a cached snapshot: immediate
    /// consistency for the operator's rollback lever, at the cost of one
    /// indexed primary-key read. The plan's `ROLLOUT_REFRESH_MS` cache is the
    /// optimization to take when this read shows up in a profile — with the
    /// note that it trades rollback latency for it.
    pub async fn routes_to_datastore(&self, scope_kind: &str, scope_id: &str) -> Result<bool> {
        Ok(self
            .selector
            .get(scope_kind, scope_id)
            .await?
            .map(|entry| entry.serving == "datastore")
            .unwrap_or(false))
    }

    /// Serve one prepared query for a datastore-selected scope.
    ///
    /// `scope` is the selector scope this request was routed under — it names
    /// the failure, so an operator reading a `datastore-unavailable` problem
    /// knows which selector row to roll back.
    pub async fn search(
        &self,
        pg: &PgRetrieval,
        scope: (&str, &str),
        index_version_id: &str,
        watermark: u64,
        prepared: &Arc<PreparedSearchQuery>,
    ) -> Result<SearchResult> {
        let unavailable = |reason: &str| {
            KernelError::DatastoreUnavailable(format!(
                "{} {} (version {index_version_id}): {reason}",
                scope.0, scope.1
            ))
        };

        let execution = match self
            .executor
            .execute(
                index_version_id,
                BindingSlot::Serving,
                Residency::ServingRequired,
                TextPayload::Served,
                prepared,
            )
            .await
        {
            ExecutionOutcome::Executed(e) => e,
            ExecutionOutcome::Refused(reason) => return Err(unavailable(&reason)),
            ExecutionOutcome::Failed(reason) => return Err(unavailable(&reason)),
        };

        // Provenance enrichment: current source content hashes from the
        // control plane. A source the catalog no longer holds keeps its
        // record-side path and an empty hash — the id still identifies it,
        // which is the same degradation the PostgreSQL path chose.
        let mut hits = execution.hits;
        let mut source_ids: Vec<String> = hits.iter().map(|h| h.source_id.clone()).collect();
        source_ids.sort();
        source_ids.dedup();
        let provenance = pg.source_provenance(&source_ids).await?;
        for hit in hits.iter_mut() {
            match provenance.get(&hit.source_id) {
                Some((_path, hash)) => hit.source_content_hash = hash.clone(),
                None => hit.source_content_hash = String::new(),
            }
        }

        let mut hashes: Vec<String> = hits
            .iter()
            .map(|h| h.source_content_hash.clone())
            .filter(|h| !h.is_empty())
            .collect();
        hashes.sort();
        hashes.dedup();
        let mut source_paths: Vec<String> = hits.iter().map(|h| h.source_path.clone()).collect();
        source_paths.sort();
        source_paths.dedup();

        Ok(SearchResult {
            envelope: ProvenanceEnvelope {
                chunk_ids: hits.iter().map(|h| h.chunk_id.clone()).collect(),
                source_ids,
                source_paths,
                source_content_hashes: hashes,
                index_version: index_version_id.to_string(),
                event_watermark: watermark,
                // The embedder that produced the artifact's vectors — the
                // same one the prepared query's vector came from.
                provider_fingerprint: Some(format!(
                    "local/{}/{}",
                    munarium_retrieval_pg::LOCAL_EMBEDDER,
                    munarium_retrieval_pg::EMBED_DIMS
                )),
            },
            hits,
        })
    }
}
