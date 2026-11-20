-- SPDX-License-Identifier: Apache-2.0
-- 0087 (2026-11-20, ClearView SCALE-3 and U10 / LOGIN1): replica heartbeats,
-- the fleet-wide console login mode and the console session epoch.
--
-- `server_replica_heartbeats`: one row per running Server process, written
-- at startup and every 15 seconds on the database clock, scoped by
-- MUNARIUM_DEPLOYMENT_ENVIRONMENT_ID. A row is live while `expires_at` is in
-- the future; expired rows are ignored by every reader and deleted by the
-- next heartbeat of any replica in the environment. Columns:
--
-- - `replica_id`: a random identity generated once per process. It is not
--   derived from the host and means nothing outside this table.
-- - `instance_id`: MUNARIUM_INSTANCE_ID, else the host name, as the scale
--   report and `console_sessions.created_by` already show it.
-- - `version`, `plane`, `replica_count`: the build version,
--   MUNARIUM_DEPLOYMENT_PLANE and the process's MUNARIUM_REPLICA_COUNT.
-- - `capabilities`: `console-sessions` when the build supports revocable
--   console sessions and MUNARIUM_CONSOLE_SESSIONS allows them.
--
-- A v1.4 process never writes a row, so the scale report and the console
-- cutover compare live rows with the configured replica count instead
-- (server/docs/design/console-sessions.md#cutover).
--
-- `console_fleet_mode`: the console login mode of every replica in an
-- environment, changed only by `munarium-server console-fleet`. No row means
-- `legacy` at epoch 0, which is the v1.4 login. `epoch` is incremented on
-- every switch from `enabled` back to `legacy`.
--
-- `console_sessions.fleet_environment` and `fleet_epoch`: the environment and
-- epoch a session was issued under. A replica honours a session only while
-- its environment's mode is `enabled` at that same epoch, so a fleet that is
-- rolled back and enabled again refuses every handle issued before. Rows
-- written before this migration have NULLs and are never honoured again.
--
-- Additive: two new tables and two nullable columns; no existing row is
-- rewritten and no existing index is touched.

CREATE TABLE IF NOT EXISTS server_replica_heartbeats (
    environment_id TEXT NOT NULL,
    replica_id     TEXT NOT NULL,
    instance_id    TEXT NOT NULL,
    version        TEXT NOT NULL,
    plane          TEXT NOT NULL,
    replica_count  INT NOT NULL,
    capabilities   TEXT[] NOT NULL DEFAULT '{}',
    started_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at     TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (environment_id, replica_id)
);

CREATE INDEX IF NOT EXISTS server_replica_heartbeats_expires
    ON server_replica_heartbeats (environment_id, expires_at);

CREATE TABLE IF NOT EXISTS console_fleet_mode (
    environment_id TEXT PRIMARY KEY,
    mode           TEXT NOT NULL CHECK (mode IN ('legacy', 'enabled')),
    epoch          BIGINT NOT NULL CHECK (epoch >= 0),
    changed_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    changed_by     TEXT NOT NULL
);

ALTER TABLE console_sessions ADD COLUMN IF NOT EXISTS fleet_environment TEXT;
ALTER TABLE console_sessions ADD COLUMN IF NOT EXISTS fleet_epoch BIGINT;
