// SPDX-License-Identifier: Apache-2.0
//! The ten plane interfaces — one surface, two transports. Request/response
//! models are the `munarium-api-types` DTOs (the server's own JSON-casing truth),
//! so the client can never drift from the wire contract.

use crate::error::Result;
use async_trait::async_trait;
use futures_core::stream::BoxStream;
use munarium_api_types as dto;
use std::sync::Arc;

/// An explicit idempotency key for a command. `None` = the client generates a
/// fresh UUIDv4. Pass your own key to get true replay semantics across YOUR
/// retries (same key + same body → the recorded response).
pub type IdemKey = Option<String>;

/// Query options for `facts`.
#[derive(Debug, Clone, Default)]
pub struct FactsQuery {
    /// Exact or `prefix.%` scope match.
    pub scope_prefix: Option<String>,
    /// Point-in-time pin (bounds facts/anchors/promises/counters together).
    /// Pins start at 1; `Some(0)` is rejected on gRPC (proto3 sentinel).
    pub as_of_seq: Option<u64>,
    /// Status filter. Empty = server default (accepted only).
    pub statuses: Vec<dto::ClaimStatusDto>,
    /// Keep the NEWEST n.
    pub limit: Option<usize>,
}

/// Query options for `findings`.
#[derive(Debug, Clone, Default)]
pub struct FindingsQuery {
    /// Point-in-time pin: only findings whose write settled at or before it.
    pub as_of_seq: Option<u64>,
    /// info | warn | block (the server rejects anything else — typed).
    pub severity: Option<String>,
    /// Exact rule id, e.g. "gate.ledger-conflict".
    pub rule_id: Option<String>,
    pub limit: Option<usize>,
}

/// Query options for `compose_context`.
#[derive(Debug, Clone, Default)]
pub struct ContextQuery {
    pub scope: Option<String>,
    /// Token budget; digest tiers degrade before fact trimming.
    pub budget_tokens: Option<u64>,
    pub fact_limit: Option<usize>,
    pub as_of_seq: Option<u64>,
}

/// Metadata for a content-addressed source upload.
#[derive(Debug, Clone, Default)]
pub struct SourceMeta {
    /// Declared hex sha-256; the server verifies it before commit.
    /// Empty = server hashes without verification.
    pub declared_sha256: String,
    /// Defaults to application/octet-stream.
    pub media_type: Option<String>,
    /// REQUIRED: the source's logical path. It is the source's identity and
    /// its object-store location, and it is what a runbook collection's
    /// `filenamePrefix` matches against — a source without one could never be
    /// bound. The server rejects an absent or empty filename.
    pub filename: Option<String>,
    pub shape_ref: Option<String>,
}

/// Writes through the deterministic gates. Idempotency-Key required — the
/// client auto-generates one when `idem` is None.
#[async_trait]
pub trait CommandsPlane: Send + Sync {
    async fn create_version(
        &self,
        req: dto::CreateVersionRequest,
        idem: IdemKey,
    ) -> Result<dto::CreateVersionResponse>;

    /// Propose one claim. A gate-blocked claim is NOT an error: the response
    /// carries `claim.status == disputed` plus the findings (recorded, never
    /// dropped). Only `expected_head` mismatch errors (`HeadConflict`).
    async fn propose_claim(
        &self,
        version_id: &str,
        req: dto::ProposeClaimRequest,
        idem: IdemKey,
    ) -> Result<dto::ProposeClaimResponse>;

    /// Batched claims, gated as ONE candidate unit.
    async fn append_events(
        &self,
        version_id: &str,
        req: dto::AppendEventsRequest,
        idem: IdemKey,
    ) -> Result<dto::AppendEventsResponse>;

    async fn open_promise(
        &self,
        version_id: &str,
        req: dto::OpenPromiseRequest,
        idem: IdemKey,
    ) -> Result<dto::PromiseDto>;

    async fn fulfill_promise(
        &self,
        version_id: &str,
        key: &str,
        idem: IdemKey,
    ) -> Result<dto::FulfillPromiseResponse>;

    async fn lock_anchor(
        &self,
        version_id: &str,
        req: dto::LockAnchorRequest,
        idem: IdemKey,
    ) -> Result<dto::AnchorDto>;

