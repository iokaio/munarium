// SPDX-License-Identifier: Apache-2.0
//! One typed error surface keyed on the problem-slug registry
//! (server/docs/api/errors.md). REST decodes `application/problem+json`;
//! gRPC decodes the `google.rpc.ErrorInfo` detail in
//! `grpc-status-details-bin`. **No English message text is ever parsed.**

use munarium_api_types::GateFindingDto;
use std::time::Duration;

/// Errors a client call can surface. The first group mirrors the server's
/// problem registry one-to-one; the last three are client-side conditions.
#[derive(Debug, thiserror::Error)]
pub enum MunariumError {
    /// Optimistic `expected_head` mismatch — normal and retryable: re-read
    /// head, re-decide, retry (or use `MunariumClient::propose_claim_with_retry`).
    /// `actual == 0` means the server did not carry structured details
    /// (e.g. an intermediary stripped them) — re-read the head yourself.
    #[error("head conflict: expected seq {expected}, actual {actual}")]
    HeadConflict { expected: u64, actual: u64 },

    /// Block-severity gate findings on a non-claim path. NOTE: a gated
    /// `propose_claim`/`append_events` does NOT error — the claim is recorded
    /// `disputed` and returned with findings (success, invariant #1).
    /// On gRPC the findings list is size-capped by the server to fit the
    /// HTTP/2 trailer: `findings_total` is the real count and
    /// `findings_truncated` marks a capped list.
    #[error("policy rejection: {} finding(s)", findings.len())]
    PolicyRejection {
        findings: Vec<GateFindingDto>,
        findings_total: u64,
        findings_truncated: bool,
    },

    #[error("shape violation ({shape_ref}): {detail}")]
    ShapeViolation { shape_ref: String, detail: String },

    #[error("idempotency key replayed with a different request")]
    IdempotencyMismatch,

    #[error("not found: {kind} {id}")]
    NotFound { kind: String, id: String },

    #[error("invalid input: {detail}")]
    InvalidInput { detail: String },

    #[error("unauthenticated: {detail}")]
    Unauthenticated { detail: String },

    #[error("forbidden: {detail}")]
    Forbidden { detail: String },

    /// Per-tenant limits or provider budget exhausted. `retry_after` carries
    /// the server's Retry-After hint when it sent one (REST). Deliberately
    /// NOT auto-retried — honor the hint in your own pacing.
    #[error("rate limited: {detail}")]
    RateLimited {
        detail: String,
        retry_after: Option<Duration>,
    },

    /// Load-shed / graceful drain (503 / `overloaded`) — transient, retried
    /// automatically on read paths.
    #[error("server overloaded: {detail}")]
    Overloaded { detail: String },

    /// Another run holds this runbook's run lock (409 / gRPC ABORTED with
    /// reason `run-locked`, 2026-08-17). The server rejected the request
    /// BEFORE executing anything, and the lock clears when the holding run
    /// finishes — retryable in YOUR OWN pacing, like `RateLimited`, and for
    /// the same reason deliberately NOT in the transient class: a run lock
    /// is held for a whole run (minutes), so sub-second auto-retry would be
    /// futile churn that masks the typed signal.
    #[error("run locked: {detail}")]
    RunLocked { detail: String },

    #[error("server storage error: {detail}")]
    Storage { detail: String },

    #[error("provider error: {detail}")]
    Provider { detail: String },

    /// The operation has no RPC/route on the transport this client was built
    /// with (e.g. index builds are REST-only today).
    #[error("unsupported on this transport: {detail}")]
    Unsupported { detail: String },

    /// Connection-level failure (DNS, refused, reset, timeout) — the request
    /// may never have reached the server.
    #[error("transport error: {detail}")]
    Transport {
        detail: String,
        /// True when the request may already have reached the server (an
        /// established connection that then timed out or dropped). The
        /// server records an idempotency key only AFTER a command finishes,
        /// so a command that fails this way is NOT auto-retried: a retry
        /// could overtake an in-flight attempt and execute twice. Reads are
        /// retried regardless. `false` means the request provably never
        /// left (connect-phase failure) and is always safe to re-send.
        may_have_reached_server: bool,
    },

