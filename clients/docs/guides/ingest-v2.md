# Ingest v2: the file plane, collection binding, and bulk upload sessions

The streamed `put_source` + ingest-event path
([ingest.md](ingest.md)) binds sources into a *ledger lineage*. The **file plane** is the other front door: one-shot document ingestion into
**collections** — the compartmentalized retrieval stores that runbooks and
sessions search. Content travels base64 (JSON-safe) with an optional
declared sha256, verified before commit — the same content-addressing
contract as `PUT /v1/sources`, so the same idempotency table applies: same
path + same bytes is a replay (`existed: true`), same path + new bytes is an
update in place, and a rebuild is then owed.

**Scope note:** the file plane requires the `ingest` scope on a capability
token (an `rw` static token passes). That is the designed division of
labor — a loader holds an ingest-scoped token and can ingest but not
query; see [tokens-and-reports.md](tokens-and-reports.md).

## Auto-binding vs explicit targets

Where a document lands is declarative by default: omit `collections` and
the server binds the file via the `sources:` matchers (filename prefixes)
of every active runbook the token may reach. Pass an explicit
`collections` list to override the matchers. Either way the token's access
level/compartments must permit each target collection — an explicit name is
a request, not an escalation. The result names what actually happened:
`bound_to` lists the collections this call bound the file into.

**Rust**
```rust
use base64::Engine as _;

let client = MunariumClient::rest(
    MunariumClientOptions::new("http://127.0.0.1:8080").token("devtoken").uid("loader"))?;
let res = client.ingest.ingest(dto::IngestFileRequest {
    filename: "handbook/vacation.md".into(),
    media_type: "text/markdown".into(),
    content_base64: base64::engine::general_purpose::STANDARD.encode(text),
    sha256: None,
    collections: None,   // auto-bind via runbook `sources:` matchers
}).await?;
println!("{} -> {:?} (existed: {})", res.filename, res.bound_to, res.existed);
```

**Python**
```python
import base64

client = MunariumClient.rest(
    ClientOptions("http://127.0.0.1:8080", token="devtoken", uid="loader"))
res = client.ingest.ingest({
    "filename": "handbook/vacation.md",
    "media_type": "text/markdown",
    "content_base64": base64.b64encode(text.encode()).decode(),
})  # collections omitted = auto-bind
print(res.filename, res.bound_to, res.existed)
```

**.NET**
```csharp
await using var client = MunariumClient.Rest(new MunariumClientOptions
    { Endpoint = "http://127.0.0.1:8080", Token = "devtoken", Uid = "loader" });
var res = await client.Ingest.IngestAsync(new IngestFile
{
    Filename = "handbook/vacation.md",
    MediaType = "text/markdown",
    ContentBase64 = Convert.ToBase64String(Encoding.UTF8.GetBytes(text)),
    // Collections omitted = auto-bind via runbook sources: matchers
});
Console.WriteLine($"{res.Filename} -> [{string.Join(", ", res.BoundTo)}]");
```

**Java**
```java
var res = client.ingest.ingest(
        Ingesting.IngestFile.ofText("handbook/vacation.md", "text/markdown", text));
// explicit targets instead: .withCollections(List.of("hr-policies"))
System.out.println(res.filename() + " -> " + res.boundTo());
```

Single-file `ingest` has the REST `POST /v1/ingest` outcome shape on BOTH
transports. That route rejects a bad file with a typed 400 rather than
returning a per-item error, so the gRPC twin (a one-file `IngestFiles`)
does the same: a body that does not decode as base64 is the typed
invalid-input error, raised locally before anything ships. A server-side
per-item error on that one file (a hash mismatch, a collection the token
may not write) surfaces as the unexpected-server error carrying the
server's text — a documented parity gap, because the gRPC wire carries
per-item errors as free strings with no problem slug, so the typed kind
REST would give cannot be recovered there. Two more transport facts:

- **`collections: []` is a gRPC sentinel case.** REST reads an explicit
  empty list as "bind to nothing"; a proto3 empty repeated field is
  indistinguishable from absent, which the server reads as "matcher
  auto-bind". The gRPC clients refuse the explicit `[]` with the typed
  invalid-input error instead of silently auto-binding — omit the field
  (`None`/`null`) or use REST.
- **Base64 with surrounding or embedded ASCII whitespace is accepted on
  both transports** (a trailing newline from a `base64` pipeline is the
  common case): the REST server trims before decoding, and the gRPC
  clients strip whitespace before their local decode so the same input
  succeeds either way.

## Batch: per-item outcomes on BOTH transports

