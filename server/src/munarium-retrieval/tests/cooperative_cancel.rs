// SPDX-License-Identifier: Apache-2.0
//! ClearView R6 cooperative cancellation (R6-T1), against a real PostgreSQL
//! and a real local-file artifact store.
//!
//! Each case runs a QUEUED build under a job claim, pauses it at a named
//! `BuildPhase`, cancels the job through the ordinary `BuildJobs::cancel`,
//! and resumes or abandons the old worker. The assertions are the R6-T1
//! table: what the v1.4 cancel response and job state are, and what the
//! durable per-version history (`BuildJobs::version_history`) establishes.
//!
//! Job rows are seeded with tenant-scoped SQL that mirrors `claim_any`
//! (including its run record), never with the global `claim_any`, and every
//! seeded running row has `claimed_at = 'infinity'`, so no claimer or
//! expired-claim cleaner can take it. A re-enqueue goes through the real
//! `enqueue` and is immediately claimed the same way by the test's second
//! builder. Every case leaves its jobs terminal. Skips loudly when
//! `MUNARIUM_TEST_DATABASE_URL` is unset.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use munarium_retrieval::backfill::{backfill_collection, backfill_one};
use munarium_retrieval::direct::build_collection_direct;
use munarium_retrieval::mirror::{
    reconcile_attempts, BuildPhase, FaultHook, LocalStoreFactory, MirrorContext, MirrorOutcome,
    MirrorTarget, VectorPolicy,
};
use munarium_retrieval_pg::required::RequiredVersionsPolicy;
use munarium_retrieval_pg::PgRetrieval;
use munarium_store_pg::artifacts::{ArtifactCatalog, BindingSlot};
use munarium_store_pg::attempts::{AttemptState, BuildAttempts, IntentState};
use munarium_store_pg::jobs::{
    BuildJobs, EnqueueOutcome, HistoryStatus, JobFence, JobId, JobKind, JobSignal, JobTarget,
    ProgressPhase, StopReason, STALE_GENERATION_MESSAGE,
};
use munarium_store_pg::PgStore;

fn url() -> Option<String> {
    std::env::var("MUNARIUM_TEST_DATABASE_URL").ok()
}

macro_rules! guard {
    () => {
        if url().is_none() {
            eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
            return;
        }
    };
}

const NODE_A: &str = "builder-a";
const NODE_B: &str = "builder-b";

struct Fixture {
    pg: PgRetrieval,
    pool: sqlx::PgPool,
    tenant: String,
    collection_id: String,
    /// The versions built in PostgreSQL, oldest first (empty for a direct
    /// fixture).
    versions: Vec<String>,
    artifacts: tempfile::TempDir,
    staging: tempfile::TempDir,
}

async fn add_doc(pg: &PgRetrieval, collection_id: &str, i: usize) {
    let body = format!(
        "Cancelled document {i}. The surveyor recorded the river crossing at dawn.\n\n\
         A second paragraph about the ferry and the toll road beyond it."
    );
    let (source_id, _, _) = pg
        .put_source(
            "",
            "text/markdown",
            &format!("cancel/doc-{i}.md"),
            Some("para"),
            body.as_bytes(),
        )
        .await
        .unwrap();
    pg.bind_source(collection_id, &source_id, None)
        .await
        .unwrap();
}

/// `versions` PostgreSQL versions, each activated in turn, so earlier ones
/// are retired but within the pin horizon.
async fn fixture(tag: &str, versions: usize) -> Fixture {
    let tenant = format!("tenant-r6-{tag}-{}", uuid::Uuid::new_v4().simple());
    let store = PgStore::connect(&url().expect("guarded"), &tenant)
        .await
        .unwrap();
    let pg = PgRetrieval::new(store.pool().clone(), &tenant);
    let col = pg
        .ensure_collection(
            &format!("col-{}", uuid::Uuid::new_v4().simple()),
            "para",
            0,
            &[],
            Some("R6 cooperative cancellation"),
        )
        .await
        .unwrap();
    add_doc(&pg, &col.id, 0).await;
    add_doc(&pg, &col.id, 1).await;
    let mut built = Vec::new();
    for v in 0..versions {
        if v > 0 {
            add_doc(&pg, &col.id, 10 + v).await;
        }
        built.push(
            pg.build_collection_index(&col.id, 400, 1 + v as u64, true)
                .await
                .unwrap()
                .id,
        );
    }
    Fixture {
        pg,
        pool: store.pool().clone(),
        tenant,
        collection_id: col.id.clone(),
        versions: built,
        artifacts: tempfile::tempdir().unwrap(),
        staging: tempfile::tempdir().unwrap(),
    }
}