    /// An error response that did not match the registry.
    #[error("unexpected server error (status {status:?}): {detail}")]
    Unexpected { status: Option<u16>, detail: String },
}

pub type Result<T> = std::result::Result<T, MunariumError>;

impl MunariumError {
    /// Registry slug for this error, when it maps to one.
    pub fn slug(&self) -> Option<&'static str> {
        Some(match self {
            MunariumError::HeadConflict { .. } => "head-conflict",
            MunariumError::PolicyRejection { .. } => "policy-rejection",
            MunariumError::ShapeViolation { .. } => "shape-violation",
            MunariumError::IdempotencyMismatch => "idempotency-mismatch",
            MunariumError::NotFound { .. } => "not-found",
            MunariumError::InvalidInput { .. } => "invalid-input",
            MunariumError::Unauthenticated { .. } => "unauthenticated",
            MunariumError::Forbidden { .. } => "forbidden",
            MunariumError::RateLimited { .. } => "rate-limited",
            MunariumError::Overloaded { .. } => "overloaded",
            MunariumError::RunLocked { .. } => "run-locked",
            MunariumError::Storage { .. } => "storage-error",
            MunariumError::Provider { .. } => "provider-error",
            _ => return None,
        })
    }

    /// True when re-sending the SAME command (same idempotency key) cannot
    /// double-execute: the request provably never reached the server, or the
    /// server shed it before executing. Strictly narrower than
    /// `is_transient` — see the `Transport::may_have_reached_server` note.
    pub fn is_command_retry_safe(&self) -> bool {
        matches!(
            self,
            MunariumError::Transport {
                may_have_reached_server: false,
                ..
            } | MunariumError::Overloaded { .. }
        )
    }

    /// Head conflicts are retryable too but need a REBUILT request — see
    /// `propose_claim_with_retry`. Rate limits are NOT transient: honor
    /// `retry_after` in your own pacing.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            MunariumError::Transport { .. }
                | MunariumError::Overloaded { .. }
                | MunariumError::Unexpected {
                    status: Some(502..=504),
                    ..
                }
        )
    }

    /// Decode a REST error response (problem+json) into a typed error.
    /// `retry_after` is the parsed Retry-After header, when present.
    pub fn from_problem(
        status: u16,
        retry_after: Option<Duration>,
        body: &serde_json::Value,
    ) -> MunariumError {
        let slug = body["type"]
            .as_str()
            .unwrap_or_default()
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string();
        let detail = body["detail"].as_str().unwrap_or_default().to_string();
        from_parts(
            &slug,
            detail,
            Some(status),
            retry_after,
            ProblemExt::from_json(body),
        )
    }
}

/// Extension members carried beside the slug — same names on both transports.
pub(crate) struct ProblemExt {
    pub expected: Option<u64>,
    pub actual: Option<u64>,
    pub gate_findings: Vec<GateFindingDto>,
    pub findings_total: Option<u64>,
    pub findings_truncated: bool,
    pub shape_ref: Option<String>,
    pub kind: Option<String>,
    pub id: Option<String>,
}

impl ProblemExt {
    fn from_json(body: &serde_json::Value) -> Self {
        use serde::Deserialize;
        let gate_findings = body
            .get("gate_findings")
            .and_then(|v| Vec::<GateFindingDto>::deserialize(v).ok())
            .unwrap_or_default();
        Self {
            expected: body["expected"].as_u64(),
            actual: body["actual"].as_u64(),
            findings_total: Some(gate_findings.len() as u64),
            findings_truncated: false, // REST bodies carry the full list
            gate_findings,
            shape_ref: body["shape_ref"].as_str().map(String::from),
            kind: body["kind"].as_str().map(String::from),
            id: body["id"].as_str().map(String::from),
        }
    }

