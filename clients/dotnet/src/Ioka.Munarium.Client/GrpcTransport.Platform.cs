// SPDX-License-Identifier: Apache-2.0
// The platform gRPC surface: SessionService, AdminService's served
// access-token trio, the RetrievalService collections trio, RunbookService
// management v2, and IngestFiles (the file-plane twin, with the per-item
// contract held ACROSS transports). Everything REST-only throws the typed
// UnsupportedTransportException — see the header notes in GrpcTransport.cs.

using System.Text.Json;
using Google.Protobuf;
using Grpc.Core;
using Mmp.V1;

namespace Ioka.Munarium.Client;

internal sealed partial class GrpcTransport
{
    // -- query: findings (REST-only) ----------------------------------------

    public Task<IReadOnlyList<StoredFinding>> FindingsAsync(
        string versionId, ulong? asOfSeq = null, string? severity = null,
        string? ruleId = null, int? limit = null, CancellationToken ct = default) =>
        Task.FromException<IReadOnlyList<StoredFinding>>(new UnsupportedTransportException(
            "findings have no gRPC RPC today — use the REST client " +
            "(GET /v1/versions/{id}/findings)"));

    // -- ingest: file plane (IngestFiles) + bulk (REST-only) ----------------

    /// <summary>Single-file ingest holds the REST twin's contract: on
    /// POST /v1/ingest the server answers a typed 400 for undecodable
    /// base64 (the client throws invalid-input), so the local decode
    /// failure throws the same here instead of returning a "successful"
    /// error result. A server-side per-item error on the one result is
    /// surfaced as a thrown UnexpectedServerException carrying the error
    /// text — documented parity gap: the gRPC wire carries per-item errors
    /// as free text with no registry slug, so the typed kind REST would
    /// give (e.g. forbidden for a collection the token cannot write) is not
    /// recoverable. Batch keeps per-item outcomes on both transports.</summary>
    public async Task<IngestResult> IngestAsync(
        IngestFile file, CancellationToken ct = default)
    {
        if (SingleFileLocalError(file) is { } invalid) throw invalid;
        var results = await IngestFilesAsync([file], ct).ConfigureAwait(false);
        var result = results[0]; // SpliceIngestResults guarantees exactly one
        return result.Error is { Length: > 0 } error
            ? throw new UnexpectedServerException($"ingest '{file.Filename}' failed: {error}")
            : result;
    }

    /// <summary>The single-file decode path, testable offline: null when
    /// the file would ship, else the thrown invalid-input.</summary>
    internal static InvalidInputException? SingleFileLocalError(IngestFile file)
    {
        var (sent, localError) = ToPbIngestFile(file);
        return sent is null ? new InvalidInputException(localError!.Error!) : null;
    }

    public Task<IReadOnlyList<IngestResult>> IngestBatchAsync(
        IReadOnlyList<IngestFile> files, CancellationToken ct = default)
    {
        Validation.CheckBulkFiles("batch", files.Count);
        return IngestFilesAsync(files, ct);
    }

    /// <summary>The per-item contract holds ACROSS transports: a file whose
    /// base64 cannot decode becomes its own error result (never sent), the
    /// valid remainder ships, and results splice back in input order —
    /// exactly the outcome the REST plane's server-side per-item handling
    /// produces.</summary>
    private async Task<IReadOnlyList<IngestResult>> IngestFilesAsync(
        IReadOnlyList<IngestFile> files, CancellationToken ct)
    {
        var localErrors = new IngestResult?[files.Count];
        var msg = new IngestFilesRequest();
        for (var i = 0; i < files.Count; i++)
        {
            var (sent, error) = ToPbIngestFile(files[i]);
            localErrors[i] = error;
            if (sent is not null) msg.Files.Add(sent);
        }
        IReadOnlyList<IngestResult> serverResults = [];
        if (msg.Files.Count > 0)
        {
            // Content-addressed and per-item idempotent, but a batch can
            // partially apply — send once, like the REST file plane, and
            // DEADLINE-EXEMPT like its REST twin (/v1/ingest, /batch): the
            // body runs to the server's 256 MiB ceiling and a client-side
            // abort does not stop the server's work.
            var resp = await RunAsync(
                o => _ingest.IngestFilesAsync(msg, o), RetryClass.Write, null, ct,
                deadline: false)
                .ConfigureAwait(false);
            serverResults = resp.Results.Select(ToIngestResult).ToArray();
        }
        return SpliceIngestResults(files, localErrors, serverResults);
    }

