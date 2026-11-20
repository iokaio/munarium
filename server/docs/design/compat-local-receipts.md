# ClearView local compatibility receipts (v1.4)

These receipts replay v1.4 behavior through the **published v1.4.0 image** and a
**local build of the ClearView branch**, on one Windows workstation with Docker.
Every receipt is `evidence_class: local`. Local evidence is separate from
`release-candidate-cluster` evidence. The candidate is a pre-release build of
`feat/clearview` at `4a67227`, not a frozen release candidate. A release receipt
must be collected again on the exact candidate. The release owner has not
recorded local receipts as an accepted environment of record. Under the
maintainer's decision of 20 November 2026, nothing here used paid cloud or
cluster resources.

The fields follow the [release evidence format](README.md#release-evidence) and
the [receipt skeleton](cluster-qualification-plan.md#receipt-format). Outcomes use
the [validation guide](../guides/validation.md#outcomes-and-receipts) vocabulary.
Known deltas are deliberate, documented behavior changes. They are recorded
here, not counted as failures. The baseline record is [P0-T1](v14-baseline.md).

## Summary

| Receipt | Outcome | Main result |
|---|---|---|
| [COMPAT-T1](#compat-t1) | passed (local) | 127 REST, 126 native-bridge and 29 typed gRPC cases matched v1.4.0. The only differences are two additive problem members and one problem-detail wording change. All 139 v1.4 bridge operations are served; none were removed. |
| [COMPAT-T2](#compat-t2) | passed (local), with gaps | The published 1.4.0 Rust, Python, .NET and Java clients passed against the candidate. The checkout clients passed against v1.4.0, apart from one catalog test that probes nine new operations. Twenty `mmctl` commands gave identical output and exit codes in all four CLI/server pairs. No Matrix peer or chart run was included. |
| [COMPAT-T3](#compat-t3) | failed, as expected (negative control) | v1.4.0 exits 1 against the candidate-migrated database: `migration 43 was previously applied but is missing`. The rollback route is still undecided. |
| [COMPAT-T4](#compat-t4) | passed (local) | Both binaries made identical provider arrivals (2 tags, 7 chat, 1 embed) and produced identical index IDs and hit order after a fresh build, including legacy PDF. No opt-in feature activated. |
| [COMPAT-T5](#compat-t5) | passed (local), with three known deltas | Bookmarks, login, cookie, CSRF refusal and view-only behaved the same on both binaries. Known deltas: revoke and approve POSTs now return a review page (U4), and the issue form moved to its own page (U8). |
| [A5-T2](#a5-t2) | passed (local) | With MCP off, the published v1.4 clients and stubs got the same native results, refusals and budget refusal from both binaries. `/mcp` returns 404 on both. |

No compatibility regression was found. The [findings](#findings-for-review)
section lists observations that need review.

## Harness

The harness is in [`server/tools/compat/`](../../tools/compat/). It uses the
Python standard library, except for `grpc_driver.py`, which runs inside a
virtual environment with the published `munarium-client` 1.4.0 wheel. That way
the gRPC calls go through the released v1.4 stubs, the old strict readers.

| File | Role |
|---|---|
| `compat_local.py` | Orchestrator. Steps: `up`, `reset`, `seed`, `fork`, `replay`, `diff`, `t4`, `mmctl`, `clients`, `console`, `t3` and `down`. Containers, the network and the volume use the `mcompat-` prefix. Server ports bind to `127.0.0.1` 28080/28051/28090 (baseline) and 29080/29051/29090 (candidate). |
| `compat_wire.py` | Case runner (templating, capture, polling, SSE parsing) and the comparator |
| `grpc_driver.py` | Native-bridge transport, which routes each REST case by its v1.4 `server_api.proto` comment; a typed `CommandService`/`QueryService`/`SessionService` replay; and a probe of the served catalog |
| `count_stub.py` | Counting provider stub (Ollama and OpenAI-compatible). It logs the method, path, size and client address of each arrival, never the body. |
| `released_clients.py` | Builds disposable conformance projects that depend on the **published** 1.4.0 packages, from an export of `clients-v1.4.0` |
| `cases/seed-v14.json`, `cases/replay-v14.json`, `cases/mmctl-*.yaml` | Seed and replay cases |
| `test_compat.py` | 31 unit tests, including negative controls for each comparison rule. Run with `py -m unittest discover -s server/tools/compat`. Server CI and `gates.ps1` run them as the `compat.regression` gate; the Docker-based replay stays local. |

**Same data for both binaries.** `seed` writes v1.4 state through the published
v1.4.0 binary on an empty database (`compat_base`). The seed covers a ledger with
a correction, a promise, an anchor and a counter; two shapes; an Ollama provider
configuration pointing at the stub; and a runbook with a text collection and a
PDF collection. It then ingests documents and runs, approves and completes the
build. Finally it creates a session and a turn, and issues capability tokens.
`fork` stops v1.4.0 and copies the database with `CREATE DATABASE compat_cand
TEMPLATE compat_base`. The candidate then upgrades `compat_cand` in place, so
both binaries start from identical v1.4-written rows and IDs.

**Comparison rules.** Status and media type must match exactly. JSON is compared
member by member. A member only in the candidate is *added*; this is reported
and tolerated, as the [wire policy](../wire-compatibility.md#unknown-field-and-presence-policy-by-direction)
allows. A member only in the baseline is *removed* and fails. A change of JSON
kind fails, so absent, `null`, `[]` and `{}` stay distinct. A changed string
fails, including a new enum value. Seeded IDs must be byte-identical. Every other
generated ID (`memv-`, `claim-`, `ses-`, `tok-`, `run-`, `idx-`, UUIDs) is bound to
its counterpart on first sight and must keep that one-to-one binding for the
rest of the transcript. Timestamps and JWTs must keep their format. Numbers under
latency-style keys are volatile. Problem `detail` and gRPC `details` text is
informational. Cases marked `exact` must match byte for byte. Concurrent
per-collection `retrieval` progress events are sorted within their run before
comparison, because v1.4.0 itself emits them in completion order. The rehearsal
showed two v1.4.0 runs on identical data ordering them differently. All other
SSE events keep their order.

**Reproduce.** From `server/`, after building the candidate with
`docker build -t <tag> .` and preparing the client environments (below):

```powershell
$env:MUNARIUM_COMPAT_CANDIDATE = '<tag>'
foreach ($s in 'up','seed','fork','replay','t4','mmctl','clients','console','t3','down') { py tools/compat/compat_local.py $s }
```

The `replay` step needs a Python with `munarium-client==1.4.0` from PyPI
(`--client-python`) and the v1.4.0 `server_api.proto`. Export that file with
`git archive 25012c22087fbaa8ce29ddd68d2880d6e9499b1c clients server/proto`
(`--v14-proto`). The `clients` step expects the projects from
`released_clients.py`, a second virtual environment with the checkout's Python
client, and built checkout suites. Each run writes its transcripts, diffs and
logs to the ignored `server/scratch/compat/<prefix>/`.

## Shared run record

```yaml
receipt_format: clearview-cluster-1        # skeleton reused; see evidence_class
environment: local-docker                  # one workstation, plain Docker containers, no Kubernetes
evidence_class: local
window: local-2026-11-20 (13:26–13:37 UTC; a rehearsal run preceded it)
candidate:
  server_image: sha256:6122154cc647d4f187a3796e317fe9d298d79013646f77bd65aeb942e2569d4b   # local image ID, linux/amd64
  source: feat/clearview 4a67227 (worktree clearview/compat), `docker build` of server/Dockerfile
  build_args: {SOURCE_REVISION: 4a67227, BUILD_VERSION: 1.5.0-compat-local}   # label only; /version still reports the 1.4.0 workspace version
  chart_version: not used
baseline:
  server_image: docker.io/iokaio/munarium@sha256:800a7b19cd3e2b82a3fb8f0b33e86e3f21fc3ea71ac12a81d6a9a18a816f556a
  local_image_id: sha256:362fa72696e5489020dd55293e0d9473eb74e643be395a775f45bdfd0ba50265   # amd64; label revision 25012c2
  signature: not verified locally (cosign is not installed on the workstation)
  clients:
    rust: munarium-client 1.4.0 (crates.io checksum 5a8550d9…5755), munarium-api-types 1.4.0, munarium-proto 1.4.0
    python: munarium-client 1.4.0 wheel sha256 cbb229bb…084f
    dotnet: Ioka.Munarium.Client 1.4.0 (NuGet sha512 Hplyr3MZ…VsQ==)
    java: io.ioka.munarium:munarium-client 1.4.0 (jar sha256 0e3338d9…67f3)
configuration:                             # non-secret; tokens are public test values from compat_local.py
  both: MUNARIUM_STORE=postgres, MUNARIUM_SOURCE_STORE=pg, MUNARIUM_AUTH_MODE=static,
        MUNARIUM_STATIC_TOKENS=<test tokens, tenants compat, compat-other, conformance>,
        MUNARIUM_TOKEN_SECRET=<test value>, MUNARIUM_LOG=info, MUNARIUM_INSTANCE_ID=compat-base|compat-cand
  new ClearView settings: none set
topology: {nodes: 1, replicas: 1 per binary, db: pgvector/pgvector:pg16@sha256:ccc6e83d…4d6b (two databases), object_store: none (pg source store), provider: count_stub on python:3.12-alpine}
host: Windows 11 Pro, Docker Desktop 29.8.0 (WSL2 kernel 5.15.167.4, 8 CPUs, 16 GB to the VM)
fixture_revision: the commit that adds this page (server/tools/compat/**)
owner: <release owner role — assign privately>
evidence_location: ignored local path server/scratch/compat/mcompat/ on the collecting workstation (transcripts, diffs, logs; contain test tokens, not shared)
```

<a id="compat-t1"></a>

## COMPAT-T1 · REST, typed gRPC, native bridge and SSE

```yaml
acceptance_id: COMPAT-T1
outcome: passed
expected:
  - Paths, methods, protobuf tags, DTO types, errors, pagination and terminal events unchanged for v1.4 operations
  - Omitted options keep v1.4 meaning; new members only optional
  - Absent/null/empty distinctions and strict enum readers preserved
observed:
  rest: 127 cases (87 success, 32 refusal, 5 conflict, 3 streaming; 5 exact-byte) — 0 failing, 4 added, 1 informational
  native_bridge: 126 cases (the REST set less one unroutable unknown-path case) — 0 failing, 4 added, 1 informational
  typed_grpc: 29 calls through the published 1.4.0 stubs (6 refusal, 3 conflict) — 0 failing, 2 added
  openapi_surface: 139 v1.4 operations all present with unchanged response codes; 9 added
  served_catalog: all 139 v1.4 bridge RPCs served by both binaries; identical status-code mix for the invalid-credential probe
  added_members:
    - head-conflict problem (REST, bridge ErrorInfo metadata, typed ErrorInfo metadata): retryable=false, retry_action=reread (R11)
    - provider tpm budget 429 problem (REST, bridge): retryable=false, retry_action=retry_later (R11)
  informational:
    - invalid-scope 400 detail now lists the new scopes `mcp` and `evidence_read`
coverage_gaps:
  - Exact-byte checks cover healthz and the composed-context bodies only; other bodies compare by meaning
  - Typed gRPC replay covers the ledger, query and session services, not every typed service
  - One replica per binary; mixed replicas are COMPAT-T3/cluster work
  - Matrix-enabled installation not exercised (no Matrix peer)
evidence_location: replay-{rest,bridge,typed}-{base,cand}.json, replay-*-diff.json, openapi-surface.json, catalog-{base,cand}.json
```

The cases cover the following. Health and version. Ledger reads, including a
point-in-time pin, a status filter, a scope prefix, a superseded claim, the
composed context under a budget and a pin, and an unimplemented `as_of_date`.
Writes: a child version; a claim; an idempotent replay; an idempotency mismatch
(422); a head conflict (409); an expected-head success; a gate-disputed claim; an
unknown enum, a missing field and malformed JSON (400); explicit `null` members;
an unknown request member; the `ro` role (403); a missing token and an invalid
token (401); a missing uid and a missing idempotency key (400); a promise, a
counter, a `u64::MAX` counter (500 `storage-error` on both) and an anchor.
Retrieval: `/v1.2/search` by collection, with a pinned index; `/v1.2/query`;
refusals. Sessions: turns with `complete` false, omitted and true (the stub
model); `top_k`; streamed turns with `done` and with a streamed `error` event;
an unknown research profile; capability-token turns and a uid mismatch; close
twice; a turn after close (409). Runbooks and runs, approval of a completed
step, validation, shape re-apply with the same and changed content, token
issue and revocation, providers (list, health, complete, embed), a provider
`tpm` budget refusal (429), ingest of text, existing and PDF sources, a fresh
build with two approvals, mgmt reports and a role refusal, configuration reads
and the job list.

<a id="compat-t2"></a>

## COMPAT-T2 · CLI, SDKs and configuration

```yaml
acceptance_id: COMPAT-T2
outcome: passed (local), with coverage gaps
expected:
  - Released 1.4 clients and CLI keep their effect against the candidate
  - Candidate clients' 1.4 operations work against v1.4.0
  - Existing Matrix command output and exit codes unchanged
observed: see the client table and the mmctl table below
coverage_gaps:
  - Helm: the v1.4 chart upgrade with original values is covered by scripts/test_helm_chart.py (8 cases, legacy render byte-compared with v1.4.0; passed locally on 20 November 2026). No chart was installed; replica-count migration and rollback need L2/L3 (SCALE-1-T1).
  - Matrix: no Matrix peer. `mmctl matrix` verbs were run only against an absent peer; /v1/reports/matrix and /admin/matrix were checked unconfigured. Matrix-enabled runs wait for M1 step 3 and a pinned peer image.
  - mmp-conformance (the Server's own black-box suite) was not run in this window; the four SDK suites carry the same seven wire scenarios.
  - Python conformance skips 13 tests (the kernel-only chronology scenario and the Server 1.1.1 Ollama qualification), .NET skips 2, Java skips 1, as in CI.
evidence_location: t2-clients.json, t2-*.log, catalog-*.json, mmctl-matrix.json, mmctl-diff.json
```

| Suite (both transports) | Published 1.4.0 → candidate | Published 1.4.0 → v1.4.0 (control) | Checkout → v1.4.0 | Checkout → candidate (control) |
|---|---|---|---|---|
| Rust (`munarium-client-conformance --smoke`) | passed: scenarios, plane smokes, platform smokes, 0 failed | passed | passed | passed |
| Python (`pytest conformance`, sync and async) | 45 passed, 13 skipped | 45 passed, 13 skipped | 44 passed, **1 failed**, 13 skipped: `test_every_documented_operation_has_a_served_rpc` stops at `GetInference` (UNIMPLEMENTED) | 45 passed, 13 skipped |
| .NET (`dotnet test` conformance) | 18 passed, 2 skipped | 18 passed, 2 skipped | 18 passed, 2 skipped | 18 passed, 2 skipped |
| Java (`conformanceTest`) | 29 tests, 0 failed, 1 skipped | 29, 0, 1 | 29, 0, 1 | 29, 0, 1 |

The single checkout-to-v1.4.0 failure is outside the v1.4 operation set. The
checkout catalog (`clients/server-api.json`, 148 operations) adds nine
operations that v1.4.0 does not have: `GetInference`, `PutInference`,
`EnrollInference`, `RollbackInference`, `ProbeProvider`, `ListJobEvents`,
`GetJobProgress`, `ListJobVersions` and `ScaleReport`. The harness's catalog probe
found that v1.4.0 returns UNIMPLEMENTED for exactly these nine and serves all 139
v1.4 operations. The candidate serves all 148. For the 139 v1.4 operations, the
invalid-credential probe produced the same status mix on both binaries (43
INVALID_ARGUMENT, 1 PERMISSION_DENIED, 91 UNAUTHENTICATED).

The released projects are generated by `released_clients.py`. Each takes the
`clients-v1.4.0` conformance sources unchanged and swaps only the client
dependency for the published package. For Java, the suite also needs
`io.grpc:grpc-api` at compile time, because the published POM scopes gRPC as
runtime while `ServerApiClient(options, ManagedChannel)` is public API (see
[findings](#findings-for-review)).

**`mmctl`.** Twenty commands were run in each of four pairs: the v1.4.0 CLI
against v1.4.0 and against the candidate, and the candidate CLI against both.
The candidate CLI still reports `mmctl 1.4.0`. stdout (as JSON, compared by
meaning) and exit codes were identical in every pair.

| Command | Exit (all four pairs) |
|---|---|
| no arguments; unknown verb | 2 |
| `runbook list`, `runbook info starter`, `runbook info starter@1`, `get run`, `runbook validate -f`, `apply -f` (shape), `token issue` (mgmt), `datastore jobs list`, `author patterns` | 0 |
| `runbook info` missing (404), `apply -f` unknown kind, `token issue` with rw (403), `token issue` with a non-integer level, `datastore rollout get` with no entry (404), `bulk status` missing (404), no token (401) | 1 |
| `matrix version`, `matrix journal --limit 1` with no Matrix peer | 1 |

<a id="compat-t3"></a>

## COMPAT-T3 · Binary rollback (negative control)

```yaml
acceptance_id: COMPAT-T3
outcome: failed            # expected; recorded, not worked around
not_run_reason: null
expected (current branch): v1.4.0 refuses to start against the upgraded database (sqlx VersionMissing), pending the maintainer's rollback decision
observed:
  procedure: stop the candidate; start the published v1.4.0 image against compat_cand (migrated by the candidate to 0043–0047, 0050, 0060, 0070, 0080)
  container_state: exited, ExitCode 1, after one startup attempt
  log: "startup error: storage error: migration 43 was previously applied but is missing in the resolved migrations"
  migrations_in_db_after: unchanged (1–47, 50, 60, 70, 80); the failed start wrote nothing
  candidate_restart: the candidate started again on the same database and served
coverage_gaps:
  - Mixed replicas, backup-restore (route B) and a v1.4.x with ignore_missing (route A) were not exercised; the route is undecided
  - Sealed evidence, a pinned session read after rollback and the memory-store surface were not exercised
evidence_location: t3.json
```

Rollback boundaries for each migration are in the [baseline record](v14-baseline.md#rollback-boundaries-by-migration).

<a id="compat-t4"></a>

## COMPAT-T4 · Optional work and cost

```yaml
acceptance_id: COMPAT-T4
outcome: passed (local)
expected:
  - Installing the candidate with no new settings enables no MCP, inference, isolated PDF extraction, artifact damage handling, console sessions, fair scheduling or admission reserve
  - Existing provider configurations keep the same outbound calls
  - Old index identities and ordering preserved for local-hash retrieval and legacy PDF ingestion
observed:
  outbound_calls (count stub, by server address, across the REST, bridge and typed replays):
    v1.4.0:    {GET /api/tags: 2, POST /api/chat: 7, POST /api/embed: 1}
    candidate: {GET /api/tags: 2, POST /api/chat: 7, POST /api/embed: 1}
    during seed: 0 (provider apply makes no call)
  provider_metrics: munarium_provider_calls_total complete=7 embed=1, tokens in=70 out=14 on both
  index_identity:
    seeded indexes (read by both): identical
    fresh build in the replay (new text and PDF sources, run, two approvals): identical content-derived IDs on both binaries (starter-documents idx-46b8295f007e03f6, starter-pdfs idx-6898a4283a038a51 in this run)
    hit order and scores: identical for 8 retrieval reads, including a search pinned to the old index after the rebuild and the PDF collection
  opt_in_features:
    mcp: /mcp, POST /mcp and /.well-known/mcp return 404 on both; mcp_* tables empty
    inference: inference_* and collection_inference_* tables empty; no probe; no inference metrics
    isolated_pdf: no startup line; the PDF source was extracted in-process (default inline) and indexed identically
    artifact_damage: artifact_* tables empty; no "damage handling enabled" log line
    console_sessions: console_sessions empty after console login; the legacy cookie was used
    fair_scheduling: with no builder configured, an enqueued backfill stayed pending on both; the candidate's insert trigger added tenant `compat` to index_build_tenant_turns with last_served_seq NULL (never served)
    admission_reserve: only the `ordinary` pool exists (512 slots per plane); control-class requests used ordinary slots
  new_tables: 26 tables from 0043–0080, all empty except index_build_job_event_retention (1 row seeded by migration 0046)
  new_metric_families (candidate only, additive): munarium_admission_*, munarium_db_pool_max_connections, munarium_index_build_*, munarium_replica_count
coverage_gaps:
  - Egress is counted only at the configured provider endpoint. The network was a plain bridge, so a call to an unconfigured host would not be counted; logs showed no connection errors on either binary.
  - No paid provider, Document Intelligence (paid OCR) or Matrix configuration was exercised
  - No builder was configured, so the default FIFO claim was not exercised with queued work; the Phase 1 fixture tests cover it
evidence_location: t4.json, stub/arrivals.jsonl, replay-rest-*.json
```

<a id="compat-t5"></a>

## COMPAT-T5 · Operator interface

```yaml
acceptance_id: COMPAT-T5
outcome: passed (local), with three known deltas
expected:
  - Existing console URLs and bookmarks, login, CSRF and view-only mode keep working
  - Known deltas (U4): the v1.4 POSTs for token revoke and step approve now return a review page
observed:
  bookmarks: 20 v1.4 pages (overview, traffic, endpoints, usage, health, runbooks, runbook, shape, run, providers, collections, collection, storage, sessions, session, tokens, audit, findings, matrix, login) return 200 with the same titles on both
  anonymous: /admin and /admin/tokens → 303 to /admin/login on both
  rw_bearer_on_console: 303 on both
  login: invalid token and rw token re-render the login page (200); mgmt token → 303 /admin and sets the same cookie name (__munarium_admin) on both; cookie-only browsing works on both
  csrf: token issue without CSRF or with a wrong CSRF re-renders without minting on both; with the page's CSRF, both mint a JWT
  view_only: the x-munarium-admin-view-only header shows the view-only badge and no CSRF field or action form on both
  supported_automation: REST revoke of the same token succeeds on both (unchanged)
known_deltas:
  - U4 token revoke: v1.4.0 POST /admin/tokens/{jti}/revoke → 303 and revokes; the candidate → 200 review page and does not revoke; POST …/revoke/confirm with the typed jti then revokes (303)
  - U4 step approve: v1.4.0 POST /admin/runs/{id}/steps/{n}/approve → 303 and approves; the candidate → 200 review page and the step stays awaiting_approval; POST …/approve/confirm with the typed run ID and rw token then approves (303). On a completed step the candidate also shows a review page (v1.4.0 re-renders the run page); the step stays done on both
  - U8 token issue: the issue form moved from /admin/tokens to its own page, GET /admin/tokens/issue (200, new); POST /admin/tokens/issue with CSRF mints exactly as in v1.4
coverage_gaps:
  - Mixed-replica sessions and the login cutover (LOGIN1) are cluster work; MUNARIUM_CONSOLE_SESSIONS stayed at its legacy default
  - Keyboard flows and visual layout were not checked; the checks are HTTP-level
evidence_location: console.json, console-diff.json
```

<a id="a5-t2"></a>

## A5-T2 · Native baseline with MCP disabled

```yaml
acceptance_id: A5-T2
outcome: passed (local)
expected: With MCP off, published v1.4 clients get the same native results, refusals and budget limits over REST and gRPC from both binaries
observed:
  mcp_state: MUNARIUM_MCP unset on the candidate; /mcp 404 on both; no mcp_* rows
  native_replay: the COMPAT-T1 REST, native-bridge and typed-gRPC replays (published 1.4.0 Python stubs) — 0 failing differences; refusals and the provider tpm budget refusal (429 rate-limited) matched, with only the additive R11 retry members
  sdk_suites: published 1.4.0 Rust, Python, .NET and Java suites passed on both binaries (COMPAT-T2)
  state_transitions: session open → closed → 409 on a further turn; run awaiting_approval → done; token revoke; identical on both
  volatile_fields: generated IDs (bound one to one), timestamps, JWTs and latency numbers; comparison fixture cases/replay-v14.json
coverage_gaps:
  - Budget coverage is the provider tpm reservation refusal; per-request token allowances and MCP spending are MCP-only (A8) and out of scope with MCP off
  - Pinned indexes: covered by a search pinned to the pre-rebuild index; runbook version pins by session creation at starter@1
evidence_location: replay-*.json, t2-clients.json
```

## Findings for review

None of these is a compatibility regression. Each is recorded for a reviewer.

1. **Candidate `/version` still reports `1.4.0`** (and `mmctl 1.4.0`), because
   the workspace version has not been bumped. A v1.5 client cannot distinguish
   the binaries by version yet. This must change before release, and the change
   will itself alter the `/version` body.
2. **New R11 problem members on existing refusals.** The head conflict carries
   `retryable: false, retry_action: "reread"`. The provider `tpm` 429 carries
   `retryable: false, retry_action: "retry_later"`. Both are additive and
   tolerated by the wire policy. The 429 pairing of `retryable: false` with
   `retry_later` deserves a semantic check against the R11 design.
3. **Invalid-scope detail text** now lists `mcp` and `evidence_read`. Only the
   wording changed; the slug and status are unchanged.
4. **Java 1.4.0 POM scope.** `ServerApiClient(ManagedChannel)` is public, but the
   published POM puts gRPC in runtime scope. A consumer calling that overload
   needs its own `io.grpc:grpc-api` compile dependency. This is a v1.4.0
   packaging property, not a candidate change.
5. **Concurrent SSE progress order.** v1.4.0 emits per-collection `retrieval`
   progress in completion order, so strict ordering is not a v1.4 promise. The
   harness compares those runs as multisets.
6. **`u64::MAX` counter** returns 500 `storage-error` on both binaries, as the
   [wire inventory](../wire-compatibility.md) documents.

## What still needs a cluster or the release candidate

- Every receipt above must be repeated on the **frozen release candidate**, by
  digest, and accepted by the release owner.
- **COMPAT-T2:** chart install, upgrade with `--reuse-values`, replica-count
  migration and rollback (L2/L3), plus Matrix-enabled runs with a pinned peer
  after M1 step 3.
- **COMPAT-T3:** the rollback route is a pre-upgrade backup restore (route B,
  maintainer decision of 20 November 2026). Sealed evidence and pinned sessions
  read after a drilled restore into v1.4.0, with the `VersionMissing` negative
  control.
- **COMPAT-T5:** mixed-replica console sessions and the LOGIN1 cutover.
- **A5-T3** and the other multi-replica MCP cases.

Cosign verification of the baseline is not required (maintainer decision of
20 November 2026).
