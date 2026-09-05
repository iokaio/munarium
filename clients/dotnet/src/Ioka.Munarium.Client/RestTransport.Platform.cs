// SPDX-License-Identifier: Apache-2.0
// The platform REST surface: sessions + SSE streaming turns, access tokens,
// reports, guided authoring, collections, runbook management v2 +
// chronology rules, the file/bulk ingest planes, findings, provider
// disclosure, the max_tokens budgets, and GET /version. Same retry
// classes as the core surface; the paid/large sends (unary turns,
// file/batch/bulk ingest) are DEADLINE-EXEMPT like the streaming source
// upload — a client-side abort does not stop the server's paid work, and
// bulk bodies run to 256 MiB.

using System.Net.Http.Headers;
using System.Runtime.CompilerServices;
using System.Text.Json;
using System.Text.Json.Serialization.Metadata;

namespace Ioka.Munarium.Client;

internal sealed partial class RestTransport
{
    /// <summary>The SSE idle watchdog: the server heartbeats comment
    /// keep-alives every 15 s, so 60 s of wire silence means a wedged peer,
    /// not a slow completion.</summary>
    private static readonly TimeSpan SseIdleTimeout = TimeSpan.FromSeconds(60);

    /// <summary>Send-once POST for the paid/large writes: unary turns spend
    /// provider tokens a client-side abort cannot stop, and the file/bulk
    /// bodies run to the server's 256 MiB ceiling — so the per-request
    /// deadline that suits small JSON writes is skipped (the PutSourceAsync
    /// reasoning); only the caller's token bounds the call. A call site of
    /// the one pipeline, not a copy of it.</summary>
    private Task<T> SendLargeOnceAsync<T>(
        string path, Func<HttpContent> content, JsonTypeInfo<T> responseType,
        CancellationToken ct) =>
        SendAsync(
            HttpMethod.Post, path, content, responseType, RetryClass.Write, null, ct,
            deadline: false);

    /// <summary>Idempotent-read path for a text (non-JSON) body — chronology
    /// rules come back as the applied YAML verbatim. Same pipeline and retry
    /// class as GetAsync, with the text decoder.</summary>
    private Task<string> GetTextAsync(string pathAndQuery, CancellationToken ct) =>
        SendAsync(HttpMethod.Get, pathAndQuery, null, Text, RetryClass.Read, null, ct);

    // -- query: findings ----------------------------------------------------

    public async Task<IReadOnlyList<StoredFinding>> FindingsAsync(
        string versionId, ulong? asOfSeq = null, string? severity = null,
        string? ruleId = null, int? limit = null, CancellationToken ct = default)
    {
        var resp = await GetAsync(
            $"/v1/versions/{Seg(versionId)}/findings" + Query(
                ("as_of_seq", asOfSeq?.ToString()),
                ("severity", severity),
                ("rule_id", ruleId),
                ("limit", limit?.ToString())),
            MunariumJsonContext.Default.FindingsResponse, ct).ConfigureAwait(false);
        return resp.Findings;
    }

    // -- sealed evidence: reads only --------------------------------

    // Explicit interface implementation: this class implements every plane on
    // one type, and `GetAsync` is already taken by the transport's own request
    // helper. Explicit form keeps the call site reading `client.Evidence
    // .GetAsync(id)` instead of forcing a redundant `GetEvidenceAsync`.
    Task<JsonElement> IEvidencePlane.GetAsync(string evidenceId, CancellationToken ct) =>
        GetAsync(
            $"/v1/evidence/{Seg(evidenceId)}",
            MunariumJsonContext.Default.JsonElement, ct);

    Task<EvidenceRows> IEvidencePlane.RowsAsync(
        string evidenceId, int? from, int? limit,
        CancellationToken ct) =>
        GetAsync(
            $"/v1/evidence/{Seg(evidenceId)}/rows" + Query(
                ("from", from?.ToString()),
                ("limit", limit?.ToString())),
            MunariumJsonContext.Default.EvidenceRows, ct);

