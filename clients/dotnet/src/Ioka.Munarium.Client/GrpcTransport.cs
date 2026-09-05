// SPDX-License-Identifier: Apache-2.0
// gRPC transport: Grpc.Net.Client over the direct :50051 plane (or :443 via
// the gateway). Errors decode the google.rpc.ErrorInfo structured detail;
// commands carry auto-generated idempotency keys and are re-sent with the
// SAME key ONLY on the typed `overloaded` (the server shed the request
// before executing it). On gRPC, commands are NEVER re-sent on a transport
// failure: unlike REST, where a connect-phase failure proves the request
// never left, no gRPC failure is provably undelivered — an UNAVAILABLE or
// deadline expiry on an established HTTP/2 stream cannot be distinguished
// from a call the server is still running, and the server records an
// idempotency key only AFTER a command completes (a re-send could execute
// it twice). Reads retry every transient failure. Plaintext exactly when
// the endpoint scheme is http://.
//
// Transport notes (documented parity gaps, not bugs):
// - BuildIndex has no gRPC RPC — UnsupportedTransportException.
// - The REST-only platform surface fails with UnsupportedTransportException
//   here — as a FAULTED task (or, for the SSE iterator, on the first
//   MoveNextAsync), so it surfaces at await/Task.WhenAll like every other
//   failure, never synchronously from the call:
//   TurnStreamAsync (SSE), the four bulk-upload routes, GetSourceAsync,
//   FindingsAsync, chronology rules, Providers.ListAsync and the max-tokens
//   budget pair (Get/ReplaceMaxTokensAsync), every reports method
//   (AdminService.Usage is declared but UNIMPLEMENTED — not wired), the
//   whole authoring plane, and ServerVersionAsync.
// - proto3 scalars cannot carry "explicitly zero": asOfSeq/limit/topK/
//   factLimit/budgetTokens/maxTokens/ttlSecs of 0, a counter budget of 0,
//   and a confidence/temperature of 0.0 are rejected as
//   InvalidInputException instead of silently meaning "absent" (REST
//   carries them faithfully).
// - RateLimitedException.RetryAfter is always null on gRPC: the server
//   emits no google.rpc.RetryInfo today (REST reads the Retry-After header).
// - An explicit EMPTY list for IngestFile.Collections or a minted token's
//   runbook refs is rejected as InvalidInputException: proto3 reads an
//   empty repeated field as absent (auto-bind / any runbook) where REST
//   reads [] as "none" (null — omitted — is fine on both).
// - Single-file IngestAsync surfaces a server per-item error as
//   UnexpectedServerException with the error text: the gRPC wire carries
//   per-item errors slug-less, so REST's typed kind is not recoverable.

using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using Grpc.Core;
using Grpc.Net.Client;
using Mmp.V1;

namespace Ioka.Munarium.Client;

internal sealed partial class GrpcTransport : ITransport
{
    private readonly GrpcChannel _channel;
    private readonly string? _token;
    private readonly string? _uid;
    private readonly TimeSpan _requestTimeout;
    private readonly int _retries;

    private readonly CommandService.CommandServiceClient _commands;
    private readonly QueryService.QueryServiceClient _queries;
    private readonly IngestService.IngestServiceClient _ingest;
    private readonly RetrievalService.RetrievalServiceClient _retrieval;
    private readonly RunbookService.RunbookServiceClient _runbooks;
    private readonly ProviderService.ProviderServiceClient _providers;
    private readonly SessionService.SessionServiceClient _sessions;
    private readonly AdminService.AdminServiceClient _admin;

    internal GrpcTransport(MunariumClientOptions options)
    {
        _channel = GrpcChannel.ForAddress(options.Endpoint);
        _token = options.Token;
        _uid = options.Uid;
        _requestTimeout = options.RequestTimeout;
        _retries = options.ReadRetries;
        _commands = new CommandService.CommandServiceClient(_channel);
        _queries = new QueryService.QueryServiceClient(_channel);
        _ingest = new IngestService.IngestServiceClient(_channel);
        _retrieval = new RetrievalService.RetrievalServiceClient(_channel);
        _runbooks = new RunbookService.RunbookServiceClient(_channel);
        _providers = new ProviderService.ProviderServiceClient(_channel);
        _sessions = new SessionService.SessionServiceClient(_channel);
        _admin = new AdminService.AdminServiceClient(_channel);
    }

    public ValueTask DisposeAsync()
    {
        _channel.Dispose();
        return ValueTask.CompletedTask;
    }

    private CallOptions Call(string? idem, CancellationToken ct, bool deadline = true)
    {
        var metadata = new Metadata();
        if (_token is not null) metadata.Add("authorization", $"Bearer {_token}");
        if (_uid is not null) metadata.Add("munarium-uid", _uid);
        if (idem is not null) metadata.Add("idempotency-key", idem);
        // A non-positive timeout is .NET's "disabled" spelling
        // (Timeout.InfiniteTimeSpan is -1ms): it must mean NO deadline, not
        // one already in the past — which would fail every call instantly.
        var hasDeadline = deadline && _requestTimeout > TimeSpan.Zero;
        return new CallOptions(
            metadata,
            hasDeadline ? DateTime.UtcNow + _requestTimeout : null,
            ct);
    }

