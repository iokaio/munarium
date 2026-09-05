// SPDX-License-Identifier: Apache-2.0
// Offline unit tests: error decoding, path encoding, retry-class semantics.

using Xunit;

namespace Ioka.Munarium.Client.Tests;

public class ErrorTests
{
    private static string ProblemJson(string slug, int status, string extra = "") =>
        $$"""
        {"type": "https://munarium.ioka.io/problems/{{slug}}", "title": "{{slug}}",
         "status": {{status}}, "detail": "{{slug}} detail"{{extra}}}
        """;

    [Fact]
    public void HeadConflictDecodesExtensions()
    {
        var e = Errors.FromProblem(
            409, ProblemJson("head-conflict", 409, ", \"expected\": 3, \"actual\": 7"), null);
        var conflict = Assert.IsType<HeadConflictException>(e);
        Assert.Equal(3UL, conflict.Expected);
        Assert.Equal(7UL, conflict.Actual);
    }

    [Fact]
    public void PolicyRejectionCarriesFindings()
    {
        var e = Errors.FromProblem(
            422,
            ProblemJson(
                "policy-rejection", 422,
                ", \"gate_findings\": [{\"rule_id\": \"gate.ledger-conflict\", " +
                "\"severity\": \"block\", \"message\": \"m\"}]"),
            null);
        var rejection = Assert.IsType<PolicyRejectionException>(e);
        Assert.Single(rejection.Findings);
        Assert.Equal("gate.ledger-conflict", rejection.Findings[0].RuleId);
        Assert.Equal(1UL, rejection.FindingsTotal);
        Assert.False(rejection.FindingsTruncated);
    }

    [Theory]
    [InlineData("unauthenticated", 401)]
    [InlineData("forbidden", 403)]
    public void AuthSlugsMapToTypedErrors(string slug, int status)
    {
        var e = Errors.FromProblem(status, ProblemJson(slug, status), null);
        Assert.Equal(slug, e.Slug);
    }

    [Fact]
    public void NotFoundUsesKindAndId()
    {
        var e = Errors.FromProblem(
            404, ProblemJson("not-found", 404, ", \"kind\": \"claim\", \"id\": \"c-1\""), null);
        var notFound = Assert.IsType<MunariumNotFoundException>(e);
        Assert.Equal("claim", notFound.Kind);
        Assert.Equal("c-1", notFound.Id);
        Assert.Contains("not-found detail", notFound.Message);
    }

    [Fact]
    public void OverloadedIsTypedAndTransient()
    {
        var e = Errors.FromProblem(503, ProblemJson("overloaded", 503), null);
        Assert.IsType<OverloadedException>(e);
        Assert.True(e.Transient);
        // an unknown slug on a 5xx gateway status is also transient
        var raw = Errors.FromProblem(503, "{\"type\": \"x\", \"status\": 503}", null);
        Assert.IsType<UnexpectedServerException>(raw);
        Assert.True(raw.Transient);
    }

    [Fact]
    public void RateLimitedCarriesRetryAfter()
    {
        var e = Errors.FromProblem(
            429, ProblemJson("rate-limited", 429), TimeSpan.FromSeconds(7));
        var limited = Assert.IsType<RateLimitedException>(e);
        Assert.Equal(TimeSpan.FromSeconds(7), limited.RetryAfter);
    }

    [Fact]
    public void NonJsonBodyIsUnexpected()
    {
        var e = Errors.FromProblem(500, "<html>boom</html>", null);
        Assert.IsType<UnexpectedServerException>(e);
    }

    [Fact]
    public void SegPercentEncodesPathSegments()
    {
        Assert.Equal("phase%2F1.reveal%3Fx", RestTransport.Seg("phase/1.reveal?x"));
        Assert.Equal("support-tickets%401", RestTransport.Seg("support-tickets@1"));
    }

    [Fact]
    public void DisputedIsSuccessNotError()
    {
        var outcome = new ClaimOutcome
        {
            Claim = new Claim
            {
                Id = "c-1", VersionId = "v-1", Seq = 2, ClaimType = "fact",
                Subject = "hero", Key = "eyes", Value = "blue",
                NormalizedText = "hero.eyes=blue", Status = "disputed",
                Provenance = "witnessed",
            },
            Findings =
            [
                new GateFinding
                {
                    RuleId = "gate.ledger-conflict", Severity = "block", Message = "m",
                },
            ],
            HeadSeq = 2,
        };
        Assert.True(outcome.IsDisputed);
    }
}
