// SPDX-License-Identifier: Apache-2.0
//! Central error mapping: one kernel error surface -> problem+json (REST)
//! and tonic::Status (gRPC). Registry: docs/api/errors.md.
//!
//! gRPC statuses carry structured details in `grpc-status-details-bin`
//! (google.rpc.Status with a google.rpc.ErrorInfo detail): `reason` is the
//! problem slug, `domain` is "mmp.ioka.io", and `metadata` carries the same
//! extension member NAMES as the REST problem+json (expected/actual on
//! head-conflict, gate_findings on policy-rejection, shape_ref on
//! shape-violation, kind/id on not-found) — clients never parse English
//! error text.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use munarium_api_conv::{convert, Convert};
use munarium_api_types::Problem;
use munarium_core::KernelError;
use tonic_types::{ErrorDetails, StatusExt};

const BASE: &str = "https://munarium.ioka.io/problems";
/// The ErrorInfo domain for every munarium-emitted gRPC error detail.
const ERROR_DOMAIN: &str = "mmp.ioka.io";

#[derive(Debug)]
pub enum ApiError {
    Mesh(KernelError),
    /// platform: server-emitted problems that are not kernel errors (uid contract,
    /// token lifecycle, override policy). Same registry, own slugs.
    Custom(CustomError),
}

impl From<KernelError> for ApiError {
    fn from(e: KernelError) -> Self {
        Self::Mesh(e)
    }
}

/// The text of an error as a CLIENT may see it inside a 200 response (a
/// per-item ingest result, a bulk file's `error` column). A storage error's
/// message is the driver's — table, column and constraint names — and is
/// logged here instead; every other class is written for the caller.
pub(crate) fn client_facing_error(e: &ApiError) -> String {
    match e {
        ApiError::Mesh(KernelError::Storage(_)) => {
            tracing::warn!(error = %e, "storage error behind a per-item result");
            "storage error; see the server log".to_string()
        }
        other => other.to_string(),
    }
}

impl From<CustomError> for ApiError {
    fn from(e: CustomError) -> Self {
        Self::Custom(e)
    }
}

/// A problem outside the KernelError surface. `slug` is registered in
/// docs/api/errors.md exactly like the kernel slugs; gRPC carries it as the
/// ErrorInfo `reason` with the same domain.
#[derive(Debug, Clone)]
pub struct CustomError {
    pub slug: &'static str,
    pub status: StatusCode,
    pub code: tonic::Code,
    pub title: &'static str,
    pub detail: String,
}

impl CustomError {
    pub fn uid_required() -> Self {
        Self {
            slug: "uid-required",
            status: StatusCode::BAD_REQUEST,
            code: tonic::Code::InvalidArgument,
            title: "uid required",
            detail: "X-Munarium-Uid header (REST) / munarium-uid metadata (gRPC) is required on every /v1 request".into(),
        }
    }

    pub fn uid_mismatch(header_uid: &str, token_sub: &str) -> Self {
        Self {
            slug: "uid-mismatch",
            status: StatusCode::FORBIDDEN,
            code: tonic::Code::PermissionDenied,
            title: "uid mismatch",
            detail: format!(
                "asserted uid '{header_uid}' does not match the access token subject '{token_sub}'"
            ),
        }
    }

    pub fn token_expired() -> Self {
        Self {
            slug: "token-expired",
            status: StatusCode::UNAUTHORIZED,
            code: tonic::Code::Unauthenticated,
            title: "access token expired",
            detail: "the access token has expired; request a new one from the management plane"
                .into(),
        }
    }

    pub fn token_revoked() -> Self {
        Self {
            slug: "token-revoked",
            status: StatusCode::UNAUTHORIZED,
            code: tonic::Code::Unauthenticated,
            title: "access token revoked",
            detail: "the access token has been revoked".into(),
        }
    }

    pub fn override_not_allowed(provider: &str, runbook_ref: &str) -> Self {
        Self {
            slug: "override-not-allowed",
            status: StatusCode::FORBIDDEN,
            code: tonic::Code::PermissionDenied,
            title: "model override not allowed",
            detail: format!(
                "runbook '{runbook_ref}' does not permit overriding to provider '{provider}' (models.allowOverrides)"
            ),
        }
    }

