# Ingest: streamed, path-identified, integrity-checked, witnessed

Sources enter the mesh in two steps: an **upload** (the server verifies your
declared sha-256 before commit) and an **ingest event** binding the stored
source into a lineage. Requires the postgres store.

**`filename` is required — it is the source's identity.** A source is
identified by its *logical path*, not by its content hash. That path is also
where the bytes live in object storage, and it is what a runbook collection's
`filenamePrefix` matches against, so a source without one could never be
bound to anything. The response carries a `source_id` derived from it.

The content hash is still verified and still travels with the source, but as
**integrity**, not identity. The practical consequence:

| You upload | Result |
|---|---|
| same path, same bytes | idempotent replay — `already_existed: true` |
| same path, new bytes | an **update** in place — `already_existed: false`, a rebuild is owed |
| different path, same bytes | two **separate** sources, bound and retired independently |

That last row is the one to notice: the same document staged under
`smoke/policy.md` and `northgate/policy.md` is two sources, because each must
be bindable to its own collection on its own schedule.

Uploads take a replayable chunk **source** — a factory the transport calls
once per attempt — not a one-shot stream. That is what lets the clients retry
a transient failure for you: an upload is idempotent by content address, so
re-sending is always safe.

> **Two front doors:** this guide is the *ledger* path — a streamed upload
> plus an ingest event binding the source into a lineage. The file plane
> (`ingest`/`ingest_batch` — one-shot ingestion with matcher auto-binding
> into retrieval **collections**) and the bulk upload sessions are the other
> door, covered in [ingest-v2.md](ingest-v2.md).

**Rust** (`chunks_from_bytes` / `chunks_from_vec`, or any `Fn() -> BoxStream`)
```rust
use munarium_client::{chunks_from_vec, dto, SourceMeta};

let client = MunariumClient::rest(
    MunariumClientOptions::new("http://127.0.0.1:8080").token("devtoken").uid("user-1"))?;
let resp = client.ingest.put_source(
    SourceMeta {
        declared_sha256: hex_sha256(&bytes),
        media_type: Some("text/plain".into()),
        filename: Some("ticket-1.txt".into()),
        shape_ref: Some("support-tickets@1".into()),
    },
    chunks_from_vec(chunks),   // replayable: rebuilt per attempt
).await?;
client.ingest.record_ingest(&v, dto::RecordIngestRequest {
    content_hash: resp.content_hash, shape_ref: Some("support-tickets@1".into()),
}).await?;
```

**Python** (bytes, or a zero-arg callable returning a fresh iterable)
```python
from munarium_client import chunks_from_list

client = MunariumClient.rest(
    ClientOptions("http://127.0.0.1:8080", token="devtoken", uid="user-1"))
resp = client.ingest.put_source(
    chunks_from_list(chunks), declared_sha256=digest, media_type="text/plain",
    filename="ticket-1.txt", shape_ref="support-tickets@1")
client.ingest.record_ingest(v, content_hash=resp.content_hash,
                            shape_ref="support-tickets@1")
```

**.NET** (`ChunkSource` delegate — `Chunks.FromBytes` / `Chunks.FromList`)
```csharp
await using var client = MunariumClient.Rest(new MunariumClientOptions
    { Endpoint = "http://127.0.0.1:8080", Token = "devtoken", Uid = "user-1" });
var resp = await client.Ingest.PutSourceAsync(
    Chunks.FromBytes(content), declaredSha256: digest, mediaType: "text/plain",
    filename: "ticket-1.txt", shapeRef: "support-tickets@1");
await client.Ingest.RecordIngestAsync(v, resp.ContentHash, "support-tickets@1");
```

**Java** (`Params.ChunkSource` — an `InputStream` factory; `ofBytes` helper)
```java
var resp = client.ingest.putSource(
        Params.ChunkSource.ofBytes(bytes),
        Params.SourceMeta.of("ticket-1.txt", "text/plain")
                .withSha256(digest).withShapeRef("support-tickets@1"));
client.ingest.recordIngest(v, resp.contentHash(), "support-tickets@1");
```

Notes:

- Uploads stream in constant memory on both transports and are **exempt from
  the per-request deadline** (only your cancellation bounds them).
- The `shape_ref` binding decides which index builds chunk the source.
- The ingest event lands in the ledger as a `source-<hash12>.ingested` claim
  — auditable like every other fact.
- Transient upload failures ARE auto-retried (bounded by `read_retries`):
  the chunk source is rebuilt per attempt, and re-sending the same bytes to
  the same path is idempotent — it just returns `already_existed: true`.
- A one-shot iterator is REJECTED with a typed invalid-input error. It would
  be exhausted by the first attempt, so the retry would upload zero bytes —
  and without a declared hash the server would store the empty content and
  report `already_existed: true`. Pass bytes or a factory.
- Size: REST accepts up to 256 MiB per source; the helpers frame at 1 MiB so
  no single gRPC message approaches the transport's message limit.
- gRPC-specific: the Python sync stubs can't drive an async iterator — pass
  bytes or a callable returning a sync iterable there (typed error, never a
  crash).