    // -- ingest: file plane + bulk sessions ---------------------------------

    public Task<IngestResult> IngestAsync(IngestFile file, CancellationToken ct = default) =>
        SendLargeOnceAsync(
            "/v1/ingest", JsonStreamContent(file, MunariumJsonContext.Default.IngestFile),
            MunariumJsonContext.Default.IngestResult, ct);

    public async Task<IReadOnlyList<IngestResult>> IngestBatchAsync(
        IReadOnlyList<IngestFile> files, CancellationToken ct = default)
    {
        Validation.CheckBulkFiles("batch", files.Count);
        var resp = await SendLargeOnceAsync(
            "/v1/ingest/batch",
            JsonStreamContent(new IngestBatchBody { Files = files }, MunariumJsonContext.Default.IngestBatchBody),
            MunariumJsonContext.Default.IngestBatchResponse, ct).ConfigureAwait(false);
        return resp.Results;
    }

    public Task<BulkOpenResult> BulkOpenAsync(
        IReadOnlyList<BulkManifestEntry> files, string? label = null,
        CancellationToken ct = default) =>
        SendLargeOnceAsync(
            "/v1/ingest/bulk",
            JsonStreamContent(
                new BulkOpenBody { Files = files, Label = label },
                MunariumJsonContext.Default.BulkOpenBody),
            MunariumJsonContext.Default.BulkOpenResult, ct);

    public Task<BulkChunkResult> BulkChunkAsync(
        string bulkId, IReadOnlyList<IngestFile> files, CancellationToken ct = default)
    {
        Validation.CheckBulkFiles("bulk chunk", files.Count);
        return SendLargeOnceAsync(
            $"/v1/ingest/bulk/{Seg(bulkId)}/chunk",
            JsonStreamContent(new IngestBatchBody { Files = files }, MunariumJsonContext.Default.IngestBatchBody),
            MunariumJsonContext.Default.BulkChunkResult, ct);
    }

    public Task<BulkStatusResult> BulkStatusAsync(
        string bulkId, bool includeNeeded = false, CancellationToken ct = default) =>
        GetAsync(
            $"/v1/ingest/bulk/{Seg(bulkId)}" + Query(
                ("include_needed", includeNeeded ? "true" : null)),
            MunariumJsonContext.Default.BulkStatusResult, ct);

    public Task<BulkCompleteResult> BulkCompleteAsync(
        string bulkId, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, $"/v1/ingest/bulk/{Seg(bulkId)}/complete", null,
            MunariumJsonContext.Default.BulkCompleteResult, RetryClass.Write, null, ct);

    public Task<SourceInfo> GetSourceAsync(string sourceId, CancellationToken ct = default) =>
        GetAsync($"/v1/sources/{Seg(sourceId)}", MunariumJsonContext.Default.SourceInfo, ct);

    // -- retrieval: collections ---------------------------------------------

    public Task<Collection> CreateCollectionAsync(
        string name, string shapeRef, int accessLevel = 0,
        IReadOnlyList<string>? compartments = null, string? description = null,
        CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, "/v1/collections",
            JsonContent(
                new CreateCollectionBody
                {
                    Name = name, ShapeRef = shapeRef, AccessLevel = accessLevel,
                    Compartments = compartments ?? [], Description = description,
                },
                MunariumJsonContext.Default.CreateCollectionBody),
            MunariumJsonContext.Default.Collection, RetryClass.Write, null, ct);

    public async Task<IReadOnlyList<Collection>> ListCollectionsAsync(
        CancellationToken ct = default)
    {
        var resp = await GetAsync(
            "/v1/collections", MunariumJsonContext.Default.CollectionsResponse, ct)
            .ConfigureAwait(false);
        return resp.Collections;
    }

    public Task<Collection> GetCollectionAsync(string id, CancellationToken ct = default) =>
        GetAsync($"/v1/collections/{Seg(id)}", MunariumJsonContext.Default.Collection, ct);

    // -- runbooks: management v2 + chronology rules -------------------------

