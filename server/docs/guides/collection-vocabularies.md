# Collection vocabularies and file references (Server 1.2)

Server owns vocabulary generation, storage and query expansion. The application
owns user authentication, file permissions, original-file retention and its viewer.
A citation is a reference to a pinned source, never a download URL or permission grant.

## Versioned API

All requests require bearer authorization and `X-Munarium-Uid`, including reads.
Existing `/v1` routes remain available. Every operation below also has a named
RPC on `mmp.v1.ServerApiService` and methods in all four Server SDKs. See the
[complete API client guide](../../../clients/docs/guides/server-1.2.md) for
the transport contract, method names and language examples.

| Method and path | Authorization | Behavior |
|---|---|---|
| `GET /v1.2/vocabulary-settings` | Static control-plane credential | Read tenant defaults and revision |
| `PUT /v1.2/vocabulary-settings` | Static `rw` | Replace defaults using the last revision |
| `GET /v1.2/collections/{id}/vocabulary` | `vocabulary` capability or static `rw` | Read terms, configuration, generation status and revision |
| `PUT /v1.2/collections/{id}/vocabulary` | Same, with collection clearance | Replace terms and configuration |
| `PATCH /v1.2/collections/{id}/vocabulary` | Same | Update enabled, auto_generate or groups |
| `POST /v1.2/collections/{id}/vocabulary/refresh` | Same | Generate and activate a replacement from samples |
| `GET /v1.2/collections/{id}/vocabulary/revision` | `query`, with collection clearance | Read revision only, for answer-cache invalidation |
| `POST /v1.2/search` | `query` | Search an explicit collection and optional pinned index with its vocabulary |
| `POST /v1.2/answers` | `query` for every supplied collection | Verify pinned passages, generate a narrative and return checked citations |

Collection identifiers accept IDs or names. A `vocabulary` capability grants no
query, ingest, provider configuration or token-minting privilege. A query capability
cannot read or edit the vocabulary itself. Compartment and access-level checks apply
to each operation; a different tenant or inaccessible collection is not disclosed.

## Defaults, generation and editing

`GET /v1.2/vocabulary-settings` starts with:

```json
{
  "auto_generate": true,
  "sampling": {
    "document_count": 12,
    "balance_document_types": true,
    "media_types": [],
    "characters_per_document": 4000
  },
  "provider": "default",
  "tier": "fast",
  "max_groups": 40,
  "revision": 0
}
```

The provider is a named ProviderConfig or the reserved `default` configuration;
its selected tier is also used by `/v1.2/answers`. Credentials stay in the existing
provider secret mechanism. The completion token budget is the tenant's existing
`complete_default` budget. Set a suitable budget before generating large vocabularies.
Generation invokes that model and may incur provider charges. Set `auto_generate`
to false before ingest if the deployment must not send samples automatically.

The worker observes bound source documents after ingest, debounces new bindings
for 60 seconds and scans every 30 seconds. It does not block ingest or index building.
It retries failed attempts after ten minutes; a five-minute lease prevents concurrent
replicas from publishing competing results. Collection settings, defaults and source
hashes are checked again before a result is committed. A stale generation never
replaces a newer edit. A request that times out can leave a lease until it expires;
reload the status before retrying.

Generation is enabled by default for active collections, including existing collections
after upgrade. Existing generated vocabularies refresh when bound source identities or
hashes change. Manual term edits are retained until an explicit refresh; changing only
enablement or sampling preserves the current origin. `enabled:false` stops vocabulary
application and automatic generation while retaining terms. `auto_generate:false`
stops background generation independently of whether saved terms are applied.

Sampling selects at most 12 files by default, alternating deterministically across
media types rather than taking only the most numerous format. `media_types:[]`
includes every available format. The filter uses the media type submitted at ingest;
an application that submits extracted text will be sampled as text. Long files contribute
bounded beginning, middle and ending excerpts. The same extraction pipeline and source
SHA-256 verification used for indexing are used for sampling. Limits are 1–100 files,
256–20,000 characters per file, and a combined 200,000-character sample budget.

A collection can override defaults with `auto_generate` and `sampling`; null inherits.
For example, after reading revision 3:

```json
{
  "revision": 3,
  "enabled": true,
  "auto_generate": null,
  "sampling": null,
  "groups": [["purchase order", "PO"], ["invoice", "supplier bill"]]
}
```

Send this to PUT. PATCH requires `revision` and the fields being changed. Use PUT to
restore inheritance; PATCH's omitted/null fields remain unchanged. A stale revision
returns `400 invalid-input`; reload and reconcile instead of overwriting.

Groups contain 2–12 distinct phrases, at most 120 Unicode characters each; there may
be at most 200 groups. Each generated group must include wording found in its samples.
This validation does not prove that every proposed synonym is semantically equivalent:
review and edit the terms for the collection. Unrelated concepts should remain separate.

Refresh accepts `{"revision":4,"source_ids":[]}`. An empty list uses sampling defaults;
an explicit list must contain unique source IDs bound to that collection and allowed by
its sampling policy. The result replaces and activates the saved terms (unless application
is disabled). Refresh is a direct server operation, not an application-side draft.

