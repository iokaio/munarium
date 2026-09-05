-- M7: identity + interaction capture.
--
-- access_tokens is the issuance audit (and optional revocation deny-list)
-- for the deliberately-light capability JWTs the API-management layer
-- requests via POST /v1/access-tokens. Token material is NEVER stored —
-- only the jti and the claims needed for audit/reporting.
CREATE TABLE IF NOT EXISTS access_tokens (
    tenant_id    TEXT NOT NULL,
    jti          TEXT NOT NULL,                    -- 'tok-…'
    uid          TEXT NOT NULL,
    access_level INTEGER NOT NULL,
    compartments TEXT[] NOT NULL DEFAULT '{}',
    scopes       TEXT[] NOT NULL,                  -- subset of {query, ingest}
    runbook_refs TEXT[],                           -- NULL = any runbook
    issued_by    TEXT NOT NULL,                    -- uid asserted by the mgmt caller
    issued_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at   TIMESTAMPTZ NOT NULL,
    revoked_at   TIMESTAMPTZ,
    PRIMARY KEY (tenant_id, jti)
);
CREATE INDEX IF NOT EXISTS idx_access_tokens_uid
    ON access_tokens (tenant_id, uid, issued_at);

-- interactions records every API request/response (uid-attributed) for
-- audit and reporting. Bodies are capped (MMESH_INTERACTION_BODY_MAX);
-- oversized bodies store a {sha256, bytes_len} summary instead.
CREATE TABLE IF NOT EXISTS interactions (
    tenant_id      TEXT NOT NULL,
    id             TEXT NOT NULL,                  -- 'int-…'
    uid            TEXT NOT NULL,
    session_id     TEXT,
    request_id     TEXT,                           -- 'req-…' (also echoed to the caller)
    plane          TEXT NOT NULL,                  -- 'rest' | 'grpc'
    method         TEXT NOT NULL,                  -- 'POST /v1/sessions/{id}/turns' | '/mmp.v1.CommandService/ProposeClaim'
    runbook_ref    TEXT,
    collection_ids TEXT[],
    token_jti      TEXT,
    request        JSONB,
    response       JSONB,
    status         INTEGER,                        -- HTTP status (REST) or gRPC code (grpc)
    latency_ms     INTEGER,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, id)
);
CREATE INDEX IF NOT EXISTS idx_interactions_uid
    ON interactions (tenant_id, uid, created_at);
CREATE INDEX IF NOT EXISTS idx_interactions_session
    ON interactions (tenant_id, session_id, created_at);
CREATE INDEX IF NOT EXISTS idx_interactions_runbook
    ON interactions (tenant_id, runbook_ref, created_at);
