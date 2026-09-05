-- The claims projection: derived-but-transactional (written in the same
-- transaction as the ledger event), regenerable from ledger_events —
-- "rebuild, don't migrate".
CREATE TABLE IF NOT EXISTS claims (
    tenant_id     TEXT NOT NULL,
    id            TEXT NOT NULL,
    version_id    TEXT NOT NULL,
    seq           BIGINT NOT NULL,
    claim_type    TEXT NOT NULL,
    subject       TEXT NOT NULL,
    key           TEXT NOT NULL,
    value         TEXT NOT NULL,
    scope_path    TEXT,
    status        TEXT NOT NULL,
    provenance    TEXT NOT NULL,
    supersedes_id TEXT,
    entity_id     TEXT,
    evidence      JSONB,
    confidence    DOUBLE PRECISION,
    shape_ref     TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, id)
);

CREATE INDEX IF NOT EXISTS idx_claims_version_seq
    ON claims (tenant_id, version_id, seq);
CREATE INDEX IF NOT EXISTS idx_claims_supersedes
    ON claims (tenant_id, supersedes_id);
