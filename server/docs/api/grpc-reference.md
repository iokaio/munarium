# MMP v1 gRPC reference

> Generated from `proto/mmp/v1/` by `cargo run -p munarium-proto --bin gen-grpc-docs -- docs/api/grpc-reference.md`.
> Do not edit by hand — CI drift-checks this file against the protos.

The proto files are normative; see [grpc.md](grpc.md) for connection and metadata conventions.

## mmp/v1/common.proto

### message TenantRef

Tenant scoping travels in auth metadata, not messages; TenantRef appears only
on admin surfaces that operate across tenants.

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | tenant_id | string |  |

### message VersionRef

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | version_id | string | memory version id (lineage node) |

### message GateFinding

A deterministic-gate (or shape/policy) finding. rule_id keeps the
dotted vocabulary: gate.anchor-consistency, gate.ledger-conflict,
gate.orphaned-reference, gate.meta-leakage, gate.lexical-similarity,
gate.chronology-order|-deadline|-duration, shape.schema-violation, ...

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | rule_id | string |  |
| 2 | severity | Severity |  |
| 3 | message | string |  |
| 4 | scope_path | string |  |
| 5 | detail_json | string | structured detail, JSON-encoded |

### message PolicyRejection

Machine-actionable rejection payload (mirrors REST problem+json).

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | problem_type | string | e.g. "https://munarium.ioka.io/problems/policy-rejection" |
| 2 | detail | string |  |
| 3 | findings | repeated GateFinding |  |
| 4 | policy_citation | string |  |

### message PageRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | page_size | uint32 | server caps apply |
| 2 | page_token | string |  |

### message PageResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | next_page_token | string |  |

### message ProvenanceEnvelope

Every retrieval answer carries one of these; reproducibility is the demo.

Sources are named three ways on purpose: ids are stable identity, paths say
WHICH DOCUMENT answered (a bare hash never did), and hashes prove which
bytes it held when indexed.

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | chunk_ids | repeated string |  |
| 2 | source_content_hashes | repeated string | hex sha-256 |
| 3 | index_version | string |  |
| 4 | event_watermark | uint64 | ledger seq the index reflects |
| 5 | provider_fingerprint | string | embedding provider/model/dims, when applicable |
| 6 | generated_at | google.protobuf.Timestamp |  |
| 7 | source_ids | repeated string | 'src-…', identity of each source |
| 8 | source_paths | repeated string | the logical paths of those sources |

### enum ClaimStatus

Claim status: blocked claims are recorded disputed,
never dropped.

| Value | # | Notes |
|---|---|---|
| CLAIM_STATUS_UNSPECIFIED | 0 |  |
| CLAIM_STATUS_ACCEPTED | 1 |  |
| CLAIM_STATUS_DISPUTED | 2 |  |

### enum Severity

| Value | # | Notes |
|---|---|---|
| SEVERITY_UNSPECIFIED | 0 |  |
| SEVERITY_INFO | 1 |  |
| SEVERITY_WARN | 2 |  |
| SEVERITY_BLOCK | 3 |  |

## mmp/v1/ledger.proto

### message ClaimOrigin

Where a connector-originated claim came from. Absent on every
model-extracted claim; provenance for humans and reconciliation, never a
gate input.

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | kind | string | "connector" \| "rollback" |
| 2 | source_id | string |  |
| 3 | mapping_version | string | name@version of the producing ClaimMapping |
| 4 | row_key | string | the source row's stable key |
| 5 | event_position | string | LSN / delta version / manifest offset; empty = none |
| 6 | observed_at | string | RFC-3339; empty = none |
| 7 | evidence_id | string | the sealed observation batch; empty = none |

