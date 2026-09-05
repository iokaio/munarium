# munarium-server REST API

**Status: live.** Every `/v1` route (Command/Query, shapes, ingest, retrieval, providers,
runbooks) plus the meta routes carries OpenAPI annotations, and the spec declares the
`bearerAuth` security scheme. This document is the human guide; the machine
truth is [openapi.json](openapi.json) (generated from the `munarium-api-types` structs by
`munarium-server openapi`, served live at `GET /openapi.json`, browsable at `GET /docs`).

## Base URL and versioning

All routes are versioned under `/v1/`. The REST plane listens on **:8080** in-container and is
served on **:443** through the gateway in deployed environments.

```
https://<host>/v1/...
```

## Authentication and the uid contract

`Authorization: Bearer <token>` — one of two credential kinds:

- **Static token** (`MUNARIUM_AUTH_MODE=static`): maps to `tenant:role`, role ∈
  `rw | ro | mgmt`. The control plane (shapes, providers, runbooks, ledger)
  takes `rw`/`ro`; the management plane (`POST /v1/access-tokens`, reports)
  takes `mgmt`.
- **Capability JWT**: a short-lived HS256 token minted by
  `POST /v1/access-tokens` carrying `{sub, ten, lvl, cmp, scopes, rb, jti, exp}`.
  Data-plane credentials for sessions/turns (`query` scope) and file
  ingestion (`ingest` scope).

OIDC is deliberately **not** implemented: user authentication belongs to the
enterprise API-management layer in front of munarium-server — the full rationale
and division of responsibilities is [../security-posture.md](../security-posture.md).

**uid contract:** every `/v1` request must carry `X-Munarium-Uid: <uid>` — the
end-user id the API-management layer authenticated. Missing → `400
uid-required`, unless the bearer is a capability JWT: there the token's `sub`
supplies the uid, since the header could only agree with it anyway. With a
capability JWT a header that is present must equal the token's `sub`
→ `403 uid-mismatch` otherwise. The uid attributes every log line and every
row in the interaction audit (requests + responses, bodies capped at
`MUNARIUM_INTERACTION_BODY_MAX`). Responses echo `x-munarium-request-id`.

Every request is tenant-scoped by its token. There is no cross-tenant read path outside the Admin API.

## Idempotency