    pub fn authoring_draft_invalid(detail: String) -> Self {
        Self {
            slug: "authoring-draft-invalid",
            status: StatusCode::CONFLICT,
            code: tonic::Code::FailedPrecondition,
            title: "authoring draft invalid",
            detail,
        }
    }

    pub fn removal_not_confirmed(detail: String) -> Self {
        Self {
            slug: "removal-not-confirmed",
            status: StatusCode::CONFLICT,
            code: tonic::Code::FailedPrecondition,
            title: "removal requires the double-pass confirmation",
            detail,
        }
    }

    pub fn runbook_removed(runbook_ref: &str) -> Self {
        Self {
            slug: "runbook-removed",
            status: StatusCode::GONE,
            code: tonic::Code::NotFound,
            title: "runbook removed",
            detail: format!(
                "runbook '{runbook_ref}' has been removed (soft; its data is retained)"
            ),
        }
    }

    // ---- the sealed evidence plane ------------------------------

    /// 403: the caller does not dominate the artifact's authorization class.
    ///
    /// The detail deliberately says nothing about WHAT the artifact is — not
    /// its source, not its class, not even that the class is higher rather
    /// than differently-compartmented. An under-cleared caller learning "this
    /// exists and is above you" is a disclosure; the point of the class is
    /// that the artifact is invisible, not merely unreadable.
    pub fn evidence_forbidden() -> Self {
        Self {
            slug: "evidence-forbidden",
            status: StatusCode::FORBIDDEN,
            code: tonic::Code::PermissionDenied,
            title: "evidence not accessible",
            detail: "this session does not dominate the artifact's authorization class".into(),
        }
    }

    /// 410: retention purged the bytes. The metadata row survives precisely so
    /// this is distinguishable from `not-found` — a citation to expired
    /// evidence is an honest statement about a retention policy, whereas a 404
    /// reads as though the citation had been fabricated.
    pub fn evidence_expired(evidence_id: &str) -> Self {
        Self {
            slug: "evidence-expired",
            status: StatusCode::GONE,
            code: tonic::Code::NotFound,
            title: "evidence expired",
            detail: format!(
                "evidence '{evidence_id}' was purged under its retention policy; \
                 its manifest is retained but its rows are gone"
            ),
        }
    }

    /// 409: the artifact is under legal hold and cannot be deleted.
    ///
    /// The refusal that makes a hold mean something. Distinct from
    /// `evidence-expired`: that one says the bytes are already gone, this one
    /// says they are deliberately being kept.
    pub fn evidence_on_hold(evidence_id: &str) -> Self {
        Self {
            slug: "evidence-on-hold",
            status: StatusCode::CONFLICT,
            code: tonic::Code::FailedPrecondition,
            title: "evidence on legal hold",
            detail: format!(
                "evidence '{evidence_id}' is under legal hold and cannot be purged; \
                 lift the hold first if the hold no longer applies"
            ),
        }
    }

    /// 409: the artifact exists but its bytes were never committed. A pending
    /// artifact is not evidence yet, and saying so beats returning an empty
    /// result that reads like a complete one.
    pub fn evidence_not_committed(evidence_id: &str) -> Self {
        Self {
            slug: "evidence-not-committed",
            status: StatusCode::CONFLICT,
            code: tonic::Code::FailedPrecondition,
            title: "evidence not committed",
            detail: format!(
                "evidence '{evidence_id}' has a manifest but no committed bytes; \
                 complete the grant flow before resolving it"
            ),
        }
    }

    /// 409: the committed bytes do not hash to what the manifest declared.
    /// Fails closed — nothing is stored, because an artifact whose bytes are
    /// not the bytes it claims is worse than no artifact.
    pub fn evidence_hash_mismatch(detail: String) -> Self {
        Self {
            slug: "evidence-hash-mismatch",
            status: StatusCode::CONFLICT,
            code: tonic::Code::FailedPrecondition,
            title: "evidence hash mismatch",
            detail,
        }
    }