### message Claim

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | id | string |  |
| 2 | version_id | string |  |
| 3 | seq | uint64 |  |
| 4 | claim_type | ClaimType |  |
| 5 | subject | string |  |
| 6 | key | string |  |
| 7 | value | string |  |
| 8 | normalized_text | string | canonical "subject.key=value" |
| 9 | scope_path | string |  |
| 10 | status | ClaimStatus |  |
| 11 | provenance | Provenance |  |
| 12 | supersedes_id | string | empty unless this claim supersedes another |
| 13 | entity_id | string |  |
| 14 | evidence_json | string |  |
| 15 | confidence | double |  |
| 16 | shape_ref | string | shape id@version validating this claim's body |
| 17 | recorded_at | google.protobuf.Timestamp |  |
| 18 | origin | ClaimOrigin | unset unless a connector proposed it |

### message FactSlice

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | facts | repeated Claim |  |
| 2 | as_of_seq | uint64 | the pin the slice was resolved at (0 = head) |
| 3 | head_seq | uint64 |  |

### message Anchor

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | id | string |  |
| 2 | version_id | string |  |
| 3 | detail_key | string | "subject.key" |
| 4 | locked_value | string |  |
| 5 | locked_at_scope | string |  |
| 6 | status | string | locked \| released |
| 7 | seq | uint64 |  |

### message Promise

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | id | string |  |
| 2 | version_id | string |  |
| 3 | key | string | stable coordination key |
| 4 | kind | string |  |
| 5 | description | string |  |
| 6 | origin_scope | string |  |
| 7 | due_scope | string |  |
| 8 | status | string | open \| fulfilled \| expired \| violated |
| 9 | seq | uint64 |  |
| 10 | fulfilled_seq | uint64 | 0 = not fulfilled |

### message CounterState

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | key | string |  |
| 2 | total | uint64 |  |
| 3 | budget | uint64 | 0 = no budget |

### message Lineage

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | version_ids | repeated string | root -> leaf inclusive |

### message Digest

A digest ladder rung. tier 0 = scope, 1 = group, 2 = rollup. Stored rungs
are never served under a pin — pinned reads rebuild deterministically.

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | version_id | string |  |
| 2 | tier | uint32 |  |
| 3 | scope_path | string |  |
| 4 | content | string |  |
| 5 | content_hash | string |  |
| 6 | built_from_seq | uint64 |  |

### message ComposedContext

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | sections | repeated ComposedContext.Section |  |
| 2 | text | string |  |
| 3 | estimated_tokens | uint64 |  |
| 4 | content_hash | string | feeds invocation caching |
| 5 | as_of_seq | uint64 | 0 = head |

### message ComposedContext.Section

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | title | string |  |
| 2 | body | string |  |

### enum ClaimType

| Value | # | Notes |
|---|---|---|
| CLAIM_TYPE_UNSPECIFIED | 0 |  |
| CLAIM_TYPE_FACT | 1 |  |
| CLAIM_TYPE_UPDATE | 2 | value legitimately changed (status transition) |
| CLAIM_TYPE_CORRECTION | 3 | the earlier value was wrong |

### enum Provenance

| Value | # | Notes |
|---|---|---|
| PROVENANCE_UNSPECIFIED | 0 |  |
| PROVENANCE_WITNESSED | 1 |  |
| PROVENANCE_BACKFILLED | 2 |  |
| PROVENANCE_REPAIRED | 3 |  |
| PROVENANCE_EMERGENT | 4 |  |
| PROVENANCE_COVERAGE_REPAIR | 5 |  |

## mmp/v1/command.proto

### service CommandService

| RPC | Request | Response | Notes |
|---|---|---|---|
| CreateVersion | CreateVersionRequest | CreateVersionResponse |  |
| ProposeClaim | ProposeClaimRequest | ProposeClaimResponse |  |
| AppendEvents | AppendEventsRequest | AppendEventsResponse |  |
| OpenPromise | OpenPromiseRequest | OpenPromiseResponse |  |
| FulfillPromise | FulfillPromiseRequest | FulfillPromiseResponse |  |
| LockAnchor | LockAnchorRequest | LockAnchorResponse |  |
| RecordCounts | RecordCountsRequest | RecordCountsResponse |  |
| UpsertDigest | UpsertDigestRequest | UpsertDigestResponse |  |