    public async Task<IReadOnlyList<RunbookSummary>> ListAsync(
        bool includeRemoved = false, CancellationToken ct = default)
    {
        var resp = await GetAsync(
            "/v1/runbooks" + Query(("include_removed", includeRemoved ? "true" : null)),
            MunariumJsonContext.Default.RunbooksResponse, ct).ConfigureAwait(false);
        return resp.Runbooks;
    }

    public Task<RunbookInfo> GetInfoAsync(string name, CancellationToken ct = default) =>
        GetAsync($"/v1/runbooks/{Seg(name)}", MunariumJsonContext.Default.RunbookInfo, ct);

    public Task<RunbookValidation> ValidateAsync(
        string yaml, bool suggest = false, string? provider = null,
        string? model = null, string? tier = null, CancellationToken ct = default) =>
        // With suggest=true this spends provider tokens — send once.
        SendAsync(
            HttpMethod.Post,
            "/v1/runbooks/validate" + Query(
                ("suggest", suggest ? "true" : null),
                ("provider", provider), ("model", model), ("tier", tier)),
            YamlContent(yaml), MunariumJsonContext.Default.RunbookValidation,
            RetryClass.Write, null, ct);

    public Task<RemovalRequest> RemoveRequestAsync(
        string name, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, $"/v1/runbooks/{Seg(name)}/remove-request", null,
            MunariumJsonContext.Default.RemovalRequest, RetryClass.Write, null, ct);

    public Task<RemovalConfirmation> RemoveConfirmAsync(
        string name, string removalId, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, $"/v1/runbooks/{Seg(name)}/remove-confirm",
            JsonContent(
                new RemovalConfirmBody { RemovalId = removalId },
                MunariumJsonContext.Default.RemovalConfirmBody),
            MunariumJsonContext.Default.RemovalConfirmation, RetryClass.Write, null, ct);

    public Task<ChronologyRulesApplied> ApplyChronologyRulesAsync(
        string yaml, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, "/v1/chronology-rules", YamlContent(yaml),
            MunariumJsonContext.Default.ChronologyRulesApplied, RetryClass.Write, null, ct);

    public Task<string> GetChronologyRulesAsync(string name, CancellationToken ct = default) =>
        GetTextAsync($"/v1/chronology-rules/{Seg(name)}", ct);

    // -- providers: disclosure ----------------------------------------------

    public async Task<IReadOnlyList<ProviderModels>> ListAsync(CancellationToken ct = default)
    {
        var resp = await GetAsync(
            "/v1/providers", MunariumJsonContext.Default.ProviderListResponse, ct)
            .ConfigureAwait(false);
        return resp.Providers;
    }

    // -- providers: per-call max_tokens budgets -----------------------------

    public Task<MaxTokensResponse> GetMaxTokensAsync(CancellationToken ct = default) =>
        GetAsync("/v1/max-tokens", MunariumJsonContext.Default.MaxTokensResponse, ct);

    public Task<MaxTokensResponse> ReplaceMaxTokensAsync(
        MaxTokensBudgets budgets, CancellationToken ct = default) =>
        // A whole-set replace on the rw role — ApplyConfigAsync's class: sent
        // once, no idempotency key (the route records none).
        SendAsync(
            HttpMethod.Post, "/v1/max-tokens",
            JsonContent(budgets, MunariumJsonContext.Default.MaxTokensBudgets),
            MunariumJsonContext.Default.MaxTokensResponse, RetryClass.Write, null, ct);

    // -- sessions + streaming turns -----------------------------------------

    public Task<SessionCreated> CreateAsync(string runbookName, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, $"/v1/runbooks/{Seg(runbookName)}/sessions", null,
            MunariumJsonContext.Default.SessionCreated, RetryClass.Write, null, ct);

