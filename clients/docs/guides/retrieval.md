# Retrieval: hybrid search and the ProvenanceEnvelope

Search runs over **versioned immutable indexes** (tsvector lexical +
pgvector fused by RRF). Build an index side-by-side and flip it atomically;
search the active index or pin a specific `index_version`. Requires the
postgres store.

**Every answer carries a ProvenanceEnvelope** — the chunk ids, source
content hashes, index version, and the ledger event watermark the index
reflects. It is a required member on every client's result type: render it,
log it, store it beside the answer. Reproducibility is the product.

**Rust**
```rust
let client = MunariumClient::rest(
    MunariumClientOptions::new("http://127.0.0.1:8080").token("devtoken").uid("user-1"))?;
let iv = client.retrieval.build_index("support-tickets@1", Some(&v)).await?;
let res = client.retrieval.search(dto::SearchRequest {
    query: Some("printer".into()),
    shape_ref: Some("support-tickets@1".into()),
    top_k: Some(5),
    ..Default::default()
}).await?;
println!("index {} @ watermark {} — sources: {:?}",
    res.envelope.index_version, res.envelope.event_watermark,
    res.envelope.source_paths);   // which documents answered
```

**Python**
```python
client = MunariumClient.rest(
    ClientOptions("http://127.0.0.1:8080", token="devtoken", uid="user-1"))
client.retrieval.build_index("support-tickets@1", version_id=v)
res = client.retrieval.search(query="printer", shape_ref="support-tickets@1", top_k=5)
e = res.envelope
print(f"index {e.index_version} @ watermark {e.event_watermark}")
```

**.NET**
```csharp
await using var client = MunariumClient.Rest(new MunariumClientOptions
    { Endpoint = "http://127.0.0.1:8080", Token = "devtoken", Uid = "user-1" });
await client.Retrieval.BuildIndexAsync("support-tickets@1", v);
var res = await client.Retrieval.SearchAsync("printer", "support-tickets@1", topK: 5);
Console.WriteLine($"index {res.Envelope.IndexVersion} @ {res.Envelope.EventWatermark}");
```

**Java**
```java
client.retrieval.buildIndex("support-tickets@1", v);
var res = client.retrieval.search(
        Params.SearchQuery.of("printer", "support-tickets@1").withTopK(5));
var e = res.envelope();
System.out.println("index " + e.indexVersion() + " @ " + e.eventWatermark());
```

Notes:

- **Index builds are REST-only today** — the gRPC clients throw/return a
  typed `Unsupported` error rather than pretending.
- Search is classified as a *read* (it just happens to be a POST): it gets
  the same transient-retry policy as GETs.
- The `filter` member accepts exactly one shape,
  `{"collections": ["<name-or-id>"]}`, which routes the search to that
  collection's partitioned index. Any other shape is rejected explicitly
  (`invalid-input`) rather than silently ignored, and the clients pass the
  member through so you see that rejection.
- `search` on the memory store returns `invalid-input` ("retrieval requires
  the postgres store").