    /// <summary>One retry policy for every unary RPC, derived from the
    /// request class. Reads retry transient failures; commands re-send only
    /// on the typed overloaded — never on a transport failure (the server
    /// records an idempotency key only AFTER the command completes, so a
    /// retry that overtakes an in-flight attempt executes it twice, and
    /// UNAVAILABLE on an established stream cannot be told apart from a call
    /// still running). Non-replayable writes send once. The default is the
    /// SAFE direction. <paramref name="deadline"/> false exempts the call
    /// from the per-request deadline (paid/large sends).</summary>
    private Task<T> RunAsync<T>(
        Func<CallOptions, AsyncUnaryCall<T>> call, RetryClass retryClass,
        string? idempotencyKey, CancellationToken ct, bool deadline = true) =>
        Retry.RunAsync(retryClass, _retries, idempotencyKey, async idem =>
        {
            try
            {
                // AsyncUnaryCall owns a deadline timer and a registration on
                // the caller's token; both leak without the dispose.
                using var rpc = call(Call(idem, ct, deadline));
                return await rpc.ConfigureAwait(false);
            }
            catch (RpcException e) when (IsCallerCancellation(e, ct))
            {
                // caller cancellation is not a server error
                throw new OperationCanceledException(ct);
            }
            catch (RpcException e)
            {
                throw Errors.FromRpc(e);
            }
        }, ct);

    private static bool IsCallerCancellation(RpcException e, CancellationToken ct) =>
        e.StatusCode == StatusCode.Cancelled && ct.IsCancellationRequested;

    private static void RejectZero(string name, ulong? value)
    {
        if (value == 0)
        {
            throw new InvalidInputException(
                $"{name} = 0 cannot be represented on the gRPC wire (proto3 uses 0 for " +
                "'absent'); omit it, or use the REST transport");
        }
    }

    private static void RejectZero(string name, double? value)
    {
        if (value == 0.0)
        {
            throw new InvalidInputException(
                $"{name} = 0.0 cannot be represented on the gRPC wire (proto3 uses 0.0 " +
                "for 'absent'); omit it, or use the REST transport");
        }
    }

    private static void RejectNegative(string name, int? value)
    {
        if (value is < 0)
        {
            throw new InvalidInputException($"{name} must be non-negative, got {value}");
        }
    }

    private static string? Opt(string s) => s.Length == 0 ? null : s;

    private static JsonElement? JsonOpt(string s)
    {
        if (s.Length == 0) return null;
        try
        {
            // JsonDocument rents from ArrayPool; Clone() detaches the value
            // so the rented buffer goes back when the document is disposed.
            using var doc = JsonDocument.Parse(s);
            return doc.RootElement.Clone();
        }
        catch (JsonException)
        {
            return null;
        }
    }

    // -- conversions --------------------------------------------------------

    private static readonly Dictionary<string, ClaimType> ClaimTypeToPb = new()
    {
        ["fact"] = ClaimType.Fact,
        ["update"] = ClaimType.Update,
        ["correction"] = ClaimType.Correction,
    };

    private static readonly Dictionary<string, Provenance> ProvenanceToPb = new()
    {
        ["witnessed"] = Provenance.Witnessed,
        ["backfilled"] = Provenance.Backfilled,
        ["repaired"] = Provenance.Repaired,
        ["emergent"] = Provenance.Emergent,
        ["coverage_repair"] = Provenance.CoverageRepair,
    };

    private static string ClaimTypeName(ClaimType t) => t switch
    {
        ClaimType.Update => "update",
        ClaimType.Correction => "correction",
        _ => "fact",
    };

    private static string ProvenanceName(Provenance p) => p switch
    {
        Provenance.Backfilled => "backfilled",
        Provenance.Repaired => "repaired",
        Provenance.Emergent => "emergent",
        Provenance.CoverageRepair => "coverage_repair",
        _ => "witnessed",
    };

    private static Claim ToClaim(Mmp.V1.Claim c) => new()
    {
        Id = c.Id,
        VersionId = c.VersionId,
        Seq = c.Seq,
        ClaimType = ClaimTypeName(c.ClaimType),
        Subject = c.Subject,
        Key = c.Key,
        Value = c.Value,
        NormalizedText = c.NormalizedText,
        ScopePath = Opt(c.ScopePath),
        // Fail CLOSED: only an explicit ACCEPTED reads as accepted, so
        // UNSPECIFIED (the proto3 default) and any status a newer server
        // adds can never satisfy invariant #1.
        Status = c.Status == ClaimStatus.Accepted ? "accepted" : "disputed",
        Provenance = ProvenanceName(c.Provenance),
        SupersedesId = Opt(c.SupersedesId),
        EntityId = Opt(c.EntityId),
        Evidence = JsonOpt(c.EvidenceJson),
        // proto3 sentinel: 0.0 = absent (explicit 0.0 is rejected on send).
        Confidence = c.Confidence != 0.0 ? c.Confidence : null,
        ShapeRef = Opt(c.ShapeRef),
        // A message field has presence: null IS absent, no sentinel needed.
        Origin = c.Origin is { } o ? ToOrigin(o) : null,
    };

