-- Additive-only migration discipline: new tables, nullable/defaulted columns,
-- new partitions. Never destructive DDL against the ledger (CI-enforced).
CREATE TABLE IF NOT EXISTS tenants (
    id          TEXT PRIMARY KEY,
    slug        TEXT NOT NULL UNIQUE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The demo default tenant; production posture is db-per-tenant per cell.
INSERT INTO tenants (id, slug) VALUES ('tenant-default', 'default')
ON CONFLICT (id) DO NOTHING;
