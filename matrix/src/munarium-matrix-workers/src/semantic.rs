// SPDX-License-Identifier: Apache-2.0
//! Semantic execution over a metric view: the `MEASURE()`
//! path beside the query-contract path in [`crate::query`].
//!
//! Same protocol, one extra gate. A metric view is a definition the SOURCE
//! owns and can change without telling anyone, so before the compiled
//! statement runs the adapter reports the view's current definition and its
//! fingerprint is compared with the one recorded when the view's verified
//! questions last passed. No record: the view is not evidence yet, and the
//! intent is refused `not_covered` naming the verify step. A different
//! fingerprint: `metric_view_changed`, until an operator verifies again. The
//! comparison happens BEFORE the statement, so a changed definition never
//! produces a number that is then explained away.
//!
//! Everything after that gate is the contract path's: bind by name, execute
//! under the effective limits, take the declared schema over the driver's
//! inference, seal through the same `evidence::seal`, cite rows by key.

use crate::evidence::{seal, SealContext};
use crate::query::{reconcile_schema, ExecuteContext, VerifiedQuestionOutcome};
use munarium_matrix_adapter::{bind_named, Limits, SourceAdapter};
use munarium_matrix_core::semantic::{self, FilterRef, SemanticRequest};
use munarium_matrix_core::value::ColumnType;
use munarium_matrix_core::Refusal;
use munarium_matrix_server_client::ServerClient;
use munarium_matrix_types::assets::{DataViewDoc, MetricVerifiedQuestion, MetricViewDoc};
use munarium_matrix_types::contract::*;
use munarium_matrix_types::validate::{data_view_scope, semantic_scope};
use std::collections::BTreeMap;

/// The two assets the semantic path serves: a metric view the source owns
/// or a native data view over one fact table. Same gate,
/// same seal; only the compiled aggregate and the fingerprinted object differ.
#[derive(Debug, Clone, Copy)]
pub enum SemanticView<'a> {
    Metric(&'a MetricViewDoc),
    Native(&'a DataViewDoc),
}

impl<'a> SemanticView<'a> {
    pub fn kind(&self) -> &'static str {
        match self {
            SemanticView::Metric(_) => "MetricView",
            SemanticView::Native(_) => "DataView",
        }
    }
    pub fn name(&self) -> &'a str {
        match self {
            SemanticView::Metric(d) => &d.metadata.name,
            SemanticView::Native(d) => &d.metadata.name,
        }
    }
    pub fn version(&self) -> u32 {
        match self {
            SemanticView::Metric(d) => d.metadata.version,
            SemanticView::Native(d) => d.metadata.version,
        }
    }
    pub fn asset_ref(&self) -> String {
        match self {
            SemanticView::Metric(d) => d.metadata.asset_ref(),
            SemanticView::Native(d) => d.metadata.asset_ref(),
        }
    }
    pub fn source(&self) -> &'a str {
        match self {
            SemanticView::Metric(d) => &d.spec.source,
            SemanticView::Native(d) => &d.spec.source,
        }
    }
    /// The catalog object whose definition is fingerprinted.
    pub fn definition_object(&self) -> &'a str {
        match self {
            SemanticView::Metric(d) => &d.spec.view,
            SemanticView::Native(d) => &d.spec.table,
        }
    }
    fn limits(&self) -> &'a munarium_matrix_types::assets::ContractLimits {
        match self {
            SemanticView::Metric(d) => &d.spec.limits,
            SemanticView::Native(d) => &d.spec.limits,
        }
    }
    fn denied_columns(&self) -> Vec<String> {
        match self {
            SemanticView::Metric(d) => d.spec.policy.denied_columns.clone(),
            SemanticView::Native(d) => d.spec.policy.denied_columns.clone(),
        }
    }
    fn retention_days(&self) -> u32 {
        match self {
            SemanticView::Metric(d) => d.spec.evidence.retention_days,
            SemanticView::Native(d) => d.spec.evidence.retention_days,
        }
    }
    fn verified_questions(&self) -> &'a [MetricVerifiedQuestion] {
        match self {
            SemanticView::Metric(d) => &d.spec.verified_questions,
            SemanticView::Native(d) => &d.spec.verified_questions,
        }
    }
    /// The closed lists, with the engine's quoting and placeholder style.
    ///
    /// Fallible because the dialect comes from the ADAPTER, not from a
    /// literal: an engine this build does not know how to quote for is
    /// refused rather than compiled under Postgres conventions, which is what
    /// a catch-all did until 2026-08-30 — a native view on MySQL emitted
    /// `FROM "opportunities"`, and MySQL reads that as a string literal.
    fn scope(&self, dialect: &str) -> Result<semantic::SemanticScope, Refusal> {
        match self {
            // A metric view is compiled by the PROVIDER's own `MEASURE()`
            // grammar, not by this quoting table, so it takes the default.
            SemanticView::Metric(d) => Ok(semantic_scope(&d.spec)),
            SemanticView::Native(d) => data_view_scope(&d.spec).try_with_dialect(dialect),
        }
    }
    fn require_capability(
        &self,
        caps: &munarium_matrix_adapter::Capabilities,
    ) -> Result<(), Refusal> {
        match self {
            SemanticView::Metric(_) => caps.require_metric_views(),
            SemanticView::Native(_) => caps.require_data_views(),
        }
    }
}

