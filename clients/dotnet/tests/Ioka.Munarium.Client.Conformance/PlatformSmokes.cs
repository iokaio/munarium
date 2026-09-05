// SPDX-License-Identifier: Apache-2.0
// The platform surface, proven through the TYPED client planes — a
// native port of the Rust client's platform_smoke.rs (same scenario names,
// platform. prefix, so CI output is comparable), including the SSE
// streaming-turn smoke, the route-coverage sweep, and the gRPC-surface
// scenario.
//
// Where the server's raw suite asserts HTTP statuses + problem slugs, this
// port asserts the TYPED exceptions the client decodes them into — that
// mapping is exactly what the client exists to provide. Requires the pg
// store, an rw and a mgmt static token on the SAME tenant
// (MUNARIUM_TOKEN / MUNARIUM_MGMT_TOKEN), and MUNARIUM_TOKEN_SECRET configured
// server-side. Zero provider keys — nothing here completes.
//
// Re-runnable against a shared dev tenant BY DESIGN: content and doomed
// runbook versions are nonce'd, and no scenario asserts global tenant state
// beyond what this run created.
//
// The scenarios run IN ORDER inside one fact (the application scenario mints
// the token the SSE scenario uses — the same ordering dependency the
// server's own suite has), each under its own try/catch so every failing
// scenario is reported by name.

using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using Xunit;
using Xunit.Sdk;

namespace Ioka.Munarium.Client.Conformance;

public class PlatformSmokes
{
    private const string ShapeYaml =
        "apiVersion: munarium.ioka.io/v1\nkind: Shape\n" +
        "metadata: { name: entdocs, version: 1 }\nspec:\n  fact:\n    schema: { type: object }\n";

    private static string RunbookYaml(uint version) => $$"""
        apiVersion: munarium.ioka.io/v1
        kind: Runbook
        metadata: { name: ent-support, version: {{version}} }
        spec:
          collections:
            - name: ent-public
              shape: entdocs@1
              accessLevel: 0
              sources: { filenamePrefix: "public/" }
            - name: ent-secret
              shape: entdocs@1
              accessLevel: 2
              compartments: [eng]
              sources: { filenamePrefix: "eng/" }
          retrieval: { topK: 5 }
          models:
            default: { provider: default, tier: fast }
            allowOverrides: [default]
          completion:
            promptTemplate: "Answer from context only.\n{context}\n\nQ: {query}"
          steps:
            - resolveSources: {}
            - buildIndex: {}
            - verify: {}
            - cutover: { approval: required }
            - retireOld: { keep_versions: 2 }
        """;

    private static string Nonce() => DateTime.UtcNow.Ticks.ToString("x");

    private static string B64(string s) => Convert.ToBase64String(Encoding.UTF8.GetBytes(s));

