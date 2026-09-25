// SPDX-License-Identifier: Apache-2.0
using System.Reflection;
using System.Text;
using System.Text.Json;
using Google.Protobuf;
using Xunit;

namespace Ioka.Munarium.Client.Tests;

public class WireCompatibilityTests
{
    public static TheoryData<ulong> Boundaries => new()
    {
        0, (1UL << 53) - 1, 1UL << 53, (1UL << 53) + 1,
        long.MaxValue, 1UL << 63, ulong.MaxValue,
    };

    [Theory]
    [MemberData(nameof(Boundaries))]
    public void RestAndProtobufPreserveExactSequences(ulong value)
    {
        var json = $$"""{"head_seq":{{value}},"future":{"n":9223372036854775807} }""";
        var head = JsonSerializer.Deserialize(json, MunariumJsonContext.Default.HeadResponse)!;
        Assert.Equal(value, head.HeadSeq);
        var encoded = JsonSerializer.Serialize(head, MunariumJsonContext.Default.HeadResponse);
        Assert.Equal(value, JsonDocument.Parse(encoded).RootElement.GetProperty("head_seq").GetUInt64());
        var pb = new Mmp.V1.Claim { Seq = value };
        // Unknown varint field 127 is ignored by the typed SDK conversion.
        var decoded = Mmp.V1.Claim.Parser.ParseFrom([.. pb.ToByteArray(), 0xf8, 0x07, 0x01]);
        Assert.Equal(value, ConvertClaim(decoded).Seq);

        var request = ApiRequest.Json(new Dictionary<string, object?>
        {
            ["expected_head"] = value, ["optional"] = null,
        });
        var proto = (Mmp.V1.ServerApiRequest)typeof(ServerApiClient)
            .GetMethod("Proto", BindingFlags.Static | BindingFlags.NonPublic)!.Invoke(null, [request])!;
        var response = new ApiResponse(200, "application/json", proto.Body.ToByteArray()).Json();
        Assert.Equal(value, response.GetProperty("expected_head").GetUInt64());
        Assert.Equal(JsonValueKind.Null, response.GetProperty("optional").ValueKind);
        Assert.False(response.TryGetProperty("omitted", out _));
        var error = Assert.IsType<HeadConflictException>(Errors.FromProblem(409,
            $$"""{"type":"https://munarium.ioka.io/problems/head-conflict","expected":{{value}},"actual":{{value}} }""",
            null));
        Assert.Equal(value, error.Expected);
        Assert.Equal(value, error.Actual);
    }

    [Theory]
    [InlineData("-1")]
    [InlineData("18446744073709551616")]
    [InlineData("1.5")]
    [InlineData("9007199254740992.0")]
    [InlineData("true")]
    [InlineData("\"12\"")]
    [InlineData("\"bad\"")]
    [InlineData("null")]
    public void RestRejectsInvalidUnsignedSequences(string value)
    {
        Assert.Throws<JsonException>(() => JsonSerializer.Deserialize(
            $$"""{"head_seq":{{value}}}""", MunariumJsonContext.Default.HeadResponse));
    }

    [Theory]
    [InlineData(0)]
    [InlineData(999)]
    public void UnknownGovernanceEnumsStayConservative(int tag)
    {
        var claim = ConvertClaim(new Mmp.V1.Claim
        {
            Status = (Mmp.V1.ClaimStatus)tag,
            Provenance = (Mmp.V1.Provenance)tag,
        });
        Assert.Equal("disputed", claim.Status);
        Assert.Equal("emergent", claim.Provenance);
        var finding = (GateFinding)typeof(GrpcTransport)
            .GetMethod("ToFinding", BindingFlags.Static | BindingFlags.NonPublic)!
            .Invoke(null, [new Mmp.V1.GateFinding { Severity = (Mmp.V1.Severity)tag }])!;
        Assert.Equal("block", finding.Severity);
    }

    [Fact]
    public void KnownProvenanceAndTypedNullOmissionKeepTheirMeaning()
    {
        Assert.Equal("witnessed", ConvertClaim(new Mmp.V1.Claim
        {
            Provenance = Mmp.V1.Provenance.Witnessed,
        }).Provenance);
        var input = new ClaimInput { Subject = "fixture", Key = "key", Value = "value" };
        var omitted = JsonSerializer.Serialize(input, MunariumJsonContext.Default.ClaimInput);
        var explicitNull = JsonSerializer.Serialize(input with { Origin = null },
            MunariumJsonContext.Default.ClaimInput);
        Assert.Equal(omitted, explicitNull);
        Assert.False(JsonDocument.Parse(explicitNull).RootElement.TryGetProperty("origin", out _));
    }

    [Theory]
    [InlineData("")]
    [InlineData(",\"usage\":null")]
    [InlineData(",\"usage\":{\"quality\":\"future_quality\",\"input_tokens\":null}")]
    public void PreviousAndAdditiveCurrentCompletionShapesRemainReadable(string additions)
    {
        // Synthetic 1.1 baseline and additive current/future fields, not old binaries.
        var raw = "{\"text\":\"fictional answer\",\"stop_reason\":\"stop\","
            + "\"input_tokens\":9007199254740993,\"output_tokens\":1,"
            + "\"provider\":\"fixture\",\"model\":\"fixture\","
            + "\"future_optional\":{\"opaque_id\":\"id:with/slash+suffix\"}" + additions + "}";
        var typed = JsonSerializer.Deserialize(raw, MunariumJsonContext.Default.CompleteResult)!;
        Assert.Equal(9007199254740993UL, typed.InputTokens);
        Assert.Null(typed.InvocationEventId);
        var response = new ApiResponse(200, "application/json", Encoding.UTF8.GetBytes(raw)).Json();
        Assert.Equal("id:with/slash+suffix", response.GetProperty("future_optional")
            .GetProperty("opaque_id").GetString());
        Assert.Equal(additions.Length != 0, response.TryGetProperty("usage", out _));
    }

    private static Claim ConvertClaim(Mmp.V1.Claim claim) => (Claim)typeof(GrpcTransport)
        .GetMethod("ToClaim", BindingFlags.Static | BindingFlags.NonPublic)!
        .Invoke(null, [claim])!;

    [Theory]
    [InlineData("\"accepted\"", false)]
    [InlineData("\"disputed\"", true)]
    [InlineData("\"future_status\"", true)]
    [InlineData("\"\"", true)]
    [InlineData("null", true)]
    public void OnlyExplicitAcceptedRestStatusCanReadAsUndisputed(string status, bool disputed)
    {
        var claim = "{\"id\":\"fixture\",\"version_id\":\"v\",\"seq\":1,\"claim_type\":\"fact\","
            + "\"subject\":\"fixture\",\"key\":\"key\",\"value\":\"value\",\"normalized_text\":\"fixture\","
            + "\"provenance\":\"witnessed\",\"status\":" + status + "}";
        var outcome = JsonSerializer.Deserialize("{\"claim\":" + claim + ",\"findings\":[],\"head_seq\":1}",
            MunariumJsonContext.Default.ClaimOutcome)!;
        Assert.Equal(disputed, outcome.IsDisputed);
        var events = JsonSerializer.Deserialize("{\"claims\":[" + claim + "],\"findings\":[],\"head_seq\":1}",
            MunariumJsonContext.Default.EventsOutcome)!;
        Assert.Equal(disputed, events.IsDisputed);
    }
}