    /// 403: the upload grant is unknown, expired, already spent, or belongs to
    /// a different artifact. One slug for all four on purpose — distinguishing
    /// them would let a caller probe for valid grant ids.
    pub fn evidence_grant_invalid() -> Self {
        Self {
            slug: "evidence-grant-invalid",
            status: StatusCode::FORBIDDEN,
            code: tonic::Code::PermissionDenied,
            title: "evidence grant invalid",
            detail: "the upload grant is unknown, expired, already used, or not for this artifact"
                .into(),
        }
    }

    /// 413: the artifact is larger than the inline path accepts. Names the
    /// grant flow, so the caller is told what to do rather than only what
    /// failed.
    pub fn evidence_too_large(bytes: usize, cap: usize) -> Self {
        Self {
            slug: "result-too-large",
            status: StatusCode::PAYLOAD_TOO_LARGE,
            code: tonic::Code::ResourceExhausted,
            title: "result too large",
            detail: format!(
                "artifact is {bytes} bytes, over the {cap}-byte inline cap; seal it through \
                 the grant flow (POST /v1/evidence without bytes, then PUT .../bytes)"
            ),
        }
    }

    pub fn scope_missing(scope: &str) -> Self {
        Self {
            slug: "scope-missing",
            status: StatusCode::FORBIDDEN,
            code: tonic::Code::PermissionDenied,
            title: "scope missing",
            detail: format!("the access token does not carry the '{scope}' scope"),
        }
    }

    /// 409: a turn (or other lifecycle action) against a session that is no
    /// longer open (2026-08-17, with the close-session endpoint — §13 entry
    /// 11). The extensions carry the actual state so clients can
    /// distinguish closed from expired without parsing text.
    pub fn session_not_open(state: &str) -> Self {
        Self {
            slug: "session-not-open",
            status: StatusCode::CONFLICT,
            code: tonic::Code::FailedPrecondition,
            title: "session is not open",
            detail: format!("session is {state}; open a new session to continue"),
        }
    }

    /// 424: a REQUIRED evidence layer produced nothing, so the turn refuses
    /// rather than answering from an incomplete hierarchy.
    ///
    /// The detail names the LAYER and its refusal code, never the layer's
    /// sources. A caller who cannot see a source must not learn it exists
    /// from the shape of a refusal — the hidden-required-layer rule. A layer
    /// name is the runbook author's word, and safe; a source name may be a
    /// customer table the caller has no clearance for.
    pub fn required_layer_unavailable(layer: &str, refusal_code: &str) -> Self {
        Self {
            slug: "required-evidence-unavailable",
            status: StatusCode::FAILED_DEPENDENCY,
            code: tonic::Code::FailedPrecondition,
            title: "required evidence unavailable",
            detail: format!(
                "the '{layer}' evidence layer is required for this profile and did not                  produce evidence ({refusal_code}); refusing rather than answering                  from an incomplete hierarchy"
            ),
        }
    }

    /// 400: the turn named a research profile the pinned runbook does not
    /// declare. Fails closed — silently falling back to the document path
    /// would answer a different question than the caller asked.
    pub fn unknown_research_profile(profile: &str) -> Self {
        Self {
            slug: "unknown-research-profile",
            status: StatusCode::BAD_REQUEST,
            code: tonic::Code::InvalidArgument,
            title: "unknown research profile",
            detail: format!("this session's runbook declares no research profile '{profile}'"),
        }
    }

    /// 503 load shed: the instance is at MUNARIUM_MAX_CONCURRENCY. Registered
    /// in errors.md since the milestone as "reserved: not yet emitted" — emitted for
    /// real since 2026-08-17. REST responses also carry `Retry-After: 1`.
    pub fn overloaded() -> Self {
        Self {
            slug: "overloaded",
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: tonic::Code::ResourceExhausted,
            title: "server overloaded",
            detail: "the instance is at its concurrency limit; retry after a short delay".into(),
        }
    }