    /// <summary>Decode one file-plane entry to the wire shape. The REST
    /// plane carries content as base64 INSIDE the JSON body; the gRPC
    /// message carries raw bytes — so the client decodes here. A bad file
    /// yields an error RESULT, not a batch failure: the plane's contract is
    /// per-item outcomes.</summary>
    internal static (Mmp.V1.IngestFile? Sent, IngestResult? LocalError) ToPbIngestFile(
        IngestFile file)
    {
        // proto3 empty-repeated sentinel: REST `collections: []` binds to
        // NOTHING, while an empty repeated field reads as absent (= matcher
        // auto-bind) on the wire — a client-side input error, not a per-item
        // server outcome.
        Validation.RejectEmptyList("collections", file.Collections);
        ByteString content;
        try
        {
            // One decode straight into the proto buffer (no intermediate
            // byte[] + copy). FromBase64 rides Convert.FromBase64String,
            // which skips ASCII whitespace — so "aGVsbG8=\n" decodes here
            // exactly as the REST server (which trims) accepts it; pinned by
            // a unit test.
            content = ByteString.FromBase64(file.ContentBase64);
        }
        catch (FormatException e)
        {
            return (null, new IngestResult
            {
                Filename = file.Filename,
                Existed = false,
                BoundTo = [],
                Error = $"content_base64 is not valid base64: {e.Message}",
            });
        }
        var pb = new Mmp.V1.IngestFile
        {
            Filename = file.Filename,
            MediaType = file.MediaType,
            Content = content,
            Sha256 = file.Sha256 ?? "",
        };
        pb.Collections.AddRange(file.Collections ?? []);
        return (pb, null);
    }

    /// <summary>Splice server results back into input order around the
    /// locally-failed slots. A short server results array is a typed
    /// error naming the starved file, never an index panic; a SURPLUS is a
    /// typed error too — silently dropping extra results would hide a
    /// mis-pairing (the server answered for files this client did not
    /// send, so every pairing is suspect).</summary>
    internal static IReadOnlyList<IngestResult> SpliceIngestResults(
        IReadOnlyList<IngestFile> files, IReadOnlyList<IngestResult?> localErrors,
        IReadOnlyList<IngestResult> serverResults)
    {
        var results = new List<IngestResult>(files.Count);
        var next = 0;
        for (var i = 0; i < files.Count; i++)
        {
            if (localErrors[i] is { } local)
            {
                results.Add(local);
                continue;
            }
            results.Add(next < serverResults.Count
                ? serverResults[next++]
                : throw new UnexpectedServerException(
                    $"IngestFilesResponse carried no result for '{files[i].Filename}'"));
        }
        if (next < serverResults.Count)
        {
            throw new UnexpectedServerException(
                $"IngestFilesResponse carried {serverResults.Count} results for " +
                $"{next} files sent — results cannot be paired with inputs");
        }
        return results;
    }

    private static IngestResult ToIngestResult(Mmp.V1.IngestResult r) => new()
    {
        Filename = r.Filename,
        SourceId = Opt(r.SourceId),
        Sha256 = Opt(r.Sha256),
        Existed = r.Existed,
        BoundTo = r.BoundTo.ToArray(),
        Error = Opt(r.Error),
    };

    private static UnsupportedTransportException BulkUnsupported() => new(
        "bulk upload sessions have no gRPC RPCs today — use the REST client " +
        "(POST /v1/ingest/bulk …), or stream single sources via PutSource");

    public Task<BulkOpenResult> BulkOpenAsync(
        IReadOnlyList<BulkManifestEntry> files, string? label = null,
        CancellationToken ct = default) => Task.FromException<BulkOpenResult>(BulkUnsupported());

