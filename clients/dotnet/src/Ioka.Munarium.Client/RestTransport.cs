// SPDX-License-Identifier: Apache-2.0
// REST transport: HttpClient + problem+json error decoding, automatic
// idempotency keys on commands, bounded retries by request class:
// - reads (+search): transport failures and transient server outcomes
//   (overloaded / 5xx gateway) retried with backoff;
// - core commands: re-sent with the SAME idempotency key ONLY when the
//   request provably never reached the server (a connect-phase failure) or
//   the server shed it before executing — the server records an idempotency
//   key AFTER a command completes, so a possibly-delivered command is never
//   re-sent (it could execute twice);
// - non-idempotent writes (turns, provider calls, ingest, …): sent exactly
//   once.

using System.Net.Http.Headers;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization.Metadata;

namespace Ioka.Munarium.Client;

internal enum RetryClass
{
    Read,
    Command,
    Write,
}

internal sealed partial class RestTransport : ITransport
{
    private readonly HttpClient _http;
    private readonly bool _ownsHttp;

    /// <summary>The underlying client (tests pin the Timeout contract).</summary>
    internal HttpClient Http => _http;
    private readonly string _base;
    private readonly TimeSpan _requestTimeout;
    private readonly int _retries;

    private readonly AuthenticationHeaderValue? _auth;
    private readonly string? _uid;

    /// <summary>Attach auth + the uid to a request — the single header
    /// injection point, used by both SendAsync and the streaming
    /// PutSourceAsync (which bypasses SendAsync).</summary>
    private void ApplyHeaders(HttpRequestMessage request)
    {
        request.Headers.Authorization = _auth;
        if (_uid is not null) request.Headers.Add("X-Munarium-Uid", _uid);
    }

    internal RestTransport(MunariumClientOptions options, HttpClient? httpClient)
    {
        _ownsHttp = httpClient is null;
        if (httpClient is null)
        {
            _http = new HttpClient(new SocketsHttpHandler
            {
                ConnectTimeout = options.ConnectTimeout,
            // The client is explicitly long-lived; without a lifetime the
            // pool pins retired IPs across a DNS cutover forever.
            PooledConnectionLifetime = TimeSpan.FromMinutes(5),
            })
            {
                // The per-attempt token carries the deadline (the paid/large
                // sends and streams are exempt), so the client-level timeout
                // is disabled: it is the ONLY way an exempt send stays
                // unbounded by anything but the caller's token.
                Timeout = Timeout.InfiniteTimeSpan,
            };
        }
        else
        {
            // A caller-supplied client (IHttpClientFactory) is NEVER mutated:
            // auth rides each request; its own Timeout stays in force — so
            // its default 100 s still caps the deadline-exempt sends (see
            // MunariumClient.Rest's remarks).
            _http = httpClient;
        }
        _auth = options.Token is null
            ? null
            : new AuthenticationHeaderValue("Bearer", options.Token);
        _uid = options.Uid;
        _base = options.Endpoint.TrimEnd('/');
        _requestTimeout = options.RequestTimeout;
        _retries = options.ReadRetries;
    }

    public ValueTask DisposeAsync()
    {
        if (_ownsHttp) _http.Dispose();
        return ValueTask.CompletedTask;
    }

    /// <summary>Percent-encode a path segment — promise keys, shape refs, and
    /// runbook names are free-form; a raw '/' or '?' must not change the
    /// route shape.</summary>
    internal static string Seg(string s) => Uri.EscapeDataString(s);

    private static string Query(params (string Key, string? Value)[] pairs)
    {
        var parts = pairs
            .Where(p => p.Value is not null)
            .Select(p => $"{p.Key}={Uri.EscapeDataString(p.Value!)}")
            .ToArray();
        return parts.Length == 0 ? "" : "?" + string.Join('&', parts);
    }

    private static TimeSpan? RetryAfter(HttpResponseMessage resp)
    {
        var ra = resp.Headers.RetryAfter;
        if (ra is null) return null;
        if (ra.Delta is not null) return ra.Delta;
        if (ra.Date is not null)
        {
            var delta = ra.Date.Value - DateTimeOffset.UtcNow;
            return delta > TimeSpan.Zero ? delta : TimeSpan.Zero;
        }
        return null;
    }

