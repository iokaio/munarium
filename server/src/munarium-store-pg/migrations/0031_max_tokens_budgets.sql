-- 0031 (2026-09-02): per-call output-token budgets, replaceable per tenant
-- through GET/POST /v1/max-tokens.
--
-- One row per tenant holding the WHOLE object as JSON. The API's contract is
-- "replace the set, never part of it", so the storage shape is the object and
-- a partial write is unrepresentable here rather than merely refused. No row
-- means the tenant is on the process defaults (MUNARIUM_MAX_TOKENS_* over the
-- built-ins); the server never invents a row on read.
CREATE TABLE IF NOT EXISTS max_tokens_budgets (
    tenant_id  TEXT PRIMARY KEY,
    budgets    JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