    public Task<BulkChunkResult> BulkChunkAsync(
        string bulkId, IReadOnlyList<IngestFile> files, CancellationToken ct = default) =>
        Task.FromException<BulkChunkResult>(BulkUnsupported());

    public Task<BulkStatusResult> BulkStatusAsync(
        string bulkId, bool includeNeeded = false, CancellationToken ct = default) =>
        Task.FromException<BulkStatusResult>(BulkUnsupported());

    public Task<BulkCompleteResult> BulkCompleteAsync(
        string bulkId, CancellationToken ct = default) => Task.FromException<BulkCompleteResult>(BulkUnsupported());

    public Task<SourceInfo> GetSourceAsync(string sourceId, CancellationToken ct = default) =>
        Task.FromException<SourceInfo>(new UnsupportedTransportException(
            "source metadata has no gRPC RPC today — use the REST client " +
            "(GET /v1/sources/{source_id})"));

    // -- retrieval: collections trio ----------------------------------------

    public async Task<Collection> CreateCollectionAsync(
        string name, string shapeRef, int accessLevel = 0,
        IReadOnlyList<string>? compartments = null, string? description = null,
        CancellationToken ct = default)
    {
        var msg = new CreateCollectionRequest
        {
            Name = name,
            ShapeRef = shapeRef,
            AccessLevel = accessLevel,
            Description = description ?? "",
        };
        msg.Compartments.AddRange(compartments ?? []);
        // Create-or-update — but not replay-keyed: send once.
        var resp = await RunAsync(
            o => _retrieval.CreateCollectionAsync(msg, o), RetryClass.Write, null, ct)
            .ConfigureAwait(false);
        return ToCollection(resp);
    }

    public async Task<IReadOnlyList<Collection>> ListCollectionsAsync(
        CancellationToken ct = default)
    {
        var resp = await RunAsync(
            o => _retrieval.ListCollectionsAsync(new ListCollectionsRequest(), o),
            RetryClass.Read, null, ct).ConfigureAwait(false);
        return resp.Collections.Select(ToCollection).ToArray();
    }

    public async Task<Collection> GetCollectionAsync(string id, CancellationToken ct = default)
    {
        var resp = await RunAsync(
            o => _retrieval.GetCollectionAsync(new GetCollectionRequest { Id = id }, o),
            RetryClass.Read, null, ct).ConfigureAwait(false);
        return ToCollection(resp);
    }

    private static Collection ToCollection(CollectionInfo c) => new()
    {
        Id = c.Id,
        Name = c.Name,
        ShapeRef = c.ShapeRef,
        AccessLevel = c.AccessLevel,
        Compartments = c.Compartments.ToArray(),
        Status = c.Status,
        Description = Opt(c.Description),
        CreatedAt = c.CreatedAt,
        SourceCount = c.SourceCount,
        ActiveIndex = Opt(c.ActiveIndex),
    };

    // -- runbooks: management v2 + chronology (REST-only) -------------------

    public async Task<IReadOnlyList<RunbookSummary>> ListAsync(
        bool includeRemoved = false, CancellationToken ct = default)
    {
        var msg = new ListRunbooksRequest { IncludeRemoved = includeRemoved };
        var resp = await RunAsync(
            o => _runbooks.ListRunbooksAsync(msg, o), RetryClass.Read, null, ct)
            .ConfigureAwait(false);
        return resp.Runbooks.Select(ToRunbookSummary).ToArray();
    }

    public async Task<RunbookInfo> GetInfoAsync(string name, CancellationToken ct = default)
    {
        var resp = await RunAsync(
            o => _runbooks.GetRunbookInfoAsync(new GetRunbookInfoRequest { Name = name }, o),
            RetryClass.Read, null, ct).ConfigureAwait(false);
        return new RunbookInfo
        {
            RunbookRef = resp.RunbookRef,
            Name = resp.Name,
            Version = resp.Version,
            Status = resp.Status,
            Collections = resp.Collections.Select(ToRunbookCollection).ToArray(),
            Versions = resp.Versions.ToArray(),
            Models = JsonOpt(resp.ModelsJson),
            Retrieval = JsonOpt(resp.RetrievalJson),
            HasCompletion = resp.HasCompletion,
            CreatedAt = resp.CreatedAt,
        };
    }

