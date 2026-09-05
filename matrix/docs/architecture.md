# Architecture

The shape of Matrix in one sitting, for someone who has to operate or extend
it.

## What Matrix is

The **structured-evidence plane**. Where munarium-server reasons over
documents, Matrix registers formal data sources, materializes governed record
collections from them, executes verified query contracts, **seals the exact
typed evidence an answer used** into the server, and emits typed observations
the ledger reconciles against document-derived claims.

It never calls a model provider, never writes a server table, and never issues
DDL or DML against a customer source.

## Three modes

| Mode | Name | What it does |
|---|---|---|
| **A** | Materialize | Renders source rows into a governed record collection, uploaded to the server as documents with a coverage statement. |
| **B** | Query | Executes a pre-declared contract or semantic view and seals the typed result. |
| **C** | Reconcile | Observes the source, compares against canon, files findings — and, once promoted, proposes claims. |

They are modes of one product, not three products: one registry, one credential
seam, one evidence envelope, one audit vocabulary, one control plane.

## Seven guarantees

Each has at least one conformance scenario; a guarantee with no scenario is a
claim with no test.

| | |
|---|---|
| **G1** | Evidence identity is exact and reproducible. Two hashes, never conflated: `logical_result_hash` over the canonical encoding, `artifact_hash` over stored bytes. |
| **G2** | Replay says what it can honestly say — `sealed_result` or `source_time_travel`, and never claims a snapshot marker resting on a race. |
| **G3** | Provenance is complete: adapter, version, engine, principal, statement id, positions, timings. |
| **G4** | Coverage is stated. A collection says the rows it covers **and** the rows it excludes; truncated never equals complete. |
| **G5** | Derivations recompute from sealed cells, and a derivation over a truncated result is not a total. |
| **G6** | Authorization is the source's, proven rather than trusted: role posture read from the catalog, denied columns removed before rendering, row security reported present or absent. |
| **G7** | A typed refusal, never a degraded answer. See [errors.md](errors.md). |

## Runtime roles

One binary, one image, one selected role. **Role gating is structural**: a
container does not mount the routes it does not serve, which is stronger than a
guard inside each handler and impossible to forget on a new route.

| Role | Serves |
|---|---|
| `control` | Registry, validation, journal, reports, scheduler, `/admin` |
| `query` | `execute` / `verify`, the gRPC data plane, `/mcp` |
| `sync` | Materialization jobs (mode A) |
| `reconcile` | Observation → discrepancy passes (mode C) |
| `all` | Everything — a laptop, or a single-container deployment |

A hung CDC stream cannot consume interactive query capacity, because they are
different containers with different queues, pools, budgets and credentials.

## The four ground rules

**Ground rule 1: `matrix/` never depends on a `server/` crate, and vice
versa.** [`contract/`](../contract/) — JSON Schemas, vendored into
`server/contract/matrix/` — is the entire boundary, and CI fails the build on a
`cargo tree` grep. This is what forced a thin REST client rather than the
official Rust client, which path-depends on three server crates.

**Ground rule 2: server-side work is the server's, and is approved
separately.** The server is a separate product with its own release cadence;
Matrix asks nothing of it that is not already on its REST surface.

**Ground rule 3: rustls only, named exactly, never by prefix.** No crate in
the shipping graph may link OpenSSL. `openssl-probe` is allowed because it is
`rustls-native-certs`' CA-path finder and links nothing, which is why the
check matches the crate name exactly rather than a prefix — a prefix match
would flag it as if `openssl` itself had entered the graph.

**Ground rule 4: a claim that cannot fail is not a claim.** A property
demonstrated once by hand and then left as a paragraph is not evidence; it
must be a registered, running scenario, because a paragraph cannot fail and a
scenario proven once by hand quietly reverts to an untested claim the moment
anything nearby changes.

## How a query actually flows

```
intent ──▶ tenant check ──▶ budget reserve ──▶ load asset
                                                   │
                              compile (allowlist walk over a parsed AST)
                                                   │
                                        adapter.execute / semantic_execute
                                                   │
                                 declared result shape wins over inference
                                                   │
                                          evidence::seal ──▶ munarium-server
                                                   │
                                    EvidenceBlock + journal row
```

Both wire planes — REST and gRPC — call **one** `execute.rs`. The gRPC path
adds progress events and nothing else, so the two cannot drift about policy.

The compiler is an **allowlist walk over a parsed AST**, not string filtering:
undeclared tables and columns, `SELECT *`, subqueries, non-deterministic
functions and any non-`SELECT` are refused; a denied column is refused in
*every* clause; and `:name` is rewritten so no bound value ever reaches the
statement text. A result declaring neither key columns nor a total ordering
cannot be sealed at all.

## Where the code lives

| Crate | What it owns |
|---|---|
| `munarium-matrix-core` | Runtime-free kernel: `canon@1` identity, the closed `RefusalClass`, exact decimals, the SQL and semantic compilers, the planner seam |
| `munarium-matrix-types` | Asset grammar and validation; the DTOs |
| `munarium-matrix-adapter` | The `SourceAdapter` trait and its seams |
| `munarium-matrix-adapter-*` | One per engine. In this repository: postgres, landing, mysql, sqlserver. Databricks, BigQuery, Snowflake, Cube and dbt are Munarium Matrix Enterprise, registered through the same trait |
| `munarium-matrix-store` | Postgres persistence, `matrix` schema, `matrix_owner` role |
| `munarium-matrix-workers` | Mode A/B/C bodies |
| `munarium-matrix-server` | REST 8180, ops 9190, gRPC 50151, `/admin` |
| `munarium-matrix-proto` | The gRPC mirror of the contract, with a drift test |
| `mxctl` | The CLI |

`munarium-matrix-core` depends on no runtime and no driver — asserted by a
`cargo tree` boundary check, so evidence identity stays testable in
milliseconds.

## Further reading

- [errors.md](errors.md) — the refusal vocabulary
- [api/rest.md](api/rest.md) — the REST surface
- [api/grpc.md](api/grpc.md) · [api/mcp.md](api/mcp.md) · [api/planner.md](api/planner.md)
- [guides/admin-ui.md](guides/admin-ui.md) · [security/admin-ui.md](security/admin-ui.md)
- [adapters/build-matrix.md](adapters/build-matrix.md) — what each adapter can actually do
- [ops/runbooks.md](ops/runbooks.md) — resnapshot, retention and legal holds, the circuit breaker
