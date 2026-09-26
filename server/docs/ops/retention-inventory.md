# Retention inventory and removal modes

Server preserves soft removal and historical evidence. Excluding a source from
current retrieval does not erase its original bytes, immutable citations, copied
answers, audit records, exports or backups. Explicit [source retention](source-retention.md)
now adds durable denial and optional cleanup of PostgreSQL original bytes. It
does not establish all-copy erasure or a retention deadline.

The machine-readable [inventory](../../retention/inventory.json) identifies every
table and column declared by Server migrations, including JSON payloads, and
every declared artifact family. Each entry names its owner, content, origin files
and a policy. The referenced policy declares read behavior, cleanup, holds,
restore handling and coverage for **each of the five modes** below. Fields such
as `coverage: Gap: ...` describe missing assurance, not successful tests.

| Mode | Current scope | What the mode does not establish |
|---|---|---|
| Retrieval exclusion | Eligible current retrieval can stop selecting content; rebuilding selects the current source bytes | Removal of historical citations, other collections' versions or originals |
| Access revocation | Current endpoint authority restricts the caller; see the [authority audit](../authority-audit.md) for exact paths and session snapshot limits | Recall of delivered answers, exported files or provider copies |
| Logical removal | Source denial also governs bound scopes, pins, rebuilds and public session copies; existing lifecycle states retain their scope | Cascading removal of independent audit copies |
| Physical erasure | PostgreSQL source-original cleanup, evidence-byte purge and separately authorized operator procedures have narrow scopes | All-copy deletion, cloud object erasure or operational restore qualification |
| Protected retention | Append-only provenance, source-original holds and separate evidence legal holds preserve their declared surfaces | A general regulatory retention API or uniform duration |

## Surfaces and retained content

Guarded command recovery adds durable tenant activation and command claims.
Completed claims follow the existing receipt TTL; unresolved claims and activation
records never expire automatically. Claims can retain a response payload after
completion. Restore must reconcile these records before command writers start;
see [command recovery](command-recovery.md). These records do not authorize
source erasure or removal of ledger history.

The JSON inventory is exhaustive for migration columns; this table groups those
entries and artifact families so operators can trace where content can remain.
Identifiers, filenames, hashes, labels and operational metadata are included even
when a surface has no raw source text. JSON payload notes are deliberate review
points: adding another key to an existing JSON document does not add a SQL column.

| Surface | Ownership and retained content | Current cleanup/read implications |
|---|---|---|
| Sources and `source_blobs` | Tenant source owner and every bound collection; original bytes, logical paths, hashes and extraction metadata | Shared sources survive collection/runbook removal. Backend versions and backups are independent copies |
| Collection and legacy chunks | Collection/shape owner; text, tsvector, embeddings, GIN/HNSW indexes, numeric/lexical summaries and chunk provenance | `retireOld` can reclaim selected inactive PostgreSQL chunk data while retaining manifests. It does not erase datastore copies or source bytes |
| Manifests, catalog and build jobs | Tenant/scope and datastore operator; source sets/paths, build settings, probe queries, attribution, bindings and strict execution references | Active-pointer changes select current search. Catalog history, pending/retried jobs and retained originals can still support historical reads or rebuilds |
| Datastore components and archives | Tenant/scope and object-store operator; immutable records, lexical indexes, embeddings, filters, range maps and sidecars | Historical citation text deliberately survives original-source changes. `ArtifactStore` has no delete operation |
| L0, L1 and build staging | Process/node operator; open/mapped shards, hydrated files, quarantine/residue and build intermediates | Handle/size eviction and failed-attempt cleanup are local resource policies. They do not establish content erasure across nodes or L2 |
| Vocabulary | Tenant/collection owner; authored/generated terms, generation inputs and revision stamps; provider copies of samples | Regeneration replaces current vocabulary JSON. Query clients receive revision stamps for cache invalidation; old backups/client/provider copies remain |
| Checked answers | Requesting caller plus independent audit/copy owners; validated answer text, quotes, citations and evidence | Server has no dedicated persisted checked-answer cache/table. Captured interactions, session completions and client caches can retain copies |
| Sessions and interactions | Tenant audit owner and session uid; query, hit text, completion, provenance, hierarchy decisions, capped request/response JSON | Session close/expiry and runbook removal preserve history. A request-body cap is not an erasure guarantee; scope checks do not recall delivered content |
| Ledger and projections | Tenant lineage owner; claims, values, evidence, anchors, promises, digests, gate findings and governance metadata | History is intentionally append-only. Deleting a projection would not remove the events from which it can be rebuilt |
| Sealed evidence | Sealing tenant and evidence operator; result bytes, manifest, grants and access audit | Byte purge retains the metadata row and reports `evidence-expired`; manifests/answers/audit copies remain. Access audit does not contain sealed rows |
| Definitions, drafts and control records | Tenant configuration owner or deployment operator; full YAML, interview documents, policies, accounting/usage evidence, identities and diagnostics | Draft/runbook removal is soft; configuration replacement, idempotency TTL and operational snapshot updates each have narrower purposes than source erasure |
| Exports, providers and logs | Recipient/provider/log-sink operator; downloaded originals/evidence, bundles, reports, prompts, completions and diagnostics | Server cannot recall delivered bytes. Destination retention, holds and deletion evidence require their own policy |
| Backups and replicas | Database/object-storage operator; SQL dumps/base backups/WAL, object versions and snapshots | A database restore excludes external source/evidence/artifact bytes. Replication is availability, not backup or erasure |