    private static ClaimOrigin ToOrigin(Mmp.V1.ClaimOrigin o) => new()
    {
        Kind = o.Kind,
        SourceId = o.SourceId,
        MappingVersion = o.MappingVersion,
        RowKey = o.RowKey,
        EventPosition = Opt(o.EventPosition),
        ObservedAt = Opt(o.ObservedAt),
        EvidenceId = Opt(o.EvidenceId),
    };

    private static Mmp.V1.ClaimOrigin ToPbOrigin(ClaimOrigin o) => new()
    {
        Kind = o.Kind,
        SourceId = o.SourceId,
        MappingVersion = o.MappingVersion,
        RowKey = o.RowKey,
        EventPosition = o.EventPosition ?? "",
        ObservedAt = o.ObservedAt ?? "",
        EvidenceId = o.EvidenceId ?? "",
    };

    private static GateFinding ToFinding(Mmp.V1.GateFinding f) => new()
    {
        RuleId = f.RuleId,
        // Fail CLOSED like the claim status: an unknown severity is the
        // most severe one, never silently downgraded to "info".
        Severity = f.Severity switch
        {
            Severity.Info => "info",
            Severity.Warn => "warn",
            _ => "block",
        },
        Message = f.Message,
        ScopePath = Opt(f.ScopePath),
        Detail = JsonOpt(f.DetailJson),
    };

    private static Anchor ToAnchor(Mmp.V1.Anchor a) => new()
    {
        Id = a.Id,
        VersionId = a.VersionId,
        DetailKey = a.DetailKey,
        LockedValue = a.LockedValue,
        LockedAtScope = Opt(a.LockedAtScope),
        Status = a.Status,
        Seq = a.Seq,
    };

    private static Promise ToPromise(Mmp.V1.Promise p) => new()
    {
        Id = p.Id,
        VersionId = p.VersionId,
        Key = p.Key,
        Kind = p.Kind,
        Description = p.Description,
        OriginScope = Opt(p.OriginScope),
        DueScope = Opt(p.DueScope),
        Status = p.Status,
        Seq = p.Seq,
        FulfilledSeq = p.FulfilledSeq > 0 ? p.FulfilledSeq : null,
    };

    private static ProposeClaimRequest ToPb(string versionId, ClaimInput c)
    {
        RejectZero("confidence", c.Confidence);
        if (!ClaimTypeToPb.TryGetValue(c.ClaimType, out var claimType))
        {
            throw new InvalidInputException(
                $"unknown claim_type '{c.ClaimType}' (fact | update | correction)");
        }
        if (c.Provenance is { } provName && !ProvenanceToPb.ContainsKey(provName))
        {
            throw new InvalidInputException(
                $"unknown provenance '{provName}' (witnessed | backfilled | repaired | " +
                "emergent | coverage_repair)");
        }
        var msg = new ProposeClaimRequest
        {
            VersionId = versionId,
            ClaimType = claimType,
            Subject = c.Subject,
            Key = c.Key,
            Value = c.Value,
            ScopePath = c.ScopePath ?? "",
            Provenance = c.Provenance is { } p
                ? ProvenanceToPb.GetValueOrDefault(p, Provenance.Unspecified)
                : Provenance.Unspecified,
            SupersedesId = c.SupersedesId ?? "",
            EntityId = c.EntityId ?? "",
            EvidenceJson = c.Evidence?.GetRawText() ?? "",
            Confidence = c.Confidence ?? 0.0,
            ShapeRef = c.ShapeRef ?? "",
        };
        if (c.Origin is { } origin) msg.Origin = ToPbOrigin(origin);
        if (c.ExpectedHead is { } head) msg.ExpectedHead = head;
        return msg;
    }

    // -- commands -----------------------------------------------------------

    public async Task<string> CreateVersionAsync(
        string? parentVersionId = null, JsonElement? metadata = null,
        string? idempotencyKey = null, CancellationToken ct = default)
    {
        var msg = new CreateVersionRequest
        {
            ParentVersionId = parentVersionId ?? "",
            MetadataJson = metadata?.GetRawText() ?? "",
        };
        var resp = await RunAsync(
            o => _commands.CreateVersionAsync(msg, o), RetryClass.Command, idempotencyKey, ct)
            .ConfigureAwait(false);
        return resp.VersionId;
    }

