// SPDX-License-Identifier: Apache-2.0
// Offline unit tests for the C9 catch-up surface: the new slug decodes
// (run-locked's deliberate non-transience, the two invalid-input lifecycle
// slugs), the client-side bulk chunk-cap guard, the gRPC per-item base64
// contract, and the flat TurnProgressEvent decode (unknown stages included).

using System.Text.Json;
using Xunit;

namespace Ioka.Munarium.Client.Tests;

public class PlatformTests
{
    private static string ProblemJson(string slug, int status) =>
        $$"""
        {"type": "https://munarium.ioka.io/problems/{{slug}}", "title": "{{slug}}",
         "status": {{status}}, "detail": "{{slug}} detail"}
        """;

    [Fact]
    public void RunLockedIsTypedButDeliberatelyNotTransient()
    {
        // Before this slug was mapped it decoded as Unexpected — hiding that
        // the request was rejected pre-execution and a later re-run succeeds
        // once the lock clears.
        var e = Errors.FromProblem(409, ProblemJson("run-locked", 409), null);
        var locked = Assert.IsType<RunLockedException>(e);
        Assert.Equal("run-locked", locked.Slug);
        // A run lock is held for a whole run (minutes) — pace yourself, like
        // RateLimited; sub-second auto-retry would be futile churn.
        Assert.False(locked.Transient);
    }

    [Theory]
    [InlineData("session-not-open")]
    [InlineData("authoring-draft-invalid")]
    public void LifecycleSlugsMapToInvalidInput(string slug)
    {
        // Same status-class convention as removal-not-confirmed.
        var e = Errors.FromProblem(409, ProblemJson(slug, 409), null);
        Assert.IsType<InvalidInputException>(e);
        Assert.Contains($"{slug} detail", e.Message);
    }

    [Theory]
    [InlineData("batch", 0)]
    [InlineData("batch", 501)]
    [InlineData("bulk chunk", 501)]
    public void OverCapFileListsAreRejectedLocally(string what, int count)
    {
        // The guard fires before 256 MiB ships to a server that will refuse
        // it — and the message names the calling surface and the cap.
        var e = Assert.Throws<InvalidInputException>(
            () => Validation.CheckBulkFiles(what, count));
        Assert.Contains("500", e.Message);
        Assert.Contains(what, e.Message);
    }

    [Fact]
    public void ExactlyCapSizedFileListsPass()
    {
        Validation.CheckBulkFiles("batch", 1);
        Validation.CheckBulkFiles("bulk chunk", IIngestPlane.BulkMaxFilesPerChunk);
    }

    [Fact]
    public void BadBase64BecomesItsOwnErrorResultNotABatchFailure()
    {
        var good = new IngestFile
        {
            Filename = "a.md", MediaType = "text/markdown",
            ContentBase64 = Convert.ToBase64String("alpha"u8.ToArray()),
        };
        var bad = good with { Filename = "b.md", ContentBase64 = "%%not-base64%%" };

        var (sentGood, errGood) = GrpcTransport.ToPbIngestFile(good);
        Assert.NotNull(sentGood);
        Assert.Null(errGood);
        Assert.Equal("alpha"u8.ToArray(), sentGood.Content.ToByteArray());

        var (sentBad, errBad) = GrpcTransport.ToPbIngestFile(bad);
        Assert.Null(sentBad);
        Assert.NotNull(errBad);
        Assert.Equal("b.md", errBad.Filename);
        Assert.Contains("base64", errBad.Error);
    }

    [Fact]
    public void IngestResultsSpliceBackInInputOrder()
    {
        IngestFile File(string name) => new()
        {
            Filename = name, MediaType = "text/markdown", ContentBase64 = "",
        };
        IngestResult Server(string name) => new() { Filename = name };
        var localError = new IngestResult { Filename = "bad.md", Error = "not base64" };

        // [sent, LOCAL ERROR, sent] + server [r0, r2] → input order held.
        var results = GrpcTransport.SpliceIngestResults(
            [File("a.md"), File("bad.md"), File("c.md")],
            [null, localError, null],
            [Server("a.md"), Server("c.md")]);
        Assert.Equal(["a.md", "bad.md", "c.md"], results.Select(r => r.Filename));
        Assert.Same(localError, results[1]);

        // A short server results array is a typed error naming the starved
        // file — never an index panic.
        var e = Assert.Throws<UnexpectedServerException>(() =>
            GrpcTransport.SpliceIngestResults(
                [File("a.md"), File("c.md")], [null, null], [Server("a.md")]));
        Assert.Contains("c.md", e.Message);
    }

    [Fact]
    public void TurnProgressDecodesEveryStageAndTolieratesUnknownOnes()
    {
        var retrieval = JsonSerializer.Deserialize(
            """{"stage":"retrieval","collection":"docs","hits":3,"skipped":false}""",
            MunariumJsonContext.Default.TurnProgressEvent)!;
        Assert.Equal("retrieval", retrieval.Stage);
        Assert.Equal("docs", retrieval.Collection);
        Assert.Equal(3u, retrieval.Hits);

        var completion = JsonSerializer.Deserialize(
            """{"stage":"completion","attempt":0,"provider":"anthropic","model":"m","input_tokens":10,"output_tokens":5}""",
            MunariumJsonContext.Default.TurnProgressEvent)!;
        Assert.Equal(0u, completion.Attempt);
        Assert.Equal(10UL, completion.InputTokens);

        // A stage this build cannot name still decodes — the Rust client
        // skips undecodable progress, and this shape makes an unknown stage
        // decodable by construction.
        var future = JsonSerializer.Deserialize(
            """{"stage":"quantum-rerank","novel_field":42}""",
            MunariumJsonContext.Default.TurnProgressEvent)!;
        Assert.Equal("quantum-rerank", future.Stage);
    }
}
