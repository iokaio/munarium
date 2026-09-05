// SPDX-License-Identifier: Apache-2.0
// Offline pins for the max_tokens budget pair (GET/POST /v1/max-tokens):
// the flattened GET decode, the POST body carrying all eight fields and
// decoding the same shape back, the client-side round-trip, the typed
// invalid-input / forbidden decodes (sent once — a 400 is never retried),
// and the gRPC transport refusing honestly (a faulted task, never a silent
// no-op). No server: a canned HttpMessageHandler stands in for the peer.

using System.Net;
using System.Text;
using System.Text.Json;
using Xunit;

namespace Ioka.Munarium.Client.Tests;

public class MaxTokensTests
{
    /// <summary>A canned HTTP peer: answers every request with the one
    /// response and keeps what it was sent. The request is captured
    /// field-by-field because the transport disposes each attempt's
    /// HttpRequestMessage (content included) once the send returns.</summary>
    private sealed class CannedHandler(HttpStatusCode status, string body, string mediaType)
        : HttpMessageHandler
    {
        public int Calls { get; private set; }
        public HttpMethod? Method { get; private set; }
        public Uri? Uri { get; private set; }
        public string? Bearer { get; private set; }
        public string? Uid { get; private set; }
        public string? ContentType { get; private set; }
        public string? RequestBody { get; private set; }

        protected override async Task<HttpResponseMessage> SendAsync(
            HttpRequestMessage request, CancellationToken cancellationToken)
        {
            Calls++;
            Method = request.Method;
            Uri = request.RequestUri;
            Bearer = request.Headers.Authorization?.Parameter;
            Uid = request.Headers.TryGetValues("X-Munarium-Uid", out var uids)
                ? string.Join(",", uids)
                : null;
            ContentType = request.Content?.Headers.ContentType?.MediaType;
            RequestBody = request.Content is null
                ? null
                : await request.Content.ReadAsStringAsync(cancellationToken);
            return new HttpResponseMessage(status)
            {
                Content = new StringContent(body, Encoding.UTF8, mediaType),
            };
        }
    }

    private static MunariumClient RestOver(CannedHandler handler) =>
        MunariumClient.Rest(
            new MunariumClientOptions
            {
                Endpoint = "http://127.0.0.1:1", Token = "rwtoken", Uid = "operator-1",
            },
            new HttpClient(handler));

    private static string ProblemJson(string slug, int status, string detail) =>
        $$"""
        {"type": "https://munarium.ioka.io/problems/{{slug}}", "title": "{{slug}}",
         "status": {{status}}, "detail": "{{detail}}"}
        """;

    private const string TenantWire = """
        {"turn_completion":4096,"query_expansion":128,"complete_default":2048,
         "healthai_probe":256,"hierarchy_classifier":16,"hierarchy_intent":320,
         "runbook_advisory":1024,"authoring_assist":4096,
         "source":"tenant","updated_at":"2026-09-02T10:15:30Z"}
        """;

    private static readonly string[] WireFields =
    [
        "turn_completion", "query_expansion", "complete_default", "healthai_probe",
        "hierarchy_classifier", "hierarchy_intent", "runbook_advisory", "authoring_assist",
    ];

    private static readonly MaxTokensBudgets Budgets = new()
    {
        TurnCompletion = 4096, QueryExpansion = 128, CompleteDefault = 2048,
        HealthaiProbe = 256, HierarchyClassifier = 16, HierarchyIntent = 320,
        RunbookAdvisory = 1024, AuthoringAssist = 4096,
    };

    private static void AssertTenantSet(MaxTokensResponse r)
    {
        Assert.Equal(4096u, r.TurnCompletion);
        Assert.Equal(128u, r.QueryExpansion);
        Assert.Equal(2048u, r.CompleteDefault);
        Assert.Equal(256u, r.HealthaiProbe);
        Assert.Equal(16u, r.HierarchyClassifier);
        Assert.Equal(320u, r.HierarchyIntent);
        Assert.Equal(1024u, r.RunbookAdvisory);
        Assert.Equal(4096u, r.AuthoringAssist);
        Assert.Equal("tenant", r.Source);
        Assert.Equal("2026-09-02T10:15:30Z", r.UpdatedAt);
    }

    // -- GET ----------------------------------------------------------------

    [Fact]
    public async Task GetDecodesTheFlattenedBudgetsAndTheirSource()
    {
        var handler = new CannedHandler(HttpStatusCode.OK, TenantWire, "application/json");
        await using var client = RestOver(handler);

        var r = await client.Providers.GetMaxTokensAsync();

        Assert.Equal(HttpMethod.Get, handler.Method);
        Assert.Equal("/v1/max-tokens", handler.Uri!.AbsolutePath);
        Assert.Equal("rwtoken", handler.Bearer);
        Assert.Equal("operator-1", handler.Uid);
        Assert.Null(handler.RequestBody);
        AssertTenantSet(r);
    }

    [Fact]
    public async Task AnEnvironmentSourcedSetHasNoUpdatedAt()
    {
        // `updated_at` is ABSENT (not null) while the process defaults apply.
        const string wire = """
            {"turn_completion":2048,"query_expansion":256,"complete_default":1024,
             "healthai_probe":512,"hierarchy_classifier":32,"hierarchy_intent":480,
             "runbook_advisory":2048,"authoring_assist":8192,"source":"environment"}
            """;
        var handler = new CannedHandler(HttpStatusCode.OK, wire, "application/json");
        await using var client = RestOver(handler);

        var r = await client.Providers.GetMaxTokensAsync();

        Assert.Equal("environment", r.Source);
        Assert.Null(r.UpdatedAt);
        Assert.Equal(2048u, r.TurnCompletion);
        Assert.Equal(8192u, r.AuthoringAssist);
    }