    public async Task<ClaimOutcome> ProposeClaimAsync(
        string versionId, ClaimInput claim,
        string? idempotencyKey = null, CancellationToken ct = default)
    {
        var msg = ToPb(versionId, claim);
        var resp = await RunAsync(
            o => _commands.ProposeClaimAsync(msg, o), RetryClass.Command, idempotencyKey, ct)
            .ConfigureAwait(false);
        if (resp.Claim is null)
        {
            throw new UnexpectedServerException("ProposeClaimResponse without claim");
        }
        return new ClaimOutcome
        {
            Claim = ToClaim(resp.Claim),
            Findings = resp.Findings.Select(ToFinding).ToArray(),
            HeadSeq = resp.HeadSeq,
        };
    }

    public async Task<EventsOutcome> AppendEventsAsync(
        string versionId, IReadOnlyList<ClaimInput> claims,
        string? candidateText = null, ulong? expectedHead = null,
        string? idempotencyKey = null, CancellationToken ct = default)
    {
        var msg = new AppendEventsRequest
        {
            VersionId = versionId,
            CandidateText = candidateText ?? "",
        };
        msg.Claims.AddRange(claims.Select(c => ToPb(versionId, c)));
        if (expectedHead is { } head) msg.ExpectedHead = head;
        var resp = await RunAsync(
            o => _commands.AppendEventsAsync(msg, o), RetryClass.Command, idempotencyKey, ct)
            .ConfigureAwait(false);
        return new EventsOutcome
        {
            Claims = resp.Claims.Select(ToClaim).ToArray(),
            Findings = resp.Findings.Select(ToFinding).ToArray(),
            HeadSeq = resp.HeadSeq,
        };
    }

    public async Task<Promise> OpenPromiseAsync(
        string versionId, string key, string kind, string description,
        string? originScope = null, string? dueScope = null,
        string? idempotencyKey = null, CancellationToken ct = default)
    {
        var msg = new OpenPromiseRequest
        {
            VersionId = versionId,
            Key = key,
            Kind = kind,
            Description = description,
            OriginScope = originScope ?? "",
            DueScope = dueScope ?? "",
        };
        var resp = await RunAsync(
            o => _commands.OpenPromiseAsync(msg, o), RetryClass.Command, idempotencyKey, ct)
            .ConfigureAwait(false);
        return resp.Promise is null
            ? throw new UnexpectedServerException("OpenPromiseResponse without promise")
            : ToPromise(resp.Promise);
    }

    public async Task<bool> FulfillPromiseAsync(
        string versionId, string key,
        string? idempotencyKey = null, CancellationToken ct = default)
    {
        var msg = new FulfillPromiseRequest { VersionId = versionId, Key = key };
        var resp = await RunAsync(
            o => _commands.FulfillPromiseAsync(msg, o), RetryClass.Command, idempotencyKey, ct)
            .ConfigureAwait(false);
        return resp.Fulfilled;
    }

    public async Task<Anchor> LockAnchorAsync(
        string versionId, string subject, string key, string value,
        string? scopePath = null, JsonElement? evidence = null,
        string? idempotencyKey = null, CancellationToken ct = default)
    {
        var msg = new LockAnchorRequest
        {
            VersionId = versionId,
            Subject = subject,
            Key = key,
            Value = value,
            ScopePath = scopePath ?? "",
            EvidenceJson = evidence?.GetRawText() ?? "",
        };
        var resp = await RunAsync(
            o => _commands.LockAnchorAsync(msg, o), RetryClass.Command, idempotencyKey, ct)
            .ConfigureAwait(false);
        return resp.Anchor is null
            ? throw new UnexpectedServerException("LockAnchorResponse without anchor")
            : ToAnchor(resp.Anchor);
    }

    public Task RecordCountsAsync(
        string versionId, string key, string scopePath, ulong count,
        ulong? budget = null, string? idempotencyKey = null, CancellationToken ct = default)
    {
        RejectZero("budget", budget);
        var msg = new RecordCountsRequest
        {
            VersionId = versionId,
            Key = key,
            ScopePath = scopePath,
            Count = count,
            Budget = budget ?? 0,
        };
        return RunAsync(
            o => _commands.RecordCountsAsync(msg, o), RetryClass.Command, idempotencyKey, ct);
    }

    public Task UpsertDigestAsync(Digest digest, CancellationToken ct = default)
    {
        // gRPC UpsertDigest is a command RPC: idempotency key required
        // (unlike the REST PUT, which is exempt by design).
        var msg = new UpsertDigestRequest
        {
            Digest = new Mmp.V1.Digest
            {
                VersionId = digest.VersionId,
                Tier = digest.Tier,
                ScopePath = digest.ScopePath,
                Content = digest.Content,
                ContentHash = digest.ContentHash,
                BuiltFromSeq = digest.BuiltFromSeq,
            },
        };
        return RunAsync(
            o => _commands.UpsertDigestAsync(msg, o), RetryClass.Command, null, ct);
    }

    // -- query --------------------------------------------------------------