/// What a verification produced: the fingerprint the questions ran under,
/// and each question's outcome. The caller records the fingerprint only
/// when every question passed — a failing suite proves nothing about the
/// definition except that it is not the one that used to pass.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricVerifyOutcome {
    pub fingerprint: String,
    pub questions: Vec<VerifiedQuestionOutcome>,
}

fn effective_limits(source: Limits, view: SemanticView<'_>, intent: &QueryIntent) -> Limits {
    let l = view.limits();
    Limits {
        max_rows: source.max_rows.min(l.max_rows).min(intent.limits.max_rows),
        max_bytes: source
            .max_bytes
            .min(l.max_bytes)
            .min(intent.limits.max_bytes),
        timeout_ms: source.timeout_ms.min(l.timeout_ms),
    }
}

/// Execute a semantic intent against a metric view and seal the result.
///
/// `verified_fingerprint` is the fingerprint recorded by the last PASSING
/// verification of this view version, or `None` when there is none.
pub async fn execute_metric(
    adapter: &dyn SourceAdapter,
    server: &dyn ServerClient,
    view: SemanticView<'_>,
    intent: &QueryIntent,
    verified_fingerprint: Option<&str>,
    ctx: &ExecuteContext<'_>,
) -> Result<EvidenceBlock, Refusal> {
    execute_metric_with_result(adapter, server, view, intent, verified_fingerprint, ctx)
        .await
        .map(|(block, _)| block)
}

/// [`execute_metric`], also returning the typed result — for invariants.
pub async fn execute_metric_with_result(
    adapter: &dyn SourceAdapter,
    server: &dyn ServerClient,
    view: SemanticView<'_>,
    intent: &QueryIntent,
    verified_fingerprint: Option<&str>,
    ctx: &ExecuteContext<'_>,
) -> Result<(EvidenceBlock, munarium_matrix_core::TypedResult), Refusal> {
    execute_metric_traced(adapter, server, view, intent, verified_fingerprint, ctx)
        .await
        .map(|t| (t.block, t.result))
}