impl Fixture {
    fn ctx(
        &self,
        node: &str,
        fence: Option<&JobFence>,
        signal: Option<&JobSignal>,
        faults: Option<FaultHook>,
    ) -> MirrorContext {
        MirrorContext {
            catalog: ArtifactCatalog::new(self.pool.clone(), &self.tenant),
            attempts: BuildAttempts::new(self.pool.clone(), &self.tenant),
            stores: Arc::new(LocalStoreFactory::new(self.artifacts.path())),
            node_id: node.into(),
            staging_root: self.staging.path().to_path_buf(),
            artifact_prefix: "v1".into(),
            tenant_path_hash: "t0000".into(),
            faults,
            observer: None,
            vector_policy: VectorPolicy {
                approx_threshold: None,
            },
            job_fence: fence.cloned(),
            job_signal: signal.cloned(),
        }
    }

    fn jobs(&self) -> BuildJobs {
        BuildJobs::new(self.pool.clone())
    }

    fn version(&self) -> &str {
        self.versions.last().expect("a built version")
    }

    /// Mirror `claim_any`'s claim of an existing row, with its run record,
    /// but tenant-scoped and with a lease no cleaner can expire.
    async fn claim_row(&self, job_id: &str, node: &str) -> JobFence {
        let generation: i32 = sqlx::query_scalar(
            "UPDATE index_build_jobs
                SET state = 'running', claimed_by = $3, claimed_at = 'infinity',
                    attempts = attempts + 1, updated_at = now()
              WHERE tenant_id = $1 AND job_id = $2
          RETURNING attempts",
        )
        .bind(&self.tenant)
        .bind(job_id)
        .bind(node)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO index_build_job_runs (tenant_id, job_id, job_generation, node_id)
             VALUES ($1,$2,$3,$4)",
        )
        .bind(&self.tenant)
        .bind(job_id)
        .bind(generation)
        .bind(node)
        .execute(&self.pool)
        .await
        .unwrap();
        JobFence {
            tenant_id: self.tenant.clone(),
            job_id: job_id.to_string(),
            node_id: node.into(),
            generation,
        }
    }

    /// Seed a job already claimed once by `node`, non-claimable.
    async fn seed_claimed_job(&self, kind: &str, scope: bool, node: &str) -> JobFence {
        let job_id = format!("bjob-r6-{}", uuid::Uuid::new_v4().simple());
        let (scope_kind, scope_id, version) = if scope {
            (Some("collection"), Some(self.collection_id.as_str()), None)
        } else {
            (None, None, Some(self.version()))
        };
        sqlx::query(
            "INSERT INTO index_build_jobs
                 (tenant_id, job_id, kind, scope_kind, scope_id, index_version_id,
                  params, state, claimed_at, attempts, requested_by)
             VALUES ($1,$2,$3,$4,$5,$6,'{}'::jsonb,'cancelled','infinity',0,'test-r6')",
        )
        .bind(&self.tenant)
        .bind(&job_id)
        .bind(kind)
        .bind(scope_kind)
        .bind(scope_id)
        .bind(version)
        .execute(&self.pool)
        .await
        .unwrap();
        // Inserted terminal, then claimed: never claimable in between.
        self.claim_row(&job_id, node).await
    }

    /// Enqueue the same target through the real API and claim it at once on
    /// the second builder.
    async fn reenqueue_and_claim(&self, kind: JobKind, node: &str) -> JobFence {
        let target = match kind {
            JobKind::Rebuild => JobTarget::Version(self.version()),
            _ => JobTarget::Scope {
                scope_kind: "collection",
                scope_id: &self.collection_id,
            },
        };
        let outcome = self
            .jobs()
            .enqueue(
                &self.tenant,
                kind,
                target,
                serde_json::json!({}),
                "test-r6",
                None,
            )
            .await
            .unwrap();
        let EnqueueOutcome::Enqueued(job_id) = outcome else {
            panic!("the cancelled target enqueues at once: {outcome:?}");
        };
        self.claim_row(&job_id, node).await
    }

    async fn finish(&self, fence: &JobFence) {
        assert!(self
            .jobs()
            .complete_fenced(
                &self.tenant,
                &fence.job_id,
                &fence.node_id,
                fence.generation,
                Ok(serde_json::json!({})),
                None,
            )
            .await
            .unwrap());
    }

    /// The v1.4 view of a cancel: `ok` and the row's visible state.
    async fn cancel(&self, fence: &JobFence) -> (bool, String) {
        let ok = self
            .jobs()
            .cancel(&self.tenant, &fence.job_id)
            .await
            .unwrap();
        let state = self
            .jobs()
            .get(&self.tenant, &fence.job_id)
            .await
            .unwrap()
            .unwrap()
            .state;
        (ok, state)
    }

    async fn history(&self, fence: &JobFence) -> munarium_store_pg::jobs::JobVersionHistory {
        self.jobs()
            .version_history(
                &self.tenant,
                &JobId::parse(&fence.job_id).unwrap(),
                None,
                50,
            )
            .await
            .unwrap()
            .unwrap()
    }

    async fn assert_stale_refused(&self, stale: &JobFence) {
        let jobs = self.jobs();
        assert!(!jobs
            .heartbeat_fenced(
                &self.tenant,
                &stale.job_id,
                &stale.node_id,
                stale.generation
            )
            .await
            .unwrap());
        assert!(!jobs
            .complete_fenced(
                &self.tenant,
                &stale.job_id,
                &stale.node_id,
                stale.generation,
                Ok(serde_json::json!({"stale": true})),
                None,
            )
            .await
            .unwrap());
    }

    fn target(&self) -> MirrorTarget<'_> {
        MirrorTarget::Collection {
            collection_id: &self.collection_id,
        }
    }

    async fn linked_attempts(&self, fence: &JobFence) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT attempt_id FROM index_build_attempt_jobs
              WHERE tenant_id = $1 AND job_id = $2 AND job_generation = $3
              ORDER BY linked_at",
        )
        .bind(&self.tenant)
        .bind(&fence.job_id)
        .bind(fence.generation)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    fn object_at(&self, key: &str) -> Option<Vec<u8>> {
        std::fs::read(self.artifacts.path().join(key)).ok()
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    hex::encode(sha2::Sha256::digest(bytes))
}

