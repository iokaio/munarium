# Complete Server API and collection vocabularies

Server **1.2.0** and Server client packages **1.1.0** introduced a complete named API
client in Rust, Python, .NET and Java. Every documented REST operation has a
named RPC on `mmp.v1.ServerApiService` and a corresponding SDK method on
`ServerApiClient`. Python also supplies `AsyncServerApiClient`; Java methods
have `CompletableFuture` variants; Rust and .NET methods are asynchronous.

The existing `MunariumClient` typed planes remain compatible with Server 1.1.
Their historical transport gaps do not describe the complete API client: use
`ServerApiClient` for vocabulary, answers, reports, authoring, bulk ingest,
index administration, source metadata and the complete provider surface over
either transport. New gRPC operations require Server 1.2; they are not emulated
with HTTP network calls against an older Server.

**Server 1.2.1** extends this surface to 121 named
operations. All four SDKs include `QueryCollections`, `GetCollectionGovernance`,
`ReplaceCollectionGovernance` and `AuthorizePublication` (using each language's
naming convention) over REST and gRPC. These operations require 1.2.1; a 1.2.0
server does not supply them. The current SDK source targets Server **1.2.1**;
client source package versions are **1.1.1**. All four clients are published;
see [installation and publication](../../README.md#installation-and-publication)
for registry links and available versions.

A collection query supplies `question`, `collections` and optional `effective_on`.
The capability carries the acting user's clearance and collection compartments.
Server owns governing versions, vocabulary application, retrieval and answer
composition. Applications transfer authoring metadata separately through the
governance API; they do not send selected passages with a collection query.

Display `content.answer` as the explanation and list `references` underneath it.
The `insufficient` and `review` statuses may also contain explanatory prose and
verified references. Retain those explanations. References identify indexed
sources, never download URLs. The ingesting application maps them to retained
originals and rechecks access with `AuthorizePublication` before serving a file.
References carry internal source collection/index identities, so retain the
publisher's mapping to the parent collection and publication ID required by that
call. The query response also carries `effective_on`, `governance_revisions` and
`vocabulary_revisions`; revision maps use parent collection IDs.
See the [governance and answer contract](../../../server/docs/guides/collection-vocabularies.md).

## Contract and representation

[`server-api.json`](../../server-api.json) lists every method, RPC name, route,
request media type and streaming flag. The normative payload schemas remain in
[OpenAPI](../../../server/docs/api/openapi.json). Request objects carry:

- `path`: unescaped named path parameters, such as the collection `id`;
- `query`: ordered name/value pairs, preserving repeated query parameters;
- `body`: UTF-8 JSON, YAML or binary bytes;
- `content_type`: optional override of the operation's request media type;
- `source_headers`: only `x-filename`, `x-content-sha256`, `x-shape-ref`;
- `idempotency_key`: optional explicit command key.

JSON helpers preserve explicit `null`, omitted fields and 64-bit integers.
The protobuf carries bytes instead of `google.protobuf.Struct`, whose number
representation cannot preserve all 64-bit integer values. Response objects
expose status, media type and body, plus a JSON decoder. Requests use the same
schemas on both transports; there are no separately maintained copies of the
vocabulary and answer JSON schemas in each language.

Each RPC selects a fixed local route through the same server handlers,
authorization, body limits, revision checks and audit capture as REST. There is
no network proxy, arbitrary target URL, elevated bearer credential or
client-supplied tenant override. Bearer credentials and user identity travel in
normal transport metadata. Source headers cannot replace them.

Rich gRPC errors preserve the REST problem category and extension names.
Trailer size limits require large findings lists to be capped: `findings_total`
and `findings_truncated` identify this case. Oversized messages or other
extensions carry `detail_truncated` or `metadata_truncated`; omitted details
are never presented as a complete list.

Calls are sent once. Writes, paid model calls and interrupted streams are never
automatically replayed. GET requests use the configured request timeout; paid
or streaming calls are bounded by explicit cancellation and server budgets.

`turn_stream` / `TurnStreamAsync` is a server-streaming RPC. Each result contains
an incremental body fragment with the same SSE event framing as REST. Consumers
must parse across fragment boundaries rather than assume one fragment is one
event. Dropping/disposing a stream releases its transport resources; it does
not promise to undo a model call already sent by the server.

## Python

```python
from munarium_client import ApiRequest, ClientOptions, ServerApiClient

api = ServerApiClient(
    ClientOptions("https://server.example.invalid", token=token, uid="admin-1"),
    grpc_transport=True,
)
try:
    request = ApiRequest(path={"id": collection_id})
    vocabulary = api.get_collection_vocabulary(request).json()
    updated = api.update_collection_vocabulary(ApiRequest.json(
        {"revision": vocabulary["revision"], "enabled": False},
        path={"id": collection_id},
    )).json()
finally:
    api.close()
```

For REST, omit `grpc_transport=True` and supply the HTTP API origin. The async
class takes the same arguments; await its calls and `close()`.

## .NET

```csharp
await using var api = ServerApiClient.Grpc(new MunariumClientOptions {
    Endpoint = "https://server.example.invalid", Token = token, Uid = "admin-1"
});
var path = new Dictionary<string, string> { ["id"] = collectionId };
var vocabulary = (await api.GetCollectionVocabularyAsync(new ApiRequest { Path = path })).Json();
await api.UpdateCollectionVocabularyAsync(ApiRequest.Json(new {
    revision = vocabulary.GetProperty("revision").GetInt64(), enabled = false
}, path));
```

Use `ServerApiClient.Rest` for REST. Cancellation tokens apply to every call.

## Rust

```rust,no_run
use munarium_client::{MunariumClientOptions, server_api::{ApiRequest, ServerApiClient}};
# async fn example(token: &str, collection_id: &str) -> munarium_client::Result<()> {
let api = ServerApiClient::grpc(
    MunariumClientOptions::new("https://server.example.invalid").token(token).uid("admin-1")
).await?;
let vocabulary: serde_json::Value = api.get_collection_vocabulary(
    ApiRequest::default().with_path("id", collection_id)
).await?.json()?;
api.update_collection_vocabulary(ApiRequest::json(&serde_json::json!({
    "revision": vocabulary["revision"], "enabled": false
}))?.with_path("id", collection_id)).await?;
# Ok(()) }
```

Use `ServerApiClient::rest` for REST. Dropping the call future cancels the client wait.

## Java

```java
try (var api = new ServerApiClient(
        MunariumClientOptions.of("https://server.example.invalid")
            .withToken(token).withUid("admin-1"), true)) {
    var input = ServerApiClient.ApiRequest.empty().withPath("id", collectionId);
    var vocabulary = api.getCollectionVocabularyAsync(input).get().json();
    var patch = new ObjectMapper().createObjectNode()
        .put("revision", vocabulary.get("revision").asLong()).put("enabled", false);
    api.updateCollectionVocabulary(ServerApiClient.ApiRequest.json(patch, Map.of("id", collectionId)));
}
```

Pass `false` for REST. Each client owns and closes its connections and virtual-thread executor.

## Coverage and exceptions

The contract generator and CI drift check compare all documented REST operations
with the generated native RPC and four SDK method surfaces. Live conformance
probes every named RPC and exercises vocabulary edits, typed errors and source
identity on both transports. Server tests exercise generation, authorization,
revision conflicts, search application and checked answers over actual gRPC.

HTML operator pages, Swagger UI and Prometheus exposition are browser/tool
representations, not additional programmatic Server API operations. gRPC health
and reflection remain available alongside the named API. Previously declared
tenant-management placeholders on `AdminService` have no implemented REST
operation; this release does not create a new tenant-management feature.

See [collection vocabularies](../../../server/docs/guides/collection-vocabularies.md)
for generation defaults, sampling, per-collection controls, authorization and
opaque citation references. Original files remain the ingesting application's
responsibility; no SDK reference grants permission to download one.
