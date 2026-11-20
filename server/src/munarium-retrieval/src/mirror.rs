// SPDX-License-Identifier: Apache-2.0
//! Mirror builds: §7.1's eleven steps, in the order that makes them safe.
//!
//! A mirror build turns an existing PostgreSQL index version into a datastore
//! artifact **without re-running extraction, chunking or embedding**. The
//! logical version id is the one PostgreSQL already assigned; only a physical
//! artifact is new. So a mirror never creates or activates a logical version
//! and never invalidates a session's pin — it adds an implementation of
//! something that already exists. That is also what makes stage 5's shadow
//! comparison meaningful: a difference between the two engines is attributable
//! to the engine, because the content was prepared once.
//!
//! ## The ordering is the safety property
//!
//! Content is sealed into a local staging directory, the catalog row is written
//! as `sealed`, components are uploaded, and the canonical manifest is written
//! **last** and read back through the same path a query node uses. Every other
//! order has a window in which something advertises an artifact that is not
//! fully there:
//!
//! - manifest before components → a reader finds a manifest naming files that
//!   do not exist;
//! - `verified` before the read-back → the catalog advertises an artifact whose
//!   stored bytes nobody has checked;
//! - catalog row after upload → two builders can both upload to one prefix
//!   before either discovers the other.
//!
//! ## What a mirror never does
//!
//! It inserts a `staged` binding when the version has none. It never writes
//! `serving`, never touches the collection's active pointer, and never fails a
//! user request: mirror failures are telemetry (§9.2).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use munarium_core::{KernelError, Result};

use crate::build_metrics::{BuildMetrics, BuildTimer, Observer};
use munarium_datastore::model::*;
use munarium_datastore::shard::{SealedArtifact, ShardWriter, MANIFEST};
use munarium_datastore::store::{ArtifactStore, LocalFileStore};
use munarium_datastore::PreparedChunk;
use munarium_retrieval_pg::export::{ExportStats, ExportedSource};
use munarium_retrieval_pg::PgRetrieval;
use munarium_store_pg::artifacts::{
    ArtifactCatalog, ArtifactState, BindingSlot, FencedBind, InsertOutcome, NewArtifact,
};
use munarium_store_pg::attempts::{
    reconcile_sealed, AttemptMode, AttemptState, BuildAttempts, ClaimOutcome, IntentState,
    PublicationIntent, SealedVerdict,
};
use munarium_store_pg::jobs::{JobFence, JobSignal, ProgressPhase, StopReason};

/// Supplies an `ArtifactStore` rooted at a prefix.
///
/// A trait rather than a concrete store, so the coordinator stays free of cloud
/// specifics: Server injects a factory that builds Azure clients, tests inject
/// one over a temporary directory. It also means the coordinator never opens a
/// store by naming a container — it asks for a prefix and is handed something
/// that can only see inside it.
pub trait ArtifactStoreFactory: Send + Sync + std::fmt::Debug {
    fn store_for_prefix(&self, prefix: &str) -> Result<Arc<dyn ArtifactStore>>;
}

/// A factory over a local directory, for development and tests.
#[derive(Debug, Clone)]
pub struct LocalStoreFactory {
    root: PathBuf,
}

impl LocalStoreFactory {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl ArtifactStoreFactory for LocalStoreFactory {
    fn store_for_prefix(&self, prefix: &str) -> Result<Arc<dyn ArtifactStore>> {
        // Every prefix segment is composed by this crate from hex ids and
        // already-validated names, but it is still checked rather than trusted,
        // and `LocalFileStore` re-normalizes each component path on top.
        for segment in prefix.split('/') {
            if segment.is_empty() || segment == "." || segment == ".." {
                return Err(KernelError::InvalidInput(format!(
                    "refusing artifact prefix {prefix:?}"
                )));
            }
        }
        let store = LocalFileStore::new(self.root.join(prefix))
            .map_err(|e| KernelError::Storage(format!("artifact store: {e}")))?;
        Ok(Arc::new(store))
    }
}

/// Points a build can be interrupted at, for the §15.4 failure injection.
///
/// Declared in the ordinary type rather than behind `#[cfg(test)]` because the
/// tests that exercise it are integration tests outside this crate, and because
/// a failure-injection seam that exists in only one build configuration is one
/// the shipped configuration has never run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildPhase {
    AfterClaim,
    AfterExport,
    AfterSeal,
    AfterCatalogInsert,
    AfterComponentUpload,
    AfterManifestWrite,
    BeforeBinding,
    /// A queued direct build committed its PostgreSQL logical version and
    /// has not yet acquired an artifact attempt (ClearView R6).
    AfterLogicalCommit,
}

/// Returns `Err` to interrupt the build at that phase.
pub type FaultHook = Arc<dyn Fn(BuildPhase) -> Result<()> + Send + Sync>;

/// What a mirror build did.
#[derive(Debug, Clone, PartialEq)]
pub enum MirrorOutcome {
    /// A new artifact was sealed, published and verified, and bound to `staged`
    /// if the version had no `staged` binding.
    Published {
        artifact_id: String,
        chunks: u64,
        bound_staged: bool,
    },
    /// An identical artifact was already catalogued and verified. The local
    /// output was discarded — this is success, not failure.
    Converged { artifact_id: String },
    /// Another node holds the lease for this exact plan.
    AlreadyRunning { owner_node_id: String },
    /// A verified artifact for this plan existed before any work began.
    AlreadyBuilt { artifact_id: String },
}

/// Which index a mirror is building from.
#[derive(Debug, Clone, Copy)]
pub enum MirrorTarget<'a> {
    Collection {
        collection_id: &'a str,
    },
    /// The legacy shape-scoped `index_chunks` path.
    LegacyShape {
        shape_ref: &'a str,
    },
}

impl MirrorTarget<'_> {
    fn scope(&self) -> Scope {
        match self {
            Self::Collection { collection_id } => Scope {
                kind: ScopeKind::Collection,
                id: (*collection_id).to_string(),
            },
            Self::LegacyShape { shape_ref } => Scope {
                kind: ScopeKind::LegacyShape,
                id: (*shape_ref).to_string(),
            },
        }
    }

    fn path_kind(&self) -> &'static str {
        match self {
            Self::Collection { .. } => "collection",
            Self::LegacyShape { .. } => "legacy",
        }
    }
}

/// Everything a mirror build needs that is not the data itself.
#[derive(Clone)]
pub struct MirrorContext {
    pub catalog: ArtifactCatalog,
    pub attempts: BuildAttempts,
    pub stores: Arc<dyn ArtifactStoreFactory>,
    /// Opaque, stable per-process id. Used for the lease, and for deciding at
    /// reconciliation whether an interrupted attempt was ours.
    pub node_id: String,
    /// Where a build stages content before it is published. Local by
    /// definition: staging is L1 work, and a staging directory inside object
    /// storage would be indistinguishable from a published artifact.
    pub staging_root: PathBuf,
    /// The artifact-tree prefix, e.g. `v1`.
    pub artifact_prefix: String,
    /// An opaque, stable per-tenant path element. Server derives it from the
    /// already-authorized tenant; it is never the tenant id in clear, because
    /// object keys appear in storage inventories and access logs.
    pub tenant_path_hash: String,
    /// Interrupt hook. `None` in every real deployment.
    pub faults: Option<FaultHook>,
    /// Where build cost is reported. `None` records nothing.
    pub observer: Option<Observer>,
    /// The exact-vs-approximate vector decision for DIRECT builds (§6.3).
    /// Mirror builds ignore it: the mirror is the reference reconstruction
    /// and stays exact. Lives here rather than in an env read inside the
    /// build, so a test can pin it and the server reads env at its edge.
    pub vector_policy: VectorPolicy,
    /// The job claim a QUEUED build runs under (ClearView R4). `None` for a
    /// synchronous build, which keeps its existing artifact-attempt authority
    /// unchanged. With `Some`, the build links its attempt to the claim,
    /// commits a publication intent before cataloguing or uploading, and
    /// binds `staged` only while the claim's generation is current.
    pub job_fence: Option<JobFence>,
    /// The worker-local stop signal for that claim (ClearView R6). The
    /// builder stops it on a confirmed cancel or lease loss; the build checks
    /// it, and revalidates the claim, at each safe boundary
    /// ([`MirrorContext::checkpoint`]). `None` for a synchronous build.
    pub job_signal: Option<JobSignal>,
}

impl std::fmt::Debug for MirrorContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // No tenant path hash and no staging path: this type is formatted on
        // failure paths, and neither belongs in a log line.
        f.debug_struct("MirrorContext")
            .field("node_id", &self.node_id)
            .field("artifact_prefix", &self.artifact_prefix)
            .field("faults", &self.faults.is_some())
            .field("observer", &self.observer.is_some())
            .field("job_fence", &self.job_fence.is_some())
            .field("job_signal", &self.job_signal)
            .finish_non_exhaustive()
    }
}