    pub fn to_problem(&self) -> (StatusCode, Problem) {
        (
            self.status,
            Problem {
                problem_type: format!("{BASE}/{}", self.slug),
                title: self.title.to_string(),
                status: self.status.as_u16(),
                detail: Some(self.detail.clone()),
                gate_findings: None,
                policy_citation: None,
                expected: None,
                actual: None,
                shape_ref: None,
                kind: None,
                id: None,
            },
        )
    }

    pub fn to_status(&self) -> tonic::Status {
        let mut details = ErrorDetails::new();
        details.set_error_info(
            self.slug,
            ERROR_DOMAIN,
            std::collections::HashMap::<String, String>::new(),
        );
        tonic::Status::with_error_details(self.code, self.detail.clone(), details)
    }
}

/// Sentinel prefix for the runbook execution lock (2026-08-17): op-layer
/// code speaks KernelError only, so a lost `pg_try_advisory_lock` race
/// surfaces as InvalidInput carrying this prefix, and the two transport
/// converters below promote it to the `run-locked` slug at 409/ABORTED —
/// the same promote-at-the-boundary pattern the token-lifecycle sentinels
/// use (state.rs).
pub const RUN_LOCKED_PREFIX: &str = "run-locked: ";

fn run_locked_detail(e: &KernelError) -> Option<&str> {
    match e {
        KernelError::InvalidInput(msg) => msg.strip_prefix(RUN_LOCKED_PREFIX),
        _ => None,
    }
}

/// Sentinel promoting a `RateLimited` into the `daily-cap-reached` problem
/// type (spending caps) — same promote-at-the-boundary pattern as run-locked.
/// A daily cap refusal is a 429 like a rate limit, but the client's recovery
/// differs (wait for midnight UTC or drop a tier, not retry in a minute), so
/// it earns its own slug.
pub const DAILY_CAP_PREFIX: &str = "daily-cap: ";

fn daily_cap_detail(e: &KernelError) -> Option<&str> {
    match e {
        KernelError::RateLimited(msg) => msg.strip_prefix(DAILY_CAP_PREFIX),
        _ => None,
    }
}

/// Seconds until the daily cap's window resets (midnight UTC) — the
/// `Retry-After` value for `daily-cap-reached`.
fn seconds_to_utc_midnight() -> u64 {
    let now = chrono::Utc::now();
    let midnight = (now.date_naive() + chrono::Days::new(1))
        .and_hms_opt(0, 0, 0)
        .expect("midnight is a valid time")
        .and_utc();
    (midnight - now).num_seconds().max(1) as u64
}

/// The problem slug — the one cross-transport error key (docs/api/errors.md).
pub fn slug(e: &KernelError) -> &'static str {
    match e {
        KernelError::HeadConflict { .. } => "head-conflict",
        KernelError::PolicyRejection { .. } => "policy-rejection",
        KernelError::ShapeViolation { .. } => "shape-violation",
        KernelError::IdempotencyMismatch => "idempotency-mismatch",
        KernelError::NotFound { .. } => "not-found",
        KernelError::InvalidInput(_) => "invalid-input",
        KernelError::Unauthenticated(_) => "unauthenticated",
        KernelError::Forbidden(_) => "forbidden",
        KernelError::RateLimited(_) => "rate-limited",
        KernelError::Storage(_) => "storage-error",
        KernelError::DatastoreUnavailable(_) => "datastore-unavailable",
        KernelError::Provider(_) => "provider-error",
    }
}

