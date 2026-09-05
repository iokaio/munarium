// SPDX-License-Identifier: Apache-2.0
//! Guided runbook-set authoring: the REST plane over `munarium-authoring`.
//!
//! A draft is a server-side workspace (pg `authoring_drafts` table) holding
//! interview answers plus the materialized shape+runbook document set. The
//! flow: create (optionally from a §19 pattern) → answer the §16-ordered
//! interview (documents re-materialize deterministically) → optionally an
//! AI assist pass → validate (per-document + set-level) → export a
//! hash-manifested bundle that mmctl/CI applies to production through
//! the EXISTING /v1/shapes + /v1/runbooks routes → or apply in place.
//!
//! Postures worth stating:
//! - Stored `state`/`findings` are a progress display; export and apply
//!   always re-validate inline — a snapshot can be stale relative to a
//!   later edit, and revalidation is milliseconds.
//! - The assist pass resolves its model from the REQUEST (provider
//!   defaulting to the tenant chain), not through `models::resolve_model`:
//!   a draft has no published runbook whose `allowOverrides` policy could
//!   meaningfully gate the call — the draft's own YAML authorizing
//!   overrides of itself would be pure self-authorization. Cost control is
//!   already tenant-side in `op_complete` (budget, metering, provenance).
//! - Assist NEVER fails the request: no provider, budget exhaustion, or a
//!   parse failure degrade to `assist_note` on a 200, so the surface works
//!   identically on a keyless deployment.

use crate::error::ApiError;
use crate::runbooks_api::{pool, uuid_suffix};
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use munarium_api_types as dto;
use munarium_authoring::{bundle, catalog, interview, materialize, setcheck};
use munarium_core::{KernelError, Result};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

type ApiResult<T> = std::result::Result<T, ApiError>;

fn rest_auth(state: &AppState, headers: &HeaderMap) -> ApiResult<crate::state::TenantCtx> {
    crate::rest::auth_ctx(state, headers)
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// ---------------------------------------------------------------------------
// dto conversions
// ---------------------------------------------------------------------------

fn pattern_summary(p: &catalog::PatternEntry) -> dto::PatternSummaryDto {
    dto::PatternSummaryDto {
        id: p.id.into(),
        name: p.name.into(),
        description: p.description.into(),
        start_from: p.start_from.into(),
        guidance: p.guidance.into(),
        has_completion: p.has_completion,
    }
}

fn authoring_finding_dto(f: &munarium_authoring::Finding) -> dto::ValidationFindingDto {
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

fn interview_dto(sections: Vec<interview::Section>) -> Vec<dto::InterviewSectionDto> {
    sections
        .into_iter()
        .map(|s| dto::InterviewSectionDto {
            id: s.id.into(),
            title: s.title.into(),
            doc_ref: s.doc_ref.into(),
            questions: s
                .questions
                .into_iter()
                .map(|q| dto::InterviewQuestionDto {
                    id: q.id.into(),
                    prompt: q.prompt.into(),
                    guidance: q.guidance.into(),
                    kind: q.kind.into(),
                    required: q.required,
                    default: q.default,
                    choices: q.choices.into_iter().map(String::from).collect(),
                    maps_to: q.maps_to.into(),
                })
                .collect(),
        })
        .collect()
}

/// Parsed kind, as a display/routing string. Unknown maps to "Runbook" so a
/// (should-be-impossible) malformed draft document routes to the parser
/// with the better error message at apply time.
fn doc_kind(yaml: &str) -> &'static str {
    match munarium_authoring::doc_kind(yaml) {
        munarium_authoring::DocKind::Shape => "Shape",
        _ => "Runbook",
    }
}

fn document_dtos(docs: &BTreeMap<String, String>) -> Vec<dto::DraftDocumentDto> {
    docs.iter()
        .map(|(path, yaml)| dto::DraftDocumentDto {
            path: path.clone(),
            kind: doc_kind(yaml).into(),
            yaml: yaml.clone(),
            sha256: munarium_authoring::sha256_hex(yaml.as_bytes()),
        })
        .collect()
}

/// Every published (shape_ref -> yaml_hash) for the tenant — the set
/// checks' additive-versioning preflight input.
async fn published_shape_hashes(state: &AppState, tenant: &str) -> Result<HashMap<String, String>> {
    state.ensure_shapes_loaded(tenant).await?;
    Ok(state.shapes.list(tenant).into_iter().collect())
}

fn validation_dto(v: setcheck::SetValidation, todos: Vec<String>) -> dto::DraftValidationResponse {
    dto::DraftValidationResponse {
        valid: v.valid,
        documents: v
            .per_doc
            .iter()
            .map(|(path, findings)| dto::DocumentFindingsDto {
                path: path.clone(),
                findings: findings.iter().map(authoring_finding_dto).collect(),
            })
            .collect(),
        set_findings: v.set.iter().map(authoring_finding_dto).collect(),
        todos,
    }
}

async fn validate_documents(
    state: &AppState,
    tenant: &str,
    docs: &BTreeMap<String, String>,
    todos: Vec<String>,
) -> Result<dto::DraftValidationResponse> {
    let published = published_shape_hashes(state, tenant).await?;
    Ok(validation_dto(
        setcheck::validate_set(docs, &published),
        todos,
    ))
}

// ---------------------------------------------------------------------------
// draft persistence
// ---------------------------------------------------------------------------

struct DraftRow {
    draft_id: String,
    name: String,
    pattern_id: Option<String>,
    state: String,
    answers: serde_json::Value,
    documents: BTreeMap<String, String>,
    findings: Option<serde_json::Value>,
    assist_note: Option<String>,
    created_by: String,
    created_at: String,
    updated_at: String,
}

impl DraftRow {
    fn pattern(&self) -> Option<&'static catalog::PatternEntry> {
        self.pattern_id.as_deref().and_then(catalog::pattern)
    }
}

