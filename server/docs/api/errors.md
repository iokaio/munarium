# Error registry

One kernel error surface (`munarium_core::KernelError`), mapped centrally to both planes.
REST: RFC 9457 `application/problem+json`, `type` = `https://munarium.ioka.io/problems/<slug>`.
gRPC: `tonic::Status` with structured details in `grpc-status-details-bin` — a
`google.rpc.Status` carrying one `google.rpc.ErrorInfo`:

- `reason` — the problem slug below (the one cross-transport error key),
- `domain` — `mmp.ioka.io`,
- `metadata` — the same extension member names as the REST problem+json:
  `expected`/`actual` on `head-conflict`; `gate_findings` (a JSON-encoded array of
  gate findings, identical shape to the REST member — plus `findings_total`, and
  `findings_truncated: "true"` when the list was capped to fit the HTTP/2 trailer
  size limit) on `policy-rejection`; `shape_ref` on `shape-violation`;
  `kind`/`id` on `not-found`.

Clients must key on the slug (REST `type`, gRPC `ErrorInfo.reason`) — never on
English message text.

## Retry guidance

**ClearView R11.** Some problems carry two optional members,
identical in REST problem+json and as gRPC `ErrorInfo.metadata` keys of the
same names (`"true"`/`"false"` and the action string):

