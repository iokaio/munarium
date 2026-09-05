// SPDX-License-Identifier: Apache-2.0
//! The client Matrix uses to talk to munarium-server.
//!
//! **Why this exists instead of the official Rust client.** The official
//! client path-depends on three `server/` crates (`munarium-api-types`,
//! `munarium-proto`, `munarium-core`), so depending on it would put those
//! crates in Matrix's graph transitively and break ground rule 1 — which is
//! CI-enforced by a `cargo tree` grep and is the thing that keeps the two
//! trees genuinely decoupled. This client is written against the **vendored
//! contract** and the server's documented REST surface instead. See
//! owner question Q7 (2026-08-28; the questions record was closed and archived on 2026-09-02).
//!
//! Everything is behind the [`ServerClient`] trait so that:
//!
//! - conformance and unit tests run against [`MockServer`] with no server at
//!   all, and
//! - the parts of the server API that do not exist yet (the evidence plane is
//!   an unapproved S-package) have a contract-conformant stand-in, so Matrix's
//!   side is finished and tested rather than blocked.

#![forbid(unsafe_code)]
#![allow(clippy::result_large_err)]

pub mod http;
pub mod mock;

use async_trait::async_trait;
use munarium_matrix_core::{Refusal, RefusalClass};
use munarium_matrix_types::contract::EvidenceManifest;

pub use http::HttpServerClient;
pub use mock::MockServer;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ServerError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("server said {status}: {slug} — {detail}")]
    Problem {
        status: u16,
        slug: String,
        detail: String,
    },
    #[error("unexpected response shape: {0}")]
    Malformed(String),
}

impl ServerError {
    /// Map a server failure onto the refusal a caller should surface.
    ///
    /// This maps EVERY server call, not only a seal — a claim proposal, a
    /// finding, a ledger read. The messages therefore say "request"; the
    /// `seal_failed` code is the generic transport-level one and is read
    /// alongside the message, never on its own. (A 409 on `propose_claim`
    /// reading "server rejected the seal" sent a reader to the evidence plane
    /// for a problem in the ledger write — found 2026-08-29.)
    ///
    /// The distinction that matters: a 403 is `denied` and must NOT be
    /// retried, a 5xx or a transport failure is `unavailable` and should be,
    /// and a 4xx that is neither is `invalid` — our bug, not the server's
    /// mood.
    pub fn to_refusal(&self) -> Refusal {
        match self {
            ServerError::Transport(m) => Refusal::seal_failed(format!("server unreachable: {m}")),
            ServerError::Malformed(m) => Refusal::new(
                RefusalClass::Invalid,
                "seal_failed",
                format!("bad response: {m}"),
            ),
            ServerError::Problem {
                status,
                slug,
                detail,
            } => match *status {
                401 | 403 => Refusal::new(
                    RefusalClass::Denied,
                    "policy_denied",
                    format!("server refused ({slug}): {detail}"),
                ),
                404 => Refusal::new(
                    RefusalClass::NotCovered,
                    "seal_failed",
                    format!("server route missing ({slug}); is the evidence plane deployed?"),
                ),
                409 | 422 => Refusal::new(
                    RefusalClass::Invalid,
                    "seal_failed",
                    format!("server rejected the request ({slug}): {detail}"),
                ),
                s if s >= 500 => Refusal::seal_failed(format!("server error {s} ({slug})")),
                s => Refusal::new(
                    RefusalClass::Invalid,
                    "seal_failed",
                    format!("server returned {s} ({slug}): {detail}"),
                ),
            },
        }
    }
}

pub type Result<T> = std::result::Result<T, ServerError>;

/// One document for the bulk upload plane.
#[derive(Debug, Clone, PartialEq)]
pub struct UploadDocument {
    pub path: String,
    pub bytes: Vec<u8>,
    pub media_type: String,
    pub metadata: Vec<(String, String)>,
}

