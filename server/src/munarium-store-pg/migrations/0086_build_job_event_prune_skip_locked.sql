-- SPDX-License-Identifier: Apache-2.0
-- 0086 (2026-11-20, ClearView B2 follow-up): the build-job event ring's
-- prune no longer waits on rows another transaction is already pruning.
--
-- Migration 0046's trigger pruned with a plain
-- `DELETE ... WHERE tenant_id = ... AND event_id <= boundary`. Two
-- transactions pruning the same tenant's ring lock the same old event rows,
-- in whatever order each one's plan visits them, and a statement that moves
-- several job rows (`fail_expired_final_claims`) prunes once per row while
-- it holds every one of those job rows' locks. A prune could therefore block
-- a job transition behind another transaction's prune, and two prunes that
-- reached shared rows in different orders could deadlock, aborting a job
-- state change.
--
-- The replacement locks the rows to prune in event order and skips any row
-- another transaction has already locked. Only a prune locks event rows, so a
-- skipped row is one a concurrent prune is deleting. Retention is unchanged:
-- the newest `events_per_tenant` events of a tenant are never deleted, and
-- when the concurrent prune commits, the result is exactly what 0046 left
-- (0046's prune also skipped a row that another prune deleted while it
-- waited). If that concurrent prune rolls back, its job change and its own
-- event roll back with it, and the rows it had locked stay until the next
-- event recorded for the tenant prunes them.
--
-- Additive: replaces the trigger FUNCTION only (CREATE OR REPLACE); the
-- trigger, tables and the event-numbering function from 0046 are unchanged,
-- and no row is touched. Rollback: as for 0043, an older binary refuses to
-- start at this migration level (VersionMissing).

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
        DELETE FROM index_build_job_events e
         USING (SELECT event_id FROM index_build_job_events
                 WHERE tenant_id = NEW.tenant_id AND event_id <= boundary
                 ORDER BY event_id
                 FOR UPDATE SKIP LOCKED) doomed
         WHERE e.tenant_id = NEW.tenant_id AND e.event_id = doomed.event_id;
    END IF;
    RETURN NULL;
END;
$$;
