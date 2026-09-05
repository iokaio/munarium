// SPDX-License-Identifier: Apache-2.0
//! The REST plane on :8180.
//!
//! Route-level rules:
//!
//! - **Role gating is structural.** A `sync` container answers 404 on the
//!   registry, because it does not mount those routes at all. That is stronger
//!   than a guard inside each handler and impossible to forget on a new route.
//! - **Every failure is problem+json** with a `matrix:` slug, and a typed
//!   refusal travels in the body rather than being flattened to a message.
//! - **Every WRITE is journaled** with its outcome, redacted by default —
//!   apply, execute, verify, and the two scheduling routes. Reads (list, get,
//!   validate, journal, healthdata) are not: they change nothing, and
//!   journaling them would bury the writes an auditor came for. This line used
//!   to say "every request", which was never true of any route but apply.

use crate::state::{AppState, Caller};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use munarium_matrix_core::Refusal;
use munarium_matrix_store::journal::{JournalQuery, JournalRecord};
use munarium_matrix_types::dto::*;
use munarium_matrix_types::{parse_asset, validate};
use std::sync::Arc;

/// A problem+json response. Implemented as a wrapper so `?` works on handlers
/// that mix store errors, refusals and validation failures.
pub struct ApiError(pub Problem);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status =
            StatusCode::from_u16(self.0.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut r = (status, Json(&self.0)).into_response();
        r.headers_mut().insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/problem+json"),
        );
        r
    }
}

impl From<Problem> for ApiError {
    fn from(p: Problem) -> Self {
        ApiError(p)
    }
}

impl From<munarium_matrix_store::StoreError> for ApiError {
    fn from(e: munarium_matrix_store::StoreError) -> Self {
        use munarium_matrix_store::StoreError as E;
        ApiError(match e {
            E::NotFound { kind, id } => {
                Problem::new("not-found", 404, "not found", format!("{kind} '{id}'"))
            }
            E::Conflict(m) => Problem::new("conflict", 409, "conflict", m),
            other => Problem::new("storage", 500, "storage error", other.to_string()),
        })
    }
}

impl From<Refusal> for ApiError {
    fn from(r: Refusal) -> Self {
        // The refusal's CLASS decides the status; the code is detail. That
        // mapping lives in one place so two routes cannot disagree about what
        // `policy_denied` means over HTTP.
        use munarium_matrix_core::RefusalClass as C;
        let status = match r.class {
            C::NotCovered => 422,
            C::Unavailable => 503,
            C::Denied => 403,
            C::Incomplete => 200, // an incomplete result is a real answer
            C::Invalid => 400,
            C::Exhausted => 429,
        };
        let slug = r.code.replace('_', "-");
        let mut p = Problem::new(&slug, status, r.class.as_str(), r.message.clone());
        p.refusal = serde_json::to_value(&r).ok();
        ApiError(p)
    }
}

type ApiResult<T> = std::result::Result<T, ApiError>;

pub(crate) fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

pub(crate) fn auth(state: &AppState, headers: &HeaderMap) -> ApiResult<Caller> {
    Ok(state.authenticate(bearer(headers))?)
}

/// The plane a journal row came in on.
///
/// A closed little vocabulary rather than free text, and a PARAMETER rather
/// than a header: `via` is an audit field, and an audit field a caller can set
/// is one nobody can trust.
pub(crate) const VIA_API: &str = "api";
pub(crate) const VIA_ADMIN_UI: &str = "admin-ui";

fn request_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-munarium-request-id")
        .and_then(|v| v.to_str().ok())
        .map(String::from)
}

// ---------------------------------------------------------------------------
// meta
// ---------------------------------------------------------------------------

async fn healthz() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true }))
}

async fn readyz(State(state): State<Arc<AppState>>) -> Response {
    if state.draining.load(std::sync::atomic::Ordering::Relaxed) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "ok": false, "status": "draining" })),
        )
            .into_response();
    }
    // A role that owns no store work is still only ready when it can journal.
    if state.store.ready().await {
        (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "ok": false, "status": "store not ready" })),
        )
            .into_response()
    }
}

async fn version(State(state): State<Arc<AppState>>) -> Json<VersionResponse> {
    Json(VersionResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        contract_version: munarium_matrix_core::CONTRACT_VERSION.to_string(),
        role: state.role().as_str().to_string(),
        target_server_version: state.config.target_server_version.clone(),
        server_version: state.server_version.clone(),
        server_compatibility: Some(state.server_compatibility.as_str().to_string()),
        uptime_seconds: state.uptime_seconds(),
    })
}

async fn openapi_json() -> Json<serde_json::Value> {
    Json(crate::openapi::document())
}

async fn docs_page() -> axum::response::Html<&'static str> {
    axum::response::Html(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>munarium-matrix</title>
<style>body{font:14px/1.5 system-ui;margin:40px auto;max-width:52rem;padding:0 1rem}
code{background:#f4f4f2;padding:1px 4px;border-radius:3px}</style></head><body>
<h1>munarium-matrix</h1>
<p>The structured-evidence plane. This service registers formal data sources,
materializes governed record collections, executes verified query contracts,
and seals the exact typed evidence an answer used into munarium-server.</p>
<ul>
<li><strong>registry</strong> — <code>/v1/datasources</code>, <code>/v1/contracts</code>, <code>/v1/mappings</code> (apply is idempotent by name+version)</li>
<li><strong>operations</strong> — <code>/v1/datasources/{name}/introspect|probe|sync</code>, <code>/v1/contracts/{name}/execute|verify</code>, <code>/v1/mappings/{name}/run</code></li>
<li><strong>observability</strong> — <code>/v1/journal</code>, <code>/healthdata</code>; <code>/metrics</code> on the ops port</li>
</ul>
<p>The machine-readable contract with munarium-server is <code>matrix/contract/</code>;
this API's own schema is <a href="/openapi.json">/openapi.json</a>.</p>
</body></html>"#,
    )
}

