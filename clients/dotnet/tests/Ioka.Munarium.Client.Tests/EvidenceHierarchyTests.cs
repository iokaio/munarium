// SPDX-License-Identifier: Apache-2.0
// Offline pins for the S-3.5 evidence-hierarchy surface: the governing
// invariant (a caller who does not use a research profile sees byte-identical
// request and response JSON), the six new SSE stages, and the two new
// management reports.

using System.Text.Json;
using Xunit;

namespace Ioka.Munarium.Client.Tests;

public class EvidenceHierarchyTests
{
    private static TurnProgressEvent Progress(string json) =>
        JsonSerializer.Deserialize(json, MunariumJsonContext.Default.TurnProgressEvent)!;

    // -- the governing invariant -------------------------------------------

    [Fact]
    public void ATurnRequestWithoutAResearchProfileGainsNoKey()
    {
        // The whole of S-3.x rests on this: the field is additive on a wire
        // contract every deployed client already speaks, so a legacy caller's
        // request bytes must not move by one character.
        var legacy = new TurnRequest
        {
            Query = "who signed it?",
            TopK = 5,
            Complete = true,
            ModelOverride = new ModelOverride { Tier = "capable" },
        };
        var json = JsonSerializer.Serialize(legacy, MunariumJsonContext.Default.TurnRequest);
        Assert.DoesNotContain("research_profile", json);
        Assert.Equal(
            """{"query":"who signed it?","top_k":5,"complete":true,"model_override":{"tier":"capable"}}""",
            json);

        var profiled = legacy with { ResearchProfile = "diligence" };
        Assert.Contains(
            "\"research_profile\":\"diligence\"",
            JsonSerializer.Serialize(profiled, MunariumJsonContext.Default.TurnRequest));
    }

    [Fact]
    public void ATurnResponseWithoutAHierarchyRoundTripsWithoutTheKey()
    {
        const string wire = """
            {"session_id":"s1","ordinal":1,"collections_searched":["docs"],
             "skipped":[],"hits":[],"envelopes":[]}
            """;
        var result = JsonSerializer.Deserialize(wire, MunariumJsonContext.Default.TurnResult)!;
        Assert.Null(result.Hierarchy);
        Assert.Null(result.Completion);

        var round = JsonSerializer.Serialize(result, MunariumJsonContext.Default.TurnResult);
        Assert.DoesNotContain("hierarchy", round);
        Assert.DoesNotContain("completion", round);
    }

    [Fact]
    public void AHierarchyDecisionDecodesLayerByLayer()
    {
        const string wire = """
            {"session_id":"s1","ordinal":2,"hits":[],
             "hierarchy":{
               "profile":"diligence","intent_kind":"enumerate","intent_explicit":true,
               "layers":[
                 {"layer":"register","role":"controlling","requirement":"required",
                  "block":"complete_table","evidence_id":"ev-1",
                  "supports_completeness":true,"elapsed_ms":42},
                 {"layer":"documents","role":"supporting","requirement":"optional",
                  "block":"refusal","supports_completeness":false,
                  "refusal_code":"evidence-expired","elapsed_ms":7}],
               "completeness_available":true,"disclosed_conflicts":2,
               "conflicts_policy":"disclose"}}
            """;
        var h = JsonSerializer.Deserialize(wire, MunariumJsonContext.Default.TurnResult)!.Hierarchy;
        Assert.NotNull(h);
        Assert.Equal("diligence", h.Profile);
        Assert.True(h.IntentExplicit);
        Assert.Equal(2u, h.DisclosedConflicts);
        Assert.Equal(2, h.Layers.Count);

        // A layer that sealed evidence names it; a layer that refused names
        // why and seals nothing. Both are outcomes, and the turn returned 200.
        Assert.Equal("ev-1", h.Layers[0].EvidenceId);
        Assert.Null(h.Layers[0].RefusalCode);
        Assert.True(h.Layers[0].SupportsCompleteness);
        Assert.Null(h.Layers[1].EvidenceId);
        Assert.Equal("evidence-expired", h.Layers[1].RefusalCode);
        Assert.False(h.Layers[1].SupportsCompleteness);
    }

