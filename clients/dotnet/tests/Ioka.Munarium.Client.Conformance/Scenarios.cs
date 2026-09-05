// SPDX-License-Identifier: Apache-2.0
// The server's 7 MMP conformance scenarios, ported to the black-box API
// surface (same names as server/conformance/src/lib.rs so CI output is
// comparable across languages), run as a [Theory] over both transports.
//
// Port deviations (the same the Python suite makes):
// - composer.budget-degradation and digests.rebuilt-under-pin assert via the
//   server's ComposeContext (the Rust suite composes client-side through
//   munarium-core, which .NET does not link);
// - gates.chronology-certain-only is a pure-kernel check with no API surface
//   — SCENARIOS.md marks it kernel-only; no client port carries it, skipped here.
//
// Requires a running server:
//   MUNARIUM_REST_URL=http://127.0.0.1:18080 MUNARIUM_GRPC_URL=http://127.0.0.1:15051
//   MUNARIUM_TOKEN=devtoken dotnet test

using Xunit;

namespace Ioka.Munarium.Client.Conformance;

public static class Clients
{
    public static MunariumClient Make(string transport)
    {
        var (envVar, factory) = transport switch
        {
            "rest" => ("MUNARIUM_REST_URL",
                (Func<MunariumClientOptions, MunariumClient>)(o => MunariumClient.Rest(o))),
            "grpc" => ("MUNARIUM_GRPC_URL",
                (Func<MunariumClientOptions, MunariumClient>)(o => MunariumClient.Grpc(o))),
            _ => throw new ArgumentOutOfRangeException(nameof(transport)),
        };
        var endpoint = Environment.GetEnvironmentVariable(envVar);
        Skip.If(endpoint is null, $"{envVar} not set");
        var token = Environment.GetEnvironmentVariable("MUNARIUM_TOKEN") ?? "devtoken";
        return factory(new MunariumClientOptions { Endpoint = endpoint!, Token = token, Uid = "conformance" });
    }
}

public class Scenarios
{
    public static TheoryData<string> Transports => new("rest", "grpc");

    [SkippableTheory]
    [MemberData(nameof(Transports))]
    public async Task LedgerAppendHeadConflict(string transport)
    {
        await using var client = Clients.Make(transport);
        var v = await client.Commands.CreateVersionAsync();
        var outcome = await client.Commands.ProposeClaimAsync(v, new ClaimInput
        {
            Subject = "hero", Key = "eyes", Value = "green", ExpectedHead = 0,
        });
        Assert.Equal(1UL, outcome.Claim.Seq);

        await Assert.ThrowsAsync<HeadConflictException>(() =>
            client.Commands.ProposeClaimAsync(v, new ClaimInput
            {
                Subject = "hero", Key = "home", Value = "harbor", ExpectedHead = 0,
            }));
        Assert.Equal(1UL, await client.Query.HeadAsync(v));
    }

    [SkippableTheory]
    [MemberData(nameof(Transports))]
    public async Task LedgerOriginRoundTrips(string transport)
    {
        // S-4.1: a connector claim's origin survives the round trip on both
        // transports; a claim proposed without one reads back without one.
        await using var client = Clients.Make(transport);
        var v = await client.Commands.CreateVersionAsync();
        var origin = new ClaimOrigin
        {
            Kind = "connector", SourceId = "crm", MappingVersion = "captable-holdings@1",
            RowKey = "holder_id=43", EventPosition = "lsn/0/1A2B",
            ObservedAt = "2026-08-28T09:15:00Z", EvidenceId = "ev-batch-0001",
        };
        var outcome = await client.Commands.ProposeClaimAsync(v, new ClaimInput
        {
            Subject = "shareholder.43", Key = "shares", Value = "90500", Origin = origin,
        });
        Assert.NotNull(outcome.Claim.Origin);
        Assert.Equal(origin, outcome.Claim.Origin);

        var read = await client.Query.GetClaimAsync(outcome.Claim.Id);
        Assert.Equal("holder_id=43", read.Claim.Origin?.RowKey);

        var plain = await client.Commands.ProposeClaimAsync(v, new ClaimInput
        {
            Subject = "shareholder.43", Key = "class", Value = "A",
        });
        Assert.Null(plain.Claim.Origin);
    }