    public async Task<ulong> HeadAsync(string versionId, CancellationToken ct = default)
    {
        var resp = await RunAsync(
            o => _queries.GetHeadAsync(new GetHeadRequest { VersionId = versionId }, o),
            RetryClass.Read, null, ct).ConfigureAwait(false);
        return resp.HeadSeq;
    }

    public async Task<ClaimLookup> GetClaimAsync(string claimId, CancellationToken ct = default)
    {
        var resp = await RunAsync(
            o => _queries.GetClaimAsync(new GetClaimRequest { ClaimId = claimId }, o),
            RetryClass.Read, null, ct).ConfigureAwait(false);
        return resp.Claim is null
            ? throw new UnexpectedServerException("GetClaimResponse without claim")
            : new ClaimLookup
            {
                Claim = ToClaim(resp.Claim),
                Superseded = resp.Superseded,
                SupersededBy = Opt(resp.SupersededBy),
            };
    }

    public async Task<FactsPage> FactsAsync(
        string versionId, string? scopePrefix = null, ulong? asOfSeq = null,
        IReadOnlyList<string>? statuses = null, int? limit = null,
        CancellationToken ct = default)
    {
        RejectZero("as_of_seq", asOfSeq);
        RejectNegative("limit", limit);
        RejectZero("limit", (ulong?)limit);
        var msg = new SliceFactsRequest
        {
            VersionId = versionId,
            ScopePrefix = scopePrefix ?? "",
            AsOfSeq = asOfSeq ?? 0,
            Limit = (uint)(limit ?? 0),
        };
        foreach (var status in statuses ?? [])
        {
            msg.Statuses.Add(status switch
            {
                "accepted" => ClaimStatus.Accepted,
                "disputed" => ClaimStatus.Disputed,
                _ => throw new InvalidInputException(
                    $"unknown claim status '{status}' (accepted | disputed)"),
            });
        }
        var resp = await RunAsync(
            o => _queries.SliceFactsAsync(msg, o), RetryClass.Read, null, ct)
            .ConfigureAwait(false);
        return resp.Slice is null
            ? throw new UnexpectedServerException("SliceFactsResponse without slice")
            : new FactsPage
            {
                Facts = resp.Slice.Facts.Select(ToClaim).ToArray(),
                AsOfSeq = resp.Slice.AsOfSeq,
                HeadSeq = resp.Slice.HeadSeq,
            };
    }

    public async Task<IReadOnlyList<string>> LineageAsync(
        string versionId, CancellationToken ct = default)
    {
        var resp = await RunAsync(
            o => _queries.GetLineageAsync(new GetLineageRequest { VersionId = versionId }, o),
            RetryClass.Read, null, ct).ConfigureAwait(false);
        return resp.Lineage?.VersionIds.ToArray() ?? [];
    }

    public async Task<IReadOnlyList<Anchor>> AnchorsAsync(
        string versionId, ulong? asOfSeq = null, CancellationToken ct = default)
    {
        RejectZero("as_of_seq", asOfSeq);
        var msg = new ListAnchorsRequest { VersionId = versionId, AsOfSeq = asOfSeq ?? 0 };
        var resp = await RunAsync(
            o => _queries.ListAnchorsAsync(msg, o), RetryClass.Read, null, ct)
            .ConfigureAwait(false);
        return resp.Anchors.Select(ToAnchor).ToArray();
    }

    public async Task<IReadOnlyList<Promise>> PromisesAsync(
        string versionId, ulong? asOfSeq = null, string? status = null,
        CancellationToken ct = default)
    {
        Validation.CheckPromiseStatus(status);
        RejectZero("as_of_seq", asOfSeq);
        var msg = new ListPromisesRequest
        {
            VersionId = versionId,
            Status = status ?? "",
            AsOfSeq = asOfSeq ?? 0,
        };
        var resp = await RunAsync(
            o => _queries.ListPromisesAsync(msg, o), RetryClass.Read, null, ct)
            .ConfigureAwait(false);
        return resp.Promises.Select(ToPromise).ToArray();
    }

    public async Task<IReadOnlyList<Counter>> CountersAsync(
        string versionId, ulong? asOfSeq = null, CancellationToken ct = default)
    {
        RejectZero("as_of_seq", asOfSeq);
        var msg = new CounterTotalsRequest { VersionId = versionId, AsOfSeq = asOfSeq ?? 0 };
        var resp = await RunAsync(
            o => _queries.CounterTotalsAsync(msg, o), RetryClass.Read, null, ct)
            .ConfigureAwait(false);
        return resp.Counters
            .Select(c => new Counter
            {
                Key = c.Key,
                Total = c.Total,
                Budget = c.Budget > 0 ? c.Budget : null,
            })
            .ToArray();
    }

