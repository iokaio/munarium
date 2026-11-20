// SPDX-License-Identifier: Apache-2.0
//! ClearView R11-T1: typed retry guidance on every registered problem slug,
//! identical on REST problems and gRPC `ErrorInfo`, and harmless to a v1.4
//! problem reader. The guarded-command (409 `command-unresolved`) end-to-end
//! cases are in `command_recovery_tests`.
use super::*;
use std::collections::HashMap;
use tonic_types::StatusExt;

/// Every slug in docs/api/errors.md, with the guidance it must carry. A new
/// registered slug fails this test until someone classifies it on purpose.
const CLASSIFIED: &[(&str, Option<(bool, &str)>)] = &[
    ("head-conflict", Some((false, "reread"))),
    ("policy-rejection", None),
    ("shape-violation", None),
    ("idempotency-mismatch", None),
    ("command-unresolved", Some((false, "inspect_outcome"))),
    ("source-denied", None),
    ("overloaded", Some((true, "repeat"))),
    ("session-not-open", None),
    ("required-evidence-unavailable", None),
    ("unknown-research-profile", None),
    ("run-locked", Some((false, "retry_later"))),
    ("not-found", None),
    ("invalid-input", None),
    ("unauthenticated", None),
    ("forbidden", None),
    ("rate-limited", Some((false, "retry_later"))),
    ("daily-cap-reached", Some((false, "retry_later"))),
    ("provider-error", None),
    ("storage-error", None),
    ("datastore-unavailable", Some((false, "retry_later"))),
    ("uid-required", None),
    ("uid-mismatch", None),
    ("token-expired", None),
    ("token-revoked", None),
    ("scope-missing", None),
    ("override-not-allowed", None),
    ("removal-not-confirmed", None),
    ("authoring-draft-invalid", None),
    ("runbook-removed", None),
    ("evidence-forbidden", None),
    ("evidence-expired", None),
    ("evidence-not-committed", None),
    ("evidence-hash-mismatch", None),
    ("evidence-grant-invalid", None),
    ("evidence-on-hold", None),
    ("result-too-large", None),
];

fn registered_slugs() -> Vec<String> {
    let registry = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/api/errors.md"),
    )
    .unwrap();
    // Only the "Problem types" section (and its subsections) registers
    // problem slugs. Other tables in the registry, such as the build-job
    // failure codes (C2), are a different vocabulary.
    registry
        .lines()
        .skip_while(|line| *line != "## Problem types")
        .skip(1)
        .take_while(|line| !line.starts_with("## "))
        .filter_map(|line| line.strip_prefix("| `"))
        .filter_map(|rest| rest.split_once('`').map(|(slug, _)| slug))
        .filter(|slug| !slug.contains('.') && !slug.contains(' '))
        .map(str::to_string)
        .collect()
}

#[test]
fn every_registered_slug_is_classified_and_only_shedding_is_retryable() {
    let classified: HashMap<&str, Option<(bool, &str)>> = CLASSIFIED.iter().copied().collect();
    let registered = registered_slugs();
    assert!(registered.len() >= CLASSIFIED.len());
    for slug in &registered {
        let expected = classified
            .get(slug.as_str())
            .unwrap_or_else(|| panic!("registered slug '{slug}' has no R11 classification"));
        assert_eq!(retry_hint(slug), *expected, "{slug}");
    }
    for (slug, hint) in CLASSIFIED {
        assert!(
            registered.iter().any(|r| r == slug),
            "{slug} is not registered"
        );
        if let Some((retryable, action)) = hint {
            assert_eq!(*retryable, *action == "repeat", "{slug}");
        }
    }
    assert_eq!(retry_hint("a-future-slug"), None);
}

fn rest_hint(problem: &Problem) -> Option<(bool, String)> {
    match (problem.retryable, problem.retry_action.clone()) {
        (Some(r), Some(a)) => Some((r, a)),
        (None, None) => None,
        other => panic!("half a hint: {other:?}"),
    }
}

