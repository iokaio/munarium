-- Per-tenant provider configurations. credentialRef ONLY — key material
-- never enters this table, the ledger, or any log line.
CREATE TABLE IF NOT EXISTS provider_configs (
    tenant_id  TEXT NOT NULL,
    name       TEXT NOT NULL,
    yaml       TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, name)
);
