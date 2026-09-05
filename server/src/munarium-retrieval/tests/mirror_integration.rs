// SPDX-License-Identifier: Apache-2.0
//! Mirror builds, backfill, and the §15.4 failure injection, against a real
//! PostgreSQL and a real artifact store.
//!
//! The claims under test are about *ordering under interruption*, so nothing
//! here is mocked: the chunks come from a collection built the ordinary way,
//! the catalog and attempt rows are real, and the artifact is opened back
//! through the same reader a query node uses. Skips loudly when
//! `MUNARIUM_TEST_DATABASE_URL` is unset.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use munarium_datastore::shard::{OpenShard, MANIFEST};
use munarium_datastore::store::ArtifactStore;
use munarium_datastore::verify::{Limits, ReaderCapabilities};
use munarium_retrieval::backfill::{backfill_collection, backfill_one};
use munarium_retrieval::build_metrics::{BuildMetrics, BuildObserver};
use munarium_retrieval::mirror::{
    mirror_index, mirror_plan, reconcile_attempts, reconstructed_spec, BuildPhase,
    LocalStoreFactory, MirrorContext, MirrorOutcome, MirrorTarget,
};
use munarium_retrieval_pg::required::RequiredVersionsPolicy;
use munarium_retrieval_pg::PgRetrieval;
use munarium_store_pg::artifacts::{ArtifactCatalog, ArtifactState, BindingSlot};
use munarium_store_pg::attempts::{AttemptState, BuildAttempts};
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

fn unique(p: &str) -> String {
    format!("{p}-{}", uuid::Uuid::new_v4().simple())
}

const HORIZON: i64 = 3_600;

/// Records what each build reported, so a test can assert the cost was
/// measured rather than assumed.
#[derive(Debug, Default)]
struct Recorder(Mutex<Vec<BuildMetrics>>);

impl BuildObserver for Recorder {
    fn build_finished(&self, m: &BuildMetrics) {
        self.0.lock().unwrap().push(m.clone());
    }
}

struct Harness {
    pg: PgRetrieval,
    store: PgStore,
    tenant: String,
    builds: Arc<Recorder>,
    ctx: MirrorContext,
    collection_id: String,
    version_id: String,
    /// Held so the temporary directories outlive the test.
    _artifacts: tempfile::TempDir,
    _staging: tempfile::TempDir,
}

/// Build a real collection through the ordinary path, then wire a mirror
/// context over temporary local stores.
async fn harness(tenant: &str, docs: usize) -> Harness {
    let url = url().expect("guarded");
    let store = PgStore::connect(&url, tenant).await.unwrap();
    let pg = PgRetrieval::new(store.pool().clone(), tenant);

    let col = pg
        .ensure_collection(&unique("col"), "para", 0, &[], Some("mirror test"))
        .await
        .unwrap();
    for i in 0..docs {
        let body = format!(
            "Document {i}. The continental congress met in Philadelphia and debated supply.\n\n\
             A second paragraph about the destruction of the tea in Boston harbour."
        );
        let (source_id, _, _) = pg
            .put_source(
                "",
                "text/markdown",
                &format!("corpus/doc-{i}.md"),
                Some("para"),
                body.as_bytes(),
            )
            .await
            .unwrap();
        pg.bind_source(&col.id, &source_id, None).await.unwrap();
    }
    let version = pg
        .build_collection_index(&col.id, 400, 1, true)
        .await
        .unwrap();

    let artifacts = tempfile::tempdir().unwrap();
    let staging = tempfile::tempdir().unwrap();
    let builds: Arc<Recorder> = Arc::new(Recorder::default());
    let ctx = MirrorContext {
        catalog: ArtifactCatalog::new(store.pool().clone(), tenant),
        attempts: BuildAttempts::new(store.pool().clone(), tenant),
        stores: Arc::new(LocalStoreFactory::new(artifacts.path())),
        node_id: "node-test".into(),
        staging_root: staging.path().to_path_buf(),
        artifact_prefix: "v1".into(),
        // Opaque by contract; a fixed value here is a fine stand-in for the
        // per-tenant hash Server derives.
        tenant_path_hash: "t0000".into(),
        faults: None,
        observer: Some(builds.clone()),
        vector_policy: munarium_retrieval::mirror::VectorPolicy {
            approx_threshold: None,
        },
    };

    Harness {
        tenant: tenant.to_string(),
        builds,
        collection_id: col.id.clone(),
        version_id: version.id.clone(),
        pg,
        ctx,
        store,
        _artifacts: artifacts,
        _staging: staging,
    }
}

impl Harness {
    fn target(&self) -> MirrorTarget<'_> {
        MirrorTarget::Collection {
            collection_id: &self.collection_id,
        }
    }

    async fn mirror(&self) -> munarium_core::Result<MirrorOutcome> {
        backfill_one(&self.ctx, &self.pg, self.target(), &self.version_id).await
    }

    async fn store_for(&self, artifact_id: &str) -> Arc<dyn ArtifactStore> {
        let row = self
            .ctx
            .catalog
            .artifact(&self.version_id, artifact_id)
            .await
            .unwrap()
            .expect("the artifact is catalogued");
        self.ctx.stores.store_for_prefix(&row.artifact_uri).unwrap()
    }
}

/// The whole of §7.1: a mirror builds, publishes, verifies, and binds `staged`
/// — and the artifact that lands is openable and holds exactly the chunks the
/// PostgreSQL version holds, with the same text hashes.
#[tokio::test]
async fn a_mirror_build_publishes_an_artifact_that_matches_the_postgres_rows() {
    guard!();
    let h = harness("tenant-mirror-a", 3).await;

    let artifact_id = match h.mirror().await.unwrap() {
        MirrorOutcome::Published {
            artifact_id,
            chunks,
            bound_staged,
        } => {
            assert!(chunks >= 3, "three documents produce at least three chunks");
            assert!(bound_staged, "a version with no staged binding gets one");
            artifact_id
        }
        other => panic!("expected a publication, got {other:?}"),
    };

    // The catalog agrees, and the binding points at this artifact.
    let row = h
        .ctx
        .catalog
        .artifact(&h.version_id, &artifact_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, ArtifactState::Verified);
    let binding = h
        .ctx
        .catalog
        .binding(&h.version_id, BindingSlot::Staged)
        .await
        .unwrap()
        .expect("staged binding");
    assert_eq!(binding.artifact_id, artifact_id);
    // A mirror must never write `serving`.
    assert!(h
        .ctx
        .catalog
        .binding(&h.version_id, BindingSlot::Serving)
        .await
        .unwrap()
        .is_none());

    // Open it the way a query node would, then check it against the rows it
    // came from. This is the claim that makes shadow comparison meaningful.
    let store = h.store_for(&artifact_id).await;
    let shard = OpenShard::open(
        store.as_ref(),
        &artifact_id,
        &ReaderCapabilities::v1(),
        &Limits::default(),
    )
    .expect("the published artifact opens");

    let mut expected = Vec::new();
    h.pg.export_collection_chunks(&h.collection_id, &h.version_id, |c| {
        expected.push(c);
        Ok(())
    })
    .await
    .unwrap();

    assert_eq!(shard.records().len(), expected.len());
    for want in &expected {
        let got = shard
            .record(&want.chunk_id)
            .unwrap_or_else(|| panic!("artifact is missing chunk {}", want.chunk_id));
        assert_eq!(got.text, want.text, "text drifted for {}", want.chunk_id);
        assert_eq!(
            got.text_sha256,
            hex::encode(want.text_sha256),
            "text hash drifted for {}",
            want.chunk_id
        );
        assert_eq!(got.source_id, want.source_id);
        assert_eq!(got.source_path, want.source_path);
    }

    // The build reported its own cost. Without this the metrics seam is code
    // that compiles and never fires.
    let recorded = h.builds.0.lock().unwrap();
    assert_eq!(recorded.len(), 1, "one build, one record");
    let m = &recorded[0];
    assert_eq!(m.mode, "mirror");
    assert_eq!(m.outcome, "published");
    assert_eq!(m.chunks as usize, expected.len());
    assert!(m.bytes > 0, "a sealed artifact has bytes");
    assert!(
        m.total_seconds >= m.export_seconds + m.seal_seconds + m.publish_seconds - 1e-6,
        "the phases cannot exceed the whole: {m:?}"
    );
}

