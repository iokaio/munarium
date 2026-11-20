// SPDX-License-Identifier: Apache-2.0
// Offline pins for the Server 1.5 (ClearView) wire fields on the typed
// facade: C1 collection_availability/coverage, R1 disclose_execution/
// execution and R11 retryable/retry_action. Each request option leaves the
// process only when set; each response member is optional, so a v1.4 body
// still decodes. No server: a canned HttpMessageHandler stands in for the
// REST peer, and the gRPC mapping is exercised on hand-built messages.

using System.Net;
using System.Text;
using System.Text.Json;
using Google.Protobuf;
using Google.Protobuf.WellKnownTypes;
using Grpc.Core;
using Xunit;

namespace Ioka.Munarium.Client.Tests;

public class ClearViewFieldsTests
{
    private sealed class CannedHandler(string body) : HttpMessageHandler
    {
        public List<string?> Bodies { get; } = [];

        protected override async Task<HttpResponseMessage> SendAsync(
            HttpRequestMessage request, CancellationToken cancellationToken)
        {
            Bodies.Add(request.Content is null
                ? null
                : await request.Content.ReadAsStringAsync(cancellationToken));
            return new HttpResponseMessage(HttpStatusCode.OK)
            {
                Content = new StringContent(body, Encoding.UTF8, "application/json"),
            };
        }
    }

    private static MunariumClient RestOver(CannedHandler handler) =>
        MunariumClient.Rest(
            new MunariumClientOptions { Endpoint = "http://127.0.0.1:1", Token = "t", Uid = "u" },
            new HttpClient(handler));

    private const string LegacyTurn = """
        {"session_id":"s-1","ordinal":1,"collections_searched":["contracts","letters"],
         "skipped":[],"hits":[],"envelopes":[]}
        """;

    private const string CoveredTurn = """
        {"session_id":"s-1","ordinal":1,"collections_searched":["contracts","letters"],
         "skipped":[],"hits":[],"envelopes":[],
         "coverage":{"policy":"report","complete":false,
           "members":[
             {"collection":"contracts","stage":"search","outcome":"complete","hits":3},
             {"collection":"memos","stage":"search","outcome":"empty","hits":0},
             {"collection":"letters","stage":"probe","outcome":"unavailable",
              "reason":"no_active_index"}],
           "withheld_claims":["absence","exhaustive_coverage"]}}
        """;

    private const string Envelope = """
        {"chunk_ids":[],"source_ids":[],"source_paths":[],"source_content_hashes":[],
         "index_version":"iv-1","event_watermark":9}
        """;

    private static readonly string LegacySearch = $$"""{"hits":[],"envelope":{{Envelope}}}""";

    private static readonly string DisclosedSearch = $$$"""
        {"hits":[],"envelope":{{{Envelope}}},
         "execution":{"version":1,"engine":"postgresql","lexical_domain":"ts_rank",
           "legs":[
             {"leg":"lexical","candidate_limit":50,"fetched":12,"exhausted":true},
             {"leg":"vector","candidate_limit":50,"fetched":null,"exhausted":null}],
           "stages":[{"stage":"reranking","policy":"prefer","policy_revision":4,
             "applied":false,"state":"unavailable","fallback":"fused_order"}]}}
        """;

    // -- C1: the request option ---------------------------------------------

    [Fact]
    public async Task ATurnSendsCollectionAvailabilityOnlyWhenSelected()
    {
        var handler = new CannedHandler(LegacyTurn);
        await using var client = RestOver(handler);
        await client.Sessions.TurnAsync("s-1", new TurnRequest { Query = "q" });
        await client.Sessions.TurnAsync(
            "s-1",
            new TurnRequest { Query = "q", CollectionAvailability = CollectionAvailability.Require });
        Assert.Equal("""{"query":"q"}""", handler.Bodies[0]);
        Assert.Equal("""{"query":"q","collection_availability":"require"}""", handler.Bodies[1]);
    }

    [Fact]
    public void TheGrpcTurnRequestCarriesThePolicyOnlyWhenSelected()
    {
        var plain = GrpcTransport.ToPbTurnRequest("s-1", new TurnRequest { Query = "q" });
        Assert.Equal("", plain.CollectionAvailability);
        // proto3 puts no bytes on the wire for "": exactly the v1.4 request.
        Assert.Equal(
            new Mmp.V1.TurnRequest { SessionId = "s-1", Query = "q" }.ToByteArray(),
            plain.ToByteArray());
        var selected = GrpcTransport.ToPbTurnRequest(
            "s-1", new TurnRequest { Query = "q", CollectionAvailability = "report" });
        Assert.Equal("report", selected.CollectionAvailability);
    }

    // -- C1: the response member --------------------------------------------