    // -- the six hierarchy SSE stages --------------------------------------

    [Fact]
    public void TheHierarchyStagesDecodeAndUnknownOnesStillDoNotThrow()
    {
        var profile = Progress(
            """{"stage":"profile","profile":"diligence","layers":["register","documents"],"intent_kind":"enumerate","intent_explicit":false}""");
        Assert.Equal("profile", profile.Stage);
        Assert.Equal(["register", "documents"], profile.Layers!);
        Assert.Equal("enumerate", profile.IntentKind);
        Assert.False(profile.IntentExplicit);

        var start = Progress(
            """{"stage":"layer_start","layer":"register","role":"controlling","requirement":"required"}""");
        Assert.Equal("register", start.Layer);
        Assert.Equal("controlling", start.Role);
        Assert.Equal("required", start.Requirement);

        var source = Progress(
            """{"stage":"layer_source","layer":"register","source":"holdings","provider":"matrix"}""");
        Assert.Equal("holdings", source.Source);
        Assert.Equal("matrix", source.Provider);

        var complete = Progress(
            """{"stage":"layer_complete","layer":"register","block":"complete_table","supports_completeness":true,"refusal_code":null,"elapsed_ms":42}""");
        Assert.Equal("complete_table", complete.Block);
        Assert.True(complete.SupportsCompleteness);
        Assert.Null(complete.RefusalCode);
        Assert.Equal(42UL, complete.ElapsedMs);

        var coverage = Progress(
            """{"stage":"coverage","completeness_available":true,"disclosed_conflicts":3}""");
        Assert.True(coverage.CompletenessAvailable);
        Assert.Equal(3u, coverage.DisclosedConflicts);

        var compose = Progress(
            """{"stage":"compose","layers_used":2,"context_chars":8192,"layers_dropped":["news"]}""");
        Assert.Equal(2u, compose.LayersUsed);
        Assert.Equal(8192u, compose.ContextChars);
        Assert.Equal(["news"], compose.LayersDropped!);

        // Widening the union must not have cost forward-compatibility: a
        // seventh hierarchy stage this build cannot name still decodes.
        Assert.Equal("layer_teleport", Progress("""{"stage":"layer_teleport","hops":9}""").Stage);
    }

    [Fact]
    public void TheVerifyStageIsUnchangedWithoutALayerAndCarriesItWithOne()
    {
        var legacy = Progress(
            """{"stage":"verify","attempt":0,"checks":["quotes"],"violations":1}""");
        Assert.Null(legacy.Layer);
        Assert.Equal(1u, legacy.Violations);

        var scoped = Progress(
            """{"stage":"verify","attempt":1,"checks":["citations"],"violations":0,"layer":"register"}""");
        Assert.Equal("register", scoped.Layer);
    }

    // -- the two new mgmt reports ------------------------------------------

    [Fact]
    public void EvidenceReportDecodesPerProfileAndLayer()
    {
        const string wire = """
            {"window":"24h","hierarchy_turns":40,"legacy_turns":160,
             "completeness_available":33,
             "layers":[{"profile":"diligence","layer":"register","turns":40,
                        "refusals":31,"complete":9,
                        "refusal_codes":["matrix-unavailable","evidence-expired"],
                        "p50_ms":120,"p95_ms":900}]}
            """;
        var report = JsonSerializer.Deserialize(wire, MunariumJsonContext.Default.EvidenceReport)!;
        Assert.Equal("24h", report.Window);
        Assert.Equal(40, report.HierarchyTurns);
        Assert.Equal(160, report.LegacyTurns);
        Assert.Equal(33, report.CompletenessAvailable);

        // The reason this report exists: a layer refusing on 31 of 40 turns
        // is invisible in every other report, because all 40 returned 200.
        var layer = Assert.Single(report.Layers);
        Assert.Equal("register", layer.Layer);
        Assert.Equal(31, layer.Refusals);
        Assert.Equal("matrix-unavailable", layer.RefusalCodes[0]);
        Assert.Equal(900, layer.P95Ms);
    }