## Applying a vocabulary without widening file access

```json
{
  "query": "PO approval",
  "collection": "manuals",
  "index_version": "the-pinned-index-id",
  "top_k": 8
}
```

`POST /v1.2/search` returns the existing hits and provenance envelope, plus
`expanded_query` and `vocabulary_revision`. Equivalent phrases are added when a
case-insensitive whole phrase matches the question. Expansion does not choose a
new collection, index or governing version.

Applications with one physical index per immutable file can supply a separate
`vocabulary_collection` shared by those indexes. The caller must independently have
clearance for both collections. Bind eligible sampling files to that shared collection;
it does not need its own index. Disable automatic generation on the physical collections
to avoid generating one vocabulary per file. Cache keys must include the vocabulary
revision and should recheck it before returning a newly generated answer.

Collection-filtered `/v1/search` and session retrieval also apply the native collection
vocabulary. Session completion retains its existing runbook model-routing semantics.
The original question remains the answer prompt; expanded terms are retrieval hints.

## Narrative answers and citations

`POST /v1.2/answers` accepts `question` and `sources`, where each item has a caller
citation `id`, `collection`, `index_version`, `source_id`, `source_path`,
`source_content_hash` and exact `text`. The server checks each supplied passage against
the pinned retrieval index before calling the configured model. Optional
`expected_provider` and `expected_model` reject a mismatch with the caller's processing
policy before any provider call. They pin the configuration; they do not select a model.
The current lookup searches the selected index with the passage and considers up to
100 hits; an unresolvable passage is rejected rather than trusted.

The response has `api_version:"1.2"`, `content:{status,answer,citations}`, `references`,
provider/model and token usage. A supported answer has a narrative and 1–4 exact
quotations with citation IDs. The model cannot supply source identities: the server
constructs references from verified hits and includes only cited IDs. It checks current
capability and collection clearance again after completion. Exact-quote validation
checks provenance; it does not prove every narrative claim is semantically correct.

Display `content.answer` once. Group supporting citations below it by the immutable
source version, retaining distinct passages and separately identified amendments.
Do not turn each retrieved chunk into a competing answer.

## Mapping references to original files

### Collection queries (unreleased 1.2.1)

`POST /v1.2/query` accepts `question`, `collections`, and optional `effective_on`
in ISO calendar-date form. Send keyword topics unchanged. The query capability
supplies the user's clearance and compartments. The request does not accept
passages, file allowlists, index pins, model overrides, or prompts. Server applies
the collection vocabulary, chooses governing publications, retrieves and ranks
their passages, and returns the same checked narrative/reference envelope.

A publisher with a static `rw` credential manages publication snapshots through
`GET` and `PUT /v1.2/collections/{id}/governance`. A snapshot contains a revision,
all retained publication records, dated document relationships, and query policy.
Each record has `id`, `document_id`, internal `collection`, `index_version`,
`source_id`, `source_content_hash`, `effective_from`, optional `effective_until`,
`published_at`, and `state` (`approved`, `superseded`, or `withdrawn`). Server checks
the pinned source against an activated internal index before accepting an active record.
Those internal indexes remain inaccessible to a query capability containing only
the parent collection's compartment; the author's explicit binding delegates
their use through Server's governed query. A child cannot require a higher access
level than the parent.

PUT uses the last revision and returns an incremented revision; stale writes fail
with `409 head-conflict`. Retained identities, source hashes and publication dates
cannot be rewritten, withdrawn records cannot be reactivated, and missing records
must be retained as withdrawal tombstones. Migration 0034 stores append-only
snapshots. Rebuilding an index may update its pin while preserving source identity
and bytes. Existing indexes do not need rebuilding merely to register governance.

Server chooses the latest eligible publication in each document family before
checking expiry or withdrawal. It never revives an older version to fill a gap.
Ambiguous versions, unresolved supersession chains, conflicts, and missing
prerequisites require review. Dated amendments to selected content also require
review. When authorized passages are available, Server still asks the model to
explain their content and the review qualification, while retaining the review
status. It does not ask the model to adjudicate precedence.
Clearance and snapshot revisions are checked again after model completion. A
concurrent policy change rejects the in-flight result.

Query policy defaults are: enabled, historical clearance level 2, 24 passages,
60,000 serialized context characters, retrieval concurrency 20, and 12 candidates
per internal index. These bound each answer's work, not the number of documents
in a collection. Provider and tier inherit the Server vocabulary model settings;
the publisher can configure them per collection. External processing is disabled
unless explicitly allowed. Multiple selected collections must agree on model
routing; restrictive processing and context/concurrency settings apply together.

`query.model_routes` optionally overrides provider, tier, context budget, output
token budget and enabled state for an exact `access_level`. A higher clearance
does not inherit a lower clearance's route. Duplicate levels are rejected.
`query.max_output_tokens` defaults to the tenant completion budget when omitted.
Automatic and manual vocabulary generation use the collection's base provider
and tier, with the same external-processing policy. A governance revision change
during generation discards the result.