    #[cfg(feature = "grpc")]
    pub(crate) fn from_metadata(md: &std::collections::HashMap<String, String>) -> Self {
        use serde::Deserialize;
        let gate_findings = md
            .get("gate_findings")
            .and_then(|v| serde_json::from_str::<serde_json::Value>(v).ok())
            .and_then(|v| Vec::<GateFindingDto>::deserialize(&v).ok())
            .unwrap_or_default();
        Self {
            expected: md.get("expected").and_then(|v| v.parse().ok()),
            actual: md.get("actual").and_then(|v| v.parse().ok()),
            findings_total: md.get("findings_total").and_then(|v| v.parse().ok()),
            findings_truncated: md.get("findings_truncated").map(String::as_str) == Some("true"),
            gate_findings,
            shape_ref: md.get("shape_ref").cloned(),
            kind: md.get("kind").cloned(),
            id: md.get("id").cloned(),
        }
    }
}

pub(crate) fn from_parts(
    slug: &str,
    detail: String,
    status: Option<u16>,
    retry_after: Option<Duration>,
    ext: ProblemExt,
) -> MunariumError {
    match slug {
        "head-conflict" => MunariumError::HeadConflict {
            expected: ext.expected.unwrap_or(0),
            actual: ext.actual.unwrap_or(0),
        },
        "policy-rejection" => MunariumError::PolicyRejection {
            findings_total: ext.findings_total.unwrap_or(ext.gate_findings.len() as u64),
            findings_truncated: ext.findings_truncated,
            findings: ext.gate_findings,
        },
        "shape-violation" => MunariumError::ShapeViolation {
            shape_ref: ext.shape_ref.unwrap_or_default(),
            detail,
        },
        "idempotency-mismatch" => MunariumError::IdempotencyMismatch,
        "not-found" => MunariumError::NotFound {
            kind: ext.kind.unwrap_or_else(|| "resource".into()),
            id: ext.id.unwrap_or(detail),
        },
        "invalid-input" => MunariumError::InvalidInput { detail },
        "unauthenticated" => MunariumError::Unauthenticated { detail },
        "forbidden" => MunariumError::Forbidden { detail },
        "rate-limited" => MunariumError::RateLimited {
            detail,
            retry_after,
        },
        "overloaded" => MunariumError::Overloaded { detail },
        "storage-error" => MunariumError::Storage { detail },
        "provider-error" => MunariumError::Provider { detail },
        // platform identity/lifecycle slugs — mapped to the existing kinds by
        // status class so re-auth/permission logic keeps working; the token
        // lifecycle (expired/revoked) is Unauthenticated so a caller can
        // refresh, and runbook-removed is a 410 (gone) surfaced as NotFound.
        "uid-required" => MunariumError::InvalidInput { detail },
        "token-expired" | "token-revoked" => MunariumError::Unauthenticated { detail },
        "uid-mismatch" | "scope-missing" | "override-not-allowed" => {
            MunariumError::Forbidden { detail }
        }
        "removal-not-confirmed" => MunariumError::InvalidInput { detail },
        // 2026-08-17/19 lifecycle slugs (sessions, run lock, authoring).
        // session-not-open / authoring-draft-invalid follow the same
        // status-class convention as removal-not-confirmed; run-locked is
        // its own kind because its RETRYABILITY is semantic — before this
        // mapping it decoded as Unexpected, hiding that a later re-run
        // succeeds once the holding run finishes.
        "session-not-open" | "authoring-draft-invalid" => MunariumError::InvalidInput { detail },
        "run-locked" => MunariumError::RunLocked { detail },
        "runbook-removed" => MunariumError::NotFound {
            kind: ext.kind.unwrap_or_else(|| "runbook".into()),
            id: ext.id.unwrap_or(detail),
        },
        _ => MunariumError::Unexpected { status, detail },
    }
}

