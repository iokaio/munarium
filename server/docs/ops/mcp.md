# Operating the MCP endpoint

**ClearView (Server 1.5).** Server can serve an opt-in
[Model Context Protocol](https://modelcontextprotocol.io/) endpoint, `POST /mcp`,
for agents acting on behalf of one user. It is **off by default**: with
`MUNARIUM_MCP` unset there is no `/mcp` route, no background task, no outbound
call and no read or write of the MCP tables. This guide covers enabling it,
its authority model, the generation policy, interrupted-call recovery, the
console view and rollback. The design and its decisions A1–A9 are in
[MCP analysis](../design/mcp-analysis.md); the shared semantics are in
hub decision records 0025, 0026 and 0027, accepted on 20 November 2026.

## What the endpoint serves

`POST /mcp` on the existing REST listener (no new port), JSON-RPC 2.0 over
Streamable HTTP with unary `application/json` responses only. It speaks two
protocol revisions (A3):

| Revision | How a client uses it |
|---|---|
| `2026-07-28` | Stateless. Every request carries `params._meta` with `io.modelcontextprotocol/protocolVersion` and `io.modelcontextprotocol/clientCapabilities`, and the headers `MCP-Protocol-Version`, `Mcp-Method` and, for `tools/call`, `Mcp-Name` (the `=?base64?…?=` form is decoded before comparison). Methods: `server/discover`, `tools/list`, `tools/call`, `ping`. A client may start with a tool call. |
| `2025-11-25` | `initialize` (answered with `protocolVersion: "2025-11-25"`, the `tools` capability and `serverInfo`), `notifications/initialized`, `ping`, then `tools/list` and `tools/call` with `MCP-Protocol-Version: 2025-11-25`. No MCP session id is issued. |

Transport rules: one JSON-RPC message per request (batches are refused with
400/`-32600`); notifications get 202 with an empty body; `GET` and `DELETE`
get 405; a request body over `MUNARIUM_MCP_INPUT_MAX_BYTES` gets 413; a body
that is not `application/json` gets 415 and an `Accept` that does not allow
JSON gets 406. A header that disagrees with the body is 400/`-32020`; an
unsupported version is 400/`-32022` with `data.supported`; an unknown method
is 404/`-32601` (including `subscriptions/listen`, resources, prompts and
tasks, which this profile does not offer); an unknown tool name is `-32602`.
A request with modern metadata is never served by the legacy path. Every MCP
response carries `Cache-Control: private, no-store`, because listings depend
on the caller's scopes. Only the `tools` capability is advertised, without
list-change or subscription support.

The endpoint is not part of the `/v1` OpenAPI contract, like `/admin`.

## Enabling it

1. Run a build with the endpoint on **every** replica that will receive
   `/mcp` traffic, with `MUNARIUM_MCP` unset. Migration `0070_mcp_endpoint`
   applies at startup; it only adds tables.
2. Configure the settings below identically on every replica, then set
   `MUNARIUM_MCP=enabled` and restart the replicas that should serve it.
3. Only then let the deployment's broker issue tokens carrying `mcp`
   ([tokens](#authority-and-tokens)).
4. Publish `/mcp` through the gateway (TLS terminates there). The gateway may
   expose just this path on a separate hostname.

Startup refuses an enabled endpoint without authentication
(`MUNARIUM_AUTH_MODE=disabled`), without `MUNARIUM_TOKEN_SECRET`, or under the
`platform-v1` authority profile, with an actionable `config error`. On the
memory store the endpoint starts, discovery and listing work, and **every tool
call is refused** with `mcp-audit-unavailable`, because there is no durable
audit store (A6).

| Setting | Default | Meaning |
|---|---|---|
| `MUNARIUM_MCP` | `disabled` | `enabled` registers `POST /mcp`. |
| `MUNARIUM_MCP_ALLOWED_ORIGINS` | empty | Comma-separated exact origins (`https://agent.example.com`). A request carrying any other `Origin` gets 403. Every `Origin` header is checked, and one whose value is not visible ASCII counts as present and refused, never as absent. Server-side agents send no `Origin` and are unaffected. With the list empty, every request that carries an `Origin` is refused. |
| `MUNARIUM_MCP_MAX_CONCURRENCY` | `0` = half the REST ordinary pool, at least 1 | MCP requests in flight per replica. Each MCP request also takes one ordinary REST slot (R7) before its body is read, so MCP can never exceed the REST ceiling or touch the control reserve. **Provisional**: measure against your workload before relying on it. |
| `MUNARIUM_MCP_PAID_TOOLS` | `disabled` | `enabled` lists the execution forms of the generating tools (`ask_collections`, `ask_session`). Generation still needs the policy below (A2). |
| `MUNARIUM_MCP_EXECUTION` | `enabled` | `recovery-only` lists only the recovery form of the five keyed tools, so no new keyed work starts; the six bounded reads stay. Use it to drain. |
| `MUNARIUM_MCP_GENERATION_POLICY` | unset | Path to the trusted generation policy file (A2/A8/A9). Unset: no collection may generate over MCP. |
| `MUNARIUM_MCP_RESULT_MAX_BYTES` | `262144` | Bound on the whole serialized tool response. |
| `MUNARIUM_MCP_INPUT_MAX_BYTES` | `1048576` | Bound on the request body. |
| `MUNARIUM_MCP_AUDIT_DEADLINE_MS` | `2000` | How long a tool call waits for its durable admission commit to be acknowledged before refusing. |
| `MUNARIUM_MCP_CALL_DEADLINE_SECS` | `300` | Bound on one admitted tool's execution. A keyed outcome still missing after it reads `unknown`. |
| `MUNARIUM_MCP_KEY_MAX_VALIDITY_SECS` | `86400` | Longest operation-key lifetime accepted, judged on the database clock. Must exceed the call deadline. |
| `MUNARIUM_TOKEN_TTL_SECS` (existing) | `3600` | Default lifetime of newly issued capability tokens (A7). Unchanged. |

Every value is validated at startup whether or not the endpoint is enabled.

## Authority and tokens

**A1.** Entry needs a valid, unrevoked, tenant-bound capability JWT carrying
the `mcp` scope. Static role tokens are refused even with full native
authority. A missing, invalid or expired token gets 401 with
`WWW-Authenticate: Bearer`; a token without `mcp` gets 403 `scope-missing`. An
`X-Munarium-Uid` header, when sent, must equal the token subject.

`mcp` admits only discovery and a scope-filtered `tools/list`. Every tool also
needs its operation scope, on the listing and on a direct call:

| Scopes | Listed and callable |
|---|---|
| `mcp` only | nothing (discovery works, the list is empty) |
| `mcp`, `query` | the nine query and session tools |
| `mcp`, `evidence_read` | `get_evidence`, `read_evidence_rows` |
| `query` only | no MCP entry; native routes as before |

The native handler then applies its own tenant, clearance, compartment,
resource and runbook checks with the caller's own bearer. `mcp` grants nothing
on any native route, and enabling the endpoint widens no existing token.

**A4, the read-only evidence capability.** The scope value is
`evidence_read` (a new, distinct value; open choice in hub record 0025,
chosen here). It opens `GET /v1/evidence/{id}` and `GET /v1/evidence/{id}/rows`
natively and the two MCP readers with `mcp`. It cannot seal, upload or commit.
A v1.4 `evidence` token keeps its native read and sealing permissions but does
**not** open the MCP readers: only the read-only grant does. Issue it through
the existing `POST /v1/access-tokens` (or the typed gRPC issuer) like any other
scope. Every resolution is audited in the evidence access log as before.

**A7, the broker.** The deployment operates the broker: it authenticates the
user, derives tenant, uid, clearance, compartments, scopes and permitted
runbooks from its own policy, and calls `POST /v1/access-tokens` with the
tenant's static management credential. For example:

```json
{
  "uid": "example-reader",
  "access_level": 1,
  "compartments": [],
  "scopes": ["mcp", "query"],
  "runbook_refs": ["handbook"]
}
```

Omit `ttl_secs` to use `MUNARIUM_TOKEN_TTL_SECS`. The client receives the MCP
URL, its own bearer and expiry; the management credential stays with the
broker. On expiry the client renews through the broker and reconnects;
renewal never replays an interrupted tool call.

**Revocation.** Revoke a token in the console (`/admin/tokens` or
`/admin/mcp`, review then typed confirmation) or through
`POST /v1/access-tokens/{jti}/revoke`. It stops the next MCP request **only**
with `MUNARIUM_TOKEN_REVOCATION_CHECK=true`; otherwise the token works until
it expires. The MCP console page states which applies.

## Tools

| Tool | Scope | Native operation | Notes |
|---|---|---|---|
| `list_collections` | `query` | dedicated read | Active collections the caller's clearance and compartments permit, filtered before ordering, counting and paging. `limit` ≤ 100, `cursor` bound to the caller's authority. |
| `list_runbooks` | `query` | dedicated read | Latest active version of each runbook in the token's allowlist with at least one visible collection; only visible collection names; no YAML. |
| `search_collection` | `query` | `POST /v1.2/search` | Keyed. |
| `ask_collections` | `query` | `POST /v1.2/query` | Keyed, generating. |
| `open_session` | `query` | `POST /v1/runbooks/{name}/sessions` | Keyed. |
| `ask_session` | `query` | `POST /v1/sessions/{id}/turns` | Keyed, generating. Sends `collection_availability: "require"` (strict availability). |
| `get_session` | `query` | dedicated bounded read of `GET /v1/sessions/{id}` | One turn by `ordinal`, or up to 10 turns after `after_ordinal`; native field values. |
| `close_session` | `query` | `POST /v1/sessions/{id}/close` | Keyed. Closing a closed session reports its state. |
| `authorize_publication` | `query` | `GET /v1.2/collections/{id}/publications/{publication_id}` | No download grant. |
| `get_evidence` | `evidence_read` | `GET /v1/evidence/{id}` | |
| `read_evidence_rows` | `evidence_read` | `GET /v1/evidence/{id}/rows` | `limit` ≤ 200. |

Before an MCP turn, transcript read or close, the endpoint checks that the
session belongs to the caller's uid, that the token's runbook allowlist still
names its runbook, and that the token's clearance and compartments still
cover the clearance the session was opened with. This is stricter than the
native routes, by design: a narrower same-user token cannot reach a session
opened with wider authority.

Nothing else is reachable: index lifecycle, collections, shapes, runbook
authoring and runs, ingestion, providers, budgets, tokens, evidence writes,
governed memory, reports, recovery and platform operations stay native-only
(design §6). The table is compiled; a unit test checks it against a separate
route allowlist.

**Results.** A fresh native result is returned unchanged in
`structuredContent`, and the native response body's own bytes are the text
block, so both representations agree exactly. `_meta["io.munarium/result"]`
says `native`, `projection` (the dedicated reads), `receipt` or `problem`. A
refusal is a tool result with `isError: true` whose structured content is the
native problem (type URI, status, R11 `retryable`/`retry_action` when present)
and whose `_meta["io.munarium/problem"]` is the slug. A result larger than
`MUNARIUM_MCP_RESULT_MAX_BYTES` becomes a `result-too-large` tool error that
names a smaller limit or cursor for a read, or, for a keyed operation that ran,
the recovery read.

## Durable admission (A6)

Before any tool handler runs (all eleven, including both listings), one
PostgreSQL transaction commits a row in `mcp_invocations`: a server-assigned
invocation id, the tool, tenant, uid, token id, request id, protocol revision,
form and time — never credentials, arguments, retrieved text or evidence. For a
keyed execution the same transaction claims the operation key. The call waits
for the commit acknowledgment for at most `MUNARIUM_MCP_AUDIT_DEADLINE_MS`. If
the store is unavailable, the deadline passes or the acknowledgment is lost,
the call is refused with `mcp-audit-unavailable` (`executed: false` in the
problem) and **nothing runs**: no read, mutation, reservation or provider call.
After a possibly committed but unacknowledged claim, the server marks the key
`withheld` when the store returns, so its receipt says the work never ran.

Discovery, `initialize` and `tools/list` are protocol operations and execute no
tool. Each admitted call also writes one content-free interaction row on the
`mcp` plane (`method = "tools/call <tool>"`), best effort like native capture;
the durable record is `mcp_invocations`. The native `rest` and `grpc` capture
is unchanged, and the in-process dispatch behind a tool is neither admitted nor
recorded a second time.

## Generation: opt-in and allowances (A2, A8, A9)

No MCP tool call reaches a model unless **all** of these hold:

1. `MUNARIUM_MCP_PAID_TOOLS=enabled`;
2. every contributing collection has an entry in the policy file that names a
   `budget_owner` and both `request_tokens` and `daily_tokens`;
3. every contributor names the **same** budget owner (the payer);
4. the reservation for the next physical attempt fits.

This applies to local and remote models alike, with or without price data.
Embedding and reranking on enrolled collections follow the native inference
budgets, not this policy.

The policy file (`MUNARIUM_MCP_GENERATION_POLICY`) is trusted deployment
configuration, read at startup; tool arguments cannot select a policy or payer.
Deploy the same file to every replica. Helm's `mcp.generationPolicy` renders
and mounts one ConfigMap on both serving and builder pods. The policy checksum
rolls both deployments when the policy changes. Each live replica reports the
SHA-256 of the bytes it loaded in `GET /v1/reports/scale`. The report and
console Health page warn when digests differ. All replicas sharing the fleet
must use the same PostgreSQL database and `MUNARIUM_DEPLOYMENT_ENVIRONMENT_ID`.
During a mixed policy rollout,
generation waits for agreement across the observed fleet and a current
heartbeat from the answering replica. Allow up to 45 seconds for a stopped
replica's heartbeat to expire.

```json
{
  "version": 1,
  "model_bytes_per_token": {"example-completion-model": 3},
  "collections": [
    {"tenant": "acme", "collection": "legal", "budget_owner": "shared-research",
     "request_tokens": 2000, "daily_tokens": 200000,
     "indexes": [{"index_version": "iv-legal-2026", "request_tokens": 3000, "daily_tokens": 50000}]},
    {"tenant": "acme", "collection": "finance", "budget_owner": "shared-research",
     "request_tokens": 8000, "daily_tokens": 400000}
  ]
}
```

`collection` matches a collection id or name. An index override must give both
allowances, or startup fails (the A8 inheritance recommendation, implemented
here). Without an override the pinned index inherits the collection's pair.

**Contributors.** Server identifies a call's contributors before dispatch (an
open choice in hub record 0026, chosen here): for `ask_collections`, the
authorized collections named by the call; for `ask_session`, the authorized
collections of the session's pinned runbook at the session's clearance; each
with its active index at that moment. A merely available collection never
contributes.

**Accounting.** Every physical submission — expansion, verification, the
answer, a re-ask and each HTTP retry inside a provider adapter — is reserved
before it is sent, in one PostgreSQL transaction, against:

- the call's **request allowance**: the largest effective per-request
  allowance among its contributors (they never add), shared by every stage and
  retry of the call; and
- every applicable **daily allowance**: each contributing collection's, and
  each contributing index's own when it overrides, for the database's UTC day.

The reservation estimates input tokens as serialized request and schema bytes
divided by the resolved model's positive integer `model_bytes_per_token`
setting, rounded up. The default is 3 bytes per token. Server adds 64 tokens
of framing and the permitted output. A refusal
reserves nothing anywhere and is a tool error: `mcp-generation-not-authorized`
(no opt-in, owner, allowance or common payer), `mcp-allowance-exhausted`, or
`mcp-accounting-unavailable`. Each attempt is settled once from the provider's
reported input and output counts; missing or unreliable usage keeps the full
reservation against its original day. An attempt the provider answered with
an error, whose transport failed, or that a retry replaced may still have
run: it is settled `uncertain` at its full bound when the model call ends, so
no attempt is left `reserved` and nothing is charged twice. Reliable usage
above the estimate is charged in full and recorded atomically in
`mcp_bound_violations`. The excess increments
`munarium_mcp_generation_excess_tokens_total`. Later attempts use the reduced
request and daily balances. An attempt can overshoot a request allowance by
its own estimate error. If usage is unavailable, the reservation remains the
recorded charge.

The native token budgets (`dailyTokens`, `dailyTotalTokens`) still apply to
the same calls, unchanged.

## Interrupted calls and operation keys (§5.3)

The five keyed tools (`search_collection`, `ask_collections`, `open_session`,
`ask_session`, `close_session`) take an `operation_key` the client chooses and
keeps **before** sending (the shape is an open choice in hub record 0027,
chosen here):

```json
{"operation_key": {"id": "k7Q2v9…", "expires_at": "2026-11-21T09:00:00Z"}}
```

`id` is 16–128 characters of `A-Z a-z 0-9 - _` from a random source;
`expires_at` is an immutable RFC 3339 UTC time ending in `Z`. On the database
clock, at claim time, the expiry must be in the future, within
`MUNARIUM_MCP_KEY_MAX_VALIDITY_SECS`, and at least
`MUNARIUM_MCP_CALL_DEADLINE_SECS` away; otherwise `mcp-operation-key-invalid`
and nothing runs. The key is bound to the tenant, uid and tool, so it survives
token renewal. The expiry is part of the key: presenting a known key id with a
different `expires_at` is `mcp-operation-key-invalid`, never a receipt or a
new claim.

- **Execution**: the key plus the tool's arguments. The first admission claims
  the key and stores a digest of the canonical arguments; only it dispatches.
  A repeat with the same key and arguments gets the receipt, never a second
  execution; different arguments get `mcp-operation-conflict`. Different keys
  are different calls even with identical arguments.
- **Recovery**: the key alone. It reads a bounded receipt and never starts the
  operation, even with new work disabled or a budget exhausted, and makes no
  model call or reservation.

Every duplicate and recovery request is admitted durably on its own (a fresh
invocation id linked to the original) and rechecks the current credential's
authority over the original resources and retention before revealing anything.
An absent key and one the caller may not see return the same
`{"kind": "operation_receipt", …, "status": "not_found"}`.

A receipt holds identifiers, status and spend only: `status` is `pending`,
`completed`, `refused`, `not_executed` or `unknown` (pending past the work
deadline); `outcome` carries the HTTP status, problem type, session id, turn
ordinal and result size, with `content: "not_retained"`; the generating tools
add `spend` (`attempts`, `settled_tokens`, `outstanding_reserved_tokens`).
Read a recovered turn with `get_session` and its ordinal. A client must never
extend a key or mint a new one automatically after an interruption.

Keys are deleted a day after their expiry by a janitor that runs only while
the endpoint is enabled. In the same statement it records each deleted key's
minimal identity (tenant, uid, tool, key id and expiry) in
`mcp_operation_tombstones`, and an execution presenting a tombstoned key id is
`mcp-operation-key-invalid` whatever expiry it now carries, so pruning never
reopens a key. Tombstones have no server deadline: they are kept until an
operator deletes them, and a key id whose tombstone is deleted could be
claimed again, so delete only tombstones whose keys no client can still hold.
Audit rows and reservations are never deleted by the janitor either.

Known limits: the session or turn link is recorded immediately after the
native effect commits, not in the same transaction, so a crash in that window
leaves the receipt `unknown` (never replayed). An enrolled search's embedding
and reranking receipts are not yet correlated (`enrolled_inference:
"not_correlated"`).

## Console

`/admin/mcp` (access and audit group) shows, for the signed-in tenant: whether
this replica serves MCP and with which forms; tool calls by tool over 24 hours
with completed, refused and unknown counts; keyed operations whose outcome is
unknown; the newest invocations with their linkage; and the unrevoked tokens
carrying `mcp`, each with the standard revocation review. It adds no action of
its own, and every count on it is the tenant's own. This replica's
audit-admission refusals, MCP load shedding and native interaction-record
drops are process counters across every tenant, so they are on the
replica-scoped `/admin/health` page, labelled as such. The traffic and usage
pages accept the `mcp` plane.

## Metrics

| Metric | Labels |
|---|---|
| `munarium_mcp_requests_total` | `method` (bounded), `outcome` |
| `munarium_mcp_tool_calls_total` | `tool`, `outcome` |
| `munarium_mcp_admissions_total` | `tool`, `form` |
| `munarium_mcp_audit_refusals_total` | `reason`: `no-durable-store`, `store-error`, `deadline`, `lost-ack` |
| `munarium_mcp_dispatches_total` | `tool` |

MCP requests are also counted in `munarium_admission_total{plane="rest"}` and
shed with 503 `overloaded` like REST.

## Disabling and rollback

1. Set `MUNARIUM_MCP_EXECUTION=recovery-only` to stop new keyed work while
   clients recover outstanding keys.
2. Unset `MUNARIUM_MCP` and restart: `/mcp` answers 404, native routes and
   reports are unchanged. Stop issuing `mcp` tokens; revoke outstanding ones
   if revocation checks are on.
3. Disabling MCP leaves the 0070 tables and the `mcp`-plane interaction rows
   in place. Returning to a v1.4.0 binary is by restoring the pre-upgrade
   backup ([rolling back to 1.4.0](../guides/server-1.5.md#rolling-back-to-140)), because v1.4.0 does
   not start against a database with the 1.5 migrations. That restore discards
   everything written since the upgrade, MCP records included.

## Qualification status

The automated suite (`mcp_tests.rs`) covers the default-off upgrade, A1, A3
fixtures, A4, A5-T1/T4/T5, A6, A2, A8 and A9 against an isolated PostgreSQL.
The real-client runs required by A3-T2 and A3-T3 are **pending**; their
procedure is in [MCP client qualification](mcp-client-qualification.md). The
endpoint is not qualified for release until those runs, the hub contract
exports and the A5 release record are complete.
