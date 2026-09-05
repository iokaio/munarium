// SPDX-License-Identifier: Apache-2.0
//! Running the shadow: sampling, admission, deadline, and the guarantee that
//! none of it reaches the user.
//!
//! The governing property is negative. In `shadow` mode PostgreSQL produces the
//! response and the error; the shadow's latency, its timeouts and its failures
//! must be invisible from outside. That is enforced by construction rather than
//! by care: [`ShadowRunner::submit`] never awaits the candidate. It decides
//! whether to sample, tries — never waits — for a permit, and hands the work to
//! a detached task under its own deadline. The only thing the request path pays
//! is a counter increment and a non-blocking `try_acquire`.
//!
//! Shadow work is also the first thing shed. A saturated semaphore means
//! `Dropped`, immediately, with a record saying so — because a drop rate that
//! is invisible looks like a sample rate, and the difference decides whether a
//! parity window means anything.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use munarium_core::retrieval::PreparedSearchQuery;

use crate::shadow::{QueryFingerprint, ShadowComparison, ShadowOutcome};

/// Where completed comparisons go.
///
/// A trait, like the build observer: this crate must not know whether a
/// comparison becomes a metric, a structured log line or a row.
pub trait ShadowObserver: Send + Sync + std::fmt::Debug {
    fn comparison(&self, comparison: &ShadowComparison);
}

pub type Observer = Arc<dyn ShadowObserver>;

/// How much shadow work this process will do.
#[derive(Debug, Clone)]
pub struct ShadowConfig {
    /// Sample one request in N. `0` disables shadowing entirely.
    ///
    /// A count rather than a float. "One in ten" is exactly the semantics, it
    /// cannot express 0.37 of a request, and it makes a test deterministic
    /// without a seeded RNG.
    pub sample_one_in: u64,
    /// How many shadow executions may run at once. Beyond this, requests are
    /// dropped rather than queued — a queue would let shadow work outlive the
    /// load spike that caused it.
    pub max_concurrent: usize,
    /// The candidate's own deadline. A shadow that exceeds it is abandoned.
    pub deadline: Duration,
}

impl Default for ShadowConfig {
    fn default() -> Self {
        // Off. Shadowing costs CPU on a replica that is serving users, and a
        // default that sampled would turn enabling `shadow` mode into a
        // performance change nobody asked for.
        Self {
            sample_one_in: 0,
            max_concurrent: 2,
            deadline: Duration::from_secs(5),
        }
    }
}

/// One request's worth of shadow work.
pub struct ShadowRequest {
    pub fingerprint: QueryFingerprint,
    pub reference_version: String,
    /// The SAME prepared query the reference search used.
    ///
    /// Shared rather than re-prepared, and that is the point of stage 1's
    /// coordinator-owned preparation: re-preparing would embed the query a
    /// second time — doubling the cost of the one part of a request that can
    /// call a provider — and would compare two independently produced vectors,
    /// which measures the embedder rather than the engines.
    pub prepared: Arc<PreparedSearchQuery>,
}

/// Decides, admits and runs shadow executions.
#[derive(Debug)]
pub struct ShadowRunner {
    config: ShadowConfig,
    permits: Arc<tokio::sync::Semaphore>,
    counter: AtomicU64,
    observer: Option<Observer>,
}

impl ShadowRunner {
    pub fn new(config: ShadowConfig, observer: Option<Observer>) -> Self {
        Self {
            permits: Arc::new(tokio::sync::Semaphore::new(config.max_concurrent.max(1))),
            counter: AtomicU64::new(0),
            config,
            observer,
        }
    }

    pub fn config(&self) -> &ShadowConfig {
        &self.config
    }

    /// Whether this request is the sampled one.
    ///
    /// Counter-based rather than random: every query class is sampled at the
    /// same rate, which a fingerprint-derived decision would not do — that
    /// would pick a fixed subset of questions and never measure the rest.
    fn sampled(&self) -> bool {
        match self.config.sample_one_in {
            0 => false,
            1 => true,
            n => self
                .counter
                .fetch_add(1, Ordering::Relaxed)
                .is_multiple_of(n),
        }
    }