    // -- POST ---------------------------------------------------------------

    [Fact]
    public async Task ReplaceSendsAllEightFieldsAndDecodesTheSameShapeBack()
    {
        var handler = new CannedHandler(HttpStatusCode.OK, TenantWire, "application/json");
        await using var client = RestOver(handler);

        var r = await client.Providers.ReplaceMaxTokensAsync(Budgets);

        Assert.Equal(HttpMethod.Post, handler.Method);
        Assert.Equal("/v1/max-tokens", handler.Uri!.AbsolutePath);
        Assert.Equal("application/json", handler.ContentType);
        Assert.Equal("rwtoken", handler.Bearer);
        Assert.Equal("operator-1", handler.Uid);

        // The body is exactly the eight required fields, by their wire names —
        // a missing one is a server-side invalid-input, so the client never
        // omits any, and it adds nothing the server would have to ignore.
        using var sent = JsonDocument.Parse(handler.RequestBody!);
        var props = sent.RootElement.EnumerateObject()
            .ToDictionary(p => p.Name, p => p.Value.GetUInt32());
        Assert.Equal(WireFields.Order(), props.Keys.Order());
        Assert.Equal(4096u, props["turn_completion"]);
        Assert.Equal(128u, props["query_expansion"]);
        Assert.Equal(2048u, props["complete_default"]);
        Assert.Equal(256u, props["healthai_probe"]);
        Assert.Equal(16u, props["hierarchy_classifier"]);
        Assert.Equal(320u, props["hierarchy_intent"]);
        Assert.Equal(1024u, props["runbook_advisory"]);
        Assert.Equal(4096u, props["authoring_assist"]);

        AssertTenantSet(r);
    }

    [Fact]
    public void AGetResponseRoundTripsIntoAReplaceBody()
    {
        // The server flattens the response so a GET body is a POST body
        // (extra keys ignored). Client-side the typed twin of that property
        // is ToBudgets(): the eight fields and nothing else — never `source`
        // or `updated_at`.
        var got = JsonSerializer.Deserialize(
            TenantWire, MunariumJsonContext.Default.MaxTokensResponse)!;
        var body = got.ToBudgets() with { TurnCompletion = 8192 };
        Assert.Equal(Budgets with { TurnCompletion = 8192 }, body);

        var json = JsonSerializer.Serialize(body, MunariumJsonContext.Default.MaxTokensBudgets);
        using var doc = JsonDocument.Parse(json);
        Assert.Equal(
            WireFields.Order(),
            doc.RootElement.EnumerateObject().Select(p => p.Name).Order());
        Assert.DoesNotContain("source", json);
        Assert.DoesNotContain("updated_at", json);
    }

    // -- errors: typed, and a 400 is sent once --------------------------------

    [Fact]
    public async Task AnOutOfRangeFieldIsTheTypedInvalidInput()
    {
        var handler = new CannedHandler(
            HttpStatusCode.BadRequest,
            ProblemJson("invalid-input", 400, "turn_completion must be within 256..=16384"),
            "application/problem+json");
        await using var client = RestOver(handler);

        var e = await Assert.ThrowsAsync<InvalidInputException>(() =>
            client.Providers.ReplaceMaxTokensAsync(Budgets with { TurnCompletion = 1 }));

        Assert.Equal("invalid-input", e.Slug);
        Assert.Contains("256..=16384", e.Message);
        Assert.False(e.Transient);
        // A whole-set replace is the send-once write class: one attempt, and
        // a 400 could never earn a second anyway.
        Assert.Equal(1, handler.Calls);
    }

    [Fact]
    public async Task ANonRwRoleIsTheTypedForbidden()
    {
        var handler = new CannedHandler(
            HttpStatusCode.Forbidden,
            ProblemJson("forbidden", 403, "static rw role required"),
            "application/problem+json");
        await using var client = RestOver(handler);

        var e = await Assert.ThrowsAsync<ForbiddenException>(() =>
            client.Providers.ReplaceMaxTokensAsync(Budgets));

        Assert.Equal("forbidden", e.Slug);
        Assert.Equal(1, handler.Calls);
    }

    // -- gRPC: no twin, refused honestly --------------------------------------

    [Fact]
    public async Task TheGrpcTransportFaultsBothAtAwaitLikeItsRestOnlySiblings()
    {
        // No server at this endpoint: nothing may be sent, and building the
        // task must not throw — the await does, with the typed gap.
        await using var client = MunariumClient.Grpc(
            new MunariumClientOptions { Endpoint = "http://127.0.0.1:1", Token = "t" });
        var get = client.Providers.GetMaxTokensAsync();
        var replace = client.Providers.ReplaceMaxTokensAsync(Budgets);

        var e1 = await Assert.ThrowsAsync<UnsupportedTransportException>(() => get);
        var e2 = await Assert.ThrowsAsync<UnsupportedTransportException>(() => replace);
        Assert.Contains("/v1/max-tokens", e1.Message);
        Assert.Contains("/v1/max-tokens", e2.Message);
    }
}
