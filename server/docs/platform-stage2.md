# Experimental Stage 2 action records

Server's S2-A implementation consumes the unchanged hub candidate at
`2fb118909633264fad23ede2e7c9942aba374312`, bundle
`8aca66588c87107a0c7a7720c68f921dfd2bdcaecf6433728afa4b1c51420aa6`.
The [exported candidate](../contract/platform-stage2-v1/README.md) remains proposed;
this implementation is not contract acceptance or production qualification.
[ADR 0013](https://github.com/iokaio/munarium-platform/blob/implement/stage2-packets/docs/decisions/0013-action-record-admission.md)
records the admission and recovery choices. Stage 1 bytes and behavior are unchanged.

## Admission and API

Use the existing [platform enrollment and mTLS profile](platform-stage1.md).
The current independently ratified authority artifact must contain both
`identity:<audience>` and `action-records:<audience>` bindings. The former supplies
Warden identity verification; the latter deserializes to the closed
[`ActionPolicy`](../src/munarium-core/src/platform_actions/mod.rs):

| Field | Meaning |
|---|---|
| `schema_version`, `profile` | `1`, `stage2-single-cell-v1` |
| `scope` | Qualified `domain`, `tenant`, `deployment`, `cell`; tenant/deployment must match enrollment |
| `streams` | 1–128 unique registrations: `stream_id`, logical `producer`, actual enrolled `service`, current positive `generation`, allowed `kinds` |
| `readers` | At most 32 enrolled service names permitted to read |
| `recovery` | At most 256 exact historical permits; empty by default |

Requests use `POST /v1/platform/{tenant}/records` or
`ServerApiService/PlatformRecords`, including the existing SDK `platform_records`
method. The same handler requires a current recipient-bound signed `chain`, the
actual enrolled mTLS peer and `action-records:<tenant>` resources. Writes require
`propose` identity scope and the peer's `record` scope; reads require `read` in both.
Only service origins record or read through this adapter. Missing, malformed or
removed policy refuses, including an otherwise exact retry. Caller-provided policy
and transport identity fields are not accepted. The authority/checkpoint fence
remains held through the operation.

The closed body is `{"chain":["signed assertion"],"action":{...}}`:

| `action.operation` | Other fields | Result |
|---|---|---|
| `action-archive` | `record`: canonical JSON string | Artifact `digest`, `ledger_position`, `record_type` |
| `action-append` | `event`: canonical JSON string | Candidate `event-ack` after durable append |
| `action-lookup` | `operation_id` | Pinned `ledger_position`, `artifacts`, `events` for the operation |
| `action-transition` | `transition_id` | Pinned position, archived activation and activation events |
| `action-source-head` | `stream_id`, `generation` | `sequence` and `event_digest`, or `0`/`null` when empty |

Gate's registered service archives requests and decisions; Council's archives
approvals and activation assertions. Stream registrations bind each event kind's
logical producer to its actual transport service. An artifact receipt is not an
event acknowledgement. Neither receipt authorizes execution or asserts that a
target effect occurred.

## Custody, ordering and recovery

Action artifacts and events occupy distinct subjects in the existing protected
physical tenant/version. This reuses the Stage 1 additive ledger mapping; no schema
reset, applied migration change, or new data-plane access is needed. Canonical
JSON, closed shapes, qualified scope, intrinsic digests and stored prerequisites
are checked before append. A request fixes operation intent and attempt context;
changed bytes under the same identity conflict. Distinct lifecycle kinds can share
an operation. Each attempt has one immutable claim, grant, consumption, predispatch,
send intent and initial outcome. Reconciliation appends evidence without overwriting
that outcome. Worker takeover and send retries are separate Gate responsibilities.

Approval events reference archived approvals. Claims follow recorded approval;
grants reference claims; consumption references grants; predispatch binds consumption;
send intent binds the exact Server predispatch acknowledgement. Outcomes refer to
send intent and reconciliation retains a prior outcome reference. Activation events
bind their archived transition and exact participant/artifact sets. A missing
prerequisite refuses; there is no pending-success acknowledgement.

Expected-head transactions serialize races and revalidate after CAS conflicts.
Immutable event identity and stream/generation continuity are checked independently
of the global ledger position. Receipt time is persisted, so reconnect and exact
retry return the same entire acknowledgement. A store outage is never success.

An old source generation requires a separately ratified recovery permit with
`recorder`, original `producer`, `stream_id`, `generation`, `kind`, exact
`event_digest`, original `claim_id`/`claim_event_digest`, and occurrence `cutoff`.
The generation must be older than the retained registration. Claim-created must
match its attested digest; subsequent events require the original claim already
recorded. Only claim/grant/consumption/predispatch/send/outcome/reconciliation kinds
are recoverable. Original bytes remain unchanged, with current recorder identity
stored separately. Governance attests retained committed producer evidence; Server
does not independently inspect a remote Gate journal. Recovery never grants dispatch.

## Validation and limits

From `server/`, run:

```console
cargo test --locked -p munarium-core -p munarium-store-mem --test platform_actions
cargo test --locked -p munarium-store-pg --test platform_actions -- --ignored
```

The latter requires an explicit isolated `MUNARIUM_TEST_DATABASE_URL`. It exercises
the same lifecycle/deny assertions against PostgreSQL, concurrent duplicate writers,
reconnect with a changed receipt clock, tenant isolation and bounded recovery.
Rust independently consumes all 19 canonical candidate records/digests and verifies
the exported file and aggregate hashes. It does not claim to reproduce every
contextual case in the hub's fixture oracle.

The [live transport test](../../clients/python/tests/test_platform_actions_live.py)
uses temporary synthetic signing keys/certificates and actual memory/PostgreSQL
Server processes, testing REST/gRPC parity, producer roles, resource separation,
current binding removal and immutable retries. Set `MUNARIUM_PLATFORM_TEST_BINARY`
and an isolated `MUNARIUM_PLATFORM_TEST_DATABASE_URL`, then run it from
`clients/python/` with pytest. CI selects both the live PostgreSQL test and transport
test explicitly. An omitted live environment is skipped, never reported as success.

The [CI inventory baseline](../tools/gate-ci-baseline.json) includes the two
Stage 2 path triggers, Rust memory/vector and explicit PostgreSQL commands, and
the combined Stage 1/Stage 2 transport command. Its fingerprint was audited against
the pre-Stage 2 workflow at `7cc0c97fe124d446064bb8cf0dfcb6d868d04987`: removing
only those additions restores the previous pinned workflow. The
[catalog regression controls](../tools/test_gate_catalog.py) refuse removal of
either transport suite, the new triggers or Rust coverage, and removal of the
PostgreSQL `--ignored` selection. Run the complete CI formatting entry point from
`server/` when changing orchestration:

```console
python tools/gate_catalog.py runner.regression catalog.regression catalog.equivalence format
```

This adapter scans the protected ledger and is not scale qualified. Stored producer
assertions do not independently prove human eligibility, evidence authenticity,
current activation, safe target credentials, absence of duplicate effects or
restore safety. Council/Registry activation, Gate/Warden enforcement and independent
Harness target observations remain their owning packets' obligations.
