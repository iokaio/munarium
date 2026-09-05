# Sessions: multiturn retrieval, streaming turns, and the transcript

A **session** pins a runbook version and takes retrieval turns over the
collections that runbook grants — filtered by the caller's access
level/compartments, so the create response is a least-privilege echo of what
this caller can actually see. Auth is the data plane's: a capability JWT
with the `query` scope, or a static token; the uid contract applies to every
call. Requires the postgres store and an applied v2 runbook with collections
and an active index.

Two postures to hold before the first call:

- **A turn spends provider tokens.** Turns are sent exactly once, never
  auto-retried, and **deadline-exempt** — a client-side abort cannot stop
  the server's paid completion, so the clients don't pretend a timeout
  un-spends it. Only your own cancellation bounds a turn.
- **The streaming turn is REST-only.** SessionService has no streaming RPC;
  the gRPC clients raise the typed `Unsupported` error — at the moment the
  REST twin would surface a pre-stream refusal, not synchronously from the
  call: Python's sync `turn_stream` raises when called and
  `AsyncMunariumClient.grpc(...).sessions.turn_stream` is a real async
  generator that raises on the first iteration; .NET's `TurnStreamAsync`
  faults on the first `MoveNextAsync`; Rust's `turn_stream(...).await` and
  Java's `turnStream` return/throw the typed error directly.

## Create a session, take a turn, close it

`create` takes a bare runbook name (latest non-removed version) or an exact
`name@version`; every turn then runs against that pin. `close` is a write
(`ro` tokens are refused) and idempotent — closing a closed/expired session
echoes its state unchanged.

**Rust**
```rust
let client = MunariumClient::rest(
    MunariumClientOptions::new("http://127.0.0.1:8080").token("devtoken").uid("user-1"))?;
let session = client.sessions.create("ent-support").await?;
println!("permitted: {:?}", session.permitted_collections);

let turn = client.sessions.turn(&session.session_id, dto::TurnRequest {
    query: "vacation policy".into(),
    complete: Some(true),           // run the runbook's completion step
    ..Default::default()
}).await?;
for hit in &turn.hits { println!("[{}] {}", hit.collection, hit.source_path); }
if let Some(c) = &turn.completion { println!("{} ({}): {}", c.provider, c.model, c.text); }

client.sessions.close(&session.session_id).await?;
```

**Python**
```python
client = MunariumClient.rest(
    ClientOptions("http://127.0.0.1:8080", token="devtoken", uid="user-1"))
session = client.sessions.create("ent-support")

turn = client.sessions.turn(session.session_id, query="vacation policy", complete=True)
for hit in turn.hits:
    print(f"[{hit.collection}] {hit.source_path}")
if turn.completion:
    print(f"{turn.completion.provider} ({turn.completion.model}): {turn.completion.text}")

client.sessions.close(session.session_id)
```

**.NET**
```csharp
await using var client = MunariumClient.Rest(new MunariumClientOptions
    { Endpoint = "http://127.0.0.1:8080", Token = "devtoken", Uid = "user-1" });
var session = await client.Sessions.CreateAsync("ent-support");

var turn = await client.Sessions.TurnAsync(session.SessionId, new TurnRequest
    { Query = "vacation policy", Complete = true });
foreach (var hit in turn.Hits) Console.WriteLine($"[{hit.Collection}] {hit.SourcePath}");
if (turn.Completion is { } c) Console.WriteLine($"{c.Provider} ({c.Model}): {c.Text}");

await client.Sessions.CloseAsync(session.SessionId);
```

**Java**
```java
var session = client.sessions.create("ent-support");

var turn = client.sessions.turn(
        session.sessionId(),
        new Params.TurnOptions("vacation policy", null, true, null));
for (var hit : turn.hits()) {
    System.out.println("[" + hit.collection() + "] " + hit.sourcePath());
}
if (turn.completion() != null) {
    System.out.println(turn.completion().text());
}

client.sessions.close(session.sessionId());
```

