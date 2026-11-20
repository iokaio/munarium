-- SPDX-License-Identifier: Apache-2.0
-- 0044 (2026-11-19, ClearView R6): durable per-version outcomes for queued
-- builds, so a cancelled or retired job can be reconciled and reported.
--
-- 0043 already records the job and generation behind a logical-version commit,
-- an artifact attempt and a publication intent. Two facts were missing:
--
-- - `index_build_job_runs`: which generations of a job a fenced builder
--   claimed, written in the claiming transaction. A job whose generations have
--   no run rows was claimed by a v1.4 builder (or before this migration), so
--   its per-version history is reported as unavailable, never as an empty
--   page that would mean "it did nothing".
-- - `index_build_job_versions`: the versions a backfill or rebuild selected,
--   retained BEFORE the first version starts, plus what the job found for a
--   version without publishing (`reused`: a verified artifact already existed;
--   `deferred`: another attempt held the plan). A selected version with no
--   attempt, commit or disposition is reported as untouched.
--
-- Additive: two new tables; no existing table, column, index or row changes.
-- Rollback: a v1.4.0 binary refuses to start against this migration level
-- (sqlx VersionMissing), exactly as for 0043; see the Datastore guide.

CREATE TABLE IF NOT EXISTS index_build_job_runs (
    tenant_id        TEXT        NOT NULL,
    job_id           TEXT        NOT NULL,
    -- `index_build_jobs.attempts` after the claim: the claim's fence.
    job_generation   INT         NOT NULL,
    -- The claiming node. Already exposed as `claimed_by` on the job row.
    node_id          TEXT        NOT NULL,
    claimed_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, job_id, job_generation)
);

CREATE TABLE IF NOT EXISTS index_build_job_versions (
    tenant_id        TEXT        NOT NULL,
    job_id           TEXT        NOT NULL,
    index_version_id TEXT        NOT NULL,
    -- Position in the selected set (0 for a single-version job).
    ordinal          INT         NOT NULL,
    -- The generation that selected the version.
    job_generation   INT         NOT NULL,
    -- NULL | reused | deferred. What the job observed without publishing.
    disposition      TEXT,
    selected_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, job_id, index_version_id),
    CONSTRAINT index_build_job_versions_disposition
        CHECK (disposition IS NULL OR disposition IN ('reused', 'deferred'))
);

-- Recovery reads a job's commits by job, not by version.
CREATE INDEX IF NOT EXISTS index_version_job_commits_job_idx
    ON index_version_job_commits (tenant_id, job_id, job_generation);
