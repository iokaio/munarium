-- 0030 (2026-09-02, code review; numbered 0029 on the review branch, renumbered at merge because 0029 was already the token-budget ledger)
--
-- The serving-required horizon term (§9.1, `required_versions`) exists to
-- cover session pins after supersession: a version a live session could still
-- be holding must keep an artifact until the pin horizon has passed. A pin is
-- taken while a version is ACTIVE, so the last moment a session could have
-- pinned one is the moment it stopped being active — and until now the term
-- was anchored on `built_at` instead. A version built a month ago and retired
-- ten minutes ago therefore fell out of the required set the instant it was
-- superseded, which is the short direction the module header names as the
-- dangerous one: backfill never mirrored it, the readiness set did not carry
-- it, and a session pinned to it lost its version after cutover.
--
-- `deactivated_at` records when a version LAST stopped being active; every
-- deactivating UPDATE stamps it and every activation clears it, so a rolled-
-- back version reads as active again rather than as retired-then-revived.
--
-- Backfill: an inactive row that has been activated was deactivated at some
-- unknown time, and the only bound the database holds is `built_at` — which
-- is exactly the old rule. Stamping it there keeps the required set of an
-- existing database byte-identical across the migration; the new rule takes
-- effect for every deactivation that happens after it.
ALTER TABLE index_versions ADD COLUMN IF NOT EXISTS deactivated_at TIMESTAMPTZ;
UPDATE index_versions SET deactivated_at = built_at
 WHERE deactivated_at IS NULL AND NOT active AND activated_at IS NOT NULL;

-- The partition sweep (`partitions::ensure_ledger_partitions`) asks for
-- `max(tenant_seq)` at every startup and daily, under the DDL advisory lock.
-- `ledger_events` (0002) indexed only `(tenant_id, version_id, seq)`, so that
-- was a full scan of every partition. A partitioned-table index is created on
-- every existing partition and inherited by every future one.
CREATE INDEX IF NOT EXISTS idx_ledger_events_tenant_seq
    ON ledger_events (tenant_seq);
