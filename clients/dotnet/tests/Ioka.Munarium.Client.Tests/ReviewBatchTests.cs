// SPDX-License-Identifier: Apache-2.0
// Offline pins for the 2026-08-24 code-review fix batch: the command
// retry table, ingest result pairing (surplus + shortfall), base64
// whitespace agreement across transports, the SSE overflow contract
// (completed events survive an oversized trailing chunk; the bound is
// exact; the parser poisons), the Problem.status fallback, the proto3
// empty-list sentinels, the gRPC single-file ingest parity, faulted (not
// synchronous) Unsupported failures, the streaming JSON body, and the
// client-owned HttpClient timeout contract.

using System.Text;
using System.Text.Json;
using Google.Protobuf;
using Xunit;

namespace Ioka.Munarium.Client.Tests;

public class ReviewBatchTests
{
    // -- retry table --------------------------------------------------------

    [Fact]
    public void CommandRetryTableIsExactlyOverloadedOrProvablyUndelivered()
    {
        Assert.True(Retry.Retryable(RetryClass.Command, new OverloadedException("shed")));
        Assert.True(Retry.Retryable(
            RetryClass.Command, new MunariumTransportException("refused", mayHaveReachedServer: false)));
        // A gateway 502/504 is transient, but the command may still be
        // executing upstream — never re-sent.
        Assert.False(Retry.Retryable(RetryClass.Command, new UnexpectedServerException("bad gateway", 502)));
        Assert.False(Retry.Retryable(RetryClass.Command, new UnexpectedServerException("gateway timeout", 504)));
        Assert.False(Retry.Retryable(
            RetryClass.Command, new MunariumTransportException("reset", mayHaveReachedServer: true)));
        // Reads still retry the gateway case.
        Assert.True(Retry.Retryable(RetryClass.Read, new UnexpectedServerException("bad gateway", 502)));
        Assert.True(Retry.Retryable(RetryClass.Read, new UnexpectedServerException("gateway timeout", 504)));
        Assert.True(Retry.Retryable(RetryClass.Read, new MunariumTransportException("reset")));
        Assert.False(Retry.Retryable(RetryClass.Write, new OverloadedException("shed")));
    }

    // -- ingest pairing -----------------------------------------------------

    private static IngestFile File(string name, string b64 = "") => new()
    {
        Filename = name, MediaType = "text/markdown", ContentBase64 = b64,
    };

    [Fact]
    public void SurplusServerResultsAreATypedErrorNotSilentlyDropped()
    {
        var e = Assert.Throws<UnexpectedServerException>(() =>
            GrpcTransport.SpliceIngestResults(
                [File("a.md")], [null],
                [new IngestResult { Filename = "a.md" }, new IngestResult { Filename = "ghost.md" }]));
        Assert.Contains("2 results", e.Message);
        Assert.Contains("1 files", e.Message);
    }

    [Fact]
    public void Base64WithWhitespaceDecodesOnBothPaths()
    {
        // The REST server trims; the gRPC client must agree.
        Assert.Equal("hello"u8.ToArray(), Convert.FromBase64String("aGVsbG8=\n"));
        Assert.Equal("hello"u8.ToArray(), ByteString.FromBase64("aGVsbG8=\n").ToByteArray());
        var (sent, err) = GrpcTransport.ToPbIngestFile(File("h.md", "aGVs\r\nbG8=\n"));
        Assert.Null(err);
        Assert.Equal("hello"u8.ToArray(), sent!.Content.ToByteArray());
    }

    [Fact]
    public void SingleFileBadBase64ThrowsInvalidInputLikeRest()
    {
        var invalid = GrpcTransport.SingleFileLocalError(File("x.md", "%%not-base64%%"));
        Assert.NotNull(invalid);
        Assert.Contains("base64", invalid.Message);
        Assert.Null(GrpcTransport.SingleFileLocalError(File("ok.md", "aGVsbG8=")));
    }

