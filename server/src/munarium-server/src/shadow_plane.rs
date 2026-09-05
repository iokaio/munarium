// SPDX-License-Identifier: Apache-2.0
//! The server's shadow plane: wiring `ShadowRunner` + the candidate executor
//! into the turn pipeline, and counting what happens.
//!
//! Everything here is `shadow`-mode only and fails towards absence: a process
//! in any other mode, or one whose configuration cannot support a candidate
//! (no local root, no artifact store), simply has no plane — and the request
//! path's whole interaction with a missing plane is one `is_none` check. The
//! plan's negative property (§9.1: shadow work is invisible to the user and
//! shed first under load) is enforced inside `ShadowRunner::submit`; this
//! module only decides what a candidate DOES when it is admitted.
//!
//! The stats exist because stage 6's per-corpus gates read them: sampled,
//! dropped, timed out, rejected, completed, and — separately, because no
//! tolerance band may absorb it — corrupting comparisons. A drop rate that is
//! invisible looks like a sample rate (§13.2), so every outcome is counted,
//! including the not-sampled ones.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use munarium_core::retrieval::{PreparedSearchQuery, SearchResult};
use munarium_retrieval::executor::ExecutionOutcome;
use munarium_retrieval::shadow::{PhaseLatency, QueryFingerprint, ShadowComparison, ShadowOutcome};
use munarium_retrieval::shadow_candidate::{comparison, execute_candidate};
use munarium_retrieval::shadow_exec::{
    ShadowConfig, ShadowObserver, ShadowRequest, ShadowResult, ShadowRunner,
};

use crate::datastore_serving::DatastoreParts;
use crate::state::AppState;

/// Counters over every shadow decision this process made.
///
/// Atomics rather than a lock: the request path increments these, and the
/// admin page reads them, and neither should ever wait for the other.
#[derive(Debug, Default)]
pub struct ShadowStats {
    pub completed: AtomicU64,
    pub not_sampled: AtomicU64,
    pub dropped: AtomicU64,
    pub timeout: AtomicU64,
    pub rejected: AtomicU64,
    pub error: AtomicU64,
    /// Completed comparisons whose identity check failed — a text-hash or
    /// provenance mismatch. Counted apart because §13.2 says no tolerance
    /// band absorbs one; a single non-zero here is a finding, not a rate.
    pub corrupting: AtomicU64,
    /// Sum of fused overlap fractions ×1000, over completed comparisons.
    /// Integer so it can be atomic; the page divides back out. Three decimal
    /// places is more resolution than any parity gate reads.
    pub fused_overlap_millis: AtomicU64,
}