impl MirrorContext {
    /// The immutable prefix an artifact's components live under (§5.2).
    fn artifact_prefix_for(
        &self,
        target: MirrorTarget<'_>,
        scope_id: &str,
        version: &str,
        artifact: &str,
    ) -> String {
        format!(
            "{}/{}/{}/{}/{}/{}",
            self.artifact_prefix,
            self.tenant_path_hash,
            target.path_kind(),
            scope_id,
            version,
            artifact
        )
    }

    fn staging_dir(&self, attempt_id: &str) -> PathBuf {
        self.staging_root.join(attempt_id)
    }

    fn fault(&self, phase: BuildPhase) -> Result<()> {
        match &self.faults {
            Some(h) => h(phase),
            None => Ok(()),
        }
    }

    /// Run the failure-injection hook at `phase`. Public so the direct build,
    /// which lives in its own module, can expose its own pause points.
    pub fn inject(&self, phase: BuildPhase) -> Result<()> {
        self.fault(phase)
    }

    /// Stop with the fence's stale error if the worker-local signal has
    /// already stopped, without a database round trip. Writes no progress.
    /// A synchronous build always continues.
    pub fn stop_if_signalled(&self) -> Result<()> {
        match (&self.job_fence, &self.job_signal) {
            (Some(fence), Some(signal)) if signal.stopped().is_some() => Err(fence.stale()),
            _ => Ok(()),
        }
    }

    /// A safe boundary of a queued build (ClearView R6): between sources,
    /// between versions, before seal and before upload.
    ///
    /// Stops the build with the fence's stale error when the worker-local
    /// signal has stopped, or when the claim is no longer current in
    /// PostgreSQL. The check is the fenced progress write (ClearView R9):
    /// `phase` and the counters are recorded only while the claim is
    /// current, so the same statement is how a build learns of a cancel that
    /// committed since its last boundary, and how it revalidates a lease
    /// whose last renewal failed with an error. If PostgreSQL cannot answer,
    /// the build does not go on to its next shared write. `None` counters
    /// keep their recorded values. A synchronous build has no fence and
    /// always continues.
    ///
    /// A DRAIN does not stop the build at [`ProgressPhase::Uploading`] (see
    /// [`drain_finishes`]): past that point the attempt is sealed and
    /// catalogued under a committed intent, the claim is still current, and
    /// finishing is the only way the current generation can bind it. The
    /// claim is still revalidated there, so a cancel or lost lease stops the
    /// build exactly as before (ClearView R6).
    pub async fn checkpoint(
        &self,
        phase: ProgressPhase,
        units_done: Option<u64>,
        units_total: Option<u64>,
    ) -> Result<()> {
        let Some(fence) = &self.job_fence else {
            return Ok(());
        };
        if let Some(signal) = &self.job_signal {
            match signal.stopped() {
                None => {}
                Some(StopReason::Draining) if drain_finishes(phase) => {
                    tracing::info!(
                        job_id = %fence.job_id,
                        generation = fence.generation,
                        "queued build draining past its publication intent; finishing the upload"
                    );
                }
                Some(_) => return Err(fence.stale()),
            }
        }
        let current = self
            .attempts
            .record_job_progress(fence, phase, units_done, units_total)
            .await?;
        let reason = if current {
            None
        } else {
            Some(
                self.attempts
                    .job_claim_stop_reason(fence)
                    .await?
                    .unwrap_or(StopReason::LeaseLost),
            )
        };
        match reason {
            None => {
                if let Some(signal) = &self.job_signal {
                    signal.set_uncertain(false);
                }
                Ok(())
            }
            Some(reason) => {
                if let Some(signal) = &self.job_signal {
                    signal.stop(reason);
                }
                tracing::info!(
                    job_id = %fence.job_id,
                    generation = fence.generation,
                    cancelled = reason == StopReason::Cancelled,
                    "queued build stopping at a safe boundary"
                );
                Err(fence.stale())
            }
        }
    }
}

/// Whether a DRAINING builder carries on through the safe boundary at
/// `phase` rather than stopping there (SCALE-2).
///
/// Only the boundary before upload. A build there has sealed its attempt,
/// committed its publication intent and catalogued the artifact while its
/// claim was current. Stopping would release the claim to a replacement
/// while the attempt stays `sealed` under a generation that can no longer
/// bind, and only the owning node's reconciler could still publish it. A
/// draining claim is still current, so the build finishes and binds within
/// the shutdown grace. If the grace runs out first the process is aborted,
/// the claim lapses as after a crash, and the replacement generation waits
/// for the sealed attempt rather than deferring to it (see
/// [`await_earlier_generation`]). Every earlier boundary still stops: the
/// attempt is then failed and its staging removed, and the replacement
/// builds afresh. A cancel or lost lease stops at every boundary.
pub fn drain_finishes(phase: ProgressPhase) -> bool {
    phase == ProgressPhase::Uploading
}

/// How often a queued build re-examines a plan that an earlier generation
/// of its own job still holds (see [`await_earlier_generation`]).
const EARLIER_GENERATION_POLL: std::time::Duration = std::time::Duration::from_secs(1);

/// What acquiring a plan's single-flight attempt came to.
enum Acquired {
    /// This build claimed a fresh attempt and does the work.
    Claimed(String),
    /// No work for this build: the outcome to report.
    Finished(MirrorOutcome),
}

/// Steps 3a and 3b, shared by mirror and direct builds: reuse a verified
/// artifact, defer to a live attempt on the plan, or claim a new attempt.
///
/// A queued build does NOT defer to an attempt an earlier generation of its
/// own job still holds. That generation is retired and never binds, so
/// deferring would let the job finish with the version unbound, and with
/// nothing published once that attempt's owner is gone. It waits instead,
/// at safe boundaries, until the attempt resolves; then the loop runs
/// again: a verified artifact is reused and bound by the current
/// generation, and a failed or lapsed attempt leaves the plan free to
/// claim. A retired generation still never binds, and only its recorded
/// intent can finish (ClearView R4).
async fn acquire_attempt(
    ctx: &MirrorContext,
    index_version_id: &str,
    plan_hash: &str,
    mode: AttemptMode,
) -> Result<Acquired> {
    loop {
        // Step 3a: reuse. A verified artifact for this exact plan means the
        // work is already done, and doing it again would spend a build
        // producing the same bytes under a new attempt.
        for existing in ctx.catalog.artifacts_for_version(index_version_id).await? {
            if existing.artifact_plan_sha256 == plan_hash
                && existing.state == ArtifactState::Verified
            {
                // A queued build under the CURRENT claim fills an empty
                // `staged` slot for the artifact it reuses. A retired
                // generation that finished its recorded intent never binds,
                // so without this the replacement would leave the version
                // verified but unbound.
                if let Some(fence) = &ctx.job_fence {
                    bind_staged_fenced(ctx, index_version_id, &existing.artifact_id, fence).await?;
                    record_disposition(ctx, fence, index_version_id, "reused").await;
                }
                return Ok(Acquired::Finished(MirrorOutcome::AlreadyBuilt {
                    artifact_id: existing.artifact_id,
                }));
            }
        }

        // An earlier generation of this job still holds the plan: wait for
        // it rather than deferring to it. Checked BEFORE the deferrals below,
        // so an attempt seen there cannot be that generation's: a retired
        // generation links no new attempt, and one it already held would
        // have been seen here.
        if await_earlier_generation(ctx, index_version_id, plan_hash).await? {
            continue;
        }

        // An attempt that sealed and then failed before publishing is still
        // in-flight work on this plan, but it has dropped out of the
        // `running` single-flight index. Rebuilding past it would duplicate
        // the work and — because the lexical engine is not byte-deterministic
        // — produce a SECOND artifact id for one plan. Defer to it while its
        // lease is fresh; once the lease lapses the reconciler abandons it
        // and this proceeds.
        if let Some(sealed) = ctx
            .attempts
            .sealed_for_plan(index_version_id, plan_hash)
            .await?
        {
            if let Some(fence) = &ctx.job_fence {
                record_disposition(ctx, fence, index_version_id, "deferred").await;
            }
            return Ok(Acquired::Finished(MirrorOutcome::AlreadyRunning {
                owner_node_id: sealed.owner_node_id,
            }));
        }

        // Step 3b: claim. Single-flight is the database's decision, not a
        // read-then-write here.
        match ctx
            .attempts
            .claim(
                index_version_id,
                plan_hash,
                mode,
                &ctx.node_id,
                // The ROOT, because the attempt id that names this build's
                // own directory is minted by the claim itself. The directory
                // is always `<this>/<attempt_id>`, which is how the
                // reconciler finds it, so recording the root states
                // something true rather than a path that will not exist.
                ctx.staging_root.to_str(),
            )
            .await?
        {
            ClaimOutcome::Claimed(id) => return Ok(Acquired::Claimed(id)),
            ClaimOutcome::AlreadyRunning { owner_node_id } => {
                if let Some(fence) = &ctx.job_fence {
                    record_disposition(ctx, fence, index_version_id, "deferred").await;
                }
                return Ok(Acquired::Finished(MirrorOutcome::AlreadyRunning {
                    owner_node_id,
                }));
            }
        }
    }
}

