-- SPDX-License-Identifier: Apache-2.0
-- 0050 (2026-11-19, ClearView U10 / LOGIN1): revocable console sessions.
--
-- The shared store behind MUNARIUM_CONSOLE_SESSIONS=enabled
-- (server/docs/design/console-sessions.md). One row per console sign-in:
--
-- - `handle_digest`: SHA-256 of the opaque handle the browser cookie carries.
--   The handle itself is never stored.
-- - `session_id`: the public identifier the console lists and revokes by. It
--   grants nothing on its own.
-- - `tenant_id`, `role`, `credential_digest`: the tenant and authority the
--   session is bound to, and a digest of the static credential that signed
--   in, salted with the session id. Every request re-resolves the credential
--   against the replica's configuration.
-- - `binding_secret`: the per-session secret the CSRF token and review plans
--   derive from. Never sent to the browser.
-- - `last_seen_at`, `idle_secs`, `expires_at`: idle and absolute expiry,
--   enforced on every request with the database clock.
-- - `revoked_at`, `revoked_reason`: sign-out or console revocation. A revoked
--   row is never live again.
--
-- Rows are deleted at the next sign-in once they are seven days past
-- `expires_at`. A v1.4 replica never reads this table, and a replica in the
-- default `legacy` mode never reads the session cookie, so the table changes
-- nothing until an operator switches the fleet.
--
-- Additive: one new table and its indexes; no existing table, column, index
-- or row is touched.

CREATE TABLE IF NOT EXISTS console_sessions (
    handle_digest     TEXT PRIMARY KEY,
    session_id        TEXT NOT NULL UNIQUE,
    tenant_id         TEXT NOT NULL,
    role              TEXT NOT NULL,
    credential_digest TEXT NOT NULL,
    binding_secret    TEXT NOT NULL,
    created_by        TEXT NOT NULL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    idle_secs         BIGINT NOT NULL,
    expires_at        TIMESTAMPTZ NOT NULL,
    revoked_at        TIMESTAMPTZ,
    revoked_reason    TEXT
);

CREATE INDEX IF NOT EXISTS console_sessions_tenant_created
    ON console_sessions (tenant_id, created_at DESC);

CREATE INDEX IF NOT EXISTS console_sessions_expires
    ON console_sessions (expires_at);
