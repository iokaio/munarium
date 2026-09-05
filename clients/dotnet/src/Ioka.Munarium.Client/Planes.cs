// SPDX-License-Identifier: Apache-2.0
// The ten plane interfaces — one surface, two transports. Async-only with a
// CancellationToken on every method. Every REST-only method surfaces the
// typed UnsupportedTransportException on gRPC, never silently.

using System.Text.Json;

namespace Ioka.Munarium.Client;

/// <summary>Writes through the deterministic gates. Idempotency keys are
/// auto-generated per command call when <c>idempotencyKey</c> is null; pass
/// your own key for true replay semantics across YOUR retries.</summary>
public interface ICommandsPlane
{
    Task<string> CreateVersionAsync(
        string? parentVersionId = null, JsonElement? metadata = null,
        string? idempotencyKey = null, CancellationToken ct = default);

    /// <summary>Propose one claim. A gate-blocked claim is NOT an error: the
    /// outcome carries <c>Claim.Status == "disputed"</c> plus the findings
    /// (recorded, never dropped). Only an <c>ExpectedHead</c> mismatch throws
    /// (<see cref="HeadConflictException"/>).</summary>
    Task<ClaimOutcome> ProposeClaimAsync(
        string versionId, ClaimInput claim,
        string? idempotencyKey = null, CancellationToken ct = default);

    /// <summary>Batched claims, gated as ONE candidate unit.</summary>
    Task<EventsOutcome> AppendEventsAsync(
        string versionId, IReadOnlyList<ClaimInput> claims,
        string? candidateText = null, ulong? expectedHead = null,
        string? idempotencyKey = null, CancellationToken ct = default);

    Task<Promise> OpenPromiseAsync(
        string versionId, string key, string kind, string description,
        string? originScope = null, string? dueScope = null,
        string? idempotencyKey = null, CancellationToken ct = default);

    Task<bool> FulfillPromiseAsync(
        string versionId, string key,
        string? idempotencyKey = null, CancellationToken ct = default);

    Task<Anchor> LockAnchorAsync(
        string versionId, string subject, string key, string value,
        string? scopePath = null, JsonElement? evidence = null,
        string? idempotencyKey = null, CancellationToken ct = default);

    Task RecordCountsAsync(
        string versionId, string key, string scopePath, ulong count,
        ulong? budget = null, string? idempotencyKey = null, CancellationToken ct = default);

    /// <summary>Upsert by definition — the digest's own VersionId names the
    /// lineage (outside REST idempotency scope; a command RPC on gRPC).</summary>
    Task UpsertDigestAsync(Digest digest, CancellationToken ct = default);
}

/// <summary>Point-in-time reads. One <c>asOfSeq</c> pin bounds facts,
/// anchors, promises (post-pin fulfillment reads back open), and counters
/// together; digests are rebuilt under a pin, never served stored.</summary>
public interface IQueryPlane
{
    Task<ulong> HeadAsync(string versionId, CancellationToken ct = default);
    Task<ClaimLookup> GetClaimAsync(string claimId, CancellationToken ct = default);
    Task<FactsPage> FactsAsync(
        string versionId, string? scopePrefix = null, ulong? asOfSeq = null,
        IReadOnlyList<string>? statuses = null, int? limit = null,
        CancellationToken ct = default);
    Task<IReadOnlyList<string>> LineageAsync(string versionId, CancellationToken ct = default);
    Task<IReadOnlyList<Anchor>> AnchorsAsync(
        string versionId, ulong? asOfSeq = null, CancellationToken ct = default);
    Task<IReadOnlyList<Promise>> PromisesAsync(
        string versionId, ulong? asOfSeq = null, string? status = null,
        CancellationToken ct = default);
    Task<IReadOnlyList<Counter>> CountersAsync(
        string versionId, ulong? asOfSeq = null, CancellationToken ct = default);
    /// <summary>Stored head rungs; pinned reads REBUILD via
    /// <see cref="ComposeContextAsync"/> with <c>asOfSeq</c>.</summary>
    Task<IReadOnlyList<Digest>> DigestsAsync(string versionId, CancellationToken ct = default);
    Task<ComposedContext> ComposeContextAsync(
        string versionId, string? scope = null, ulong? budgetTokens = null,
        int? factLimit = null, ulong? asOfSeq = null, CancellationToken ct = default);

