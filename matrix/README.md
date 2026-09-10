# Munarium Matrix

The **structured-evidence plane** of Munarium: a separately deployed Rust
service that registers formal data sources, materializes governed record
collections from them, executes verified query contracts against them, seals
the exact typed evidence an answer used into munarium-server, and produces
typed observations the server's ledger can reconcile against document-derived
claims.

The server stays the governance authority. Matrix never talks to a model
provider, never writes a server table, and never issues DDL or DML against a
customer source.


> **Status.** Munarium Matrix source version **1.0.0** includes the runtime
> and its four roles (`control`, `query`, `sync`, `reconcile`), the asset grammar, the refusal registry, the query compiler, materialization,
> reconcile, and the adapters for the databases most applications already run. It is validated by a
> conformance registry of scenarios that run on every push, and by a compose tier that exercises the
> HTTP, gRPC, MCP and admin planes against a real Munarium Server.
>
> **What is validated, and how.** Every adapter row in
> [docs/adapters/build-matrix.md](docs/adapters/build-matrix.md) states what that adapter can do,
> what it refuses, and what it has actually been run against — a claim there is backed by a
> conformance tier or it is marked as not run. The refusal registry
> ([docs/errors.md](docs/errors.md)) is kept honest by a test. `SCENARIOS.md` is generated, so it
> cannot drift from what exists.
>
> **What is not in this repository.** The adapters for analytics platforms — Databricks, BigQuery,
> Snowflake, Cube and dbt — are part of **Munarium Matrix Enterprise**, a separate proprietary
> product that builds on this one through the same public adapter interface. An asset naming one of
> them validates here and is refused at execution with `adapter_not_available`, naming what it
> needs. See [NOTICE](NOTICE) and [SUPPORT.md](../SUPPORT.md).
>
> **Known limitations at 1.0**, stated rather than implied: this release commits to the wire
> contract, the asset grammar, the refusal registry and the adapter interface under semantic
> versioning. It does not claim that every planned capability is finished. Each release publishes
> its open items in the release notes: [CHANGELOG.md](CHANGELOG.md).

## About this repository

Matrix versions independently of Server, whose current published image is 1.1.1.
Build Matrix from this checkout using its Dockerfile or Rust workspace. The
Server's `iokaio/munarium` image contains Server and `mmctl`; it does not contain
Matrix or `mxctl`. Matrix's compatibility check distinguishes an exact version
match from a compatible major; see [the user guide](docs/user-guide.md).

## What is here

```
matrix/
├── contract/          THE cross-tree boundary: JSON Schemas + examples, vendored into server/
├── src/
│   ├── munarium-matrix-core/            pure kernel — canon@1 identity, refusals, compiler, rendering
│   ├── munarium-matrix-types/           asset grammar + contract DTOs + validators
│   ├── munarium-matrix-adapter/         the SourceAdapter seam, capabilities, parameter binding
│   ├── munarium-matrix-adapter-landing/ manifest-driven immutable exports (CSV/JSONL)
│   ├── munarium-matrix-adapter-postgres/ role-posture proof, snapshot/watermark reads, execute
│   ├── munarium-matrix-adapter-mysql/    the second SQL engine behind the same seam
│   ├── munarium-matrix-adapter-sqlserver/ the third
│   ├── munarium-matrix-server-client/   thin client for munarium-server + a conformant MockServer
│   ├── munarium-matrix-store/           Postgres persistence, schema matrix.*
│   ├── munarium-matrix-workers/         sync (A), query (B), reconcile (C)
│   ├── munarium-matrix-server/          the binary: REST :8180, ops :9190
│   ├── munarium-matrix-client/          Rust client for Matrix's own API
│   └── munarium-matrix-cli/             mxctl
├── conformance/       scenarios that run in-process and over HTTP
├── deploy/            the Helm chart
├── fixtures/t0/       the adversarial fixture, with every planted trap documented
└── docs/
```

## Quickstart

From the repository root, enter `matrix/`. The test scripts require PowerShell 7
and the Rust toolchain selected by [rust-toolchain.toml](rust-toolchain.toml).
Docker with Compose is needed for the database and service checks.

```powershell
cd matrix
# offline: unit tests, boundary checks, contract validation. No database.
./test.ps1

# + store and conformance against a compose Postgres
docker compose up -d postgres
./test.ps1 -Postgres

# the gates a reviewer expects
./test.ps1 -Gates
```

Run the service:

```powershell
docker compose up -d --build  # REST :8180, gRPC :50151, ops :9190
$env:MUNARIUM_MATRIX_TOKEN = 'mxdev'
cargo run -p munarium-matrix-cli -- version
cargo run -p munarium-matrix-cli -- validate -f fixtures/assets/valid/datasource.crm.yaml
cargo run -p munarium-matrix-cli -- apply -f fixtures/assets/valid/datasource.crm.yaml
cargo run -p munarium-matrix-cli -- list datasources
```

These are development credentials and ports. `cargo run` executes `mxctl` from
source; building the container does not install that command on your host.
Contracts and mappings must be applied before verification or reconciliation.
See [the user guide](docs/user-guide.md) for the full asset workflow.