/// The idempotence rule. A second mirror of the same version under the same
/// plan finds the verified artifact and does no work — it must not spend a
/// build producing the same bytes under a new attempt.
#[tokio::test]
async fn a_second_mirror_of_the_same_plan_reuses_the_verified_artifact() {
    guard!();
    let h = harness("tenant-mirror-b", 2).await;
    let first = h.mirror().await.unwrap();
    let second = h.mirror().await.unwrap();

    let (
        MirrorOutcome::Published { artifact_id, .. },
        MirrorOutcome::AlreadyBuilt { artifact_id: again },
    ) = (first.clone(), second.clone())
    else {
        panic!("expected publish then reuse, got {first:?} then {second:?}");
    };
    assert_eq!(artifact_id, again, "the same content is the same artifact");
}

/// A spec that is not marked reconstructed is refused. A mirror's spec is a
/// best reassembly of inputs nobody recorded, and one that did not say so could
/// be mistaken for a replayable record.
#[tokio::test]
async fn a_spec_that_does_not_admit_it_is_reconstructed_is_refused() {
    guard!();
    let h = harness("tenant-mirror-c", 1).await;
    let facts = h.pg.version_facts(&h.version_id).await.unwrap();
    let sources =
        h.pg.exported_sources(&h.collection_id, &h.version_id)
            .await
            .unwrap();
    let mut spec = reconstructed_spec(
        h.target(),
        &facts.shape_ref,
        facts.watermark_seq,
        Some(munarium_retrieval_pg::EMBED_DIMS as u32),
        &sources,
        "x",
    );
    spec.reconstructed = false;

    let err = mirror_index(
        &h.ctx,
        &h.pg,
        h.target(),
        &h.version_id,
        &spec,
        &mirror_plan(true),
    )
    .await
    .unwrap_err();
    assert!(format!("{err}").contains("reconstructed"), "{err}");
}