    private static string Sha(string s) =>
        Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(s)));

    /// <summary>Guarded per-item access — a short results array must become
    /// a counted failure message, never an IndexOutOfRange that hides the
    /// real shape of the response.</summary>
    private static IngestResult Nth(IReadOnlyList<IngestResult> results, int i, string what) =>
        i < results.Count
            ? results[i]
            : throw new XunitException($"{what}: expected result #{i}, got {results.Count} results");

    private static void Expect(bool condition, string message)
    {
        if (!condition) throw new XunitException(message);
    }

    private sealed class Env : IAsyncDisposable
    {
        public required string Base { get; init; }
        public string? GrpcUrl { get; init; }
        public required string RwToken { get; init; }
        public required string MgmtToken { get; init; }

        /// <summary>One pooled client per role, built once — the connection
        /// behavior a real consumer has.</summary>
        public required MunariumClient Ops { get; init; }
        public required MunariumClient Mgr { get; init; }

        /// <summary>comp-bob's capability token, minted by the application
        /// scenario and reused by the SSE scenario — the same ordering
        /// dependency the server's own suite has.</summary>
        public string? BobToken { get; set; }

        /// <summary>A one-off client for a minted persona (capability JWT +
        /// its uid).</summary>
        public MunariumClient Rest(string token, string uid) =>
            MunariumClient.Rest(new MunariumClientOptions { Endpoint = Base, Token = token, Uid = uid });

        /// <summary>Mint a capability token via the typed tokens plane.</summary>
        public Task<IssuedToken> Mint(
            string uid, int level, IReadOnlyList<string> compartments,
            IReadOnlyList<string> scopes) =>
            Mgr.Tokens.MintAsync(uid, level, scopes, compartments);

        public async ValueTask DisposeAsync()
        {
            await Ops.DisposeAsync();
            await Mgr.DisposeAsync();
        }
    }

    [SkippableFact]
    public async Task PlatformSurface()
    {
        var restUrl = Environment.GetEnvironmentVariable("MUNARIUM_REST_URL");
        Skip.If(restUrl is null, "MUNARIUM_REST_URL not set");
        var mgmtToken = Environment.GetEnvironmentVariable("MUNARIUM_MGMT_TOKEN");
        Skip.If(mgmtToken is null, "MUNARIUM_MGMT_TOKEN not set (the platform smokes WRITE)");
        var rwToken = Environment.GetEnvironmentVariable("MUNARIUM_TOKEN") ?? "devtoken";
        var baseUrl = restUrl!.TrimEnd('/');

        await using var env = new Env
        {
            Base = baseUrl,
            GrpcUrl = Environment.GetEnvironmentVariable("MUNARIUM_GRPC_URL"),
            RwToken = rwToken,
            MgmtToken = mgmtToken!,
            Ops = MunariumClient.Rest(
                new MunariumClientOptions { Endpoint = baseUrl, Token = rwToken, Uid = "ops" }),
            Mgr = MunariumClient.Rest(
                new MunariumClientOptions { Endpoint = baseUrl, Token = mgmtToken!, Uid = "mgr" }),
        };

        var failures = new List<string>();
        async Task Run(string name, Func<Task> scenario)
        {
            try
            {
                await scenario();
            }
            catch (Exception e)
            {
                failures.Add($"FAIL {name}: {e.Message}");
            }
        }

        await Run("platform.uid-contract", () => UidContract(env));
        await Run("platform.role-partition", () => RolePartition(env));
        await Run("platform.application-and-compartments", () => ApplicationAndCompartments(env));
        await Run("platform.removal-double-pass", () => RemovalDoublePass(env));
        await Run("platform.reports-and-revoke", () => ReportsAndRevoke(env));
        await Run("platform.authoring-lifecycle", () => AuthoringLifecycle(env));
        await Run("platform.bulk-upload-lifecycle", () => BulkUploadLifecycle(env));
        await Run("platform.route-coverage", () => RouteCoverage(env));
        await Run("platform.turn-stream-sse", () => TurnStreamSse(env));
        await Run("platform.grpc-surface", () => GrpcSurface(env));

        Assert.True(failures.Count == 0, string.Join("\n", failures));
    }

    /// <summary>The uid contract: no uid draws the typed uid-required
    /// rejection; a JWT presented under a different uid draws the typed
    /// Forbidden.</summary>
    private static async Task UidContract(Env env)
    {
        await using var noUid = MunariumClient.Rest(
            new MunariumClientOptions { Endpoint = env.Base, Token = env.RwToken });
        var e = await Assert.ThrowsAsync<InvalidInputException>(() =>
            noUid.Runbooks.ListAsync());
        Expect(
            e.Message.Contains("uid"),
            $"uid-required detail should name the uid, got '{e.Message}'");

        var alice = await env.Mint("uid-alice", 0, [], ["query"]);
        await using var mallory = env.Rest(alice.Token, "mallory");
        await Assert.ThrowsAsync<ForbiddenException>(() => mallory.Runbooks.ListAsync());
    }

    /// <summary>Role partition: rw cannot mint tokens; mgmt cannot write the
    /// ledger.</summary>
    private static async Task RolePartition(Env env)
    {
        await Assert.ThrowsAsync<ForbiddenException>(() =>
            env.Ops.Tokens.MintAsync("x", 0, ["query"]));
        await Assert.ThrowsAsync<ForbiddenException>(() =>
            env.Mgr.Commands.CreateVersionAsync());
    }

    /// <summary>The full retrieval-application lifecycle + compartmentalized
    /// sessions.</summary>
    private static async Task ApplicationAndCompartments(Env env)
    {
        var ops = env.Ops;
        await ops.Runbooks.ApplyShapeAsync(ShapeYaml);

        // Validation first: clean passes, topK: 0 invalidates.
        var clean = await ops.Runbooks.ValidateAsync(RunbookYaml(1));
        Expect(clean.Valid, $"clean runbook must validate: {string.Join("; ", clean.Findings.Select(f => f.Message))}");
        var bad = await ops.Runbooks.ValidateAsync(RunbookYaml(1).Replace("topK: 5", "topK: 0"));
        Expect(!bad.Valid, "topK: 0 must invalidate");

        await ops.Runbooks.ApplyRunbookAsync(RunbookYaml(1));

        // Ingest via the file plane under the ingest scope; matchers auto-bind.
        var loaderToken = await env.Mint("loader", 2, ["eng"], ["ingest"]);
        await using var loader = env.Rest(loaderToken.Token, "loader");
        var batch = await loader.Ingest.IngestBatchAsync(
        [
            new IngestFile
            {
                Filename = "public/handbook.md", MediaType = "text/markdown",
                ContentBase64 = B64("The public handbook grants twenty vacation days."),
            },
            new IngestFile
            {
                Filename = "eng/launch.md", MediaType = "text/markdown",
                ContentBase64 = B64("Secret launch window: vacation blackout in Q4."),
            },
        ]);
        Expect(batch.Count == 2, $"expected 2 results, got {batch.Count}");
        Expect(
            Nth(batch, 0, "batch").BoundTo.SequenceEqual(["ent-public"])
                && Nth(batch, 1, "batch").BoundTo.SequenceEqual(["ent-secret"]),
            "matcher auto-bind wrong: " + string.Join(
                " | ", batch.Select(r => $"{r.Filename} -> [{string.Join(",", r.BoundTo)}]")));

        // A level-0 ingest token must NOT write into ent-secret.
        var lowToken = await env.Mint("lowloader", 0, [], ["ingest"]);
        await using var low = env.Rest(lowToken.Token, "lowloader");
        await Assert.ThrowsAsync<ForbiddenException>(() =>
            low.Ingest.IngestAsync(new IngestFile
            {
                Filename = "sneak.md", MediaType = "text/markdown",
                ContentBase64 = B64("nope"), Collections = ["ent-secret"],
            }));

        // Run with two per-collection approval passes.
        var run = await ops.Runbooks.RunRunbookAsync("ent-support");
        Expect(run.State == "awaiting_approval", $"run must pause, got '{run.State}'");
        for (var pass = 0; pass < 2; pass++)
        {
            var status = await ops.Runbooks.GetRunAsync(run.RunId);
            var awaiting = status.Steps.FirstOrDefault(s => s.State == "awaiting_approval")
                ?? throw new XunitException(
                    $"no step awaiting approval on pass {pass}: " +
                    string.Join(" | ", status.Steps.Select(s => $"{s.Name}={s.State}")));
            await ops.Runbooks.ApproveStepAsync(run.RunId, awaiting.Ordinal);
        }
        var done = await ops.Runbooks.GetRunAsync(run.RunId);
        Expect(done.State == "done", $"run must finish done, got '{done.State}'");

        // List + info expose per-collection access requirements.
        var list = await ops.Runbooks.ListAsync();
        var entry = list.FirstOrDefault(b => b.RunbookRef == "ent-support@1")
            ?? throw new XunitException("ent-support@1 missing from list");
        var levels = entry.Collections.Select(c => c.AccessLevel).ToArray();
        Expect(
            levels.Contains(0) && levels.Contains(2),
            $"list must show levels 0 and 2: [{string.Join(",", levels)}]");
        var info = await ops.Runbooks.GetInfoAsync("ent-support");
        Expect(
            info.Collections.Count == 2 && info.HasCompletion,
            $"info must carry both collections + completion (got {info.Collections.Count}, " +
            $"has_completion={info.HasCompletion})");

        // Two clearances, one runbook: disjoint result sets for one query.
        var aliceToken = await env.Mint("comp-alice", 0, [], ["query"]);
        var bobToken = await env.Mint("comp-bob", 2, ["eng"], ["query"]);
        await using var alice = env.Rest(aliceToken.Token, "comp-alice");
        await using var bob = env.Rest(bobToken.Token, "comp-bob");

        var sessionA = await alice.Sessions.CreateAsync("ent-support");
        Expect(
            sessionA.PermittedCollections.SequenceEqual(["ent-public"]),
            $"alice must see only ent-public: [{string.Join(",", sessionA.PermittedCollections)}]");
        var sessionB = await bob.Sessions.CreateAsync("ent-support");
        Expect(
            sessionB.PermittedCollections.Count == 2,
            $"bob must see both collections: [{string.Join(",", sessionB.PermittedCollections)}]");

        var turnA = await alice.Sessions.TurnAsync(
            sessionA.SessionId, new TurnRequest { Query = "vacation" });
        Expect(
            turnA.Hits.Count > 0 && turnA.Hits.All(h => h.Collection == "ent-public"),
            $"alice hits must be ent-public only ({turnA.Hits.Count} hits from " +
            $"[{string.Join(",", turnA.Hits.Select(h => h.Collection).Distinct())}])");

        var turnB = await bob.Sessions.TurnAsync(
            sessionB.SessionId, new TurnRequest { Query = "vacation" });
        Expect(
            turnB.Hits.Any(h => h.Collection == "ent-secret"),
            "bob's merged hits must include ent-secret");
        Expect(
            turnB.Envelopes.Count == 2,
            $"bob must get one envelope per collection, got {turnB.Envelopes.Count}");

        // Multiturn continuity, transcript readback, cross-uid refusal.
        var turn2 = await bob.Sessions.TurnAsync(
            sessionB.SessionId, new TurnRequest { Query = "blackout" });
        Expect(turn2.Ordinal == 2, $"follow-on turn must be ordinal 2, got {turn2.Ordinal}");
        var readback = await bob.Sessions.GetAsync(sessionB.SessionId);
        Expect(
            readback.Turns.Count == 2 && readback.State == "open",
            $"transcript must hold both turns (got {readback.Turns.Count}, state {readback.State})");
        await Assert.ThrowsAsync<ForbiddenException>(() =>
            alice.Sessions.TurnAsync(sessionB.SessionId, new TurnRequest { Query = "x" }));

        // Model-override policy refusal (checked BEFORE any provider spend).
        await Assert.ThrowsAsync<ForbiddenException>(() =>
            bob.Sessions.TurnAsync(sessionB.SessionId, new TurnRequest
            {
                Query = "x", Complete = true,
                ModelOverride = new ModelOverride { Provider = "not-allowed-provider" },
            }));

        // Scope enforcement: a query token cannot ingest.
        await Assert.ThrowsAsync<ForbiddenException>(() =>
            bob.Ingest.IngestAsync(new IngestFile
            {
                Filename = "x.md", MediaType = "text/markdown", ContentBase64 = B64("x"),
            }));

        env.BobToken = bobToken.Token;
    }

    /// <summary>Soft removal is double-pass and leaves data intact.</summary>
    private static async Task RemovalDoublePass(Env env)
    {
        var ops = env.Ops;
        // The doomed version is NONCE'D (seconds since epoch): removal is
        // permanent, so a fixed number makes this scenario single-use
        // against a shared dev tenant.
        var doomedVersion = (uint)(DateTimeOffset.UtcNow.ToUnixTimeSeconds() % 2_000_000_000);
        var doomed = $"ent-support@{doomedVersion}";
        await ops.Runbooks.ApplyRunbookAsync(RunbookYaml(doomedVersion));

        // Single-pass confirm is refused (409 removal-not-confirmed → typed).
        await Assert.ThrowsAsync<InvalidInputException>(() =>
            ops.Runbooks.RemoveConfirmAsync(doomed, "rm-guess"));

        var removal = await ops.Runbooks.RemoveRequestAsync(doomed);
        Expect(removal.RemovalId.Length > 0, "removal_id missing");

        // A WRONG removal_id must draw the SAME typed refusal as no request
        // — accepting any error here would let a transient 503 or a routing
        // bug masquerade as the double-pass guard working.
        await Assert.ThrowsAsync<InvalidInputException>(() =>
            ops.Runbooks.RemoveConfirmAsync(doomed, "rm-wrong"));

        var confirmed = await ops.Runbooks.RemoveConfirmAsync(doomed, removal.RemovalId);
        Expect(confirmed.Status == "removed", $"confirm status: {confirmed.Status}");

        // Sessions on the removed exact ref: typed NotFound (410
        // runbook-removed); the bare name still resolves to a LIVE version —
        // not asserted to be @1, because earlier smoke runs against a shared
        // tenant may have left other versions, but never the one this run
        // just removed.
        var user = await env.Mint("rm-user", 0, [], ["query"]);
        await using var rmUser = env.Rest(user.Token, "rm-user");
        await Assert.ThrowsAsync<MunariumNotFoundException>(() =>
            rmUser.Sessions.CreateAsync(doomed));
        var live = await rmUser.Sessions.CreateAsync("ent-support");
        Expect(
            live.RunbookRef.StartsWith("ent-support@") && live.RunbookRef != doomed,
            $"bare name must resolve to a live version, got {live.RunbookRef}");

        // Hidden from the default list; visible with include_removed.
        var list = await env.Ops.Runbooks.ListAsync();
        Expect(
            !list.Any(b => b.RunbookRef == doomed),
            "removed ref must be hidden from the default list");
        var all = await env.Ops.Runbooks.ListAsync(includeRemoved: true);
        Expect(all.Any(b => b.RunbookRef == doomed), "include_removed must show it");
    }

    /// <summary>Reports are mgmt-gated and reflect this suite's traffic;
    /// revocation lands in the issuance audit.</summary>
    private static async Task ReportsAndRevoke(Env env)
    {
        await Assert.ThrowsAsync<ForbiddenException>(() =>
            env.Ops.Reports.UsageAsync(groupBy: "uid"));

        var mgr = env.Mgr;
        var usage = await mgr.Reports.UsageAsync(groupBy: "uid");
        var keys = usage.Rows.Select(r => r.Key).ToArray();
        Expect(
            keys.Contains("comp-alice") && keys.Contains("comp-bob"),
            $"usage rows must include the session uids: [{string.Join(",", keys)}]");

        var audit = await mgr.Reports.AuditAsync(uid: "comp-bob", limit: 10);
        Expect(audit.Entries.Count > 0, "audit for comp-bob must be non-empty");

        // The dashboard-view reports answer too (2026-08-18 routes).
        var ts = await mgr.Reports.TimeseriesAsync("24h");
        Expect(ts.Window == "24h", $"timeseries window echo: {ts.Window}");
        var eps = await mgr.Reports.EndpointsAsync("24h", 5);
        Expect(eps.Rows.Count > 0, "endpoint rows must reflect traffic");
        await mgr.Reports.RunbooksAsync("24h");
        var sess = await mgr.Reports.SessionsAsync("24h");
        Expect(
            sess.Buckets.Any(b => b.Turns > 0),
            "sessions report must show the turns this suite took");
        await mgr.Reports.CostAsync();

        // S-3.5. This suite takes no research-profile turns, so the counts
        // are the legacy path only — what is being proven here is that the
        // routes answer a mgmt bearer and decode, and that a turn WITHOUT a
        // profile is counted as legacy rather than as an empty hierarchy.
        var evidence = await mgr.Reports.EvidenceAsync("24h");
        Expect(evidence.Window == "24h", $"evidence window echo: {evidence.Window}");
        Expect(
            evidence.LegacyTurns > 0 && evidence.HierarchyTurns == 0,
            $"turns without a profile are legacy: {evidence.LegacyTurns} legacy, " +
            $"{evidence.HierarchyTurns} hierarchy");
        Expect(evidence.Layers.Count == 0, "no profile ran, so no layer stats");

        // Unwired and wired-but-failing must not read the same, so an
        // unconfigured plane must never report failures — asserted this way
        // rather than pinning `configured`, which depends on whether the
        // server under test happens to have a Matrix base URL.
        var matrix = await mgr.Reports.MatrixAsync();
        Expect(
            matrix.Configured || (!matrix.CircuitOpen && matrix.ConsecutiveFailures == 0),
            $"an unwired Matrix plane cannot have failed: open={matrix.CircuitOpen}, " +
            $"failures={matrix.ConsecutiveFailures}");
        await Assert.ThrowsAsync<ForbiddenException>(() => env.Ops.Reports.MatrixAsync());

        // Revoke: the deny-list row lands and the audit shows it.
        var revokee = await env.Mint("revokee", 0, [], ["query"]);
        var revoked = await mgr.Tokens.RevokeAsync(revokee.Jti);
        Expect(revoked.Revoked, "revoke must land");
        var tokens = await mgr.Tokens.ListAsync(uid: "revokee");
        Expect(
            tokens.Count > 0 && tokens[0].RevokedAt is not null,
            "issuance audit must show revoked_at");
    }

    /// <summary>Guided authoring end to end, keyless: catalog → draft →
    /// answers → validate → assist (degrades to a note) → export
    /// (hash-verified client-side) → apply → hosted → cleaned up.</summary>
    private static async Task AuthoringLifecycle(Env env)
    {
        var ops = env.Ops;
        var patterns = await ops.Authoring.ListPatternsAsync();
        Expect(patterns.Count == 7, $"expected the 7 patterns, got {patterns.Count}");
        var detail = await ops.Authoring.GetPatternAsync("ask-the-corpus");
        Expect(
            detail.RunbookYaml.Contains("kind: Runbook"),
            "pattern detail carries the exemplar");

        var draft = await ops.Authoring.CreateDraftAsync(
            "vendor-security", patternId: "ask-the-corpus");
        Expect(draft.DraftId.Length > 0, "draft_id missing");
        Expect(
            draft.Interview.FirstOrDefault()?.Id == "identity",
            "interview starts at identity");

        // The workspace listing + readback name the draft.
        var drafts = await ops.Authoring.ListDraftsAsync();
        Expect(
            drafts.Any(d => d.DraftId == draft.DraftId),
            "list_drafts must contain the new draft");
        var readback = await ops.Authoring.GetDraftAsync(draft.DraftId);
        Expect(readback.Name == "vendor-security", $"draft readback name: {readback.Name}");

        // A blank draft refuses to export (409 authoring-draft-invalid → typed).
        await Assert.ThrowsAsync<InvalidInputException>(() =>
            ops.Authoring.ExportAsync(draft.DraftId));

        using var answers = JsonDocument.Parse("""
            {
              "identity.description": "Vendor security reviews for procurement.",
              "prefix.root": "vendors/",
              "prefix.areas": [
                { "path": "public/", "description": "published attestations" },
                { "path": "contracts/", "description": "signed agreements" }
              ],
              "access.uniform_public": false,
              "access.area_levels": { "public": 0, "contracts": 2 },
              "access.area_compartments": { "contracts": ["legal"] }
            }
            """);
        var updated = await ops.Authoring.PutAnswersAsync(
            draft.DraftId, answers.RootElement.Clone());
        Expect(
            updated.Validation?.Valid == true,
            "canonical answers must validate clean");
        Expect(
            updated.Documents.Count == 2,
            $"one shape + one runbook, got {updated.Documents.Count}");

        // Assist DEGRADES keyless: 200 + assist_note, documents intact.
        var assist = await ops.Authoring.AssistAsync(draft.DraftId);
        Expect(assist.AssistNote is not null, "keyless assist must carry a degrade note");
        Expect(assist.Documents.Count == 2, "assist must not lose documents");

        var validation = await ops.Authoring.ValidateAsync(draft.DraftId);
        Expect(validation.Valid, "draft must validate");

        // Export: verify the manifest CLIENT-side, exactly as mmctl does.
        var bundle = await ops.Authoring.ExportAsync(draft.DraftId);
        Expect(bundle.Kind == "MunariumAuthoringBundle", $"bundle kind: {bundle.Kind}");
        var buf = new StringBuilder();
        foreach (var path in bundle.Files.Keys.OrderBy(p => p, StringComparer.Ordinal))
        {
            var actual = Sha(bundle.Files[path]);
            Expect(
                bundle.Hashes.TryGetValue(path, out var declared) && declared == actual,
                $"per-file hash mismatch for {path}");
            buf.Append(path).Append('\0').Append(actual).Append('\n');
        }
        var manifest = Sha(buf.ToString());
        Expect(
            bundle.ManifestHash == manifest,
            $"manifest hash mismatch (client-recomputed {manifest})");
        Expect(
            bundle.ApplyOrder.FirstOrDefault()?.StartsWith("shapes/") == true,
            $"shapes apply first: [{string.Join(",", bundle.ApplyOrder)}]");

        var applied = await ops.Authoring.ApplyAsync(draft.DraftId);
        Expect(applied.Count == 2, $"apply covers the set, got {applied.Count}");
        var hosted = await ops.Runbooks.GetInfoAsync("vendor-security");
        Expect(
            hosted.Collections.Count == 2,
            "applied runbook reaches its two collections");

        // Draft cleanup — the client surface's one DELETE (soft, workspace-only).
        var deleted = await ops.Authoring.DeleteDraftAsync(draft.DraftId);
        Expect(deleted.Status == "deleted", $"delete status: {deleted.Status}");
    }

    /// <summary>Bulk upload sessions: manifest diff, chunked upload with
    /// per-file sha verification, replay idempotency, finalize verification,
    /// the zero-byte re-run — plus the CLIENT-side chunk-cap guard.</summary>
    private static async Task BulkUploadLifecycle(Env env)
    {
        var ops = env.Ops;
        const string Shape =
            "apiVersion: munarium.ioka.io/v1\nkind: Shape\n" +
            "metadata: { name: bulkdocs, version: 1 }\nspec:\n  fact:\n    schema: { type: object }\n";
        await ops.Runbooks.ApplyShapeAsync(Shape);
        const string Runbook = """
            apiVersion: munarium.ioka.io/v1
            kind: Runbook
            metadata: { name: bulk-archive, version: 1 }
            spec:
              collections:
                - name: bulk-open-docs
                  shape: bulkdocs@1
                  accessLevel: 0
                  sources: { filenamePrefix: "bulkdocs/" }
              retrieval: { topK: 5 }
              steps:
                - resolveSources: {}
                - buildIndex: {}
                - verify: {}
                - cutover: { approval: required }
                - retireOld: { keep_versions: 2 }
            """;
        await ops.Runbooks.ApplyRunbookAsync(Runbook);

        var loaderToken = await env.Mint("bulkloader", 0, [], ["ingest"]);
        await using var loader = env.Rest(loaderToken.Token, "bulkloader");
        // Nonce'd contents: this scenario re-runs against a shared dev
        // server, and the zero-byte-re-run assertion needs fresh bytes.
        var n = Nonce();
        var a = $"Bulk document alpha {n}: the treaty was signed.";
        var b = $"Bulk document beta {n}: the harbor closed in March.";
        var c = $"Bulk document gamma {n}: the assembly dissolved.";
        BulkManifestEntry Entry(string name, string text) => new()
        {
            Filename = name, Sha256 = Sha(text),
            BytesLen = (ulong)Encoding.UTF8.GetByteCount(text), MediaType = "text/markdown",
        };
        IngestFile File(string name, string text) => new()
        {
            Filename = name, MediaType = "text/markdown", ContentBase64 = B64(text),
        };

        // CLIENT-side guard: an over-cap chunk never leaves the process.
        var oversized = Enumerable.Range(0, 501)
            .Select(i => File($"bulkdocs/f{i}.md", "x"))
            .ToArray();
        var capError = await Assert.ThrowsAsync<InvalidInputException>(() =>
            loader.Ingest.BulkChunkAsync("blk-any", oversized));
        Expect(capError.Message.Contains("500"), $"cap error must name the cap: {capError.Message}");

        // Manifest validation server-side: duplicates rejected whole.
        await Assert.ThrowsAsync<InvalidInputException>(() =>
            loader.Ingest.BulkOpenAsync([Entry("bulkdocs/a.md", a), Entry("bulkdocs/a.md", a)]));

        // Open: fresh manifest, all three needed.
        var open = await loader.Ingest.BulkOpenAsync(
            [Entry("bulkdocs/a.md", a), Entry("bulkdocs/b.md", b), Entry("bulkdocs/c.md", c)],
            label: "client-conformance");
        Expect(
            open.Total == 3 && open.AlreadyPresent == 0 && open.Needed.Count == 3,
            $"fresh open must need all three (total {open.Total}, present " +
            $"{open.AlreadyPresent}, needed {open.Needed.Count})");

        // Chunk 1: a good; b deliberately corrupt (per-file sha mismatch).
        var chunk1 = await loader.Ingest.BulkChunkAsync(
            open.BulkId, [File("bulkdocs/a.md", a), File("bulkdocs/b.md", "corrupted bytes")]);
        Expect(Nth(chunk1.Results, 0, "chunk 1").Error is null, "a.md must store");
        Expect(
            Nth(chunk1.Results, 1, "chunk 1").Error?.Contains("sha256 mismatch") == true,
            $"corrupt b.md must fail per-file, got '{Nth(chunk1.Results, 1, "chunk 1").Error}'");
        Expect(
            chunk1.Stored == 1 && chunk1.Failed == 1 && chunk1.Pending == 1,
            $"chunk 1 counts: stored {chunk1.Stored} failed {chunk1.Failed} pending {chunk1.Pending}");

        // Early finalize: incomplete, naming what is owed.
        var early = await loader.Ingest.BulkCompleteAsync(open.BulkId);
        Expect(
            early.Status == "incomplete" && early.MissingCount == 2,
            $"early complete: {early.Status}, missing {early.MissingCount}");

        // Chunk 2 — wholesale replay: a again (idempotent), b fixed, c.
        var chunk2 = await loader.Ingest.BulkChunkAsync(
            open.BulkId,
            [File("bulkdocs/a.md", a), File("bulkdocs/b.md", b), File("bulkdocs/c.md", c)]);
        Expect(
            Nth(chunk2.Results, 0, "chunk 2").Existed,
            "replayed a.md must be an idempotent no-op");
        Expect(
            chunk2.Pending == 0 && chunk2.Failed == 0,
            $"nothing owed after chunk 2 (pending {chunk2.Pending}, failed {chunk2.Failed})");

        // Finalize + status agree; the session's stored count survives replay.
        var complete = await loader.Ingest.BulkCompleteAsync(open.BulkId);
        Expect(complete.Status == "completed", $"complete: {complete.Status}");
        var status = await loader.Ingest.BulkStatusAsync(open.BulkId, includeNeeded: true);
        Expect(
            status.Status == "completed" && status.Needed is { Count: 0 } && status.Stored == 3,
            $"status after complete: {status.Status}, needed " +
            $"{status.Needed?.Count.ToString() ?? "absent"}, stored {status.Stored}");

        // Zero-byte re-run: same manifest, nothing owed, completes chunkless.
        var rerun = await loader.Ingest.BulkOpenAsync(
            [Entry("bulkdocs/a.md", a), Entry("bulkdocs/b.md", b), Entry("bulkdocs/c.md", c)]);
        Expect(
            rerun.AlreadyPresent == 3 && rerun.Needed.Count == 0,
            $"re-run open must owe nothing (present {rerun.AlreadyPresent}, " +
            $"needed {rerun.Needed.Count})");
        var rerunDone = await loader.Ingest.BulkCompleteAsync(rerun.BulkId);
        Expect(rerunDone.Status == "completed", $"zero-byte re-run: {rerunDone.Status}");

        // Unknown session: typed NotFound.
        await Assert.ThrowsAsync<MunariumNotFoundException>(() =>
            loader.Ingest.BulkStatusAsync("blk-doesnotexist"));

        // get_source is a CONTROL-plane read: static tokens only — a
        // capability JWT draws the typed 403, and the rw static token reads
        // the metadata back.
        var sourceId = Nth(chunk2.Results, 2, "chunk 2").SourceId
            ?? throw new XunitException("c.md must carry a source_id");
        await Assert.ThrowsAsync<ForbiddenException>(() =>
            loader.Ingest.GetSourceAsync(sourceId));
        var infoBack = await ops.Ingest.GetSourceAsync(sourceId);
        Expect(
            infoBack.Filename == "bulkdocs/c.md" && infoBack.ContentHash == Sha(c),
            $"source metadata must match what was uploaded: {infoBack.Filename}");
    }

    /// <summary>The routes no other scenario touches: /version, the
    /// collections trio, chronology rules, and the findings query — so a
    /// regression in any of them fails a smoke instead of shipping green.</summary>
    private static async Task RouteCoverage(Env env)
    {
        var ops = env.Ops;

        // GET /version (unauthenticated meta).
        var version = await ops.ServerVersionAsync();
        Expect(
            version.Name == "munarium-server" && version.Version.Length > 0,
            $"version handshake: {version.Name} {version.Version}");

        // Collections trio (depends on entdocs@1 from the application scenario).
        var name = $"cov-{Nonce()}";
        var created = await ops.Retrieval.CreateCollectionAsync(
            name, "entdocs@1", accessLevel: 1, compartments: ["cov"],
            description: "route-coverage smoke");
        Expect(
            created.Name == name && created.AccessLevel == 1,
            $"created collection echo: {created.Name} level {created.AccessLevel}");
        var listed = await ops.Retrieval.ListCollectionsAsync();
        Expect(
            listed.Any(col => col.Id == created.Id),
            "collection must appear in the listing");
        var fetched = await ops.Retrieval.GetCollectionAsync(created.Id);
        Expect(
            fetched.Compartments.SequenceEqual(["cov"])
                && fetched.Description == "route-coverage smoke",
            "collection round-trip");

        // Chronology rules: apply (upsert) + verbatim readback.
        const string RulesYaml =
            "apiVersion: munarium.ioka.io/v1\nkind: ChronologyRules\n" +
            "metadata: { name: cov-rules }\nspec:\n  order:\n" +
            "    - { before: founding.date, after: dissolution.date }\n";
        var applied = await ops.Runbooks.ApplyChronologyRulesAsync(RulesYaml);
        Expect(
            applied.Name == "cov-rules" && applied.RuleCount == 2,
            $"chronology apply echo: {applied.Name} rules {applied.RuleCount}");
        var rulesBack = await ops.Runbooks.GetChronologyRulesAsync("cov-rules");
        Expect(rulesBack == RulesYaml, "chronology rules must read back verbatim");

        // Findings query: empty on a fresh lineage, severity filter
        // accepted, and a bogus severity draws the typed rejection.
        var v = await ops.Commands.CreateVersionAsync();
        var findings = await ops.Query.FindingsAsync(v, severity: "block");
        Expect(findings.Count == 0, "fresh lineage must have no findings");
        await Assert.ThrowsAsync<InvalidInputException>(() =>
            ops.Query.FindingsAsync(v, severity: "bogus"));
    }

    /// <summary>The SSE streaming turn: progress events at real stage
    /// boundaries, then exactly one Done that matches the unary shape; a
    /// closed session draws the typed session-not-open refusal.</summary>
    private static async Task TurnStreamSse(Env env)
    {
        var bobToken = env.BobToken
            ?? throw new XunitException("application scenario did not run — no session token");
        await using var bob = env.Rest(bobToken, "comp-bob");
        var session = await bob.Sessions.CreateAsync("ent-support");

        var progress = 0;
        TurnResult? done = null;
        await foreach (var item in bob.Sessions.TurnStreamAsync(
            session.SessionId, new TurnRequest { Query = "vacation" }))
        {
            switch (item)
            {
                case TurnStreamEvent.Progress:
                    Expect(done is null, "no progress may arrive after the terminal done event");
                    progress++;
                    break;
                case TurnStreamEvent.Done d:
                    Expect(done is null, "exactly one done event");
                    done = d.Response;
                    break;
                default:
                    throw new XunitException($"unknown stream item {item}");
            }
        }
        Expect(done is not null, "stream ended without a done event");
        Expect(
            progress >= 1,
            $"expected at least one progress event (retrieval/merge), got {progress}");
        Expect(
            done!.Hits.Count > 0 && done.Ordinal >= 1,
            $"streamed done must carry the full TurnResult ({done.Hits.Count} hits, " +
            $"ordinal {done.Ordinal})");

        // Closed session: the refusal is typed session-not-open. It may land
        // either pre-stream (plain problem+json) or as the stream's terminal
        // error event (proven live on the Rust port) — both decode
        // identically through the one registry, both surface here as the
        // typed exception during enumeration, and an errored stream yields
        // nothing else (the exception ends it).
        var closed = await bob.Sessions.CloseAsync(session.SessionId);
        Expect(closed.State == "closed", $"close must land: {closed.State}");
        await Assert.ThrowsAsync<InvalidInputException>(async () =>
        {
            await foreach (var item in bob.Sessions.TurnStreamAsync(
                session.SessionId, new TurnRequest { Query = "x" }))
            {
                throw new XunitException(
                    $"closed-session stream must refuse before any event, got {item}");
            }
        });
    }

    /// <summary>gRPC halves of the platform surface: the AdminService
    /// token trio, the SessionService round-trip, the collections trio, and
    /// the honest Unsupported set.</summary>
    private static async Task GrpcSurface(Env env)
    {
        if (env.GrpcUrl is null) return; // REST-only invocation — nothing to prove

        await using var mgr = MunariumClient.Grpc(new MunariumClientOptions
        {
            Endpoint = env.GrpcUrl, Token = env.MgmtToken, Uid = "mgr",
        });

        // Token trio over AdminService.
        var minted = await mgr.Tokens.MintAsync(
            "grpc-user", 2, ["query"], compartments: ["eng"]);
        var listed = await mgr.Tokens.ListAsync(uid: "grpc-user", active: true);
        Expect(
            listed.Any(t => t.Jti == minted.Jti),
            "grpc-minted token must appear in the audit");

        // Collections trio over RetrievalService (rw static token).
        await using var rw = MunariumClient.Grpc(new MunariumClientOptions
        {
            Endpoint = env.GrpcUrl, Token = env.RwToken, Uid = "ops",
        });
        var name = $"cov-grpc-{Nonce()}";
        var created = await rw.Retrieval.CreateCollectionAsync(name, "entdocs@1");
        var fetched = await rw.Retrieval.GetCollectionAsync(created.Id);
        Expect(fetched.Name == name, "grpc collection round-trip");
        var collections = await rw.Retrieval.ListCollectionsAsync();
        Expect(collections.Any(c => c.Id == created.Id), "grpc collection listing");

        // SessionService round-trip with the minted JWT.
        await using var user = MunariumClient.Grpc(new MunariumClientOptions
        {
            Endpoint = env.GrpcUrl, Token = minted.Token, Uid = "grpc-user",
        });
        var session = await user.Sessions.CreateAsync("ent-support");
        var turn = await user.Sessions.TurnAsync(
            session.SessionId, new TurnRequest { Query = "vacation" });
        Expect(
            turn.Hits.Count > 0 && turn.Envelopes.Count > 0,
            $"grpc turn must carry hits + envelopes ({turn.Hits.Count} hits, " +
            $"{turn.Envelopes.Count} envelopes)");
        var readback = await user.Sessions.GetAsync(session.SessionId);
        Expect(readback.Turns.Count == 1, "grpc transcript readback");
        var closed = await user.Sessions.CloseAsync(session.SessionId);
        Expect(closed.State == "closed", $"grpc close: {closed.State}");

        // The honest Unsupported set — every one surfaces at await (the
        // SSE iterator on its first MoveNextAsync), never synchronously.
        await Assert.ThrowsAsync<UnsupportedTransportException>(async () =>
        {
            await foreach (var _ in user.Sessions.TurnStreamAsync(
                session.SessionId, new TurnRequest { Query = "x" }))
            {
            }
        });
        await Assert.ThrowsAsync<UnsupportedTransportException>(() =>
            mgr.Reports.UsageAsync());
        await Assert.ThrowsAsync<UnsupportedTransportException>(() =>
            mgr.Authoring.ListPatternsAsync());

        // Revoke last so the earlier calls ran under a live token.
        var revoked = await mgr.Tokens.RevokeAsync(minted.Jti);
        Expect(revoked.Revoked, "grpc revoke must land");
    }
}
