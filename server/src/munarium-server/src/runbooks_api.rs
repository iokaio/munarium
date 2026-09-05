// SPDX-License-Identifier: Apache-2.0
//! The runbook executor: a checkpointed step machine over the pg
//! tables, pausing at approval gates, resuming from persisted state, and —
//! when the run names a version — recording every transition as a ledger
//! event. Deliberately NOT a workflow engine (architecture.md §15.3).

use crate::error::ApiError;
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use munarium_api_types as dto;
use munarium_core::{KernelError, Result};
use munarium_runbooks::{parse_runbook, RunbookDoc, StepSpec, StepState};
use std::sync::Arc;

/// The pg pool or a uniform "this endpoint requires postgres" error — shared
/// by every pg-only feature route (runbooks, sessions, ingest, reports) so
/// they present one error contract.
pub(crate) fn pool(state: &AppState) -> Result<&sqlx::PgPool> {
    state.pg_pool().ok_or_else(|| {
        KernelError::InvalidInput(
            "this endpoint requires the postgres store (MUNARIUM_STORE=postgres)".into(),
        )
    })
}

/// Load by name (latest version) or exact name@version, with lifecycle
/// status. Removed runbooks resolve only when `include_removed`.
pub(crate) async fn load_runbook_with_status(
    state: &AppState,
    tenant: &str,
    name_or_ref: &str,
    include_removed: bool,
) -> Result<(RunbookDoc, String)> {
    let pool = pool(state)?;
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT yaml, status FROM runbooks
          WHERE tenant_id = $1 AND (runbook_ref = $2 OR split_part(runbook_ref, '@', 1) = $2)
            AND ($3 OR status != 'removed')
          -- numeric version ordering: '@10' must beat '@9'
          ORDER BY COALESCE(NULLIF(split_part(runbook_ref, '@', 2), '')::int, 0) DESC
          LIMIT 1",
    )
    .bind(tenant)
    .bind(name_or_ref)
    .bind(include_removed)
    .fetch_optional(pool)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    let (yaml, status) = row.ok_or_else(|| KernelError::NotFound {
        kind: "runbook",
        id: name_or_ref.to_string(),
    })?;
    let doc = parse_runbook(&yaml).map_err(KernelError::InvalidInput)?;
    Ok((doc, status))
}

async fn load_runbook(state: &AppState, tenant: &str, name_or_ref: &str) -> Result<RunbookDoc> {
    Ok(load_runbook_with_status(state, tenant, name_or_ref, false)
        .await?
        .0)
}

async fn set_step(
    state: &AppState,
    tenant: &str,
    run_id: &str,
    ordinal: usize,
    name: &str,
    step_state: StepState,
    detail: Option<serde_json::Value>,
    version_id: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "UPDATE runbook_steps SET state = $4, detail = COALESCE($5, detail), updated_at = now()
          WHERE tenant_id = $1 AND run_id = $2 AND ordinal = $3",
    )
    .bind(tenant)
    .bind(run_id)
    .bind(ordinal as i32)
    .bind(step_state.as_str())
    .bind(&detail)
    .execute(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    state.metrics.inc(
        "munarium_runbook_step_transitions_total",
        crate::metrics::labels(&[("state", step_state.as_str())]),
    );

    // every transition is a ledger event when the run names a lineage
    if let Some(version_id) = version_id {
        let store = state.store_for(tenant).await?;
        let mut claim = munarium_core::storage::NewClaim::fact(
            &format!("runbook-run-{}", &run_id[..12.min(run_id.len())]),
            &format!("step-{ordinal}-{name}-{}", step_state.as_str()),
            step_state.as_str(),
        );
        claim.evidence = detail;
        let _ = store.append_claim(version_id, claim, None).await?;
    }
    Ok(())
}

async fn set_run_state(
    state: &AppState,
    tenant: &str,
    run_id: &str,
    run_state: &str,
) -> Result<()> {
    sqlx::query("UPDATE runbook_runs SET state = $3 WHERE tenant_id = $1 AND id = $2")
        .bind(tenant)
        .bind(run_id)
        .bind(run_state)
        .execute(pool(state)?)
        .await
        .map_err(|e| KernelError::Storage(e.to_string()))?;
    Ok(())
}