    [SkippableTheory]
    [MemberData(nameof(Transports))]
    public async Task LedgerSupersessionPin(string transport)
    {
        await using var client = Clients.Make(transport);
        var v1 = await client.Commands.CreateVersionAsync();
        var original = await client.Commands.ProposeClaimAsync(v1, new ClaimInput
        {
            Subject = "hero", Key = "eyes", Value = "green",
        });
        await client.Commands.ProposeClaimAsync(v1, new ClaimInput
        {
            Subject = "hero", Key = "home", Value = "harbor",
        });

        // correction in a CHILD version supersedes across the lineage
        var v2 = await client.Commands.CreateVersionAsync(parentVersionId: v1);
        await client.Commands.ProposeClaimAsync(v2, new ClaimInput
        {
            Subject = "hero", Key = "eyes", Value = "blue",
            ClaimType = "correction", SupersedesId = original.Claim.Id,
        });

        var headFacts = (await client.Query.FactsAsync(v2)).Facts;
        var eyes = headFacts.Where(f => f.Key == "eyes").ToArray();
        Assert.Single(eyes);
        Assert.Equal("blue", eyes[0].Value);

        var pinned = (await client.Query.FactsAsync(v2, asOfSeq: 2)).Facts;
        eyes = pinned.Where(f => f.Key == "eyes").ToArray();
        Assert.Single(eyes);
        Assert.Equal("green", eyes[0].Value); // current AT THE PIN
    }

    [SkippableTheory]
    [MemberData(nameof(Transports))]
    public async Task PinsOnePinBoundsAllStores(string transport)
    {
        await using var client = Clients.Make(transport);
        var v = await client.Commands.CreateVersionAsync();
        await client.Commands.ProposeClaimAsync(v, new ClaimInput
        {
            Subject = "hero", Key = "eyes", Value = "green",
        }); // seq 1
        await client.Commands.OpenPromiseAsync(
            v, "reveal", "setup", "open the letter", "ch1", "ch3"); // seq 2 (registration advances the clock)
        await client.Commands.LockAnchorAsync(v, "hero", "eyes", "green", "ch1"); // seq 3
        await client.Commands.RecordCountsAsync(v, "flashback", "ch1", 1, budget: 2); // seq 4
        await client.Commands.ProposeClaimAsync(v, new ClaimInput
        {
            Subject = "hero", Key = "home", Value = "harbor",
        }); // seq 5
        await client.Commands.FulfillPromiseAsync(v, "reveal"); // fulfilled_seq 6

        // pin at 1: only claim 1 exists — nothing registered later may leak back
        Assert.Empty(await client.Query.AnchorsAsync(v, asOfSeq: 1));
        Assert.Empty(await client.Query.CountersAsync(v, asOfSeq: 1));
        Assert.Empty(await client.Query.PromisesAsync(v, asOfSeq: 1));

        // pin at 2: promise registered and OPEN; anchor and counter still ahead
        Assert.Empty(await client.Query.AnchorsAsync(v, asOfSeq: 2));
        Assert.Empty(await client.Query.CountersAsync(v, asOfSeq: 2));
        var promises = await client.Query.PromisesAsync(v, asOfSeq: 2);
        Assert.Single(promises);
        Assert.Equal("open", promises[0].Status); // post-pin fulfillment reads OPEN

        // head: everything visible, promise fulfilled
        Assert.NotEmpty(await client.Query.AnchorsAsync(v));
        Assert.Equal("fulfilled", (await client.Query.PromisesAsync(v))[0].Status);
    }