    public Task<TurnResult> TurnAsync(
        string sessionId, TurnRequest request, CancellationToken ct = default) =>
        // A turn spends provider tokens — send-once, never auto-retried, and
        // DEADLINE-EXEMPT: a client-side abort does not stop the server's
        // paid completion (the transcript ordinal still advances), so a 30 s
        // cap on a capable-tier completion is a double-spend invitation. The
        // SSE variant is the way to watch a long turn.
        SendLargeOnceAsync(
            $"/v1/sessions/{Seg(sessionId)}/turns",
            JsonStreamContent(request, MunariumJsonContext.Default.TurnRequest),
            MunariumJsonContext.Default.TurnResult, ct);

    public async IAsyncEnumerable<TurnStreamEvent> TurnStreamAsync(
        string sessionId, TurnRequest request,
        [EnumeratorCancellation] CancellationToken ct = default)
    {
        // No overall deadline (a capable-tier completion can exceed 30 s),
        // but the 60 s idle watchdog covers the header wait and every read:
        // the server heartbeats keep-alive comments every 15 s, so a silent
        // wire means a wedged peer and the caller gets a typed transport
        // error instead of hanging forever. ONE linked CTS for the whole
        // stream, its timer armed ONLY while network I/O is pending (not a
        // CTS + timer + registration per 16 KiB read). Keeping the timer
        // armed while an iterator consumer handles a progress event would
        // incorrectly classify a slow consumer as a silent peer.
        using var idle = CancellationTokenSource.CreateLinkedTokenSource(ct);
        using var resp = await GuardIdleAsync(
            idle,
            token => OpenTurnStreamAsync(sessionId, request, token, ct),
            ct).ConfigureAwait(false);
        var stream = await GuardIdleAsync(
            idle,
            token => resp.Content.ReadAsStreamAsync(token),
            ct).ConfigureAwait(false);
        await using var body = stream.ConfigureAwait(false);

        var parser = new SseParser();
        var buffer = new byte[16 * 1024];
        while (true)
        {
            var n = await GuardIdleAsync(
                idle,
                token => stream.ReadAsync(buffer, token).AsTask(),
                ct).ConfigureAwait(false);
            if (n == 0)
            {
                // The invariant: a stream that ends WITHOUT a terminal event
                // is a typed transport error — never a silent success.
                throw new MunariumTransportException(
                    "SSE stream ended without a terminal done/error event");
            }
            IReadOnlyList<SseEvent> events;
            try
            {
                events = parser.Push(buffer.AsSpan(0, n));
            }
            catch (SseOverflowException)
            {
                throw new UnexpectedServerException(
                    $"SSE peer exceeded the {SseParser.MaxEventBytes / (1024 * 1024)} MiB " +
                    "event buffer without completing an event");
            }
            foreach (var ev in events)
            {
                switch (ev.Event)
                {
                    case "progress":
                        // Undecodable progress is skipped — a newer server
                        // may add stages this build cannot name, and
                        // progress is informational.
                        if (TryDecodeProgress(ev.Data) is { } progress)
                        {
                            yield return new TurnStreamEvent.Progress(progress);
                        }
                        break;
                    case "done":
                        // Nothing rides after the terminal event.
                        yield return new TurnStreamEvent.Done(DecodeDone(ev.Data));
                        yield break;
                    case "error":
                        // The error event carries the same problem+json body
                        // the unary route would have returned — decoded
                        // through the one registry. Terminal.
                        throw DecodeStreamError(ev.Data);
                    default:
                        break; // unnamed/unknown events: ignored (forward-compat)
                }
            }
        }
    }

    /// <summary>Open the SSE response on the shared raw primitive under the
    /// idle token. Pre-stream failures (auth, refusals, shed) are plain
    /// problem+json — decoded by the ONE error path, Retry-After included.</summary>
    private async Task<HttpResponseMessage> OpenTurnStreamAsync(
        string sessionId, TurnRequest request, CancellationToken idle, CancellationToken ct)
    {
        using var httpRequest = NewRequest(
            HttpMethod.Post, $"/v1/sessions/{Seg(sessionId)}/turns/stream");
        httpRequest.Headers.Accept.Add(new MediaTypeWithQualityHeaderValue("text/event-stream"));
        httpRequest.Content = new JsonStreamContent<TurnRequest>(
            request, MunariumJsonContext.Default.TurnRequest);
        var resp = await SendRawAsync(httpRequest, idle, ct).ConfigureAwait(false);
        if (resp.IsSuccessStatusCode) return resp;
        using (resp)
        {
            throw await ProblemAsync(resp, idle, ct).ConfigureAwait(false);
        }
    }

