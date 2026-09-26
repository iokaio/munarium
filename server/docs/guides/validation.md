# Local validation and JSON persistence qualification

Run these PowerShell 7 commands from `server/`:

```powershell
./test.ps1
./test.ps1 -Postgres -BlackBox -Platform -Cluster
./gates.ps1
py -m unittest discover -s tools -p test_validation.py
py -m unittest discover -s tools -p test_gate_catalog.py
py tools/check_gate_equivalence.py
./tools/test-json-features.ps1
./tools/test-json-features.ps1 -Postgres
./tools/test-datastore-permissions.ps1
```

`-All` selects every test tier. `-Enterprise` remains accepted as an alias for
`-Platform`; new examples use `-Platform`. The default profile clears the test
database URL for workspace tests and restores the caller's value afterward.
Database integration functions that return early without a URL are not evidence
of PostgreSQL coverage. The JSON qualification database tests use explicit
`--ignored` selection and require a URL rather than returning early.

## Shared gates and CI inventory

[gate-catalog.json](../../tools/gate-catalog.json) defines stable portable step
IDs, argument arrays, working directories, prerequisites, features, dependencies,
required status and executor adapters. Local receipts register its commands
through [gate-catalog.ps1](../../tools/gate-catalog.ps1); independent CI steps
invoke [gate_catalog.py](../../tools/gate_catalog.py). Python uses its running
interpreter; PowerShell uses `py`. Neither constructs shell command strings.
The local gate profile now includes the Matrix publisher self-test, DiskANN
clippy/tests and all-features cargo-deny coverage that CI already required.

Database provisioning, database-scoped test environments, server ownership,
document comparisons, notices snapshots, and the embedded consumer remain explicit
platform adapters. Linux keeps its CI service and shell process orchestration;
local validation keeps disposable owned resources and receipts. The independent
embedded, cargo-deny action and Terraform jobs remain automatic and unchanged.
This is shared portable coverage, not a claim that local gates run Terraform or
that a Windows result qualifies the hosted Linux environment.

[gate-ci-baseline.json](../../tools/gate-ci-baseline.json) records the reviewed
pre-P16 revision, original migrated CI blocks and expected catalog semantics.
[check_gate_equivalence.py](../../tools/check_gate_equivalence.py) checks exact
catalog identities, feature arguments, dependency closure, required status and
boundary rules. It substitutes the reviewed catalog calls and hashes the rest
of the workflow, preserving triggers, paths, permissions, runner assignments,
services, independent jobs and their commands. New runner/checker self-tests are
explicit additions. It also requires byte-identical root agent guidance.

The baseline is review evidence, never a second execution source. Intentional
future coverage changes must update it after reviewing old/new commands and
prerequisites; do not regenerate it merely to clear a failure. Its exact text
comparison deliberately requires review even for an unrelated workflow comment.
It cannot prove semantic equivalence of arbitrary programs. The original blocks,
boundary controls and actual command runs supply that evidence. Original Matrix
paths ran from root; their catalog paths run from `server/` and resolve to the
same publisher and vendored directory. The publisher's `--check` rejects a
missing directory, retaining the original prerequisite check.

Boundary commands reject nonzero `cargo tree`, empty/malformed output and missing
requested roots. Retrieval and migration checks recurse over their source trees
and reject missing trees. Fixtures cover every crate rule, nested retrieval
imports, destructive DDL, resolution failure and valid controls. The migration
check remains a lexical rule, not a SQL parser or proof of additive schema
semantics. A dependency scan checks the selected normal graph, not every possible
feature/platform combination. Semantic answer verification and human review
remain separate obligations.

## Outcomes and receipts

Each selected required step has a stable ID, prerequisite resolution, timestamps,
commands and one outcome: `passed`, `failed`, or `not_run`, with a reason code.
Unselected tiers appear in `not_requested`. Failures take precedence:

| Exit | Meaning |
|---|---|
| 0 | Every selected requirement passed and source identity stayed unchanged |
| 1 | A check, configured prerequisite, runner or cleanup failed |
| 3 | No failure, but required coverage was unavailable/incomplete or source changed |

For example, a missing executable or preloaded PostgreSQL image produces
`not_run`; an available Docker command that cannot reach its configured daemon
fails. A native nonzero exit is retained in its command record (including native
interruption codes). Missing `cargo-deny` makes the `gates` profile incomplete.
Later successful parsing cannot erase a preceding command failure. Independent
checks continue; dependent checks record `dependency_not_passed`.

Receipts default to a unique `scratch/validation/<run-id>/receipt.json`.
`-ReceiptPath` chooses another new path; an existing receipt is never overwritten
by a different run. Schema version 1 records the run/profile, required steps,
tool executable hashes, PowerShell version, source commit and pre/post SHA-256
input manifests, duration, resource ownership, outcome and completion marker.
The input manifest includes tracked repository files and explicitly named new
runner/test inputs. It does not discover arbitrary untracked files or read ignored
secrets. Receipts and build outputs are excluded. These hashes identify the
checkout inputs, not a hermetic build: downloaded dependencies, toolchain state
and external execution environments need separate qualification.
The notices checker runs against a temporary copy of these declared Server
inputs, then deletes that copy. Its recursive workspace discovery cannot include
unrelated ignored scratch checkouts.

An initial incomplete receipt is atomically replaced after step updates. The
terminal marker follows execution, cleanup and the final source hash. Forced
termination may leave only the incomplete receipt. A completed receipt may still
record failed or unavailable requirements; completion is not success.