    public async Task<RunbookValidation> ValidateAsync(
        string yaml, bool suggest = false, string? provider = null,
        string? model = null, string? tier = null, CancellationToken ct = default)
    {
        var msg = new ValidateRunbookRequest
        {
            Yaml = yaml,
            Suggest = suggest,
            Provider = provider ?? "",
            Model = model ?? "",
            Tier = tier ?? "",
        };
        // With suggest=true this spends provider tokens — send once.
        var resp = await RunAsync(
            o => _runbooks.ValidateRunbookAsync(msg, o), RetryClass.Write, null, ct)
            .ConfigureAwait(false);
        return new RunbookValidation
        {
            Valid = resp.Valid,
            Findings = resp.Findings
                .Select(f => new ValidationFinding
                {
                    Severity = f.Severity, Code = f.Code, Message = f.Message, Path = f.Path,
                })
                .ToArray(),
            Suggestions = resp.Suggestions
                .Select(s => new Suggestion
                {
                    Title = s.Title, Rationale = s.Rationale, PatchHint = Opt(s.PatchHint),
                })
                .ToArray(),
            SuggestNote = Opt(resp.SuggestNote),
        };
    }

    public async Task<RemovalRequest> RemoveRequestAsync(
        string name, CancellationToken ct = default)
    {
        var msg = new RequestRemovalRequest { RunbookRef = name };
        var resp = await RunAsync(
            o => _runbooks.RequestRemovalAsync(msg, o), RetryClass.Write, null, ct)
            .ConfigureAwait(false);
        return new RemovalRequest
        {
            RunbookRef = resp.RunbookRef,
            RemovalId = resp.RemovalId,
            ExpiresAt = resp.ExpiresAt,
        };
    }

    public async Task<RemovalConfirmation> RemoveConfirmAsync(
        string name, string removalId, CancellationToken ct = default)
    {
        var msg = new ConfirmRemovalRequest { RunbookRef = name, RemovalId = removalId };
        var resp = await RunAsync(
            o => _runbooks.ConfirmRemovalAsync(msg, o), RetryClass.Write, null, ct)
            .ConfigureAwait(false);
        return new RemovalConfirmation { RunbookRef = resp.RunbookRef, Status = resp.Status };
    }

    private static RunbookCollection ToRunbookCollection(RunbookCollectionInfo c) => new()
    {
        Name = c.Name,
        CollectionId = Opt(c.CollectionId),
        ShapeRef = c.ShapeRef,
        AccessLevel = c.AccessLevel,
        Compartments = c.Compartments.ToArray(),
        ActiveIndex = Opt(c.ActiveIndex),
        SourceCount = c.SourceCount,
    };

    private static RunbookSummary ToRunbookSummary(Mmp.V1.RunbookSummary r) => new()
    {
        RunbookRef = r.RunbookRef,
        Name = r.Name,
        Version = r.Version,
        Status = r.Status,
        MinAccessLevel = r.MinAccessLevel,
        Collections = r.Collections.Select(ToRunbookCollection).ToArray(),
        CreatedAt = r.CreatedAt,
    };

    public Task<ChronologyRulesApplied> ApplyChronologyRulesAsync(
        string yaml, CancellationToken ct = default) =>
        Task.FromException<ChronologyRulesApplied>(new UnsupportedTransportException(
            "chronology rules have no gRPC RPC today — use the REST client " +
            "(POST /v1/chronology-rules)"));

    public Task<string> GetChronologyRulesAsync(string name, CancellationToken ct = default) =>
        Task.FromException<string>(new UnsupportedTransportException(
            "chronology rules have no gRPC RPC today — use the REST client " +
            "(GET /v1/chronology-rules/{name})"));

    // -- providers: disclosure (REST-only) ----------------------------------

    public Task<IReadOnlyList<ProviderModels>> ListAsync(CancellationToken ct = default) =>
        Task.FromException<IReadOnlyList<ProviderModels>>(new UnsupportedTransportException(
            "provider disclosure has no gRPC RPC today — use the REST client " +
            "(GET /v1/providers)"));

