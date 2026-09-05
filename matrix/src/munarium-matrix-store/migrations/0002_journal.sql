-- The journal and the budget ledger.
--
-- The journal is Matrix's audit trail: one row per execute, sync run, mapping
-- run, apply, or refusal. Parameters and results are REDACTED BY DEFAULT —
-- they are stored in `payload_json` only when the source's policy permits it,
-- and a reveal in the operator UI is itself journaled as a separate row. A
-- journal that quietly contains customer data is a second copy of the data
-- with none of the controls.
--
-- The budget ledger is a RESERVATION table, not a counter. A reservation is
-- taken before a statement runs and settled after, so two concurrent
-- executions cannot both pass a check-then-act against the same hourly budget.

CREATE TABLE IF NOT EXISTS matrix.journal (
    id            TEXT        PRIMARY KEY,     -- 'jrn-' + uuid7
    tenant_id     TEXT        NOT NULL,
    kind          TEXT        NOT NULL,        -- execute | sync | mapping | apply | verify | introspect | probe | reveal
    source_name   TEXT,
    asset_ref     TEXT,
    request_id    TEXT,                        -- the server's x-munarium-request-id
    actor         TEXT,                        -- uid or principal that caused it
    via           TEXT,                        -- api | admin-ui | scheduler | mxctl
    outcome       TEXT        NOT NULL,        -- ok | refused | error
    refusal_class TEXT,
    refusal_code  TEXT,
    rows_out      BIGINT,
    bytes_out     BIGINT,
    duration_ms   BIGINT,
    evidence_id   TEXT,
    -- Redacted by default; NULL means "nothing kept", not "nothing happened".
    payload_json  JSONB,
    redacted      BOOLEAN     NOT NULL DEFAULT true,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_journal_tenant_created
    ON matrix.journal (tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_journal_source
    ON matrix.journal (tenant_id, source_name, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_journal_request
    ON matrix.journal (request_id) WHERE request_id IS NOT NULL;
-- Refusals are what an operator hunts for; give them their own path.
CREATE INDEX IF NOT EXISTS idx_journal_refusals
    ON matrix.journal (tenant_id, refusal_code, created_at DESC)
    WHERE refusal_code IS NOT NULL;

CREATE TABLE IF NOT EXISTS matrix.budget_reservations (
    id           TEXT        PRIMARY KEY,
    tenant_id    TEXT        NOT NULL,
    source_name  TEXT        NOT NULL,
    -- The hour this reservation belongs to, truncated. Makes the running total
    -- an indexed aggregate over a bounded set rather than a full scan.
    window_start TIMESTAMPTZ NOT NULL,
    units        BIGINT      NOT NULL,
    state        TEXT        NOT NULL DEFAULT 'held',   -- held | settled | released
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    settled_at   TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_budget_window
    ON matrix.budget_reservations (tenant_id, source_name, window_start)
    WHERE state <> 'released';
