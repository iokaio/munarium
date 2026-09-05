-- 0022 (2026-08-28, S-4.1): connector claim origin. A claim proposed by
-- Munarium Matrix carries the source, mapping version, row key and sealed
-- evidence it was derived from, so a reviewer walks from a ledger fact back
-- to the exact evidence without trusting the claim's text. Additive: NULL
-- for every model-extracted claim, and no gate reads it.
--
-- The plan reserved 0022 for the evidence store (S-2.1); that package is
-- unapproved, and migration numbers are ORDER, not identity, so origin takes
-- the next free number and evidence takes the one after (decisions.md).
ALTER TABLE claims ADD COLUMN IF NOT EXISTS origin JSONB;