    public async Task<IReadOnlyList<Digest>> DigestsAsync(
        string versionId, CancellationToken ct = default)
    {
        var resp = await RunAsync(
            o => _queries.ListDigestsAsync(new ListDigestsRequest { VersionId = versionId }, o),
            RetryClass.Read, null, ct).ConfigureAwait(false);
        return resp.Digests
            .Select(d => new Digest
            {
                VersionId = d.VersionId,
                Tier = (byte)d.Tier,
                ScopePath = d.ScopePath,
                Content = d.Content,
                ContentHash = d.ContentHash,
                BuiltFromSeq = d.BuiltFromSeq,
            })
            .ToArray();
    }

    public async Task<ComposedContext> ComposeContextAsync(
        string versionId, string? scope = null, ulong? budgetTokens = null,
        int? factLimit = null, ulong? asOfSeq = null, CancellationToken ct = default)
    {
        RejectZero("as_of_seq", asOfSeq);
        RejectNegative("fact_limit", factLimit);
        RejectZero("fact_limit", (ulong?)factLimit);
        RejectZero("budget_tokens", budgetTokens);
        var msg = new ComposeContextRequest
        {
            VersionId = versionId,
            Scope = scope ?? "",
            BudgetTokens = budgetTokens ?? 0,
            FactLimit = (uint)(factLimit ?? 0),
            AsOfSeq = asOfSeq ?? 0,
        };
        var resp = await RunAsync(
            o => _queries.ComposeContextAsync(msg, o), RetryClass.Read, null, ct)
            .ConfigureAwait(false);
        var context = resp.Context
            ?? throw new UnexpectedServerException("ComposeContextResponse without context");
        return new ComposedContext
        {
            Sections = context.Sections
                .Select(s => new Section { Title = s.Title, Body = s.Body })
                .ToArray(),
            Text = context.Text,
            EstimatedTokens = context.EstimatedTokens,
            ContentHash = context.ContentHash,
            AsOfSeq = context.AsOfSeq,
        };
    }

    // -- ingest -------------------------------------------------------------

    public Task<PutSourceResult> PutSourceAsync(
        ChunkSource chunks, string declaredSha256 = "",
        string? mediaType = null, string? filename = null, string? shapeRef = null,
        CancellationToken ct = default) =>
        // Uploads are idempotent by content address, so transient failures
        // retry — the ChunkSource factory rebuilds the sequence per attempt.
        Retry.RunAsync(RetryClass.Read, _retries, null, _ =>
            PutSourceOnceAsync(chunks(), declaredSha256, mediaType, filename, shapeRef, ct), ct);

    private async Task<PutSourceResult> PutSourceOnceAsync(
        IAsyncEnumerable<ReadOnlyMemory<byte>> chunks, string declaredSha256,
        string? mediaType, string? filename, string? shapeRef, CancellationToken ct)
    {
        // Streaming uploads run without the per-request deadline.
        using var call = _ingest.PutSource(Call(null, ct, deadline: false));
        try
        {
            await call.RequestStream.WriteAsync(
                new PutSourceRequest
                {
                    Header = new SourceHeader
                    {
                        DeclaredSha256 = declaredSha256,
                        MediaType = mediaType ?? "",
                        Filename = filename ?? "",
                        ShapeRef = shapeRef ?? "",
                    },
                }, ct).ConfigureAwait(false);
            await foreach (var chunk in chunks.WithCancellation(ct).ConfigureAwait(false))
            {
                await call.RequestStream.WriteAsync(
                    new PutSourceRequest
                    {
                        // deliberate copy: UnsafeWrap would tie proto
                        // buffer lifetime to the caller's memory
                        Chunk = Google.Protobuf.ByteString.CopyFrom(chunk.Span),
                    }, ct).ConfigureAwait(false);
            }
            await call.RequestStream.CompleteAsync().ConfigureAwait(false);
            var resp = await call.ResponseAsync.ConfigureAwait(false);
            return new PutSourceResult
            {
                SourceId = resp.SourceId,
                ContentHash = resp.ContentHash,
                BytesLen = resp.BytesLen,
                AlreadyExisted = resp.AlreadyExisted,
            };
        }
        catch (RpcException e) when (IsCallerCancellation(e, ct))
        {
            throw new OperationCanceledException(ct); // same mapping as RunAsync
        }
        catch (RpcException e)
        {
            throw Errors.FromRpc(e);
        }
    }

    public async Task<RecordIngestResult> RecordIngestAsync(
        string versionId, string contentHash, string? shapeRef = null,
        CancellationToken ct = default)
    {
        var msg = new RecordIngestRequest
        {
            VersionId = versionId,
            ContentHash = contentHash,
            ShapeRef = shapeRef ?? "",
        };
        var resp = await RunAsync(
            o => _ingest.RecordIngestAsync(msg, o), RetryClass.Write, null, ct)
            .ConfigureAwait(false);
        return new RecordIngestResult { EventId = resp.EventId, Seq = resp.Seq };
    }

    // -- retrieval ----------------------------------------------------------

