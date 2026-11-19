# Hosting MCP tools in Munarium Server

**Status:** proposal, 19 November 2026. Nothing here is implemented, scheduled or accepted. It is input to a design decision for Munarium Server only.

**Written against:** the Server workspace under [`server/src/`](../../src/) at `bb18085` (1.4.0), and the Model Context Protocol specification revision `2026-07-28`. Statements about Server were checked against the files they link. Statements about the protocol were checked against the published specification pages for that revision; section 13 lists what was not read.

**Assumes:** most of the recommendations in [airk-analysis.md](airk-analysis.md) will be implemented in Server. Identifiers such as R7, IR-2 and U4 refer to that document. Section 9 lists each one's effect on this design and what this design needs if an item is dropped or arrives later.

## 1. Summary

Server has deliberately had no Model Context Protocol (MCP) surface. This document proposes adding one, hosted by the `munarium-server` binary itself, without changing the native REST or gRPC API.

The proposal in five points:

1. **One endpoint, off by default.** `POST /mcp` on the REST listener, served only when `MUNARIUM_MCP=enabled`. It speaks the stateless Streamable HTTP transport and answers every request with a single JSON response.
2. **A transport, not an authority.** A tool call is translated into a request for an existing `/v1` or `/v1.2` route and dispatched in-process through the same router, exactly as `ServerApiService` already does for gRPC. The call passes the same authentication, clearance checks, budgets, load shedding and audit as a REST call. There is no privileged path.
3. **Capability tokens only.** `/mcp` accepts a capability JWT carrying a new `mcp` scope, and refuses static `rw`, `ro` and `mgmt` tokens. Every agent call therefore has a subject, an expiry, an access level, compartments and an optional runbook allowlist.
4. **A closed, static tool table.** Eleven tools in the first phase, all on the query plane: discover, search, ask, converse, and resolve citations. No tool takes a path, a method or a route name as an argument, so the table is the complete list of what an agent can reach.
5. **No administration.** Index builds, activation, rollout, collections, shapes, runbooks, providers, tokens, ingestion, retention, reports and platform authority are not exposed. Two independent mechanisms keep them out (section 6).

A second phase, which needs a contract decision first, would let an agent read governed memory and propose claims through the existing gates (section 5.3).

The planned Server work in [airk-analysis.md](airk-analysis.md) changes this design in three places: admission (R7 replaces a dedicated MCP pool), the cost of search (an endpoint-backed embedder and reranker make `search_collection` a tool that calls a model), and what a tool error tells an agent about retrying (R11). Section 9 has the full mapping.

## 2. Why now, and why inside Server

The original reason for leaving MCP out still holds for most of the API: an agent holding a general tool surface over Server could rebuild an index, flip a rollout selector or mint a token, and none of those actions has an agent-appropriate governance model.

Three things have changed the balance for the query plane:

- **The query plane is already governed per call.** Sessions, scoped search, collection queries and checked answers all take a capability JWT, filter collections by level and compartment at query time, and write a uid-attributed interaction row ([security-posture.md](../security-posture.md)). An agent calling these is governed in the same way as an application calling them.
- **The dispatch pattern exists.** [`grpc_api.rs`](../../src/munarium-server/src/grpc_api.rs) maps a fixed set of named RPCs onto route templates and sends each one through `rest::router` with `oneshot`. Its header comment states the property this proposal needs: "Dispatch is in-process through the same bounded, authorized and audited handlers as REST; no network request or alternate service credential is involved. Clients cannot choose an arbitrary URI."
- **Munarium Matrix already ships this shape.** Matrix serves `POST /mcp` beside its `/v1` routes as "a transport, not an authority", with tools generated from declared assets and no free-form query tool ([Matrix MCP reference](https://github.com/iokaio/munarium-matrix/blob/main/docs/api/mcp.md)). An agent that reaches Matrix over MCP and Server over a hand-written wrapper has two integration models for one product.

Hosting it in Server, instead of in a separate bridge process, matters for governance. A bridge needs its own credential to Server, and whatever that credential can do, the bridge can do for any agent that reaches it. In-process dispatch forwards the caller's own bearer token and nothing else, so the agent's authority is exactly its token's authority.

## 3. Design rules

These are the constraints every later section is held to.

| # | Rule | Consequence |
|---|---|---|
| D1 | The native API does not change. | No route, DTO, proto message or SDK method is altered. `/mcp` sits outside `/v1` and outside the OpenAPI contract, like `/admin` and `/docs`. |
| D2 | MCP grants no capability the caller's token lacks. | Every tool resolves to handlers that check the caller's own token. The MCP layer holds no credential. |
| D3 | The tool set is closed and static. | A compiled table. No generic "call route" tool, no argument that selects an operation, no tool generated from tenant data. |
| D4 | Every call is attributed and audited. | Static tokens are refused, so every call has a JWT `sub` and `jti`. Calls are recorded in the interactions trail with the tool name. |
| D5 | A refusal is distinguishable from an empty result. | Server problem types reach the agent as tool errors carrying the problem slug. |
| D6 | Off by default. | A deployment that does not set `MUNARIUM_MCP=enabled` serves 404 on `/mcp` and behaves exactly as today. |
| D7 | Tool metadata never contains tenant content. | Names, descriptions and schemas are compiled constants. Tenant-authored text (runbook descriptions, document passages) appears only in tool results. |

D7 is a prompt-injection control. A tool description is read by the model as instruction; a tool result is read as data. Matrix generates tools from applied assets because the asset is the bound. Server's bound is the token, so its tools can be constant, and a tenant document or runbook can never alter what `tools/list` says.

## 4. Hosting and transport

### 4.1 Where it listens

`POST /mcp` on the REST listener (`MUNARIUM_HTTP_ADDR`, :8080 in the container). A separate listener was considered and is not recommended for the first version: the gateway, TLS termination and network policy described in [security-posture.md](../security-posture.md) already cover :8080, and a fourth port would need the same treatment again. Operators who want agents on a different hostname or policy can route `/mcp` separately at the gateway.

`MUNARIUM_AUTH_MODE=disabled` and the memory store are development modes. `/mcp` is not served under disabled auth, because rule D4 cannot hold without a token.

### 4.2 Protocol revision

Target the `2026-07-28` revision. Its core is stateless: there is no `initialize` handshake and no `Mcp-Session-Id`, and each request carries its protocol version. That suits N identical replicas behind a gateway with no session affinity, which is the deployment Server documents in [architecture.md](../architecture.md).

| Concern | Behaviour |
|---|---|
| Methods | `server/discover`, `tools/list`, `tools/call`. Anything else is `-32601`. |
| Response form | One `application/json` response per POST. No SSE. Notifications answer `202` with no body. |
| `GET` / `DELETE` on `/mcp` | `405`. |
| Batching | Refused. The body must be a single JSON-RPC request or notification. |
| `MCP-Protocol-Version` | Required. An unsupported value is `400` with `-32022`. |
| `Mcp-Method`, `Mcp-Name` | Required and compared with the body. A missing or mismatched header is `400` with `-32020`. |
| `Origin` | Validated against `MUNARIUM_MCP_ALLOWED_ORIGINS`. Present and not allowed is `403`. Absent is accepted (server-side agents send none). |
| `tools/list` caching | `ttlMs` from configuration, `cacheScope: "private"`, because the list depends on the token's scopes (section 5). |
| Results | `resultType: "complete"` always. Server never returns `input_required`, never starts a task and never sends a server-to-client request. |
| Body limit | 1 MiB on `/mcp`. No tool carries document bytes. |

**Older clients.** Many clients in use still send `initialize` with `2025-03-26`, `2025-06-18` or `2025-11-25`. The specification allows a server to serve both eras on one endpoint. The proposal is to answer a legacy `initialize` and `ping` statelessly: report the requested version if it is one of those three, mint no session id, ignore any `Mcp-Session-Id` that comes back, and treat a missing `MCP-Protocol-Version` header as `2025-03-26`. This is the same single-round-trip behaviour Matrix has today. It should be controlled by `MUNARIUM_MCP_LEGACY_INITIALIZE` (default enabled) so it can be withdrawn later.

**No stdio.** Server is a network service and does not host a stdio transport. A desktop client that only speaks stdio needs a local stdio-to-HTTP relay, which is client tooling and out of scope here.

### 4.3 Authentication and the uid contract

`/mcp` reads `Authorization: Bearer <token>` and accepts exactly one credential kind.

- **Capability JWT with the `mcp` scope:** accepted. The uid is the token's `sub`. An `X-Munarium-Uid` header is not required; if present it must equal `sub`, as on `/v1`.
- **Static `rw`, `ro` or `mgmt` token:** refused with `forbidden`. On `/v1` a static `rw` token maps to an unrestricted data-plane principal (`Principal::access_ctx` in [`state.rs`](../../src/munarium-server/src/state.rs)); that is a convenience for conformance and must not be an agent's authority.
- **Missing, invalid, expired or revoked token:** HTTP `401` with a JSON-RPC error body carrying the problem slug (`unauthenticated`, `token-expired`, `token-revoked`). Both are sent because an MCP client that receives a bare status code reports a transport fault it cannot explain.

`mcp` is a new, additive scope in [`munarium-access`](../../src/munarium-access/src/lib.rs). It grants nothing by itself: each tool also requires the scope its underlying route requires (`query` or `evidence`). Its purpose is separation in both directions. A token minted for an application's REST calls cannot be handed to an agent and used over MCP, and the manager decides explicitly, per token, that an agent may connect. Tokens are minted the existing way, by the API-management layer with its `mgmt` credential through `POST /v1/access-tokens`, with the usual level, compartments, TTL and `rb` runbook allowlist.

**OAuth.** The MCP authorization specification is optional and, where implemented, is built on OAuth 2.1 with protected-resource metadata and an authorization server. Server is deliberately not an identity provider and has no authorization server. This proposal does not add one. A client that insists on the OAuth discovery flow must be fronted by the API-management layer, which performs the OAuth exchange with the organisation's identity provider and swaps the result for a capability JWT, the same division of responsibility that applies to human users today. A consequence worth stating plainly: clients that can only authenticate through MCP's OAuth flow will not connect to a bare Server.

## 5. Proposed tools

Tool names use lowercase letters, digits and underscores only. The specification also allows dots and hyphens, but several model APIs restrict function names to a narrower set, and the narrower set works everywhere.

Every input schema is a JSON Schema object with `additionalProperties: false`. Arguments are validated against it before any dispatch; a failure is a protocol error (`-32602`) and no Server route is called.

`tools/list` returns only the tools the presented token could use: a token without `evidence` does not see the evidence tools, and with `MUNARIUM_MCP_PAID_TOOLS=disabled` the tools that run a completion are not listed for anyone. Calling an unlisted tool is still refused by the route's own scope check, so the listing is a courtesy and not the control.

### 5.1 Phase 1: knowledge tools

The "model calls" column separates two kinds of cost. A **completion** is a generative call through the provider gateway, charged against the provider configuration's token caps. A **retrieval call** is an embedding or rerank call to an inference endpoint, which exists only for collections enrolled under IR-2 or IR-3 and is charged in tokens and calls with no price.

| Tool | Underlying operation | Scope | Model calls | Changes state |
|---|---|---|---|---|
| `list_collections` | New filtered read (section 7.2) | `query` | None | No |
| `list_runbooks` | New filtered read (section 7.2) | `query` | None | No |
| `search_collection` | `POST /v1.2/search` | `query` | Retrieval calls on an enrolled collection; none otherwise | No |
| `ask_collections` | `POST /v1.2/query` | `query` | Completion, plus retrieval calls on an enrolled collection | No |
| `open_session` | `POST /v1/runbooks/{name}/sessions` | `query` | None | Creates a session |
| `ask_session` | `POST /v1/sessions/{id}/turns` | `query` | Retrieval calls on an enrolled collection; completion when `complete` is true | Appends a turn |
| `get_session` | `GET /v1/sessions/{id}` | `query` | No | No |
| `close_session` | `POST /v1/sessions/{id}/close` | `query` | No | Closes a session |
| `authorize_publication` | `GET /v1.2/collections/{id}/publications/{publication_id}` | `query` | No | No |
| `get_evidence` | `GET /v1/evidence/{id}` | `evidence` | No | Writes an access-audit row |
| `read_evidence_rows` | `GET /v1/evidence/{id}/rows` | `evidence` | No | Writes an access-audit row |

Notes on individual tools:

- **`list_collections`, `list_runbooks`.** The existing `GET /v1/collections` and `GET /v1/runbooks` take static control-plane tokens and return every collection and runbook in the tenant with its access requirements ([`collections_api.rs`](../../src/munarium-server/src/collections_api.rs), [`runbooks_api.rs`](../../src/munarium-server/src/runbooks_api.rs)). An agent should see less. These two tools return only the collections the token's level and compartments permit and the runbooks its `rb` allowlist permits, with names, descriptions and, for runbooks, the permitted collections and declared research profile names. Without them an agent has to be told collection and runbook names out of band.
- **`search_collection`.** Arguments: `query`, `collection`, optional `top_k`, optional `index_version`. Returns hits and the provenance envelope unchanged, and the search execution record once R1 exists. On a collection enrolled for an endpoint-backed embedder or reranker, a search is no longer a local operation: it makes one embedding call before the vector leg and, where enrolled, one rerank call after fusion. The tool takes no argument that selects or bypasses either stage; the collection's `prefer` or `require` policy decides, as it does for every other caller. `vocabulary_collection` is not exposed in the first version; the default (the searched collection's own vocabulary) is the common case and the cross-collection form has its own authorization rule that is better left to applications.
- **`ask_collections`.** Arguments: `question`, `collections`, optional `effective_on`. Server selects governing publications, retrieves, composes and returns checked citations with server-resolved source references ([collection guide](../guides/collection-vocabularies.md)). This is the tool most agents will want.
- **`ask_session`.** Arguments: `session_id`, `query`, optional `top_k`, `complete`, `research_profile`. `model_override` is deliberately absent: choosing a more expensive model is a cost decision, and it stays with the application and the runbook's `allowOverrides` policy. The streaming route is not exposed; the tool returns the same `TurnResponse` the unary route returns, including the `EvidenceHierarchyDecision` when a research profile ran ([evidence hierarchy](../guides/evidence-hierarchy.md)). R2 adds a per-collection outcome to that response and a request-level availability policy. The tool does not expose the policy: turns run with `require`, the default, so an agent never receives a partial multi-collection answer it has to recognise as partial. Exposing `report` can be reconsidered once agents are shown to handle the per-member record correctly.
- **`authorize_publication`.** Returns an identity for an original file, never bytes or a download grant, as on REST.
- **`get_evidence`, `read_evidence_rows`.** These resolve a citation such as `evidence/<id>#r0003` to the sealed manifest and rows. Class domination is checked per artifact by the existing handlers, a refusal names nothing about the artifact, and each read is recorded in `evidence_access`. The `evidence` scope also permits sealing on REST; no sealing tool exists, so an agent token with `evidence` can read over MCP and could seal only by calling REST directly. Section 12 asks whether a read-only evidence scope is wanted before these two tools ship.

`POST /v1.2/answers` (compose an answer from caller-supplied pinned passages) is not proposed. It exists for applications that run their own retrieval and want a checked narrative; an agent that has retrieved passages is itself the model, and `ask_collections` covers the case where it wants Server to do both.

### 5.2 Tool results

A successful call returns:

- `structuredContent`: the REST response body, unchanged. The JSON an agent sees is the JSON an SDK user sees, so the two cannot drift.
- `content`: one text block containing the same JSON serialised, for clients that do not read structured content.
- `isError: false`.

A Server refusal (any `application/problem+json` response) returns `isError: true` with `structuredContent` of `{slug, status, detail, request_id}` plus the problem's extension members, and a text block that leads with the slug. Examples an agent must be able to tell apart: `scope-missing`, `forbidden`, `not-found`, `session-not-open`, `required-evidence-unavailable`, `daily-cap-reached`, `rate-limited`, `datastore-unavailable`, `evidence-forbidden`, `evidence-expired`. The registry in [errors.md](../api/errors.md) applies unchanged and no new slug is needed.

**Retry guidance.** R11 adds `retryable` and `retry_action` to problem responses. They arrive in `structuredContent` with the other extension members, and the text block states the action in words directly after the slug: repeat the same request, retry later, do not retry and inspect, or do not retry. An agent is the caller least able to infer this from a status code, and the one most likely to loop on a refusal. Until R11 exists, the MCP layer does not invent the mapping: the text block carries the slug and detail only, and the reference documentation tells agent authors which slugs are safe to retry.

**Disclosure of how a result was produced.** Two additions from the planned work reach the agent with no MCP code, because the body is passed through unchanged: the search execution record (R1: which engine and legs ran, and whether the embedder and reranker applied) and the per-collection outcomes of a turn (R2). The tool descriptions for `search_collection` and `ask_session` should say that these fields exist and what a not-applied stage means, since a lexical-only result from a collection the agent believes is semantic is otherwise indistinguishable from a full one.

**Passage text is returned verbatim.** U9 has the admin console render invisible and direction-changing characters as visible escapes. Tool results cannot do the same: citation quotes are checked against the passage exactly, and an agent that quotes an escaped form would fail that check. Passages therefore reach the model as stored. Tenant documents are untrusted input to the agent, and a passage can contain text written to look like an instruction. Rule D7 keeps such text out of tool metadata; nothing in Server can keep it out of results. The reference documentation must say so.

Protocol errors (unknown tool, invalid arguments, header mismatch, unsupported version) are JSON-RPC errors and never tool results.

**Size.** A turn with many hits can be large. Results above `MUNARIUM_MCP_RESULT_MAX_BYTES` (proposed default 256 KiB) are refused with a tool error that says so and names the argument that reduces it (`top_k`, or `limit` for evidence rows). Truncating a result silently would let an agent quote an incomplete answer as complete, which is the failure the evidence hierarchy exists to prevent.

**Output schemas.** Declaring `outputSchema` obliges the server to conform on every call. The schemas already exist in the generated OpenAPI document. The first version should omit `outputSchema` and add it once a generator from the OpenAPI components is in place, so the two are produced from one source. The planned work adds response members to exactly the bodies these tools return (R1, R2, R11 and the rerank receipt), so a schema written by hand now would be wrong within a release. When schemas are added they must permit additional properties. The same applies in the other direction to R12: where a request DTO gains a validating newtype, the tool's input schema must carry the same constraint explicitly, as the OpenAPI annotation must.

**Annotations.** Read-only tools are marked read-only. `open_session`, `ask_session` and `close_session` are marked as state-changing, non-destructive and closed-world. Clients treat annotations as hints, so nothing depends on them.

### 5.3 Phase 2: governed memory tools (needs a contract decision)

The ledger is the part of Server designed for a model to write to: "the model proposes; the mesh disposes". A claim that conflicts with the record is stored as disputed with gate findings, never dropped and never silently applied. Letting an agent propose claims is therefore governed in the sense this proposal requires.

It cannot ship in phase 1, because the ledger routes authenticate with static tokens only (`AppState::authenticate` refuses a capability JWT), and the ledger has no level or compartment model: a static `rw` token can write any lineage in the tenant. Rule D4 forbids giving an agent that.

Candidate tools, for discussion:

| Tool | Underlying operation | Kind |
|---|---|---|
| `memory_get_head` | `GET /v1/versions/{id}/head` | Read |
| `memory_read_facts` | `GET /v1/versions/{id}/facts` | Read, supports `as_of_seq` |
| `memory_compose_context` | `GET /v1/versions/{id}/context` | Read, budget-bounded |
| `memory_get_claim` | `GET /v1/claims/{id}` | Read |
| `memory_list_findings` | `GET /v1/versions/{id}/findings` | Read |
| `memory_list_promises` | `GET /v1/versions/{id}/promises` | Read |
| `memory_propose_claim` | `POST /v1/versions/{id}/claims` | Gated write |
| `memory_open_promise` | `POST /v1/versions/{id}/promises` | Gated write |
| `memory_fulfill_promise` | `POST /v1/versions/{id}/promises/{key}/fulfill` | Gated write |

What it would take:

- Two new capability scopes (working names `memory.read` and `memory.write`) and a new optional token claim listing the version ids the token may touch, analogous to `rb`. A token with no list reaches no lineage.
- A decision on how the ledger routes accept such a token. The smallest change is in the route's authentication helper; it is still a change to who may call a native route, so it needs a hub decision record and client conformance review before implementation.
- Write tools take a required `idempotency_key` argument and a required `expected_head`. Server must not invent an idempotency key from the JSON-RPC id: a retried tool call with a new id would then execute twice.
- Not proposed even in phase 2: `CreateVersion`, raw `AppendEvents`, `LockAnchor`, counters, digests and governance-profile transitions. These shape the lineage itself and belong to the application.

## 6. What is not exposed, and how that is enforced

| Area | Routes | Why not |
|---|---|---|
| Index lifecycle | `/v1/indexes/*`, `/v1/collections/{id}/activate-index`, `/v1/index-artifacts/*`, `/v1/index-build-jobs*`, `/v1/retrieval-rollout*` | Alters what every reader is served. Builds are expensive and promotion is fleet-gated. |
| Collections, shapes, runbooks | `POST /v1/collections`, `/v1/shapes`, `/v1/runbooks*` (apply, run, approve, remove), `/v1/runs/*`, `/v1/authoring/*`, `/v1/chronology-rules*` | Defines the governance an agent is subject to. Approval is a human gate. |
| Ingestion | `/v1/sources`, `/v1/ingest*`, `/v1/versions/{id}/ingests` | Changes the corpus and therefore the next index. Section 12 lists this as a possible later, separately scoped addition, with R3 as a precondition. |
| Retrieval model enrollment (planned, IR-1 and IR-4) | The provider conformance probe, and the per-collection embedder and reranker enrollment and policy API | Decides which model every reader's results pass through, and an embedder change forces a rebuild. Management role by design. |
| Build-job control (planned, R6 and R9) | Cooperative cancel, job progress and the job event ring | Operator view of builds an agent cannot start. |
| Vocabulary and publication governance | `/v1.2/vocabulary-settings`, `/v1.2/collections/{id}/vocabulary*` (write, refresh), `/v1.2/collections/{id}/governance` | Changes how every query in a collection is expanded or which publication governs. |
| Providers and budgets | `/v1/providers*`, `/healthai`, `/v1/max-tokens`, `/v1/monetary/*` | Credentials, spend ceilings and raw model access. A raw `complete` tool would make Server a model proxy for the agent. |
| Tokens | `/v1/access-tokens*` | An agent must not mint or revoke authority. |
| Evidence writes and retention | `POST /v1/evidence`, upload, commit, `DELETE`, legal hold, access audit | Sealing is a service role; purge and hold are management-only. |
| Findings writes | `POST /v1/versions/{id}/findings` | A service-to-service operation for Matrix. |
| Reports and audit | `/v1/reports/*`, `/admin/*` | Management-only, and includes other users' activity. |
| Recovery and retention | `/v1/command-recovery*`, `/v1/source-retention` | Operator procedures with restore duties. |
| Platform plane | `/v1/platform/*` | mTLS-admitted authority, record and activation operations. |

Enforcement does not rest on the table alone. Two independent mechanisms apply:

1. **The table is closed.** The dispatcher maps a tool name to a compiled `(method, route template, argument mapping)` entry. No argument reaches the method or the literal part of the path; path parameters are percent-encoded per segment with the same `component` and `request_uri` logic `ServerApiService` uses, whose test `path_values_cannot_change_the_selected_operation` already pins that property.
2. **The credential cannot administer.** `/mcp` accepts only capability JWTs. Every control-plane and management route authenticates through `AppState::authenticate`, which rejects a capability JWT with "access tokens are data-plane credentials; this endpoint requires a static token". So even a mistaken table entry pointing at an administrative route would answer `forbidden`.

A third, test-time control keeps the first two honest: a unit test enumerates the table and fails if any entry targets a route outside an explicit allowlist checked into the test, and a conformance test presents a maximally scoped MCP token and asserts that representative administrative operations cannot be reached by any tool name or argument.

## 7. Implementation in Server

### 7.1 Layout

- A new module `mcp.rs` in `munarium-server` holding the JSON-RPC envelope, header and version checks, the tool table and the dispatcher. Matrix's equivalent is about 700 lines including tests.
- No new crate dependency. The envelope is small enough to write over `serde_json`, which keeps the static musl build and the `cargo deny` surface unchanged. An MCP SDK crate would bring its own transport and session machinery that a stateless single-response server does not use. This should be re-examined if the server later needs streaming or tasks.
- `munarium-access`: the `mcp` scope constant and its acceptance in `issue`.
- `config.rs`: the settings in section 7.5.
- `rest.rs`: the `/mcp` route, registered only when enabled.
- `munarium-core` is untouched, preserving its independence from HTTP.

### 7.2 Dispatch

Two kinds of tool, both resolved from the same table.

**Routed tools (nine of eleven).** The dispatcher builds an `http::Request` for the entry's route, copies the caller's `Authorization` header and nothing else from the outer request, sets `X-Munarium-Uid` from the token subject, serialises the mapped arguments as the body or query string, and sends it through the REST router with `oneshot`. The inner request then runs the full existing stack: the capture middleware's load shed, uid check, body limit and interaction record, then the handler's own scope, clearance, session-ownership and revocation checks. This is the pattern in [`grpc_api.rs`](../../src/munarium-server/src/grpc_api.rs), including its rule that no caller-controlled header other than the contract ones is forwarded.

**Native tools (`list_collections`, `list_runbooks`).** No existing route gives a clearance-filtered listing to a capability token. These two are small handlers that resolve the token with `data_plane_access` (so scope and revocation checks are the shared ones), call the existing `op_list_collections` and `op_list_runbooks`, and filter with `AccessCtx::permits` and `AccessCtx::permits_runbook`. They add no route to the native API. Because they bypass the router they must write their own interaction record; that is the one place the audit path is not inherited and it needs a direct test.

### 7.3 Audit and metrics

The capture middleware records only `/v1` and `/v1.2` paths ([`middleware.rs`](../../src/munarium-server/src/middleware.rs)), so the outer `/mcp` request is counted in metrics but not captured, and the inner dispatched request is captured as an ordinary REST call. Left like that, an operator could not tell an agent's search from an application's.

Proposal: the dispatcher attaches a request extension (not a header, so it cannot be set from the network) naming the tool. When present, the capture middleware records `plane = "mcp"` and `method = "tools/call <tool>"` on the interaction row, and labels the request metrics with `plane="mcp"` and the tool name as the route. Tool names are a closed set, so this respects the cardinality rules in `metrics.rs`. The row's uid, tenant, `token_jti`, session and collection attribution, body capture and redaction all come from the existing path unchanged. Whether the `interactions.plane` column or any report view constrains its values must be confirmed before implementation; if it does, the change is an additive migration.

The reports that group by plane (`/v1/reports/timeseries?plane=`) would then show agent traffic as its own series with no new report.

### 7.4 Concurrency and cost

- **Admission as a work class.** R7 replaces the single permit pool per plane with admission by work class and a slot reserved for control. MCP should be built on that, not beside it. Agent traffic is one class with its own ceiling (`MUNARIUM_MCP_MAX_CONCURRENCY`), taken before the body is parsed and shed with `overloaded` when full, so an agent loop cannot consume the capacity application traffic uses. Calls that reach an inference endpoint (search on an enrolled collection, and every completion) are additionally admitted through the heavy class that section 8.9 of [airk-analysis.md](airk-analysis.md) assigns to rerank and embedding, so a slow endpoint cannot hold the agent class or the serving plane. R7's reserved control slot is what guarantees that the operator's response to a runaway agent, revoking its token, is never shed along with the traffic it is meant to stop.

  Classification must be explicit: each row of the tool table names its class, and the table test in section 8 fails for an entry with none. If MCP ships before R7, the interim is a dedicated `mcp_permits` semaphore with the same setting, with the inner dispatched request also taking a REST permit. That gives the ceiling without the control reservation or the heavy class, and it should be replaced, not kept, when R7 lands.
- **Completion spend.** `ask_collections` and `ask_session` with `complete` go through the provider gateway like any other call, so per-config rate budgets and daily token caps apply and refuse with `rate-limited` or `daily-cap-reached` ([token budgets](../tokenbudgets.md)). There is no per-token or per-uid spend ceiling in Server today. An agent can spend a tenant's daily cap faster than a person can. Section 12 raises whether MCP needs its own cap; until then, operators who want a hard separation can disable paid tools or give agents runbooks bound to a provider config with its own budget.
- **Retrieval-call load.** Embedding and rerank calls for an enrolled collection are bound to the collection, not to the caller, and are charged to whichever scope the collection-binding decision in section 8.9 of [airk-analysis.md](airk-analysis.md) settles on. An agent that searches in a loop therefore loads a shared inference endpoint and a shared budget that no setting in this document limits per agent. `MUNARIUM_MCP_PAID_TOOLS` does not cover it, deliberately: switching search off for agents would remove the tool that makes the others usable. The controls are the agent class ceiling above, the heavy class queue, and the endpoint's own budget. Whether that is enough is part of open decision 3.
- **Timeouts.** A completion turn can take tens of seconds, and a search on an enrolled collection now includes one or two network calls bounded by the stage deadline. This design sends no progress notifications. The endpoint documentation must state an expected upper bound per tool so clients set tool timeouts accordingly. Streaming progress is deferred (section 11).

### 7.5 Configuration

| Setting | Default | Meaning |
|---|---|---|
| `MUNARIUM_MCP` | `disabled` | `enabled` serves `POST /mcp`. |
| `MUNARIUM_MCP_ALLOWED_ORIGINS` | empty | Comma-separated origins accepted when an `Origin` header is present. Empty refuses every browser origin. |
| `MUNARIUM_MCP_MAX_CONCURRENCY` | 64 | Ceiling of the agent work class per instance (section 7.4). |
| `MUNARIUM_MCP_PAID_TOOLS` | `enabled` | `disabled` removes `ask_collections` and refuses `complete: true` on `ask_session`. It governs completions only; embedding and rerank calls made by search follow the collection's enrollment. |
| `MUNARIUM_MCP_RESULT_MAX_BYTES` | 262144 | Largest tool result returned. |
| `MUNARIUM_MCP_TOOLS_TTL_MS` | 300000 | `ttlMs` on `tools/list` and `server/discover`. |
| `MUNARIUM_MCP_LEGACY_INITIALIZE` | `enabled` | Answer `initialize` from pre-2026 clients. |

A per-tenant switch is not proposed. Whether a tenant's agents may connect is already decided per token by the `mcp` scope.

### 7.6 Documentation and contract effects

- New `docs/api/mcp.md` as the reference, linked from the documentation index, with the tool table generated from the compiled table and drift-checked like `grpc-reference.md`.
- One row in the "Ops" group of [rest.md](../api/rest.md) for `/mcp`. It is outside OpenAPI, so the `docs_coverage` route tests are unaffected.
- A section in [security-posture.md](../security-posture.md) for the `mcp` scope, the static-token refusal and the OAuth position, and a line in its deployment checklist.
- [grpc.md](../api/grpc.md): no gRPC twin. MCP is itself an alternate transport.
- Client SDKs: no change. The `mcp` scope is a new accepted string in token issuance, which the SDKs pass through.
- Helm chart: an `mcp.enabled` value mapped to the environment settings.
- Admin console: the changes in section 9.3.

## 8. Testing

| Level | What it must show |
|---|---|
| Unit, `mcp.rs` | Envelope parsing; header and body agreement; version handling for both eras; every tool's schema rejects undeclared arguments; argument-to-request mapping for each tool equals the request an SDK would send. |
| Unit, table | Every routed entry is in the checked-in allowlist; no entry's route requires `rw` or `mgmt`; every entry names its work class; tool names match the permitted character set. |
| Pass-through of planned members | As each lands: the execution record (R1), per-member outcomes (R2), retry members (R11) and the rerank receipt appear in `structuredContent` byte-identical to the REST body; a not-applied embedder or reranker stage under `prefer` is visible in the result, and under `require` arrives as a tool error. |
| Admission | At the agent class ceiling `/mcp` sheds while `/v1` application requests and a management token revocation still succeed. |
| Server tests | Static tokens are refused on `/mcp`; a JWT without `mcp` is refused; `tools/list` varies with scopes; a refusal arrives as `isError` with its slug; an oversize result is refused and not truncated; the interaction row carries `plane = "mcp"`, the tool name, uid and `jti`; the native listing tools hide collections above the token's clearance and write an interaction row. |
| Conformance, black box | Over real HTTP against compose: discover, list, search, open a session, ask, resolve a citation, close. The same token over REST and over MCP returns the same body for the same request. A maximally scoped MCP token cannot build an index, activate one, apply a runbook, mint a token or read a report. |
| Cluster tier | Consecutive calls land on different replicas and succeed with no affinity. |
| Client compatibility | Each client in section 10.5, at a recorded version, connects, lists tools, calls one and is taken through token expiry against a real Server. Results are published with the reference page. Manual until a client offers a headless mode. |
| Negative controls | Removing the static-token refusal, the allowlist test or the `mcp` scope check each fails a named test. |

## 9. Interaction with the planned Server work

[airk-analysis.md](airk-analysis.md) proposes twelve recommendations (R1 to R12), an endpoint-backed embedder and reranker (IR-0 to IR-6) and admin console changes (U1 to U14). This design assumes most of them will be built. None changes its core: in-process dispatch, capability tokens only and the closed tool table stand whichever items land.

### 9.1 Effect of each item

| Item | Effect on MCP | Where |
|---|---|---|
| R1 search execution record | Passes through in results. Tool descriptions must explain it. Needed for an agent to tell a full hybrid result from a degraded one. | 5.1, 5.2 |
| R2 per-member outcomes | Passes through in turn results. The availability policy is not exposed; turns run with `require`. | 5.1 |
| R3 isolated PDF extraction | None while ingestion is excluded. A precondition if ingestion over MCP is ever proposed, since agents would become a new source of hostile documents. | 6, 12 |
| R4, R6, R8, R9 build-job fencing, cancellation, fairness, progress | None on the tool set. Their new control and status routes join the exclusion list. | 6 |
| R5 content-free job failure codes | None directly; no job is visible to an agent. The same principle already holds for tool errors, which carry only what problem responses carry. | 5.2 |
| R7 work-class admission | Replaces the dedicated MCP permit pool. MCP becomes a class, model-calling tools use the heavy class, and the reserved control slot protects revocation. | 7.4 |
| R10 durable artifact damage record | A damaged artifact gates serving for every replica at once, so an agent sees `datastore-unavailable` consistently instead of on some replicas. No MCP change. | 5.2 |
| R11 typed retry guidance | Carried in tool errors and stated in the error text. Higher value for agents than for SDK callers. | 5.2 |
| R12 validating newtypes | Tool input schemas must state the same constraints as the validators. | 5.2 |
| IR-0 to IR-3 embedder and reranker | `search_collection` and the retrieval half of the ask tools make model calls on enrolled collections. Changes the cost table, the meaning of the paid-tools switch and the timeout guidance. | 5.1, 7.4, 7.5 |
| IR-1, IR-4 probe, enrollment and policy | Management operations; excluded. The per-collection `prefer` or `require` policy applies to agents unchanged. | 6 |
| IR-5 shorter context packing | Smaller turn results when the reranker applied. Lowers pressure on the result size limit; no MCP change. | 5.2 |
| U1 to U14 admin console | See 9.3. | 9.3 |

### 9.2 Sequencing

Phase 1 of MCP does not have to wait for any of this work, but the order affects what an agent can safely be told.

1. **R7 before or with MCP.** It is the one item this design builds on structurally. Shipping MCP first means building an interim pool and then removing it.
2. **R11 with MCP.** It is small, additive and removes the main reason an agent loops on a refusal. Because of MCP it deserves a higher priority than the "low" the other document gives it.
3. **R1 before IR-2 or IR-3 is enabled for any collection agents can reach.** Without the execution record an agent cannot see that a stage did not apply.
4. **R2 before `ask_session` is recommended for runbooks spanning several collections.** Today a skipped collection has no reason attached.
5. **The collection-binding decision before agents search enrolled collections at volume.** It settles whose budget an agent's embedding and rerank calls are charged to.

If an item is dropped, the consequences are limited to the following.

- **R7 dropped:** the interim `mcp_permits` pool becomes permanent, with no control reservation.
- **R11 dropped:** retry behaviour is documented per slug for agent authors.
- **R1 or R2 dropped:** `search_collection` and `ask_session` keep the limitations noted against them, and the embedder and reranker should not use `prefer` on collections agents reach.

### 9.3 Admin console

The console changes in section 9 of [airk-analysis.md](airk-analysis.md) are where an operator would see and stop agent activity. MCP adds four requirements to them.

- **Tokens page (U4, U8).** Show the `mcp` scope as its own marked column or badge, and allow filtering by it, so the tokens that agents hold can be found quickly. Revoking one is the stop control for an agent, and U4's review step should state on the review page that the token is an agent token and whether revocation is enforced at verification time (`MUNARIUM_TOKEN_REVOCATION_CHECK`). If it is not enforced, the page must say that the agent keeps access until the token expires.
- **Traffic pages (U1).** Show `mcp` as a plane beside `rest` and `grpc`, with per-tool counts and error rates from the interaction rows described in 7.3.
- **Attention strip (U3).** Add two conditions: the agent work class shedding load, and a single token accounting for an abnormal share of completions or retrieval calls in the window.
- **Session and audit pages (U9).** Agent-originated turns appear in the same lists. The visible-escape rendering U9 proposes matters more here, because an operator reviewing what an agent asked and was told is the reader most exposed to text crafted to mislead.

No console action is added for MCP. Enabling the endpoint is deployment configuration, and agent access is granted by minting a token, which the console already does.

## 10. Configuring agents to connect

This section shows how common agent clients would be pointed at `/mcp` and how each supplies the bearer token. The examples use `https://munarium.example.com/mcp` and an environment variable named `MUNARIUM_TOKEN`. Client syntax was taken from each vendor's documentation as read on 19 November 2026. None of it has been run against Server, because the endpoint does not exist yet, and these formats change often; section 13 states the limits.

### 10.1 What every client needs

Three things, whatever the client:

1. **The URL** of the endpoint, over HTTPS, as exposed by the gateway.
2. **One request header:** `Authorization: Bearer <capability JWT>`. No `X-Munarium-Uid` header is needed; the token's subject supplies the uid (section 4.3).
3. **A way to replace the token when it expires.** Capability tokens live for one hour by default and at most 24 hours. This is the part that differs most between clients and causes most of the practical difficulty.

**Where the token comes from.** The API-management layer mints it with its `mgmt` credential, for one user, with the scopes and clearance that user's agent should have:

```bash
curl -sS https://munarium.example.com/v1/access-tokens \
  -H "Authorization: Bearer $MGMT_TOKEN" -H "X-Munarium-Uid: token-broker" \
  -H "Content-Type: application/json" \
  -d '{"uid":"alice","access_level":2,"compartments":["eng"],
       "scopes":["mcp","query"],"runbook_refs":["field-support"],"ttl_secs":28800}'
```

The `mgmt` credential must never be placed on a developer's machine or in an agent's configuration: it can mint a token for any user at any clearance. An organisation therefore needs a small broker in front of this call, which authenticates the person through its own identity provider and returns a token for that person only. This document calls that broker's command-line form `munarium-agent-token` in the examples below. It is a placeholder for something each deployment supplies, not a tool that ships with Server; section 12 asks whether Server should provide a reference one.

**Three ways a client can hold the token**, in increasing order of convenience:

| Pattern | How it works | When the token expires |
|---|---|---|
| Environment variable | The user obtains a token and exports it before starting the client. The configuration file refers to the variable and never contains the token. | The user obtains a new token and restarts or reloads the server entry. Works everywhere. Choose a TTL that covers a working session. |
| Header helper | The client runs a command that prints the current headers, and runs it again after a `401`. | Renewed with no user action. Supported by Claude Code and Codex, and by agents built on the MCP SDKs. |
| Per-request token | The calling application passes the token with every model request. | The application supplies a fresh token each time. This is how the hosted model APIs work. |

Two rules apply to all three. Do not write a token into a file that is committed or shared; every client below can reference a variable instead. And do not reuse one token across people: the token's subject is what the audit trail records as the caller.

### 10.2 Coding agents and editors

**Claude Code.** From the command line, at user scope:

```bash
claude mcp add --transport http --scope user munarium \
  https://munarium.example.com/mcp \
  --header "Authorization: Bearer $MUNARIUM_TOKEN"
```

That form stores the expanded token in the user's own configuration. To share the server definition with a team without sharing a token, commit a `.mcp.json` at the project root that references the variable:

```json
{
  "mcpServers": {
    "munarium": {
      "type": "http",
      "url": "https://munarium.example.com/mcp",
      "headers": { "Authorization": "Bearer ${MUNARIUM_TOKEN}" }
    }
  }
}
```

For automatic renewal, replace `headers` with a helper. Claude Code runs it at each connection and again after a `401` or `403`, then retries the call once:

```json
{
  "mcpServers": {
    "munarium": {
      "type": "http",
      "url": "https://munarium.example.com/mcp",
      "headersHelper": "munarium-agent-token --format headers-json"
    }
  }
}
```

The helper must print a JSON object such as `{"Authorization": "Bearer eyJ..."}` within ten seconds. Three details matter for Server:

- When a static `Authorization` header is configured and Server rejects it, Claude Code reports the connection as failed and does not attempt OAuth. That is the behaviour this design wants.
- For a project-declared server, variables whose names contain `TOKEN`, `KEY`, `SECRET`, `AUTH` or `PASSWORD` are removed from the helper's environment, so the helper must read its own credential from a file or the operating system's credential store.
- The variable name must not begin with `ANTHROPIC_`; those names are not expanded into headers for remote servers.

Tools appear to the model as `mcp__munarium__search_collection` and so on.

**OpenAI Codex CLI.** In `~/.codex/config.toml`:

```toml
[mcp_servers.munarium]
url = "https://munarium.example.com/mcp"
bearer_token_env_var = "MUNARIUM_TOKEN"
```

`bearer_token_env_var` names the variable; Codex sends its value in `Authorization`. For renewal, Codex has `http_headers_helper`, a local command that prints a JSON object of headers and is run again once after a `401` or `403`. An explicit bearer token takes precedence over a helper-provided `Authorization`, so use one or the other.

**Cursor.** In `.cursor/mcp.json` for a project or `~/.cursor/mcp.json` for the user:

```json
{
  "mcpServers": {
    "munarium": {
      "url": "https://munarium.example.com/mcp",
      "headers": { "Authorization": "Bearer ${env:MUNARIUM_TOKEN}" }
    }
  }
}
```

**VS Code with GitHub Copilot.** VS Code documents two formats, and the older one is scheduled for removal in February 2027. The older editor-managed format, in `.vscode/mcp.json`, can prompt for the token and store it without writing it to the file:

```json
{
  "inputs": [
    { "type": "promptString", "id": "munarium-token",
      "description": "Munarium agent token", "password": true }
  ],
  "servers": {
    "munarium": {
      "type": "http",
      "url": "https://munarium.example.com/mcp",
      "headers": { "Authorization": "Bearer ${input:munarium-token}" }
    }
  }
}
```

The newer portable format, a `.mcp.json` at the workspace root or `~/.copilot/mcp-config.json`, uses the `mcpServers` key and `${MUNARIUM_TOKEN}`, the same shape as the Claude Code project file above. Input prompts are not supported in the portable format, so it relies on the environment variable.

**Gemini CLI.** In `~/.gemini/settings.json` or a project's `.gemini/settings.json`. Note `httpUrl`; in this client `url` selects the older SSE transport:

```json
{
  "mcpServers": {
    "munarium": {
      "httpUrl": "https://munarium.example.com/mcp",
      "headers": { "Authorization": "Bearer ${MUNARIUM_TOKEN}" }
    }
  }
}
```

Gemini CLI documents variable expansion for settings values in general but shows it for MCP only in the `env` block, so expansion inside `headers` needs to be confirmed in testing.

**Windsurf and Devin.** Windsurf's documentation now lives under Devin's, and the configuration file location has moved; the editor's "Open MCP config file" command is the reliable way to find it. The entry uses `serverUrl`:

```json
{
  "mcpServers": {
    "munarium": {
      "serverUrl": "https://munarium.example.com/mcp",
      "headers": { "Authorization": "Bearer ${env:MUNARIUM_TOKEN}" }
    }
  }
}
```

**Claude Desktop.** Remote servers are added as custom connectors in the application's settings, not in a file. The connector form accepts fixed request headers, where `Authorization` with the value `Bearer <token>` can be entered, with sign-in set to none. The value is static, so it is replaced by hand when the token expires. For this client a longer TTL, up to the 24-hour maximum, is the practical choice.

### 10.3 Clients that only speak stdio

A client with no remote-server support can reach Server through a local relay that speaks stdio to the client and HTTP to Server. The widely used one is the `mcp-remote` package:

```json
{
  "mcpServers": {
    "munarium": {
      "command": "npx",
      "args": ["mcp-remote", "https://munarium.example.com/mcp",
               "--header", "Authorization:${AUTH_HEADER}",
               "--transport", "http-only"],
      "env": { "AUTH_HEADER": "Bearer <token>" }
    }
  }
}
```

The header is written without a space after the colon, with `Bearer` and the token together in the variable, because several clients do not escape spaces in arguments. `--transport http-only` stops the relay from falling back to the SSE transport, which Server does not serve. The relay also accepts `--header-file`, which keeps the token out of the process list. This is third-party software running on the user's machine with the user's token, and an organisation should review and pin it as it would any other dependency.

### 10.4 Agents built on a model API or an SDK

**Hosted model APIs.** With these, the model vendor's servers call `/mcp`, not the application.

Claude API, Messages endpoint with the MCP connector beta:

```json
{
  "mcp_servers": [
    { "type": "url", "name": "munarium",
      "url": "https://munarium.example.com/mcp",
      "authorization_token": "<token>" }
  ],
  "tools": [ { "type": "mcp_toolset", "mcp_server_name": "munarium" } ]
}
```

OpenAI Responses API:

```json
{
  "type": "mcp",
  "server_label": "munarium",
  "server_url": "https://munarium.example.com/mcp",
  "authorization": "<token>",
  "allowed_tools": ["list_collections", "search_collection", "ask_collections"],
  "require_approval": "never"
}
```

Both take the token on every request and do not store it, which suits short-lived tokens: the application asks the broker for a token for the current user and passes it along. Both have the same two consequences for a deployment, and they are significant:

- **`/mcp` must be reachable from the public internet**, because the caller is the vendor's infrastructure. Server's security posture assumes :8080 is reachable only from the API-management layer, so the gateway must publish `/mcp` deliberately, and nothing else.
- **Tool results pass through the vendor's servers** on their way to the model. That is true of any agent, since the model reads the results, but here the vendor also makes the request. Operators whose collections must not leave their network should use a client-side agent with a model they host, not a hosted connector.

**Claude Agent SDK.** The application process makes the MCP call, so the endpoint does not need to be public:

```python
options = ClaudeAgentOptions(
    mcp_servers={
        "munarium": {
            "type": "http",
            "url": "https://munarium.example.com/mcp",
            "headers": {"Authorization": f"Bearer {token_for(user)}"},
        }
    },
    allowed_tools=["mcp__munarium__*"],
)
```

The TypeScript form is the same object under `mcpServers`. The SDK runs no interactive sign-in, so the header is the only credential. Build the options with a current token for each session.

**MCP SDKs directly.** A custom agent should fetch the token per request instead of holding one. The TypeScript client transport takes a provider that it calls before every request:

```ts
const transport = new StreamableHTTPClientTransport(
  new URL("https://munarium.example.com/mcp"),
  { authProvider: { token: async () => tokenFor(user) } }
);
```

In the Python SDK the headers belong to the HTTP client passed to `streamable_http_client`, and rotation is done with that client's authentication hook. The OpenAI Agents SDK has an equivalent client-side class that takes `url` and `headers`.

A service that runs agents for many users must hold one token per user and never a shared service token. Otherwise every agent call is attributed to the service, and every user receives the service's clearance.

### 10.5 Summary

| Client | Where configured | Token source | Renewal without restart |
|---|---|---|---|
| Claude Code | `claude mcp add`, `.mcp.json` | `${VAR}` in `headers`, or `headersHelper` | Yes, with `headersHelper` |
| Codex CLI | `~/.codex/config.toml` | `bearer_token_env_var`, or `http_headers_helper` | Yes, with the helper |
| Cursor | `.cursor/mcp.json` | `${env:VAR}` in `headers` | No |
| VS Code (Copilot) | `.vscode/mcp.json`, or portable `.mcp.json` | Password input, or `${VAR}` | No |
| Gemini CLI | `.gemini/settings.json` | `${VAR}` in `headers` (to confirm) | No |
| Windsurf, Devin | Editor MCP config file | `${env:VAR}` in `headers` | No |
| Claude Desktop | Custom connector settings | Fixed request header, entered by hand | No |
| stdio-only clients | `mcp-remote` relay | `--header` or `--header-file` | No |
| Claude API, OpenAI Responses API | Request body | `authorization_token`, `authorization` | Yes, per request |
| Claude Agent SDK, MCP SDKs | Application code | `headers`, or a token provider | Yes, in code |

### 10.6 What this requires of Server

The client survey feeds back into the design in five places.

1. **An expired token must be recognisable.** Most clients cannot renew a token, and Gemini CLI and Devin are documented as treating a `401` as the start of an OAuth sign-in. Against Server, which has no authorization server, that produces a failed discovery and an error that does not mention expiry. Server's `401` must therefore carry a JSON-RPC error whose message says plainly that the token has expired and names the problem slug, and it must not send a `WWW-Authenticate` header containing `resource_metadata`, which would invite a discovery that cannot succeed. The reference documentation should list, per client, what an expired token looks like.
2. **Token lifetime is the usability lever.** With a one-hour default and six of the ten client types in the table unable to renew, a deployment will either raise the TTL toward 24 hours or supply a helper. Both are legitimate. The guidance should be explicit: helper-capable clients keep short tokens; the rest use a TTL matched to a working day, with revocation checking enabled (`MUNARIUM_TOKEN_REVOCATION_CHECK=true`) so a longer-lived agent token can still be withdrawn.
3. **The legacy handshake is required, not optional.** Of the clients surveyed, only the MCP TypeScript SDK states which protocol revision it speaks. The others must be assumed to send `initialize`. This confirms the dual-era behaviour in section 4.2 and argues for leaving `MUNARIUM_MCP_LEGACY_INITIALIZE` enabled by default for a long time.
4. **SSE fallback must fail cleanly.** Several clients try Streamable HTTP and fall back to the older SSE transport on a `404` or `405`. Server answers `GET /mcp` with `405`. Clients that fall back on that response will then report that SSE is unsupported, which is correct but confusing, so the examples above pin the HTTP transport wherever the client allows it.
5. **Hosted connectors change the exposure.** Section 10.4's requirement that `/mcp` be public for hosted model APIs should appear in the security posture and the deployment checklist as a decision the operator makes, with the gateway publishing that single path and applying its own rate limits.

A sixth item is a test obligation, not a design change: before release, each client in the table should be connected to a real Server and taken through connect, list, call and token expiry, and the results published in the reference page with the client versions used.

## 11. Deferred

- **Streaming turns and progress.** `POST /v1/sessions/{id}/turns/stream` already emits stage events. Mapping them to progress notifications on an SSE response is feasible but adds a streaming body to the MCP path and to the audit wrapper. Wait for evidence that single-response turns time out in practice.
- **Tasks.** The tasks extension suits long builds, which are not exposed, so nothing proposed here needs it.
- **Resources and prompts.** Collections and sessions could be modelled as resources. Tools cover the same reads with clearer authorization, and one primitive is easier to audit than three.
- **`listChanged` notifications.** The tool list is static per token scope set, so there is nothing to notify.
- **An MCP OAuth authorization server.** Out of scope by the security posture (section 4.3).

## 12. Open decisions

1. **Is `mcp` a separate required scope?** This proposal says yes. The alternative, accepting any `query` token on `/mcp`, is simpler for operators and removes the separation between application and agent tokens.
2. **A read-only evidence scope.** Should `get_evidence` and `read_evidence_rows` wait for a scope that cannot seal, or ship on `evidence` with the limitation documented?
3. **Agent spend and load ceiling.** Is the provider-config daily cap enough for completions, or does MCP need a per-token or per-uid token budget before paid tools are enabled by default? With an endpoint-backed embedder and reranker the same question applies to retrieval calls, which the paid-tools switch does not cover and which are charged to the collection's scope, not the agent's.
4. **Phase 2 memory tools.** Whether to extend ledger routes to accept a version-bound capability token, and under which scope names. This is a shared semantic choice and needs a hub decision record first.
5. **Ingestion over MCP.** An agent filing documents into a collection it is cleared for is a plausible request. It changes the corpus, so it is excluded here. If wanted, it should be its own scope and its own review, and it should not precede R3, which bounds what a hostile PDF can do to a replica.
6. **Legacy protocol support window.** How long to keep answering pre-2026 `initialize`.
7. **Release vehicle.** The change is additive and off by default, which fits a minor release. It still adds a network surface and a token scope, so it should be called out in release notes and the security posture, not folded into a patch.
8. **Order relative to R7.** Whether MCP waits for work-class admission or ships on the interim pool (section 9.2).
9. **A reference token broker.** Most clients need a command that returns a token for the signed-in user (section 10.1). Should Server ship a reference implementation, for example an `mmctl` subcommand that exchanges an identity-provider sign-in at the API-management layer for a capability token, or leave this entirely to each deployment? It must never hold the `mgmt` credential on the user's machine.
10. **Default token lifetime for agent tokens.** Whether tokens carrying the `mcp` scope keep the one-hour default, given that most clients cannot renew (section 10.6).

## 13. Limits of this analysis

- **The planned work is not built.** Section 9 and the passages that refer to R, IR and U items describe how this design meets [airk-analysis.md](airk-analysis.md) as proposed. Field names, class names, policies and the collection-binding rule there are proposals and may change in implementation; the mapping should be rechecked against each item when it is designed in detail. Which items are in the "most" that will be implemented was not specified, so each dependency is stated with its fallback.

- **Protocol pages not read.** The discovery result schema, the base error-code table, and the progress, cancellation, subscription and task pages of the `2026-07-28` revision were not read. The exact fields of the `server/discover` result, and whether a tools-only server has any obligation on the subscription endpoint, must be confirmed from the schema before implementation. The tool annotation field names were not confirmed for this revision.
- **Client configuration is from documentation, not from testing.** Section 10 was written from each vendor's published documentation on 19 November 2026 and nothing in it was run, since no endpoint exists. Points the documentation left open are marked there: variable expansion inside `headers` for Gemini CLI, the exact label of the fixed-header field in Claude Desktop, and what Cursor, VS Code, Codex and `mcp-remote` do on a `401` with a static header. Only one client states which protocol revision it speaks. `mcp-remote` is third-party software and was not reviewed. `munarium-agent-token` is a placeholder name.
- **Server areas not examined.** The `interactions` table constraints and the report queries that read `plane`; the session TTL and turn limits; and the cost of building the REST router a second time for in-process dispatch, as `ServerApiSvc::new` does today.
- **Matrix is cited from its published documentation**, not from a review of its implementation.
- **No prototype was built.** Line counts and defaults here are estimates.
