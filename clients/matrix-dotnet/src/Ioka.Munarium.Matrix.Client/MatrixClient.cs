// SPDX-License-Identifier: Apache-2.0
// The client. One transport (REST), one request pipeline, and a surface that
// is small because Matrix's whole job is small: register assets, run the three
// modes, read what happened.
//
// Async only, and not as a style preference. A synchronous wrapper over
// HttpClient is the classic way to deadlock an application: .Result on a
// captured synchronization context blocks the thread the continuation needs,
// and the failure shows up in production under load rather than in a test. A
// client that offers a sync overload will have that overload called, so the
// only safe number of them is zero. The Python client ships sync and async
// because Python's sync HTTP is a genuinely separate, safe implementation;
// .NET has no such thing to offer.

using System.Net.Http.Headers;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization.Metadata;

namespace Ioka.Munarium.Matrix.Client;

/// <summary>Connection options.</summary>
public sealed record MatrixClientOptions
{
    /// <summary>Matrix's REST base URL (http://host:8180, or :443 behind a
    /// gateway). Not the gRPC endpoint — see <see cref="MatrixClient"/>.</summary>
    public required string Endpoint { get; init; }

    /// <summary>Bearer token. Matrix gates routes by the token's role, so a
    /// read-only token 403s on apply and a non-mgmt token 403s on the
    /// journal; that is the deployment's answer, not this client's.</summary>
    public string? Token { get; init; }

    /// <summary>The acting operator id, sent as <c>X-Munarium-Uid</c>.
    /// Matrix itself does not read it — the munarium-server does, on every
    /// /v1 request — but sending it keeps one identity across both journals
    /// when the same operator drives them.</summary>
    public string? Uid { get; init; }

    /// <summary>Per-request deadline. Long enough for a verify that runs a
    /// contract's whole suite against a cold warehouse, short enough that a
    /// wedged call is not forever.</summary>
    public TimeSpan Timeout { get; init; } = TimeSpan.FromSeconds(30);
}

/// <summary>The .NET client for Munarium Matrix, the structured-evidence
/// plane.</summary>
/// <remarks>
/// <para>
/// <b>REST only — there is no gRPC transport here.</b> Matrix's gRPC plane
/// serves <c>Execute</c> alone, and <c>Execute</c> is service-to-service: the
/// munarium-server calls it while answering a turn, so that the evidence an
/// answer cites is sealed by the same process that ran the query. An
/// application is on the other side of the server, asking for an answer. If
/// that ever changes, this client grows a transport rather than acquiring a
/// sibling.
/// </para>
/// <para>
/// Three absences, each a decision rather than a missing feature.
/// <b>No sealing:</b> a manifest is a statement about work the <i>sealer</i>
/// did, so an SDK offering it would invite an application to assert provenance
/// it cannot vouch for. Evidence is <i>read</i> through the server's client,
/// resolving an <c>[evidence/&lt;id&gt;#&lt;row&gt;]</c> citation.
/// <b>No local validation:</b> <see cref="ValidateAsync"/> posts the YAML and
/// returns Matrix's own findings, because a client carrying its own copy of
/// the rules would drift from the service that enforces them, and the drift
/// would surface as an asset that validates here and is refused there.
/// <b>No SQL:</b> nothing on this surface takes a statement; queries are
/// pre-declared contracts and views, executed by name.
/// </para>
/// </remarks>
public sealed class MatrixClient : IDisposable
{
    /// <summary>The Matrix and munarium-server version this client is in
    /// lockstep with. A wire-surface change bumps all three together.</summary>
    public const string TargetVersion = "1.0.0";

    private readonly HttpClient _http;
    private readonly bool _ownsHttp;
    private readonly string _base;
    private readonly TimeSpan _timeout;
    private readonly AuthenticationHeaderValue? _auth;
    private readonly string? _uid;

