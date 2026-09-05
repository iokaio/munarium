# Ioka.Munarium.Matrix.Client (.NET)

The .NET client for **Munarium Matrix**, the structured-evidence plane. It
speaks Matrix's REST API and it is deliberately small: Matrix's whole surface
is *registering assets, running the three modes, and reading what happened*.

**Zero package dependencies.** REST rides `System.Net.Http` and
`System.Text.Json`, both of which ship in the `net10.0` shared framework, so
this client adds nothing to a consumer's dependency graph — no second HTTP
stack with its own proxy and TLS behaviour to explain, no transitive version
conflict to resolve in an application that already pins something. The
server's .NET client carries gRPC packages because it serves a gRPC data
plane; this one does not (see below), so it needs none of them.

## Use

```csharp
using Ioka.Munarium.Matrix.Client;

using var mx = new MatrixClient(new MatrixClientOptions
{
    Endpoint = "https://matrix.example",
    Token = "...",
    Uid = "ops@example.com",
});

var version = await mx.VersionAsync(ct);
Console.WriteLine(version.LockstepOk);        // does Matrix agree with its server?

await mx.ApplyAsync(await File.ReadAllTextAsync("datasource.crm.yaml", ct), ct);
await mx.ApplyAsync(await File.ReadAllTextAsync("contract.pipeline.yaml", ct), ct);

var outcome = await mx.VerifyAsync("open-pipeline-by-region", ct);
if (outcome.Failed > 0)
{
    foreach (var q in outcome.Questions.Where(q => !q.Ok))
    {
        Console.WriteLine($"{q.Question}: {string.Join("; ", q.Failures)}");
    }
    return 3;                                  // the exit discipline `mxctl` uses
}
```

### Async only

Every call returns a `Task<T>` and takes a `CancellationToken`. There is no
synchronous overload and there will not be one: a `.Result` on a captured
synchronization context blocks the thread the continuation needs, and a client
that ships that overload will have it called — under load, in production,
where the deadlock is hardest to read. The Python client ships sync and async
because Python's synchronous HTTP is a genuinely separate, safe
implementation. .NET has no such thing to offer, so the safe number of sync
methods here is zero, and a test asserts it stays zero.

### Refusals are typed

Matrix answers a refusal as RFC 9457 problem+json carrying a `refusal` object
with the **class** and the **code** — the closed vocabulary the whole system
rests on. They arrive as properties, not prose:

```csharp
try
{
    await mx.VerifyAsync("open-pipeline-by-region", ct);
}
catch (MatrixException e) when (e.Retryable)   // unavailable | exhausted
{
    await Task.Delay(e.RetryAfter ?? TimeSpan.FromMinutes(1), ct);
}
catch (MatrixException e) when (e.Code == "not_covered")
{
    // the collection cannot answer it
}
```

`Retryable` is a property and not a guess: `unavailable` and `exhausted` are
states of the world, and every other class is a statement about the request or
the assets, where repeating it changes nothing. Retrying a `denied` is
hammering a door that is locked on purpose. A transport failure — a refused
connection, a DNS miss, an expired deadline — becomes a `MatrixException` with
class `unavailable`, because a request that got no answer is the same kind of
fact as a source that is down.

There is deliberately no retry loop inside the client. The two retryable
classes want pacing the caller owns: an exhausted budget refusal carries the
wait the service asked for, and burning it down inside a client would spend
the caller's budget on its behalf.

### Lockstep

`(await mx.VersionAsync(ct)).LockstepOk` is true only when the service reports
`server_compatibility == "exact"`. That is the one state in which an evidence
id minted by this Matrix is certain to resolve on that server — which is what
a citation like `[evidence/<id>#r0003]` depends on.

## What this client deliberately does NOT do

Four absences, each of them a design decision rather than a missing feature:

* **No gRPC transport.** Matrix's gRPC plane serves `Execute` alone, and
  `Execute` is service-to-service — the munarium-server calls it while
  answering a turn, so that the evidence an answer cites is sealed by the
  process that ran the query. An application sits on the other side of the
  server, asking for the answer. If that ever changes, this package grows a
  transport rather than acquiring a sibling.
* **No sealing.** A manifest is a statement about work the *sealer* did. An
  SDK offering `SealEvidence` would invite an application to assert provenance
  it cannot vouch for. Sealing is Matrix's own act; evidence is *read* through
  the **server's** client, resolving `[evidence/<id>#<row>]`. A test asserts
  that no public member's name contains "Seal" or "Evidence", so the absence
  cannot erode by accident.
* **No local validation.** `ValidateAsync` posts the YAML and returns Matrix's
  own findings. A client carrying its own copy of the rules would drift from
  the service that enforces them, and the drift would surface as an asset that
  validates here and is refused there.
* **No SQL.** Nothing on this surface takes a statement. Queries are
  pre-declared contracts and views, executed by name — and a test asserts that
  too.

## Surface

| Area | Methods |
| --- | --- |
| meta | `VersionAsync`, `HealthzAsync`, `HealthdataAsync` |
| registry | `ApplyAsync`, `ValidateAsync`, `ListAssetsAsync`, `GetYamlAsync` |
| sources | `IntrospectAsync`, `ProbeAsync`, `SyncAsync` |
| contracts and views | `VerifyAsync`, `VerifyViewAsync` |
| reconcile | `ReconcileAsync`, `PromotionStatusAsync`, `GateHistoryAsync`, `PromoteAsync`, `DemoteAsync`, `RollbackAsync` |
| audit | `JournalAsync` |

`VerifyViewAsync` takes either a metric view or a native data view and tries
the metric-view route first, falling back when that route says there is no
such view — the caller names the view, not the route it happens to live on.
Matrix spells "no such view" two ways (a bare 404 from the store, and a
`not_covered` refusal at HTTP 422 when the miss goes through the asset
loader), and the verify route takes the second path, so the fallback keys on
both. When the fallback also says no such view, the metric-view error is what
surfaces; when it fails for any other reason the view exists and that error
surfaces instead.

`SyncAsync` and `ReconcileAsync` return a `JobAccepted`: Matrix queues them,
so the call returns an id rather than an outcome. Poll `JournalAsync` or
`PromotionStatusAsync` (whose `LatestRun` carries the pass's terminal state)
for what happened.

A shape the contract pins arrives as a record; a shape that is open,
role-gated, or exists mainly to be read by a human — journal rows,
introspection, probes, gate history, a rollback tally — arrives as a
`JsonElement`. Inventing records for the second group would create a second
normative copy of shapes that are allowed to grow, and the growth would land
here as a silent field drop.

## Versioning

This package targets Matrix `1.0.0`; `MatrixClient.TargetVersion` says so in
code. A version bump on the wire surface bumps them together.

## Tests

```bash
dotnet build
dotnet test
```

The offline tier drives a stub `HttpMessageHandler` and asserts the response
*shapes* this client claims to understand, which is what catches a field
rename. Setting `MUNARIUM_MATRIX_TEST_URL` (and optionally
`MUNARIUM_MATRIX_TEST_TOKEN`) adds a live round-trip against a real Matrix;
with it unset that test is reported as a real **skip with its reason**, not an
early `return` that reads as a pass — which is how Matrix's own Postgres
conformance tier stayed vacuously green for a phase.