| Slug | `retryable` | `retry_action` | Meaning |
|---|---|---|---|
| `overloaded` | `true` | `repeat` | Shed at admission before any handler ran. The identical request may be repeated after `Retry-After`. |
| `rate-limited`, `daily-cap-reached`, `run-locked`, `datastore-unavailable`, `inference-stage-unavailable`, `mcp-audit-unavailable`, `mcp-allowance-exhausted`, `mcp-accounting-unavailable` | `false` | `retry_later` | Transient, but the response does not prove the request had no effect. Repeat only a safe read or a request under a documented idempotency key; otherwise decide again later. Existing `Retry-After` headers are unchanged. |
| `head-conflict`, `inference-not-qualified` | `false` | `reread` | Read the current head (or the collection's inference state) and make a fresh decision. Never replay the stale request. |
| `command-unresolved` | `false` | `inspect_outcome` | The guarded command's outcome is unknown. Inspect it ([guarded command recovery](../ops/command-recovery.md)) before explicitly authorizing a new key. Never replay automatically. |

Every other slug carries neither member. An absent, unknown or future action
authorizes **no** automated replay, and `retryable` is never `true` without
`repeat`. A v1.4 client ignores both members; problems without guidance
serialize exactly as before. Session turns remain non-retryable in the
official clients regardless of these members.

## Problem types

Each slug's row carries a stable anchor, `#problem-<slug>`. The operator console
links every problem slug it shows to that anchor, and `docs_coverage` checks that
every slug the server emits has one.

| Slug | HTTP | gRPC code | Meaning / extensions |
|---|---|---|---|
| `head-conflict`<a id="problem-head-conflict"></a> | 409 | `ABORTED` | optimistic `expected_head` mismatch. Extensions: `expected`, `actual`. Normal and retryable: re-read, re-decide, retry. |
| `policy-rejection`<a id="problem-policy-rejection"></a> | 422 | `FAILED_PRECONDITION` | block-severity gate findings. The claim IS recorded (`disputed`) and the rejection is itself a ledger event. Extensions: `gate_findings` (rule_id, severity, message, detail), `policy_citation`. |
| `shape-violation`<a id="problem-shape-violation"></a> | 422 | `FAILED_PRECONDITION` | event body failed its shape's JSON Schema at the command gate. Extensions: `shape_ref`, `policy_citation`. Recorded as an event. |
| `idempotency-mismatch`<a id="problem-idempotency-mismatch"></a> | 422 | `INVALID_ARGUMENT` | same `Idempotency-Key`, different request hash. |
| `command-unresolved`<a id="problem-command-unresolved"></a> | 409 | `FAILED_PRECONDITION` | guarded command is executing or its outcome is unknown. Inspect the outcome before authorizing a new key; never automatically re-execute. See [guarded command recovery](../ops/command-recovery.md). |
| `source-denied`<a id="problem-source-denied"></a> | 410 | `FAILED_PRECONDITION` | source or bound scope is unavailable under durable tenant retention policy, including retained pins and public session copies. See [source retention](../ops/source-retention.md). |
| `overloaded`<a id="problem-overloaded"></a> | 503 | `RESOURCE_EXHAUSTED` | load shed: the instance is at `MUNARIUM_MAX_CONCURRENCY` in-flight /v1 (REST) or /mmp.v1 (gRPC) requests. REST responses carry `Retry-After: 1`. Health/meta routes never shed. Emitted since 2026-08-17. With the opt-in [admission classes](../ops/admission.md) it is also returned when a heavy request meets a full `MUNARIUM_HEAVY_CONCURRENCY`, or when a control request finds no ordinary slot and cannot use the reserve (the reserve is full or the caller lacks the route's role). |
| `session-not-open`<a id="problem-session-not-open"></a> | 409 | `FAILED_PRECONDITION` | a turn (or lifecycle action) against a session that is closed or expired; the detail names the actual state. Added 2026-08-17 with `POST /v1/sessions/{id}/close`. |
| `required-evidence-unavailable`<a id="problem-required-evidence-unavailable"></a> | 424 | `FAILED_PRECONDITION` | a REQUIRED layer of the turn's research profile produced no evidence, so the turn refuses rather than answering from an incomplete hierarchy. The detail names the **layer** and its refusal code, never the layer's sources — a caller who cannot see a source must not learn it exists from the shape of a refusal. With an explicit `collection_availability` policy it is also returned when the answer requires a member collection that could not be searched: any unavailable member under `require`, no completed member under `report`, or a required layer missing a member (refusal code `collection_unavailable`). A collection named in that detail is one the session can see; hidden collections are never searched or named. |
| `unknown-research-profile`<a id="problem-unknown-research-profile"></a> | 400 | `INVALID_ARGUMENT` | the turn named a `research_profile` the session's pinned runbook does not declare. Fails closed rather than falling back to the document path: silently answering a different question than the one asked is the worse failure. |
| `run-locked`<a id="problem-run-locked"></a> | 409 | `ABORTED` | this runbook run is already executing on another instance (the cross-instance advisory lock, 2026-08-17). Poll `GET /v1/runs/{run_id}` and retry when it settles. |
| `not-found`<a id="problem-not-found"></a> | 404 | `NOT_FOUND` | extensions: `kind`, `id`. |
| `invalid-input`<a id="problem-invalid-input"></a> | 400 | `INVALID_ARGUMENT` | malformed request outside schema concerns. |
| `unauthenticated`<a id="problem-unauthenticated"></a> | 401 | `UNAUTHENTICATED` | missing/invalid bearer token. |
| `forbidden`<a id="problem-forbidden"></a> | 403 | `PERMISSION_DENIED` | valid token, wrong tenant/role. |
| `rate-limited`<a id="problem-rate-limited"></a> | 429 | `RESOURCE_EXHAUSTED` | per-tenant limits or provider budget (rpm/tpm) exhausted — including an upstream provider 429 that survived the retry policy (2026-09-01; previously flattened into `provider-error`). REST responses carry `Retry-After: 60` (the rpm window's upper bound). |
| `daily-cap-reached`<a id="problem-daily-cap-reached"></a> | 429 | `RESOURCE_EXHAUSTED` | the (provider config × tier) daily token cap is exhausted (spending caps, 2026-09-01: `spec.budgets.dailyTokens`, UTC-day window, migration 0029). The detail names the config, tier, remaining/limit and the reset; REST responses carry `Retry-After` = seconds to midnight UTC. Recovery differs from `rate-limited`: wait for the window, or re-send on a cheaper tier. |
| `provider-error`<a id="problem-provider-error"></a> | 502 | `UNAVAILABLE` | upstream LLM failure, refused/malformed/unsupported completion, or session token exhaustion after its single bounded retry; extensions: `provider`, `endpoint_fingerprint` (never key material). Dispatched calls retain their accounted usage even when no answer is returned. |
| `storage-error`<a id="problem-storage-error"></a> | 500 | `INTERNAL` | backend failure. |
| `datastore-unavailable`<a id="problem-datastore-unavailable"></a> | 503 | `UNAVAILABLE` | a scope selected to serve from the datastore could not be served from its artifacts. There is deliberately **no fallback past selection**: a replica that agreed to serve a scope from the datastore leaves the traffic pool rather than silently answering from PostgreSQL, so the two engines can never be confused for each other. Recovery is an operator action, never a request-path one — `PUT /v1/retrieval-rollout` with `serving: postgres` (the rollback, never gated) or `mmctl datastore rollout set <collection|shape> <id> postgres` per scope for a whole corpus. |
| `inference-stage-unavailable`<a id="problem-inference-stage-unavailable"></a> | 503 | `UNAVAILABLE` | **ClearView I1.** A read of an inference-enrolled collection whose configured stage could not run under the `require` policy (the default). The detail is content-free: `stage=<embedding\|reranking> state=<disabled\|unconfigured\|probe_failed\|unavailable\|budget_exhausted> reason=<code> policy=require policy_revision=<n>`. No partial or fallback result is returned. Recovery is an operator action: restore the endpoint or budget, re-probe after a configuration change, explicitly select `prefer`, or roll the collection back. See [inference endpoints](../guides/inference-endpoints.md#outage-policy). |
| `inference-not-qualified`<a id="problem-inference-not-qualified"></a> | 409 | `FAILED_PRECONDITION` | **ClearView IR-1 to IR-4.** An inference management operation whose precondition is not met: inference disabled on the deployment, no `dailyTotalTokens` or `shadowDailyTokens` for what was asked, no current passing probe, an incomplete or mismatched semantic index, a change that requires rollback first, a state that does not permit the transition, or (SC2) a placement the configuration does not satisfy, with a `placement_*` reason. Also the refusal of the generic provider routes (`/v1/providers/{name}/embed`, `/complete`, `/health` and their `ProviderService` RPCs) for an `openai-compatible` configuration, which is reachable only through the governed inference routes; nothing is sent. The detail names the unmet condition. |

### platform identity/token slugs (emitted by the server layer, not the kernel)

| Slug | HTTP | gRPC code | Meaning |
|---|---|---|---|
| `uid-required`<a id="problem-uid-required"></a> | 400 | `INVALID_ARGUMENT` | `X-Munarium-Uid` header (REST) / `munarium-uid` metadata (gRPC) missing on a `/v1` / `mmp.v1` call while `MUNARIUM_REQUIRE_UID=true` (the default) **and** the bearer is not a capability JWT — when it is, the token's `sub` supplies the uid and the call proceeds. |
| `uid-mismatch`<a id="problem-uid-mismatch"></a> | 403 | `PERMISSION_DENIED` | the asserted uid differs from the capability token's `sub` claim. |
| `token-expired`<a id="problem-token-expired"></a> | 401 | `UNAUTHENTICATED` | the capability JWT's `exp` has passed (30 s leeway); request a new token from the management plane. |
| `token-revoked`<a id="problem-token-revoked"></a> | 401 | `UNAUTHENTICATED` | the capability JWT's `jti` is revoked (only when `MUNARIUM_TOKEN_REVOCATION_CHECK=true`). |
| `scope-missing`<a id="problem-scope-missing"></a> | 403 | `PERMISSION_DENIED` | the capability token lacks the scope the endpoint requires (`query` for sessions/turns, `ingest` for file ingestion, `findings` for filing findings, `evidence` for the evidence plane). |
| `override-not-allowed`<a id="problem-override-not-allowed"></a> | 403 | `PERMISSION_DENIED` | an API model override named a provider the runbook's `models.allowOverrides` policy does not permit. |
| `removal-not-confirmed`<a id="problem-removal-not-confirmed"></a> | 409 | `FAILED_PRECONDITION` | runbook removal is double-pass: no pending request, wrong `removal_id`, or the 15-minute confirmation window expired. |
| `authoring-draft-invalid`<a id="problem-authoring-draft-invalid"></a> | 409 | `FAILED_PRECONDITION` | an authoring draft cannot export/apply while error-severity findings exist (or it has no documents yet) — export and apply re-validate inline; the detail lists the finding codes. |
| `runbook-removed`<a id="problem-runbook-removed"></a> | 410 | `NOT_FOUND` | the exact name@version was soft-removed; its data is retained but the runbook is inaccessible. |

### Sealed evidence slugs

The structured-evidence plane. Note what the refusals deliberately do **not**
say: `evidence-forbidden` describes neither the artifact nor its class, because
learning "this exists and is above you" is itself a disclosure; and
`evidence-grant-invalid` covers four distinct causes under one slug so a caller
cannot probe for valid grant ids.

| Slug | HTTP | gRPC code | Meaning |
|---|---|---|---|
| `evidence-forbidden`<a id="problem-evidence-forbidden"></a> | 403 | `PERMISSION_DENIED` | the session does not **dominate** the artifact's authorization class (level, or a missing compartment). Also returned when a caller tries to SEAL above its own clearance — a principal may not mint evidence it could not itself read. |
| `evidence-expired`<a id="problem-evidence-expired"></a> | 410 | `NOT_FOUND` | retention purged the bytes. The metadata row survives precisely so this is distinguishable from a 404: the citation was real, and the retention policy is the honest reason it no longer resolves. |
| `evidence-not-committed`<a id="problem-evidence-not-committed"></a> | 409 | `FAILED_PRECONDITION` | the manifest is registered but the bytes were never committed. A pending artifact is not evidence yet. |
| `evidence-hash-mismatch`<a id="problem-evidence-hash-mismatch"></a> | 409 | `FAILED_PRECONDITION` | the bytes do not match the manifest's `artifact_hash` or `bytes_len`. Fails closed — nothing is stored. |
| `evidence-grant-invalid`<a id="problem-evidence-grant-invalid"></a> | 403 | `PERMISSION_DENIED` | the upload grant is unknown, expired, already spent, or bound to a different artifact. One slug for all four, on purpose. |
| `evidence-on-hold`<a id="problem-evidence-on-hold"></a> | 409 | `FAILED_PRECONDITION` | the artifact is under legal hold and cannot be purged. Distinct from `evidence-expired`: that one says the bytes are already gone, this one says they are deliberately being kept. A hold blocks deletion and never reading. |
| `result-too-large`<a id="problem-result-too-large"></a> | 413 | `RESOURCE_EXHAUSTED` | the artifact is over the 1 MiB inline cap; the detail names the grant flow to use instead. On the MCP endpoint, also a tool result over `MUNARIUM_MCP_RESULT_MAX_BYTES`; the detail names a smaller limit or cursor, or the recovery read for an operation that ran. |

### MCP endpoint slugs

ClearView ([operating the MCP endpoint](../ops/mcp.md)). These
are returned only by the opt-in `POST /mcp` endpoint, inside a tool result with
`isError: true` whose structured content is the problem and whose
`_meta["io.munarium/problem"]` is the slug. The two generation slugs and
`mcp-accounting-unavailable` are raised only under an MCP tool call; native
routes never return any of these. Entry refusals (`unauthenticated`,
`token-expired`, `token-revoked`, `scope-missing`, `uid-mismatch`, `forbidden`)
keep their registered meanings and HTTP statuses, with a `Bearer` challenge on
401.

| Slug | HTTP | gRPC code | Meaning |
|---|---|---|---|
| `mcp-audit-unavailable`<a id="problem-mcp-audit-unavailable"></a> | 503 | `UNAVAILABLE` | the durable admission record for the tool call was not confirmed (no durable store, store error, deadline, or a lost commit acknowledgment), so the tool **did not run**: no read, mutation, reservation or provider call. The problem carries `executed: false`. Content-free. A keyed call can be recovered with the same operation key. |
| `mcp-tool-unavailable`<a id="problem-mcp-tool-unavailable"></a> | 403 | `PERMISSION_DENIED` | the tool or argument form is not offered on this deployment: the execution form of a generating tool with `MUNARIUM_MCP_PAID_TOOLS=disabled`, or any keyed execution with `MUNARIUM_MCP_EXECUTION=recovery-only`. The recovery form (the operation key alone) stays available. |
| `mcp-operation-conflict`<a id="problem-mcp-operation-conflict"></a> | 409 | `FAILED_PRECONDITION` | the operation key already identifies a call with different arguments. Nothing ran. Recover with the key alone, or choose a new key for new work. |
| `mcp-operation-key-invalid`<a id="problem-mcp-operation-key-invalid"></a> | 400 | `INVALID_ARGUMENT` | the operation key cannot admit work: expired, beyond `MUNARIUM_MCP_KEY_MAX_VALIDITY_SECS`, expiring before the work deadline, or expired before dispatch. Nothing ran. A client never extends a key or mints a replacement automatically. |
| `mcp-generation-not-authorized`<a id="problem-mcp-generation-not-authorized"></a> | 403 | `PERMISSION_DENIED` | an MCP call reached a model stage that is not authorized: generation disabled, a contributing collection without a budget owner and both allowances, no common budget owner, or a model suspended after usage exceeded its reservation bound. Refused before the provider is contacted. |
| `mcp-allowance-exhausted`<a id="problem-mcp-allowance-exhausted"></a> | 429 | `RESOURCE_EXHAUSTED` | the next physical model attempt does not fit the call's request allowance or a contributor's UTC-day allowance. Refused before dispatch with nothing reserved. Earlier stages of the same call may have run and been charged. |
| `mcp-accounting-unavailable`<a id="problem-mcp-accounting-unavailable"></a> | 503 | `UNAVAILABLE` | the generation accounting store could not reserve the attempt, so no model call was made. |

## Idempotency scope

The `Idempotency-Key` header (REST) / `idempotency-key` metadata (gRPC) is
required on the command operations only — and the two planes differ by exactly
one operation. **gRPC: all eight `CommandService` RPCs** — CreateVersion,
ProposeClaim, AppendEvents, OpenPromise, FulfillPromise, LockAnchor,
RecordCounts, UpsertDigest. **REST: the seven twins under `/v1/versions...`**
— `PUT /v1/versions/{id}/digests` (an upsert by definition) takes no key,
while its gRPC twin `UpsertDigest`, like every other command RPC, requires
one. Replaying the same key with the same body returns the recorded response;
the same key with a different body is `idempotency-mismatch` (422).

Records are **plane-namespaced**: the stored request hash carries a `rest:` /
`grpc:` prefix, so reusing one Idempotency-Key for a REST command and its gRPC
twin is a 422 `idempotency-mismatch` — never a cross-format replay of a
response encoded for the other plane.

Idempotency is **not** enforced on the un-keyed writes
(shapes, sources, ingests, index build, providers, runbooks, approve) — source
upload is idempotent by content address instead.

## Content-hash inputs

Wherever a request *supplies* a `content_hash` (ingest records, runbook
`contentHashes` bindings), it must be a full 64-character hex sha-256 digest —
anything else is `invalid-input` (400 / `INVALID_ARGUMENT`). Truncated or
non-hex values are rejected rather than matched loosely, because the hash is
an integrity claim, not a search term.

## Gate rule ids (`gate_findings[].rule_id`)

The gates' dotted rule vocabulary:

| Rule | Severity | Fires when |
|---|---|---|
| `gate.anchor-consistency` | block | claim/correction value differs from a locked anchor for the same `subject.key` |
| `gate.ledger-conflict` | block | a plain claim contradicts current accepted canon (declared supersessions exempt); suppressed when the same claim_key already drew an anchor finding |
| `gate.orphaned-reference` | warn | a correction targets canon never established |
| `gate.meta-leakage` | warn | AI/boilerplate markers in output text |
| `gate.lexical-similarity` | warn | ≥ 90% similar to a previous unit's text |
| `gate.chronology-order` / `-deadline` / `-duration` | warn (rule may declare block) | declaratively-armed calendar rules; fires only on CERTAIN violations |
| `gate.promise-unfulfilled` | warn | open promise past its due scope / final unit |
| `gate.counter-budget` | warn | whole-document frequency budget exceeded |
| `shape.schema-violation` | block | event body fails its shape's JSON Schema |

Backfill mode (`mode: backfill`) downgrades blocks to warns — conflicts in an existing corpus are
surfaced (claims disputed, findings filed) but never block.

## Build-job failure codes

A failed build job (`GET /v1/index-build-jobs`, `GET /v1/index-build-jobs/{job_id}`)
carries `error_code` only when the builder classified the failure's typed cause as it
recorded it (Server v1.5, ClearView C2; migration 0047). These are job outcomes, not
request errors: the read itself succeeds. Codes are lowercase snake case of at most 64
characters, enforced on write. Clients key on `error_code`, never on `error` text.
Each code's row carries a stable anchor, `#job-failure-<code>`, which the console's
build-job page links to.

| Code | Recorded when | `error` |
|---|---|---|
| `attempts_exhausted`<a id="job-failure-attempts_exhausted"></a> | the final attempt's lease expired before it completed, and expired-claim cleanup failed the job | build job failed: its final attempt stopped renewing its lease before completing |
| `invalid_input`<a id="job-failure-invalid_input"></a> | the build refused the job's target or parameters (`invalid-input` cause) | build job failed: the build refused the job's target or parameters |
| `not_found`<a id="job-failure-not_found"></a> | something the job names no longer exists (`not-found` cause) | build job failed: something the job names no longer exists |
| `storage_error`<a id="job-failure-storage_error"></a> | a storage-layer failure (`storage-error` cause); the driver text goes to the server log only | build job failed: a storage error occurred; see the server log |
| `datastore_unavailable`<a id="job-failure-datastore_unavailable"></a> | a datastore artifact could not be read or written (`datastore-unavailable` cause), including a build refused in `MUNARIUM_ARTIFACT_DAMAGE_MODE=enforce` because its result is a fleet-blocked artifact ([artifact damage](../ops/artifact-damage.md#builds-of-a-blocked-artifact)) | build job failed: a datastore artifact could not be read or written |
| `provider_error`<a id="job-failure-provider_error"></a> | an upstream model provider call failed (`provider-error` cause) | build job failed: an upstream model provider call failed |

Any other cause is unclassified: `error` is the generic message "build job failed; no
classified cause was recorded for this failure" and `error_code` is omitted. The same
applies to every failure recorded before migration 0047 and to a free-text failure
written by a v1.4 builder during rollout. Server never derives a code from stored text
and does not rewrite those rows; their original text remains in the database for the
operator. A successful, pending, running or cancelled job omits both members.

**Displayed-text change (v1.5).** In v1.4, `error` returned the stored error string,
which for a storage failure was the database driver's message. From v1.5 it is one of
the messages above. Scripts that matched `error` text must move to `error_code` where
one exists; a failure without a code has no stable machine-readable cause.

## Console fleet cutover refusals

`munarium-server console-fleet enable` (Server v1.5, ClearView LOGIN1; migration 0087)
runs the cutover preflight before it switches a deployment environment's console fleet
mode to `enabled`. A refusal writes nothing, exits with status 3 and prints
`{"result": "refused", "reason": <code>, "live_replicas": <count or null>}`. The codes
name the failed rule only; they carry no replica identity or configuration. This is an
operator command, not an HTTP problem type. See
[console sessions](../design/console-sessions.md#cutover).

| Code | Refused when |
|---|---|
| `membership_unavailable` | the replica heartbeats could not be read |
| `replica_lacks_console_sessions` | a live replica does not report the `console-sessions` capability (its `MUNARIUM_CONSOLE_SESSIONS` is not `enabled`) |
| `replica_count_mismatch` | a live replica's `MUNARIUM_REPLICA_COUNT` differs from `--expected-replicas` |
| `replica_shortfall` | fewer replicas are live than expected: a v1.4 replica, one of unknown version or one whose heartbeat fails may be serving |
| `replica_excess` | more replicas are live than expected |

`console-fleet disable` is never refused. A usage error exits with status 2 and a
database that cannot be reached with status 1 (status 2 when the failure occurs after
connecting).

## Platform action records

Experimental [Stage 2 records](../platform-stage2.md) use existing error families:
`invalid-input` for invalid canonical shapes/digests or missing prerequisites,
`forbidden` for current identity, producer, scope or recovery refusal, and
`idempotency-mismatch` for changed immutable identities. Exhausted storage
contention returns a storage failure; no error is an acknowledgement. REST and
`PlatformRecords` gRPC share these checks and the existing error mapping.

## Durable governance policies

Memory-version metadata can pin an immutable governance profile; omitted legacy
configuration retains legacy text comparison. See the
[policy and upgrade contract](../ops/governance-policy-upgrade.md) for creation,
explicit transitions, companion findings, exports and collection rebuilds.
Unknown profiles, undeclared transitions and external `governance.` findings are
`invalid-input`; a stale transition head is `head-conflict`. REST and native gRPC
share the same version creation and command evaluation behavior.