/// Wait one poll for a plan an earlier generation of this queued build's
/// own job still holds. `Ok(false)` at once when no such attempt exists, or
/// for a synchronous build.
///
/// Each poll is a safe boundary: a cancel, lost lease or drain stops the
/// waiting build there (a drain then releases the claim, which holds no
/// work yet). The wait is bounded by the attempt's own lease: it ends when
/// the attempt publishes, fails, or lapses unrenewed.
async fn await_earlier_generation(
    ctx: &MirrorContext,
    index_version_id: &str,
    plan_hash: &str,
) -> Result<bool> {
    let Some(fence) = &ctx.job_fence else {
        return Ok(false);
    };
    if !ctx
        .attempts
        .earlier_generation_holds_plan(fence, index_version_id, plan_hash)
        .await?
    {
        return Ok(false);
    }
    ctx.checkpoint(ProgressPhase::Publishing, None, None)
        .await?;
    tracing::debug!(
        job_id = %fence.job_id,
        generation = fence.generation,
        "an earlier generation of this build job still holds the plan; waiting for it"
    );
    tokio::time::sleep(EARLIER_GENERATION_POLL).await;
    Ok(true)
}

/// How often the lease is extended while chunks stream.
///
/// The export's callback is synchronous, so the heartbeat runs as its own task
/// rather than being counted off inside the loop: a counter that only advances
/// between pages stops advancing exactly when a page is slow, which is when the
/// lease matters.
const HEARTBEAT_SECS: u64 = 30;

/// Build a datastore artifact from an existing PostgreSQL index version.
///
/// `spec` describes the logical corpus as best it can be reconstructed (see
/// [`reconstructed_spec`]) and `plan` describes the physical realization. The
/// caller supplies both, so an engine upgrade is a new plan rather than a
/// hidden default.
/// Where a build's chunks come from.
///
/// A MIRROR re-reads chunks PostgreSQL already committed (the exporter); a
/// DIRECT build carries the chunks it just extracted, so provider
/// and extraction work happens once and fans to both sinks. One publication
/// path serves both — the §7.1 steps do not care where the stream began.
pub enum ChunkFeed<'a> {
    /// Stream committed chunks back out of PostgreSQL.
    Export(MirrorTarget<'a>),
    /// The chunks are already in hand.
    Prepared(Vec<PreparedChunk>),
}

pub async fn mirror_index(
    ctx: &MirrorContext,
    pg: &PgRetrieval,
    target: MirrorTarget<'_>,
    index_version_id: &str,
    spec: &BuildSpec,
    plan: &ArtifactBuildPlan,
) -> Result<MirrorOutcome> {
    if !spec.reconstructed {
        return Err(KernelError::InvalidInput(
            "a mirror build's BuildSpec must be marked reconstructed: it is a best reassembly of \
             inputs nobody recorded, and its hash must never be used as replay input"
                .into(),
        ));
    }
    if spec.scope != target.scope() {
        return Err(KernelError::InvalidInput(format!(
            "the spec's scope {:?} is not the target being mirrored",
            spec.scope.id
        )));
    }
    let plan_hash = plan
        .plan_sha256()
        .map_err(|e| KernelError::InvalidInput(format!("canonical plan: {e}")))?;

    let attempt_id =
        match acquire_attempt(ctx, index_version_id, &plan_hash, AttemptMode::Mirror).await? {
            Acquired::Claimed(id) => id,
            Acquired::Finished(done) => {
                let outcome = Ok(done);
                report(ctx, "mirror", &BuildTimer::start(), &outcome);
                return outcome;
            }
        };

    // Everything past the claim must reach a terminal attempt state or the
    // lease leaks until it expires. Run the body once and record the outcome.
    let mut timer = BuildTimer::start();
    let outcome = mirror_body(
        ctx,
        pg,
        ChunkFeed::Export(target),
        target,
        index_version_id,
        spec,
        plan,
        &attempt_id,
        &mut timer,
    )
    .await;
    report(ctx, "mirror", &timer, &outcome);
    match &outcome {
        Ok(MirrorOutcome::Published { .. }) => {
            ctx.attempts.mark_succeeded(&attempt_id).await?;
            cleanup_staging(&ctx.staging_dir(&attempt_id));
        }
        Ok(MirrorOutcome::Converged { artifact_id }) => {
            ctx.attempts
                .mark_converged(&attempt_id, artifact_id)
                .await?;
            cleanup_staging(&ctx.staging_dir(&attempt_id));
        }
        Ok(_) => {
            ctx.attempts.mark_cancelled(&attempt_id).await.ok();
            cleanup_staging(&ctx.staging_dir(&attempt_id));
        }
        Err(e) => {
            // A failure that got as far as `sealed` is LEFT SEALED, with its
            // staging directory intact. That state is the whole recovery
            // mechanism: §7.4's reconciler decides whether the publication can
            // be resumed by asking whether the content is still there, and both
            // marking the attempt failed and deleting the directory would
            // answer that question with "no" before anyone asked it.
            //
            // Nothing leaks by leaving it. `sealed` is excluded from the
            // running single-flight index, so it blocks no rebuild, and
            // `expire_stale` deliberately does not touch it; once the lease
            // lapses, any node may abandon it.
            let sealed = ctx
                .attempts
                .get(&attempt_id)
                .await
                .ok()
                .flatten()
                .map(|r| r.state == AttemptState::Sealed)
                .unwrap_or(false);
            if sealed {
                tracing::warn!(
                    error = %e,
                    attempt_id = %attempt_id,
                    "a sealed mirror build failed before publication; leaving it for the reconciler"
                );
            } else {
                // A queued build that recorded an intent and then failed
                // before sealing will never publish it.
                if ctx.job_fence.is_some() {
                    fail_pending_intent(ctx, &attempt_id, &e.to_string()).await;
                }
                ctx.attempts
                    .mark_failed(&attempt_id, "mirror_build_failed", &e.to_string())
                    .await
                    .ok();
                cleanup_staging(&ctx.staging_dir(&attempt_id));
            }
        }
    }
    outcome
}

#[allow(clippy::too_many_arguments)]
/// Publish an artifact from chunks ALREADY IN HAND — the stage 7 direct
/// build's publication half. Identical to `mirror_index` past the guard: the
/// same single-flight attempt, the same seal/publish/verify/bind steps, the
/// same convergence rules.
///
/// The guard is INVERTED: a direct build's spec must NOT be marked
/// reconstructed. Its hash IS the version identity (`idx2-…`), so it must be
/// a record of the inputs — real per-source outcomes, real snapshot — not a
/// best reassembly of them.
pub async fn direct_index(
    ctx: &MirrorContext,
    pg: &PgRetrieval,
    target: MirrorTarget<'_>,
    index_version_id: &str,
    spec: &BuildSpec,
    plan: &ArtifactBuildPlan,
    chunks: Vec<PreparedChunk>,
) -> Result<MirrorOutcome> {
    if spec.reconstructed {
        return Err(KernelError::InvalidInput(
            "a direct build's BuildSpec must not be marked reconstructed: its hash is the \
             version identity, so it must record the inputs rather than reassemble them"
                .into(),
        ));
    }
    if spec.scope != target.scope() {
        return Err(KernelError::InvalidInput(format!(
            "the spec's scope {:?} is not the target being built",
            spec.scope.id
        )));
    }
    let expected = spec
        .index_version_id()
        .map_err(|e| KernelError::InvalidInput(format!("canonical spec: {e}")))?;
    if expected != index_version_id {
        return Err(KernelError::InvalidInput(format!(
            "the spec hashes to {expected}, not {index_version_id}; an idx2 version id and its \
             spec are one identity and may not drift"
        )));
    }
    let plan_hash = plan
        .plan_sha256()
        .map_err(|e| KernelError::InvalidInput(format!("canonical plan: {e}")))?;

    let attempt_id =
        match acquire_attempt(ctx, index_version_id, &plan_hash, AttemptMode::Direct).await? {
            Acquired::Claimed(id) => id,
            Acquired::Finished(done) => {
                let outcome = Ok(done);
                report(ctx, "direct", &BuildTimer::start(), &outcome);
                return outcome;
            }
        };

    let mut timer = BuildTimer::start();
    let outcome = mirror_body(
        ctx,
        pg,
        ChunkFeed::Prepared(chunks),
        target,
        index_version_id,
        spec,
        plan,
        &attempt_id,
        &mut timer,
    )
    .await;
    report(ctx, "direct", &timer, &outcome);
    match &outcome {
        Ok(MirrorOutcome::Published { .. }) => {
            ctx.attempts.mark_succeeded(&attempt_id).await?;
            cleanup_staging(&ctx.staging_dir(&attempt_id));
        }
        Ok(MirrorOutcome::Converged { artifact_id }) => {
            ctx.attempts
                .mark_converged(&attempt_id, artifact_id)
                .await?;
            cleanup_staging(&ctx.staging_dir(&attempt_id));
        }
        Ok(_) => {
            ctx.attempts.mark_cancelled(&attempt_id).await.ok();
            cleanup_staging(&ctx.staging_dir(&attempt_id));
        }
        Err(e) => {
            // Same recovery contract as the mirror settle above: a failure
            // that reached `sealed` is left sealed for the reconciler.
            let sealed = ctx
                .attempts
                .get(&attempt_id)
                .await
                .ok()
                .flatten()
                .map(|r| r.state == AttemptState::Sealed)
                .unwrap_or(false);
            if sealed {
                tracing::warn!(
                    error = %e,
                    attempt_id = %attempt_id,
                    "a sealed direct build failed before publication; leaving it for the reconciler"
                );
            } else {
                // A queued build that recorded an intent and then failed
                // before sealing will never publish it.
                if ctx.job_fence.is_some() {
                    fail_pending_intent(ctx, &attempt_id, &e.to_string()).await;
                }
                ctx.attempts
                    .mark_failed(&attempt_id, "direct_build_failed", &e.to_string())
                    .await
                    .ok();
                cleanup_staging(&ctx.staging_dir(&attempt_id));
            }
        }
    }
    outcome
}