    /// Consider running a shadow, and return WITHOUT waiting for it.
    ///
    /// The returned outcome is the admission decision, not the comparison: a
    /// caller that wanted the comparison would have to await it, which is the
    /// one thing this must never make possible. Completed comparisons reach the
    /// observer.
    pub fn submit<F, Fut>(&self, request: ShadowRequest, candidate: F) -> ShadowOutcome
    where
        F: FnOnce(Arc<PreparedSearchQuery>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ShadowResult> + Send,
    {
        if !self.sampled() {
            self.record(ShadowComparison::unrun(
                request.fingerprint,
                request.reference_version,
                ShadowOutcome::NotSampled,
            ));
            return ShadowOutcome::NotSampled;
        }

        // try_acquire, never acquire: waiting for a permit would put shadow
        // admission on the request path, which is exactly the coupling this
        // whole design refuses.
        let Ok(permit) = Arc::clone(&self.permits).try_acquire_owned() else {
            self.record(ShadowComparison::unrun(
                request.fingerprint,
                request.reference_version,
                ShadowOutcome::Dropped,
            ));
            return ShadowOutcome::Dropped;
        };

        let observer = self.observer.clone();
        let deadline = self.config.deadline;
        let fingerprint = request.fingerprint.clone();
        let version = request.reference_version.clone();
        let prepared = request.prepared;

        tokio::spawn(async move {
            // The permit is held for the whole execution and released on drop,
            // including on timeout — the task is abandoned, so the abandoned
            // work must not hold capacity for the next one.
            let _permit = permit;
            let outcome = match tokio::time::timeout(deadline, candidate(prepared)).await {
                Ok(ShadowResult::Compared(c)) => *c,
                Ok(ShadowResult::Refused) => {
                    ShadowComparison::unrun(fingerprint, version, ShadowOutcome::Rejected)
                }
                Ok(ShadowResult::Failed) => {
                    ShadowComparison::unrun(fingerprint, version, ShadowOutcome::Error)
                }
                Err(_) => ShadowComparison::unrun(fingerprint, version, ShadowOutcome::Timeout),
            };
            if let Some(o) = observer {
                o.comparison(&outcome);
            }
        });

        ShadowOutcome::Completed
    }

    fn record(&self, comparison: ShadowComparison) {
        if let Some(o) = &self.observer {
            o.comparison(&comparison);
        }
    }
}

/// What one candidate execution produced.
///
/// The comparison is boxed: it is by far the largest variant, and every
/// refusal, failure and timeout would otherwise carry its footprint through the
/// task machinery for nothing.
pub enum ShadowResult {
    Compared(Box<ShadowComparison>),
    /// The candidate declined: no `shadow` binding, an unsupported reader, a
    /// quarantined artifact. Distinct from a failure because it is a state, not
    /// an incident, and a rollout gate reads them differently.
    Refused,
    Failed,
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::shadow::PhaseLatency;

    #[derive(Debug, Default)]
    pub(super) struct Recorder(pub(super) Mutex<Vec<ShadowComparison>>);

    impl ShadowObserver for Recorder {
        fn comparison(&self, c: &ShadowComparison) {
            self.0.lock().unwrap().push(c.clone());
        }
    }

    impl Recorder {
        fn outcomes(&self) -> Vec<ShadowOutcome> {
            self.0.lock().unwrap().iter().map(|c| c.outcome).collect()
        }
    }

    fn prepared() -> Arc<PreparedSearchQuery> {
        Arc::new(PreparedSearchQuery {
            lexical: None,
            embedding: None,
            lexical_candidates: 10,
            vector_candidates: 10,
            top_k: 5,
            rrf_k: 60.0,
        })
    }

    fn request() -> ShadowRequest {
        ShadowRequest {
            fingerprint: QueryFingerprint::of("q"),
            reference_version: "idx-1".into(),
            prepared: prepared(),
        }
    }

    fn runner(config: ShadowConfig, r: &Arc<Recorder>) -> ShadowRunner {
        ShadowRunner::new(config, Some(r.clone()))
    }

    /// Disabled means disabled: nothing runs, and every request is recorded as
    /// unsampled so the rate is observable rather than inferred.
    #[tokio::test]
    async fn a_zero_rate_never_runs_the_candidate() {
        let rec = Arc::new(Recorder::default());
        let r = runner(
            ShadowConfig {
                sample_one_in: 0,
                ..Default::default()
            },
            &rec,
        );
        let ran = Arc::new(AtomicU64::new(0));
        for _ in 0..5 {
            let ran = ran.clone();
            let outcome = r.submit(request(), move |_| async move {
                ran.fetch_add(1, Ordering::SeqCst);
                ShadowResult::Failed
            });
            assert_eq!(outcome, ShadowOutcome::NotSampled);
        }
        assert_eq!(ran.load(Ordering::SeqCst), 0);
        assert_eq!(rec.outcomes().len(), 5);
        assert!(rec
            .outcomes()
            .iter()
            .all(|o| *o == ShadowOutcome::NotSampled));
    }