impl ShadowStats {
    /// A point-in-time copy for rendering.
    pub fn snapshot(&self) -> ShadowStatsSnapshot {
        let completed = self.completed.load(Ordering::Relaxed);
        ShadowStatsSnapshot {
            completed,
            not_sampled: self.not_sampled.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            timeout: self.timeout.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
            error: self.error.load(Ordering::Relaxed),
            corrupting: self.corrupting.load(Ordering::Relaxed),
            mean_fused_overlap: if completed == 0 {
                None
            } else {
                Some(
                    self.fused_overlap_millis.load(Ordering::Relaxed) as f64
                        / 1000.0
                        / completed as f64,
                )
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ShadowStatsSnapshot {
    pub completed: u64,
    pub not_sampled: u64,
    pub dropped: u64,
    pub timeout: u64,
    pub rejected: u64,
    pub error: u64,
    pub corrupting: u64,
    /// `None` until a comparison completes — the mean of no measurements is
    /// not 1.0 and not 0.0, it is no measurement.
    pub mean_fused_overlap: Option<f64>,
}

impl ShadowObserver for ShadowStats {
    fn comparison(&self, c: &ShadowComparison) {
        let counter = match c.outcome {
            ShadowOutcome::Completed => &self.completed,
            ShadowOutcome::NotSampled => &self.not_sampled,
            ShadowOutcome::Dropped => &self.dropped,
            ShadowOutcome::Timeout => &self.timeout,
            ShadowOutcome::Rejected => &self.rejected,
            ShadowOutcome::Error => &self.error,
        };
        counter.fetch_add(1, Ordering::Relaxed);
        if c.outcome == ShadowOutcome::Completed {
            if c.is_corrupting() {
                self.corrupting.fetch_add(1, Ordering::Relaxed);
                // The one shadow event that warrants a log line on its own:
                // the same chunk id resolved to different bytes or a different
                // document. Fingerprint only — never the query.
                tracing::error!(
                    fingerprint = %c.query_fingerprint,
                    version = %c.reference_version,
                    artifact = c.candidate_artifact_id.as_deref().unwrap_or("-"),
                    "shadow comparison found a corrupting identity mismatch"
                );
            }
            if let Some(fused) = &c.fused {
                self.fused_overlap_millis.fetch_add(
                    (fused.overlap_fraction.clamp(0.0, 1.0) * 1000.0) as u64,
                    Ordering::Relaxed,
                );
            }
        }
    }
}

/// The process-wide shadow machinery: one runner, one cache, one store
/// factory, shared across tenants. Tenant scoping happens per submit, in the
/// catalog handle and the cache key's isolation domain.
pub struct ShadowPlane {
    runner: ShadowRunner,
    /// The SHARED datastore infrastructure — the same L1 cache and L0
    /// open-shard tier the serving plane uses. The shadow plane predates
    /// `DatastoreParts` and briefly built its own cache; two caches over one
    /// directory are two eviction ledgers, each free to delete what the
    /// other believes resident, so it shares now.
    parts: Arc<DatastoreParts>,
    stats: Arc<ShadowStats>,
    sample_one_in: u64,
}

impl std::fmt::Debug for ShadowPlane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShadowPlane")
            .field("sample_one_in", &self.sample_one_in)
            .finish_non_exhaustive()
    }
}

impl ShadowPlane {
    /// Build the plane, or explain why there is none.
    ///
    /// `None` is the ordinary answer for every mode but `shadow`. In `shadow`
    /// mode a missing prerequisite logs at error level and still returns
    /// `None` — the mode's contract is that PostgreSQL serves regardless, so a
    /// half-configured shadow disables itself rather than degrading requests.
    pub fn build(state: &AppState) -> Option<Arc<Self>> {
        if state.retrieval_mode_str() != "shadow" {
            return None;
        }

        let sample_one_in = std::env::var("MUNARIUM_DATASTORE_SHADOW_SAMPLE_RATE")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        if sample_one_in == 0 {
            // Deliberate: sampling costs CPU on a serving replica, so the rate
            // is an explicit operator choice, never a default. But shadow mode
            // with no rate measures nothing, and someone should know that.
            tracing::warn!(
                "shadow mode is on but MUNARIUM_DATASTORE_SHADOW_SAMPLE_RATE is unset or 0; \
                 no comparisons will run"
            );
        }

        let Some(parts) = state.datastore_parts() else {
            // The parts builder already logged exactly what was missing.
            tracing::error!("shadow plane disabled: no datastore infrastructure");
            return None;
        };

        let deadline_ms = std::env::var("MUNARIUM_DATASTORE_QUERY_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5_000u64);
        let max_concurrent = std::env::var("MUNARIUM_DATASTORE_SHADOW_MAX_CONCURRENT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2usize);

        let stats = Arc::new(ShadowStats::default());
        let runner = ShadowRunner::new(
            ShadowConfig {
                sample_one_in,
                max_concurrent,
                deadline: Duration::from_millis(deadline_ms.max(1)),
            },
            Some(stats.clone()),
        );

        Some(Arc::new(Self {
            runner,
            parts: Arc::clone(parts),
            stats,
            sample_one_in,
        }))
    }

    pub fn stats(&self) -> &Arc<ShadowStats> {
        &self.stats
    }

    pub fn sample_one_in(&self) -> u64 {
        self.sample_one_in
    }

    /// Consider shadowing one collection's deep search. Never waits.
    ///
    /// The reference result's envelope names the logical version both sides
    /// answer; a result with no version (a legacy producer) is skipped rather
    /// than compared against nothing.
    pub fn submit(
        self: &Arc<Self>,
        pool: sqlx::PgPool,
        tenant: &str,
        query: &str,
        prepared: &Arc<PreparedSearchQuery>,
        reference: &SearchResult,
        reference_latency: PhaseLatency,
    ) {
        let version = reference.envelope.index_version.clone();
        if version.is_empty() {
            return;
        }

        let ctx = self.parts.executor(&pool, tenant);
        // Owned copies for the detached task. The query string crosses into
        // the closure to be fingerprinted and compared — the comparison type
        // has nowhere to store it, so it cannot outlive the task.
        let query = query.to_string();
        let reference = reference.clone();

        self.runner.submit(
            ShadowRequest {
                fingerprint: QueryFingerprint::of(&query),
                reference_version: version.clone(),
                prepared: Arc::clone(prepared),
            },
            move |prepared| async move {
                match execute_candidate(&ctx, &version, &prepared).await {
                    ExecutionOutcome::Executed(execution) => {
                        ShadowResult::Compared(Box::new(comparison(
                            &query,
                            &version,
                            &reference,
                            &execution,
                            reference_latency,
                            None,
                        )))
                    }
                    ExecutionOutcome::Refused(reason) => {
                        tracing::debug!(%version, %reason, "shadow candidate refused");
                        ShadowResult::Refused
                    }
                    ExecutionOutcome::Failed(reason) => {
                        tracing::warn!(%version, %reason, "shadow candidate failed");
                        ShadowResult::Failed
                    }
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use munarium_retrieval::shadow::{IdentityDelta, LegComparison};

    fn completed(with_overlap: f64, corrupting: bool) -> ShadowComparison {
        let mut c =
            ShadowComparison::unrun(QueryFingerprint::of("q"), "idx-1", ShadowOutcome::Completed);
        c.fused = Some(LegComparison::of(&[], &[]));
        if let Some(f) = c.fused.as_mut() {
            f.overlap_fraction = with_overlap;
        }
        if corrupting {
            c.identity = IdentityDelta {
                text_hash_mismatches: vec!["x".into()],
                ..Default::default()
            };
        }
        c
    }

    /// Every outcome lands in exactly one counter, and corruption is counted
    /// beside completion rather than instead of it.
    #[test]
    fn stats_count_every_outcome_and_corruption_separately() {
        let stats = ShadowStats::default();
        for outcome in [
            ShadowOutcome::NotSampled,
            ShadowOutcome::Dropped,
            ShadowOutcome::Timeout,
            ShadowOutcome::Rejected,
            ShadowOutcome::Error,
        ] {
            stats.comparison(&ShadowComparison::unrun(
                QueryFingerprint::of("q"),
                "idx-1",
                outcome,
            ));
        }
        stats.comparison(&completed(0.8, false));
        stats.comparison(&completed(0.6, true));

        let s = stats.snapshot();
        assert_eq!(s.not_sampled, 1);
        assert_eq!(s.dropped, 1);
        assert_eq!(s.timeout, 1);
        assert_eq!(s.rejected, 1);
        assert_eq!(s.error, 1);
        assert_eq!(s.completed, 2);
        assert_eq!(s.corrupting, 1);
        let mean = s.mean_fused_overlap.unwrap();
        assert!((mean - 0.7).abs() < 0.01, "{mean}");
    }

    /// No completions means no mean — not a perfect 1.0 and not a damning 0.0.
    #[test]
    fn the_mean_overlap_of_no_comparisons_is_no_measurement() {
        let stats = ShadowStats::default();
        stats.comparison(&ShadowComparison::unrun(
            QueryFingerprint::of("q"),
            "idx-1",
            ShadowOutcome::Dropped,
        ));
        assert_eq!(stats.snapshot().mean_fused_overlap, None);
    }
}
