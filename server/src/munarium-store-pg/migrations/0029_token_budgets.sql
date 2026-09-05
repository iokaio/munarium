-- 0029: daily token budget reservations (spending caps).
--
-- Reserve -> work -> settle-or-release, the Matrix budget mechanism with the
-- window renamed from hour to UTC day and units renamed from executions to
-- tokens. The enforcer's window expression is `(now() AT TIME ZONE
-- 'utc')::date`; every report over this table must use the same expression,
-- or the report and the ceiling will disagree about which day it is.
--
-- A `held` row is a reservation whose work is in flight; `settled` keeps the
-- row (corrected to actual tokens where the caller reported them); `released`
-- is a refund for work that never started. The crashed-process direction is
-- SPENT: the janitor stamps stale `held` rows settled at their estimate,
-- never released.

CREATE TABLE IF NOT EXISTS token_budget_reservations (
    id          TEXT PRIMARY KEY,
    tenant_id   TEXT NOT NULL,
    config_name TEXT NOT NULL,
    tier        TEXT NOT NULL,
    day         DATE NOT NULL,
    units       BIGINT NOT NULL CHECK (units >= 0),
    state       TEXT NOT NULL CHECK (state IN ('held', 'settled', 'released')),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    settled_at  TIMESTAMPTZ
);

-- The enforcer's sum and the ledger report both scan active rows for one
-- scope's day; released rows are refunds and excluded from both.
CREATE INDEX IF NOT EXISTS idx_token_budget_active
    ON token_budget_reservations (tenant_id, config_name, tier, day)
    WHERE state <> 'released';

-- The janitor's predicate.
CREATE INDEX IF NOT EXISTS idx_token_budget_stale
    ON token_budget_reservations (created_at)
    WHERE state = 'held';
