-- SPDX-License-Identifier: Apache-2.0
-- 0046 (2026-11-19, ClearView R9 / B2): fenced job progress and a
-- transactional, per-tenant bounded event ring.
--
-- Progress: four nullable columns on the job row, written only by the claim
-- that holds the job (state `running`, matching node and generation), at the
-- same safe boundaries R6 checks. A stale holder's write matches no row. The
-- counters never decrease within a claim, and an unknown total stays NULL,
-- never zero. A new claim clears them: progress belongs to one generation.
--
-- Events: `index_build_job_events` is written ONLY by triggers on
-- `index_build_jobs`, so an event commits or rolls back with the state change
-- that caused it and covers v1.4 writers as well. Meaningful transitions are
-- an insert (enqueue), a change of state, of `attempts` (a claim or reclaim)
-- or of progress phase. Heartbeats (`claimed_at` only), progress counters and
-- a no-op replay write no event. Each job numbers its own events
-- (`event_seq` on the job row, `job_event_seq` on the event), assigned under
-- the job row's lock, so a reader can see that older events were pruned. A
-- job created before this migration starts its numbering at 2, so its
-- unrecorded earlier history reads as a gap.
--
-- Retention: the newest `events_per_tenant` events are kept for each tenant,
-- pruned in the inserting transaction. The default, 1024, is PROVISIONAL: the
-- B2 measurement (event rates, row and index storage, read and prune cost and
-- the history window operators need) has not been made. The job row keeps its
-- state and outcome independently of the ring.
--
-- Additive: new nullable columns, two tables, two triggers. Rollback: as for
-- 0043, a v1.4.0 binary refuses to start at this migration level.

ALTER TABLE index_build_jobs
    ADD COLUMN IF NOT EXISTS progress_phase TEXT,
    ADD COLUMN IF NOT EXISTS progress_units_done BIGINT,
    ADD COLUMN IF NOT EXISTS progress_units_total BIGINT,
    ADD COLUMN IF NOT EXISTS progress_advanced_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS event_seq BIGINT;

CREATE TABLE IF NOT EXISTS index_build_job_events (
    tenant_id        TEXT        NOT NULL,
    event_id         BIGINT      GENERATED ALWAYS AS IDENTITY,
    job_id           TEXT        NOT NULL,
    -- 1, 2, 3, ... per job, in commit order (assigned under the row lock).
    job_event_seq    BIGINT      NOT NULL,
    -- enqueued | claimed | state | progress
    kind             TEXT        NOT NULL,
    from_state       TEXT,
    to_state         TEXT        NOT NULL,
    job_generation   INT         NOT NULL,
    -- The holder node already exposed as `claimed_by`.
    node_id          TEXT,
    progress_phase   TEXT,
    recorded_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, event_id)
);

CREATE INDEX IF NOT EXISTS index_build_job_events_job_idx
    ON index_build_job_events (tenant_id, job_id, job_event_seq);

-- One row: the per-tenant retention count. The builder sets it from
-- MUNARIUM_DATASTORE_JOB_EVENTS_PER_TENANT when that is configured.
CREATE TABLE IF NOT EXISTS index_build_job_event_retention (
    singleton          BOOLEAN     NOT NULL DEFAULT true,
    events_per_tenant  INT         NOT NULL,
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (singleton),
    CONSTRAINT index_build_job_event_retention_one CHECK (singleton),
    CONSTRAINT index_build_job_event_retention_positive CHECK (events_per_tenant > 0)
);

INSERT INTO index_build_job_event_retention (singleton, events_per_tenant)
VALUES (true, 1024)
ON CONFLICT (singleton) DO NOTHING;

CREATE OR REPLACE FUNCTION index_build_jobs_event_seq() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        NEW.event_seq := 1;
    ELSIF NEW.state IS DISTINCT FROM OLD.state
       OR NEW.attempts IS DISTINCT FROM OLD.attempts
       OR NEW.progress_phase IS DISTINCT FROM OLD.progress_phase THEN
        NEW.event_seq := COALESCE(OLD.event_seq, 1) + 1;
    ELSE
        NEW.event_seq := OLD.event_seq;
    END IF;
    RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS index_build_jobs_event_seq ON index_build_jobs;
CREATE TRIGGER index_build_jobs_event_seq
BEFORE INSERT OR UPDATE ON index_build_jobs
FOR EACH ROW EXECUTE FUNCTION index_build_jobs_event_seq();

CREATE OR REPLACE FUNCTION index_build_jobs_event() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    keep INT;
    boundary BIGINT;
    ev_kind TEXT;
BEGIN
    IF TG_OP = 'UPDATE' AND NEW.event_seq IS NOT DISTINCT FROM OLD.event_seq THEN
        RETURN NULL;
    END IF;
    IF TG_OP = 'INSERT' THEN
        ev_kind := 'enqueued';
    ELSIF NEW.attempts IS DISTINCT FROM OLD.attempts THEN
        ev_kind := 'claimed';
    ELSIF NEW.state IS DISTINCT FROM OLD.state THEN
        ev_kind := 'state';
    ELSE
        ev_kind := 'progress';
    END IF;
    INSERT INTO index_build_job_events
        (tenant_id, job_id, job_event_seq, kind, from_state, to_state,
         job_generation, node_id, progress_phase)
    VALUES
        (NEW.tenant_id, NEW.job_id, NEW.event_seq, ev_kind,
         CASE WHEN TG_OP = 'UPDATE' THEN OLD.state END, NEW.state,
         NEW.attempts, NEW.claimed_by, NEW.progress_phase);

    SELECT events_per_tenant INTO keep FROM index_build_job_event_retention;
    keep := COALESCE(keep, 1024);
    SELECT event_id INTO boundary FROM index_build_job_events
     WHERE tenant_id = NEW.tenant_id
     ORDER BY event_id DESC
     OFFSET keep LIMIT 1;
    IF boundary IS NOT NULL THEN
        DELETE FROM index_build_job_events
         WHERE tenant_id = NEW.tenant_id AND event_id <= boundary;
    END IF;
    RETURN NULL;
END;
$$;

DROP TRIGGER IF EXISTS index_build_jobs_event ON index_build_jobs;
CREATE TRIGGER index_build_jobs_event
AFTER INSERT OR UPDATE ON index_build_jobs
FOR EACH ROW EXECUTE FUNCTION index_build_jobs_event();
