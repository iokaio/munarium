-- Published shapes (per tenant). Shapes are data; publication is recorded in
-- the ledger when the caller names a version. Rows are append-only by
-- convention: a shape_ref, once published, never changes content (registry-
-- enforced) — new content = new version.
CREATE TABLE IF NOT EXISTS shapes (
    tenant_id  TEXT NOT NULL,
    shape_ref  TEXT NOT NULL,
    yaml       TEXT NOT NULL,
    yaml_hash  TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, shape_ref)
);
