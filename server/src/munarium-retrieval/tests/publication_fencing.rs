// SPDX-License-Identifier: Apache-2.0
//! ClearView R4 publication fencing (R4-T1), against a real PostgreSQL and a
//! real local-file artifact store.
//!
//! Each case runs a QUEUED build under a job claim, pauses or crashes it at a
//! named `BuildPhase`, reclaims the job under the SAME node name (a new
//! generation), and then lets the old execution and the sealed-attempt
//! reconciler continue. The claims under test are about which writes a
//! retired generation can still make, so nothing is mocked.
//!
//! The job rows are seeded and reclaimed with tenant-scoped SQL that mirrors
//! `claim_any`'s update, never with the global `claim_any`: that would take
//! other tests' queued jobs. Every seeded row has `claimed_at = 'infinity'`,
//! so no claimer or expired-claim cleaner anywhere can take it, even if a test
//! panics before it completes its job. Each test completes its job before it
//! returns. Skips loudly when `MUNARIUM_TEST_DATABASE_URL` is unset.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use munarium_retrieval::backfill::backfill_one;
use munarium_retrieval::direct::build_collection_direct;
use munarium_retrieval::mirror::{
    reconcile_attempts, BuildPhase, FaultHook, LocalStoreFactory, MirrorContext, MirrorOutcome,
    MirrorTarget, VectorPolicy,
};
use munarium_retrieval_pg::PgRetrieval;
use munarium_store_pg::artifacts::{ArtifactCatalog, ArtifactState, BindingSlot};
use munarium_store_pg::attempts::{AttemptState, BuildAttempts, IntentState};
use munarium_store_pg::jobs::{BuildJobs, JobFence, STALE_GENERATION_MESSAGE};
use munarium_store_pg::PgStore;
use sqlx::Row as _;

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

/// The node name both executions share: a restarted StatefulSet pod, or a
/// fixed `MUNARIUM_INSTANCE_ID`. Identity alone cannot tell them apart.
const NODE: &str = "builder-same";

struct Fixture {
    pg: PgRetrieval,
    pool: sqlx::PgPool,
    tenant: String,
    collection_id: String,
    /// Empty for a direct-build fixture, which has no version until it builds.
    version_id: String,
    artifacts: tempfile::TempDir,
    staging: tempfile::TempDir,
}

async fn fixture(tag: &str, docs: usize, build_pg: bool) -> Fixture {
    let tenant = format!("tenant-r4pub-{tag}-{}", uuid::Uuid::new_v4().simple());
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
            Some("R4 publication fencing"),
        )
        .await
        .unwrap();
    for i in 0..docs {
        let body = format!(
            "Fenced document {i}. The quartermaster counted barrels of flour at Valley Forge.\n\n\
             A second paragraph about the winter encampment and its supply roads."
        );
        let (source_id, _, _) = pg
            .put_source(
                "",
                "text/markdown",
                &format!("fenced/doc-{i}.md"),
                Some("para"),
                body.as_bytes(),
            )
            .await
            .unwrap();
        pg.bind_source(&col.id, &source_id, None).await.unwrap();
    }
    let version_id = if build_pg {
        pg.build_collection_index(&col.id, 400, 1, true)
            .await
            .unwrap()
            .id
    } else {
        String::new()
    };
    Fixture {
        pg,
        pool: store.pool().clone(),
        tenant,
        collection_id: col.id.clone(),
        version_id,
        artifacts: tempfile::tempdir().unwrap(),
        staging: tempfile::tempdir().unwrap(),
    }
}

impl Fixture {
    fn ctx(&self, fence: Option<&JobFence>, faults: Option<FaultHook>) -> MirrorContext {
        MirrorContext {
            catalog: ArtifactCatalog::new(self.pool.clone(), &self.tenant),
            attempts: BuildAttempts::new(self.pool.clone(), &self.tenant),
            stores: Arc::new(LocalStoreFactory::new(self.artifacts.path())),
            node_id: NODE.into(),
            staging_root: self.staging.path().to_path_buf(),
            artifact_prefix: "v1".into(),
            tenant_path_hash: "t0000".into(),
            faults,
            observer: None,
            vector_policy: VectorPolicy {
                approx_threshold: None,
            },
            job_fence: fence.cloned(),
            job_signal: None,
        }
    }