async fn healthdata(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<HealthDataResponse>> {
    let caller = auth(&state, &headers)?;
    let sources = state
        .store
        .list_assets(&caller.tenant, Some("DataSource"), true)
        .await?;
    // Registration, NOT connectivity — probing every source on a health call
    // would make a health endpoint an outbound-traffic amplifier.
    // `POST /v1/datasources/{name}/probe` is the deliberate per-source check,
    // and as of 2026-08-29 it exists; this route pointed at it for months while
    // it answered 404.
    //
    // `reachable` is None here rather than `true`. It used to be hard-coded
    // true, which made this a health endpoint that could not report ill health
    // — worse than none, because it looks like an answer.
    let rows = sources
        .into_iter()
        .map(|s| ProbeResponse {
            source: s.name,
            reachable: false,
            latency_ms: None,
            breaker: "unknown".into(),
            detail: Some(
                "registered; connectivity NOT checked — POST /v1/datasources/{name}/probe".into(),
            ),
        })
        .collect::<Vec<_>>();
    Ok(Json(HealthDataResponse {
        healthy: true,
        sources: rows,
    }))
}

// ---------------------------------------------------------------------------
// registry
// ---------------------------------------------------------------------------

/// `POST /v1/assets` — apply any asset kind, sniffed by parsing.
async fn apply_asset(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    yaml: String,
) -> ApiResult<Json<ApplyResponse>> {
    let caller = auth(&state, &headers)?;
    caller.require_rw()?;
    op_apply_asset(&state, &caller, &yaml, request_id(&headers), VIA_API)
        .await
        .map(Json)
}

/// The body `/v1` and the admin console share. See [`op_probe`].
///
/// `request_id` doubles as the console's DECISION id on an apply-in-place: it
/// is the operator's record of why the repository was not the thing that
/// changed, and it belongs in the same journal column an API caller's
/// correlation id would occupy — one place to look, rather than two.
pub(crate) async fn op_apply_asset(
    state: &Arc<AppState>,
    caller: &Caller,
    yaml: &str,
    request_id: Option<String>,
    via: &str,
) -> ApiResult<ApplyResponse> {
    let yaml = yaml.to_string();
    let asset = parse_asset(&yaml)
        .map_err(|e| Problem::new("asset-invalid", 400, "asset did not parse", e.to_string()))?;
    let findings = asset.validate();
    if !validate::is_valid(&findings) {
        let mut p = Problem::new(
            "asset-invalid",
            422,
            "asset failed validation",
            format!(
                "{} error finding(s); nothing was applied",
                findings.iter().filter(|f| validate::is_error(f)).count()
            ),
        );
        p.refusal = serde_json::to_value(&findings).ok();
        return Err(ApiError(p));
    }

    let outcome = state
        .store
        .apply_asset(&caller.tenant, &asset, &yaml)
        .await?;
    let _ = state
        .store
        .journal(
            &caller.tenant,
            JournalRecord::new("apply", "ok")
                .asset(&outcome.asset_ref)
                .request(request_id)
                .via(via),
        )
        .await;
    state.metrics.inc(
        "munarium_matrix_assets_applied_total",
        &[("kind", asset.kind())],
    );
    Ok(ApplyResponse {
        asset_ref: outcome.asset_ref,
        kind: outcome.kind,
        unchanged: outcome.unchanged,
        findings,
    })
}

/// `POST /v1/assets/validate` — the same validators, without applying. This is
/// the code path `mxctl validate` uses, so the CLI and the API cannot disagree
/// about whether an asset is healthy.
async fn validate_asset(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    yaml: String,
) -> ApiResult<Json<ValidateResponse>> {
    let _ = auth(&state, &headers)?;
    match parse_asset(&yaml) {
        Ok(asset) => {
            let findings = asset.validate();
            Ok(Json(ValidateResponse {
                valid: validate::is_valid(&findings),
                findings,
            }))
        }
        Err(e) => Ok(Json(ValidateResponse {
            valid: false,
            findings: vec![validate::Finding {
                code: "parse".into(),
                path: "$".into(),
                message: e.to_string(),
            }],
        })),
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    all_versions: bool,
}

fn summarize(a: munarium_matrix_store::StoredAsset) -> AssetSummary {
    AssetSummary {
        asset_ref: a.asset_ref(),
        name: a.name,
        version: a.version,
        kind: a.kind,
        created_at: a.created_at,
        source: a.source_name,
    }
}

async fn list_kind(
    state: &AppState,
    headers: &HeaderMap,
    kind: &str,
    q: &ListQuery,
) -> ApiResult<Json<AssetListResponse>> {
    let caller = auth(state, headers)?;
    let assets = state
        .store
        .list_assets(&caller.tenant, Some(kind), !q.all_versions)
        .await?;
    Ok(Json(AssetListResponse {
        assets: assets.into_iter().map(summarize).collect(),
    }))
}

async fn get_kind(
    state: &AppState,
    headers: &HeaderMap,
    kind: &str,
    name: &str,
) -> ApiResult<Response> {
    let caller = auth(state, headers)?;
    let asset = state.store.get_asset(&caller.tenant, kind, name).await?;
    // The applied YAML back, verbatim. A round-trip through the parsed form
    // would silently normalize an operator's file.
    Ok((
        [(axum::http::header::CONTENT_TYPE, "text/yaml; charset=utf-8")],
        asset.yaml,
    )
        .into_response())
}

async fn list_datasources(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ListQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<AssetListResponse>> {
    list_kind(&state, &headers, "DataSource", &q).await
}
async fn get_datasource(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    get_kind(&state, &headers, "DataSource", &name).await
}
async fn list_contracts(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ListQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<AssetListResponse>> {
    list_kind(&state, &headers, "QueryContract", &q).await
}
async fn get_contract(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    get_kind(&state, &headers, "QueryContract", &name).await
}
async fn list_metric_views(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ListQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<AssetListResponse>> {
    list_kind(&state, &headers, "MetricView", &q).await
}
async fn get_metric_view(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    get_kind(&state, &headers, "MetricView", &name).await
}
async fn list_data_views(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ListQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<AssetListResponse>> {
    list_kind(&state, &headers, "DataView", &q).await
}
async fn get_data_view(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    get_kind(&state, &headers, "DataView", &name).await
}
async fn list_mappings(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ListQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<AssetListResponse>> {
    list_kind(&state, &headers, "ClaimMapping", &q).await
}
async fn get_mapping(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    get_kind(&state, &headers, "ClaimMapping", &name).await
}

// ---------------------------------------------------------------------------
// journal
// ---------------------------------------------------------------------------

/// Query params for `GET /v1/mappings/{name}/gate-history`.
#[derive(Debug, serde::Deserialize)]
pub struct GateHistoryParams {
    /// Runs to return, newest first. Clamped to [1, 500] by the store.
    limit: Option<i64>,
}

#[derive(Debug, serde::Deserialize)]
pub struct JournalParams {
    kind: Option<String>,
    source: Option<String>,
    #[serde(default)]
    refusals: bool,
    limit: Option<String>,
}

async fn list_journal(
    State(state): State<Arc<AppState>>,
    Query(p): Query<JournalParams>,
    headers: HeaderMap,
) -> ApiResult<Json<JournalListResponse>> {
    let caller = auth(&state, &headers)?;
    caller.require_mgmt()?;
    let q = JournalQuery {
        kind: p.kind,
        source_name: p.source,
        refusals_only: p.refusals,
        before: None,
        // Text, then parsed: a cleared `?limit=` in a form must not 422.
        limit: p.limit.and_then(|l| l.trim().parse().ok()).unwrap_or(100),
    };
    let entries = state.store.list_journal(&caller.tenant, &q).await?;
    Ok(Json(JournalListResponse {
        entries,
        next_before: None,
    }))
}

// ---------------------------------------------------------------------------
// the query plane (mode B) and the job plane
// ---------------------------------------------------------------------------

/// Choose the authorization class an intent may read under.
///
/// The caller's session snapshot must **dominate** the class: at least its
/// access level, and every one of its compartments. When several classes
/// qualify we take the most specific the caller can see — the highest level
/// they dominate — because a caller cleared for more should not silently get
/// the least-privileged view.
///
/// Nothing here filters rows. A class the caller cannot dominate is refused
/// outright, because a partial answer that looks whole is the failure mode this
/// design exists to prevent.
pub(crate) fn class_for_intent<'a>(
    classes: &'a [munarium_matrix_workers::ResolvedClass],
    snapshot: &munarium_matrix_types::contract::AuthorizationSnapshot,
) -> Result<&'a munarium_matrix_workers::ResolvedClass, Refusal> {
    let mut permitted: Vec<&munarium_matrix_workers::ResolvedClass> = classes
        .iter()
        .filter(|c| {
            c.as_core()
                .dominated_by(snapshot.access_level, &snapshot.compartments)
        })
        .collect();
    permitted.sort_by_key(|c| -c.access_level);
    permitted.first().copied().ok_or_else(|| {
        // Name the requirement, never the data. Saying which compartment is
        // missing is fine; saying what it would have revealed is not.
        Refusal::policy_denied(format!(
            "this session (level {}, compartments {:?}) dominates none of the source's \
             {} authorization class(es); nothing partial is returned",
            snapshot.access_level,
            snapshot.compartments,
            classes.len()
        ))
    })
}

/// The dialect a source's statements are written in. Taken from what the
/// adapter DECLARES, never from the adapter's name: a contract compiled for the
/// wrong dialect would parse and mean something else.
pub(crate) fn dialect_of(
    adapter: &dyn munarium_matrix_adapter::SourceAdapter,
) -> Result<String, Refusal> {
    adapter.capabilities().dialect.clone().ok_or_else(|| {
        Refusal::not_covered(format!(
            "adapter '{}' declares no SQL dialect, so a query contract cannot be compiled \
             for it",
            adapter.kind()
        ))
    })
}

/// Values pinned for `allowedValuesFrom` parameters at introspect time.
pub(crate) async fn pinned_domains(
    state: &AppState,
    tenant: &str,
    contract: &munarium_matrix_types::QueryContractDoc,
) -> std::collections::BTreeMap<String, Vec<String>> {
    let mut out = std::collections::BTreeMap::new();
    for (name, spec) in &contract.spec.parameters {
        if spec.allowed_values_from.is_none() {
            continue;
        }
        if let Ok(Some(values)) = state
            .store
            .parameter_domain(
                tenant,
                &contract.metadata.name,
                contract.metadata.version,
                name,
            )
            .await
        {
            out.insert(name.clone(), values);
        }
    }
    out
}

/// Whether a refusal means the source was reached.
///
/// This decides whether a failed execution spends budget, so it is a policy
/// question rather than a formatting one. `Invalid`, `NotCovered` and `Denied`
/// are all raised by the compiler, the binder or the policy check — before any
/// statement exists — so the source did no work and the units go back.
/// `Unavailable`, `Incomplete` and `Exhausted` can only be known by trying, so
/// the units are kept.
///
/// Refunding everything would let a client hammer a source for free by sending
/// requests that always fail late; keeping everything would charge for typos.
pub(crate) fn source_was_touched(r: &Refusal) -> bool {
    use munarium_matrix_core::RefusalClass as C;
    // Codes first, because the CLASS is not always specific enough. A
    // `schema_drift` is class `Invalid`, but it is the ENGINE rejecting a
    // statement we sent — the source answered, so the unit is spent. Class
    // `Invalid` otherwise covers compile and bind failures that never left the
    // process. Observed live on 2026-08-28: without this, a contract naming a
    // missing relation could be retried without limit.
    // `metric_view_changed` joins them: the definition was READ from the
    // source to be compared, so the source was reached.
    if matches!(
        r.code.as_str(),
        "schema_drift" | "deadline_exceeded" | "metric_view_changed"
    ) {
        return true;
    }
    match r.class {
        C::Invalid | C::NotCovered | C::Denied => false,
        C::Unavailable | C::Incomplete | C::Exhausted => true,
    }
}

/// Journal one outcome, best-effort.
///
/// Best-effort on purpose: a journal write that fails must not turn a
/// successful execution into an error for the caller. It is logged instead, so
/// the gap is visible to an operator rather than silent.
async fn journal_outcome(
    state: &AppState,
    caller: &Caller,
    rec: munarium_matrix_store::journal::JournalRecord,
) {
    if let Err(e) = state.store.journal(&caller.tenant, rec).await {
        tracing::warn!(error = %e, tenant = %caller.tenant, "journal write failed");
    }
}

/// `POST /v1/contracts/{name}/execute` — run one verified query contract.
///
/// The turn path. The caller holds the other end of the deadline, which is why
/// this is a synchronous route and not a queued job.
async fn execute_contract(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(intent): Json<munarium_matrix_types::contract::QueryIntent>,
) -> ApiResult<Response> {
    let caller = auth(&state, &headers)?;
    caller.require_rw()?;
    // The body lives in `execute.rs`, shared with the gRPC plane, so the two
    // cannot diverge. REST has no progress to report.
    let (block, report) = crate::execute::execute_intent_timed(
        &state,
        &caller,
        &name,
        &intent,
        request_id(&headers),
        "api",
        |_| {},
    )
    .await?;
    // Where the time went, as a header rather than a body field: the block is
    // the vendored contract's `EvidenceBlock`, and a measurement is not part
    // of what an answer cites. `Server-Timing` is the standard shape for it
    // (2026-08-30, the §18.3 harness reads it).
    let mut resp = Json(block).into_response();
    if let Ok(v) = axum::http::HeaderValue::from_str(&report.server_timing()) {
        resp.headers_mut().insert("server-timing", v);
    }
    Ok(resp)
}

/// `POST /v1/metricviews/{name}/verify` — run the metric view's verified
/// questions under the definition the source reports NOW, and record that
/// definition's fingerprint with the outcome.
///
/// The record is what a later execute is held to. It is written whether the
/// suite passed or failed — a failing record after a passing one BLOCKS
/// execution, because the latest word on the definition is that it no longer
/// answers as it did — and the execute path reads only the latest.
async fn verify_metric_view(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<VerifyResponse>> {
    verify_semantic_view(state, name, headers, "MetricView").await
}

/// `POST /v1/dataviews/{name}/verify` — the same, for a native data view.
async fn verify_data_view(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<VerifyResponse>> {
    verify_semantic_view(state, name, headers, "DataView").await
}

async fn verify_semantic_view(
    state: Arc<AppState>,
    name: String,
    headers: HeaderMap,
    kind: &str,
) -> ApiResult<Json<VerifyResponse>> {
    let caller = auth(&state, &headers)?;
    caller.require_rw()?;

    let doc = crate::runtime::load_semantic_view(&state, &caller.tenant, &name, Some(kind)).await?;
    let view = doc.as_view();
    let wiring = crate::runtime::wire(&state, &caller.tenant, view.source()).await?;

    let classes = munarium_matrix_workers::resolve_classes(&wiring.source.spec.authorization)?;
    let class = classes
        .iter()
        .max_by_key(|c| c.access_level)
        .ok_or_else(|| {
            Refusal::policy_delegation_unavailable(format!(
                "source '{}' resolved no authorization class",
                wiring.source.metadata.name
            ))
        })?;
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

    let outcome = munarium_matrix_workers::verify_metric(
        wiring.adapter.as_ref(),
        wiring.server.as_ref(),
        view,
        &caller.tenant,
        &ctx,
    )
    .await?;
    let failed = outcome.questions.iter().filter(|o| !o.ok).count();
    let passed = outcome.questions.len() - failed;
    state
        .store
        .record_metric_verification(
            &caller.tenant,
            &munarium_matrix_store::MetricVerificationRecord {
                kind: view.kind(),
                view_name: view.name(),
                view_version: view.version(),
                fingerprint: &outcome.fingerprint,
                passed,
                failed,
            },
        )
        .await
        .map_err(|e| Refusal::source_unavailable(format!("verification store: {e}")))?;
    journal_outcome(
        &state,
        &caller,
        JournalRecord::new("verify", if failed == 0 { "ok" } else { "failed" })
            .asset(view.asset_ref())
            .source(&wiring.source.metadata.name)
            .request(request_id(&headers))
            .via("api")
            .rows(outcome.questions.len()),
    )
    .await;
    Ok(Json(VerifyResponse {
        contract: view.asset_ref(),
        passed,
        failed,
        fingerprint: Some(outcome.fingerprint),
        questions: outcome
            .questions
            .into_iter()
            .map(|o| VerifiedQuestionResult {
                question: o.question,
                ok: o.ok,
                rows: o.rows,
                logical_result_hash: o.logical_result_hash,
                failures: o.failures,
            })
            .collect(),
    }))
}

/// `POST /v1/contracts/{name}/verify` — run the contract's verified questions.
///
/// Not a smoke test: these questions are the contract's regression suite, and a
/// failure here means the contract no longer means what it claimed when it was
/// reviewed. The route answers 200 with per-question outcomes so a caller can
/// see WHICH question moved; the CLI turns a non-empty failure list into a
/// non-zero exit.
async fn verify_contract(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<VerifyResponse>> {
    let caller = auth(&state, &headers)?;
    caller.require_rw()?;
    op_verify_contract(&state, &caller, &name, request_id(&headers), VIA_API)
        .await
        .map(Json)
}

/// The body `/v1` and the admin console share. See [`op_probe`].
///
/// Note where this is mounted: verification is a QUERY-plane act, so a
/// control-only container does not serve it and the console renders the
/// action as a note there rather than as a button that would 404. On `all`
/// — the laptop and a single-container deployment — both are in one process.
pub(crate) async fn op_verify_contract(
    state: &Arc<AppState>,
    caller: &Caller,
    name: &str,
    request_id: Option<String>,
    via: &str,
) -> ApiResult<VerifyResponse> {
    let contract = crate::runtime::load_contract(state, &caller.tenant, name).await?;
    let wiring = crate::runtime::wire(state, &caller.tenant, &contract.spec.source).await?;

    let classes = munarium_matrix_workers::resolve_classes(&wiring.source.spec.authorization)?;
    // Verification runs as the source's OWN most-privileged class: it is an
    // operator action about whether the contract still computes what it said,
    // not a customer read.
    let class = classes
        .iter()
        .max_by_key(|c| c.access_level)
        .ok_or_else(|| {
            Refusal::policy_delegation_unavailable(format!(
                "source '{}' resolved no authorization class",
                wiring.source.metadata.name
            ))
        })?;
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

    let outcomes = munarium_matrix_workers::verify(
        wiring.adapter.as_ref(),
        wiring.server.as_ref(),
        &contract,
        &caller.tenant,
        &ctx,
    )
    .await;
    let failed = outcomes.iter().filter(|o| !o.ok).count();
    journal_outcome(
        state,
        caller,
        JournalRecord::new("verify", if failed == 0 { "ok" } else { "failed" })
            .asset(contract.metadata.asset_ref())
            .source(&wiring.source.metadata.name)
            .request(request_id)
            .via(via)
            .rows(outcomes.len()),
    )
    .await;
    Ok(VerifyResponse {
        contract: contract.metadata.asset_ref(),
        passed: outcomes.len() - failed,
        failed,
        fingerprint: None,
        questions: outcomes
            .into_iter()
            .map(|o| VerifiedQuestionResult {
                question: o.question,
                ok: o.ok,
                rows: o.rows,
                logical_result_hash: o.logical_result_hash,
                failures: o.failures,
            })
            .collect(),
    })
}

/// `POST /v1/datasources/{name}/probe` — is this source reachable, right now?
///
/// Advertised by `/docs` and named by `/healthdata` since the first release, and not
/// implemented until 2026-08-29: `/healthdata` told operators to "probe
/// explicitly for connectivity" via a route that answered 404.
///
/// A deliberate per-source act rather than something a health check does for
/// you — probing every source on every health call would turn a health
/// endpoint into an outbound-traffic amplifier, which is why `/healthdata`
/// reports registration and this reports reachability.
async fn probe_source(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<ProbeResponse>> {
    let caller = auth(&state, &headers)?;
    caller.require_rw()?;
    op_probe(&state, &caller, &name, request_id(&headers), VIA_API)
        .await
        .map(Json)
}

/// The body `/v1` and the admin console share.
///
/// Lifted out rather than copied, for the reason `execute.rs` was: the console
/// must not be a second implementation of a policy. `via` is a PARAMETER and
/// never a request header — `via` is an audit field, and an audit field a
/// caller can set is one nobody can trust.
pub(crate) async fn op_probe(
    state: &Arc<AppState>,
    caller: &Caller,
    name: &str,
    request_id: Option<String>,
    via: &str,
) -> ApiResult<ProbeResponse> {
    let name = name.to_string();
    // Load first, so a typo is a 404 rather than a connection attempt.
    let wiring = crate::runtime::wire(state, &caller.tenant, &name).await?;
    let started = std::time::Instant::now();
    let result = wiring.adapter.probe().await;
    let elapsed = started.elapsed().as_millis() as u64;

    let response = match result {
        Ok(p) => ProbeResponse {
            source: name.clone(),
            reachable: p.reachable,
            latency_ms: Some(elapsed),
            breaker: "closed".into(),
            detail: p.detail,
        },
        // A refusal is an ANSWER here, not an error: "unreachable, and here is
        // the typed reason" is exactly what an operator asked for. Returning
        // 503 would make a working probe look like a broken endpoint.
        Err(refusal) => ProbeResponse {
            source: name.clone(),
            reachable: false,
            latency_ms: Some(elapsed),
            breaker: "closed".into(),
            detail: Some(refusal.message.clone()),
        },
    };
    journal_outcome(
        state,
        caller,
        JournalRecord::new("probe", if response.reachable { "ok" } else { "refused" })
            .source(&name)
            .request(request_id)
            .via(via),
    )
    .await;
    Ok(response)
}

/// `POST /v1/datasources/{name}/introspect` — prove the role posture and read
/// the schema.
///
/// The seed for an authored contract: it reports the tables and columns the
/// source actually exposes to the effective principal, so an author can write
/// `spec.reads` against what is there rather than guessing. the console's configure
/// loop is built on this.
///
/// It REFUSES a role that is a superuser, an owner, or holds DML — the posture
/// is proven from the catalog at connect time, never taken on trust from the
/// asset.
async fn introspect_source(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<IntrospectResponse>> {
    let caller = auth(&state, &headers)?;
    caller.require_rw()?;
    op_introspect(&state, &caller, &name, request_id(&headers), VIA_API)
        .await
        .map(Json)
}

/// The body `/v1` and the admin console share. See [`op_probe`].
pub(crate) async fn op_introspect(
    state: &Arc<AppState>,
    caller: &Caller,
    name: &str,
    request_id: Option<String>,
    via: &str,
) -> ApiResult<IntrospectResponse> {
    let name = name.to_string();
    let wiring = crate::runtime::wire(state, &caller.tenant, &name).await?;
    let (posture, fingerprint) = wiring.adapter.introspect().await?;

    // Each posture check is reported individually. A single boolean would hide
    // WHICH requirement failed, and "your role is wrong" is not actionable.
    let posture = RolePostureReport {
        ok: posture.ok(),
        principal: posture.principal.clone(),
        checks: posture
            .checks
            .iter()
            .map(|c| PostureCheck {
                name: c.name.clone(),
                required: c.required,
                observed: c.observed,
                ok: c.ok,
                detail: c.detail.clone(),
            })
            .collect(),
    };

    let tables: Vec<TableInfo> = fingerprint
        .tables
        .iter()
        .map(|tb| TableInfo {
            name: tb.name.clone(),
            row_security_enabled: tb.row_security_enabled,
            columns: tb
                .columns
                .iter()
                .map(|c| ColumnInfo {
                    name: c.name.clone(),
                    source_type: c.source_type.clone(),
                    // None stays None: a column that maps to no canon@1 type
                    // cannot be used, and that is reported rather than
                    // silently coerced to a string.
                    logical_type: c.logical_type.map(|lt| lt.as_str().to_string()),
                    nullable: c.nullable,
                })
                .collect(),
        })
        .collect();

    journal_outcome(
        state,
        caller,
        JournalRecord::new("introspect", "ok")
            .source(&name)
            .request(request_id)
            .via(via)
            .rows(tables.len()),
    )
    .await;
    Ok(IntrospectResponse {
        source: name,
        posture,
        schema_fingerprint: Some(fingerprint.fingerprint.clone()),
        tables,
        // Deliberately None. Seeding a draft is the admin console's job
        // (`/admin/author`), and emitting one here would make this route look
        // like it authors contracts when all it does is report what the source
        // exposes.
        draft_contract_yaml: None,
    })
}

/// `POST /v1/datasources/{name}/sync` — enqueue a sync run.
///
/// Enqueue, not execute. A sync can take minutes and must survive the caller
/// hanging up, so it belongs to the sync role's queue and this route returns
/// the job id to watch.
async fn enqueue_sync(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<JobAccepted>> {
    let caller = auth(&state, &headers)?;
    caller.require_rw()?;
    op_enqueue_sync(&state, &caller, &name, request_id(&headers), VIA_API)
        .await
        .map(Json)
}

/// The body `/v1` and the admin console share. See [`op_probe`].
pub(crate) async fn op_enqueue_sync(
    state: &Arc<AppState>,
    caller: &Caller,
    name: &str,
    request_id: Option<String>,
    via: &str,
) -> ApiResult<JobAccepted> {
    let name = name.to_string();
    // Load the source so a typo is a 404 now rather than a failed job later.
    let source = crate::runtime::load_data_source(state, &caller.tenant, &name).await?;
    let sync = source.spec.sync.as_ref().ok_or_else(|| {
        Refusal::not_covered(format!(
            "source '{name}' declares no `sync:` block, so there is nothing to materialize"
        ))
    })?;

    // One job per authorization class: a collection carries exactly one class,
    // so a multi-class source needs one run each. Fanning out here rather than
    // inside the worker keeps each job independently retryable.
    let classes = munarium_matrix_workers::resolve_classes(&source.spec.authorization)?;
    let mut jobs = Vec::new();
    for class in &classes {
        jobs.push(
            state
                .store
                .enqueue_sync(&caller.tenant, &name, &class.name)
                .await?,
        );
    }
    journal_outcome(
        state,
        caller,
        JournalRecord::new("schedule_sync", "ok")
            .source(&name)
            .request(request_id)
            .via(via)
            .rows(jobs.len()),
    )
    .await;
    Ok(JobAccepted {
        accepted: jobs.len(),
        jobs,
        detail: format!(
            "queued {} sync job(s) for entity '{}'",
            classes.len(),
            sync.entity.table
        ),
    })
}

/// `POST /v1/datasources/{name}/planner/ask` — ask a conversational planner
/// a question.
///
/// Two modes, and the route executes NOTHING in either. `assist` returns the
/// SQL the allowlist admitted, for the caller to run through a contract —
/// where the compiler's allowlist walk, the budget and the seal live.
/// `evaluation` records what the planner said and admits nothing, because
/// measuring a planner and trusting it are different acts.
///
/// rw, like every other act that reaches a source. The answer is journaled
/// with its refusal code, so "what did the planner propose and why was it
/// refused" is answerable after the fact.
async fn planner_ask(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(req): Json<PlannerAskRequest>,
) -> ApiResult<Json<PlannerAskResponse>> {
    let caller = auth(&state, &headers)?;
    caller.require_rw()?;
    if req.question.trim().is_empty() {
        return Err(Refusal::invalid(
            "question_required",
            "a planner needs a question: asking a model surface nothing costs money and              answers nothing",
        )
        .into());
    }
    let mode = match req.mode.as_deref().unwrap_or("assist") {
        "assist" => munarium_matrix_workers::genie::PlannerMode::PlannerAssist,
        "evaluation" => munarium_matrix_workers::genie::PlannerMode::Evaluation,
        other => {
            return Err(Refusal::invalid(
                "planner_mode_unknown",
                format!("mode must be 'assist' or 'evaluation', not '{other}'"),
            )
            .into())
        }
    };

    let wiring = crate::runtime::wire(&state, &caller.tenant, &name).await?;
    // The spec comes from the APPLIED asset, never from the request: a caller
    // naming its own space or its own allowlist would be choosing what it is
    // allowed to do.
    let spec = crate::runtime::planner_spec(&wiring.source).ok_or_else(|| {
        Refusal::not_covered(format!(
            "source '{name}' declares no planner surface; add a `genie:` block to its              connection to open one"
        ))
    })?;

    let limits = munarium_matrix_adapter::Limits {
        max_rows: wiring.source.spec.limits.max_rows,
        max_bytes: wiring.source.spec.limits.max_bytes,
        timeout_ms: wiring.source.spec.limits.statement_timeout_ms,
    };

    // A planner question SPENDS A BUDGET UNIT, exactly as an execute does.
    //
    // It is not a free read: it reaches the vendor and bills a model call
    // there, which is more expensive per question than the statement an
    // execute runs. `budgetPerHour` exists to bound what a source costs, and a
    // surface that reached outside it would be a hole in the one ceiling this
    // system has. Probe and introspect are deliberately NOT metered — they are
    // operator acts against the same connection an execute uses — and this is
    // the line between the two: it calls a model.
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
            return Err(Refusal::budget_exceeded(format!(
                "source '{}' has {remaining} of {limit} unit(s) left this hour and this                  planner question needs {requested}",
                wiring.source.metadata.name
            ))
            .into())
        }
    };

    let asked = munarium_matrix_workers::genie::ask(
        wiring.adapter.as_ref(),
        &spec,
        mode,
        &req.question,
        limits,
    )
    .await;

    // Settled when the planner was reached, released when it was not — the
    // SAME predicate the execute path uses, rather than a second list of codes
    // that would drift from it.
    //
    // Note where the allowlist refusals land: `decide` returns them inside
    // `Ok`, because by then the planner has already answered and been billed.
    // Only an error that stopped this process BEFORE the call — evaluation
    // not enabled, no planner surface, a question addressed to another space —
    // reaches the `Err` arm, and those are the ones that cost nothing. An
    // earlier draft of this keyed on the refusal code and would have refunded
    // an allowlist denial that had already paid for its answer.
    if let Some(r) = &reservation {
        let spent = asked.is_ok() || asked.as_ref().err().is_some_and(source_was_touched);
        let _ = if spent {
            state.store.settle_budget(r, None).await
        } else {
            state.store.release_budget(r).await
        };
    }
    let outcome = asked?;

    let described = outcome.describe();
    journal_outcome(
        &state,
        &caller,
        JournalRecord::new(
            "planner_ask",
            if outcome.admitted_sql.is_some() {
                "ok"
            } else {
                "refused"
            },
        )
        .source(&name)
        .request(request_id(&headers))
        .via(VIA_API),
    )
    .await;

    Ok(Json(PlannerAskResponse {
        source: name,
        mode: match mode {
            munarium_matrix_workers::genie::PlannerMode::PlannerAssist => "assist".into(),
            munarium_matrix_workers::genie::PlannerMode::Evaluation => "evaluation".into(),
        },
        pin: described["pin"].clone(),
        plan_pinned: outcome.pin.pinned,
        prose: outcome.prose,
        proposed_sql: outcome.proposed_sql,
        admitted_sql: outcome.admitted_sql,
        refusal: described["refusal"]
            .as_object()
            .map(|_| described["refusal"].clone()),
        note: described["note"].as_str().unwrap_or_default().to_string(),
    }))
}

/// `POST /v1/mappings/{name}/run` — enqueue a reconcile pass.
async fn enqueue_mapping(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<JobAccepted>> {
    let caller = auth(&state, &headers)?;
    caller.require_rw()?;
    op_enqueue_mapping(&state, &caller, &name, request_id(&headers), VIA_API)
        .await
        .map(Json)
}

/// The body `/v1` and the admin console share. See [`op_probe`].
pub(crate) async fn op_enqueue_mapping(
    state: &Arc<AppState>,
    caller: &Caller,
    name: &str,
    request_id: Option<String>,
    via: &str,
) -> ApiResult<JobAccepted> {
    // Parse it now: a mapping that does not load is an operator error worth
    // reporting at submit time.
    crate::runtime::load_asset(state, &caller.tenant, "ClaimMapping", name).await?;
    let job = state.store.enqueue_mapping(&caller.tenant, name).await?;
    journal_outcome(
        state,
        caller,
        JournalRecord::new("schedule_reconcile", "ok")
            .asset(name)
            .request(request_id)
            .via(via)
            .rows(1),
    )
    .await;
    Ok(JobAccepted {
        accepted: 1,
        jobs: vec![job],
        detail: format!("queued a reconcile pass for mapping '{name}'"),
    })
}

// ---------------------------------------------------------------------------
// promotion (mode C/authoritative)
// ---------------------------------------------------------------------------

async fn load_mapping(
    state: &AppState,
    tenant: &str,
    name: &str,
) -> ApiResult<munarium_matrix_types::ClaimMappingDoc> {
    match crate::runtime::load_asset(state, tenant, "ClaimMapping", name).await? {
        munarium_matrix_types::Asset::ClaimMapping(m) => Ok(*m),
        other => Err(Refusal::invalid(
            "wrong_kind",
            format!("'{name}' is a {}, not a ClaimMapping", other.kind()),
        )
        .into()),
    }
}

fn gates_of(state: &AppState, run: &munarium_matrix_store::MappingRunStats) -> PromotionGates {
    PromotionGates {
        identity_precision: run.identity_precision(),
        value_conformance: run.value_conformance(),
        min_identity_precision: state.config.promotion_min_identity_precision,
        min_value_conformance: state.config.promotion_min_value_conformance,
        observations: run.observations,
        run_id: Some(run.run_id.clone()),
    }
}

/// `GET /v1/mappings/{name}/promotion` — where a mapping stands against the
/// gates, whether or not it is promoted.
async fn promotion_status(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<PromotionStatus>> {
    let caller = auth(&state, &headers)?;
    op_promotion_status(&state, &caller, &name).await.map(Json)
}

/// The body `/v1` and the admin console share. A read, so it takes no `via`:
/// reads are not journaled, and journaling a console poll would bury the
/// writes an auditor came for.
pub(crate) async fn op_promotion_status(
    state: &Arc<AppState>,
    caller: &Caller,
    name: &str,
) -> ApiResult<PromotionStatus> {
    let mapping = load_mapping(state, &caller.tenant, name).await?;
    let promotion = state.store.active_promotion(&caller.tenant, name).await?;
    let run = state.store.latest_mapping_run(&caller.tenant, name).await?;
    Ok(PromotionStatus {
        mapping: mapping.metadata.asset_ref(),
        mode: format!("{:?}", mapping.spec.mode).to_lowercase(),
        promoted: promotion
            .as_ref()
            .is_some_and(|p| p.mapping_version == mapping.metadata.version as i32),
        promoted_version: promotion.as_ref().map(|p| p.mapping_version),
        decision_id: promotion.as_ref().map(|p| p.decision_id.clone()),
        promoted_at: promotion.as_ref().map(|p| p.promoted_at.clone()),
        gates: run.as_ref().map(|r| gates_of(state, r)),
        authority_scopes: mapping.spec.authority.len(),
        latest_run: run
            .as_ref()
            .map(|r| munarium_matrix_types::dto::MappingRun {
                run_id: r.run_id.clone(),
                state: r.state.clone(),
                observations: r.observations,
                discrepancies: r.discrepancies,
                ambiguous: r.ambiguous,
                findings_filed: r.findings_filed,
                proposals: r.proposals,
                ended_at: r.ended_at.clone(),
            }),
    })
}

/// `GET /v1/mappings/{name}/gate-history?limit=` — the promotion gates over
/// time, for every completed run.
///
/// The monitoring half of the owner's 2026-08-28 answer on Q8: 0.95 confirmed
/// **with monitoring**, so the threshold can be revised on evidence rather than
/// argued about. Three things this shows that a promotion row cannot:
///
/// 1. **Runs of mappings that were never promoted.** Those are the interesting
///    ones when asking whether a threshold is too strict — a mapping blocked at
///    0.94 on every run leaves no promotion row at all.
/// 2. **Margin, not pass/fail.** A run clearing 0.95 by 0.0004 and one sitting
///    at 0.999 are the same boolean and very different facts.
/// 3. **What a threshold CHANGE would have done.** `would_pass` is computed
///    against the thresholds in force now, so lowering the env var and
///    re-reading this endpoint says exactly which past runs the new number
///    admits — before anything is promoted under it.
async fn gate_history(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(q): Query<GateHistoryParams>,
    headers: HeaderMap,
) -> ApiResult<Json<GateHistory>> {
    let caller = auth(&state, &headers)?;
    let mapping = load_mapping(&state, &caller.tenant, &name).await?;
    let runs = state
        .store
        .mapping_run_history(&caller.tenant, &name, q.limit.unwrap_or(50))
        .await?;

    let min_ip = state.config.promotion_min_identity_precision;
    let min_vc = state.config.promotion_min_value_conformance;
    let entries: Vec<GateHistoryEntry> = runs
        .iter()
        .map(|r| {
            let ip = r.identity_precision();
            let vc = r.value_conformance();
            GateHistoryEntry {
                run_id: r.run_id.clone(),
                state: r.state.clone(),
                ended_at: r.ended_at.clone(),
                observations: r.observations,
                ambiguous: r.ambiguous,
                nonconforming: r.nonconforming,
                identity_precision: ip,
                value_conformance: vc,
                identity_margin: ip - min_ip,
                value_margin: vc - min_vc,
                // A run with no observations measures nothing; both ratios are
                // defined as 0.0 there, so it fails, which is the honest
                // answer — an empty run is not evidence that a mapping is safe.
                would_pass: r.observations > 0 && ip >= min_ip && vc >= min_vc,
            }
        })
        .collect();
    let passing = entries.iter().filter(|e| e.would_pass).count();
    Ok(Json(GateHistory {
        mapping: mapping.metadata.asset_ref(),
        min_identity_precision: min_ip,
        min_value_conformance: min_vc,
        runs: entries,
        passing,
    }))
}

/// `POST /v1/mappings/{name}/promote` — let a mapping write canon.
///
/// Every gate is checked HERE, at the moment of the decision, against the
/// latest completed run — not at reconcile time, where a slipping number would
/// silently turn writes off and on. The gates: the asset declares
/// `mode: authoritative` and at least one authority scope; the latest run
/// completed with observations; identity precision and value conformance
/// clear the configured minimums; and a decision id is present. A refusal
/// names the gate and the numbers.
async fn promote_mapping(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(req): Json<PromoteRequest>,
) -> ApiResult<Json<PromotionStatus>> {
    let caller = auth(&state, &headers)?;
    caller.require_rw()?;
    op_promote(&state, &caller, &name, &req, request_id(&headers), VIA_API)
        .await
        .map(Json)
}

/// The body `/v1` and the admin console share. See [`op_probe`]. Every gate is
/// checked in here, so the console cannot promote past one.
pub(crate) async fn op_promote(
    state: &Arc<AppState>,
    caller: &Caller,
    name: &str,
    req: &PromoteRequest,
    request_id: Option<String>,
    via: &str,
) -> ApiResult<PromotionStatus> {
    let name = name.to_string();
    if req.decision_id.trim().is_empty() {
        return Err(Refusal::invalid(
            "decision_required",
            "a promotion needs a decision id: the operator's record of why",
        )
        .into());
    }
    let mapping = load_mapping(state, &caller.tenant, &name).await?;
    if mapping.spec.mode != munarium_matrix_types::assets::MappingMode::Authoritative {
        return Err(Refusal::not_covered(format!(
            "mapping '{}' declares mode {:?}; only an asset that declares \
             `mode: authoritative` can be promoted — the declaration is the intent, \
             the promotion is the decision, and both are required",
            mapping.metadata.asset_ref(),
            mapping.spec.mode
        ))
        .into());
    }
    if mapping.spec.authority.is_empty() {
        return Err(Refusal::not_covered(format!(
            "mapping '{}' declares no `authority:` scopes; promoting it would authorize \
             nothing, and a promotion that authorizes nothing is a trap for the next reader",
            mapping.metadata.asset_ref()
        ))
        .into());
    }
    let run = state
        .store
        .latest_mapping_run(&caller.tenant, &name)
        .await?
        .ok_or_else(|| {
            Refusal::not_covered(format!(
                "mapping '{}' has no completed reconcile run; run it in shadow first so \
                 the gates have something to measure",
                mapping.metadata.asset_ref()
            ))
        })?;
    if run.state != "ok" || run.observations == 0 {
        return Err(Refusal::not_covered(format!(
            "the latest run ({}) ended '{}' with {} observations; promotion needs a \
             completed run that observed something",
            run.run_id, run.state, run.observations
        ))
        .into());
    }
    let gates = gates_of(state, &run);
    if gates.identity_precision < gates.min_identity_precision {
        return Err(Refusal::new(
            munarium_matrix_core::RefusalClass::NotCovered,
            "promotion_gate_identity",
            format!(
                "identity precision {:.4} is below the minimum {:.4} ({} ambiguous of {} \
                 observations in run {})",
                gates.identity_precision,
                gates.min_identity_precision,
                run.ambiguous,
                run.observations,
                run.run_id
            ),
        )
        .into());
    }
    if gates.value_conformance < gates.min_value_conformance {
        return Err(Refusal::new(
            munarium_matrix_core::RefusalClass::NotCovered,
            "promotion_gate_conformance",
            format!(
                "value conformance {:.4} is below the minimum {:.4} ({} non-conforming of \
                 {} observations in run {})",
                gates.value_conformance,
                gates.min_value_conformance,
                run.nonconforming,
                run.observations,
                run.run_id
            ),
        )
        .into());
    }
    let actor = req
        .actor
        .clone()
        .unwrap_or_else(|| format!("{}:{}", caller.tenant, caller.role));
    state
        .store
        .promote_mapping(
            &caller.tenant,
            &name,
            mapping.metadata.version as i32,
            &req.decision_id,
            &actor,
            req.reason.as_deref(),
            gates.identity_precision,
            gates.value_conformance,
        )
        .await?;
    journal_outcome(
        state,
        caller,
        JournalRecord::new("promote", "ok")
            .asset(mapping.metadata.asset_ref())
            .request(request_id)
            .actor(Some(actor))
            .via(via),
    )
    .await;
    op_promotion_status(state, caller, &name).await
}

/// `POST /v1/mappings/{name}/demote` — stop the writes. One call, effective
/// on the next reconcile poll; nothing already proposed is touched (that is
/// what rollback is for).
async fn demote_mapping(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(req): Json<DecisionRequest>,
) -> ApiResult<Json<PromotionStatus>> {
    let caller = auth(&state, &headers)?;
    caller.require_rw()?;
    op_demote(&state, &caller, &name, &req, request_id(&headers), VIA_API)
        .await
        .map(Json)
}

/// The body `/v1` and the admin console share. See [`op_probe`].
pub(crate) async fn op_demote(
    state: &Arc<AppState>,
    caller: &Caller,
    name: &str,
    req: &DecisionRequest,
    request_id: Option<String>,
    via: &str,
) -> ApiResult<PromotionStatus> {
    let name = name.to_string();
    if req.decision_id.trim().is_empty() {
        return Err(Refusal::invalid("decision_required", "a demotion needs a decision id").into());
    }
    load_mapping(state, &caller.tenant, &name).await?;
    let closed = state
        .store
        .demote_mapping(&caller.tenant, &name, &req.decision_id)
        .await?;
    if !closed {
        return Err(Refusal::not_covered(format!(
            "mapping '{name}' has no active promotion to demote"
        ))
        .into());
    }
    journal_outcome(
        state,
        caller,
        JournalRecord::new("demote", "ok")
            .asset(&name)
            .request(request_id)
            .via(via),
    )
    .await;
    op_promotion_status(state, caller, &name).await
}

/// `POST /v1/mappings/{name}/rollback` — supersede every claim this mapping
/// proposed with the value the ledger held before, under a recorded decision.
/// Append-only: history is never rewritten, and the rollback claims carry
/// `origin.kind = "rollback"` so a reviewer can see both moves.
async fn rollback_mapping(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(req): Json<DecisionRequest>,
) -> ApiResult<Json<RollbackResponse>> {
    let caller = auth(&state, &headers)?;
    caller.require_rw()?;
    if req.decision_id.trim().is_empty() {
        return Err(Refusal::invalid("decision_required", "a rollback needs a decision id").into());
    }
    let mapping = load_mapping(&state, &caller.tenant, &name).await?;
    let mapping_ref = mapping.metadata.asset_ref();
    let wiring = crate::runtime::wire(&state, &caller.tenant, &mapping.spec.source).await?;
    let rows = state
        .store
        .proposals_for_mapping(&caller.tenant, &mapping_ref)
        .await?;
    let records: Vec<munarium_matrix_workers::ProposalRecord> = rows
        .iter()
        .filter(|r| r.rolled_back_by.is_none() && r.claim_type != "correction")
        .map(crate::proposals::to_record)
        .collect();
    let ledger = crate::proposals::StoreLedger { state: &state };
    let out = munarium_matrix_workers::rollback(
        wiring.server.as_ref(),
        &munarium_matrix_workers::RollbackRequest {
            tenant: &caller.tenant,
            source_id: &wiring.source.metadata.name,
            mapping_ref: &mapping_ref,
            decision_id: &req.decision_id,
            proposals: &records,
            ledger: &ledger,
        },
    )
    .await?;
    for (original_key, rollback_claim) in &out.items {
        state
            .store
            .mark_rolled_back(&caller.tenant, original_key, rollback_claim)
            .await?;
    }
    journal_outcome(
        &state,
        &caller,
        JournalRecord::new("rollback", "ok")
            .asset(&mapping_ref)
            .request(request_id(&headers))
            .via("api")
            .rows(out.superseded as usize),
    )
    .await;
    Ok(Json(RollbackResponse {
        mapping: mapping_ref,
        decision_id: req.decision_id,
        superseded: out.superseded,
        skipped_no_prior: out.skipped_no_prior,
        already_rolled_back: out.already_rolled_back,
        disputed: out.disputed,
    }))
}

// ---------------------------------------------------------------------------
// router
// ---------------------------------------------------------------------------

pub fn router(state: Arc<AppState>) -> Router {
    let role = state.config.role;

    // Meta is served by every role: an orchestrator must be able to health-check
    // a sync worker.
    let mut app = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/version", get(version))
        .route("/openapi.json", get(openapi_json))
        .route("/docs", get(docs_page));

    // The registry lives on the control role only. A query/sync/reconcile
    // container answers 404 here — structurally, not by a guard.
    if role.serves_control() {
        app = app
            .route("/v1/assets", post(apply_asset))
            .route("/v1/assets/validate", post(validate_asset))
            .route("/v1/datasources", get(list_datasources).post(apply_asset))
            .route("/v1/datasources/{name}", get(get_datasource))
            .route("/v1/contracts", get(list_contracts).post(apply_asset))
            .route("/v1/contracts/{name}", get(get_contract))
            .route("/v1/metricviews", get(list_metric_views).post(apply_asset))
            .route("/v1/metricviews/{name}", get(get_metric_view))
            .route("/v1/dataviews", get(list_data_views).post(apply_asset))
            .route("/v1/dataviews/{name}", get(get_data_view))
            .route("/v1/mappings", get(list_mappings).post(apply_asset))
            .route("/v1/mappings/{name}", get(get_mapping))
            .route("/v1/journal", get(list_journal))
            .route("/healthdata", get(healthdata))
            // Scheduling is a control-plane act: it decides what the fleet does
            // next. The work itself runs on the sync/reconcile roles, which is
            // why these enqueue rather than execute.
            .route("/v1/datasources/{name}/probe", post(probe_source))
            .route("/v1/datasources/{name}/introspect", post(introspect_source))
            .route("/v1/datasources/{name}/sync", post(enqueue_sync))
            // On the control plane because it CONFIGURES nothing and
            // EXECUTES nothing: it asks a planner what it would do, and the
            // answer is a proposal an operator or a caller then runs through
            // a contract.
            .route("/v1/datasources/{name}/planner/ask", post(planner_ask))
            .route("/v1/mappings/{name}/run", post(enqueue_mapping))
            // The decision to write canon is a control-plane act.
            .route("/v1/mappings/{name}/promotion", get(promotion_status))
            .route("/v1/mappings/{name}/gate-history", get(gate_history))
            .route("/v1/mappings/{name}/promote", post(promote_mapping))
            .route("/v1/mappings/{name}/demote", post(demote_mapping))
            .route("/v1/mappings/{name}/rollback", post(rollback_mapping));

        // Mounted only here, and only when enabled: a query, sync or
        // reconcile container 404s on /admin because the routes are ABSENT,
        // not because a guard turned them down.
        if state.config.admin_enabled {
            app = app.merge(crate::admin::routes());
        }
    }

    // The query plane is the turn path and lives on the query role. A control
    // container does not serve it unless it is also `all` — the two have
    // different scaling shapes and a long registry call must not sit behind a
    // deadline-bounded execute.
    if role.serves_query() {
        app = app
            // The MCP toolset, on the query plane beside /v1 —
            // the same tokens, the same budget, the same seal, `via: mcp`.
            .route("/mcp", post(crate::mcp::handle))
            .route("/v1/contracts/{name}/execute", post(execute_contract))
            .route("/v1/contracts/{name}/verify", post(verify_contract))
            // Metric views: the same execute handler — the
            // intent's kind selects the semantic path — and a verify that
            // records the definition fingerprint the questions passed under.
            .route("/v1/metricviews/{name}/execute", post(execute_contract))
            .route("/v1/metricviews/{name}/verify", post(verify_metric_view))
            .route("/v1/dataviews/{name}/execute", post(execute_contract))
            .route("/v1/dataviews/{name}/verify", post(verify_data_view));
    }

    app.with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AuthMode, Config, Role};
    use munarium_matrix_store::MatrixStore;
    use tower::ServiceExt;

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

    async fn get(state: Arc<AppState>, path: &str) -> (StatusCode, String) {
        let resp = router(state)
            .oneshot(
                axum::http::Request::get(path)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[tokio::test]
    async fn meta_routes_answer_on_every_role() {
        for role in [
            Role::Control,
            Role::Query,
            Role::Sync,
            Role::Reconcile,
            Role::All,
        ] {
            let (status, body) = get(state_with_role(role), "/healthz").await;
            assert_eq!(status, StatusCode::OK, "{role:?}");
            assert!(body.contains("\"ok\":true"));

            let (status, body) = get(state_with_role(role), "/version").await;
            assert_eq!(status, StatusCode::OK);
            assert!(
                body.contains(role.as_str()),
                "{role:?} version must name its role: {body}"
            );
        }
    }

    #[tokio::test]
    async fn the_registry_is_mounted_only_on_the_control_role() {
        // A sync container is not a half-broken control container: the route
        // does not exist at all.
        for role in [Role::Query, Role::Sync, Role::Reconcile] {
            let (status, _) = get(state_with_role(role), "/v1/datasources").await;
            assert_eq!(
                status,
                StatusCode::NOT_FOUND,
                "{role:?} must not serve the registry"
            );
        }
    }

    #[tokio::test]
    async fn validation_is_reachable_without_a_database() {
        let bad = "apiVersion: munarium.ioka.io/v1\nkind: DataSource\nmetadata: { name: x, version: 1 }\nspec:\n  adapter: postgres\n";
        let resp = router(state_with_role(Role::All))
            .oneshot(
                axum::http::Request::post("/v1/assets/validate")
                    .header("content-type", "text/yaml")
                    .body(axum::body::Body::from(bad))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["valid"], false);
        // Egress default-deny is the finding an empty allowlist must produce.
        let codes: Vec<&str> = v["findings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["code"].as_str().unwrap())
            .collect();
        assert!(codes.contains(&"egress.empty-allowlist"), "{codes:?}");
    }

    #[tokio::test]
    async fn the_query_plane_is_mounted_only_where_query_work_belongs() {
        // A sync container must not serve the turn path, and a control
        // container must not either: they have different scaling shapes, and a
        // long registry call sitting behind a deadline-bounded execute is
        // exactly the interference the role split exists to prevent.
        for role in [Role::Sync, Role::Reconcile, Role::Control] {
            let resp = router(state_with_role(role))
                .oneshot(
                    axum::http::Request::post("/v1/contracts/x/verify")
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::NOT_FOUND,
                "{role:?} must not serve the query plane"
            );
        }
        // `all` is the laptop: it serves everything. Probed with a GET, which
        // axum answers 405 from the routing table WITHOUT entering the handler
        // — the handler would reach for a database this test does not have and
        // would spend the pool's connect timeout proving nothing.
        let resp = router(state_with_role(Role::All))
            .oneshot(
                axum::http::Request::get("/v1/contracts/x/verify")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "405 means the route exists on this role; 404 would mean it does not"
        );
    }

    #[tokio::test]
    async fn scheduling_is_a_control_plane_act() {
        // Enqueueing decides what the fleet does next, so it lives with the
        // registry — not on the worker that happens to run it.
        for role in [Role::Query, Role::Sync, Role::Reconcile] {
            let resp = router(state_with_role(role))
                .oneshot(
                    axum::http::Request::post("/v1/datasources/crm/sync")
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::NOT_FOUND,
                "{role:?} must not schedule work"
            );
        }
    }

    /// The planner route is a CONTROL-plane act: it configures nothing and
    /// executes nothing, and it must not appear on a worker container.
    ///
    /// Probed with a GET, which axum answers 405 from the routing table
    /// WITHOUT entering the handler — the handler reaches for a database this
    /// test does not have, and would spend the pool's connect timeout proving
    /// nothing. The same trick the verify-route test uses.
    #[tokio::test]
    async fn the_planner_route_is_control_plane_only() {
        for role in [Role::Query, Role::Sync, Role::Reconcile] {
            let resp = router(state_with_role(role))
                .oneshot(
                    axum::http::Request::get("/v1/datasources/x/planner/ask")
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::NOT_FOUND,
                "{role:?} must not serve the planner route"
            );
        }
        for role in [Role::Control, Role::All] {
            let resp = router(state_with_role(role))
                .oneshot(
                    axum::http::Request::get("/v1/datasources/x/planner/ask")
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "{role:?} must serve it (POST only)"
            );
        }
    }

    /// A planner mode the vocabulary does not contain is refused BY NAME,
    /// before anything is wired — so a typo is an answer rather than a
    /// connection attempt against a source.
    #[tokio::test]
    async fn an_unknown_planner_mode_is_refused_before_the_source_is_touched() {
        let resp = router(state_with_role(Role::All))
            .oneshot(
                axum::http::Request::post("/v1/datasources/x/planner/ask")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        r#"{"question":"how much pipeline?","mode":"whatever"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        // 400, not 422: the refusal CLASS decides the status, and a mode the
        // vocabulary does not contain is an invalid REQUEST, not an
        // uncoverable one.
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8_lossy(&bytes);
        assert!(body.contains("'assist' or 'evaluation'"), "{body}");
    }

    /// An empty question is refused too, and for the same reason: asking a
    /// model surface nothing costs money and answers nothing.
    #[tokio::test]
    async fn an_empty_planner_question_is_refused() {
        let resp = router(state_with_role(Role::All))
            .oneshot(
                axum::http::Request::post("/v1/datasources/x/planner/ask")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"question":"   "}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn a_session_must_dominate_a_class_to_read_it() {
        use munarium_matrix_types::contract::AuthorizationSnapshot;
        let classes = vec![
            munarium_matrix_workers::ResolvedClass {
                name: "open".into(),
                access_level: 1,
                compartments: vec![],
                credential_ref: None,
            },
            munarium_matrix_workers::ResolvedClass {
                name: "legal".into(),
                access_level: 5,
                compartments: vec!["legal".into()],
                credential_ref: Some("k".into()),
            },
        ];
        let snap = |level: i32, comps: &[&str]| AuthorizationSnapshot {
            tenant: "acme".into(),
            uid: None,
            access_level: level,
            compartments: comps.iter().map(|s| s.to_string()).collect(),
            session_id: None,
            runbook_ref: None,
        };

        // Level alone is not enough: the compartment is a conjunction.
        assert_eq!(
            class_for_intent(&classes, &snap(9, &[])).unwrap().name,
            "open",
            "a high level without the compartment must not reach the legal class"
        );
        // With both, the MOST privileged dominated class wins — a caller
        // cleared for more should not silently get the narrower view.
        assert_eq!(
            class_for_intent(&classes, &snap(9, &["legal"]))
                .unwrap()
                .name,
            "legal"
        );
        // Dominating nothing is a refusal, not an empty result.
        let err = class_for_intent(&classes, &snap(0, &[])).expect_err("must refuse");
        assert_eq!(err.class, munarium_matrix_core::RefusalClass::Denied);
        assert!(
            !err.message.contains("legal"),
            "the refusal names the requirement, never what it would have revealed: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn a_refusal_becomes_problem_json_with_its_class_status() {
        use munarium_matrix_core::Refusal;
        let denied: ApiError = Refusal::policy_denied("no").into();
        assert_eq!(denied.0.status, 403);
        assert_eq!(denied.0.slug(), "policy-denied");
        assert!(
            denied.0.refusal.is_some(),
            "the typed refusal travels in the body"
        );

        let gone: ApiError = Refusal::source_unavailable("down").into();
        assert_eq!(gone.0.status, 503);

        let over: ApiError = Refusal::budget_exceeded("no budget").into();
        assert_eq!(over.0.status, 429);
    }
}
