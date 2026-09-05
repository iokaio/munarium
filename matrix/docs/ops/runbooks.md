# Runbooks

Three operations an operator performs on a running Matrix, each with the
reasoning that decides *whether* to do it — because in all three cases the
wrong call is worse than the delay.

---

## 1. Resnapshot a collection

**Symptom.** A sync run refuses `checkpoint_gap`, `cdc_checkpoint_gap` or
`cdf_checkpoint_gap`, or `/admin/runs` shows a checkpoint whose position the
source can no longer replay.

**What it means.** The incremental position Matrix holds is behind what the
source still has. A watermark moved past retention, a Delta version was
vacuumed, a replication slot was dropped and recreated. **Matrix refuses rather
than continuing**, because continuing from a gap produces a collection that
reports coverage it does not have — and a collection that overstates coverage
is worse than one that is late.

**Do NOT** hand-edit the checkpoint to an earlier position. It will appear to
work: the run completes and rows arrive. What it silently loses is everything
that changed in the gap, and nothing downstream can tell.

**The fix.**

1. Confirm the gap is real, not a transient outage:
   ```
   mmctl matrix journal --limit 50          # or /admin/journal, refusals only
   ```
   A `checkpoint_gap` beside a run of `source_unavailable` is usually the
   source having been down; wait for it to come back before resnapshotting.

2. Drop the checkpoint for that source and entity. There is no CLI verb for
   this on purpose — it discards a position — so it is a deliberate SQL
   statement against the `matrix` schema as `matrix_owner`:
   ```sql
   DELETE FROM matrix.sync_checkpoints
    WHERE tenant_id = :tenant AND source_name = :source AND entity = :entity;
   ```

3. Re-run the sync. With no checkpoint the worker takes a fresh snapshot:
   ```
   mmctl matrix sync <source>
   ```

4. Watch it on `/admin/runs`. A resnapshot re-renders every row, so
   `records_rendered` will match the source's full count and
   `documents_skipped` will be high — the server's idempotency store recognises
   bytes it already holds.

**The CDF and CDC cases do this for you.** A `cdf_checkpoint_gap` or
`cdc_checkpoint_gap` makes the worker resnapshot on its own rather than report
coverage it lacks. The manual path above is for the watermark modes.

---

## 2. Retention: evidence, and what a legal hold does

**Retention is the server's, not Matrix's.** Matrix seals evidence *into*
munarium-server; the artifacts, their expiry, the purge janitor and legal holds
all live there. This runbook is how an operator of Matrix reaches them.

**Setting it.** A contract or view declares `retentionDays`, and an intent may
override it downward. There is no ambient default that quietly keeps regulated
bytes forever.

**The janitor is OFF unless configured.** `MUNARIUM_EVIDENCE_PURGE_INTERVAL_SECS`
defaults to **0**, meaning never. That is deliberate: a janitor nobody
configured, deleting regulated data on a schedule nobody chose, is worse than
one that never runs.

**Checking what is due.**
```
GET /v1/reports/evidence          # on the SERVER, mgmt role
```

**Placing a legal hold.**
```
POST /v1/evidence/{id}/legal-hold  # on the SERVER, mgmt role
```
A hold **blocks deletion and never blocks reading**. An artifact under hold
cannot be purged and cannot be deleted by hand; a request to delete it is
refused `evidence-on-hold`, which is a reachable refusal precisely because the
hold route exists.

**Deletion order, and why it matters.** Bytes are deleted **before** the row is
marked purged. The other order leaves an artifact reporting itself purged while
its regulated bytes remain, and no later sweep revisits it. This order is
briefly untidy and self-healing.

**Do NOT** delete evidence to reclaim space during an incident. A sealed
manifest is what an answer's citation resolves to; removing it turns every
`[evidence/<id>#<row>]` in a past answer into a dangling reference.

---

## 3. The circuit breaker

**Symptom.** `/admin/matrix` on the **server** shows `circuit open`, or turns
come back thin with a Matrix layer refusing.

**What it is.** Per **instance**, per provider, shared by every tenant. It
trips after consecutive failures and refuses immediately for a cool-off, so a
Matrix outage costs one timeout rather than one per turn.

**It carries no tenant label anywhere, on purpose.** A shared breaker reported
per tenant would let one tenant's scrape reveal another's traffic.

**Reading it.**
```
GET /v1/reports/matrix   # on the SERVER: configured?, circuit_open, consecutive_failures
```
`configured: false` and `circuit_open: true` must never read the same. The
first is a deployment that does not use Matrix; the second is an outage.

**Resetting it.** There is no reset button, and that is the design: the breaker
closes on its own when the cool-off elapses and a call succeeds. A manual reset
would let an operator re-open a path to a source that is still down, turning
one timeout per cool-off back into one per turn.

**The fix is upstream.** Find why calls failed:

1. `POST /v1/datasources/{name}/probe` on Matrix — reachable, right now?
2. `/admin/journal?refusals=1` — what refusal, and how often?
3. If the refusals are `budget_exceeded`, the source's `budgetPerHour` is spent
   and the breaker is a symptom, not the problem.
4. If they are `schema_drift`, the source changed under a verified contract.
   **Verify the contract again** — that is the intended response, and it is
   what re-records the fingerprint an execute compares against.

**The flag worth watching** is on the server's `/admin/matrix`: a layer
refusing on at least half its turns is badged red. Those turns **still return
200**. The answers get thinner, the runbook keeps claiming the layer, and
nothing else on any dashboard goes red — which is the whole reason that page
exists.