    /// <summary>Connect-phase failures provably never delivered the request;
    /// everything else (timeouts, resets mid-flight) may have.</summary>
    private static bool MayHaveReached(Exception e) => e is not HttpRequestException
    {
        HttpRequestError: HttpRequestError.ConnectionError
            or HttpRequestError.NameResolutionError
            or HttpRequestError.SecureConnectionError
            or HttpRequestError.ProxyTunnelError,
    };

    /// <summary>Errors thrown by the HTTP stack and the deadline/idle
    /// tokens, mapped to the ONE typed transport error. Caller cancellation
    /// is never a transport failure. <paramref name="classifyReach"/> is true
    /// for the send phase, where a connect-phase failure proves the request
    /// never left; a failure while reading a body always may have reached.</summary>
    private static async Task<T> TransportGuardAsync<T>(
        Func<Task<T>> op, CancellationToken ct, bool classifyReach)
    {
        try
        {
            return await op().ConfigureAwait(false);
        }
        catch (OperationCanceledException) when (ct.IsCancellationRequested)
        {
            throw; // caller cancellation is not a transport failure
        }
        catch (Exception e)
            when (e is HttpRequestException or OperationCanceledException or IOException)
        {
            throw new MunariumTransportException(
                e.Message, !classifyReach || MayHaveReached(e));
        }
    }

    /// <summary>The attempt token: the caller's token, plus the per-request
    /// deadline unless the call is deadline-exempt (streaming ingest, the
    /// paid/large sends).</summary>
    private CancellationTokenSource Attempt(bool deadline, CancellationToken ct)
    {
        var cts = CancellationTokenSource.CreateLinkedTokenSource(ct);
        if (deadline) cts.CancelAfter(_requestTimeout);
        return cts;
    }

    /// <summary>Build a request against the configured endpoint with auth +
    /// uid applied — the single URL/header construction point.</summary>
    private HttpRequestMessage NewRequest(HttpMethod method, string pathAndQuery)
    {
        var request = new HttpRequestMessage(method, _base + pathAndQuery);
        ApplyHeaders(request);
        return request;
    }