    /// <summary>Construct against an endpoint. Pass an
    /// <paramref name="httpClient"/> from IHttpClientFactory when you manage
    /// pooling yourself — a caller-supplied client is never mutated, since
    /// auth rides each request.</summary>
    public MatrixClient(MatrixClientOptions options, HttpClient? httpClient = null)
    {
        ArgumentNullException.ThrowIfNull(options);
        _ownsHttp = httpClient is null;
        _http = httpClient ?? new HttpClient(new SocketsHttpHandler
        {
            ConnectTimeout = TimeSpan.FromSeconds(5),
            // The client is explicitly long-lived; without a lifetime the pool
            // pins retired IPs across a DNS cutover forever.
            PooledConnectionLifetime = TimeSpan.FromMinutes(5),
        })
        {
            // The per-request deadline rides the attempt token instead, so a
            // deadline expiry can be typed as a transport failure rather than
            // arriving as a bare TaskCanceledException.
            Timeout = System.Threading.Timeout.InfiniteTimeSpan,
        };
        _base = options.Endpoint.TrimEnd('/');
        _timeout = options.Timeout;
        _auth = options.Token is null
            ? null
            : new AuthenticationHeaderValue("Bearer", options.Token);
        _uid = options.Uid;
    }

    /// <summary>Convenience over <see cref="MatrixClientOptions"/>.</summary>
    public MatrixClient(string endpoint, string? token = null, string? uid = null)
        : this(new MatrixClientOptions { Endpoint = endpoint, Token = token, Uid = uid })
    {
    }

    public void Dispose()
    {
        if (_ownsHttp) _http.Dispose();
    }

    // -- meta ---------------------------------------------------------------

    /// <summary>Matrix's version, its contract version, the role it serves,
    /// and whether it agrees with the server it seals into.</summary>
    public Task<MatrixVersion> VersionAsync(CancellationToken ct = default) =>
        SendAsync(HttpMethod.Get, "/version", null, MatrixJsonContext.Default.MatrixVersion, ct);

    /// <summary>Liveness. A refusal is an ANSWER here, so this returns false
    /// rather than throwing — a health check that throws forces every caller
    /// to write the try/catch this one has already written.</summary>
    public async Task<bool> HealthzAsync(CancellationToken ct = default)
    {
        try
        {
            var body = await SendAsync(
                HttpMethod.Get, "/healthz", null,
                MatrixJsonContext.Default.JsonElement, ct).ConfigureAwait(false);
            return body.ValueKind == JsonValueKind.Object
                && body.TryGetProperty("ok", out var ok)
                && ok.ValueKind == JsonValueKind.True;
        }
        catch (MatrixException)
        {
            return false;
        }
    }

    /// <summary>Registration health per source. Registration, NOT
    /// connectivity: probing every source on a health call would make a
    /// health endpoint an outbound-traffic amplifier.</summary>
    public Task<JsonElement> HealthdataAsync(CancellationToken ct = default) =>
        SendAsync(HttpMethod.Get, "/healthdata", null, MatrixJsonContext.Default.JsonElement, ct);

    // -- registry -----------------------------------------------------------

    /// <summary>Apply one asset, kind-sniffed by Matrix from its
    /// <c>kind:</c> line.</summary>
    /// <remarks>Re-applying identical bytes is
    /// <see cref="ApplyOutcome.Unchanged"/>, not an error: that is ordinary
    /// GitOps. The same version with DIFFERENT bytes is refused, because a
    /// version is provenance — sealed evidence cites it.</remarks>
    public Task<ApplyOutcome> ApplyAsync(string yaml, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, "/v1/assets", Yaml(yaml),
            MatrixJsonContext.Default.ApplyOutcome, ct);

    /// <summary>Matrix's own findings, from the same validators
    /// <c>mxctl validate</c> uses, without applying anything.</summary>
    public Task<ValidationOutcome> ValidateAsync(string yaml, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, "/v1/assets/validate", Yaml(yaml),
            MatrixJsonContext.Default.ValidationOutcome, ct);

