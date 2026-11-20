# Semantic indexing and reranking through an inference endpoint

**ClearView IR-0 to IR-4, I1, I2, SC2 (Server 1.5).** Server can send an enrolled
collection's query and passage embeddings, and the reranking of its fused
candidates, to a self-hosted inference endpoint. Munarium Lens is the
reference endpoint. Any server implementing the same routes and passing the
probe can be used. The design is in
[airk-analysis.md §8](../design/airk-analysis.md#8-semantic-embedding-and-reranking-through-an-inference-endpoint).

Nothing changes on upgrade. Installation alone downloads no model, makes no
inference call, rebuilds no corpus and changes no index id. Every collection
keeps the v1.4 hash path until an operator completes all of the steps
below.

## Contents

1. [What you need](#what-you-need)
2. [Turn the capability on](#turn-the-capability-on)
3. [Apply and probe the endpoint configuration](#apply-and-probe-the-endpoint-configuration)
4. [Bind a collection](#bind-a-collection)
5. [Build the semantic index](#build-the-semantic-index)
6. [Compare in shadow](#compare-in-shadow)
7. [Enroll and verify](#enroll-and-verify)
8. [Outage policy](#outage-policy)
9. [Shared and dedicated placement](#shared-and-dedicated-placement)
10. [Roll back](#roll-back)
11. [Budgets, attribution and shadow allocation](#budgets-attribution-and-shadow-allocation)
12. [The endpoint protocol Server speaks](#the-endpoint-protocol-server-speaks)
13. [Limitations](#limitations)

## What you need

- The PostgreSQL store. Bindings, probes, attempts and semantic vectors are
  tables (migration `0080_inference_stages.sql`; the binding's placement
  requirement is migration `0081_inference_placement.sql`).
- An endpoint serving an embedding model at **1,024 dimensions** (the one
  semantic dimension this release stores), a reranking model, or both.
- A management (`mgmt`) token for the operations below and an `rw` token for
  the build job.
- A builder process (`MUNARIUM_DATASTORE_BUILDER=enabled`) to run the queued
  semantic build.

## Turn the capability on

| Setting | Default | Meaning |
|---|---|---|
| `MUNARIUM_INFERENCE` | `disabled` | `enabled` lets this process call inference endpoints. `disabled` sends nothing: probes are refused, and an enrolled collection's stages report `disabled` (a refusal under `require`). Rollback works in either setting. Any other value stops startup. |
| `MUNARIUM_INFERENCE_CONCURRENCY` | `8` | Outbound inference calls in flight per process (1 to 1,024). A call waits for a slot no longer than its own request deadline, then fails as `unavailable` with reason `queue_deadline`. |

Set the same values on every replica that serves enrolled collections and on
the builders.

## Apply and probe the endpoint configuration

Apply a provider configuration of kind `openai-compatible` with
`POST /v1/providers`:

```yaml
apiVersion: munarium.ioka.io/v1
kind: ProviderConfig
metadata: {name: lens}
spec:
  provider: openai-compatible
  endpoint: https://lens.internal.example/v1   # /v1 is applied once
  credentialRef: {env: MUNARIUM_SECRET_LENS}   # optional bearer credential
  models:
    embed: [qwen3-embedding-0.6b-fp16-1024d]
    rerank: [qwen3-reranker-0.6b-fp16]
  inference:
    embedDimensions: 1024      # required with models.embed
    maxBatch: 32               # inputs per embedding request
    maxRequestBytes: 65536     # larger requests are refused, never split mid-input
    maxAttempts: 2             # physical attempts per call within one deadline (1-3)
    allowTestBackend: false    # true accepts "retrieval_model": false (test doubles only)
  budgets:
    dailyTotalTokens: 2000000  # required: the tenant budget that pays
    shadowDailyTokens: 100000  # optional: the shadow allocation inside that cap
```

The kind requires an explicit HTTP(S) endpoint with no credentials, query or
fragment, and `budgets.dailyTotalTokens`: a configuration without the daily
cap is refused when applied, because every inference attempt reserves against
it. It serves embed and rerank models only, is never chosen by default
provider selection, and needs a credential only when the endpoint requires one.

An `openai-compatible` configuration is reachable **only through the governed
inference path** described in this guide: the probe, collection bindings,
semantic builds and enrolled reads, behind the deployment switch, the
placement check, a current probe and the daily cap. The generic provider
routes `POST /v1/providers/{name}/embed`, `POST /v1/providers/{name}/complete`
and `GET /v1/providers/{name}/health` (and their `ProviderService` RPCs)
refuse such a configuration with `inference-not-qualified` (409) before any
network call, whatever the switch, placement or budget. The paid `/healthai`
diagnostics never select it (it serves no completion tier).
`GET /v1/providers` and `GET /v1/providers/{name}/diagnostics` make no
provider call and list it as before. The probe and the inference stages also
refuse a stored configuration without a daily cap (one applied before the
rule): the probe with `inference-not-qualified`, the stages as `unconfigured`.

Probe it:

```console
curl -X POST -H "Authorization: Bearer $MGMT" \
  https://server.example/v1/providers/lens/inference-probe
```

The probe checks discovery (`GET /v1/models` lists each configured model for
the right stage, at the declared dimension, and as a retrieval model unless
`allowTestBackend` is set), one embedding of two fixed synthetic passages, and
one rerank of three. It records the result with the endpoint fingerprint and
the hash of the configuration text, timed at the probe's start on the
database clock. **A probe is valid only for that text, and only when it
started after the text's latest change**: any change to the configuration is
drift, including a revert to an earlier text, and the stages report
`probe_failed` (`no_current_probe`) until a new probe passes. Re-applying
identical text is not a change. A failed probe changes nothing else.

## Bind a collection

```console
curl -X PUT -H "Authorization: Bearer $MGMT" -H "Content-Type: application/json" \
  https://server.example/v1/collections/handbook/inference -d '{
    "expected_revision": 0,
    "provider_config": "lens",
    "embedding": {"model": "qwen3-embedding-0.6b-fp16-1024d",
                  "query_instruction": "Instruct: Given a web search query, retrieve relevant passages that answer the query\nQuery:"},
    "rerank": {"model": "qwen3-reranker-0.6b-fp16", "max_candidates": 20,
               "instruction": "Given a web search query, retrieve relevant passages that answer the query"},
    "shadow": {"semantic_index": true, "rerank": true},
    "timeout_ms": 5000
  }'
```

- `expected_revision` is compare-and-set: 0 creates the binding, a stale
  value is `head-conflict`. Every change appends a history row.
- The configuration must set `dailyTotalTokens` (it cannot be applied
  without it); shadow switches need `shadowDailyTokens`.
- The embedding model, instructions, configuration name, endpoint
  fingerprint, dimension, L2 normalization and no-truncation rule together
  are the **semantic space**. Its digest (`sem-…`) is part of the semantic
  index's identity, so a change of any of them needs a new index.
- The rerank `instruction`, when set, is part of the **reranking stage
  identity**: the model name followed by `#instruct-` and a digest of the
  instruction (receipts and `stages[].identity` never carry its text).
  Without it the identity is the model name, as before. It does not affect
  the semantic space, so changing it needs no new index. See
  [Qwen3 instruction settings](#qwen3-instruction-settings).
- `policy` per stage is `require` when omitted. See [Outage policy](#outage-policy).
- `max_candidates` (2 to 100, default 20) bounds the reranked head;
  `max_document_bytes` (default 8,192) refuses, never truncates, a longer
  candidate.

A binding in state `bound` serves nothing: reads stay on the hash path.

### Qwen3 instruction settings

The recommended model pair is Qwen3-Embedding-0.6B and Qwen3-Reranker-0.6B.
Under hub ADR 0029 (instruction-prefix ownership), Server owns retrieval
instructions and records them in the identity they affect; Lens, like any
endpoint, applies none. The settings take two forms:

| Setting | Form | What Server sends |
|---|---|---|
| `embedding.query_instruction` | The full literal prefix, labels and separators included. Server prepends it unchanged. | `{query_instruction}{query}` as each query `input` |
| `embedding.passage_instruction` | The full literal prefix. | `{passage_instruction}{passage}` as each passage `input` |
| `rerank.instruction` | The task text only. Server adds the labels. | `<Instruct>: {instruction}\n<Query>: {query}` as the rerank `query`; when unset, `{query}` unchanged |

Recommended values, with the model cards' example task:

| Setting | Value (JSON string) |
|---|---|
| `embedding.query_instruction` | `"Instruct: Given a web search query, retrieve relevant passages that answer the query\nQuery:"` |
| `embedding.passage_instruction` | Omit it: the card embeds documents without a prefix. |
| `rerank.instruction` | `"Given a web search query, retrieve relevant passages that answer the query"` |

- **No space after `Query:`.** The embedding card's query format is
  `Instruct: {task}\nQuery:{query}`. Because `query_instruction` is a literal
  prefix, end it with `Query:`, not `Query: `. A trailing space is a
  different prefix, so it defines a different semantic space and index, and
  it is not the card's input.
- **Server adds the rerank labels.** With `rerank.instruction` set, the
  query `orchid survey` is sent as
  `<Instruct>: Given a web search query, retrieve relevant passages that answer the query\n<Query>: orchid survey`.
  The endpoint's fixed template adds the system prompt, the role markers and
  `\n<Document>: ` before each document, so the scored prompt equals the
  Qwen3-Reranker card's byte for byte. The instruction is 1 to 1,024
  characters with no control characters, so it cannot add a label line of
  its own.
- **Unset is unchanged.** A binding without `rerank.instruction` sends the
  query exactly as before and keeps the model name as its identity. Existing
  bindings and semantic indexes keep their identities.
- **Replace the example task** with one that describes the collection's
  retrieval task once you have evidence that it helps; the cards recommend
  task-specific instructions written in English. Changing either instruction
  is a rebind (a new binding revision). An embedding instruction change also
  needs a rollback first and a new semantic index. A rerank instruction change
  takes effect on the next read and changes the reranking identity.

## Build the semantic index

The build is always a queued job:

```console
curl -X POST -H "Authorization: Bearer $RW" -H "x-munarium-uid: alice" \
  -H "Content-Type: application/json" https://server.example/v1/index-build-jobs \
  -d '{"kind": "semantic", "collection_id": "col-…"}'
```

The builder chunks the collection's complete source inventory into a **new
index version beside the active one**, then embeds every chunk through the
binding (passage instruction applied) in batches of `maxBatch`. Each batch is
reserved against the budget, attributed to the initiating caller (`alice`)
and the job id, and written under the job's fence. A cancelled, superseded
or budget-stopped job resumes where it stopped when re-enqueued. The job
never activates anything. Its result names the index and its coverage:

```json
{"index_version_id": "idx-…", "semantic_identity": "sem-…", "chunks": 812, "vectors": 812, "complete": true}
```

Hash-only collections and the hash index keep their v1.4 ids: the semantic
identity is appended only to the new index's identity.

## Compare in shadow

With `shadow.semantic_index`, every served read of a bound collection also
searches the built semantic index in a background task and records the top-k
overlap and top-1 agreement. With `shadow.rerank`, it reranks the served head
and records how many positions moved. Records hold a query fingerprint and
chunk identities, never text (`inference_shadow_comparisons`). Shadow work
can never change a response, runs only against a ready, probed endpoint, and
is metered from the shadow allocation.

## Enroll and verify

```console
curl -X POST -H "Authorization: Bearer $MGMT" -H "Content-Type: application/json" \
  https://server.example/v1/collections/handbook/inference/enroll -d '{"expected_revision": 2}'
```

Enrollment refuses (`inference-not-qualified`, 409) unless: inference is
enabled, the configuration has a current passing probe, the semantic index
matches the binding's current space and every chunk has a vector, and the
active index is a hash index to roll back to. It then records that index as
the **prior approved index**, activates the semantic index by pointer swap
and serves both stages.

Verify with `GET /v1/collections/{id}/inference` (binding, stage readiness
without calling the endpoint, coverage, active and prior index, today's
usage by purpose) and with a search. An enrolled collection's search
response always carries `execution.stages`, one receipt per configured stage:

```json
{"stage": "embedding", "policy": "require", "policy_revision": 3, "applied": true,
 "state": "ready", "reason": null, "fallback": null, "identity": "sem-…", "scored": null}
```

`envelope.index_version` names the semantic index and
`envelope.provider_fingerprint` is `endpoint/sem-…/1024`. Reranking reorders
the head only, keeps every hit, score and citation identity, and is applied
on single-collection reads (`POST /v1/search`, v1.2 search and answers). In
session turns and `POST /v1.2/query`, whose results are merged across
collections by raw leg scores, the embedding stage applies and reranking is
reported `not_applicable` (`merged_read`) and never charged.

Indexes are read in their own space: pinning a historical hash index uses the
hash embedder with no endpoint call.

## Outage policy

| Policy | Embedding unavailable | Reranking unavailable |
|---|---|---|
| `require` (default) | 503 `inference-stage-unavailable` | 503 `inference-stage-unavailable` |
| `prefer` (explicit) | lexical-only search of the same index, disclosed as `fallback: lexical_only`; the vector leg is absent | the original fused order, disclosed as `fallback: fused_order` |

"Unavailable" includes a transport failure, an endpoint refusal, an invalid
reply (wrong count, duplicate or out-of-range index, wrong dimension,
non-finite value, model mismatch), an expired deadline, an exhausted budget,
a missing or stale probe, and the deployment switch being off. A failed stage
is never marked applied. Every refusal and fallback is recorded in
`inference_stage_events` with the stage, state, reason and policy revision.
Authorization, compartments and evidence checks are unchanged on every path.
In a session turn with an explicit `collection_availability` policy, a
refusing collection is reported unavailable (`inference_unavailable`) and
contributes nothing.

To change the policy, bind again with the current revision and the same
stages. Changing the configuration, the embedding space or which stages are
configured requires a rollback first.

## Shared and dedicated placement

**ClearView SC2 (hub ADR 0024, accepted).** A configuration can
declare where its endpoint's pool runs work and for whom, and a binding can
require a placement. Server checks both before it sends any content: on
serving, shadow, build and every retry, because each of them obtains its
client through the same check. With neither a declaration nor a requirement,
nothing is checked, exactly as before.

Declare the pool in the configuration's `inference` block:

```yaml
  inference:
    embedDimensions: 1024
    placement:
      mode: shared                   # or dedicated
      tenants: [tenant-a, tenant-b]  # shared only: every covered tenant, this one included
      processing:                    # the pool's processing rules
        location: eu-west
        retention: none
```

- `dedicated` is a pool approved for this tenant alone. It lists no tenants;
  `processing` is optional.
- `shared` must list every tenant whose work the pool may receive and must
  declare at least one processing rule. Up to 256 tenants and 16 rules.
  Rule names are 1-64 lowercase letters, digits, `_`, `-` or `.`; values
  are 1-200 characters, compared exactly.
- A shared pool that does not list the configuring tenant receives nothing
  from it: the probe and the bind are refused (`inference-not-qualified`,
  409), and the stages of an existing binding report `unconfigured` with
  reason `placement_tenant_not_covered`. Sharing is never implied by
  omission, and this rule applies even to a binding without a requirement.

Require a placement on the binding with `placement`:

```json
"placement": {"isolation": "dedicated", "processing": {"location": "eu-west"}}
```

| Rule | Refusal reason |
|---|---|
| The configuration is shared and does not list the tenant | `placement_tenant_not_covered` |
| The binding has a requirement and the configuration declares no placement | `placement_undeclared` |
| `isolation: dedicated` and the configuration is `shared` | `placement_dedicated_required` |
| A required processing rule is not declared with the same value | `placement_processing_incompatible` |

`isolation: shared` accepts a dedicated or a shared pool. A refused bind is a
409 `inference-not-qualified` whose detail names the reason. Once bound, a
later change of the configuration that breaks the requirement turns every
stage `unconfigured` with the same reason, before any client is used: under
`require` the read is refused (503 `inference-stage-unavailable`), and under
`prefer` it takes the collection's disclosed fallback (`lexical_only`,
`fused_order`), which sends nothing.

**What a bind records.** An explicit `placement` is validated and stored. When
the request omits it, an existing binding keeps its current requirement, so a
rebind never loosens placement. A first binding to a configuration that
declares a placement records that declaration (mode and processing rules) as
its requirement. A first binding to an undeclared configuration, and an
existing Phase 6 binding rebound without `placement`, record none. The
requirement appears as `placement` in the binding response and history only
when it is set, so Phase 6 bindings keep their response shape.

**Loss of dedicated capacity.** Server never chooses another endpoint: a
binding names one configuration, and its stage policy decides what happens
when that endpoint is unavailable. If an operator repoints a configuration
whose bindings recorded `dedicated` at a shared pool, even one that covers
the tenant and passes its probe, those stages refuse with
`placement_dedicated_required` and the shared pool receives no content. Using
the shared pool requires an explicit rebind with
`"placement": {"isolation": "shared"}`, which the binding history records.

**What Server cannot check.** Server compares declarations; it cannot see
which workers stand behind an endpoint URL. A load balancer that sends a
dedicated configuration's traffic to shared workers defeats the check, so
give each pool its own endpoint and disable gateway fallbacks between pools.
Each tenant's configuration is checked on its own: Server does not compare
the tenant lists that different tenants declare for one pool, so every
participating tenant must list the same set. Lens result caches remain the
Lens deployment's responsibility under SC2. The representation is release
ready but is not yet exported through the contract process.

## Roll back

```console
curl -X POST -H "Authorization: Bearer $MGMT" -H "Content-Type: application/json" \
  https://server.example/v1/collections/handbook/inference/rollback -d '{"expected_revision": 3}'
```

Rollback reactivates the prior approved index and sets the binding to
`rolled_back`. It calls no endpoint, so it works with the endpoint down or
inference disabled, and it is a control operation that can use the
[control reserve](../ops/admission.md). Afterwards reads are v1.4 reads. The
semantic index and the prior index are kept by index retirement while the
binding names them; enroll again (after a new probe if the configuration
changed) to return to the semantic index.

## Budgets, attribution and shadow allocation

- **Payer.** The bound configuration's `dailyTotalTokens`, in the
  collection's own tenant, pays for every embedding and rerank call.
- **Reservation before dispatch.** Each physical attempt reserves its
  estimate (serialized request bytes / 4 + 32) first. A denied reservation
  sends nothing. Usage the endpoint reports settles the attempt; missing usage
  is charged at the estimate and counted as `missing_usage`. A request that
  never left Server (credential unavailable, request over `maxRequestBytes`,
  deadline already passed) is released.
- **Attribution.** Every attempt row (`inference_attempts`) names the
  purpose (`serving`, `shadow`, `build`, `probe`), the initiating caller (the
  request uid, or the caller who enqueued the build job), the collection, the
  job and the reservation.
- **Shadow allocation.** `shadowDailyTokens` is carved out of
  `dailyTotalTokens`: shadow attempts reserve only from it (ledger scope
  `shadow`) and serving, build and probe attempts only from the remainder
  (scope `all`). Shadow can never consume serving capacity, and the two
  together never exceed the cap. Without the allocation no shadow work is
  authorized. Failed and discarded shadow attempts are metered like
  successful ones.
- **Deadline.** One deadline (`timeout_ms`) covers queueing for an outbound
  slot, every attempt and delivery, across both stages of a read.

## The endpoint protocol Server speaks

Server matches the Munarium Lens LENS-1 profile.

| Route | Server sends | Server requires |
|---|---|---|
| `GET {base}/v1/models` | nothing | `data[].id`; when present, `stage`, `retrieval_model` and `dimensions` must agree with the configuration |
| `POST {base}/v1/embeddings` | `{"model", "input": [strings]}` only | `data[]` with every input `index` exactly once and a finite `embedding` at the declared dimension; `model`, when present, equals the request |
| `POST {base}/v1/rerank` | `{"model", "query", "documents": [strings]}` only | `results[]` with every document `index` exactly once and a finite `relevance_score`; Server sorts by score itself, ties in original order, and ignores any returned text |

- **Base URL.** A configured path ending in `/v1` is used as is; otherwise
  `/v1` is appended once. A private prefix (`https://gateway/lens`) is kept.
- **Instructions.** Server applies query and passage instructions as text
  prefixes and the rerank instruction as the query's `<Instruct>:` and
  `<Query>:` lines; the endpoint applies none (hub ADR 0029). See
  [Qwen3 instruction settings](#qwen3-instruction-settings).
- **Usage.** `usage.prompt_tokens`, else `usage.total_tokens`.
- **Authentication.** Optional `Authorization: Bearer`; redirects are not
  followed.
- **Deadline header.** Each request carries `x-munarium-deadline-ms`, the
  milliseconds left in the read's deadline. Server stops waiting at the
  deadline whether or not the endpoint reads it.
- **Refusals.** The JSON `code` is read from a fixed set
  (`inference_unavailable`, `overloaded`, `model_not_found`, `too_many_items`,
  `input_too_long`, `batch_too_large`, `body_too_large`, `invalid_request`,
  `unsupported_media_type`, `inference_failed`, …) and otherwise the status
  decides. Bodies are never logged, relayed or stored. A retry is a new
  reserved attempt, only for transport failures, 429, 502, 503 and
  `overloaded`, within `maxAttempts` and the deadline.
- **Empty query.** Reranking is skipped (`not_applicable`, `empty_query`)
  because the endpoint refuses an empty query.

## Limitations

- One semantic dimension (1,024). The vector leg over semantic vectors is an
  exact scan of one index version; an approximate index awaits I3
  measurements.
- Semantic indexes are served from PostgreSQL only. In `datastore` retrieval
  mode an enrolled semantic read reports `unconfigured` (`engine_unsupported`).
- Reranking applies to single-collection reads only (see above).
- A prepared query embedding is not reused across collections of one fan-out;
  each enrolled collection's read embeds its own query and is charged once.
- Server cannot verify model weights behind an endpoint. Give each model
  revision and precision a distinct served name; a change of name is a change
  of identity Server can see.
- The wire shapes of the stage receipt, the binding API (including
  `placement`), the configuration's `inference.placement` and the two problem
  types are release ready but not yet exported through the contract process.
- Placement is checked against declarations. Qualification against real
  shared and dedicated Lens pools (SC2-T1 to SC2-T3) needs a cluster; see the
  [cluster qualification plan](../design/cluster-qualification-plan.md#plan-sc2).