    /// <summary>Persisted gate findings with the head seq each write settled
    /// at (2026-08-17). <paramref name="severity"/> is info | warn | block
    /// (the server rejects anything else — typed); <paramref name="ruleId"/>
    /// is an exact rule id, e.g. "gate.ledger-conflict". REST-only today —
    /// the gRPC client throws <see cref="UnsupportedTransportException"/>
    /// (QueryService has no findings RPC).</summary>
    Task<IReadOnlyList<StoredFinding>> FindingsAsync(
        string versionId, ulong? asOfSeq = null, string? severity = null,
        string? ruleId = null, int? limit = null, CancellationToken ct = default);
}

/// <summary>A replayable chunk source: invoked once per upload attempt, so
/// the transport can retry a transient failure with a fresh sequence.</summary>
public delegate IAsyncEnumerable<ReadOnlyMemory<byte>> ChunkSource();

/// <summary>Content-addressed source intake.</summary>
public interface IIngestPlane
{
    /// <summary>Upload source bytes. The source is a FACTORY, not a sequence:
    /// uploads are idempotent by content address, so a transient failure is
    /// safe to retry — and retrying needs a fresh sequence. Re-uploading known
    /// bytes returns AlreadyExisted = true.</summary>
    Task<PutSourceResult> PutSourceAsync(
        ChunkSource chunks, string declaredSha256 = "",
        string? mediaType = null, string? filename = null, string? shapeRef = null,
        CancellationToken ct = default);

    Task<RecordIngestResult> RecordIngestAsync(
        string versionId, string contentHash, string? shapeRef = null,
        CancellationToken ct = default);

    /// <summary>The server's per-chunk file cap on <see cref="BulkChunkAsync"/>
    /// (and batch ingest) — an over-cap list is a typed client-side error,
    /// not a server round-trip.</summary>
    const int BulkMaxFilesPerChunk = 500;

    /// <summary>Ingest ONE document through the file plane (base64 body,
    /// declarative collection auto-binding via runbook <c>sources:</c>
    /// matchers, or the explicit collections list). Requires the ingest
    /// scope on a capability token (rw static tokens pass). gRPC twin:
    /// IngestFiles with a single entry.</summary>
    Task<IngestResult> IngestAsync(IngestFile file, CancellationToken ct = default);

    /// <summary>Batch ingest (1..=500 files) with per-item outcomes — one
    /// failed file does not fail the batch; check each result's
    /// <see cref="IngestResult.Error"/>.</summary>
    Task<IReadOnlyList<IngestResult>> IngestBatchAsync(
        IReadOnlyList<IngestFile> files, CancellationToken ct = default);

    /// <summary>Open a bulk upload session from a manifest. The response's
    /// <see cref="BulkOpenResult.Needed"/> is the upload work list — entries
    /// already stored byte-identically are skipped, so an identical re-run
    /// uploads nothing. REST-only.</summary>
    Task<BulkOpenResult> BulkOpenAsync(
        IReadOnlyList<BulkManifestEntry> files, string? label = null,
        CancellationToken ct = default);

    /// <summary>Upload one chunk of files (at most
    /// <see cref="BulkMaxFilesPerChunk"/> — a larger list is a typed
    /// client-side error, not a server round-trip) into an open bulk
    /// session. Per-document idempotent. REST-only.</summary>
    Task<BulkChunkResult> BulkChunkAsync(
        string bulkId, IReadOnlyList<IngestFile> files, CancellationToken ct = default);