struct Pause {
    reached: tokio::sync::oneshot::Receiver<()>,
    resume: std::sync::mpsc::Sender<()>,
}

/// Pause the build the `nth` time (1-based) it reaches `phase`.
fn pause_at_nth(phase: BuildPhase, nth: usize, crash_after: bool) -> (FaultHook, Pause) {
    let (reached_tx, reached) = tokio::sync::oneshot::channel();
    let (resume, resume_rx) = std::sync::mpsc::channel::<()>();
    let reached_tx = Mutex::new(Some(reached_tx));
    let resume_rx = Mutex::new(resume_rx);
    let seen = AtomicUsize::new(0);
    let hook: FaultHook = Arc::new(move |p| {
        if p == phase && seen.fetch_add(1, Ordering::SeqCst) + 1 == nth {
            if let Some(tx) = reached_tx.lock().unwrap().take() {
                let _ = tx.send(());
            }
            tokio::task::block_in_place(|| resume_rx.lock().unwrap().recv().unwrap());
            if crash_after {
                return Err(munarium_core::KernelError::Storage(format!(
                    "injected crash at {p:?}"
                )));
            }
        }
        Ok(())
    });
    (hook, Pause { reached, resume })
}

fn pause_at(phase: BuildPhase) -> (FaultHook, Pause) {
    pause_at_nth(phase, 1, false)
}