    /// <summary>List applied assets of one kind.</summary>
    /// <param name="kind">A route segment: datasources, contracts, mappings,
    /// metricviews, dataviews.</param>
    /// <param name="allVersions">Every version rather than the latest.</param>
    /// <param name="ct">Cancellation.</param>
    public async Task<IReadOnlyList<AssetSummary>> ListAssetsAsync(
        string kind, bool allVersions = false, CancellationToken ct = default)
    {
        // The parameter is `all_versions`. Anything else is ignored in silence
        // by the service's deserializer, which is the worst possible failure
        // for a flag: the call succeeds and answers the other question.
        var query = allVersions ? "?all_versions=true" : "";
        var page = await SendAsync(
            HttpMethod.Get, $"/v1/{Seg(kind)}{query}", null,
            MatrixJsonContext.Default.AssetListResponse, ct).ConfigureAwait(false);
        return page.Assets;
    }

    /// <summary>The applied YAML, verbatim — the bytes Matrix stored, not a
    /// re-serialisation of a parse.</summary>
    public Task<string> GetYamlAsync(string kind, string name, CancellationToken ct = default) =>
        SendTextAsync(HttpMethod.Get, $"/v1/{Seg(kind)}/{Seg(name)}", null, ct);

    // -- operations ---------------------------------------------------------

    /// <summary>What the configured role can actually see, and what the
    /// schema holds — with a draft contract the operator edits, never applied
    /// automatically.</summary>
    public Task<JsonElement> IntrospectAsync(string source, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, $"/v1/datasources/{Seg(source)}/introspect", null,
            MatrixJsonContext.Default.JsonElement, ct);

    /// <summary>Reachability now. A refusal is an ANSWER here —
    /// <c>reachable: false</c> with a typed reason — not an exception.</summary>
    public Task<JsonElement> ProbeAsync(string source, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, $"/v1/datasources/{Seg(source)}/probe", null,
            MatrixJsonContext.Default.JsonElement, ct);

    /// <summary>Queue a materialization (mode A), one job per authorization
    /// class — a collection carries exactly one class, so a multi-class
    /// source needs one run each.</summary>
    public Task<JobAccepted> SyncAsync(string source, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, $"/v1/datasources/{Seg(source)}/sync", null,
            MatrixJsonContext.Default.JobAccepted, ct);

    /// <summary>Run a query contract's verified questions — its regression
    /// suite.</summary>
    /// <remarks>The call succeeding and the CONTRACT passing are different
    /// things: check <see cref="VerifyOutcome.Failed"/>. <c>mxctl</c> exits 3
    /// on a non-zero <c>failed</c> for exactly this reason, so CI can tell a
    /// broken contract from a broken command.</remarks>
    public Task<VerifyOutcome> VerifyAsync(string contract, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, $"/v1/contracts/{Seg(contract)}/verify", null,
            MatrixJsonContext.Default.VerifyOutcome, ct);

    /// <summary>The same for a metric view or a native data view, recording
    /// the definition fingerprint the questions ran under.</summary>
    /// <remarks>
    /// <para>
    /// A metric view first, a data view when there is none by that name — the
    /// caller names the view, not the route it happens to live on.
    /// </para>
    /// <para>
    /// The fallback fires on two different answers, because Matrix spells
    /// "no such asset" two ways: a bare 404 when the store misses, and a
    /// <c>not_covered</c> refusal (HTTP 422) when the miss goes through the
    /// asset loader — which is the path <c>/v1/metricviews/{name}/verify</c>
    /// actually takes. Keying on 404 alone reads correctly and never fires,
    /// so every data view would be reported as an unknown metric view.
    /// </para>
    /// <para>
    /// When the fallback ALSO says no such asset, the metric-view error is
    /// what surfaces: the view exists nowhere, and the first answer is the
    /// one the caller asked for. When the fallback fails for any other reason
    /// the data view exists and something else went wrong, so that error
    /// surfaces instead — reporting "no such metric view" there would hide a
    /// real fault behind a naming detail.
    /// </para>
    /// </remarks>
    public async Task<VerifyOutcome> VerifyViewAsync(string view, CancellationToken ct = default)
    {
        MatrixException first;
        try
        {
            return await SendAsync(
                HttpMethod.Post, $"/v1/metricviews/{Seg(view)}/verify", null,
                MatrixJsonContext.Default.VerifyOutcome, ct).ConfigureAwait(false);
        }
        catch (MatrixException e) when (e.IsNoSuchAsset)
        {
            first = e;
        }

        try
        {
            return await SendAsync(
                HttpMethod.Post, $"/v1/dataviews/{Seg(view)}/verify", null,
                MatrixJsonContext.Default.VerifyOutcome, ct).ConfigureAwait(false);
        }
        catch (MatrixException e) when (e.IsNoSuchAsset)
        {
            throw first;
        }
    }

