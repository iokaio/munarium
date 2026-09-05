# T0 — the adversarial fixture

A small operational database that stands in for a customer system of record.
Everything awkward in it is awkward **on purpose**: each trap is a way a naive
implementation passes its own tests and is still wrong in production.

Read [`sql/02-crm-fixture.sql`](sql/02-crm-fixture.sql) as the answer key — every
trap is commented where it sits, with what it breaks. This file is the index.

## Loading it

```powershell
docker compose up -d postgres      # runs sql/ as init, into database `matrix`
```

The **caller chooses the database**. Compose loads the fixture into `matrix`;
a live deployment can give it its own `crm` database. The fixture used to
contain a `\connect matrix`, which silently put it in the wrong database
anywhere but compose — caught on the first live run (2026-08-28) and removed.

## The traps

| # | Where | What it breaks if you get it wrong | Defended by |
|---|---|---|---|
| 1 | `amount NUMERIC(18,2)` | A currency column read through a float is wrong in the last cent. | `value.rs` decimal is exact; `postgres_types_map_onto_the_closed_canon_set` |
| 2 | `900000.50` (id 8) | A reader that renders `900000.5` changes the logical result hash for the same data. | canon@1 decimal rule + `render.rs` |
| 3 | `notes` NULL vs `''` (ids 1, 2) | Conflating them files a false discrepancy in reconcile. | `canonical_csv` distinguishes them; `evidence.rs` tests |
| 4 | two rows share `updated_at` (ids 6, 7) | An exclusive watermark with no tie-break drops one row **forever**, silently. | `checkpoint.rs` `validate_sync`; fixture `sync.watermark.yaml`; asserted live as well as in compose |
| 5 | `updated_at` straddling midnight UTC (ids 1, 2) | A date filter that ignores offsets moves rows between periods. | `TIMESTAMPTZ` + canon@1 instant encoding |
| 6 | `owner_email` | A denied column must not reach a statement, a log, a journal row, or a manifest. | `policy_denied_column_never_appears_anywhere`; `compile.rs` checks every clause; three `policy.denied-column-in-*` fixtures |
| 7 | RLS `emea_only` on `matrix_reader` | Source-native authorization must actually filter, not be assumed. | `role.mustBe.subjectToRowSecurity`, proven from the catalog |
| 8 | `matrix_bad_reader` owns the table and holds DML | A role that "looks fine" until you ask the catalog. Ownership implies RLS bypass. | `introspect()` posture refusal; asserted live as well as in compose |
| 9 | `J. Rowntree` / `Jane  Rowntree` (ids 51, 58) | Ambiguous identity must file a finding and merge **nothing**. | `AmbiguityPolicy` has no `pick_best` variant; the alias table in `mapping.captable.yaml`; `reconcile.ambiguous_identity_never_merges` |
| 10 | holder 43 = 90500, corpus says 90000 | A planted document-vs-register disagreement. Mode C must surface it with **both** citations and pick no side. | `reconcile.discrepancy_carries_both_evidence_sides` |
| 11 | holder 44 `effective_date` 2025-11-15 | A backdated *legitimate* update is a new fact about an old period, not a correction. | `classify_change` → `requires_review` |
| 12 | `opportunity_tags` (ids 1 and 8 have two tags) | Joining and summing `amount` double-counts. | contract declares its grain; the compiler refuses undeclared tables |
| 13 | `shares NUMERIC(38,0)` | Share counts overflow a double. | exact decimal end to end |
| 14 | `matrix_owner` has no rights in `public` | The store's isolation claim must be a tested contract, not a convention. | `registry.matrix_owner_cannot_write_public` |
| 15 | `Tomas Berg` — declared in `mapping.captable.yaml`, in no `holdings` row | A ledger claim the register cannot support must be `missing_in_source`, and only a read that would have RETURNED the row may say so. | `reconcile.absent_declared_holder_is_missing_in_source`; `ReconcileOptions::source_complete` |

## Row keys

Every observation cites its row as the declared key values joined with `|` —
`43|7` for holder 43 on company 7 — whatever adapter produced it. The postgres
adapter keys a record by its first projected column and the landing adapter by
the manifest's keys, and until 2026-08-29 `observe` passed the adapter's choice
through, so the same row would have been cited two ways from two sources.

## Trap 9 was undefended, and nothing said so (2026-08-29)

This table said trap 9 was "defended by `reconcile.rs`". It was not, and the way
it was not is worth keeping.

`observe` emitted exactly one entity candidate per row, key-derived, at
confidence 1.0. Holders 51 and 58 have distinct `holder_id`s, so they produce
`shareholder.51` and `shareholder.58` — two ordinary rows. No input to the
pipeline could make them ambiguous. `reconcile.rs` did hold a correct ambiguity
rule and a unit test for it, but the test hand-built an observation with two
candidates, and **nothing in the system produced one**. The trap was planted, the
defence was written, and the two were never connected.

What hid it: `mapping.captable.yaml` declared `resolver: terminology_alias` and
`minConfidence: 0.95`, so the mapping read as though identity resolution was
configured. Neither field was consulted by any code — the resolver was never
read at all, and `compare` used a hard-coded 0.5. That asset's own header comment
warns that "an ignored field in an asset is a lie waiting to be believed", four
lines above two of them.

Closed by a declared alias table, and a validator that refuses the resolver
without the table and the table without the resolver, so the shape that hid
this cannot be written again.

## The landing export (2026-08-30)

`landing/` is the same kind of world as an immutable CSV export: eight
opportunities with a `manifest.json` carrying the schema, keys, the file's
sha256 and the snapshot id. It is the input to the live mode-A **blob**
scenario: uploaded to a blob container under `landing/crm/`, then read back by
Matrix through its managed identity as a `store: az` source. Its bytes are part
of the fixture hash a live run records, beside the SQL.

## Two grants that only matter on real infrastructure

```sql
GRANT matrix_bad_reader TO CURRENT_USER;
GRANT CREATE ON SCHEMA crm TO matrix_bad_reader;
```

On a laptop the bootstrap role is a superuser and both are no-ops. On Azure
Database for PostgreSQL the admin login is **not** a superuser, and without
them `ALTER TABLE ... OWNER TO` fails with *permission denied for schema crm* —
so trap 8 would never get planted and the posture test would pass vacuously.

This is the clearest single argument for the live tier: compose is happy either
way, and the difference is a security check that silently stops testing
anything.