    /// <summary>One wait under the idle watchdog. Wire faults are typed
    /// transport errors; watchdog expiry names itself (the token the caller
    /// passed is the idle-linked one, so expiry surfaces as cancellation).</summary>
    internal static async Task<T> GuardIdleAsync<T>(
        CancellationTokenSource idle,
        Func<CancellationToken, Task<T>> op,
        CancellationToken ct,
        TimeSpan? timeout = null)
    {
        var budget = timeout ?? SseIdleTimeout;
        idle.CancelAfter(budget);
        try
        {
            return await op(idle.Token).ConfigureAwait(false);
        }
        catch (OperationCanceledException) when (ct.IsCancellationRequested)
        {
            throw; // caller cancellation is not a transport failure
        }
        catch (OperationCanceledException)
        {
            throw new MunariumTransportException(
                $"SSE idle watchdog: no bytes in {budget.TotalSeconds:0} s " +
                "(the server heartbeats keep-alives every 15 s)");
        }
        catch (Exception e) when (e is IOException or HttpRequestException)
        {
            throw new MunariumTransportException(e.Message);
        }
        finally
        {
            // A yielded progress event can remain with application code for
            // arbitrarily long. The watchdog measures wire silence only, so
            // disarm it until the next header/body read actually begins.
            if (!idle.IsCancellationRequested)
            {
                idle.CancelAfter(Timeout.InfiniteTimeSpan);
            }
        }
    }

    private static TurnProgressEvent? TryDecodeProgress(string data)
    {
        try
        {
            return JsonSerializer.Deserialize(data, MunariumJsonContext.Default.TurnProgressEvent);
        }
        catch (JsonException)
        {
            return null;
        }
    }

    /// <summary>An undecodable terminal event is an error — the caller was
    /// owed a <see cref="TurnResult"/>.</summary>
    private static TurnResult DecodeDone(string data)
    {
        try
        {
            return JsonSerializer.Deserialize(data, MunariumJsonContext.Default.TurnResult)
                ?? throw new UnexpectedServerException("undecodable SSE done event: null");
        }
        catch (JsonException e)
        {
            throw new UnexpectedServerException($"undecodable SSE done event: {e.Message}");
        }
    }

    /// <summary>The SSE error event has no transport status — the problem
    /// body's own <c>status</c> member stands in (one parse, one path).</summary>
    private static MunariumException DecodeStreamError(string data) =>
        Errors.FromProblem(null, data, null);

    public Task<Session> GetAsync(string sessionId, CancellationToken ct = default) =>
        GetAsync($"/v1/sessions/{Seg(sessionId)}", MunariumJsonContext.Default.Session, ct);

    public Task<Session> CloseAsync(string sessionId, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, $"/v1/sessions/{Seg(sessionId)}/close", null,
            MunariumJsonContext.Default.Session, RetryClass.Write, null, ct);

    // -- access tokens (mgmt) -----------------------------------------------

    public Task<IssuedToken> MintAsync(
        string uid, int accessLevel, IReadOnlyList<string> scopes,
        IReadOnlyList<string>? compartments = null,
        IReadOnlyList<string>? runbookRefs = null, ulong? ttlSecs = null,
        CancellationToken ct = default) =>
        // Minting twice issues two live tokens — send once.
        SendAsync(
            HttpMethod.Post, "/v1/access-tokens",
            JsonContent(
                new IssueTokenBody
                {
                    Uid = uid, AccessLevel = accessLevel, Scopes = scopes,
                    Compartments = compartments ?? [], RunbookRefs = runbookRefs,
                    TtlSecs = ttlSecs,
                },
                MunariumJsonContext.Default.IssueTokenBody),
            MunariumJsonContext.Default.IssuedToken, RetryClass.Write, null, ct);