    /// One in N means one in N, deterministically.
    #[tokio::test]
    async fn sampling_takes_one_request_in_n() {
        let rec = Arc::new(Recorder::default());
        let r = runner(
            ShadowConfig {
                sample_one_in: 3,
                ..Default::default()
            },
            &rec,
        );
        let mut admitted = 0;
        for _ in 0..9 {
            if r.submit(request(), |_| async { ShadowResult::Failed }) != ShadowOutcome::NotSampled
            {
                admitted += 1;
            }
        }
        assert_eq!(admitted, 3, "one in three of nine");
    }

    /// The user path never waits. A candidate that takes far longer than the
    /// deadline must not delay `submit` at all.
    #[tokio::test]
    async fn submit_returns_without_waiting_for_the_candidate() {
        let rec = Arc::new(Recorder::default());
        let r = runner(
            ShadowConfig {
                sample_one_in: 1,
                max_concurrent: 1,
                deadline: Duration::from_millis(50),
            },
            &rec,
        );
        let started = std::time::Instant::now();
        r.submit(request(), |_| async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            ShadowResult::Failed
        });
        assert!(
            started.elapsed() < Duration::from_millis(200),
            "submit blocked for {:?}",
            started.elapsed()
        );

        // And the candidate is abandoned at the deadline, recorded as a
        // timeout rather than left to finish in 30 seconds.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(rec.outcomes(), vec![ShadowOutcome::Timeout]);
    }

    /// Saturation sheds rather than queues. A queue would let shadow work
    /// outlive the load spike that caused it.
    #[tokio::test]
    async fn a_saturated_runner_drops_rather_than_queueing() {
        let rec = Arc::new(Recorder::default());
        let r = runner(
            ShadowConfig {
                sample_one_in: 1,
                max_concurrent: 1,
                deadline: Duration::from_secs(5),
            },
            &rec,
        );
        let gate = Arc::new(tokio::sync::Notify::new());

        let held = gate.clone();
        r.submit(request(), move |_| async move {
            held.notified().await;
            ShadowResult::Failed
        });
        // Let the spawned task take the permit.
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(
            r.submit(request(), |_| async { ShadowResult::Failed }),
            ShadowOutcome::Dropped
        );
        assert!(rec.outcomes().contains(&ShadowOutcome::Dropped));

        // The permit comes back when the first finishes, so a drop is a
        // momentary shed and not a permanent loss of capacity.
        gate.notify_waiters();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            r.submit(request(), |_| async { ShadowResult::Failed }),
            ShadowOutcome::Completed
        );
    }

    /// A refusal and a failure are different records: one is a state a rollout
    /// gate expects to see, the other is an incident.
    #[tokio::test]
    async fn a_refusal_is_recorded_separately_from_a_failure() {
        let rec = Arc::new(Recorder::default());
        let r = runner(
            ShadowConfig {
                sample_one_in: 1,
                max_concurrent: 4,
                deadline: Duration::from_secs(5),
            },
            &rec,
        );
        r.submit(request(), |_| async { ShadowResult::Refused });
        r.submit(request(), |_| async { ShadowResult::Failed });
        tokio::time::sleep(Duration::from_millis(100)).await;

        let got = rec.outcomes();
        assert!(got.contains(&ShadowOutcome::Rejected), "{got:?}");
        assert!(got.contains(&ShadowOutcome::Error), "{got:?}");
    }

    /// The candidate receives the SAME prepared query the reference used —
    /// same allocation, so the vector cannot have been produced twice.
    #[tokio::test]
    async fn the_candidate_receives_the_reference_prepared_query() {
        let rec = Arc::new(Recorder::default());
        let r = runner(
            ShadowConfig {
                sample_one_in: 1,
                max_concurrent: 2,
                deadline: Duration::from_secs(5),
            },
            &rec,
        );
        let shared = prepared();
        // The ADDRESS, not the pointer: a raw pointer is not `Send`, and the
        // claim under test is only that both sides hold one allocation.
        let want = Arc::as_ptr(&shared) as usize;
        let seen: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(None));
        let sink = seen.clone();

        r.submit(
            ShadowRequest {
                fingerprint: QueryFingerprint::of("q"),
                reference_version: "idx-1".into(),
                prepared: shared.clone(),
            },
            move |p| async move {
                *sink.lock().unwrap() = Some(Arc::as_ptr(&p) as usize);
                ShadowResult::Compared(Box::new(ShadowComparison::unrun(
                    QueryFingerprint::of("q"),
                    "idx-1",
                    ShadowOutcome::Completed,
                )))
            },
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            seen.lock().unwrap().unwrap(),
            want,
            "the shadow must share the reference's prepared query, not a copy"
        );
    }

    /// A completed comparison reaches the observer with its content intact.
    #[tokio::test]
    async fn a_completed_comparison_reaches_the_observer() {
        let rec = Arc::new(Recorder::default());
        let r = runner(
            ShadowConfig {
                sample_one_in: 1,
                max_concurrent: 2,
                deadline: Duration::from_secs(5),
            },
            &rec,
        );
        r.submit(request(), |_| async {
            let mut c = ShadowComparison::unrun(
                QueryFingerprint::of("q"),
                "idx-1",
                ShadowOutcome::Completed,
            );
            c.candidate_artifact_id = Some("art".into());
            c.candidate_latency = PhaseLatency {
                total_ms: 12.5,
                ..Default::default()
            };
            ShadowResult::Compared(Box::new(c))
        });
        tokio::time::sleep(Duration::from_millis(100)).await;

        let got = rec.0.lock().unwrap();
        let c = got.first().expect("one comparison");
        assert_eq!(c.outcome, ShadowOutcome::Completed);
        assert_eq!(c.candidate_artifact_id.as_deref(), Some("art"));
        assert_eq!(c.candidate_latency.total_ms, 12.5);
    }
}