    async fn record_counts(
        &self,
        version_id: &str,
        req: dto::RecordCountsRequest,
        idem: IdemKey,
    ) -> Result<()>;

    /// Upsert by definition — the one command outside REST idempotency
    /// scope. The digest's own `version_id` names the lineage.
    async fn upsert_digest(&self, digest: dto::DigestDto) -> Result<()>;
}

/// Point-in-time reads. One `as_of_seq` pin bounds facts, anchors, promises
/// (post-pin fulfillment reads back open), and counters together; digests are
/// rebuilt under a pin, never served stored.
#[async_trait]
pub trait QueryPlane: Send + Sync {
    async fn head(&self, version_id: &str) -> Result<u64>;
    async fn get_claim(&self, claim_id: &str) -> Result<dto::GetClaimResponse>;
    async fn facts(&self, version_id: &str, q: FactsQuery) -> Result<dto::FactsResponse>;
    async fn lineage(&self, version_id: &str) -> Result<dto::LineageResponse>;
    async fn anchors(
        &self,
        version_id: &str,
        as_of_seq: Option<u64>,
    ) -> Result<dto::AnchorsResponse>;
    async fn promises(
        &self,
        version_id: &str,
        as_of_seq: Option<u64>,
        status: Option<&str>,
    ) -> Result<dto::PromisesResponse>;
    async fn counters(
        &self,
        version_id: &str,
        as_of_seq: Option<u64>,
    ) -> Result<dto::CountersResponse>;
    async fn digests(&self, version_id: &str) -> Result<dto::DigestsResponse>;
    /// Persisted gate findings with the head seq each write settled at
    /// (2026-08-17). REST-only today — the gRPC client returns `Unsupported`
    /// (QueryService has no findings RPC).
    async fn findings(&self, version_id: &str, q: FindingsQuery) -> Result<dto::FindingsResponse>;
    async fn compose_context(
        &self,
        version_id: &str,
        q: ContextQuery,
    ) -> Result<dto::ComposedContextDto>;
}

/// Content-addressed source intake.
#[async_trait]
pub trait IngestPlane: Send + Sync {
    /// Upload source bytes. The source is a FACTORY, not a stream: uploads
    /// are idempotent by content address, so a transient failure is safe to
    /// retry — and retrying needs a fresh stream. Re-uploading known bytes
    /// returns `already_existed: true`.
    async fn put_source(
        &self,
        meta: SourceMeta,
        chunks: ChunkSource,
    ) -> Result<dto::PutSourceResponse>;

    /// Record the ingest event binding a stored source into a lineage.
    async fn record_ingest(
        &self,
        version_id: &str,
        req: dto::RecordIngestRequest,
    ) -> Result<dto::RecordIngestResponse>;

    /// Ingest ONE document through the file plane (base64 body,
    /// declarative collection auto-binding via runbook `sources:` matchers,
    /// or the explicit `collections` list). Requires the `ingest` scope on a
    /// capability token (rw static tokens pass). gRPC twin: `IngestFiles`
    /// with a single entry.
    async fn ingest(&self, file: dto::IngestFileRequest) -> Result<dto::IngestResultDto>;

    /// Batch ingest (1..=500 files) with per-item outcomes — one failed file
    /// does not fail the batch; check each result's `error`.
    async fn ingest_batch(&self, req: dto::IngestBatchRequest) -> Result<dto::IngestBatchResponse>;

    /// Open a bulk upload session from a manifest. The response's `needed`
    /// is the upload work list — entries already stored byte-identically are
    /// skipped, so an identical re-run uploads nothing. REST-only.
    async fn bulk_open(&self, req: dto::BulkOpenRequest) -> Result<dto::BulkOpenResponse>;

    /// Upload one chunk of files (at most [`BULK_MAX_FILES_PER_CHUNK`] — a
    /// larger list is a typed client-side error, not a server round-trip)
    /// into an open bulk session. Per-document idempotent. REST-only.
    async fn bulk_chunk(
        &self,
        bulk_id: &str,
        files: Vec<dto::IngestFileRequest>,
    ) -> Result<dto::BulkChunkResponse>;