fn crash_at(phase: BuildPhase) -> FaultHook {
    let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
    Arc::new(move |p| {
        if p == phase && !fired.swap(true, Ordering::SeqCst) {
            return Err(munarium_core::KernelError::Storage(format!(
                "injected crash at {p:?}"
            )));
        }
        Ok(())
    })
}

fn spawn_rebuild(
    f: &Fixture,
    ctx: MirrorContext,
) -> tokio::task::JoinHandle<munarium_core::Result<MirrorOutcome>> {
    let pg = f.pg.clone();
    let collection_id = f.collection_id.clone();
    let version = f.version().to_string();
    tokio::spawn(async move {
        backfill_one(
            &ctx,
            &pg,
            MirrorTarget::Collection {
                collection_id: &collection_id,
            },
            &version,
        )
        .await
    })
}

/// R6-T1 row 2, with the two-job case. Cancel wins after claim and before
/// the manifest intent. The v1.4 response is `ok: true` and `cancelled`; the
/// target is re-enqueued at once and claimed by another builder, which
/// defers while the old attempt is live and publishes once the old worker
/// stops at its next boundary. The old generation publishes nothing, cleans
/// only its own staging, and cannot renew or complete; its history shows the
/// selected version with no manifest.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_before_the_intent_stops_the_worker_and_frees_the_target() {
    guard!();
    let f = fixture("before-intent", 1).await;
    let gen1 = f.seed_claimed_job("rebuild", false, NODE_A).await;
    let signal = JobSignal::new();

    let (hook, pause) = pause_at(BuildPhase::AfterSeal);
    let old = spawn_rebuild(&f, f.ctx(NODE_A, Some(&gen1), Some(&signal), Some(hook)));
    pause.reached.await.unwrap();
    let old_attempt = f.linked_attempts(&gen1).await.pop().expect("linked");

    assert_eq!(f.cancel(&gen1).await, (true, "cancelled".to_string()));
    let job2 = f.reenqueue_and_claim(JobKind::Rebuild, NODE_B).await;

    // The replacement defers while the old attempt holds the plan; it does
    // not wait for an acknowledgment from the old worker.
    let deferred = backfill_one(
        &f.ctx(NODE_B, Some(&job2), None, None),
        &f.pg,
        f.target(),
        f.version(),
    )
    .await
    .unwrap();
    assert!(
        matches!(deferred, MirrorOutcome::AlreadyRunning { .. }),
        "{deferred:?}"
    );

    pause.resume.send(()).unwrap();
    let err = old.await.unwrap().unwrap_err();
    assert!(err.to_string().contains(STALE_GENERATION_MESSAGE), "{err}");
    assert_eq!(
        signal.stopped(),
        Some(StopReason::Cancelled),
        "the worker-local signal stopped"
    );

    let attempts = BuildAttempts::new(f.pool.clone(), &f.tenant);
    assert!(attempts
        .publication_intent(&old_attempt)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        attempts.get(&old_attempt).await.unwrap().unwrap().state,
        AttemptState::Failed
    );
    assert!(
        !f.staging.path().join(&old_attempt).exists(),
        "the stopped attempt's own staging is cleaned"
    );
    f.assert_stale_refused(&gen1).await;

    let published = backfill_one(
        &f.ctx(NODE_B, Some(&job2), None, None),
        &f.pg,
        f.target(),
        f.version(),
    )
    .await
    .unwrap();
    let MirrorOutcome::Published {
        artifact_id,
        bound_staged,
        ..
    } = published
    else {
        panic!("the replacement publishes: {published:?}");
    };
    assert!(bound_staged);

    let h1 = f.history(&gen1).await;
    assert_eq!(
        (h1.history, h1.job.state.as_str()),
        (HistoryStatus::Recorded, "cancelled")
    );
    assert_eq!(h1.versions.len(), 1);
    let v = &h1.versions[0];
    assert_eq!(v.index_version_id, f.version());
    assert_eq!(v.selected_ordinal, Some(0));
    assert_eq!(
        (v.attempt_count, v.attempt_state.as_deref()),
        (1, Some("failed"))
    );
    assert_eq!(
        v.intent_state, None,
        "no manifest from the cancelled generation"
    );
    assert!(!v.staged_bound);

    f.finish(&job2).await;
    let h2 = f.history(&job2).await;
    let v = &h2.versions[0];
    assert_eq!(v.intent_state.as_deref(), Some("published"));
    assert_eq!(v.artifact_id.as_deref(), Some(artifact_id.as_str()));
    assert_eq!(v.artifact_state.as_deref(), Some("verified"));
    assert!(v.staged_bound);
    assert_eq!(
        v.disposition.as_deref(),
        Some("deferred"),
        "the earlier deferral is kept"
    );
}