#[allow(clippy::too_many_arguments)]
async fn mirror_body(
    ctx: &MirrorContext,
    pg: &PgRetrieval,
    feed: ChunkFeed<'_>,
    target: MirrorTarget<'_>,
    index_version_id: &str,
    spec: &BuildSpec,
    plan: &ArtifactBuildPlan,
    attempt_id: &str,
    timer: &mut BuildTimer,
) -> Result<MirrorOutcome> {
    ctx.fault(BuildPhase::AfterClaim)?;

    // ClearView R4: a queued build links its attempt to the job claim before
    // any work that could be published, under the job row's lock. A claim
    // retired before this point leaves no link and publishes nothing.
    if let Some(fence) = &ctx.job_fence {
        if !ctx
            .attempts
            .link_job(fence, attempt_id, index_version_id)
            .await?
        {
            return Err(fence.stale());
        }
    }

    // Steps 4-5: stream committed chunks into a writer, OUTSIDE any
    // transaction. Not holding one is the whole reason attempts exist.
    let dims = spec.embedder.as_ref().map(|e| e.dimensions as usize);
    let mut writer = ShardWriter::new(dims);

    let lease_lost = Arc::new(AtomicBool::new(false));
    // Held for the whole build, not just the export: sealing a large corpus
    // builds a whole lexical index, and a lease that lapsed mid-seal would let
    // another node claim the plan while this one is still writing it. The guard
    // stops it on every path out.
    let _beat = spawn_heartbeat(ctx, attempt_id, Arc::clone(&lease_lost));

    let stats: ExportStats = match feed {
        ChunkFeed::Export(export_target) => {
            let add = |c: munarium_retrieval_pg::export::ExportedChunk| -> Result<()> {
                writer
                    .add(PreparedChunk {
                        chunk_id: c.chunk_id,
                        source_id: c.source_id,
                        source_path: c.source_path,
                        // Collection and legacy chunks carry no node id: the
                        // section-tree locus is an experiment-harness concept the server
                        // has never stored, and leaving it None says so.
                        node_id: None,
                        ordinal: c.ordinal,
                        text: c.text,
                        text_sha256: c.text_sha256,
                        embedding: c.embedding,
                        metadata: std::collections::BTreeMap::from([
                            ("source_content_hash".into(), c.source_hash),
                            ("provenance".into(), c.metadata.to_string()),
                        ]),
                    })
                    .map_err(|e| KernelError::Storage(format!("shard writer: {e}")))
            };
            match export_target {
                MirrorTarget::Collection { collection_id } => {
                    pg.export_collection_chunks(collection_id, index_version_id, add)
                        .await
                }
                MirrorTarget::LegacyShape { .. } => {
                    pg.export_legacy_chunks(index_version_id, add).await
                }
            }?
        }
        ChunkFeed::Prepared(chunks) => {
            // The chunks are in hand and COMPLETE by construction: the direct
            // builder extracted them in this process moments ago. The stats
            // are derived from what is actually written, not asserted.
            let mut sources = std::collections::HashSet::new();
            let mut with_embedding = 0u64;
            let count = chunks.len() as u64;
            for c in chunks {
                sources.insert(c.source_id.clone());
                if c.embedding.is_some() {
                    with_embedding += 1;
                }
                writer
                    .add(c)
                    .map_err(|e| KernelError::Storage(format!("shard writer: {e}")))?;
            }
            ExportStats {
                chunks: count,
                sources: sources.len() as u64,
                with_embedding,
                complete: true,
            }
        }
    };
    timer.finished_export();
    ctx.fault(BuildPhase::AfterExport)?;
    // ClearView R6: a safe boundary before the (long) seal.
    ctx.checkpoint(ProgressPhase::Sealing, None, None).await?;

    // A build that lost its lease must not publish: another node may already
    // have reclaimed the plan and be building it too.
    if lease_lost.load(Ordering::Relaxed)
        || !ctx.attempts.heartbeat(attempt_id, &ctx.node_id).await?
    {
        return Err(KernelError::Storage(
            "this build lost its lease while streaming; another node may have reclaimed the plan, \
             so publishing now could put two builders on one prefix"
                .into(),
        ));
    }

    // Step 6: seal into the local staging directory and compute the artifact
    // id. The manifest is written HERE as well as at the final prefix, which is
    // what makes an interrupted attempt resumable: staging then describes
    // itself, and the reconciler needs no audit projection to republish it.
    //
    // On the BLOCKING pool. Sealing is synchronous CPU and disk work — hashing
    // every component and building a lexical index — and running it on an async
    // worker starves the runtime this process serves requests with.
    let seal_dir = ctx.staging_dir(attempt_id);
    let seal_spec = spec.clone();
    let seal_plan = plan.clone();
    let sealed = tokio::task::spawn_blocking(move || -> Result<SealedArtifact> {
        let staging = LocalFileStore::new(seal_dir)
            .map_err(|e| KernelError::Storage(format!("staging: {e}")))?;
        let sealed = writer
            .seal(&seal_spec, &seal_plan, &staging)
            .map_err(|e| KernelError::Storage(format!("seal: {e}")))?;
        sealed
            .publish_manifest(&staging)
            .map_err(|e| KernelError::Storage(format!("staging manifest: {e}")))?;
        Ok(sealed)
    })
    .await
    .map_err(|e| KernelError::Storage(format!("seal task: {e}")))??;
    timer.finished_seal();
    ctx.fault(BuildPhase::AfterSeal)?;
    // ClearView R6: a cancel that committed during the seal stops the build
    // here, before it can commit a publication intent.
    ctx.checkpoint(ProgressPhase::Publishing, None, None)
        .await?;

    // The same lease check again, AFTER the seal. The heartbeat ran through
    // the seal (that is why it is held for the whole build), but the flag it
    // sets was read only once, before the seal began — and a seal of a large
    // OCR corpus is minutes, long enough for the lease to lapse and another
    // node to reclaim the plan. Publishing past a lost lease is exactly the
    // two-builders-on-one-prefix case the pre-seal check refuses.
    if lease_lost.load(Ordering::Relaxed)
        || !ctx.attempts.heartbeat(attempt_id, &ctx.node_id).await?
    {
        return Err(KernelError::Storage(
            "this build lost its lease while sealing; another node may have reclaimed the plan, \
             so publishing now could put two builders on one prefix"
                .into(),
        ));
    }

    let final_prefix = ctx.artifact_prefix_for(
        target,
        &spec.scope.id,
        index_version_id,
        &sealed.artifact_id,
    );

    // ClearView R4: the decision to publish. A queued build commits ONE
    // immutable manifest key and digest under the job row's lock, while its
    // generation is current, and before the catalog insert or any upload. A
    // generation retired before this commit publishes nothing; one retired
    // after it may only finish this exact intent and never binds.
    let intent = match &ctx.job_fence {
        None => None,
        Some(fence) => {
            let intent = PublicationIntent {
                attempt_id: attempt_id.to_string(),
                index_version_id: index_version_id.to_string(),
                job_id: fence.job_id.clone(),
                job_generation: fence.generation,
                artifact_id: sealed.artifact_id.clone(),
                manifest_key: manifest_key(&final_prefix),
                // The artifact id IS the sha256 of the canonical manifest
                // bytes; `publish_and_verify` re-checks the stored object
                // against this digest before anything is recorded.
                manifest_sha256: sealed.artifact_id.clone(),
                state: IntentState::Pending,
                outcome_detail: None,
            };
            if !ctx
                .attempts
                .commit_publication_intent(
                    fence,
                    attempt_id,
                    index_version_id,
                    &intent.artifact_id,
                    &intent.manifest_key,
                    &intent.manifest_sha256,
                )
                .await?
            {
                return Err(fence.stale());
            }
            Some((fence, intent))
        }
    };

    // Step 7: catalog as `sealed`, converging on conflict.
    let new = NewArtifact {
        index_version_id: index_version_id.to_string(),
        artifact_id: sealed.artifact_id.clone(),
        engine_id: plan.lexical.engine_id.clone(),
        format_version: plan.envelope.format_version as i32,
        artifact_uri: final_prefix.clone(),
        artifact_plan: serde_json::to_value(plan)
            .map_err(|e| KernelError::InvalidInput(e.to_string()))?,
        artifact_plan_sha256: sealed.manifest.artifact_plan_sha256.clone(),
        artifact_manifest: serde_json::to_value(&sealed.manifest)
            .map_err(|e| KernelError::InvalidInput(e.to_string()))?,
        bytes_len: sealed
            .manifest
            .components
            .iter()
            .map(|c| c.bytes_len as i64)
            .sum(),
        file_count: sealed.manifest.components.len() as i32,
        built_by: Some(ctx.node_id.clone()),
        attempt_id: Some(attempt_id.to_string()),
    };
    match ctx.catalog.insert_sealed(&new).await? {
        InsertOutcome::Converged { .. } => {
            if intent.is_some() {
                ctx.attempts
                    .resolve_publication_intent(attempt_id, IntentState::Converged, None)
                    .await?;
            }
            return Ok(MirrorOutcome::Converged {
                artifact_id: sealed.artifact_id,
            });
        }
        InsertOutcome::Inserted | InsertOutcome::Adopted { .. } => {}
        InsertOutcome::Blocked { existing_state } => {
            return Err(KernelError::InvalidInput(format!(
                "artifact {} already exists in state {existing_state:?}; a failed or retired \
                 artifact is not republished over — retire it explicitly or clear the row before \
                 rebuilding this content",
                sealed.artifact_id
            )));
        }
    }
    // The attempt records the artifact id the moment the catalog does. A crash
    // between here and the manifest write is precisely the §7.4 case, and it is
    // recoverable only if the attempt names what it was publishing.
    ctx.attempts
        .mark_sealed(attempt_id, &sealed.artifact_id)
        .await?;
    ctx.fault(BuildPhase::AfterCatalogInsert)?;
    // ClearView R6: the boundary before upload. A queued build stopped here
    // (cancelled, or its lease lost) leaves its attempt `sealed` and its
    // intent pending; only that exact intent can still finish, through the
    // sealed-attempt reconciler, and a newer generation of the same job waits
    // for it and binds the result. A DRAINING build does not stop here: its
    // claim is still current, so it finishes and binds (`drain_finishes`).
    ctx.checkpoint(ProgressPhase::Uploading, None, None).await?;

    let bound_staged = publish_and_verify(
        ctx,
        index_version_id,
        &final_prefix,
        &sealed.artifact_id,
        &sealed.manifest,
        &ctx.staging_dir(attempt_id),
        intent.as_ref().map(|(fence, intent)| (*fence, intent)),
    )
    .await?;
    if intent.is_some() {
        ctx.attempts
            .resolve_publication_intent(attempt_id, IntentState::Published, None)
            .await?;
    }

    timer.finished_publish();
    timer.bytes = new.bytes_len.max(0) as u64;
    Ok(MirrorOutcome::Published {
        artifact_id: sealed.artifact_id,
        chunks: stats.chunks,
        bound_staged,
    })
}