    /// Session progress; `include_needed` adds the resume work list. REST-only.
    async fn bulk_status(
        &self,
        bulk_id: &str,
        include_needed: bool,
    ) -> Result<dto::BulkStatusResponse>;

    /// Close the session against its manifest: `completed` when every entry
    /// is stored and hash-matched, else `incomplete` (session stays open)
    /// with the missing/mismatched lists. REST-only.
    async fn bulk_complete(&self, bulk_id: &str) -> Result<dto::BulkCompleteResponse>;

    /// Metadata for one stored source (never the bytes). REST-only.
    async fn get_source(&self, source_id: &str) -> Result<dto::SourceInfoDto>;
}

/// The server's per-chunk file cap on `bulk_chunk` (and batch ingest).
pub const BULK_MAX_FILES_PER_CHUNK: usize = 500;

/// Reject an over-cap file list before it ships 256 MiB the server will
/// refuse. `what` names the calling surface ("batch" / "bulk chunk") so the
/// error speaks the API the caller actually used.
pub(crate) fn check_bulk_chunk_size(what: &str, n: usize) -> crate::Result<()> {
    if n == 0 || n > BULK_MAX_FILES_PER_CHUNK {
        return Err(crate::MunariumError::InvalidInput {
            detail: format!("{what} must carry 1..={BULK_MAX_FILES_PER_CHUNK} files (got {n})"),
        });
    }
    Ok(())
}

/// A replayable chunk source: called once per upload attempt, so the
/// transport can retry a transient failure with a fresh stream.
/// Frame size for the byte/vec chunk helpers — comfortably under tonic's
/// 4 MiB default max message size, and a reasonable HTTP write unit.
pub const CHUNK_BYTES: usize = 1024 * 1024;

pub type ChunkSource = Arc<dyn Fn() -> BoxStream<'static, Vec<u8>> + Send + Sync>;

/// Build a [`ChunkSource`] from in-memory bytes (cheaply cloned per attempt).
pub fn chunks_from_bytes(bytes: Vec<u8>) -> ChunkSource {
    let bytes = Arc::new(bytes);
    Arc::new(move || {
        let bytes = bytes.clone();
        // Split into wire-sized frames: one whole-payload item would exceed
        // tonic's default 4 MiB max message size on the gRPC transport.
        let chunks: Vec<Vec<u8>> = bytes
            .chunks(CHUNK_BYTES)
            .map(<[u8]>::to_vec)
            .collect::<Vec<_>>();
        Box::pin(futures_util::stream::iter(chunks))
    })
}

/// Build a [`ChunkSource`] from a pre-split chunk list.
pub fn chunks_from_vec(chunks: Vec<Vec<u8>>) -> ChunkSource {
    let chunks = Arc::new(chunks);
    Arc::new(move || {
        let chunks = chunks.clone();
        Box::pin(futures_util::stream::iter(chunks.as_ref().clone()))
    })
}

/// Convenience: upload a whole in-memory source.
pub async fn put_source_bytes(
    ingest: &dyn IngestPlane,
    meta: SourceMeta,
    bytes: Vec<u8>,
) -> Result<dto::PutSourceResponse> {
    ingest.put_source(meta, chunks_from_bytes(bytes)).await
}

/// Hybrid search over versioned immutable indexes. Every answer carries a
/// `ProvenanceEnvelope` — surface it, don't hide it. Postgres store only.
#[async_trait]
pub trait RetrievalPlane: Send + Sync {
    async fn search(&self, req: dto::SearchRequest) -> Result<dto::SearchResponse>;
    async fn index_status(&self, shape_ref: &str) -> Result<dto::IndexStatusResponse>;
    /// Side-by-side build + atomic flip. REST-only today — the gRPC client
    /// returns `Unsupported` (no BuildIndex RPC exists).
    async fn build_index(
        &self,
        shape_ref: &str,
        version_id: Option<&str>,
    ) -> Result<dto::IndexStatusResponse>;

    /// Create-or-update a compartmentalized collection. There is no
    /// delete anywhere — collections retire softly.
    async fn create_collection(
        &self,
        req: dto::CreateCollectionRequest,
    ) -> Result<dto::CollectionDto>;
    async fn list_collections(&self) -> Result<dto::CollectionsResponse>;
    async fn get_collection(&self, id: &str) -> Result<dto::CollectionDto>;
}

