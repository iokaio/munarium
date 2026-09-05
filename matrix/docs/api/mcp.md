# The MCP toolset

`POST /mcp` on the REST port, beside `/v1`.

Matrix speaks [Model Context Protocol](https://modelcontextprotocol.io) so an
agent can reach it through the transport it already expects — **without gaining
one capability it did not already have.** That constraint is the design, and it
is worth stating first because the obvious thing to build here is the wrong
one.

## What this is not

There is no `run_sql` tool. There is no `query` tool that takes a string. There
is no tool whose arguments become part of a statement.

The agent-facing pattern the market settled on — Google's MCP Toolbox for
Databases, and every warehouse vendor's server since — is **pre-declared
parameterized tools**, and Matrix already had the pre-declaration:

| Asset | What it declares | What the tool schema becomes |
| --- | --- | --- |
| `QueryContract` | `spec.parameters`, with types and optional `allowedValues` | an object schema; a bounded parameter becomes an `enum` |
| `MetricView` | closed lists of measures and dimensions | `measures` / `dimensions` arrays whose items are `enum`s of exactly what the asset declares |
| `DataView` | the same, over a native aggregate | as above |

An agent cannot ask for a measure the asset does not declare, because **the
schema does not have one**. The bound is in the schema, not only in the
validator — a well-behaved client will not even offer the wrong value, and a
badly-behaved one is refused by the same validator every other plane goes
through.

## A transport, not an authority

`tools/call` builds the same `QueryIntent` a REST or gRPC caller would send and
hands it to the same `execute_intent`:

- the same bearer token and the same tenant,
- the same authorization class and compartments,
- the same budget unit spent,
- the same evidence sealed into munarium-server,
- the same journal row — with `via: "mcp"`, so an operator can see which plane
  a query arrived on.

There is no privileged in-process path and there is no second policy to keep in
step. A role that does not serve the query plane answers **404** on `/mcp`,
exactly as it does on `/v1/contracts/{name}/execute`.

## The answer is evidence, not prose

A tool result carries the block's `evidence_id` alongside its rows, so an agent
that quotes a number can cite `[evidence/<id>#r0003]` and a reader can resolve
it through munarium-server. A refusal comes back as an MCP **tool error**
carrying its typed code — not as an empty result — because an agent that cannot
tell *no rows* from *not allowed* will report the wrong thing, confidently.

Authentication failure is the one thing answered as a JSON-RPC error rather
than a bare 401: an MCP client reads the envelope, and a status code with no
envelope reaches it as a transport fault it cannot explain.

## Methods

One JSON-RPC 2.0 request, one response. Streamable HTTP without the streaming:
every method here answers in a single round trip, and a transport that promised
SSE it never uses would be a larger surface for no gain.

| Method | Behaviour |
| --- | --- |
| `initialize` | Reports protocol `2025-03-26`, `tools.listChanged: false`, server name and version, and instructions that state the no-free-SQL rule and the citation form. |
| `ping` | `{}`. |
| `tools/list` | Every tool this **tenant's** applied assets declare. Another tenant's assets are not merely hidden, they are not loaded. |
| `tools/call` | Executes one. Unknown tool name → `INVALID_PARAMS` naming it. |
| `notifications/*` | No-op; `notifications/initialized` is the one that matters. |

Tool names are `<kind>.<asset name>` — `contract.open-pipeline-by-region`,
`metricview.pipeline-by-region`. The dot keeps the two apart without inventing
an escaping rule, because an asset name is already lowercase with no dots and
`metadata.name` enforces it.

## Use

```bash
curl -sS http://localhost:8180/mcp \
  -H 'authorization: Bearer mxdev' \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
```

```bash
curl -sS http://localhost:8180/mcp \
  -H 'authorization: Bearer mxdev' \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
        "name":"contract.open-pipeline-by-region",
        "arguments":{"as_of":"2026-07-01"}}}'
```

The result's `content[0].text` carries the rows and the evidence id; `isError`
is `true` when Matrix refused, and the text then names the refusal class and
code.

## Tested by

`grpc.tier_mcp_lists_declared_tools_and_a_call_seals_evidence` in the
conformance crate — it lists the tools over real HTTP, calls one, and asserts
the result carries a **citable evidence id**, which is the property that
separates this from a chat integration. It runs in `test.ps1 -BlackBox` against
compose. Five unit tests cover the schema
generation, including that a `QueryContract`'s `allowedValues` reaches the tool
schema as an `enum`.

## Related

- [gRPC data plane](grpc.md) — the service-to-service `Execute`.
- [Verified query contracts](../guides/mode-b-query.md#a-contract) — what a tool is built from.
