// SPDX-License-Identifier: Apache-2.0
//! What a build cost, reported to whoever is counting.
//!
//! A trait rather than a metrics dependency: this crate must stay free of the
//! server's exposition format, and Server owns the decision about what a series
//! is called and how it is labelled.
//!
//! ## Nothing here identifies anybody
//!
//! There is no tenant, collection, version, artifact id, path or query text in
//! this type, and there must never be — §13.1 forbids them as metric labels,
//! and a struct that carried one would make the forbidden thing the easy thing.
//! Tenant-scoped diagnosis goes through structured logs with hashed bounded
//! identifiers; this is for aggregate rates and durations.

use std::sync::Arc;
use std::time::Instant;

/// What one build did, in resources.
#[derive(Debug, Clone, PartialEq)]
pub struct BuildMetrics {
    /// `mirror` today; `direct` arrives with stage 7.
    pub mode: &'static str,
    /// `published` | `converged` | `already_built` | `deferred` | `failed`.
    pub outcome: &'static str,
    pub chunks: u64,
    /// Sealed component bytes. Zero when the build did no sealing.
    pub bytes: u64,
    /// Streaming committed chunks out of PostgreSQL.
    pub export_seconds: f64,
    /// Building and sealing the local artifact.
    pub seal_seconds: f64,
    /// Uploading components, writing the manifest, and reading it back.
    pub publish_seconds: f64,
    pub total_seconds: f64,
}

/// Receives one record per finished build.
pub trait BuildObserver: Send + Sync + std::fmt::Debug {
    fn build_finished(&self, metrics: &BuildMetrics);
}

/// A shareable observer handle.
pub type Observer = Arc<dyn BuildObserver>;

/// Accumulates phase timings across a build.
///
/// Deliberately a plain struct with explicit `mark` calls rather than a guard
/// type: the phases are not nested, and a guard would have to guess which one a
/// drop belonged to.
#[derive(Debug)]
pub(crate) struct BuildTimer {
    started: Instant,
    phase_started: Instant,
    pub export_seconds: f64,
    pub seal_seconds: f64,
    pub publish_seconds: f64,
    /// Sealed component bytes, filled in once the manifest exists.
    pub bytes: u64,
}

impl BuildTimer {
    pub fn start() -> Self {
        let now = Instant::now();
        Self {
            started: now,
            phase_started: now,
            export_seconds: 0.0,
            seal_seconds: 0.0,
            publish_seconds: 0.0,
            bytes: 0,
        }
    }

    fn lap(&mut self) -> f64 {
        let now = Instant::now();
        let d = now.duration_since(self.phase_started).as_secs_f64();
        self.phase_started = now;
        d
    }

    pub fn finished_export(&mut self) {
        self.export_seconds = self.lap();
    }

    pub fn finished_seal(&mut self) {
        self.seal_seconds = self.lap();
    }

    pub fn finished_publish(&mut self) {
        self.publish_seconds = self.lap();
    }

    pub fn total(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The type carries no identifier. This is asserted by construction —
    /// there is no field to set — and the test exists so that adding one is a
    /// deliberate act with a failing test beside it.
    #[test]
    fn build_metrics_carry_no_identifiers() {
        let m = BuildMetrics {
            mode: "mirror",
            outcome: "published",
            chunks: 10,
            bytes: 2048,
            export_seconds: 0.5,
            seal_seconds: 1.0,
            publish_seconds: 0.25,
            total_seconds: 1.75,
        };
        let debug = format!("{m:?}");
        for forbidden in ["tenant", "collection", "artifact", "version", "path"] {
            assert!(
                !debug.contains(forbidden),
                "a build metric must not carry {forbidden}"
            );
        }
    }

    #[test]
    fn phases_are_measured_separately_and_sum_below_the_total() {
        let mut t = BuildTimer::start();
        t.finished_export();
        t.finished_seal();
        t.finished_publish();
        let sum = t.export_seconds + t.seal_seconds + t.publish_seconds;
        assert!(sum <= t.total() + 1e-6, "phases cannot exceed the whole");
    }
}