    /// <summary>Session progress; <paramref name="includeNeeded"/> adds the
    /// resume work list. REST-only.</summary>
    Task<BulkStatusResult> BulkStatusAsync(
        string bulkId, bool includeNeeded = false, CancellationToken ct = default);

    /// <summary>Close the session against its manifest: "completed" when
    /// every entry is stored and hash-matched, else "incomplete" (session
    /// stays open) with the missing/mismatched lists. REST-only.</summary>
    Task<BulkCompleteResult> BulkCompleteAsync(string bulkId, CancellationToken ct = default);

    /// <summary>Metadata for one stored source (never the bytes). REST-only.</summary>
    Task<SourceInfo> GetSourceAsync(string sourceId, CancellationToken ct = default);
}

/// <summary>Hybrid search over versioned immutable indexes. Every answer
/// carries a <see cref="ProvenanceEnvelope"/> — surface it, don't hide it.
/// Postgres store only.</summary>
public interface IRetrievalPlane
{
    Task<SearchResult> SearchAsync(
        string query, string shapeRef, uint? topK = null, string? indexVersion = null,
        JsonElement? filter = null, CancellationToken ct = default);
    Task<IndexStatus> IndexStatusAsync(string shapeRef, CancellationToken ct = default);
    /// <summary>Side-by-side build + atomic flip. REST-only today — the gRPC
    /// client throws <see cref="UnsupportedTransportException"/>.</summary>
    Task<IndexStatus> BuildIndexAsync(
        string shapeRef, string? versionId = null, CancellationToken ct = default);

    /// <summary>Create-or-update a compartmentalized collection. There
    /// is no delete anywhere — collections retire softly.</summary>
    Task<Collection> CreateCollectionAsync(
        string name, string shapeRef, int accessLevel = 0,
        IReadOnlyList<string>? compartments = null, string? description = null,
        CancellationToken ct = default);

    Task<IReadOnlyList<Collection>> ListCollectionsAsync(CancellationToken ct = default);

    Task<Collection> GetCollectionAsync(string id, CancellationToken ct = default);
}

/// <summary>Shapes + runbooks: declarative YAML in, checkpointed step
/// machines out.</summary>
public interface IRunbooksPlane
{
    /// <summary>Apply a Shape; <c>versionId</c> records the publication as a
    /// ledger claim and returns its EventId.</summary>
    Task<ApplyShapeResult> ApplyShapeAsync(
        string yaml, string? versionId = null, CancellationToken ct = default);
    Task<string> ApplyRunbookAsync(string yaml, CancellationToken ct = default);
    /// <summary>Start a run; pauses at awaiting_approval gates.</summary>
    Task<RunbookRun> RunRunbookAsync(
        string name, string? versionId = null, CancellationToken ct = default);
    Task<RunStatus> GetRunAsync(string runId, CancellationToken ct = default);
    Task<RunbookRun> ApproveStepAsync(string runId, uint ordinal, CancellationToken ct = default);

    /// <summary>Every hosted runbook (all versions) with per-collection
    /// access requirements.</summary>
    Task<IReadOnlyList<RunbookSummary>> ListAsync(
        bool includeRemoved = false, CancellationToken ct = default);

    /// <summary>One runbook's collections, sibling versions, models block,
    /// and retrieval knobs. <paramref name="name"/> is a bare name (latest)
    /// or exact name@version.</summary>
    Task<RunbookInfo> GetInfoAsync(string name, CancellationToken ct = default);

    /// <summary>Deterministic validation findings; <paramref name="suggest"/>
    /// adds AI improvement suggestions (a BYOK provider call, policy-gated
    /// override via <paramref name="provider"/>/<paramref name="model"/>/
    /// <paramref name="tier"/>).</summary>
    Task<RunbookValidation> ValidateAsync(
        string yaml, bool suggest = false, string? provider = null,
        string? model = null, string? tier = null, CancellationToken ct = default);

