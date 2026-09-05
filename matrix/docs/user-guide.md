# Munarium Matrix — user guide

How to get from an empty registry to a sealed answer: author an asset, apply
it, prove it against the real source, and read what comes back — including a
refusal.

## The assets

Everything Matrix does is declared in advance by an **immutable, versioned
asset**. Runtime input fills declared parameters or chooses from a closed
vocabulary; it never widens the contract.

| Asset | What it declares | Guide |
|---|---|---|
| `DataSource` | Adapter kind, connection metadata, `credentialRef`, egress allowlist, the role posture the source principal must have, limits, sync policy | below |
| `QueryContract` | One reviewed SQL statement per dialect, typed parameters, the tables and columns it may read, the result schema, verified questions | [mode B](guides/mode-b-query.md) |
| `DataView` / `MetricView` | Closed lists of measures and dimensions over a native aggregate or a provider's semantic layer | [mode B](guides/mode-b-query.md) |
| `ClaimMapping` | Row identity, properties, aliases, temporal meaning, shadow or authoritative mode, authority scopes | [mode C](guides/mode-c-reconcile.md) |

The committed examples under [`fixtures/assets/valid/`](../fixtures/assets/valid/)
are complete and annotated; start from
[`datasource.crm.yaml`](../fixtures/assets/valid/datasource.crm.yaml) and
[`contract.open-pipeline.yaml`](../fixtures/assets/valid/contract.open-pipeline.yaml).
Every file under [`fixtures/assets/invalid/`](../fixtures/assets/invalid/) is
one fail-closed rule, named for the finding it produces.

A `DataSource` names a **reference** to a credential, never a value:
`credentialRef: matrix-crm` resolves at call time from
`MUNARIUM_MATRIX_SECRET_MATRIX_CRM` (or an `env:NAME` / `file:PATH` form). The
validator refuses a literal secret in an asset.

## Apply it

`mxctl` talks to a running Matrix through `MUNARIUM_MATRIX_URL` (default
`http://localhost:8180`) and `MUNARIUM_MATRIX_TOKEN`.

```text
mxctl validate -f datasource.crm.yaml     # local; no service needed
mxctl apply -f datasource.crm.yaml        # rw token; idempotent by name@version
mxctl apply -f contract.open-pipeline.yaml
mxctl list datasources --all
```

An applied version is immutable. A correction is a new version; re-applying
identical bytes answers `unchanged`. Exit code **3** from `validate` means
findings — a broken asset, as distinct from a broken command.

## Prove it against the source

Applying proves syntax and declared invariants. It does not prove the source
is reachable, that the principal has the posture the asset claims, or that a
business question still returns what it did when it was reviewed. Three
operations do that, in order:

```text
POST /v1/datasources/crm/probe          # reachable, right now?
POST /v1/datasources/crm/introspect     # the role posture, read from the catalog
mxctl verify open-pipeline-by-region    # the contract's verified questions; exit 3 on a failure
```

`introspect` refuses a superuser, an owner, or a role holding DML — the
posture is read from the catalog, never taken on trust from the asset. `verify`
answers 200 with per-question outcomes even when a question fails: a failed
question is a result, not a transport error.

## Run it

| Mode | Command | What happens |
|---|---|---|
| A — materialize | `mxctl sync crm` | Enqueues one job per authorization class on the `sync` role; watch `/admin/runs` or `GET /v1/journal`. |
| B — query | `POST /v1/contracts/{name}/execute` with a `QueryIntent` | Binds, compiles, executes, canonicalizes and seals; returns an `EvidenceBlock` with an `evidence_id`. |
| C — reconcile | `mxctl reconcile captable` | Enqueues a pass; in shadow it files findings and touches canon not at all. |

The full REST surface is in [api/rest.md](api/rest.md); the same `execute`
is reachable over [gRPC](api/grpc.md) and as an [MCP tool](api/mcp.md).

## Read a refusal

A refusal is an answer, not an error:

```json
{ "class": "exhausted", "code": "budget_exceeded", "message": "...", "retry_after_seconds": 1800 }
```

Switch on **`class`** — six closed values that will not grow without a
contract MAJOR. Read **`code`** as an operator; a code you do not know falls
back to its class. The registry is [errors.md](errors.md), including
[which refusals spend budget](errors.md#which-refusals-spend-budget).

Two refusals worth knowing before the first apply: `adapter_not_available`
means the asset is valid but this build carries no adapter for the kind it
names — the Databricks, BigQuery, Snowflake, Cube and dbt adapters are
Munarium Matrix Enterprise, and
[adapters/build-matrix.md](adapters/build-matrix.md) says exactly what each
adapter in this repository can do. `metric_view_changed` means a semantic
definition moved since it was verified: verify it again.

## Operating it

[guides/admin-ui.md](guides/admin-ui.md) is the operator console;
[ops/runbooks.md](ops/runbooks.md) is what to do — and what not to do — when
a checkpoint gaps, evidence retention comes due, or the circuit breaker opens.