pub fn to_problem(e: &KernelError) -> (StatusCode, Problem) {
    if let Some(detail) = run_locked_detail(e) {
        return CustomError {
            slug: "run-locked",
            status: StatusCode::CONFLICT,
            code: tonic::Code::Aborted,
            title: "run is executing elsewhere",
            detail: detail.to_string(),
        }
        .to_problem();
    }
    if let Some(detail) = daily_cap_detail(e) {
        return CustomError {
            slug: "daily-cap-reached",
            status: StatusCode::TOO_MANY_REQUESTS,
            code: tonic::Code::ResourceExhausted,
            title: "daily token cap reached",
            detail: detail.to_string(),
        }
        .to_problem();
    }
    let (status, title) = match e {
        KernelError::HeadConflict { .. } => (StatusCode::CONFLICT, "optimistic head conflict"),
        KernelError::PolicyRejection { .. } => {
            (StatusCode::UNPROCESSABLE_ENTITY, "policy rejection")
        }
        KernelError::ShapeViolation { .. } => (StatusCode::UNPROCESSABLE_ENTITY, "shape violation"),
        KernelError::IdempotencyMismatch => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "idempotency key replayed with a different request",
        ),
        KernelError::NotFound { .. } => (StatusCode::NOT_FOUND, "not found"),
        KernelError::InvalidInput(_) => (StatusCode::BAD_REQUEST, "invalid input"),
        KernelError::Unauthenticated(_) => (StatusCode::UNAUTHORIZED, "unauthenticated"),
        KernelError::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden"),
        KernelError::RateLimited(_) => (StatusCode::TOO_MANY_REQUESTS, "rate limited"),
        KernelError::Storage(_) => (StatusCode::INTERNAL_SERVER_ERROR, "storage error"),
        // 503, not 500: the scope is temporarily unservable and retrying can
        // help (a warming replica, a re-verified artifact) -- and rollback to
        // PostgreSQL is an operator action, deliberately not a request-path
        // one.
        KernelError::DatastoreUnavailable(_) => {
            (StatusCode::SERVICE_UNAVAILABLE, "datastore unavailable")
        }
        KernelError::Provider(_) => (StatusCode::BAD_GATEWAY, "provider error"),
    };
    // A storage error's text is the driver's — table, column and constraint
    // names, occasionally connection detail — and belongs in the log with the
    // request id, not in the response body. Every other class carries a
    // message written for the caller.
    let detail = match e {
        KernelError::Storage(_) => {
            tracing::error!(error = %e, "storage error");
            "storage error; see the server log for this request id".to_string()
        }
        _ => e.to_string(),
    };
    let mut problem = Problem {
        problem_type: format!("{BASE}/{}", slug(e)),
        title: title.to_string(),
        status: status.as_u16(),
        detail: Some(detail),
        gate_findings: None,
        policy_citation: None,
        expected: None,
        actual: None,
        shape_ref: None,
        kind: None,
        id: None,
    };
    match e {
        KernelError::HeadConflict { expected, actual } => {
            problem.expected = Some(*expected);
            problem.actual = Some(*actual);
        }
        KernelError::PolicyRejection { findings } => {
            problem.gate_findings = Some(findings.iter().map(convert).collect());
        }
        KernelError::ShapeViolation { shape_ref, .. } => {
            problem.shape_ref = Some(shape_ref.clone());
        }
        KernelError::NotFound { kind, id } => {
            problem.kind = Some((*kind).to_string());
            problem.id = Some(id.clone());
        }
        _ => {}
    }
    (status, problem)
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, problem) = match &self {
            ApiError::Mesh(e) => to_problem(e),
            ApiError::Custom(c) => c.to_problem(),
        };
        // Every 429 carries Retry-After (closes the docs/api/errors.md gap):
        // a daily cap resets at midnight UTC; the rpm/tpm bucket's window is
        // a fixed 60 seconds, so 60 is the honest upper bound there.
        let retry_after = (status == StatusCode::TOO_MANY_REQUESTS).then(|| {
            if problem.problem_type.ends_with("/daily-cap-reached") {
                seconds_to_utc_midnight()
            } else {
                60
            }
        });
        let mut resp = (status, Json(problem)).into_response();
        resp.headers_mut().insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/problem+json"),
        );
        if let Some(secs) = retry_after {
            if let Ok(v) = axum::http::HeaderValue::from_str(&secs.to_string()) {
                resp.headers_mut()
                    .insert(axum::http::header::RETRY_AFTER, v);
            }
        }
        resp
    }
}

