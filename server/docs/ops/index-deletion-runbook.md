# DBA runbook: physically deleting a collection's index data

**Policy:** no munarium-server API can delete index data — anywhere, under any
role. Runbook removal is soft (visibility only); `retireOld` reclaims only
*inactive* index versions' chunks and keeps manifests. This document is the
**only sanctioned path** to physically remove a collection's data, and it is
a manual PostgreSQL-administrator operation gated by your change-control
process (ticket + DBA + a second approver recommended).

Why it works this way: every collection owns one LIST partition of
`collection_chunks` (`collection_chunks_p_<id>`). Detaching and dropping the
partition is an O(1) catalog operation — no long DELETE, no vacuum debt —
and the application's queries physically cannot express it.

## 1. Preconditions (verify ALL before touching anything)

- [ ] A change ticket authorizes the deletion, naming the collection.
- [ ] Every runbook referencing the collection is `status = 'removed'`
      (`SELECT runbook_ref, status FROM runbooks WHERE tenant_id = $tenant;`
      then check specs — step 2 below maps runbooks → collections).
- [ ] A recent base backup / PITR point covers the current state.
- [ ] You are on the correct database (cell) for the tenant.

## 2. Identify the collection and its partition

```sql
-- The collection row (by tenant + name):
SELECT id, name, shape_ref, access_level, compartments, status
  FROM collections
 WHERE tenant_id = :'tenant' AND name = :'collection_name';

-- Runbooks whose yaml references it (visual check of spec.collections):
SELECT runbook_ref, status FROM runbooks
 WHERE tenant_id = :'tenant' AND yaml LIKE '%' || :'collection_name' || '%';

-- Its partition name: 'collection_chunks_p_' || replace(ltrim(id, 'col-'), '-', '_')
-- Size and row counts before you act:
SELECT c.relname, pg_size_pretty(pg_total_relation_size(c.oid)) AS size,
       (SELECT count(*) FROM collection_chunks
         WHERE collection_id = col.id) AS rows
  FROM pg_inherits i
  JOIN pg_class c ON c.oid = i.inhrelid
  JOIN pg_class p ON p.oid = i.inhparent
  JOIN collections col ON c.relname = 'collection_chunks_p_' || replace(ltrim(col.id, 'col-'), '-', '_')
 WHERE p.relname = 'collection_chunks' AND col.tenant_id = :'tenant' AND col.name = :'collection_name';
```

## 3. Retire the collection logically first

```sql
-- Stop the app from ever searching it again (sessions skip retired rows):
UPDATE collections SET status = 'retired'
 WHERE tenant_id = :'tenant' AND name = :'collection_name';

-- Deactivate its index pointer (no active version may remain):
UPDATE index_versions SET active = false
 WHERE tenant_id = :'tenant'
   AND collection_id = (SELECT id FROM collections
                         WHERE tenant_id = :'tenant' AND name = :'collection_name');
```

## 4. Optional archive, then detach and drop

```sql
-- Optional: archive the partition before dropping.
--   pg_dump -t collection_chunks_p_<id> -Fc -f <ticket>-<id>.dump <db>

-- DETACH CONCURRENTLY cannot run inside a transaction block.
ALTER TABLE collection_chunks DETACH PARTITION collection_chunks_p_<id> CONCURRENTLY;

-- Point of no return (without the archive):
DROP TABLE collection_chunks_p_<id>;
```

## 5. What is deliberately KEPT

Do **not** delete these — provenance must keep resolving:

| Kept | Why |
|---|---|
| `collections` row (status `retired`) | the id is referenced by manifests, sessions, interactions |
| `index_versions` rows + manifests | every past ProvenanceEnvelope names an index version |
| `collection_sources` rows | the audit of what fed the collection |
| `sources` rows | content-addressed; may feed OTHER collections |
| `sessions` / `session_turns` / `interactions` | the uid-attributed audit trail |
| ledger events | append-only by design |

## 6. And the blobs

Dropping the partition removes **derived index data only**. The `sources`
rows stay in Postgres (see the table above — they are deliberately kept), and
the **document bytes themselves are not in Postgres at all** unless
`MUNARIUM_SOURCE_STORE=pg`: they live in whatever backend the environment
configured, laid out at the logical path ingest wrote —
`{tenant}/{filename}`, the same string a runbook's `filenamePrefix` matches.
Dropping partitions and even deleting `sources` rows leaves those bytes
untouched.

If — and **only** if — the change ticket explicitly extends to destroying the
source documents (a legal purge, not storage reclaim), remove them in the
backend, per backend:

| Backend | How |
|---|---|
| `az` | `az storage blob delete-batch --account-name <acct> -s sources --pattern '<tenant>/<prefix>/*' --auth-mode login` (one blob: `az storage blob delete`). Your principal needs data-plane RBAC (Storage Blob Data Contributor) — control-plane roles do not grant it and shared-key auth should be disabled on the account (the example module disables it). |
| `s3` | `aws s3 rm --recursive s3://<bucket>/<tenant>/<prefix>/` |
| `gcs` | `gcloud storage rm -r gs://<bucket>/<tenant>/<prefix>/` |
| `file` | delete the directory under `MUNARIUM_FILE_ROOT` (`<root>/<tenant>/<prefix>/`) |
| `pg` | `DELETE FROM source_blobs WHERE tenant_id = :'tenant' AND blob_name LIKE :'tenant' \|\| '/<prefix>/%';` — the one backend the same DB session as the partition drop can also clean |

Two cautions before any of these:

- **Sources are shared.** A source may feed OTHER collections
  (`SELECT collection_id FROM collection_sources WHERE source_id = …`).
  Deleting its bytes silently breaks every future rebuild of every collection
  that binds it, not just the one you retired. Verify each path is bound only
  by the retired collection first.
- **Scope the prefix precisely.** Matching everywhere in this system is a
  literal `starts_with`, so `<tenant>/north` also sweeps
  `<tenant>/northgate-archive/`. Use the trailing slash.

Deleted bytes are recoverable only from your storage-side protections (blob
soft delete / bucket versioning / backups) — the application keeps no copy.

## 7. Verify

```sql
-- Partition gone:
SELECT 1 FROM pg_class WHERE relname = 'collection_chunks_p_<id>';   -- 0 rows
-- Parent intact, other collections unaffected:
SELECT count(*) FROM collection_chunks;                              -- other partitions' rows
-- App behavior: searches naming the collection now return not-found /
-- collection-retired; envelopes referencing old index versions still resolve
-- their manifests via GET /v1/indexes or the info endpoints.
```

## 8. Rollback

Restore from the archive taken in step 4:

```sql
pg_restore -d <db> <ticket>-<id>.dump          -- recreates the table standalone
ALTER TABLE collection_chunks ATTACH PARTITION collection_chunks_p_<id>
  FOR VALUES IN ('<col-id>');
UPDATE collections SET status = 'active'
 WHERE tenant_id = :'tenant' AND name = :'collection_name';
-- Re-activate the desired index version, or re-run the runbook pipeline.
```

Without an archive, rollback = re-ingest sources (still present) and re-run
the runbook's build/cutover steps — the deterministic chunker/embedder
reproduce identical chunk ids for identical bytes.