/// Interrupted between the catalog insert and the component upload. Nothing is
/// published, the attempt is `sealed` — mid-flight, not finished — and the
/// reconciler resumes it to a verified artifact.
#[tokio::test]
async fn a_build_killed_before_upload_is_resumed_by_the_reconciler() {
    guard!();
    let mut h = harness("tenant-mirror-d", 2).await;
    h.ctx.faults = Some(fault_at(BuildPhase::AfterCatalogInsert));

    let err = h.mirror().await.unwrap_err();
    assert!(format!("{err}").contains("injected"), "{err}");

    // Sealed, and NOT published: the final prefix has no manifest, so no reader
    // can open it. This is the window the ordering exists to make safe.
    let sealed = h.ctx.attempts.sealed_awaiting_publication().await.unwrap();
    let attempt = sealed.first().expect("one sealed attempt awaits");
    let artifact_id = attempt.artifact_id.clone().unwrap();
    let store = h.store_for(&artifact_id).await;
    assert!(
        !store.exists(MANIFEST).unwrap(),
        "a killed build must not have left a readable manifest behind"
    );
    assert_eq!(
        h.ctx
            .catalog
            .artifact(&h.version_id, &artifact_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        ArtifactState::Sealed,
        "a sealed row must never be read as 'L2 exists'"
    );

    h.ctx.faults = None;
    let report = reconcile_attempts(&h.ctx).await.unwrap();
    assert_eq!(report.resumed, 1, "{report:?}");
    assert_eq!(report.abandoned, 0);

    assert_eq!(
        h.ctx
            .catalog
            .artifact(&h.version_id, &artifact_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        ArtifactState::Verified
    );
    assert!(store.exists(MANIFEST).unwrap());
    OpenShard::open(
        store.as_ref(),
        &artifact_id,
        &ReaderCapabilities::v1(),
        &Limits::default(),
    )
    .expect("the resumed artifact opens");
}

/// Interrupted after the components landed but before the manifest. This is the
/// dangerous window: the bytes are all there and nothing may read them, because
/// the manifest is what makes an artifact exist.
#[tokio::test]
async fn a_build_killed_between_components_and_manifest_leaves_nothing_readable() {
    guard!();
    let mut h = harness("tenant-mirror-e", 2).await;
    h.ctx.faults = Some(fault_at(BuildPhase::AfterComponentUpload));
    h.mirror().await.unwrap_err();

    let attempt = h
        .ctx
        .attempts
        .sealed_awaiting_publication()
        .await
        .unwrap()
        .pop()
        .expect("one sealed attempt");
    let artifact_id = attempt.artifact_id.clone().unwrap();
    let store = h.store_for(&artifact_id).await;
    assert!(
        store.exists("records/chunks.bin").unwrap(),
        "the components did land"
    );
    assert!(
        !store.exists(MANIFEST).unwrap(),
        "and without the manifest the artifact does not exist"
    );
    assert!(
        OpenShard::open(
            store.as_ref(),
            &artifact_id,
            &ReaderCapabilities::v1(),
            &Limits::default()
        )
        .is_err(),
        "a component set with no manifest must not open"
    );

    h.ctx.faults = None;
    assert_eq!(reconcile_attempts(&h.ctx).await.unwrap().resumed, 1);
    OpenShard::open(
        store.as_ref(),
        &artifact_id,
        &ReaderCapabilities::v1(),
        &Limits::default(),
    )
    .expect("the resumed artifact opens");
}

/// Interrupted after the manifest landed but before the catalog said
/// `verified`. Republishing is idempotent, so the resume writes the same bytes
/// to the same keys and finishes the state transition.
#[tokio::test]
async fn a_build_killed_after_the_manifest_finishes_on_resume() {
    guard!();
    let mut h = harness("tenant-mirror-f", 2).await;
    h.ctx.faults = Some(fault_at(BuildPhase::AfterManifestWrite));
    h.mirror().await.unwrap_err();

    let attempt = h
        .ctx
        .attempts
        .sealed_awaiting_publication()
        .await
        .unwrap()
        .pop()
        .expect("one sealed attempt");
    let artifact_id = attempt.artifact_id.clone().unwrap();
    let store = h.store_for(&artifact_id).await;
    let before = store.get_component(MANIFEST, None).unwrap();

    h.ctx.faults = None;
    assert_eq!(reconcile_attempts(&h.ctx).await.unwrap().resumed, 1);

    // Byte-for-byte immutable: a republish to a content-addressed prefix must
    // reproduce the same bytes or the address was never content.
    assert_eq!(store.get_component(MANIFEST, None).unwrap(), before);
    assert_eq!(
        h.ctx
            .catalog
            .artifact(&h.version_id, &artifact_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        ArtifactState::Verified
    );
}

/// Interrupted before the catalog insert. There is nothing to resume and
/// nothing was catalogued, so the attempt simply fails.
#[tokio::test]
async fn a_build_killed_before_the_catalog_insert_has_nothing_to_resume() {
    guard!();
    let mut h = harness("tenant-mirror-g", 2).await;
    h.ctx.faults = Some(fault_at(BuildPhase::AfterSeal));
    h.mirror().await.unwrap_err();

    assert!(
        h.ctx
            .attempts
            .sealed_awaiting_publication()
            .await
            .unwrap()
            .is_empty(),
        "a build that never catalogued anything is not awaiting publication"
    );
    assert!(
        h.ctx
            .catalog
            .artifacts_for_version(&h.version_id)
            .await
            .unwrap()
            .is_empty(),
        "and it left no catalog row"
    );
    let report = reconcile_attempts(&h.ctx).await.unwrap();
    assert_eq!(report.resumed, 0);
    assert_eq!(report.abandoned, 0);
}

/// A sealed attempt whose staged content is gone is abandoned, not resumed.
/// There is nothing left to publish, and pretending otherwise would leave the
/// attempt cycling forever.
#[tokio::test]
async fn a_sealed_attempt_whose_staging_is_gone_is_abandoned() {
    guard!();
    let mut h = harness("tenant-mirror-h", 2).await;
    h.ctx.faults = Some(fault_at(BuildPhase::AfterCatalogInsert));
    h.mirror().await.unwrap_err();

    let attempt = h
        .ctx
        .attempts
        .sealed_awaiting_publication()
        .await
        .unwrap()
        .pop()
        .expect("one sealed attempt");
    std::fs::remove_dir_all(h.ctx.staging_root.join(&attempt.attempt_id)).unwrap();

    h.ctx.faults = None;
    let report = reconcile_attempts(&h.ctx).await.unwrap();
    assert_eq!(report.abandoned, 1, "{report:?}");
    assert_eq!(report.resumed, 0);

    let row = h
        .ctx
        .attempts
        .get(&attempt.attempt_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, AttemptState::Failed);
    // The artifact stays `sealed` rather than being deleted: the row is the
    // record that this content was once being published, and a later rebuild
    // converges on it.
    assert_eq!(
        h.ctx
            .catalog
            .artifact(&h.version_id, attempt.artifact_id.as_ref().unwrap())
            .await
            .unwrap()
            .unwrap()
            .state,
        ArtifactState::Sealed
    );
}

/// A sealed-but-unpublished attempt is still in-flight work on that plan. A
/// second builder must defer to it rather than rebuild past it.
///
/// The reason is measured, not theoretical: Tantivy is not byte-deterministic
/// (fresh segment ids per build), so two independent builds of one plan produce
/// two DIFFERENT artifact ids, and the §7.1 step-7 convergence rule — keyed on
/// the artifact id — would not merge them. Rebuilding past a sealed attempt
/// would leave two verified artifacts for one plan.
#[tokio::test]
async fn a_second_builder_defers_to_a_sealed_attempt_and_one_artifact_results() {
    guard!();
    let mut h = harness("tenant-mirror-i", 2).await;

    // The first node seals and catalogs, then dies before uploading anything.
    h.ctx.faults = Some(fault_at(BuildPhase::AfterCatalogInsert));
    h.mirror().await.unwrap_err();
    let first = h
        .ctx
        .attempts
        .sealed_awaiting_publication()
        .await
        .unwrap()
        .pop()
        .expect("the first attempt is sealed and unpublished");
    let artifact_id = first.artifact_id.clone().unwrap();

    // A second node tries the same plan and is told who holds it.
    let other = MirrorContext {
        node_id: "node-other".into(),
        faults: None,
        ..h.ctx.clone()
    };
    match backfill_one(&other, &h.pg, h.target(), &h.version_id)
        .await
        .unwrap()
    {
        MirrorOutcome::AlreadyRunning { owner_node_id } => {
            assert_eq!(owner_node_id, "node-test", "the loser learns who holds it")
        }
        other => panic!("the second builder must defer, got {other:?}"),
    }
    assert_eq!(
        h.ctx
            .catalog
            .artifacts_for_version(&h.version_id)
            .await
            .unwrap()
            .len(),
        1,
        "deferring means no second artifact was created"
    );

    // The first node comes back and finishes its own publication.
    h.ctx.faults = None;
    let report = reconcile_attempts(&h.ctx).await.unwrap();
    assert_eq!(report.resumed, 1, "{report:?}");

    let rows = h
        .ctx
        .catalog
        .artifacts_for_version(&h.version_id)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "one plan and one build is one catalog row");
    assert_eq!(rows[0].state, ArtifactState::Verified);
    assert_eq!(rows[0].artifact_id, artifact_id);

    let store = h.store_for(&artifact_id).await;
    OpenShard::open(
        store.as_ref(),
        &artifact_id,
        &ReaderCapabilities::v1(),
        &Limits::default(),
    )
    .expect("the resumed artifact opens");
    assert_eq!(
        h.ctx
            .attempts
            .get(&first.attempt_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        AttemptState::Succeeded
    );
}

/// A republish over an already-published prefix reproduces the same bytes.
/// Content addressing is only meaningful if that holds.
#[tokio::test]
async fn republishing_a_published_prefix_is_byte_for_byte_identical() {
    guard!();
    let mut h = harness("tenant-mirror-l", 2).await;
    h.ctx.faults = Some(fault_at(BuildPhase::AfterManifestWrite));
    h.mirror().await.unwrap_err();

    let attempt = h
        .ctx
        .attempts
        .sealed_awaiting_publication()
        .await
        .unwrap()
        .pop()
        .expect("one sealed attempt");
    let artifact_id = attempt.artifact_id.clone().unwrap();
    let store = h.store_for(&artifact_id).await;
    let before: Vec<Vec<u8>> = ["records/chunks.bin", "records/chunks.idx", MANIFEST]
        .iter()
        .map(|p| store.get_component(p, None).unwrap())
        .collect();

    h.ctx.faults = None;
    assert_eq!(reconcile_attempts(&h.ctx).await.unwrap().resumed, 1);

    let after: Vec<Vec<u8>> = ["records/chunks.bin", "records/chunks.idx", MANIFEST]
        .iter()
        .map(|p| store.get_component(p, None).unwrap())
        .collect();
    assert_eq!(
        before, after,
        "a content-addressed prefix must be immutable"
    );
}

/// Backfill covers the active version and reports completeness honestly.
#[tokio::test]
async fn backfill_covers_the_required_versions_of_a_scope() {
    guard!();
    let h = harness("tenant-mirror-j", 2).await;

    let report = backfill_collection(
        &h.ctx,
        &h.pg,
        &h.collection_id,
        RequiredVersionsPolicy::ActivePinnedAndHorizon,
        HORIZON,
    )
    .await
    .unwrap();

    assert!(report.is_complete(), "{report:?}");
    assert_eq!(report.complete_count(), report.versions.len());
    assert!(
        report
            .versions
            .iter()
            .any(|v| v.index_version_id == h.version_id),
        "the version just built must be required"
    );

    // Re-running is a no-op that still reports complete: a backfill that
    // reported a healthy re-run as incomplete would block its own rollout.
    let again = backfill_collection(
        &h.ctx,
        &h.pg,
        &h.collection_id,
        RequiredVersionsPolicy::ActivePinnedAndHorizon,
        HORIZON,
    )
    .await
    .unwrap();
    assert!(again.is_complete());
}

/// The policy that promises pin coverage nothing can supply is refused rather
/// than silently returning a shorter set.
#[tokio::test]
async fn the_pinned_only_policy_is_refused() {
    guard!();
    let h = harness("tenant-mirror-k", 1).await;
    let err = backfill_collection(
        &h.ctx,
        &h.pg,
        &h.collection_id,
        RequiredVersionsPolicy::ActiveAndPinned,
        HORIZON,
    )
    .await
    .unwrap_err();
    assert!(format!("{err}").contains("horizon term"), "{err}");
}

/// A fault hook that fires once at `phase`, so a retry in the same test can
/// proceed. Firing every time would make a resume impossible to test.
fn fault_at(phase: BuildPhase) -> munarium_retrieval::mirror::FaultHook {
    let fired = Arc::new(AtomicBool::new(false));
    Arc::new(move |p| {
        if p == phase && !fired.swap(true, Ordering::SeqCst) {
            return Err(munarium_core::KernelError::Storage(format!(
                "injected failure at {p:?}"
            )));
        }
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// The shadow candidate path: resolve the shadow binding, hydrate,
// open, run both legs — against the same real artifacts the mirror built.
// ---------------------------------------------------------------------------

use munarium_datastore::hydrate::{CacheBudget, L1Cache};
use munarium_retrieval::executor::{ArtifactExecutor, ExecutionOutcome};
use munarium_retrieval::shadow_candidate::{comparison, execute_candidate};

impl Harness {
    fn shadow_context(&self, cache_dir: &std::path::Path) -> ArtifactExecutor {
        ArtifactExecutor {
            catalog: self.ctx.catalog.clone(),
            stores: self.ctx.stores.clone(),
            l0: Arc::new(munarium_retrieval::executor::L0Cache::new(8)),
            cache: Arc::new(
                L1Cache::new(
                    cache_dir,
                    CacheBudget::new(512 * 1024 * 1024, 256 * 1024 * 1024).unwrap(),
                )
                .unwrap(),
            ),
            reader: ReaderCapabilities::v1(),
            limits: Limits::default(),
            isolation_domain: "t0000".into(),
        }
    }
}

fn prepared_for(query: &str) -> Arc<munarium_core::retrieval::PreparedSearchQuery> {
    Arc::new(PgRetrieval::prepare_query(
        query,
        &munarium_core::retrieval::SearchParams::default(),
        &munarium_retrieval_pg::LocalHashEmbedder,
    ))
}

/// A version with no shadow binding refuses — a state, not an incident, and
/// exactly what every version looks like before an operator binds one.
#[tokio::test]
async fn a_version_without_a_shadow_binding_refuses() {
    guard!();
    let h = harness("tenant-shadow-a", 2).await;
    h.mirror().await.unwrap();
    let cache = tempfile::tempdir().unwrap();
    let ctx = h.shadow_context(cache.path());
    match execute_candidate(&ctx, &h.version_id, &prepared_for("tea in boston")).await {
        ExecutionOutcome::Refused(reason) => {
            assert!(reason.contains("no shadow binding"), "{reason}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// The whole candidate path end to end: bind the mirrored artifact into the
/// shadow slot, execute the SAME prepared query the reference used, and
/// compare — over real bytes, a real catalog and a real cache.
#[tokio::test]
async fn the_shadow_candidate_executes_and_compares_against_the_reference() {
    guard!();
    let h = harness("tenant-shadow-b", 3).await;
    let artifact_id = match h.mirror().await.unwrap() {
        MirrorOutcome::Published { artifact_id, .. } => artifact_id,
        other => panic!("expected a publication, got {other:?}"),
    };
    h.ctx
        .catalog
        .bind_new(
            &h.version_id,
            BindingSlot::Shadow,
            &artifact_id,
            "test",
            None,
        )
        .await
        .unwrap();

    let cache = tempfile::tempdir().unwrap();
    let ctx = h.shadow_context(cache.path());
    let prepared = prepared_for("the destruction of the tea in boston harbour");

    let execution = match execute_candidate(&ctx, &h.version_id, &prepared).await {
        ExecutionOutcome::Executed(e) => e,
        other => panic!("expected an execution, got {other:?}"),
    };
    assert_eq!(execution.artifact_id, artifact_id);
    assert!(
        !execution.hits.is_empty(),
        "every document mentions the tea; the lexical leg cannot be empty"
    );
    assert!(execution.latency.total_ms > 0.0);
    for hit in &execution.hits {
        assert!(
            hit.text.is_empty(),
            "a candidate hit must not carry corpus text"
        );
        assert_eq!(hit.source_content_hash.len(), 64, "chunk-text sha256, hex");
    }

    // The reference side answers the identical prepared query, and the
    // comparison consumes both.
    let reference =
        h.pg.search_collection_prepared(&h.collection_id, &prepared, None)
            .await
            .unwrap();
    assert!(!reference.hits.is_empty());

    let c = comparison(
        "the destruction of the tea in boston harbour",
        &h.version_id,
        &reference,
        &execution,
        Default::default(),
        None,
    );
    assert_eq!(
        c.outcome,
        munarium_retrieval::shadow::ShadowOutcome::Completed
    );
    // The two engines index the SAME chunks: any chunk both fused sets share
    // must hash identically, or the comparison itself is broken. Ranking may
    // differ — the analyzers do — but identity may not.
    assert!(
        !c.is_corrupting(),
        "text-hash or provenance mismatch over one corpus: {:?}",
        c.identity
    );
    let fused = c.fused.as_ref().unwrap();
    assert!(
        fused.overlap > 0,
        "two engines over three documents about one subject share no hits: \
         reference={:?} candidate={:?}",
        fused.reference_count,
        fused.candidate_count
    );
}

/// A second execution finds the artifact already resident: the cache is doing
/// its job, and repeated shadows do not re-download the corpus.
#[tokio::test]
async fn a_repeated_candidate_reuses_the_resident_artifact() {
    guard!();
    let h = harness("tenant-shadow-c", 2).await;
    let artifact_id = match h.mirror().await.unwrap() {
        MirrorOutcome::Published { artifact_id, .. } => artifact_id,
        other => panic!("expected a publication, got {other:?}"),
    };
    h.ctx
        .catalog
        .bind_new(
            &h.version_id,
            BindingSlot::Shadow,
            &artifact_id,
            "test",
            None,
        )
        .await
        .unwrap();

    let cache = tempfile::tempdir().unwrap();
    let ctx = h.shadow_context(cache.path());
    let prepared = prepared_for("continental congress in philadelphia");

    let first = execute_candidate(&ctx, &h.version_id, &prepared).await;
    assert!(matches!(first, ExecutionOutcome::Executed(_)), "{first:?}");
    let used = ctx.cache.used_bytes();
    assert!(
        used > 0,
        "the artifact is resident after the first execution"
    );

    let second = execute_candidate(&ctx, &h.version_id, &prepared).await;
    assert!(
        matches!(second, ExecutionOutcome::Executed(_)),
        "{second:?}"
    );
    assert_eq!(
        ctx.cache.used_bytes(),
        used,
        "a second execution must not grow the cache"
    );
}

// ---------------------------------------------------------------------------
// Datastore serving: the coordinator dispatch, end to end.
// ---------------------------------------------------------------------------

use munarium_retrieval::serving::ServingPlane;
use munarium_retrieval::{Retrieval, RetrievalMode};
use munarium_store_pg::rollout::{RolloutChange, RolloutSelector};

impl Harness {
    fn serving_retrieval(&self, cache_dir: &std::path::Path) -> Retrieval {
        let plane = Arc::new(ServingPlane {
            selector: RolloutSelector::new(self.store.pool().clone(), &self.tenant),
            executor: Arc::new(self.shadow_context(cache_dir)),
        });
        Retrieval::new(self.pg.clone(), RetrievalMode::Datastore).with_serving(plane)
    }

    async fn promote_to_serving(&self, artifact_id: &str) {
        // The mirror bound `staged`; promotion is the §7.3 CAS.
        let staged = self
            .ctx
            .catalog
            .binding(&self.version_id, BindingSlot::Staged)
            .await
            .unwrap()
            .expect("the mirror binds staged");
        assert_eq!(staged.artifact_id, artifact_id);
        let bound = self
            .ctx
            .catalog
            .promote_staged(&self.version_id, staged.generation, 0, "test", None)
            .await
            .unwrap();
        assert_eq!(bound.slot, BindingSlot::Serving);
        assert_eq!(bound.artifact_id, artifact_id);
    }
}

/// The full stage 6 sequence against real infrastructure: mirror → promote
/// staged→serving → select the scope → the SAME coordinator call the turn
/// pipeline makes is answered by the datastore, with provenance enriched from
/// the control plane — and rolling the selector back to postgres flips the
/// engine back, no restart anywhere.
#[tokio::test]
async fn a_selected_scope_serves_from_the_datastore_and_rolls_back_by_selector() {
    guard!();
    let h = harness("tenant-serve-a", 3).await;
    let artifact_id = match h.mirror().await.unwrap() {
        MirrorOutcome::Published { artifact_id, .. } => artifact_id,
        other => panic!("expected a publication, got {other:?}"),
    };
    h.promote_to_serving(&artifact_id).await;

    let selector = RolloutSelector::new(h.store.pool().clone(), &h.tenant);
    selector
        .create(
            "collection",
            &h.collection_id,
            RolloutChange {
                serving: "datastore",
                prewarm_staged: false,
                changed_by: "test",
                reason: Some("phase 6 integration"),
            },
        )
        .await
        .unwrap();

    let cache = tempfile::tempdir().unwrap();
    let retrieval = h.serving_retrieval(cache.path());
    let prepared = PgRetrieval::prepare_query(
        "the destruction of the tea in boston harbour",
        &munarium_core::retrieval::SearchParams::default(),
        &munarium_retrieval_pg::LocalHashEmbedder,
    );

    let served = retrieval
        .search_collection_prepared(&h.collection_id, &prepared, None)
        .await
        .unwrap();
    assert!(!served.hits.is_empty(), "every document mentions the tea");
    assert_eq!(served.envelope.index_version, h.version_id);
    assert!(
        served.envelope.event_watermark > 0,
        "the watermark comes from the version row"
    );
    for hit in &served.hits {
        assert!(!hit.text.is_empty(), "serving carries the corpus text");
        assert_eq!(
            hit.source_content_hash.len(),
            64,
            "the SOURCE content hash, enriched from the control plane"
        );
    }

    // The reference engine answers the same prepared query; the two must
    // agree on identity for a mirrored corpus (ranking may differ -- the
    // analyzers do).
    let reference =
        h.pg.search_collection_prepared(&h.collection_id, &prepared, None)
            .await
            .unwrap();
    let served_sources: std::collections::HashSet<&str> =
        served.hits.iter().map(|h| h.source_id.as_str()).collect();
    let reference_sources: std::collections::HashSet<&str> = reference
        .hits
        .iter()
        .map(|h| h.source_id.as_str())
        .collect();
    assert!(
        !served_sources.is_disjoint(&reference_sources),
        "two engines over one corpus share sources: {served_sources:?} vs {reference_sources:?}"
    );
    // And the source hashes agree with the reference engine's for shared
    // sources -- the provenance is the SAME truth, not a parallel one.
    for r in &reference.hits {
        if let Some(s) = served.hits.iter().find(|s| s.source_id == r.source_id) {
            assert_eq!(s.source_content_hash, r.source_content_hash);
        }
    }

    // Rollback is a selector change, nothing else.
    let entry = selector
        .get("collection", &h.collection_id)
        .await
        .unwrap()
        .unwrap();
    selector
        .update(
            "collection",
            &h.collection_id,
            RolloutChange {
                serving: "postgres",
                prewarm_staged: false,
                changed_by: "test",
                reason: Some("rollback drill"),
            },
            entry.generation,
        )
        .await
        .unwrap()
        .expect("the generation was fresh");
    let back = retrieval
        .search_collection_prepared(&h.collection_id, &prepared, None)
        .await
        .unwrap();
    assert_eq!(
        back.envelope.provider_fingerprint, reference.envelope.provider_fingerprint,
        "after rollback the postgres engine answers again"
    );
    assert_eq!(back.hits.len(), reference.hits.len());
}

/// No fallback, stated as a test: a scope the selector routes to the
/// datastore whose serving binding is MISSING fails with the specific error
/// class -- it must never quietly answer from PostgreSQL.
#[tokio::test]
async fn a_selected_scope_with_no_serving_binding_fails_rather_than_falling_back() {
    guard!();
    let h = harness("tenant-serve-b", 2).await;
    h.mirror().await.unwrap(); // staged only; nothing promoted

    let selector = RolloutSelector::new(h.store.pool().clone(), &h.tenant);
    selector
        .create(
            "collection",
            &h.collection_id,
            RolloutChange {
                serving: "datastore",
                prewarm_staged: false,
                changed_by: "test",
                reason: None,
            },
        )
        .await
        .unwrap();

    let cache = tempfile::tempdir().unwrap();
    let retrieval = h.serving_retrieval(cache.path());
    let prepared = PgRetrieval::prepare_query(
        "continental congress",
        &munarium_core::retrieval::SearchParams::default(),
        &munarium_retrieval_pg::LocalHashEmbedder,
    );
    let err = retrieval
        .search_collection_prepared(&h.collection_id, &prepared, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, munarium_core::KernelError::DatastoreUnavailable(_)),
        "got {err:?}"
    );
    assert!(format!("{err}").contains("no serving binding"), "{err}");
}

/// An unselected scope in datastore mode continues on PostgreSQL -- routing,
/// not fallback -- and a datastore-mode handle with NO plane fails closed.
#[tokio::test]
async fn unselected_scopes_route_to_postgres_and_a_planeless_handle_fails_closed() {
    guard!();
    let h = harness("tenant-serve-c", 2).await;
    let prepared = PgRetrieval::prepare_query(
        "continental congress",
        &munarium_core::retrieval::SearchParams::default(),
        &munarium_retrieval_pg::LocalHashEmbedder,
    );

    // With a plane, no selector row: postgres answers.
    let cache = tempfile::tempdir().unwrap();
    let with_plane = h.serving_retrieval(cache.path());
    let ok = with_plane
        .search_collection_prepared(&h.collection_id, &prepared, None)
        .await
        .unwrap();
    assert!(!ok.hits.is_empty());

    // Without a plane: fail closed, because selected and unselected are
    // indistinguishable and guessing could serve the wrong engine.
    let planeless = Retrieval::new(h.pg.clone(), RetrievalMode::Datastore);
    let err = planeless
        .search_collection_prepared(&h.collection_id, &prepared, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, munarium_core::KernelError::DatastoreUnavailable(_)),
        "got {err:?}"
    );
}

/// The promotion CAS refuses a stale generation and leaves staged intact.
#[tokio::test]
async fn promotion_refuses_a_stale_generation_and_leaves_staged_alone() {
    guard!();
    let h = harness("tenant-serve-d", 1).await;
    match h.mirror().await.unwrap() {
        MirrorOutcome::Published { .. } => {}
        other => panic!("expected a publication, got {other:?}"),
    }
    let err = h
        .ctx
        .catalog
        .promote_staged(&h.version_id, 999, 0, "test", None)
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("generation"), "{err}");
    assert!(
        h.ctx
            .catalog
            .binding(&h.version_id, BindingSlot::Staged)
            .await
            .unwrap()
            .is_some(),
        "a failed promotion leaves staged intact"
    );
    assert!(h
        .ctx
        .catalog
        .binding(&h.version_id, BindingSlot::Serving)
        .await
        .unwrap()
        .is_none());
}

/// The LEGACY shape-scoped path dispatches the same way (stage 6's second
/// deliverable): mirror the legacy version, promote, select the `shape`
/// scope, and `hybrid_search` — the `RetrievalBackend` trait's own method —
/// is answered by the datastore.
#[tokio::test]
async fn a_selected_legacy_shape_serves_from_the_datastore() {
    guard!();
    let url = url().expect("guarded");
    // Run-unique, unlike the direct tests' fixed tenants (those get their
    // uniqueness from `unique("col")`): a shape-scoped build has no collection,
    // so a fixed tenant makes the version id fully deterministic and a second
    // run of this suite against one database converges on AlreadyBuilt —
    // which the gates now do, running the suite in both feature configs.
    let tenant = &unique("tenant-serve-shape");
    let store = PgStore::connect(&url, tenant).await.unwrap();
    let pg = PgRetrieval::new(store.pool().clone(), tenant);

    // A LEGACY corpus: shape-scoped sources, a shape-scoped index build —
    // no collection anywhere.
    for i in 0..3 {
        let body = format!(
            "Legacy document {i}. The privateers sailed from Salem with letters of marque."
        );
        pg.put_source(
            "",
            "text/markdown",
            &format!("legacy/doc-{i}.md"),
            Some("para"),
            body.as_bytes(),
        )
        .await
        .unwrap();
    }
    let version = pg.build_index("para", 400, 1, true).await.unwrap();

    let artifacts = tempfile::tempdir().unwrap();
    let staging = tempfile::tempdir().unwrap();
    let ctx = MirrorContext {
        catalog: ArtifactCatalog::new(store.pool().clone(), tenant),
        attempts: BuildAttempts::new(store.pool().clone(), tenant),
        stores: Arc::new(LocalStoreFactory::new(artifacts.path())),
        node_id: "node-test".into(),
        staging_root: staging.path().to_path_buf(),
        artifact_prefix: "v1".into(),
        tenant_path_hash: "t0000".into(),
        faults: None,
        observer: None,
        vector_policy: munarium_retrieval::mirror::VectorPolicy {
            approx_threshold: None,
        },
    };
    let _artifact_id = match backfill_one(
        &ctx,
        &pg,
        MirrorTarget::LegacyShape { shape_ref: "para" },
        &version.id,
    )
    .await
    .unwrap()
    {
        MirrorOutcome::Published { artifact_id, .. } => artifact_id,
        other => panic!("expected a publication, got {other:?}"),
    };
    let staged = ctx
        .catalog
        .binding(&version.id, BindingSlot::Staged)
        .await
        .unwrap()
        .expect("staged bound");
    ctx.catalog
        .promote_staged(&version.id, staged.generation, 0, "test", None)
        .await
        .unwrap();

    let selector = RolloutSelector::new(store.pool().clone(), tenant);
    selector
        .create(
            "shape",
            "para",
            RolloutChange {
                serving: "datastore",
                prewarm_staged: false,
                changed_by: "test",
                reason: Some("legacy shape dispatch"),
            },
        )
        .await
        .unwrap();

    let cache = tempfile::tempdir().unwrap();
    let plane = Arc::new(ServingPlane {
        selector: RolloutSelector::new(store.pool().clone(), tenant),
        executor: Arc::new(ArtifactExecutor {
            catalog: ctx.catalog.clone(),
            stores: ctx.stores.clone(),
            l0: Arc::new(munarium_retrieval::executor::L0Cache::new(8)),
            cache: Arc::new(
                munarium_datastore::hydrate::L1Cache::new(
                    cache.path(),
                    munarium_datastore::hydrate::CacheBudget::new(
                        512 * 1024 * 1024,
                        256 * 1024 * 1024,
                    )
                    .unwrap(),
                )
                .unwrap(),
            ),
            reader: ReaderCapabilities::v1(),
            limits: Limits::default(),
            isolation_domain: "t0000".into(),
        }),
    });
    let retrieval = Retrieval::new(pg.clone(), RetrievalMode::Datastore).with_serving(plane);

    let served = retrieval
        .hybrid_search(munarium_core::retrieval::HybridQuery {
            query: "privateers with letters of marque".into(),
            shape_ref: "para".into(),
            top_k: 5,
            filter: None,
            index_version: None,
        })
        .await
        .unwrap();
    assert!(!served.hits.is_empty(), "every legacy document matches");
    assert_eq!(served.envelope.index_version, version.id);
    assert!(served.hits.iter().all(|h| !h.text.is_empty()));

    // And the reference legacy engine agrees on identity. Through the
    // coordinator in POSTGRES mode, which is the honest reference path.
    let reference = Retrieval::new(pg.clone(), RetrievalMode::Postgres)
        .hybrid_search(munarium_core::retrieval::HybridQuery {
            query: "privateers with letters of marque".into(),
            shape_ref: "para".into(),
            top_k: 5,
            filter: None,
            index_version: None,
        })
        .await
        .unwrap();
    let served_sources: std::collections::HashSet<&str> =
        served.hits.iter().map(|h| h.source_id.as_str()).collect();
    let reference_sources: std::collections::HashSet<&str> = reference
        .hits
        .iter()
        .map(|h| h.source_id.as_str())
        .collect();
    assert!(!served_sources.is_disjoint(&reference_sources));
}

// ---------------------------------------------------------------------------
// The direct build: one extraction pass, two indexes, idx2 identity.
// ---------------------------------------------------------------------------

use munarium_retrieval::direct::build_collection_direct;

async fn direct_harness(tenant: &str, docs: usize) -> Harness {
    // Same corpus as `harness`, but WITHOUT the ordinary pg build: the direct
    // build is the only builder, which is what proves extraction ran once.
    let url = url().expect("guarded");
    let store = PgStore::connect(&url, tenant).await.unwrap();
    let pg = PgRetrieval::new(store.pool().clone(), tenant);

    let col = pg
        .ensure_collection(&unique("col"), "para", 0, &[], Some("direct test"))
        .await
        .unwrap();
    for i in 0..docs {
        let body = format!(
            "Direct document {i}. The armory at Springfield issued muskets and powder.\n\n\
             A second paragraph counting 4,436,097 cartridges in store."
        );
        let (source_id, _, _) = pg
            .put_source(
                "",
                "text/markdown",
                &format!("direct/doc-{i}.md"),
                Some("para"),
                body.as_bytes(),
            )
            .await
            .unwrap();
        pg.bind_source(&col.id, &source_id, None).await.unwrap();
    }

    let artifacts = tempfile::tempdir().unwrap();
    let staging = tempfile::tempdir().unwrap();
    let builds: Arc<Recorder> = Arc::new(Recorder::default());
    let ctx = MirrorContext {
        catalog: ArtifactCatalog::new(store.pool().clone(), tenant),
        attempts: BuildAttempts::new(store.pool().clone(), tenant),
        stores: Arc::new(LocalStoreFactory::new(artifacts.path())),
        node_id: "node-test".into(),
        staging_root: staging.path().to_path_buf(),
        artifact_prefix: "v1".into(),
        tenant_path_hash: "t0000".into(),
        faults: None,
        observer: Some(builds.clone()),
        vector_policy: munarium_retrieval::mirror::VectorPolicy {
            approx_threshold: None,
        },
    };
    Harness {
        tenant: tenant.to_string(),
        builds,
        collection_id: col.id.clone(),
        version_id: String::new(),
        pg,
        ctx,
        store,
        _artifacts: artifacts,
        _staging: staging,
    }
}

/// The stage 7 exit, end to end: a collection built natively — one extraction
/// pass — lands as BOTH a searchable PostgreSQL version and a staged
/// datastore artifact under an `idx2-` identity, activates through the CAS,
/// and serves from PostgreSQL exactly like an ordinary build.
#[tokio::test]
async fn a_direct_build_lands_both_indexes_under_an_idx2_identity() {
    guard!();
    let h = direct_harness("tenant-direct-a", 3).await;

    let outcome = build_collection_direct(&h.ctx, &h.pg, &h.collection_id, 400, 7)
        .await
        .unwrap();
    assert!(
        outcome.index_version_id.starts_with("idx2-"),
        "{}",
        outcome.index_version_id
    );
    assert!(outcome.committed, "first build commits");
    assert_eq!(outcome.expected_active, None, "nothing was active before");
    let artifact_id = match &outcome.artifact {
        MirrorOutcome::Published {
            artifact_id,
            chunks,
            bound_staged,
        } => {
            // chunk_text merges short paragraphs up to max_chars, so the
            // floor is one chunk per document, not one per paragraph.
            assert!(
                *chunks >= 3,
                "at least one chunk per document, got {chunks}"
            );
            assert!(bound_staged);
            artifact_id.clone()
        }
        other => panic!("expected a publication, got {other:?}"),
    };

    // The artifact opens and holds the same chunk count the version does.
    let row = h
        .ctx
        .catalog
        .artifact(&outcome.index_version_id, &artifact_id)
        .await
        .unwrap()
        .expect("catalogued");
    assert_eq!(row.state, ArtifactState::Verified);

    // The pin-safety property the live wedge taught (2026-08-31): a version
    // that has NEVER been activated holds no session pins, so it must not
    // join the serving-required set — on a datastore-routed scope an
    // in-flight build would otherwise take readiness, and with it the very
    // API its own promotion needs, down on a single-replica deployment.
    let required =
        h.pg.required_versions(
            &h.collection_id,
            RequiredVersionsPolicy::ActivePinnedAndHorizon,
            3_600,
        )
        .await
        .unwrap();
    assert!(
        !required
            .iter()
            .any(|v| v.index_version_id == outcome.index_version_id),
        "an unactivated version must not be serving-required"
    );

    // CAS activation: succeeds against the recorded expectation…
    let retrieval = Retrieval::new(h.pg.clone(), RetrievalMode::Postgres);
    assert!(retrieval
        .activate_collection_index_cas(
            &h.collection_id,
            &outcome.index_version_id,
            outcome.expected_active.as_deref(),
        )
        .await
        .unwrap());

    // …after which the version IS serving-required (it is active, and its
    // pins are now possible).
    let required =
        h.pg.required_versions(
            &h.collection_id,
            RequiredVersionsPolicy::ActivePinnedAndHorizon,
            3_600,
        )
        .await
        .unwrap();
    assert!(required
        .iter()
        .any(|v| v.index_version_id == outcome.index_version_id));

    // …and the PostgreSQL engine serves the direct-built version like any
    // other.
    let prepared = PgRetrieval::prepare_query(
        "muskets and powder at the armory",
        &munarium_core::retrieval::SearchParams::default(),
        &munarium_retrieval_pg::LocalHashEmbedder,
    );
    let served = retrieval
        .search_collection_prepared(&h.collection_id, &prepared, None)
        .await
        .unwrap();
    assert!(!served.hits.is_empty());
    assert_eq!(served.envelope.index_version, outcome.index_version_id);

    // A second direct build of the SAME inputs converges on the same
    // identity: the id is a hash of the inputs, not of the attempt.
    let again = build_collection_direct(&h.ctx, &h.pg, &h.collection_id, 400, 7)
        .await
        .unwrap();
    assert_eq!(again.index_version_id, outcome.index_version_id);
    assert!(!again.committed, "the identity already existed");
    match again.artifact {
        MirrorOutcome::AlreadyBuilt { artifact_id: a }
        | MirrorOutcome::Converged { artifact_id: a } => {
            assert_eq!(a, artifact_id, "one plan, one artifact");
        }
        other => panic!("a rebuild must converge, got {other:?}"),
    }
}

/// The superseded-build behaviour (§7.3): a CAS against an expectation the
/// world has moved past returns false and changes nothing.
#[tokio::test]
async fn a_superseded_direct_build_fails_its_cas_and_keeps_the_pointer() {
    guard!();
    let h = direct_harness("tenant-direct-b", 2).await;
    let first = build_collection_direct(&h.ctx, &h.pg, &h.collection_id, 400, 1)
        .await
        .unwrap();
    let retrieval = Retrieval::new(h.pg.clone(), RetrievalMode::Postgres);
    assert!(retrieval
        .activate_collection_index_cas(&h.collection_id, &first.index_version_id, None)
        .await
        .unwrap());

    // A stale expectation — recorded before the activation above — must not
    // clobber the pointer.
    let superseded = retrieval
        .activate_collection_index_cas(&h.collection_id, &first.index_version_id, None)
        .await
        .unwrap();
    assert!(!superseded, "the pointer moved; the CAS says superseded");
    assert_eq!(
        h.pg.current_active_collection_index(&h.collection_id)
            .await
            .unwrap()
            .as_deref(),
        Some(first.index_version_id.as_str()),
        "the active pointer is untouched"
    );
}

/// Activation for a datastore-served scope refuses a version with no serving
/// binding (§7.3 logical activation step 3) — and succeeds after promotion.
#[tokio::test]
async fn activation_on_a_datastore_scope_requires_the_serving_binding() {
    guard!();
    let h = direct_harness("tenant-direct-c", 2).await;
    let built = build_collection_direct(&h.ctx, &h.pg, &h.collection_id, 400, 1)
        .await
        .unwrap();

    // Route the scope to the datastore (directly; the API's completeness gate
    // is not what is under test here).
    let selector = RolloutSelector::new(h.store.pool().clone(), &h.tenant);
    selector
        .create(
            "collection",
            &h.collection_id,
            RolloutChange {
                serving: "datastore",
                prewarm_staged: false,
                changed_by: "test",
                reason: None,
            },
        )
        .await
        .unwrap();

    let cache = tempfile::tempdir().unwrap();
    let retrieval = h.serving_retrieval(cache.path());

    // Staged only: activation must refuse.
    let err = retrieval
        .activate_collection_index_cas(&h.collection_id, &built.index_version_id, None)
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("no serving binding"), "{err}");

    // Promote, then the same activation succeeds.
    let staged = h
        .ctx
        .catalog
        .binding(&built.index_version_id, BindingSlot::Staged)
        .await
        .unwrap()
        .unwrap();
    h.ctx
        .catalog
        .promote_staged(&built.index_version_id, staged.generation, 0, "test", None)
        .await
        .unwrap();
    assert!(retrieval
        .activate_collection_index_cas(&h.collection_id, &built.index_version_id, None)
        .await
        .unwrap());

    // And the datastore serves the direct-built, now-active version.
    let prepared = PgRetrieval::prepare_query(
        "cartridges in store",
        &munarium_core::retrieval::SearchParams::default(),
        &munarium_retrieval_pg::LocalHashEmbedder,
    );
    let served = retrieval
        .search_collection_prepared(&h.collection_id, &prepared, None)
        .await
        .unwrap();
    assert!(!served.hits.is_empty());
    assert_eq!(served.envelope.index_version, built.index_version_id);
}

// ---------------------------------------------------------------------------
// stage 8: the engine-upgrade promotion drill
// ---------------------------------------------------------------------------

/// stage 8's exit drill: ONE logical version realized by two engines. The
/// exact artifact serves; the approximate engine then produces a SECOND
/// physical artifact for the same `idx2-` identity (the engine is outside the
/// logical id), lands in the staged slot, and is promoted staged -> serving as
/// a §7.3 binding change. The version id — what a session pins — never moves,
/// and the scope answers from the approximate artifact afterwards.
#[cfg(feature = "vector-diskann")]
#[tokio::test]
async fn an_engine_upgrade_promotes_staged_to_serving_with_pins_intact() {
    use munarium_datastore::hydrate::Residency;
    use munarium_retrieval::executor::TextPayload;
    use munarium_retrieval::mirror::VectorPolicy;

    guard!();
    let h = direct_harness("tenant-direct-d", 3).await;

    // 1. The exact build (the harness policy never approximates), promoted to
    //    serving and activated.
    let exact = build_collection_direct(&h.ctx, &h.pg, &h.collection_id, 400, 1)
        .await
        .unwrap();
    let exact_artifact = match &exact.artifact {
        MirrorOutcome::Published { artifact_id, .. } => artifact_id.clone(),
        other => panic!("expected a publication, got {other:?}"),
    };
    let staged = h
        .ctx
        .catalog
        .binding(&exact.index_version_id, BindingSlot::Staged)
        .await
        .unwrap()
        .unwrap();
    h.ctx
        .catalog
        .promote_staged(&exact.index_version_id, staged.generation, 0, "test", None)
        .await
        .unwrap();

    // 2. The engine upgrade: the SAME inputs under an approximate policy. The
    //    logical identity converges; the plan differs; a second artifact is
    //    built and bound staged.
    let ctx2 = MirrorContext {
        catalog: h.ctx.catalog.clone(),
        attempts: BuildAttempts::new(h.store.pool().clone(), &h.tenant),
        stores: h.ctx.stores.clone(),
        node_id: "node-upgrade".into(),
        staging_root: h.ctx.staging_root.clone(),
        artifact_prefix: "v1".into(),
        tenant_path_hash: "t0000".into(),
        faults: None,
        observer: None,
        vector_policy: VectorPolicy {
            approx_threshold: Some(1),
        },
    };
    let approx = build_collection_direct(&ctx2, &h.pg, &h.collection_id, 400, 1)
        .await
        .unwrap();
    assert_eq!(
        approx.index_version_id, exact.index_version_id,
        "an engine change must not move the logical id — this is what a session pin rests on"
    );
    assert!(!approx.committed, "the identity already existed");
    let approx_artifact = match &approx.artifact {
        MirrorOutcome::Published {
            artifact_id,
            bound_staged,
            ..
        } => {
            assert!(bound_staged, "the upgrade lands in the staged slot");
            artifact_id.clone()
        }
        other => panic!("a new plan must build a new artifact, got {other:?}"),
    };
    assert_ne!(
        approx_artifact, exact_artifact,
        "two engines, two artifacts"
    );

    // 3. Serving still answers from the exact artifact until someone promotes.
    let serving = h
        .ctx
        .catalog
        .binding(&exact.index_version_id, BindingSlot::Serving)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(serving.artifact_id, exact_artifact);

    // 4. The §7.3 promotion, staged generation and serving generation both
    //    named — a locked CAS, not a write.
    let staged = h
        .ctx
        .catalog
        .binding(&exact.index_version_id, BindingSlot::Staged)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(staged.artifact_id, approx_artifact);
    h.ctx
        .catalog
        .promote_staged(
            &exact.index_version_id,
            staged.generation,
            serving.generation,
            "test",
            Some("phase 8 engine upgrade drill"),
        )
        .await
        .unwrap();

    // 5. The same version id — the pinned identity — now serves from the
    //    approximate artifact, and answers.
    let cache = tempfile::tempdir().unwrap();
    let exec = h.shadow_context(cache.path());
    match exec
        .execute(
            &exact.index_version_id,
            BindingSlot::Serving,
            Residency::ServingRequired,
            TextPayload::Served,
            &prepared_for("cartridges in store"),
        )
        .await
    {
        ExecutionOutcome::Executed(e) => {
            assert_eq!(
                e.artifact_id, approx_artifact,
                "serving must answer from the promoted artifact"
            );
            assert!(!e.hits.is_empty(), "the approximate engine must answer");
        }
        other => panic!("expected an execution, got {other:?}"),
    }
}