/// R6-T1 row 3. The intent wins, then cancel wins before completion. The old
/// worker stops at the boundary before upload with its attempt sealed and its
/// intent pending, so the history reports the manifest as unknown. The
/// sealed-attempt reconciler then finishes exactly that object; the history
/// records `published`, the artifact is verified, nothing is bound by the
/// cancelled generation, and the job stays `cancelled` throughout.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_after_the_intent_lets_only_the_recorded_object_finish() {
    guard!();
    let f = fixture("after-intent", 1).await;
    let gen1 = f.seed_claimed_job("rebuild", false, NODE_A).await;

    let (hook, pause) = pause_at(BuildPhase::AfterCatalogInsert);
    let old = spawn_rebuild(&f, f.ctx(NODE_A, Some(&gen1), None, Some(hook)));
    pause.reached.await.unwrap();
    let attempt = f.linked_attempts(&gen1).await.pop().expect("linked");
    let attempts = BuildAttempts::new(f.pool.clone(), &f.tenant);
    let intent = attempts
        .publication_intent(&attempt)
        .await
        .unwrap()
        .expect("intent");

    assert_eq!(f.cancel(&gen1).await, (true, "cancelled".to_string()));
    pause.resume.send(()).unwrap();
    let err = old.await.unwrap().unwrap_err();
    assert!(err.to_string().contains(STALE_GENERATION_MESSAGE), "{err}");
    assert!(
        f.object_at(&intent.manifest_key).is_none(),
        "stopped before upload"
    );

    let v = f.history(&gen1).await.versions.pop().unwrap();
    assert_eq!(
        (
            v.intent_state.as_deref(),
            v.artifact_state.as_deref(),
            v.attempt_state.as_deref()
        ),
        (Some("pending"), Some("sealed"), Some("sealed")),
        "unresolved: the manifest outcome is unknown"
    );

    let report = reconcile_attempts(&f.ctx(NODE_A, None, None, None))
        .await
        .unwrap();
    assert_eq!((report.resumed, report.abandoned), (1, 0), "{report:?}");
    let stored = f
        .object_at(&intent.manifest_key)
        .expect("the recorded object");
    assert_eq!(sha256_hex(&stored), intent.manifest_sha256);

    let h = f.history(&gen1).await;
    assert_eq!(h.job.state, "cancelled", "the job keeps its terminal state");
    let v = &h.versions[0];
    assert_eq!(v.intent_state.as_deref(), Some("published"));
    assert_eq!(v.artifact_state.as_deref(), Some("verified"));
    assert!(!v.staged_bound, "a cancelled generation never binds");
    let catalog = ArtifactCatalog::new(f.pool.clone(), &f.tenant);
    assert!(catalog
        .binding(f.version(), BindingSlot::Staged)
        .await
        .unwrap()
        .is_none());
    // A repeated cancel creates no new publication or recovery operation.
    assert_eq!(f.cancel(&gen1).await, (false, "cancelled".to_string()));
    let again = reconcile_attempts(&f.ctx(NODE_A, None, None, None))
        .await
        .unwrap();
    assert_eq!((again.resumed, again.abandoned), (0, 0));
}