/// Shapes + runbooks: declarative YAML in, checkpointed step machines out.
#[async_trait]
pub trait RunbooksPlane: Send + Sync {
    /// Apply a Shape; `version_id` records the publication as a ledger claim
    /// and returns its `event_id`.
    async fn apply_shape(
        &self,
        yaml: &str,
        version_id: Option<&str>,
    ) -> Result<dto::ApplyShapeResponse>;
    async fn apply_runbook(&self, yaml: &str) -> Result<dto::ApplyRunbookResponse>;
    /// Start a run; pauses at `awaiting_approval` gates.
    async fn run_runbook(
        &self,
        name: &str,
        version_id: Option<&str>,
    ) -> Result<dto::RunbookRunResponse>;
    async fn get_run(&self, run_id: &str) -> Result<dto::RunStatusResponse>;
    async fn approve_step(&self, run_id: &str, ordinal: u32) -> Result<dto::RunbookRunResponse>;

    /// Every hosted runbook (all versions) with per-collection access
    /// requirements.
    async fn list(&self, include_removed: bool) -> Result<dto::RunbooksResponse>;
    /// One runbook's collections, sibling versions, models block, and
    /// retrieval knobs. `name` is a bare name (latest) or exact name@version.
    async fn get_info(&self, name: &str) -> Result<dto::RunbookInfoResponse>;
    /// Deterministic validation findings; `opts.suggest` adds AI improvement
    /// suggestions (a BYOK provider call, policy-gated override).
    async fn validate(
        &self,
        yaml: &str,
        opts: ValidateOptions,
    ) -> Result<dto::ValidateRunbookResponse>;
    /// First pass of the double-pass soft removal: returns the `removal_id`
    /// to present to `remove_confirm` within the TTL. `name` must be an
    /// EXACT name@version.
    async fn remove_request(&self, name: &str) -> Result<dto::RemovalRequestResponse>;
    /// Second pass: confirm with the removal_id. Removal is visibility-only —
    /// yaml, run history, collections, and index data are all retained.
    async fn remove_confirm(
        &self,
        name: &str,
        removal_id: &str,
    ) -> Result<dto::RemovalConfirmResponse>;

    /// Apply (upsert) a chronology-rules asset — the sixth gate's arming
    /// surface (2026-08-17). REST-only; text/yaml like shapes.
    async fn apply_chronology_rules(&self, yaml: &str)
        -> Result<dto::ApplyChronologyRulesResponse>;
    /// The applied rules YAML back, verbatim. REST-only.
    async fn get_chronology_rules(&self, name: &str) -> Result<String>;
}

/// Options for `runbooks.validate`.
#[derive(Debug, Clone, Default)]
pub struct ValidateOptions {
    /// Add AI-assisted suggestions (spends provider tokens).
    pub suggest: bool,
    /// Model override for the suggestion pass (policy-gated).
    pub provider: Option<String>,
    pub model: Option<String>,
    /// fast | capable | frontier
    pub tier: Option<String>,
}

/// BYOK provider gateway. `complete`/`embed` accept the reserved config name
/// `default` to engage the server's default rule (anthropic → openai →
/// openrouter, first family with a usable credential), an optional
/// `provider` family override, and (complete only) a `tier` of `fast`,
/// `capable` or `frontier` resolved to the built-in tier models server-side.
/// Explicit `model` always wins and may name any model the provider supports.
#[async_trait]
pub trait ProvidersPlane: Send + Sync {
    async fn apply_config(&self, yaml: &str) -> Result<dto::ApplyProviderConfigResponse>;
    async fn health(&self, name: &str) -> Result<dto::ProviderHealthResponse>;
    /// Live probe of the server's nine built-in default models (three provider
    /// families × three tiers) — spends real provider tokens. REST-only: the
    /// gRPC client returns `Unsupported` (no HealthAi RPC exists).
    async fn health_ai(&self) -> Result<dto::HealthAiResponse>;
    async fn complete(
        &self,
        name: &str,
        req: dto::CompleteRequest,
    ) -> Result<dto::CompleteResponse>;
    async fn embed(&self, name: &str, req: dto::EmbedRequest) -> Result<dto::EmbedResponse>;

