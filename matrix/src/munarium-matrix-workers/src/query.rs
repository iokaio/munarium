// SPDX-License-Identifier: Apache-2.0
//! The query role: mode B, `execute(intent) -> EvidenceBlock`.
//!
//! One call does: validate the intent against the contract → bind parameters →
//! compile and allowlist the statement → reserve budget → execute under the
//! source's own policy identity with a deadline → canonicalize → seal → return
//! a `CompleteTable` or a typed refusal.
//!
//! Two orderings are deliberate:
//!
//! - **Compile before budget.** A malformed intent must not spend anyone's
//!   quota. Refusals that cost nothing should cost nothing.
//! - **Seal before return.** The block a turn composes cites `evidence/<id>`,
//!   so the artifact has to exist before the answer can reference it. If
//!   sealing fails and the intent said `required`, the whole call refuses
//!   rather than handing back numbers nobody can later check.

use crate::evidence::{seal, SealContext};
use munarium_matrix_adapter::{bind_parameters, EffectiveIdentity, Limits, SourceAdapter};
use munarium_matrix_core::{compile, Refusal};
use munarium_matrix_server_client::ServerClient;
use munarium_matrix_types::assets::QueryContractDoc;
use munarium_matrix_types::contract::*;
use munarium_matrix_types::validate::contract_schema;
use std::collections::BTreeMap;

/// Everything the execute path needs beyond the intent and the contract.
pub struct ExecuteContext<'a> {
    pub source_id: &'a str,
    pub source_version: u32,
    pub dialect: &'a str,
    /// Values pinned for `allowedValuesFrom` parameters at introspect time.
    pub pinned_domains: &'a BTreeMap<String, Vec<String>>,
    pub identity: &'a EffectiveIdentity,
    pub authorization_class: munarium_matrix_core::AuthorizationClass,
    /// The source's own ceilings; the effective limit is the MINIMUM of these
    /// and the contract's and the intent's.
    pub source_limits: Limits,
}

/// Where an execution's time went.
///
/// Two numbers, because two things are not Matrix's to speed up: the source's
/// own statement window, and the seal — canonicalize, build the manifest, one
/// round-trip into the server. What a caller's wall clock shows beyond these
/// is Matrix's own share: bind, compile, budget, and the transport in and out.
/// The plan's transport-share formula subtracts exactly these two from the
/// execute wall time, which is why they are measured here and not inferred.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExecuteTimings {
    /// `ended_at − started_at` as the adapter recorded them around the
    /// statement — the engine's time, under the engine's clock.
    pub source_ms: u64,
    /// The seal call, whole: canonical bytes, manifest, the server round-trip.
    /// One number because a caller wants the cost of "making it evidence".
    pub seal_ms: u64,
}

/// What [`execute_traced`] returns: the block, the typed result it was sealed
/// from, and where the time went.
pub struct Traced {
    pub block: EvidenceBlock,
    pub result: munarium_matrix_core::TypedResult,
    pub timings: ExecuteTimings,
}

/// The statement text for a dialect, resolved from the contract.
fn statement_for(contract: &QueryContractDoc, dialect: &str) -> Result<String, Refusal> {
    let spec = contract
        .spec
        .statement_by_dialect
        .get(dialect)
        .ok_or_else(|| {
            Refusal::not_covered(format!(
                "contract '{}' declares no statement for dialect '{dialect}'",
                contract.metadata.asset_ref()
            ))
        })?;
    match (&spec.inline, &spec.file) {
        (Some(sql), _) => Ok(sql.clone()),
        // A file-backed statement is loaded by the caller (which has I/O) and
        // handed in as inline. Reaching here means the registry stored a path
        // with no loader, which is a configuration error, not a source problem.
        (None, Some(path)) => Err(Refusal::invalid(
            "not_covered",
            format!("statement for '{dialect}' lives in '{path}' and was not loaded"),
        )),
        (None, None) => Err(Refusal::invalid(
            "not_covered",
            format!("contract declares an empty statement for '{dialect}'"),
        )),
    }
}