/// The whole semantic path, with the timings beside the block
/// ([`crate::query::Traced`]) — the same shape the contract path returns, so
/// one journal row and one `Server-Timing` header serve both.
pub async fn execute_metric_traced(
    adapter: &dyn SourceAdapter,
    server: &dyn ServerClient,
    view: SemanticView<'_>,
    intent: &QueryIntent,
    verified_fingerprint: Option<&str>,
    ctx: &ExecuteContext<'_>,
) -> Result<crate::query::Traced, Refusal> {
    if intent.kind != IntentKind::Semantic {
        return Err(Refusal::invalid(
            "not_covered",
            "a metric view answers semantic intents; a structured query names a query contract",
        ));
    }
    let sem = intent
        .semantic
        .as_ref()
        .ok_or_else(|| Refusal::invalid("not_covered", "a semantic intent carries `semantic`"))?;
    view.require_capability(&adapter.capabilities())?;

    // --- 1. Compile against the asset's closed lists. Before any budget,
    // before the source is touched.
    let scope = view.scope(ctx.dialect)?;
    let filters: Vec<FilterRef<'_>> = sem
        .filters
        .iter()
        .map(|f| FilterRef {
            dimension: &f.dimension,
            op: &f.op,
        })
        .collect();
    let compiled = semantic::compile(
        &scope,
        &SemanticRequest {
            measures: &sem.measures,
            dimensions: &sem.dimensions,
            filters,
        },
    )?;

    // --- 2. Deadline, as the contract path does it.
    let limits = effective_limits(ctx.source_limits, view, intent);
    let limits = match intent.deadline_at {
        Some(deadline) => {
            let remaining = (deadline - chrono::Utc::now()).num_milliseconds();
            if remaining <= 0 {
                return Err(Refusal::deadline_exceeded(
                    "the intent's deadline had already passed when it arrived",
                ));
            }
            Limits {
                timeout_ms: limits.timeout_ms.min(remaining as u64),
                ..limits
            }
        }
        None => limits,
    };

    // --- 3. The gate this path adds: is the definition the verified one?
    let definition = adapter
        .definition_of(view.definition_object(), limits)
        .await?;
    let fingerprint = semantic::fingerprint(&definition);
    match verified_fingerprint {
        None => {
            return Err(Refusal::not_covered(format!(
                "{} {} has no passing verification on record; verify it before it is executed",
                view.kind(),
                view.asset_ref()
            )))
        }
        Some(v) if v != fingerprint => {
            return Err(Refusal::metric_view_changed(format!(
                "the definition of {} is not the one that was verified ({fingerprint} now, {v} verified); \
                 verify it again before it is executed",
                view.definition_object()
            )))
        }
        Some(_) => {}
    }

    // --- 4. Bind the filter values by name, typed as their dimensions.
    let values: Vec<(String, TypedValueDto, ColumnType, Option<u32>)> = compiled
        .parameter_names
        .iter()
        .zip(&compiled.parameter_dimensions)
        .zip(&sem.filters)
        .map(|((name, dim), f)| {
            let ty = scope
                .dimensions
                .get(dim)
                .map(|d| d.ty)
                .unwrap_or(ColumnType::String);
            (name.clone(), f.value.clone(), ty, None)
        })
        .collect();
    let bound = bind_named(&values)?;

    // --- 5. Execute. A provider that owns the metric definitions answers the
    // ask directly; everything else runs the compiled statement. The
    // declared shape wins over the driver's inference either way.
    let executed = match adapter
        .semantic_execute(
            &munarium_matrix_adapter::SemanticAsk {
                view: scope.view.clone(),
                measures: sem.measures.clone(),
                dimensions: sem.dimensions.clone(),
                filters: sem
                    .filters
                    .iter()
                    .map(|f| {
                        (
                            f.dimension.clone(),
                            f.op.clone(),
                            match &f.value.value {
                                serde_json::Value::String(v) => v.clone(),
                                other => other.to_string(),
                            },
                        )
                    })
                    .collect(),
            },
            limits,
        )
        .await?
    {
        Some(e) => e,
        None => {
            adapter
                .execute(&compiled.sql, &bound, ctx.identity, limits)
                .await?
        }
    };
    let source_ms = (executed.ended_at - executed.started_at)
        .num_milliseconds()
        .max(0) as u64;
    let mut result = executed.result;
    result.schema = reconcile_schema(&compiled.schema, &result.schema)?;
    // Cells conform to the measures' declared scales, exactly as on the
    // contract path — an imported semantic layer's wire can render a decimal
    // minimally too, and identity must not vary by provider.
    result.conform_decimal_scales()?;
    result.authorization_class = ctx.authorization_class.clone();
    result.denied_columns = view.denied_columns();

    if let Some(f) = &intent.freshness {
        let age = (chrono::Utc::now() - executed.ended_at)
            .num_seconds()
            .max(0) as u64;
        if age > f.max_staleness_seconds && f.on_violation == FreshnessAction::Refuse {
            return Err(Refusal::source_stale(format!(
                "result is {age}s old, over the profile's {}s bound",
                f.max_staleness_seconds
            )));
        }
    }

    // --- 6. Seal. The manifest names the metric view as the semantic
    // provider and carries the semantic plan hash; the artifact kind and the
    // resolution path are exactly a contract's, so a citation
    // `[evidence/<id>#<row>]` reads back the same way.
    let ctx_seal = SealContext {
        tenant: intent.authorization.tenant.clone(),
        kind: ArtifactKind::Table,
        source_id: ctx.source_id.to_string(),
        source_version: ctx.source_version,
        adapter: adapter.kind().to_string(),
        adapter_version: Some(adapter.adapter_version().to_string()),
        engine: executed.engine.clone(),
        versions: ManifestVersions {
            semantic_provider: Some(match adapter.capabilities().semantic_provider {
                Some(family) => format!("{}:{}", family, view.asset_ref()),
                None => view.asset_ref(),
            }),
            policy: Some(format!("policy@{}", view.version())),
            compiler: Some(munarium_matrix_core::COMPILER_VERSION.to_string()),
            ..Default::default()
        },
        plan: Some(ManifestPlan {
            canonical_plan_hash: compiled.plan_hash.clone(),
            bound_parameters_hash: bound.hash(),
        }),
        snapshot_marker: executed.snapshot_marker.clone(),
        isolation: executed.isolation.clone(),
        replay_level: adapter.capabilities().replay_level,
        effective_principal: Some(ctx.identity.principal.clone()),
        statement_id: executed.statement_id.clone(),
        started_at: executed.started_at,
        ended_at: executed.ended_at,
        retention_days: intent.seal.retention_days.or(Some(view.retention_days())),
        declared_max_rows: Some(limits.max_rows),
        rows_covered: Some(result.rows.len() as u64),
        rows_excluded: None,
        exclusion_reason: None,
        freshness_watermark: None,
    };
    let seal_started = std::time::Instant::now();
    let (evidence_id, manifest) = seal(
        server,
        &result,
        &ctx_seal,
        intent.seal.idempotency_key.as_deref(),
    )
    .await?;
    let seal_ms = seal_started.elapsed().as_millis() as u64;

    let rows: Vec<BlockRow> = result
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| BlockRow {
            row_id: munarium_matrix_core::row_id(&result, i),
            cells: row.cells.iter().map(|c| c.canonical_text()).collect(),
        })
        .collect();

    let block = EvidenceBlock::CompleteTable {
        contract_version: munarium_matrix_core::CONTRACT_VERSION.to_string(),
        evidence_id,
        manifest: Box::new(manifest),
        rows,
        truncated: result.truncated,
        derivations: Vec::new(),
    };
    Ok(crate::query::Traced {
        block,
        result,
        timings: crate::query::ExecuteTimings { source_ms, seal_ms },
    })
}