    /// Free disclosure of every provider config visible to the tenant —
    /// applied configs plus synthesized env defaults, each with its resolved
    /// fast/capable/frontier tier models and `credential_ok`. Zero provider
    /// calls; the credential itself is never echoed. REST-only
    /// (`GET /v1/providers`).
    async fn list(&self) -> Result<dto::ProviderListResponse>;

    /// The effective per-call output-token budgets (`max_tokens`) for the
    /// caller's tenant and where they come from (`GET /v1/max-tokens`). Any
    /// authenticated role — the numbers shape spend, they are not secrets.
    /// `source` is `tenant` after a replacement through `replace_max_tokens`,
    /// else `environment` (the process's `MUNARIUM_MAX_TOKENS_*` variables
    /// over the built-ins), and `updated_at` is present only for `tenant`.
    /// The budgets are flattened beside `source`, so `resp.budgets` is a
    /// ready-made `replace_max_tokens` body. REST-only: the gRPC client
    /// returns `Unsupported` (no MaxTokens RPC exists).
    async fn max_tokens(&self) -> Result<dto::MaxTokensResponse>;

    /// Replace the tenant's WHOLE budget set (`POST /v1/max-tokens`). There
    /// is no partial update: all eight fields are required on the wire and
    /// each is range-checked server-side (`turn_completion` 256..=16384,
    /// `query_expansion` 32..=512, the rest 1..=65536) — a missing or
    /// out-of-range field is the typed `InvalidInput` (400 `invalid-input`)
    /// and nothing changes. Static **rw** role only (`Forbidden` otherwise),
    /// like provider configs and runbooks. The answer is the shape
    /// `max_tokens` returns, with `source: tenant`. Sent once, never
    /// auto-retried. REST-only: the gRPC client returns `Unsupported`.
    async fn replace_max_tokens(
        &self,
        budgets: &dto::MaxTokensBudgets,
    ) -> Result<dto::MaxTokensResponse>;
}

/// One item on the streaming turn plane: progress events at real stage
/// boundaries, then exactly one `Done` carrying the full [`dto::TurnResponse`].
/// A server-side failure mid-stream arrives as the stream's `Err` item
/// (decoded through the standard problem registry) and ends it.
///
/// The hierarchy stages (`profile`, `layer_start`, `layer_source`,
/// `layer_complete`, `coverage`, `compose`) appear only when the turn
/// runs a research profile; a turn without one emits the same stage sequence
/// it always has. A stage this build cannot name is skipped rather than
/// failing the stream (see `classify_turn_event`), so a newer server's
/// progress never breaks an older client.
#[derive(Debug, Clone)]
pub enum TurnStreamEvent {
    Progress(dto::TurnProgressEvent),
    // Boxed: a TurnResponse dwarfs a progress event, and the enum flows
    // through every stream item (clippy::large_enum_variant).
    Done(Box<dto::TurnResponse>),
}

/// The stream `sessions.turn_stream` yields.
pub type TurnStream = BoxStream<'static, Result<TurnStreamEvent>>;

/// GET /version body — the server's own `VersionInfo` DTO (shared wire
/// type since C11; the alias keeps this crate's exported name stable).
pub type ServerVersionInfo = dto::VersionInfo;

/// Meta routes outside the ten-plane surface.
#[async_trait]
pub trait MetaPlane: Send + Sync {
    /// GET /version — the server's name + workspace version, unauthenticated.
    /// Handy for asserting the [`crate::TARGET_SERVER_VERSION`] handshake.
    /// REST-only: gRPC has no version RPC (use server reflection there).
    async fn server_version(&self) -> Result<ServerVersionInfo>;
}

/// Multiturn sessions over a runbook's access-permitted collections.
/// Auth is the data plane's: a capability JWT with the `query` scope (or a
/// static token), and the uid contract applies to every call.
#[async_trait]
pub trait SessionsPlane: Send + Sync {
    /// Open a session on a runbook (bare name = latest non-removed version,
    /// or exact name@version). The response echoes the collections the
    /// caller's access level/compartments actually permit.
    async fn create(&self, runbook_name: &str) -> Result<dto::CreateSessionResponse>;