    public async Task<IReadOnlyList<TokenInfo>> ListAsync(
        string? uid = null, bool? active = null, CancellationToken ct = default)
    {
        var resp = await GetAsync(
            "/v1/access-tokens" + Query(
                ("uid", uid),
                ("active", active is null ? null : active.Value ? "true" : "false")),
            MunariumJsonContext.Default.TokensResponse, ct).ConfigureAwait(false);
        return resp.Tokens;
    }

    public Task<TokenRevocation> RevokeAsync(string jti, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, $"/v1/access-tokens/{Seg(jti)}/revoke", null,
            MunariumJsonContext.Default.TokenRevocation, RetryClass.Write, null, ct);

    // -- reports (mgmt) -----------------------------------------------------

    public Task<UsageReport> UsageAsync(
        string? groupBy = null, string? from = null, string? to = null,
        CancellationToken ct = default) =>
        GetAsync(
            "/v1/reports/usage" + Query(("group_by", groupBy), ("from", from), ("to", to)),
            MunariumJsonContext.Default.UsageReport, ct);

    public Task<AuditReport> AuditAsync(
        string? uid = null, string? sessionId = null, string? runbook = null,
        string? from = null, string? to = null, int? limit = null,
        bool bodies = false, string? before = null, CancellationToken ct = default) =>
        GetAsync(
            "/v1/reports/audit" + Query(
                ("uid", uid), ("session_id", sessionId), ("runbook", runbook),
                ("from", from), ("to", to), ("limit", limit?.ToString()),
                ("bodies", bodies ? "true" : null), ("before", before)),
            MunariumJsonContext.Default.AuditReport, ct);

    public Task<CostReport> CostAsync(
        string? from = null, string? to = null, CancellationToken ct = default) =>
        GetAsync(
            "/v1/reports/cost" + Query(("from", from), ("to", to)),
            MunariumJsonContext.Default.CostReport, ct);

    public Task<TimeseriesReport> TimeseriesAsync(
        string? window = null, string? plane = null, CancellationToken ct = default) =>
        GetAsync(
            "/v1/reports/timeseries" + Query(("window", window), ("plane", plane)),
            MunariumJsonContext.Default.TimeseriesReport, ct);

    public Task<EndpointsReport> EndpointsAsync(
        string? window = null, long? limit = null, CancellationToken ct = default) =>
        GetAsync(
            "/v1/reports/endpoints" + Query(("window", window), ("limit", limit?.ToString())),
            MunariumJsonContext.Default.EndpointsReport, ct);

    public Task<RunbookReport> RunbooksAsync(
        string? window = null, CancellationToken ct = default) =>
        GetAsync(
            "/v1/reports/runbooks" + Query(("window", window)),
            MunariumJsonContext.Default.RunbookReport, ct);

    public Task<SessionsReport> SessionsAsync(
        string? window = null, CancellationToken ct = default) =>
        GetAsync(
            "/v1/reports/sessions" + Query(("window", window)),
            MunariumJsonContext.Default.SessionsReport, ct);

    public Task<EvidenceReport> EvidenceAsync(
        string? window = null, CancellationToken ct = default) =>
        GetAsync(
            "/v1/reports/evidence" + Query(("window", window)),
            MunariumJsonContext.Default.EvidenceReport, ct);

    public Task<MatrixReport> MatrixAsync(CancellationToken ct = default) =>
        GetAsync("/v1/reports/matrix", MunariumJsonContext.Default.MatrixReport, ct);

    // -- guided authoring ---------------------------------------------------

    public async Task<IReadOnlyList<PatternSummary>> ListPatternsAsync(
        CancellationToken ct = default)
    {
        var resp = await GetAsync(
            "/v1/authoring/patterns", MunariumJsonContext.Default.PatternsResponse, ct)
            .ConfigureAwait(false);
        return resp.Patterns;
    }