`ingest_batch` takes 1..=500 files and reports **per item** — one bad file
(a mangled base64 body, a hash mismatch, a forbidden collection) never
fails the batch; its result row carries `error` while its neighbors land.
The 500-file cap is enforced client-side as a typed error before any bytes
ship. This contract holds on gRPC too: the twin is `IngestFiles` (the
transport decodes base64 back to raw bytes for the wire, and a per-item
decode failure becomes that item's error, not the batch's). The
`collections: []` sentinel above is not a per-item server outcome, so on
gRPC it is refused client-side before the batch ships (Rust reports it as
that item's error row instead of refusing the batch).

**Rust**
```rust
let resp = client.ingest.ingest_batch(dto::IngestBatchRequest { files }).await?;
for r in resp.results.iter().filter(|r| r.error.is_some()) {
    eprintln!("FAILED {}: {:?}", r.filename, r.error);
}
```

**Python**
```python
results = client.ingest.ingest_batch(files)
for r in results:
    if r.error:
        print("FAILED", r.filename, r.error)
```

**.NET**
```csharp
var results = await client.Ingest.IngestBatchAsync(files);
foreach (var r in results.Where(r => r.Error is not null))
    Console.WriteLine($"FAILED {r.Filename}: {r.Error}");
```

**Java**
```java
var results = client.ingest.ingestBatch(files);
for (var r : results) {
    if (r.error() != null) System.out.println("FAILED " + r.filename() + ": " + r.error());
}
```

## Bulk upload sessions: manifest-driven, resumable, verified

For a corpus, batches alone leave you bookkeeping: what is already up, what
failed, whether the load is actually complete. A **bulk upload session**
moves that bookkeeping server-side. REST-only (typed `Unsupported` on
gRPC). The lifecycle:

1. **Open** with the full manifest (filename, sha256, length, media type
   per entry). The server diffs it against stored sources — entries whose
   path already holds those exact bytes are `already_present` — and answers
   with `needed`: the upload work list.
2. **Chunk** the needed files up in lists of **at most 500** (the same
   client-side typed guard as batch; per-document idempotent, per-item
   outcomes).
3. **Status** at any time; `include_needed` returns the remaining work list
   — the resume point after a crash.
4. **Complete**: the server verifies every manifest entry is stored AND
   hash-matched. `completed` closes the session; `incomplete` leaves it
   open and names exactly what is `missing` or `mismatched` (a path someone
   overwrote with different bytes after the manifest declared it).

The payoff is the **zero-byte re-run**: run the same load twice and the
second open answers `needed: []` — nothing uploads, complete verifies, and
the load is provably done. Idempotence you can see, not assume.

**Rust** (abridged from `examples/bulk_upload.rs`)
```rust
let open = client.ingest.bulk_open(dto::BulkOpenRequest {
    files: manifest, label: Some("corpus-2026-08".into()) }).await?;
if !open.needed.is_empty() {
    let resp = client.ingest.bulk_chunk(&open.bulk_id, needed_files).await?;
    println!("{} stored, {} failed", resp.stored, resp.failed);
}
let done = client.ingest.bulk_complete(&open.bulk_id).await?;
assert_eq!(done.status, "completed"); // else: done.missing / done.mismatched
```

**Python**
```python
open_ = client.ingest.bulk_open(manifest, label="corpus-2026-08")
if open_.needed:
    resp = client.ingest.bulk_chunk(
        open_.bulk_id, [f for f in files if f["filename"] in set(open_.needed)])
    print(resp.stored, "stored,", resp.failed, "failed")
done = client.ingest.bulk_complete(open_.bulk_id)
assert done.status == "completed", (done.missing, done.mismatched)
```

**.NET**
```csharp
var open = await client.Ingest.BulkOpenAsync(manifest, label: "corpus-2026-08");
if (open.Needed.Count > 0)
{
    var needed = open.Needed.ToHashSet();
    var resp = await client.Ingest.BulkChunkAsync(
        open.BulkId, files.Where(f => needed.Contains(f.Filename)).ToList());
    Console.WriteLine($"{resp.Stored} stored, {resp.Failed} failed");
}
var done = await client.Ingest.BulkCompleteAsync(open.BulkId);
// done.Status: "completed" | "incomplete" (+ Missing / Mismatched lists)
```

**Java**
```java
var open = client.ingest.bulkOpen(manifest, "corpus-2026-08");
if (!open.needed().isEmpty()) {
    var needed = Set.copyOf(open.needed());
    var chunk = files.stream().filter(f -> needed.contains(f.filename())).toList();
    var resp = client.ingest.bulkChunk(open.bulkId(), chunk);
    System.out.println(resp.stored() + " stored, " + resp.failed() + " failed");
}
var done = client.ingest.bulkComplete(open.bulkId());
// done.status(): "completed" | "incomplete" (+ missing() / mismatched())
```

## Where did it go? `get_source`

Metadata for one stored source by id — never the bytes: logical path,
content hash, storage backend, and the extraction status/method
(`text | docx | pdf-text | ocr` — OCR'd text is not equivalent evidence).
REST-only.

**Rust**
```rust
let info = client.ingest.get_source(&res.source_id.unwrap()).await?;
println!("{} on {} ({:?})", info.filename, info.storage_backend, info.extraction_status);
```

**Python**
```python
info = client.ingest.get_source(res.source_id)
print(info.filename, info.storage_backend, info.extraction_status)
```

**.NET**
```csharp
var info = await client.Ingest.GetSourceAsync(res.SourceId!);
Console.WriteLine($"{info.Filename} on {info.StorageBackend} ({info.ExtractionStatus})");
```

**Java**
```java
var info = client.ingest.getSource(res.sourceId());
System.out.println(info.filename() + " on " + info.storageBackend());
```

Notes:

- The file/bulk writes are **deadline-exempt** like `put_source` (bulk
  bodies run to 256 MiB) — only your cancellation bounds them; they are
  sent once, never auto-retried (re-running them is safe by content
  address, which is what bulk's `needed` diff automates for you). The gRPC
  `IngestFiles` twin (single + batch) is deadline-exempt too — a 500-file
  message runs to the same ceiling.
- **.NET, caller-supplied `HttpClient`:** the client never mutates a
  handed-in `HttpClient`, so its own `Timeout` (default 100 s) still caps
  these deadline-exempt sends. Set `httpClient.Timeout =
  Timeout.InfiniteTimeSpan` on it if you use file/batch/bulk ingest or
  streaming source upload; the `HttpClient` the library creates already
  has that.
- `existed: true` means a genuine idempotent replay — same path, same
  bytes. Same path with NEW content reports `existed: false` because a
  rebuild is now owed.
- The bulk session expires (`expires_at` on status); an expired session
  refuses further chunks — reopen with the same manifest and only the
  still-missing entries come back `needed`.