    // -- providers: max_tokens budgets (REST-only) --------------------------

    private static UnsupportedTransportException MaxTokensUnsupported() => new(
        "the max_tokens budgets have no gRPC RPC today — use the REST client " +
        "(GET/POST /v1/max-tokens)");

    public Task<MaxTokensResponse> GetMaxTokensAsync(CancellationToken ct = default) =>
        Task.FromException<MaxTokensResponse>(MaxTokensUnsupported());

    public Task<MaxTokensResponse> ReplaceMaxTokensAsync(
        MaxTokensBudgets budgets, CancellationToken ct = default) =>
        Task.FromException<MaxTokensResponse>(MaxTokensUnsupported());

    // -- sessions -----------------------------------------------------

    public async Task<SessionCreated> CreateAsync(
        string runbookName, CancellationToken ct = default)
    {
        var msg = new CreateSessionRequest { RunbookName = runbookName };
        // Opens server-side state — send once.
        var resp = await RunAsync(
            o => _sessions.CreateSessionAsync(msg, o), RetryClass.Write, null, ct)
            .ConfigureAwait(false);
        return new SessionCreated
        {
            SessionId = resp.SessionId,
            RunbookRef = resp.RunbookRef,
            PermittedCollections = resp.PermittedCollections.ToArray(),
        };
    }

    public async Task<TurnResult> TurnAsync(
        string sessionId, TurnRequest request, CancellationToken ct = default)
    {
        RejectZero("top_k", (ulong?)request.TopK);
        var msg = new Mmp.V1.TurnRequest
        {
            SessionId = sessionId,
            Query = request.Query,
            TopK = request.TopK ?? 0,
            Complete = request.Complete ?? false,
            // proto3 has no optional string here: "" IS absent, which is
            // exactly the legacy document path the REST twin gets by omitting
            // the key.
            ResearchProfile = request.ResearchProfile ?? "",
        };
        if (request.ModelOverride is { } o)
        {
            msg.ModelOverride = new SessionModelOverride
            {
                Provider = o.Provider ?? "",
                Model = o.Model ?? "",
                Tier = o.Tier ?? "",
            };
        }
        // A turn spends provider tokens — send once, never auto-retried, and
        // DEADLINE-EXEMPT like the REST twin: aborting client-side does not
        // stop the server's paid completion.
        var resp = await RunAsync(
            o => _sessions.TurnAsync(msg, o), RetryClass.Write, null, ct, deadline: false)
            .ConfigureAwait(false);
        return ToTurnResult(resp);
    }

    public IAsyncEnumerable<TurnStreamEvent> TurnStreamAsync(
        string sessionId, TurnRequest request, CancellationToken ct = default) =>
        // Surfaces on the first MoveNextAsync, like every other failure —
        // never synchronously from the call that builds the enumerable.
        Faulted<TurnStreamEvent>(new UnsupportedTransportException(
            "streaming turns have no gRPC RPC today — use the REST client " +
            "(POST /v1/sessions/{id}/turns/stream), or the unary turn here"));

    private static async IAsyncEnumerable<T> Faulted<T>(Exception e)
    {
        yield return await Task.FromException<T>(e).ConfigureAwait(false);
    }

    public async Task<Session> GetAsync(string sessionId, CancellationToken ct = default)
    {
        var resp = await RunAsync(
            o => _sessions.GetSessionAsync(new GetSessionRequest { SessionId = sessionId }, o),
            RetryClass.Read, null, ct).ConfigureAwait(false);
        return ToSession(resp);
    }

    public async Task<Session> CloseAsync(string sessionId, CancellationToken ct = default)
    {
        // Idempotent by construction server-side, but still a write — sent
        // once, matching the REST transport.
        var resp = await RunAsync(
            o => _sessions.CloseSessionAsync(new CloseSessionRequest { SessionId = sessionId }, o),
            RetryClass.Write, null, ct).ConfigureAwait(false);
        return ToSession(resp);
    }

