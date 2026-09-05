# Ioka.Munarium.Client (.NET)

Official .NET client for munarium-server: the full ten-plane surface
(`Commands`, `Query`, `Ingest`, `Retrieval`, `Runbooks`, `Providers`,
`Sessions`, `Tokens`, `Reports`, `Authoring`). net10.0, async-only
(`CancellationToken` everywhere), both transports, typed exceptions, the
head-conflict write loop built in. See the
[clients front door](../README.md) for the invariants, the transport-gap
ledger, and guides.

## Install

```xml
<PackageReference Include="Ioka.Munarium.Client" Version="1.0.0" />
```

The gRPC stubs compile at build time from the normative protos under
`server/proto/mmp/v1/` (Grpc.Tools — zero drift, nothing committed).

## Use

```csharp
using Ioka.Munarium.Client;

// REST — pass an IHttpClientFactory client if you manage pooling
// (a caller-supplied HttpClient is never mutated) …
await using var client = MunariumClient.Rest(new MunariumClientOptions
    { Endpoint = "http://127.0.0.1:8080", Token = "devtoken", Uid = "user-1" });
// … or direct gRPC (https:// enables TLS)
await using var grpc = MunariumClient.Grpc(new MunariumClientOptions
    { Endpoint = "http://127.0.0.1:50051", Token = "devtoken", Uid = "user-1" });

var v = await client.Commands.CreateVersionAsync();

// Disputed is SUCCESS — the governance record, not an error.
var outcome = await client.Commands.ProposeClaimAsync(v, new ClaimInput
{
    Subject = "hero", Key = "eyes", Value = "blue",
});
if (outcome.IsDisputed)
{
    foreach (var f in outcome.Findings) Console.WriteLine($"{f.RuleId}: {f.Message}");
}

// The write loop: expected_head + fresh idempotency key per attempt.
outcome = await client.ProposeClaimWithRetryAsync(v, head => new ClaimInput
{
    Subject = "hero", Key = "home", Value = "harbor",
});

// One pin bounds all stores.
var pinned = await client.Query.FactsAsync(v, asOfSeq: 1);
```

`Uid` is the acting end-user id (audit attribution), required by the server's
default posture (`MUNARIUM_REQUIRE_UID=true`) — omit it and every call draws the
typed `uid-required` error.

The platform surface is idiomatic .NET: `Sessions.TurnStreamAsync(...)` is
an `IAsyncEnumerable<TurnStreamEvent>` over the SSE turn (progress events,
then exactly one `Done`; no overall deadline, a 60 s idle watchdog over the
server's 15 s heartbeats — and because enumeration defers, a pre-stream
refusal surfaces at the first `MoveNextAsync`). Unary turns are
deadline-exempt and never auto-retried (they spend provider tokens a client
abort cannot stop); bulk upload sessions ride
`Ingest.BulkOpen/BulkChunk/BulkCompleteAsync`; `Tokens`/`Reports`/`Authoring`
cover the management plane (mint with a mgmt-role bearer).

**Research profiles (S-3.5).** `TurnRequest.ResearchProfile` runs a turn
through a named evidence hierarchy and `TurnResult.Hierarchy` reports what
each layer produced — including the layers that refused, on a turn that
still returned 200. Leave it null and nothing changes: the key is omitted
from the request, no `hierarchy` key comes back, and the SSE stage sequence
is the one it has always been. The streaming plane's six hierarchy stages
(`profile`, `layer_start`, `layer_source`, `layer_complete`, `coverage`,
`compose`) ride the same flat `TurnProgressEvent`, so an unknown stage still
decodes. Operators read the aggregate through `Reports.EvidenceAsync` — the
one view that shows a layer quietly refusing — and `Reports.MatrixAsync`,
which distinguishes a Matrix plane that is not wired from one that is wired
and failing.