Consumers must supply expected identity, not trust the newest file in a folder:

```powershell
py tools/check_validation_receipt.py <receipt-path> --run-id <expected-run-id> `
  --profile <expected-profile> --source-sha256 <expected-input-manifest-sha256> `
  --required-step <first-expected-step> --required-step <next-expected-step>
```

The checker rejects incomplete/stale identities, changed source, duplicate or
missing steps, inconsistent passes and exit summaries. Waiver references are
separate metadata and cannot change actual outcomes. Command arguments redact
database credentials and tokens. Raw command output remains in ignored local
logs; those logs are not sanitized publication evidence. Review/redact an export
separately before sharing it. Invalid command-line binding or initialization
fails with exit 1 and may have no usable receipt.

## Resource ownership

The database tiers create a uniquely named container from the pinned pgvector
image used by CI, with an ephemeral loopback port and disposable anonymous
storage. Preload that image before selecting database coverage:

```powershell
docker pull pgvector/pgvector:pg16@sha256:ccc6e83d6e35e931dc7c5def2022729d5a6c370318d099181995567ff1fb4d6b
```

The runner never recreates a developer database or uses the developer Compose
project. Live servers bind fresh loopback ports and use test-only configuration.
Cleanup stops recorded child handles and removes only the created container ID
and its anonymous storage. There is no executable-name/port reaping. A forced
kill can bypass cleanup: the incomplete receipt retains acquired resource IDs
for inspection, not permission to stop matching unrelated resources. Caller
environment values and working directory are restored during normal unwinding.

## Linux filesystem qualification

The separate `datastore-linux-permissions` profile exercises the
[restricted filesystem fixture](datastore.md#linux-artifact-and-scratch-permissions)
using Docker, with no database or provider. Its receipt records build and each
serving case separately. Windows AppContainer is not requested by this profile.

## JSON feature matrix

`test-json-features.ps1` resolves default and `json-arbitrary-precision` graphs
separately, records `cargo tree -e features -i serde_json`, and checks that only
the second enables `serde_json/arbitrary_precision`. It runs DTO/native gRPC byte
envelopes, evidence discriminators, real datastore build/seal/reopen tests and
both directions of artifact exchange in separate Cargo test processes. With
`-Postgres`, it also runs validated ledger writes, version/evidence JSONB reads,
runbook result writers/readers, and session turns read through a new state/pool.
The authoring suite also runs in each configuration: JSON numeric values must
become YAML numbers when generating shapes and runbooks.
No paid model or remote service is needed.

The fixtures use independent expected values, including literal
`$serde_json::private::Number` keys alone, nested and with ordinary keys, plus
literal `$serde_json::private::RawValue` keys; signed
and unsigned integer limits; arrays, null, Unicode, a 16 KiB string and 24 levels
of nesting. Null in an `Option<Value>` request field retains the existing absence
semantics. JSONB comparisons are semantic, not object-key-order comparisons.
Decimals in arbitrary JSON follow the existing default binary64 precision;
the fixtures use exactly representable fractions. Exact high-precision decimals
are strings. Datastore parameters accept only the existing typed scalars, and
canonicalization rejects fractions and integers outside signed 64-bit range.
String chunk metadata, citation identity and canonical parameter bytes must
survive across configurations; arbitrary JSON is not a manifest requirement.

**Qualification resolution (2026-09-23):** the initial tests reproduced two
failures with locked `serde_json` 1.0.151 and `arbitrary_precision`: a literal
`{"$serde_json::private::Number":"123"}` became the number `123`, and runbook
JSONB containing a nonnumeric value under that key failed to decode.

The shared `munarium_api_types::json::LiteralValue` adapter now decodes JSON
containers from raw tokens before constructing `Value`. Object keys stay keys,
including nested marker-like keys. It applies to DTO `Value` fields and the
ledger metadata/evidence, runbook result, and session JSONB readers. The adapter
retains scalar decoding for each serde_json configuration, bounds nesting at
128 levels, and rejects malformed input. It is for JSON boundaries, not a
general YAML or binary-format deserializer. Scalar precision beyond the declared
default policy is not an additional API guarantee.

Both feature configurations pass the qualification, including literal pre-fix
JSONB fixtures inserted independently of the adapter, read after reopening, and
decoded through response DTOs. Artifact exchange passes in both directions.
No stored values, migrations or artifact hashes change; writes remain ordinary
JSON. The wire DTO crate explicitly enables `raw_value` for this decoder; the
production default still does not enable `arbitrary_precision`.

The all-features workspace run also exposed JSON numbers leaking as marker
objects into generated YAML. Authoring now translates JSON text into YAML's
own value tree before emission. Existing materialization assertions and an
independent scalar/literal-key control cover this boundary in both graphs.

The `json-arbitrary-precision` features remain qualification-only. These checks
qualify the named paths, not every use of `serde_json::Value` in dependencies or
provider integrations. Direct `Value` decoding outside the adapter still has
the upstream marker ambiguity; a successful compile alone is insufficient.

The session fixture exercises the no-completion writer/read path. Typed evidence
blocks have discriminator and precision controls; that does not qualify every
provider-generated answer or hierarchy workflow. The native gRPC fixture checks
the actual protobuf byte envelope and DTO decode, not a network listener. The
broader conformance tiers remain separate requirements. The shared catalog
preserves the CI inventory; local receipts still establish only their selected
profile and environment, not hosted CI or infrastructure qualification.
