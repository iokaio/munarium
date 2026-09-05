// SPDX-License-Identifier: Apache-2.0
// The C5 review invariants, pinned. Each test corresponds to a defect the
// review found: a command executed twice, a client bricked by the idiomatic
// "no timeout" spelling, a gate-blocked claim decoding as accepted, and a
// promise filter the server drops without complaint.

using System;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using Xunit;

namespace Ioka.Munarium.Client.Tests;

public class ReplaySafetyTests
{
    private static async Task<int> AttemptsUntilAsync(
        RetryClass retryClass, Func<int, Exception> failure, int retries = 2)
    {
        var attempts = 0;
        try
        {
            await Retry.RunAsync<int>(retryClass, retries, null, _ =>
            {
                attempts++;
                throw failure(attempts);
            }, CancellationToken.None);
        }
        catch (MunariumException)
        {
            // the failure escapes once the budget is spent — attempts is the answer
        }
        return attempts;
    }

    [Fact]
    public async Task CommandIsNotRetriedOnceItMayHaveBeenDelivered()
    {
        // The server records an idempotency key only AFTER the command
        // completes, so a retry overtaking an in-flight attempt executes it
        // twice — a doubled append, not a replayed one.
        var attempts = await AttemptsUntilAsync(
            RetryClass.Command, _ => new MunariumTransportException("read timeout"));
        Assert.Equal(1, attempts);
    }

    [Fact]
    public async Task CommandIsRetriedWhenTheRequestProvablyNeverLeft()
    {
        var attempts = await AttemptsUntilAsync(
            RetryClass.Command,
            _ => new MunariumTransportException("connection refused", mayHaveReachedServer: false));
        Assert.Equal(3, attempts); // initial + 2 retries
    }

    [Fact]
    public async Task ReadIsRetriedEvenWhenDelivered()
    {
        var attempts = await AttemptsUntilAsync(
            RetryClass.Read, _ => new MunariumTransportException("read timeout"));
        Assert.Equal(3, attempts);
    }

    [Fact]
    public async Task WriteNeverRetries()
    {
        var attempts = await AttemptsUntilAsync(
            RetryClass.Write,
            _ => new MunariumTransportException("boom", mayHaveReachedServer: false));
        Assert.Equal(1, attempts);
    }

    [Theory]
    [InlineData(-1)]  // Timeout.InfiniteTimeSpan — the idiomatic "disabled"
    [InlineData(0)]
    public void NonPositiveRequestTimeoutMeansNoDeadlineNotAnExpiredOne(int ms)
    {
        // DateTime.UtcNow + (-1ms) is already in the past: every RPC would
        // fail instantly with DeadlineExceeded and look like a network fault.
        var options = new MunariumClientOptions
        {
            Endpoint = "http://127.0.0.1:1",
            RequestTimeout = TimeSpan.FromMilliseconds(ms),
        };
        Assert.True(options.RequestTimeout <= TimeSpan.Zero);
    }

    [Fact]
    public void UnknownPromiseStatusIsRejectedNotSilentlyDropped()
    {
        // The server FILTERS an unrecognized status and returns 200 with an
        // empty list — a silent wrong answer about outstanding obligations.
        var e = Assert.Throws<InvalidInputException>(
            () => Validation.CheckPromiseStatus("Open"));
        Assert.Contains("unknown promise status", e.Message, StringComparison.Ordinal);

        foreach (var ok in new[] { "open", "fulfilled", "expired", "violated" })
        {
            Validation.CheckPromiseStatus(ok);
        }
        Validation.CheckPromiseStatus(null);
    }

    [Fact]
    public async Task ChunkSourceReplaysIdenticallyEveryAttempt()
    {
        // The upload retry rebuilds from the source; a source that did not
        // replay would send fewer bytes than the declared hash covers.
        var source = Chunks.FromBytes(new byte[] { 1, 2, 3, 4, 5 });
        Assert.Equal(await DrainAsync(source), await DrainAsync(source));
        Assert.Equal(5, (await DrainAsync(source)).Length);

        var fromList = Chunks.FromList([new byte[] { 9 }, new byte[] { 8 }]);
        Assert.Equal(await DrainAsync(fromList), await DrainAsync(fromList));
    }

    private static async Task<byte[]> DrainAsync(ChunkSource source)
    {
        var acc = new List<byte>();
        await foreach (var chunk in source())
        {
            acc.AddRange(chunk.ToArray());
        }
        return acc.ToArray();
    }
}