/// R6-T1 row 3, lost acknowledgment. The manifest reaches storage but the
/// worker dies before recording it. Until reconciliation the history says
/// unknown, never published; the reconciler verifies the exact object and
/// only then records `published`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lost_manifest_acknowledgment_stays_unknown_until_verified() {
    guard!();
    let f = fixture("lost-ack", 1).await;
    let gen1 = f.seed_claimed_job("rebuild", false, NODE_A).await;
    backfill_one(
        &f.ctx(
            NODE_A,
            Some(&gen1),
            None,
            Some(crash_at(BuildPhase::AfterManifestWrite)),
        ),
        &f.pg,
        f.target(),
        f.version(),
    )
    .await
    .unwrap_err();
    let attempt = f.linked_attempts(&gen1).await.pop().expect("linked");
    let intent = BuildAttempts::new(f.pool.clone(), &f.tenant)
        .publication_intent(&attempt)
        .await
        .unwrap()
        .unwrap();
    assert!(
        f.object_at(&intent.manifest_key).is_some(),
        "the write landed"
    );
    assert_eq!(
        intent.state,
        IntentState::Pending,
        "but was never acknowledged"
    );
    assert_eq!(f.cancel(&gen1).await, (true, "cancelled".to_string()));
    let v = f.history(&gen1).await.versions.pop().unwrap();
    assert_eq!(v.intent_state.as_deref(), Some("pending"));

    let report = reconcile_attempts(&f.ctx(NODE_A, None, None, None))
        .await
        .unwrap();
    assert_eq!(report.resumed, 1, "{report:?}");
    let v = f.history(&gen1).await.versions.pop().unwrap();
    assert_eq!(v.intent_state.as_deref(), Some("published"));
    assert_eq!(v.artifact_state.as_deref(), Some("verified"));
}

/// R6-T1 row 4. Completion wins before cancel: `ok: false`, the terminal
/// state stays, and the history keeps the recorded outcome.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_after_completion_keeps_the_recorded_outcome() {
    guard!();
    let f = fixture("after-complete", 1).await;
    let gen1 = f.seed_claimed_job("rebuild", false, NODE_A).await;
    let outcome = backfill_one(
        &f.ctx(NODE_A, Some(&gen1), None, None),
        &f.pg,
        f.target(),
        f.version(),
    )
    .await
    .unwrap();
    assert!(matches!(outcome, MirrorOutcome::Published { .. }));
    // ClearView R9: the holder recorded its last boundary as progress, and
    // the trigger recorded the phase changes as events.
    let p = f
        .jobs()
        .progress(&f.tenant, &JobId::parse(&gen1.job_id).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p.phase.as_deref(), Some("uploading"));
    f.finish(&gen1).await;
    let page = f
        .jobs()
        .events(&f.tenant, &JobId::parse(&gen1.job_id).unwrap(), 0, 50)
        .await
        .unwrap()
        .unwrap();
    let kinds: Vec<_> = page
        .events
        .iter()
        .map(|e| (e.kind.as_str(), e.phase.clone()))
        .collect();
    assert!(!page.gap);
    assert_eq!(
        kinds,
        vec![
            ("enqueued", None),
            ("claimed", None),
            ("progress", Some("sealing".to_string())),
            ("progress", Some("publishing".to_string())),
            ("progress", Some("uploading".to_string())),
            ("state", Some("uploading".to_string())),
        ]
    );
    let before = f.history(&gen1).await;
    assert_eq!(f.cancel(&gen1).await, (false, "succeeded".to_string()));
    let after = f.history(&gen1).await;
    assert_eq!(before, after);
    assert_eq!(after.versions[0].intent_state.as_deref(), Some("published"));
}

