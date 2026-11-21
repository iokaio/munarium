// SPDX-License-Identifier: Apache-2.0
//! Mirror publication and fleet blocks (ClearView ART1/ART2, the maintainer
//! decision of 20 November 2026 on artifact-damage open question 5), against
//! a real PostgreSQL and a real local-file artifact store.
//!
//! A rebuild of unchanged content converges on the artifact id it built
//! before. When that pin carries an active fleet block, `enforce` refuses the
//! build with an `artifact-blocked` error (a build job records
//! `artifact_blocked`, not the transient `datastore_unavailable`), `observe`
//! publishes as before and reports it, and `off` is unchanged. A verified
//! ART2 clearance lets it publish again.
//!
//! The block and the clearance are committed through the real ledger with
//! disposable, test-only verifier keys enrolled in this database. Skips
//! loudly when `MUNARIUM_TEST_DATABASE_URL` is unset.

use std::sync::{Arc, Mutex};

use munarium_core::artifact_verifier::VerifierSigningKey;
use munarium_core::KernelError;
use munarium_retrieval::backfill::backfill_one;
use munarium_retrieval::build_metrics::{BuildMetrics, BuildObserver};
use munarium_retrieval::damage::{DamageMode, PublicationGate};
use munarium_retrieval::mirror::{
    LocalStoreFactory, MirrorContext, MirrorOutcome, MirrorTarget, VectorPolicy,
};
use munarium_retrieval_pg::PgRetrieval;
use munarium_store_pg::artifact_damage::{
    enroll_verifier, ArtifactDamageLedger, AuthoritativeObservation, Authority, Decision,
    ObjectPin, ObservationSignature, Outcome, VerifierIdentity, MANIFEST_OBJECT,
};
use munarium_store_pg::artifacts::{ArtifactCatalog, BindingSlot};
use munarium_store_pg::attempts::BuildAttempts;
use munarium_store_pg::job_failure::{classify, JobFailureCode};
use munarium_store_pg::jobs::BuildJobs;
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

const ENFORCE: Authority = Authority {
    may_block: true,
    may_clear: true,
};

