-- Runbooks + durable, resumable executions. Every step transition persists
-- here; a caller-named version additionally records each transition as a
-- ledger event, making executions reproducible, auditable objects.
CREATE TABLE IF NOT EXISTS runbooks (
    tenant_id   TEXT NOT NULL,
    runbook_ref TEXT NOT NULL,
    yaml        TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, runbook_ref)
);

CREATE TABLE IF NOT EXISTS runbook_runs (
    tenant_id   TEXT NOT NULL,
    id          TEXT NOT NULL,
    runbook_ref TEXT NOT NULL,
    state       TEXT NOT NULL DEFAULT 'running', -- running | awaiting_approval | done | failed
    version_id  TEXT,                            -- lineage receiving execution events
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, id)
);

CREATE TABLE IF NOT EXISTS runbook_steps (
    tenant_id  TEXT NOT NULL,
    run_id     TEXT NOT NULL,
    ordinal    INTEGER NOT NULL,
    name       TEXT NOT NULL,
    state      TEXT NOT NULL DEFAULT 'pending',
    detail     JSONB,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, run_id, ordinal)
);
