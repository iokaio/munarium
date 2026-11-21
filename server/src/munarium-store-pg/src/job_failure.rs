// SPDX-License-Identifier: Apache-2.0
//! Stable failure codes for build jobs (ClearView R5 / C2).
//!
//! A failure is CLASSIFIED when the builder records it, from the typed cause:
//! [`classify`] maps a [`KernelError`] variant to a [`JobFailureCode`] and a
//! fixed, safe message. Nothing here reads message text, and the read rule
//! [`client_failure`] never derives a code from a stored `error`. A failure
//! with no code (a historical row, a v1.4 builder's free text during rollout,
//! or a cause this binary does not classify) is reported with
//! [`GENERIC_FAILURE_MESSAGE`] and no code.
//!
//! The vocabulary is registered in `docs/api/errors.md` ("Build-job failure
//! codes"); a `munarium-server` docs-coverage test checks [`JobFailureCode::ALL`]
//! against it in both directions. Codes are lowercase snake case of at most
//! [`CODE_MAX_LEN`] characters, which migration 0047 also enforces on write.

use munarium_core::KernelError;

/// The longest code the column accepts (migration 0047's CHECK).
pub const CODE_MAX_LEN: usize = 64;

/// The `error` a client sees for a failed job that carries no code. Stored
/// text behind it, including database-driver text from v1.4, is not returned.
pub const GENERIC_FAILURE_MESSAGE: &str =
    "build job failed; no classified cause was recorded for this failure";

/// A stable, machine-readable build-job failure cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobFailureCode {
    /// The final attempt's lease expired before it completed; cleanup failed
    /// the job (ClearView R4).
    AttemptsExhausted,
    /// The job's target or parameters were refused by the build.
    InvalidInput,
    /// Something the job names (a collection, a version, a source) no
    /// longer exists.
    NotFound,
    /// A storage-layer failure. The driver's text goes to the server log.
    StorageError,
    /// A datastore artifact could not be read or written.
    DatastoreUnavailable,
    /// An upstream model provider (for example an embedding call) failed.
    ProviderError,
    /// The build's result is a datastore artifact with an active fleet
    /// block, refused in `MUNARIUM_ARTIFACT_DAMAGE_MODE=enforce` (ClearView
    /// ART1/ART2). Not transient: only a verified ART2 repair clears it.
    ArtifactBlocked,
    /// A PDF exceeded its wait for an isolated extraction slot.
    ExtractionDeferred,
}

impl JobFailureCode {
    /// Every code, in registry order.
    pub const ALL: [JobFailureCode; 8] = [
        Self::AttemptsExhausted,
        Self::InvalidInput,
        Self::NotFound,
        Self::StorageError,
        Self::DatastoreUnavailable,
        Self::ProviderError,
        Self::ArtifactBlocked,
        Self::ExtractionDeferred,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::AttemptsExhausted => "attempts_exhausted",
            Self::InvalidInput => "invalid_input",
            Self::NotFound => "not_found",
            Self::StorageError => "storage_error",
            Self::DatastoreUnavailable => "datastore_unavailable",
            Self::ProviderError => "provider_error",
            Self::ArtifactBlocked => "artifact_blocked",
            Self::ExtractionDeferred => "extraction_deferred",
        }
    }

    /// The fixed, safe `error` recorded with this code. It names no table,
    /// node, path, driver or provider detail.
    pub fn message(self) -> &'static str {
        match self {
            Self::AttemptsExhausted => crate::jobs::ATTEMPTS_EXHAUSTED_MESSAGE,
            Self::InvalidInput => {
                "build job failed: the build refused the job's target or parameters"
            }
            Self::NotFound => "build job failed: something the job names no longer exists",
            Self::StorageError => "build job failed: a storage error occurred; see the server log",
            Self::DatastoreUnavailable => {
                "build job failed: a datastore artifact could not be read or written"
            }
            Self::ProviderError => "build job failed: an upstream model provider call failed",
            Self::ArtifactBlocked => {
                "build job failed: its result is an artifact blocked by artifact damage; \
                 repair is required"
            }
            Self::ExtractionDeferred => {
                "build job failed: isolated PDF extraction was deferred; retry when extraction capacity returns"
            }
        }
    }
}