    public Task<PatternDetail> GetPatternAsync(string id, CancellationToken ct = default) =>
        GetAsync($"/v1/authoring/patterns/{Seg(id)}", MunariumJsonContext.Default.PatternDetail, ct);

    public Task<Draft> CreateDraftAsync(
        string name, string? patternId = null, bool seedFromExemplar = false,
        CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, "/v1/authoring/drafts",
            JsonContent(
                new CreateDraftBody
                {
                    Name = name, PatternId = patternId, SeedFromExemplar = seedFromExemplar,
                },
                MunariumJsonContext.Default.CreateDraftBody),
            MunariumJsonContext.Default.Draft, RetryClass.Write, null, ct);

    public async Task<IReadOnlyList<DraftSummary>> ListDraftsAsync(
        CancellationToken ct = default)
    {
        var resp = await GetAsync(
            "/v1/authoring/drafts", MunariumJsonContext.Default.DraftsResponse, ct)
            .ConfigureAwait(false);
        return resp.Drafts;
    }

    public Task<Draft> GetDraftAsync(string draftId, CancellationToken ct = default) =>
        GetAsync($"/v1/authoring/drafts/{Seg(draftId)}", MunariumJsonContext.Default.Draft, ct);

    public Task<DraftDeletion> DeleteDraftAsync(
        string draftId, CancellationToken ct = default) =>
        // The client surface's ONE delete — a soft workspace-draft removal,
        // never ledger data.
        SendAsync(
            HttpMethod.Delete, $"/v1/authoring/drafts/{Seg(draftId)}", null,
            MunariumJsonContext.Default.DraftDeletion, RetryClass.Write, null, ct);

    public Task<Draft> PutAnswersAsync(
        string draftId, JsonElement answers, bool materialize = true,
        CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Put, $"/v1/authoring/drafts/{Seg(draftId)}/answers",
            JsonContent(
                new UpdateAnswersBody { Answers = answers, Materialize = materialize },
                MunariumJsonContext.Default.UpdateAnswersBody),
            MunariumJsonContext.Default.Draft, RetryClass.Write, null, ct);

    public Task<DraftValidation> ValidateAsync(string draftId, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, $"/v1/authoring/drafts/{Seg(draftId)}/validate", null,
            MunariumJsonContext.Default.DraftValidation, RetryClass.Write, null, ct);

    public Task<AssistResult> AssistAsync(
        string draftId, string? description = null, string? instructions = null,
        string? provider = null, string? model = null, string? tier = null,
        CancellationToken ct = default) =>
        // A BYOK provider call rides behind this — send once.
        SendAsync(
            HttpMethod.Post, $"/v1/authoring/drafts/{Seg(draftId)}/assist",
            JsonContent(
                new AssistBody
                {
                    Description = description, Instructions = instructions,
                    Provider = provider, Model = model, Tier = tier,
                },
                MunariumJsonContext.Default.AssistBody),
            MunariumJsonContext.Default.AssistResult, RetryClass.Write, null, ct);

    public Task<DraftBundle> ExportAsync(string draftId, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, $"/v1/authoring/drafts/{Seg(draftId)}/export", null,
            MunariumJsonContext.Default.DraftBundle, RetryClass.Write, null, ct);

    public async Task<IReadOnlyList<AppliedDoc>> ApplyAsync(
        string draftId, CancellationToken ct = default)
    {
        var resp = await SendAsync(
            HttpMethod.Post, $"/v1/authoring/drafts/{Seg(draftId)}/apply", null,
            MunariumJsonContext.Default.ApplyDraftResponse, RetryClass.Write, null, ct)
            .ConfigureAwait(false);
        return resp.Applied;
    }

    // -- meta ---------------------------------------------------------------

    public Task<ServerVersionInfo> ServerVersionAsync(CancellationToken ct = default) =>
        GetAsync("/version", MunariumJsonContext.Default.ServerVersionInfo, ct);
}
