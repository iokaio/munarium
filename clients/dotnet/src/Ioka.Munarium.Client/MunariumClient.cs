// SPDX-License-Identifier: Apache-2.0
// The facade: ten plane sub-clients over one connection + auth config, plus
// the head-conflict write loop (invariant #2) and the GET /version handshake.
//
// The invariants this client encodes:
// 1. Disputed != error — ClaimOutcome.IsDisputed is SUCCESS state.
// 2. Head conflicts are normal — ProposeClaimWithRetryAsync re-reads,
//    rebuilds, retries with a fresh idempotency key per attempt.
// 3. One pin bounds everything — asOfSeq threads through every query.
// 4. Every retrieval answer carries a ProvenanceEnvelope (required member).
// 5. Append-only — no update/delete methods; corrections name SupersedesId.
// 6. Idempotency keys auto-generate per command and are caller-overridable.

namespace Ioka.Munarium.Client;

/// <summary>Connection + behavior options shared by both transports.</summary>
public sealed record MunariumClientOptions
{
    /// <summary>REST base URL (http://host:8080) or gRPC endpoint
    /// (http://host:50051; https enables TLS).</summary>
    public required string Endpoint { get; init; }

    /// <summary>Bearer token — a static token, or a capability JWT for the
    /// data plane; null only works against MUNARIUM_AUTH_MODE=disabled.</summary>
    public string? Token { get; init; }

    /// <summary>The acting end-user id (uid contract). Sent as X-Munarium-Uid
    /// (REST) / munarium-uid metadata (gRPC) on every request. Required by
    /// servers running MUNARIUM_REQUIRE_UID=true (the default); when the bearer
    /// is a capability JWT it must equal the token's sub.</summary>
    public string? Uid { get; init; }

    public TimeSpan ConnectTimeout { get; init; } = TimeSpan.FromSeconds(5);

    /// <summary>Per-request deadline, enforced by the client per attempt.
    /// Streaming ingest, the paid/large sends (turns, file/batch/bulk
    /// ingest), and the SSE turn stream are exempt — only your token bounds
    /// them. On REST with a caller-supplied <see cref="HttpClient"/>, its
    /// own <c>Timeout</c> (default 100 s) still applies to the exempt sends;
    /// set it to <c>Timeout.InfiniteTimeSpan</c> (see <see cref="MunariumClient.Rest"/>).</summary>
    public TimeSpan RequestTimeout { get; init; } = TimeSpan.FromSeconds(30);

    /// <summary>Extra attempts for reads (and search) on transient failures.
    /// Commands re-send the SAME idempotency key only when the request
    /// provably never reached the server (a connect-phase failure — REST
    /// only) or the server shed it before executing (the typed overloaded) —
    /// the server records an idempotency key AFTER a command completes, so
    /// re-sending a possibly-delivered command could execute it twice. On
    /// gRPC, commands are NEVER re-sent on a transport failure: no gRPC
    /// failure is provably undelivered, so only the typed overloaded is
    /// retried there. Non-replayable writes send exactly once.</summary>
    public int ReadRetries { get; init; } = 2;
}

/// <summary>Options for the head-conflict write loop.</summary>
public sealed record WriteLoopOptions
{
    /// <summary>Max attempts including the first.</summary>
    public int MaxAttempts { get; init; } = 3;
}

/// <summary>Official .NET client for munarium-server: one plane surface, two
/// transports (REST + gRPC), typed errors keyed on the problem-slug
/// registry, and the head-conflict write loop built in.</summary>
public sealed class MunariumClient : IAsyncDisposable
{
    /// <summary>The server version this client tracks (lockstep with the
    /// repo workspace).</summary>
    public const string TargetServerVersion = "1.0.0";

    public ICommandsPlane Commands => _transport;
    public IQueryPlane Query => _transport;
    public IIngestPlane Ingest => _transport;
    public IRetrievalPlane Retrieval => _transport;
    public IRunbooksPlane Runbooks => _transport;
    public IProvidersPlane Providers => _transport;

    /// <summary>Multiturn sessions + the streaming turn plane.</summary>
    public ISessionsPlane Sessions => _transport;

    /// <summary>Capability-token mint/audit/revoke (mgmt role).</summary>
    public IAccessTokensPlane Tokens => _transport;

    /// <summary>Management reports (mgmt role; REST-only).</summary>
    public IReportsPlane Reports => _transport;

    /// <summary>Guided runbook authoring (REST-only).</summary>
    public IAuthoringPlane Authoring => _transport;

    /// <summary>Sealed evidence READS (REST-only). Resolve an
    /// <c>[evidence/&lt;id&gt;#&lt;row&gt;]</c> citation to what an answer was
    /// computed from. Sealing is not here on purpose — see
    /// <see cref="IEvidencePlane"/>.</summary>
    public IEvidencePlane Evidence => _transport;

    private readonly ITransport _transport;

    private MunariumClient(ITransport transport) => _transport = transport;

    /// <summary>REST transport (:8080 in the demo posture; :443 behind
    /// gateways). Pass an <see cref="HttpClient"/> from IHttpClientFactory
    /// when you manage pooling yourself — a caller-supplied client is never
    /// mutated (auth rides each request).</summary>
    /// <remarks>The request deadline (<see cref="MunariumClientOptions.RequestTimeout"/>)
    /// is enforced by this client per attempt, and the paid/large sends
    /// (turns, file/batch/bulk ingest, streaming source upload) and the SSE
    /// turn stream are deliberately deadline-exempt. A client this library
    /// creates has <c>Timeout = Timeout.InfiniteTimeSpan</c> so that holds;
    /// a client YOU hand in keeps its own <see cref="HttpClient.Timeout"/>
    /// (default 100 s), which still caps those exempt sends — set
    /// <c>httpClient.Timeout = Timeout.InfiniteTimeSpan</c> on it if you use
    /// turns, bulk ingest, or streaming.</remarks>
    public static MunariumClient Rest(MunariumClientOptions options, HttpClient? httpClient = null) =>
        new(new RestTransport(options, httpClient));

