// SPDX-License-Identifier: Apache-2.0
//! Backfill: give every serving-required version an artifact.
//!
//! A scope cannot be selected for datastore serving until every version it must
//! be able to answer has a verified artifact bound to it (§9.1). Backfill is
//! what makes that true: given a scope, it enumerates those versions and
//! mirrors each one that lacks an artifact.
//!
//! The set includes retired-but-within-horizon versions, not just the active
//! one. That is the whole point — the failure this prevents is a session that
//! resolved a version mid-conversation, watched it stop being active, and then
//! could not read it back.
//!
//! ## Backfill never fails the caller for one bad version
//!
//! Each version is reported independently. A scope where four versions built
//! and one failed is not a failed backfill, it is an *incomplete* one, and the
//! difference matters: the selector CAS refuses an incomplete set, so the
//! honest report is per-version and the completeness judgement belongs to
//! whoever reads it.

use munarium_core::Result;
use munarium_retrieval_pg::required::{RequiredReason, RequiredVersionsPolicy};
use munarium_retrieval_pg::PgRetrieval;

use crate::mirror::{
    mirror_index, mirror_plan, reconstructed_spec, MirrorContext, MirrorOutcome, MirrorTarget,
};

/// What one version's backfill did.
#[derive(Debug, Clone, PartialEq)]
pub struct VersionOutcome {
    pub index_version_id: String,
    pub reason: RequiredReason,
    /// `Ok` for every terminal mirror outcome, including `AlreadyBuilt` and
    /// `Converged`, which are successes. `Err` carries the failure MESSAGE, not
    /// the error, so a report can be serialized and stored.
    pub result: std::result::Result<MirrorOutcome, String>,
}

impl VersionOutcome {
    /// Whether this version now has a verified artifact.
    ///
    /// `AlreadyRunning` is deliberately NOT complete: another node holds the
    /// lease and may still fail, so counting it would report coverage that does
    /// not exist yet.
    pub fn is_complete(&self) -> bool {
        matches!(
            self.result,
            Ok(MirrorOutcome::Published { .. })
                | Ok(MirrorOutcome::Converged { .. })
                | Ok(MirrorOutcome::AlreadyBuilt { .. })
        )
    }
}

/// The result of backfilling one scope.
#[derive(Debug, Clone, PartialEq)]
pub struct BackfillReport {
    pub scope_id: String,
    pub policy: &'static str,
    pub versions: Vec<VersionOutcome>,
}

impl BackfillReport {
    /// True only when every required version has a verified artifact.
    ///
    /// An empty required set is NOT complete: a collection with no active
    /// version has nothing to serve, and reporting it as ready for datastore
    /// selection would let the CAS pass on a scope that cannot answer a query.
    pub fn is_complete(&self) -> bool {
        !self.versions.is_empty() && self.versions.iter().all(VersionOutcome::is_complete)
    }

    pub fn complete_count(&self) -> usize {
        self.versions.iter().filter(|v| v.is_complete()).count()
    }
}

/// Mirror every serving-required version of one collection that lacks an
/// artifact.
///
/// `pin_horizon_secs` comes from the coordinator's configured pin horizon, not
/// from a local default: it is the same number the retention janitor and the
/// eviction leases use, and two of them disagreeing would make a version
/// evictable while still pinned.
pub async fn backfill_collection(
    ctx: &MirrorContext,
    pg: &PgRetrieval,
    collection_id: &str,
    policy: RequiredVersionsPolicy,
    pin_horizon_secs: i64,
) -> Result<BackfillReport> {
    let required = pg
        .required_versions(collection_id, policy, pin_horizon_secs)
        .await?;

    let mut versions = Vec::with_capacity(required.len());
    for v in required {
        let target = MirrorTarget::Collection { collection_id };
        let result = backfill_one(ctx, pg, target, &v.index_version_id)
            .await
            .map_err(|e| e.to_string());
        versions.push(VersionOutcome {
            index_version_id: v.index_version_id,
            reason: v.reason,
            result,
        });
    }

    Ok(BackfillReport {
        scope_id: collection_id.to_string(),
        policy: policy.as_str(),
        versions,
    })
}

/// Mirror one version, reconstructing its spec and plan from what it recorded.
///
/// Exposed on its own because "rebuild this one version" is a real operator
/// request — an artifact that failed verification, or one built under a plan an
/// engine upgrade has superseded.
pub async fn backfill_one(
    ctx: &MirrorContext,
    pg: &PgRetrieval,
    target: MirrorTarget<'_>,
    index_version_id: &str,
) -> Result<MirrorOutcome> {
    let facts = pg.version_facts(index_version_id).await?;
    let sources = match target {
        MirrorTarget::Collection { collection_id } => {
            pg.exported_sources(collection_id, index_version_id).await?
        }
        MirrorTarget::LegacyShape { .. } => pg.exported_legacy_sources(index_version_id).await?,
    };

    let dims = facts
        .embedded
        .then_some(munarium_retrieval_pg::EMBED_DIMS as u32);
    let spec = reconstructed_spec(
        target,
        &facts.shape_ref,
        facts.watermark_seq,
        dims,
        &sources,
        // The version's OWN recorded extractor set. A manifest that predates
        // the field says `unknown`, which is true; substituting the current
        // extractor version would state that this version's text was produced
        // by software that may not have existed when it was built.
        facts
            .recorded_extractor_version
            .as_deref()
            .unwrap_or("unknown"),
    );
    let plan = mirror_plan(facts.embedded);
    mirror_index(ctx, pg, target, index_version_id, &spec, &plan).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(result: std::result::Result<MirrorOutcome, String>) -> VersionOutcome {
        VersionOutcome {
            index_version_id: "idx-a".into(),
            reason: RequiredReason::Active,
            result,
        }
    }

    /// Converged and already-built are successes. Counting them as failures
    /// would make a healthy re-run of a backfill look like an outage.
    #[test]
    fn convergence_counts_as_coverage_and_a_held_lease_does_not() {
        assert!(outcome(Ok(MirrorOutcome::Converged {
            artifact_id: "a".into()
        }))
        .is_complete());
        assert!(outcome(Ok(MirrorOutcome::AlreadyBuilt {
            artifact_id: "a".into()
        }))
        .is_complete());
        assert!(!outcome(Ok(MirrorOutcome::AlreadyRunning {
            owner_node_id: "n".into()
        }))
        .is_complete());
        assert!(!outcome(Err("boom".into())).is_complete());
    }

    /// A scope with no required versions is not a covered scope. Reporting it
    /// complete would let the selector CAS pass on something that cannot answer
    /// a query at all.
    #[test]
    fn an_empty_required_set_is_not_complete() {
        let report = BackfillReport {
            scope_id: "col".into(),
            policy: "active_pinned_and_horizon",
            versions: vec![],
        };
        assert!(!report.is_complete());
        assert_eq!(report.complete_count(), 0);
    }

    #[test]
    fn one_failed_version_makes_the_scope_incomplete() {
        let report = BackfillReport {
            scope_id: "col".into(),
            policy: "active",
            versions: vec![
                outcome(Ok(MirrorOutcome::Published {
                    artifact_id: "a".into(),
                    chunks: 3,
                    bound_staged: true,
                })),
                outcome(Err("export failed".into())),
            ],
        };
        assert!(!report.is_complete());
        assert_eq!(report.complete_count(), 1);
    }
}
