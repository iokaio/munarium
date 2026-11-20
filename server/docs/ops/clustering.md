# Running munarium-server as a cluster (N instances, one PostgreSQL)

For Server 1.3, upgrade and drain every reader, writer and worker before relying
on guarded commands, source denial, governance profiles or new provider controls.
The older binaries do not enforce these policies even where the schema remains
readable. Follow the [1.3 upgrade guide](../guides/server-1.3.md); a historical
same-version cluster test is not qualification of a mixed-version upgrade.

Use [Measuring performance](../guides/measuring-performance.md) to establish
capacity, identify shared bottlenecks and verify the benefit of adding instances.

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
| Collection vocabularies (1.2) | tenant defaults, revisions and generation leases in PostgreSQL; workers use a five-minute lease and revision/source checks before publishing |
| Publication governance (1.2.1) | append-only `collection_governance` snapshots; an advisory transaction lock serializes revision-checked writes; queries re-read governance and vocabulary revisions before returning |
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
  which is what you want — shed reflects each instance's own capacity. The
  opt-in control reserve and heavy bound ([admission classes](admission.md))
  are per instance per plane too.
- **`/metrics`**: per instance; give each instance its own scrape target
  and let the scraper label `instance`. Cross-instance truth lives in the
  `/v1/reports/*` views, which read the shared tables. The
  [diagnostics below](#replica-pool-and-queue-diagnostics) cover replica
  identity, pools and the build queue.

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

**On Kubernetes** the Helm chart renders `MUNARIUM_REPLICA_COUNT` only when
`providerReplicaCount` is set. The bare chart omits it, as v1.4 did, so two
default pods each enforce the full provider limit and the cluster checks
stay off. The production profile (`values-production.yaml`) sets `auto`,
which renders the replica count. A rollout surge pod takes a full share as
well. See the
[chart's replica-count section](../../deploy/helm/munarium/README.md#replica-count-and-provider-limits)
for upgrade, migration, `platform-v1` and rollback.

**Separate builders (SCALE-2).** `builder.enabled: true` adds a
`munarium-builder` Deployment with its own replicas, resources and drain
bound. It runs the build-job worker (`MUNARIUM_DATASTORE_BUILDER=enabled`),
reports plane `builder`, starts no direct gRPC listener and is selected by no
Service or gateway route, so builds and queries scale independently. With
`auto`, both groups render `replicas + builder.replicas`. Add a builder by
raising `builder.replicas`; drain one by lowering it. See the
[chart's builder group](../../deploy/helm/munarium/README.md#builder-group).

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

A process running the build-job worker drains its builder on the same signal
and within the same grace window: it claims no further jobs, and a build in
flight either finishes or stops at its next safe boundary and releases its
claim. A released job keeps its state and generation with the lease expired,
so the next poll of any remaining builder reclaims it under a new generation
and the drained holder's fenced writes are refused. A release counts as one of
the job's three claims, like a crash. A build still running at the deadline is
abandoned and reclaimed once its job lease lapses. Neither path records a
failure for a routine drain, except that a drained final claim is failed with
`attempts_exhausted`.

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

## Replica, pool and queue diagnostics

ClearView SCALE-3. Three views read the same snapshot:

| Where | What it shows | Scope |
|---|---|---|
| `/metrics` (ops plane) | `munarium_replica_count`, `munarium_db_pool_connections` / `_idle` / `_max_connections`, `munarium_admission_slots` / `_in_use`, and the build queue: `munarium_index_build_jobs{state="pending"\|"running"}`, `munarium_index_build_oldest_pending_seconds`, `munarium_index_build_queue_up` | Process gauges per pod; the queue across all tenants, counts only |
| `GET /v1/reports/scale` (mgmt) | The answering replica's `instance_id`, version, configured count, builder flag, SQL pool and admission pools; the caller's tenant's backlog | One replica per call; one tenant's queue |
| Console Health page | The same report as tiles and an admission table | As the report |

**Counting replicas.** Each process knows only its configured
`MUNARIUM_REPLICA_COUNT`. Live membership is not recorded, so the report's
`observed_replicas` is null and the console says membership is unknown.
Compare the configured count with ready pods
(`kubectl get deploy munarium-server`, or kube-state-metrics
`kube_deployment_status_replicas_ready` beside `kube_deployment_spec_replicas`)
and with `count(munarium_build_info)` across scrape targets. More live
processes than the count means the cluster admits more than each provider
limit, as during a rollout surge.

**Pools.** Busy connections are `open - idle`. Sum `max_connections` across
every pod, including surge pods, against PostgreSQL `max_connections` (see
[pool math](#pool-math)). Admission saturation shows as `in_use` at
`capacity` beside `munarium_admission_shed_total` ([admission classes](admission.md)).

**Build queue.** The read is one indexed, read-only count over
`index_build_jobs` per scrape or page load, bounded at 2 seconds including
the wait for a pool connection. Every replica reports the same shared queue,
so aggregate with `max`, not `sum`. A failed or timed-out read sets
`munarium_index_build_queue_up 0` and omits the counts; the report and
console show `unavailable`, never 0. `running` includes jobs whose lease has
lapsed and that a builder may reclaim. A growing oldest-pending age with
`running` at the number of builders means builders are the bottleneck; see
[choosing the resource to scale](../design/scale-out.md#choose-the-server-resource-to-scale).

What has not been qualified: SCALE-3-T1 drives a known queue and pool
bottleneck on a real fleet, resizes it, and compares these counts with
fleet and database observations. That run needs a cluster.

## Diagnosing a crash-orphaned run

Symptom: `GET /v1/runs/{run_id}` shows `state: "running"` with a stale
newest `updated_at`, and no instance logs progress. What happened: the
executing instance died; its connection dropped, so PostgreSQL released
the advisory lock — the run is UNLOCKED but nothing auto-resumes it (by
design: silent auto-resume of half-done index builds is how surprises
ship). A pending `awaiting_approval` step can be approved through the existing
approval endpoint. There is no general public resume endpoint: starting the
runbook again creates a **new run**, not a continuation of the old run. Inspect
persisted step details and actual index state before deciding how to proceed;
a cutover effect can already be active while its checkpoint still says running.
Private executor re-entry is qualified in tests, not exposed as an operator API.
A second executor arriving while one is still alive answers 409 `run-locked`.
Approval state is checked under that lock; a retry after completion is rejected
as no longer awaiting approval.

Each new step checkpoint and, when the run has a `version_id`, its ledger
transition commit in one PostgreSQL transaction. Failure before commit leaves
neither new record; failure after commit leaves both. Runs without a version
intentionally keep only the checkpoint. This does not make index effects and
checkpoints atomic, provide exactly-once effects, or automatically resume work.
Run-summary state is a separate write; inspect the step checkpoints when a
summary remains running after a crash.

Historical runs may have a done checkpoint without its event. Done steps remain
skipped and their details retained; no event is fabricated or backdated to fill
the gap. An old checkpoint is not proof of complete historical transition or
approval evidence. New writes do not certify that earlier history.

No schema migration or backfill is required. Drain all older checkpoint/approval
writers before relying on atomic checkpoint/history writes. Old readers can read
the unchanged rows and wire responses, but mixed writers or rollback to an older
binary reopen the gap. Retain all history on rollback. Qualification covers
application-process death with PostgreSQL continuously running, not database
crashes, power loss, backup restore, or arbitrary external effects.

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