    /// <summary>Direct gRPC transport (:50051, or :443 via the gateway
    /// plane). Plaintext exactly when the endpoint scheme is http://.</summary>
    public static MunariumClient Grpc(MunariumClientOptions options) =>
        new(new GrpcTransport(options));

    /// <summary>The head-conflict write loop (invariant #2): read head, build
    /// the claim via <paramref name="build"/>(head) with ExpectedHead set,
    /// propose with a FRESH idempotency key; on
    /// <see cref="HeadConflictException"/> back off (jittered) and rebuild
    /// against the actual head. Never retries other errors. An
    /// <c>Actual == 0</c> conflict (stripped details) re-reads the head.</summary>
    public async Task<ClaimOutcome> ProposeClaimWithRetryAsync(
        string versionId, Func<ulong, ClaimInput> build,
        WriteLoopOptions? options = null, CancellationToken ct = default)
    {
        options ??= new WriteLoopOptions();
        var head = await Query.HeadAsync(versionId, ct).ConfigureAwait(false);
        var attempt = 0;
        while (true)
        {
            attempt++;
            var claim = build(head) with { ExpectedHead = head };
            try
            {
                return await Commands.ProposeClaimAsync(versionId, claim, null, ct)
                    .ConfigureAwait(false);
            }
            catch (HeadConflictException e) when (attempt < options.MaxAttempts)
            {
                await Retry.DelayAsync(attempt, ct).ConfigureAwait(false);
                head = e.Actual > 0
                    ? e.Actual
                    : await Query.HeadAsync(versionId, ct).ConfigureAwait(false);
            }
        }
    }

    /// <summary>GET /version — the served name + workspace version (REST
    /// transport only; gRPC clients get the typed
    /// <see cref="UnsupportedTransportException"/>). Compare against
    /// <see cref="TargetServerVersion"/> to catch a stale deploy early.</summary>
    public Task<ServerVersionInfo> ServerVersionAsync(CancellationToken ct = default) =>
        _transport.ServerVersionAsync(ct);

    public ValueTask DisposeAsync() => _transport.DisposeAsync();
}

/// <summary>What both transports implement — the facade holds exactly one.</summary>
internal interface ITransport :
    ICommandsPlane, IQueryPlane, IIngestPlane, IRetrievalPlane, IRunbooksPlane,
    IProvidersPlane, ISessionsPlane, IAccessTokensPlane, IReportsPlane,
    IAuthoringPlane, IEvidencePlane, IMetaPlane, IAsyncDisposable;

internal static class Retry
{
    /// <summary>Jittered backoff for attempt n (1-based): base 2^(n-1)*100ms,
    /// ±50% decorrelated jitter, clamped to [50ms, 2s].</summary>
    internal static bool Retryable(RetryClass retryClass, MunariumException e) => retryClass switch
    {
        RetryClass.Read => e.Transient,
        // A transport failure that may have been delivered is NOT safe to
        // re-send as a command; a typed transient (503/overload) means the
        // server shed the request without executing it, so that one is.
        // Commands re-send ONLY when provably safe: a connect-phase
        // transport failure (never left) or the typed `overloaded` (the
        // server shed it BEFORE executing). A transient 502/504 from a
        // gateway means the command may still be executing upstream, so
        // re-sending could execute it twice (the C10 review's cross-client
        // finding; Rust always classified this correctly).
        RetryClass.Command => e is OverloadedException
            || e is MunariumTransportException { MayHaveReachedServer: false },
        _ => false,
    };

    internal static Task DelayAsync(int attempt, CancellationToken ct)
    {
        var baseMs = 100L * (1L << Math.Min(attempt - 1, 6));
        var ms = Math.Clamp(Random.Shared.NextInt64(baseMs / 2, baseMs * 3 / 2 + 1), 50, 2000);
        return Task.Delay(TimeSpan.FromMilliseconds(ms), ct);
    }

    /// <summary>The one retry engine both transports share (the client's core
    /// replay-safety invariant lives HERE). Reads retry transient failures.
    /// Commands retry only when the request provably never reached the
    /// server: the server records an idempotency key AFTER the command
    /// completes, so re-sending a possibly-delivered command could execute
    /// it twice. Non-replayable writes send once.
    /// The attempt callback receives the key (null outside command class) and
    /// must throw <see cref="MunariumException"/> for server/transport failures.</summary>
    internal static async Task<T> RunAsync<T>(
        RetryClass retryClass, int retries, string? idempotencyKey,
        Func<string?, Task<T>> attempt, CancellationToken ct)
    {
        var idem = retryClass == RetryClass.Command
            ? idempotencyKey ?? Guid.NewGuid().ToString()
            : null;
        var tries = 0;
        while (true)
        {
            tries++;
            try
            {
                return await attempt(idem).ConfigureAwait(false);
            }
            catch (MunariumException e) when (tries <= retries && Retryable(retryClass, e))
            {
                await DelayAsync(tries, ct).ConfigureAwait(false);
            }
        }
    }
}
