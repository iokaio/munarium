-- SPDX-License-Identifier: Apache-2.0
-- 0085 (ClearView §5.3): tombstones for pruned MCP operation keys.
--
-- Additive only. The key janitor deletes an `mcp_operations` row a day after
-- its expiry. Without a trace of the deleted row, a client presenting the
-- same key id with a new `expires_at` could claim it again and run the
-- operation a second time. The janitor now records the minimal identity of
-- each key it deletes here, in the same statement, and admission refuses a
-- key that has a tombstone. Tombstones are never pruned by the server; an
-- operator may delete them directly. Content-free: tenant, uid, tool, the
-- caller-chosen random key id, the key's immutable expiry and the prune time.
CREATE TABLE IF NOT EXISTS mcp_operation_tombstones (
    tenant_id  TEXT        NOT NULL,
    uid        TEXT        NOT NULL,
    tool       TEXT        NOT NULL,
    key_id     TEXT        NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    pruned_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, uid, tool, key_id)
);