    /// <summary>First pass of the double-pass soft removal: returns the
    /// removal id to present to <see cref="RemoveConfirmAsync"/> within the
    /// TTL. <paramref name="name"/> must be an EXACT name@version.</summary>
    Task<RemovalRequest> RemoveRequestAsync(string name, CancellationToken ct = default);

    /// <summary>Second pass: confirm with the removal id. Removal is
    /// visibility-only — yaml, run history, collections, and index data are
    /// all retained.</summary>
    Task<RemovalConfirmation> RemoveConfirmAsync(
        string name, string removalId, CancellationToken ct = default);

    /// <summary>Apply (upsert) a chronology-rules asset — the sixth gate's
    /// arming surface (2026-08-17). REST-only; text/yaml like shapes.</summary>
    Task<ChronologyRulesApplied> ApplyChronologyRulesAsync(
        string yaml, CancellationToken ct = default);

    /// <summary>The applied rules YAML back, verbatim. REST-only.</summary>
    Task<string> GetChronologyRulesAsync(string name, CancellationToken ct = default);
}

/// <summary>BYOK provider gateway. The reserved config name <c>default</c>
/// engages the server's default rule (anthropic → openai → openrouter, first
/// family with a usable credential); <c>provider</c> overrides the family and
/// <c>tier</c> (<c>fast</c>|<c>capable</c>) resolves the built-in tier models
/// server-side. An explicit <c>model</c> always wins and may name any model
/// the provider supports.</summary>
public interface IProvidersPlane
{
    Task<string> ApplyConfigAsync(string yaml, CancellationToken ct = default);
    Task<ProviderHealth> HealthAsync(string name, CancellationToken ct = default);
    /// <summary>Live probe of the server's six built-in default models (three
    /// provider families × two tiers) — spends real provider tokens. REST-only:
    /// the gRPC client throws <see cref="UnsupportedTransportException"/>.</summary>
    Task<HealthAiResult> HealthAiAsync(CancellationToken ct = default);
    Task<CompleteResult> CompleteAsync(
        string name, string prompt, string? model = null, string? system = null,
        uint? maxTokens = null, double? temperature = null, string? versionId = null,
        string? provider = null, string? tier = null,
        CancellationToken ct = default);
    Task<EmbedResult> EmbedAsync(
        string name, IReadOnlyList<string> inputs, string? model = null,
        string? versionId = null, string? provider = null, CancellationToken ct = default);

    /// <summary>Free disclosure of every provider config visible to the
    /// tenant — applied configs plus synthesized env defaults, each with its
    /// resolved fast/capable tier models and credential status. Zero
    /// provider calls; the credential itself is never echoed. REST-only
    /// (GET /v1/providers).</summary>
    Task<IReadOnlyList<ProviderModels>> ListAsync(CancellationToken ct = default);

    /// <summary>The effective per-call output-token budgets for the caller's
    /// tenant and where they come from (<see cref="MaxTokensResponse.Source"/>:
    /// <c>tenant</c> after a replacement, else <c>environment</c>). Any
    /// authenticated role — the numbers shape spend, they are not secrets.
    /// REST-only (GET /v1/max-tokens): the gRPC client's task faults with
    /// <see cref="UnsupportedTransportException"/>.</summary>
    Task<MaxTokensResponse> GetMaxTokensAsync(CancellationToken ct = default);

    /// <summary>Replace the tenant's WHOLE budget set — there is no partial
    /// update: every field of <paramref name="budgets"/> is sent, and the
    /// server answers the same shape <see cref="GetMaxTokensAsync"/> returns
    /// (start from its <see cref="MaxTokensResponse.ToBudgets"/> to change
    /// one). A field outside its range is the typed
    /// <see cref="InvalidInputException"/> (400); static <c>rw</c> role only,
    /// like provider configs and runbooks (<see cref="ForbiddenException"/>
    /// otherwise). Sent once, never auto-retried. REST-only
    /// (POST /v1/max-tokens): the gRPC client's task faults with
    /// <see cref="UnsupportedTransportException"/>.</summary>
    Task<MaxTokensResponse> ReplaceMaxTokensAsync(
        MaxTokensBudgets budgets, CancellationToken ct = default);
}