### message CreateVersionRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | parent_version_id | string | empty = new lineage root |
| 2 | metadata_json | string | e.g. {"as_of": "2026-08-08"} |

### message CreateVersionResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | version_id | string |  |

### message ProposeClaimRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | version_id | string |  |
| 2 | expected_head | optional uint64 | optimistic head check; absent = don't check |
| 3 | claim_type | ClaimType |  |
| 4 | subject | string |  |
| 5 | key | string |  |
| 6 | value | string |  |
| 7 | scope_path | string |  |
| 8 | provenance | Provenance |  |
| 9 | supersedes_id | string | corrections/updates name the superseded claim |
| 10 | entity_id | string |  |
| 11 | evidence_json | string |  |
| 12 | confidence | double |  |
| 13 | shape_ref | string |  |
| 14 | origin | ClaimOrigin | connector provenance, optional |

### message ProposeClaimResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | claim | Claim | recorded ACCEPTED or DISPUTED — never dropped |
| 2 | findings | repeated GateFinding | gate outcomes for this proposal |
| 3 | head_seq | uint64 |  |

### message AppendEventsRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | version_id | string |  |
| 2 | expected_head | optional uint64 |  |
| 3 | claims | repeated ProposeClaimRequest | batched; gated as one candidate unit |
| 4 | candidate_text | string | optional full output text for text gates |

### message AppendEventsResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | claims | repeated Claim |  |
| 2 | findings | repeated GateFinding |  |
| 3 | head_seq | uint64 |  |

### message OpenPromiseRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | version_id | string |  |
| 2 | key | string |  |
| 3 | kind | string |  |
| 4 | description | string |  |
| 5 | origin_scope | string |  |
| 6 | due_scope | string |  |

### message OpenPromiseResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | promise | Promise |  |

### message FulfillPromiseRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | version_id | string |  |
| 2 | key | string |  |
| 3 | result_ref | string |  |

### message FulfillPromiseResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | fulfilled | bool | false = no open promise with that key |

### message LockAnchorRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | version_id | string |  |
| 2 | subject | string |  |
| 3 | key | string |  |
| 4 | value | string |  |
| 5 | scope_path | string |  |
| 6 | evidence_json | string |  |

### message LockAnchorResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | anchor | Anchor |  |

### message RecordCountsRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | version_id | string |  |
| 2 | key | string |  |
| 3 | scope_path | string |  |
| 4 | count | uint64 |  |
| 5 | budget | uint64 | 0 = no budget |

### message RecordCountsResponse

_(no fields)_

### message UpsertDigestRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | digest | Digest |  |

### message UpsertDigestResponse

_(no fields)_

## mmp/v1/query.proto

### service QueryService

| RPC | Request | Response | Notes |
|---|---|---|---|
| GetHead | GetHeadRequest | GetHeadResponse |  |
| GetClaim | GetClaimRequest | GetClaimResponse |  |
| SliceFacts | SliceFactsRequest | SliceFactsResponse |  |
| GetLineage | GetLineageRequest | GetLineageResponse |  |
| ListAnchors | ListAnchorsRequest | ListAnchorsResponse |  |
| ListPromises | ListPromisesRequest | ListPromisesResponse |  |
| ComposeContext | ComposeContextRequest | ComposeContextResponse |  |
| CounterTotals | CounterTotalsRequest | CounterTotalsResponse |  |
| ListDigests | ListDigestsRequest | ListDigestsResponse |  |

### message GetHeadRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | version_id | string |  |

### message GetHeadResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | head_seq | uint64 |  |

### message GetClaimRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | claim_id | string |  |

### message GetClaimResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | claim | Claim |  |
| 2 | superseded | bool |  |
| 3 | superseded_by | string |  |

