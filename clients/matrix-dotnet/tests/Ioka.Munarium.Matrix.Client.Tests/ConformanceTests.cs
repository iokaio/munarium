// SPDX-License-Identifier: Apache-2.0
// Conformance for the .NET Matrix client.
//
// Two tiers, deliberately:
//
// * Offline — the response SHAPES this client claims to understand, driven
//   through a stub HttpMessageHandler. These run everywhere and are what catch
//   a field rename in the API.
// * Live — against a real Matrix when MUNARIUM_MATRIX_TEST_URL is set, skipped
//   OUT LOUD otherwise. A skip that prints nothing is indistinguishable from a
//   pass, which is how a tier stays vacuously green for a phase.
//
// There is no mock of Matrix's SEMANTICS here. A client test that asserted
// what a refusal means would be asserting its own opinion; these assert only
// that the client reads what the service says.

using System.Net;
using System.Reflection;
using Xunit;

namespace Ioka.Munarium.Matrix.Client.Tests;

public class ConformanceTests
{
    [Fact]
    public async Task VersionReportsLockstepFromTheServicesOwnWord()
    {
        var (client, stub) = Stub.Over(_ => Stub.Ok("""
            {"version": "0.1.0", "contract_version": "0.1.0", "role": "all",
             "server_version": "0.5.0", "target_server_version": "0.5.0",
             "server_compatibility": "exact"}
            """));
        using var _ = client;

        var version = await client.VersionAsync();

        Assert.True(version.LockstepOk);
        Assert.Equal("all", version.Role);
        Assert.Equal("/version", stub.Seen[0].RequestUri!.AbsolutePath);
    }

    [Fact]
    public async Task ANonExactLockstepIsNotOk()
    {
        using var client = Stub.ClientOver(_ => Stub.Ok("""
            {"version": "0.1.0", "contract_version": "0.1.0", "role": "all",
             "server_compatibility": "minor_behind"}
            """));

        // The distinction the whole lockstep exists for: an id minted against
        // a server that does not agree on the contract may not resolve there.
        Assert.False((await client.VersionAsync()).LockstepOk);
    }

    [Fact]
    public async Task ApplyPostsYamlAsYamlAndReportsUnchanged()
    {
        var (client, stub) = Stub.Over(request =>
        {
            Assert.Equal("text/yaml", request.Content!.Headers.ContentType!.MediaType);
            return Stub.Ok("""
                {"asset_ref": "crm@2", "kind": "DataSource", "unchanged": true, "findings": []}
                """);
        });
        using var _ = client;

        var outcome = await client.ApplyAsync("kind: DataSource\n");

        Assert.Equal("crm@2", outcome.AssetRef);
        // Re-applying identical bytes is ordinary GitOps, not an error.
        Assert.True(outcome.Unchanged);
        Assert.Contains("kind: DataSource", stub.Bodies[0], StringComparison.Ordinal);
    }

    [Fact]
    public async Task ARefusalSurfacesItsClassAndCodeRatherThanProse()
    {
        using var client = Stub.ClientOver(_ => Stub.Json(
            HttpStatusCode.TooManyRequests,
            """
            {"type": "https://munarium.ioka.io/problems/matrix/budget-exceeded",
             "title": "exhausted", "status": 429,
             "detail": "source 'crm' has 0 of 2 unit(s) left this hour",
             "refusal": {"class": "exhausted", "code": "budget_exceeded",
                         "message": "out of budget", "retry_after_seconds": 900}}
            """));

        var error = await Assert.ThrowsAsync<MatrixException>(
            () => client.VerifyAsync("open-pipeline-by-region"));

        Assert.Equal("budget_exceeded", error.Code);
        Assert.Equal("exhausted", error.RefusalClass);
        // A caller deciding whether to retry must not be parsing prose to do it.
        Assert.True(error.Retryable);
        // And it must not be guessing at the pacing either, when the refusal said.
        Assert.Equal(TimeSpan.FromMinutes(15), error.RetryAfter);
    }

