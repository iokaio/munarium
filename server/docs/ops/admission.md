# Admission by work class

Each Server instance admits at most `MUNARIUM_MAX_CONCURRENCY` in-flight requests
per plane: one ceiling for REST `/v1/` and `/v1.2/` requests on the main port, and
a separate one for typed gRPC `/mmp.v1.*` calls. A request that cannot be admitted
is shed at once with 503 `overloaded` and `Retry-After: 1` on REST, or
`RESOURCE_EXHAUSTED` with the `overloaded` reason on gRPC. Nothing queues. Health,
readiness, metrics, the console and the unversioned pages are never admitted or
shed.

This page describes the opt-in work classes added for ClearView R7/B3
([design](../design/airk-analysis.md#r7), [route inventory](../design/phase3-inventory.md#b3-t2-classification-proposed)).

## Settings

| Variable | Default | Meaning |
|---|---|---|
| `MUNARIUM_MAX_CONCURRENCY` | `512` | Every class together, per plane per instance. Unchanged from v1.4. |
| `MUNARIUM_CONTROL_RESERVE` | `0` (off) | Slots per plane carved out of the ceiling for control operations. The ordinary capacity becomes `MUNARIUM_MAX_CONCURRENCY - MUNARIUM_CONTROL_RESERVE`. |
| `MUNARIUM_HEAVY_CONCURRENCY` | `0` (off) | Heavy requests in flight per plane. Heavy work also holds an ordinary slot, so the bound sits inside the ordinary capacity. |

With neither setting there is one pool per plane, sized `MUNARIUM_MAX_CONCURRENCY`,
exactly as in v1.4. A v1.4 configuration with a one-slot ceiling stays valid.

Startup fails with a `config error:` line and exit status 2 when:

- `MUNARIUM_CONTROL_RESERVE` is not smaller than `MUNARIUM_MAX_CONCURRENCY`. A reserve
  must leave at least one ordinary slot, so one reserved slot on a one-slot plane is
  rejected.
- `MUNARIUM_HEAVY_CONCURRENCY` exceeds the ordinary capacity.
- Either value is not a non-negative integer. An empty value means 0.

The same values apply to each plane separately. No production sizes are
recommended yet; measure before enabling. The settings are read once at startup.
Server has no live reconfiguration, so a limit is never lowered under in-flight
work. A restart applies a new value to new requests.

## How a request is admitted

1. Every admitted request first tries an ordinary slot, before authentication, as
   in v1.4.
2. A heavy request needs a heavy token as well when `MUNARIUM_HEAVY_CONCURRENCY` is
   set. Without one it is shed even if ordinary slots are free. Heavy work never
   uses the reserve.
3. A control request that finds no ordinary slot, on a plane with a reserve,
   authenticates and checks the role its own handler requires. Only when that
   check passes does it take a reserved slot. A caller without that authority is
   shed exactly as it would be without a reserve.

Held ordinary and reserved slots together never exceed `MUNARIUM_MAX_CONCURRENCY`.
Authentication and its order are unchanged for every other request; the handler
still runs its own checks after admission.

## Classification

Classes come from explicit allowlists in
[`admission.rs`](../../src/munarium-server/src/admission.rs), keyed on the HTTP
method plus the route template, or on the full gRPC method path. Every other route,
including any route added later, is ordinary and can never take a reserved slot.

**Control (eligible for the reserve)**

| Operation | Role checked before a reserved slot |
|---|---|
| `PUT /v1/retrieval-rollout` | `rw` |
| `POST /v1/index-build-jobs/{job_id}/cancel` | `rw` |
| `POST /v1/index-artifacts/{index_version_id}/promote` | `rw` |
| `POST /v1/access-tokens/{jti}/revoke`, `AdminService.RevokeAccessToken` | `mgmt` |
| `POST /v1/collections/{id}/inference/rollback` (calls no inference endpoint; [inference endpoints](../guides/inference-endpoints.md#roll-back)) | `mgmt` |
| `GET /v1/index-build-jobs/{job_id}`, `GET /v1/retrieval-rollout/{scope_kind}/{scope_id}`, `GET /v1/index-artifacts/{index_version_id}` | any authenticated static-token role |

Under `MUNARIUM_AUTH_MODE=disabled` every role check passes, as it does in the
handlers.

**Heavy (bounded by `MUNARIUM_HEAVY_CONCURRENCY`)**

- REST: `POST /v1/indexes/{shape_ref}/build`, `POST /v1/runbooks/{name}/runs`,
  `POST /v1/runs/{run_id}/steps/{ordinal}/approve`,
  `POST /v1.2/collections/{id}/vocabulary/refresh`,
  `POST /v1/index-artifacts/{index_version_id}/rebuild`,
  `POST /v1/index-artifacts/{index_version_id}/verify`,
  `POST /v1/index-artifacts/backfill`, `POST /v1/sessions/{id}/turns`,
  `POST /v1/sessions/{id}/turns/stream`, `POST /v1.2/answers`,
  `POST /v1/providers/{name}/complete`, `POST /v1/providers/{name}/embed`,
  `POST /v1/providers/{name}/inference-probe`,
  `PUT /v1/sources`, `POST /v1/ingest`, `POST /v1/ingest/batch`,
  `POST /v1/ingest/bulk`, `POST /v1/ingest/bulk/{bulk_id}/chunk` and
  `PUT /v1/evidence/{evidence_id}/bytes`.
- Typed gRPC: `IngestService.PutSource` and `IngestFiles`, `ProviderService.Complete`
  and `Embed`, `RunbookService.RunRunbook` and `ApproveStep`, and `SessionService.Turn`.

Run-step approval is heavy, not control, because approval resumes the run inline.

The native gRPC bridge, `ServerApiService`, replays each call through the REST
router. A bridged call is charged one REST slot and no gRPC slot, and takes the
class of its REST route.

## Metrics

| Metric | Labels |
|---|---|
| `munarium_admission_total` | `plane`, `class` (`ordinary`, `control`, `heavy`), `slot` (`ordinary`, `reserve`) |
| `munarium_admission_shed_total` | `plane`, `class`; every shed is also counted in `munarium_load_shed_total` |
| `munarium_admission_slots`, `munarium_admission_slots_in_use` | `plane`, `pool` (`ordinary`, plus `reserve` and `heavy` when enabled) |

No metric carries a tenant, user or route label. The same pool usage, per
replica, is in `GET /v1/reports/scale` and on the console Health page
([replica diagnostics](clustering.md#replica-pool-and-queue-diagnostics)).

## Known limits

- The console's `/admin` pages and actions stay outside the capture
  middleware's admission, with one exception. The console run-step approval
  confirmation (`POST /admin/runs/{run_id}/steps/{ordinal}/approve/confirm`)
  runs the same inline resume as the REST approval, so it admits itself as
  **heavy** on the REST plane: an ordinary slot plus, when
  `MUNARIUM_HEAVY_CONCURRENCY` is on, a heavy token, and never the control
  reserve. It is admitted after its own role, view-only, CSRF, typed-id and rw
  credential checks and before any store read, and holds the slot until the
  resume finishes. Without a slot it answers 503 with `Retry-After: 1` and an
  "overloaded" notice, approves nothing, and counts in
  `munarium_admission_shed_total{plane="rest",class="heavy"}` and
  `munarium_load_shed_total` like a shed `/v1` request. The list is
  `CONSOLE_HEAVY` in `admission.rs`. The review pages are reads and are never
  admitted.
- A gRPC slot is released when the response head is returned, as in v1.4; a REST
  streaming turn holds its slot until the stream ends.
- The heavy list comes from traced call sites, not from measurement.
