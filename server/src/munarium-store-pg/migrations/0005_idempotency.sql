-- Idempotency-key replay store (M2 uses it; created here so the schema is
-- complete for the server milestone).
CREATE TABLE IF NOT EXISTS idempotency_keys (
    tenant_id     TEXT NOT NULL,
    key           TEXT NOT NULL,
    request_hash  TEXT NOT NULL,
    response_body TEXT,
    status_code   INTEGER,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, key)
);