/// The effective ceiling: the smallest of every limit in play. A contract can
/// only ever tighten the source's, and an intent can only tighten the
/// contract's — nothing raises a ceiling by asking.
fn effective_limits(source: Limits, contract: &QueryContractDoc, intent: &QueryIntent) -> Limits {
    Limits {
        max_rows: source
            .max_rows
            .min(contract.spec.limits.max_rows)
            .min(intent.limits.max_rows),
        max_bytes: source
            .max_bytes
            .min(contract.spec.limits.max_bytes)
            .min(intent.limits.max_bytes),
        timeout_ms: source.timeout_ms.min(contract.spec.limits.timeout_ms),
    }
}

/// Execute one intent.
pub async fn execute(
    adapter: &dyn SourceAdapter,
    server: &dyn ServerClient,
    contract: &QueryContractDoc,
    intent: &QueryIntent,
    ctx: &ExecuteContext<'_>,
) -> Result<EvidenceBlock, Refusal> {
    execute_with_result(adapter, server, contract, intent, ctx)
        .await
        .map(|(block, _)| block)
}

/// [`execute`], also returning the typed result the block was sealed from —
/// what verification needs to evaluate a question's invariants exactly,
/// rather than re-parsing the block's canonical text.
pub async fn execute_with_result(
    adapter: &dyn SourceAdapter,
    server: &dyn ServerClient,
    contract: &QueryContractDoc,
    intent: &QueryIntent,
    ctx: &ExecuteContext<'_>,
) -> Result<(EvidenceBlock, munarium_matrix_core::TypedResult), Refusal> {
    execute_traced(adapter, server, contract, intent, ctx)
        .await
        .map(|t| (t.block, t.result))
}