    [SkippableTheory]
    [MemberData(nameof(Transports))]
    public async Task GatesBlockRecordsDisputed(string transport)
    {
        await using var client = Clients.Make(transport);
        var v = await client.Commands.CreateVersionAsync();
        await client.Commands.ProposeClaimAsync(v, new ClaimInput
        {
            Subject = "hero", Key = "eyes", Value = "green",
        });

        // the command path IS the governance path: the conflicting plain
        // claim comes back SUCCESS with status disputed + the gate finding.
        var outcome = await client.Commands.ProposeClaimAsync(v, new ClaimInput
        {
            Subject = "hero", Key = "eyes", Value = "blue", ScopePath = "ch2",
        });
        Assert.True(outcome.IsDisputed, "conflicting plain claim must be recorded disputed");
        Assert.Contains(outcome.Findings,
            f => f.RuleId == "gate.ledger-conflict" && f.Severity == "block");

        var accepted = (await client.Query.FactsAsync(v)).Facts;
        var eyes = accepted.Where(f => f.Key == "eyes").ToArray();
        Assert.Single(eyes);
        Assert.Equal("green", eyes[0].Value); // canon unchanged

        var disputed = (await client.Query.FactsAsync(v, statuses: ["disputed"])).Facts;
        Assert.Contains(disputed, f => f.Value == "blue"); // recorded, not dropped
    }

    [SkippableTheory]
    [MemberData(nameof(Transports))]
    public async Task ComposerBudgetDegradation(string transport)
    {
        await using var client = Clients.Make(transport);
        var v = await client.Commands.CreateVersionAsync();
        for (var i = 1; i <= 20; i++)
        {
            await client.Commands.ProposeClaimAsync(v, new ClaimInput
            {
                Subject = "hero",
                Key = $"k{i}",
                Value = $"value-{i} with prose attached",
                ScopePath = i <= 10 ? "book.ch1" : "book.ch2",
            });
        }
        var full = await client.Query.ComposeContextAsync(v, scope: "book.ch1");
        var budget = full.EstimatedTokens - 20;
        var degraded = await client.Query.ComposeContextAsync(
            v, scope: "book.ch1", budgetTokens: budget);
        Assert.True(degraded.EstimatedTokens <= budget, "budget must hold");
        var factsSection = degraded.Sections.FirstOrDefault(s => s.Title == "Accepted facts");
        var kept = factsSection?.Body.Split('\n').Length ?? 0;
        Assert.Equal(20, kept); // digests degrade BEFORE facts trim
    }

    [SkippableTheory]
    [MemberData(nameof(Transports))]
    public async Task DigestsRebuiltUnderPin(string transport)
    {
        await using var client = Clients.Make(transport);
        var v = await client.Commands.CreateVersionAsync();
        await client.Commands.ProposeClaimAsync(v, new ClaimInput
        {
            Subject = "hero", Key = "eyes", Value = "green", ScopePath = "ch1",
        }); // seq 1
        await client.Commands.ProposeClaimAsync(v, new ClaimInput
        {
            Subject = "hero", Key = "home", Value = "harbor", ScopePath = "ch1",
        }); // seq 2

        // store a HEAD-shaped digest, then pin before seq 2: the stored rung
        // (which mentions "home") must never be served under the pin.
        await client.Commands.UpsertDigestAsync(new Digest
        {
            VersionId = v,
            Tier = 0,
            ScopePath = "ch1",
            Content = "[ch1] hero eyes green; hero home harbor",
            ContentHash = "head-shaped",
            BuiltFromSeq = 2,
        });
        var pinned = await client.Query.ComposeContextAsync(v, asOfSeq: 1);
        Assert.DoesNotContain("home", pinned.Text);
    }

    [SkippableTheory]
    [MemberData(nameof(Transports))]
    public Task GatesChronologyCertainOnly(string transport)
    {
        Skip.If(true,
            "pure-kernel scenario (client-side check_chronology over declarative rules; " +
            "no API surface) — SCENARIOS.md marks it kernel-only and no client port " +
            $"carries it (transport {transport})");
        return Task.CompletedTask;
    }
}