The result names the collections actually searched
(`collections_searched`), the permitted-but-indexless ones (`skipped`), and
carries one `ProvenanceEnvelope` **per collection** (`envelopes`) — the
per-source provenance contract from the retrieval plane, kept per
compartment boundary.

## Model overrides — honored or refused, never downgraded

A turn may carry a `model_override` (`provider`/`model`/`tier`). It is
honored only under the runbook's `models.allowOverrides` policy; a
disallowed override draws the typed `override-not-allowed` error (the
Forbidden family in every client) — **never a silent downgrade** to the
runbook's default. The completion echoes what actually served
(`provider`, `model`, `was_override`), so an audit can always tell.

**Rust**
```rust
let turn = client.sessions.turn(&session.session_id, dto::TurnRequest {
    query: "vacation policy".into(),
    complete: Some(true),
    model_override: Some(dto::ModelOverrideDto {
        tier: Some("capable".into()), ..Default::default() }),
    ..Default::default()
}).await?; // MunariumError::Forbidden when the runbook forbids overrides
```

**Python**
```python
turn = client.sessions.turn(
    session.session_id, query="vacation policy", complete=True,
    model_override={"tier": "capable"})   # ForbiddenError when not allowed
```

**.NET**
```csharp
var turn = await client.Sessions.TurnAsync(session.SessionId, new TurnRequest
{
    Query = "vacation policy", Complete = true,
    ModelOverride = new ModelOverride { Tier = "capable" },
});  // ForbiddenException when the runbook forbids overrides
```

**Java**
```java
var turn = client.sessions.turn(
        session.sessionId(),
        Params.TurnOptions.of("vacation policy")
                .withCompletion(new SessionsApi.ModelOverride(null, null, "capable")));
// ForbiddenException when the runbook forbids overrides
```

## The streaming turn

`turn_stream` runs the same turn over SSE: progress events at real stage
boundaries (`retrieval` per collection, `merge`, `model`, `completion` per
paid attempt, `verify`), then the full turn result. Each language surfaces
the stream in its own idiom:

**Rust** — a `Stream` of typed events
```rust
use futures_util::StreamExt;
use munarium_client::TurnStreamEvent;

let mut stream = client.sessions.turn_stream(&session.session_id,
    dto::TurnRequest { query: "vacation policy".into(), ..Default::default() }).await?;
while let Some(event) = stream.next().await {
    match event? {
        TurnStreamEvent::Progress(p) => println!("… {p:?}"),
        TurnStreamEvent::Done(turn) => println!("turn {} done", turn.ordinal),
    }
}
```

**Python** — an iterator whose LAST item is the `TurnResult`
```python
from munarium_client.models import TurnProgress

for event in client.sessions.turn_stream(session.session_id, query="vacation policy"):
    if isinstance(event, TurnProgress):
        print("…", event.stage)
    else:
        result = event   # the final TurnResult — always last
```

The async twin is an async generator (`async for`). If you may leave it
EARLY, wrap it in `contextlib.aclosing(...)`: an abandoned async generator
is finalized by the garbage collector at an unspecified later time, and the
pooled connection stays checked out until then — `aclosing` runs the
generator's cleanup (closing the response, releasing the connection)
deterministically on exit:

```python
from contextlib import aclosing

async with aclosing(
        aclient.sessions.turn_stream(session.session_id, query="vacation policy")) as events:
    async for event in events:
        if isinstance(event, TurnProgress) and event.stage == "model":
            break   # the connection is released when the block exits
```

**.NET** — an `IAsyncEnumerable<TurnStreamEvent>`
```csharp
await foreach (var ev in client.Sessions.TurnStreamAsync(
    session.SessionId, new TurnRequest { Query = "vacation policy" }))
{
    switch (ev)
    {
        case TurnStreamEvent.Progress p: Console.WriteLine($"… {p.Event.Stage}"); break;
        case TurnStreamEvent.Done d: Console.WriteLine($"turn {d.Response.Ordinal} done"); break;
    }
}
```

