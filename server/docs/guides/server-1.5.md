# Server 1.5: release and upgrade (unreleased)

**1.5.0 is unreleased.** The source version on the ClearView branch is 1.5.0;
no image, tag or package has been published, and installation defaults remain
on the published [1.4.0 image](server-1.4.md). This page records the upgrade
and rollback procedure that applies to a 1.5.0 candidate. The
[ClearView plan](../design/README.md) and its
[implementation ledger](../design/implementation-ledger.md) track scope,
qualification and release evidence.

## Changes since 1.4.0

The baseline is tag `v1.4.0`, source
`25012c22087fbaa8ce29ddd68d2880d6e9499b1c`. ClearView adds, among other work:

- search execution disclosure (R1), explicit partial multi-collection reads
  with coverage (C1), retry guidance on problems (R11) and stable build-job
  `error_code` values (C2);
- optional inference stages through a self-hosted endpoint such as Munarium
  Lens, with stage receipts and binding routes
  ([inference endpoints](inference-endpoints.md));
- an opt-in MCP endpoint ([operating the MCP endpoint](../ops/mcp.md));
- build publication fencing, per-version outcomes, fair turns and progress
  events for index builds ([Datastore guide](datastore.md)).

Each wire addition is opt-in or an optional member. Omitted options return the
v1.4 response. The new wire fields are release ready by maintainer decision of
20 November 2026; see [wire compatibility](../wire-compatibility.md#clearview-release-status).

## API and SDK compatibility

MMP remains major 1; existing paths, protobuf tags and defaults are unchanged.
SDK source packages and their version-handshake targets are prepared at
**1.5.0**, declaring the N/N-1 range **1.5/1.4**, pending qualification.
Published 1.4.0 packages keep their 1.4/1.3 range. See
[compatibility](../../../clients/compatibility.json).

## Database and configuration upgrade

| Migration after the 1.4.0 schema | State |
|---|---|
| 0043 | Build publication fencing |
| 0044 | Per-version build-job outcomes |
| 0045 | Fair tenant turns for build claims |
| 0046 | Build-job progress columns and events |
| 0047 | Build-job `error_code` |
| 0050 | Console sessions (unread in `legacy` mode) |
| 0060 | Artifact damage records (unread with the default `off`) |
| 0070 | MCP store (unwritten with MCP unset) |
| 0080 | Inference stage bindings (empty without an enrolled collection) |
| 0081 | Inference placement requirement on a binding |
| 0082 | Artifact damage assurance columns (`independent` for every existing incident) |
| 0083 | Enrolled artifact damage verifier keys (empty until an operator enrolls one) |
| 0084 | Provider configuration `text_changed_at` (existing rows take the migration time, so earlier inference probes must be repeated) |
| 0085 | MCP operation key tombstones (empty until the key janitor prunes a key) |
| 0086 | Build-job event prune that skips rows a concurrent prune holds (function only) |
| 0090 | Replica heartbeats, the console fleet mode (no row: `legacy`) and the session fleet epoch columns |

Every migration is additive. Their effect on existing rows and the rollback
boundary of each is in the
[rollback boundaries by migration](../design/v14-baseline.md#rollback-boundaries-by-migration).

1. **Take a backup before upgrading.** Take and verify a consistent backup of
   PostgreSQL, together with the matching object-store revisions for sources
   and Datastore artifacts and any independently retained records, immediately
   before the first 1.5.0 process starts. Follow the
   [backup and restore runbook](../ops/backup-restore.md). Rehearse the restore
   on a disposable copy. This backup is the only rollback route to 1.4.0.
2. Rehearse 1.4.0 to the exact 1.5.0 candidate on a disposable copy. Never
   reset the schema or change an applied migration.
3. Drain v1.4 builders before 1.5.0 builders claim work, as in
   [mixed-version builders](datastore.md#mixed-version-builders-drain-before-switching).
4. Upgrade all relevant readers, writers and workers. New behavior stays off
   until an operator enables it: MCP, inference enrollment, console sessions,
   artifact damage blocking and fair scheduling each have their own setting.
   So does isolated PDF extraction (`MUNARIUM_PDF_EXTRACTION=isolated`). Under
   it, `MUNARIUM_PDF_EXTRACTION_ESCALATE_OVER_BUDGET` (default `false`) decides
   whether an over-budget document may escalate to document intelligence, and
   `MUNARIUM_PDF_EXTRACTION_DEFERRED` (default `skip`; or `fail`) decides
   whether a deferred document is left for a rebuild or fails the build. The
   profile's OS memory and CPU limits are applied with `setrlimit` on Linux,
   the only production platform. Windows builds are for development and
   testing only and apply no OS limit (identity `os=none`). See
   [isolated PDF extraction](document-intelligence.md#isolated-pdf-extraction).
5. Verify health, readiness, `/version`, authentication on both transports,
   scoped retrieval and persistence after restart.

## Rolling back to 1.4.0

Rollback across the 1.5 migrations is **by backup restore** (maintainer
decision of 20 November 2026). A 1.4.0 binary refuses to start against a
database that carries any of migrations 0043–0090, because it does not know
them (`migration 43 was previously applied but is missing in the resolved
migrations`). There is no image-only downgrade, and no 1.4.x release tolerates
the newer migrations.

To roll back:

1. Stop every 1.5.0 process, so nothing writes to the database or object
   stores.
2. Restore the backup taken before the upgrade, with its matching object-store
   revisions, into a database that 1.4.0 will use. Follow the
   [backup and restore runbook](../ops/backup-restore.md), including its source
   journal and command recovery reconciliation.
3. Start 1.4.0 against the restored database, verify health and readiness, and
   re-read the data you checked before the upgrade.

**Data written by 1.5.0 after the upgrade is lost on rollback.** That includes
ledger writes, ingested sources, sessions, build jobs and indexes, MCP records,
inference bindings and any other change made while 1.5.0 served. Export or
re-create anything you need before restoring, and plan the upgrade window so
that a rollback decision is made before significant new data is written.
Do not delete migration history, and do not hand-edit `_sqlx_migrations` to
make 1.4.0 start: the 1.4.0 binary does not understand the new tables or
columns, and fenced build state written by 1.5.0 is unsafe for a v1.4 builder.

## Testing and release

The [1.4 release guide](server-1.4.md#testing-documentation-and-ci) lists the
validation entry points, and its
[build and release checklist](server-1.4.md#build-and-release-checklist)
applies to a 1.5.0 candidate. Cluster qualification for ClearView runs on the
maintainer's workstation; see the
[cluster qualification plan](../design/cluster-qualification-plan.md).
Publication is a separate, authorized action.
