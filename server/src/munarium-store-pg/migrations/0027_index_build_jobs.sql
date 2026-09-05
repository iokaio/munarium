-- 0027 (2026-08-30, datastore Phase 9): durable build jobs.
--
-- §8.6's rule was "do not deploy a separate builder role until a durable job
-- model exists" — this is that model. A job is the DURABLE REQUEST for a
-- build; an attempt (0026's `index_build_attempts`) is one EXECUTION of one.
-- The two are deliberately separate tables: a job survives the process that
-- was running it, carries the caller's intent and correlation, and can be
-- retried through fresh attempts without losing its identity.
--
-- Additive: no existing table is touched. A deployment that never enqueues a
-- job carries one empty table and behaves exactly as before.

CREATE TABLE IF NOT EXISTS index_build_jobs (
    tenant_id        TEXT        NOT NULL,
    job_id           TEXT        NOT NULL,
    -- backfill | rebuild | direct
    kind             TEXT        NOT NULL,
    -- What the job builds. backfill/direct name a scope; rebuild names a
    -- version. Both nullable because each kind uses its own pair, and a CHECK
    -- below keeps a job from being enqueued with neither.
    scope_kind       TEXT,
    scope_id         TEXT,
    index_version_id TEXT,
    -- Bounded caller parameters (max_chars, watermark_seq, …). JSONB so a new
    -- knob is not a migration.
    params           JSONB       NOT NULL DEFAULT '{}'::jsonb,
    -- pending | running | succeeded | failed | cancelled | superseded
    state            TEXT        NOT NULL DEFAULT 'pending',
    -- The attempt row(s) that executed this job, newest last. An array
    -- because a retried job runs through more than one attempt, and replacing
    -- the link would erase the history the attempt table still holds.
    attempt_ids      TEXT[]      NOT NULL DEFAULT '{}',
    -- Runbook/execution/step correlation, where the caller has one.
    correlation_id   TEXT,
    claimed_by       TEXT,
    claimed_at       TIMESTAMPTZ,
    -- How many times this job has been CLAIMED. A lease-lapsed reclaim
    -- increments it; a bounded ceiling stops a poisonous job from being
    -- retried forever.
    attempts         INT         NOT NULL DEFAULT 0,
    -- Bounded outcome. The full build record lives with the attempt and the
    -- catalog; this is what a caller polling the job needs to read.
    result           JSONB,
    error            TEXT,
    requested_by     TEXT        NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, job_id),
    CONSTRAINT index_build_jobs_target CHECK (
        (scope_kind IS NOT NULL AND scope_id IS NOT NULL) OR index_version_id IS NOT NULL
    )
);

-- The claim scan: pending first-in-first-out, plus lease-lapsed running rows.
CREATE INDEX IF NOT EXISTS index_build_jobs_claim_idx
    ON index_build_jobs (state, created_at);

-- Enqueue dedup: at most one OPEN job per (tenant, kind, target). Partial
-- unique index rather than application logic, so two concurrent enqueues
-- cannot both win.
CREATE UNIQUE INDEX IF NOT EXISTS index_build_jobs_open_target_idx
    ON index_build_jobs (
        tenant_id, kind,
        COALESCE(scope_kind, ''), COALESCE(scope_id, ''),
        COALESCE(index_version_id, '')
    )
    WHERE state IN ('pending', 'running');