    /// One retrieval turn (+ optional completion when the runbook declares
    /// one). `req.model_override` is honored only under the runbook's
    /// `models.allowOverrides` policy — a disallowed override draws the
    /// typed `override-not-allowed` error, never a silent downgrade.
    ///
    /// `req.research_profile` runs the turn through a named
    /// evidence hierarchy and fills `resp.hierarchy` with the decision. Left
    /// `None`, the request and the response are byte-identical to what this
    /// client sent and parsed before the field existed — which is the point
    /// of the field being optional on both ends.
    async fn turn(&self, session_id: &str, req: dto::TurnRequest) -> Result<dto::TurnResponse>;

    /// The same turn, streamed: N progress events at real stage boundaries,
    /// then exactly one [`TurnStreamEvent::Done`]. Pre-stream failures
    /// return `Err` here; mid-stream failures arrive as the stream's `Err`
    /// item. A stream that ends without a terminal event is a typed
    /// transport error — never a silent success. REST-only: the gRPC client
    /// returns `Unsupported` (SessionService has no streaming RPC).
    async fn turn_stream(&self, session_id: &str, req: dto::TurnRequest) -> Result<TurnStream>;

    /// The session envelope + stored turn transcript.
    async fn get(&self, session_id: &str) -> Result<dto::SessionResponse>;

    /// Close the session (a write — `ro` tokens are refused). Idempotent:
    /// closing a closed/expired session returns its state unchanged.
    async fn close(&self, session_id: &str) -> Result<dto::SessionResponse>;
}

/// Capability-token management (mgmt role). "Tokens" here are the
/// short-lived end-user capability JWTs — not the bearer this client
/// authenticates with.
#[async_trait]
pub trait TokensPlane: Send + Sync {
    /// Mint a capability JWT for an authenticated end user. The token
    /// material is returned ONCE and never persisted server-side.
    async fn mint(&self, req: dto::IssueTokenRequest) -> Result<dto::IssueTokenResponse>;
    /// The issuance audit — metadata only, never token material.
    async fn list(&self, q: TokenListQuery) -> Result<dto::TokensResponse>;
    /// Deny-list a token by jti. Note `revocation_check_enabled` in the
    /// response: the list is only consulted when the server enables it.
    async fn revoke(&self, jti: &str) -> Result<dto::RevokeTokenResponse>;
}

/// Filters for `tokens.list`.
#[derive(Debug, Clone, Default)]
pub struct TokenListQuery {
    pub uid: Option<String>,
    /// true = unexpired + unrevoked only.
    pub active: Option<bool>,
}

/// Management reports over the interactions audit trail (mgmt role) —
/// plus the two operator views, which read the turn transcript and
/// this instance's live Matrix state rather than the audit trail.
/// REST-only: the gRPC client returns `Unsupported` on every method
/// (AdminService.Usage is declared but UNIMPLEMENTED — not wired).
#[async_trait]
pub trait ReportsPlane: Send + Sync {
    async fn usage(&self, q: UsageQuery) -> Result<dto::UsageResponse>;
    async fn audit(&self, q: AuditQuery) -> Result<dto::AuditResponse>;
    /// Model-spend token rollup (dollar pricing lives upstream).
    async fn cost(&self, from: Option<&str>, to: Option<&str>) -> Result<dto::CostResponse>;
    /// Bucketed request/error/latency series. `window`: 1h | 24h | 7d | 30d
    /// (server default 24h); `plane`: rest | grpc.
    async fn timeseries(
        &self,
        window: Option<&str>,
        plane: Option<&str>,
    ) -> Result<dto::TimeseriesResponse>;
    async fn endpoints(
        &self,
        window: Option<&str>,
        limit: Option<i64>,
    ) -> Result<dto::EndpointsResponse>;
    async fn runbooks(&self, window: Option<&str>) -> Result<dto::RunbookReportResponse>;
    async fn sessions(&self, window: Option<&str>) -> Result<dto::SessionsReportResponse>;
    /// How the evidence hierarchy behaved. `window`: 24h (default)
    /// | 7d | 30d. The question it answers is "which layer is quietly
    /// refusing?" — a refusing layer's turns still return 200, so nothing
    /// goes red while the served answers get thinner than the runbook
    /// claims. `legacy_turns` counts turns that ran no research profile.
    async fn evidence(&self, window: Option<&str>) -> Result<dto::EvidenceReportResponse>;
    /// Munarium Matrix's health as THIS server sees it. No window:
    /// `configured`/`circuit_open` are current state, not a rate. Read
    /// `configured` first — not wired and wired-but-failing are different
    /// operational facts, and the breaker is per instance, never per tenant.
    async fn matrix(&self) -> Result<dto::MatrixReportResponse>;
}

