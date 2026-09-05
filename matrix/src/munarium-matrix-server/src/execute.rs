// SPDX-License-Identifier: Apache-2.0
//! The one execute path, behind both planes.
//!
//! `POST /v1/contracts/{name}/execute` and `matrix.v1.MatrixQuery/Execute`
//! call this and nothing else. It used to be the REST handler's body; it was
//! lifted out when the gRPC plane arrived (2026-08-29) so that the two planes
//! cannot diverge — the same tenant check, the same budget reservation before
//! the source is touched, the same settle-or-release, the same journal record
//! (which names the plane in `via`), the same metrics.
//!
//! `progress` is called at each stage. REST passes a no-op; gRPC turns each
//! call into a `Progress` event on the stream.

use crate::rest::{class_for_intent, dialect_of, pinned_domains, source_was_touched};
use crate::state::{AppState, Caller};
use munarium_matrix_core::Refusal;
use munarium_matrix_store::journal::JournalRecord;
use munarium_matrix_types::contract::{EvidenceBlock, IntentKind, QueryIntent};

/// Where one execution's time went, as the REST plane reports it in a
/// `Server-Timing` header and the journal keeps it (2026-08-30, §18.3).
///
/// `total_ms` is the wall clock around the workers' call — bind, compile,
/// the statement, canonicalize, seal. `source_ms` and `seal_ms` are the two
/// pieces Matrix does not own; the difference is Matrix's own share, which is
/// what the plan's transport-share formula needs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExecuteReport {
    pub total_ms: u64,
    pub source_ms: u64,
    pub seal_ms: u64,
}

impl ExecuteReport {
    /// Matrix's own time: everything the source and the seal did not take.
    pub fn matrix_ms(&self) -> u64 {
        self.total_ms.saturating_sub(self.source_ms + self.seal_ms)
    }

    /// The `Server-Timing` header value (RFC-standard shape; browsers and
    /// `curl -i` both show it). Names are deliberately short and stable —
    /// the measurement harness parses them.
    pub fn server_timing(&self) -> String {
        format!(
            "total;dur={}, source;dur={}, seal;dur={}, matrix;dur={}",
            self.total_ms,
            self.source_ms,
            self.seal_ms,
            self.matrix_ms()
        )
    }
}