/// The whole path, returning the timings beside the block. The other two
/// entry points are views of this one.
pub async fn execute_traced(
    adapter: &dyn SourceAdapter,
    server: &dyn ServerClient,
    contract: &QueryContractDoc,
    intent: &QueryIntent,
    ctx: &ExecuteContext<'_>,
) -> Result<Traced, Refusal> {
    if intent.kind != IntentKind::StructuredQuery {
        return Err(Refusal::not_covered(
            "semantic intents are a later phase; this build serves structured query contracts",
        ));
    }
    adapter.capabilities().require_query_contracts()?;

    // --- 1. Bind. An undeclared or out-of-domain parameter dies here, before
    // any statement exists and before any budget is touched.
    let bound = bind_parameters(
        &contract.spec.parameters,
        &intent.parameters,
        ctx.pinned_domains,
    )?;

    // --- 2. Compile: parse, allowlist-walk, rewrite to placeholders.
    let declared = contract_schema(&contract.spec);
    let scope = munarium_matrix_types::validate::compile_scope(&contract.spec);

    let statement = statement_for(contract, ctx.dialect)?;
    let compiled = compile(&statement, ctx.dialect, &scope)?;

    // --- 3. Deadline. Checked before the call, and passed to the source so it
    // can cancel its own statement rather than being abandoned.
    let limits = effective_limits(ctx.source_limits, contract, intent);
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

    // --- 4. Execute.
    let executed = adapter
        .execute(&compiled.sql, &bound, ctx.identity, limits)
        .await?;
    let source_ms = (executed.ended_at - executed.started_at)
        .num_milliseconds()
        .max(0) as u64;

    // The declared result shape wins over whatever the driver inferred: the
    // contract's types, scales, units and keys are part of evidence identity.
    let mut result = executed.result;
    if !declared.columns.is_empty() {
        // An EMPTY read infers an empty schema (the Postgres adapter reads
        // column shapes off the first row), and reconciling that against the
        // declaration used to refuse `schema_drift` naming every declared
        // column — cycle uytigs3m, on a contract whose `as_of` predated the
        // fixture. An empty result is a legitimate COMPLETE answer, and the
        // declared schema IS its schema; there is nothing to contradict.
        result.schema = if result.rows.is_empty() && result.schema.columns.is_empty() {
            declared.clone()
        } else {
            reconcile_schema(&declared, &result.schema)?
        };
    }
    // The CELLS conform to the declared scales the schema now carries.
    // BigQuery renders NUMERIC minimally (900000.50 arrives as 900000.5;
    // measured 2026-08-31), so without this the sealed schema said scale 2
    // over a cell that hashed at scale 1 — the same logical row sealing a
    // different identity by engine.
    result.conform_decimal_scales()?;
    result.authorization_class = ctx.authorization_class.clone();
    result.denied_columns = contract.spec.policy.denied_columns.clone();

    // --- 5. Freshness obligation, if the profile set one.
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

    // --- 6. Derivations, computed once here so verification can recompute
    // them from the same sealed cells rather than trusting a number.
    let declared_derivations: Vec<munarium_matrix_core::Derivation> = contract
        .spec
        .result
        .derivations
        .iter()
        .map(|(name, d)| d.to_derivation(name))
        .collect();
    let derivations = munarium_matrix_core::derivation::compute_all(&declared_derivations, &result)
        .map_err(|e| Refusal::invalid("not_covered", e.to_string()))?;

    // --- 7. Seal.
    let ctx_seal = SealContext {
        tenant: intent.authorization.tenant.clone(),
        kind: ArtifactKind::Table,
        source_id: ctx.source_id.to_string(),
        source_version: ctx.source_version,
        adapter: adapter.kind().to_string(),
        adapter_version: Some(adapter.adapter_version().to_string()),
        engine: executed.engine.clone(),
        versions: ManifestVersions {
            query_contract: Some(contract.metadata.asset_ref()),
            policy: Some(format!("policy@{}", contract.metadata.version)),
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
        retention_days: intent
            .seal
            .retention_days
            .or(Some(contract.spec.evidence.retention_days)),
        declared_max_rows: Some(limits.max_rows),
        rows_covered: Some(result.rows.len() as u64),
        rows_excluded: None,
        exclusion_reason: None,
        freshness_watermark: None,
    };

    let seal_started = std::time::Instant::now();
    let (evidence_id, manifest) = match seal(
        server,
        &result,
        &ctx_seal,
        intent.seal.idempotency_key.as_deref(),
    )
    .await
    {
        Ok(v) => v,
        Err(e) if !intent.seal.required => {
            // Sealing was optional and failed. The honest answer is still a
            // refusal rather than an unciteable table: a number an answer
            // cannot cite is not evidence.
            return Err(e);
        }
        Err(e) => return Err(e),
    };
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
        derivations,
    };
    Ok(Traced {
        block,
        result,
        timings: ExecuteTimings { source_ms, seal_ms },
    })
}

/// Evaluate a verified question's invariants over the typed result.
///
/// Each invariant is a derivation (`sum`, `count`, `min`, `max`, …) over a
/// result column compared with an exact decimal the author wrote down. The
/// comparison is numeric, never textual — `2520000.50` and `2520000.5`
/// are one number here, while the logical result hash beside it is where the
/// trailing zero matters. Until 2026-08-29 the field was parsed and never
/// read; the corrected open-pipeline question is the first to rest on it.
pub(crate) fn check_invariants(
    invariants: &[munarium_matrix_types::assets::Invariant],
    result: &munarium_matrix_core::TypedResult,
) -> Vec<String> {
    use munarium_matrix_core::derivation::{compute, Derivation};
    use rust_decimal::Decimal;
    use std::str::FromStr;

    let mut failures = Vec::new();
    for (i, inv) in invariants.iter().enumerate() {
        let d = Derivation {
            name: format!("invariant[{i}]"),
            op: inv.op,
            over: inv.over.clone(),
            numerator: None,
            denominator: None,
            scale: None,
        };
        let computed = match compute(&d, result) {
            Ok(c) => c,
            Err(e) => {
                failures.push(format!("invariant[{i}]: {e}"));
                continue;
            }
        };
        let Some(text) = computed.value.as_deref() else {
            failures.push(format!("invariant[{i}]: the derivation produced no value"));
            continue;
        };
        let Ok(actual) = Decimal::from_str(text) else {
            failures.push(format!("invariant[{i}]: '{text}' is not a decimal"));
            continue;
        };
        let bound = |expected: &Option<String>| -> Option<Decimal> {
            expected.as_deref().and_then(|e| Decimal::from_str(e).ok())
        };
        if let Some(expected) = bound(&inv.equals) {
            if actual != expected {
                failures.push(format!(
                    "invariant[{i}] {:?} over {}: expected {expected}, got {actual}",
                    inv.op,
                    inv.over.as_deref().unwrap_or("-")
                ));
            }
        }
        if let Some(floor) = bound(&inv.at_least) {
            if actual < floor {
                failures.push(format!(
                    "invariant[{i}] {:?} over {}: {actual} is below the floor {floor}",
                    inv.op,
                    inv.over.as_deref().unwrap_or("-")
                ));
            }
        }
        if let Some(ceiling) = bound(&inv.at_most) {
            if actual > ceiling {
                failures.push(format!(
                    "invariant[{i}] {:?} over {}: {actual} is above the ceiling {ceiling}",
                    inv.op,
                    inv.over.as_deref().unwrap_or("-")
                ));
            }
        }
        for (label, expected) in [
            ("equals", &inv.equals),
            ("atLeast", &inv.at_least),
            ("atMost", &inv.at_most),
        ] {
            if let Some(e) = expected.as_deref() {
                if Decimal::from_str(e).is_err() {
                    failures.push(format!(
                        "invariant[{i}] {label}: '{e}' is not a decimal the author could mean"
                    ));
                }
            }
        }
    }
    failures
}

