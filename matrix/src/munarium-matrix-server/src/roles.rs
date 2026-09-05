// SPDX-License-Identifier: Apache-2.0
//! Role loops: the queue consumers that make a `sync` or `reconcile` container
//! do work rather than merely answer `/healthz`.
//!
//! One loop per role, spawned from `main` when the configured role calls for
//! it. Each is the same shape: claim a job, run it, finish it, poll again;
//! sleep only when the queue is empty.
//!
//! The details that are not obvious, each of which is a way to lose work:
//!
//! - **A claimed job is always finished.** Every exit path writes a terminal
//!   state. A job claimed and never finished is invisible work that no operator
//!   can see and no retry picks up until its lease expires — worse than a job
//!   that plainly failed.
//! - **Poll immediately after working, sleep only when idle.** A backlog drains
//!   at the speed of the work, not the poll interval.
//! - **Draining is checked before claiming, never mid-job.** A stop signal must
//!   not abandon a claimed job; it stops us taking a new one.
//! - **A refusal is an outcome, not a crash.** `schema_drift` means the run
//!   correctly refused. It is recorded with its reason and the loop continues:
//!   one misconfigured source must not stop every other source from syncing.
//! - **The run row closes before the checkpoint moves.** An operator reading
//!   `matrix.sync_runs` must never see a window no run row covers.

use crate::runtime;
use crate::state::AppState;
use munarium_matrix_adapter::{EffectiveIdentity, Limits};
use munarium_matrix_core::checkpoint::Checkpoint;
use munarium_matrix_core::Refusal;
use munarium_matrix_store::journal::JournalRecord;
use munarium_matrix_store::queue::ClaimedJob;
use munarium_matrix_types::assets::{DataSourceDoc, SourceLimits};
use munarium_matrix_workers::classes::resolve_classes;
use munarium_matrix_workers::observe::{observe, ObserveContext};
use munarium_matrix_workers::sync::{run_sync, SyncRequest};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;

/// How long a loop waits when the queue is empty.
const IDLE_POLL: Duration = Duration::from_secs(2);
/// How long a loop waits after an error it cannot attribute to a job — a
/// database that is down, say. Longer than the idle poll so a broken store does
/// not become a hot loop against itself.
const ERROR_BACKOFF: Duration = Duration::from_secs(10);

/// Spawn the loops this role calls for.
pub fn spawn(state: Arc<AppState>) -> Vec<JoinHandle<()>> {
    let mut handles = Vec::new();
    let role = state.role();

    if role.runs_sync() {
        tracing::info!(worker = "sync", "role loop starting");
        let s = state.clone();
        handles.push(tokio::spawn(async move { sync_loop(s).await }));
    }
    if role.runs_reconcile() {
        tracing::info!(worker = "reconcile", "role loop starting");
        let s = state.clone();
        handles.push(tokio::spawn(async move { reconcile_loop(s).await }));
    }
    // The query role has no loop on purpose: its work arrives over HTTP with a
    // caller holding the other end of the deadline. Queueing it would add
    // latency to something already on the turn path.
    handles
}

fn worker_id(state: &AppState, kind: &str) -> String {
    format!("{}-{kind}", state.config.instance_id)
}

fn draining(state: &AppState) -> bool {
    state.draining.load(Ordering::Relaxed)
}

/// A source's own ceilings, as the adapter's shape.
fn source_limits(limits: &SourceLimits) -> Limits {
    Limits {
        max_rows: limits.max_rows,
        max_bytes: limits.max_bytes,
        timeout_ms: limits.statement_timeout_ms,
    }
}

/// The identity a run reads under. Under source-native policy there is no
/// per-class credential and the source itself filters; the principal recorded
/// in evidence says exactly that rather than naming a credential that does not
/// exist.
fn identity_for(class: &munarium_matrix_workers::ResolvedClass) -> EffectiveIdentity {
    EffectiveIdentity {
        class: Some(class.name.clone()),
        credential_ref: class.credential_ref.clone(),
        principal: class
            .credential_ref
            .clone()
            .unwrap_or_else(|| "source-native".into()),
    }
}

/// Pick the class this job runs as.
///
/// A multi-class source syncs once per class, because one collection carries
/// exactly one authorization class — that is what makes a citation resolvable
/// by clearance instead of by filtering after the fact. The job's `entity`
/// field carries the class name when the scheduler fanned out; an empty or
/// unmatched value runs the first class, which is the only class when the
/// source is source-native.
fn class_for<'a>(
    classes: &'a [munarium_matrix_workers::ResolvedClass],
    job: &ClaimedJob,
) -> Result<&'a munarium_matrix_workers::ResolvedClass, Refusal> {
    classes
        .iter()
        .find(|c| c.name == job.entity)
        .or_else(|| classes.first())
        .ok_or_else(|| {
            Refusal::policy_delegation_unavailable(format!(
                "source '{}' resolved no authorization class",
                job.target
            ))
        })
}