### message SliceFactsRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | version_id | string |  |
| 2 | scope_prefix | string | exact or "prefix.%" match |
| 3 | as_of_seq | uint64 | 0 = head |
| 4 | statuses | repeated ClaimStatus | default: [ACCEPTED] |
| 5 | limit | uint32 | keeps the NEWEST n |

### message SliceFactsResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | slice | FactSlice |  |

### message GetLineageRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | version_id | string |  |

### message GetLineageResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | lineage | Lineage |  |

### message ListAnchorsRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | version_id | string |  |
| 2 | as_of_seq | uint64 |  |

### message ListAnchorsResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | anchors | repeated Anchor |  |

### message ListPromisesRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | version_id | string |  |
| 2 | status | string | empty = all; applies to the AS-OF status |
| 3 | as_of_seq | uint64 |  |

### message ListPromisesResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | promises | repeated Promise |  |

### message ComposeContextRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | version_id | string |  |
| 2 | scope | string |  |
| 3 | budget_tokens | uint64 | 0 = unbounded |
| 4 | fact_limit | uint32 | default 60 |
| 5 | as_of_seq | uint64 |  |
| 6 | as_of_date | string | YYYY-MM-DD; resolved to a seq pin via version as_of metadata |

### message ComposeContextResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | context | ComposedContext |  |

### message CounterTotalsRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | version_id | string |  |
| 2 | as_of_seq | uint64 |  |

### message CounterTotalsResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | counters | repeated CounterState |  |

### message ListDigestsRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | version_id | string |  |

### message ListDigestsResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | digests | repeated Digest |  |

## mmp/v1/retrieval.proto

### service RetrievalService

| RPC | Request | Response | Notes |
|---|---|---|---|
| HybridSearch | HybridSearchRequest | HybridSearchResponse |  |
| GetIndexVersion | GetIndexVersionRequest | GetIndexVersionResponse |  |
| CreateCollection | CreateCollectionRequest | CollectionInfo | REST twins: |
| ListCollections | ListCollectionsRequest | ListCollectionsResponse |  |
| GetCollection | GetCollectionRequest | CollectionInfo |  |

### message HybridSearchRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | query | string |  |
| 2 | shape_ref | string | which shape's index to search |
| 3 | top_k | uint32 | default 10 |
| 4 | filter_json | string | shape-defined metadata filters |
| 5 | index_version | string | empty = active index |

### message SearchHit

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | chunk_id | string |  |
| 2 | source_content_hash | string | integrity of the bytes indexed |
| 3 | text | string |  |
| 4 | score | double |  |
| 5 | lexical_rank | double |  |
| 6 | vector_rank | double |  |
| 7 | metadata_json | string |  |
| 8 | source_id | string | stable identity of the source |
| 9 | source_path | string | the logical path — which document answered |

### message HybridSearchResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | hits | repeated SearchHit |  |
| 2 | envelope | ProvenanceEnvelope |  |

### message GetIndexVersionRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | shape_ref | string |  |

### message GetIndexVersionResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | index_version | string |  |
| 2 | event_watermark | uint64 |  |
| 3 | manifest_json | string |  |
| 4 | active | bool |  |

### message CreateCollectionRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | name | string | tenant-unique stable handle |
| 2 | shape_ref | string | immutable after creation |
| 3 | access_level | int32 |  |
| 4 | compartments | repeated string |  |
| 5 | description | string | empty = none |

### message CollectionInfo

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | id | string |  |
| 2 | name | string |  |
| 3 | shape_ref | string |  |
| 4 | access_level | int32 |  |
| 5 | compartments | repeated string |  |
| 6 | status | string | active \| retired |
| 7 | description | string | empty = none |
| 8 | created_at | string |  |
| 9 | source_count | int64 |  |
| 10 | active_index | string | empty = no index cut over |

### message ListCollectionsRequest

_(no fields)_