/// What a builder records for a failed job: a safe message and, when the
/// cause was classified, its code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobFailure {
    pub message: String,
    pub code: Option<JobFailureCode>,
}

impl JobFailure {
    /// A classified failure: the code and its fixed message.
    pub fn classified(code: JobFailureCode) -> Self {
        Self {
            message: code.message().to_owned(),
            code: Some(code),
        }
    }

    /// A caller's own message with no code: what the v1.4-shaped
    /// [`complete`](crate::jobs::BuildJobs::complete) API records. Its text
    /// is stored as given and, having no code, read back as generic.
    pub fn text(message: String) -> Self {
        Self {
            message,
            code: None,
        }
    }

    /// A failure whose cause is not classified: the generic message and no
    /// code. The cause belongs in the server log.
    pub fn unclassified() -> Self {
        Self {
            message: GENERIC_FAILURE_MESSAGE.to_owned(),
            code: None,
        }
    }
}

/// Classify a build's typed cause. Matches the error's TYPE only; a variant
/// outside the vocabulary is unclassified rather than guessed.
pub fn classify(e: &KernelError) -> JobFailure {
    let code = match e {
        KernelError::InvalidInput(_) => JobFailureCode::InvalidInput,
        KernelError::NotFound { .. } => JobFailureCode::NotFound,
        KernelError::Storage(_) => JobFailureCode::StorageError,
        KernelError::DatastoreUnavailable(_) => JobFailureCode::DatastoreUnavailable,
        KernelError::ArtifactBlocked(_) => JobFailureCode::ArtifactBlocked,
        KernelError::ExtractionDeferred(_) => JobFailureCode::ExtractionDeferred,
        KernelError::Provider(_) => JobFailureCode::ProviderError,
        KernelError::HeadConflict { .. }
        | KernelError::PolicyRejection { .. }
        | KernelError::ShapeViolation { .. }
        | KernelError::IdempotencyMismatch
        | KernelError::Unauthenticated(_)
        | KernelError::Forbidden(_)
        | KernelError::RateLimited(_) => return JobFailure::unclassified(),
    };
    JobFailure::classified(code)
}

