-- SPDX-License-Identifier: Apache-2.0
-- 0043 (2026-11-19, ClearView R4): publication fencing for queued builds.
--
-- 0027's job row carries the claim's fencing generation (`attempts` after the
-- claim), and the builder already requires it for lease renewal and
-- completion. A queued build also writes OUTSIDE the job row: a direct build
-- commits a PostgreSQL logical version, and every build publishes an artifact
-- through an attempt (0026). Those effects are fenced by taking the job row's
-- lock (`SELECT ... FOR UPDATE`) and rechecking state, generation and holder in
-- the same transaction that records them. Cancel, completion, reclaim and
-- expired-claim cleanup all UPDATE that row, so they serialize against it.
--
-- Three records make the fence durable:
--
-- - `index_version_job_commits`: the job and generation that committed a
--   logical version, written in the version's own commit transaction.
-- - `index_build_attempt_jobs`: the job and generation an artifact attempt
--   was acquired under, written before anything is published.
-- - `index_build_publication_intents`: the ONE immutable manifest key and
--   digest a generation decided to publish, committed while that generation
--   was current and before the catalog insert or any upload. After the
--   generation is retired, a worker or the sealed-attempt reconciler may only
--   finish that exact intent and record what it observed; it may not bind the
--   artifact or authorize another publication.
--
-- Synchronous (non-job) builds write none of these rows and keep their
-- existing artifact-attempt authority.
--
-- Additive: three new tables; no existing table, column, index or row is
-- touched. A v1.4 row has no attribution, link or intent, which reads as
-- "unfenced" and keeps its v1.4 meaning.
--
-- Rollback note: a v1.4.0 binary runs `sqlx::migrate!` without
-- `ignore_missing`, so it refuses to start against a database that has applied
-- this migration (VersionMissing). Returning to v1.4.0 therefore needs either a
-- v1.4.x binary that tolerates unknown migrations or a restore of a backup
-- taken before the upgrade. The maintainer tracks that decision. Before any
-- v1.4 builder returns to service, drain the fenced builders and resolve
-- every `pending` publication intent (see the Datastore guide).

CREATE TABLE IF NOT EXISTS index_version_job_commits (
    tenant_id        TEXT        NOT NULL,
    index_version_id TEXT        NOT NULL,
    job_id           TEXT        NOT NULL,
    -- The committing claim's generation (`index_build_jobs.attempts`).
    job_generation   INT         NOT NULL,
    committed_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, index_version_id)
);

CREATE TABLE IF NOT EXISTS index_build_attempt_jobs (
    tenant_id        TEXT        NOT NULL,
    attempt_id       TEXT        NOT NULL,
    index_version_id TEXT        NOT NULL,
    job_id           TEXT        NOT NULL,
    job_generation   INT         NOT NULL,
    linked_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, attempt_id)
);

CREATE INDEX IF NOT EXISTS index_build_attempt_jobs_job_idx
    ON index_build_attempt_jobs (tenant_id, job_id, job_generation);

CREATE TABLE IF NOT EXISTS index_build_publication_intents (
    tenant_id        TEXT        NOT NULL,
    attempt_id       TEXT        NOT NULL,
    index_version_id TEXT        NOT NULL,
    job_id           TEXT        NOT NULL,
    job_generation   INT         NOT NULL,
    artifact_id      TEXT        NOT NULL,
    -- The canonical manifest's object key and its sha256. Immutable: the
    -- intent is the whole authority a retired generation keeps.
    manifest_key     TEXT        NOT NULL,
    manifest_sha256  TEXT        NOT NULL,
    -- pending | published | converged | failed
    state            TEXT        NOT NULL DEFAULT 'pending',
    -- Bounded, safe outcome text; never source or query content.
    outcome_detail   TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at      TIMESTAMPTZ,
    PRIMARY KEY (tenant_id, attempt_id),
    CONSTRAINT index_build_publication_intents_link
        FOREIGN KEY (tenant_id, attempt_id)
        REFERENCES index_build_attempt_jobs (tenant_id, attempt_id),
    CONSTRAINT index_build_publication_intents_state
        CHECK (state IN ('pending', 'published', 'converged', 'failed'))
);

-- Operators drain outstanding intents before a v1.4 builder returns.
CREATE INDEX IF NOT EXISTS index_build_publication_intents_pending_idx
    ON index_build_publication_intents (tenant_id, created_at)
    WHERE state = 'pending';