/// Counts `publication_blocked` reports as `(mode, refused)`.
#[derive(Debug, Default)]
struct Reports(Mutex<Vec<(&'static str, bool)>>);

impl BuildObserver for Reports {
    fn build_finished(&self, _: &BuildMetrics) {}
    fn publication_blocked(&self, mode: &'static str, refused: bool) {
        self.0.lock().unwrap().push((mode, refused));
    }
}

impl Reports {
    fn take(&self) -> Vec<(&'static str, bool)> {
        std::mem::take(&mut self.0.lock().unwrap())
    }
}

struct Fixture {
    pg: PgRetrieval,
    pool: sqlx::PgPool,
    tenant: String,
    collection_id: String,
    version_id: String,
    artifacts: tempfile::TempDir,
    staging: tempfile::TempDir,
    reports: Arc<Reports>,
    keys: Vec<VerifierSigningKey>,
}

async fn fixture(tag: &str) -> Fixture {
    let tenant = format!("tenant-artpub-{tag}-{}", uuid::Uuid::new_v4().simple());
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
            Some("blocked publication"),
        )
        .await
        .unwrap();
    for i in 0..2 {
        let body = format!(
            "Blocked publication document {i}. The lighthouse keeper logged the fog.\n\n\
             A second paragraph about the harbour pilots and their charts."
        );
        let (source_id, _, _) = pg
            .put_source(
                "",
                "text/markdown",
                &format!("blocked/doc-{i}.md"),
                Some("para"),
                body.as_bytes(),
            )
            .await
            .unwrap();
        pg.bind_source(&col.id, &source_id, None).await.unwrap();
    }
    let version_id = pg
        .build_collection_index(&col.id, 400, 1, true)
        .await
        .unwrap()
        .id;
    let pool = store.pool().clone();
    // Two disposable, test-only verifier keys: two independent verifiers.
    let keys = vec![
        VerifierSigningKey::generate(),
        VerifierSigningKey::generate(),
    ];
    for (i, k) in keys.iter().enumerate() {
        enroll_verifier(&pool, &k.public_key(), &format!("artpub-{i}"), "test")
            .await
            .unwrap();
    }
    Fixture {
        pg,
        pool,
        tenant,
        collection_id: col.id.clone(),
        version_id,
        artifacts: tempfile::tempdir().unwrap(),
        staging: tempfile::tempdir().unwrap(),
        reports: Arc::new(Reports::default()),
        keys,
    }
}

impl Fixture {
    fn ctx(&self, mode: DamageMode, gate_pool: Option<&sqlx::PgPool>) -> MirrorContext {
        MirrorContext {
            catalog: ArtifactCatalog::new(self.pool.clone(), &self.tenant),
            attempts: BuildAttempts::new(self.pool.clone(), &self.tenant),
            stores: Arc::new(LocalStoreFactory::new(self.artifacts.path())),
            node_id: "builder-artpub".into(),
            staging_root: self.staging.path().to_path_buf(),
            artifact_prefix: "v1".into(),
            tenant_path_hash: "t0000".into(),
            faults: None,
            observer: Some(self.reports.clone()),
            vector_policy: VectorPolicy {
                approx_threshold: None,
            },
            job_fence: None,
            job_signal: None,
            publication_damage: PublicationGate::for_mode(
                mode,
                gate_pool.unwrap_or(&self.pool).clone(),
                &self.tenant,
            ),
        }
    }

    fn target(&self) -> MirrorTarget<'_> {
        MirrorTarget::Collection {
            collection_id: &self.collection_id,
        }
    }

    async fn build(&self, ctx: &MirrorContext) -> munarium_core::Result<MirrorOutcome> {
        backfill_one(ctx, &self.pg, self.target(), &self.version_id).await
    }

    fn ledger(&self) -> ArtifactDamageLedger {
        ArtifactDamageLedger::new(self.pool.clone(), &self.tenant)
    }

    /// One signed authoritative observation of the manifest by key `k`.
    async fn observe(
        &self,
        artifact: &str,
        k: usize,
        outcome: Outcome,
        attempt: Option<&str>,
    ) -> Decision {
        let ledger = self.ledger();
        let key = &self.keys[k];
        let mut o = AuthoritativeObservation {
            pin: ObjectPin {
                index_version_id: self.version_id.clone(),
                artifact_id: artifact.to_string(),
                object_key: MANIFEST_OBJECT.into(),
                expected_sha256: artifact.to_string(),
                storage_generation: None,
            },
            verifier: VerifierIdentity {
                verifier_id: key.fingerprint(),
                run_id: uuid::Uuid::new_v4().to_string(),
            },
            fetch_id: uuid::Uuid::new_v4().to_string(),
            fetched_at: ledger.db_now().await.unwrap(),
            outcome,
            observed_sha256: Some(match outcome {
                Outcome::Mismatch => "b".repeat(64),
                _ => artifact.to_string(),
            }),
            repair_attempt_id: attempt.map(str::to_string),
            detail: None,
            signature: None,
        };
        let generation = ledger
            .admit(&self.version_id, artifact)
            .await
            .unwrap()
            .generation;
        o.signature = Some(ObservationSignature {
            pin_generation: generation,
            signature: key.sign(&o.claims(&self.tenant, generation)),
        });
        ledger
            .record_authoritative(&o, ENFORCE)
            .await
            .unwrap()
            .decision
    }

    /// Commit a fleet block on `artifact`; return the incident id.
    async fn block(&self, artifact: &str) -> String {
        assert_eq!(
            self.observe(artifact, 0, Outcome::Mismatch, None).await,
            Decision::Candidate
        );
        match self.observe(artifact, 1, Outcome::Mismatch, None).await {
            Decision::Blocked { incident_id, .. } => incident_id,
            other => panic!("expected a block, got {other:?}"),
        }
    }

    /// Clear the block with a preauthorized, independently verified repair.
    async fn clear(&self, artifact: &str, incident: &str) {
        let attempt = self
            .ledger()
            .authorize_repair(incident, "repair-workflow", "operator", None)
            .await
            .unwrap();
        let a = Some(attempt.attempt_id.as_str());
        assert_eq!(
            self.observe(artifact, 0, Outcome::Match, a).await,
            Decision::RepairMatch
        );
        assert!(matches!(
            self.observe(artifact, 1, Outcome::Match, a).await,
            Decision::Cleared { .. }
        ));
    }

    /// Publish the version once with no gate; return its artifact id.
    async fn publish_first(&self) -> String {
        match self.build(&self.ctx(DamageMode::Off, None)).await.unwrap() {
            MirrorOutcome::Published {
                artifact_id,
                bound_staged,
                ..
            } => {
                assert!(bound_staged);
                artifact_id
            }
            other => panic!("expected a publication, got {other:?}"),
        }
    }

    async fn staged(&self) -> (String, i64) {
        let b = ArtifactCatalog::new(self.pool.clone(), &self.tenant)
            .binding(&self.version_id, BindingSlot::Staged)
            .await
            .unwrap()
            .expect("a staged binding");
        (b.artifact_id, b.generation)
    }
}

fn already_built(outcome: &MirrorOutcome, artifact: &str) {
    assert!(
        matches!(outcome, MirrorOutcome::AlreadyBuilt { artifact_id } if artifact_id == artifact),
        "{outcome:?}"
    );
}