    public async Task<SearchResult> SearchAsync(
        string query, string shapeRef, uint? topK = null, string? indexVersion = null,
        JsonElement? filter = null, CancellationToken ct = default)
    {
        RejectZero("top_k", topK);
        var msg = new HybridSearchRequest
        {
            Query = query,
            ShapeRef = shapeRef,
            TopK = topK ?? 0,
            FilterJson = filter?.GetRawText() ?? "",
            IndexVersion = indexVersion ?? "",
        };
        var resp = await RunAsync(
            o => _retrieval.HybridSearchAsync(msg, o), RetryClass.Read, null, ct)
            .ConfigureAwait(false);
        var envelope = resp.Envelope
            ?? throw new UnexpectedServerException("HybridSearchResponse without ProvenanceEnvelope");
        return new SearchResult
        {
            Hits = resp.Hits
                .Select(h => new SearchHit
                {
                    ChunkId = h.ChunkId,
                    SourceId = h.SourceId,
                    SourcePath = h.SourcePath,
                    SourceContentHash = h.SourceContentHash,
                    Text = h.Text,
                    Score = h.Score,
                    // ranks are 1-based; the wire uses 0 for absent
                    LexicalRank = h.LexicalRank > 0 ? (uint)h.LexicalRank : null,
                    VectorRank = h.VectorRank > 0 ? (uint)h.VectorRank : null,
                    Metadata = JsonOpt(h.MetadataJson),
                })
                .ToArray(),
            Envelope = new ProvenanceEnvelope
            {
                ChunkIds = envelope.ChunkIds.ToArray(),
                SourceIds = envelope.SourceIds.ToArray(),
                SourcePaths = envelope.SourcePaths.ToArray(),
                SourceContentHashes = envelope.SourceContentHashes.ToArray(),
                IndexVersion = envelope.IndexVersion,
                EventWatermark = envelope.EventWatermark,
                ProviderFingerprint = Opt(envelope.ProviderFingerprint),
            },
        };
    }

    public async Task<IndexStatus> IndexStatusAsync(
        string shapeRef, CancellationToken ct = default)
    {
        var resp = await RunAsync(
            o => _retrieval.GetIndexVersionAsync(
                new GetIndexVersionRequest { ShapeRef = shapeRef }, o),
            RetryClass.Read, null, ct).ConfigureAwait(false);
        return new IndexStatus
        {
            IndexVersion = resp.IndexVersion,
            ShapeRef = shapeRef,
            EventWatermark = resp.EventWatermark,
            Active = resp.Active,
            Manifest = JsonOpt(resp.ManifestJson),
        };
    }

    public Task<IndexStatus> BuildIndexAsync(
        string shapeRef, string? versionId = null, CancellationToken ct = default) =>
        Task.FromException<IndexStatus>(new UnsupportedTransportException(
            "index builds have no gRPC RPC today — use the REST client " +
            "(POST /v1/indexes/{shape_ref}/build)"));

    // -- runbooks + shapes --------------------------------------------------