**Timeouts and a caller-supplied `HttpClient`.** `RequestTimeout` (30 s) is
enforced by this client per attempt, and the paid/large sends (turns,
file/batch/bulk ingest, streaming source upload) and the SSE turn stream are
deliberately deadline-exempt. The `HttpClient` this library creates has
`Timeout = Timeout.InfiniteTimeSpan` so that holds; a client YOU hand in
keeps its own `HttpClient.Timeout` (default 100 s), which still caps those
exempt sends — set `httpClient.Timeout = Timeout.InfiniteTimeSpan` on it if
you use turns, bulk ingest, or streaming.

**Per-call `max_tokens` budgets.** `Providers.GetMaxTokensAsync()` reads
the tenant's effective output-token ceilings — eight fields, flattened,
plus `Source` (`tenant` | `environment`) and `UpdatedAt` — and
`Providers.ReplaceMaxTokensAsync(budgets)` replaces the WHOLE set on the
static rw role: there is no partial update, so change one by starting from
`(await client.Providers.GetMaxTokensAsync()).ToBudgets() with { TurnCompletion = 8192 }`.
A value outside its range is the typed `InvalidInputException`. REST-only.

**On gRPC, REST-only methods fail like every other failure.** Every
REST-only method (the four bulk routes, `GetSourceAsync`, `FindingsAsync`,
the chronology-rules pair, `Providers.ListAsync`, the max-tokens budget
pair `Providers.GetMaxTokensAsync` / `ReplaceMaxTokensAsync`, all of
`Reports` and `Authoring`, `ServerVersionAsync`) returns a FAULTED task
carrying `UnsupportedTransportException` — it surfaces at `await` /
`Task.WhenAll`, never synchronously from the call — and `TurnStreamAsync`
faults on the first `MoveNextAsync`. Two gRPC input rules beyond the proto3
zero sentinels: `IngestAsync` mirrors REST `POST /v1/ingest` (a locally
undecodable `ContentBase64` throws `InvalidInputException`; a server-side
per-item error on that one file throws `UnexpectedServerException` with the
text — the wire has no slug; `IngestBatchAsync` keeps per-item results), and
an explicit empty `Collections` on an ingest file or `runbookRefs` on
`Tokens.MintAsync` throws `InvalidInputException` (proto3 cannot carry
"explicitly empty"; pass `null` or use REST). `IngestFiles` is
deadline-exempt like the REST file/bulk sends.

Exceptions mirror the problem-slug registry (`HeadConflictException` with
Expected/Actual, `PolicyRejectionException` with findings + truncation
markers, `RunLockedException` — typed but `Transient = false`, pace it like
a rate limit — `RateLimitedException.RetryAfter` — populated only if the
server sends a `Retry-After` header, which it does not today).
`UnsupportedTransportException` marks the documented gRPC gaps.

Command retry is deliberately narrow: on REST a command re-sends its SAME
idempotency key only after a connect-phase failure
(`MunariumTransportException` with `MayHaveReachedServer == false` — connection,
name-resolution, TLS, or proxy-tunnel errors: the request never left) or the
typed `OverloadedException` (shed before executing). A gateway 502/504 is
transient for reads but never re-sent as a command — it may still be
executing upstream. On gRPC commands re-send only on `OverloadedException`,
never on a transport failure. Without the `ErrorInfo` detail an `ABORTED`
status decodes as `HeadConflictException(0, 0)`, so a run-lock rejection is
then indistinguishable from a head conflict (shared by all four clients).

## Tests

```bash
dotnet test tests/Ioka.Munarium.Client.Tests           # offline unit tests
MUNARIUM_REST_URL=http://127.0.0.1:18080 MUNARIUM_GRPC_URL=http://127.0.0.1:15051 \
MUNARIUM_TOKEN=devtoken MUNARIUM_MGMT_TOKEN=devmgmt \
dotnet test tests/Ioka.Munarium.Client.Conformance
# MUNARIUM_MGMT_TOKEN powers the PlatformSurface scenarios (all 10);
# without it they are skipped, the core scenarios still run.
```
