# Running munarium-server as a cluster (N instances, one PostgreSQL)

Since 2026-08-17 the server is correct to run as N identical instances
sharing one PostgreSQL database. This page is the operator's contract: what
is shared, what stays per-instance, how to run it, the pool math, and the
two manual procedures. The black-box proof is the cluster conformance tier
(`test.ps1 -Cluster`: two live instances, one database, five scenarios —
all green on 2026-08-17).

Scalability posture, stated honestly: this makes scale-OUT *correct*, not
yet *fast*. There is no pgcat pooler, no replica-read routing, and no
dedicated retrieval tier — those remain design targets in
architecture.md §4/§11. Scale-UP works today: raise `MUNARIUM_DB_MAX_CONNS`
and `MUNARIUM_MAX_CONCURRENCY` on a bigger box.

## What is shared (in PostgreSQL — correct across instances by construction)

| Concern | Mechanism |
|---|---|
| Ledger seq allocation | `lineage_heads` row lock (`SELECT … FOR UPDATE`) — the cross-process mutex; stale `expected_head` still 409s |
| Idempotency | `idempotency_keys` table; a replay via ANY instance returns the recorded response; pruned by the janitor (below) |
| Shapes / provider configs / per-call token budgets | persisted tables + per-instance registry caches that re-read after `MUNARIUM_REGISTRY_TTL_SECS` (default 15s; see staleness contract below). The token budgets (`POST /v1/max-tokens`, 2026-09-02, one JSONB row per tenant) follow exactly the provider-config pattern: the instance that took the write answers it immediately, the others within the TTL |
| Sessions, runbook runs/steps | plain tables |
| Runbook execution | at most ONE instance executes a given run: `pg_try_advisory_lock(hashtext(tenant), hashtext(run_id))` held on a detached connection for the duration; the loser answers 409 `run-locked` |
| Document bytes | whatever `MUNARIUM_SOURCE_STORE` names — `pg`, `az`, `s3`, `gcs` are shared; `file` only on a shared mount (the server warns); `mem` is refused in cluster mode |
| Interaction audit | every instance writes its own rows, stamped with its `instance_id`; the reports API and `/admin` dashboards aggregate across all of them |
| `ledger_events` partitions | every instance runs the daily ensure-partitions sweep under `pg_advisory_xact_lock('munarium:partition-ddl')` — concurrent sweeps serialize |

## What stays per-instance (and why that is fine — or approximately fine)

- **Registry caches** (shapes, provider configs): pure caches over the
  tables. Staleness contract: a shape or config applied on instance A is
  visible on B within `MUNARIUM_REGISTRY_TTL_SECS` (shapes are immutable per
  ref, so this is a missing-entry delay, never a wrong entry). `0` restores
  the load-once behavior — do not use it with more than one instance.
- **Embedding cache**: per-process; N instances just means N cold caches.
- **Provider rate budgets**: each instance enforces
  `ceil(configured / MUNARIUM_REPLICA_COUNT)`, so the CLUSTER honors a
  configured rpm/tpm instead of multiplying it by N. Known approximations,
  stated plainly: uneven load balancing under-uses the budget, and a
  restarted instance resets its one-minute window. If exact global
  enforcement ever matters, that is a table-backed budget — a deliberate
  non-feature today.
- **Load-shed ceilings** (`MUNARIUM_MAX_CONCURRENCY`): per instance per plane,
  which is what you want — shed reflects each instance's own capacity.
- **`/metrics`**: per instance; give each instance its own scrape target
  and let the scraper label `instance`. Cross-instance truth lives in the
  `/v1/reports/*` views, which read the shared tables.

## How to run it

Every instance gets identical configuration except `MUNARIUM_INSTANCE_ID`
(defaults to `HOSTNAME`, which Kubernetes and compose set uniquely), plus:

```
MUNARIUM_STORE=postgres            # cluster mode refuses the memory store
MUNARIUM_SOURCE_STORE=pg|az|s3|gcs # shared bytes (file only on a shared mount)
MUNARIUM_REPLICA_COUNT=<N>         # arms validation + splits rate budgets
MUNARIUM_DATABASE_URL=<same for all>
```