/// The raw `authoring_drafts` row tuple: (name, pattern_id, state, answers,
/// documents, findings, assist_note, created_by, created_at, updated_at).
type DraftTuple = (
    String,
    Option<String>,
    String,
    serde_json::Value,
    serde_json::Value,
    Option<serde_json::Value>,
    Option<String>,
    String,
    String,
    String,
);

async fn load_draft(state: &AppState, tenant: &str, draft_id: &str) -> Result<DraftRow> {
    let row: Option<DraftTuple> = sqlx::query_as(
        "SELECT name, pattern_id, state, answers, documents, findings, assist_note,
                created_by, created_at::text, updated_at::text
           FROM authoring_drafts
          WHERE tenant_id = $1 AND draft_id = $2 AND status = 'active'",
    )
    .bind(tenant)
    .bind(draft_id)
    .fetch_optional(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    let (
        name,
        pattern_id,
        draft_state,
        answers,
        documents,
        findings,
        assist_note,
        created_by,
        created_at,
        updated_at,
    ) = row.ok_or_else(|| KernelError::NotFound {
        kind: "draft",
        id: draft_id.to_string(),
    })?;
    Ok(DraftRow {
        draft_id: draft_id.to_string(),
        name,
        pattern_id,
        state: draft_state,
        answers,
        documents: docs_from_json(&documents),
        findings,
        assist_note,
        created_by,
        created_at,
        updated_at,
    })
}

fn docs_from_json(v: &serde_json::Value) -> BTreeMap<String, String> {
    v.as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

fn docs_to_json(docs: &BTreeMap<String, String>) -> serde_json::Value {
    serde_json::Value::Object(
        docs.iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect(),
    )
}

async fn store_draft_update(
    state: &AppState,
    tenant: &str,
    draft_id: &str,
    draft_state: &str,
    answers: Option<serde_json::Value>,
    documents: Option<&BTreeMap<String, String>>,
    findings: Option<serde_json::Value>,
    assist_note: Option<Option<&str>>,
) -> Result<()> {
    sqlx::query(
        "UPDATE authoring_drafts
            SET state = $3,
                answers = COALESCE($4, answers),
                documents = COALESCE($5, documents),
                findings = COALESCE($6, findings),
                assist_note = CASE WHEN $7 THEN $8 ELSE assist_note END,
                updated_at = now()
          WHERE tenant_id = $1 AND draft_id = $2 AND status = 'active'",
    )
    .bind(tenant)
    .bind(draft_id)
    .bind(draft_state)
    .bind(answers)
    .bind(documents.map(docs_to_json))
    .bind(findings)
    .bind(assist_note.is_some())
    .bind(assist_note.flatten())
    .execute(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    Ok(())
}

fn draft_response(
    row: DraftRow,
    validation: Option<dto::DraftValidationResponse>,
) -> dto::DraftResponse {
    let todos = validation
        .as_ref()
        .map(|v| v.todos.clone())
        .unwrap_or_default();
    dto::DraftResponse {
        draft_id: row.draft_id.clone(),
        name: row.name.clone(),
        state: row.state.clone(),
        pattern_id: row.pattern_id.clone(),
        answers: row.answers.clone(),
        interview: interview_dto(interview::interview(row.pattern())),
        documents: document_dtos(&row.documents),
        validation,
        todos,
        assist_note: row.assist_note.clone(),
        created_by: row.created_by.clone(),
        created_at: row.created_at.clone(),
        updated_at: row.updated_at.clone(),
    }
}

fn stored_validation(row: &DraftRow) -> Option<dto::DraftValidationResponse> {
    row.findings
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
}

fn valid_draft_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some('a'..='z') | Some('0'..='9'))
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

// ---------------------------------------------------------------------------
// op functions (shared by REST now, /admin later)
// ---------------------------------------------------------------------------

pub(crate) async fn op_create_draft(
    state: &AppState,
    tenant: &str,
    created_by: &str,
    req: &dto::CreateDraftRequest,
) -> Result<DraftRowResponse> {
    if !valid_draft_name(&req.name) {
        return Err(KernelError::InvalidInput(format!(
            "draft name '{}' must match ^[a-z0-9][a-z0-9-]*$ ('@' is the ref separator)",
            req.name
        )));
    }
    let pattern = match &req.pattern_id {
        Some(id) => Some(catalog::pattern(id).ok_or_else(|| {
            KernelError::InvalidInput(format!(
                "unknown pattern '{id}' (known: {})",
                catalog::patterns()
                    .iter()
                    .map(|p| p.id)
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?),
        None => None,
    };
    let (documents, draft_state) = if req.seed_from_exemplar {
        let pattern = pattern.ok_or_else(|| {
            KernelError::InvalidInput("seed_from_exemplar requires pattern_id".into())
        })?;
        (
            materialize::seed_documents(&req.name, pattern).map_err(KernelError::InvalidInput)?,
            "drafted",
        )
    } else {
        (BTreeMap::new(), "interview")
    };
    let draft_id = format!("draft-{}", uuid_suffix());
    sqlx::query(
        "INSERT INTO authoring_drafts
             (tenant_id, draft_id, name, pattern_id, state, documents, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(tenant)
    .bind(&draft_id)
    .bind(&req.name)
    .bind(&req.pattern_id)
    .bind(draft_state)
    .bind(docs_to_json(&documents))
    .bind(created_by)
    .execute(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    Ok(DraftRowResponse { draft_id })
}

pub(crate) struct DraftRowResponse {
    pub draft_id: String,
}

pub(crate) async fn op_update_answers(
    state: &AppState,
    tenant: &str,
    draft_id: &str,
    req: &dto::UpdateAnswersRequest,
) -> Result<dto::DraftResponse> {
    let row = load_draft(state, tenant, draft_id).await?;
    if !req.answers.is_object() {
        return Err(KernelError::InvalidInput(
            "answers must be a JSON object keyed by interview question id".into(),
        ));
    }
    // The identity.pattern answer is REAL: it (re)binds the draft's pattern,
    // exactly as if it had been chosen at creation — a wizard question whose
    // answer changed nothing would be a lie.
    let answered_pattern = req
        .answers
        .get("identity.pattern")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let pattern = match answered_pattern {
        Some(id) => Some(catalog::pattern(id).ok_or_else(|| {
            KernelError::InvalidInput(format!(
                "identity.pattern: unknown pattern '{id}' (known: {})",
                catalog::patterns()
                    .iter()
                    .map(|p| p.id)
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?),
        None => row.pattern(),
    };
    if answered_pattern.is_some() && answered_pattern != row.pattern_id.as_deref() {
        sqlx::query(
            "UPDATE authoring_drafts SET pattern_id = $3, updated_at = now()
              WHERE tenant_id = $1 AND draft_id = $2 AND status = 'active'",
        )
        .bind(tenant)
        .bind(draft_id)
        .bind(answered_pattern)
        .execute(pool(state)?)
        .await
        .map_err(|e| KernelError::Storage(e.to_string()))?;
    }
    let (documents, validation, draft_state) = if req.materialize {
        let m = materialize::build_documents(&row.name, pattern, &req.answers)
            .map_err(KernelError::InvalidInput)?;
        let validation = validate_documents(state, tenant, &m.documents, m.todos).await?;
        (Some(m.documents), Some(validation), "drafted".to_string())
    } else {
        (None, None, row.state.clone())
    };
    let findings_json = validation
        .as_ref()
        .map(|v| serde_json::to_value(v).unwrap_or_default());
    store_draft_update(
        state,
        tenant,
        draft_id,
        &draft_state,
        Some(req.answers.clone()),
        documents.as_ref(),
        findings_json,
        None,
    )
    .await?;
    let row = load_draft(state, tenant, draft_id).await?;
    Ok(draft_response(row, validation))
}

pub(crate) async fn op_validate_draft(
    state: &AppState,
    tenant: &str,
    draft_id: &str,
) -> Result<dto::DraftValidationResponse> {
    let row = load_draft(state, tenant, draft_id).await?;
    // Todos come from materialization; validation alone cannot compute them,
    // so carry the stored ones forward.
    let todos = stored_validation(&row).map(|v| v.todos).unwrap_or_default();
    let validation = validate_documents(state, tenant, &row.documents, todos).await?;
    // The badge tracks the verdict both ways: a previously-'validated'
    // draft whose findings turned red (e.g. a shape published elsewhere now
    // conflicts) must not keep wearing the badge.
    let draft_state = if row.documents.is_empty() {
        row.state.as_str()
    } else if validation.valid {
        "validated"
    } else {
        "drafted"
    };
    store_draft_update(
        state,
        tenant,
        draft_id,
        draft_state,
        None,
        None,
        Some(serde_json::to_value(&validation).unwrap_or_default()),
        None,
    )
    .await?;
    Ok(validation)
}

/// Re-validate inline and refuse on error findings — stored state is never
/// the gate.
async fn require_exportable(
    state: &AppState,
    tenant: &str,
    row: &DraftRow,
) -> ApiResult<dto::DraftValidationResponse> {
    let todos = stored_validation(row).map(|v| v.todos).unwrap_or_default();
    let validation = validate_documents(state, tenant, &row.documents, todos).await?;
    if !validation.valid {
        let codes: Vec<String> = validation
            .documents
            .iter()
            .flat_map(|d| d.findings.iter())
            .chain(validation.set_findings.iter())
            .filter(|f| f.severity == "error")
            .map(|f| f.code.clone())
            .collect();
        return Err(ApiError::Custom(
            crate::error::CustomError::authoring_draft_invalid(format!(
                "draft '{}' has error findings: {}",
                row.draft_id,
                codes.join(", ")
            )),
        ));
    }
    Ok(validation)
}

fn bundle_dto(b: bundle::Bundle) -> dto::ExportDraftResponse {
    dto::ExportDraftResponse {
        kind: b.kind,
        api_version: b.api_version,
        tool: dto::BundleToolDto {
            name: b.tool.name,
            version: b.tool.version,
        },
        draft_id: b.draft_id,
        name: b.name,
        created_at: b.created_at,
        files: b.files,
        hashes: b.hashes,
        apply_order: b.apply_order,
        manifest_hash: b.manifest_hash,
        validation: dto::BundleValidationDto {
            valid: b.validation.valid,
            errors: b.validation.errors as u64,
            warns: b.validation.warns as u64,
            infos: b.validation.infos as u64,
        },
    }
}

fn count_severity(v: &dto::DraftValidationResponse, severity: &str) -> usize {
    v.documents
        .iter()
        .flat_map(|d| d.findings.iter())
        .chain(v.set_findings.iter())
        .filter(|f| f.severity == severity)
        .count()
}

// ---------------------------------------------------------------------------
// REST handlers
// ---------------------------------------------------------------------------

/// GET /v1/authoring/patterns — the pattern catalog.
#[utoipa::path(get, path = "/v1/authoring/patterns",
    responses((status = 200, body = dto::PatternsResponse)), tag = "authoring")]
pub async fn list_patterns(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::PatternsResponse>> {
    rest_auth(&state, &headers)?;
    Ok(Json(dto::PatternsResponse {
        patterns: catalog::patterns().iter().map(pattern_summary).collect(),
    }))
}

/// GET /v1/authoring/patterns/{id} — one pattern with its exemplar YAML.
#[utoipa::path(get, path = "/v1/authoring/patterns/{id}",
    params(("id" = String, Path, description = "pattern id, e.g. ask-the-corpus")),
    responses((status = 200, body = dto::PatternDetailResponse),
              (status = 404, description = "unknown pattern")), tag = "authoring")]
pub async fn get_pattern(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::PatternDetailResponse>> {
    rest_auth(&state, &headers)?;
    let p = catalog::pattern(&id).ok_or(KernelError::NotFound {
        kind: "pattern",
        id: id.clone(),
    })?;
    Ok(Json(dto::PatternDetailResponse {
        id: p.id.into(),
        name: p.name.into(),
        description: p.description.into(),
        start_from: p.start_from.into(),
        guidance: p.guidance.into(),
        has_completion: p.has_completion,
        decision_notes: p.decision_notes.iter().map(|s| s.to_string()).collect(),
        runbook_yaml: catalog::exemplar_runbook(p.start_from)
            .unwrap_or_default()
            .to_string(),
        shapes: p
            .shape_names
            .iter()
            .filter_map(|n| {
                catalog::exemplar_shape(n).map(|y| dto::NamedYamlDto {
                    name: n.to_string(),
                    yaml: y.to_string(),
                })
            })
            .collect(),
    }))
}

/// POST /v1/authoring/drafts — create a draft workspace. rw role;
/// postgres store only.
#[utoipa::path(post, path = "/v1/authoring/drafts",
    request_body = dto::CreateDraftRequest,
    responses((status = 200, body = dto::DraftResponse)), tag = "authoring")]
pub async fn create_draft(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uid: Option<axum::Extension<crate::middleware::Uid>>,
    crate::rest::ProblemJson(req): crate::rest::ProblemJson<dto::CreateDraftRequest>,
) -> ApiResult<Json<dto::DraftResponse>> {
    let ctx = rest_auth(&state, &headers)?;
    ctx.require_rw()?;
    let created_by = crate::middleware::uid_or_anonymous(uid.as_ref());
    let created = op_create_draft(&state, &ctx.tenant_id, &created_by, &req).await?;
    let row = load_draft(&state, &ctx.tenant_id, &created.draft_id).await?;
    Ok(Json(draft_response(row, None)))
}

/// GET /v1/authoring/drafts — active drafts, newest first.
#[utoipa::path(get, path = "/v1/authoring/drafts",
    responses((status = 200, body = dto::DraftsResponse)), tag = "authoring")]
pub async fn list_drafts(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::DraftsResponse>> {
    let ctx = rest_auth(&state, &headers)?;
    Ok(Json(dto::DraftsResponse {
        drafts: op_list_drafts(&state, &ctx.tenant_id).await?,
    }))
}

pub(crate) async fn op_list_drafts(
    state: &AppState,
    tenant: &str,
) -> Result<Vec<dto::DraftSummaryDto>> {
    let rows: Vec<(String, String, String, Option<String>, String, String)> = sqlx::query_as(
        "SELECT draft_id, name, state, pattern_id, created_by, updated_at::text
           FROM authoring_drafts
          WHERE tenant_id = $1 AND status = 'active'
          ORDER BY updated_at DESC",
    )
    .bind(tenant)
    .fetch_all(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    Ok(rows
        .into_iter()
        .map(
            |(draft_id, name, state, pattern_id, created_by, updated_at)| dto::DraftSummaryDto {
                draft_id,
                name,
                state,
                pattern_id,
                created_by,
                updated_at,
            },
        )
        .collect())
}

pub(crate) async fn op_get_draft(
    state: &AppState,
    tenant: &str,
    draft_id: &str,
) -> Result<dto::DraftResponse> {
    let row = load_draft(state, tenant, draft_id).await?;
    let validation = stored_validation(&row);
    Ok(draft_response(row, validation))
}

/// GET /v1/authoring/drafts/{draft_id} — the draft with its interview,
/// documents, and last validation snapshot.
#[utoipa::path(get, path = "/v1/authoring/drafts/{draft_id}",
    params(("draft_id" = String, Path)),
    responses((status = 200, body = dto::DraftResponse),
              (status = 404, description = "unknown draft")), tag = "authoring")]
pub async fn get_draft(
    State(state): State<Arc<AppState>>,
    Path(draft_id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::DraftResponse>> {
    let ctx = rest_auth(&state, &headers)?;
    Ok(Json(op_get_draft(&state, &ctx.tenant_id, &draft_id).await?))
}

/// DELETE /v1/authoring/drafts/{draft_id} — soft delete (the row is kept).
#[utoipa::path(delete, path = "/v1/authoring/drafts/{draft_id}",
    params(("draft_id" = String, Path)),
    responses((status = 200, body = dto::DraftDeleteResponse),
              (status = 404, description = "unknown draft")), tag = "authoring")]
pub async fn delete_draft(
    State(state): State<Arc<AppState>>,
    Path(draft_id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::DraftDeleteResponse>> {
    let ctx = rest_auth(&state, &headers)?;
    ctx.require_rw()?;
    let updated = sqlx::query(
        "UPDATE authoring_drafts SET status = 'deleted', updated_at = now()
          WHERE tenant_id = $1 AND draft_id = $2 AND status = 'active'",
    )
    .bind(&ctx.tenant_id)
    .bind(&draft_id)
    .execute(pool(&state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    if updated.rows_affected() == 0 {
        return Err(KernelError::NotFound {
            kind: "draft",
            id: draft_id,
        }
        .into());
    }
    Ok(Json(dto::DraftDeleteResponse {
        draft_id,
        status: "deleted".into(),
    }))
}

/// PUT /v1/authoring/drafts/{draft_id}/answers — store answers and (by
/// default) re-materialize + validate the document set.
#[utoipa::path(put, path = "/v1/authoring/drafts/{draft_id}/answers",
    params(("draft_id" = String, Path)),
    request_body = dto::UpdateAnswersRequest,
    responses((status = 200, body = dto::DraftResponse)), tag = "authoring")]
pub async fn update_answers(
    State(state): State<Arc<AppState>>,
    Path(draft_id): Path<String>,
    headers: HeaderMap,
    crate::rest::ProblemJson(req): crate::rest::ProblemJson<dto::UpdateAnswersRequest>,
) -> ApiResult<Json<dto::DraftResponse>> {
    let ctx = rest_auth(&state, &headers)?;
    ctx.require_rw()?;
    Ok(Json(
        op_update_answers(&state, &ctx.tenant_id, &draft_id, &req).await?,
    ))
}

/// POST /v1/authoring/drafts/{draft_id}/validate — per-document + set-level
/// validation of the current documents.
#[utoipa::path(post, path = "/v1/authoring/drafts/{draft_id}/validate",
    params(("draft_id" = String, Path)),
    responses((status = 200, body = dto::DraftValidationResponse)), tag = "authoring")]
pub async fn validate_draft(
    State(state): State<Arc<AppState>>,
    Path(draft_id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::DraftValidationResponse>> {
    let ctx = rest_auth(&state, &headers)?;
    ctx.require_rw()?;
    Ok(Json(
        op_validate_draft(&state, &ctx.tenant_id, &draft_id).await?,
    ))
}

/// POST /v1/authoring/drafts/{draft_id}/export — the hash-manifested bundle.
/// Refuses (409 authoring-draft-invalid) while error findings exist.
#[utoipa::path(post, path = "/v1/authoring/drafts/{draft_id}/export",
    params(("draft_id" = String, Path)),
    responses((status = 200, body = dto::ExportDraftResponse),
              (status = 409, description = "draft has error findings")), tag = "authoring")]
pub async fn export_draft(
    State(state): State<Arc<AppState>>,
    Path(draft_id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::ExportDraftResponse>> {
    let ctx = rest_auth(&state, &headers)?;
    ctx.require_rw()?;
    Ok(Json(
        op_export_draft(&state, &ctx.tenant_id, &draft_id).await?,
    ))
}

pub(crate) async fn op_export_draft(
    state: &AppState,
    tenant: &str,
    draft_id: &str,
) -> ApiResult<dto::ExportDraftResponse> {
    let row = load_draft(state, tenant, draft_id).await?;
    if row.documents.is_empty() {
        return Err(ApiError::Custom(
            crate::error::CustomError::authoring_draft_invalid(format!(
                "draft '{draft_id}' has no documents — answer the interview or seed from a pattern first"
            )),
        ));
    }
    let validation = require_exportable(state, tenant, &row).await?;
    let summary = bundle::ValidationSummary {
        valid: validation.valid,
        errors: count_severity(&validation, "error"),
        warns: count_severity(&validation, "warn"),
        infos: count_severity(&validation, "info"),
    };
    let b = bundle::build_bundle(
        &row.draft_id,
        &row.name,
        &now_rfc3339(),
        env!("CARGO_PKG_VERSION"),
        &row.documents,
        summary,
    );
    store_draft_update(
        state,
        tenant,
        draft_id,
        "exported",
        None,
        None,
        Some(serde_json::to_value(&validation).unwrap_or_default()),
        None,
    )
    .await?;
    Ok(bundle_dto(b))
}

/// POST /v1/authoring/drafts/{draft_id}/apply — apply the set to THIS
/// server, shapes first (so collection materialization can never hit an
/// unpublished shape). Re-validates inline; refuses on error findings.
#[utoipa::path(post, path = "/v1/authoring/drafts/{draft_id}/apply",
    params(("draft_id" = String, Path)),
    responses((status = 200, body = dto::ApplyDraftResponse),
              (status = 409, description = "draft has error findings")), tag = "authoring")]
pub async fn apply_draft(
    State(state): State<Arc<AppState>>,
    Path(draft_id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::ApplyDraftResponse>> {
    let ctx = rest_auth(&state, &headers)?;
    ctx.require_rw()?;
    let row = load_draft(&state, &ctx.tenant_id, &draft_id).await?;
    if row.documents.is_empty() {
        return Err(ApiError::Custom(
            crate::error::CustomError::authoring_draft_invalid(format!(
                "draft '{draft_id}' has no documents to apply"
            )),
        ));
    }
    require_exportable(&state, &ctx.tenant_id, &row).await?;
    let mut applied = Vec::new();
    // Shapes first, then runbooks — the bundle's apply_order contract.
    let ordered = row
        .documents
        .iter()
        .filter(|(_, y)| doc_kind(y) == "Shape")
        .chain(row.documents.iter().filter(|(_, y)| doc_kind(y) != "Shape"));
    for (path, yaml) in ordered {
        if doc_kind(yaml) == "Shape" {
            let resp =
                crate::runbooks_api::op_apply_shape(&state, &ctx.tenant_id, yaml, None, None)
                    .await?;
            applied.push(dto::AppliedDocDto {
                path: path.clone(),
                kind: "Shape".into(),
                r#ref: resp.shape_ref,
                yaml_hash: resp.yaml_hash,
            });
        } else {
            let runbook_ref =
                crate::runbooks_api::op_apply_runbook(&state, &ctx.tenant_id, yaml).await?;
            applied.push(dto::AppliedDocDto {
                path: path.clone(),
                kind: "Runbook".into(),
                r#ref: runbook_ref,
                yaml_hash: munarium_authoring::sha256_hex(yaml.as_bytes()),
            });
        }
    }
    Ok(Json(dto::ApplyDraftResponse { applied }))
}

// ---------------------------------------------------------------------------
// AI assist (BYOK; degrades to a note, never fails)
// ---------------------------------------------------------------------------

#[derive(Debug, Default, serde::Deserialize)]
struct AssistPayload {
    #[serde(default)]
    documents: BTreeMap<String, String>,
    #[serde(default)]
    suggestions: Vec<dto::SuggestionDto>,
}

/// POST /v1/authoring/drafts/{draft_id}/assist — BYOK drafting/refinement.
/// The model may replace whole documents (known paths only, must re-parse)
/// and add suggestions; any failure degrades to assist_note on a 200.
#[utoipa::path(post, path = "/v1/authoring/drafts/{draft_id}/assist",
    params(("draft_id" = String, Path)),
    request_body = dto::AssistDraftRequest,
    responses((status = 200, body = dto::AssistDraftResponse)), tag = "authoring")]
pub async fn assist_draft(
    State(state): State<Arc<AppState>>,
    Path(draft_id): Path<String>,
    headers: HeaderMap,
    crate::rest::ProblemJson(req): crate::rest::ProblemJson<dto::AssistDraftRequest>,
) -> ApiResult<Json<dto::AssistDraftResponse>> {
    let ctx = rest_auth(&state, &headers)?;
    ctx.require_rw()?;
    let row = load_draft(&state, &ctx.tenant_id, &draft_id).await?;
    // A blank draft has nothing to refine: materialize placeholders first so
    // the model edits a valid skeleton instead of inventing the file layout.
    let base_docs = if row.documents.is_empty() {
        materialize::build_documents(&row.name, row.pattern(), &row.answers)
            .map_err(KernelError::InvalidInput)?
            .documents
    } else {
        row.documents.clone()
    };
    let todos = stored_validation(&row).map(|v| v.todos).unwrap_or_default();
    let base_validation =
        validate_documents(&state, &ctx.tenant_id, &base_docs, todos.clone()).await?;

    let (documents, suggestions, note) = match run_assist(
        &state,
        &ctx.tenant_id,
        &row,
        &req,
        &base_docs,
        &base_validation,
    )
    .await
    {
        Ok(outcome) => (outcome.documents, outcome.suggestions, outcome.note),
        Err(e) => (None, Vec::new(), Some(format!("assist unavailable: {e}"))),
    };

    // Persist only what actually changed. A degraded pass on an unchanged
    // draft must not rewrite documents or knock a 'validated' draft back to
    // 'drafted' — the note and fresh findings are the whole delta. (An empty
    // draft's materialized skeleton DOES count as a change: base_docs differ
    // from the stored empty set, and keeping them is the useful outcome.)
    let final_docs = documents.unwrap_or(base_docs);
    let changed = final_docs != row.documents;
    let validation = validate_documents(&state, &ctx.tenant_id, &final_docs, todos).await?;
    store_draft_update(
        &state,
        &ctx.tenant_id,
        &draft_id,
        if changed { "drafted" } else { &row.state },
        None,
        changed.then_some(&final_docs),
        Some(serde_json::to_value(&validation).unwrap_or_default()),
        Some(note.as_deref()),
    )
    .await?;
    Ok(Json(dto::AssistDraftResponse {
        documents: document_dtos(&final_docs),
        suggestions,
        assist_note: note,
        validation,
    }))
}

struct AssistOutcome {
    /// Replacement document set, when the model's edits were accepted.
    documents: Option<BTreeMap<String, String>>,
    suggestions: Vec<dto::SuggestionDto>,
    /// Set when the document edits were DISCARDED (unknown path, parse
    /// failure) while the suggestions were kept.
    note: Option<String>,
}

/// The provider call. Model resolution is request-supplied (see module doc:
/// there is no published runbook whose allowOverrides policy could gate a
/// draft), with "default" engaging the tenant fallback chain.
async fn run_assist(
    state: &AppState,
    tenant: &str,
    row: &DraftRow,
    req: &dto::AssistDraftRequest,
    docs: &BTreeMap<String, String>,
    validation: &dto::DraftValidationResponse,
) -> std::result::Result<AssistOutcome, ApiError> {
    let store = state.store_for(tenant).await?;
    let description = req
        .description
        .clone()
        .or_else(|| {
            row.answers
                .get("identity.description")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "(no description provided)".into());
    let mut docs_block = String::new();
    for (path, yaml) in docs {
        docs_block.push_str(&format!("--- {path}\n```yaml\n{yaml}\n```\n"));
    }
    let findings_json = serde_json::to_string(validation).unwrap_or_default();
    let answers_json = serde_json::to_string(&row.answers).unwrap_or_default();
    let instructions = req.instructions.clone().unwrap_or_default();
    let prompt = format!(
        "You draft munarium authoring document sets: a Shape (fact schema, chunking) plus a \
         Runbook (compartmentalized collections bound to blob-path prefixes, retrieval \
         knobs, model defaults, the five lifecycle steps, optional RAG completion).\n\n\
         Hold every one of these measured conventions:\n\
         1. Collection boundaries follow GOVERNANCE, not topics (who must NOT see this folder?).\n\
         2. Prefixes end in '/'; matching is a literal starts_with; no bound prefix nests \
            inside another unless the overlap is a decision.\n\
         3. Use few access levels (0-3); compartments are data-sensitivity sets that AND \
            together; uniform level 0 is honest for public corpora.\n\
         4. mediaTypes only where the corpus genuinely mixes formats — prefix AND media \
            type must both match, so an unnecessary constraint silently binds nothing.\n\
         5. Answer keys are never uploaded or bound — a key inside the retrieval index is \
            not a measurement.\n\
         6. Fact keys carry NO dots (subject.key splits at the LAST dot); dash/colon \
            encode version-like parts.\n\
         7. Shapes are shared, not copied; collections are shared when the corpus is.\n\
         8. Completion templates must reference {{context}} and {{query}} and should \
            carry grounding rules (cite-or-insufficient; a search hit you did not read \
            is not a citation; enumerate enumerable sets).\n\
         9. Steps stay the canonical five: resolveSources, buildIndex, verify, \
            cutover (approval: required), retireOld.\n\n\
         Corpus description:\n{description}\n\n\
         Interview answers so far:\n{answers_json}\n\n\
         Current documents:\n{docs_block}\n\
         Deterministic findings already reported (do not repeat them):\n{findings_json}\n\n\
         Operator instructions: {instructions}\n\n\
         Respond with ONLY a JSON object: {{\"documents\": {{\"<path>\": \"<complete \
         replacement yaml>\"}}, \"suggestions\": [{{\"title\": \"...\", \"rationale\": \
         \"...\", \"patch_hint\": \"...\"}}]}}. Include a path under \"documents\" ONLY \
         to replace that document entirely, and use ONLY the paths shown above. An empty \
         documents object with suggestions alone is a valid answer."
    );
    let provider_name = req.provider.clone().unwrap_or_else(|| "default".into());
    let budgets = state.max_tokens.effective(state, tenant).await?;
    let resp = crate::providers_api::op_complete(
        state,
        tenant,
        store.as_ref(),
        &provider_name,
        dto::CompleteRequest {
            prompt: Some(prompt),
            system: None,
            model: req.model.clone(),
            tier: req.tier.clone(),
            provider: None,
            // `authoring_assist` (`/v1/max-tokens`; built-in 8,192 since
            // 2026-09-02, 4,096 before).
            max_tokens: Some(budgets.authoring_assist),
            temperature: None,
            version_id: None,
        },
    )
    .await?;
    let text = resp.text;
    // strict-first, then rescue a fenced/prefixed object. Output with no
    // JSON object at all is a real failure — surfacing it beats returning
    // a silent empty success.
    let payload: AssistPayload = serde_json::from_str(&text)
        .or_else(|_| {
            let start = text.find('{');
            let end = text.rfind('}');
            match (start, end) {
                (Some(s), Some(e)) if e > s => serde_json::from_str(&text[s..=e]),
                _ => Err(serde_json::Error::io(std::io::Error::other(
                    "response contains no JSON object",
                ))),
            }
        })
        .map_err(|e| KernelError::Provider(format!("assist parse: {e}")))?;
    let suggestions: Vec<dto::SuggestionDto> = payload.suggestions.into_iter().take(5).collect();
    if payload.documents.is_empty() {
        return Ok(AssistOutcome {
            documents: None,
            suggestions,
            note: None,
        });
    }
    // Safety rails: known paths only; every returned document must parse.
    // On any violation the WHOLE document update is discarded but the
    // suggestions survive — losing good advice over one bad file helps
    // nobody.
    let mut updated = docs.clone();
    for (path, yaml) in &payload.documents {
        let discard = |why: String| AssistOutcome {
            documents: None,
            suggestions: suggestions.clone(),
            note: Some(format!("assist document edits discarded: {why}")),
        };
        if !docs.contains_key(path) {
            return Ok(discard(format!("unknown path '{path}'")));
        }
        let parse_err = match munarium_authoring::doc_kind(yaml) {
            munarium_authoring::DocKind::Shape => munarium_shapes::parse_shape(yaml).err(),
            munarium_authoring::DocKind::Runbook => munarium_runbooks::parse_runbook(yaml).err(),
            munarium_authoring::DocKind::Unknown => {
                Some("declares neither kind: Shape nor kind: Runbook".into())
            }
        };
        if let Some(e) = parse_err {
            return Ok(discard(format!("'{path}' does not parse: {e}")));
        }
        updated.insert(path.clone(), yaml.clone());
    }
    Ok(AssistOutcome {
        documents: Some(updated),
        suggestions,
        note: None,
    })
}

#[cfg(test)]
mod tests {
    use crate::config::{AuthMode, Config, DocIntelConfig, SourceStoreConfig, StoreKind};
    use crate::state::AppState;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn test_config() -> Config {
        Config {
            http_addr: "127.0.0.1:0".into(),
            grpc_addr: None,
            ops_addr: "127.0.0.1:0".into(),
            store: StoreKind::Memory,
            database_url: None,
            auth: AuthMode::Disabled,
            shutdown_grace_secs: 1,
            token_secret: None,
            token_ttl_secs: 3600,
            require_uid: false,
            interaction_body_max: 32768,
            token_revocation_check: false,
            matrix_base_url: None,
            matrix_admin_url: None,
            max_concurrency: 4,
            db_max_conns: 2,
            idempotency_ttl_secs: 86_400,
            replica_count: 1,
            registry_ttl_secs: 15,
            session_idle_ttl_secs: 0,
            evidence_purge_interval_secs: 0,
            max_tokens: munarium_api_types::MaxTokensBudgets::default(),
            instance_id: "test-instance".into(),
            source_store: SourceStoreConfig::Mem,
            doc_intel: DocIntelConfig::None,
        }
    }

    async fn state() -> Arc<AppState> {
        AppState::new(test_config()).await.expect("state")
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    }

    #[tokio::test]
    async fn patterns_catalog_serves_every_embedded_pattern_with_exemplars() {
        let rest = crate::rest::router(state().await);
        let resp = rest
            .clone()
            .oneshot(
                axum::http::Request::get("/v1/authoring/patterns")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let body = body_json(resp).await;
        // Seven with the experiment exemplars (the default); the catalog decides, so a
        // trimmed build without them serves exactly the patterns it can back.
        assert_eq!(
            body["patterns"].as_array().unwrap().len(),
            munarium_authoring::catalog::patterns().len()
        );

        // red-flag-review starts from due-diligence, which every build embeds
        // (every build embeds it), so this holds in any configuration.
        let resp = rest
            .oneshot(
                axum::http::Request::get("/v1/authoring/patterns/red-flag-review")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["start_from"], "due-diligence");
        assert!(body["runbook_yaml"]
            .as_str()
            .unwrap()
            .contains("kind: Runbook"));
        assert!(!body["shapes"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unknown_pattern_is_404() {
        let resp = crate::rest::router(state().await)
            .oneshot(
                axum::http::Request::get("/v1/authoring/patterns/nope")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn drafts_require_the_postgres_store() {
        // Memory-store mode returns the honest postgres-required problem
        // (the shared pool() contract), not a fake success.
        let resp = crate::rest::router(state().await)
            .oneshot(
                axum::http::Request::post("/v1/authoring/drafts")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::json!({ "name": "demo" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
        let body = body_json(resp).await;
        assert!(
            body["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("postgres"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn a_bad_draft_name_is_rejected_before_touching_storage() {
        // Name validation runs before the pg gate would even matter; the
        // error names the rule, not the store.
        let resp = crate::rest::router(state().await)
            .oneshot(
                axum::http::Request::post("/v1/authoring/drafts")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::json!({ "name": "Bad@Name" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
        let body = body_json(resp).await;
        assert!(
            body["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("must match"),
            "{body}"
        );
    }
}