/// `enforce` refuses a rebuild that converges on a blocked pin, with a
/// content-free `artifact-blocked` error that a build job records as
/// `artifact_blocked`, and leaves the binding untouched.
#[tokio::test]
async fn enforce_refuses_a_rebuild_that_converges_on_a_blocked_pin() {
    guard!();
    let f = fixture("enforce").await;
    let artifact = f.publish_first().await;
    let staged_before = f.staged().await;
    let incident = f.block(&artifact).await;

    let err = f
        .build(&f.ctx(DamageMode::Enforce, None))
        .await
        .expect_err("a blocked pin is not published");
    let KernelError::ArtifactBlocked(reason) = &err else {
        panic!("expected artifact-blocked, got {err:?}");
    };
    assert!(
        reason.contains(&incident) && reason.contains(&artifact),
        "{reason}"
    );
    assert!(!reason.contains("Lighthouse") && !reason.contains("lighthouse"));
    assert_eq!(f.reports.take(), vec![("enforce", true)]);
    assert_eq!(f.staged().await, staged_before, "nothing was rebound");

    // The job outcome: the builder records the typed cause's code (C2).
    let failure = classify(&err);
    assert_eq!(failure.code, Some(JobFailureCode::ArtifactBlocked));
    let jobs = BuildJobs::new(f.pool.clone());
    let job_id = format!("bjob-artpub-{}", uuid::Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO index_build_jobs
             (tenant_id, job_id, kind, index_version_id, params, state, claimed_by,
              claimed_at, attempts, requested_by)
         VALUES ($1,$2,'rebuild',$3,'{}'::jsonb,'running','builder-artpub','infinity',1,
                 'test-artpub')",
    )
    .bind(&f.tenant)
    .bind(&job_id)
    .bind(&f.version_id)
    .execute(&f.pool)
    .await
    .unwrap();
    assert!(jobs
        .complete_fenced_classified(&f.tenant, &job_id, "builder-artpub", 1, Err(failure), None)
        .await
        .unwrap());
    let job = jobs.get(&f.tenant, &job_id).await.unwrap().unwrap();
    assert_eq!(job.state, "failed");
    assert_eq!(job.error_code.as_deref(), Some("artifact_blocked"));
    let error = job.error.unwrap_or_default();
    assert_eq!(error, JobFailureCode::ArtifactBlocked.message());
    assert!(!error.contains(&incident) && !error.contains(&artifact));
}

/// `observe` publishes as before and reports the blocked pin.
#[tokio::test]
async fn observe_publishes_a_blocked_pin_and_reports_it() {
    guard!();
    let f = fixture("observe").await;
    let artifact = f.publish_first().await;
    f.block(&artifact).await;
    let outcome = f.build(&f.ctx(DamageMode::Observe, None)).await.unwrap();
    already_built(&outcome, &artifact);
    assert_eq!(f.reports.take(), vec![("observe", false)]);
}

/// `off` (the default) is unchanged: no check, no report.
#[tokio::test]
async fn off_publishes_a_blocked_pin_unchanged() {
    guard!();
    let f = fixture("off").await;
    let artifact = f.publish_first().await;
    f.block(&artifact).await;
    let ctx = f.ctx(DamageMode::Off, None);
    assert!(ctx.publication_damage.is_none());
    already_built(&f.build(&ctx).await.unwrap(), &artifact);
    assert_eq!(f.reports.take(), vec![]);
}

/// An unblocked pin publishes in every mode without a report.
#[tokio::test]
async fn an_unblocked_pin_publishes_in_every_mode() {
    guard!();
    let f = fixture("clean").await;
    let artifact = f.publish_first().await;
    for mode in [DamageMode::Observe, DamageMode::Enforce] {
        already_built(&f.build(&f.ctx(mode, None)).await.unwrap(), &artifact);
    }
    assert_eq!(f.reports.take(), vec![]);
}

/// After a verified ART2 clearance the same pin publishes again in `enforce`.
#[tokio::test]
async fn a_cleared_pin_publishes_again() {
    guard!();
    let f = fixture("cleared").await;
    let artifact = f.publish_first().await;
    let incident = f.block(&artifact).await;
    let enforce = f.ctx(DamageMode::Enforce, None);
    assert!(f.build(&enforce).await.is_err());
    f.clear(&artifact, &incident).await;
    already_built(&f.build(&enforce).await.unwrap(), &artifact);
    assert_eq!(f.reports.take(), vec![("enforce", true)]);
}

/// An unavailable pin check refuses in `enforce`, as serving admission does,
/// and is only reported in `observe`.
#[tokio::test]
async fn an_unavailable_check_refuses_only_in_enforce() {
    guard!();
    let f = fixture("unavail").await;
    let artifact = f.publish_first().await;
    let closed = PgStore::connect(&url().unwrap(), &f.tenant)
        .await
        .unwrap()
        .pool()
        .clone();
    closed.close().await;
    let err = f
        .build(&f.ctx(DamageMode::Enforce, Some(&closed)))
        .await
        .expect_err("an unavailable check refuses");
    // Transient, unlike a block: it stays `datastore-unavailable`.
    assert!(
        matches!(err, KernelError::DatastoreUnavailable(_)),
        "{err:?}"
    );
    assert_eq!(
        classify(&err).code,
        Some(JobFailureCode::DatastoreUnavailable)
    );
    already_built(
        &f.build(&f.ctx(DamageMode::Observe, Some(&closed)))
            .await
            .unwrap(),
        &artifact,
    );
}