    /// <summary>Queue a reconcile pass (mode C).</summary>
    public Task<JobAccepted> ReconcileAsync(string mapping, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, $"/v1/mappings/{Seg(mapping)}/run", null,
            MatrixJsonContext.Default.JobAccepted, ct);

    // -- promotion ----------------------------------------------------------

    /// <summary>Whether a mapping may write canon, the gate numbers from its
    /// latest completed run, and the state of the most recent pass.</summary>
    public Task<PromotionStatus> PromotionStatusAsync(
        string mapping, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Get, $"/v1/mappings/{Seg(mapping)}/promotion", null,
            MatrixJsonContext.Default.PromotionStatus, ct);

    /// <summary>The promotion gates over time, newest first, each scored
    /// against the thresholds in force RIGHT NOW.</summary>
    /// <remarks>Which is the point: lowering a threshold and re-reading this
    /// says exactly which past runs the new number would have admitted,
    /// before anything is promoted under it.</remarks>
    public Task<JsonElement> GateHistoryAsync(
        string mapping, int? limit = null, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Get,
            $"/v1/mappings/{Seg(mapping)}/gate-history{(limit is null ? "" : $"?limit={limit}")}",
            null, MatrixJsonContext.Default.JsonElement, ct);

    /// <summary>Let a mapping's claims reach the ledger.</summary>
    /// <remarks>
    /// The gates (identity precision, value conformance) are checked by
    /// MATRIX at the decision, not here: a client that pre-checked them would
    /// be a second opinion nobody audited. <paramref name="decisionId"/> is
    /// the operator's record — a ticket, a change number — and it is required
    /// because a promotion nobody can trace to a decision is a promotion
    /// nobody made.
    /// </remarks>
    public Task<PromotionStatus> PromoteAsync(
        string mapping, string decisionId, string actor, string? reason = null,
        CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, $"/v1/mappings/{Seg(mapping)}/promote",
            Json(new PromoteBody { DecisionId = decisionId, Actor = actor, Reason = reason },
                MatrixJsonContext.Default.PromoteBody),
            MatrixJsonContext.Default.PromotionStatus, ct);

    /// <summary>Stop the writes, effective on the next reconcile poll.
    /// Nothing already proposed is touched — that is what
    /// <see cref="RollbackAsync"/> is for.</summary>
    public Task<PromotionStatus> DemoteAsync(
        string mapping, string decisionId, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, $"/v1/mappings/{Seg(mapping)}/demote",
            Json(new DecisionBody { DecisionId = decisionId },
                MatrixJsonContext.Default.DecisionBody),
            MatrixJsonContext.Default.PromotionStatus, ct);

    /// <summary>Undo what a promoted mapping wrote — by SUPERSESSION, never
    /// by deletion. History is not rewritten, and the correcting claims carry
    /// <c>origin.kind = "rollback"</c> so a reviewer sees both moves.</summary>
    public Task<JsonElement> RollbackAsync(
        string mapping, string decisionId, CancellationToken ct = default) =>
        SendAsync(
            HttpMethod.Post, $"/v1/mappings/{Seg(mapping)}/rollback",
            Json(new DecisionBody { DecisionId = decisionId },
                MatrixJsonContext.Default.DecisionBody),
            MatrixJsonContext.Default.JsonElement, ct);

    // -- journal ------------------------------------------------------------

    /// <summary>Every operation, redacted by default: parameters and results
    /// never appear, only what happened and how it ended. Management role
    /// only — a journal readable by the tokens it audits is not an audit
    /// log.</summary>
    public async Task<IReadOnlyList<JsonElement>> JournalAsync(
        int limit = 50, CancellationToken ct = default)
    {
        var page = await SendAsync(
            HttpMethod.Get, $"/v1/journal?limit={limit}", null,
            MatrixJsonContext.Default.JournalListResponse, ct).ConfigureAwait(false);
        return page.Entries;
    }

