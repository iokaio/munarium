# Backup and restore (PITR)

`munarium-server` keeps its system of record in PostgreSQL, so backup and
restore are PostgreSQL operations. This page says what a database restore
covers and does not, and gives the point-in-time restore procedure in both
shapes the shipped deployment code produces: a managed PostgreSQL service
with built-in PITR, and a CloudNativePG (CNPG) cell as the Helm chart creates
it. No RPO/RTO numbers are quoted here — a number without a captured run in
your own environment is a guess. Drill the procedure, then record what you
measured.

## What is covered by a database restore, and what is not

- **In the database, restored together**: the ledger and every projection
  (claims, anchors, promises, counters, digests), shapes, provider
  configs, chronology rules, collections + chunk partitions + indexes,
  runbook definitions and runs/steps, sessions/turns, access-token audit,
  idempotency keys, interactions, gate findings, and — with
  `MUNARIUM_SOURCE_STORE=pg` — the document bytes themselves.
- **NOT in the database**: document BYTES when `MUNARIUM_SOURCE_STORE` is an
  object-store backend (`az`, `s3`, `gcs`, `file`), sealed evidence bytes,
  and datastore search artifacts. Object storage has its own
  soft-delete/versioning; a point-in-time database restore against a LIVE
  container is consistent as long as objects are never deleted — which is
  the shipped posture (no delete API exists; physical deletion is
  [index-deletion-runbook.md](index-deletion-runbook.md), a DBA
  change-ticket procedure that must then be replayed after any restore to a
  point before it). Datastore artifacts are content-addressed and
  regenerable from ledger + sources: a restored catalog row whose artifact is
  missing fails verification loudly rather than serving something else.

## Where point-in-time recovery comes from

- **Managed PostgreSQL** (Azure Database for PostgreSQL Flexible Server,
  Amazon RDS, Cloud SQL, …): continuous backup is built in, retention is a
  server setting (commonly 7–35 days), and a restore always creates a **new**
  server rather than rewinding the old one.
- **CNPG**: the chart's `Cluster` has a streaming replica and **no WAL
  archive**. Replication is availability, not recovery — it cannot undo a
  bad write. PITR requires `spec.backup.barmanObjectStore` pointing at object
  storage (the Barman Cloud plugin) plus a `ScheduledBackup`; until you add
  them, this page has nothing to restore from. Add them before production.

## The procedure

1. **Stop writes.** Scale the deployment to zero
   (`kubectl -n munarium scale deployment/munarium-server --replicas=0`) or
   stop routing at your ingress. Leave PostgreSQL running — the restore reads
   its backups.
2. **Restore to a NEW database.** Managed: the provider's point-in-time
   restore into a new server (for example
   `az postgres flexible-server restore --source-server <server> --name <server>-drill --restore-time "<UTC ISO-8601 inside the retention window>"`,
   or `aws rds restore-db-instance-to-point-in-time`). CNPG: a new `Cluster`
   with `bootstrap.recovery` naming the source's object store and a
   `recoveryTarget.targetTime`.
3. **Point ONE instance at it.** Set `MUNARIUM_DATABASE_URL` to the restored
   database (in the chart the URL comes from the CNPG-generated `-app`
   secret, so a recovered cell's own secret is the natural source) and start
   a single replica.
4. **Verify.** `/readyz` answers `ok`; `GET /v1/versions/{id}/facts?as_of_seq=<pin>`
   returns the pre-restore-point slice; a claim written AFTER the restore
   point is absent; `SELECT max(tenant_seq) FROM ledger_events` is ≤ the
   value at the restore time; one runbook run resumes correctly
   ([clustering.md](clustering.md)'s orphaned-run recovery); and a few
   `GET /v1/sources/{id}` rows still resolve to bytes — this is the check
   that catches an object-store container the restore did not move.
5. **Record.** Wall-clock from step 2 to a green step 4 (the measured RTO),
   the restore-point lag you chose (the demonstrated RPO), and every
   surprise, in your own operations record — a procedure that has never
   been executed is a hypothesis.
6. **Cut over or tear down.** Repoint the release at the restored database
   and scale back up, or delete the drill server and repoint to the
   original.

## After a restore

Sessions opened after the restore point are gone (`session-not-open` to
their clients); capability tokens issued after it are unknown and fail
verification; runbook applications after it must be re-applied from git,
which is why `mmctl apply` belongs in your CI rather than in a terminal.
Anything the [index-deletion runbook](index-deletion-runbook.md) removed
after the restore point exists again and must be removed again.