/// Filters for `reports.usage`.
#[derive(Debug, Clone, Default)]
pub struct UsageQuery {
    /// uid | session | runbook | collection (server default: uid).
    pub group_by: Option<String>,
    /// RFC 3339 window bounds.
    pub from: Option<String>,
    pub to: Option<String>,
}

/// Filters for `reports.audit`.
#[derive(Debug, Clone, Default)]
pub struct AuditQuery {
    pub uid: Option<String>,
    pub session_id: Option<String>,
    pub runbook: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub limit: Option<i64>,
    /// Include the captured request/response bodies (heavy; off by default).
    pub bodies: bool,
    /// Keyset cursor: pass the previous page's `next_before` verbatim.
    pub before: Option<String>,
}

/// Guided runbook authoring: pattern catalog, interview-driven drafts,
/// deterministic validation, optional AI assist, hash-manifested export,
/// and apply. REST-only (no authoring RPCs exist).
///
/// Query options for `evidence_rows`.
#[derive(Debug, Clone, Default)]
pub struct EvidenceRowsQuery {
    /// Zero-based first row. Default 0.
    pub from: Option<usize>,
    /// Rows per page. Default 100, capped server-side at 1000.
    pub limit: Option<usize>,
}

/// The sealed evidence plane — **reads only**.
///
/// Sealing is deliberately absent from this client. An artifact is produced by
/// a connector that computed it (today, Munarium Matrix, through its own thin
/// client) and the manifest it seals is a statement about work that client did.
/// A general-purpose SDK offering `seal_evidence` would invite an application
/// to assert provenance it cannot actually vouch for.
///
/// What an application legitimately needs is the other direction: an answer
/// cites `[evidence/<id>#<row>]`, and the application resolves that citation to
/// show a reader what the number was computed from. That is what these two
/// methods are for.
///
/// Access is checked per artifact against the **session's** clearance, not the
/// sealer's: a citation is readable exactly when the reader dominates the
/// artifact's authorization class. Expect `evidence-forbidden` (403),
/// `evidence-expired` (410, retention purged the bytes — the citation was real)
/// and `evidence-not-committed` (409).
///
/// REST-only: the evidence plane has no gRPC twin in v1, so the gRPC client
/// answers `Unsupported`, exactly as it does for `findings`.
#[async_trait]
pub trait EvidencePlane: Send + Sync {
    /// The manifest, access-checked and audited. Returns the contract's
    /// `EvidenceManifest` verbatim — the route returns it unwrapped, so this
    /// does too.
    async fn evidence(&self, evidence_id: &str) -> Result<serde_json::Value>;

    /// A bounded, audited window over the sealed rows.
    ///
    /// Served for canonical-CSV artifacts only; a Parquet artifact is sealed
    /// and replayable byte-for-byte but the server does not decode it, and
    /// says so rather than pretending the rows are unavailable.
    async fn evidence_rows(
        &self,
        evidence_id: &str,
        q: EvidenceRowsQuery,
    ) -> Result<dto::EvidenceRowsResponse>;
}

