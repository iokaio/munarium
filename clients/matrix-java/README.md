# munarium-matrix-client (Java)

The Java client for **Munarium Matrix**, the structured-evidence plane. It
speaks Matrix's REST API and it is deliberately small: Matrix's whole surface
is *registering assets, running the three modes, and reading what happened*.

Depend on it by path from your own Gradle build:

```kotlin
// settings.gradle.kts
includeBuild("path/to/munarium/clients/matrix-java")
// build.gradle.kts
dependencies { implementation("io.ioka.munarium:munarium-matrix-client:1.0.0") }
```

**One runtime dependency: Jackson databind.** REST rides `java.net.http`,
which ships with the JDK, and the tests drive a `com.sun.net.httpserver` stub
rather than a mock-server library. A client whose dependency list is one line
cannot break a consumer's build over a transitive conflict.

Bytecode targets **Java 21** via `options.release`, so it builds on any newer
JDK.

## Use

```java
try (var mx = MatrixClient.of("https://matrix.example", "token")) {
    System.out.println(mx.version().lockstepOk());   // does Matrix agree with its server?

    mx.apply(Files.readString(Path.of("datasource.crm.yaml")));
    mx.apply(Files.readString(Path.of("contract.pipeline.yaml")));

    VerifyOutcome outcome = mx.verify("open-pipeline-by-region");
    if (outcome.failed() > 0) {
        outcome.questions().stream()
                .filter(q -> !q.ok())
                .forEach(q -> System.out.println(q.question() + " " + q.failures()));
        System.exit(3);                              // the exit discipline `mxctl` uses
    }
}
```

Pass an operator identity when one person drives both services — Matrix does
not require a uid, but the munarium-server's planes do, and sending the same
one keeps a single story across both journals:

```java
var options = MatrixClientOptions.of("https://matrix.example")
        .withToken(token)
        .withUid("ops@example.com");
```

Async is the same surface, awaited:

```java
try (var mx = AsyncMatrixClient.of("https://matrix.example", token)) {
    mx.sync("crm").thenAccept(job -> System.out.println(job.jobs()));
}
```

`AsyncMatrixClient` offloads each call to a virtual thread rather than
maintaining a second hand-written non-blocking path. On Java 21 blocking *is*
the scalable primitive, and a second path would be free to disagree with the
first about what a refusal means. A test asserts the two twins are
**method-for-method** the same, because a method that exists on one and not the
other is a trap for a caller porting between them.

### Refusals are typed

Matrix answers a refusal as RFC 9457 problem+json carrying a `refusal` object
with the **class** and the **code** — the closed vocabulary the whole system
rests on. They arrive as accessors, not prose:

```java
try {
    mx.verify("open-pipeline-by-region");
} catch (MatrixException e) {
    if (e.retryable()) {                    // unavailable | exhausted
        ...
    } else if ("not_covered".equals(e.code())) {   // the collection cannot answer it
        ...
    }
}
```

`retryable()` is not a guess: `unavailable` and `exhausted` are states of the
world, and every other class — `not_covered`, `denied`, `incomplete`,
`invalid` — is a statement about the request or the assets, where repeating it
changes nothing. Retrying a `denied` is hammering a door that is locked on
purpose. A failure carrying no refusal at all (a 404 for a missing asset) is
not retryable either: absent is not "maybe".

A transport failure becomes a `MatrixException` with class `unavailable`, so a
caller writes one retry rule and not two.

**Nothing here retries on your behalf.** Matrix's mutating routes carry no
idempotency-key contract, so a blind re-send of an accepted `sync` queues a
second one. That is a real trade-off, and it lands on the side of never doing
twice what the caller asked once.

### Lockstep

`version().lockstepOk()` is true only when the server reports `exact`. That is
the one state in which an evidence id minted by this Matrix is certain to
resolve on that server — which is what a citation like `[evidence/<id>#r0003]`
depends on. Every other value is a maybe, and a maybe about whether a citation
resolves is a no.

## What this client deliberately does NOT do

Three absences, each of them a design decision rather than a missing feature —
and each of them asserted by a test in `SurfaceTest`, because a rule that lives
only in a README is one well-meaning pull request from being gone:

* **No sealing.** A manifest is a statement about work the *sealer* did. An SDK
  offering `sealEvidence` would invite an application to assert provenance it
  cannot vouch for. Sealing is Matrix's own act; evidence is *read* through the
  **server's** Java client, resolving `[evidence/<id>#<row>]`.
* **No local validation.** `validate(yaml)` posts the YAML and returns Matrix's
  own findings and its own verdict. A client carrying its own copy of the rules
  would drift from the service that enforces them, and the drift would surface
  as an asset that validates here and is refused there.
* **No SQL.** Nothing on this surface takes a statement. Queries are
  pre-declared contracts and views, executed by name.

There is also **no gRPC transport**, and so no protobuf or grpc dependency.
Matrix's gRPC plane serves `MatrixQuery/Execute` alone, and that call is
service-to-service: the munarium-server makes it while answering a turn,
carrying a session's authorization snapshot an application does not hold.
Generating stubs would put megabytes of transitive netty on every consumer's
classpath to expose a call none of them may make. When that changes, this
library grows a transport rather than a second client.

## Surface

| Area | Methods |
| --- | --- |
| meta | `version`, `healthz`, `healthdata` |
| registry | `apply`, `validate`, `listAssets`, `getYaml` |
| sources | `introspect`, `probe`, `sync` |
| contracts and views | `verify`, `verifyView` |
| reconcile | `reconcile`, `promotionStatus`, `gateHistory`, `promote`, `demote`, `rollback` |
| audit | `journal` |

`verifyView` takes either a metric view or a native data view and tries the
metric-view route first, falling back on a 404 — the caller names the view, not
the route it happens to live on.

`sync` and `reconcile` return a `JobAccepted`: Matrix queues them, so the call
returns ids rather than an outcome. A sync fans out to one job per
authorization class, because a collection carries exactly one class. Poll
`journal()` or `promotionStatus(...).latestRun()` for the terminal state.

Responses are **records**, one per fixed contract DTO. Two reads stay
`JsonNode` on purpose — `introspect`, whose role-posture report and table list
are still moving, and `journal`, whose entry shape varies by operation kind.
Mirroring either would put a second normative copy of an unsettled shape in
this client, or invent a union that nothing on the wire writes.

## Versioning

This library targets Matrix `1.0.0`. A version bump on the wire surface bumps
them together.

## Tests

```powershell
.\gradlew.bat test          # ./gradlew test on macOS/Linux
```

The offline tier drives a `com.sun.net.httpserver` stub on an ephemeral port
and asserts the response *shapes* this client claims to understand, which is
what catches a field rename. There is no mock of Matrix's *semantics*: a client
test that asserted what a refusal MEANS would be asserting its own opinion.

Setting `MUNARIUM_MATRIX_TEST_URL` (and optionally `MUNARIUM_MATRIX_TEST_TOKEN`)
adds a live round trip against a real Matrix; with it unset that test **says it
skipped**, because a skip that prints nothing is indistinguishable from a pass
— which is exactly how Matrix's own Postgres conformance tier stayed vacuously
green for a phase.
