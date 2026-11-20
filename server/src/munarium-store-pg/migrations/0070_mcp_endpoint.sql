-- SPDX-License-Identifier: Apache-2.0
-- 0070 (2026-11-20, ClearView A6/A8/A9 and the §5.3 recovery contract): the
-- durable store behind the opt-in MCP endpoint (server/docs/ops/mcp.md).
--
-- - `mcp_invocations`: one row per admitted `tools/call` (A6). The row is
--   committed, and its commit acknowledged, BEFORE any tool handler runs.
--   It holds the server-assigned invocation id, the fixed tool name, tenant,
--   uid, token id, request id and time; never credentials, argument bodies,
--   retrieved text or evidence rows. A duplicate or recovery request gets its
--   own row linked to the original execution by `original_invocation_id`.
--   `dispatched_at` and `outcome` are written after the fact: a row with no
--   outcome after a crash is "unknown", never a cue to replay.
-- - `mcp_operations`: the caller-known operation keys of the five keyed tools
--   (§5.3). The key is bound to tenant, uid and tool; the row stores the
--   canonical argument digest, the immutable expiry, the work deadline and a
--   bounded receipt (state, status, session id and ordinal, result size). It
--   never stores request bodies, transcripts, answers or evidence.
-- - `mcp_token_attempts` / `mcp_token_attempt_scopes`: one row per physical
--   generation attempt reserved under the A8 request and daily allowances,
--   with the UTC day it was reserved against (database clock) and every
--   collection or index scope it was charged to (A9). Usage is the settled
--   value when reliable, otherwise the full reservation.
-- - `mcp_bound_violations`: an attempt whose provider-reported usage exceeded
--   the reservation bound; further attempts on that provider configuration
--   and model are refused until an operator clears the row.
--
-- A v1.4 replica never reads these tables, and a replica with MUNARIUM_MCP
-- unset never writes them. Additive: new tables and indexes only; no
-- existing table, column, index or row is touched.

CREATE TABLE IF NOT EXISTS mcp_invocations (
    invocation_id          TEXT PRIMARY KEY,
    tenant_id              TEXT NOT NULL,
    uid                    TEXT NOT NULL,
    token_jti              TEXT NOT NULL,
    request_id             TEXT NOT NULL,
    tool                   TEXT NOT NULL,
    protocol_version       TEXT NOT NULL,
    form                   TEXT NOT NULL,
    original_invocation_id TEXT,
    instance_id            TEXT NOT NULL,
    admitted_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    dispatched_at          TIMESTAMPTZ,
    outcome                TEXT,
    outcome_status         INTEGER,
    outcome_at             TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS mcp_invocations_tenant_admitted
    ON mcp_invocations (tenant_id, admitted_at DESC);

CREATE INDEX IF NOT EXISTS mcp_invocations_original
    ON mcp_invocations (tenant_id, original_invocation_id)
    WHERE original_invocation_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS mcp_operations (
    tenant_id        TEXT NOT NULL,
    uid              TEXT NOT NULL,
    tool             TEXT NOT NULL,
    key_id           TEXT NOT NULL,
    expires_at       TIMESTAMPTZ NOT NULL,
    args_digest      TEXT NOT NULL,
    invocation_id    TEXT NOT NULL,
    state            TEXT NOT NULL,
    deadline_at      TIMESTAMPTZ NOT NULL,
    resources        JSONB NOT NULL DEFAULT '{}'::jsonb,
    http_status      INTEGER,
    problem_type     TEXT,
    session_id       TEXT,
    turn_ordinal     INTEGER,
    result_bytes     BIGINT,
    result_too_large BOOLEAN NOT NULL DEFAULT false,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    outcome_at       TIMESTAMPTZ,
    PRIMARY KEY (tenant_id, uid, tool, key_id)
);

CREATE INDEX IF NOT EXISTS mcp_operations_expires
    ON mcp_operations (expires_at);

CREATE INDEX IF NOT EXISTS mcp_operations_invocation
    ON mcp_operations (tenant_id, invocation_id);

CREATE TABLE IF NOT EXISTS mcp_token_attempts (
    attempt_id      TEXT PRIMARY KEY,
    invocation_id   TEXT NOT NULL,
    tenant_id       TEXT NOT NULL,
    payer           TEXT NOT NULL,
    day             DATE NOT NULL,
    provider_config TEXT NOT NULL,
    model           TEXT NOT NULL,
    request_ceiling BIGINT NOT NULL,
    bound_units     BIGINT NOT NULL,
    settled_units   BIGINT,
    state           TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    settled_at      TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS mcp_token_attempts_invocation
    ON mcp_token_attempts (tenant_id, invocation_id);

CREATE INDEX IF NOT EXISTS mcp_token_attempts_payer_day
    ON mcp_token_attempts (tenant_id, payer, day);

CREATE TABLE IF NOT EXISTS mcp_token_attempt_scopes (
    attempt_id TEXT NOT NULL,
    tenant_id  TEXT NOT NULL,
    scope_kind TEXT NOT NULL,
    scope_id   TEXT NOT NULL,
    day        DATE NOT NULL,
    PRIMARY KEY (attempt_id, scope_kind, scope_id)
);

CREATE INDEX IF NOT EXISTS mcp_token_attempt_scopes_day
    ON mcp_token_attempt_scopes (tenant_id, scope_kind, scope_id, day);

CREATE TABLE IF NOT EXISTS mcp_bound_violations (
    attempt_id      TEXT PRIMARY KEY,
    tenant_id       TEXT NOT NULL,
    provider_config TEXT NOT NULL,
    model           TEXT NOT NULL,
    bound_units     BIGINT NOT NULL,
    observed_units  BIGINT NOT NULL,
    recorded_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    cleared_at      TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS mcp_bound_violations_open
    ON mcp_bound_violations (tenant_id, provider_config, model)
    WHERE cleared_at IS NULL;
