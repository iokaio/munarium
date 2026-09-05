# munarium-client (Java)

Official Java client for [munarium-server](../../server/): the ten-plane
surface on both transports (REST via `java.net.http`, gRPC via
netty-shaded), **sync and async** facades, typed exceptions keyed on the
problem-slug registry, and the head-conflict write loop built in.

- **Coordinates**: `io.ioka.munarium:munarium-client`, Apache-2.0
  ([LICENSE](../LICENSE); the POM declares it, the jar carries LICENSE and
  NOTICE under `META-INF`). Maven Central publication is a release-time step
  of the public Clients repository — see [compatibility.json](../compatibility.json).
  Package `io.ioka.munarium.client`.
- **Toolchain**: bytecode targets Java 21 (LTS) via `--release`; builds on
  any newer JDK. Only the JDK is needed — the committed
  Gradle wrapper fetches everything else.
- **Models**: Jackson over records, snake_case wire naming pinned by the
  shared mapper; unknown fields ignored (forward-compatible).
- **gRPC stubs**: generated at BUILD time from `../../server/proto` (all
  ten protos, `session.proto` and `admin.proto` included) — the .NET
  posture: no committed stubs, no drift check, zero contract drift.
- **Async**: `AsyncMunariumClient` offloads each call to a VIRTUAL thread —
  one sync-first implementation, zero sync/async drift (the same trade the
  Python client made for its async gRPC, made explicit; on Java 21 blocking
  is the scalable primitive).
- **One typed `Transport`**: `io.ioka.munarium.client.Transport` extends all
  ten plane interfaces plus `serverVersion()` and `AutoCloseable`; both
  `RestTransport` and `GrpcTransport` implement it, so a transport missing
  a plane is a COMPILE error, and a custom or decorating transport
  (metrics, logging) implements the same ten planes.
- **Dependencies**: only Jackson is `api`-scoped. The gRPC/protobuf stack
  (`grpc-netty-shaded`, `grpc-protobuf`, `grpc-stub`, `protobuf-java`,
  `proto-google-common-protos`) is `implementation`-scoped — nothing
  pb-typed is in the public API, so it never lands on a consumer's compile
  classpath.

## Quickstart

```java
import io.ioka.munarium.client.*;
import io.ioka.munarium.client.model.Ledger;

try (var client = MunariumClient.rest(
        MunariumClientOptions.of("http://127.0.0.1:8080")
                .withToken("devtoken")
                .withUid("user-1"))) {   // the uid contract: required by default
    String v = client.commands.createVersion();
    var outcome = client.commands.proposeClaim(
            v, Ledger.ClaimInput.fact("hero", "eyes", "green"), null, null);
    if (outcome.isDisputed()) {
        // SUCCESS state: gate-blocked claims are recorded disputed with
        // findings — governance records, never drops (invariant #1).
        System.out.println(outcome.findings());
    }
}
```

Streaming session turn (SSE — progress callback + full result):

```java
var session = client.sessions.create("ent-support");
var turn = client.sessions.turnStream(
        session.sessionId(),
        Params.TurnOptions.of("vacation policy"),
        progress -> System.out.println("… " + progress.stage()));
```

Research profiles (S-3.3/S-3.5) ride the same turn call:
`Params.TurnOptions.of(q).withResearchProfile("regulatory")` routes the turn
through a named evidence hierarchy and the result carries
`turn.hierarchy()` — which profile ran, what each layer produced, and whether
a completeness claim was permissible at all. Leave it unset and nothing
changes: the request grows no key, the response carries no `hierarchy`, and
the SSE sequence is the one it always was. The streaming plane's six
hierarchy stages (`profile`, `layer_start`, `layer_source`, `layer_complete`,
`coverage`, `compose`) are appended after the existing ones, and an unnamed
stage from a newer server still flows through the callback. The operator
views are `reports.evidenceReport(window)` (which layer is quietly refusing)
and `reports.matrix()` (the Matrix plane as this server instance sees it),
both mgmt-scoped like every other report. `evidenceReport`, not `evidence`:
`Transport` implements every plane on one type, and the evidence-artifact
read already owns that name.

Per-call output-token budgets (`/v1/max-tokens`) ride the providers plane:
`providers.maxTokens()` reads the effective set with its `source()`
(`tenant` | `environment`) and `updatedAt()`, and
`providers.replaceMaxTokens(budgets)` replaces the WHOLE set (static rw;
every one of the eight members is required and range-checked, `invalid-input`
on a miss). There is no partial update, so the read-modify-write seam is
`providers.maxTokens().budgets().withTurnCompletion(4096)`. REST-only, like
the reports plane; the async facade carries both.

The gRPC twin is `MunariumClient.grpc(options)` — same planes; operations with
no RPC on that transport throw the typed `UnsupportedTransportException`
(never a silent drop). See the front door
([clients/README.md](../README.md)) for the transport-gap ledger and the
invariants all four clients encode.

Command retry is deliberately narrow: on REST a command re-sends its SAME
idempotency key only after a connect-phase failure (`ConnectException`,
`HttpConnectTimeoutException`, `SSLHandshakeException`,
`UnknownHostException` — the request never left) or the typed
`OverloadedException` (shed before executing). A gateway 502/504
(`UnexpectedServerException`, transient) is retried for reads but never
re-sent as a command — it may still be executing upstream. On gRPC commands
re-send only on `OverloadedException`, never on a transport failure. Two
gRPC input rules beyond the proto3 zero sentinels: `ingest(file)` mirrors
REST `POST /v1/ingest` (a locally undecodable `contentBase64` throws
`InvalidInputException`; a server-side per-item error on that one file
throws `UnexpectedServerException` with the text — the wire has no slug;
`ingestBatch` keeps per-item results), and an explicit empty
`collections()` on an ingest file or `runbookRefs()` on `tokens.mint`
throws `InvalidInputException` (proto3 cannot carry "explicitly empty";
pass `null` or use REST). `IngestFiles` is deadline-exempt like the REST
file/bulk sends, and a null `turnStream` progress callback means "final
result only".

## Build + test

```bash
./gradlew build                 # generates gRPC stubs, compiles, offline unit tests

# Live conformance (the 7 ported scenarios × both transports + async
# round-trips + the 10 platform smokes) against a pg-backed server:
MUNARIUM_REST_URL=http://127.0.0.1:18080 MUNARIUM_GRPC_URL=127.0.0.1:15051 \
MUNARIUM_TOKEN=devtoken MUNARIUM_MGMT_TOKEN=devmgmt ./gradlew conformanceTest
```

`gates.chronology-certain-only` is a documented skip (pure-kernel scenario
with no API surface; `SCENARIOS.md` marks it kernel-only, and no client port
carries it — the Rust suite is client-native), the
same deviation as the Python and .NET ports.
