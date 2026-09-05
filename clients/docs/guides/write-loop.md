# The write loop: expected_head, idempotency, disputed-is-success

Three rules govern every write to the mesh ledger, and all four clients
encode them for you.

One connection rule first: every snippet constructs its client with a `uid`
— the acting end-user id the server stamps into its audit trail. That is not
optional politeness: the server requires a uid by default
(`MUNARIUM_REQUIRE_UID=true`) and answers the typed `uid-required` error
without one, so no write below even starts until the client carries it.

## 1. Disputed is SUCCESS, not an error

The command path IS the governance path: a claim that trips a block-severity
gate is **recorded `disputed`** and returned with the findings — never
dropped, never thrown. Check `is_disputed`; don't wrap propose calls in
try/catch expecting governance errors.

**Rust**
```rust
let client = MunariumClient::rest(
    MunariumClientOptions::new("http://127.0.0.1:8080").token("devtoken").uid("user-1"))?;
let out = client.commands.propose_claim(&v, req, None).await?;
if out.is_disputed() {
    for f in out.findings() { println!("{}: {}", f.rule_id, f.message); }
}
```

**Python**
```python
client = MunariumClient.rest(
    ClientOptions("http://127.0.0.1:8080", token="devtoken", uid="user-1"))
out = client.commands.propose_claim(v, subject="hero", key="eyes", value="blue")
if out.is_disputed:
    for f in out.findings: print(f.rule_id, f.message)
```

**.NET**
```csharp
await using var client = MunariumClient.Rest(new MunariumClientOptions
    { Endpoint = "http://127.0.0.1:8080", Token = "devtoken", Uid = "user-1" });
var outcome = await client.Commands.ProposeClaimAsync(v, new ClaimInput
    { Subject = "hero", Key = "eyes", Value = "blue" });
if (outcome.IsDisputed)
    foreach (var f in outcome.Findings) Console.WriteLine($"{f.RuleId}: {f.Message}");
```

**Java**
```java
try (var client = MunariumClient.rest(
        MunariumClientOptions.of("http://127.0.0.1:8080")
                .withToken("devtoken").withUid("user-1"))) {
    var out = client.commands.proposeClaim(
            v, Ledger.ClaimInput.fact("hero", "eyes", "blue"), null, null);
    if (out.isDisputed()) {
        for (var f : out.findings()) System.out.println(f.ruleId() + ": " + f.message());
    }
}
```

## 2. Head conflicts are normal — use the built-in loop

`expected_head` is optimistic concurrency: pass the head seq you read, and a
mismatch fails with a typed head-conflict error (409 / `ABORTED`) carrying
`expected`/`actual`. That's a *normal, retryable* outcome — re-read,
re-decide, retry. Each client ships the loop; the builder callback receives
the current head so you can re-decide against fresh state:

**Rust**
```rust
let client = MunariumClient::rest(
    MunariumClientOptions::new("http://127.0.0.1:8080").token("devtoken").uid("user-1"))?;
let out = client.propose_claim_with_retry(&v, |head| build_request(head),
    WriteLoopOptions::default()).await?;
```

**Python**
```python
client = MunariumClient.rest(
    ClientOptions("http://127.0.0.1:8080", token="devtoken", uid="user-1"))
out = client.propose_claim_with_retry(
    v, lambda head: {"subject": "hero", "key": "eyes", "value": "green"})
```

**.NET**
```csharp
await using var client = MunariumClient.Rest(new MunariumClientOptions
    { Endpoint = "http://127.0.0.1:8080", Token = "devtoken", Uid = "user-1" });
var outcome = await client.ProposeClaimWithRetryAsync(
    v, head => new ClaimInput { Subject = "hero", Key = "eyes", Value = "green" });
```

**Java**
```java
var out = client.proposeClaimWithRetry(
        v, head -> Ledger.ClaimInput.fact("hero", "eyes", "green"));
```

The loop sets `expected_head` for you, uses a **fresh idempotency key per
rebuilt attempt** (the body changed, so replay of the old key would be
wrong), backs off with jitter, and never retries any other error. A conflict
whose `actual` is 0 means an intermediary stripped the structured details —
the loop re-reads the head itself.

## 3. Idempotency keys: automatic, overridable

Every core command (versions, claims, events, promises, anchors, counters)
requires an `Idempotency-Key`. The clients auto-generate a UUID per call.
Pass your own key when YOU retry across process boundaries: replaying the
same key + same body returns the recorded response; the same key with a
different body fails with `idempotency-mismatch` (422).

When a command IS auto-retried — only a REST connect-phase failure (the
request never left) or the server's typed `overloaded` (shed BEFORE
executing), the two cases where the request provably never executed — the
client re-sends the SAME key, so a server that did record it replays rather
than re-runs. A transient 502/504 from a gateway is deliberately NOT in that
set even though reads retry it: the gateway answered, but the command may
still be executing upstream, and the same key re-sent now could execute it
twice. Any such ambiguous failure surfaces to you instead (the table below).
un-keyed writes (shapes, sources, ingests, index builds, providers, runbooks)
take no key and are sent exactly once (source upload is idempotent by
content address instead).

## Corrections, not updates

There is no update/delete anywhere. To change canon, propose a claim with
`claim_type: correction` (or `update` for a legitimate status transition)
naming `supersedes_id` — the ledger stays append-only and the old value
remains readable under pins.

## What is retried for you, and what is not

| Class | Examples | Auto-retried? |
|---|---|---|
| read | every query, compose, search, health | yes, on any transient failure |
| command | propose_claim, append_events, promises, anchors | **only** when the request provably never reached the server (a REST connect-phase failure) or the server answered the typed `overloaded` (shed before executing); a gateway 502/504 is NOT re-sent; on gRPC no transport failure is provably undelivered, so only the typed `overloaded` re-sends — identical in all four clients |
| write | apply_shape, apply_runbook, approve_step, record_ingest | never (sent exactly once) |
| upload | put_source | yes — the chunk source is rebuilt per attempt and the content address makes a re-send idempotent |

The command rule is the load-bearing one. The server writes an idempotency
key **after** the command completes, not when it starts, so there is no
in-flight reservation: a retry that overtakes a still-running first attempt
executes the command twice — a doubled append, not a replayed one. A timeout
on an established connection cannot be told apart from that case, so the
clients surface it instead of guessing. If you know the first attempt never
landed, re-issue with the SAME idempotency key yourself; by then the server
has either recorded the key (and replays) or never saw it.