    [Fact]
    public async Task ADenialIsNotRetryable()
    {
        using var client = Stub.ClientOver(_ => Stub.Json(
            HttpStatusCode.Forbidden,
            """
            {"type": "https://munarium.ioka.io/problems/matrix/policy-denied",
             "title": "denied", "status": 403,
             "detail": "role 'ro' cannot execute commands",
             "refusal": {"class": "denied", "code": "policy_denied", "message": "no"}}
            """));

        var error = await Assert.ThrowsAsync<MatrixException>(() => client.SyncAsync("crm"));

        // Repeating a request against a door locked on purpose is not a retry.
        Assert.False(error.Retryable);
        Assert.Equal("policy_denied", error.Code);
    }

    [Fact]
    public async Task AnAssetInvalidRefusalIsAnArrayAndMustNotCrashTheErrorPath()
    {
        // POST /v1/assets puts the FINDINGS ARRAY in `refusal` when an asset
        // fails validation — the same member that carries the typed refusal
        // object everywhere else. Reading it as an object without checking
        // turns the most ordinary failure this client sees into a crash
        // inside the error path, which is the worst place for one.
        using var client = Stub.ClientOver(_ => Stub.Json(
            HttpStatusCode.UnprocessableEntity,
            """
            {"type": "https://munarium.ioka.io/problems/matrix/asset-invalid",
             "title": "asset failed validation", "status": 422,
             "detail": "1 error finding(s); nothing was applied",
             "refusal": [{"code": "source.host-missing", "path": "spec.connection",
                          "message": "no host"}]}
            """));

        var error = await Assert.ThrowsAsync<MatrixException>(
            () => client.ApplyAsync("kind: DataSource\n"));

        Assert.Equal(422, error.Status);
        Assert.Contains("nothing was applied", error.Message, StringComparison.Ordinal);
        // No refusal OBJECT was sent, so there is no class to report — and
        // reporting none is the honest answer, not a default.
        Assert.Null(error.RefusalClass);
        Assert.False(error.Retryable);
    }

    [Fact]
    public async Task VerifyReportsWhichQuestionMoved()
    {
        using var client = Stub.ClientOver(_ => Stub.Ok("""
            {"contract": "open-pipeline-by-region@3", "passed": 0, "failed": 1,
             "questions": [{"question": "What is the open pipeline by region?",
                            "ok": false, "rows": 1,
                            "failures": ["expected 3 rows, got 1"]}]}
            """));

        var outcome = await client.VerifyAsync("open-pipeline-by-region");

        // The call succeeded and the CONTRACT did not: different things.
        Assert.Equal(1, outcome.Failed);
        Assert.Equal(new[] { "expected 3 rows, got 1" }, outcome.Questions[0].Failures);
    }

    [Fact]
    public async Task ValidateReportsTheServicesOwnValidFlagAndNotAnEmptyFindingsList()
    {
        // Three validator codes are advisory. An asset that produces one is
        // valid and will apply, so a client that decided validity by counting
        // findings would refuse three healthy assets.
        using var client = Stub.ClientOver(_ => Stub.Ok("""
            {"valid": true,
             "findings": [{"code": "mapping.authority-inert", "path": "spec.authority",
                           "message": "no authority scope matches any observed property"}]}
            """));

        var outcome = await client.ValidateAsync("kind: ClaimMapping\n");

        Assert.True(outcome.Valid);
        Assert.Single(outcome.Findings);
        Assert.Equal("mapping.authority-inert", outcome.Findings[0].Code);
    }

    [Fact]
    public async Task ListAssetsAsksForAllVersionsUnderTheNameTheServiceReads()
    {
        // The service deserializes `all_versions`. Any other spelling is
        // ignored in silence, which is the worst possible failure for a flag:
        // the call succeeds and answers the other question.
        var (client, stub) = Stub.Over(_ => Stub.Ok("""
            {"assets": [{"asset_ref": "crm@2", "name": "crm", "version": 2,
                         "kind": "DataSource", "created_at": "2026-08-29T00:00:00Z"}]}
            """));
        using var _ = client;

        var assets = await client.ListAssetsAsync("datasources", allVersions: true);

        Assert.Equal("all_versions=true", stub.Seen[0].RequestUri!.Query.TrimStart('?'));
        Assert.Equal(2, assets[0].Version);
    }