    private static ProvenanceEnvelope ToEnvelope(Mmp.V1.ProvenanceEnvelope e) => new()
    {
        ChunkIds = e.ChunkIds.ToArray(),
        SourceIds = e.SourceIds.ToArray(),
        SourcePaths = e.SourcePaths.ToArray(),
        SourceContentHashes = e.SourceContentHashes.ToArray(),
        IndexVersion = e.IndexVersion,
        EventWatermark = e.EventWatermark,
        ProviderFingerprint = Opt(e.ProviderFingerprint),
    };

    private static TurnResult ToTurnResult(Mmp.V1.TurnResponse resp) => new()
    {
        SessionId = resp.SessionId,
        Ordinal = resp.Ordinal,
        CollectionsSearched = resp.CollectionsSearched.ToArray(),
        Skipped = resp.Skipped.ToArray(),
        Hits = resp.Hits
            .Select(h => new TurnHit
            {
                Collection = h.Collection,
                ChunkId = h.ChunkId,
                SourceId = h.SourceId,
                SourcePath = h.SourcePath,
                SourceContentHash = h.SourceContentHash,
                Text = h.Text,
                Score = h.Score,
            })
            .ToArray(),
        Envelopes = resp.Envelopes
            .Select(e => new CollectionEnvelope
            {
                Collection = e.Collection,
                Envelope = e.Envelope is { } env
                    ? ToEnvelope(env)
                    : throw new UnexpectedServerException(
                        $"CollectionEnvelope for '{e.Collection}' without ProvenanceEnvelope"),
            })
            .ToArray(),
        Completion = resp.Completion is { } c
            ? new TurnCompletion
            {
                Provider = c.Provider,
                Model = c.Model,
                WasOverride = c.WasOverride,
                Text = c.Text,
                InputTokens = c.InputTokens,
                OutputTokens = c.OutputTokens,
                Verification = c.Verification is { } v
                    ? new TurnVerification
                    {
                        Checks = v.Checks.ToArray(),
                        Retries = v.Retries,
                        FirstPassViolations = v.FirstPassViolations.ToArray(),
                        Violations = v.Violations.ToArray(),
                    }
                    : null,
            }
            : null,
        Hierarchy = resp.Hierarchy is { } h
            ? new EvidenceHierarchyDecision
            {
                Profile = h.Profile,
                IntentKind = Opt(h.IntentKind),
                IntentExplicit = h.IntentExplicit,
                Layers = h.Layers
                    .Select(l => new LayerOutcome
                    {
                        Layer = l.Layer,
                        Role = l.Role,
                        Requirement = l.Requirement,
                        Block = l.Block,
                        EvidenceId = Opt(l.EvidenceId),
                        SupportsCompleteness = l.SupportsCompleteness,
                        RefusalCode = Opt(l.RefusalCode),
                        ElapsedMs = l.ElapsedMs,
                    })
                    .ToArray(),
                CompletenessAvailable = h.CompletenessAvailable,
                DisclosedConflicts = h.DisclosedConflicts,
                ConflictsPolicy = h.ConflictsPolicy,
            }
            : null,
    };

    private static Session ToSession(GetSessionResponse resp) => new()
    {
        SessionId = resp.SessionId,
        Uid = resp.Uid,
        RunbookRef = resp.RunbookRef,
        AccessLevel = resp.AccessLevel,
        Compartments = resp.Compartments.ToArray(),
        State = resp.State,
        CreatedAt = resp.CreatedAt,
        Turns = resp.Turns
            .Select(t => new SessionTurn
            {
                Ordinal = t.Ordinal,
                Query = t.Query,
                CollectionsSearched = t.CollectionsSearched.ToArray(),
                // Stored transcript rows ride as JSON strings on the wire —
                // parse-or-null keeps a mangled row visible instead of
                // failing the whole session read.
                Hits = JsonOpt(t.HitsJson),
                Envelope = JsonOpt(t.EnvelopeJson),
                Completion = JsonOpt(t.CompletionJson),
                CreatedAt = t.CreatedAt,
            })
            .ToArray(),
    };

    // -- access tokens (AdminService's served trio) -------------------------