**Java** — a progress callback + the full result as the return value
```java
var turn = client.sessions.turnStream(
        session.sessionId(),
        Params.TurnOptions.of("vacation policy"),
        progress -> System.out.println("… " + progress.stage()));
// async twin: asyncClient.sessions.turnStream(...) -> CompletableFuture<TurnResult>
```

### The SSE invariants (all languages)

- The wire is N `progress` events, then **exactly one terminal event**: a
  `done` carrying the full turn result, or an `error` carrying problem+json.
  The terminal error decodes through the SAME slug decoder as every other
  failure — a mid-stream refusal (e.g. the closed-session error) throws the
  same typed error the unary route would.
- A stream that ends **without** a terminal event is a typed transport
  error — never a silent success or a half-answer.
- There is no overall deadline (a capable-tier completion can exceed the
  default request timeout) but a **60 s idle watchdog** per read — safe
  because the server heartbeats keep-alive comments every 15 s, so a healthy
  stream is never idle that long. When it fires, the typed transport error
  means a wedged peer, NOT an un-spent turn: the turn may still be
  executing server-side (the completion was paid and the transcript ordinal
  still advances), so read the transcript with `sessions.get` before
  re-sending the turn. (Python's error text says exactly this.)
- **.NET, caller-supplied `HttpClient`:** the client never mutates a
  handed-in `HttpClient`, so its own `Timeout` (default 100 s) still bounds
  the stream and the deadline-exempt unary turn. Set `httpClient.Timeout =
  Timeout.InfiniteTimeSpan` on it if you use turns or streaming; the
  `HttpClient` the library creates already has that.
- Event retention is capped at **16 MiB** per pending event (a peer that
  never terminates a line or an event cannot grow client memory past it),
  and the cap never drops an event the same chunk completed: a `done` that
  arrives beside oversized trailing bytes is delivered as the real result,
  and the overflow surfaces as a typed error afterwards.
- Progress is informational and forward-compatible: a newer server may emit
  stages this client build cannot name. Python/.NET/Java DELIVER such events
  (permissive/flat records with a `stage` string); Rust skips an
  undecodable progress event rather than failing the stream. Terminal
  events are never skipped anywhere. Stages today: `probe` (only when the
  runbook declares `retrieval.collectionSelection` — one per permitted
  collection as its probe completes: `collection`, `hits`, `skipped`),
  `selection` (same condition — `probed`, `selected`, `collections`),
  `expansion` (only with
  `retrieval.modelQueryExpansion` — `provider`, `model`, `terms`,
  `input_tokens`, `output_tokens`), `retrieval`, `merge`, `model`,
  `completion`, `verify`; the first two landed server-side 2026-08-25 and
  ride through every client as the forward-compatible case above.

## Reading the transcript

Every turn is stored on the session; `get` returns the envelope plus the
turn-by-turn transcript (stored transcript rows are JSON documents — a
record, not re-typed models).

**Rust**
```rust
let s = client.sessions.get(&session.session_id).await?;
println!("{} turns, state {}", s.turns.len(), s.state);
```

**Python**
```python
s = client.sessions.get(session.session_id)
print(len(s.turns), s.state)   # state: open | closed | expired
```

**.NET**
```csharp
var s = await client.Sessions.GetAsync(session.SessionId);
Console.WriteLine($"{s.Turns.Count} turns, state {s.State}");
```

**Java**
```java
var s = client.sessions.get(session.sessionId());
System.out.println(s.turns().size() + " turns, state " + s.state());
```

Notes:

- When the runbook declares `completion.verification`, the completion
  carries a `verification` block: which checks ran, corrective retries paid,
  and the violations remaining on the FINAL answer (empty = verified;
  non-empty = the answer stands UNVERIFIED after the retry budget — you
  decide what that means for display). The streaming turn narrates each
  verify pass as it happens.
- Completion token counts sum over ALL completions the turn paid for,
  verification retries included — the honest bill, not the last call's.
- A turn against a closed session draws the typed `session-not-open` error;
  on the streaming route it can land MID-STREAM as the terminal error event.