/// `delete_draft` is the client surface's ONE delete — it removes a
/// workspace draft (soft), never ledger data, so the append-only invariant
/// is untouched.
#[async_trait]
pub trait AuthoringPlane: Send + Sync {
    async fn list_patterns(&self) -> Result<dto::PatternsResponse>;
    async fn get_pattern(&self, id: &str) -> Result<dto::PatternDetailResponse>;
    async fn create_draft(&self, req: dto::CreateDraftRequest) -> Result<dto::DraftResponse>;
    async fn list_drafts(&self) -> Result<dto::DraftsResponse>;
    async fn get_draft(&self, draft_id: &str) -> Result<dto::DraftResponse>;
    async fn delete_draft(&self, draft_id: &str) -> Result<dto::DraftDeleteResponse>;
    /// Replace the stored answers (and by default re-materialize documents).
    async fn put_answers(
        &self,
        draft_id: &str,
        req: dto::UpdateAnswersRequest,
    ) -> Result<dto::DraftResponse>;
    async fn validate(&self, draft_id: &str) -> Result<dto::DraftValidationResponse>;
    /// AI-assisted drafting pass. NEVER fails the request: a degraded pass
    /// (no provider, budget, parse failure) sets `assist_note` instead.
    async fn assist(
        &self,
        draft_id: &str,
        req: dto::AssistDraftRequest,
    ) -> Result<dto::AssistDraftResponse>;
    /// Self-contained hash-manifested bundle (shapes before runbooks in
    /// `apply_order`).
    async fn export(&self, draft_id: &str) -> Result<dto::ExportDraftResponse>;
    /// Apply the draft's documents to THIS server (validates inline).
    async fn apply(&self, draft_id: &str) -> Result<dto::ApplyDraftResponse>;
}

/// Ergonomics for the disputed-is-not-an-error invariant.
pub trait ClaimOutcome {
    /// True when the claim was recorded but gate-blocked (`disputed`).
    /// This is a SUCCESS state — the governance record, not a failure.
    fn is_disputed(&self) -> bool;
    fn findings(&self) -> &[dto::GateFindingDto];
}

impl ClaimOutcome for dto::ProposeClaimResponse {
    fn is_disputed(&self) -> bool {
        self.claim.status == dto::ClaimStatusDto::Disputed
    }
    fn findings(&self) -> &[dto::GateFindingDto] {
        &self.findings
    }
}

impl ClaimOutcome for dto::AppendEventsResponse {
    fn is_disputed(&self) -> bool {
        self.claims
            .iter()
            .any(|c| c.status == dto::ClaimStatusDto::Disputed)
    }
    fn findings(&self) -> &[dto::GateFindingDto] {
        &self.findings
    }
}

/// The promise statuses the server matches on. It FILTERS an unrecognized
/// value rather than erroring, so an unvalidated typo returns an empty list
/// — a silent wrong answer about outstanding obligations.
pub const PROMISE_STATUSES: [&str; 4] = ["open", "fulfilled", "expired", "violated"];

/// Reject a promise status filter the server would silently drop.
pub(crate) fn check_promise_status(status: Option<&str>) -> crate::Result<()> {
    match status {
        Some(s) if !PROMISE_STATUSES.contains(&s) => Err(crate::MunariumError::InvalidInput {
            detail: format!(
                "unknown promise status '{s}' ({})",
                PROMISE_STATUSES.join(" | ")
            ),
        }),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;

    async fn drain(src: &ChunkSource) -> Vec<u8> {
        src().collect::<Vec<_>>().await.concat()
    }

    #[tokio::test]
    async fn a_chunk_source_replays_identically_every_attempt() {
        // The upload retry rebuilds from the source; if it did not replay,
        // attempt 2 would send fewer bytes than the declared hash covers.
        let src = chunks_from_bytes(b"abcdefghij".to_vec());
        assert_eq!(drain(&src).await, b"abcdefghij");
        assert_eq!(drain(&src).await, b"abcdefghij");
    }

    #[tokio::test]
    async fn bytes_are_framed_under_the_grpc_message_limit() {
        let big = vec![7u8; CHUNK_BYTES * 2 + 5];
        let frames: Vec<Vec<u8>> = chunks_from_bytes(big.clone())().collect().await;
        assert_eq!(
            frames.len(),
            3,
            "one whole-payload frame would exceed 4 MiB"
        );
        assert!(frames.iter().all(|f| f.len() <= CHUNK_BYTES));
        assert_eq!(frames.concat(), big);
    }

    #[test]
    fn unknown_promise_status_is_rejected_not_silently_dropped() {
        // The server FILTERS an unrecognized status and returns an empty
        // list — a silent wrong answer about outstanding obligations.
        let err = check_promise_status(Some("Open")).unwrap_err();
        assert!(matches!(err, crate::MunariumError::InvalidInput { .. }));
        for ok in PROMISE_STATUSES {
            check_promise_status(Some(ok)).unwrap();
        }
        check_promise_status(None).unwrap();
    }
}