// ---------------------------------------------------------------------------
// sync
// ---------------------------------------------------------------------------

async fn sync_loop(state: Arc<AppState>) {
    let worker = worker_id(&state, "sync");
    loop {
        if draining(&state) {
            tracing::info!(worker = %worker, "sync loop stopping (draining)");
            return;
        }
        match state.store.claim_sync_job(&worker, job_lease_secs()).await {
            Ok(Some(job)) => {
                let id = job.id.clone();
                let outcome = run_one_sync(&state, &job).await;
                let (status, detail) = match &outcome {
                    Ok(()) => ("done", None),
                    Err(r) => {
                        // A refusal is the system working. Warn, not error: an
                        // operator scanning for errors should find broken
                        // things, not correctly-refused ones.
                        tracing::warn!(
                            job = %id, source = %job.target,
                            class = %r.class.as_str(), code = %r.code,
                            "sync job refused: {}", r.message
                        );
                        ("failed", Some(r.message.as_str()))
                    }
                };
                if let Err(e) = state.store.finish_sync_job(&id, status, detail).await {
                    // The one genuinely bad case: we cannot record what
                    // happened, so the job may be re-claimed after its lease
                    // expires. Say so loudly.
                    tracing::error!(job = %id, error = %e, "could not finish a claimed sync job");
                }
            }
            Ok(None) => tokio::time::sleep(IDLE_POLL).await,
            Err(e) => {
                tracing::warn!(worker = %worker, error = %e, "could not claim a sync job");
                tokio::time::sleep(ERROR_BACKOFF).await;
            }
        }
    }
}