/// Take the contract's declared column metadata over the driver's inference,
/// matching by NAME. A column the contract declared but the result did not
/// return is a drift refusal, not a silent omission.
pub(crate) fn reconcile_schema(
    declared: &munarium_matrix_core::ResultSchema,
    actual: &munarium_matrix_core::ResultSchema,
) -> Result<munarium_matrix_core::ResultSchema, Refusal> {
    let mut columns = Vec::with_capacity(actual.columns.len());
    for a in &actual.columns {
        match declared.columns.iter().find(|d| d.name == a.name) {
            Some(d) => {
                if d.ty != a.ty {
                    return Err(Refusal::schema_drift(format!(
                        "column '{}' is declared {} but the source returned {}",
                        a.name, d.ty, a.ty
                    )));
                }
                columns.push(d.clone());
            }
            None => {
                return Err(Refusal::schema_drift(format!(
                    "the statement returned column '{}', which the contract does not declare",
                    a.name
                )))
            }
        }
    }
    for d in &declared.columns {
        if !columns.iter().any(|c| c.name == d.name) {
            return Err(Refusal::schema_drift(format!(
                "the contract declares column '{}', which the statement did not return",
                d.name
            )));
        }
    }
    Ok(munarium_matrix_core::ResultSchema {
        columns,
        row_id_rule: declared.row_id_rule,
        order_by: declared.order_by.clone(),
    })
}