    [Fact]
    public async Task ARestTurnDecodesCoverage()
    {
        await using var client = RestOver(new CannedHandler(CoveredTurn));
        var result = await client.Sessions.TurnAsync(
            "s-1", new TurnRequest { Query = "q", CollectionAvailability = "report" });
        AssertCoverage(result.Coverage);
    }

    [Fact]
    public void AV14TurnWithoutCoverageDecodesToNullAndRoundTripsWithoutTheKey()
    {
        var result = JsonSerializer.Deserialize(LegacyTurn, MunariumJsonContext.Default.TurnResult)!;
        Assert.Null(result.Coverage);
        Assert.DoesNotContain(
            "coverage", JsonSerializer.Serialize(result, MunariumJsonContext.Default.TurnResult));
    }

    [Fact]
    public void CoverageRoundTripsThroughJson()
    {
        var result = JsonSerializer.Deserialize(CoveredTurn, MunariumJsonContext.Default.TurnResult)!;
        var again = JsonSerializer.Deserialize(
            JsonSerializer.Serialize(result, MunariumJsonContext.Default.TurnResult),
            MunariumJsonContext.Default.TurnResult)!;
        AssertCoverage(again.Coverage);
    }

    [Fact]
    public void MemberProgressCarriesOutcomeAndReasonOnlyUnderAPolicy()
    {
        var unavailable = JsonSerializer.Deserialize(
            """{"stage":"retrieval","collection":"letters","hits":0,"skipped":false,"outcome":"unavailable","reason":"engine_unavailable"}""",
            MunariumJsonContext.Default.TurnProgressEvent)!;
        Assert.Equal("unavailable", unavailable.Outcome);
        Assert.Equal("engine_unavailable", unavailable.Reason);
        var legacy = JsonSerializer.Deserialize(
            """{"stage":"retrieval","collection":"contracts","hits":3,"skipped":false}""",
            MunariumJsonContext.Default.TurnProgressEvent)!;
        Assert.Null(legacy.Outcome);
        Assert.Null(legacy.Reason);
    }

    [Fact]
    public void GrpcCoverageMapsLikeTheRestTwin()
    {
        var resp = new Mmp.V1.TurnResponse
        {
            SessionId = "s-1",
            Ordinal = 1,
            Coverage = new Mmp.V1.TurnCoverage
            {
                Policy = "report",
                Complete = false,
                Members =
                {
                    new Mmp.V1.CollectionOutcome
                    {
                        Collection = "contracts", Stage = "search", Outcome = "complete", Hits = 3,
                    },
                    new Mmp.V1.CollectionOutcome
                    {
                        Collection = "memos", Stage = "search", Outcome = "empty", Hits = 0,
                    },
                    new Mmp.V1.CollectionOutcome
                    {
                        Collection = "letters", Stage = "probe", Outcome = "unavailable",
                        Reason = "no_active_index",
                    },
                },
                WithheldClaims = { "absence", "exhaustive_coverage" },
            },
        };
        AssertCoverage(GrpcTransport.ToTurnResult(resp).Coverage);
        Assert.Null(GrpcTransport.ToTurnResult(new Mmp.V1.TurnResponse { SessionId = "s-1" }).Coverage);
    }

    private static void AssertCoverage(TurnCoverage? c)
    {
        Assert.NotNull(c);
        Assert.Equal("report", c.Policy);
        Assert.False(c.Complete);
        Assert.Equal(["absence", "exhaustive_coverage"], c.WithheldClaims);
        Assert.Equal(3, c.Members.Count);
        var complete = c.Members[0];
        Assert.Equal(("contracts", "complete", 3u), (complete.Collection, complete.Outcome, complete.Hits));
        Assert.Null(complete.Layer);
        Assert.Null(complete.Reason);
        // An authorized empty member is a successful outcome with a zero count.
        Assert.Equal(("empty", 0u), (c.Members[1].Outcome, c.Members[1].Hits));
        var unavailable = c.Members[2];
        Assert.Equal(("probe", "unavailable"), (unavailable.Stage, unavailable.Outcome));
        Assert.Null(unavailable.Hits);
        Assert.Equal("no_active_index", unavailable.Reason);
    }

    // -- R1: disclose_execution / execution ----------------------------------

    [Fact]
    public async Task ASearchSendsDiscloseExecutionOnlyWhenSetAndDecodesExecution()
    {
        var handler = new CannedHandler(DisclosedSearch);
        await using var client = RestOver(handler);
        await client.Retrieval.SearchAsync("q", "docs");
        var result = await client.Retrieval.SearchAsync(
            new SearchRequest { Query = "q", ShapeRef = "docs", DiscloseExecution = true });
        Assert.DoesNotContain("disclose_execution", handler.Bodies[0]);
        Assert.Contains("\"disclose_execution\":true", handler.Bodies[1]);

        var e = result.Execution;
        Assert.NotNull(e);
        Assert.Equal((1u, "postgresql", "ts_rank"), (e.Version, e.Engine, e.LexicalDomain));
        Assert.Equal((12ul, true), (e.Legs[0].Fetched, e.Legs[0].Exhausted));
        // null is "not measured": unknown, never zero.
        Assert.Null(e.Legs[1].Fetched);
        Assert.Null(e.Legs[1].Exhausted);
        Assert.Equal("fused_order", e.Stages[0].Fallback);
        Assert.Equal(4ul, e.Stages[0].PolicyRevision);
    }