impl UploadDocument {
    pub fn content_hash(&self) -> String {
        use sha2::Digest;
        hex::encode(sha2::Sha256::digest(&self.bytes))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UploadOutcome {
    pub stored: u64,
    /// The server already held these exact bytes at these paths. On a replayed
    /// checkpoint this should equal the batch size and `stored` should be 0 —
    /// that is the idempotency proof.
    pub skipped_existing: u64,
    pub failed: u64,
}

/// A fact as the ledger reports it, in the shape mode C compares against.
#[derive(Debug, Clone, PartialEq)]
pub struct LedgerFact {
    pub claim_id: Option<String>,
    pub subject: String,
    pub key: String,
    pub value: String,
    pub seq: u64,
    pub status: Option<String>,
    pub provenance: Option<String>,
    /// `origin.kind` when the claim carries a connector origin:
    /// `connector` or `rollback`. None on model-extracted claims. This — not
    /// provenance — is how Matrix recognises its own earlier proposals.
    pub origin_kind: Option<String>,
}

/// A warn-only finding Matrix files against a lineage (the findings route).
#[derive(Debug, Clone, PartialEq)]
pub struct FindingRequest {
    pub version_id: String,
    pub rule_id: String,
    pub severity: String,
    pub message: String,
    pub scope_path: Option<String>,
    pub detail: serde_json::Value,
}

/// The connector provenance a proposed claim carries (`origin`). The
/// field names are the server's wire contract; `observed_at` is RFC-3339.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ClaimOriginWire {
    pub kind: String,
    pub source_id: String,
    pub mapping_version: String,
    pub row_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_position: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_id: Option<String>,
}

/// A claim Matrix proposes into the ledger (mode C/authoritative).
/// Mirrors the server's `ProposeClaimRequest` field for field — Matrix keeps
/// its own copy because it must not depend on a server crate (ground rule 1).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProposeClaimRequest {
    pub version_id: String,
    /// `fact` | `update` | `correction`.
    pub claim_type: String,
    pub subject: String,
    pub key: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_path: Option<String>,
    /// A correction or update names the claim it supersedes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<serde_json::Value>,
    pub origin: ClaimOriginWire,
}

/// What the server did with a proposal. `disputed` is SUCCESS with findings —
/// the claim was recorded and a gate disputed it; it is never dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposeOutcome {
    pub claim_id: String,
    /// `accepted` | `disputed`.
    pub status: String,
    pub head_seq: u64,
    /// Rule ids of the gate findings the write drew.
    pub findings: Vec<String>,
}

/// What Matrix needs from munarium-server. Deliberately small: every method
/// here is a capability the plan justifies, and nothing else is reachable.
#[async_trait]
pub trait ServerClient: Send + Sync {
    /// `GET /version` — used at startup for the `TARGET_SERVER_VERSION` check.
    async fn server_version(&self) -> Result<String>;

    /// Seal an artifact. Under the 1 MiB inline cap this is ONE round-trip,
    /// which is the whole reason the turn path can afford to seal at all.
    async fn seal_evidence(
        &self,
        manifest: &EvidenceManifest,
        bytes: &[u8],
        idempotency_key: Option<&str>,
    ) -> Result<String>;

    /// `GET /v1/evidence/{id}` — access-checked manifest read.
    async fn get_evidence(&self, evidence_id: &str) -> Result<EvidenceManifest>;

    /// Upload rendered record documents through the bulk upload sessions plane.
    async fn bulk_upload(&self, label: &str, documents: &[UploadDocument])
        -> Result<UploadOutcome>;

    /// Accepted facts at a pinned seq — the mode-C comparison read.
    async fn slice_facts(
        &self,
        version_id: &str,
        as_of_seq: Option<u64>,
    ) -> Result<Vec<LedgerFact>>;

    /// The lineage head, so a comparison can pin itself.
    async fn head_seq(&self, version_id: &str) -> Result<u64>;

    /// File a warn-only discrepancy finding.
    async fn file_finding(&self, req: &FindingRequest) -> Result<String>;

    /// Propose a claim into a lineage. `idempotency_key` is the
    /// content identity Matrix computed; the server's own idempotency store
    /// returns the first outcome for a replay. The only trait method that can
    /// change canon, which is why authoritative mode is gated per mapping.
    async fn propose_claim(
        &self,
        req: &ProposeClaimRequest,
        idempotency_key: &str,
    ) -> Result<ProposeOutcome>;
}