/// <summary>Multiturn sessions over a runbook's access-permitted collections
///. Auth is the data plane's: a capability JWT with the query scope
/// (or a static token), and the uid contract applies to every call.</summary>
public interface ISessionsPlane
{
    /// <summary>Open a session on a runbook (bare name = latest non-removed
    /// version, or exact name@version). The response echoes the collections
    /// the caller's access level/compartments actually permit.</summary>
    Task<SessionCreated> CreateAsync(string runbookName, CancellationToken ct = default);

    /// <summary>One retrieval turn (+ optional completion when the runbook
    /// declares one). The request's model override is honored only under the
    /// runbook's <c>models.allowOverrides</c> policy — a disallowed override
    /// draws the typed <see cref="ForbiddenException"/>, never a silent
    /// downgrade. A turn spends provider tokens — sent once, never
    /// auto-retried, and DEADLINE-EXEMPT: aborting client-side does not stop
    /// the server's paid completion.</summary>
    Task<TurnResult> TurnAsync(
        string sessionId, TurnRequest request, CancellationToken ct = default);

    /// <summary>The same turn, streamed: N progress events at real stage
    /// boundaries, then exactly one <see cref="TurnStreamEvent.Done"/>.
    /// Failures — pre-stream refusals and the stream's terminal error event
    /// alike — throw the typed error during enumeration, decoded through the
    /// one problem registry; a stream that ends without a terminal event
    /// throws <see cref="MunariumTransportException"/> — never a silent
    /// success. No overall deadline (a capable-tier completion can exceed
    /// 30 s) but a 60 s idle watchdog per read (the server heartbeats
    /// keep-alives every 15 s). REST-only: the gRPC client throws
    /// <see cref="UnsupportedTransportException"/>.</summary>
    IAsyncEnumerable<TurnStreamEvent> TurnStreamAsync(
        string sessionId, TurnRequest request, CancellationToken ct = default);

    /// <summary>The session envelope + stored turn transcript.</summary>
    Task<Session> GetAsync(string sessionId, CancellationToken ct = default);

    /// <summary>Close the session (a write — ro tokens are refused).
    /// Idempotent: closing a closed/expired session returns its state
    /// unchanged.</summary>
    Task<Session> CloseAsync(string sessionId, CancellationToken ct = default);
}

/// <summary>Capability-token management (mgmt role). "Tokens" here are
/// the short-lived end-user capability JWTs — not the bearer this client
/// authenticates with.</summary>
public interface IAccessTokensPlane
{
    /// <summary>Mint a capability JWT for an authenticated end user. The
    /// token material is returned ONCE and never persisted server-side.
    /// <paramref name="scopes"/> is "query" and/or "ingest";
    /// <paramref name="ttlSecs"/> is clamped to the server's 24 h ceiling.</summary>
    Task<IssuedToken> MintAsync(
        string uid, int accessLevel, IReadOnlyList<string> scopes,
        IReadOnlyList<string>? compartments = null,
        IReadOnlyList<string>? runbookRefs = null, ulong? ttlSecs = null,
        CancellationToken ct = default);

    /// <summary>The issuance audit — metadata only, never token material.
    /// <paramref name="active"/> true = unexpired + unrevoked only.</summary>
    Task<IReadOnlyList<TokenInfo>> ListAsync(
        string? uid = null, bool? active = null, CancellationToken ct = default);

    /// <summary>Deny-list a token by jti. Note
    /// <see cref="TokenRevocation.RevocationCheckEnabled"/> in the response:
    /// the list is only consulted when the server enables it.</summary>
    Task<TokenRevocation> RevokeAsync(string jti, CancellationToken ct = default);
}