    // -- the request pipeline -----------------------------------------------

    /// <summary>Percent-encode a path segment. Asset names are free-form
    /// enough that a raw '/' or '?' must not change the route shape.</summary>
    private static string Seg(string s) => Uri.EscapeDataString(s);

    private static Func<HttpContent> Yaml(string yaml) =>
        () => new StringContent(yaml, Encoding.UTF8, "text/yaml");

    private static Func<HttpContent> Json<TBody>(TBody body, JsonTypeInfo<TBody> type)
    {
        // Serialize once; the content object itself is single-use.
        var bytes = JsonSerializer.SerializeToUtf8Bytes(body, type);
        return () =>
        {
            var content = new ByteArrayContent(bytes);
            content.Headers.ContentType = new MediaTypeHeaderValue("application/json");
            return content;
        };
    }

    /// <summary>THE one request path: build, send under the deadline, decode a
    /// non-success body through the refusal registry, decode a success body
    /// into the typed model.</summary>
    /// <remarks>
    /// There is deliberately no retry loop. Matrix's refusals already say
    /// whether retrying could help (<see cref="MatrixException.Retryable"/>),
    /// and the two classes that could — unavailable and exhausted — want
    /// pacing the caller owns: an exhausted budget refusal carries the wait
    /// the service asked for, and burning it down inside a client would spend
    /// the caller's budget on its behalf.
    /// </remarks>
    private async Task<HttpResponseMessage> SendRawAsync(
        HttpMethod method, string pathAndQuery, Func<HttpContent>? content,
        CancellationTokenSource attempt, CancellationToken ct)
    {
        using var request = new HttpRequestMessage(method, _base + pathAndQuery);
        request.Headers.Authorization = _auth;
        if (_uid is not null) request.Headers.Add("X-Munarium-Uid", _uid);
        if (content is not null) request.Content = content();
        try
        {
            return await _http
                .SendAsync(request, HttpCompletionOption.ResponseHeadersRead, attempt.Token)
                .ConfigureAwait(false);
        }
        catch (OperationCanceledException) when (ct.IsCancellationRequested)
        {
            throw; // caller cancellation is not a transport failure
        }
        catch (Exception e)
            when (e is HttpRequestException or OperationCanceledException or IOException)
        {
            throw Errors.FromTransport(e);
        }
    }

    private async Task<string> ReadBodyAsync(
        HttpResponseMessage resp, CancellationTokenSource attempt, CancellationToken ct)
    {
        try
        {
            return await resp.Content.ReadAsStringAsync(attempt.Token).ConfigureAwait(false);
        }
        catch (OperationCanceledException) when (ct.IsCancellationRequested)
        {
            throw;
        }
        catch (Exception e)
            when (e is HttpRequestException or OperationCanceledException or IOException)
        {
            throw Errors.FromTransport(e);
        }
    }

    private async Task<T> SendAsync<T>(
        HttpMethod method, string pathAndQuery, Func<HttpContent>? content,
        JsonTypeInfo<T> responseType, CancellationToken ct)
    {
        var body = await SendTextAsync(method, pathAndQuery, content, ct).ConfigureAwait(false);
        try
        {
            return JsonSerializer.Deserialize(body, responseType)
                ?? throw new MatrixException("matrix answered an empty body");
        }
        catch (JsonException e)
        {
            throw new MatrixException($"undecodable success body: {e.Message}", inner: e);
        }
    }

    private async Task<string> SendTextAsync(
        HttpMethod method, string pathAndQuery, Func<HttpContent>? content, CancellationToken ct)
    {
        using var attempt = CancellationTokenSource.CreateLinkedTokenSource(ct);
        attempt.CancelAfter(_timeout);
        using var resp = await SendRawAsync(method, pathAndQuery, content, attempt, ct)
            .ConfigureAwait(false);
        var body = await ReadBodyAsync(resp, attempt, ct).ConfigureAwait(false);
        if (!resp.IsSuccessStatusCode)
        {
            throw Errors.FromProblem((int)resp.StatusCode, body);
        }
        return body;
    }
}