### message ListCollectionsResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | collections | repeated CollectionInfo |  |

### message GetCollectionRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | id | string |  |

## mmp/v1/ingest.proto

### service IngestService

| RPC | Request | Response | Notes |
|---|---|---|---|
| PutSource | stream PutSourceRequest | PutSourceResponse |  |
| RecordIngest | RecordIngestRequest | RecordIngestResponse |  |
| IngestFiles | IngestFilesRequest | IngestFilesResponse | REST twin: |

### message PutSourceRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | header | SourceHeader | (oneof `msg`) first message |
| 2 | chunk | bytes | (oneof `msg`) subsequent messages |

### message SourceHeader

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | declared_sha256 | string | hex; server verifies before commit |
| 2 | media_type | string |  |
| 3 | filename | string |  |
| 4 | shape_ref | string |  |

### message PutSourceResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | content_hash | string | integrity of the stored bytes |
| 2 | bytes_len | uint64 |  |
| 3 | already_existed | bool | True only when this path already held these exact bytes; re-putting a path with new content is an update and reports false. |
| 4 | source_id | string | stable identity, derived from the logical path |

### message RecordIngestRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | version_id | string |  |
| 2 | content_hash | string |  |
| 3 | shape_ref | string |  |
| 4 | metadata_json | string |  |

### message RecordIngestResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | event_id | string |  |
| 2 | seq | uint64 |  |

### message IngestFile

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | filename | string | identity + storage path; required |
| 2 | media_type | string |  |
| 3 | content | bytes |  |
| 4 | sha256 | string | optional declared hash, verified before commit |
| 5 | collections | repeated string | Explicit collection names to bind into; empty = auto-bind via the declarative sources: matchers of every reachable active runbook. |

### message IngestFilesRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | files | repeated IngestFile | 1..500 |

### message IngestResult

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | filename | string |  |
| 2 | source_id | string | empty on per-item error |
| 3 | sha256 | string |  |
| 4 | existed | bool | true only on a genuine idempotent replay |
| 5 | bound_to | repeated string |  |
| 6 | error | string | empty = success |

### message IngestFilesResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | results | repeated IngestResult |  |

## mmp/v1/runbook.proto

### service RunbookService

| RPC | Request | Response | Notes |
|---|---|---|---|
| ApplyShape | ApplyShapeRequest | ApplyShapeResponse |  |
| ApplyRunbook | ApplyRunbookRequest | ApplyRunbookResponse |  |
| RunRunbook | RunRunbookRequest | RunRunbookResponse |  |
| GetRun | GetRunRequest | GetRunResponse |  |
| ApproveStep | ApproveStepRequest | ApproveStepResponse |  |
| ListRunbooks | ListRunbooksRequest | ListRunbooksResponse | REST twins: |
| GetRunbookInfo | GetRunbookInfoRequest | GetRunbookInfoResponse |  |
| ValidateRunbook | ValidateRunbookRequest | ValidateRunbookResponse |  |
| RequestRemoval | RequestRemovalRequest | RequestRemovalResponse |  |
| ConfirmRemoval | ConfirmRemovalRequest | ConfirmRemovalResponse |  |

### message ApplyShapeRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | yaml | string | apiVersion: munarium.ioka.io/v1, kind: Shape |
| 2 | version_id | string | optional lineage: when set, the publication is recorded as a ledger claim |

### message ApplyShapeResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | shape_ref | string | name@version |
| 2 | event_id | string | set when the request named a version_id |

### message ApplyRunbookRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | yaml | string | kind: Runbook |

### message ApplyRunbookResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | runbook_ref | string |  |
| 2 | event_id | string | reserved: runbook application records no ledger event yet (either plane) |

### message RunRunbookRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | runbook_ref | string |  |
| 2 | params_json | string |  |

### message RunRunbookResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | run_id | string |  |
| 2 | state | string | post-transition run state (running \| awaiting_approval \| done \| failed) |