Collection queries rank comparable scores together. Datastore BM25 scores from
different indexes have separate statistical domains; equally ranked hits from
those domains receive equal lexical contributions. Comparable vector scores can
then distinguish relevance across files. Index names and completion order do not
give early files priority. Legacy search/session fusion retains its existing policy.

The `content.answer` field contains the model's explanation, including partial
findings and gaps. `insufficient` and `review` may also carry explanatory prose
and verified citations. Consumers should display that explanation and its file
references, rather than replace it with a generic search-result message. Each
citation still has to match a pinned passage. A supported answer requires at
least one verified citation; a description of missing information need not cite
a nonexistent fact.

Server requests structured output for answer and vocabulary protocols using
[OpenAI](https://developers.openai.com/api/docs/guides/structured-outputs),
[OpenRouter](https://openrouter.ai/docs/guides/features/structured-outputs),
[Anthropic](https://platform.claude.com/docs/en/build-with-claude/structured-outputs)
and [local Ollama](https://docs.ollama.com/capabilities/structured-outputs)
schema controls. Select a model/endpoint that supports them. The schema organizes
the response; it does not verify its factual meaning. Server continues checking
status, citation identities, exact quotes, scope and concurrent policy changes.
Ordinary provider completion requests retain their existing response format.

For OpenRouter, `ProviderConfig.spec.openrouterProvider` can select one downstream
provider slug. Server sends `only`, disables fallback, requires parameter support
and requests `data_collection: deny`. This is an explicit routing request, not an
independent guarantee about a provider's retention practices. Omitting the field
retains the existing provider routing behavior.

`GET /v1.2/collections/{id}/publications/{publication_id}` authorizes an original
for the caller's current collection clearance and optional `effective_on` date.
It returns the publication identity, never file bytes or a download URL. Use it
when opening a saved citation to recheck Server's current governance. Disabling
queries does not by itself delete retained originals or authorize historical
access. The ingesting application still checks its own current membership and
the returned identity/hash before serving the retained original. A URL is not
an access grant. All four new operations have named native RPCs and generated
methods in each of the four Server SDKs.

1.2.1 remains a candidate: these source contracts are not available in the
published 1.2.0 image. Release qualification and consumer migration must finish
before switching an application to these operations.

Keep the ingest response's `source_id`, `filename` and `sha256` against the application's
immutable file record. Source identity is `src-` followed by the first 16 hex characters
of SHA-256 over `{tenant}/{logical-path}`. The content hash is separate: equal bytes at
two paths are distinct sources. A chunk ID is `{source_id}#{ordinal}`; the ordinal is
not a page number and is meaningful only with its pinned index version.

An answer reference includes citation ID, collection ID/name, index version, source ID,
logical path, indexed source SHA-256, chunk ID and optional metadata. Resolve it against
the application's stored mapping, compare the indexed hash and enforce current access.
Open the retained original for that version. Do not silently substitute a newer file
from a connector whose contents have changed. Sharing a citation URL grants no access.

New 1.2 indexes populate `metadata.extraction_method` and `metadata.location`:

```json
{
  "extraction_method": "pdf",
  "location": {
    "utf8_start": 240,
    "utf8_length": 180,
    "character_start": 238,
    "character_length": 176,
    "unit": "page",
    "numbers": [2]
  }
}
```

Offsets are zero-based, half-open spans in extracted text. UTF-8 offsets count bytes;
character offsets count Unicode scalar values, not JavaScript/.NET UTF-16 code units.
Location numbers are the extractor's one-based page numbers for PDF and paragraph
numbers for DOCX (`unit:"paragraph"`), never invented Word page numbers. Plain text has
no original page numbers. A chunk may span multiple pages. Normalized, noncontiguous
chunks have a null location; the server does not guess offsets. No arbitrary document
or chunk tags are introduced; keep application labels keyed by source ID.

PostgreSQL and Datastore return the same pinned metadata. New Datastore records store
the indexed source hash; older artifacts use the immutable pinned chunk rows as a
compatibility lookup, never the current mutable source hash. Pre-1.2 indexes have no
location metadata; build a new index to obtain it. This is not a backfill that rewrites
existing index identities. The chunker identity advances to `para@2` for these builds,
and new artifact physical plans identify `munarium-records@2`; older records remain
readable. Retain PostgreSQL chunk rows while serving pre-1.2 artifacts.

## Upgrade and rollback

Back up PostgreSQL and source/artifact stores. Upgrade with automatic generation
disabled first if existing collections need sampling-policy review. Additive migrations
0032 and 0033 create chunk provenance and vocabulary tables; they do not rewrite originals
or old indexes. Configure the provider, migrate existing application vocabularies through
the API and verify scope/identity mapping before enabling new clients and automatic work.

Server 1.1 clients keep their `/v1` contracts. A 1.2 application must not be pointed at a
1.1 server because its versioned answer/vocabulary routes do not exist there. Roll back
application and server together using the recorded database backup when necessary;
do not remove migration rows or downgrade a database in place. The older sqlx migrator
rejects a database containing migrations it does not recognize.
