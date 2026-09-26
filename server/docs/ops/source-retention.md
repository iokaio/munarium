# Source denial and PostgreSQL original cleanup

Source retention is an explicit management action for a tenant's stable logical
path. It is separate from runbook soft removal and sealed-evidence retention.
The default remains retained and readable under existing endpoint authority.

`POST /v1/source-retention` accepts `{"path":"fictional/report.txt","action":"deny"}`.
The same operation is available through native `ServerApiService` and all four
generated SDKs. Only management credentials may change or list these records.
Paths use the existing 1–1024-byte source validation; `evidence/` is reserved.

| Action | Effect |
|---|---|
| `deny` | Permanently deny this path, including replacement content at the same identity |
| `deny-and-erase-pg-original` | Deny and durably enqueue deletion of the PostgreSQL original bytes; requires an existing source whose recorded backend is `pg` |
| `hold` | Preserve the original against this cleanup process; may precede source upload or denial |
| `release-hold` | Release that hold; does not release denial or recreate erased bytes |

There is no undeny operation. A hold never grants access and cannot be placed
after original cleanup completed. Non-PostgreSQL originals support denial, but
their erasure requires a separate storage-owner procedure; this API refuses to
claim that its PostgreSQL cleaner erased them.

A source hold blocks this cleaner; it does not make source revisions immutable
or prevent ordinary replacement uploads while the source remains readable.

## Read and rebuild behavior

Denied content returns `source-denied` (HTTP 410, typed/native gRPC
`FAILED_PRECONDITION`) through supported source metadata/sample reads, retrieval,
original-publication authorization and checked-answer composition. A collection
bound to a denied source is conservatively unavailable as a whole, including its
vocabulary. A legacy shape containing that source is also unavailable. Build a
new collection from permitted sources to continue serving the unaffected corpus;
there is no implicit removal of another collection owner's bindings.

The check reads durable state outside artifact caches. Warm L0, reopened L1,
cold hydration and historical/retired pins cannot bypass it. Source reads,
replacement uploads, direct builds and mirror/backfill exports also enforce the
denial. PostgreSQL triggers serialize original writes and new bindings with the
denial, closing in-flight resurrection through those tables.

Existing sessions recheck denial before new model work; sessions containing a
denied source in recorded hits cannot be reused or fetched through the public
session transports. New answers recheck their referenced sources. Requests that
already passed a check may have sent data to a provider or caller before a
concurrent denial: drain in-flight work when a strict cutover is required.
Delivered copies cannot be recalled.

## Cleanup, shared ownership and holds

Denial and `pending` cleanup commit before deletion. Each PostgreSQL-backed
Server checks up to 32 pending records every 60 seconds, oldest first. A hold
or more than one collection binding keeps cleanup pending with `cleanup_blocked`
equal to `hold` or `shared-source`. Shared-owner reconciliation is an explicit
operator responsibility, not an automatic cascade. Failed transactions remain
pending and are retried; `storage-error` identifies a failed sweep without
publishing database error text.

Deleting `source_blobs` bytes and marking `completed` share one transaction and
the same path lock used by holds. A hold that wins the race protects the bytes;
a hold after deletion is refused. A failed delete commits neither completion
nor byte removal. Startup/restart rediscovers pending jobs. The sweep interval
is not an erasure deadline: holds, shared owners, failures and backlog can delay
completion indefinitely.

`completed` means **only the PostgreSQL original bytes are gone**. Catalog
metadata, PostgreSQL chunks/embeddings, immutable datastore artifacts and caches,
vocabulary generations, session/audit history, ledger/evidence, logs, exports,
provider copies, replicas and backups remain under their own inventory policies.
Trusted management audit views may retain historical session/interaction content;
retention is not permission for a data-plane read. No all-copy erasure or
regulatory retention claim is made. Existing sealed-evidence holds/purge ordering
are unchanged; source controls cannot target its reserved namespace.

## Restore, upgrade and rollback

`GET /v1/source-retention` returns up to 100 records and `next`; pass that value as
`after` until `next` is null. Export every page to an independently protected
operational record before backups/restore. The journal has no automatic expiry.
It contains tenant-scoped source identities, paths, policy state and timestamps,
not source text. Apply the same protection to this export as to source metadata.

Keep a restored instance isolated. Replay holds first, then `deny` for every
denied path, and requeue original cleanup where required and supported by the
restored backend. Verify the complete exported journal before serving. Replaying
the same actions is safe; `completed` in an old journal is not proof that restored
bytes are still absent. If the authoritative journal is missing or incomplete,
the restore cannot satisfy the denial contract and must remain isolated.

Migration 0040 is additive. Upgrade and drain **all** readers, workers and writers
before relying on denial; older binaries do not enforce every read path. After
activation, roll forward or use a compatible binary. Never drop journal rows or
disable triggers to perform a rollback. Database-owner access and direct access
to retained object artifacts remain trusted operational boundaries.

## Evidence

The store fixtures test holds, shared owners, real delete failure/rollback,
idempotent retry, hold-versus-cleanup races, cross-tenant isolation, resurrection
denial, application kill/restart with PostgreSQL alive, and a controlled restored
state followed by journal replay. The retrieval fixture covers warm/cold caches,
retired pins, shared collections and rebuild denial with an allowed-source control.
Server fixtures cover REST/native management authority, typed/public session
denial, retained audit history and zero provider calls after denial.

These local/CI fixtures do not qualify database crashes, power loss, cloud object
erasure or an actual operational backup restoration. Those require separately
owned environments and evidence.
