// SPDX-License-Identifier: Apache-2.0
// The live tier: a real round-trip against a real Matrix, and — when there is
// no Matrix to reach — a skip that SAYS SO.
//
// Why this needs its own attribute. xunit's [Fact(Skip = "...")] is static, so
// the ordinary way to make a test conditional in .NET is an `if` and an early
// `return`, which reports as a PASS. That is precisely the failure mode this
// tier exists to avoid: Matrix's own Postgres conformance tier was vacuously
// green for a whole phase because a setup failure turned into a skip that
// reported as a pass, and the Databricks tier had to be taught to say SKIPPED
// out loud for the same reason. A test that proves nothing must not look like
// a test that proved something.
//
// LiveFact makes the runner record a real SKIP with the reason attached, and
// the module initializer prints the same sentence to stderr so it is visible
// in a plain `dotnet test` run without a verbosity flag.

using System.Runtime.CompilerServices;
using Xunit;

namespace Ioka.Munarium.Matrix.Client.Tests;

internal static class Live
{
    internal const string UrlVar = "MUNARIUM_MATRIX_TEST_URL";
    internal const string TokenVar = "MUNARIUM_MATRIX_TEST_TOKEN";

    internal const string SkipReason =
        "SKIPPED OUT LOUD: set MUNARIUM_MATRIX_TEST_URL to run against a real Matrix";

    internal static string? Url =>
        Environment.GetEnvironmentVariable(UrlVar) is { Length: > 0 } url ? url : null;

    [ModuleInitializer]
    internal static void Announce()
    {
        if (Url is null)
        {
            Console.Error.WriteLine($"[munarium-matrix client tests] {SkipReason}");
        }
    }
}

/// <summary>A fact that runs only against a real Matrix, and is reported as a
/// genuine skip — never a silent pass — when there is none.</summary>
internal sealed class LiveFactAttribute : FactAttribute
{
    public LiveFactAttribute()
    {
        if (Live.Url is null) Skip = Live.SkipReason;
    }
}

public class LiveTests
{
    [LiveFact]
    public async Task LiveVersionAndRegistryRoundTrip()
    {
        using var client = new MatrixClient(
            Live.Url!, Environment.GetEnvironmentVariable(Live.TokenVar));

        var version = await client.VersionAsync();
        Assert.NotEmpty(version.Version);
        Assert.NotEmpty(version.ContractVersion);

        // The registry answers, and a listing is a list even when empty.
        Assert.NotNull(await client.ListAssetsAsync("datasources"));
    }
}