Runtime ledger and collection partitions inherit their parent policies and appear
as declared artifact families. The checker does not enumerate live database
partitions. In-memory stores lose their maps on process exit; host dumps/snapshots
and external copies remain separate. Full persistent sessions require PostgreSQL.

## Evidence purge and holds

The existing [evidence implementation](../../src/munarium-server/src/evidence_api.rs)
deletes bytes **before** marking a row purged. A failed byte deletion leaves the
row due for retry; a failed mark leaves a subsequent sweep to finish. Marking the
row first would lose that retry without durable pending-cleanup work. This
inventory preserves the ordering.

The due scan excludes held evidence and operator purge checks a hold before
deleting bytes. SQL state and object deletion are separate operations: a hold
placed concurrently with a scan/deletion is not guaranteed to win. Existing
claim-once and hold fixtures do not prove an atomic hold-versus-purge protocol.
Hold retention also does not expand a caller's authority to read evidence.

## Restore and any future erasure contract

Follow [backup and restore](backup-restore.md) and the separately authorized
[index deletion procedure](index-deletion-runbook.md). An older backup can restore
previous eligibility, revoked-token state, soft lifecycle state or a pre-purge
evidence row. Restoring external storage can also restore previously deleted
bytes. Keep restored instances isolated until required exclusion, revocation,
removal, purge and hold decisions have been reconciled.

The durable source-retention journal must be replayed before a restored instance
serves; its [contract](source-retention.md#restore-upgrade-and-rollback) describes
the export, holds-first replay and remaining backup limits. This does not prove
a general erasure guarantee. Any broader mode must commit
read/retrieval denial and durable pending cleanup before asynchronous erasure,
use stable identities and idempotent retries, protect shared ownership, define
holds and deadlines, and account for exports and backups. A pin that must become
unavailable must not recreate erased content by rebuilding from retained sources.
These requirements do not authorize changing the existing evidence purge order.

## Coverage and limits

| Fixture or check | Surface/mode established | Material limit |
|---|---|---|
| `retention_rebuild_excludes_old_text_but_preserves_pins_and_shared_sources` in [mirror integration](../../src/munarium-retrieval/tests/mirror_integration.rs) | Current rebuild excludes old source text; inactive PostgreSQL chunks can be retired; immutable historical citation text survives warm L0, a new executor using existing L1, and cold L1 hydration; another collection's old version remains and its rebuild reads the retained replacement source | Requires PostgreSQL. Reopen/hydration is not a process-crash or backup-restore test. This proves retention and exclusion, not physical erasure |
| `artifact_process_recovery` in [process recovery](../../src/munarium-retrieval/tests/process_recovery.rs) and [datastore round trips](../../src/munarium-datastore/tests/round_trip.rs) | Artifact recovery, immutable records and historical pins | Does not establish deletion of every retained copy |
| `a_purge_is_claimed_once` and `a_hold_can_be_placed_and_lifted_after_sealing` in [store integration](../../src/munarium-store-pg/tests/pg_integration.rs) | Conditional evidence state transition and hold placement/lifting, with store parity | PostgreSQL coverage needs a database. Claim-once marking does not make the byte-delete/mark sequence atomic |
| Purge, expiry, replay and hold fixtures in [evidence routes](../../src/munarium-server/src/evidence_routes.rs) | Existing evidence-byte removal, retained metadata, unavailable reads and evidence-only protected retention | No injected object-delete/mark failure or simultaneous hold race is claimed |
| [Vocabulary transport fixtures](../../src/munarium-server/src/v12_tests.rs) | Authorization, generation and revision changes used by cache invalidation | Does not erase provider prompts, historical backups or caller caches |
| [Inventory negative controls](../../tools/test_retention_inventory.py) | Added SQL table/column, JSON-array payload, declared artifact and datastore component kind fail without policy; missing modes/origins/coverage and malformed/duplicate JSON fail | Declaration completeness is not proof of runtime policy enforcement |

Unqualified scenarios include general source erasure through a pinned session,
all-node removal after restart, restore from pre-removal state, shared-source
physical deletion, atomic hold placement against deletion, partial cleanup failure
and recipient/provider/backup deletion. No passing registry check closes these
gaps. Future work must name the mode and surface tested instead of reporting a
single undifferentiated deletion pass.

## Keeping the registry current

Run from the repository root with Python 3.9 or later:

```powershell
python server/tools/check_retention_inventory.py
python -m unittest discover -s server/tools -p test_retention_inventory.py
```

The workspace integration test
[`retention_inventory.rs`](../../src/munarium-server/tests/retention_inventory.rs)
runs the same suite, including the actual inventory comparison, so the existing
offline `test.ps1` workspace tier and automatic workspace CI include the gate.
The bridge tries `python3`, `python`, then `py`; the test-only `RETENTION_PYTHON`
override accepts an explicit interpreter path. Missing Python fails the test.

For SQL changes, update every table/column entry and describe each JSON payload.
Unsupported table DDL fails for deliberate parser review. For a new artifact
family, add its producer/storage origins to the separate
[artifact declarations](../../retention/artifacts.json) and add its inventory
policy. `ComponentPurpose` variants are discovered independently from the Rust
model, so adding a datastore component cannot pass just by omitting a declaration.

This is a declared-surface checker, not a data-flow analyzer. It cannot infer new
keys inside existing JSON, an undeclared archive format, an unmarked cache, new
dynamic SQL, a provider's storage practices or a deployment's backup topology.
Reviewers must trace new writers and copies and update declarations. Negative
controls prove that known declarations cannot silently lose their policies; they
do not prove that arbitrary new content storage is automatically discoverable.