### message RunbookStepState

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | ordinal | uint32 |  |
| 2 | name | string |  |
| 3 | state | string | pending \| running \| awaiting_approval \| done \| failed |
| 4 | detail_json | string |  |
| 5 | updated_at | google.protobuf.Timestamp |  |

### message GetRunRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | run_id | string |  |

### message GetRunResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | run_id | string |  |
| 2 | runbook_ref | string |  |
| 3 | state | string |  |
| 4 | steps | repeated RunbookStepState |  |
| 5 | version_id | string | the lineage every step transition was evented into; empty = none |

### message ApproveStepRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | run_id | string |  |
| 2 | step_ordinal | uint32 |  |
| 3 | note | string |  |

### message ApproveStepResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | event_id | string | Reserved: the approval transition IS recorded as a ledger event when the run names a version, but neither plane surfaces its id yet (REST returns {run_id, state}); empty until then. |
| 2 | state | string | post-approval run state — same shape as REST |

### message ListRunbooksRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | include_removed | bool |  |

### message RunbookCollectionInfo

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | name | string |  |
| 2 | collection_id | string | empty until the runbook is applied |
| 3 | shape_ref | string |  |
| 4 | access_level | int32 |  |
| 5 | compartments | repeated string |  |
| 6 | active_index | string | empty = no index cut over |
| 7 | source_count | int64 |  |

### message RunbookSummary

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | runbook_ref | string | name@version |
| 2 | name | string |  |
| 3 | version | uint32 |  |
| 4 | status | string | active \| remove_requested \| removed |
| 5 | min_access_level | int32 |  |
| 6 | collections | repeated RunbookCollectionInfo |  |
| 7 | created_at | string |  |

### message ListRunbooksResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | runbooks | repeated RunbookSummary |  |

### message GetRunbookInfoRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | name | string | runbook name (latest) or exact name@version |

### message GetRunbookInfoResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | runbook_ref | string |  |
| 2 | name | string |  |
| 3 | version | uint32 |  |
| 4 | status | string |  |
| 5 | collections | repeated RunbookCollectionInfo |  |
| 6 | versions | repeated string | sibling refs of the same name, incl. this |
| 7 | models_json | string | the models block, echoed; empty = none |
| 8 | retrieval_json | string | retrieval knobs in effect |
| 9 | has_completion | bool |  |
| 10 | created_at | string |  |

### message ValidateRunbookRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | yaml | string |  |
| 2 | suggest | bool | add AI improvement suggestions (provider required) |
| 3 | provider | string | Model override for the suggestion pass; empty strings = not set. |
| 4 | model | string |  |
| 5 | tier | string |  |

### message ValidationFinding

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | severity | string | error \| warn \| info |
| 2 | code | string | stable dotted code |
| 3 | message | string |  |
| 4 | path | string |  |

### message RunbookSuggestion

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | title | string |  |
| 2 | rationale | string |  |
| 3 | patch_hint | string | empty = none |

### message ValidateRunbookResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | valid | bool | false when any error-severity finding is present |
| 2 | findings | repeated ValidationFinding |  |
| 3 | suggestions | repeated RunbookSuggestion |  |
| 4 | suggest_note | string | empty = none |

### message RequestRemovalRequest

Removal is double-pass and soft only: request, then confirm with the
removal_id inside the TTL. All data is retained — removal is
visibility-only (same contract as REST).

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | runbook_ref | string | EXACT name@version |

### message RequestRemovalResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | runbook_ref | string |  |
| 2 | removal_id | string |  |
| 3 | expires_at | string |  |

### message ConfirmRemovalRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | runbook_ref | string | EXACT name@version |
| 2 | removal_id | string |  |

### message ConfirmRemovalResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | runbook_ref | string |  |
| 2 | status | string | always "removed" on success |

## mmp/v1/provider.proto

### service ProviderService