/// Report one finished build, if anyone is counting.
fn report(
    ctx: &MirrorContext,
    mode: &'static str,
    timer: &BuildTimer,
    outcome: &Result<MirrorOutcome>,
) {
    let Some(observer) = ctx.observer.as_ref() else {
        return;
    };
    let (label, chunks) = match outcome {
        Ok(MirrorOutcome::Published { chunks, .. }) => ("published", *chunks),
        Ok(MirrorOutcome::Converged { .. }) => ("converged", 0),
        Ok(MirrorOutcome::AlreadyBuilt { .. }) => ("already_built", 0),
        Ok(MirrorOutcome::AlreadyRunning { .. }) => ("deferred", 0),
        Err(_) => ("failed", 0),
    };
    observer.build_finished(&BuildMetrics {
        mode,
        outcome: label,
        chunks,
        bytes: timer.bytes,
        export_seconds: timer.export_seconds,
        seal_seconds: timer.seal_seconds,
        publish_seconds: timer.publish_seconds,
        total_seconds: timer.total(),
    });
}

/// Steps 8-11, shared by a fresh build and by a resumed one.
///
/// Idempotent by construction: every write is a put to a content-addressed
/// prefix, `mark_verified` only moves `sealed`→`verified`, and the binding is
/// inserted only into an empty slot. Re-running it after a crash reproduces the
/// same bytes at the same keys.
///
/// `fenced` is a queued build's job claim and the publication intent it
/// committed (ClearView R4). With it, publication is limited to that intent:
/// the manifest key and digest must be the recorded ones, an object already at
/// the key must verify against the digest and is never overwritten, and the
/// `staged` binding is made only while the claim is current. `None` is the
/// unfenced publication, unchanged.
async fn publish_and_verify(
    ctx: &MirrorContext,
    index_version_id: &str,
    final_prefix: &str,
    artifact_id: &str,
    manifest: &ArtifactManifest,
    staging_dir: &Path,
    fenced: Option<(&JobFence, &PublicationIntent)>,
) -> Result<bool> {
    if let Some((_, intent)) = fenced {
        require_intent_target(intent, index_version_id, final_prefix, artifact_id)?;
    }
    let intent_digest = fenced.map(|(_, intent)| intent.manifest_sha256.clone());
    // Steps 8 and 9 run on the BLOCKING pool.
    //
    // `ArtifactStore` is a synchronous trait — the datastore crate is
    // engine-side and CPU-bound, and an async trait there would push a runtime
    // choice into a crate meant to be liftable. The object-store implementation
    // therefore bridges with `Handle::block_on`, which PANICS when it is called
    // on a runtime worker thread. Every test here runs over local files, where
    // the same code is merely synchronous IO on an async worker, so this is a
    // failure the local tier cannot show: it appears the first time an artifact
    // is published to Azure.
    let store = ctx.stores.store_for_prefix(final_prefix)?;
    let components = manifest.components.clone();
    let artifact = artifact_id.to_string();
    let dir = staging_dir.to_path_buf();
    let faults = ctx.faults.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let fault = |p: BuildPhase| -> Result<()> {
            match &faults {
                Some(h) => h(p),
                None => Ok(()),
            }
        };
        let staging =
            LocalFileStore::new(dir).map_err(|e| KernelError::Storage(format!("staging: {e}")))?;

        // A fenced publication first checks what is already at the intent's
        // key. Matching bytes mean an earlier run already published this
        // exact intent (the manifest goes last, after every component), so
        // nothing is written again; bytes that do not match are refused and
        // left in place for an operator rather than overwritten.
        if let Some(digest) = &intent_digest {
            if store
                .exists(MANIFEST)
                .map_err(|e| KernelError::Storage(format!("manifest probe: {e}")))?
            {
                let existing = store
                    .get_component(MANIFEST, None)
                    .map_err(|e| KernelError::Storage(format!("existing manifest: {e}")))?;
                munarium_datastore::verify::verify_manifest_bytes(&existing, digest).map_err(
                    |e| {
                        KernelError::Storage(format!(
                            "the publication intent's key already holds a different manifest; \
                             it was not overwritten: {e}"
                        ))
                    },
                )?;
                return Ok(());
            }
        }

        // Step 8: components to the immutable prefix. The manifest is NOT among
        // them; it goes last, after everything it names is durable.
        for component in &components {
            let bytes = staging.get_component(&component.path, None).map_err(|e| {
                KernelError::Storage(format!("read staged {}: {e}", component.path))
            })?;
            // The staged bytes are checked against the manifest before upload.
            // Staging is a local directory that may have survived a crash and a
            // restart, and publishing it unchecked would let a truncated file
            // become an artifact whose own manifest disagrees with it.
            munarium_datastore::verify::verify_component(component, &bytes)
                .map_err(|e| KernelError::Storage(format!("staged {}: {e}", component.path)))?;
            store
                .put_component(&component.path, &bytes)
                .map_err(|e| KernelError::Storage(format!("upload {}: {e}", component.path)))?;
        }
        fault(BuildPhase::AfterComponentUpload)?;

        // Step 9: the manifest last, then read it back through the same path a
        // query node uses. Verifying the bytes still in memory would verify
        // this process against itself and prove nothing about what was stored.
        let manifest_bytes = staging
            .get_component(MANIFEST, None)
            .map_err(|e| KernelError::Storage(format!("staged manifest: {e}")))?;
        munarium_datastore::verify::verify_manifest_bytes(&manifest_bytes, &artifact)
            .map_err(|e| KernelError::Storage(format!("staged manifest: {e}")))?;
        store
            .put_component(MANIFEST, &manifest_bytes)
            .map_err(|e| KernelError::Storage(format!("publish manifest: {e}")))?;
        fault(BuildPhase::AfterManifestWrite)?;

        let readback = store
            .get_component(MANIFEST, None)
            .map_err(|e| KernelError::Storage(format!("manifest read-back: {e}")))?;
        munarium_datastore::verify::verify_manifest_bytes(&readback, &artifact)
            .map_err(|e| KernelError::Storage(format!("manifest read-back: {e}")))?;
        Ok(())
    })
    .await
    .map_err(|e| KernelError::Storage(format!("publication task: {e}")))??;

    // Step 10: verified.
    ctx.catalog
        .mark_verified(index_version_id, artifact_id, &ctx.node_id)
        .await?;
    ctx.fault(BuildPhase::BeforeBinding)?;

    // Step 11: a `staged` binding if the version has none — and NEVER
    // `serving`, and never the active pointer. A mirror adds an implementation
    // of a version that already exists; promoting it is a separate operation
    // with its own safety argument. A fenced publication binds only while its
    // claim is current: a retired generation never calls the unfenced bind.
    match fenced {
        None => bind_staged_if_empty(ctx, index_version_id, artifact_id).await,
        Some((fence, _)) => bind_staged_fenced(ctx, index_version_id, artifact_id, fence).await,
    }
}

