# The REST API

Port **8180**. The machine-readable spec is
[`openapi.json`](openapi.json), generated from the code and checked against it
on every run of `test.ps1` and in CI — `openapi --check` compares the binary's
own document against the committed copy and exits 1 on drift, so this page and
that file cannot quietly describe a service that no longer exists.

Ops (`/metrics`, liveness) is on **9190**, its own listener.
[gRPC](grpc.md) is on **50151** and serves `Execute` alone.

## Shape

- **Bearer token** on everything but the meta routes. Three roles: `ro`, `rw`,
  `mgmt`.
- **Every failure is problem+json** with a `matrix:` slug, and a
  [typed refusal](../errors.md) travels in the body rather than being flattened
  to a message.
- **Every WRITE is journaled**, redacted by default. Reads are not: they change
  nothing, and journaling them would bury the writes an auditor came for.
- **Role gating is structural.** A `sync` container answers 404 on the
  registry because it does not mount those routes at all.

## Meta — every role

| Route | |
|---|---|
| `GET /healthz` | Liveness. Unauthenticated. |
| `GET /readyz` | Readiness; 503 while draining, so a load balancer stops routing before in-flight work finishes. |
| `GET /version` | Build, role, contract version — and the **lockstep** answer: `server_version`, `target_server_version`, `server_compatibility`. Anything but `exact` means an evidence id minted here may not resolve there. |
| `GET /openapi.json`, `GET /docs` | This surface, machine- and human-readable. |

## Registry — `control`

Assets are **immutable once applied**. A correction is a new version; apply is
idempotent by `name@version`, and re-applying identical bytes answers
`unchanged: true` rather than minting a second version.

| Route | |
|---|---|
| `POST /v1/assets` | Apply any kind — the kind is sniffed by parsing, so one route takes them all. |
| `POST /v1/assets/validate` | The same validators, without applying. `valid` is a flag beside the findings: three codes are **advisory**, so a valid asset can carry findings. |
| `GET/POST /v1/{datasources,contracts,metricviews,dataviews,mappings}` | List (`?all_versions=true` for history) and apply. |
| `GET /v1/{kind}/{name}` | The applied YAML, **verbatim** — the bytes Matrix stored, not a re-serialisation of a parse. |

## Sources — `control`

| Route | |
|---|---|
| `POST /v1/datasources/{name}/probe` | Reachable, right now? A refusal is an **answer** here — `reachable: false` with a typed reason — not a 5xx. |
| `POST /v1/datasources/{name}/introspect` | Prove the role posture and read the schema as the **effective principal** sees it. Refuses a superuser, an owner, or a role holding DML: the posture is read from the catalog, never taken on trust from the asset. |
| `POST /v1/datasources/{name}/sync` | **Enqueue** a materialization run — one job per authorization class. A sync takes minutes and must survive the caller hanging up. |
| `POST /v1/datasources/{name}/planner/ask` | Ask a [conversational planner](planner.md). Executes nothing. |

`/healthdata` reports **registration, not connectivity**. Probing every source
on a health call would turn a health endpoint into an outbound-traffic
amplifier, which is why reachability is the explicit per-source act above.

## Execution — `query`

| Route | |
|---|---|
| `POST /v1/{contracts,metricviews,dataviews}/{name}/execute` | One handler for all three: the intent's `kind` selects the path, and the route segment only says which registry to look in. |
| `POST /v1/{contracts,metricviews,dataviews}/{name}/verify` | Run the asset's verified questions — its regression suite. |

**Verify answers 200 even when questions fail.** A failed question is a
*result*, not a transport error, so the body carries per-question outcomes and
the caller inspects `failed`. `mxctl verify` exits **3** on a non-zero `failed`,
which is what lets CI tell a broken contract from a broken command.

A semantic view executes only after a **passing verification on record**, and
the definition fingerprint is re-read before every execute: a definition that
moved is `metric_view_changed` until someone verifies it again.

**Where the time went.** Every `execute` answer carries a `Server-Timing`
header (since 2026-08-30):

```
Server-Timing: total;dur=48, source;dur=11, seal;dur=29, matrix;dur=8
```

`total` is the wall clock around the whole call; `source` is the engine's own
statement window as the adapter recorded it; `seal` is canonicalize + manifest
+ the one round-trip into the server; `matrix` is what is left — bind,
compile, budget, transport — and is the only one Matrix could make smaller. It
is a header rather than a body field because the body is the vendored
contract's `EvidenceBlock`, and a measurement is not something an answer
cites. The same three numbers land on the journal row (`duration_ms`,
`source_ms`, `seal_ms`), which is how the measurement harness pairs a
server-side turn with the execute it triggered.

## Reconciliation — `control`

| Route | |
|---|---|
| `POST /v1/mappings/{name}/run` | Enqueue a pass. |
| `GET /v1/mappings/{name}/promotion` | Mode, state, the two gates with their minimums, and the latest run. |
| `GET /v1/mappings/{name}/gate-history` | The gates over time — the monitoring surface for the thresholds. |
| `POST /v1/mappings/{name}/promote` \| `/demote` | Every gate is checked **at the moment of the decision**, against the latest completed run — not at reconcile time, where a slipping number would silently turn writes off and on. A decision id is required. |
| `POST /v1/mappings/{name}/rollback` | Undo by **supersession**, never deletion. History is not rewritten, and the rollback claims carry `origin.kind = "rollback"` so a reviewer sees both moves. |

## Audit — `mgmt`

`GET /v1/journal` — every operation, with `via` naming the plane it arrived on
(`api`, `grpc`, `mcp`, `admin-ui`). Payloads are **redacted at write time**: a
parameter value is customer data. An `execute` row carries `duration_ms` and,
since 2026-08-30, `source_ms` and `seal_ms` — the two pieces of that time that
are not Matrix's own (see *Where the time went* above).

## Elsewhere

`/admin` — the [operator console](../guides/admin-ui.md), `control` role only.
`/mcp` — the [MCP toolset](mcp.md), `query` role only.

`/admin` is **not** in the OpenAPI spec, deliberately: it is a human surface
outside the API contract, like the server's own console.

`/mcp` **is** in it, as one path with one verb — JSON-RPC puts the method in
the body, and describing the individual MCP methods here would invent an
OpenAPI shape for a protocol that already has its own schema, giving two
descriptions to drift apart. `tools/list` is the authoritative description of
what a deployment offers, and it is generated from the assets.