| RPC | Request | Response | Notes |
|---|---|---|---|
| ApplyProviderConfig | ApplyProviderConfigRequest | ApplyProviderConfigResponse |  |
| ProviderHealth | ProviderHealthRequest | ProviderHealthResponse |  |
| Complete | CompleteRequest | CompleteResponse |  |
| Embed | EmbedRequest | EmbedResponse |  |

### message ApplyProviderConfigRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | yaml | string | kind: ProviderConfig (provider, endpoint, models, credentialRef, budgets) |

### message ApplyProviderConfigResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | config_name | string |  |

### message ProviderHealthRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | config_name | string |  |

### message ProviderHealthResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | healthy | bool |  |
| 2 | provider | string |  |
| 3 | endpoint_fingerprint | string |  |
| 4 | detail | string | key validity / reachability detail, never key material |

### message CompleteRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | config_name | string | Config name, or the reserved name "default" to engage the default rule: anthropic first, openai second, openrouter third — the first family with a usable credential serves the request. |
| 2 | model | string | Explicit model id — any model the selected provider supports. Empty = tier default (when tier set) or the config's first complete model. |
| 3 | system | string |  |
| 4 | prompt | string |  |
| 5 | max_tokens | uint32 |  |
| 6 | temperature | double |  |
| 7 | tools_json | string |  |
| 8 | version_id | string | When set, the invocation is recorded as a ledger event in this lineage and its id comes back as invocation_event_id. Empty = not recorded. |
| 9 | provider | string | Provider family override (anthropic\|openai\|openrouter). Only honored with config_name "default". Empty = default-priority rule. |
| 10 | tier | string | Model tier: "fast" (lesser model), "capable", or "frontier" (top model). Ignored when model set. |

### message CompleteResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | text | string |  |
| 2 | stop_reason | string |  |
| 3 | input_tokens | uint64 |  |
| 4 | output_tokens | uint64 |  |
| 5 | invocation_event_id | string |  |
| 6 | provider | string | The provider family and resolved model that served the request. |
| 7 | model | string |  |

### message EmbedRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | config_name | string | Config name, or the reserved name "default" (see CompleteRequest). |
| 2 | model | string |  |
| 3 | inputs | repeated string |  |
| 4 | version_id | string | When set, the invocation is recorded as a ledger event in this lineage. |
| 5 | provider | string | Provider family override (anthropic\|openai\|openrouter); only honored with config_name "default". |

### message EmbedResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | vectors | repeated EmbedResponse.Vector |  |
| 2 | dimensions | uint32 |  |
| 3 | cache_hit | bool | embedding calls are cached by request hash |
| 4 | invocation_event_id | string |  |
| 5 | provider | string | The provider family and resolved model that served the request. |
| 6 | model | string |  |

### message EmbedResponse.Vector

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | values | repeated float |  |

## mmp/v1/admin.proto

**Reserved — declared, not served.** No server routes exist for `AdminService`; it is excluded from client libraries and this reference until it ships.

## mmp/v1/session.proto

### service SessionService

| RPC | Request | Response | Notes |
|---|---|---|---|
| CreateSession | CreateSessionRequest | CreateSessionResponse |  |
| Turn | TurnRequest | TurnResponse |  |
| GetSession | GetSessionRequest | GetSessionResponse |  |
| CloseSession | CloseSessionRequest | GetSessionResponse | Idempotent; closing a closed/expired session returns its state unchanged. |

### message CreateSessionRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | runbook_name | string | Runbook name (latest non-removed version) or exact name@version. |

### message CreateSessionResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | session_id | string |  |
| 2 | runbook_ref | string | pinned name@version for every turn |
| 3 | permitted_collections | repeated string |  |

### message SessionModelOverride

API-level model override — honored only under the runbook's
models.allowOverrides policy. Empty strings mean "not set".

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | provider | string |  |
| 2 | model | string |  |
| 3 | tier | string | fast \| capable \| frontier |

