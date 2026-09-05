// SPDX-License-Identifier: Apache-2.0
//! # munarium-client
//!
//! Official Rust client for munarium-server: one plane interface, two transports
//! (REST + gRPC), typed errors keyed on the problem-slug registry, and the
//! head-conflict write loop built in.
//!
//! ```no_run
//! # async fn demo() -> munarium_client::Result<()> {
//! use munarium_client::{MunariumClient, MunariumClientOptions};
//!
//! let client = MunariumClient::rest(MunariumClientOptions::new("http://127.0.0.1:8080")
//!     .token("devtoken"))?;
//! let v = client.commands.create_version(Default::default(), None).await?;
//! let head = client.query.head(&v.version_id).await?;
//! # Ok(()) }
//! ```
//!
//! ## The invariants this client encodes
//! 1. **Disputed ≠ error.** A gate-blocked claim comes back as SUCCESS with
//!    `claim.status == disputed` + findings (see [`planes::ClaimOutcome`]).
//! 2. **Head conflicts are normal.** [`MunariumClient::propose_claim_with_retry`]
//!    re-reads, rebuilds, retries with a fresh idempotency key per attempt.
//! 3. **One pin bounds everything.** `as_of_seq` threads through every query.
//! 4. **Every retrieval answer carries a ProvenanceEnvelope** — required,
//!    non-optional on [`munarium_api_types::SearchResponse`].
//! 5. **Append-only.** There are no update/delete methods; corrections name
//!    `supersedes_id` explicitly.
//! 6. **Idempotency keys** are auto-generated per command call and
//!    caller-overridable.

pub mod error;
pub mod planes;

#[cfg(feature = "grpc")]
pub mod grpc;
#[cfg(feature = "rest")]
pub mod rest;
#[cfg(feature = "rest")]
pub(crate) mod sse;

pub(crate) mod retry;

pub use error::{MunariumError, Result};
pub use planes::{
    chunks_from_bytes, chunks_from_vec, put_source_bytes, AuditQuery, AuthoringPlane, ChunkSource,
    ClaimOutcome, CommandsPlane, ContextQuery, EvidencePlane, EvidenceRowsQuery, FactsQuery,
    FindingsQuery, IdemKey, IngestPlane, MetaPlane, ProvidersPlane, QueryPlane, ReportsPlane,
    RetrievalPlane, RunbooksPlane, ServerVersionInfo, SessionsPlane, SourceMeta, TokenListQuery,
    TokensPlane, TurnStream, TurnStreamEvent, UsageQuery, ValidateOptions,
    BULK_MAX_FILES_PER_CHUNK,
};

/// Re-exported wire models — the server's own JSON-casing truth.
pub use munarium_api_types as dto;

use std::sync::Arc;
use std::time::Duration;

/// The server version this client tracks (lockstep with the repo workspace).
pub const TARGET_SERVER_VERSION: &str = "1.0.0";

/// Connection + behavior options, shared by both transports.
#[derive(Debug, Clone)]
pub struct MunariumClientOptions {
    /// REST base URL (`http://host:8080`) or gRPC endpoint (`http://host:50051`).
    pub endpoint: String,
    /// Bearer token — a static token, or a capability JWT for the data plane;
    /// None only works against `MUNARIUM_AUTH_MODE=disabled`.
    pub token: Option<String>,
    /// The acting end-user id (uid contract). Sent as `X-Munarium-Uid` (REST)
    /// / `munarium-uid` metadata (gRPC) on every request. Required by servers
    /// running `MUNARIUM_REQUIRE_UID=true` (the default); when the bearer is a
    /// capability JWT it must equal the token's `sub`.
    pub uid: Option<String>,
    pub connect_timeout: Duration,
    /// Per-request deadline (streaming ingest is exempt).
    pub request_timeout: Duration,
    /// Extra attempts for READS on transport errors / overload (default 2).
    /// Commands re-send the SAME idempotency key, and only when the request
    /// provably never reached the server (a connect-phase failure) or the
    /// server shed it before executing — the server records an idempotency
    /// key AFTER a command completes, so re-sending a possibly-delivered
    /// command could execute it twice. On gRPC a transport failure is never
    /// provably undelivered (a failed lazy reconnect and a broken
    /// established stream both surface as UNAVAILABLE), so only the typed
    /// pre-execution shed (`overloaded`) re-sends there.
    pub read_retries: u32,
}

impl MunariumClientOptions {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            token: None,
            uid: None,
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(30),
            read_retries: 2,
        }
    }

    pub fn token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Set the acting end-user id (the uid contract).
    pub fn uid(mut self, uid: impl Into<String>) -> Self {
        self.uid = Some(uid.into());
        self
    }

    pub fn request_timeout(mut self, d: Duration) -> Self {
        self.request_timeout = d;
        self
    }

    pub fn connect_timeout(mut self, d: Duration) -> Self {
        self.connect_timeout = d;
        self
    }

    pub fn read_retries(mut self, n: u32) -> Self {
        self.read_retries = n;
        self
    }
}

/// Options for the head-conflict write loop.
#[derive(Debug, Clone)]
pub struct WriteLoopOptions {
    /// Max attempts including the first (default 3).
    pub max_attempts: u32,
}

impl Default for WriteLoopOptions {
    fn default() -> Self {
        Self { max_attempts: 3 }
    }
}