/// The canonical manifest's full object key under an artifact prefix.
fn manifest_key(final_prefix: &str) -> String {
    format!("{final_prefix}/{MANIFEST}")
}

/// Refuse to publish anything other than the intent's exact target.
fn require_intent_target(
    intent: &PublicationIntent,
    index_version_id: &str,
    final_prefix: &str,
    artifact_id: &str,
) -> Result<()> {
    if intent.index_version_id != index_version_id
        || intent.artifact_id != artifact_id
        || intent.manifest_sha256 != artifact_id
        || intent.manifest_key != manifest_key(final_prefix)
    {
        return Err(KernelError::InvalidInput(format!(
            "attempt {} may publish only its recorded intent; this publication names another \
             manifest key or digest",
            intent.attempt_id
        )));
    }
    Ok(())
}

/// Step 11 for a queued build: bind `staged` only if the slot is empty AND
/// the job claim is still current, both under the job row's lock.
async fn bind_staged_fenced(
    ctx: &MirrorContext,
    index_version_id: &str,
    artifact_id: &str,
    fence: &JobFence,
) -> Result<bool> {
    match ctx
        .catalog
        .bind_new_if_empty_fenced(
            index_version_id,
            BindingSlot::Staged,
            artifact_id,
            &ctx.node_id,
            Some("mirror build"),
            fence,
        )
        .await?
    {
        FencedBind::Bound(_) => Ok(true),
        FencedBind::Occupied => Ok(false),
        FencedBind::Superseded => {
            tracing::warn!(
                artifact_id = %artifact_id,
                "build job claim was superseded; the published artifact was left unbound"
            );
            Ok(false)
        }
    }
}

/// Record a version's disposition for a queued build, best effort: the
/// outcome read reports a version without one as untouched, which is the
/// conservative reading (ClearView R6).
async fn record_disposition(
    ctx: &MirrorContext,
    fence: &JobFence,
    index_version_id: &str,
    disposition: &str,
) {
    match ctx
        .attempts
        .record_version_disposition(fence, index_version_id, disposition)
        .await
    {
        Ok(true) => {}
        Ok(false) => tracing::info!(
            job_id = %fence.job_id,
            "build job claim was superseded; a version disposition was not recorded"
        ),
        Err(e) => tracing::warn!(error = %e, "could not record a build-job version disposition"),
    }
}

/// Mark a queued build's pending intent failed, best effort. Used when the
/// build fails before it sealed, so nothing will resume the intent.
async fn fail_pending_intent(ctx: &MirrorContext, attempt_id: &str, detail: &str) {
    if let Err(e) = ctx
        .attempts
        .resolve_publication_intent(attempt_id, IntentState::Failed, Some(detail))
        .await
    {
        tracing::warn!(error = %e, "could not record a failed publication intent");
    }
}

/// Step 11 on its own: bind `artifact_id` into the version's `staged` slot if
/// that slot is empty. `Ok(true)` when this call bound it, `Ok(false)` when
/// the slot was already taken (by this artifact or another).
///
/// Separate from `publish_and_verify` because a resumed attempt can find its
/// artifact already `verified` — a crash between step 10 and step 11 — and
/// must still perform step 11, or a verified artifact ends up with no binding
/// and the attempt is marked succeeded over an unservable result.
async fn bind_staged_if_empty(
    ctx: &MirrorContext,
    index_version_id: &str,
    artifact_id: &str,
) -> Result<bool> {
    match ctx
        .catalog
        .binding(index_version_id, BindingSlot::Staged)
        .await?
    {
        Some(_) => Ok(false),
        None => {
            ctx.catalog
                .bind_new(
                    index_version_id,
                    BindingSlot::Staged,
                    artifact_id,
                    &ctx.node_id,
                    Some("mirror build"),
                )
                .await?;
            Ok(true)
        }
    }
}

/// A running heartbeat, stopped when it goes out of scope.
///
/// A guard rather than an explicit `abort()`, because the build has half a
/// dozen early returns and one of them forgetting to stop the beat would leave
/// a task extending the lease of an attempt nobody is working on — which is the
/// one thing worse than a lease that expires too soon.
struct Heartbeat(tokio::task::JoinHandle<()>);

impl Drop for Heartbeat {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn spawn_heartbeat(ctx: &MirrorContext, attempt_id: &str, lost: Arc<AtomicBool>) -> Heartbeat {
    let attempts = ctx.attempts.clone();
    let attempt_id = attempt_id.to_string();
    let node_id = ctx.node_id.clone();
    Heartbeat(tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(HEARTBEAT_SECS)).await;
            match attempts.heartbeat(&attempt_id, &node_id).await {
                Ok(true) => {}
                // The attempt is no longer ours. Recording it and stopping is
                // the point: the builder checks this flag before publishing.
                Ok(false) => {
                    lost.store(true, Ordering::Relaxed);
                    break;
                }
                // A transient database error is not evidence the lease is gone.
                // Keep beating; the pre-publication check makes the decision
                // against the database rather than against this loop.
                Err(e) => tracing::warn!(error = %e, "mirror heartbeat failed"),
            }
        }
    }))
}

fn cleanup_staging(dir: &Path) {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(error = %e, "could not remove a build staging directory"),
    }
}

// --- reconciliation ---------------------------------------------------------

/// What one reconciliation pass did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReconcileReport {
    /// Attempts whose lease ran out while `running`.
    pub expired: u64,
    /// `sealed` attempts this node resumed and published.
    pub resumed: u64,
    /// `sealed` attempts abandoned because their content is gone.
    pub abandoned: u64,
    /// `sealed` attempts belonging to another node whose lease is still live.
    pub left_alone: u64,
}

/// Reconcile interrupted attempts (§7.4).
///
/// Runs at startup and on a bounded interval. A `sealed` attempt is mid-flight,
/// not finished, and **a `sealed` row is never read as "L2 exists"** — the
/// verdict comes from what this node can observe: whether the attempt is ours,
/// whether its lease is fresh, and whether its staged content is still on disk.
pub async fn reconcile_attempts(ctx: &MirrorContext) -> Result<ReconcileReport> {
    let mut report = ReconcileReport {
        expired: ctx.attempts.expire_stale().await?,
        ..Default::default()
    };

    for row in ctx.attempts.sealed_awaiting_publication().await? {
        let staging_dir = ctx.staging_dir(&row.attempt_id);
        let staging_present = staging_dir.join(MANIFEST).exists();
        match reconcile_sealed(&row, &ctx.node_id, staging_present) {
            SealedVerdict::NotOurs { .. } => report.left_alone += 1,
            SealedVerdict::Abandon { reason } => {
                // A queued build's pending intent, if any, ends with it.
                fail_pending_intent(ctx, &row.attempt_id, reason).await;
                ctx.attempts
                    .mark_failed(&row.attempt_id, "publication_abandoned", reason)
                    .await?;
                cleanup_staging(&staging_dir);
                report.abandoned += 1;
            }
            SealedVerdict::Resume => {
                match resume_publication(ctx, &row.attempt_id, &staging_dir).await {
                    Ok(()) => {
                        ctx.attempts.mark_succeeded(&row.attempt_id).await?;
                        cleanup_staging(&staging_dir);
                        report.resumed += 1;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "could not resume a sealed build");
                        fail_pending_intent(ctx, &row.attempt_id, &e.to_string()).await;
                        ctx.attempts
                            .mark_failed(&row.attempt_id, "publication_abandoned", &e.to_string())
                            .await?;
                        // The attempt is terminal now; nothing will read the
                        // staged copy again, and leaving it would leak a whole
                        // sealed corpus under `staging_root` per failed resume.
                        cleanup_staging(&staging_dir);
                        report.abandoned += 1;
                    }
                }
            }
        }
    }
    Ok(report)
}

