// SPDX-License-Identifier: Apache-2.0
using Grpc.Net.Client;
using Xunit;

namespace Ioka.Munarium.Client.Tests;

public class ServerApiTransportTests
{
    private sealed class Handler : HttpMessageHandler
    {
        public bool Disposed { get; private set; }
        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken token)
            => Task.FromResult(new HttpResponseMessage(System.Net.HttpStatusCode.OK));
        protected override void Dispose(bool disposing) { Disposed = true; base.Dispose(disposing); }
    }

    [Fact]
    public async Task BorrowedHttpClientRemainsUsable()
    {
        var handler = new Handler();
        using var http = new HttpClient(handler);
        var client = ServerApiClient.Rest(new MunariumClientOptions { Endpoint = "https://fixture.invalid" }, http);
        await client.DisposeAsync();
        Assert.False(handler.Disposed);
        using var response = await http.GetAsync("https://fixture.invalid");
        Assert.True(response.IsSuccessStatusCode);
    }

    [Fact]
    public async Task BorrowedGrpcChannelRemainsUsable()
    {
        using var channel = GrpcChannel.ForAddress("https://fixture.invalid");
        var client = ServerApiClient.Grpc(new MunariumClientOptions { Endpoint = "https://fixture.invalid" }, channel);
        await client.DisposeAsync();
        Assert.NotNull(channel.CreateCallInvoker());
    }
}
