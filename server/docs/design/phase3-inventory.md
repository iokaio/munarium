# Phase 3 inventory: admission routes, permits and extractors

Recorded **19 November 2026** for [Phase 3](README.md#phase-3-reserve-control-capacity-and-contain-pdf-work)
of the ClearView plan. It is the entry inventory for [R7/B3](airk-analysis.md#r7) and
[R3/B4](airk-analysis.md#r3). This page states what the code does now and does not
classify any route. The proposed classes in the candidate tables still need B3-T2
review.

**Revision.** These are facts about the `feat/clearview` branch based on `0791d06`.
The admission, routing and extraction sources are unchanged from v1.4.0
(`25012c2`), so the facts also describe v1.4.

**Method.** The REST table was extracted mechanically from every `.route(...)`
call in `server/src/munarium-server/src`, excluding test modules. Each handler's
body was scanned for its first authentication or access call, its `require_*`
checks and its `SCOPE_*` constants. The method set was cross-checked against
[`openapi.json`](../api/openapi.json). All 139 OpenAPI operations are registered,
and the 29 other registered operations are the console, the ops port and unversioned
pages. The authorization column records only what the handler calls directly. A
handler that delegates authorization to a helper shows that helper's name, and the
helper's own checks need review for B3-T2.

## Admission today

| Plane | Pool and size | Applies to | Behavior at the ceiling |
|---|---|---|---|
| REST (main port) | `rest_permits`, `MUNARIUM_MAX_CONCURRENCY` per instance (default 512, minimum 1) | Every path starting `/v1/` or `/v1.2/`, taken before authentication ([`middleware.rs`](../../src/munarium-server/src/middleware.rs)) | `try_acquire_owned` fails: HTTP 503 `overloaded`, `Retry-After: 1`, `munarium_load_shed_total` incremented. No queueing. |
| REST platform routes | Same REST permit, then mTLS peer admission. Activation also takes `activation_permits` inside the handler. | `/v1/platform/{tenant}/...` (4 operations) | As REST. Activation returns unavailable when its own permit is exhausted. |
| Typed gRPC | `grpc_permits`, the same `MUNARIUM_MAX_CONCURRENCY` | `mmp.v1.*` services other than `ServerApiService` | `grpc_reject(overloaded)`. Health and reflection pass without a permit. |
| Native gRPC bridge | **REST permit**: `ServerApiService` replays each call through `rest::router` in process ([`grpc_api.rs`](../../src/munarium-server/src/grpc_api.rs)) | 139 generated methods mirroring the REST operations | The gRPC capture layer skips `ServerApiService`, so a bridged call is charged one REST permit and no gRPC permit. |
| Ops port | None | `/healthz`, `/readyz`, `/metrics` ([`ops.rs`](../../src/munarium-server/src/ops.rs)) | Never shed. |
| Console and unversioned pages | None | `/admin/*` (25 operations), `/healthz`, `/readyz`, `/healthai`, `/version`, `/openapi.json`, `/docs` on the main port | Never shed by admission. |
| Background builder | None | `MUNARIUM_DATASTORE_BUILDER=enabled` job loop ([`datastore_jobs.rs`](../../src/munarium-server/src/datastore_jobs.rs)) | Polls every `MUNARIUM_DATASTORE_BUILDER_POLL_MS` (default 5000). Claims one job at a time. |

Two facts matter for B3. First, a console action such as `/admin/tokens/{jti}/revoke`
is never shed today, while its REST equivalent is. Second, the native bridge already
charges a nested dispatch once, so the "charge once" rule holds for it. MCP
dispatch does not exist yet.

**R7 implemented.** [`admission.rs`](../../src/munarium-server/src/admission.rs)
implements the [B3-T2 classification](#b3-t2-classification-proposed) as written,
behind two opt-in settings that default to off: `MUNARIUM_CONTROL_RESERVE` and
`MUNARIUM_HEAVY_CONCURRENCY` ([admission classes](../ops/admission.md)). With
neither set, the table above still describes the behavior; the `rest_permits` and
`grpc_permits` pools are now each plane's ordinary pool in `AppState::admission`. The reserve is carved
out of `MUNARIUM_MAX_CONCURRENCY`, is granted only after the control route's own
authentication and role check, and is never used by heavy work. Bridged calls take
their REST route's class and are still charged once. There is no live
reconfiguration. The console findings below remain follow-ups.

## Typed gRPC services

| Service | Methods | Pool |
|---|---|---|
| `AdminService` | CreateTenant, ListTenants, Usage, IssueAccessToken, ListAccessTokens, RevokeAccessToken | gRPC permit |
| `CommandService` | CreateVersion, ProposeClaim, AppendEvents, OpenPromise, FulfillPromise, LockAnchor, RecordCounts, UpsertDigest | gRPC permit |
| `IngestService` | PutSource, RecordIngest, IngestFiles | gRPC permit |
| `ProviderService` | ApplyProviderConfig, ProviderHealth, Complete, Embed | gRPC permit |
| `QueryService` | GetHead, GetClaim, SliceFacts, GetLineage, ListAnchors, ListPromises, ComposeContext, CounterTotals, ListDigests | gRPC permit |
| `RetrievalService` | HybridSearch, GetIndexVersion, CreateCollection, ListCollections, GetCollection | gRPC permit |
| `RunbookService` | ApplyShape, ApplyRunbook, RunRunbook, GetRun, ApproveStep, ListRunbooks, GetRunbookInfo, ValidateRunbook, RequestRemoval, ConfirmRemoval | gRPC permit |
| `SessionService` | CreateSession, Turn, GetSession, CloseSession | gRPC permit |
| `ServerApiService` | 139 methods, generated from the REST operations | REST permit (see above) |

## Candidate control operations

These are the operator levers R7 names, plus their equivalents. Classification is
for B3-T2 review.

| Operation | REST route and authorization | Other surfaces |
|---|---|---|
| Change retrieval rollout | `PUT /v1/retrieval-rollout`: `auth_ctx()`, `require_rw()` | Bridge only |
| Cancel a build job | `POST /v1/index-build-jobs/{job_id}/cancel`: `auth_ctx()`, `require_rw()` | Bridge only |
| Promote an artifact binding | `POST /v1/index-artifacts/{index_version_id}/promote`: `auth_ctx()`, `require_rw()` | Bridge only |
| Revoke an access token | `POST /v1/access-tokens/{jti}/revoke`: `auth_ctx()`, `require_mgmt()` | `AdminService.RevokeAccessToken`; console `POST /admin/tokens/{jti}/revoke` (not admitted) |
| Approve a run step | `POST /v1/runs/{run_id}/steps/{ordinal}/approve`: `rest_auth()`, `require_rw()` | `RunbookService.ApproveStep`; console `POST /admin/runs/{run_id}/steps/{ordinal}/approve` (not admitted) |

## Candidate heavy requests

Each of these holds one ordinary permit for the whole request today.

| Route | Why it is heavy | Evidence |
|---|---|---|
| `POST /v1/indexes/{shape_ref}/build` | Synchronous index build: extraction of every source, chunking and embedding | `rest.rs` calls `build_index(..., true)` |
| `POST /v1/runbooks/{name}/runs` | Index steps build synchronously; model and provider calls | `runbooks_api.rs` calls `build_collection_index` and `build_index` |
| `POST /v1/runs/{run_id}/steps/{ordinal}/approve` | Resumes the run inline after approval, so it can execute later steps, including builds | `op_approve_step` calls `execute(...)` under the run lock |
| `POST /v1.2/collections/{id}/vocabulary/refresh` | Samples sources through extraction | `vocabulary_api.rs` calls `source_sample` |
| `POST /v1/index-artifacts/{index_version_id}/rebuild`, `/verify`, `POST /v1/index-artifacts/backfill` | Synchronous mirror, verification and backfill. Verify opens artifacts on the blocking pool. | `datastore_builds.rs` `op_rebuild_artifact`, `op_verify_artifacts`, `op_backfill_collection` |
| `POST /v1/sessions/{id}/turns`, `/turns/stream`, `POST /v1.2/answers` | Model generation; streaming holds the permit for the stream's life | `sessions_api.rs`, `answers_api.rs` |
| `POST /v1/providers/{name}/complete`, `/embed` | Direct provider calls | `providers_api.rs` |
| `PUT /v1/sources`, `POST /v1/ingest`, `/ingest/batch`, `/ingest/bulk`, `/ingest/bulk/{bulk_id}/chunk`, `PUT /v1/evidence/{evidence_id}/bytes` | Request bodies up to 256 MiB (`MAX_SOURCE_BYTES`) | Route body-limit layers in `rest.rs` |

## PDF and document extractors

| Item | Current fact |
|---|---|
| Extractor-set identity | `extract@1[docx@1,pdf-text@1]` from `ExtractorRegistry::new()`. It joins the index identity, so a change forces a rebuild ([`lib.rs`](../../src/munarium-extract/src/lib.rs)). |
| `pdf-text@1` | `pdf_extract::extract_text_from_mem` (pdf-extract 0.12 over lopdf), in process, wrapped in `catch_unwind`. No time, memory or output bound. ([`pdf.rs`](../../src/munarium-extract/src/pdf.rs)) |
| `docx@1` | In process. Inflated document XML bounded at 128 MiB, checked on the declared and the actual size. ([`docx.rs`](../../src/munarium-extract/src/docx.rs)) |
| Plain text | `text/*` and five structured types are decoded lossily without an extractor id. |
| `ocr@1` | Compiled only with the `ocr` feature and not registered by `ExtractorRegistry::new()`. `DEFAULT_MAX_PAGES` is 50. |
| Scheduling | `extract_source` moves local extraction to `spawn_blocking`, which cannot be cancelled ([`munarium-retrieval-pg/src/lib.rs`](../../src/munarium-retrieval-pg/src/lib.rs)). A panic becomes an `empty` result. |
| Paid escalation | Azure Document Intelligence runs only when `MUNARIUM_DOCINTEL=azure` (default `none`) and local extraction did not return `ok`. A provider failure keeps the local result. |
| Input ceiling | 256 MiB per source (`MAX_SOURCE_BYTES`). |
| Call sites | `build_index` (shape builds), `build_collection_index` (collection builds), `extract_collection_prepared` (direct builds, including queued `direct` jobs) and `source_sample` (vocabulary refresh). |

B4 adds a new extractor identity for the opt-in child-process profile and leaves
`pdf-text@1` and the set identity above available to v1.4 rebuilds.

**B4 update (19 November 2026).** The rows above remain the default
(`MUNARIUM_PDF_EXTRACTION=inline`). The opt-in `isolated` profile replaces
`pdf-text@1` with `pdf-text-isolated@1{...}` and runs it in a child of the
server binary with OS memory and CPU limits, a deadline, an output cap and a
per-process admission cap; `extract_source` cancels the child when its build is
dropped, and over-budget, deferred and cancelled outcomes get their own
`extraction_status`. Limits are provisional
([guide](../guides/document-intelligence.md#isolated-pdf-extraction),
[measurements](pdf-isolation-measurements.md)).

## B3-T2 classification (proposed)

Recorded 19 November 2026 for review. The classes are proposals that R7
implements. This section does not change any behavior.

**Rules.**

1. Reserved capacity is granted only by an explicit allowlist keyed on method
   and route template. A route that is not on the allowlist, including any route
   added later, is ordinary and can never take a reserved slot.
2. Today the REST permit is taken before authentication. A reserve slot can be
   granted only after the route's own authentication and role check pass. R7
   therefore tries an ordinary slot first. Only when none is free does it
   authenticate, check the role and take a reserved slot.
3. Heavy routes count against the same plane ceiling and never use the reserve.
4. The native bridge reaches these routes through the REST router, so a bridged
   call gets the same class as its REST route and is charged once.

**Control: eligible for the reserve after authentication and role check.**

| Operation | Authorization verified at helper level |
|---|---|
| `PUT /v1/retrieval-rollout` | `auth_ctx` → `AppState::authenticate`; `TenantCtx::require_rw` accepts only role `rw` |
| `POST /v1/index-build-jobs/{job_id}/cancel` | `auth_ctx`; `require_rw` |
| `POST /v1/index-artifacts/{index_version_id}/promote` | `auth_ctx`; `require_rw` |
| `POST /v1/access-tokens/{jti}/revoke` and `AdminService.RevokeAccessToken` | `auth_ctx`; `require_mgmt` accepts role `mgmt`, or any caller when `MUNARIUM_AUTH_MODE=disabled` |
| Status reads: `GET /v1/index-build-jobs/{job_id}`, `GET /v1/retrieval-rollout/{scope_kind}/{scope_id}`, `GET /v1/index-artifacts/{index_version_id}` | `auth_ctx` (any authenticated role). Proposed so an operator can see the effect of a control action under saturation. |

**Heavy: bounded within the ordinary ceiling.**

These are the routes in [Candidate heavy requests](#candidate-heavy-requests),
with one change from the candidate list. `POST /v1/runs/{run_id}/steps/{ordinal}/approve` and
`RunbookService.ApproveStep` are **heavy, not control**, because approval runs the
remaining steps inline. A split into approve-only and resume would let approval
become control; that is a design choice for R7. The typed gRPC heavy methods are
`IngestService.PutSource` and `IngestFiles`, `ProviderService.Complete` and
`Embed`, `RunbookService.RunRunbook` and `ApproveStep`, and `SessionService.Turn`.

**Ordinary.** Every other `/v1/` and `/v1.2/` operation and every other typed gRPC
method.

**Unadmitted.** Console, ops-port and unversioned routes stay outside admission.

**Finding for R7.** The console action `POST /admin/runs/{run_id}/steps/{ordinal}/approve`
runs the same inline `execute` as the REST approval, with no admission permit. A
console approval can therefore start heavy work at any load. R7 should either
admit console actions as heavy or have the console enqueue the resume.

## Limits and follow-ups

- Admission columns are derived from path prefixes in the middleware. A route
  added outside `/v1/` or `/v1.2/` is unadmitted by construction, so B3-T2 should
  check new routes against this rule.
- The heavy-request list comes from traced call sites, not from measurement.
  B3's reserve sizes and the MCP concurrency default remain open items in the plan.
- The bridge's single charge comes from reading `GrpcCaptureLayer` and
  `ServerApiSvc::dispatch`. No test exercises it under saturation yet.

## Registered REST, console and ops operations

| Method | Path | Admission | Handler | Authorization entry in handler | Notes |
|---|---|---|---|---|---|
| GET | `/healthz` | ops port; none | `healthz` | none |  |
| GET | `/readyz` | ops port; none | `readyz` | none |  |
| GET | `/metrics` | ops port; none | `metrics` | none |  |
| POST | `/v1/platform/{tenant}/activation` | REST permit; mTLS | `platform_activation::platform_activation` | mTLS peer | route body limit |
| POST | `/v1/platform/{tenant}/records` | REST permit; mTLS | `platform_records::platform_records` | mTLS peer | route body limit |
| GET | `/v1/platform/{tenant}/authority` | REST permit; mTLS | `platform_api::get_platform_authority` | mTLS peer; get_platform_authority() |  |
| POST | `/v1/platform/{tenant}/authority` | REST permit; mTLS | `platform_api::transition_platform_authority` | mTLS peer; transition_platform_authority() |  |
| GET | `/healthz` | none | `healthz` | none |  |
| GET | `/readyz` | none | `readyz` | none |  |
| GET | `/healthai` | none | `providers_api::healthai` | rest_auth(), authenticate(); require_mgmt(), require_rw_pub() |  |
| GET | `/v1/command-recovery` | REST permit | `command_recovery::policy` | auth_ctx(); require_mgmt() |  |
| POST | `/v1/command-recovery` | REST permit | `command_recovery::enable` | auth_ctx(); require_mgmt() |  |
| GET | `/v1/command-recovery/receipt` | REST permit | `command_recovery::receipt` | auth_ctx(); require_mgmt() |  |
| GET | `/v1/source-retention` | REST permit | `source_retention_api::list_source_retention` | auth_ctx(); require_mgmt() |  |
| POST | `/v1/source-retention` | REST permit | `source_retention_api::change_source_retention` | auth_ctx(); require_mgmt() |  |
| GET | `/version` | none | `version_info` | none |  |
| GET | `/openapi.json` | none | `openapi_json` | none |  |
| GET | `/docs` | none | `docs_page` | none |  |
| POST | `/v1/versions` | REST permit | `create_version` | auth() |  |
| POST | `/v1/versions/{version_id}/claims` | REST permit | `propose_claim` | auth() |  |
| POST | `/v1/versions/{version_id}/events` | REST permit | `append_events` | auth() |  |
| POST | `/v1/versions/{version_id}/promises` | REST permit | `open_promise` | auth() |  |
| GET | `/v1/versions/{version_id}/promises` | REST permit | `get_promises` | auth() |  |
| POST | `/v1/versions/{version_id}/promises/{key}/fulfill` | REST permit | `fulfill_promise` | auth() |  |
| POST | `/v1/versions/{version_id}/anchors` | REST permit | `lock_anchor` | auth() |  |
| GET | `/v1/versions/{version_id}/anchors` | REST permit | `get_anchors` | auth() |  |
| POST | `/v1/versions/{version_id}/counters` | REST permit | `record_counts` | auth() |  |
| GET | `/v1/versions/{version_id}/counters` | REST permit | `get_counters` | auth() |  |
| PUT | `/v1/versions/{version_id}/digests` | REST permit | `upsert_digest` | auth(); require_rw() |  |
| GET | `/v1/versions/{version_id}/digests` | REST permit | `get_digests` | auth() |  |
| GET | `/v1/versions/{version_id}/head` | REST permit | `get_head` | auth() |  |
| GET | `/v1/versions/{version_id}/findings` | REST permit | `get_findings` | auth() |  |
| POST | `/v1/versions/{version_id}/findings` | REST permit | `record_findings` | authenticate_principal(); require_rw(); SCOPE_FINDINGS |  |
| GET | `/v1/versions/{version_id}/facts` | REST permit | `get_facts` | auth() |  |
| GET | `/v1/versions/{version_id}/lineage` | REST permit | `get_lineage` | auth() |  |
| GET | `/v1/versions/{version_id}/context` | REST permit | `get_context` | auth() |  |
| GET | `/v1/claims/{claim_id}` | REST permit | `get_claim` | auth() |  |
| POST | `/v1/shapes` | REST permit | `apply_shape` | auth_ctx(); require_rw() |  |
| POST | `/v1/chronology-rules` | REST permit | `chronology_api::apply_rules` | auth_ctx(); require_rw() |  |
| GET | `/v1/chronology-rules/{name}` | REST permit | `chronology_api::get_rules` | auth_ctx() |  |
| PUT | `/v1/sources` | REST permit | `put_source` | auth_ctx(); require_rw() | route body limit |
| POST | `/v1/versions/{version_id}/ingests` | REST permit | `record_ingest` | auth(); require_rw() |  |
| POST | `/v1/indexes/{shape_ref}/build` | REST permit | `build_index` | auth(); require_rw() |  |
| GET | `/v1/indexes/{shape_ref}` | REST permit | `get_index` | auth_ctx() |  |
| POST | `/v1/search` | REST permit | `hybrid_search` | data_plane_access(), auth_ctx(); SCOPE_QUERY |  |
| POST | `/v1.2/answers` | REST permit | `answers_api::answer` | data_plane_access(); SCOPE_QUERY |  |
| POST | `/v1.2/query` | REST permit | `query_api::query_collections` | data_plane_access(); SCOPE_QUERY |  |
| GET | `/v1.2/collections/{id}/publications/{publication_id}` | REST permit | `query_api::authorize_publication` | authorize_publication(), data_plane_access(); SCOPE_QUERY |  |
| GET | `/v1.2/collections/{id}/governance` | REST permit | `governance_api::get_collection_governance` | auth_ctx(); require_rw() |  |
| PUT | `/v1.2/collections/{id}/governance` | REST permit | `governance_api::replace_collection_governance` | auth_ctx(); require_rw() |  |
| POST | `/v1.2/search` | REST permit | `search_v12::search` | data_plane_access(); SCOPE_QUERY |  |
| GET | `/v1.2/collections/{id}/vocabulary/revision` | REST permit | `vocabulary_api::get_revision` | authorized(); SCOPE_QUERY |  |
| GET | `/v1.2/vocabulary-settings` | REST permit | `vocabulary_api::get_settings` | auth_ctx() |  |
| PUT | `/v1.2/vocabulary-settings` | REST permit | `vocabulary_api::put_settings` | auth_ctx(); require_rw() |  |
| GET | `/v1.2/collections/{id}/vocabulary` | REST permit | `vocabulary_api::get` | authorized(); SCOPE_VOCABULARY |  |
| PUT | `/v1.2/collections/{id}/vocabulary` | REST permit | `vocabulary_api::put` | authorized(); SCOPE_VOCABULARY |  |
| PATCH | `/v1.2/collections/{id}/vocabulary` | REST permit | `vocabulary_api::patch` | authorized(); SCOPE_VOCABULARY |  |
| POST | `/v1.2/collections/{id}/vocabulary/refresh` | REST permit | `vocabulary_api::refresh` | authorized(); SCOPE_VOCABULARY |  |
| POST | `/v1/providers` | REST permit | `providers_api::apply_provider` | rest_auth(); require_rw() |  |
| GET | `/v1/providers` | REST permit | `providers_api::list_providers` | rest_auth() |  |
| GET | `/v1/providers/{name}/health` | REST permit | `providers_api::provider_health` | rest_auth(); require_mgmt() |  |
| GET | `/v1/providers/{name}/diagnostics` | REST permit | `providers_api::provider_diagnostics` | auth_ctx(); require_mgmt() |  |
| POST | `/v1/providers/{name}/complete` | REST permit | `providers_api::provider_complete` | rest_auth(); require_rw() |  |
| POST | `/v1/providers/{name}/embed` | REST permit | `providers_api::provider_embed` | rest_auth(); require_rw() |  |
| POST | `/v1/runbooks` | REST permit | `runbooks_api::apply_runbook` | rest_auth(); require_rw() |  |
| GET | `/v1/runbooks` | REST permit | `runbooks_api::list_runbooks` | rest_auth() |  |
| POST | `/v1/runbooks/validate` | REST permit | `runbooks_api::validate_runbook` | rest_auth(); require_rw() |  |
| GET | `/v1/runbooks/{name}` | REST permit | `runbooks_api::get_runbook_info` | rest_auth() |  |
| POST | `/v1/runbooks/{name}/runs` | REST permit | `runbooks_api::run_runbook` | rest_auth(); require_rw() |  |
| GET | `/v1/authoring/patterns` | REST permit | `authoring_api::list_patterns` | rest_auth() |  |
| GET | `/v1/authoring/patterns/{id}` | REST permit | `authoring_api::get_pattern` | rest_auth() |  |
| POST | `/v1/authoring/drafts` | REST permit | `authoring_api::create_draft` | rest_auth(); require_rw() |  |
| GET | `/v1/authoring/drafts` | REST permit | `authoring_api::list_drafts` | rest_auth() |  |
| GET | `/v1/authoring/drafts/{draft_id}` | REST permit | `authoring_api::get_draft` | rest_auth() |  |
| DELETE | `/v1/authoring/drafts/{draft_id}` | REST permit | `authoring_api::delete_draft` | rest_auth(); require_rw() |  |
| PUT | `/v1/authoring/drafts/{draft_id}/answers` | REST permit | `authoring_api::update_answers` | rest_auth(); require_rw() |  |
| POST | `/v1/authoring/drafts/{draft_id}/validate` | REST permit | `authoring_api::validate_draft` | rest_auth(); require_rw() |  |
| POST | `/v1/authoring/drafts/{draft_id}/assist` | REST permit | `authoring_api::assist_draft` | rest_auth(); require_rw() |  |
| POST | `/v1/authoring/drafts/{draft_id}/export` | REST permit | `authoring_api::export_draft` | rest_auth(); require_rw() |  |
| POST | `/v1/authoring/drafts/{draft_id}/apply` | REST permit | `authoring_api::apply_draft` | rest_auth(), authoring_draft_invalid(); require_exportable(), require_rw() |  |
| POST | `/v1/runbooks/{name}/sessions` | REST permit | `sessions_api::create_session` | auth_query() |  |
| POST | `/v1/sessions/{id}/turns` | REST permit | `sessions_api::turn` | auth_query() |  |
| POST | `/v1/sessions/{id}/turns/stream` | REST permit | `sessions_api::turn_stream` | auth_query() |  |
| GET | `/v1/sessions/{id}` | REST permit | `sessions_api::get_session` | authenticate_principal(), auth_query() |  |
| POST | `/v1/sessions/{id}/close` | REST permit | `sessions_api::close_session` | authenticate_principal(), auth_query() |  |
| POST | `/v1/ingest` | REST permit | `ingest_api::ingest_file` | auth_ingest() | route body limit |
| POST | `/v1/ingest/batch` | REST permit | `ingest_api::ingest_batch` | auth_ingest() | route body limit |
| POST | `/v1/ingest/bulk` | REST permit | `ingest_api::bulk_open` | auth_ingest() | route body limit |
| GET | `/v1/ingest/bulk/{bulk_id}` | REST permit | `ingest_api::bulk_status` | auth_ingest() |  |
| POST | `/v1/ingest/bulk/{bulk_id}/chunk` | REST permit | `ingest_api::bulk_chunk` | auth_ingest() | route body limit |
| POST | `/v1/ingest/bulk/{bulk_id}/complete` | REST permit | `ingest_api::bulk_complete` | auth_ingest() |  |
| GET | `/v1/sources/{source_id}` | REST permit | `get_source` | auth_ctx() |  |
| POST | `/v1/evidence` | REST permit | `evidence_routes::seal` | evidence_access() |  |
| GET | `/v1/evidence/{evidence_id}` | REST permit | `evidence_routes::get_manifest` | evidence_access() |  |
| DELETE | `/v1/evidence/{evidence_id}` | REST permit | `evidence_routes::purge` | auth_ctx(); require_mgmt() |  |
| POST | `/v1/evidence/{evidence_id}/legal-hold` | REST permit | `evidence_routes::legal_hold` | auth_ctx(); require_mgmt() |  |
| PUT | `/v1/evidence/{evidence_id}/bytes` | REST permit | `evidence_routes::put_bytes` | evidence_access() | route body limit |
| POST | `/v1/evidence/{evidence_id}/commit` | REST permit | `evidence_routes::commit` | evidence_access() |  |
| GET | `/v1/evidence/{evidence_id}/rows` | REST permit | `evidence_routes::get_rows` | evidence_access() |  |
| GET | `/v1/evidence/{evidence_id}/accesses` | REST permit | `evidence_routes::get_accesses` | auth_ctx(); require_mgmt() |  |
| POST | `/v1/runbooks/{name}/remove-request` | REST permit | `runbooks_api::remove_request` | rest_auth(); require_rw() |  |
| POST | `/v1/runbooks/{name}/remove-confirm` | REST permit | `runbooks_api::remove_confirm` | rest_auth(); require_rw() |  |
| GET | `/v1/runs/{run_id}` | REST permit | `runbooks_api::get_run` | rest_auth() |  |
| POST | `/v1/runs/{run_id}/steps/{ordinal}/approve` | REST permit | `runbooks_api::approve_step` | rest_auth(); require_rw() |  |
| POST | `/v1/access-tokens` | REST permit | `tokens_api::issue_access_token` | auth_ctx(); require_mgmt() |  |
| GET | `/v1/access-tokens` | REST permit | `reports_api::list_tokens` | auth_ctx(); require_mgmt() |  |
| POST | `/v1/access-tokens/{jti}/revoke` | REST permit | `reports_api::revoke_token` | auth_ctx(); require_mgmt() |  |
| GET | `/v1/reports/usage` | REST permit | `reports_api::usage` | auth_ctx(); require_mgmt() |  |
| GET | `/v1/reports/audit` | REST permit | `reports_api::audit` | auth_ctx(); require_mgmt() |  |
| GET | `/v1/reports/cost` | REST permit | `reports_api::cost` | auth_ctx(); require_mgmt() |  |
| GET | `/v1/reports/money` | REST permit | `money_api::monetary_report` | auth_ctx(), monetary_rest_grpc_authority_tenant_isolation_and_large_amounts(), test_state_with_auth(); require_mgmt() |  |
| GET | `/v1/monetary/prices` | REST permit | `money_api::monetary_prices` | auth_ctx(); require_mgmt() |  |
| POST | `/v1/monetary/prices` | REST permit | `money_api::add_monetary_price` | auth_ctx(); require_mgmt() |  |
| POST | `/v1/monetary/observations` | REST permit | `money_api::add_monetary_observation` | auth_ctx(); require_mgmt() |  |
| GET | `/v1/reports/budgets` | REST permit | `reports_api::budgets` | auth_ctx(); require_mgmt() |  |
| GET | `/v1/budgets/{id}/evidence` | REST permit | `budget_api::budget_evidence` | auth_ctx(); require_mgmt() |  |
| GET | `/v1/budgets/{id}/adjustments` | REST permit | `budget_api::budget_adjustments` | auth_ctx(); require_mgmt() |  |
| POST | `/v1/budgets/{id}/adjustments` | REST permit | `budget_api::reconcile_budget` | auth_ctx(); require_mgmt() |  |
| GET | `/v1/reports/budget-usage` | REST permit | `budget_api::budget_usage_report` | auth_ctx(), test_state_with_auth(); require_mgmt() |  |
| GET | `/v1/max-tokens` | REST permit | `max_tokens_api::get_max_tokens` | auth_ctx() |  |
| POST | `/v1/max-tokens` | REST permit | `max_tokens_api::replace_max_tokens` | auth(); require_rw() |  |
| GET | `/v1/reports/timeseries` | REST permit | `reports_api::timeseries` | auth_ctx(); require_mgmt() |  |
| GET | `/v1/reports/endpoints` | REST permit | `reports_api::endpoints` | auth_ctx(); require_mgmt() |  |
| GET | `/v1/reports/evidence` | REST permit | `reports_api::evidence_report` | auth_ctx(); require_mgmt() |  |
| GET | `/v1/reports/matrix` | REST permit | `reports_api::matrix_report` | auth_ctx(); require_mgmt() |  |
| GET | `/v1/reports/runbooks` | REST permit | `reports_api::runbook_report` | auth_ctx(); require_mgmt() |  |
| GET | `/v1/reports/sessions` | REST permit | `reports_api::sessions_report` | auth_ctx(); require_mgmt() |  |
| POST | `/v1/collections` | REST permit | `collections_api::create_collection` | auth_ctx(); require_rw() |  |
| GET | `/v1/collections` | REST permit | `collections_api::list_collections` | auth_ctx() |  |
| GET | `/v1/collections/{id}` | REST permit | `collections_api::get_collection` | auth_ctx() |  |
| POST | `/v1/index-artifacts/backfill` | REST permit | `datastore_builds::backfill` | auth_ctx(); require_rw() |  |
| GET | `/v1/index-artifacts/{index_version_id}` | REST permit | `datastore_builds::artifact_status` | auth_ctx() |  |
| POST | `/v1/index-artifacts/{index_version_id}/verify` | REST permit | `datastore_builds::verify_artifacts` | auth_ctx(); require_rw() |  |
| POST | `/v1/index-artifacts/{index_version_id}/rebuild` | REST permit | `datastore_builds::rebuild_artifact` | auth_ctx(); require_rw() |  |
| POST | `/v1/index-artifacts/{index_version_id}/bind` | REST permit | `datastore_builds::bind_artifact` | auth_ctx(); require_rw() |  |
| POST | `/v1/index-artifacts/{index_version_id}/promote` | REST permit | `datastore_builds::promote_artifact` | auth_ctx(); require_rw() |  |
| POST | `/v1/collections/{collection_id}/activate-index` | REST permit | `datastore_builds::activate_collection_index` | auth_ctx(); require_rw() |  |
| POST | `/v1/index-build-jobs` | REST permit | `datastore_jobs::enqueue_job` | auth_ctx(); require_rw() |  |
| GET | `/v1/index-build-jobs` | REST permit | `datastore_jobs::list_jobs` | auth_ctx() |  |
| GET | `/v1/index-build-jobs/{job_id}` | REST permit | `datastore_jobs::get_job` | auth_ctx() |  |
| POST | `/v1/index-build-jobs/{job_id}/cancel` | REST permit | `datastore_jobs::cancel_job` | auth_ctx(); require_rw() |  |
| PUT | `/v1/retrieval-rollout` | REST permit | `datastore_serving::rollout_set` | auth_ctx(); require_rw() |  |
| GET | `/v1/retrieval-rollout/{scope_kind}/{scope_id}` | REST permit | `datastore_serving::rollout_get` | auth_ctx() |  |
| GET | `/admin` | none (console) | `monitoring::overview` | admin_auth() |  |
| GET | `/admin/traffic` | none (console) | `monitoring::traffic` | admin_auth() |  |
| GET | `/admin/endpoints` | none (console) | `monitoring::endpoints` | admin_auth() |  |
| GET | `/admin/usage` | none (console) | `monitoring::usage` | admin_auth() |  |
| GET | `/admin/health` | none (console) | `monitoring::health` | admin_auth() |  |
| GET | `/admin/runbooks` | none (console) | `runbooks::hub` | admin_auth() |  |
| GET | `/admin/runbooks/{name}` | none (console) | `runbooks::runbook` | admin_auth() |  |
| GET | `/admin/shapes/{shape_ref}` | none (console) | `runbooks::shape` | admin_auth() |  |
| GET | `/admin/chronology-rules/{name}` | none (console) | `runbooks::chronology` | admin_auth() |  |
| GET | `/admin/runs/{run_id}` | none (console) | `runbooks::run` | admin_auth() |  |
| POST | `/admin/runs/{run_id}/steps/{ordinal}/approve` | none (console) | `runbooks::approve` | admin_auth() |  |
| GET | `/admin/providers` | none (console) | `inventory::providers` | admin_auth() |  |
| GET | `/admin/collections` | none (console) | `inventory::collections` | admin_auth() |  |
| GET | `/admin/storage` | none (console) | `storage::storage` | admin_auth() |  |
| GET | `/admin/collections/{id}` | none (console) | `inventory::collection` | admin_auth() |  |
| GET | `/admin/sessions` | none (console) | `inventory::sessions` | admin_auth() |  |
| GET | `/admin/sessions/{id}` | none (console) | `inventory::session` | admin_auth() |  |
| GET | `/admin/tokens` | none (console) | `inventory::tokens` | admin_auth() |  |
| POST | `/admin/tokens/issue` | none (console) | `inventory::token_issue` | admin_auth() |  |
| POST | `/admin/tokens/{jti}/revoke` | none (console) | `inventory::token_revoke` | admin_auth() |  |
| GET | `/admin/audit` | none (console) | `inventory::audit` | admin_auth() |  |
| GET | `/admin/findings` | none (console) | `inventory::findings` | admin_auth() |  |
| GET | `/admin/matrix` | none (console) | `inventory::matrix` | admin_auth() |  |
| GET | `/admin/login` | none (console) | `login_form` | none |  |
| POST | `/admin/login` | none (console) | `login_submit` | authenticate() |  |