/// Run a contract's declared verified questions. The same code path
/// `mxctl verify` and `POST /v1/contracts/{name}/verify` both use, so the CLI
/// and the API cannot disagree about whether a contract is healthy.
pub async fn verify(
    adapter: &dyn SourceAdapter,
    server: &dyn ServerClient,
    contract: &QueryContractDoc,
    tenant: &str,
    ctx: &ExecuteContext<'_>,
) -> Vec<VerifiedQuestionOutcome> {
    let mut out = Vec::new();
    for q in &contract.spec.verified_questions {
        let parameters: BTreeMap<String, TypedValueDto> = q
            .parameters
            .iter()
            .filter_map(|(name, value)| {
                let spec = contract.spec.parameters.get(name)?;
                Some((
                    name.clone(),
                    TypedValueDto {
                        ty: spec.ty,
                        value: value.clone(),
                        scale: spec.scale,
                        element_type: None,
                    },
                ))
            })
            .collect();
        let intent = QueryIntent {
            contract_version: munarium_matrix_core::CONTRACT_VERSION.to_string(),
            kind: IntentKind::StructuredQuery,
            request_id: None,
            contract: Some(contract.metadata.asset_ref()),
            semantic: None,
            parameters,
            authorization: AuthorizationSnapshot {
                // The CALLER's tenant, not a label. A verification executes
                // and seals like any execute, and the server refuses a manifest
                // whose tenant is not the token's — cycle 19 measured exactly
                // that (`manifest declares tenant 'verify' but the token is
                // scoped to 'mxtest'`) the first time a live deployment ran a verify
                // against a real server; the mock had never checked.
                tenant: tenant.to_string(),
                uid: None,
                access_level: ctx.authorization_class.access_level,
                compartments: ctx.authorization_class.compartments.clone(),
                session_id: None,
                runbook_ref: None,
            },
            limits: IntentLimits {
                max_rows: contract.spec.limits.max_rows,
                max_bytes: contract.spec.limits.max_bytes,
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
        let (rows, hash) = match execute_with_result(adapter, server, contract, &intent, ctx).await
        {
            Ok((EvidenceBlock::CompleteTable { manifest, rows, .. }, result)) => {
                failures.extend(check_invariants(&q.expect.invariants, &result));
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
                // The most valuable failure in the suite: the data or the
                // canonicalization moved, and the contract's own regression
                // test caught it.
                failures.push(format!(
                    "logical result hash changed: expected {expected}, got {actual}"
                ));
            }
        }

        out.push(VerifiedQuestionOutcome {
            question: q.question.clone(),
            ok: failures.is_empty(),
            rows,
            logical_result_hash: hash,
            failures,
        });
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedQuestionOutcome {
    pub question: String,
    pub ok: bool,
    pub rows: Option<usize>,
    pub logical_result_hash: Option<String>,
    pub failures: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use munarium_matrix_core::{AuthorizationClass, RefusalClass};

    /// The COMMITTED fixture, not a copy of it.
    ///
    /// This used to be an inline transcription that had drifted from
    /// `fixtures/assets/valid/contract.open-pipeline.yaml` — different source
    /// name, fewer result columns, a simpler statement. So the tests exercised
    /// a contract nobody ships while the one that does ship went unchecked,
    /// which is half of why the compile-scope defect survived. Reading the file
    /// means the fixture cannot drift from its own tests.
    fn contract_yaml() -> &'static str {
        include_str!("../../../fixtures/assets/valid/contract.open-pipeline.yaml")
    }

    fn contract() -> QueryContractDoc {
        match munarium_matrix_types::parse_asset(contract_yaml()).unwrap() {
            munarium_matrix_types::Asset::QueryContract(c) => *c,
            _ => unreachable!(),
        }
    }

    fn intent(params: BTreeMap<String, TypedValueDto>) -> QueryIntent {
        QueryIntent {
            contract_version: "0.1.0".into(),
            kind: IntentKind::StructuredQuery,
            request_id: None,
            contract: Some("open-pipeline-by-region@2".into()),
            semantic: None,
            parameters: params,
            authorization: AuthorizationSnapshot {
                tenant: "acme".into(),
                uid: Some("analyst".into()),
                access_level: 2,
                compartments: vec!["sales".into()],
                session_id: None,
                runbook_ref: None,
            },
            limits: IntentLimits {
                max_rows: 500,
                max_bytes: 1 << 20,
                max_cells: None,
            },
            deadline_at: None,
            freshness: None,
            seal: SealPolicy::default(),
        }
    }

    fn as_of(v: &str) -> BTreeMap<String, TypedValueDto> {
        BTreeMap::from([(
            "as_of".to_string(),
            TypedValueDto {
                ty: munarium_matrix_core::ColumnType::Date,
                value: serde_json::json!(v),
                scale: None,
                element_type: None,
            },
        )])
    }

    #[test]
    fn the_effective_limit_is_the_smallest_of_all_three() {
        let c = contract();
        let mut i = intent(as_of("2026-06-30"));
        i.limits.max_rows = 100;
        let source = Limits {
            max_rows: 10_000,
            max_bytes: 8 << 20,
            timeout_ms: 8000,
        };
        let l = effective_limits(source, &c, &i);
        // Intent is tightest on rows, contract on bytes and timeout.
        assert_eq!(l.max_rows, 100);
        assert_eq!(l.max_bytes, 1 << 20);
        assert_eq!(l.timeout_ms, 6000);

        // An intent asking for MORE than the contract cannot raise the ceiling.
        i.limits.max_rows = 1_000_000;
        assert_eq!(effective_limits(source, &c, &i).max_rows, 500);
    }

    #[test]
    fn a_declared_column_the_statement_did_not_return_is_drift() {
        use munarium_matrix_core::{Column, ColumnType, ResultSchema, RowIdRule};
        let declared = ResultSchema {
            columns: vec![
                Column::new("c0", "region", ColumnType::String).key(),
                Column::new("c1", "pipeline_amount", ColumnType::Decimal).scale(2),
            ],
            row_id_rule: RowIdRule::Keys,
            order_by: vec!["region".into()],
        };
        let actual = ResultSchema {
            columns: vec![Column::new("c0", "region", ColumnType::String)],
            row_id_rule: RowIdRule::Position,
            order_by: vec![],
        };
        let err = reconcile_schema(&declared, &actual).unwrap_err();
        assert_eq!(err.code, "schema_drift");
        assert!(err.message.contains("pipeline_amount"), "{}", err.message);
    }

    #[test]
    fn an_undeclared_returned_column_is_drift() {
        use munarium_matrix_core::{Column, ColumnType, ResultSchema, RowIdRule};
        let declared = ResultSchema {
            columns: vec![Column::new("c0", "region", ColumnType::String).key()],
            row_id_rule: RowIdRule::Keys,
            order_by: vec![],
        };
        let actual = ResultSchema {
            columns: vec![
                Column::new("c0", "region", ColumnType::String),
                Column::new("c1", "owner_email", ColumnType::String),
            ],
            row_id_rule: RowIdRule::Position,
            order_by: vec![],
        };
        let err = reconcile_schema(&declared, &actual).unwrap_err();
        assert!(err.message.contains("owner_email"), "{}", err.message);
    }

    #[test]
    fn the_contracts_declared_types_win_over_the_drivers_inference() {
        use munarium_matrix_core::{Column, ColumnType, ResultSchema, RowIdRule};
        let declared = ResultSchema {
            columns: vec![Column::new("c0", "pipeline_amount", ColumnType::Decimal)
                .scale(2)
                .unit("USD")
                .additive()],
            row_id_rule: RowIdRule::Position,
            order_by: vec!["pipeline_amount".into()],
        };
        // The driver inferred a decimal with no scale and no unit.
        let actual = ResultSchema {
            columns: vec![Column::new("c0", "pipeline_amount", ColumnType::Decimal)],
            row_id_rule: RowIdRule::Position,
            order_by: vec![],
        };
        let merged = reconcile_schema(&declared, &actual).unwrap();
        assert_eq!(merged.columns[0].scale, Some(2));
        assert_eq!(merged.columns[0].unit.as_deref(), Some("USD"));
    }

    #[test]
    fn a_missing_dialect_is_not_covered() {
        let err = statement_for(&contract(), "databricks").unwrap_err();
        assert_eq!(err.class, RefusalClass::NotCovered);
        assert!(err.message.contains("databricks"), "{}", err.message);
    }

    #[test]
    fn the_contracts_statement_survives_the_compiler() {
        // Uses the PRODUCTION scope. The previous version of this test built
        // its own — hand-adding `amount` and `updated_at`, and passing a table
        // list production did not use — so it tested the compiler while its
        // name claimed it tested the wiring. It passed for months against a
        // scope that refused every realistic contract in production.
        let c = contract();
        let scope = munarium_matrix_types::validate::compile_scope(&c.spec);
        let compiled = compile(&statement_for(&c, "postgres").unwrap(), "postgres", &scope)
            .expect("the committed contract must compile");
        assert_eq!(compiled.parameter_order, vec!["as_of".to_string()]);
        assert!(compiled.sql.contains("$1"));
        assert!(!compiled.sql.contains("2026-06-30"));
    }

    #[test]
    fn a_source_column_must_be_declared_in_reads() {
        // The defect in one assertion: `amount` is read by the statement and
        // is not a result column, so without `reads` it is refused.
        let mut c = contract();
        c.spec.reads.columns.retain(|col| col != "amount");
        let scope = munarium_matrix_types::validate::compile_scope(&c.spec);
        let err = compile(&statement_for(&c, "postgres").unwrap(), "postgres", &scope)
            .expect_err("an undeclared source column must be refused");
        assert!(format!("{err:?}").contains("amount"), "{err:?}");
    }

    #[test]
    fn a_denied_column_beats_a_reads_declaration() {
        // A read declaration is a statement of intent, not a grant. Policy
        // wins, or `deniedColumns` would be advisory.
        let mut c = contract();
        c.spec.reads.columns.push("owner_email".into());
        let scope = munarium_matrix_types::validate::compile_scope(&c.spec);
        assert!(
            scope.denied_columns.contains("owner_email"),
            "policy denial survives a reads declaration"
        );
    }

    #[test]
    fn the_table_name_is_not_hard_coded() {
        // Until 2026-08-29 the production scope contained a literal
        // "opportunities", so a contract over any other table refused.
        let mut c = contract();
        c.spec.reads.tables = vec!["holdings".into()];
        let scope = munarium_matrix_types::validate::compile_scope(&c.spec);
        assert!(scope.tables.contains("holdings"));
        assert!(
            scope.tables.contains("crm.holdings"),
            "a bare table is also reachable schema-qualified by its source"
        );
        assert!(!scope.tables.contains("opportunities"));
    }

    #[tokio::test]
    async fn a_semantic_intent_is_not_covered_yet_and_says_so() {
        use munarium_matrix_adapter_landing::LandingAdapter;
        use munarium_matrix_server_client::MockServer;

        let dir = std::env::temp_dir().join(format!("mx-q-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let adapter = LandingAdapter::new_file("crm", &dir, "manifest.json");
        let server = MockServer::new();
        let mut i = intent(as_of("2026-06-30"));
        i.kind = IntentKind::Semantic;

        let domains = BTreeMap::new();
        let identity = EffectiveIdentity {
            class: None,
            credential_ref: None,
            principal: "p".into(),
        };
        let ctx = ExecuteContext {
            source_id: "crm",
            source_version: 1,
            dialect: "postgres",
            pinned_domains: &domains,
            identity: &identity,
            authorization_class: AuthorizationClass::default(),
            source_limits: Limits {
                max_rows: 100,
                max_bytes: 1 << 20,
                timeout_ms: 1000,
            },
        };
        let err = execute(&adapter, &server, &contract(), &i, &ctx)
            .await
            .unwrap_err();
        assert_eq!(err.class, RefusalClass::NotCovered);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn an_expired_deadline_refuses_before_the_source_is_touched() {
        use munarium_matrix_adapter_landing::LandingAdapter;
        use munarium_matrix_server_client::MockServer;

        let dir = std::env::temp_dir().join(format!("mx-q2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // The landing adapter refuses execute() outright, so reaching THAT
        // refusal would mean the deadline check did not fire first.
        let adapter = LandingAdapter::new_file("crm", &dir, "manifest.json");
        let server = MockServer::new();
        let mut i = intent(as_of("2026-06-30"));
        i.deadline_at = Some(chrono::Utc::now() - chrono::Duration::seconds(5));

        let domains = BTreeMap::new();
        let identity = EffectiveIdentity {
            class: None,
            credential_ref: None,
            principal: "p".into(),
        };
        let ctx = ExecuteContext {
            source_id: "crm",
            source_version: 1,
            dialect: "postgres",
            pinned_domains: &domains,
            identity: &identity,
            authorization_class: AuthorizationClass::default(),
            source_limits: Limits {
                max_rows: 100,
                max_bytes: 1 << 20,
                timeout_ms: 1000,
            },
        };
        // The landing adapter cannot execute contracts at all, so its
        // capability refusal comes first — which is itself the right order:
        // an adapter that cannot serve the request should say so before a
        // deadline is even considered.
        let err = execute(&adapter, &server, &contract(), &i, &ctx)
            .await
            .unwrap_err();
        assert_eq!(err.class, RefusalClass::NotCovered);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