/// A cancelled backfill: the first version is published, the second is
/// selected and untouched. The cancel commits while the first version waits
/// to bind, so the cancelled generation leaves it unbound; the boundary
/// between versions then stops the backfill before the second starts. The
/// set was retained before the first version started, so the history can say
/// which version was never reached.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancelled_backfill_reports_a_published_first_and_an_untouched_second_version() {
    guard!();
    let f = fixture("backfill", 2).await;
    let gen1 = f.seed_claimed_job("backfill", true, NODE_A).await;

    let (hook, pause) = pause_at(BuildPhase::BeforeBinding);
    let ctx = f.ctx(NODE_A, Some(&gen1), None, Some(hook));
    let pg = f.pg.clone();
    let collection_id = f.collection_id.clone();
    let old = tokio::spawn(async move {
        backfill_collection(
            &ctx,
            &pg,
            &collection_id,
            RequiredVersionsPolicy::ActivePinnedAndHorizon,
            86_400,
        )
        .await
    });
    pause.reached.await.unwrap();
    assert_eq!(f.cancel(&gen1).await, (true, "cancelled".to_string()));
    pause.resume.send(()).unwrap();
    let err = old.await.unwrap().unwrap_err();
    assert!(
        err.to_string().contains(STALE_GENERATION_MESSAGE),
        "the backfill stops at the boundary between versions: {err}"
    );

    let h = f.history(&gen1).await;
    assert_eq!(h.history, HistoryStatus::Recorded);
    assert_eq!(h.versions.len(), 2, "both selected versions are listed");
    let first = h
        .versions
        .iter()
        .find(|v| v.selected_ordinal == Some(0))
        .unwrap();
    let second = h
        .versions
        .iter()
        .find(|v| v.selected_ordinal == Some(1))
        .unwrap();
    assert_eq!(
        (
            first.intent_state.as_deref(),
            first.artifact_state.as_deref()
        ),
        (Some("published"), Some("verified"))
    );
    assert!(
        !first.staged_bound,
        "the cancelled generation does not bind"
    );
    assert_eq!(
        (
            second.attempt_count,
            second.intent_state.as_deref(),
            second.disposition.as_deref()
        ),
        (0, None, None),
        "the second version is untouched: no attempt, no intent"
    );
    // Neither case activates an index.
    for v in &f.versions {
        assert!(ArtifactCatalog::new(f.pool.clone(), &f.tenant)
            .binding(v, BindingSlot::Serving)
            .await
            .unwrap()
            .is_none());
    }
}

/// A direct build cancelled after its PostgreSQL commit and before any
/// artifact attempt exists. The history reports the committed logical
/// version and its generation, and no attempt or manifest.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_direct_build_cancelled_after_its_commit_reports_the_version_only() {
    guard!();
    let f = fixture("direct", 0).await;
    let gen1 = f.seed_claimed_job("direct", true, NODE_A).await;
    let (hook, pause) = pause_at(BuildPhase::AfterLogicalCommit);
    let ctx = f.ctx(NODE_A, Some(&gen1), None, Some(hook));
    let pg = f.pg.clone();
    let collection_id = f.collection_id.clone();
    let old =
        tokio::spawn(
            async move { build_collection_direct(&ctx, &pg, &collection_id, 400, 3).await },
        );
    pause.reached.await.unwrap();
    assert_eq!(f.cancel(&gen1).await, (true, "cancelled".to_string()));
    pause.resume.send(()).unwrap();
    let err = old.await.unwrap().unwrap_err();
    assert!(err.to_string().contains(STALE_GENERATION_MESSAGE), "{err}");

    let h = f.history(&gen1).await;
    assert_eq!(h.versions.len(), 1);
    let v = &h.versions[0];
    assert_eq!(v.committed_generation, Some(gen1.generation));
    assert_eq!((v.attempt_count, v.intent_state.as_deref()), (0, None));
    assert!(
        f.linked_attempts(&gen1).await.is_empty(),
        "no artifact attempt"
    );
    let attempts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM index_build_attempts WHERE tenant_id = $1")
            .bind(&f.tenant)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(attempts, 0);
}