/// The facade: eleven plane sub-clients over one connection + auth config.
pub struct MunariumClient {
    pub commands: Arc<dyn CommandsPlane>,
    pub query: Arc<dyn QueryPlane>,
    pub ingest: Arc<dyn IngestPlane>,
    pub retrieval: Arc<dyn RetrievalPlane>,
    pub runbooks: Arc<dyn RunbooksPlane>,
    pub providers: Arc<dyn ProvidersPlane>,
    /// Multiturn sessions + the streaming turn plane.
    pub sessions: Arc<dyn SessionsPlane>,
    /// Capability-token mint/audit/revoke (mgmt role).
    pub tokens: Arc<dyn TokensPlane>,
    /// Management reports (mgmt role; REST-only).
    pub reports: Arc<dyn ReportsPlane>,
    /// Guided runbook authoring (REST-only).
    pub authoring: Arc<dyn AuthoringPlane>,
    /// Sealed evidence READS (REST-only). Resolve an
    /// `[evidence/<id>#<row>]` citation to what an answer was computed from.
    /// Sealing is not here on purpose — see [`planes::EvidencePlane`].
    pub evidence: Arc<dyn EvidencePlane>,
    meta: Arc<dyn MetaPlane>,
}

/// The one head-conflict loop both write helpers expand to. `actual == 0`
/// in a conflict means the transport carried no structured seqs (stripped
/// details) — re-read the head instead of trusting the sentinel.
macro_rules! head_retry_loop {
    ($self:expr, $version_id:expr, $opts:expr, |$head:ident| $send:expr) => {{
        let mut $head = $self.query.head($version_id).await?;
        let mut attempt = 0;
        loop {
            attempt += 1;
            match $send.await {
                Ok(resp) => return Ok(resp),
                Err(MunariumError::HeadConflict { actual, .. }) if attempt < $opts.max_attempts => {
                    retry::jitter_sleep(attempt).await;
                    $head = if actual > 0 {
                        actual
                    } else {
                        $self.query.head($version_id).await?
                    };
                }
                Err(e) => return Err(e),
            }
        }
    }};
}

impl MunariumClient {
    /// REST transport (`:8080` in the demo posture; `:443` behind gateways).
    #[cfg(feature = "rest")]
    pub fn rest(options: MunariumClientOptions) -> Result<Self> {
        let t = Arc::new(rest::RestTransport::new(options)?);
        Ok(Self {
            commands: t.clone(),
            query: t.clone(),
            ingest: t.clone(),
            retrieval: t.clone(),
            runbooks: t.clone(),
            providers: t.clone(),
            sessions: t.clone(),
            tokens: t.clone(),
            reports: t.clone(),
            authoring: t.clone(),
            evidence: t.clone(),
            meta: t,
        })
    }

    /// Direct gRPC transport (`:50051`, or `:443` via the gateway plane).
    /// Plaintext is used exactly when the endpoint scheme is `http://`.
    #[cfg(feature = "grpc")]
    pub async fn grpc(options: MunariumClientOptions) -> Result<Self> {
        let t = Arc::new(grpc::GrpcTransport::connect(options).await?);
        Ok(Self {
            commands: t.clone(),
            query: t.clone(),
            ingest: t.clone(),
            retrieval: t.clone(),
            runbooks: t.clone(),
            providers: t.clone(),
            sessions: t.clone(),
            tokens: t.clone(),
            reports: t.clone(),
            authoring: t.clone(),
            evidence: t.clone(),
            meta: t,
        })
    }

    /// The head-conflict write loop (invariant #2): read head → build the
    /// request via `build(head)` with `expected_head = Some(head)` → propose
    /// with a FRESH idempotency key → on `HeadConflict` back off (jittered
    /// 50–500 ms) and rebuild against the actual head. Never retries other
    /// errors.
    pub async fn propose_claim_with_retry<F>(
        &self,
        version_id: &str,
        mut build: F,
        opts: WriteLoopOptions,
    ) -> Result<dto::ProposeClaimResponse>
    where
        F: FnMut(u64) -> dto::ProposeClaimRequest + Send,
    {
        head_retry_loop!(self, version_id, opts, |head| {
            let mut req = build(head);
            req.expected_head = Some(head);
            self.commands.propose_claim(version_id, req, None)
        })
    }

    /// GET /version — the served name + version (REST transport only;
    /// gRPC clients get the typed `Unsupported`). Compare against
    /// [`TARGET_SERVER_VERSION`] to catch a stale deploy early.
    pub async fn server_version(&self) -> Result<planes::ServerVersionInfo> {
        self.meta.server_version().await
    }

    /// Generalized `expected_head` loop for batched writes.
    pub async fn append_events_with_retry<F>(
        &self,
        version_id: &str,
        mut build: F,
        opts: WriteLoopOptions,
    ) -> Result<dto::AppendEventsResponse>
    where
        F: FnMut(u64) -> dto::AppendEventsRequest + Send,
    {
        head_retry_loop!(self, version_id, opts, |head| {
            let mut req = build(head);
            req.expected_head = Some(head);
            self.commands.append_events(version_id, req, None)
        })
    }
}

pub(crate) fn new_idem_key() -> String {
    uuid::Uuid::new_v4().to_string()
}