async fn step_detail(
    state: &AppState,
    tenant: &str,
    run_id: &str,
    name: &str,
) -> Result<Option<serde_json::Value>> {
    let row: Option<(Option<serde_json::Value>,)> = sqlx::query_as(
        "SELECT detail FROM runbook_steps
          WHERE tenant_id = $1 AND run_id = $2 AND name = $3 ORDER BY ordinal LIMIT 1",
    )
    .bind(tenant)
    .bind(run_id)
    .bind(name)
    .fetch_optional(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    Ok(row.and_then(|(d,)| d))
}

/// The flattened execution plan: v1 = one unit per step (the pipeline,
/// unchanged); v2 = one unit per (step × collection), named
/// `"<step>:<collection>"` so runbook_steps shows per-collection progress
/// and cutover approval gates once per collection.
///
/// The ORDER of those units is the runbook's `execution.order`, and it
/// decides how much work one HTTP request has to carry. Only `cutover` can
/// pause a run, so under the default `stepMajor` every collection's
/// buildIndex happens in the FIRST request, before any gate. That is fine
/// for a data room (13 collections built in 3.9 s) and impossible for a
/// 530 MB archive. `collectionMajor` walks each collection through all its
/// steps in turn, so the first request stops at collection 1's cutover and
/// every later request builds exactly one collection.
struct ExecUnit {
    step: StepSpec,
    /// None on the v1 legacy path.
    collection: Option<munarium_runbooks::CollectionSpec>,
    name: String,
}

fn exec_units(doc: &RunbookDoc) -> Vec<ExecUnit> {
    if doc.spec.is_v2() {
        let unit = |step: &StepSpec, col: &munarium_runbooks::CollectionSpec| ExecUnit {
            step: step.clone(),
            collection: Some(col.clone()),
            name: format!("{}:{}", step.name(), col.name),
        };
        // Data views belong to the RUNBOOK, not to a collection, so
        // this step is planned once. Fanning it out would verify the same
        // contracts once per collection and report N identical steps.
        let runbook_scoped = |step: &StepSpec| ExecUnit {
            step: step.clone(),
            collection: None,
            name: step.name().to_string(),
        };
        let expand = |step: &StepSpec| -> Vec<ExecUnit> {
            match step {
                StepSpec::VerifyDataViews {} => vec![runbook_scoped(step)],
                _ => doc
                    .spec
                    .collections
                    .iter()
                    .map(|col| unit(step, col))
                    .collect(),
            }
        };
        match doc.spec.execution_order() {
            munarium_runbooks::ExecutionOrder::StepMajor => {
                doc.spec.steps.iter().flat_map(expand).collect()
            }
            munarium_runbooks::ExecutionOrder::CollectionMajor => doc
                .spec
                .collections
                .iter()
                .flat_map(|col| {
                    doc.spec
                        .steps
                        .iter()
                        .filter(|s| !matches!(s, StepSpec::VerifyDataViews {}))
                        .map(move |step| unit(step, col))
                })
                .chain(
                    doc.spec
                        .steps
                        .iter()
                        .filter(|s| matches!(s, StepSpec::VerifyDataViews {}))
                        .map(runbook_scoped),
                )
                .collect(),
        }
    } else {
        doc.spec
            .steps
            .iter()
            .map(|step| ExecUnit {
                step: step.clone(),
                collection: None,
                name: step.name().to_string(),
            })
            .collect()
    }
}

/// Check every declared data view against Munarium Matrix.
///
/// Read-only, and deliberately a STEP rather than an apply-time check: it
/// needs Matrix reachable, and applying a runbook must not depend on a second
/// service being up.
///
/// **A runbook that declares data views with no Matrix configured FAILS here.**
/// The tempting alternative — skip, report a pass — is the vacuously-green
/// trap the Postgres conformance tier already fell into once: a verification
/// step that passes when it verified nothing is worse than no step at all,
/// because it is now evidence.
async fn verify_data_views(state: &AppState, doc: &RunbookDoc) -> Result<serde_json::Value> {
    let views = &doc.spec.data_views;
    if views.is_empty() {
        // Nothing declared is honestly nothing to verify.
        return Ok(serde_json::json!({ "data_views": 0, "verified": 0 }));
    }
    let Some(base) = state.config.matrix_base_url.clone() else {
        return Err(KernelError::InvalidInput(format!(
            "this runbook declares {} data view(s) but MUNARIUM_MATRIX_BASE_URL is not set, \
             so they cannot be verified; set it or remove spec.dataViews",
            views.len()
        )));
    };
    let token = std::env::var("MUNARIUM_MATRIX_TOKEN").ok();
    verify_data_views_against(&base, token.as_deref(), views).await
}

/// The step's body, with its two inputs — where Matrix is and what bearer to
/// send — as parameters rather than reads of process state, so a test can
/// point it at a listener it owns (2026-08-30). Dev-guide §13.5 entry 23
/// recorded that neither of the step's defects had a unit test "because the
/// crate has no HTTP mock"; the crate has `axum` and a loopback interface,
/// which is the only mock a request header needs.
pub(crate) async fn verify_data_views_against(
    base: &str,
    token: Option<&str>,
    views: &[munarium_runbooks::DataViewSpec],
) -> Result<serde_json::Value> {
    let http = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| KernelError::Provider(format!("http client: {e}")))?;

    let mut verified = Vec::new();
    let mut failed = Vec::new();
    for v in views {
        let url = format!(
            "{}/v1/{}/{}/verify",
            base.trim_end_matches('/'),
            v.kind.route(),
            v.contract
        );
        // The same credentials the execute path sends
        // (`evidence_providers::MatrixProvider`): the bearer from
        // MUNARIUM_MATRIX_TOKEN and a uid. Until 2026-08-29 this request
        // carried neither, and the first runbook with data views to run against
        // a real Matrix — one whose registry is not anonymous — failed its
        // `verifyDataViews` step with two 401s. The mock had accepted anything.
        let mut rb = http
            .post(&url)
            .timeout(std::time::Duration::from_secs(30))
            .header("X-Munarium-Uid", "munarium-server");
        if let Some(t) = token {
            rb = rb.bearer_auth(t);
        }
        let outcome = rb.send().await;
        match outcome {
            // A 200 is not a pass. Matrix's verify answers 200 with per-question
            // outcomes so a caller can see WHICH question moved, and the first
            // real run of this step (2026-08-29, dev) reported a data view
            // verified while its one question had failed — the body said
            // `failed: 1` and nobody read it. The body is the verdict.
            Ok(r) if r.status().is_success() => {
                let body: serde_json::Value = r.json().await.unwrap_or_default();
                let failed_questions = body["failed"].as_u64().unwrap_or(0);
                if failed_questions == 0 {
                    verified.push(v.name.clone());
                } else {
                    let questions: Vec<serde_json::Value> = body["questions"]
                        .as_array()
                        .map(|qs| {
                            qs.iter()
                                .filter(|q| q["ok"] == false)
                                .map(|q| {
                                    serde_json::json!({
                                        "question": q["question"],
                                        "failures": q["failures"],
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    failed.push(serde_json::json!({
                        "dataView": v.name,
                        "contract": v.contract,
                        "status": 200,
                        "failedQuestions": failed_questions,
                        "questions": questions,
                    }));
                }
            }
            Ok(r) => failed.push(serde_json::json!({
                "dataView": v.name,
                "contract": v.contract,
                "status": r.status().as_u16(),
            })),
            Err(e) => failed.push(serde_json::json!({
                "dataView": v.name,
                "contract": v.contract,
                "error": e.to_string(),
            })),
        }
    }
    if !failed.is_empty() {
        return Err(KernelError::InvalidInput(format!(
            "{} of {} data view(s) failed verification: {}",
            failed.len(),
            views.len(),
            serde_json::Value::Array(failed)
        )));
    }
    Ok(serde_json::json!({
        "data_views": views.len(),
        "verified": verified.len(),
        "views": verified,
    }))
}

/// Ensure the collection exists and its declarative source binding is
/// synced. Returns (info, bound_count, missing_explicit_hashes).
async fn sync_collection(
    state: &AppState,
    tenant: &str,
    retrieval: &munarium_retrieval::Retrieval,
    col: &munarium_runbooks::CollectionSpec,
) -> Result<(munarium_core::retrieval::CollectionInfo, i64, Vec<String>)> {
    // Same invariant as POST /v1/collections: a collection cannot bind to an
    // unpublished shape (otherwise buildIndex silently falls back to default
    // chunking and the operator debugs retrieval quality instead of a clear
    // "shape not published" at apply time).
    state.ensure_shapes_loaded(tenant).await?;
    if state.shapes.get(tenant, &col.shape).is_none() {
        return Err(KernelError::NotFound {
            kind: "shape",
            id: col.shape.clone(),
        });
    }
    let info = retrieval
        .ensure_collection(
            &col.name,
            &col.shape,
            col.access_level,
            &col.compartments,
            None,
        )
        .await?;
    let mut missing = Vec::new();
    if let Some(binding) = &col.sources {
        // Matcher-selected sources (prefix AND media-type when both present).
        if binding.filename_prefix.is_some() || !binding.media_types.is_empty() {
            // starts_with, NOT LIKE: a prefix is literal, so '_' and '%' in
            // a filenamePrefix must not act as wildcards (this must match the
            // ingest-plane matcher's `filename.starts_with(prefix)`).
            let ids: Vec<(String,)> = sqlx::query_as(
                "SELECT source_id FROM sources
                  WHERE tenant_id = $1
                    AND ($2::text IS NULL OR starts_with(filename, $2))
                    AND (cardinality($3::text[]) = 0 OR media_type = ANY($3))",
            )
            .bind(tenant)
            .bind(&binding.filename_prefix)
            .bind(&binding.media_types)
            .fetch_all(pool(state)?)
            .await
            .map_err(|e| KernelError::Storage(e.to_string()))?;
            // One statement for the whole match set: this runs for every
            // collection on every apply, and a per-source round trip was
            // ~137k sequential queries per `POST /v1/runbooks` on a deployment
            // with 68k sources.
            let ids: Vec<String> = ids.into_iter().map(|(id,)| id).collect();
            retrieval.bind_sources(&info.id, &ids, None).await?;
        }
        // Explicitly listed content hashes resolve to source_ids first (two
        // paths holding identical bytes are two sources and both bind).
        // Absent hashes are reported, not fatal — the step detail carries
        // them for the operator.
        for hash in &binding.content_hashes {
            let ids: Vec<(String,)> = sqlx::query_as(
                "SELECT source_id FROM sources WHERE tenant_id = $1 AND content_hash = $2",
            )
            .bind(tenant)
            .bind(hash)
            .fetch_all(pool(state)?)
            .await
            .map_err(|e| KernelError::Storage(e.to_string()))?;
            if ids.is_empty() {
                missing.push(hash.clone());
                continue;
            }
            let ids: Vec<String> = ids.into_iter().map(|(id,)| id).collect();
            retrieval.bind_sources(&info.id, &ids, None).await?;
        }
    }
    let count = retrieval.collection_source_count(&info.id).await?;
    Ok((info, count, missing))
}

/// Walk the flattened plan from the first non-done unit. Stops at approval
/// gates (run -> awaiting_approval) and on failure. `approved_ordinal` marks
/// one gate as human-approved this pass.
async fn execute(
    state: &AppState,
    tenant: &str,
    run_id: &str,
    doc: &RunbookDoc,
    version_id: Option<&str>,
    approved_ordinal: Option<usize>,
) -> Result<String> {
    let retrieval = state.retrieval_for(tenant)?;
    state.ensure_shapes_loaded(tenant).await?;
    let max_chars_for = |shape_ref: &str| {
        state
            .shapes
            .get(tenant, shape_ref)
            .and_then(|s| s.doc.spec.chunking.as_ref().map(|c| c.max_chars))
            .unwrap_or(2000)
    };

    // current step states
    let rows: Vec<(i32, String)> = sqlx::query_as(
        "SELECT ordinal, state FROM runbook_steps
          WHERE tenant_id = $1 AND run_id = $2 ORDER BY ordinal",
    )
    .bind(tenant)
    .bind(run_id)
    .fetch_all(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;

    for (ordinal, unit) in exec_units(doc).iter().enumerate() {
        let current = rows
            .iter()
            .find(|(o, _)| *o == ordinal as i32)
            .map(|(_, s)| s.as_str())
            .unwrap_or("pending");
        if current == "done" {
            continue;
        }
        let name = unit.name.as_str();

        // approval gate (v2: once per collection)
        if unit.step.requires_approval()
            && approved_ordinal != Some(ordinal)
            && current != "awaiting_approval"
        {
            set_step(
                state,
                tenant,
                run_id,
                ordinal,
                name,
                StepState::AwaitingApproval,
                None,
                version_id,
            )
            .await?;
            set_run_state(state, tenant, run_id, "awaiting_approval").await?;
            return Ok("awaiting_approval".into());
        }
        if current == "awaiting_approval" && approved_ordinal != Some(ordinal) {
            return Ok("awaiting_approval".into());
        }

        set_step(
            state,
            tenant,
            run_id,
            ordinal,
            name,
            StepState::Running,
            None,
            version_id,
        )
        .await?;
        let outcome: Result<serde_json::Value> = match (&unit.step, &unit.collection) {
            // ---- v2: per-collection over the partitioned store -------------
            (StepSpec::ResolveSources {}, Some(col)) => {
                match sync_collection(state, tenant, &retrieval, col).await {
                    Ok((info, count, missing)) => Ok(serde_json::json!({
                        "collection_id": info.id,
                        "sources": count,
                        "missing_declared_hashes": missing,
                    })),
                    Err(e) => Err(e),
                }
            }
            (StepSpec::BuildIndex {}, Some(col)) => {
                let watermark = match version_id {
                    Some(v) => state.store_for(tenant).await?.head(v).await?,
                    None => 0,
                };
                match retrieval.collection_by_name(&col.name).await {
                    Ok(info) => retrieval
                        .build_collection_index(&info.id, max_chars_for(&col.shape), watermark, false)
                        .await
                        .map(|iv| serde_json::json!({ "index_version": iv.id, "watermark": iv.event_watermark })),
                    Err(e) => Err(e),
                }
            }
            (StepSpec::Verify {}, Some(col)) => {
                let built = step_detail(state, tenant, run_id, &format!("buildIndex:{}", col.name))
                    .await?
                    .and_then(|d| d["index_version"].as_str().map(String::from))
                    .ok_or_else(|| {
                        KernelError::InvalidInput("verify needs a prior buildIndex".into())
                    })?;
                retrieval.verify_collection_index(&built).await
            }
            (StepSpec::Cutover { .. }, Some(col)) => {
                let built = step_detail(state, tenant, run_id, &format!("buildIndex:{}", col.name))
                    .await?
                    .and_then(|d| d["index_version"].as_str().map(String::from))
                    .ok_or_else(|| {
                        KernelError::InvalidInput("cutover needs a prior buildIndex".into())
                    })?;
                match retrieval.collection_by_name(&col.name).await {
                    Ok(info) => {
                        // Derived lexeme tables for the version going live
                        // (2026-08-30, §13.5 entry 25): number forms and the
                        // frequency statistics, at build time rather than on
                        // the first query that needs them. Best-effort — both
                        // populate lazily with a sentinel if this fails, and
                        // a cutover must not fail on a derived table.
                        if let Err(e) = retrieval.record_number_lexemes(&info.id, &built).await {
                            tracing::warn!(error = %e, collection = %col.name, "number-lexeme scan at cutover failed; it will populate lazily");
                        }
                        if let Err(e) = retrieval.record_lexeme_frequency(&info.id, &built).await {
                            tracing::warn!(error = %e, collection = %col.name, "lexeme-frequency scan at cutover failed; it will populate lazily");
                        }
                        retrieval
                            .activate_collection_index(&info.id, &built)
                            .await
                            .map(|_| serde_json::json!({ "activated": built }))
                    }
                    Err(e) => Err(e),
                }
            }
            (StepSpec::RetireOld { keep_versions }, Some(col)) => {
                match retrieval.collection_by_name(&col.name).await {
                    Ok(info) => retrieval
                        .retire_old_collection(&info.id, *keep_versions)
                        .await
                        .map(|n| serde_json::json!({ "retired_chunk_rows": n })),
                    Err(e) => Err(e),
                }
            }
            // ---- Runbook-scoped, on both v1 and v2 ------------------
            (StepSpec::VerifyDataViews {}, _) => verify_data_views(state, doc).await,
            // ---- v1: the legacy shape-scoped pipeline, byte-for-byte -------
            (step, None) => {
                let shape = doc.spec.shape.as_deref().unwrap_or_default();
                match step {
                    StepSpec::ResolveSources {} => retrieval
                        .source_count(shape)
                        .await
                        .map(|n| serde_json::json!({ "sources": n })),
                    StepSpec::BuildIndex {} => {
                        let watermark = match version_id {
                            Some(v) => state.store_for(tenant).await?.head(v).await?,
                            None => 0,
                        };
                        retrieval
                            .build_index(shape, max_chars_for(shape), watermark, false)
                            .await
                            .map(|iv| serde_json::json!({ "index_version": iv.id, "watermark": iv.event_watermark }))
                    }
                    StepSpec::Verify {} => {
                        let built = step_detail(state, tenant, run_id, "buildIndex")
                            .await?
                            .and_then(|d| d["index_version"].as_str().map(String::from))
                            .ok_or_else(|| {
                                KernelError::InvalidInput("verify needs a prior buildIndex".into())
                            })?;
                        retrieval.verify_index(&built).await
                    }
                    StepSpec::Cutover { .. } => {
                        let built = step_detail(state, tenant, run_id, "buildIndex")
                            .await?
                            .and_then(|d| d["index_version"].as_str().map(String::from))
                            .ok_or_else(|| {
                                KernelError::InvalidInput("cutover needs a prior buildIndex".into())
                            })?;
                        retrieval
                            .activate_index(shape, &built)
                            .await
                            .map(|_| serde_json::json!({ "activated": built }))
                    }
                    StepSpec::RetireOld { keep_versions } => retrieval
                        .retire_old(shape, *keep_versions)
                        .await
                        .map(|n| serde_json::json!({ "retired_chunk_rows": n })),
                    // Unreachable: the runbook-scoped arm above matches this
                    // step on both v1 and v2 before control reaches here.
                    StepSpec::VerifyDataViews {} => {
                        unreachable!("verifyDataViews is handled by the runbook-scoped arm")
                    }
                }
            }
        };

        match outcome {
            Ok(detail) => {
                set_step(
                    state,
                    tenant,
                    run_id,
                    ordinal,
                    name,
                    StepState::Done,
                    Some(detail),
                    version_id,
                )
                .await?;
            }
            Err(e) => {
                set_step(
                    state,
                    tenant,
                    run_id,
                    ordinal,
                    name,
                    StepState::Failed,
                    Some(serde_json::json!({ "error": e.to_string() })),
                    version_id,
                )
                .await?;
                set_run_state(state, tenant, run_id, "failed").await?;
                return Err(e);
            }
        }
    }
    set_run_state(state, tenant, run_id, "done").await?;
    Ok("done".into())
}

// ---------------------------------------------------------------------------
// operations shared by both planes
// ---------------------------------------------------------------------------

/// Apply a Shape and optionally record the publication as a ledger claim.
/// The one implementation behind both REST `/v1/shapes` and gRPC `ApplyShape`.
/// `store` lets a caller that already materialized the tenant store pass it
/// in; None fetches one only when `version_id` demands it.
pub async fn op_apply_shape(
    state: &AppState,
    tenant: &str,
    yaml: &str,
    version_id: Option<&str>,
    store: Option<Arc<dyn munarium_core::storage::StorageBackend>>,
) -> Result<dto::ApplyShapeResponse> {
    // Resolve the lineage BEFORE activating the shape, so a bad version_id
    // fails without leaving an applied-but-unwitnessed shape behind.
    let store_for_event = match version_id {
        Some(v) => {
            let store = match store {
                Some(s) => s,
                None => state.store_for(tenant).await?,
            };
            store.head(v).await?; // errors NotFound on an unknown lineage
            Some(store)
        }
        None => None,
    };
    state.ensure_shapes_loaded(tenant).await?;
    let shape = state
        .shapes
        .apply(tenant, yaml)
        .map_err(KernelError::InvalidInput)?;
    state
        .persist_shape(tenant, &shape.shape_ref(), yaml, &shape.yaml_hash)
        .await?;
    let mut event_id = None;
    if let (Some(version_id), Some(store)) = (version_id, store_for_event) {
        // publication is itself an event in the named lineage
        let mut claim = munarium_core::storage::NewClaim::fact(
            "munarium-shapes",
            &shape.doc.metadata.name,
            &format!("{}@{}", shape.doc.metadata.version, &shape.yaml_hash[..12]),
        );
        claim.provenance = munarium_core::types::Provenance::Witnessed;
        event_id = Some(store.append_claim(version_id, claim, None).await?.id);
    }
    Ok(dto::ApplyShapeResponse {
        shape_ref: shape.shape_ref(),
        yaml_hash: shape.yaml_hash.clone(),
        event_id,
    })
}

/// Apply = upsert the yaml (re-apply of the same name@version is the
/// documented in-place upgrade) + materialize a v2 spec's collections and
/// source bindings. A removed ref cannot be resurrected by re-apply.
pub async fn op_apply_runbook(state: &AppState, tenant: &str, yaml: &str) -> Result<String> {
    let doc = parse_runbook(yaml).map_err(KernelError::InvalidInput)?;
    let existing: Option<(String,)> =
        sqlx::query_as("SELECT status FROM runbooks WHERE tenant_id = $1 AND runbook_ref = $2")
            .bind(tenant)
            .bind(doc.runbook_ref())
            .fetch_optional(pool(state)?)
            .await
            .map_err(|e| KernelError::Storage(e.to_string()))?;
    if matches!(existing, Some((ref s,)) if s == "removed") {
        return Err(KernelError::InvalidInput(format!(
            "runbook '{}' was removed; publish a new version instead",
            doc.runbook_ref()
        )));
    }
    // Re-apply resets any in-flight removal request: the yaml changed, so a
    // removal_id armed against the OLD content must not be able to remove the
    // fresh version (status back to active, removal_id cleared).
    sqlx::query(
        "INSERT INTO runbooks (tenant_id, runbook_ref, yaml) VALUES ($1, $2, $3)
         ON CONFLICT (tenant_id, runbook_ref)
           DO UPDATE SET yaml = EXCLUDED.yaml, updated_at = now(),
                         status = 'active', removal_id = NULL,
                         removal_requested_at = NULL, removal_requested_by = NULL",
    )
    .bind(tenant)
    .bind(doc.runbook_ref())
    .bind(yaml)
    .execute(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    // v2: materialize collections + declarative source bindings now, so the
    // runbook is inspectable (list/info) before its first run.
    if doc.spec.is_v2() {
        let retrieval = state.retrieval_for(tenant)?;
        for col in &doc.spec.collections {
            sync_collection(state, tenant, &retrieval, col).await?;
        }
    }
    Ok(doc.runbook_ref())
}

/// Cross-instance execution lock for one run: a session advisory lock held
/// on a DETACHED connection, so the lock's lifetime is the TCP connection's
/// lifetime — a pooled connection can never be returned still holding it
/// (the classic sqlx advisory-lock footgun). Dropping the guard closes the
/// connection, which releases the lock. Crash story: the process dying
/// drops the connection, Postgres releases the lock, and `execute` resumes
/// from the first non-done step — recovery, not a hazard
/// (docs/ops/clustering.md has the diagnosis walk). Cost: one real
/// connection OUTSIDE the pool per in-flight run execution — bounded by
/// concurrent runs, which are operator-initiated.
struct RunLock {
    _conn: sqlx::postgres::PgConnection,
}

async fn acquire_run_lock(state: &AppState, tenant: &str, run_id: &str) -> Result<RunLock> {
    let mut conn = pool(state)?
        .acquire()
        .await
        .map_err(|e| KernelError::Storage(e.to_string()))?
        .detach();
    // Two-int form keyed on (tenant, run) — the same hashtext idiom as the
    // collection-DDL lock in munarium-retrieval-pg.
    let (locked,): (bool,) =
        sqlx::query_as("SELECT pg_try_advisory_lock(hashtext($1), hashtext($2))")
            .bind(tenant)
            .bind(run_id)
            .fetch_one(&mut conn)
            .await
            .map_err(|e| KernelError::Storage(e.to_string()))?;
    if !locked {
        return Err(KernelError::InvalidInput(format!(
            "{}run '{run_id}' is already executing on another instance; poll GET /v1/runs/{run_id} and retry when it settles",
            crate::error::RUN_LOCKED_PREFIX
        )));
    }
    Ok(RunLock { _conn: conn })
}

pub async fn op_run_runbook(
    state: &AppState,
    tenant: &str,
    name_or_ref: &str,
    version_id: Option<&str>,
) -> Result<(String, String)> {
    let doc = load_runbook(state, tenant, name_or_ref).await?;
    let run_id = format!("run-{}", uuid_suffix());
    // Run row + full step plan in ONE transaction: a mid-plan failure must
    // not leave a 'running' run with a truncated step list.
    let mut tx = pool(state)?
        .begin()
        .await
        .map_err(|e| KernelError::Storage(e.to_string()))?;
    sqlx::query(
        "INSERT INTO runbook_runs (tenant_id, id, runbook_ref, state, version_id)
         VALUES ($1, $2, $3, 'running', $4)",
    )
    .bind(tenant)
    .bind(&run_id)
    .bind(doc.runbook_ref())
    .bind(version_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    for (ordinal, unit) in exec_units(&doc).iter().enumerate() {
        sqlx::query(
            "INSERT INTO runbook_steps (tenant_id, run_id, ordinal, name, state)
             VALUES ($1, $2, $3, $4, 'pending')",
        )
        .bind(tenant)
        .bind(&run_id)
        .bind(ordinal as i32)
        .bind(&unit.name)
        .execute(&mut *tx)
        .await
        .map_err(|e| KernelError::Storage(e.to_string()))?;
    }
    tx.commit()
        .await
        .map_err(|e| KernelError::Storage(e.to_string()))?;
    let _lock = acquire_run_lock(state, tenant, &run_id).await?;
    let state_now = execute(state, tenant, &run_id, &doc, version_id, None).await?;
    Ok((run_id, state_now))
}

pub async fn op_get_run(
    state: &AppState,
    tenant: &str,
    run_id: &str,
) -> Result<dto::RunStatusResponse> {
    let run: Option<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT runbook_ref, state, version_id FROM runbook_runs
          WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant)
    .bind(run_id)
    .fetch_optional(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    let (runbook_ref, run_state, version_id) = run.ok_or_else(|| KernelError::NotFound {
        kind: "run",
        id: run_id.to_string(),
    })?;
    let steps: Vec<(i32, String, String, Option<serde_json::Value>)> = sqlx::query_as(
        "SELECT ordinal, name, state, detail FROM runbook_steps
          WHERE tenant_id = $1 AND run_id = $2 ORDER BY ordinal",
    )
    .bind(tenant)
    .bind(run_id)
    .fetch_all(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    Ok(dto::RunStatusResponse {
        run_id: run_id.to_string(),
        runbook_ref,
        state: run_state,
        version_id,
        steps: steps
            .into_iter()
            .map(|(o, n, s, d)| dto::RunbookStepDto {
                ordinal: o as u32,
                name: n,
                state: s,
                detail: d,
            })
            .collect(),
    })
}

pub async fn op_approve_step(
    state: &AppState,
    tenant: &str,
    run_id: &str,
    ordinal: usize,
) -> Result<String> {
    let run: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT runbook_ref, version_id FROM runbook_runs WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant)
    .bind(run_id)
    .fetch_optional(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    let (runbook_ref, version_id) = run.ok_or_else(|| KernelError::NotFound {
        kind: "run",
        id: run_id.to_string(),
    })?;
    let current: Option<(String,)> = sqlx::query_as(
        "SELECT state FROM runbook_steps WHERE tenant_id = $1 AND run_id = $2 AND ordinal = $3",
    )
    .bind(tenant)
    .bind(run_id)
    .bind(ordinal as i32)
    .fetch_optional(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    match current {
        Some((s,)) if s == "awaiting_approval" => {}
        Some((s,)) => {
            return Err(KernelError::InvalidInput(format!(
                "step {ordinal} is '{s}', not awaiting_approval"
            )))
        }
        None => {
            return Err(KernelError::NotFound {
                kind: "step",
                id: ordinal.to_string(),
            })
        }
    }
    let doc = load_runbook(state, tenant, &runbook_ref).await?;
    // The lock must be held BEFORE the state flip: two concurrent approvals
    // must resolve to exactly one executor (the loser 409s `run-locked`).
    let _lock = acquire_run_lock(state, tenant, run_id).await?;
    set_run_state(state, tenant, run_id, "running").await?;
    execute(
        state,
        tenant,
        run_id,
        &doc,
        version_id.as_deref(),
        Some(ordinal),
    )
    .await
}

// ---------------------------------------------------------------------------
// list / info / validate
// ---------------------------------------------------------------------------

/// Collection rows for a runbook: DB values are authoritative once applied;
/// unapplied (or v1-implicit) collections fall back to the spec.
async fn runbook_collection_dtos(
    state: &AppState,
    tenant: &str,
    doc: &RunbookDoc,
) -> Result<Vec<dto::RunbookCollectionDto>> {
    let retrieval = state.retrieval_for(tenant)?;
    let mut out = Vec::new();
    for spec in doc.spec.effective_collections() {
        match retrieval.collection_by_name(&spec.name).await {
            Ok(info) => {
                let source_count = retrieval.collection_source_count(&info.id).await?;
                let active_index = retrieval.active_collection_index(&info.id).await?;
                out.push(dto::RunbookCollectionDto {
                    name: info.name,
                    collection_id: Some(info.id),
                    shape_ref: info.shape_ref,
                    access_level: info.access_level,
                    compartments: info.compartments,
                    active_index,
                    source_count,
                });
            }
            Err(KernelError::NotFound { .. }) => out.push(dto::RunbookCollectionDto {
                name: spec.name.clone(),
                collection_id: None,
                shape_ref: spec.shape.clone(),
                access_level: spec.access_level,
                compartments: spec.compartments.clone(),
                active_index: None,
                source_count: 0,
            }),
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

pub async fn op_list_runbooks(
    state: &AppState,
    tenant: &str,
    include_removed: bool,
) -> Result<Vec<dto::RunbookSummaryDto>> {
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT runbook_ref, yaml, status, created_at::text FROM runbooks
          WHERE tenant_id = $1 AND ($2 OR status != 'removed')
          ORDER BY runbook_ref",
    )
    .bind(tenant)
    .bind(include_removed)
    .fetch_all(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    let mut out = Vec::new();
    for (runbook_ref, yaml, status, created_at) in rows {
        let Ok(doc) = parse_runbook(&yaml) else {
            // A stored runbook that no longer parses is surfaced, not hidden.
            out.push(dto::RunbookSummaryDto {
                runbook_ref: runbook_ref.clone(),
                name: runbook_ref.clone(),
                version: 0,
                status: format!("{status} (unparsable)"),
                min_access_level: 0,
                collections: Vec::new(),
                created_at,
            });
            continue;
        };
        let collections = runbook_collection_dtos(state, tenant, &doc).await?;
        let min_access_level = collections
            .iter()
            .map(|c| c.access_level)
            .min()
            .unwrap_or(0);
        out.push(dto::RunbookSummaryDto {
            runbook_ref,
            name: doc.metadata.name.clone(),
            version: doc.metadata.version,
            status,
            min_access_level,
            collections,
            created_at,
        });
    }
    Ok(out)
}

pub async fn op_runbook_info(
    state: &AppState,
    tenant: &str,
    name_or_ref: &str,
) -> Result<dto::RunbookInfoResponse> {
    let (doc, status) = load_runbook_with_status(state, tenant, name_or_ref, true).await?;
    let collections = runbook_collection_dtos(state, tenant, &doc).await?;
    let versions: Vec<(String,)> = sqlx::query_as(
        "SELECT runbook_ref FROM runbooks
          WHERE tenant_id = $1 AND runbook_ref LIKE $2 || '@%'
          ORDER BY runbook_ref",
    )
    .bind(tenant)
    .bind(&doc.metadata.name)
    .fetch_all(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    let created_at: (String,) = sqlx::query_as(
        "SELECT created_at::text FROM runbooks WHERE tenant_id = $1 AND runbook_ref = $2",
    )
    .bind(tenant)
    .bind(doc.runbook_ref())
    .fetch_one(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    let retrieval_spec = doc.spec.retrieval.clone().unwrap_or_default();
    Ok(dto::RunbookInfoResponse {
        runbook_ref: doc.runbook_ref(),
        name: doc.metadata.name.clone(),
        version: doc.metadata.version,
        status,
        collections,
        versions: versions.into_iter().map(|(r,)| r).collect(),
        models: doc
            .spec
            .models
            .as_ref()
            .map(|m| serde_json::to_value(m).unwrap_or_default()),
        retrieval: serde_json::json!({
            "top_k": retrieval_spec.top_k,
            "rrf_k": retrieval_spec.rrf_k,
            "candidate_n": retrieval_spec.candidate_n,
            "query_expansions": retrieval_spec.query_expansions,
            "query_expansion_weight": retrieval_spec.query_expansion_weight,
            "model_query_expansion": retrieval_spec.model_query_expansion,
            "collection_selection": retrieval_spec.collection_selection,
            "fusion": retrieval_spec.fusion,
            "collection_routes": retrieval_spec.collection_routes,
            "content_demotions": retrieval_spec.content_demotions,
        }),
        has_completion: doc.spec.completion.is_some(),
        created_at: created_at.0,
    })
}

fn finding_dto(f: &munarium_runbooks::validate::ValidationFinding) -> dto::ValidationFindingDto {
    dto::ValidationFindingDto {
        severity: match f.severity {
            munarium_runbooks::validate::Severity::Error => "error",
            munarium_runbooks::validate::Severity::Warn => "warn",
            munarium_runbooks::validate::Severity::Info => "info",
        }
        .to_string(),
        code: f.code.clone(),
        message: f.message.clone(),
        path: f.path.clone(),
    }
}

/// Deterministic validation always; `suggest` adds an AI advisory pass via
/// the runbook's `models.tasks.validation` default (BYOK), overridable when
/// the policy permits. Suggestion failures NEVER fail validation — they
/// degrade to a note.
pub async fn op_validate_runbook(
    state: &AppState,
    tenant: &str,
    yaml: &str,
    suggest: bool,
    override_req: Option<&crate::models::ModelOverride>,
) -> std::result::Result<dto::ValidateRunbookResponse, ApiError> {
    let doc = match parse_runbook(yaml) {
        Ok(doc) => doc,
        Err(e) => {
            return Ok(dto::ValidateRunbookResponse {
                valid: false,
                findings: vec![dto::ValidationFindingDto {
                    severity: "error".into(),
                    code: "parse".into(),
                    message: e,
                    path: "$".into(),
                }],
                suggestions: Vec::new(),
                suggest_note: None,
            })
        }
    };
    let findings = munarium_runbooks::validate::validate_runbook(&doc);
    let valid = munarium_runbooks::validate::is_valid(&findings);
    let findings: Vec<dto::ValidationFindingDto> = findings.iter().map(finding_dto).collect();

    let (suggestions, suggest_note) = if suggest {
        match ai_suggestions(state, tenant, yaml, &doc, &findings, override_req).await {
            Ok(s) => (s, None),
            Err(e) => (Vec::new(), Some(format!("suggestions unavailable: {e}"))),
        }
    } else {
        (Vec::new(), None)
    };

    Ok(dto::ValidateRunbookResponse {
        valid,
        findings,
        suggestions,
        suggest_note,
    })
}

/// The AI advisory pass: prompt the resolved validation model with the yaml
/// + deterministic findings; parse a strict-JSON suggestion array.
async fn ai_suggestions(
    state: &AppState,
    tenant: &str,
    yaml: &str,
    doc: &RunbookDoc,
    findings: &[dto::ValidationFindingDto],
    override_req: Option<&crate::models::ModelOverride>,
) -> std::result::Result<Vec<dto::SuggestionDto>, ApiError> {
    let resolved = crate::models::resolve_model(doc, "validation", override_req)?;
    let store = state.store_for(tenant).await?;
    let findings_json = serde_json::to_string(findings).unwrap_or_default();
    let prompt = format!(
        "You review munarium runbook definitions (declarative retrieval applications: \
         compartmentalized collections, index lifecycle steps, retrieval knobs, model \
         defaults, optional RAG completion).\n\nRunbook YAML:\n```yaml\n{yaml}\n```\n\n\
         Deterministic findings already reported (do not repeat them):\n{findings_json}\n\n\
         Suggest up to 5 improvements for retrieval quality, cost efficiency, and access \
         design. Respond with ONLY a JSON array: \
         [{{\"title\": \"...\", \"rationale\": \"...\", \"patch_hint\": \"...\"}}]. \
         An empty array is a valid answer."
    );
    let budgets = state.max_tokens.effective(state, tenant).await?;
    let resp = crate::providers_api::op_complete(
        state,
        tenant,
        store.as_ref(),
        &resolved.provider_name,
        dto::CompleteRequest {
            prompt: Some(prompt),
            system: None,
            model: resolved.model.clone(),
            tier: resolved.tier.clone(),
            provider: None,
            // `runbook_advisory` (`/v1/max-tokens`; built-in 2,048 since
            // 2026-09-02, 1,024 before).
            max_tokens: Some(budgets.runbook_advisory),
            temperature: None,
            version_id: None,
        },
    )
    .await?;
    let text = resp.text;
    // strict-first, then rescue a fenced/prefixed array
    let parsed: Vec<dto::SuggestionDto> = serde_json::from_str(&text)
        .or_else(|_| {
            let start = text.find('[');
            let end = text.rfind(']');
            match (start, end) {
                (Some(s), Some(e)) if e > s => serde_json::from_str(&text[s..=e]),
                _ => serde_json::from_str("[]"),
            }
        })
        .map_err(|e| KernelError::Provider(format!("suggestion parse: {e}")))?;
    Ok(parsed.into_iter().take(5).collect())
}

// ---------------------------------------------------------------------------
// double-pass soft removal. Nothing is ever deleted — a removed runbook
// is invisible (list/info/sessions) but its yaml, run history, collections,
// and index data all remain. Physical index deletion is the DBA's manual
// runbook: docs/ops/index-deletion-runbook.md.
// ---------------------------------------------------------------------------

const REMOVAL_TTL_SECS: i64 = 15 * 60;

/// Pass one: request removal of an EXACT name@version. Returns the
/// removal_id the confirm pass must present within 15 minutes.
pub async fn op_request_removal(
    state: &AppState,
    tenant: &str,
    runbook_ref: &str,
    requested_by: &str,
) -> std::result::Result<dto::RemovalRequestResponse, ApiError> {
    if !runbook_ref.contains('@') {
        return Err(ApiError::Mesh(KernelError::InvalidInput(
            "removal targets an exact name@version (a bare name would remove the latest silently)"
                .into(),
        )));
    }
    let row: Option<(String,)> =
        sqlx::query_as("SELECT status FROM runbooks WHERE tenant_id = $1 AND runbook_ref = $2")
            .bind(tenant)
            .bind(runbook_ref)
            .fetch_optional(pool(state)?)
            .await
            .map_err(|e| KernelError::Storage(e.to_string()))?;
    match row {
        None => {
            return Err(ApiError::Mesh(KernelError::NotFound {
                kind: "runbook",
                id: runbook_ref.to_string(),
            }))
        }
        Some((s,)) if s == "removed" => {
            return Err(ApiError::Custom(
                crate::error::CustomError::runbook_removed(runbook_ref),
            ))
        }
        Some(_) => {}
    }
    let removal_id = format!("rm-{}", uuid::Uuid::now_v7().simple());
    sqlx::query(
        "UPDATE runbooks
            SET status = 'remove_requested', removal_id = $3,
                removal_requested_at = now(), removal_requested_by = $4,
                updated_at = now()
          WHERE tenant_id = $1 AND runbook_ref = $2",
    )
    .bind(tenant)
    .bind(runbook_ref)
    .bind(&removal_id)
    .bind(requested_by)
    .execute(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    let expires_at = chrono::Utc::now() + chrono::Duration::seconds(REMOVAL_TTL_SECS);
    Ok(dto::RemovalRequestResponse {
        runbook_ref: runbook_ref.to_string(),
        removal_id,
        expires_at: expires_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    })
}

/// Pass two: confirm with the matching removal_id inside the TTL.
pub async fn op_confirm_removal(
    state: &AppState,
    tenant: &str,
    runbook_ref: &str,
    removal_id: &str,
) -> std::result::Result<dto::RemovalConfirmResponse, ApiError> {
    let row: Option<(
        String,
        Option<String>,
        Option<chrono::DateTime<chrono::Utc>>,
    )> = sqlx::query_as(
        "SELECT status, removal_id, removal_requested_at
               FROM runbooks WHERE tenant_id = $1 AND runbook_ref = $2",
    )
    .bind(tenant)
    .bind(runbook_ref)
    .fetch_optional(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    let (status, stored_id, requested_at) = row.ok_or_else(|| KernelError::NotFound {
        kind: "runbook",
        id: runbook_ref.to_string(),
    })?;
    if status == "removed" {
        return Err(ApiError::Custom(
            crate::error::CustomError::runbook_removed(runbook_ref),
        ));
    }
    if status != "remove_requested" {
        return Err(ApiError::Custom(
            crate::error::CustomError::removal_not_confirmed(format!(
                "no pending removal request for '{runbook_ref}' — call /remove-request first"
            )),
        ));
    }
    if stored_id.as_deref() != Some(removal_id) {
        return Err(ApiError::Custom(
            crate::error::CustomError::removal_not_confirmed(
                "removal_id does not match the pending request".into(),
            ),
        ));
    }
    let fresh = requested_at
        .map(|t| (chrono::Utc::now() - t).num_seconds() <= REMOVAL_TTL_SECS)
        .unwrap_or(false);
    if !fresh {
        // Expired: reset to active so the state cannot wedge.
        sqlx::query(
            "UPDATE runbooks SET status = 'active', removal_id = NULL, updated_at = now()
              WHERE tenant_id = $1 AND runbook_ref = $2",
        )
        .bind(tenant)
        .bind(runbook_ref)
        .execute(pool(state)?)
        .await
        .map_err(|e| KernelError::Storage(e.to_string()))?;
        return Err(ApiError::Custom(
            crate::error::CustomError::removal_not_confirmed(
                "removal request expired (15 min TTL); request again".into(),
            ),
        ));
    }
    sqlx::query(
        "UPDATE runbooks SET status = 'removed', removed_at = now(), updated_at = now()
          WHERE tenant_id = $1 AND runbook_ref = $2",
    )
    .bind(tenant)
    .bind(runbook_ref)
    .execute(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    Ok(dto::RemovalConfirmResponse {
        runbook_ref: runbook_ref.to_string(),
        status: "removed".into(),
    })
}

/// POST /v1/runbooks/{name}/remove-request
#[utoipa::path(post, path = "/v1/runbooks/{name}/remove-request",
    params(("name" = String, Path, description = "EXACT name@version")),
    responses((status = 200, body = dto::RemovalRequestResponse),
              (status = 404, description = "unknown runbook"),
              (status = 410, description = "already removed")),
    tag = "runbooks")]
pub async fn remove_request(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    uid: Option<axum::Extension<crate::middleware::Uid>>,
) -> ApiResult<Json<dto::RemovalRequestResponse>> {
    let ctx = rest_auth(&state, &headers)?;
    ctx.require_rw()?;
    let requested_by = crate::middleware::uid_or_anonymous(uid.as_ref());
    Ok(Json(
        op_request_removal(&state, &ctx.tenant_id, &name, &requested_by).await?,
    ))
}

/// POST /v1/runbooks/{name}/remove-confirm
#[utoipa::path(post, path = "/v1/runbooks/{name}/remove-confirm",
    params(("name" = String, Path, description = "EXACT name@version")),
    request_body = dto::RemovalConfirmRequest,
    responses((status = 200, description = "soft-removed; all data retained", body = dto::RemovalConfirmResponse),
              (status = 409, description = "no pending request / wrong removal_id / expired")),
    tag = "runbooks")]
pub async fn remove_confirm(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    crate::rest::ProblemJson(req): crate::rest::ProblemJson<dto::RemovalConfirmRequest>,
) -> ApiResult<Json<dto::RemovalConfirmResponse>> {
    let ctx = rest_auth(&state, &headers)?;
    ctx.require_rw()?;
    Ok(Json(
        op_confirm_removal(&state, &ctx.tenant_id, &name, &req.removal_id).await?,
    ))
}

pub(crate) fn uuid_suffix() -> String {
    // UUIDv7 like every other id (ses-/tok-/int-/rm-): wall-clock nanos
    // collided on coarse clocks and were guessable.
    uuid::Uuid::now_v7().simple().to_string()
}

// ---------------------------------------------------------------------------
// REST handlers
// ---------------------------------------------------------------------------

type ApiResult<T> = std::result::Result<T, ApiError>;

fn rest_auth(state: &AppState, headers: &HeaderMap) -> ApiResult<crate::state::TenantCtx> {
    crate::rest::auth_ctx(state, headers)
}

#[utoipa::path(post, path = "/v1/runbooks",
    request_body(content = String, content_type = "text/yaml", description = "kind: Runbook"),
    responses((status = 200, body = dto::ApplyRunbookResponse)), tag = "runbooks")]
pub async fn apply_runbook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    yaml: String,
) -> ApiResult<Json<dto::ApplyRunbookResponse>> {
    let ctx = rest_auth(&state, &headers)?;
    ctx.require_rw()?;
    let runbook_ref = op_apply_runbook(&state, &ctx.tenant_id, &yaml).await?;
    Ok(Json(dto::ApplyRunbookResponse { runbook_ref }))
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct RunQuery {
    version_id: Option<String>,
}

#[utoipa::path(post, path = "/v1/runbooks/{name}/runs",
    params(("name" = String, Path, description = "runbook name or name@version"),
           ("version_id" = Option<String>, Query, description = "lineage: every step transition becomes a ledger event")),
    responses((status = 200, body = dto::RunbookRunResponse)), tag = "runbooks")]
pub async fn run_runbook(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(q): Query<RunQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::RunbookRunResponse>> {
    let ctx = rest_auth(&state, &headers)?;
    ctx.require_rw()?;
    let (run_id, state_now) =
        op_run_runbook(&state, &ctx.tenant_id, &name, q.version_id.as_deref()).await?;
    Ok(Json(dto::RunbookRunResponse {
        run_id,
        state: state_now,
    }))
}

#[utoipa::path(get, path = "/v1/runs/{run_id}",
    params(("run_id" = String, Path)),
    responses((status = 200, body = dto::RunStatusResponse)), tag = "runbooks")]
pub async fn get_run(
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::RunStatusResponse>> {
    let ctx = rest_auth(&state, &headers)?;
    Ok(Json(op_get_run(&state, &ctx.tenant_id, &run_id).await?))
}

#[utoipa::path(post, path = "/v1/runs/{run_id}/steps/{ordinal}/approve",
    params(("run_id" = String, Path), ("ordinal" = usize, Path, description = "step ordinal awaiting approval")),
    responses((status = 200, body = dto::RunbookRunResponse)), tag = "runbooks")]
pub async fn approve_step(
    State(state): State<Arc<AppState>>,
    Path((run_id, ordinal)): Path<(String, usize)>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::RunbookRunResponse>> {
    let ctx = rest_auth(&state, &headers)?;
    ctx.require_rw()?;
    let state_now = op_approve_step(&state, &ctx.tenant_id, &run_id, ordinal).await?;
    Ok(Json(dto::RunbookRunResponse {
        run_id,
        state: state_now,
    }))
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct ListRunbooksQuery {
    #[serde(default)]
    include_removed: bool,
}

/// GET /v1/runbooks — every hosted runbook (all versions) with per-collection
/// access requirements (plan req. 7).
#[utoipa::path(get, path = "/v1/runbooks",
    params(("include_removed" = Option<bool>, Query, description = "include soft-removed runbooks (default false)")),
    responses((status = 200, body = dto::RunbooksResponse)), tag = "runbooks")]
pub async fn list_runbooks(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ListRunbooksQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::RunbooksResponse>> {
    let ctx = rest_auth(&state, &headers)?;
    Ok(Json(dto::RunbooksResponse {
        runbooks: op_list_runbooks(&state, &ctx.tenant_id, q.include_removed).await?,
    }))
}

/// GET /v1/runbooks/{name} — the indexes (collections) a runbook reaches and
/// each one's access requirements (plan req. 8), plus sibling versions.
#[utoipa::path(get, path = "/v1/runbooks/{name}",
    params(("name" = String, Path, description = "runbook name (latest) or name@version")),
    responses((status = 200, body = dto::RunbookInfoResponse),
              (status = 404, description = "unknown runbook")), tag = "runbooks")]
pub async fn get_runbook_info(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::RunbookInfoResponse>> {
    let ctx = rest_auth(&state, &headers)?;
    Ok(Json(op_runbook_info(&state, &ctx.tenant_id, &name).await?))
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct ValidateQuery {
    #[serde(default)]
    suggest: bool,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    tier: Option<String>,
}

/// POST /v1/runbooks/validate — deterministic findings always; ?suggest=true
/// adds AI improvement suggestions via the runbook's validation-task model
/// default (?provider=&model=&tier= override, policy-gated) (plan req. 6).
#[utoipa::path(post, path = "/v1/runbooks/validate",
    request_body(content = String, content_type = "text/yaml", description = "kind: Runbook"),
    params(("suggest" = Option<bool>, Query, description = "add AI-assisted suggestions (BYOK provider call)"),
           ("provider" = Option<String>, Query, description = "model override for the suggestion pass (policy-gated)"),
           ("model" = Option<String>, Query), ("tier" = Option<String>, Query)),
    responses((status = 200, body = dto::ValidateRunbookResponse)), tag = "runbooks")]
pub async fn validate_runbook(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ValidateQuery>,
    headers: HeaderMap,
    yaml: String,
) -> ApiResult<Json<dto::ValidateRunbookResponse>> {
    let ctx = rest_auth(&state, &headers)?;
    ctx.require_rw()?;
    let override_req = crate::models::ModelOverride {
        provider: q.provider,
        model: q.model,
        tier: q.tier,
    };
    let override_ref = (!override_req.is_empty()).then_some(&override_req);
    Ok(Json(
        op_validate_runbook(&state, &ctx.tenant_id, &yaml, q.suggest, override_ref).await?,
    ))
}

// ---------------------------------------------------------------------------
// Control-plane reads for the /admin runbooks hub (2026-08-27). SQL stays in
// this module; dashboard/runbooks.rs only renders what these return.
// ---------------------------------------------------------------------------

/// One published shape as the dashboard lists it. The in-process registry
/// is the source of truth for refs and hashes on BOTH stores (it is what
/// claim validation reads); `created_at` comes from the pg `shapes` row when
/// there is one.
pub struct ShapeSummary {
    pub shape_ref: String,
    pub yaml_hash: String,
    pub created_at: Option<String>,
    pub has_fact_schema: bool,
    /// (strategy, max_chars) when the shape declares chunking.
    pub chunking: Option<(String, usize)>,
}

pub async fn op_list_shapes(state: &AppState, tenant: &str) -> Result<Vec<ShapeSummary>> {
    state.ensure_shapes_loaded(tenant).await?;
    let mut created: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    if let Some(pool) = state.pg_pool() {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT shape_ref, created_at::text FROM shapes WHERE tenant_id = $1")
                .bind(tenant)
                .fetch_all(pool)
                .await
                .map_err(|e| KernelError::Storage(e.to_string()))?;
        created = rows.into_iter().collect();
    }
    let mut out: Vec<ShapeSummary> = state
        .shapes
        .list(tenant)
        .into_iter()
        .filter_map(|(shape_ref, yaml_hash)| {
            let shape = state.shapes.get(tenant, &shape_ref)?;
            Some(ShapeSummary {
                created_at: created.get(&shape_ref).cloned(),
                has_fact_schema: shape.doc.spec.fact.is_some(),
                chunking: shape
                    .doc
                    .spec
                    .chunking
                    .as_ref()
                    .map(|c| (c.strategy.clone(), c.max_chars)),
                shape_ref,
                yaml_hash,
            })
        })
        .collect();
    out.sort_by(|a, b| a.shape_ref.cmp(&b.shape_ref));
    Ok(out)
}

/// The applied YAML of one shape. On pg it is the stored bytes (the row's
/// hash is the hash of exactly this text); on the memory store the registry
/// holds only the parsed document, so the viewer gets a re-serialization
/// and `stored: false` lets it say so.
pub struct ShapeSource {
    pub yaml: String,
    pub stored: bool,
    pub created_at: Option<String>,
}

pub async fn op_shape_source(
    state: &AppState,
    tenant: &str,
    shape_ref: &str,
) -> Result<ShapeSource> {
    state.ensure_shapes_loaded(tenant).await?;
    let shape = state
        .shapes
        .get(tenant, shape_ref)
        .ok_or_else(|| KernelError::NotFound {
            kind: "shape",
            id: shape_ref.to_string(),
        })?;
    if let Some(pool) = state.pg_pool() {
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT yaml, created_at::text FROM shapes WHERE tenant_id = $1 AND shape_ref = $2",
        )
        .bind(tenant)
        .bind(shape_ref)
        .fetch_optional(pool)
        .await
        .map_err(|e| KernelError::Storage(e.to_string()))?;
        if let Some((yaml, created_at)) = row {
            return Ok(ShapeSource {
                yaml,
                stored: true,
                created_at: Some(created_at),
            });
        }
    }
    let yaml =
        serde_yaml::to_string(&shape.doc).map_err(|e| KernelError::Storage(e.to_string()))?;
    Ok(ShapeSource {
        yaml,
        stored: false,
        created_at: None,
    })
}

/// The stored runbook row for the viewer: the applied yaml plus every
/// lifecycle column. Unlike `load_runbook`, a removed runbook resolves —
/// the viewer shows it WITH its status, because "what was this and when did
/// it go" is exactly the operator's question. A bare name resolves to the
/// latest version like every other read.
pub struct RunbookSource {
    pub runbook_ref: String,
    pub yaml: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub removal_requested_by: Option<String>,
    pub removal_requested_at: Option<String>,
    pub removed_at: Option<String>,
}

#[allow(clippy::type_complexity)]
pub async fn op_runbook_source(
    state: &AppState,
    tenant: &str,
    name_or_ref: &str,
) -> Result<RunbookSource> {
    let row: Option<(
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT runbook_ref, yaml, status, created_at::text, updated_at::text,
                removal_requested_by, removal_requested_at::text, removed_at::text
           FROM runbooks
          WHERE tenant_id = $1 AND (runbook_ref = $2 OR split_part(runbook_ref, '@', 1) = $2)
          ORDER BY COALESCE(NULLIF(split_part(runbook_ref, '@', 2), '')::int, 0) DESC
          LIMIT 1",
    )
    .bind(tenant)
    .bind(name_or_ref)
    .fetch_optional(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    let (
        runbook_ref,
        yaml,
        status,
        created_at,
        updated_at,
        removal_requested_by,
        removal_requested_at,
        removed_at,
    ) = row.ok_or_else(|| KernelError::NotFound {
        kind: "runbook",
        id: name_or_ref.to_string(),
    })?;
    Ok(RunbookSource {
        runbook_ref,
        yaml,
        status,
        created_at,
        updated_at,
        removal_requested_by,
        removal_requested_at,
        removed_at,
    })
}

/// Recent runs of one runbook — every version when given a bare name.
/// `(id, runbook_ref, state, created_at)`, newest first.
pub async fn op_runs_for_runbook(
    state: &AppState,
    tenant: &str,
    name_or_ref: &str,
    limit: i64,
) -> Result<Vec<(String, String, String, String)>> {
    sqlx::query_as(
        "SELECT id, runbook_ref, state, created_at::text FROM runbook_runs
          WHERE tenant_id = $1 AND (runbook_ref = $2 OR split_part(runbook_ref, '@', 1) = $2)
          ORDER BY created_at DESC LIMIT $3",
    )
    .bind(tenant)
    .bind(name_or_ref)
    .bind(limit)
    .fetch_all(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in Matrix on a loopback port: records what the verify request
    /// carried and answers a scripted body. This is the "HTTP mock" the crate
    /// was said not to have.
    async fn stand_in_matrix(
        answer: serde_json::Value,
    ) -> (
        String,
        std::sync::Arc<std::sync::Mutex<Vec<axum::http::HeaderMap>>>,
    ) {
        use axum::{extract::State, routing::post, Json, Router};
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<axum::http::HeaderMap>::new()));
        let app = Router::new()
            .route(
                "/v1/{kind}/{name}/verify",
                post(
                    |State((seen, answer)): State<(
                        std::sync::Arc<std::sync::Mutex<Vec<axum::http::HeaderMap>>>,
                        serde_json::Value,
                    )>,
                     headers: axum::http::HeaderMap| async move {
                        seen.lock().unwrap().push(headers);
                        Json(answer)
                    },
                ),
            )
            .with_state((seen.clone(), answer));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), seen)
    }

    fn views() -> Vec<munarium_runbooks::DataViewSpec> {
        parse_runbook(
            "apiVersion: munarium.ioka.io/v1\nkind: Runbook\n\
             metadata: { name: dv-demo, version: 1 }\nspec:\n\
             \x20 collections:\n\
             \x20   - { name: c1, shape: s@1, sources: { filenamePrefix: \"a/\" } }\n\
             \x20 dataViews:\n\
             \x20   - { name: pipeline, contract: open-pipeline-by-region@3, parameters: { as_of: { type: date, value: \"2026-06-30\" } } }\n\
             \x20 steps:\n\
             \x20   - resolveSources: {}\n\
             \x20   - buildIndex: {}\n\
             \x20   - verifyDataViews: {}\n\
             \x20   - cutover: { approval: required }\n",
        )
        .expect("parses")
        .spec
        .data_views
    }

    /// Entry 23's first defect, as a test: the verify request carries the
    /// bearer and the uid. Until 2026-08-29 it carried neither, and the first
    /// runbook to run against a Matrix whose registry was not anonymous
    /// failed the step with two 401s.
    #[tokio::test]
    async fn verify_data_views_sends_the_bearer_and_the_uid() {
        let (base, seen) =
            stand_in_matrix(serde_json::json!({ "passed": 1, "failed": 0, "questions": [] })).await;
        let out = verify_data_views_against(&base, Some("matrix-token"), &views())
            .await
            .expect("a passing verify is Ok");
        assert_eq!(out["verified"], 1, "{out}");
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(
            seen[0].get("authorization").and_then(|v| v.to_str().ok()),
            Some("Bearer matrix-token")
        );
        assert_eq!(
            seen[0].get("x-munarium-uid").and_then(|v| v.to_str().ok()),
            Some("munarium-server")
        );
    }

    /// Entry 23's second defect: a 200 is not a pass. The body is the
    /// verdict, and a `failed: 1` fails the step naming the question.
    #[tokio::test]
    async fn verify_data_views_reads_the_body_not_the_status() {
        let (base, _) = stand_in_matrix(serde_json::json!({
            "passed": 0, "failed": 1,
            "questions": [{ "ok": false, "question": "open pipeline as of 2026-06-30", "failures": ["rows: expected 1, got 3"] }]
        }))
        .await;
        let err = verify_data_views_against(&base, Some("t"), &views())
            .await
            .expect_err("a failed question fails the step");
        let text = err.to_string();
        assert!(
            text.contains("1 of 1 data view(s) failed verification"),
            "{text}"
        );
        assert!(text.contains("rows: expected 1, got 3"), "{text}");
    }

    fn doc_with(order: Option<&str>) -> RunbookDoc {
        let exec = order
            .map(|o| format!("  execution: {{ order: {o} }}\n"))
            .unwrap_or_default();
        parse_runbook(&format!(
            "apiVersion: munarium.ioka.io/v1\nkind: Runbook\n\
             metadata: {{ name: order-demo, version: 2 }}\nspec:\n{exec}\
             \x20 collections:\n\
             \x20   - {{ name: c1, shape: s@1, sources: {{ filenamePrefix: \"a/\" }} }}\n\
             \x20   - {{ name: c2, shape: s@1, sources: {{ filenamePrefix: \"b/\" }} }}\n\
             \x20 steps:\n\
             \x20   - resolveSources: {{}}\n\
             \x20   - buildIndex: {{}}\n\
             \x20   - cutover: {{ approval: required }}\n"
        ))
        .expect("parses")
    }

    /// The ordering contract that decides how much work one HTTP request
    /// carries. Only `cutover` gates, so under stepMajor BOTH collections
    /// build before the first gate; under collectionMajor the run stops
    /// after c1's build, leaving c2's build to the next request.
    #[test]
    fn exec_units_order_bounds_the_first_request() {
        let names =
            |d: &RunbookDoc| -> Vec<String> { exec_units(d).into_iter().map(|u| u.name).collect() };

        // Default (no `execution:` declared) must be byte-identical to the
        // original step-major behavior every committed runbook relies on.
        let step_major = names(&doc_with(None));
        assert_eq!(step_major, names(&doc_with(Some("stepMajor"))));
        assert_eq!(
            step_major,
            vec![
                "resolveSources:c1",
                "resolveSources:c2",
                "buildIndex:c1",
                "buildIndex:c2",
                "cutover:c1",
                "cutover:c2",
            ]
        );

        assert_eq!(
            names(&doc_with(Some("collectionMajor"))),
            vec![
                "resolveSources:c1",
                "buildIndex:c1",
                "cutover:c1",
                "resolveSources:c2",
                "buildIndex:c2",
                "cutover:c2",
            ]
        );

        // The property that actually matters: how many buildIndex units sit
        // before the first approval gate.
        let builds_before_gate = |units: &[String]| {
            let gate = units
                .iter()
                .position(|n| n.starts_with("cutover:"))
                .unwrap();
            units[..gate]
                .iter()
                .filter(|n| n.starts_with("buildIndex:"))
                .count()
        };
        assert_eq!(
            builds_before_gate(&step_major),
            2,
            "step-major: all builds up front"
        );
        assert_eq!(
            builds_before_gate(&names(&doc_with(Some("collectionMajor")))),
            1,
            "collection-major: exactly one collection's build per request"
        );
    }

    /// v1 (no collections) ignores the field entirely — there is nothing to
    /// interleave, and the legacy shape-scoped path must not change.
    #[test]
    fn v1_plan_is_unaffected_by_execution_order() {
        let v1 = parse_runbook(
            "apiVersion: munarium.ioka.io/v1\nkind: Runbook\n\
             metadata: { name: legacy, version: 1 }\nspec:\n  shape: s@1\n\
             \x20 execution: { order: collectionMajor }\n\
             \x20 steps:\n    - buildIndex: {}\n    - cutover: { approval: required }\n",
        )
        .expect("parses");
        let names: Vec<String> = exec_units(&v1).into_iter().map(|u| u.name).collect();
        assert_eq!(names, vec!["buildIndex", "cutover"]);
    }

    const WITH_VIEWS: &str = r#"
apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: { name: withviews, version: 1 }
spec:
  collections:
    - { name: contracts, shape: contracts@1 }
    - { name: minutes, shape: minutes@1 }
  dataViews:
    - { name: revenue, contract: revenue_by_region@2 }
  retrieval:
    researchProfiles:
      - name: d
        layers:
          - { name: register, sources: [matrix:revenue] }
  steps:
    - buildIndex: {}
    - verifyDataViews: {}
"#;

    #[test]
    fn verify_data_views_is_planned_once_not_once_per_collection() {
        // Data views belong to the runbook. Fanning this across collections
        // would verify the same contracts twice and report two identical
        // steps -- misleading, and twice the calls to a second service.
        let doc = munarium_runbooks::parse_runbook(WITH_VIEWS).expect("parses");
        let units = exec_units(&doc);
        let names: Vec<String> = units.iter().map(|u| u.name.clone()).collect();
        assert_eq!(
            names,
            vec![
                "buildIndex:contracts",
                "buildIndex:minutes",
                "verifyDataViews"
            ],
        );
        let vdv = units
            .iter()
            .find(|u| u.name == "verifyDataViews")
            .expect("planned");
        assert!(
            vdv.collection.is_none(),
            "runbook-scoped, not collection-scoped"
        );
    }

    #[test]
    fn collection_major_still_plans_verify_data_views_once_at_the_end() {
        let yaml = WITH_VIEWS.replace(
            "  steps:",
            "  execution: { order: collectionMajor }
  steps:",
        );
        let doc = munarium_runbooks::parse_runbook(&yaml).expect("parses");
        let names: Vec<String> = exec_units(&doc).into_iter().map(|u| u.name).collect();
        assert_eq!(
            names,
            vec![
                "buildIndex:contracts",
                "buildIndex:minutes",
                "verifyDataViews"
            ],
        );
    }
}