Every **core command** request (the version/claim/event/promise/anchor/counter writes under
`/v1/versions...`) **requires** an `Idempotency-Key` header. Replaying the same key
with the same request body returns the recorded response; the same key with a different body is
rejected with problem type `idempotency-mismatch` (HTTP 422). Records are namespaced per
transport plane: reusing one key for a REST command and its gRPC twin is a mismatch (422),
never a cross-format replay. The un-keyed writes (shapes, sources,
ingests, index build, providers, runbooks, approve) take no key — source upload is idempotent by
content address instead — and `PUT /v1/versions/{id}/digests` (an upsert by definition) is exempt.
Full scope table: [errors.md](errors.md#idempotency-scope).

## Optimistic concurrency

Command bodies accept `expected_head` (the lineage head seq you read). A mismatch returns
HTTP 409 with problem type `head-conflict` — a normal, retryable outcome: re-read, re-decide, retry.

## Point-in-time reads

Every query route accepts `?as_of_seq=N`.
One pin bounds everything: facts, anchors, promises (a promise fulfilled after the pin reads back
**open**), counters — and digests are rebuilt deterministically at the pin, never served stored.
`as_of_date=YYYY-MM-DD` on ComposeContext is declared in the contract but **not yet implemented**
— the server rejects it explicitly (`invalid-input`) rather than silently ignoring it. The same
applies to the retrieval `filter` member on `POST /v1/search`.

## Route map

| Group | Method + path | gRPC twin |
|---|---|---|
| Command | `POST /v1/versions` | `CommandService/CreateVersion` |
| | `POST /v1/versions/{id}/claims` (optional `origin` block — connector provenance, returned on every claim read; `null` on model-extracted claims) | `ProposeClaim` |
| | `POST /v1/versions/{id}/events` | `AppendEvents` (batch, gated as one unit) |
| | `POST /v1/versions/{id}/promises` | `OpenPromise` |
| | `POST /v1/versions/{id}/promises/{key}/fulfill` | `FulfillPromise` |
| | `POST /v1/versions/{id}/anchors` | `LockAnchor` |
| Query | `GET /v1/versions/{id}/head` | `GetHead` |
| | `GET /v1/claims/{id}` | `GetClaim` |
| | `GET /v1/versions/{id}/facts?scope_prefix=&as_of_seq=&limit=&statuses=` | `SliceFacts` |
| | `GET /v1/versions/{id}/lineage` | `GetLineage` |
| | `GET /v1/versions/{id}/anchors?as_of_seq=` | `ListAnchors` |
| | `GET /v1/versions/{id}/promises?status=&as_of_seq=` | `ListPromises` |
| | `GET /v1/versions/{id}/context?scope=&budget_tokens=&as_of_seq=` | `ComposeContext` |
| | `POST/GET /v1/versions/{id}/counters` · `PUT/GET /v1/versions/{id}/digests` | `RecordCounts`/`CounterTotals` · `UpsertDigest`/`ListDigests` |
| Retrieval | `POST /v1/search` | `HybridSearch` |
| | `GET /v1/indexes/{shape_ref}` · `POST /v1/indexes/{shape_ref}/build?version_id=` | `GetIndexVersion` |
| Ingest | `PUT /v1/sources` (body = bytes; `X-Content-Sha256` verified before commit) | `PutSource` |
| | `GET /v1/sources/{source_id}` — metadata, never the bytes: `source_id`, `filename`, `media_type`, `content_hash`, `bytes_len`, `storage_backend`, `blob_uri`, `extraction_status`, `extraction_method`, `created_at` | REST-only |
| | `POST /v1/versions/{id}/ingests` | `RecordIngest` |
| Shapes/Runbooks | `POST /v1/shapes` · `POST /v1/runbooks` · `POST /v1/runbooks/{ref}/runs` · `GET /v1/runs/{id}` · `POST /v1/runs/{id}/steps/{n}/approve` | `RunbookService/*` |
| Token budgets (2026-09-02) | `GET /v1/max-tokens` (any authenticated role: the eight per-call output-token ceilings in effect for the tenant, flattened, plus `source` = `tenant` \| `environment` and `updated_at`) · `POST /v1/max-tokens` (rw: **replace the whole set** — all eight fields required, a missing one is 400 `invalid-input`, each range-checked; no partial update, no delete — post the environment values to return to them). Precedence per call: the runbook's own `completion.maxTokens` / `modelQueryExpansion.maxTokens` > this replacement > `MUNARIUM_MAX_TOKENS_*` > the built-ins. Persisted per tenant on Postgres (migration 0031), cached per replica with the registry TTL. Reference: [../tokenbudgets.md](../tokenbudgets.md) | REST-only |
| Providers | `POST /v1/providers` · `GET /v1/providers` (2026-08-23: free introspection — every applied config + the synthesized defaults with the concrete model each fast/capable tier resolves to; zero provider calls, `credentialRef` never echoed, only `credential_ok`) · `GET /v1/providers/{name}/health` · `POST /v1/providers/{name}/complete` · `POST /v1/providers/{name}/embed` · `GET /healthai` | `ProviderService/*` (healthai + the list are REST-only) |
| Access tokens | `POST /v1/access-tokens` (mgmt; mints a capability JWT) · `GET /v1/access-tokens?uid=&active=` · `POST /v1/access-tokens/{jti}/revoke` | REST-only until AdminService is served |
| Collections | `POST/GET /v1/collections` · `GET /v1/collections/{id}` — **no DELETE route exists anywhere** (physical deletion = [../ops/index-deletion-runbook.md](../ops/index-deletion-runbook.md)); `/v1/search` accepts `filter: {"collections": ["<name-or-id>"]}` · `POST /v1/collections/{id}/activate-index` (2026-08-31, rw: the §7.3 logical activation as a compare-and-swap — `{index_version_id, expected_active}`; a mismatch answers `activated: false` with the pointer untouched, and a datastore-routed collection refuses a version with no verified `serving` binding, so the order is build → promote → activate) | REST-first |
| Datastore plane (2026-08-30/31; Postgres store only — the memory store answers 400 `invalid-input`) | The derived-index tier beside PostgreSQL (operator commands: [../ops/mmctl.md](../ops/mmctl.md) "`mmctl datastore`"; artifact contract: [../../contract/datastore/README.md](../../contract/datastore/README.md)). Every operation names a LOGICAL version; an `artifact_id` (sha256 of the manifest) appears only in answers, never as a parameter that grants access. `GET /v1/index-artifacts/{index_version_id}` (rw: the version's catalogued artifacts — `sealed \| verified \| failed \| retired` — and its `staged \| shadow \| serving` bindings with generations) · `POST /v1/index-artifacts/{index_version_id}/verify` (rw: re-read the stored bytes, re-verify manifest and components; `verified: false` carries a bounded `detail`) · `POST /v1/index-artifacts/{index_version_id}/rebuild` (rw: build the version again; outcome `published \| converged \| already_built \| deferred`, the last meaning another node holds the build) · `POST /v1/index-artifacts/backfill` `{collection_id}` (rw: build every serving-required version of a collection — `active` and `within_horizon` — answering `complete` only when EVERY required version has a verified artifact) · `POST /v1/index-artifacts/{index_version_id}/bind` `{slot: staged \| shadow, artifact_id, expected_generation?, reason?}` (rw: `serving` is deliberately not bindable here) · `POST /v1/index-artifacts/{index_version_id}/promote` `{expected_staged_generation, expected_serving_generation, reason?}` (rw: staged → serving as a CAS against BOTH generations; refused unless a fleet gate reads every node's heartbeat and staged-open residency) · `POST /v1/index-build-jobs` `{kind: backfill \| rebuild \| direct, collection_id \| index_version_id, max_chars?, watermark_seq?, correlation_id?}` (rw: the durable request — the request path never builds; a process with `MUNARIUM_DATASTORE_BUILDER=enabled` claims it, SKIP-LOCKED, lease-lapsed re-offer, attempt ceiling) · `GET /v1/index-build-jobs` · `GET /v1/index-build-jobs/{job_id}` (any role: `pending \| running \| succeeded \| failed \| cancelled \| superseded`, attempts, `claimed_by`, bounded `result`/`error`) · `POST /v1/index-build-jobs/{job_id}/cancel` (rw) · `GET /v1/retrieval-rollout/{scope_kind}/{scope_id}` (any role: which engine serves a `collection` or `shape` — `serving: postgres \| datastore`, `prewarm_staged`, `required_versions_policy`, `generation`) · `PUT /v1/retrieval-rollout` (rw: create or CAS-change a scope's selector; selecting `datastore` is gated on serving-required completeness, selecting `postgres` — the rollback — never is). Serving refusals are 503 `datastore-unavailable` (no fallback past selection). CLI: `mmctl datastore status\|verify\|rebuild\|backfill\|bind\|promote\|rollout\|jobs` ([../ops/mmctl.md](../ops/mmctl.md)); a whole-corpus cutover, and its rollback, is `rollout set` over each of the corpus's scopes | REST-only |
| Runbooks v2 | `GET /v1/runbooks?include_removed=` (per-collection access requirements) · `GET /v1/runbooks/{name}` (info + versions + model defaults) · `POST /v1/runbooks/validate?suggest=&provider=&model=&tier=` · `POST /v1/runbooks/{ref}/remove-request` → `POST /v1/runbooks/{ref}/remove-confirm` (double-pass soft removal) | REST-first |
| Guided authoring (2026-08-19) | `GET /v1/authoring/patterns` · `GET /v1/authoring/patterns/{id}` (the dev-guide §19 catalog with embedded exemplars; any role) · `POST/GET /v1/authoring/drafts` · `GET/DELETE /v1/authoring/drafts/{id}` · `PUT /v1/authoring/drafts/{id}/answers` (§16-ordered interview → deterministic materialization) · `POST /v1/authoring/drafts/{id}/validate` (per-document + `set.*` cross-document findings) · `POST /v1/authoring/drafts/{id}/assist` (BYOK drafting; degrades to `assist_note` keyless) · `POST /v1/authoring/drafts/{id}/export` (hash-manifested bundle; 409 `authoring-draft-invalid` on error findings) · `POST /v1/authoring/drafts/{id}/apply` — drafts are rw + postgres-store only; deploy to prod with `mmctl bundle apply -f bundle.json` through the existing shapes/runbooks routes | REST-only |
| Sessions | `POST /v1/runbooks/{name}/sessions` · `POST /v1/sessions/{id}/turns` (multi-collection access-filtered retrieval + optional completion with policy-gated `model_override`) · `POST /v1/sessions/{id}/turns/stream` (2026-08-23: the same turn as SSE — `progress` events at the real stage boundaries (per-collection retrieval, merge, model resolution, each paid completion with token counts, each verification pass), terminated by exactly one `done` (the full TurnResponse) or `error` (problem+json); auth/refusals before the stream starts answer plain problem+json; delivered live — the capture middleware passes `text/event-stream` through unbuffered (same-day fix: it buffered every /v1 body at first, so the sequence arrived in one burst) and records the interaction at END of stream with the turn's session/runbook attribution and the terminal event's status via the handler's `StreamOutcome` slot; REST-only, no gRPC twin) · optional **`research_profile`** on the turn body: runs the turn through a named evidence hierarchy from the runbook's `retrieval.researchProfiles`, layering documents, Matrix data views (a `dataViews` entry with `kind: metric_view` or `data_view` is asked with a **semantic intent** the runbook's `intent` model task composes from the view's declared measures and dimensions — names, never SQL — and refuses `intent-unresolved` as a block when none was produced; `kind: contract`, the default, posts the structured query exactly as before) and pinned ledger facts in declared trust order, and returns an `EvidenceHierarchyDecision` on the response plus `profile`/`layer_start`/`layer_source`/`layer_complete`/`coverage`/`compose` SSE stages. Absent — and with no `retrieval.defaultResearchProfile` — the turn executes and serializes **exactly** as before, which is the invariant the whole feature is built around. A named-but-undeclared profile is 400 `unknown-research-profile`; a REQUIRED layer producing nothing is 424 `required-evidence-unavailable`, naming the layer and never its sources · `GET /v1/sessions/{id}` · `POST /v1/sessions/{id}/close` (2026-08-17: idempotent lifecycle end — owner or rw/mgmt; further turns answer 409 `session-not-open`) — capability JWT (`query` scope) + uid | REST-first |
| Governance reads (2026-08-17; findings write 2026-08-28) | `GET /v1/versions/{id}/findings?severity=&rule_id=&rule_prefix=&as_of_seq=&limit=` (the persisted gate findings — until now findings rode only the write response; `rule_prefix=gate.` or `matrix.` selects a family) · **`POST /v1/versions/{id}/findings`** (file findings computed outside the gates — Munarium Matrix's `matrix.discrepancy-candidate`; warn/info only, a `block` is 400; stamped at the current head seq; idempotent by content `(rule_id, detail.evidence_ref, detail.claim_id)`, no `Idempotency-Key`; static rw or a capability token with the new `findings` scope) · `GET /v1/versions/{id}/promises?overdue_scope=&final=` (adds kernel-computed `gate.promise-unfulfilled` warn findings to the response) | REST-only |
| Sealed evidence (2026-08-28) | `POST /v1/evidence` — seal an artifact **inline** (manifest + `bytes_base64` in one round-trip, at or under the 1 MiB cap) or take a single-use upload **grant** (omit the bytes) · `PUT /v1/evidence/{id}/bytes?grant=` · `POST /v1/evidence/{id}/commit` (re-reads and re-verifies both hashes; commit is the moment the artifact becomes citable, so it is the moment the claim must be true) · `GET /v1/evidence/{id}` (the manifest itself, **unwrapped** — the contract says this route returns an `EvidenceManifest`, so it does; a 200 already means `committed`, since pending answers 409 and purged 410 — access-checked, audited) · `GET /v1/evidence/{id}/rows?from=&limit=` (bounded at 1000, audited; canonical CSV only — Parquet is sealed and replayed byte-for-byte but not decoded here) · `GET /v1/evidence/{id}/accesses?limit=` (**mgmt**: who resolved it and how it went, never what was read) · **`DELETE /v1/evidence/{id}`** (**mgmt**: purge the bytes now; refuses `evidence-on-hold` under a legal hold, which is what makes a hold mean anything; the metadata row survives with `purged_at` so every citation keeps resolving as `evidence-expired` rather than `not-found`) · **`POST /v1/evidence/{id}/legal-hold`** (**mgmt**, `{hold: bool}`: a hold blocks deletion and never reading, and survives the artifact's own expiry indefinitely). A retention **janitor** sweeps expired, unheld artifacts on `MUNARIUM_EVIDENCE_PURGE_INTERVAL_SECS` (0 = disabled, the default: a janitor nobody configured, deleting regulated data on a schedule nobody chose, is worse than one that never runs); it deletes bytes BEFORE marking the row, so a failure is self-healing rather than a row that claims to be purged while its bytes remain. The manifest is the vendored contract's `EvidenceManifest` (`contract/matrix/evidence-manifest.schema.json`), carried verbatim. Auth: static rw, or a capability token with the new `evidence` scope — which must additionally **dominate** the class the manifest declares, so a principal cannot seal evidence it could not itself read. Idempotent by the domain tuple `(tenant, logical_result_hash, policy_version, authorization_class)` — note `artifact_hash` is absent from it, so re-serializing one logical result does not mint a second artifact; the seal route deliberately does not consult the header-keyed `Idempotency-Key` store the other commands use, because the domain key is the stronger guarantee (it holds across replicas and across headers) (corrected 2026-09-02; the earlier "idempotent twice over" wording described a header layer the route never had). REST-only in v1; no gRPC twin | REST-only |
| Chronology rules (2026-08-17) | `POST /v1/chronology-rules` (YAML asset, rw) · `GET /v1/chronology-rules/{name}` — the sixth gate's arming surface: a version created with metadata `{"chronology_rules": "<name>"}` runs `check_chronology` on every gated write; certain violations join the findings stream. `mmctl apply -f` kind-sniffs the asset | REST-first |
| Ingestion | `POST /v1/ingest` · `POST /v1/ingest/batch` (base64 files; explicit `collections` or declarative runbook matchers; clearance-checked) — capability JWT (`ingest` scope) + uid | REST-first |
| Bulk upload sessions (2026-08-19) | `POST /v1/ingest/bulk` (open with a manifest of `{filename, sha256, bytes_len, media_type}`; the server diffs it against `sources` and answers with the `needed` work list) · `POST /v1/ingest/bulk/{id}/chunk` (≤500 files/chunk, same envelope as batch; each file verified against its manifest sha256 — a mismatch fails per-file, never the chunk; storage/binding identical to single ingest) · `GET /v1/ingest/bulk/{id}?include_needed=` (progress + resume work list) · `POST /v1/ingest/bulk/{id}/complete` (finalize: every entry re-verified against `sources`; `incomplete` names what is missing or hash-drifted). Per-document idempotent — re-sending a failed chunk wholesale re-writes nothing already stored; a re-run over an already-loaded corpus needs zero bytes. Sessions expire after 7 days. Same auth as ingest. `mmctl bulk upload --dir <dir> --prefix <p/>` drives the whole flow | REST-only |
| Reports | `GET /v1/reports/usage?group_by=uid\|session\|runbook\|collection` · `GET /v1/reports/audit` (keyset pagination via `before` / `next_before`) · `GET /v1/reports/cost` (mgmt role) · `GET /v1/reports/budgets` (2026-09-01, mgmt: today's spending-cap ledger per provider config × tier — held/settled tokens, reservations — beside each scope's configured `spec.budgets.dailyTokens` ceiling and the remaining balance; read through the enforcer's own UTC-day window expression so the report and the 429 `daily-cap-reached` refusal cannot disagree about which day it is; a configured cap with no traffic yet still gets a zero row) | REST-only |
| Reports, dashboard views (2026-08-17) | `GET /v1/reports/timeseries?window=1h\|24h\|7d\|30d&plane=` (bucketed count/4xx/5xx/p50/p95 over the interactions trail — aggregates across every instance on the shared database) · `GET /v1/reports/endpoints?window=&limit=` · `GET /v1/reports/runbooks?window=` · `GET /v1/reports/sessions?window=` (mgmt role) | REST-only |
| Reports, evidence hierarchy (2026-08-28) | `GET /v1/reports/evidence?window=` — per-layer turns/refusals/completeness and p50/p95, read from the `session_turns.hierarchy` decision the turn persists. The operational question it answers is *which layer is quietly refusing*: a layer that refuses on most turns still returns 200, so the answers get thinner while every other dashboard stays green. · `GET /v1/reports/matrix` — whether the structured-evidence plane is configured at all (distinct from configured-and-failing), the **per-instance** circuit-breaker state, and every data view declared across the tenant's applied runbooks (mgmt role) | REST-only |
| Operator console (2026-08-17; control plane 2026-08-27) | `GET /admin` (overview with control-plane inventory tiles) + traffic/endpoints/usage/providers/runbooks/collections/storage/sessions/tokens/audit/findings/health pages, the `/admin/matrix` page (plane health + the per-layer refusal table, flagging any layer refusing on half its turns), and the viewers `/admin/runbooks/{ref}`, `/admin/shapes/{ref}`, `/admin/chronology-rules/{name}`, `/admin/runs/{id}`, `/admin/collections/{id}`, `/admin/sessions/{id}` — server-rendered HTML + inline SVG, zero JS. mgmt bearer, or browser login at `/admin/login` (HttpOnly cookie). Outside `/v1` (never captured to the audit trail) and outside the OpenAPI contract, like `/docs`. Three actions, each the same call as its `/v1` twin: `POST /admin/tokens/issue` and `POST /admin/tokens/{jti}/revoke` (mgmt, like `/v1/access-tokens`), and `POST /admin/runs/{id}/steps/{n}/approve`, which takes the **rw** credential in the form because approval is an rw operation — the mgmt session alone cannot approve. Every POST carries a per-boot CSRF synchronizer token; a proxy sending `X-Munarium-Admin-View-Only: 1` gets every action form rendered as a note (docs/security-posture.md). The former `/admin/authoring` pages were removed 2026-08-27 — authoring stays on `/v1/authoring/*` and `mmctl author` | REST-only |
| Admin | **reserved — not implemented** (no `/v1/admin/*` routes exist; `admin.proto` is declared, not served) | `AdminService/*` (reserved) |
| Ops | `GET /healthz` · `GET /readyz` · `GET /version` · `GET /openapi.json` · `GET /docs` — plus the ops plane (:9090, never via ingress): `/healthz`, `/readyz` (same store probe as the REST twin), `/metrics` (Prometheus text) | `grpc.health.v1.Health` |

## Provider selection: default rule, tiers, and /healthai

Complete/embed routes normally address an applied ProviderConfig by `{name}`. The
reserved name **`default`** (which `POST /v1/providers` refuses as a config name)
engages the **default-provider rule**: anthropic first, openai second, openrouter
third — the first family with a usable credential serves the request. Within a
family, applied configs (sorted by name) beat the server's synthesized env-backed
default (`credentialRef: {env: MUNARIUM_SECRET_ANTHROPIC|OPENAI|OPENROUTER}` — inject
your provider keys into the server's environment under these names).

Request fields on `complete` (and `provider` on `embed`):

- `provider` — family override (`anthropic|openai|openrouter`); only honored with
  the `default` name.
- `tier` — `fast` (lesser model) or `capable`. Resolves via the config's
  `models.fast`/`models.capable` override, else the built-in table:

| Family | `fast` | `capable` |
|---|---|---|
| anthropic | `claude-haiku-4-5` | `claude-sonnet-5` |
| openai | `gpt-5.4-mini` | `gpt-5.4` |
| openrouter | `deepseek/deepseek-v4-flash` | `z-ai/glm-5.2` |

- `model` — any model the selected provider supports; always wins over `tier`.
  With neither: the config's first `models.complete` entry, else the built-in
  capable model. Responses echo the serving `provider` and resolved `model`.

**`GET /healthai`** (authenticated, any role — each call spends real provider
tokens) live-probes all nine built-in models with a tiny completion and returns
per-check `ok/skipped/latency_ms/detail` plus an overall `healthy` (all
configured providers passed, at least one credential present). Families whose
`MUNARIUM_SECRET_*` env var is unset are reported `skipped`.

## Worked example

```bash
TOK="Authorization: Bearer devtoken"

# create a lineage root
VER=$(curl -s -H "$TOK" -H "Idempotency-Key: $(uuidgen)" -X POST \
  localhost:8080/v1/versions -d '{}' | jq -r .version_id)

# propose a claim (gates run in the command path)
curl -s -H "$TOK" -H "Idempotency-Key: $(uuidgen)" -X POST \
  localhost:8080/v1/versions/$VER/claims \
  -d '{"claim_type":"fact","subject":"hero","key":"eyes","value":"green","expected_head":0}'

# read facts at head, then pinned
curl -s -H "$TOK" "localhost:8080/v1/versions/$VER/facts"
curl -s -H "$TOK" "localhost:8080/v1/versions/$VER/facts?as_of_seq=1"

# a conflicting plain claim comes back 200 with the claim recorded DISPUTED
# and gate findings in the response body — blocked, never dropped.
```

## Errors

`application/problem+json` everywhere — see [errors.md](errors.md) for the full registry
(problem type ↔ HTTP status ↔ gRPC code ↔ extension members, and the `gate.*` rule-id table).

## Client libraries

Official clients (Rust, Python, .NET, Java — REST **and** gRPC, conformance-proven against
this server) live under [clients/](../../../clients/README.md) with per-plane usage guides. Prefer
them over hand-rolled HTTP: they encode the write loop, idempotency, pins, and the typed
error registry for you.

Retry contract clients must honor: idempotency keys are recorded **after**
the command completes, so there is no in-flight reservation. A client that
re-sends a command whose request may already have been delivered can execute
it twice. The official clients retry commands only on connect-phase failures
and explicit load-shed; reads retry freely.

`PUT /v1/sources` accepts bodies up to 256 MiB (the handler buffers the body,
so the limit is the memory guard).

**`X-Filename` is required** on `PUT /v1/sources`, as is `filename` on
`POST /v1/ingest[/batch]` and `SourceHeader.filename` on the gRPC stream. The
filename is the source's *logical path*: its identity, its location in object
storage, and the string a runbook collection's `filenamePrefix` matches with a
literal `starts_with`. A source without one could never be bound to a
collection, so it is rejected rather than stored unreachable. Paths are
validated — traversal (`..`), absolute, drive-qualified, and backslash paths
are refused, because the path becomes an object-store key.

The response carries `source_id` (stable, derived from the path) alongside
`content_hash` (integrity of the bytes). `already_existed` is true only when
that path already held those exact bytes; re-uploading a path with new content
is an update and returns false, because a rebuild is then owed.

`GET /v1/sources/{source_id}` answers "did this land, and where?" — the
observability route for object storage. `storage_backend` names the
`MUNARIUM_SOURCE_STORE` backend the bytes went to; `blob_uri` is the recorded,
credential-free location. `extraction_status` is `null` until an index build
first touches the source, then `ok` (text extracted), `empty` (extraction ran
but produced no text — a scan with no text layer; calling it ok would hide
the miss), or `failed`; `extraction_method` records how (and is reset with the
status when the source's bytes change).

Wherever a request *supplies* a `content_hash` (ingest records, runbook
`contentHashes` bindings), it must be a full 64-character hex sha-256 digest —
anything else is `invalid-input`.