    fn jobs(&self) -> BuildJobs {
        BuildJobs::new(self.pool.clone())
    }

    /// Seed a job already claimed once by `NODE` (generation 1).
    async fn seed_claimed_job(&self, kind: &str, scope: bool) -> JobFence {
        let job_id = format!("bjob-r4pub-{}", uuid::Uuid::new_v4().simple());
        let (scope_kind, scope_id, version) = if scope {
            (Some("collection"), Some(self.collection_id.as_str()), None)
        } else {
            (None, None, Some(self.version_id.as_str()))
        };
        sqlx::query(
            "INSERT INTO index_build_jobs
                 (tenant_id, job_id, kind, scope_kind, scope_id, index_version_id,
                  params, state, claimed_by, claimed_at, attempts, requested_by)
             VALUES ($1,$2,$3,$4,$5,$6,'{}'::jsonb,'running',$7,'infinity',1,'test-r4pub')",
        )
        .bind(&self.tenant)
        .bind(&job_id)
        .bind(kind)
        .bind(scope_kind)
        .bind(scope_id)
        .bind(version)
        .bind(NODE)
        .execute(&self.pool)
        .await
        .unwrap();
        JobFence {
            tenant_id: self.tenant.clone(),
            job_id,
            node_id: NODE.into(),
            generation: 1,
        }
    }

    /// Reclaim the job under the same node name, as `claim_any` would after
    /// the lease lapsed: `attempts + 1` is the new generation.
    async fn reclaim(&self, old: &JobFence) -> JobFence {
        let generation: i32 = sqlx::query_scalar(
            "UPDATE index_build_jobs
                SET state = 'running', claimed_by = $3, claimed_at = 'infinity',
                    attempts = attempts + 1, updated_at = now()
              WHERE tenant_id = $1 AND job_id = $2
          RETURNING attempts",
        )
        .bind(&self.tenant)
        .bind(&old.job_id)
        .bind(NODE)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        assert_eq!(generation, old.generation + 1);
        JobFence {
            generation,
            ..old.clone()
        }
    }

    /// Neither stale path can renew the lease or complete the job, and the
    /// row still belongs to the current generation.
    async fn assert_stale_writes_refused(&self, stale: &JobFence, current: &JobFence) {
        let jobs = self.jobs();
        assert!(
            !jobs
                .heartbeat_fenced(&self.tenant, &stale.job_id, NODE, stale.generation)
                .await
                .unwrap(),
            "a retired generation cannot renew the job lease"
        );
        assert!(
            !jobs
                .complete_fenced(
                    &self.tenant,
                    &stale.job_id,
                    NODE,
                    stale.generation,
                    Ok(serde_json::json!({"stale": true})),
                    None,
                )
                .await
                .unwrap(),
            "a retired generation cannot complete the job"
        );
        let job = jobs
            .get(&self.tenant, &stale.job_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (job.state.as_str(), job.attempts, job.result),
            ("running", current.generation, None)
        );
    }

