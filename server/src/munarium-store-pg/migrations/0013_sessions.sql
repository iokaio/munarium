-- M10: multiturn sessions over runbooks. A session pins the runbook
-- name@version at creation and snapshots the token's access level +
-- compartments, so a mid-session runbook upgrade or token change never
-- changes what an ongoing conversation can see.
CREATE TABLE IF NOT EXISTS sessions (
    tenant_id    TEXT NOT NULL,
    id           TEXT NOT NULL,                  -- 'ses-…'
    uid          TEXT NOT NULL,
    runbook_ref  TEXT NOT NULL,                  -- pinned name@version
    token_jti    TEXT,
    access_level INTEGER NOT NULL,
    compartments TEXT[] NOT NULL DEFAULT '{}',
    state        TEXT NOT NULL DEFAULT 'open',   -- open | closed | expired
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_turn_at TIMESTAMPTZ,
    PRIMARY KEY (tenant_id, id)
);
CREATE INDEX IF NOT EXISTS idx_sessions_uid ON sessions (tenant_id, uid, created_at);

CREATE TABLE IF NOT EXISTS session_turns (
    tenant_id            TEXT NOT NULL,
    session_id           TEXT NOT NULL,
    ordinal              INTEGER NOT NULL,
    uid                  TEXT NOT NULL,
    query                TEXT NOT NULL,
    collections_searched TEXT[] NOT NULL,        -- names actually queried after access filtering
    hits                 JSONB NOT NULL,
    envelope             JSONB NOT NULL,         -- per-collection ProvenanceEnvelopes
    completion           JSONB,                  -- resolved model audit + text + usage
    interaction_id       TEXT,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, session_id, ordinal)
);
