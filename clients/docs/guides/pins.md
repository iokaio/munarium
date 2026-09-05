# Point-in-time pins: one as_of_seq bounds everything

Every query accepts `as_of_seq`. One pin bounds **all** stores together —
the load-bearing semantic of the mesh:

- **facts**: supersession is resolved *as of* the pin — a claim corrected
  after the pin reads back as current at the pin;
- **anchors** and **counters** stamped after the pin are invisible;
- **promises** fulfilled after the pin read back **open**;
- **digests are rebuilt** deterministically from the pinned facts — stored
  head rungs are never served under a pin (use compose, not the raw digest
  list, for pinned reads).

**Rust**
```rust
use munarium_client::{ContextQuery, FactsQuery};

let client = MunariumClient::rest(
    MunariumClientOptions::new("http://127.0.0.1:8080").token("devtoken").uid("user-1"))?;
let pinned = client.query.facts(&v, FactsQuery {
    as_of_seq: Some(3), ..Default::default() }).await?;
let promises = client.query.promises(&v, Some(3), None).await?; // reads OPEN
let ctx = client.query.compose_context(&v, ContextQuery {
    as_of_seq: Some(3), ..Default::default() }).await?; // digests rebuilt
```

**Python**
```python
client = MunariumClient.rest(
    ClientOptions("http://127.0.0.1:8080", token="devtoken", uid="user-1"))
pinned = client.query.facts(v, as_of_seq=3)
promises = client.query.promises(v, as_of_seq=3)   # reads OPEN
ctx = client.query.compose_context(v, as_of_seq=3) # digests rebuilt
```

**.NET**
```csharp
await using var client = MunariumClient.Rest(new MunariumClientOptions
    { Endpoint = "http://127.0.0.1:8080", Token = "devtoken", Uid = "user-1" });
var pinned = await client.Query.FactsAsync(v, asOfSeq: 3);
var promises = await client.Query.PromisesAsync(v, asOfSeq: 3);   // reads OPEN
var ctx = await client.Query.ComposeContextAsync(v, asOfSeq: 3);  // digests rebuilt
```

**Java**
```java
var pinned = client.query.facts(v, Params.FactsQuery.atSeq(3));
var promises = client.query.promises(v, 3L, null);           // reads OPEN
var ctx = client.query.composeContext(
        v, new Params.ContextQuery(null, null, null, 3L));   // digests rebuilt
```

## Pins start at 1

`as_of_seq = 0` means "head" on the gRPC wire (proto3 sentinel), so the
clients reject an explicit zero pin on gRPC with a typed invalid-input error
instead of silently reading head state. Omit the pin for head reads.

## Lineage

Versions form parent chains (`create_version(parent_version_id=...)`);
reads resolve across the whole lineage. A correction in a child version
supersedes the parent's claim at head while the pin still shows the
original — that's the cross-version supersession the conformance scenario
`ledger.supersession-pin` locks in.

## Budgets degrade digests before facts

`compose_context` under a token budget degrades digest resolution tier by
tier before trimming any fact — the composed "Accepted facts" section keeps
all facts as long as the budget allows (`composer.budget-degradation`).