/// A direct build stops between sources: a cancel that commits while the
/// first source is extracted stops it before the second, and nothing is
/// committed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_direct_build_stops_between_sources() {
    guard!();
    let f = fixture("direct-sources", 0).await;
    let gen1 = f.seed_claimed_job("direct", true, NODE_A).await;
    let signal = JobSignal::new();
    let jobs = f.jobs();
    let tenant = f.tenant.clone();
    let job_id = gen1.job_id.clone();
    // Cancel as soon as the build passes its first source boundary, by
    // stopping the worker-local signal the way the lease watch does.
    let seen = Arc::new(AtomicUsize::new(0));
    let s2 = signal.clone();
    // The hook an R3 child-process runner registers to kill its child.
    let killed = Arc::new(Mutex::new(None));
    let k = Arc::clone(&killed);
    signal.on_stop(move |reason| *k.lock().unwrap() = Some(reason));
    let ctx = f.ctx(NODE_A, Some(&gen1), Some(&signal), None);
    let counter = Arc::clone(&seen);
    let err =
        f.pg.extract_collection_prepared_with(&f.collection_id, 400, |done, _total| {
            let ctx = &ctx;
            let counter = Arc::clone(&counter);
            let jobs = jobs.clone();
            let tenant = tenant.clone();
            let job_id = job_id.clone();
            let s2 = s2.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                if done == 1 {
                    assert!(jobs.cancel(&tenant, &job_id).await.unwrap());
                }
                ctx.checkpoint(ProgressPhase::Extracting, Some(done as u64), None)
                    .await?;
                assert!(s2.stopped().is_none());
                Ok(())
            }
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains(STALE_GENERATION_MESSAGE), "{err}");
    assert_eq!(
        seen.load(Ordering::SeqCst),
        2,
        "stopped at the second source's boundary"
    );
    assert_eq!(signal.stopped(), Some(StopReason::Cancelled));
    assert_eq!(
        *killed.lock().unwrap(),
        Some(StopReason::Cancelled),
        "stop hooks run"
    );
}

/// An uncertain lease is revalidated, not trusted. With the lease marked
/// uncertain and the job reclaimed under the SAME node name, the old worker's
/// next boundary finds its generation retired and stops; the replacement
/// proceeds without any acknowledgment from it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_uncertain_lease_is_revalidated_before_the_next_write() {
    guard!();
    let f = fixture("uncertain", 1).await;
    let gen1 = f.seed_claimed_job("rebuild", false, NODE_A).await;
    let signal = JobSignal::new();
    signal.set_uncertain(true);

    let (hook, pause) = pause_at(BuildPhase::AfterExport);
    let old = spawn_rebuild(&f, f.ctx(NODE_A, Some(&gen1), Some(&signal), Some(hook)));
    pause.reached.await.unwrap();
    let gen2 = f.claim_row(&gen1.job_id, NODE_A).await;
    assert_eq!(gen2.generation, gen1.generation + 1);
    pause.resume.send(()).unwrap();
    let err = old.await.unwrap().unwrap_err();
    assert!(err.to_string().contains(STALE_GENERATION_MESSAGE), "{err}");
    assert_eq!(signal.stopped(), Some(StopReason::LeaseLost));
    f.assert_stale_refused(&gen1).await;

    let outcome = backfill_one(
        &f.ctx(NODE_A, Some(&gen2), None, None),
        &f.pg,
        f.target(),
        f.version(),
    )
    .await
    .unwrap();
    assert!(
        matches!(
            outcome,
            MirrorOutcome::Published {
                bound_staged: true,
                ..
            }
        ),
        "{outcome:?}"
    );
    f.finish(&gen2).await;
    let h = f.history(&gen1).await;
    assert_eq!(h.history, HistoryStatus::Recorded);
    let v = &h.versions[0];
    assert_eq!(
        (
            v.attempt_count,
            v.attempt_generation,
            v.intent_state.as_deref()
        ),
        (2, Some(gen2.generation), Some("published")),
        "the newest attempt is the replacement's"
    );
}