    /// Complete the job under its current generation, so no running row is
    /// left behind.
    async fn finish(&self, current: &JobFence) {
        assert!(self
            .jobs()
            .complete_fenced(
                &self.tenant,
                &current.job_id,
                NODE,
                current.generation,
                Ok(serde_json::json!({})),
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

    /// Attempts linked to a generation of a job, oldest first.
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

    /// `(artifact_id, attempt_id, state)` for every catalog row of a version.
    async fn catalog_rows(&self, version: &str) -> Vec<(String, Option<String>, String)> {
        sqlx::query(
            "SELECT artifact_id, attempt_id, state FROM index_artifacts
              WHERE tenant_id = $1 AND index_version_id = $2
              ORDER BY artifact_id",
        )
        .bind(&self.tenant)
        .bind(version)
        .fetch_all(&self.pool)
        .await
        .unwrap()
        .into_iter()
        .map(|r| (r.get(0), r.get(1), r.get(2)))
        .collect()
    }

    async fn binding_events(&self, version: &str) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM index_artifact_binding_events
              WHERE tenant_id = $1 AND index_version_id = $2",
        )
        .bind(&self.tenant)
        .bind(version)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    /// The bytes stored at a full object key under the artifact root.
    fn object_at(&self, key: &str) -> Option<Vec<u8>> {
        let path = self.artifacts.path().join(key);
        std::fs::read(path).ok()
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    hex::encode(sha2::Sha256::digest(bytes))
}

/// A hook that PAUSES the build the first time it reaches `phase`: it reports
/// arrival, blocks until resumed, and then either continues or fails as an
/// injected crash. Blocking needs the multi-thread runtime.
struct Pause {
    reached: tokio::sync::oneshot::Receiver<()>,
    resume: std::sync::mpsc::Sender<()>,
}

fn pause_at(phase: BuildPhase, crash_after: bool) -> (FaultHook, Pause) {
    let (reached_tx, reached) = tokio::sync::oneshot::channel();
    let (resume, resume_rx) = std::sync::mpsc::channel::<()>();
    let reached_tx = Mutex::new(Some(reached_tx));
    let resume_rx = Mutex::new(resume_rx);
    let fired = AtomicBool::new(false);
    let hook: FaultHook = Arc::new(move |p| {
        if p == phase && !fired.swap(true, Ordering::SeqCst) {
            if let Some(tx) = reached_tx.lock().unwrap().take() {
                let _ = tx.send(());
            }
            // `block_in_place` hands this worker's queued tasks (a pool
            // connection being returned, a timer) to another thread while
            // the build is paused; a bare blocking `recv` strands them.
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

fn crash_at(phase: BuildPhase) -> FaultHook {
    let fired = Arc::new(AtomicBool::new(false));
    Arc::new(move |p| {
        if p == phase && !fired.swap(true, Ordering::SeqCst) {
            return Err(munarium_core::KernelError::Storage(format!(
                "injected crash at {p:?}"
            )));
        }
        Ok(())
    })
}

/// Run a fenced mirror of the fixture's version on its own task.
fn spawn_mirror(
    f: &Fixture,
    ctx: MirrorContext,
) -> tokio::task::JoinHandle<munarium_core::Result<MirrorOutcome>> {
    let pg = f.pg.clone();
    let collection_id = f.collection_id.clone();
    let version = f.version_id.clone();
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

/// R4-T1, first case. The old execution is paused before it takes the
/// publication lock; the job is reclaimed under the same node name; the old
/// execution and the reconciler then resume. The retired generation publishes
/// nothing: no intent, no catalog row, no object, no binding. The replacement
/// then builds under a distinct attempt id and owns the catalog and binding.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_generation_retired_before_the_publication_lock_publishes_nothing() {
    guard!();
    let f = fixture("before-lock", 2, true).await;
    let gen1 = f.seed_claimed_job("rebuild", false).await;

    let (hook, pause) = pause_at(BuildPhase::AfterSeal, false);
    let old = spawn_mirror(&f, f.ctx(Some(&gen1), Some(hook)));
    pause.reached.await.unwrap();
    let old_attempts = f.linked_attempts(&gen1).await;
    assert_eq!(
        old_attempts.len(),
        1,
        "the attempt is linked to its claim before anything is published"
    );
    let old_attempt = old_attempts[0].clone();

    let gen2 = f.reclaim(&gen1).await;
    pause.resume.send(()).unwrap();
    let err = old.await.unwrap().unwrap_err();
    assert!(
        err.to_string().contains(STALE_GENERATION_MESSAGE),
        "the old execution is refused at the publication lock: {err}"
    );

    // Its sealed-attempt reconciler finds nothing to finish.
    let report = reconcile_attempts(&f.ctx(None, None)).await.unwrap();
    assert_eq!((report.resumed, report.abandoned), (0, 0), "{report:?}");

    let attempts = BuildAttempts::new(f.pool.clone(), &f.tenant);
    assert!(
        attempts
            .publication_intent(&old_attempt)
            .await
            .unwrap()
            .is_none(),
        "no publication intent was committed for the retired generation"
    );
    assert_eq!(
        attempts.get(&old_attempt).await.unwrap().unwrap().state,
        AttemptState::Failed
    );
    assert!(
        f.catalog_rows(&f.version_id).await.is_empty(),
        "zero old-generation catalog rows"
    );
    let catalog = ArtifactCatalog::new(f.pool.clone(), &f.tenant);
    assert!(catalog
        .binding(&f.version_id, BindingSlot::Staged)
        .await
        .unwrap()
        .is_none());
    assert_eq!(f.binding_events(&f.version_id).await, 0);
    assert!(
        !f.staging.path().join(&old_attempt).exists(),
        "the refused attempt's staging is cleaned"
    );
    f.assert_stale_writes_refused(&gen1, &gen2).await;

    // The replacement: a distinct attempt id, and it owns catalog and binding.
    let outcome = backfill_one(&f.ctx(Some(&gen2), None), &f.pg, f.target(), &f.version_id)
        .await
        .unwrap();
    let MirrorOutcome::Published {
        artifact_id,
        bound_staged,
        ..
    } = outcome
    else {
        panic!("the replacement publishes: {outcome:?}");
    };
    assert!(bound_staged);
    let new_attempts = f.linked_attempts(&gen2).await;
    assert_eq!(new_attempts.len(), 1);
    assert_ne!(
        new_attempts[0], old_attempt,
        "a distinct artifact attempt id"
    );
    assert_eq!(
        f.catalog_rows(&f.version_id).await,
        vec![(
            artifact_id.clone(),
            Some(new_attempts[0].clone()),
            "verified".to_string()
        )],
        "the only catalog row is the replacement's"
    );
    let intent = attempts
        .publication_intent(&new_attempts[0])
        .await
        .unwrap()
        .expect("the replacement committed an intent");
    assert_eq!(intent.state, IntentState::Published);
    assert_eq!(intent.job_generation, gen2.generation);
    let stored = f.object_at(&intent.manifest_key).expect("manifest at key");
    assert_eq!(sha256_hex(&stored), intent.manifest_sha256);
    assert_eq!(intent.manifest_sha256, artifact_id);

    f.assert_stale_writes_refused(&gen1, &gen2).await;
    f.finish(&gen2).await;
}

/// R4-T1, second case. The intent is committed before reclaim; the old
/// execution is paused during its upload, the job is reclaimed, and the old
/// execution resumes and finishes. It publishes exactly the recorded
/// manifest key and digest, records the result, and never binds. The
/// replacement defers while the sealed attempt is live, then reuses the
/// verified artifact and binds it under the current generation.
///
/// The pause is inside the upload, after the component writes. ClearView R6
/// added a safe boundary between the catalog insert and the upload: an
/// execution retired before that boundary stops there and leaves its intent
/// to the sealed-attempt reconciler (the third case below, and R6's
/// `cancel_after_the_intent_lets_only_the_recorded_object_finish`). One
/// retired during the upload finishes it, which is what this case covers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_intent_committed_before_reclaim_finishes_only_its_manifest() {
    guard!();
    let f = fixture("intent-worker", 2, true).await;
    let gen1 = f.seed_claimed_job("rebuild", false).await;

    let (hook, pause) = pause_at(BuildPhase::AfterComponentUpload, false);
    let old = spawn_mirror(&f, f.ctx(Some(&gen1), Some(hook)));
    pause.reached.await.unwrap();
    let old_attempt = f.linked_attempts(&gen1).await.pop().expect("linked");
    let attempts = BuildAttempts::new(f.pool.clone(), &f.tenant);
    let intent = attempts
        .publication_intent(&old_attempt)
        .await
        .unwrap()
        .expect("the intent is committed before the catalog insert");
    assert!(
        f.object_at(&intent.manifest_key).is_none(),
        "paused before the manifest write"
    );
    assert_eq!(intent.state, IntentState::Pending);
    assert_eq!(intent.job_generation, gen1.generation);

    let gen2 = f.reclaim(&gen1).await;
    // While the old sealed attempt is live, the replacement defers.
    let deferred = backfill_one(&f.ctx(Some(&gen2), None), &f.pg, f.target(), &f.version_id)
        .await
        .unwrap();
    assert!(
        matches!(deferred, MirrorOutcome::AlreadyRunning { .. }),
        "{deferred:?}"
    );

    pause.resume.send(()).unwrap();
    let finished = old.await.unwrap().unwrap();
    match &finished {
        MirrorOutcome::Published {
            artifact_id,
            bound_staged,
            ..
        } => {
            assert_eq!(artifact_id, &intent.artifact_id);
            assert!(
                !bound_staged,
                "the retired generation finishes its intent and does not bind"
            );
        }
        other => panic!("the intent finishes: {other:?}"),
    }
    let resolved = attempts
        .publication_intent(&old_attempt)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved.state, IntentState::Published);
    assert_eq!(
        (&resolved.manifest_key, &resolved.manifest_sha256),
        (&intent.manifest_key, &intent.manifest_sha256),
        "an intent is immutable"
    );
    let stored = f.object_at(&intent.manifest_key).expect("manifest at key");
    assert_eq!(sha256_hex(&stored), intent.manifest_sha256);
    assert_eq!(
        f.catalog_rows(&f.version_id).await,
        vec![(
            intent.artifact_id.clone(),
            Some(old_attempt.clone()),
            "verified".to_string()
        )],
        "exactly the intent's artifact, and nothing else"
    );
    let catalog = ArtifactCatalog::new(f.pool.clone(), &f.tenant);
    assert!(
        catalog
            .binding(&f.version_id, BindingSlot::Staged)
            .await
            .unwrap()
            .is_none(),
        "a retired generation never binds"
    );
    assert_eq!(f.binding_events(&f.version_id).await, 0);
    f.assert_stale_writes_refused(&gen1, &gen2).await;

    // The current generation reuses the artifact and binds it.
    let reused = backfill_one(&f.ctx(Some(&gen2), None), &f.pg, f.target(), &f.version_id)
        .await
        .unwrap();
    assert_eq!(
        reused,
        MirrorOutcome::AlreadyBuilt {
            artifact_id: intent.artifact_id.clone()
        }
    );
    assert_eq!(
        catalog
            .binding(&f.version_id, BindingSlot::Staged)
            .await
            .unwrap()
            .expect("bound by the current generation")
            .artifact_id,
        intent.artifact_id
    );
    f.finish(&gen2).await;
}

/// R4-T1, the reconciler path. The old execution crashes after its intent and
/// catalog insert; the job is reclaimed; the sealed-attempt reconciler
/// finishes exactly that intent, records the result, and does not bind.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_reconciler_finishes_a_retired_intent_and_never_binds() {
    guard!();
    let f = fixture("intent-reconciler", 2, true).await;
    let gen1 = f.seed_claimed_job("rebuild", false).await;

    let err = backfill_one(
        &f.ctx(Some(&gen1), Some(crash_at(BuildPhase::AfterCatalogInsert))),
        &f.pg,
        f.target(),
        &f.version_id,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("injected"), "{err}");
    let old_attempt = f.linked_attempts(&gen1).await.pop().expect("linked");
    let attempts = BuildAttempts::new(f.pool.clone(), &f.tenant);
    let intent = attempts
        .publication_intent(&old_attempt)
        .await
        .unwrap()
        .expect("intent");
    assert!(
        f.object_at(&intent.manifest_key).is_none(),
        "nothing was published before the crash"
    );

    let gen2 = f.reclaim(&gen1).await;
    let report = reconcile_attempts(&f.ctx(None, None)).await.unwrap();
    assert_eq!((report.resumed, report.abandoned), (1, 0), "{report:?}");

    let resolved = attempts
        .publication_intent(&old_attempt)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved.state, IntentState::Published);
    let stored = f.object_at(&intent.manifest_key).expect("manifest at key");
    assert_eq!(sha256_hex(&stored), intent.manifest_sha256);
    assert_eq!(
        f.catalog_rows(&f.version_id).await,
        vec![(
            intent.artifact_id.clone(),
            Some(old_attempt.clone()),
            "verified".to_string()
        )]
    );
    let catalog = ArtifactCatalog::new(f.pool.clone(), &f.tenant);
    assert!(
        catalog
            .binding(&f.version_id, BindingSlot::Staged)
            .await
            .unwrap()
            .is_none(),
        "the reconciler of a retired generation never binds"
    );
    assert_eq!(f.binding_events(&f.version_id).await, 0);
    f.assert_stale_writes_refused(&gen1, &gen2).await;
    f.finish(&gen2).await;
}

/// An object already at the intent's key that does not hash to the recorded
/// digest is refused and left in place; the intent is recorded as failed and
/// the artifact is never marked verified or bound.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_conflicting_object_at_the_intent_key_is_never_overwritten() {
    guard!();
    let f = fixture("intent-conflict", 2, true).await;
    let gen1 = f.seed_claimed_job("rebuild", false).await;
    backfill_one(
        &f.ctx(Some(&gen1), Some(crash_at(BuildPhase::AfterCatalogInsert))),
        &f.pg,
        f.target(),
        &f.version_id,
    )
    .await
    .unwrap_err();
    let old_attempt = f.linked_attempts(&gen1).await.pop().expect("linked");
    let attempts = BuildAttempts::new(f.pool.clone(), &f.tenant);
    let intent = attempts
        .publication_intent(&old_attempt)
        .await
        .unwrap()
        .expect("intent");

    let planted = b"{\"not\":\"the recorded manifest\"}".to_vec();
    let path = f.artifacts.path().join(&intent.manifest_key);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, &planted).unwrap();

    let gen2 = f.reclaim(&gen1).await;
    let report = reconcile_attempts(&f.ctx(None, None)).await.unwrap();
    assert_eq!((report.resumed, report.abandoned), (0, 1), "{report:?}");

    let resolved = attempts
        .publication_intent(&old_attempt)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved.state, IntentState::Failed);
    assert_eq!(
        f.object_at(&intent.manifest_key),
        Some(planted),
        "the conflicting object is left in place"
    );
    let catalog = ArtifactCatalog::new(f.pool.clone(), &f.tenant);
    assert_eq!(
        catalog
            .artifact(&f.version_id, &intent.artifact_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        ArtifactState::Sealed,
        "never verified over bytes that do not match the digest"
    );
    assert!(catalog
        .binding(&f.version_id, BindingSlot::Staged)
        .await
        .unwrap()
        .is_none());
    f.finish(&gen2).await;
}

/// A queued direct build whose claim was superseded commits no logical
/// version, attempt or artifact. The current generation commits the version
/// together with its job and generation attribution.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_superseded_direct_build_commits_no_logical_version() {
    guard!();
    let f = fixture("direct", 2, false).await;
    let gen1 = f.seed_claimed_job("direct", true).await;
    let gen2 = f.reclaim(&gen1).await;