#[cfg(test)]
mod embedder_budget_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use munarium_core::retrieval::{QueryEmbedder, SearchParams};

    use super::*;

    /// Counts every embedding this process would pay for.
    #[derive(Debug, Default)]
    struct CountingEmbedder {
        embeds: AtomicUsize,
        blends: AtomicUsize,
    }

    impl CountingEmbedder {
        /// What a provider-backed embedder would have been billed for.
        fn calls(&self) -> usize {
            self.embeds.load(Ordering::SeqCst) + self.blends.load(Ordering::SeqCst)
        }
    }

    impl QueryEmbedder for CountingEmbedder {
        fn embed(&self, _text: &str) -> Vec<f32> {
            self.embeds.fetch_add(1, Ordering::SeqCst);
            vec![0.0; 4]
        }

        fn blend(&self, _original: &str, _expanded: &str, _weight: f32) -> Vec<f32> {
            self.blends.fetch_add(1, Ordering::SeqCst);
            vec![0.0; 4]
        }

        fn fingerprint(&self) -> String {
            "test/counting/4".into()
        }

        fn dimensions(&self) -> usize {
            4
        }
    }

    /// **Shadowing must not cost a second embedding.**
    ///
    /// This is the whole reason stage 1 hoisted query preparation out of the
    /// per-collection search path. If the shadow prepared its own query it
    /// would double the one part of a request that can call a paid provider,
    /// AND it would then be comparing two independently produced vectors —
    /// measuring the embedder rather than the two engines.
    ///
    /// Measured as a DIFFERENCE against the same work without a shadow, rather
    /// than as an absolute count: the turn pipeline legitimately prepares twice
    /// (the probe and the deep search are different query formulations), and an
    /// absolute expectation would encode that unrelated number here and break
    /// the day it changes.
    #[tokio::test]
    async fn a_shadowed_request_pays_for_no_extra_embedding() {
        let params = SearchParams::default();

        let without = Arc::new(CountingEmbedder::default());
        let _ = munarium_retrieval_pg::PgRetrieval::prepare_query(
            "what did washington write",
            &params,
            without.as_ref(),
        );
        let baseline = without.calls();
        assert!(
            baseline > 0,
            "the reference search must embed at least once"
        );

        let with = Arc::new(CountingEmbedder::default());
        let prepared = Arc::new(munarium_retrieval_pg::PgRetrieval::prepare_query(
            "what did washington write",
            &params,
            with.as_ref(),
        ));

        let rec = Arc::new(super::tests::Recorder::default());
        let runner = ShadowRunner::new(
            ShadowConfig {
                sample_one_in: 1,
                max_concurrent: 2,
                deadline: Duration::from_secs(5),
            },
            Some(rec.clone()),
        );
        let outcome = runner.submit(
            ShadowRequest {
                fingerprint: QueryFingerprint::of("what did washington write"),
                reference_version: "idx-1".into(),
                prepared: prepared.clone(),
            },
            move |p| async move {
                // A candidate reads the vector it was handed. If it had to
                // produce one, the count below would move.
                assert!(p.embedding.is_some());
                ShadowResult::Compared(Box::new(ShadowComparison::unrun(
                    QueryFingerprint::of("q"),
                    "idx-1",
                    ShadowOutcome::Completed,
                )))
            },
        );
        assert_eq!(outcome, ShadowOutcome::Completed);
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert_eq!(
            with.calls(),
            baseline,
            "shadowing added {} embedding call(s)",
            with.calls() - baseline
        );
    }
}