### message TurnRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | session_id | string |  |
| 2 | query | string |  |
| 3 | top_k | uint32 | 0 = runbook default |
| 4 | complete | bool | run the runbook's completion step |
| 5 | model_override | SessionModelOverride |  |
| 6 | research_profile | string | Run this turn through a named research profile (an evidence hierarchy). Empty = the legacy single-layer document path, byte-identical. |

### message TurnHit

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | collection | string |  |
| 2 | chunk_id | string |  |
| 3 | source_id | string |  |
| 4 | source_path | string |  |
| 5 | source_content_hash | string |  |
| 6 | text | string |  |
| 7 | score | double |  |

### message CollectionEnvelope

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | collection | string |  |
| 2 | envelope | ProvenanceEnvelope |  |

### message TurnVerification

Deterministic turn-verification outcome (quotes resolve in served text,
citations name served content). Violations are prefixed "quote: " /
"citation: ". Present only when the runbook declares
completion.verification.

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | checks | repeated string |  |
| 2 | retries | uint32 |  |
| 3 | first_pass_violations | repeated string |  |
| 4 | violations | repeated string | empty = verified |

### message TurnCompletion

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | provider | string |  |
| 2 | model | string |  |
| 3 | was_override | bool |  |
| 4 | text | string |  |
| 5 | input_tokens | uint64 | totals across ALL completions paid |
| 6 | output_tokens | uint64 | for this turn, retries included |
| 7 | verification | TurnVerification | absent when verification not declared |

### message TurnResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | session_id | string |  |
| 2 | ordinal | uint32 |  |
| 3 | collections_searched | repeated string |  |
| 4 | skipped | repeated string | permitted but no active index |
| 5 | hits | repeated TurnHit |  |
| 6 | envelopes | repeated CollectionEnvelope |  |
| 7 | completion | TurnCompletion | absent when no completion ran |
| 8 | hierarchy | EvidenceHierarchyDecision | Present only when a research profile ran. |

### message LayerOutcome

What one evidence layer produced.

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | layer | string |  |
| 2 | role | string | supporting \| primary \| controlling |
| 3 | requirement | string | required \| optional \| fallback |
| 4 | block | string | document_hits \| complete_table \| count \| fact_slice \| refusal |
| 5 | evidence_id | string |  |
| 6 | supports_completeness | bool | Whether an answer may make a completeness claim on THIS layer. Document hits are always false: retrieval returns what it found, never a proof that nothing else exists. |
| 7 | refusal_code | string |  |
| 8 | elapsed_ms | uint64 |  |

### message EvidenceHierarchyDecision

Why the model saw what it saw. About the DECISION, not the content:
no evidence rows appear here.

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | profile | string |  |
| 2 | intent_kind | string |  |
| 3 | intent_explicit | bool | True when the caller supplied the intent rather than a model producing it, so a keyless test result never reads as a planner result. |
| 4 | layers | repeated LayerOutcome |  |
| 5 | completeness_available | bool |  |
| 6 | disclosed_conflicts | uint32 |  |
| 7 | conflicts_policy | string |  |

### message GetSessionRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | session_id | string |  |

### message SessionTurn

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | ordinal | uint32 |  |
| 2 | query | string |  |
| 3 | collections_searched | repeated string |  |
| 4 | hits_json | string | stored transcript rows are JSON |
| 5 | envelope_json | string |  |
| 6 | completion_json | string | empty = no completion that turn |
| 7 | created_at | string | RFC 3339 |

### message GetSessionResponse

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | session_id | string |  |
| 2 | uid | string |  |
| 3 | runbook_ref | string |  |
| 4 | access_level | int32 |  |
| 5 | compartments | repeated string |  |
| 6 | state | string | open \| closed \| expired |
| 7 | created_at | string |  |
| 8 | turns | repeated SessionTurn |  |

### message CloseSessionRequest

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | session_id | string |  |