/// Run the view's verified questions under the definition the source reports
/// NOW, and return that definition's fingerprint with the outcomes.
///
/// Each question executes as a self-consistent verification — the fingerprint
/// it is checked against is the one just read — so the questions test the
/// data and the compiler, and the caller's record of the fingerprint is what
/// later executes are held to.
pub async fn verify_metric(
    adapter: &dyn SourceAdapter,
    server: &dyn ServerClient,
    view: SemanticView<'_>,
    tenant: &str,
    ctx: &ExecuteContext<'_>,
) -> Result<MetricVerifyOutcome, Refusal> {
    view.require_capability(&adapter.capabilities())?;
    let vl = view.limits();
    let limits = Limits {
        max_rows: ctx.source_limits.max_rows.min(vl.max_rows),
        max_bytes: ctx.source_limits.max_bytes.min(vl.max_bytes),
        timeout_ms: ctx.source_limits.timeout_ms.min(vl.timeout_ms),
    };
    let definition = adapter
        .definition_of(view.definition_object(), limits)
        .await?;
    let fingerprint = semantic::fingerprint(&definition);
    let scope = view.scope(ctx.dialect)?;

    let mut questions = Vec::new();
    for q in view.verified_questions() {
        let filters: Vec<SemanticFilter> = q
            .intent
            .filters
            .iter()
            .map(|f| SemanticFilter {
                dimension: f.dimension.clone(),
                op: f.op.clone(),
                value: TypedValueDto {
                    ty: scope
                        .dimensions
                        .get(&f.dimension)
                        .map(|d| d.ty)
                        .unwrap_or(ColumnType::String),
                    value: f.value.clone(),
                    scale: None,
                    element_type: None,
                },
            })
            .collect();
        let intent = QueryIntent {
            contract_version: munarium_matrix_core::CONTRACT_VERSION.to_string(),
            kind: IntentKind::Semantic,
            request_id: None,
            contract: None,
            semantic: Some(SemanticIntent {
                provider: view.name().to_string(),
                measures: q.intent.measures.clone(),
                dimensions: q.intent.dimensions.clone(),
                filters,
                grain: None,
            }),
            parameters: BTreeMap::new(),
            authorization: AuthorizationSnapshot {
                tenant: tenant.to_string(),
                uid: None,
                access_level: ctx.authorization_class.access_level,
                compartments: ctx.authorization_class.compartments.clone(),
                session_id: None,
                runbook_ref: None,
            },
            limits: IntentLimits {
                max_rows: vl.max_rows,
                max_bytes: vl.max_bytes,
                max_cells: None,
            },
            deadline_at: None,
            freshness: None,
            seal: SealPolicy {
                required: false,
                retention_days: None,
                idempotency_key: None,
            },
        };

        let mut failures = Vec::new();
        let (rows, hash) = match execute_metric_with_result(
            adapter,
            server,
            view,
            &intent,
            Some(&fingerprint),
            ctx,
        )
        .await
        {
            Ok((EvidenceBlock::CompleteTable { manifest, rows, .. }, result)) => {
                failures.extend(crate::query::check_invariants(
                    &q.expect.invariants,
                    &result,
                ));
                (Some(rows.len()), Some(manifest.logical_result_hash.clone()))
            }
            Ok((other, _)) => {
                failures.push(format!("unexpected block kind {other:?}"));
                (None, None)
            }
            Err(r) => {
                failures.push(format!("{r}"));
                (None, None)
            }
        };
        if let (Some(expected), Some(actual)) = (q.expect.rows, rows) {
            if expected != actual {
                failures.push(format!("expected {expected} rows, got {actual}"));
            }
        }
        if let (Some(expected), Some(actual)) = (&q.expect.logical_result_hash, &hash) {
            if expected != actual {
                failures.push(format!(
                    "logical result hash changed: expected {expected}, got {actual}"
                ));
            }
        }
        questions.push(VerifiedQuestionOutcome {
            question: q.question.clone(),
            ok: failures.is_empty(),
            rows,
            logical_result_hash: hash,
            failures,
        });
    }
    Ok(MetricVerifyOutcome {
        fingerprint,
        questions,
    })
}