    [Fact]
    public async Task VerifyViewFallsBackFromMetricViewToDataViewOnANotFound()
    {
        var (client, stub) = Stub.Over(request =>
            request.RequestUri!.AbsolutePath.Contains("metricviews", StringComparison.Ordinal)
                ? Stub.Json(HttpStatusCode.NotFound,
                    """
                    {"type": "https://munarium.ioka.io/problems/matrix/not-found",
                     "title": "not found", "status": 404, "detail": "MetricView 'x'"}
                    """)
                : Stub.Ok("""
                    {"contract": "pipeline-by-region@2", "passed": 1, "failed": 0,
                     "fingerprint": "sha256:abc", "questions": []}
                    """));
        using var _ = client;

        var outcome = await client.VerifyViewAsync("pipeline-by-region");

        Assert.Equal("sha256:abc", outcome.Fingerprint);
        Assert.Equal(
            new[]
            {
                "/v1/metricviews/pipeline-by-region/verify",
                "/v1/dataviews/pipeline-by-region/verify",
            },
            stub.Seen.Select(r => r.RequestUri!.AbsolutePath).ToList());
    }

    [Fact]
    public async Task VerifyViewAlsoFallsBackWhenTheMissingViewRefusesNotCovered()
    {
        // This is the answer the deployed service actually gives: the verify
        // route loads the asset through the loader, which turns a store miss
        // into a `not_covered` refusal, and `not_covered` maps to 422. A
        // fallback keyed on 404 alone reads correctly and never fires, so
        // every native data view would be reported as an unknown metric view.
        var (client, stub) = Stub.Over(request =>
            request.RequestUri!.AbsolutePath.Contains("metricviews", StringComparison.Ordinal)
                ? Stub.Json(HttpStatusCode.UnprocessableEntity,
                    """
                    {"type": "https://munarium.ioka.io/problems/matrix/not-covered",
                     "title": "not_covered", "status": 422,
                     "detail": "no MetricView named 'pipeline-by-region' is registered",
                     "refusal": {"class": "not_covered", "code": "not_covered",
                                 "message": "no MetricView named 'pipeline-by-region' is registered"}}
                    """)
                : Stub.Ok("""
                    {"contract": "pipeline-by-region@2", "passed": 1, "failed": 0,
                     "fingerprint": "sha256:native", "questions": []}
                    """));
        using var _ = client;

        var outcome = await client.VerifyViewAsync("pipeline-by-region");

        Assert.Equal("sha256:native", outcome.Fingerprint);
        Assert.Equal(2, stub.Seen.Count);
    }

    [Fact]
    public async Task VerifyViewReportsTheMetricViewErrorWhenTheViewExistsNowhere()
    {
        using var client = Stub.ClientOver(request => Stub.Json(
            HttpStatusCode.UnprocessableEntity,
            """
            {"type": "https://munarium.ioka.io/problems/matrix/not-covered",
             "title": "not_covered", "status": 422,
             "detail": "no ROUTE view named 'ghost' is registered",
             "refusal": {"class": "not_covered", "code": "not_covered", "message": "none"}}
            """.Replace(
                "ROUTE",
                request.RequestUri!.AbsolutePath.Contains("metricviews", StringComparison.Ordinal)
                    ? "metric"
                    : "data",
                StringComparison.Ordinal)));

        var error = await Assert.ThrowsAsync<MatrixException>(() => client.VerifyViewAsync("ghost"));

        // The first answer is the one the caller asked for.
        Assert.Contains("no metric view", error.Message, StringComparison.Ordinal);
    }

    [Fact]
    public async Task PromotionStatusReadsTheGatesWhereTheServicePutsThem()
    {
        // identity_precision and value_conformance live inside `gates`, not at
        // the top level. Reading them at the top level compiles, decodes, and
        // reports null for every mapping that has ever run — a promotion gate
        // that silently reads as "no measurement" is worse than one that fails.
        using var client = Stub.ClientOver(_ => Stub.Ok("""
            {"mapping": "captable@1", "mode": "authoritative", "promoted": true,
             "authority_scopes": 2,
             "gates": {"identity_precision": 1.0, "value_conformance": 0.995,
                       "min_identity_precision": 0.95, "min_value_conformance": 0.99,
                       "observations": 10, "run_id": "mrn-1"},
             "latest_run": {"run_id": "mrn-1", "state": "ok", "observations": 10,
                            "discrepancies": 7, "ambiguous": 0, "findings_filed": 7,
                            "proposals": 1}}
            """));

        var status = await client.PromotionStatusAsync("captable");

        Assert.True(status.Promoted);
        Assert.Equal(1.0, status.IdentityPrecision!.Value);
        Assert.Equal(0.995, status.ValueConformance!.Value);
        Assert.Equal(0.95, status.Gates!.MinIdentityPrecision);
        Assert.Equal("ok", status.LatestRun!.Value.GetProperty("state").GetString());
    }

