// SPDX-License-Identifier: Apache-2.0
// A transport that is a function, so a test states the exact bytes the
// service would have sent. The Python tier uses httpx.MockTransport for the
// same reason; .NET's seam is one level lower — an HttpMessageHandler — which
// costs a small class and buys the identical property: no socket, no server,
// no timing, and a response body that is a literal in the test that asserts
// on it.

using System.Net;

namespace Ioka.Munarium.Matrix.Client.Tests;

internal sealed class StubHandler(Func<HttpRequestMessage, HttpResponseMessage> handler)
    : HttpMessageHandler
{
    /// <summary>Every request the client actually sent, in order. Asserting
    /// on this is how a test proves a route or a query parameter, which is a
    /// different claim from "the client decoded the answer".</summary>
    internal List<HttpRequestMessage> Seen { get; } = [];

    internal List<string> Bodies { get; } = [];

    protected override async Task<HttpResponseMessage> SendAsync(
        HttpRequestMessage request, CancellationToken cancellationToken)
    {
        Seen.Add(request);
        Bodies.Add(request.Content is null
            ? ""
            : await request.Content.ReadAsStringAsync(cancellationToken).ConfigureAwait(false));
        return handler(request);
    }
}

internal static class Stub
{
    /// <summary>A client wired to a stub handler, with the handler handed
    /// back so a test can read what was sent.</summary>
    internal static (MatrixClient Client, StubHandler Handler) Over(
        Func<HttpRequestMessage, HttpResponseMessage> handler)
    {
        var stub = new StubHandler(handler);
        var client = new MatrixClient(
            new MatrixClientOptions { Endpoint = "http://matrix.test", Token = "t" },
            new HttpClient(stub));
        return (client, stub);
    }

    internal static MatrixClient ClientOver(Func<HttpRequestMessage, HttpResponseMessage> handler)
        => Over(handler).Client;

    internal static HttpResponseMessage Json(HttpStatusCode status, string body) =>
        new(status)
        {
            Content = new StringContent(body, System.Text.Encoding.UTF8, "application/json"),
        };

    internal static HttpResponseMessage Ok(string body) => Json(HttpStatusCode.OK, body);
}