    /// <summary>THE raw send primitive every REST path rides: headers-read
    /// completion (bodies stream), <paramref name="token"/> is the attempt
    /// token (deadline/idle-linked) and <paramref name="ct"/> the caller's,
    /// so a deadline expiry types as a transport failure while caller
    /// cancellation escapes untouched. Connect-phase failures are flagged
    /// as provably undelivered.</summary>
    private Task<HttpResponseMessage> SendRawAsync(
        HttpRequestMessage request, CancellationToken token, CancellationToken ct) =>
        TransportGuardAsync(
            () => _http.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, token),
            ct, classifyReach: true);

    /// <summary>THE non-success decoder: read the (small) problem+json body
    /// and map it through the error registry, Retry-After included.</summary>
    private static async Task<MunariumException> ProblemAsync(
        HttpResponseMessage resp, CancellationToken token, CancellationToken ct)
    {
        var body = await TransportGuardAsync(
            () => resp.Content.ReadAsStringAsync(token), ct, classifyReach: false)
            .ConfigureAwait(false);
        return Errors.FromProblem((int)resp.StatusCode, body, RetryAfter(resp));
    }

    /// <summary>A success-body decoder; runs under the attempt token.</summary>
    private delegate Task<T> Decoder<T>(HttpResponseMessage resp, CancellationToken token);

    /// <summary>Success streams straight into the typed model.</summary>
    private static Decoder<T> Json<T>(JsonTypeInfo<T> responseType) => async (resp, token) =>
    {
        try
        {
            var stream = await resp.Content.ReadAsStreamAsync(token).ConfigureAwait(false);
            var parsed = await JsonSerializer
                .DeserializeAsync(stream, responseType, token).ConfigureAwait(false);
            return parsed ?? throw new UnexpectedServerException(
                "empty success body", (int)resp.StatusCode);
        }
        catch (JsonException e)
        {
            throw new UnexpectedServerException(
                $"undecodable success body: {e.Message}", (int)resp.StatusCode);
        }
    };

    /// <summary>A text (non-JSON) success body, verbatim.</summary>
    private static readonly Decoder<string> Text =
        (resp, token) => resp.Content.ReadAsStringAsync(token);

    /// <summary>The one request pipeline: per attempt a fresh request
    /// (HttpRequestMessage disposal takes its content with it), the raw send,
    /// the shared problem decoder on non-success, the given decoder on
    /// success — all under the attempt token, retried per the request class.
    /// <paramref name="deadline"/> false exempts the call from the request
    /// deadline (only the caller's token bounds it); <paramref name="configure"/>
    /// adds per-call headers.</summary>
    private Task<T> SendAsync<T>(
        HttpMethod method, string pathAndQuery, Func<HttpContent>? content,
        Decoder<T> decode, RetryClass retryClass, string? idempotencyKey,
        CancellationToken ct, bool deadline = true,
        Action<HttpRequestMessage>? configure = null) =>
        Retry.RunAsync(retryClass, _retries, idempotencyKey, async idem =>
        {
            using var attempt = Attempt(deadline, ct);
            using var request = NewRequest(method, pathAndQuery);
            if (content is not null) request.Content = content();
            if (idem is not null) request.Headers.Add("idempotency-key", idem);
            configure?.Invoke(request);
            using var resp = await SendRawAsync(request, attempt.Token, ct).ConfigureAwait(false);
            if (!resp.IsSuccessStatusCode)
            {
                throw await ProblemAsync(resp, attempt.Token, ct).ConfigureAwait(false);
            }
            return await TransportGuardAsync(
                () => decode(resp, attempt.Token), ct, classifyReach: false)
                .ConfigureAwait(false);
        }, ct);

    private Task<T> SendAsync<T>(
        HttpMethod method, string pathAndQuery, Func<HttpContent>? content,
        JsonTypeInfo<T> responseType, RetryClass retryClass,
        string? idempotencyKey, CancellationToken ct, bool deadline = true,
        Action<HttpRequestMessage>? configure = null) =>
        SendAsync(
            method, pathAndQuery, content, Json(responseType), retryClass,
            idempotencyKey, ct, deadline, configure);

    private Task<T> GetAsync<T>(
        string pathAndQuery, JsonTypeInfo<T> responseType, CancellationToken ct) =>
        SendAsync(HttpMethod.Get, pathAndQuery, null, responseType, RetryClass.Read, null, ct);

    private static Func<HttpContent> JsonContent<B>(B body, JsonTypeInfo<B> type)
    {
        // Serialize once; each attempt wraps the same bytes in fresh content.
        var bytes = JsonSerializer.SerializeToUtf8Bytes(body, type);
        return () =>
        {
            var content = new ByteArrayContent(bytes);
            content.Headers.ContentType = new MediaTypeHeaderValue("application/json");
            return content;
        };
    }

    /// <summary>Constant-memory JSON body for the LARGE sends (batch/bulk
    /// ingest, turns): serialized straight onto the request stream per
    /// attempt — never a whole-body byte[] (a bulk chunk runs to 256 MiB).</summary>
    private static Func<HttpContent> JsonStreamContent<B>(B body, JsonTypeInfo<B> type) =>
        () => new JsonStreamContent<B>(body, type);

    private static Func<HttpContent> YamlContent(string yaml) =>
        () => new StringContent(yaml, Encoding.UTF8, "text/yaml");

    private static readonly byte[] EmptyBody = "{}"u8.ToArray();

    private static Func<HttpContent> EmptyJson() => () =>
    {
        var content = new ByteArrayContent(EmptyBody);
        content.Headers.ContentType = new MediaTypeHeaderValue("application/json");
        return content;
    };

    // -- commands -----------------------------------------------------------

    public async Task<string> CreateVersionAsync(
        string? parentVersionId = null, JsonElement? metadata = null,
        string? idempotencyKey = null, CancellationToken ct = default)
    {
        var resp = await SendAsync(
            HttpMethod.Post, "/v1/versions",
            JsonContent(
                new CreateVersionBody { ParentVersionId = parentVersionId, Metadata = metadata },
                MunariumJsonContext.Default.CreateVersionBody),
            MunariumJsonContext.Default.VersionCreated, RetryClass.Command, idempotencyKey, ct)
            .ConfigureAwait(false);
        return resp.VersionId;
    }

    public Task<ClaimOutcome> ProposeClaimAsync(
        string versionId, ClaimInput claim,
        string? idempotencyKey = null, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, $"/v1/versions/{Seg(versionId)}/claims",
            JsonContent(claim, MunariumJsonContext.Default.ClaimInput),
            MunariumJsonContext.Default.ClaimOutcome, RetryClass.Command, idempotencyKey, ct);

    public Task<EventsOutcome> AppendEventsAsync(
        string versionId, IReadOnlyList<ClaimInput> claims,
        string? candidateText = null, ulong? expectedHead = null,
        string? idempotencyKey = null, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, $"/v1/versions/{Seg(versionId)}/events",
            JsonContent(
                new EventsBody
                {
                    Claims = claims,
                    CandidateText = candidateText,
                    ExpectedHead = expectedHead,
                },
                MunariumJsonContext.Default.EventsBody),
            MunariumJsonContext.Default.EventsOutcome, RetryClass.Command, idempotencyKey, ct);

    public Task<Promise> OpenPromiseAsync(
        string versionId, string key, string kind, string description,
        string? originScope = null, string? dueScope = null,
        string? idempotencyKey = null, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, $"/v1/versions/{Seg(versionId)}/promises",
            JsonContent(
                new OpenPromiseBody
                {
                    Key = key, Kind = kind, Description = description,
                    OriginScope = originScope, DueScope = dueScope,
                },
                MunariumJsonContext.Default.OpenPromiseBody),
            MunariumJsonContext.Default.Promise, RetryClass.Command, idempotencyKey, ct);

    public async Task<bool> FulfillPromiseAsync(
        string versionId, string key,
        string? idempotencyKey = null, CancellationToken ct = default)
    {
        var resp = await SendAsync(
            HttpMethod.Post, $"/v1/versions/{Seg(versionId)}/promises/{Seg(key)}/fulfill",
            EmptyJson(),
            MunariumJsonContext.Default.FulfillResponse, RetryClass.Command, idempotencyKey, ct)
            .ConfigureAwait(false);
        return resp.Fulfilled;
    }

    public Task<Anchor> LockAnchorAsync(
        string versionId, string subject, string key, string value,
        string? scopePath = null, JsonElement? evidence = null,
        string? idempotencyKey = null, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, $"/v1/versions/{Seg(versionId)}/anchors",
            JsonContent(
                new LockAnchorBody
                {
                    Subject = subject, Key = key, Value = value,
                    ScopePath = scopePath, Evidence = evidence,
                },
                MunariumJsonContext.Default.LockAnchorBody),
            MunariumJsonContext.Default.Anchor, RetryClass.Command, idempotencyKey, ct);

    public Task RecordCountsAsync(
        string versionId, string key, string scopePath, ulong count,
        ulong? budget = null, string? idempotencyKey = null, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, $"/v1/versions/{Seg(versionId)}/counters",
            JsonContent(
                new RecordCountsBody
                {
                    Key = key, ScopePath = scopePath, Count = count, Budget = budget,
                },
                MunariumJsonContext.Default.RecordCountsBody),
            MunariumJsonContext.Default.JsonElement, RetryClass.Command, idempotencyKey, ct);

    public Task UpsertDigestAsync(Digest digest, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Put, $"/v1/versions/{Seg(digest.VersionId)}/digests",
            JsonContent(digest, MunariumJsonContext.Default.Digest),
            MunariumJsonContext.Default.JsonElement, RetryClass.Write, null, ct);

    // -- query --------------------------------------------------------------

    public async Task<ulong> HeadAsync(string versionId, CancellationToken ct = default)
    {
        var resp = await GetAsync(
            $"/v1/versions/{Seg(versionId)}/head",
            MunariumJsonContext.Default.HeadResponse, ct).ConfigureAwait(false);
        return resp.HeadSeq;
    }

    public Task<ClaimLookup> GetClaimAsync(string claimId, CancellationToken ct = default) =>
        GetAsync($"/v1/claims/{Seg(claimId)}", MunariumJsonContext.Default.ClaimLookup, ct);

    public Task<FactsPage> FactsAsync(
        string versionId, string? scopePrefix = null, ulong? asOfSeq = null,
        IReadOnlyList<string>? statuses = null, int? limit = null,
        CancellationToken ct = default)
    {
        foreach (var status in statuses ?? [])
        {
            if (status is not ("accepted" or "disputed"))
            {
                throw new InvalidInputException(
                    $"unknown claim status '{status}' (accepted | disputed)");
            }
        }
        return GetAsync(
            $"/v1/versions/{Seg(versionId)}/facts" + Query(
                ("scope_prefix", scopePrefix),
                ("as_of_seq", asOfSeq?.ToString()),
                ("statuses", statuses is { Count: > 0 } ? string.Join(',', statuses) : null),
                ("limit", limit?.ToString())),
            MunariumJsonContext.Default.FactsPage, ct);
    }

    public async Task<IReadOnlyList<string>> LineageAsync(
        string versionId, CancellationToken ct = default)
    {
        var resp = await GetAsync(
            $"/v1/versions/{Seg(versionId)}/lineage",
            MunariumJsonContext.Default.LineageResponse, ct).ConfigureAwait(false);
        return resp.VersionIds;
    }

    public async Task<IReadOnlyList<Anchor>> AnchorsAsync(
        string versionId, ulong? asOfSeq = null, CancellationToken ct = default)
    {
        var resp = await GetAsync(
            $"/v1/versions/{Seg(versionId)}/anchors" + Query(("as_of_seq", asOfSeq?.ToString())),
            MunariumJsonContext.Default.AnchorsResponse, ct).ConfigureAwait(false);
        return resp.Anchors;
    }

    public async Task<IReadOnlyList<Promise>> PromisesAsync(
        string versionId, ulong? asOfSeq = null, string? status = null,
        CancellationToken ct = default)
    {
        Validation.CheckPromiseStatus(status);
        var resp = await GetAsync(
            $"/v1/versions/{Seg(versionId)}/promises" + Query(
                ("as_of_seq", asOfSeq?.ToString()), ("status", status)),
            MunariumJsonContext.Default.PromisesResponse, ct).ConfigureAwait(false);
        return resp.Promises;
    }

    public async Task<IReadOnlyList<Counter>> CountersAsync(
        string versionId, ulong? asOfSeq = null, CancellationToken ct = default)
    {
        var resp = await GetAsync(
            $"/v1/versions/{Seg(versionId)}/counters" + Query(("as_of_seq", asOfSeq?.ToString())),
            MunariumJsonContext.Default.CountersResponse, ct).ConfigureAwait(false);
        return resp.Counters;
    }

    public async Task<IReadOnlyList<Digest>> DigestsAsync(
        string versionId, CancellationToken ct = default)
    {
        var resp = await GetAsync(
            $"/v1/versions/{Seg(versionId)}/digests",
            MunariumJsonContext.Default.DigestsResponse, ct).ConfigureAwait(false);
        return resp.Digests;
    }

    public Task<ComposedContext> ComposeContextAsync(
        string versionId, string? scope = null, ulong? budgetTokens = null,
        int? factLimit = null, ulong? asOfSeq = null, CancellationToken ct = default) =>
        GetAsync(
            $"/v1/versions/{Seg(versionId)}/context" + Query(
                ("scope", scope),
                ("budget_tokens", budgetTokens?.ToString()),
                ("fact_limit", factLimit?.ToString()),
                ("as_of_seq", asOfSeq?.ToString())),
            MunariumJsonContext.Default.ComposedContext, ct);

    // -- ingest -------------------------------------------------------------

    public Task<PutSourceResult> PutSourceAsync(
        ChunkSource chunks, string declaredSha256 = "",
        string? mediaType = null, string? filename = null, string? shapeRef = null,
        CancellationToken ct = default) =>
        // Uploads are idempotent by content address, so transient failures
        // retry — the ChunkSource factory rebuilds the body per attempt.
        // Streaming ingest is exempt from the request deadline: only the
        // caller's token bounds it. Constant-memory push-stream body.
        SendAsync(
            HttpMethod.Put, "/v1/sources",
            () => new AsyncChunkContent(chunks(), mediaType ?? "application/octet-stream"),
            MunariumJsonContext.Default.PutSourceResult, RetryClass.Read, null, ct,
            deadline: false,
            configure: request =>
            {
                if (declaredSha256.Length > 0) request.Headers.Add("x-content-sha256", declaredSha256);
                if (filename is not null) request.Headers.Add("x-filename", filename);
                if (shapeRef is not null) request.Headers.Add("x-shape-ref", shapeRef);
            });

    public Task<RecordIngestResult> RecordIngestAsync(
        string versionId, string contentHash, string? shapeRef = null,
        CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, $"/v1/versions/{Seg(versionId)}/ingests",
            JsonContent(
                new RecordIngestBody { ContentHash = contentHash, ShapeRef = shapeRef },
                MunariumJsonContext.Default.RecordIngestBody),
            MunariumJsonContext.Default.RecordIngestResult, RetryClass.Write, null, ct);

    // -- retrieval ----------------------------------------------------------

    public Task<SearchResult> SearchAsync(
        string query, string shapeRef, uint? topK = null, string? indexVersion = null,
        JsonElement? filter = null, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, "/v1/search",
            JsonContent(
                new SearchBody
                {
                    Query = query, ShapeRef = shapeRef, TopK = topK,
                    IndexVersion = indexVersion, Filter = filter,
                },
                MunariumJsonContext.Default.SearchBody),
            // a read that happens to be a POST — same retry class as GETs
            MunariumJsonContext.Default.SearchResult, RetryClass.Read, null, ct);

    public Task<IndexStatus> IndexStatusAsync(string shapeRef, CancellationToken ct = default) =>
        GetAsync($"/v1/indexes/{Seg(shapeRef)}", MunariumJsonContext.Default.IndexStatus, ct);

    public Task<IndexStatus> BuildIndexAsync(
        string shapeRef, string? versionId = null, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post,
            $"/v1/indexes/{Seg(shapeRef)}/build" + Query(("version_id", versionId)),
            null, MunariumJsonContext.Default.IndexStatus, RetryClass.Write, null, ct);

    // -- runbooks + shapes --------------------------------------------------

    public Task<ApplyShapeResult> ApplyShapeAsync(
        string yaml, string? versionId = null, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, "/v1/shapes" + Query(("version_id", versionId)),
            YamlContent(yaml), MunariumJsonContext.Default.ApplyShapeResult,
            RetryClass.Write, null, ct);

    public async Task<string> ApplyRunbookAsync(string yaml, CancellationToken ct = default)
    {
        var resp = await SendAsync(
            HttpMethod.Post, "/v1/runbooks", YamlContent(yaml),
            MunariumJsonContext.Default.RunbookApplied, RetryClass.Write, null, ct)
            .ConfigureAwait(false);
        return resp.RunbookRef;
    }

    public Task<RunbookRun> RunRunbookAsync(
        string name, string? versionId = null, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post,
            $"/v1/runbooks/{Seg(name)}/runs" + Query(("version_id", versionId)),
            null, MunariumJsonContext.Default.RunbookRun, RetryClass.Write, null, ct);

    public Task<RunStatus> GetRunAsync(string runId, CancellationToken ct = default) =>
        GetAsync($"/v1/runs/{Seg(runId)}", MunariumJsonContext.Default.RunStatus, ct);

    public Task<RunbookRun> ApproveStepAsync(
        string runId, uint ordinal, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, $"/v1/runs/{Seg(runId)}/steps/{ordinal}/approve",
            null, MunariumJsonContext.Default.RunbookRun, RetryClass.Write, null, ct);

    // -- providers ----------------------------------------------------------

    public async Task<string> ApplyConfigAsync(string yaml, CancellationToken ct = default)
    {
        var resp = await SendAsync(
            HttpMethod.Post, "/v1/providers", YamlContent(yaml),
            MunariumJsonContext.Default.ProviderApplied, RetryClass.Write, null, ct)
            .ConfigureAwait(false);
        return resp.ConfigName;
    }

    public Task<ProviderHealth> HealthAsync(string name, CancellationToken ct = default) =>
        GetAsync($"/v1/providers/{Seg(name)}/health", MunariumJsonContext.Default.ProviderHealth, ct);

    public Task<HealthAiResult> HealthAiAsync(CancellationToken ct = default) =>
        GetAsync("/healthai", MunariumJsonContext.Default.HealthAiResult, ct);

    public Task<CompleteResult> CompleteAsync(
        string name, string prompt, string? model = null, string? system = null,
        uint? maxTokens = null, double? temperature = null, string? versionId = null,
        string? provider = null, string? tier = null,
        CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, $"/v1/providers/{Seg(name)}/complete",
            JsonContent(
                new CompleteBody
                {
                    Prompt = prompt, Model = model, Provider = provider, Tier = tier,
                    System = system, MaxTokens = maxTokens, Temperature = temperature,
                    VersionId = versionId,
                },
                MunariumJsonContext.Default.CompleteBody),
            MunariumJsonContext.Default.CompleteResult, RetryClass.Write, null, ct);

    public Task<EmbedResult> EmbedAsync(
        string name, IReadOnlyList<string> inputs, string? model = null,
        string? versionId = null, string? provider = null, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, $"/v1/providers/{Seg(name)}/embed",
            JsonContent(
                new EmbedBody
                {
                    Inputs = inputs, Model = model, Provider = provider, VersionId = versionId,
                },
                MunariumJsonContext.Default.EmbedBody),
            MunariumJsonContext.Default.EmbedResult, RetryClass.Write, null, ct);
}