/// <summary>Management reports over the interactions audit trail (mgmt
/// role). REST-only: the gRPC client throws
/// <see cref="UnsupportedTransportException"/> on every method
/// (AdminService.Usage is declared but UNIMPLEMENTED — not wired).</summary>
public interface IReportsPlane
{
    /// <summary><paramref name="groupBy"/>: uid | session | runbook |
    /// collection (server default: uid); RFC 3339 window bounds.</summary>
    Task<UsageReport> UsageAsync(
        string? groupBy = null, string? from = null, string? to = null,
        CancellationToken ct = default);

    /// <summary><paramref name="bodies"/> includes the captured
    /// request/response bodies (heavy; off by default);
    /// <paramref name="before"/> is the keyset cursor — pass the previous
    /// page's <see cref="AuditReport.NextBefore"/> verbatim.</summary>
    Task<AuditReport> AuditAsync(
        string? uid = null, string? sessionId = null, string? runbook = null,
        string? from = null, string? to = null, int? limit = null,
        bool bodies = false, string? before = null, CancellationToken ct = default);

    /// <summary>Model-spend token rollup (dollar pricing lives upstream).</summary>
    Task<CostReport> CostAsync(
        string? from = null, string? to = null, CancellationToken ct = default);

    /// <summary>Bucketed request/error/latency series.
    /// <paramref name="window"/>: 1h | 24h | 7d | 30d (server default 24h);
    /// <paramref name="plane"/>: rest | grpc.</summary>
    Task<TimeseriesReport> TimeseriesAsync(
        string? window = null, string? plane = null, CancellationToken ct = default);

    Task<EndpointsReport> EndpointsAsync(
        string? window = null, long? limit = null, CancellationToken ct = default);

    Task<RunbookReport> RunbooksAsync(string? window = null, CancellationToken ct = default);

    Task<SessionsReport> SessionsAsync(string? window = null, CancellationToken ct = default);

    /// <summary>How the evidence hierarchy behaved — per profile and
    /// layer: turns, refusals, completeness, latency percentiles. Reads the
    /// question "which layer is quietly refusing?", which no other report
    /// answers because a refusing layer still returns 200.
    /// <paramref name="window"/>: 24h (server default) | 7d | 30d.</summary>
    Task<EvidenceReport> EvidenceAsync(string? window = null, CancellationToken ct = default);

    /// <summary>Munarium Matrix's health as the server sees it, plus
    /// the data views the tenant's runbooks declare. Unwindowed: the circuit
    /// breaker's state is a reading of now.</summary>
    Task<MatrixReport> MatrixAsync(CancellationToken ct = default);
}

/// <summary>Sealed evidence READS. REST-only; the gRPC transport
/// throws <c>UnsupportedTransportException</c>.
///
/// Sealing is deliberately absent. An artifact's manifest is a statement about
/// work the sealer did — an SDK offering <c>SealEvidenceAsync</c> would invite
/// an application to assert provenance it cannot vouch for. What an
/// application legitimately needs is the other direction: an answer cites
/// <c>[evidence/&lt;id&gt;#&lt;row&gt;]</c> and the application resolves that
/// citation to show a reader what the number was computed from.
///
/// Access is checked per artifact against the SESSION's clearance, not the
/// sealer's. Expect <c>evidence-forbidden</c> (403), <c>evidence-expired</c>
/// (410 — retention purged the bytes, and the citation was real) and
/// <c>evidence-not-committed</c> (409).</summary>
public interface IEvidencePlane
{
    /// <summary>The manifest, verbatim as the contract defines it. Returned
    /// UNWRAPPED by the route, so this is the manifest itself.</summary>
    Task<JsonElement> GetAsync(string evidenceId, CancellationToken ct = default);

    /// <summary>A bounded, audited window over the sealed rows.</summary>
    Task<EvidenceRows> RowsAsync(
        string evidenceId, int? from = null, int? limit = null,
        CancellationToken ct = default);
}

