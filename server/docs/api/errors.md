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

## Problem types

| Slug | HTTP | gRPC code | Meaning / extensions |
|---|---|---|---|
| `head-conflict` | 409 | `ABORTED` | optimistic `expected_head` mismatch. Extensions: `expected`, `actual`. Normal and retryable: re-read, re-decide, retry. |
| `policy-rejection` | 422 | `FAILED_PRECONDITION` | block-severity gate findings. The claim IS recorded (`disputed`) and the rejection is itself a ledger event. Extensions: `gate_findings` (rule_id, severity, message, detail), `policy_citation`. |
| `shape-violation` | 422 | `FAILED_PRECONDITION` | event body failed its shape's JSON Schema at the command gate. Extensions: `shape_ref`, `policy_citation`. Recorded as an event. |
| `idempotency-mismatch` | 422 | `INVALID_ARGUMENT` | same `Idempotency-Key`, different request hash. |
| `overloaded` | 503 | `RESOURCE_EXHAUSTED` | load shed: the instance is at `MUNARIUM_MAX_CONCURRENCY` in-flight /v1 (REST) or /mmp.v1 (gRPC) requests. REST responses carry `Retry-After: 1`. Health/meta routes never shed. Emitted since 2026-08-17. |
| `session-not-open` | 409 | `FAILED_PRECONDITION` | a turn (or lifecycle action) against a session that is closed or expired; the detail names the actual state. Added 2026-08-17 with `POST /v1/sessions/{id}/close`. |
| `required-evidence-unavailable` | 424 | `FAILED_PRECONDITION` | a REQUIRED layer of the turn's research profile produced no evidence, so the turn refuses rather than answering from an incomplete hierarchy. The detail names the **layer** and its refusal code, never the layer's sources — a caller who cannot see a source must not learn it exists from the shape of a refusal. |
| `unknown-research-profile` | 400 | `INVALID_ARGUMENT` | the turn named a `research_profile` the session's pinned runbook does not declare. Fails closed rather than falling back to the document path: silently answering a different question than the one asked is the worse failure. |
| `run-locked` | 409 | `ABORTED` | this runbook run is already executing on another instance (the cross-instance advisory lock, 2026-08-17). Poll `GET /v1/runs/{run_id}` and retry when it settles. |
| `not-found` | 404 | `NOT_FOUND` | extensions: `kind`, `id`. |
| `invalid-input` | 400 | `INVALID_ARGUMENT` | malformed request outside schema concerns. |
| `unauthenticated` | 401 | `UNAUTHENTICATED` | missing/invalid bearer token. |
| `forbidden` | 403 | `PERMISSION_DENIED` | valid token, wrong tenant/role. |
| `rate-limited` | 429 | `RESOURCE_EXHAUSTED` | per-tenant limits or provider budget (rpm/tpm) exhausted — including an upstream provider 429 that survived the retry policy (2026-09-01; previously flattened into `provider-error`). REST responses carry `Retry-After: 60` (the rpm window's upper bound). |
| `daily-cap-reached` | 429 | `RESOURCE_EXHAUSTED` | the (provider config × tier) daily token cap is exhausted (spending caps, 2026-09-01: `spec.budgets.dailyTokens`, UTC-day window, migration 0029). The detail names the config, tier, remaining/limit and the reset; REST responses carry `Retry-After` = seconds to midnight UTC. Recovery differs from `rate-limited`: wait for the window, or re-send on a cheaper tier. |
| `provider-error` | 502 | `UNAVAILABLE` | upstream LLM endpoint failure after retries; extensions: `provider`, `endpoint_fingerprint` (never key material). |
| `storage-error` | 500 | `INTERNAL` | backend failure. |
| `datastore-unavailable` | 503 | `UNAVAILABLE` | a scope selected to serve from the datastore could not be served from its artifacts. There is deliberately **no fallback past selection**: a replica that agreed to serve a scope from the datastore leaves the traffic pool rather than silently answering from PostgreSQL, so the two engines can never be confused for each other. Recovery is an operator action, never a request-path one — `PUT /v1/retrieval-rollout` with `serving: postgres` (the rollback, never gated) or `mmctl datastore rollout set <collection|shape> <id> postgres` per scope for a whole corpus. |

### platform identity/token slugs (emitted by the server layer, not the kernel)

| Slug | HTTP | gRPC code | Meaning |
|---|---|---|---|
| `uid-required` | 400 | `INVALID_ARGUMENT` | `X-Munarium-Uid` header (REST) / `munarium-uid` metadata (gRPC) missing on a `/v1` / `mmp.v1` call while `MUNARIUM_REQUIRE_UID=true` (the default) **and** the bearer is not a capability JWT — when it is, the token's `sub` supplies the uid and the call proceeds. |
| `uid-mismatch` | 403 | `PERMISSION_DENIED` | the asserted uid differs from the capability token's `sub` claim. |
| `token-expired` | 401 | `UNAUTHENTICATED` | the capability JWT's `exp` has passed (30 s leeway); request a new token from the management plane. |
| `token-revoked` | 401 | `UNAUTHENTICATED` | the capability JWT's `jti` is revoked (only when `MUNARIUM_TOKEN_REVOCATION_CHECK=true`). |
| `scope-missing` | 403 | `PERMISSION_DENIED` | the capability token lacks the scope the endpoint requires (`query` for sessions/turns, `ingest` for file ingestion, `findings` for filing findings, `evidence` for the evidence plane). |
| `override-not-allowed` | 403 | `PERMISSION_DENIED` | an API model override named a provider the runbook's `models.allowOverrides` policy does not permit. |
| `removal-not-confirmed` | 409 | `FAILED_PRECONDITION` | runbook removal is double-pass: no pending request, wrong `removal_id`, or the 15-minute confirmation window expired. |
| `authoring-draft-invalid` | 409 | `FAILED_PRECONDITION` | an authoring draft cannot export/apply while error-severity findings exist (or it has no documents yet) — export and apply re-validate inline; the detail lists the finding codes. |
| `runbook-removed` | 410 | `NOT_FOUND` | the exact name@version was soft-removed; its data is retained but the runbook is inaccessible. |

### Sealed evidence slugs

The structured-evidence plane. Note what the refusals deliberately do **not**
say: `evidence-forbidden` describes neither the artifact nor its class, because
learning "this exists and is above you" is itself a disclosure; and
`evidence-grant-invalid` covers four distinct causes under one slug so a caller
cannot probe for valid grant ids.

| Slug | HTTP | gRPC code | Meaning |
|---|---|---|---|
| `evidence-forbidden` | 403 | `PERMISSION_DENIED` | the session does not **dominate** the artifact's authorization class (level, or a missing compartment). Also returned when a caller tries to SEAL above its own clearance — a principal may not mint evidence it could not itself read. |
| `evidence-expired` | 410 | `NOT_FOUND` | retention purged the bytes. The metadata row survives precisely so this is distinguishable from a 404: the citation was real, and the retention policy is the honest reason it no longer resolves. |
| `evidence-not-committed` | 409 | `FAILED_PRECONDITION` | the manifest is registered but the bytes were never committed. A pending artifact is not evidence yet. |
| `evidence-hash-mismatch` | 409 | `FAILED_PRECONDITION` | the bytes do not match the manifest's `artifact_hash` or `bytes_len`. Fails closed — nothing is stored. |
| `evidence-grant-invalid` | 403 | `PERMISSION_DENIED` | the upload grant is unknown, expired, already spent, or bound to a different artifact. One slug for all four, on purpose. |
| `evidence-on-hold` | 409 | `FAILED_PRECONDITION` | the artifact is under legal hold and cannot be purged. Distinct from `evidence-expired`: that one says the bytes are already gone, this one says they are deliberately being kept. A hold blocks deletion and never reading. |
| `result-too-large` | 413 | `RESOURCE_EXHAUSTED` | the artifact is over the 1 MiB inline cap; the detail names the grant flow to use instead. |

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