impl ApiError {
    /// The gRPC mapping for either arm — used by the platform gRPC twins
    /// (grpc_platform.rs / grpc_data.rs, 2026-08-18).
    pub fn into_status(self) -> tonic::Status {
        match self {
            ApiError::Mesh(e) => to_status(&e),
            ApiError::Custom(c) => c.to_status(),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Mesh(e) => write!(f, "{e}"),
            ApiError::Custom(c) => write!(f, "{}: {}", c.slug, c.detail),
        }
    }
}

pub fn to_status(e: &KernelError) -> tonic::Status {
    use tonic::Code;
    if let Some(detail) = run_locked_detail(e) {
        return CustomError {
            slug: "run-locked",
            status: StatusCode::CONFLICT,
            code: Code::Aborted,
            title: "run is executing elsewhere",
            detail: detail.to_string(),
        }
        .to_status();
    }
    if let Some(detail) = daily_cap_detail(e) {
        return CustomError {
            slug: "daily-cap-reached",
            status: StatusCode::TOO_MANY_REQUESTS,
            code: Code::ResourceExhausted,
            title: "daily token cap reached",
            detail: detail.to_string(),
        }
        .to_status();
    }
    let code = match e {
        KernelError::HeadConflict { .. } => Code::Aborted,
        KernelError::PolicyRejection { .. } | KernelError::ShapeViolation { .. } => {
            Code::FailedPrecondition
        }
        KernelError::IdempotencyMismatch | KernelError::InvalidInput(_) => Code::InvalidArgument,
        KernelError::Unauthenticated(_) => Code::Unauthenticated,
        KernelError::Forbidden(_) => Code::PermissionDenied,
        KernelError::RateLimited(_) => Code::ResourceExhausted,
        KernelError::NotFound { .. } => Code::NotFound,
        KernelError::Storage(_) => Code::Internal,
        KernelError::DatastoreUnavailable(_) => Code::Unavailable,
        KernelError::Provider(_) => Code::Unavailable,
    };

    // Metadata keys use the SAME names as the REST problem+json extension
    // members (expected, actual, gate_findings, shape_ref, kind, id) so a
    // client's error mapping is one table across both transports.
    let mut metadata: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    match e {
        KernelError::HeadConflict { expected, actual } => {
            metadata.insert("expected".into(), expected.to_string());
            metadata.insert("actual".into(), actual.to_string());
        }
        KernelError::PolicyRejection { findings } => {
            // gate_findings is a JSON array of GateFindingDto — identical
            // shape to the REST member. The details ride an HTTP/2 trailer
            // and peers enforce a max header-list size (commonly 8–16 KiB).
            // The trailer carries the BASE64 of the encoded Status (~4/3
            // inflation) plus the other metadata, so budget the raw JSON at
            // 4 KiB; findings_total always carries the real count and
            // findings_truncated marks a capped list.
            const FINDINGS_BYTE_BUDGET: usize = 4 * 1024;
            let mut kept: Vec<munarium_api_types::GateFindingDto> = Vec::new();
            let mut json = String::from("[]");
            for f in findings {
                kept.push(f.convert());
                match serde_json::to_string(&kept) {
                    Ok(candidate) if candidate.len() <= FINDINGS_BYTE_BUDGET => json = candidate,
                    _ => {
                        kept.pop();
                        break;
                    }
                }
            }
            metadata.insert("findings_total".into(), findings.len().to_string());
            if kept.len() < findings.len() {
                metadata.insert("findings_truncated".into(), "true".into());
            }
            metadata.insert("gate_findings".into(), json);
        }
        KernelError::ShapeViolation { shape_ref, .. } => {
            metadata.insert("shape_ref".into(), shape_ref.clone());
        }
        KernelError::NotFound { kind, id } => {
            metadata.insert("kind".into(), (*kind).to_string());
            metadata.insert("id".into(), id.clone());
        }
        _ => {}
    }

    let mut details = ErrorDetails::new();
    details.set_error_info(slug(e), ERROR_DOMAIN, metadata);
    tonic::Status::with_error_details(code, e.to_string(), details)
}