    [Fact]
    public async Task AV14SearchWithoutExecutionDecodesToNull()
    {
        await using var client = RestOver(new CannedHandler(LegacySearch));
        var result = await client.Retrieval.SearchAsync("q", "docs");
        Assert.Null(result.Execution);
        Assert.DoesNotContain(
            "execution", JsonSerializer.Serialize(result, MunariumJsonContext.Default.SearchResult));
    }

    [Fact]
    public void GrpcSearchMapsExecutionAndSendsTheFlag()
    {
        Assert.False(GrpcTransport.ToPbSearchRequest(
            new SearchRequest { Query = "q", ShapeRef = "docs" }).DiscloseExecution);
        Assert.True(GrpcTransport.ToPbSearchRequest(
            new SearchRequest { Query = "q", ShapeRef = "docs", DiscloseExecution = true })
            .DiscloseExecution);

        var envelope = new Mmp.V1.ProvenanceEnvelope { IndexVersion = "iv-1", EventWatermark = 9 };
        var resp = new Mmp.V1.HybridSearchResponse
        {
            Envelope = envelope,
            Execution = new Mmp.V1.SearchExecution
            {
                Version = 1, Engine = "postgresql", LexicalDomain = "ts_rank",
                Legs =
                {
                    new Mmp.V1.SearchLegExecution { Leg = "lexical", CandidateLimit = 50, Fetched = 0 },
                    new Mmp.V1.SearchLegExecution { Leg = "vector" },
                },
            },
        };
        var e = GrpcTransport.ToSearchResult(resp).Execution;
        Assert.NotNull(e);
        // A measured zero stays zero; an unset optional stays unknown.
        Assert.Equal((50ul, 0ul), (e.Legs[0].CandidateLimit!.Value, e.Legs[0].Fetched!.Value));
        Assert.Null(e.Legs[0].Exhausted);
        Assert.Null(e.Legs[1].CandidateLimit);
        Assert.Empty(e.Stages);
        Assert.Null(GrpcTransport.ToSearchResult(
            new Mmp.V1.HybridSearchResponse { Envelope = envelope }).Execution);
    }

    // -- R11: retryable / retry_action ---------------------------------------

    private static string ProblemJson(string slug, int status, string extra = "") =>
        $$"""
        {"type": "https://munarium.ioka.io/problems/{{slug}}", "title": "{{slug}}",
         "status": {{status}}, "detail": "d"{{extra}}}
        """;

    [Fact]
    public void AProblemCarriesRetryGuidance()
    {
        var e = Assert.IsType<HeadConflictException>(Errors.FromProblem(
            409,
            ProblemJson("head-conflict", 409,
                ",\"expected\":1,\"actual\":2,\"retryable\":false,\"retry_action\":\"reread\""),
            null));
        Assert.Equal(2ul, e.Actual);
        Assert.Equal(false, e.Retryable);
        Assert.Equal("reread", e.RetryAction);
    }

    [Fact]
    public void AV14ProblemCarriesNoRetryGuidance()
    {
        var e = Errors.FromProblem(400, ProblemJson("invalid-input", 400), null);
        Assert.IsType<InvalidInputException>(e);
        Assert.Null(e.Retryable);
        Assert.Null(e.RetryAction);
    }

    [Fact]
    public void GrpcErrorInfoMetadataCarriesRetryGuidance()
    {
        static RpcException Rpc(string reason, params (string Key, string Value)[] md)
        {
            var info = new Google.Rpc.ErrorInfo { Reason = reason, Domain = "mmp.ioka.io" };
            foreach (var (k, v) in md) info.Metadata[k] = v;
            var status = new Google.Rpc.Status { Code = 14, Message = "m" };
            status.Details.Add(Any.Pack(info));
            return new RpcException(
                new Grpc.Core.Status(StatusCode.Unavailable, "m"),
                new Metadata { { "grpc-status-details-bin", status.ToByteArray() } });
        }

        var e = Errors.FromRpc(Rpc("overloaded", ("retryable", "true"), ("retry_action", "repeat")));
        Assert.IsType<OverloadedException>(e);
        Assert.Equal(true, e.Retryable);
        Assert.Equal("repeat", e.RetryAction);

        var bare = Errors.FromRpc(Rpc("overloaded"));
        Assert.Null(bare.Retryable);
        Assert.Null(bare.RetryAction);
    }
}