    public async Task<ApplyShapeResult> ApplyShapeAsync(
        string yaml, string? versionId = null, CancellationToken ct = default)
    {
        var msg = new ApplyShapeRequest { Yaml = yaml, VersionId = versionId ?? "" };
        var resp = await RunAsync(
            o => _runbooks.ApplyShapeAsync(msg, o), RetryClass.Write, null, ct)
            .ConfigureAwait(false);
        return new ApplyShapeResult
        {
            ShapeRef = resp.ShapeRef,
            // The wire doesn't carry the hash, but it is sha256(yaml bytes).
            YamlHash = Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(yaml))),
            EventId = Opt(resp.EventId),
        };
    }

    public async Task<string> ApplyRunbookAsync(string yaml, CancellationToken ct = default)
    {
        var resp = await RunAsync(
            o => _runbooks.ApplyRunbookAsync(new ApplyRunbookRequest { Yaml = yaml }, o),
            RetryClass.Write, null, ct).ConfigureAwait(false);
        return resp.RunbookRef;
    }

    public async Task<RunbookRun> RunRunbookAsync(
        string name, string? versionId = null, CancellationToken ct = default)
    {
        var msg = new RunRunbookRequest
        {
            RunbookRef = name,
            ParamsJson = versionId is null
                ? ""
                : JsonSerializer.Serialize(
                    new RunbookParams { VersionId = versionId },
                    MunariumJsonContext.Default.RunbookParams),
        };
        var resp = await RunAsync(
            o => _runbooks.RunRunbookAsync(msg, o), RetryClass.Write, null, ct)
            .ConfigureAwait(false);
        return new RunbookRun
        {
            RunId = resp.RunId,
            State = await ResolveRunStateAsync(resp.RunId, resp.State, ct).ConfigureAwait(false),
        };
    }

    public async Task<RunStatus> GetRunAsync(string runId, CancellationToken ct = default)
    {
        var resp = await RunAsync(
            o => _runbooks.GetRunAsync(new GetRunRequest { RunId = runId }, o),
            RetryClass.Read, null, ct).ConfigureAwait(false);
        return new RunStatus
        {
            RunId = resp.RunId,
            RunbookRef = resp.RunbookRef,
            State = resp.State,
            VersionId = Opt(resp.VersionId),
            Steps = resp.Steps
                .Select(s => new RunbookStep
                {
                    Ordinal = s.Ordinal,
                    Name = s.Name,
                    State = s.State,
                    Detail = JsonOpt(s.DetailJson),
                })
                .ToArray(),
        };
    }

    public async Task<RunbookRun> ApproveStepAsync(
        string runId, uint ordinal, CancellationToken ct = default)
    {
        var msg = new ApproveStepRequest { RunId = runId, StepOrdinal = ordinal };
        var resp = await RunAsync(
            o => _runbooks.ApproveStepAsync(msg, o), RetryClass.Write, null, ct)
            .ConfigureAwait(false);
        return new RunbookRun
        {
            RunId = runId,
            State = await ResolveRunStateAsync(runId, resp.State, ct).ConfigureAwait(false),
        };
    }

    /// <summary>State rides the response (additive proto field); against an
    /// older server that predates it, fall back to GetRun — but NEVER convert
    /// a committed write into an exception: a failed fallback read returns
    /// the empty state (unknown) for the caller to poll.</summary>
    private async Task<string> ResolveRunStateAsync(
        string runId, string wireState, CancellationToken ct)
    {
        if (wireState.Length > 0) return wireState;
        try
        {
            return (await GetRunAsync(runId, ct).ConfigureAwait(false)).State;
        }
        catch (MunariumException)
        {
            return "";
        }
    }

    // -- providers ----------------------------------------------------------

    public async Task<string> ApplyConfigAsync(string yaml, CancellationToken ct = default)
    {
        var resp = await RunAsync(
            o => _providers.ApplyProviderConfigAsync(
                new ApplyProviderConfigRequest { Yaml = yaml }, o),
            RetryClass.Write, null, ct).ConfigureAwait(false);
        return resp.ConfigName;
    }

    public async Task<ProviderHealth> HealthAsync(string name, CancellationToken ct = default)
    {
        var resp = await RunAsync(
            o => _providers.ProviderHealthAsync(
                new ProviderHealthRequest { ConfigName = name }, o),
            RetryClass.Read, null, ct).ConfigureAwait(false);
        return new ProviderHealth
        {
            Healthy = resp.Healthy,
            Provider = resp.Provider,
            EndpointFingerprint = resp.EndpointFingerprint,
            Detail = resp.Detail,
        };
    }

    public Task<HealthAiResult> HealthAiAsync(CancellationToken ct = default) =>
        Task.FromException<HealthAiResult>(new UnsupportedTransportException(
            "healthai has no gRPC RPC today — use the REST client (GET /healthai)"));

    public async Task<CompleteResult> CompleteAsync(
        string name, string prompt, string? model = null, string? system = null,
        uint? maxTokens = null, double? temperature = null, string? versionId = null,
        string? provider = null, string? tier = null,
        CancellationToken ct = default)
    {
        RejectZero("temperature", temperature);
        RejectZero("max_tokens", maxTokens);
        var msg = new CompleteRequest
        {
            ConfigName = name,
            Model = model ?? "",
            Provider = provider ?? "",
            Tier = tier ?? "",
            System = system ?? "",
            Prompt = prompt,
            MaxTokens = maxTokens ?? 0,
            Temperature = temperature ?? 0.0,
            VersionId = versionId ?? "",
        };
        var resp = await RunAsync(
            o => _providers.CompleteAsync(msg, o), RetryClass.Write, null, ct)
            .ConfigureAwait(false);
        return new CompleteResult
        {
            Text = resp.Text,
            StopReason = resp.StopReason,
            InputTokens = resp.InputTokens,
            OutputTokens = resp.OutputTokens,
            Provider = resp.Provider,
            Model = resp.Model,
            InvocationEventId = Opt(resp.InvocationEventId),
        };
    }

    public async Task<EmbedResult> EmbedAsync(
        string name, IReadOnlyList<string> inputs, string? model = null,
        string? versionId = null, string? provider = null, CancellationToken ct = default)
    {
        var msg = new EmbedRequest
        {
            ConfigName = name,
            Model = model ?? "",
            Provider = provider ?? "",
            VersionId = versionId ?? "",
        };
        msg.Inputs.AddRange(inputs);
        var resp = await RunAsync(
            o => _providers.EmbedAsync(msg, o), RetryClass.Write, null, ct)
            .ConfigureAwait(false);
        return new EmbedResult
        {
            Vectors = resp.Vectors.Select(v => (IReadOnlyList<float>)v.Values).ToArray(),
            Dimensions = resp.Dimensions,
            CacheHit = resp.CacheHit,
            Provider = resp.Provider,
            Model = resp.Model,
            InvocationEventId = Opt(resp.InvocationEventId),
        };
    }
}