/// Republish a sealed attempt from its own staging directory.
///
/// The manifest comes from STAGING, and its hash is checked against the
/// artifact id the attempt recorded. It is deliberately not read from the
/// catalog's `artifact_manifest` column: that projection is audit-only, and
/// letting it decide which components to upload would make a poisoned row a
/// publication input.
async fn resume_publication(
    ctx: &MirrorContext,
    attempt_id: &str,
    staging_dir: &Path,
) -> Result<()> {
    let row = ctx
        .attempts
        .get(attempt_id)
        .await?
        .ok_or_else(|| KernelError::NotFound {
            kind: "build attempt",
            id: attempt_id.to_string(),
        })?;
    let artifact_id = row.artifact_id.clone().ok_or_else(|| {
        KernelError::Storage(format!(
            "attempt {attempt_id} is sealed but names no artifact; it cannot be resumed"
        ))
    })?;

    let staging = LocalFileStore::new(staging_dir)
        .map_err(|e| KernelError::Storage(format!("staging: {e}")))?;
    let manifest_bytes = staging
        .get_component(MANIFEST, None)
        .map_err(|e| KernelError::Storage(format!("staged manifest: {e}")))?;
    munarium_datastore::verify::verify_manifest_bytes(&manifest_bytes, &artifact_id)
        .map_err(|e| KernelError::Storage(format!("staged manifest: {e}")))?;
    let manifest: ArtifactManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| KernelError::Storage(format!("staged manifest does not parse: {e}")))?;

    let artifact = ctx
        .catalog
        .artifact(&row.index_version_id, &artifact_id)
        .await?
        .ok_or_else(|| KernelError::NotFound {
            kind: "artifact",
            id: artifact_id.clone(),
        })?;
    // ClearView R4: an attempt a queued build acquired may finish only the
    // intent its generation committed, and binds only if that generation is
    // still current. An unlinked attempt (a synchronous build, or one from a
    // v1.4 builder) resumes exactly as before.
    let intent = match ctx.attempts.job_link(attempt_id).await? {
        None => None,
        Some(link) => {
            let intent = ctx
                .attempts
                .publication_intent(attempt_id)
                .await?
                .ok_or_else(|| {
                    KernelError::Storage(format!(
                        "attempt {attempt_id} belongs to build job {} but recorded no \
                         publication intent; it cannot be resumed",
                        link.job_id
                    ))
                })?;
            if intent.job_id != link.job_id || intent.job_generation != link.job_generation {
                return Err(KernelError::Storage(format!(
                    "attempt {attempt_id}'s publication intent does not match its job link"
                )));
            }
            require_intent_target(
                &intent,
                &row.index_version_id,
                &artifact.artifact_uri,
                &artifact_id,
            )?;
            let fence = intent.fence(ctx.catalog.tenant_id(), &row.owner_node_id);
            Some((fence, intent))
        }
    };
    let fenced = intent.as_ref().map(|(fence, intent)| (fence, intent));

    if artifact.state == ArtifactState::Verified {
        // Someone finished publication — or THIS attempt did, and crashed
        // between marking the artifact verified (step 10) and binding it
        // (step 11). Nothing to republish, but the binding must still be
        // there before the caller marks the attempt succeeded: a verified
        // artifact with no binding is not servable, and the slot is filled
        // only if it is empty, so a finished publication is left alone.
        match fenced {
            None => {
                bind_staged_if_empty(ctx, &row.index_version_id, &artifact_id).await?;
            }
            Some((fence, _)) => {
                bind_staged_fenced(ctx, &row.index_version_id, &artifact_id, fence).await?;
                ctx.attempts
                    .resolve_publication_intent(attempt_id, IntentState::Published, None)
                    .await?;
            }
        }
        return Ok(());
    }

    publish_and_verify(
        ctx,
        &row.index_version_id,
        &artifact.artifact_uri,
        &artifact_id,
        &manifest,
        staging_dir,
        fenced,
    )
    .await?;
    if fenced.is_some() {
        ctx.attempts
            .resolve_publication_intent(attempt_id, IntentState::Published, None)
            .await?;
    }
    Ok(())
}

// --- reconstructed specs and default plans ----------------------------------

/// Build the `reconstructed: true` spec for an existing PostgreSQL version.
///
/// Everything here is a best reassembly of inputs PostgreSQL never recorded in
/// this shape — which is exactly why `reconstructed` is set, and why the hash
/// must never be used as replay input or as a version id. The mirror reuses the
/// existing `idx-` id; this document exists so the artifact carries a
/// description of what it holds, not so it can name it.
pub fn reconstructed_spec(
    target: MirrorTarget<'_>,
    shape_ref: &str,
    watermark_seq: u64,
    embedder_dimensions: Option<u32>,
    sources: &[ExportedSource],
    extractor_version: &str,
) -> BuildSpec {
    BuildSpec {
        spec_version: 1,
        scope: target.scope(),
        sources: sources
            .iter()
            .map(|s| SourceRef {
                source_id: s.source_id.clone(),
                logical_path: s.logical_path.clone(),
                // A deleted `sources` row leaves no media type behind, and a
                // plausible substitute would be a guess dressed as a record.
                media_type: s
                    .media_type
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                content_sha256: s.content_sha256.clone(),
                revision: None,
            })
            .collect(),
        snapshot: Snapshot { watermark_seq },
        shape: ShapeRef {
            shape_ref: shape_ref.to_string(),
            version: 1,
        },
        chunker: Chunker {
            name: "para".into(),
            // The constant is the whole versioned identity PostgreSQL records
            // (`para@1`), carried verbatim rather than re-spelled here.
            version: munarium_retrieval_pg::CHUNKER_VERSION.into(),
            params: Default::default(),
        },
        extractor: Extractor {
            name: "munarium-extract".into(),
            version: extractor_version.to_string(),
            config: Default::default(),
            // Per-source extraction outcomes were not recorded at build time in
            // a form this can read, and inventing them would put a false
            // statement inside a hash. Empty, which `reconstructed` explains.
            per_source: Vec::new(),
        },
        embedder: embedder_dimensions.map(|dimensions| Embedder {
            model: munarium_retrieval_pg::LOCAL_EMBEDDER.into(),
            dimensions,
            normalization: Normalization::L2,
            metric: Metric::Cosine,
        }),
        lexical_analysis: LexicalAnalysis {
            contract_version: munarium_datastore::lexical::ANALYZER_CONTRACT_VERSION,
            // The MIRROR's analyzer, not PostgreSQL's. This describes what the
            // ARTIFACT contains, and the artifact is built with the Munarium
            // tokenizer; the two differ, and measuring that difference is
            // exactly what stage 5 is for.
            tokenizer: munarium_datastore::lexical::TOKENIZER_ID.into(),
            stemmer: "snowball-english".into(),
            stop_terms_ref: StopTerms {
                list_ref: "pg16/english".into(),
                sha256: munarium_datastore::lexical::stop_terms_sha256(),
            },
            index_options: IndexOptions {
                positions: true,
                case_folding: Some("lowercase".into()),
                accent_folding: Some("none".into()),
            },
        },
        reconstructed: true,
    }
}

/// The exact-vs-approximate decision for a direct build's vector leg.
///
/// One number, deliberately: §6.3's initial deterministic build policy is
/// "exact below a benchmark-derived count threshold, approved approximate
/// above it", and nothing here may read transient runtime load.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VectorPolicy {
    /// Chunk count at or above which a direct build uses the approximate
    /// engine. `None` = never (exact always) — the value when the engine is
    /// not compiled in, or when the operator sets the env var to `off`.
    pub approx_threshold: Option<u64>,
}

/// The benchmark-derived default (the recorded baseline,
/// "The exact/approximate crossover", measured 2026-08-31). NOT the raw
/// latency crossover — the graph beat the scan at every measured size with
/// recall 1.000 on clustered (embedding-shaped) data — but the point where
/// the exact scan stops being ignorable: at the measured ~0.72 ms per 1k
/// vectors, an exact leg alone crosses the entire measured all-in datastore
/// query p50 (3.35 ms) at ≈4.6k vectors. Derived there, rounded down to a
/// power of two. Below it, exactness is free quality; above it, the scan is
/// the budget.
pub const DEFAULT_APPROX_THRESHOLD: u64 = 4_096;

// Not derivable in BOTH configurations: with the feature the default carries
// the measured threshold, and one impl serving both keeps them side by side.
#[allow(clippy::derivable_impls)]
impl Default for VectorPolicy {
    fn default() -> Self {
        #[cfg(feature = "vector-diskann")]
        {
            Self {
                approx_threshold: Some(DEFAULT_APPROX_THRESHOLD),
            }
        }
        #[cfg(not(feature = "vector-diskann"))]
        {
            Self {
                approx_threshold: None,
            }
        }
    }
}

impl VectorPolicy {
    /// Read `MUNARIUM_DATASTORE_VECTOR_APPROX_THRESHOLD`. Unset -> the
    /// compiled default; `off` -> never approximate; a number -> that
    /// threshold. Anything unparseable is refused loudly by falling back to
    /// EXACT, never to a guessed number: a wrong threshold silently applied
    /// is a physical decision nobody made.
    pub fn from_env() -> Self {
        Self::from_setting(
            std::env::var("MUNARIUM_DATASTORE_VECTOR_APPROX_THRESHOLD")
                .ok()
                .as_deref(),
        )
    }

