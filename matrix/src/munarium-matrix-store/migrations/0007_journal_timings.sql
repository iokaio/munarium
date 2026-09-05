-- 0007 (2026-08-30, the §18.3 measurement): an execute's journal row says
-- where its time went. `duration_ms` was the whole wall clock; these two are
-- the pieces that are not Matrix's — the source's own statement window and
-- the seal round-trip into the server — so that what is left is Matrix's
-- share, which is the number the plan's transport-share formula needs.
-- Additive: every existing row reads NULL for both, which renders as the
-- total alone.

ALTER TABLE matrix.journal ADD COLUMN IF NOT EXISTS source_ms BIGINT;
ALTER TABLE matrix.journal ADD COLUMN IF NOT EXISTS seal_ms   BIGINT;
