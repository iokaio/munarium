# munarium-server gRPC API

**The proto files under [../../proto/mmp/v1/](../../proto/mmp/v1/) are normative.** This document
covers connections, metadata conventions, and the two gRPC planes. The generated message/service
reference is [grpc-reference.md](grpc-reference.md) — regenerate with
`cargo run -p munarium-proto --bin gen-grpc-docs -- docs/api/grpc-reference.md` (CI drift-checks it).

## Services

| Service | Purpose |
|---|---|
| `mmp.v1.CommandService` | writes through the gates: CreateVersion, ProposeClaim, AppendEvents, OpenPromise, FulfillPromise, LockAnchor, RecordCounts, UpsertDigest |
| `mmp.v1.QueryService` | GetHead, GetClaim, SliceFacts, GetLineage, ListAnchors, ListPromises, ComposeContext, CounterTotals, ListDigests — every read takes `as_of_seq` |
| `mmp.v1.RetrievalService` | HybridSearch (+ ProvenanceEnvelope), GetIndexVersion; collection twins: CreateCollection, ListCollections, GetCollection |
| `mmp.v1.IngestService` | PutSource (client-streaming, content-addressed), RecordIngest; REST twin: IngestFiles (1..500, native bytes, per-item outcomes) |
| `mmp.v1.RunbookService` | ApplyShape, ApplyRunbook, RunRunbook, GetRun, ApproveStep; REST twins: ListRunbooks, GetRunbookInfo, ValidateRunbook, RequestRemoval, ConfirmRemoval |
| `mmp.v1.ProviderService` | ApplyProviderConfig, ProviderHealth, Complete, Embed |
| `mmp.v1.SessionService` | multiturn sessions (2026-08-18): CreateSession / Turn / GetSession / CloseSession — data-plane auth (capability JWT + `munarium-uid`); no streaming turn RPC (SSE is REST-only) |
| `mmp.v1.AdminService` | **partially served** (2026-08-18): Issue/List/RevokeAccessToken (mgmt role — the tokens plane's gRPC twin); CreateTenant/ListTenants/Usage answer UNIMPLEMENTED honestly |
| `grpc.health.v1.Health` | standard health checking (tonic-health) |

Server reflection is enabled (tonic-reflection), so `grpcurl` needs no local protos.

## The two gRPC planes

Both serve the identical services — the conformance suite diffs their answers.

### 1. Direct TCP port (50051)

A dedicated raw tonic listener for direct client connections. **Plaintext by default**
(documented tradeoff; no gateway TLS exists on this port). Set `MUNARIUM_GRPC_TLS_CERT` /
`MUNARIUM_GRPC_TLS_KEY` to arm rustls. Disable the listener entirely with `MUNARIUM_GRPC_ADDR=disabled`.

```bash
grpcurl -plaintext localhost:50051 list                       # reflection
grpcurl -plaintext localhost:50051 grpc.health.v1.Health/Check
grpcurl -plaintext \
  -H "authorization: Bearer devtoken" \
  -d '{"version_id":"memv-...","as_of_seq":"5"}' \
  localhost:50051 mmp.v1.QueryService/SliceFacts
```

### 2. Gateway plane (443, gRPC over HTTP/2)

The gateway (Envoy) terminates TLS on 443 and routes by `content-type: application/grpc` to the
gRPC upstream; everything else goes to REST. Locally, `docker compose --profile gateway up`
demonstrates the same routing on :8443 (h2c).

```bash
# a TLS-terminating ingress in front of the REST plane (it must carry HTTP/2 through)
grpcurl -H "authorization: Bearer <token>" \
  <your gRPC host>:443 grpc.health.v1.Health/Check

# the Helm chart's Envoy Gateway (-insecure while the listener runs a self-signed cert)
grpcurl -insecure -H "authorization: Bearer <token>" <gateway-ip>:443 mmp.v1.QueryService/GetHead

# local compose gateway (h2c)
grpcurl -plaintext localhost:8443 grpc.health.v1.Health/Check
```

Deployment note: the direct:50051 listener always runs in-container, but whether it is
reachable from outside depends on the platform exposing a raw TCP port. The Helm chart does
so with a LoadBalancer Service (`directGrpc.enabled`); a platform with a single HTTP ingress
reaches gRPC through the 443 gateway plane instead.

## Metadata conventions

| Key | Applies to | Semantics |
|---|---|---|
| `authorization` | all | `Bearer <token>`; tenant scope derives from the token |
| `idempotency-key` | all Command RPCs | required; replay-same-request returns the recorded outcome; replay-different-request fails `INVALID_ARGUMENT` (mmp:idempotency-mismatch) |
| `munarium-uid` | all `mmp.v1.*` RPCs | required end-user id asserted by the API-management layer; missing → `INVALID_ARGUMENT` (mmp:uid-required) unless the bearer is a capability JWT, whose `sub` then supplies the uid; when present it must equal that `sub` → `PERMISSION_DENIED` (mmp:uid-mismatch). gRPC interaction rows record the call envelope (method, uid, tenant, latency) — full body capture is the REST plane. |

Set deadlines on every call (`grpcurl -max-time`, tonic `Request::set_timeout`); the server
enforces its own request timeout and load-shed (`RESOURCE_EXHAUSTED` under pressure).

**platform parity (landed 2026-08-18):** the platform surface has gRPC
twins, every one calling the SAME op function as its REST handler:

- `SessionService` (`session.proto`, new) — CreateSession / Turn /
  GetSession / CloseSession. Data-plane auth: capability JWT (or static
  token) + `munarium-uid` metadata, query scope + revocation enforced exactly
  like REST; a turn against a non-open session answers
  `FAILED_PRECONDITION` (mmp:session-not-open). Turn interaction rows carry
  session/runbook attribution.
- `AdminService` — now **served**: IssueAccessToken / ListAccessTokens /
  RevokeAccessToken (mgmt role). The tenant-lifecycle RPCs remain declared
  and answer `UNIMPLEMENTED` honestly (tenancy is provisioned out of band). The issued JWT is never stored — the gRPC capture
  layer records no bodies by construction.
- `RunbookService` — ListRunbooks / GetRunbookInfo / ValidateRunbook /
  RequestRemoval / ConfirmRemoval (additive).
- `IngestService.IngestFiles` — the batch ingest plane (ingest scope);
  bytes ride native (no base64 on this plane); per-item outcomes.
- `RetrievalService` — CreateCollection / ListCollections / GetCollection.

**Still REST-only by design:** reports + `/admin` dashboards (management
read surfaces), index builds, `/healthai`, and the `/v1/search`
multi-collection filter (the session plane is the access-checked search
path on gRPC). Verified live 2026-08-18 via grpcurl against the pg store:
shape→collection→runbook→ingest→session→turn→close end-to-end, with the
close→turn refusal and UNIMPLEMENTED tenant RPCs captured.

## Status codes

Full mapping in [errors.md](errors.md). Every error status carries structured details in
`grpc-status-details-bin`: a `google.rpc.Status` with one `google.rpc.ErrorInfo` whose
`reason` is the problem slug, `domain` is `mmp.ioka.io`, and `metadata` mirrors the REST
problem+json extensions — never parse English message text. The two you must handle in
write loops:

- `ABORTED` — optimistic head conflict (`expected_head` stale). `ErrorInfo.metadata` carries
  `expected`/`actual` (same member names as the REST problem+json). Re-read head, retry.
- `FAILED_PRECONDITION` — policy rejection. The claim was recorded **disputed**, not dropped;
  gate findings (dotted `gate.*` rule ids) ride in `ErrorInfo.metadata.gate_findings`
  (with `findings_total` / `findings_truncated` when the trailer cap applied).

Auth failures are typed: missing/invalid token → `UNAUTHENTICATED`; a valid `ro` token on a
command → `PERMISSION_DENIED`.

## Retry contract

Idempotency keys are recorded **after** a command completes, so there is no
in-flight reservation: re-sending a command whose request may already have
been delivered can execute it twice. `UNAVAILABLE` on an established stream
is indistinguishable from a call the server is still running, so the official
clients do NOT auto-retry commands on gRPC — reads retry freely. Deadline
expiry arrives as `CANCELLED` ("Timeout expired", from the client's own
timeout layer) or `DEADLINE_EXCEEDED` (server-side); both are transport
faults, and neither makes a command safe to re-send.

`IngestService.PutSource` raises its max decoding message size to 256 MiB to
match the REST body limit; the clients' chunk helpers frame at 1 MiB, so a
single message never approaches tonic's 4 MiB default.

## Client libraries

Official clients (Rust, Python, .NET, Java) speak this plane natively — see
[clients/](../../../clients/README.md). They decode the structured error details, enforce the
proto3 zero-sentinel rules, and surface the documented transport gaps as typed errors.

## Plane parity notes

- The management reporting views added 2026-08-17 — `GET /v1/reports/timeseries`,
  `/v1/reports/endpoints`, `/v1/reports/runbooks`, `/v1/reports/sessions` — and the
  `/admin` HTML dashboards are **REST-only management surfaces by design** (same
  posture as the existing reports routes): they serve operators and browsers, not
  data-plane clients, and get no gRPC twins.
- Also REST-first as of 2026-08-17, tracked here per the parity ledger rule:
  `POST /v1/sessions/{id}/close` (the session surface is REST-first overall),
  `GET /v1/versions/{id}/findings`, the promises overdue view
  (`?overdue_scope=`/`?final=`), and the chronology-rules asset routes
  (`POST/GET /v1/chronology-rules`). The chronology GATE itself runs on BOTH
  planes — arming is per-version, so gRPC `ProposeClaim`/`AppendEvents` against
  an armed version draw the same `gate.chronology-*` findings.
- `ClaimOrigin` is on BOTH planes (`ProposeClaimRequest.origin`,
  `Claim.origin`; the conformance scenario `ledger.origin-round-trips` runs on
  mem, pg, REST and gRPC). `POST /v1/versions/{id}/findings` is **REST-only**
  by the same rule as the findings read it pairs with; `rule_prefix=` on the
  read is REST-only likewise.
- Token budgets (2026-09-02): `GET`/`POST /v1/max-tokens` — the eight per-call
  output-token ceilings, read and replaced as a whole — are **REST-only** by
  the same rule as the provider-config and report surfaces they sit beside:
  an operator setting, not a data-plane call. The clients raise their
  unsupported-transport error on gRPC. Reference:
  [../tokenbudgets.md](../tokenbudgets.md).
- `ApplyShape` accepts an optional `version_id`; when set, the publication is recorded as a
  ledger claim and `event_id` is returned — identical to REST `POST /v1/shapes?version_id=`.
- `ApplyRunbookResponse.event_id` and `ApproveStepResponse.event_id` are **reserved-empty** on
  both planes today (runbook application records no ledger event; the approval transition's
  event id is not yet surfaced).
- `GetRunResponse.version_id` and `CompleteRequest`/`EmbedRequest.version_id` exist as of the
  client program's C5 pass: run-lineage attribution and invocation provenance are now identical
  on both planes (both run the shared `op_get_run` / `op_complete` / `op_embed` implementations).
- `ComposeContextRequest.as_of_date` and `HybridSearchRequest.filter_json` are declared but not
  yet implemented — the server rejects them explicitly (`INVALID_ARGUMENT`) instead of silently
  ignoring them.
