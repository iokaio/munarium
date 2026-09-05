// SPDX-License-Identifier: Apache-2.0
use crate::types::GateFinding;
use thiserror::Error;

/// Kernel error surface. The server maps these centrally to problem+json
/// (REST) and tonic::Status (gRPC) — see docs/api/errors.md.
#[derive(Debug, Error)]
pub enum KernelError {
    /// Optimistic head check failed — a normal, retryable conflict.
    #[error("head conflict: expected seq {expected}, actual {actual}")]
    HeadConflict { expected: u64, actual: u64 },

    /// One or more block-severity gate findings. The claim was recorded
    /// `disputed` (blocked, never dropped); findings carry the citations.
    #[error("policy rejection: {} finding(s)", findings.len())]
    PolicyRejection { findings: Vec<GateFinding> },

    #[error("not found: {kind} {id}")]
    NotFound { kind: &'static str, id: String },

    #[error("shape violation for {shape_ref}: {detail}")]
    ShapeViolation { shape_ref: String, detail: String },

    #[error("idempotency key replayed with a different request")]
    IdempotencyMismatch,

    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// Missing or invalid bearer token (401 / UNAUTHENTICATED).
    #[error("unauthenticated: {0}")]
    Unauthenticated(String),

    /// Valid token, insufficient role for the operation (403 / PERMISSION_DENIED).
    #[error("forbidden: {0}")]
    Forbidden(String),

    /// Per-tenant or provider budget (rpm/tpm) exhausted.
    #[error("rate limited: {0}")]
    RateLimited(String),

    #[error("storage error: {0}")]
    Storage(String),

    /// A datastore-selected scope could not be served from its bound
    /// artifact, and no fallback exists BY DESIGN: once the rollout selector
    /// routes a scope to the datastore, a missing, unbound, corrupt or
    /// unopenable artifact is this explicit per-request failure, never a
    /// silent switch back to PostgreSQL (datastore plan section 9.1). Rollback
    /// is an operator's selector change, which is observable and audited.
    #[error("datastore unavailable: {0}")]
    DatastoreUnavailable(String),

    #[error("provider error: {0}")]
    Provider(String),
}