    /// The pure half of `from_env`, so the parsing is testable without
    /// touching process environment from parallel tests.
    pub fn from_setting(raw: Option<&str>) -> Self {
        match raw.map(str::trim) {
            None | Some("") => Self::default(),
            Some("off") => Self {
                approx_threshold: None,
            },
            Some(v) => match v.parse::<u64>() {
                Ok(n) => Self {
                    approx_threshold: Some(n.max(1)),
                },
                Err(_) => {
                    tracing::warn!(
                        value = v,
                        "MUNARIUM_DATASTORE_VECTOR_APPROX_THRESHOLD is neither a number nor \
                         'off'; using exact vectors"
                    );
                    Self {
                        approx_threshold: None,
                    }
                }
            },
        }
    }

    fn wants_approximate(&self, chunk_count: u64) -> bool {
        // Without the engine compiled in, the answer is no REGARDLESS of the
        // threshold: a plan naming an engine the binary lacks would refuse to
        // seal, so it must never be produced here.
        if cfg!(not(feature = "vector-diskann")) {
            return false;
        }
        self.approx_threshold.is_some_and(|t| chunk_count >= t)
    }
}

/// The physical plan for a DIRECT build, where — unlike the mirror — the
/// chunk count is known before the plan is fixed, so §6.3's threshold is a
/// live decision here and `observed` carries the real number.
pub fn direct_plan(
    has_vectors: bool,
    chunk_count: u64,
    policy: &VectorPolicy,
) -> ArtifactBuildPlan {
    let mut plan = mirror_plan(has_vectors);
    if !has_vectors {
        return plan;
    }
    let approximate = policy.wants_approximate(chunk_count);
    if approximate {
        #[cfg(feature = "vector-diskann")]
        {
            use munarium_datastore::vector_diskann as vd;
            plan.envelope.feature_bits.push(vd::FEATURE_BIT.to_string());
            plan.vector = Some(VectorEngine {
                engine_id: vd::ENGINE_ID.into(),
                engine_revision: vd::ENGINE_REVISION.into(),
                kind: VectorKind::Approximate,
                quantization: None, // full precision, and the plan says so
                graph: Some(vd::GraphParams::default().to_plan_map()),
                rescore_depth: None, // traversal distances are already exact
            });
        }
    }
    let (chosen, because) = if approximate {
        (
            "approximate",
            "at or above the approximate threshold, and the engine is compiled in",
        )
    } else if cfg!(not(feature = "vector-diskann")) {
        (
            "exact",
            "the approximate engine is not compiled into this binary",
        )
    } else if policy.approx_threshold.is_none() {
        ("exact", "the approximate engine is turned off by policy")
    } else {
        ("exact", "below the approximate threshold")
    };
    plan.shaper.decisions = vec![ShaperDecision {
        setting: "vector.kind".into(),
        chosen: Param::Text(chosen.into()),
        because: because.into(),
        threshold: policy.approx_threshold.map(|t| Param::Int(t as i64)),
        observed: Some(Param::Int(chunk_count as i64)),
    }];
    plan
}

/// The default physical plan for a mirror build.
///
/// Takes no corpus size, and that is not an omission. The plan's hash is the
/// single-flight key, so the plan must be fixed BEFORE a single chunk is
/// streamed — which means the chunk count is genuinely unknown here. The shaper
/// decision below records `observed: None` rather than a number, because an
/// `observed` value in a hashed, auditable document is a claim about what the
/// builder actually saw, and a placeholder there would be a false one.
pub fn mirror_plan(has_vectors: bool) -> ArtifactBuildPlan {
    ArtifactBuildPlan {
        plan_version: 1,
        envelope: Envelope {
            format_version: 1,
            feature_bits: vec!["records.v1".into()],
        },
        lexical: LexicalEngine {
            engine_id: "tantivy".into(),
            engine_revision: munarium_datastore::lexical::engine_revision().into(),
            positions: true,
            segments: None,
            compression: None,
        },
        vector: has_vectors.then(|| VectorEngine {
            engine_id: "munarium-flat".into(),
            engine_revision: "0.1.0".into(),
            kind: VectorKind::Exact,
            quantization: None,
            graph: None,
            rescore_depth: None,
        }),
        records: RecordsFormat {
            format: "munarium-records@2".into(),
            compression: None,
        },
        range_map: None,
        shaper: Shaper {
            policy_version: 1,
            decisions: vec![ShaperDecision {
                setting: "vector.kind".into(),
                chosen: Param::Text("exact".into()),
                // Recorded so the choice is auditable rather than a story
                // about what someone probably ran. The mirror is the REFERENCE
                // reconstruction and stays exact by role; the live threshold
                // decision lives in `direct_plan`, where the chunk count is
                // known before the plan is fixed (stage 8 measured before
                // planning, exactly as this comment once demanded).
                because: "the mirror is the reference reconstruction; the direct build owns the approximate decision"
                    .into(),
                threshold: None,
                // The corpus size is not known when a mirror plan is fixed —
                // the plan hash is the single-flight key — and a placeholder
                // in a hashed, auditable document would be a false claim.
                observed: None,
            }],
        },
    }
}

#[cfg(test)]
mod plan_policy_tests {
    use super::*;

    #[test]
    fn the_setting_parses_or_falls_back_to_exact() {
        assert_eq!(VectorPolicy::from_setting(None), VectorPolicy::default());
        assert_eq!(
            VectorPolicy::from_setting(Some("")),
            VectorPolicy::default()
        );
        assert_eq!(
            VectorPolicy::from_setting(Some("off")).approx_threshold,
            None
        );
        assert_eq!(
            VectorPolicy::from_setting(Some("5000")).approx_threshold,
            Some(5000)
        );
        assert_eq!(
            VectorPolicy::from_setting(Some(" 42 ")).approx_threshold,
            Some(42)
        );
        // Unparseable falls back to EXACT, never to a guessed number.
        assert_eq!(
            VectorPolicy::from_setting(Some("many")).approx_threshold,
            None
        );
        // Zero would make "approximate always" indistinguishable from a typo;
        // it clamps to 1, which means the same thing out loud.
        assert_eq!(
            VectorPolicy::from_setting(Some("0")).approx_threshold,
            Some(1)
        );
    }

    #[test]
    fn below_the_threshold_the_plan_is_exact_with_the_count_recorded() {
        let policy = VectorPolicy {
            approx_threshold: Some(1_000),
        };
        let plan = direct_plan(true, 999, &policy);
        let v = plan.vector.as_ref().unwrap();
        assert_eq!(v.engine_id, "munarium-flat");
        assert_eq!(v.kind, VectorKind::Exact);
        let d = &plan.shaper.decisions[0];
        assert_eq!(d.observed, Some(Param::Int(999)));
        assert_eq!(d.chosen, Param::Text("exact".into()));
    }

    #[cfg(feature = "vector-diskann")]
    #[test]
    fn at_the_threshold_the_plan_names_the_approximate_engine_in_full() {
        let policy = VectorPolicy {
            approx_threshold: Some(1_000),
        };
        let plan = direct_plan(true, 1_000, &policy);
        let v = plan.vector.as_ref().unwrap();
        assert_eq!(v.engine_id, "diskann");
        assert_eq!(v.engine_revision, "0.56.0");
        assert_eq!(v.kind, VectorKind::Approximate);
        // The physical decisions are IN the plan: graph parameters present,
        // quantization and rescore explicitly absent (full precision).
        let graph = v.graph.as_ref().expect("graph parameters recorded");
        assert!(graph.contains_key("max_degree"));
        assert!(graph.contains_key("l_build"));
        assert!(graph.contains_key("alpha"));
        assert!(graph.contains_key("l_search"));
        assert_eq!(v.quantization, None);
        assert_eq!(v.rescore_depth, None);
        // The envelope requires the reader feature bit.
        assert!(plan
            .envelope
            .feature_bits
            .contains(&"vector.diskann.v1".to_string()));
        let d = &plan.shaper.decisions[0];
        assert_eq!(d.observed, Some(Param::Int(1_000)));
        assert_eq!(d.threshold, Some(Param::Int(1_000)));
        assert_eq!(d.chosen, Param::Text("approximate".into()));
    }

    #[cfg(not(feature = "vector-diskann"))]
    #[test]
    fn without_the_engine_compiled_the_plan_never_names_it() {
        // Even an explicit threshold cannot produce a plan the binary could
        // not seal — the decision records WHY it stayed exact.
        let policy = VectorPolicy {
            approx_threshold: Some(1),
        };
        let plan = direct_plan(true, 1_000_000, &policy);
        assert_eq!(plan.vector.as_ref().unwrap().engine_id, "munarium-flat");
        let d = &plan.shaper.decisions[0];
        assert_eq!(d.chosen, Param::Text("exact".into()));
        assert!(matches!(&d.because, s if s.contains("not compiled")));
    }

    #[test]
    fn a_vectorless_plan_is_untouched_by_the_policy() {
        let policy = VectorPolicy {
            approx_threshold: Some(1),
        };
        let plan = direct_plan(false, 1_000_000, &policy);
        assert!(plan.vector.is_none());
    }
}
