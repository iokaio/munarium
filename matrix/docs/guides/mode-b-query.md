# Mode B — verified query

Answer a question with a number the reader can check, by executing a
**pre-declared, reviewed** query and sealing its exact typed result as
evidence.

Nobody writes SQL at question time. That is the whole design: a model never
composes a statement, and the surface a caller reaches has no field that
becomes one.

## Three shapes, one execution path

| Asset | What it is | When |
|---|---|---|
| `QueryContract` | One reviewed SQL statement per dialect, with typed parameters | The default. A question with a known shape. |
| `MetricView` | An overlay on a metric layer the SOURCE owns (Databricks, Cube, dbt) | The definitions live in the warehouse and Matrix must not copy them |
| `DataView` | A native single-fact-table aggregate — declared measures and dimensions, no joins | The question needs a grouping no contract offers |

All three execute through one handler: the intent's `kind` selects the path,
and the route segment only says which registry to look in.

## A contract

```yaml
spec:
  source: crm
  description: >-
    Open pipeline (stage is not Closed Won or Closed Lost), in USD, by region,
    as of a date. "Open" is defined here and nowhere else.
  parameters:
    as_of: { type: date, required: true }
  statementByDialect:
    postgres:
      inline: >-
        SELECT region, SUM(amount) AS pipeline_amount, COUNT(*) AS opportunity_count
        FROM opportunities
        WHERE stage <> 'Closed Won' AND updated_at <= :as_of
        GROUP BY region ORDER BY region
  reads:
    tables: [opportunities]
    columns: [region, amount, stage, updated_at]
  result:
    columns:
      region: { type: string, key: true }
      pipeline_amount: { type: decimal, scale: 2, unit: USD, additivity: additive }
      opportunity_count: { type: int64, additivity: additive }
    columnOrder: [region, pipeline_amount, opportunity_count]
    orderBy: [region]
    derivations:
      total_pipeline: { op: sum, over: pipeline_amount }
  policy:
    deniedColumns: [owner_email]
```

Four things worth noticing:

**`description` is where a word gets its meaning.** "Open" means what this
contract says, and an answer that quotes the number inherits that definition.

**`reads` is separate from `result`** because a statement reads SOURCE columns
and returns RESULT columns — this one reads `amount` and returns
`pipeline_amount`. Deriving the allowlist from `result` refused every aliased
aggregate until it was separated.

**`key: true` and `orderBy` are what make a result sealable at all.** A result
declaring neither key columns nor a total ordering has no stable identity, and
`result_not_identifiable` refuses it.

**`deniedColumns` is refused in every clause**, not just the projection — a
column you cannot return is also a column you cannot filter on.

## Verified questions are the regression suite

```
mxctl verify open-pipeline-by-region     # exit 3 if any question failed
```

They run on apply, and again whenever the source's schema fingerprint moves. A
failure means the contract **no longer means what it claimed when it was
reviewed** — which is a different and more serious thing than a broken query.

`verify` answers 200 with per-question outcomes even when questions fail: a
failed question is a result, not a transport error. The exit code is what lets
CI tell a broken contract from a broken command.

## Semantic views verify differently, and it matters

A `MetricView` or `DataView` executes **only after a passing verification on
record**, and the definition's fingerprint is re-read **before every execute**.
A definition that moved since it was verified is refused
`metric_view_changed` — not because the answer would be wrong, but because
nobody can say whether it would be.

## What comes back

An `EvidenceBlock`: the rows, their `row_id`s, the manifest, and an
`evidence_id` sealed into munarium-server. An answer cites
`[evidence/<id>#r0003]`, and a reader resolves it.

**Two hashes, never conflated.** `logical_result_hash` over the canonical
encoding is the identity — the thing a replay must reproduce.
`artifact_hash` over the stored bytes is a different claim about a different
object.

Declared derivations recompute from the sealed cells, so a total in an answer
is checkable. A derivation over a **truncated** result is not a total, and is
refused as one.

## The compiler refuses more than you expect

An allowlist walk over a parsed AST — not string filtering. It refuses
undeclared tables and columns, `SELECT *`, subqueries, non-deterministic
functions, and any non-`SELECT`. `:name` is rewritten so **no bound value ever
reaches the statement text**.

This is why a [planner's](../api/planner.md) generated SQL frequently will not
survive: a generative surface writes all of those. That is the design working.

## When it refuses

`not_covered`, `metric_not_covered`, `metric_view_changed`, `schema_drift`,
`budget_exceeded`, `result_too_large`, `result_truncated`,
`deadline_exceeded` — see [errors.md](../errors.md), including
[which refusals spend budget](../errors.md#which-refusals-spend-budget).