/// Run a contract for an intent. Every failure is a typed refusal; a store
/// failure on the way is reported as the source being unavailable, because
/// from the caller's side that is what it is.
pub async fn execute_intent(
    state: &std::sync::Arc<AppState>,
    caller: &Caller,
    name: &str,
    intent: &QueryIntent,
    request_id: Option<String>,
    via: &'static str,
    progress: impl FnMut(&'static str),
) -> Result<EvidenceBlock, Refusal> {
    execute_intent_timed(state, caller, name, intent, request_id, via, progress)
        .await
        .map(|(block, _)| block)
}

/// [`execute_intent`], with the timing breakdown beside the block. The REST
/// plane uses this to answer a `Server-Timing` header; the other planes take
/// the block alone.
pub async fn execute_intent_timed(
    state: &std::sync::Arc<AppState>,
    caller: &Caller,
    name: &str,
    intent: &QueryIntent,
    request_id: Option<String>,
    via: &'static str,
    mut progress: impl FnMut(&'static str),
) -> Result<(EvidenceBlock, ExecuteReport), Refusal> {
    // The intent carries its own tenant in the authorization snapshot. It must
    // match the token's, or a caller could seal evidence into someone else's
    // tenant by editing a field.
    if intent.authorization.tenant != caller.tenant {
        return Err(Refusal::policy_denied(format!(
            "the intent's authorization snapshot names tenant '{}' but this token is for '{}'",
            intent.authorization.tenant, caller.tenant
        )));
    }

    // A semantic intent names a metric view, not a contract; the same
    // pipeline with the fingerprint gate in the middle.
    if intent.kind == IntentKind::Semantic {
        return execute_metric_intent(state, caller, name, intent, request_id, via, progress).await;
    }

    progress("loading");
    let contract = crate::runtime::load_contract(state, &caller.tenant, name).await?;
    progress("wiring");
    let wiring = crate::runtime::wire(state, &caller.tenant, &contract.spec.source).await?;

    let classes = munarium_matrix_workers::resolve_classes(&wiring.source.spec.authorization)?;
    let class = class_for_intent(&classes, &intent.authorization)?;
    let dialect = dialect_of(wiring.adapter.as_ref())?;
    let domains = pinned_domains(state, &caller.tenant, &contract).await;

    let ctx = munarium_matrix_workers::ExecuteContext {
        source_id: &wiring.source.metadata.name,
        source_version: wiring.source.metadata.version,
        dialect: &dialect,
        pinned_domains: &domains,
        identity: &munarium_matrix_adapter::EffectiveIdentity {
            class: Some(class.name.clone()),
            credential_ref: class.credential_ref.clone(),
            principal: class
                .credential_ref
                .clone()
                .unwrap_or_else(|| "source-native".into()),
        },
        authorization_class: class.as_core(),
        source_limits: munarium_matrix_adapter::Limits {
            max_rows: wiring.source.spec.limits.max_rows,
            max_bytes: wiring.source.spec.limits.max_bytes,
            timeout_ms: wiring.source.spec.limits.statement_timeout_ms,
        },
    };

    // --- Budget. Reserved BEFORE the source is touched, because a ceiling
    // enforced after the work is a report, not a ceiling.
    progress("budget");
    let reservation = match state
        .store
        .reserve_budget(
            &caller.tenant,
            &wiring.source.metadata.name,
            1,
            wiring.source.spec.limits.budget_per_hour,
        )
        .await
        .map_err(|e| Refusal::source_unavailable(format!("budget store: {e}")))?
    {
        munarium_matrix_store::BudgetOutcome::Granted(r) => Some(r),
        munarium_matrix_store::BudgetOutcome::Unlimited => None,
        munarium_matrix_store::BudgetOutcome::Exhausted {
            requested,
            remaining,
            limit,
        } => {
            let refusal = Refusal::budget_exceeded(format!(
                "source '{}' has {remaining} of {limit} unit(s) left this hour and this \
                 execution needs {requested}",
                wiring.source.metadata.name
            ));
            journal(
                state,
                caller,
                JournalRecord::new("execute", "refused")
                    .asset(contract.metadata.asset_ref())
                    .source(&wiring.source.metadata.name)
                    .request(request_id)
                    .via(via)
                    .refused(&refusal),
            )
            .await;
            state
                .metrics
                .inc("matrix_executions_total", &[("result", "exhausted")]);
            return Err(refusal);
        }
    };

    progress("executing");
    let started = std::time::Instant::now();
    let outcome = munarium_matrix_workers::execute_traced(
        wiring.adapter.as_ref(),
        wiring.server.as_ref(),
        &contract,
        intent,
        &ctx,
    )
    .await;
    let elapsed = started.elapsed();

    // Settle or release. A reservation left held is reclaimed by the sweep, so
    // the failure direction here is "budget stays spent", which is the safe one.
    if let Some(r) = &reservation {
        let spent = outcome.is_ok() || outcome.as_ref().err().is_some_and(source_was_touched);
        let _ = if spent {
            state.store.settle_budget(r, None).await
        } else {
            state.store.release_budget(r).await
        };
    }

    // Outcome is a LABEL, not two metrics: a refusal rate is only meaningful
    // against the total, and splitting them makes that a query rather than a
    // reading.
    let result = if outcome.is_ok() { "ok" } else { "refused" };
    state
        .metrics
        .inc("matrix_executions_total", &[("result", result)]);
    state.metrics.observe_ms(
        "matrix_execute_duration_ms",
        &[("result", result)],
        elapsed.as_millis() as u64,
    );

    let base = JournalRecord::new("execute", result)
        .asset(contract.metadata.asset_ref())
        .source(&wiring.source.metadata.name)
        .request(request_id)
        .via(via)
        .duration(elapsed.as_millis());
    match &outcome {
        Ok(t) => {
            journal(
                state,
                caller,
                base.evidence(t.block.evidence_id().map(str::to_string))
                    .rows(rows_of(&t.block))
                    .timings(t.timings.source_ms, t.timings.seal_ms),
            )
            .await
        }
        Err(r) => journal(state, caller, base.refused(r)).await,
    }
    if outcome.is_ok() {
        progress("sealed");
    }
    outcome.map(|t| {
        let report = ExecuteReport {
            total_ms: elapsed.as_millis() as u64,
            source_ms: t.timings.source_ms,
            seal_ms: t.timings.seal_ms,
        };
        (t.block, report)
    })
}

/// The semantic half of [`execute_intent`]: load the metric view instead of a
/// contract, look up the fingerprint its last verification recorded, and run
/// the workers' semantic path under the same budget, journal and metrics.
async fn execute_metric_intent(
    state: &std::sync::Arc<AppState>,
    caller: &Caller,
    name: &str,
    intent: &QueryIntent,
    request_id: Option<String>,
    via: &'static str,
    mut progress: impl FnMut(&'static str),
) -> Result<(EvidenceBlock, ExecuteReport), Refusal> {
    progress("loading");
    let doc = crate::runtime::load_semantic_view(state, &caller.tenant, name, None).await?;
    let view = doc.as_view();
    progress("wiring");
    let wiring = crate::runtime::wire(state, &caller.tenant, view.source()).await?;

    let classes = munarium_matrix_workers::resolve_classes(&wiring.source.spec.authorization)?;
    let class = class_for_intent(&classes, &intent.authorization)?;
    let dialect = dialect_of(wiring.adapter.as_ref())?;
    let domains = std::collections::BTreeMap::new();

    let ctx = munarium_matrix_workers::ExecuteContext {
        source_id: &wiring.source.metadata.name,
        source_version: wiring.source.metadata.version,
        dialect: &dialect,
        pinned_domains: &domains,
        identity: &munarium_matrix_adapter::EffectiveIdentity {
            class: Some(class.name.clone()),
            credential_ref: class.credential_ref.clone(),
            principal: class
                .credential_ref
                .clone()
                .unwrap_or_else(|| "source-native".into()),
        },
        authorization_class: class.as_core(),
        source_limits: munarium_matrix_adapter::Limits {
            max_rows: wiring.source.spec.limits.max_rows,
            max_bytes: wiring.source.spec.limits.max_bytes,
            timeout_ms: wiring.source.spec.limits.statement_timeout_ms,
        },
    };

    // The fingerprint a passing verification recorded. A failing latest
    // record counts as none: the last word on the definition is that it no
    // longer answers as it did.
    let verified = state
        .store
        .latest_metric_verification(&caller.tenant, view.kind(), view.name(), view.version())
        .await
        .map_err(|e| Refusal::source_unavailable(format!("verification store: {e}")))?
        .filter(|v| v.failed == 0)
        .map(|v| v.fingerprint);

    progress("budget");
    let reservation = match state
        .store
        .reserve_budget(
            &caller.tenant,
            &wiring.source.metadata.name,
            1,
            wiring.source.spec.limits.budget_per_hour,
        )
        .await
        .map_err(|e| Refusal::source_unavailable(format!("budget store: {e}")))?
    {
        munarium_matrix_store::BudgetOutcome::Granted(r) => Some(r),
        munarium_matrix_store::BudgetOutcome::Unlimited => None,
        munarium_matrix_store::BudgetOutcome::Exhausted {
            requested,
            remaining,
            limit,
        } => {
            let refusal = Refusal::budget_exceeded(format!(
                "source '{}' has {remaining} of {limit} unit(s) left this hour and this \
                 execution needs {requested}",
                wiring.source.metadata.name
            ));
            journal(
                state,
                caller,
                JournalRecord::new("execute", "refused")
                    .asset(view.asset_ref())
                    .source(&wiring.source.metadata.name)
                    .request(request_id)
                    .via(via)
                    .refused(&refusal),
            )
            .await;
            state
                .metrics
                .inc("matrix_executions_total", &[("result", "exhausted")]);
            return Err(refusal);
        }
    };

    progress("executing");
    let started = std::time::Instant::now();
    let outcome = munarium_matrix_workers::execute_metric_traced(
        wiring.adapter.as_ref(),
        wiring.server.as_ref(),
        view,
        intent,
        verified.as_deref(),
        &ctx,
    )
    .await;
    let elapsed = started.elapsed();

    if let Some(r) = &reservation {
        let spent = outcome.is_ok() || outcome.as_ref().err().is_some_and(source_was_touched);
        let _ = if spent {
            state.store.settle_budget(r, None).await
        } else {
            state.store.release_budget(r).await
        };
    }

    let result = if outcome.is_ok() { "ok" } else { "refused" };
    state
        .metrics
        .inc("matrix_executions_total", &[("result", result)]);
    state.metrics.observe_ms(
        "matrix_execute_duration_ms",
        &[("result", result)],
        elapsed.as_millis() as u64,
    );

    let base = JournalRecord::new("execute", result)
        .asset(view.asset_ref())
        .source(&wiring.source.metadata.name)
        .request(request_id)
        .via(via)
        .duration(elapsed.as_millis());
    match &outcome {
        Ok(t) => {
            journal(
                state,
                caller,
                base.evidence(t.block.evidence_id().map(str::to_string))
                    .rows(rows_of(&t.block))
                    .timings(t.timings.source_ms, t.timings.seal_ms),
            )
            .await
        }
        Err(r) => journal(state, caller, base.refused(r)).await,
    }
    if outcome.is_ok() {
        progress("sealed");
    }
    outcome.map(|t| {
        let report = ExecuteReport {
            total_ms: elapsed.as_millis() as u64,
            source_ms: t.timings.source_ms,
            seal_ms: t.timings.seal_ms,
        };
        (t.block, report)
    })
}

/// Rows a block carries, for the journal's `rows_out`.
///
/// A `Count` block is one row by construction — the count IS the row — and a
/// block with no rows at all reports zero. Reporting a count block as zero rows
/// would make a journal reader think it returned nothing.
pub fn rows_of(block: &EvidenceBlock) -> usize {
    match block {
        EvidenceBlock::CompleteTable { rows, .. } => rows.len(),
        EvidenceBlock::Count { .. } => 1,
        _ => 0,
    }
}

async fn journal(state: &AppState, caller: &Caller, rec: JournalRecord) {
    if let Err(e) = state.store.journal(&caller.tenant, rec).await {
        tracing::warn!(error = %e, tenant = %caller.tenant, "journal write failed");
    }
}