fn grpc_hint(status: &tonic::Status) -> (String, Option<(bool, String)>) {
    let info = status.get_error_details().error_info().unwrap().clone();
    let hint = match (
        info.metadata.get("retryable"),
        info.metadata.get("retry_action"),
    ) {
        (Some(r), Some(a)) => Some((r.parse().unwrap(), a.clone())),
        (None, None) => None,
        other => panic!("half a hint: {other:?}"),
    };
    (info.reason, hint)
}

#[test]
fn both_transports_carry_the_same_hint_for_the_same_error() {
    let kernel = [
        KernelError::HeadConflict {
            expected: 1,
            actual: 2,
        },
        KernelError::RateLimited("rpm budget 1 exhausted".into()),
        KernelError::RateLimited(format!("{DAILY_CAP_PREFIX}cap")),
        KernelError::InvalidInput(format!("{RUN_LOCKED_PREFIX}run-1")),
        KernelError::InvalidInput(format!("{COMMAND_UNRESOLVED_PREFIX}unknown")),
        KernelError::DatastoreUnavailable("scope".into()),
        KernelError::InvalidInput("bad".into()),
        KernelError::Storage("driver text".into()),
        KernelError::Provider("upstream".into()),
        KernelError::IdempotencyMismatch,
        KernelError::NotFound {
            kind: "collection",
            id: "x".into(),
        },
    ];
    for error in &kernel {
        let (_, problem) = to_problem(error);
        let (reason, grpc) = grpc_hint(&to_status(error));
        assert!(problem.problem_type.ends_with(&format!("/{reason}")));
        assert_eq!(rest_hint(&problem), grpc, "{reason}");
        assert_eq!(
            rest_hint(&problem),
            retry_hint(&reason).map(|(r, a)| (r, a.to_string())),
            "{reason}"
        );
    }
    for custom in [
        CustomError::overloaded(),
        CustomError::session_not_open("closed"),
        CustomError::required_collection_unavailable("alpha", "search_failed"),
        CustomError::token_expired(),
    ] {
        let (_, problem) = custom.to_problem();
        let (reason, grpc) = grpc_hint(&custom.to_status());
        assert_eq!(reason, custom.slug);
        assert_eq!(rest_hint(&problem), grpc, "{reason}");
    }
    let (_, shed) = CustomError::overloaded().to_problem();
    assert_eq!(rest_hint(&shed), Some((true, "repeat".into())));
}

#[test]
fn a_hint_does_not_replace_existing_headers_or_members() {
    let response = ApiError::Mesh(KernelError::RateLimited("rpm".into())).into_response();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::RETRY_AFTER)
            .unwrap(),
        "60"
    );
    let (_, conflict) = to_problem(&KernelError::HeadConflict {
        expected: 3,
        actual: 4,
    });
    assert_eq!((conflict.expected, conflict.actual), (Some(3), Some(4)));
    let status = to_status(&KernelError::HeadConflict {
        expected: 3,
        actual: 4,
    });
    let info = status.get_error_details().error_info().unwrap().clone();
    assert_eq!(info.metadata["expected"], "3");
    assert_eq!(info.metadata["retry_action"], "reread");
}

/// The v1.4 `Problem` reader, frozen: a body with the new members still
/// decodes, and a problem without guidance serializes exactly as before.
#[test]
fn a_v14_problem_reader_is_unaffected() {
    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct ProblemV14 {
        #[serde(rename = "type")]
        problem_type: String,
        title: String,
        status: u16,
        detail: Option<String>,
        expected: Option<u64>,
        actual: Option<u64>,
        kind: Option<String>,
        id: Option<String>,
    }
    let (_, unresolved) = to_problem(&KernelError::InvalidInput(format!(
        "{COMMAND_UNRESOLVED_PREFIX}unknown"
    )));
    let body = serde_json::to_vec(&unresolved).unwrap();
    let old: ProblemV14 = serde_json::from_slice(&body).unwrap();
    assert!(old.problem_type.ends_with("/command-unresolved"));
    assert_eq!(old.status, 409);

    let (_, invalid) = to_problem(&KernelError::InvalidInput("bad".into()));
    assert_eq!(
        serde_json::to_value(&invalid).unwrap(),
        serde_json::json!({
            "type": "https://munarium.ioka.io/problems/invalid-input",
            "title": "invalid input",
            "status": 400,
            "detail": "invalid input: bad"
        })
    );
}