/// <summary>Guided runbook authoring: pattern catalog, interview-driven
/// drafts, deterministic validation, optional AI assist, hash-manifested
/// export, and apply. REST-only (no authoring RPCs exist).
/// <see cref="DeleteDraftAsync"/> is the client surface's ONE delete — it
/// removes a workspace draft (soft), never ledger data, so the append-only
/// invariant is untouched.</summary>
public interface IAuthoringPlane
{
    Task<IReadOnlyList<PatternSummary>> ListPatternsAsync(CancellationToken ct = default);
    Task<PatternDetail> GetPatternAsync(string id, CancellationToken ct = default);
    Task<Draft> CreateDraftAsync(
        string name, string? patternId = null, bool seedFromExemplar = false,
        CancellationToken ct = default);
    Task<IReadOnlyList<DraftSummary>> ListDraftsAsync(CancellationToken ct = default);
    Task<Draft> GetDraftAsync(string draftId, CancellationToken ct = default);
    Task<DraftDeletion> DeleteDraftAsync(string draftId, CancellationToken ct = default);

    /// <summary>Replace the stored answers (and by default re-materialize
    /// documents).</summary>
    Task<Draft> PutAnswersAsync(
        string draftId, JsonElement answers, bool materialize = true,
        CancellationToken ct = default);

    Task<DraftValidation> ValidateAsync(string draftId, CancellationToken ct = default);

    /// <summary>AI-assisted drafting pass. NEVER fails the request: a
    /// degraded pass (no provider, budget, parse failure) sets
    /// <see cref="AssistResult.AssistNote"/> instead.</summary>
    Task<AssistResult> AssistAsync(
        string draftId, string? description = null, string? instructions = null,
        string? provider = null, string? model = null, string? tier = null,
        CancellationToken ct = default);

    /// <summary>Self-contained hash-manifested bundle (shapes before
    /// runbooks in <see cref="DraftBundle.ApplyOrder"/>).</summary>
    Task<DraftBundle> ExportAsync(string draftId, CancellationToken ct = default);

    /// <summary>Apply the draft's documents to THIS server (validates
    /// inline).</summary>
    Task<IReadOnlyList<AppliedDoc>> ApplyAsync(string draftId, CancellationToken ct = default);
}

/// <summary>Meta routes outside the ten-plane surface (internal — the facade
/// exposes <see cref="MunariumClient.ServerVersionAsync"/>).</summary>
internal interface IMetaPlane
{
    /// <summary>GET /version — the server's name + workspace version,
    /// unauthenticated. REST-only: gRPC has no version RPC (use server
    /// reflection there).</summary>
    Task<ServerVersionInfo> ServerVersionAsync(CancellationToken ct = default);
}

/// <summary>Helpers for building replayable <see cref="ChunkSource"/>s.</summary>
public static class Chunks
{
    /// <summary>A source that re-slices in-memory bytes per attempt.</summary>
    public static ChunkSource FromBytes(ReadOnlyMemory<byte> data, int chunkSize = 64 * 1024) =>
        () => Slice(data, chunkSize);

    /// <summary>A source over a pre-split chunk list.</summary>
    public static ChunkSource FromList(IReadOnlyList<ReadOnlyMemory<byte>> chunks) =>
        () => Replay(chunks);

    private static async IAsyncEnumerable<ReadOnlyMemory<byte>> Slice(
        ReadOnlyMemory<byte> data, int chunkSize)
    {
        for (var i = 0; i < data.Length; i += chunkSize)
        {
            yield return data.Slice(i, Math.Min(chunkSize, data.Length - i));
            await Task.Yield();
        }
    }

    private static async IAsyncEnumerable<ReadOnlyMemory<byte>> Replay(
        IReadOnlyList<ReadOnlyMemory<byte>> chunks)
    {
        foreach (var chunk in chunks)
        {
            yield return chunk;
            await Task.Yield();
        }
    }
}