    [Fact]
    public void ExplicitEmptyCollectionsIsAProto3SentinelOnGrpc()
    {
        var file = File("x.md", "aGVsbG8=") with { Collections = [] };
        var e = Assert.Throws<InvalidInputException>(() => GrpcTransport.ToPbIngestFile(file));
        Assert.Contains("collections", e.Message);
        Assert.Contains("REST", e.Message);
        // null (omitted) and a populated list both ship.
        Assert.NotNull(GrpcTransport.ToPbIngestFile(File("x.md", "aGVsbG8=")).Sent);
        Assert.NotNull(GrpcTransport.ToPbIngestFile(file with { Collections = ["c"] }).Sent);
    }

    [Fact]
    public async Task ExplicitEmptyRunbookRefsIsAProto3SentinelOnGrpc()
    {
        // Rejected before any RPC — no server at this endpoint.
        await using var client = MunariumClient.Grpc(
            new MunariumClientOptions { Endpoint = "http://127.0.0.1:1", Token = "t" });
        var e = await Assert.ThrowsAsync<InvalidInputException>(() =>
            client.Tokens.MintAsync("u", 0, ["query"], runbookRefs: []));
        Assert.Contains("runbook_refs", e.Message);
        Validation.RejectEmptyList<string>("any", null);
        Validation.RejectEmptyList("any", ["x"]);
    }

    // -- gRPC Unsupported surfaces at await, never synchronously ------------

    [Fact]
    public async Task GrpcRestOnlySurfaceFaultsTheTaskInsteadOfThrowingSynchronously()
    {
        await using var client = MunariumClient.Grpc(
            new MunariumClientOptions { Endpoint = "http://127.0.0.1:1", Token = "t" });
        // Building the task must not throw...
        var usage = client.Reports.UsageAsync();
        var findings = client.Query.FindingsAsync("v");
        var bulk = client.Ingest.BulkOpenAsync([]);
        var stream = client.Sessions.TurnStreamAsync("s", new TurnRequest { Query = "q" });
        // ...the await does.
        await Assert.ThrowsAsync<UnsupportedTransportException>(() => usage);
        await Assert.ThrowsAsync<UnsupportedTransportException>(() => findings);
        await Assert.ThrowsAsync<UnsupportedTransportException>(() => bulk);
        await Assert.ThrowsAsync<UnsupportedTransportException>(async () =>
        {
            await foreach (var _ in stream)
            {
            }
        });
    }

    // -- SSE overflow contract ---------------------------------------------

    [Fact]
    public void DonePlusOversizedTrailingBytesInOneChunkStillYieldsTheDone()
    {
        var done = "event: done\ndata: {\"ok\":true}\n\n"u8;
        var chunk = new byte[done.Length + SseParser.MaxEventBytes + 1];
        done.CopyTo(chunk);
        Array.Fill(chunk, (byte)'x', done.Length, chunk.Length - done.Length);

        var p = new SseParser();
        var events = p.Push(chunk);
        var ev = Assert.Single(events);
        Assert.Equal("done", ev.Event);
        Assert.Equal("{\"ok\":true}", ev.Data);
        // Poisoned: the NEXT push throws, whatever it carries.
        Assert.Throws<SseOverflowException>(() => p.Push("\n"u8));
    }

    [Fact]
    public void OverflowBoundIsExactAndThrowsImmediatelyWhenNothingCompleted()
    {
        var p = new SseParser();
        var exactly = new byte[SseParser.MaxEventBytes];
        Array.Fill(exactly, (byte)'y');
        Assert.Empty(p.Push(exactly)); // at the cap: buffered
        Assert.Throws<SseOverflowException>(() => p.Push("z"u8)); // one past: refused
        Assert.Throws<SseOverflowException>(() => p.Push("\n\n"u8)); // and stays poisoned
    }

    [Fact]
    public void NothingCompletedReturnsTheSharedEmptyList()
    {
        var p = new SseParser();
        Assert.Same(p.Push("event: pro"u8), p.Push("gress\n"u8));
        Assert.Empty(p.Push(": keep-alive\n\n"u8));
    }

