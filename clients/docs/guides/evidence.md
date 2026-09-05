# Sealed evidence: resolving a citation

A structured answer from Munarium cites rows, not documents:
`[evidence/<id>#<row>]`. The `<id>` is a sealed evidence artifact —
typed rows Munarium Matrix executed against a governed source and sealed
into the server with two hashes (the logical result and the stored bytes) —
and `<row>` is a row identity inside it (the key columns joined with `|`, or
a position when the result declared no keys). This guide is how a client
reads one back. Sealing is deliberately **absent** from every client: a
manifest is a statement about work the *sealer* did, and an SDK that offered
it would invite an application to assert provenance it cannot vouch for.

## The two reads

Every client exposes an **evidence plane** with two operations, both
tenant-scoped, both audited on the server (a resolution records that a read
happened, never what was read), and both refused unless the caller's
capability token **dominates** the authorization class the manifest declares.

| Operation | Route | Returns |
|---|---|---|
| get the manifest | `GET /v1/evidence/{id}` | the contract's `EvidenceManifest`, unwrapped — kind, source, contract or semantic-provider version, snapshot marker, completeness, the two hashes, retention |
| read rows | `GET /v1/evidence/{id}/rows?from=&limit=` | canonical CSV rows, capped at 1000 per page, in the sealed order |

A `200` on the manifest already means *committed*: a pending artifact answers
`409`, a purged one `410` (`evidence-expired`) — the metadata survives a purge
so a citation keeps resolving to *what this was* rather than to nothing. A
legal hold blocks deletion and never reading.

The mgmt-only `GET /v1/evidence/{id}/accesses` (who resolved it, and how it
went) and the `evidence` **report** (`GET /v1/reports/evidence?window=` —
per-layer refusals and completeness) sit beside the plane in each client's
reports surface.

## Per language

The plane is the same shape in all four clients; only the accessor differs.

```rust
// Rust — the EvidencePlane trait on the client (REST and gRPC transports)
let manifest = client.evidence().evidence("ev-52b58e0a52c64723b56013647b49d28d").await?;
let rows = client.evidence().evidence_rows("ev-52b5…", Some(0), Some(200)).await?;
```

```python
# Python — client.evidence, sync and async
manifest = client.evidence.get("ev-52b58e0a52c64723b56013647b49d28d")
rows = client.evidence.rows("ev-52b5…", start=0, limit=200)
```

```csharp
// .NET — IEvidencePlane
var manifest = await client.Evidence.GetAsync("ev-52b5…", ct);
var rows = await client.Evidence.RowsAsync("ev-52b5…", 0, 200, ct);
```

```java
// Java — synchronous and CompletableFuture forms
Evidence.EvidenceRows rows = client.evidence().evidenceRows("ev-52b5…", Params.of(0, 200));
Reports.EvidenceReport report = client.reports().evidenceReport("7d");
```

Over **gRPC** the manifest read is served (`evidence`); the row read is
REST-only in this version and the gRPC transport answers `unsupported` for it
by name rather than pretending — the same rule as every other REST-only route.

## Reading a citation end to end

1. Parse `[evidence/<id>#<row>]` from the answer (or take `evidence_refs`
   from its typed assertions block).
2. Get the manifest. Check `completeness` before quoting a total: a
   `truncated` result supports "at least", never "exactly".
3. Read rows from the cited row's neighbourhood, or the whole table when it
   is small; the row id in the CSV is the `#<row>` you started from.
4. Keep the manifest's `logical_result_hash` beside anything you persist —
   it is the identity a later reader will compare against, and it does not
   change when the same logical result is re-serialised.

## What the manifest tells you about *how* the rows were produced

- `versions.query_contract` — a verified query contract executed the
  statement the contract's author wrote; the model never wrote SQL.
- `versions.semantic_provider` — a **metric view** or a **native data view**
  answered a bounded semantic intent (measures, dimensions, equality
  filters chosen from a closed list); the definition it ran under was
  fingerprinted at verification and re-read before this execution, so a
  changed definition could not have produced these rows unnoticed.
- `snapshot_marker` — the engine position the rows were read at when the
  source can state one (`pg_current_snapshot()` on Postgres, a Delta version
  on a change-feed read); `None` where a read cannot honestly pin one.
- `replay_level` — what re-running would give you: `sealed_result` (these
  bytes), `source_time_travel` (the source can answer the same question at
  the same position), or less.

## Refusals you will meet

| Problem type | Meaning |
|---|---|
| `evidence-not-found` | no artifact by that id in this tenant |
| `evidence-forbidden` | your token does not dominate the manifest's class |
| `evidence-pending` | sealed but not yet committed |
| `evidence-expired` | purged after its retention, or by an operator |
| `evidence-on-hold` | (mgmt delete only) a legal hold blocks the purge |

Retries change nothing about these: an evidence id is immutable, and the
answer to "may I read it" depends only on your token and the artifact's state.