    public async Task<IssuedToken> MintAsync(
        string uid, int accessLevel, IReadOnlyList<string> scopes,
        IReadOnlyList<string>? compartments = null,
        IReadOnlyList<string>? runbookRefs = null, ulong? ttlSecs = null,
        CancellationToken ct = default)
    {
        // proto3 zero-sentinel: ttl_secs = 0 means "server default" on the
        // wire, so an explicit zero is rejected like the other zero traps.
        RejectZero("ttl_secs", ttlSecs);
        // proto3 empty-repeated sentinel: REST `runbook_refs: []` means NO
        // runbooks, while an empty repeated field reads as absent (= any
        // runbook) on the wire.
        Validation.RejectEmptyList("runbook_refs", runbookRefs);
        var msg = new IssueAccessTokenRequest
        {
            Uid = uid,
            AccessLevel = accessLevel,
            TtlSecs = ttlSecs ?? 0,
        };
        msg.Compartments.AddRange(compartments ?? []);
        msg.Scopes.AddRange(scopes);
        msg.RunbookRefs.AddRange(runbookRefs ?? []);
        // Minting twice issues two live tokens — send once.
        var resp = await RunAsync(
            o => _admin.IssueAccessTokenAsync(msg, o), RetryClass.Write, null, ct)
            .ConfigureAwait(false);
        return new IssuedToken { Token = resp.Token, Jti = resp.Jti, ExpiresAt = resp.ExpiresAt };
    }

    public async Task<IReadOnlyList<TokenInfo>> ListAsync(
        string? uid = null, bool? active = null, CancellationToken ct = default)
    {
        var msg = new ListAccessTokensRequest
        {
            Uid = uid ?? "",
            // proto3 bool: false = "all" — identical to the REST default, so
            // active: false and null land on the same wire value by design.
            Active = active ?? false,
        };
        var resp = await RunAsync(
            o => _admin.ListAccessTokensAsync(msg, o), RetryClass.Read, null, ct)
            .ConfigureAwait(false);
        return resp.Tokens
            .Select(t => new TokenInfo
            {
                Jti = t.Jti,
                Uid = t.Uid,
                AccessLevel = t.AccessLevel,
                Compartments = t.Compartments.ToArray(),
                Scopes = t.Scopes.ToArray(),
                RunbookRefs = t.RunbookRefs.Count > 0 ? t.RunbookRefs.ToArray() : null,
                IssuedBy = t.IssuedBy,
                IssuedAt = t.IssuedAt,
                ExpiresAt = t.ExpiresAt,
                RevokedAt = Opt(t.RevokedAt),
            })
            .ToArray();
    }

    public async Task<TokenRevocation> RevokeAsync(string jti, CancellationToken ct = default)
    {
        var resp = await RunAsync(
            o => _admin.RevokeAccessTokenAsync(new RevokeAccessTokenRequest { Jti = jti }, o),
            RetryClass.Write, null, ct).ConfigureAwait(false);
        return new TokenRevocation
        {
            Jti = resp.Jti,
            Revoked = resp.Revoked,
            RevocationCheckEnabled = resp.RevocationCheckEnabled,
        };
    }

    // -- reports / authoring / meta: REST-only, honestly typed --------------

    private static UnsupportedTransportException ReportsUnsupported() => new(
        "reports have no gRPC RPCs today (AdminService.Usage is declared but " +
        "UNIMPLEMENTED) — use the REST client (GET /v1/reports/…)");

    public Task<UsageReport> UsageAsync(
        string? groupBy = null, string? from = null, string? to = null,
        CancellationToken ct = default) => Task.FromException<UsageReport>(ReportsUnsupported());

    public Task<AuditReport> AuditAsync(
        string? uid = null, string? sessionId = null, string? runbook = null,
        string? from = null, string? to = null, int? limit = null,
        bool bodies = false, string? before = null, CancellationToken ct = default) =>
        Task.FromException<AuditReport>(ReportsUnsupported());

    public Task<CostReport> CostAsync(
        string? from = null, string? to = null, CancellationToken ct = default) =>
        Task.FromException<CostReport>(ReportsUnsupported());

    public Task<TimeseriesReport> TimeseriesAsync(
        string? window = null, string? plane = null, CancellationToken ct = default) =>
        Task.FromException<TimeseriesReport>(ReportsUnsupported());