async fn run_one_sync(state: &Arc<AppState>, job: &ClaimedJob) -> Result<(), Refusal> {
    let tenant = &job.tenant_id;
    let wiring = runtime::wire(state, tenant, &job.target).await?;
    let source: &DataSourceDoc = &wiring.source;

    let sync = source.spec.sync.as_ref().ok_or_else(|| {
        Refusal::not_covered(format!(
            "source '{}' declares no `sync:` block, so there is nothing to materialize",
            job.target
        ))
    })?;
    let entity = &sync.entity.table;

    let classes = resolve_classes(&source.spec.authorization)?;
    let class = class_for(&classes, job)?;

    let checkpoint = state
        .store
        .load_checkpoint(
            tenant,
            &job.target,
            entity,
            &source.metadata.version.to_string(),
        )
        .await
        .map_err(|e| Refusal::source_unavailable(format!("checkpoint read failed: {e}")))?
        .unwrap_or_else(|| {
            Checkpoint::start(&job.target, entity, &source.metadata.version.to_string())
        });

    let known_fingerprint = state
        .store
        .known_fingerprint(tenant, &job.target, entity)
        .await
        .map_err(|e| Refusal::source_unavailable(format!("fingerprint read failed: {e}")))?;

    let run_id = state
        .store
        .start_sync_run(tenant, &job.target, entity, sync.mode.as_str())
        .await
        .map_err(|e| Refusal::source_unavailable(format!("could not open a run row: {e}")))?;

    let req = SyncRequest {
        tenant,
        source_id: &source.metadata.name,
        source_version: source.metadata.version,
        entity,
        projection: &sync.projection,
        key_columns: &sync.entity.key,
        mode: sync.mode,
        // The DataSource's own declaration, read by the adapter rather than
        // replaced with a convention (`Watermark::resolve`).
        watermark: sync.watermark.as_ref(),
        class,
        checkpoint,
        limits: source_limits(&source.spec.limits),
        drift_policy: source.spec.schema_fingerprint.on_drift.clone(),
        known_fingerprint,
        retention_days: None,
    };

    let outcome = run_sync(wiring.adapter.as_ref(), wiring.server.as_ref(), &req).await;

    // The decision a reviewed drift ran under, if any. It lands on the journal
    // row as the request id — the same field the console's apply-in-place
    // uses for ITS decision — so "the journal records the decision"
    // is a query rather than a log line someone has to find.
    let decision = match &source.spec.schema_fingerprint.on_drift {
        munarium_matrix_core::checkpoint::DriftPolicy::Compat { decision_id } => {
            Some(decision_id.clone())
        }
        _ => None,
    };
    let base = JournalRecord::new("sync", if outcome.is_ok() { "ok" } else { "refused" })
        .source(&job.target)
        .request(decision)
        .via("scheduler");

    match &outcome {
        Ok(o) => {
            let _ = state
                .store
                .finish_sync_run(
                    &run_id,
                    "ok",
                    o.records_read,
                    o.records_rendered,
                    o.records_excluded,
                    o.documents_uploaded,
                    o.documents_skipped,
                    o.count_evidence_id.as_deref(),
                    o.next_checkpoint
                        .as_ref()
                        .and_then(|c| c.watermark.as_deref()),
                    None,
                )
                .await;
            let _ = state
                .store
                .journal(
                    tenant,
                    base.evidence(o.count_evidence_id.clone())
                        .rows(o.records_read as usize),
                )
                .await;
            // The fingerprint the NEXT run is held to. Until 2026-08-30
            // nothing recorded it, so `known_fingerprint` was always None and
            // the drift refusal — asserted offline with a fingerprint handed
            // in by the test — could never fire on a deployed sync. The
            // live tier's scenario 6 is what found it.
            if let Some(fp) = &o.fingerprint {
                let columns = serde_json::Value::Array(
                    fp.tables
                        .iter()
                        .map(|t| {
                            serde_json::json!({
                                "name": t.name,
                                "columns": t.columns.iter().map(|c| serde_json::json!({
                                    "name": c.name,
                                    "type": c.source_type,
                                    "nullable": c.nullable,
                                })).collect::<Vec<_>>(),
                            })
                        })
                        .collect(),
                );
                let _ = state
                    .store
                    .record_fingerprint(tenant, &job.target, entity, &fp.fingerprint, &columns)
                    .await;
            }
        }
        Err(r) => {
            let _ = state
                .store
                .finish_sync_run(&run_id, "refused", 0, 0, 0, 0, 0, None, None, Some(r))
                .await;
            let _ = state.store.journal(tenant, base.refused(r)).await;
        }
    }
    let outcome = outcome?;

    // The checkpoint advances LAST, and only on success. A run that died after
    // uploading re-reads the same window next time and uploads nothing, because
    // the per-record idempotency key already matched. Advancing first would
    // lose records; advancing last can only ever repeat work.
    if let Some(next) = &outcome.next_checkpoint {
        state
            .store
            .save_checkpoint(tenant, next)
            .await
            .map_err(|e| Refusal::source_unavailable(format!("checkpoint save failed: {e}")))?;
    }

    tracing::info!(
        run = %run_id, source = %job.target, entity = %entity, class = %class.name,
        read = outcome.records_read, uploaded = outcome.documents_uploaded,
        skipped = outcome.documents_skipped, excluded = outcome.records_excluded,
        up_to_date = outcome.up_to_date,
        "sync run complete"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// reconcile
// ---------------------------------------------------------------------------

async fn reconcile_loop(state: Arc<AppState>) {
    let worker = worker_id(&state, "reconcile");
    loop {
        if draining(&state) {
            tracing::info!(worker = %worker, "reconcile loop stopping (draining)");
            return;
        }
        match state
            .store
            .claim_mapping_job(&worker, job_lease_secs())
            .await
        {
            Ok(Some(job)) => {
                let id = job.id.clone();
                let outcome = run_one_reconcile(&state, &job).await;
                let (status, detail) = match &outcome {
                    Ok(()) => ("done", None),
                    Err(r) => {
                        tracing::warn!(
                            job = %id, mapping = %job.target, code = %r.code,
                            "reconcile job refused: {}", r.message
                        );
                        ("failed", Some(r.message.as_str()))
                    }
                };
                if let Err(e) = state.store.finish_mapping_job(&id, status, detail).await {
                    tracing::error!(job = %id, error = %e, "could not finish a claimed mapping job");
                }
            }
            Ok(None) => tokio::time::sleep(IDLE_POLL).await,
            Err(e) => {
                tracing::warn!(worker = %worker, error = %e, "could not claim a mapping job");
                tokio::time::sleep(ERROR_BACKOFF).await;
            }
        }
    }
}

/// One mapping pass: read → observe → seal → compare.
///
/// The seal happens BEFORE the comparison, and that ordering is the point. A
/// discrepancy finding cites the observation batch it came from; if the batch
/// were sealed afterwards, a crash between comparing and sealing would leave
/// findings pointing at evidence that does not exist.
async fn run_one_reconcile(state: &Arc<AppState>, job: &ClaimedJob) -> Result<(), Refusal> {
    // TEST HOOK, off unless set: hold the job here for a while so a live
    // check can restart the container mid-run and prove the lease re-claims
    // it. A sub-second pass cannot be aimed into a restart; this makes the
    // window real without making production slower — the variable is not in
    // any terraform env list.
    if let Some(ms) = std::env::var("MUNARIUM_MATRIX_TEST_PAUSE_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
    {
        tracing::warn!(ms, job = %job.id, "MUNARIUM_MATRIX_TEST_PAUSE_MS is set; pausing the reconcile");
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
    }
    let tenant = &job.tenant_id;
    let mapping = match runtime::load_asset(state, tenant, "ClaimMapping", &job.target).await? {
        munarium_matrix_types::Asset::ClaimMapping(m) => *m,
        other => {
            return Err(Refusal::invalid(
                "wrong_kind",
                format!("'{}' is a {}, not a ClaimMapping", job.target, other.kind()),
            ))
        }
    };
    let wiring = runtime::wire(state, tenant, &mapping.spec.source).await?;
    let source = &wiring.source;

    let classes = resolve_classes(&source.spec.authorization)?;
    let class = class_for(&classes, job)?;
    let identity = identity_for(class);

    let run_id = state
        .store
        .start_mapping_run(tenant, &job.target)
        .await
        .map_err(|e| Refusal::source_unavailable(format!("could not open a run row: {e}")))?;

    let result = reconcile_pass(state, &wiring, &mapping, &identity, tenant, &run_id).await;

    match &result {
        Ok(o) => {
            let _ = state
                .store
                .finish_mapping_run(
                    &run_id,
                    "ok",
                    o.observations,
                    o.discrepancies,
                    o.ambiguous,
                    o.findings_filed,
                    o.batch_evidence_id.as_deref(),
                    o.proposals,
                    o.value_nonconforming,
                )
                .await;
            tracing::info!(
                run = %run_id, mapping = %job.target,
                observations = o.observations, agreements = o.agreements,
                discrepancies = o.discrepancies, ambiguous = o.ambiguous,
                findings = o.findings_filed, proposals = o.proposals,
                proposals_disputed = o.proposals_disputed,
                proposals_replayed = o.proposals_replayed,
                withheld_out_of_scope = o.withheld_out_of_scope,
                withheld_document_outranks = o.withheld_document_outranks,
                withheld_requires_review = o.withheld_requires_review,
                nonconforming = o.value_nonconforming,
                canon_untouched = o.canon_untouched,
                "reconcile run complete"
            );
        }
        Err(_) => {
            let _ = state
                .store
                .finish_mapping_run(&run_id, "refused", 0, 0, 0, 0, None, 0, 0)
                .await;
        }
    }
    result.map(|_| ())
}

async fn reconcile_pass(
    state: &Arc<AppState>,
    wiring: &runtime::Wiring,
    mapping: &munarium_matrix_types::ClaimMappingDoc,
    identity: &EffectiveIdentity,
    tenant: &str,
    run_id: &str,
) -> Result<munarium_matrix_workers::ReconcileOutcome, Refusal> {
    let source = &wiring.source;
    let checkpoint = state
        .store
        .load_checkpoint(
            tenant,
            &source.metadata.name,
            &mapping.spec.entity.table,
            &source.metadata.version.to_string(),
        )
        .await
        .map_err(|e| Refusal::source_unavailable(format!("checkpoint read failed: {e}")))?
        .unwrap_or_else(|| {
            Checkpoint::start(
                &source.metadata.name,
                &mapping.spec.entity.table,
                &source.metadata.version.to_string(),
            )
        });

    let (batch, stats, _next) = observe(
        wiring.adapter.as_ref(),
        mapping,
        &checkpoint,
        &ObserveContext {
            tenant,
            source_id: &source.metadata.name,
            batch_id: run_id,
            run_id: Some(run_id),
            limits: source_limits(&source.spec.limits),
            identity,
        },
    )
    .await?;

    if stats.nulls_skipped > 0 {
        // Not an error, and worth saying: "the column is empty everywhere" and
        // "the mapping names the wrong column" look identical without it.
        tracing::info!(
            mapping = %mapping.metadata.name,
            nulls_skipped = stats.nulls_skipped,
            rows = stats.rows_read,
            "some mapped cells were NULL and produced no observation"
        );
    }

    // The version the comparison reads against. Without one there is no ledger
    // to compare to, and inventing a default would compare against the wrong
    // history.
    let version_id = source
        .spec
        .connection
        .get("ledgerVersionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            Refusal::not_covered(format!(
                "source '{}' declares no `connection.ledgerVersionId`, so there is no ledger \
                 version to reconcile against. Mode C needs one; add it to the DataSource.",
                source.metadata.name
            ))
        })?;

    let bytes = serde_json::to_vec(&batch)
        .map_err(|e| Refusal::invalid("batch_unserializable", e.to_string()))?;

    // Authoritative only when the operator promoted THIS mapping version. The
    // asset's `mode: authoritative` is a declaration of intent; the promotion
    // row is the decision. Either alone runs shadow.
    let promoted = match state
        .store
        .active_promotion(tenant, &mapping.metadata.name)
        .await
        .map_err(|e| Refusal::source_unavailable(format!("promotion read failed: {e}")))?
    {
        Some(p) if p.mapping_version == mapping.metadata.version as i32 => true,
        Some(p) => {
            tracing::warn!(
                mapping = %mapping.metadata.name,
                promoted_version = p.mapping_version,
                applied_version = mapping.metadata.version,
                "mapping was promoted at a different version; running SHADOW until re-promoted"
            );
            false
        }
        None => false,
    };
    if mapping.spec.mode == munarium_matrix_types::assets::MappingMode::Authoritative && !promoted {
        tracing::info!(
            mapping = %mapping.metadata.name,
            "authoritative mapping has no active promotion; running shadow"
        );
    }
    let ledger = crate::proposals::StoreLedger { state };
    munarium_matrix_workers::reconcile_with(
        wiring.server.as_ref(),
        mapping,
        version_id,
        &batch,
        &bytes,
        &munarium_matrix_workers::ReconcileOptions {
            tenant,
            promoted,
            source_id: &source.metadata.name,
            proposals: Some(&ledger),
            source_complete: stats.complete,
        },
    )
    .await
}

