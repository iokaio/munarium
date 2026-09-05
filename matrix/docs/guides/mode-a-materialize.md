# Mode A — materialize

Render rows from a formal source into a **governed record collection** the
server can retrieve over, with a coverage statement attached.

Use it when the answer needs the *records themselves* in the retrieval corpus —
a cap table, a holdings register, an opportunity list — rather than an
aggregate computed on demand. For aggregates, use [mode B](mode-b-query.md).

## What you declare

A `sync:` block on the `DataSource`:

```yaml
spec:
  sync:
    entity:
      table: opportunities
      keyColumns: [id]
    modes: [Snapshot, Watermark]      # or [Cdf] / [Cdc] where supported
    watermark:
      column: updated_at
      tieBreak: id
    projection: [id, name, amount, region, updated_at]
```

**`keyColumns` is the citation's shape.** `row_key` is rendered from them
joined with `|`, whatever adapter produced the row — so a citation does not
change shape when a deployment moves from Postgres to a landing export.

**`projection` is an allowlist, not a convenience.** A column absent from it is
never read, and a column the source's policy denies is removed **before**
rendering rather than masked after.

## Running it

```
mmctl matrix sync <source>        # enqueues; one job per authorization class
```

It **enqueues**. A sync takes minutes and must survive the caller hanging up,
so the call returns job ids and the work happens on the `sync` role. Watch it
on `/admin/runs` or `GET /v1/journal`.

One job per authorization class is not an optimisation: **a collection carries
exactly one class**, so a multi-class source needs one run each, and fanning
out at submit time keeps each independently retryable.

## Which mode, and why it matters

| Mode | Position it keeps | Sees a delete? |
|---|---|---|
| `Snapshot` | none — reads everything | n/a |
| `Watermark` | a column value + tie-break | **No** |
| `Cdf` | the Delta version | Yes |
| `Cdc` | the WAL LSN | Yes |

**A watermark cannot see a delete.** It re-reads rows whose watermark moved;
a row that was removed simply stops appearing, and nothing in the run says so.
That is why Databricks mode A refuses `sync_not_covered` without the Change
Data Feed: a collection built by watermark would report coverage it does not
have.

Where a change feed exists, a delete arrives as a **tombstone document** at the
row's own path, so the collection records the removal rather than quietly
lacking the row.

## Reading the coverage statement

Every run reports four numbers, and they are separate on purpose:

- `records_read` — what the source returned
- `records_rendered` — what became a document
- **`records_excluded`** — dropped by policy or drift
- `documents_uploaded` / `documents_skipped`

`records_excluded` is never folded into a total. **G4 says a collection states
the rows it covers AND the rows it excludes**, and a page that summed them away
would be undoing the guarantee.

`documents_skipped` being high is normal on a resnapshot: the server's
idempotency store recognises bytes it already holds.

## When it refuses

| | |
|---|---|
| `sync_not_covered` | The adapter cannot materialize in the mode asked for. |
| `checkpoint_gap`, `cdc_checkpoint_gap`, `cdf_checkpoint_gap` | The position is behind what the source can replay. See [the resnapshot runbook](../ops/runbooks.md#1-resnapshot-a-collection). |
| `schema_drift` | The source's shape moved. Fail-closed by design. |

Full vocabulary: [errors.md](../errors.md).

## What mode A does not do

It does not compare anything to canon — that is [mode C](mode-c-reconcile.md).
It does not compute aggregates — that is [mode B](mode-b-query.md). And it
never issues DDL or DML against the source; the posture is proven read-only at
connect time and refuses a role holding DML.