    [Fact]
    public void FieldValueSplitsOnTheFirstColonOnly()
    {
        var p = new SseParser();
        var ev = Assert.Single(p.Push("data: {\"t\":\"a:b\"}\ndata:no-space\n\n"u8));
        Assert.Equal("{\"t\":\"a:b\"}\nno-space", ev.Data);
    }

    [Fact]
    public async Task IdleWatchdogIsDisarmedWhileTheConsumerHandlesAnEvent()
    {
        using var idle = new CancellationTokenSource();
        var value = await RestTransport.GuardIdleAsync(
            idle,
            _ => Task.FromResult(7),
            CancellationToken.None,
            TimeSpan.FromMilliseconds(20));
        Assert.Equal(7, value);

        // TurnStreamAsync may now yield this value to application code. A
        // slow consumer is not wire silence; no timer should remain armed
        // until the iterator asks the network for another read.
        await Task.Delay(80);
        Assert.False(idle.IsCancellationRequested);
    }

    // -- Problem.status fallback -------------------------------------------

    [Fact]
    public void StreamErrorUsesTheBodyStatusWhenTheCarrierHasNone()
    {
        var body = """{"type":"about:blank","title":"Bad Gateway","status":502,"detail":"upstream"}""";
        var e = Assert.IsType<UnexpectedServerException>(Errors.FromProblem(null, body, null));
        Assert.Equal(502, e.Status);
        Assert.True(e.Transient);
        // The transport status wins when present.
        var e2 = Assert.IsType<UnexpectedServerException>(Errors.FromProblem(500, body, null));
        Assert.Equal(500, e2.Status);
        // A registry slug decodes identically with or without a carrier status.
        var over = """{"type":"https://munarium.ioka.io/problems/overloaded","status":503,"detail":"shed"}""";
        Assert.IsType<OverloadedException>(Errors.FromProblem(null, over, null));
        Assert.IsType<UnexpectedServerException>(Errors.FromProblem(null, "not json", null));
    }

    // -- streaming JSON body -----------------------------------------------

    [Fact]
    public async Task JsonStreamContentSerializesWithoutAKnownLength()
    {
        var body = new IngestBatchBody { Files = [File("a.md", "aGVsbG8=")] };
        var content = new JsonStreamContent<IngestBatchBody>(
            body, MunariumJsonContext.Default.IngestBatchBody);
        Assert.Null(content.Headers.ContentLength); // no whole-body buffer
        Assert.Equal("application/json", content.Headers.ContentType!.MediaType);
        var bytes = await content.ReadAsByteArrayAsync();
        var expected = JsonSerializer.SerializeToUtf8Bytes(
            body, MunariumJsonContext.Default.IngestBatchBody);
        Assert.Equal(expected, bytes);
        Assert.Contains("\"a.md\"", Encoding.UTF8.GetString(bytes));
    }

    // -- HttpClient timeout contract ---------------------------------------

    [Fact]
    public async Task ClientOwnedHttpClientHasNoTimeoutAndACallerSuppliedOneIsNeverMutated()
    {
        var options = new MunariumClientOptions { Endpoint = "http://127.0.0.1:1", Token = "t" };
        await using (var owned = new RestTransport(options, null))
        {
            // Our per-attempt deadline is the only cap: the exempt sends
            // (turns/bulk/stream) must not be cut by HttpClient.Timeout.
            Assert.Equal(Timeout.InfiniteTimeSpan, owned.Http.Timeout);
        }
        using var theirs = new HttpClient { Timeout = TimeSpan.FromSeconds(7) };
        await using (var supplied = new RestTransport(options, theirs))
        {
            Assert.Same(theirs, supplied.Http);
            Assert.Equal(TimeSpan.FromSeconds(7), theirs.Timeout);
            Assert.Null(theirs.DefaultRequestHeaders.Authorization);
        }
        Assert.Equal(TimeSpan.FromSeconds(7), theirs.Timeout); // dispose did not touch it
    }
}