To exercise evidence sealing, also start a Server and configure Matrix's peer.
From the same `matrix/` directory:

```powershell
$env:MUNARIUM_SERVER_IMAGE = 'iokaio/munarium:1.1.1'
$env:MUNARIUM_MATRIX_COMPOSE_SERVER_URL = 'http://munarium-server:8080'
docker compose --profile server up -d --build
```

The base stack alone can manage assets; operations that need the Server to seal
evidence fail without that peer. The optional Server publishes port 18080.

## The five ideas worth knowing

**1. Two hashes, never conflated.** `logical_result_hash` answers "is this the
same answer?"; `artifact_hash` answers "are these the same bytes?". A CSV and a
Parquet serialization of one result share the first and differ in the second.
`canon@1` (`contract/canonicalization.schema.json`, implemented in
`munarium-matrix-core/src/canon.rs`) is the normative rule, and a test asserts
the code and the schema agree.

**2. A result that cannot name its rows cannot be sealed.** A contract declares
key columns or a total `orderBy`. Under keys the result hashes as a multiset, so
row order is irrelevant; under position it hashes as a sequence and needs a
total ordering. Declaring neither is a refusal, at apply time.

**3. Refuse before degrade.** Every failure is a typed [`Refusal`] with a
closed `class` the server switches on and an open `code` an operator reads.
There is no "best effort" path: a truncated result is marked truncated and
cannot back a completeness claim; an ambiguous identity files a finding and
merges nothing; a schema change refuses until a human records a decision id.

**4. String concatenation is not an implementation.** A contract's SQL is
parsed, walked against an allowlist of declared tables, columns and
deterministic functions, and rewritten to positional placeholders. `SELECT *`,
subqueries, `now()`, and any non-`SELECT` statement are refused. A denied
column is refused in *every* clause, not just the projection.

**5. Evidence is regulated data.** Every artifact carries an authorization
equivalence class, and a session resolving a citation must **dominate** it —
at least the access level, and every compartment. Domination is a conjunction,
tested in both directions.

## The boundary

`matrix/` never depends on a `server/` crate and `server/` never depends on a
`matrix/` crate. The only shared thing is `contract/`, vendored into
`server/contract/matrix/` and drift-checked on both sides.

This is enforced, not asked for: `test.ps1` and CI both run a `cargo tree` grep
that fails if any server crate appears in the graph — which also catches "just
use the official Rust client", since that client path-depends on server crates.

`munarium-matrix-core` additionally depends on no runtime at all: no `sqlx`, no
`reqwest`, no `axum`, no `tokio`. That is what lets the evidence-identity rules
be tested exhaustively in milliseconds.

## Testing

Two tiers, both free.

| Tier | Where | Cost | When |
|---|---|---|---|
| offline | `./test.ps1` | $0 | every change |
| compose | `./test.ps1 -Postgres -BlackBox` | $0 | every change, and in CI |

The compose tier also stands up the MySQL and SQL Server engine tiers from
compose profiles. Live tiers against analytics platforms belong to Munarium
Matrix Enterprise and are not part of this repository.

## Environment

| Variable | Meaning |
|---|---|
| `MUNARIUM_MATRIX_ROLE` | `control` \| `query` \| `sync` \| `reconcile` \| `all` |
| `MUNARIUM_MATRIX_HTTP_ADDR` / `_OPS_ADDR` | listeners; default `0.0.0.0:8180` / `0.0.0.0:9190` |
| `MUNARIUM_MATRIX_GRPC_ADDR` | query-role listener; default `0.0.0.0:50151`; `disabled` turns it off |
| `MUNARIUM_MATRIX_DATABASE_URL` | schema `matrix`, role `matrix_owner` |
| `MUNARIUM_MATRIX_AUTH_MODE` | `static` (default) \| `disabled` |
| `MUNARIUM_MATRIX_STATIC_TOKENS` | `token:tenant:role,...` where role is `rw` \| `ro` \| `mgmt` |
| `MUNARIUM_MATRIX_SERVER_URL` | munarium-server base URL |
| `MUNARIUM_MATRIX_SERVER_TOKEN_REF` | reference to the server token; resolved at call time |
| `MUNARIUM_MATRIX_TARGET_SERVER_VERSION` | lockstep target; a **major** mismatch refuses to start |
| `MUNARIUM_MATRIX_SECRET_<NAME>` | a `credentialRef` resolves here (also `env:` / `file:`) |
| `MUNARIUM_MATRIX_MAX_CONCURRENCY` | per-role ceiling |
| `MUNARIUM_MATRIX_EGRESS_DEFAULT_DENY` | default `true` |
| `MUNARIUM_MATRIX_LOG` / `_LOG_FORMAT` | filter; `plain` \| `json` |

Roles gate the surface **structurally**: a `sync` container answers 404 on the
registry because it does not mount those routes at all.

## Ports

REST 8180, gRPC 50151, ops 9190 — no clash with the server's 8080/50051/9090 on one
laptop. The compose Postgres is on 5434 for the same reason.