    public Task<EndpointsReport> EndpointsAsync(
        string? window = null, long? limit = null, CancellationToken ct = default) =>
        Task.FromException<EndpointsReport>(ReportsUnsupported());

    public Task<RunbookReport> RunbooksAsync(
        string? window = null, CancellationToken ct = default) => Task.FromException<RunbookReport>(ReportsUnsupported());

    public Task<SessionsReport> SessionsAsync(
        string? window = null, CancellationToken ct = default) => Task.FromException<SessionsReport>(ReportsUnsupported());

    public Task<EvidenceReport> EvidenceAsync(
        string? window = null, CancellationToken ct = default) => Task.FromException<EvidenceReport>(ReportsUnsupported());

    public Task<MatrixReport> MatrixAsync(CancellationToken ct = default) =>
        Task.FromException<MatrixReport>(ReportsUnsupported());

    private static UnsupportedTransportException AuthoringUnsupported() => new(
        "guided authoring has no gRPC RPCs — use the REST client (/v1/authoring/…)");

    public Task<IReadOnlyList<PatternSummary>> ListPatternsAsync(
        CancellationToken ct = default) => Task.FromException<IReadOnlyList<PatternSummary>>(AuthoringUnsupported());

    public Task<PatternDetail> GetPatternAsync(string id, CancellationToken ct = default) =>
        Task.FromException<PatternDetail>(AuthoringUnsupported());

    public Task<Draft> CreateDraftAsync(
        string name, string? patternId = null, bool seedFromExemplar = false,
        CancellationToken ct = default) => Task.FromException<Draft>(AuthoringUnsupported());

    public Task<IReadOnlyList<DraftSummary>> ListDraftsAsync(CancellationToken ct = default) =>
        Task.FromException<IReadOnlyList<DraftSummary>>(AuthoringUnsupported());

    public Task<Draft> GetDraftAsync(string draftId, CancellationToken ct = default) =>
        Task.FromException<Draft>(AuthoringUnsupported());

    public Task<DraftDeletion> DeleteDraftAsync(
        string draftId, CancellationToken ct = default) => Task.FromException<DraftDeletion>(AuthoringUnsupported());

    public Task<Draft> PutAnswersAsync(
        string draftId, JsonElement answers, bool materialize = true,
        CancellationToken ct = default) => Task.FromException<Draft>(AuthoringUnsupported());

    public Task<DraftValidation> ValidateAsync(string draftId, CancellationToken ct = default) =>
        Task.FromException<DraftValidation>(AuthoringUnsupported());

    public Task<AssistResult> AssistAsync(
        string draftId, string? description = null, string? instructions = null,
        string? provider = null, string? model = null, string? tier = null,
        CancellationToken ct = default) => Task.FromException<AssistResult>(AuthoringUnsupported());

    public Task<DraftBundle> ExportAsync(string draftId, CancellationToken ct = default) =>
        Task.FromException<DraftBundle>(AuthoringUnsupported());

    public Task<IReadOnlyList<AppliedDoc>> ApplyAsync(
        string draftId, CancellationToken ct = default) => Task.FromException<IReadOnlyList<AppliedDoc>>(AuthoringUnsupported());

    public Task<ServerVersionInfo> ServerVersionAsync(CancellationToken ct = default) =>
        Task.FromException<ServerVersionInfo>(new UnsupportedTransportException(
            "GET /version is a REST meta route — use the REST client, or gRPC " +
            "server reflection"));

    // -- sealed evidence: REST-only in v1 -----------------------------------

    Task<JsonElement> IEvidencePlane.GetAsync(string evidenceId, CancellationToken ct) =>
        Task.FromException<JsonElement>(new UnsupportedTransportException(
            "the sealed evidence plane is REST-only in v1 — use the REST client "
            + "(GET /v1/evidence/{id})"));

    Task<EvidenceRows> IEvidencePlane.RowsAsync(
        string evidenceId, int? from, int? limit,
        CancellationToken ct) =>
        Task.FromException<EvidenceRows>(new UnsupportedTransportException(
            "the sealed evidence plane is REST-only in v1 — use the REST client "
            + "(GET /v1/evidence/{id}/rows)"));
}