    [Fact]
    public async Task ATransportFailureIsUnavailableNotABareException()
    {
        using var client = Stub.ClientOver(
            _ => throw new HttpRequestException("connection refused"));

        var error = await Assert.ThrowsAsync<MatrixException>(() => client.HealthdataAsync());

        Assert.Equal("unavailable", error.RefusalClass);
        Assert.True(error.Retryable);
    }

    [Fact]
    public async Task HealthzAnswersFalseRatherThanThrowing()
    {
        using var client = Stub.ClientOver(
            _ => throw new HttpRequestException("connection refused"));

        // A health check that throws forces every caller to write the
        // try/catch this one has already written.
        Assert.False(await client.HealthzAsync());
    }

    [Fact]
    public void NoMemberOnThisClientSealsEvidence()
    {
        // The design decision, asserted rather than described: an SDK that
        // could seal would invite an application to assert provenance it
        // cannot vouch for. A manifest is a statement about work the SEALER
        // did. Evidence is READ through the server's client, resolving an
        // [evidence/<id>#<row>] citation.
        var offenders = typeof(MatrixClient).Assembly
            .GetExportedTypes()
            .SelectMany(t => t
                .GetMembers(BindingFlags.Public | BindingFlags.Instance
                    | BindingFlags.Static | BindingFlags.DeclaredOnly)
                .Select(m => $"{t.Name}.{m.Name}")
                .Append(t.Name))
            .Where(name =>
                name.Contains("seal", StringComparison.OrdinalIgnoreCase)
                || name.Contains("evidence", StringComparison.OrdinalIgnoreCase))
            .ToList();

        Assert.Empty(offenders);
    }

    [Fact]
    public void NoMemberOnThisClientTakesSql()
    {
        // Queries are pre-declared contracts and views, executed by name.
        // Nothing on this surface takes a statement, and the absence is
        // structural rather than a convention somebody has to remember.
        var offenders = typeof(MatrixClient).Assembly
            .GetExportedTypes()
            .SelectMany(t => t
                .GetMembers(BindingFlags.Public | BindingFlags.Instance
                    | BindingFlags.Static | BindingFlags.DeclaredOnly)
                .Select(m => $"{t.Name}.{m.Name}")
                .Append(t.Name))
            .Where(name =>
                name.Contains("sql", StringComparison.OrdinalIgnoreCase)
                || name.Contains("query", StringComparison.OrdinalIgnoreCase)
                || name.Contains("statement", StringComparison.OrdinalIgnoreCase))
            .ToList();

        Assert.Empty(offenders);
    }

    [Fact]
    public void EveryPublicCallIsAsyncAndCancellable()
    {
        // No sync-over-async: a .Result on a captured synchronization context
        // blocks the thread the continuation needs, and a client that offers
        // that overload will have it called. The safe number of them is zero,
        // and this is what keeps it there.
        var calls = typeof(MatrixClient)
            .GetMethods(BindingFlags.Public | BindingFlags.Instance | BindingFlags.DeclaredOnly)
            // Property getters and setters are methods too; they are not
            // calls a consumer makes.
            .Where(m => !m.IsSpecialName && m.Name != nameof(MatrixClient.Dispose))
            .ToList();

        Assert.NotEmpty(calls);
        foreach (var call in calls)
        {
            Assert.EndsWith("Async", call.Name, StringComparison.Ordinal);
            Assert.True(
                typeof(Task).IsAssignableFrom(call.ReturnType),
                $"{call.Name} does not return a Task");
            Assert.Contains(
                call.GetParameters(),
                p => p.ParameterType == typeof(CancellationToken));
        }
    }
}