/// How long a claimed job may run before another worker may re-claim it.
/// `MUNARIUM_MATRIX_JOB_LEASE_SECS`, default 300: longer than any pass the
/// live tier has measured, shorter than an operator's patience.
fn job_lease_secs() -> i64 {
    std::env::var("MUNARIUM_MATRIX_JOB_LEASE_SECS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(300)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AuthMode, Config, Role};
    use munarium_matrix_store::MatrixStore;

    fn state_with_role(role: Role) -> Arc<AppState> {
        let config = Config {
            role,
            http_addr: "127.0.0.1:0".into(),
            ops_addr: "127.0.0.1:0".into(),
            grpc_addr: None,
            database_url: Some("postgres://unused".into()),
            db_max_conns: 1,
            auth: AuthMode::Disabled,
            server_url: None,
            server_token_ref: None,
            target_server_version: "0.3.0".into(),
            max_concurrency: 8,
            egress_default_deny: true,
            log_format_json: false,
            instance_id: "test".into(),
            file_root: None,
            promotion_min_identity_precision: 0.95,
            promotion_min_value_conformance: 0.99,
            admin_enabled: true,
            boot_secret: "test-boot-secret".into(),
        };
        AppState::new(config, MatrixStore::disconnected_for_tests())
    }

    #[tokio::test]
    async fn each_role_spawns_only_the_loops_it_owns() {
        // The loop count IS the role contract: a query container that spawned a
        // sync loop would quietly compete for jobs it was never scaled for.
        assert_eq!(spawn(state_with_role(Role::Sync)).len(), 1);
        assert_eq!(spawn(state_with_role(Role::Reconcile)).len(), 1);
        assert_eq!(spawn(state_with_role(Role::All)).len(), 2);
        assert_eq!(
            spawn(state_with_role(Role::Control)).len(),
            0,
            "the control role schedules work; it does not run it"
        );
        assert_eq!(
            spawn(state_with_role(Role::Query)).len(),
            0,
            "query work arrives over HTTP with a caller holding the deadline"
        );
    }

    #[tokio::test]
    async fn a_draining_process_stops_its_loops_without_touching_the_database() {
        // The drain contract, proven directly rather than by sending a signal:
        // with `draining` set, a loop must exit on its FIRST check — before it
        // claims anything. The store here is deliberately unconnectable, so if
        // the loop reached for the database this test would hang until the
        // pool's connect timeout instead of finishing immediately.
        let state = state_with_role(Role::All);
        state.draining.store(true, Ordering::Relaxed);

        let handles = spawn(state);
        assert_eq!(handles.len(), 2);
        let stopped = tokio::time::timeout(Duration::from_secs(2), async {
            for h in handles {
                h.await.expect("a role loop must not panic on the way out");
            }
        })
        .await;
        assert!(
            stopped.is_ok(),
            "role loops must observe `draining` and exit; a loop that ignores it              holds the process open past its drain window"
        );
    }
}