    [Fact]
    public void MatrixReportSeparatesUnwiredFromWiredAndFailing()
    {
        var unwired = JsonSerializer.Deserialize(
            """{"configured":false,"circuit_open":false,"consecutive_failures":0,"data_views":[]}""",
            MunariumJsonContext.Default.MatrixReport)!;
        Assert.False(unwired.Configured);
        Assert.Empty(unwired.DataViews);

        var failing = JsonSerializer.Deserialize(
            """
            {"configured":true,"circuit_open":true,"consecutive_failures":5,
             "data_views":[{"runbook_ref":"diligence@3","name":"holdings",
                            "contract":"holdings_by_company","access_level":2}]}
            """,
            MunariumJsonContext.Default.MatrixReport)!;
        // Neither report is serving Matrix evidence right now, and the two
        // must not read the same: one was never wired, one is broken.
        Assert.True(failing.Configured);
        Assert.True(failing.CircuitOpen);
        Assert.Equal(5UL, failing.ConsecutiveFailures);
        var view = Assert.Single(failing.DataViews);
        Assert.Equal("diligence@3", view.RunbookRef);
        Assert.Equal("holdings_by_company", view.Contract);
        Assert.Equal(2, view.AccessLevel);
    }

    [Fact]
    public async Task TheNewReportsAreRestOnlyAndFaultAtAwaitLikeTheirSiblings()
    {
        await using var client = MunariumClient.Grpc(
            new MunariumClientOptions { Endpoint = "http://127.0.0.1:1", Token = "t" });
        var evidence = client.Reports.EvidenceAsync("7d");
        var matrix = client.Reports.MatrixAsync();
        await Assert.ThrowsAsync<UnsupportedTransportException>(() => evidence);
        await Assert.ThrowsAsync<UnsupportedTransportException>(() => matrix);
    }

    // -- selection / expansion (the 2026-08-29 follow-up) ------------------

    [Fact]
    public void TheSelectionAndExpansionStagesDecodeTheirOwnFields()
    {
        // These two stages have existed server-side since 2026-08-25 and used
        // to decode with only `stage` populated — every field that made them
        // worth emitting was silently dropped.
        var selection = Progress(
            """{"stage":"selection","probed":58,"selected":3,"collections":["letterbooks","narratives","papers"]}""");
        Assert.Equal(58u, selection.Probed);
        Assert.Equal(3u, selection.Selected);
        Assert.Equal(new[] { "letterbooks", "narratives", "papers" }, selection.Collections);

        var expansion = Progress(
            """{"stage":"expansion","provider":"anthropic","model":"claude-haiku-4-5","terms":["vessel","cargo"],"input_tokens":120,"output_tokens":8}""");
        Assert.Equal("anthropic", expansion.Provider);
        Assert.Equal("claude-haiku-4-5", expansion.Model);
        Assert.Equal(new[] { "vessel", "cargo" }, expansion.Terms);
        Assert.Equal(120UL, expansion.InputTokens);
        Assert.Equal(8UL, expansion.OutputTokens);
    }

    [Fact]
    public void AnEmptyExpansionTermListIsNotTheSameAsAnAbsentOne()
    {
        // An empty list means the model was asked and returned nothing usable,
        // so the original query searched alone. A null means no expansion step
        // ran at all. Collapsing them would hide a paid call that bought
        // nothing.
        var ranAndFoundNothing = Progress(
            """{"stage":"expansion","provider":"anthropic","model":"m","terms":[],"input_tokens":90,"output_tokens":3}""");
        Assert.NotNull(ranAndFoundNothing.Terms);
        Assert.Empty(ranAndFoundNothing.Terms!);

        var neverRan = Progress("""{"stage":"retrieval","collection":"c","hits":4,"skipped":false}""");
        Assert.Null(neverRan.Terms);
    }
}