    let err = build_collection_direct(&f.ctx(Some(&gen1), None), &f.pg, &f.collection_id, 400, 3)
        .await
        .unwrap_err();
    assert!(err.to_string().contains(STALE_GENERATION_MESSAGE), "{err}");
    let versions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM index_versions WHERE tenant_id = $1 AND collection_id = $2",
    )
    .bind(&f.tenant)
    .bind(&f.collection_id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(versions, 0, "no logical version from a superseded claim");
    assert!(f.linked_attempts(&gen1).await.is_empty());
    f.assert_stale_writes_refused(&gen1, &gen2).await;

    let built = build_collection_direct(&f.ctx(Some(&gen2), None), &f.pg, &f.collection_id, 400, 3)
        .await
        .unwrap();
    assert!(built.committed);
    let (job_id, generation): (String, i32) = sqlx::query_as(
        "SELECT job_id, job_generation FROM index_version_job_commits
          WHERE tenant_id = $1 AND index_version_id = $2",
    )
    .bind(&f.tenant)
    .bind(&built.index_version_id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!((job_id, generation), (gen2.job_id.clone(), gen2.generation));
    assert!(matches!(
        built.artifact,
        MirrorOutcome::Published {
            bound_staged: true,
            ..
        }
    ));
    f.finish(&gen2).await;
}

/// A synchronous build (`job_fence: None`) writes no fencing rows: it keeps
/// its existing artifact-attempt authority exactly.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unfenced_build_writes_no_fencing_rows() {
    guard!();
    let f = fixture("unfenced", 1, true).await;
    let outcome = backfill_one(&f.ctx(None, None), &f.pg, f.target(), &f.version_id)
        .await
        .unwrap();
    assert!(matches!(outcome, MirrorOutcome::Published { .. }));
    for table in [
        "index_build_attempt_jobs",
        "index_build_publication_intents",
        "index_version_job_commits",
    ] {
        let n: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM {table} WHERE tenant_id = $1"
        ))
        .bind(&f.tenant)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(n, 0, "{table}");
    }
}
