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
  pinned chunk provenance, vocabulary defaults/terms/revisions, retained
  collection-governance snapshots,
  runbook definitions and runs/steps, sessions/turns, access-token audit,
  idempotency keys, interactions, gate findings, and — with
  `MUNARIUM_SOURCE_STORE=pg` — document and sealed-evidence bytes.
- **NOT in the database**: document and sealed-evidence bytes when
  `MUNARIUM_SOURCE_STORE` is an object-store backend (`az`, `s3`, `gcs`, `file`),
  and datastore search artifacts. Object storage has its own
  soft-delete/versioning. A database restore needs the matching object bytes or
  revisions; a live bucket can contain later replacements or lack purged evidence.
  Source deletion remains the [DBA change-ticket procedure](index-deletion-runbook.md);
  sealed evidence has its own management-gated retention purge. Reconcile those
  operations after restoring to a point before them. Datastore artifacts are
  content-addressed. Rebuilding a historical generation requires the corresponding
  retained index records or
  source revision; current source bytes alone cannot reproduce overwritten
  historical text. A restored catalog row whose artifact is missing fails
  verification rather than serving a different generation. Back up the artifact
  store when historical citations must remain readable.

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

Source-denial policy and guarded command claims are additional restore gates.
Keep the restored service isolated until the independently exported
[source journal](source-retention.md#restore-upgrade-and-rollback) and
[command recovery](command-recovery.md#activation-and-compatibility) state have
been reconciled. Restoring old bytes never clears an approved denial; a missing
authoritative journal cannot be treated as evidence that no denial existed.

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

Server 1.3 adds migrations 0035–0040 for usage evidence, governance, monetary
accounting, reconciliation, guarded commands and source retention. Follow the
[1.3 upgrade/rollback requirements](../guides/server-1.3.md#database-and-configuration-upgrade).
A restored pre-upgrade backup may omit irreversible command effects or later
source denials; keep it isolated and reconcile authoritative records before
serving. Prefer a compatible roll-forward after policy activation.

For Server 1.2 upgrades, preserve the pre-upgrade database as well as source and
artifact storage. Migrations 0032/0033 add provenance and vocabularies; 0034 adds
governance in 1.2.1. An older Server's migrator rejects unknown migrations, so
rolling back the image alone is insufficient, including from 1.2.1 to 1.2.0.
Restore the matching database backup before starting the older image; do not
delete migration-history rows. See the [verified rollback record](../../CONTAINER.md#versions-and-verification).

Sessions opened after the restore point are absent. Token issuance and
revocation records written after that point
are lost. A signed, unexpired token can still verify: the optional deny-list
rejects recorded revocations, not unknown token IDs. Restore the required
revocation state before admitting callers. Runbook applications after the restore
point must be re-applied from their version-controlled definitions. Database
rows and partitions removed by the [index-deletion runbook](index-deletion-runbook.md)
after the restore point may exist again and require the approved removal to be
reapplied; externally deleted object bytes are not restored by the database.

Keep a restored instance isolated from callers until post-backup access changes,
token revocations (when deny-list checking is enabled), logical removals and
authorized deletions have been reconciled. A database restore does not roll back
or purge object-store artifacts, hydrated caches, exports or external backups.
Server 1.3 retains a [source-denial journal](source-retention.md), but restore
does not automatically replay later denials. Export every page independently,
replay holds before denials, requeue applicable cleanup and verify completeness
before serving. This is not an all-copy erasure guarantee. Preserve and reconcile
[guarded command claims](command-recovery.md) as well; a missing claim in an old
backup is not evidence that the command never executed. The [retention inventory](retention-inventory.md) separates retained
history from current eligibility and records the remaining restore coverage.
