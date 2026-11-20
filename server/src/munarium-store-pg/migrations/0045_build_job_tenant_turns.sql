-- SPDX-License-Identifier: Apache-2.0
-- 0045 (2026-11-19, ClearView R8 / B1): tenant turns for opt-in fair claiming.
--
-- The v1.4 claim is global FIFO by `created_at`, so one tenant's large
-- backlog is served to completion before another tenant's later job. With
-- `MUNARIUM_DATASTORE_JOB_SCHEDULING=tenant_fair`, a builder instead gives
-- every tenant with eligible work an equal turn: the tenant served least
-- recently goes next (never-served tenants first), and within that tenant
-- its oldest eligible job, with the job id as a stable tie-break. A turn is
-- one committed claim; the tenant's position advances in the claiming
-- transaction, so a rolled-back claim leaves the turn available.
--
-- One row per tenant that has ever enqueued a build job. The claim locks the
-- tenant's row with the job row (`FOR UPDATE ... SKIP LOCKED`), so two
-- concurrent builders never spend the same tenant's turn twice. The row is
-- created by a trigger on job insert, which also covers a v1.4 writer, and
-- existing tenants are added below.
--
-- The default (`fifo`) never reads or writes this table, and expired-claim
-- cleanup never advances it.
--
-- Additive: one table, one sequence, one trigger. Rollback: as for 0043, a
-- v1.4.0 binary refuses to start at this migration level; disable fair
-- claiming before a v1.4 builder returns (see the Datastore guide).

CREATE TABLE IF NOT EXISTS index_build_tenant_turns (
    tenant_id        TEXT        NOT NULL,
    -- NULL until the tenant's first fair claim; then a value from
    -- `index_build_tenant_turn_seq`, so a smaller value was served earlier.
    last_served_seq  BIGINT,
    last_served_at   TIMESTAMPTZ,
    PRIMARY KEY (tenant_id)
);

CREATE SEQUENCE IF NOT EXISTS index_build_tenant_turn_seq;

CREATE OR REPLACE FUNCTION index_build_jobs_tenant_turn() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO index_build_tenant_turns (tenant_id)
    VALUES (NEW.tenant_id)
    ON CONFLICT (tenant_id) DO NOTHING;
    RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS index_build_jobs_tenant_turn ON index_build_jobs;
CREATE TRIGGER index_build_jobs_tenant_turn
AFTER INSERT ON index_build_jobs
FOR EACH ROW EXECUTE FUNCTION index_build_jobs_tenant_turn();

INSERT INTO index_build_tenant_turns (tenant_id)
SELECT DISTINCT tenant_id FROM index_build_jobs
ON CONFLICT (tenant_id) DO NOTHING;