/// <summary>Constant-memory JSON body: the model is serialized onto the
/// request stream (chunked transfer; no Content-Length, no byte[]).</summary>
internal sealed class JsonStreamContent<T> : HttpContent
{
    private readonly T _value;
    private readonly JsonTypeInfo<T> _type;

    internal JsonStreamContent(T value, JsonTypeInfo<T> type)
    {
        _value = value;
        _type = type;
        Headers.ContentType = new MediaTypeHeaderValue("application/json");
    }

    protected override Task SerializeToStreamAsync(
        Stream stream, System.Net.TransportContext? context) =>
        SerializeToStreamAsync(stream, context, CancellationToken.None);

    protected override Task SerializeToStreamAsync(
        Stream stream, System.Net.TransportContext? context, CancellationToken ct) =>
        JsonSerializer.SerializeAsync(stream, _value, _type, ct);

    protected override bool TryComputeLength(out long length)
    {
        length = 0;
        return false;
    }
}

/// <summary>Constant-memory push-stream body over an async chunk sequence.</summary>
internal sealed class AsyncChunkContent : HttpContent
{
    private readonly IAsyncEnumerable<ReadOnlyMemory<byte>> _chunks;

    internal AsyncChunkContent(IAsyncEnumerable<ReadOnlyMemory<byte>> chunks, string mediaType)
    {
        _chunks = chunks;
        Headers.ContentType = new MediaTypeHeaderValue(mediaType);
    }

    protected override Task SerializeToStreamAsync(
        Stream stream, System.Net.TransportContext? context) =>
        SerializeToStreamAsync(stream, context, CancellationToken.None);

    protected override async Task SerializeToStreamAsync(
        Stream stream, System.Net.TransportContext? context, CancellationToken ct)
    {
        await foreach (var chunk in _chunks.WithCancellation(ct).ConfigureAwait(false))
        {
            await stream.WriteAsync(chunk, ct).ConfigureAwait(false);
        }
    }

    protected override bool TryComputeLength(out long length)
    {
        length = 0;
        return false;
    }
}