`MUNARIUM_REPLICA_COUNT > 1` fails closed at startup on a per-process store
(memory ledger or mem source store) — a cluster of private worlds shares
nothing and the config error says so.

**Local demo** (round-robin Envoy over two instances, one postgres):

```
$env:MUNARIUM_REPLICA_COUNT = '2'
docker compose --profile cluster up
# gateway on :8443 round-robins REST and gRPC across compose-a / compose-b
```

**The proof harness**: `.\test.ps1 -Cluster` builds once, starts two
servers on 18082/18083 against the compose postgres with a fresh shared
tenant and `MUNARIUM_REGISTRY_TTL_SECS=1`, and runs
`mmp-conformance --cluster … --peer …`: shape convergence, provider-update
convergence (budget window preserved for unchanged configs), cross-instance
idempotency (replay + mismatch), interleaved seq allocation with a stale
`expected_head` 409, and the concurrent-approval single-executor lock.

**Rolling restarts**: the server handles SIGTERM (not just SIGINT) since
2026-08-17 — on any stop signal both planes' `/readyz` flip to 503
"draining" immediately and in-flight work gets `MUNARIUM_SHUTDOWN_GRACE_SECS`
(default 20) to finish. Give the orchestrator a termination grace period of
at least that plus a few seconds. On Kubernetes add a short `preStop` sleep
so endpoint deregistration outruns the drain.

## Pool math

```
N_instances × MUNARIUM_DB_MAX_CONNS  +  in-flight runbook executions
    < postgres max_connections (CNPG default 100)
```

Each runbook execution holds ONE extra connection outside the pool (the
advisory-lock guard). `MUNARIUM_DB_MAX_CONNS` has a floor of 2: the append
path holds a pool connection for its `FOR UPDATE` transaction, and a pool
of 1 deadlocks writers against any concurrent work. Two instances at the
default 10 fit comfortably under 100.

## Diagnosing a crash-orphaned run

Symptom: `GET /v1/runs/{run_id}` shows `state: "running"` with a stale
newest `updated_at`, and no instance logs progress. What happened: the
executing instance died; its connection dropped, so PostgreSQL released
the advisory lock — the run is UNLOCKED but nothing auto-resumes it (by
design: silent auto-resume of half-done index builds is how surprises
ship). Recovery: re-drive it — `POST` the pending approval again, or for a
pre-approval crash re-run the runbook; `execute` re-reads step states and
resumes from the first non-`done` step. A second executor arriving while
one is still alive answers 409 `run-locked` — that is the lock working,
not a stuck run.

## Partition overflow (the one manual procedure)

`ledger_events` is range-partitioned on `tenant_seq` with a DEFAULT
partition. The daily sweep creates the next 10M-wide partition when the
high-water mark comes within 2M of the top bound, so overflow should never
happen. If an ERROR log names a partition-overlap failure, rows have
already landed in `ledger_events_default` and the automatic path stops
(creating the range over occupied default rows is impossible). Manual
recovery, as a DBA change ticket:

```sql
BEGIN;
SELECT pg_advisory_xact_lock(hashtext('munarium:partition-ddl'));
-- 1. detach the default so its rows can move
ALTER TABLE ledger_events DETACH PARTITION ledger_events_default;
-- 2. create the missing range partition(s) for the overflowed span
CREATE TABLE ledger_events_pN PARTITION OF ledger_events
    FOR VALUES FROM (<top_bound>) TO (<top_bound + 10000000>);
-- 3. move the rows, then re-attach an EMPTY default
INSERT INTO ledger_events SELECT * FROM ledger_events_default;
TRUNCATE ledger_events_default;
ALTER TABLE ledger_events ATTACH PARTITION ledger_events_default DEFAULT;
COMMIT;
```

Verify afterward: `SELECT count(*) FROM ledger_events_default` is 0 and
the sweep's next run logs nothing.

## What clustering does NOT include yet (design-only, architecture.md)

pgcat transaction pooling · watermark-routed replica reads · HPA/PDB
profiles (the Helm chart had its first validated install 2026-08-18 on
kind — its README carries the evidence — but no HPA/PDB objects exist in
it) · database-per-tenant cells · runbook execution off the
request path (tracked follow-up: today a long index build holds its HTTP
request open on the executing instance).