/// Decode a tonic Status into a typed error via the ErrorInfo detail.
/// Falls back to code-based mapping when no structured details are present
/// (e.g. errors minted by intermediaries).
#[cfg(feature = "grpc")]
pub(crate) fn from_status(status: tonic::Status) -> MunariumError {
    use tonic_types::StatusExt;
    let detail_text = status.message().to_string();
    let details = status.get_error_details();
    if let Some(info) = details.error_info() {
        if info.domain == "mmp.ioka.io" {
            let ext = ProblemExt::from_metadata(&info.metadata);
            return from_parts(&info.reason, detail_text, None, None, ext);
        }
    }
    match status.code() {
        // ABORTED is the head-conflict code (grpc.md: "the two you must
        // handle in write loops"). Without details the seqs are unknown —
        // 0/0 tells the write loop to re-read the head itself.
        tonic::Code::Aborted => MunariumError::HeadConflict {
            expected: 0,
            actual: 0,
        },
        tonic::Code::NotFound => MunariumError::NotFound {
            kind: "resource".into(),
            id: detail_text,
        },
        tonic::Code::InvalidArgument => MunariumError::InvalidInput {
            detail: detail_text,
        },
        tonic::Code::Unauthenticated => MunariumError::Unauthenticated {
            detail: detail_text,
        },
        tonic::Code::PermissionDenied => MunariumError::Forbidden {
            detail: detail_text,
        },
        tonic::Code::ResourceExhausted => MunariumError::RateLimited {
            detail: detail_text,
            retry_after: None,
        },
        // UNAVAILABLE, and both spellings of deadline expiry (tonic's
        // client-side timeout layer reports CANCELLED "Timeout expired";
        // a server-side expiry is DEADLINE_EXCEEDED). All three are
        // transport faults, and all three may have reached the server.
        tonic::Code::Unavailable | tonic::Code::Cancelled | tonic::Code::DeadlineExceeded => {
            MunariumError::Transport {
                detail: detail_text,
                may_have_reached_server: true,
            }
        }
        tonic::Code::Internal => MunariumError::Storage {
            detail: detail_text,
        },
        code => MunariumError::Unexpected {
            status: None,
            detail: format!("{code:?}: {detail_text}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn problem_head_conflict_decodes_extensions() {
        let body = serde_json::json!({
            "type": "https://munarium.ioka.io/problems/head-conflict",
            "title": "optimistic head conflict",
            "status": 409,
            "detail": "head conflict: expected seq 3, actual 7",
            "expected": 3,
            "actual": 7,
        });
        match MunariumError::from_problem(409, None, &body) {
            MunariumError::HeadConflict {
                expected: 3,
                actual: 7,
            } => {}
            other => panic!("expected HeadConflict, got {other:?}"),
        }
    }

    #[test]
    fn problem_policy_rejection_carries_findings() {
        let body = serde_json::json!({
            "type": "https://munarium.ioka.io/problems/policy-rejection",
            "title": "policy rejection", "status": 422,
            "gate_findings": [
                {"rule_id": "gate.ledger-conflict", "severity": "block", "message": "boom"}
            ],
        });
        match MunariumError::from_problem(422, None, &body) {
            MunariumError::PolicyRejection {
                findings,
                findings_total,
                findings_truncated,
            } => {
                assert_eq!(findings.len(), 1);
                assert_eq!(findings[0].rule_id, "gate.ledger-conflict");
                assert_eq!(findings_total, 1);
                assert!(!findings_truncated);
            }
            other => panic!("expected PolicyRejection, got {other:?}"),
        }
    }

    #[test]
    fn problem_auth_slugs_map_to_typed_errors() {
        for (slug, status) in [("unauthenticated", 401u16), ("forbidden", 403)] {
            let body = serde_json::json!({
                "type": format!("https://munarium.ioka.io/problems/{slug}"),
                "title": slug, "status": status, "detail": "nope",
            });
            let err = MunariumError::from_problem(status, None, &body);
            assert_eq!(err.slug(), Some(slug), "{err:?}");
        }
    }

    #[test]
    fn m7_identity_slugs_map_by_status_class() {
        let cases = [
            ("uid-required", 400u16, "invalid-input"),
            ("uid-mismatch", 403, "forbidden"),
            ("token-expired", 401, "unauthenticated"),
            ("token-revoked", 401, "unauthenticated"),
            ("scope-missing", 403, "forbidden"),
            ("override-not-allowed", 403, "forbidden"),
            ("removal-not-confirmed", 409, "invalid-input"),
            ("runbook-removed", 410, "not-found"),
            ("session-not-open", 409, "invalid-input"),
            ("authoring-draft-invalid", 409, "invalid-input"),
        ];
        for (slug, status, expected_kind) in cases {
            let body = serde_json::json!({
                "type": format!("https://munarium.ioka.io/problems/{slug}"),
                "title": slug, "status": status, "detail": "d",
            });
            let err = MunariumError::from_problem(status, None, &body);
            assert_eq!(
                err.slug(),
                Some(expected_kind),
                "{slug} should map to {expected_kind}, got {err:?}"
            );
        }
    }

    #[test]
    fn problem_not_found_uses_kind_and_id() {
        let body = serde_json::json!({
            "type": "https://munarium.ioka.io/problems/not-found",
            "title": "not found", "status": 404,
            "detail": "not found: claim c-1", "kind": "claim", "id": "c-1",
        });
        match MunariumError::from_problem(404, None, &body) {
            MunariumError::NotFound { kind, id } => {
                assert_eq!((kind.as_str(), id.as_str()), ("claim", "c-1"));
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn overloaded_is_typed_and_transient() {
        let body = serde_json::json!({
            "type": "https://munarium.ioka.io/problems/overloaded",
            "title": "overloaded", "status": 503, "detail": "drain",
        });
        let err = MunariumError::from_problem(503, None, &body);
        assert!(matches!(err, MunariumError::Overloaded { .. }), "{err:?}");
        assert!(err.is_transient());
        // An unknown slug on a 5xx gateway status is also transient.
        let raw = serde_json::json!({"type": "x", "status": 503});
        assert!(MunariumError::from_problem(503, None, &raw).is_transient());
    }

    #[test]
    fn rate_limited_carries_retry_after() {
        let body = serde_json::json!({
            "type": "https://munarium.ioka.io/problems/rate-limited",
            "title": "rate limited", "status": 429, "detail": "tpm cap",
        });
        match MunariumError::from_problem(429, Some(Duration::from_secs(7)), &body) {
            MunariumError::RateLimited {
                retry_after: Some(d),
                ..
            } => assert_eq!(d, Duration::from_secs(7)),
            other => panic!("expected RateLimited with retry_after, got {other:?}"),
        }
    }

    #[cfg(feature = "grpc")]
    #[test]
    fn grpc_error_info_round_trip() {
        use tonic_types::{ErrorDetails, StatusExt};
        let mut details = ErrorDetails::new();
        let mut md = std::collections::HashMap::new();
        md.insert("expected".to_string(), "3".to_string());
        md.insert("actual".to_string(), "7".to_string());
        details.set_error_info("head-conflict", "mmp.ioka.io", md);
        let status = tonic::Status::with_error_details(
            tonic::Code::Aborted,
            "head conflict: expected seq 3, actual 7",
            details,
        );
        match from_status(status) {
            MunariumError::HeadConflict {
                expected: 3,
                actual: 7,
            } => {}
            other => panic!("expected HeadConflict from ErrorInfo, got {other:?}"),
        }
    }

    #[cfg(feature = "grpc")]
    #[test]
    fn grpc_truncated_findings_are_marked() {
        use tonic_types::{ErrorDetails, StatusExt};
        let mut details = ErrorDetails::new();
        let mut md = std::collections::HashMap::new();
        md.insert(
            "gate_findings".to_string(),
            r#"[{"rule_id":"gate.ledger-conflict","severity":"block","message":"m"}]"#.to_string(),
        );
        md.insert("findings_total".to_string(), "40".to_string());
        md.insert("findings_truncated".to_string(), "true".to_string());
        details.set_error_info("policy-rejection", "mmp.ioka.io", md);
        let status = tonic::Status::with_error_details(
            tonic::Code::FailedPrecondition,
            "policy rejection: 40 finding(s)",
            details,
        );
        match from_status(status) {
            MunariumError::PolicyRejection {
                findings,
                findings_total: 40,
                findings_truncated: true,
            } => assert_eq!(findings.len(), 1),
            other => panic!("expected truncated PolicyRejection, got {other:?}"),
        }
    }

    #[cfg(feature = "grpc")]
    #[test]
    fn grpc_aborted_without_details_is_still_a_head_conflict() {
        let status = tonic::Status::new(tonic::Code::Aborted, "conflict via intermediary");
        match from_status(status) {
            MunariumError::HeadConflict {
                expected: 0,
                actual: 0,
            } => {}
            other => panic!("expected HeadConflict fallback, got {other:?}"),
        }
    }

    #[test]
    fn run_locked_is_typed_and_transient_but_never_command_safe() {
        // Before this slug was mapped it decoded as Unexpected — hiding
        // that the request was rejected pre-execution and a later re-run
        // succeeds once the lock clears.
        let body = serde_json::json!({
            "type": "https://munarium.ioka.io/problems/run-locked",
            "title": "run locked", "status": 409,
            "detail": "run run-1 holds the lock",
        });
        let err = MunariumError::from_problem(409, None, &body);
        assert!(matches!(err, MunariumError::RunLocked { .. }), "{err:?}");
        assert_eq!(err.slug(), Some("run-locked"));
        assert!(
            !err.is_transient(),
            "a run lock is held for a whole run — pace yourself, like RateLimited; \
             auto-retry with sub-second jitter would be futile churn"
        );
        assert!(
            !err.is_command_retry_safe(),
            "no command draws run-locked; keep it out of the command class"
        );
    }

    #[test]
    fn a_possibly_delivered_command_is_not_retry_safe() {
        // The server records an idempotency key only AFTER the command
        // completes, so re-sending could execute it twice.
        let delivered = MunariumError::Transport {
            detail: "read timeout".into(),
            may_have_reached_server: true,
        };
        assert!(delivered.is_transient(), "reads still retry it");
        assert!(!delivered.is_command_retry_safe());

        let never_left = MunariumError::Transport {
            detail: "connection refused".into(),
            may_have_reached_server: false,
        };
        assert!(never_left.is_command_retry_safe());

        // Load-shed happens BEFORE execution, so it is safe either way.
        assert!(MunariumError::Overloaded {
            detail: "shed".into()
        }
        .is_command_retry_safe());
    }

    #[test]
    fn deadline_expiry_decodes_as_transport_on_both_spellings() {
        for code in [
            tonic::Code::Cancelled,
            tonic::Code::DeadlineExceeded,
            tonic::Code::Unavailable,
        ] {
            let err = from_status(tonic::Status::new(code, "timeout expired"));
            assert!(
                matches!(err, MunariumError::Transport { .. }),
                "{code:?} must classify as transport, not unexpected"
            );
            assert!(err.is_transient(), "{code:?} must be retryable for reads");
            assert!(
                !err.is_command_retry_safe(),
                "{code:?} may have reached the server"
            );
        }
    }

    #[test]
    fn retry_after_accepts_both_wire_forms() {
        assert_eq!(
            crate::rest::parse_retry_after("30"),
            Some(std::time::Duration::from_secs(30))
        );
        // The HTTP-date form is what an intermediary emits; a past date
        // yields zero rather than being discarded.
        assert_eq!(
            crate::rest::parse_retry_after("Wed, 21 Oct 1998 07:28:00 GMT"),
            Some(std::time::Duration::ZERO)
        );
        assert_eq!(crate::rest::parse_retry_after("soon"), None);
    }
}