/// The failure members a client sees for a stored row: `(error, error_code)`.
///
/// A row with a code returns its stored message (written with the code, so
/// safe) and the code. A row with stored `error` text but no code returns
/// [`GENERIC_FAILURE_MESSAGE`] and no code, whatever the text says. A row
/// with neither (success, pending, running, cancelled) returns neither.
pub fn client_failure(
    error: Option<&str>,
    error_code: Option<&str>,
) -> (Option<String>, Option<String>) {
    match (error, error_code) {
        (Some(message), Some(code)) => (Some(message.to_owned()), Some(code.to_owned())),
        (None, Some(code)) => (
            Some(GENERIC_FAILURE_MESSAGE.to_owned()),
            Some(code.to_owned()),
        ),
        (Some(_), None) => (Some(GENERIC_FAILURE_MESSAGE.to_owned()), None),
        (None, None) => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_syntax(code: &str) -> bool {
        let mut chars = code.chars();
        matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            && code.len() <= CODE_MAX_LEN
    }

    #[test]
    fn codes_are_unique_well_formed_and_carry_safe_messages() {
        let mut seen = std::collections::BTreeSet::new();
        for code in JobFailureCode::ALL {
            assert!(valid_syntax(code.as_str()), "{}", code.as_str());
            assert!(seen.insert(code.as_str()), "duplicate {}", code.as_str());
            let m = code.message();
            assert!(m.starts_with("build job failed"), "{m}");
            assert_ne!(m, GENERIC_FAILURE_MESSAGE);
        }
        assert_eq!(
            JobFailureCode::AttemptsExhausted.message(),
            crate::jobs::ATTEMPTS_EXHAUSTED_MESSAGE
        );
    }

    /// The mapping is by type. Each classified variant gets its code and the
    /// fixed message, never its own text; the rest are unclassified.
    #[test]
    fn classification_maps_types_and_never_copies_cause_text() {
        let secret = "relation \"index_versions\" violates constraint \"index_versions_pkey\"";
        let cases = [
            (
                KernelError::InvalidInput(secret.into()),
                Some(JobFailureCode::InvalidInput),
            ),
            (
                KernelError::NotFound {
                    kind: "collection",
                    id: secret.into(),
                },
                Some(JobFailureCode::NotFound),
            ),
            (
                KernelError::Storage(secret.into()),
                Some(JobFailureCode::StorageError),
            ),
            (
                KernelError::DatastoreUnavailable(secret.into()),
                Some(JobFailureCode::DatastoreUnavailable),
            ),
            (
                KernelError::Provider(secret.into()),
                Some(JobFailureCode::ProviderError),
            ),
            (
                KernelError::ArtifactBlocked(secret.into()),
                Some(JobFailureCode::ArtifactBlocked),
            ),
            (KernelError::Forbidden(secret.into()), None),
            (KernelError::RateLimited(secret.into()), None),
            (KernelError::Unauthenticated(secret.into()), None),
            (KernelError::IdempotencyMismatch, None),
            (
                KernelError::HeadConflict {
                    expected: 1,
                    actual: 2,
                },
                None,
            ),
        ];
        for (e, want) in cases {
            let got = classify(&e);
            assert_eq!(got.code, want, "{e}");
            assert!(!got.message.contains("index_versions"), "{e}");
            match want {
                Some(c) => assert_eq!(got.message, c.message()),
                None => assert_eq!(got.message, GENERIC_FAILURE_MESSAGE),
            }
        }
        // A storage error whose text happens to read like another class is
        // still a storage error: the type decides.
        assert_eq!(
            classify(&KernelError::Storage("invalid input: not found".into())).code,
            Some(JobFailureCode::StorageError)
        );
        // A blocked publication is told apart from an unavailable datastore by
        // its type alone: text that reads like either does not move it.
        assert_eq!(
            classify(&KernelError::DatastoreUnavailable(
                "artifact blocked: repair is required".into()
            ))
            .code,
            Some(JobFailureCode::DatastoreUnavailable)
        );
        assert_eq!(
            classify(&KernelError::ArtifactBlocked(
                "datastore unavailable: retry later".into()
            ))
            .code,
            Some(JobFailureCode::ArtifactBlocked)
        );
        // Text that names a code is not a code.
        assert_eq!(
            classify(&KernelError::Forbidden("attempts_exhausted".into())).code,
            None
        );
    }

    #[test]
    fn the_read_rule_never_infers_a_code_from_text() {
        let driver = "storage error: duplicate key value violates unique constraint \"x_pkey\"";
        assert_eq!(
            client_failure(Some(driver), None),
            (Some(GENERIC_FAILURE_MESSAGE.into()), None)
        );
        // Stored text that spells a code, or a classified message, still
        // carries no code without the column.
        for text in [
            "attempts_exhausted",
            crate::jobs::ATTEMPTS_EXHAUSTED_MESSAGE,
            JobFailureCode::StorageError.message(),
        ] {
            assert_eq!(
                client_failure(Some(text), None),
                (Some(GENERIC_FAILURE_MESSAGE.into()), None),
                "{text}"
            );
        }
        assert_eq!(
            client_failure(Some("safe"), Some("storage_error")),
            (Some("safe".into()), Some("storage_error".into()))
        );
        assert_eq!(
            client_failure(None, Some("attempts_exhausted")),
            (
                Some(GENERIC_FAILURE_MESSAGE.into()),
                Some("attempts_exhausted".into())
            )
        );
        assert_eq!(client_failure(None, None), (None, None));
    }
}
