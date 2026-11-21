# Hosting MCP tools in Munarium Server

**v1.5 implementation design.** Server v1.5 requires a qualified MCP endpoint. Operators enable `POST /mcp` for agents acting on behalf of an individual user. This document owns the MCP decisions A1–A9, tool contract and release tests. The roadmap owns cross-component sequencing. [Decision and test index](#12-decision-and-test-index) · [v1.5 roadmap](README.md) · [Retrieval and console analysis](airk-analysis.md)

Use the published v1.4 Server and SDK artifacts for compatibility and the separately identified checkout for implementation analysis. The roadmap records both source identities. [Source baselines](README.md#source-baselines) · [Server 1.4 upgrade and release guide](../guides/server-1.4.md)

## 1. Summary

The gateway handles TLS. An operator switch registers the route on the existing REST listener. The v1.5 catalog includes nine query and session tools and two sealed-evidence readers. Token scope, enabled operations and spending authority determine which tools each caller can see. [Tool catalog](#51-query-tools) · [Deferred governed memory](#111-governed-memory-tools)

## 2. Why host it in Server

A named tool can pass the caller's bearer token through the existing handlers for scoped search, sessions and original-reference authorization. This preserves one place to enforce tenant, classification, retention and provider rules. The two filtered listing tools need explicit authorization and attribution because the native management listings disclose more information. A compiled dispatch table follows the pattern in the existing gRPC bridge. [Server security posture](../security-posture.md) · [Native API bridge](../../src/munarium-server/src/grpc_api.rs).

The Matrix project owns its service, MCP catalog and documentation. Server can qualify local MCP queries without Matrix or Lens. For enrolled search, exercise the real inference admission and recovery paths against a recording endpoint so the test can count physical calls and charges. The Server candidate also qualifies with the pinned Lens image under LENS-D1 and LENS-D2. Existing optional evidence integrations follow the Matrix transfer and v1.4 parity checks. [Matrix and Lens handoffs](repository-handoffs.md)

## 3. Design rules

| ID | Requirement | Acceptance |
|---|---|---|
| MCP-RULE-1 | Preserve native v1.4 clients, credentials and contracts. | Run the published clients against the candidate on both native transports. Additive changes to token issuance or reporting follow the contract process. |
| MCP-RULE-2 | Use the verified caller's authority on every operation. | Enforce tenant, uid, scope, clearance, compartments, runbook restrictions and source retention across listing, dispatch and any continuation. |
| MCP-RULE-3 | Compile an explicit tool allowlist. | Fix the route, method, parameter mapping, work class, side effects and allowed model stages for each entry. |
| MCP-RULE-4 | Confirm a durable, attributed admission before executing every authorized tool invocation (A6). | Confirm the recorded tenant, uid, token ID, request ID and tool before dispatch. On failure, the tool performs zero reads, mutations and provider calls (§7.3). |
| MCP-RULE-5 | Preserve uncertainty and refusal. | Keep native evidence and error types. A failed retrieval, incomplete fan-out or lost response cannot become an empty success. |
| MCP-RULE-6 | Require operator opt-in. | With the switch unset, old routes and defaults work and `/mcp` returns 404. |
| MCP-RULE-7 | Keep tool metadata independent of tenant data. | Publish fixed names, descriptions and schemas. Treat returned passages, citations and descriptions as untrusted data. |

## 4. Hosting and transport

### 4.1 Listener

Register `/mcp` on the existing HTTP listener only when enabled with authenticated capability tokens. Reject an incompatible enabled configuration with an actionable startup error. The gateway can publish just this path on a separate hostname. Existing deployments need no listener, credential, network policy or chart change.

### 4.2 Protocol revision

<a id="a3"></a>

**A3 · decided.** The v1.5 MCP profile must pass a real-client run on each of two Streamable HTTP revisions, `2026-07-28` and `2025-11-25`. Exercise both against the release candidate as described in §10.5. A missing or failed run for either revision keeps MCP qualification open. Pin each schema revision and digest in the conformance tests. The modern wire behavior is specified below. [Modern wire schema](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/schema/2026-07-28/schema.json) · [HTTP binding](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http).

| Concern | Modern behavior |
|---|---|
| Request | Validate a single UTF-8 JSON-RPC message, media types and body size before dispatch. Clients send an `Accept` header allowing JSON and SSE. A request includes a string or integer ID and the standard version, client identity and capability metadata under `params._meta`. Client metadata supplies no authority. |
| Headers | Validate `MCP-Protocol-Version` against body metadata, `Mcp-Method` against the method, and `Mcp-Name` against the name for a named call. Apply the binding's decoding rule before comparison. A list or discovery request needs no name header. |
| Operations | Serve `server/discover`, `tools/list`, `tools/call`. Discovery returns `resultType: "complete"`, supported versions, the `tools` capability and server identity under `_meta["io.modelcontextprotocol/serverInfo"]`. Allow a client to start directly with a tool request. |
| Errors | Header mismatch returns HTTP 400 / `-32020`. An unsupported version returns 400 / `-32022` with supported versions. An unknown RPC method returns 404 / `-32601`. An unknown tool name inside a valid call is an invalid parameter (`-32602`). Handle malformed envelopes separately from a tool's execution errors. |
| Transport and cache | An accepted notification receives 202 with an empty body. Refuse batches. Use private or disabled caching for scope-dependent listings. Validate origin against exact configured origins when present and return 403 on rejection. Return 405 for standalone GET and DELETE on this profile. |
| Optional features | Advertise only the implemented tools capability. Omit list-change and subscription capabilities. Match the official SDK's no-subscription behavior for `subscriptions/listen`. Unary calls need no progress updates, tasks or SSE. Bound work after disconnect and preserve any committed side effect. |

The discovery schema, reserved error codes, subscription behavior and progress/cancellation semantics above have published references. Task support requires separate extension negotiation. Include tests both for the advertised capabilities and for a client requesting an unsupported optional feature. [Discovery](https://modelcontextprotocol.io/specification/2026-07-28/server/discover) · [Base errors](https://modelcontextprotocol.io/specification/2026-07-28/basic) · [Subscriptions](https://modelcontextprotocol.io/specification/2026-07-28/basic/patterns/subscriptions) · [Official SDK subscription behavior](https://py.sdk.modelcontextprotocol.io/handlers/subscriptions/) · [Progress](https://modelcontextprotocol.io/specification/2026-07-28/basic/patterns/progress) · [Cancellation](https://modelcontextprotocol.io/specification/2026-07-28/basic/patterns/cancellation) · [Tasks](https://modelcontextprotocol.io/extensions/tasks/overview).

**Legacy clients.** Accept `initialize` for `2025-11-25`, acknowledge `notifications/initialized` and support `ping`. Process subsequent tool requests with that revision's headers and result shapes. This profile uses no MCP session ID. Supporting `2025-03-26` or `2025-06-18` would require their own fixtures and client evidence, including missing-header behavior where applicable. A malformed modern request must never trigger a permissive legacy dispatch. [Version negotiation](https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning) · [Legacy lifecycle](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle) · [Legacy HTTP binding](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports).

### 4.3 Authentication and the uid contract

<a id="a1"></a>

**A1 · decided.** The issuer must explicitly grant `mcp` on a tenant-bound capability JWT before the caller can enter the endpoint. This admits authenticated protocol discovery and a filtered tool listing. Each tool invocation also requires its normal data-plane scope, including `query` for the collection and runbook listing tools. Enabling the listener grants no new authority to existing application tokens.

| Scopes on a valid capability JWT | MCP protocol discovery and `tools/list` | MCP query tool invocation | Native v1.4 query operation |
|---|---|---|---|
| `query` only | Entry refused with 403 `scope-missing` | Entry refused before dispatch | Allowed within the token's existing authority |
| `mcp` only | Discovery succeeds. The list is empty. | Refused before dispatch because the token lacks `query` | Refused because the token lacks `query` |
| `mcp` and `query` | Discovery succeeds. Enabled query tools are visible. | Allowed if the token also satisfies the operation's tenant, clearance, compartment, resource and runbook checks | Same authority as `query` alone |

Check the caller's asserted uid against the verified subject when present. Refuse static role credentials even when they could call the corresponding native operation. Surface the existing authentication slug and correct HTTP status, including 401 for invalid or expired credentials and 403 for insufficient entry scope. Advertise a Bearer challenge on 401. An OAuth-capable gateway can advertise its own protected-resource metadata. A bare Server should advertise only authentication mechanisms it supplies. [Problem registry](../api/errors.md) · [MCP HTTP authorization](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization).

The transport scope controls entry to `/mcp`. Additional scopes on the same token retain their v1.4 meaning on native routes. The issuer should grant only the query authority the agent needs. A deployment that needs MCP-only access or a cross-transport spend limit must enforce it at all applicable entry points. Issue `mcp` tokens and switch on the route only after all intended replicas understand the new profile. Keep v1.4 token issuance, default TTL and verification semantics working on either side of a rolling upgrade. [Token implementation](../../src/munarium-access/src/lib.rs).

## 5. Tool contract

### 5.1 Query tools

All entries require `mcp` plus the scope shown. List each authorized tool once, in deterministic order, with the closed argument forms enabled for it. Compile these schemas independently of tenant data. Disabling new work across the endpoint can leave a recovery-only form, independent of whether the caller has any receipts. Check resource-specific policies and balances at execution. Enforce the advertised forms and current resource authority on direct calls. [Tool protocol](https://modelcontextprotocol.io/specification/2026-07-28/server/tools).

| Tool | Operation | Scope | Effects and limits |
|---|---|---|---|
| `list_collections` | New filtered read | `query` | Return active, permitted collections in the caller's tenant. Apply authorization before ordering, paginating and producing counts or cursors. |
| `list_runbooks` | New filtered read | `query` | Enforce the runbook allowlist and collection clearance. Return a bounded projection without restricted collection names, descriptions of hidden resources or raw runbook YAML. |
| `search_collection` | `POST /v1.2/search` | `query` | Accept query, collection, optional `top_k` and index version. Use that collection's vocabulary and authorized retrieval policy. |
| `ask_collections` | `POST /v1.2/query` | `query` | Accept a question, collection selection and optional effective date. Preserve checked citations and source references. Requires explicit authorization for provider work. |
| `open_session` | `POST /v1/runbooks/{name}/sessions` | `query` | Creates an application session and binds its initial runbook/clearance. |
| `ask_session` | `POST /v1/sessions/{id}/turns` | `query` | Appends a turn. Allow bounded retrieval options and an authorized research profile. Model selection belongs to the runbook. Expansion and verification can invoke providers in addition to completion. |
| `get_session` | Bounded projection of `GET /v1/sessions/{id}` | `query` | Read an authorized page or a specific turn ordinal using a bounded store query. Preserve native field values for the selected turns. |
| `close_session` | `POST /v1/sessions/{id}/close` | `query` | Changes session state. Check ownership and the current scope. |
| `authorize_publication` | `GET /v1.2/collections/{id}/publications/{publication_id}` | `query` | Returns the authorized original reference, with no grant to download bytes. |
| `get_evidence` | `GET /v1/evidence/{id}` | Read-only evidence capability | Read and audit the permitted sealed manifest. |
| `read_evidence_rows` | `GET /v1/evidence/{id}/rows` | Read-only evidence capability | Read and audit a bounded window of canonical CSV rows. |

**Sessions.** A v1.4 session carries the clearance and compartments recorded at open. Before an MCP turn, transcript read or close, check current tenant, uid, runbook permission and that the live token covers the saved session authority. Qualify a renewed token, narrower same-uid token and another user's token. Preserve native v1.4 session semantics. For multi-collection answers, the proposed MCP request selects the strict availability policy. If a required collection is unavailable, the tool cannot claim a complete answer. Never expose the identity of an unauthorized collection through that check. [Session operations](../../src/munarium-server/src/sessions_api.rs) · [Evidence hierarchy](../guides/evidence-hierarchy.md).

<a id="a4"></a>

**A4 · decided.** Both evidence readers must work with an additive read-only evidence capability before the v1.5 MCP endpoint ships. Apply A1 at each call: require `mcp` and the read-only grant. On the native manifest and row handlers, the read-only grant suffices for an authorized read. Enforce tenant, clearance, compartments and retention on each resolution, and record the caller and outcome without copying evidence into the access audit. The read-only grant cannot seal, upload or commit evidence. Existing native `evidence` tokens retain their v1.4 read and sealing permissions. Record the new scope’s wire spelling and contract in the shared decision process. [Evidence handlers](../../src/munarium-server/src/evidence_routes.rs).

### 5.2 Tool results

| Outcome | What the agent receives |
|---|---|
| Fresh native dispatch | Return the native result unchanged in `structuredContent` and its JSON serialization in a text block. Preserve integer precision, array order, provenance, citations, optional fields and future extensions. |
| Duplicate execution or recovery | Return a bounded, explicitly typed operation receipt in both representations so a text-only client can recognize it. A namespaced result `_meta` field also distinguishes native results, MCP projections and operation receipts. |
| Audit admission unavailable | A typed refusal indicating that this invocation did not run. If the commit acknowledgment was lost, Server can know that it withheld dispatch even while the audit record’s presence remains uncertain. |
| Normal domain refusal | A tool result with `isError: true`, the native public problem fields including type URI and status, and R11 retry guidance when present. Derive a short slug from the type for display. An error message must never carry driver output, credentials or inaccessible data. |
| Invalid message or tool name | A JSON-RPC protocol error with the appropriate request ID. Invalid arguments on a recognized tool yield a tool error before provider work or mutation. For domain failures after dispatch, preserve the observed outcome and any uncertainty. |
| Response exceeds the size bound | A typed tool error. For a read, name an available limit or cursor. If an operation may already have changed state or incurred spend, report that uncertainty and provide the authorized recovery read when available. Prevent silent truncation and automatic replay. |

Measure the serialized envelope, including both structured and text representations, with bounded buffering. Bound inputs and responses before expensive or stateful operations where feasible. Gate every state-changing or budget-consuming tool on the interruption cases below. A JSON-RPC request ID correlates a single exchange and may change on reconnection.

Keep citation quotes exact in both result formats. Treat source text as evidence for the model and never copy it into schemas, annotations or action instructions. Document field absence on an older response and distinguish a skipped retrieval stage from an empty corpus. Generate an optional output schema only once both conformance and the reference page can use the same source. Annotations describe side effects for the user interface. Enforcement belongs to the dispatcher and native handlers. [Wire compatibility policy](../wire-compatibility.md).

### 5.3 Recovering an interrupted call

**Proposed additive MCP contract.** The client chooses and retains an operation key before sending `open_session`, `ask_session`, `close_session`, `ask_collections` or `search_collection`. Search takes a key even on a local collection because enrollment can change before a retry. Each of these existing tools accepts an execution request with the key and ordinary arguments, or a recovery request containing only the key. Recovery reads a bounded receipt and never starts the operation. The other six tools can repeat their authorized, bounded reads. Qualify both argument forms on both A3 revisions. The hub record, export and re-vendoring must precede reliance on the new wire shape. Native v1.4 calls keep their existing signatures and behavior.

| Tool | What the caller can establish from its own key |
|---|---|
| `open_session` | Commit the new session ID and its invocation link atomically. Recovery identifies this session even when the caller opened two sessions on the same runbook concurrently. |
| `ask_session` | Commit the invocation link with the appended turn and return its session ID and ordinal. Permit a bounded `get_session` read at that ordinal. A provider reservation can survive a failed turn. Report spend separately from the append outcome. |
| `close_session` | Link the observed terminal state to the invocation in the close transaction, including an already closed or expired session. A recovery read returns that state without loading the transcript. |
| `ask_collections` | Return the observed answer-operation outcome and authorized spend status. Distinguish zero provider work, settled usage and an outstanding reservation. If response content is unavailable, say so. A lookup never generates a replacement answer. |
| `search_collection` | A local-only execution records zero inference work. For an enrolled search, correlate I2 receipts for embedding, reranking and any authorized shadow work. Report known completion, fallback or refusal and any unresolved spend without submitting another search. |

Bind the key to the verified tenant, uid and tool. For new work, atomically claim the key, store the canonical argument digest and commit the A6 admission. Only the confirmed claimant may dispatch. Distinct keys identify distinct calls even with identical arguments. Every duplicate or recovery request needs its own confirmed A6 admission before any receipt read. Recheck its current credential, original resource authority and retention before revealing receipt existence, a conflict, an outcome or a reference. An authorized duplicate with matching arguments receives the receipt. Changed arguments receive a typed conflict. Keep request bodies, transcripts, answers and evidence out of receipts.

Return a bounded status for pending, completed, refused or unknown work. An absent or inaccessible receipt yields the same response. This can also occur before an in-flight admission becomes visible. Link effects and physical attempts by invocation ID. Once the execution deadline passes, report unknown for effects that still lack a durable outcome. For a known result that exceeded the size limit, return its permitted reference or completion status. Keep provider usage separate from reserved capacity when the provider outcome is uncertain.

The key includes an immutable UTC expiry and a caller-generated random identifier. Enforce the maximum validity window and the admitted work deadline against the shared store clock when claiming the key, after any lock wait. Recheck expiry before dispatch. Pruning uses the same clock and retains claims through expiry, including across restarts. A late request with a pruned key is refused. Clients must never extend the expiry or create a fresh key automatically after interruption. Expiry leaves unresolved reservations and audit retention intact. Pin the format, validity window, size bounds, result discriminator and scoped refusal shapes in the shared contract.


## 6. Dispatch boundaries

| Excluded from the v1.5 MCP catalog | Native routes or operations | Reason |
|---|---|---|
| Index lifecycle | `/v1/indexes/*`, `/v1/collections/{id}/activate-index`, `/v1/index-artifacts/*`, `/v1/index-build-jobs*`, `/v1/retrieval-rollout*` | Alters what every reader is served. Builds are expensive and promotion is fleet-gated. |
| Collections, shapes, runbooks | `POST /v1/collections`, `/v1/shapes`, `/v1/runbooks*` (apply, run, approve, remove), `/v1/runs/*`, `/v1/authoring/*`, `/v1/chronology-rules*` | Defines the governance an agent is subject to. Approval is a human gate. |
| Ingestion | `/v1/sources`, `/v1/ingest*`, `/v1/versions/{id}/ingests` | Changes the corpus and future indexes. A later ingestion tool would need separate authority and R3 extraction limits. [Deferred scope](#11-deferred) |
| Retrieval model enrollment (planned, IR-1 and IR-4) | The provider conformance probe, and the per-collection embedder and reranker enrollment and policy API | Decides which model every reader's results pass through, and an embedder change forces a rebuild. Management role by design. |
| Build-job control (planned, R6 and R9) | Cooperative cancel, job progress and the job event ring | Operator view of builds an agent cannot start. |
| Vocabulary and publication governance | `/v1.2/vocabulary-settings`, `/v1.2/collections/{id}/vocabulary*` (write, refresh), `/v1.2/collections/{id}/governance` | Changes how every query in a collection is expanded or which publication governs. |
| Providers and budgets | `/v1/providers*`, `/healthai`, `/v1/max-tokens`, `/v1/monetary/*` | Credentials, spend ceilings and raw model access. A raw `complete` tool would make Server a model proxy for the agent. |
| Tokens | `/v1/access-tokens*` | An agent must not mint or revoke authority. |
| Evidence writes and retention | `POST /v1/evidence`, upload, commit, `DELETE`, legal hold, access audit | Sealing is a service role; purge and hold are management-only. |
| Governed memory | Ledger reads and writes under `/v1/versions` and `/v1/claims` | v1.4 uses native ledger credentials. MCP needs a separately designed lineage-scoped authority model. [Future governed memory](#111-governed-memory-tools) |
| Findings writes | `POST /v1/versions/{id}/findings` | A service-to-service write. |
| Reports and audit | `/v1/reports/*`, `/admin/*` | Management-only, and includes other users' activity. |
| Recovery and retention | `/v1/command-recovery*`, `/v1/source-retention` | Operator procedures with restore duties. |
| Platform plane | `/v1/platform/*` | mTLS-admitted authority, record and activation operations. |

The dispatcher applies two independent controls.

1. **The table is closed.** Compile each tool and argument form to one fixed handler. Native entries fix their method and route template and percent-encode path parameters with the existing bridge helper. The two filtered listings, bounded session projection and operation receipts each use a dedicated read handler. The caller supplies no route, method or audit query.
2. **Each handler checks authority.** Native entries retain the native checks. The dedicated reads enforce the same applicable tenant, uid, scope, resource and retention rules. Recovery resolves at most the caller's key for the named tool. Control, management and platform operations retain their existing credential and transport requirements.

A unit test enumerates every compiled form and rejects handlers or routes outside a separately maintained allowlist. A conformance test presents a fully scoped MCP token and verifies that representative administrative operations cannot be reached by any tool name or argument.

## 7. Implementation in Server

### 7.1 Layout

Add a module in `munarium-server` for version-specific envelopes and the compiled tool table. Build the native router once and share a clone with the in-process dispatcher, or measure the startup and memory costs of the existing bridge construction pattern. Keep `munarium-core` independent of HTTP. Validate any proposed SDK dependency against the musl build, licensing and dependency gates. The size of another implementation is a poor estimate for this work.

### 7.2 Dispatch

Validate the selected form, bounded arguments and live scopes, then confirm the §7.3 admission. For a fresh routed execution, use the native router, the caller's bearer and verified uid, and a private request extension with the admission identity. The inner router omits `/mcp`. A receipt lookup uses only the scoped recovery handler. With the endpoint enabled it stays available to authorized callers after new tool execution, generation or inference is disabled or a budget is exhausted. Lookup incurs no model work or new reservation. [Native bridge](../../src/munarium-server/src/grpc_api.rs) · [Capture middleware](../../src/munarium-server/src/middleware.rs).

For `list_collections` and `list_runbooks`, resolve the ordinary query access context before reading, then authorize and project in tenant scope before pagination. Test a collection or runbook outside the first page that the token *is* allowed to see. Bind any opaque cursor to the caller's scope and ordering. The existing management listings remain available to native callers with their existing roles. [Collection listing](../../src/munarium-server/src/collections_api.rs) · [Runbook listing](../../src/munarium-server/src/runbooks_api.rs).

### 7.3 Audit and metrics

<a id="a6"></a>

**A6 · decided.** Before executing any authorized `tools/call`, Server must receive confirmation that the shared durable audit store has committed its admission record. Check the verified credential, scopes, tool and bounded arguments first. Write a server-assigned invocation ID, `plane = "mcp"`, fixed tool name, tenant, uid, token ID, request ID and time. Keep credentials, argument bodies, retrieved text and evidence rows out of the admission record. A content-free, typed refusal ends the call if the store is unavailable or the commit acknowledgment is missing by the deadline. No tool handler runs, reads application data, mutates state, reserves provider spend or contacts Lens or another provider in that case.

Apply the same gate to all eleven tools, including `list_collections` and `list_runbooks`, before fetching their data. Authenticated protocol discovery, the legacy handshake and the scope-filtered `tools/list` catalog are protocol operations. They execute no tool. Keep transport refusals and audit-admission failures observable with bounded metrics. Trust identity only after verification.

The v1.4 interaction writer enqueues best-effort records and counts drops and failed inserts. The memory store discards those records. Keep native REST and gRPC capture behavior intact. An MCP invocation requires a confirmed durable commit even when an interaction writer is healthy. A memory-only deployment without a durable audit store must refuse tool execution. [Interaction writer](../../src/munarium-server/src/interactions.rs).

Assign a fresh invocation ID to each admitted call, including duplicates and recovery reads. Correlate a lookup's own admission and result with the original execution's invocation ID when one exists. Attribute physical attempts, session changes and spend to that original ID. Count each admission once across MCP and its inner handler. Count confirmed dispatches and effects separately, using durable evidence. Admission alone leaves execution unknown after a crash. A missing outcome never triggers replay.

The v1.4 `interactions.plane` column is unconstrained text. The timeseries handler explicitly accepts only `rest` and `grpc`. Add `mcp` to that filter and its generated references. Count each admitted invocation once in the unfiltered total and report known outcomes separately. Test old SDK report decoders, older binaries reading mixed rows and counts across all three planes. Use additive storage changes for admission and outcome linkage where needed. [Database definition](../../src/munarium-store-pg/migrations/0010_identity_interactions.sql) · [Report filters](../../src/munarium-server/src/reports_api.rs).

### 7.4 Admission and cost

Acquire an R7 agent permit before parsing the body. Under B3, MCP traffic fits within the configured REST ceiling and leaves its authenticated control reserve available. Charge expensive stages under the heavy-work limit. Transfer or share admission context across the inner dispatch so an MCP call cannot take the same capacity twice. Test saturation with application requests, health and a token revocation in flight. Bound admitted work by deadline, including provider retries. Check cancellation, drop counters and permit recovery. [B3 control capacity](airk-analysis.md#b3) · [SC1 scale-out qualification](scale-out.md#sc1)

<a id="a2"></a>

**A2 · decided.** MCP generative model calls are disabled by default. An operator must explicitly enable generation, name the budget owner and configure both A8 allowances for the collection or index. These requirements apply to local and remote models, including deployments without price data. Currency may appear in reports when pricing is available. MCP generation admission uses tokens.

<a id="a8"></a>

**A8 · decided.** Operators configure a per-request allowance and a daily allowance, each in total input plus output tokens, at the collection or index level. Server resolves the policy from the authorized collection and pinned index. Tool arguments cannot select the policy or payer. In one admitted tool invocation, expansion, verification, final answers, re-asks and every physical retry all count toward the request total. A turn with `complete: false` can still request model-based expansion. Each scope’s daily allowance covers its requests across all replicas for the UTC day. Changing an index pin must preserve a collection’s accumulated daily usage. [Session execution](../../src/munarium-server/src/sessions_api.rs)

**Implementation recommendation for policy inheritance.** With no index-specific token allowances, a pinned index inherits the collection’s request and daily pair. A complete index-specific pair supplies the index’s own request ceiling and daily allowance. Reject an incomplete override during policy validation. A configured collection daily balance remains a separate limit across all of its pins, including indexes with overrides. Reserve the collection’s balance once per attempt and each distinct contributing index’s own daily balance when configured. Replacing or adding a pin cannot reset collection usage. The shared-request rule in [A9](#a9) compares the resolved per-request ceilings.

Before each physical submission, reserve against the remaining request allowance and each applicable daily allowance in one shared, durable admission decision. Use the A6 invocation identity and a distinct physical-attempt identity. The v1.5 policy estimates input from serialized bytes divided by the configured bytes per token for the model (default 3), rounded up. Add 64 framing tokens and the permitted output, including reasoning where the adapter accounts for it there. Insufficient allowance or unavailable accounting refuses the attempt before dispatch. Existing provider checks continue to apply.

Settle each reservation once against reliable usage. Keep the full reservation against its original request and day when dispatch or usage is uncertain, including after cancellation, restart or a lost reply. A retry needs a fresh reservation in the same request. Charge above-estimate usage in full, record and meter the excess, and admit later attempts only against the resulting balance. Settled usage can overshoot the request allowance by the last admitted attempt's estimate error. [Native token budgets](../tokenbudgets.md)

<a id="a9"></a>

**Shared generation · A9 decided.** For an attempt with multiple contributing collections or indexes, use their largest effective per-request allowance after resolving each contributor’s collection and index policy. The allowances do not add. Before dispatch, Server resolves the scopes that supply material or on whose behalf the stage runs, including source material carried forward in session context. For expansion before retrieval, use the authorized scopes selected for that expansion. An unrelated or merely available index cannot raise the limit. Every contributor must authorize the same payer. An unknown contributor or the absence of a commonly authorized budget refuses the attempt.

For example, Legal allows 2,000 tokens per request and Finance allows 8,000. Both contribute to a shared request, authorize the same payer and have sufficient daily balances. Legal alone retains its 2,000-token ceiling. The combined answer may exceed that ceiling because its single generation call serves both indexes and Finance authorizes the larger request. Its ceiling is 8,000 tokens. An initial 5,000-token attempt is admitted and, if it uses all 5,000, at most 3,000 remain for later stages and retries. If the contributing indexes change between stages, carry the request's usage and outstanding reservations forward when checking the next attempt. Refuse a submission that would take the combined request above 8,000.

The larger limit applies to the request allowance only. Each contributor must independently have enough daily tokens for the full attempt. Atomically reserve against the remaining shared request allowance and all applicable daily balances across replicas. Deduplicate repeated references to a scope and leave no partial reservation on refusal. Suppose the initial attempt uses 5,000 tokens, a later stage uses 2,000 and its physical retry uses 1,000, all with Legal and Finance contributing. With confirmed usage and sufficient daily balances, each collection uses 8,000 daily tokens, the common payer is charged 8,000 in total, and the invocation has zero request capacity left. Legal's standalone request ceiling remains 2,000. Correlate settlement to the physical-attempt identity so a repeated receipt cannot duplicate usage. Scope refusal details and usage reports to the caller’s authority.

Embedding and reranking on an enrolled collection incur inference costs even with generation disabled. Under I2, charge the collection's tenant-owned budget and record the verified caller. Use the same admission and accounting path as native reads. Record authorized shadow evaluations separately. The agent cannot select a different payer through tool arguments. Under I1, propagate a native typed refusal as a tool error, or preserve the skipped-stage disclosure when the operator has selected fallback. MCP tool arguments cannot relax the collection's policy, select its endpoint or enroll it. [I1 retrieval failure policy](airk-analysis.md#i1) · [I2 retrieval budgets](airk-analysis.md#i2)

### 7.5 Configuration

| Setting | Proposed default | Acceptance |
|---|---|---|
| `MUNARIUM_MCP` | `disabled` | An unset switch preserves the v1.4 listener surface. The enabled v1.5 profile serves both A3 revisions. |
| `MUNARIUM_MCP_ALLOWED_ORIGINS` | empty | A supplied origin requires an exact configured match. Server-side agents may omit it. |
| `MUNARIUM_MCP_MAX_CONCURRENCY` | Tune to the R7 pool using the qualification workload | Measure at the smallest supported pool size and record the numeric default in the [open-items register](README.md#open-items) before implementation. Protect native and control traffic. |
| `MUNARIUM_MCP_PAID_TOOLS` and operator-managed collection/index generation policy | `disabled`, with no owner or allowances supplied | Require explicit enablement, budget binding and both allowances under [A2](#a2), [A8](#a8) and [A9](#a9). Omit disabled execution forms from the catalog. Keep authorized receipt reads available under §5.3. |
| `MUNARIUM_MCP_RESULT_MAX_BYTES` | 262144 | Enforce the bound on the entire serialized tool response. Apply a separate proposed 1 MiB input ceiling. |
| `MUNARIUM_TOKEN_TTL_SECS` (existing) | 3600 seconds | The deployment operator configures the default for newly issued capability JWTs. The broker normally omits `ttl_secs` to use it. Section 10.1 gives the existing precedence and bounds. |

### 7.6 Documentation and contract effects

Generate a tool table from compiled entries for the future MCP API reference. Link the route, optional chart settings, authentication profile, cost boundaries and tested clients in the Server docs. Keep existing REST methods, typed gRPC tags, SDK signatures and required inputs intact. Recheck scope issuance, the additive timeseries filter, the required read-only evidence capability and the four released SDKs through their ordinary entry points. Publish/re-vendor applicable contract candidates through the owning process. [REST reference](../api/rest.md) · [gRPC coverage](../api/grpc.md).

## 8. Qualification and rollback

<a id="a5"></a>

**A5 · decided.** Hold Server v1.5 until the exact release candidate passes the complete MCP qualification suite. Record the Server revision and artifact digest with all results. A missing tool, a missing real-client run for either A3 revision, an A4 authority failure or another incomplete or failed required check keeps the Server release blocked. Qualify the enabled profile separately from the default-off upgrade path.

| Evidence | Pass condition |
|---|---|
| <a id="a5-t1"></a>**A5-T1** · [Complete tool catalog](#a5) | Exercise all eleven §5.1 tools through `/mcp` on the same candidate. For each, record the tool name, configuration, granted scopes, expected result or state transition and observed outcome. Use an explicitly budgeted synthetic provider for generation. Cover the filtered listings and direct-call denials separately. |
| <a id="a5-t2"></a>**A5-T2** · [Native baseline](#a5) | Use a populated database and v1.4 credentials, sessions, runbooks, pinned indexes and clients. With MCP off, replay successful native requests, refusals and budget limits over REST and gRPC. Compare stable fields and state transitions, documenting volatile IDs, timestamps and the comparison fixture. |
| <a id="a5-t3"></a>**A5-T3** · [Mixed replicas and rollback](#a5) | Keep MCP disabled while upgrading. Exercise v1.4 clients through both versions. Enable MCP only on qualified replicas, then drain and disable it, exercise old native reads/writes again, and verify access controls and old report readers on retained MCP rows. Leave additive data in place. Document any restore boundary before rollout. |
| <a id="a3-t1"></a>**A3-T1** · [Protocol fixtures](#a3) | Pin both A3 revisions, upstream schema digests and client versions. Verify headers, origin, direct modern calls, the legacy handshake, method errors, missing scopes, oversized input/results and unsupported features. Exercise requests without replica affinity. Attach the two [real-client transcripts](#105-interoperability-record) alongside the fixture results. |
| <a id="a1-t1"></a>**A1-T1** · [Entry and tool authority](#a1) | Run the three scope combinations in the authority table against discovery, listing, direct tool invocation and native REST/gRPC query entry points. A missing entry or operation scope must prevent tool dispatch and provider calls. Cover static roles, revoked/expired tokens, uid mismatch, narrower renewed authority, compartments, tenant separation and page boundaries. Test direct invocation of every hidden or paid tool. |
| <a id="a7-t1"></a>**A7-T1** · [Broker and lifetime](#a7) | On native v1.4 and the candidate, compare issuance at the built-in default, a configured default and a trusted per-request override. Check the 1- and 86,400-second bounds, native clamping and rejection of malformed input. On the candidate, issue an MCP token, change the default, and verify the old signed expiry is unchanged. After expiry plus the verification leeway, confirm 401 and zero tool execution, renew through the broker, and reconnect with the new credential on another replica. Check subject, tenant, permitted scopes and expiry. Reconnection must not replay an interrupted tool call. |
| <a id="a6-t1"></a>**A6-T1** · [Durable admission](#a6) | For each of the eleven tools, observe a confirmed record before its handler runs. Force write failure, deadline expiry and a committed write with a lost acknowledgment. Verify refusal, zero tool reads, mutations, budget reservations and provider attempts, including on the two listing paths. Verify the durable-store requirement in a memory-only deployment and distinguish authenticated protocol discovery from tool execution. |
| <a id="a6-t2"></a>**A6-T2** · [Audit recovery](#a6) | Stop a replica after admission before dispatch, then independently after a domain effect before outcome recording. Verify zero dispatches in the first fixture and one in the second. A report may claim only the execution its durable evidence establishes. Send a duplicate and a recovery read to a second replica. Require separate admission IDs linked to the original, zero further dispatches to the domain handler, and no receipt disclosure if the new audit admission fails. |
| <a id="a4-t1"></a>**A4-T1** · [Evidence readers](#a4) | Resolve a synthetic sealed manifest and a bounded row range through both MCP tools and the native handlers. A token with only the read-only grant can read natively and is refused MCP entry. Add `mcp` and both readers become visible and callable for authorized evidence. An `mcp` + `query` token without the evidence grant cannot list or invoke either reader. Exercise cross-tenant and insufficient-clearance refusals, retention and access auditing. With only the two MCP/read grants, verify that no evidence-write tool is reachable and that native seal, upload and commit are denied without mutation. Separately replay an authorized native read and seal with a v1.4 `evidence` token. |
| <a id="a5-t4"></a>**A5-T4** · [Native result parity](#a5) | Compare each fresh routed result to its actual native JSON, including refusal slugs, R1/R2/R11 fields, field absence and deterministic answers. Verify the selected turn fields on the bounded session projection. On both revisions, check the native, projection and receipt discriminators and semantic equality of text and structured representations. |
| <a id="a5-t5"></a>**A5-T5** · [State and interruption](#53-recovering-an-interrupted-call) | For every §5.3 tool, recover a lost or oversized reply on another replica using only the original key. Race identical arguments under the same and distinct keys. Check session IDs, ordinals, idempotent close, local/enrolled search, effects, calls and charges. Disable new work, discover the recovery-only form and use it. Separately exhaust a resource budget or change enrollment before recovery. Require zero new charges. Test changed arguments, missing admission, token renewal, narrowed authority and tenant/uid isolation, including conflict disclosure. Pause a retry before its key claim, expire and prune the key, then resume and verify zero new execution. |
| <a id="a2-t1"></a>**A2-T1** · [Generation opt-in](#a2) | With a recording provider and one scope, omit the opt-in, owner and each allowance in turn, then set each allowance to zero. Count zero physical calls for expansion (including `complete: false`), verification and final answers. Exercise authorized generation for a model without pricing. A separate enrolled search charges embedding and reranking under I2. |
| <a id="a8-t1"></a>**A8-T1** · [Request and daily limits](#a8) | Exhaust the request allowance across expansion, verification, answer generation and a physical retry while daily capacity remains. Separately exhaust daily capacity across several requests with request capacity remaining. Race two replicas for the last daily reservation. Compare provider arrivals with admitted attempts, including a denied retry, and confirm that an unbounded request makes zero calls. |
| <a id="a8-t3"></a>**A8-T3** · [Policy inheritance](#a8) | Exercise the recommended policy with two pins in one collection. One inherits both allowances and the other overrides both. Verify both effective request ceilings, reject an incomplete override, exhaust the shared collection day, then switch pins. A new pin with index capacity still refuses. Separately exhaust only the index-specific daily balance and confirm the collection reservation is rolled back. |
| <a id="a9-t1"></a>**A9-T1** · [Shared request ceiling](#a9) | Run the shared-call fixture in the governing decision with the named contributors, a standalone index and an unrelated higher-limit index. Check dispatch on admission, refusal above the effective request ceiling and exhaustion across stage changes and retries. Compare the provider’s physical attempts and the payer’s charge, including after a duplicate settlement receipt. |
| <a id="a9-t2"></a>**A9-T2** · [Shared authority and daily limits](#a9) | With sufficient request capacity, leave one contributor’s daily balance below the attempt bound. Verify refusal, zero provider arrivals and no partial debit. Repeat with an unknown contributor and without a commonly authorized payer. Race two replicas for the remaining capacity. Verify that all daily reservations commit together, repeated scopes are counted once and reports disclose only authorized usage. |
| <a id="a8-t2"></a>**A8-T2** · [Accounting recovery](#a8) | Interrupt after reservation and after submission, omit or truncate provider usage, and reconcile from a second replica. With a valid key, verify that recovery reports outstanding capacity separately from settled spend and creates zero new reservations. Check that the accounting store retains the reservation against its original day across UTC rollover and key expiry. Settle a duplicate receipt only once. Inject above-estimate usage and check the full charge, recorded and metered excess, and refusal after the remaining balance is exhausted. |
| <a id="a6-t3"></a>**A6-T3** · [Operational evidence](#a6) | Verify persisted MCP admissions and outcomes through the report filter. Saturate the v1.4 best-effort interaction writer separately and verify its native loss signals. Demonstrate that heavy agent load leaves the control action available. Bound session reads and test resource-level refusal after durable admission. |

Removing the outer scope check, dispatching before a durable commit is confirmed, skipping the paid-stage check, dropping filter-before-pagination or counting an inner dispatch as a second invocation must each fail a named test. Record exact artifact versions, checkout, configuration, observed outcomes and pending check owners in the release evidence. [Server validation ladder](../guides/validation.md).

## 9. Interaction with the planned Server work

### 9.1 Dependencies

| Item | MCP dependency |
|---|---|
| R7, R11 | Require bounded admission and typed retry guidance before enabling the profile. Retrying a stateful operation also requires a known outcome or an explicit idempotency contract. |
| R1, R2, IR-0 to IR-4 | Require truthful stage disclosure before opening endpoint-backed collections to agents. Enable multi-collection turns only when required-member failure and completeness are distinguishable. |
| R3, R4 to R6, R8 to R10 | PDF parsing and job orchestration stay outside the tool table. Honor native refusals when a verified artifact becomes unavailable. |
| R12 and later IR work | Share touched validation constraints with input schemas. Preserve citation binding if optional packing changes the size of a response. |
| U1 to U14 | Surface agent activity and revocation through the normal operator views. |

### 9.2 Sequencing

1. Carry A1, A2, the A4 read-only grant, the proposed interruption contract in §5.3 and the A6/A8/A9 admission and accounting rules into the shared contract decision process. Specify the trusted collection/index policy representation and how Server identifies the contributors to each shared call. Pin both A3 wire schemas and freeze v1.4 native fixtures.
2. Build an off-by-default route with discovery, filtered listing and a deterministic local search. Prove token scope, refusal semantics and receipt attribution end to end.
3. Implement both evidence readers and their shared native read-only authority. Complete the nine query tools, including the budgeted and stateful paths under their respective policies.
4. Collect the [A5 qualification evidence](#a5) and both real-client transcripts on the exact candidate. Record mixed-version native operation and operator disablement in the release guide.

### 9.3 Admin console

Show the `mcp` scope and effective revocation setting on tokens. Distinguish immediate enforcement from access continuing until token expiry. Show traffic by plane and tool, load shedding, durable audit admission refusals and invocations with unknown outcomes. Report native interaction queue loss separately. Use U9's visible escaping for operator views of untrusted text. Ensure `rest` and `grpc` reports still work before any MCP calls have occurred. The console's management actions continue to use their native roles.

## 10. Connecting and qualifying clients

### 10.1 Common setup

<a id="a7"></a>

**A7 · decided.** The deployment operates the broker, and the project supplies a documented integration example. The broker authenticates the user through the deployment’s identity provider and derives tenant, uid, clearance, compartments, scopes and permitted runbooks from trusted policy. It calls `POST /v1/access-tokens` with its tenant’s static management credential and asserted broker uid. Give the client the HTTPS MCP URL, its own issued bearer and expiry. Keep the management credential with the broker. [Token issuance](../../src/munarium-server/src/tokens_api.rs) · [Server security posture](../security-posture.md).

For example, a broker that has authenticated a reader and authorized the `handbook` runbook could send this v1.5 issuance body. Omitting `ttl_secs` uses the deployment’s configured lifetime. The `mcp` grant requires a Server that implements A1.

```json
{
  "uid": "example-reader",
  "access_level": 1,
  "compartments": [],
  "scopes": ["mcp", "query"],
  "runbook_refs": ["handbook"]
}
```

**Lifetime precedence.** The REST issuer uses the trusted request's `ttl_secs` when supplied, then `MUNARIUM_TOKEN_TTL_SECS` when configured, otherwise 3,600. Both values use unsigned whole seconds. Issuance clamps the selected integer to 1–86,400 seconds. An unparseable configuration value fails configuration loading. The existing transports differ for zero. Explicit REST `ttl_secs: 0` produces a one-second token, while the typed gRPC issuer treats zero as an omitted value and uses the configured default. Preserve both v1.4 behaviors. A broker that needs a per-request override can specify a positive duration. [Server configuration](../../src/munarium-server/src/config.rs) · [Typed gRPC issuer](../../src/munarium-server/src/grpc_platform.rs) · [JWT issuance and verification](../../src/munarium-access/src/lib.rs)

The lifetime governs newly minted capability JWTs. Each receives a signed `iat`, `exp` and distinct `jti`. A configuration change does not alter a previously issued token, the lifetime of a static role credential, or a console session’s idle policy. Verification preserves v1.4’s 30-second clock-skew allowance. On expiry, the client obtains a fresh grant from the broker and reconnects or replaces its bearer. The broker rechecks current user authority. Use the existing revocation policy for an earlier withdrawal. Token replacement alone cannot resolve the outcome of an interrupted tool invocation.

### 10.2 Editors and coding agents

A concrete candidate configuration for a Codex CLI version supporting Streamable HTTP reads the token from the named environment variable. Qualify the recorded CLI version before listing it as supported. [Codex MCP configuration](https://developers.openai.com/codex/mcp).

```toml
[mcp_servers.munarium]
url = "https://munarium.example.com/mcp"
bearer_token_env_var = "MUNARIUM_TOKEN"
```

| Client candidate | Configuration to exercise | What the run must establish |
|---|---|---|
| Claude Code | HTTP connection with headers or a deployment-controlled `headersHelper` | Confirm renewal through the deployed broker and how the client handles a refusal after renewal. [Claude Code MCP](https://code.claude.com/docs/en/mcp) |
| Cursor | A URL and `Authorization` header in `.cursor/mcp.json`, with `${env:MUNARIUM_TOKEN}` referencing the caller's environment | Confirm scope-specific listing and whether replacing the environment token requires a reload. [Cursor MCP configuration](https://cursor.com/docs/mcp) |
| VS Code | An HTTP connection with a password input in a version that supports editor-managed configuration | Confirm that the workspace file contains no bearer value and record how the user replaces the input after expiry. [VS Code MCP configuration](https://code.visualstudio.com/docs/agents/reference/mcp-configuration) |
| Gemini CLI | A Streamable HTTP connection under `httpUrl` with request headers | Verify the credential source and the action on 401. [Gemini CLI MCP servers](https://geminicli.com/docs/tools/mcp-server/) |
| Claude Desktop, Windsurf/Devin | Configure the remote endpoint through the tested client's settings | Determine the available credential mechanism first. Qualify an OAuth-only client against the deployment's gateway profile. [Claude remote connectors](https://support.claude.com/en/articles/11175166-get-started-with-custom-connectors-using-remote-mcp) |

### 10.3 Stdio-only clients

A separately reviewed and pinned local relay can translate stdio to HTTPS. For a candidate such as `mcp-remote`, qualify its HTTP-only mode and credential-file handling, and keep live token values out of process arguments. Include the relay version and credential handling in the test record. A relay's successful connection qualifies only that client–relay–Server combination. [Relay documentation](https://github.com/punkpeye/mcp-remote).

### 10.4 Hosted connectors and SDK applications

A hosted connector moves the MCP request and result through the vendor's infrastructure. Qualify one only where the gateway exposure, corpus and spend authority allow it. A local SDK client can exercise the same tool table against a loopback test service with fictional data. For multi-user applications acquire and renew the token for the active user. Applications handling tool results as model input should preserve user approval choices for access and state changes.

For a Claude Messages connector, exercise the server's `authorization_token` field. For an OpenAI Responses connector, exercise `server_url`, `authorization` and a restricted `allowed_tools` list. Record the observed protocol revision and interaction on expiry. An advertised model API option alone does not establish that the connector will work against bare Server. [Claude MCP connector](https://platform.claude.com/docs/en/agents-and-tools/mcp-connector) · [Responses MCP servers](https://developers.openai.com/api/docs/guides/tools-connectors-mcp).

### 10.5 Interoperability record

| Gate | Evidence to attach |
|---|---|
| <a id="a3-t2"></a>**A3-T2** · [Modern client](#a3) · `2026-07-28` | Connect a real versioned client to the candidate. Run authenticated `server/discover`, verify the advertised versions and tools capability, then run `tools/list` and `tools/call` and check the expected result on a synthetic collection. |
| <a id="a3-t3"></a>**A3-T3** · [Legacy client](#a3) · `2025-11-25` | Connect a real versioned client to the same candidate. Verify the revision returned by `initialize`, send `notifications/initialized`, then run `tools/list` and `tools/call` under the negotiated revision and check the same expected result. A client that supports both revisions may supply both runs. |
| Per tested client | Record the Server artifact, client name and version, observed wire revision, transport and authentication mode. Attach redacted transcripts for the successful sequence, a denied tool call and token expiry/renewal. Retain an operation key across a lost reply and demonstrate lookup by that key. Record the operator-visible error and result size. Limit published client support to the exact combinations exercised. |
| Other surveyed clients | Explicitly mark untested, incompatible or blocked and give the reason. Document vendor-specific setup only after reproducing it on the named version. |

## 11. Deferred

Streaming turns, tasks, prompts, resources, subscriptions, ingestion, a Server-owned OAuth issuer and governed memory reads and writes are outside the v1.5 MCP catalog. Existing native routes retain their v1.4 contracts. MCP qualification can exercise local query tools independently of Lens. The Server release also depends on Lens qualification. [REL1 release gate](README.md#rel1)

### 11.1 Governed memory tools

The append-only ledger accepts proposed claims, records conflicting claims as disputed and retains gate findings. A future MCP profile could expose the following reads and gated writes. The v1.4 ledger uses native static credentials. A static `rw` token can write to any lineage in its tenant. A capability-based profile would need a shared contract defining which lineages the caller can access under MCP-RULE-2.

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

- Define separate read and write capabilities (working names `memory.read` and `memory.write`) and a token claim listing permitted version IDs, analogous to `rb`. An absent list grants access to no lineage.
- Decide how native ledger handlers accept these capabilities. The route authentication change needs a shared decision record and client conformance review.
- Require `idempotency_key` and `expected_head` on writes. A new JSON-RPC ID on a retry must not duplicate a claim or promise.
- Keep version creation, raw event append, anchor locks, counters, digests and governance-profile transitions with the lineage-owning application.

## 12. Decision and test index

The decision links lead to the governing paragraphs. The test IDs identify the candidate evidence for §8 and §10.5. The roadmap tracks open values, and A8 identifies its policy-inheritance recommendation separately from the decided token limits. [Open items](README.md#open-items)

| Governing decision | Qualification tests |
|---|---|
| [A1 · Entry and tool authority](#a1) | [A1-T1](#a1-t1) |
| [A2 · Generative spending authority](#a2) | [A2-T1](#a2-t1) |
| [A3 · Protocol revisions and real clients](#a3) | [A3-T1](#a3-t1), [A3-T2](#a3-t2), [A3-T3](#a3-t3) |
| [A4 · Read-only evidence capability](#a4) | [A4-T1](#a4-t1) |
| [A5 · Complete Server release gate](#a5) | [A5-T1](#a5-t1), [A5-T2](#a5-t2), [A5-T3](#a5-t3), [A5-T4](#a5-t4), [A5-T5](#a5-t5) |
| [A6 · Durable audit admission and recovery](#a6) | [A6-T1](#a6-t1), [A6-T2](#a6-t2), [A6-T3](#a6-t3) |
| [A7 · Deployment broker and issued-token lifetime](#a7) | [A7-T1](#a7-t1) |
| [A8 · Token allowances and inheritance recommendation](#a8) | [A8-T1](#a8-t1), [A8-T2](#a8-t2), [A8-T3](#a8-t3) |
| [A9 · Token accounting for shared generation](#a9) | [A9-T1](#a9-t1), [A9-T2](#a9-t2) |

## 13. Evidence limits

At the 19 November 2026 design review, the published modern discovery and Streamable HTTP references supported the discovery shape, header/body validation and error mappings in §4.2. The legacy lifecycle reference supported the initialization sequence. These source checks cover the protocol text. Each client/version claim still requires the real-client evidence in §10.5. [Modern discovery](https://modelcontextprotocol.io/specification/2026-07-28/server/discover) · [Modern HTTP binding](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http) · [Legacy lifecycle](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle)

A1–A9 record the maintainer’s design decisions. The endpoint, protocol fixtures, rollback runs and real-client transcripts still require implementation and qualification on a named Server candidate. Collect router construction costs, safe transcript bounds and multi-replica budget measurements with their owning tests. Keep measured values and dated evidence in the roadmap register. [Open items](README.md#open-items) · [Source baselines](README.md#source-baselines)
