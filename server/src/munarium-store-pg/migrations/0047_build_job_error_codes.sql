-- SPDX-License-Identifier: Apache-2.0
-- 0047 (2026-11-20, ClearView R5 / C2): a stable failure code for build jobs.
--
-- `error_code` is written only when a builder records a failure whose typed
-- cause it classified (munarium_store_pg::job_failure). It is NULL for every
-- existing row, for success, and for a failure written by a v1.4 builder
-- during rollout. The read path never derives a code from `error` text: a
-- failed row without a code is returned with a generic build-failure message
-- and no `error_code` member. Existing rows are not touched, so historical
-- `error` text stays in the table for the database operator.
--
-- The CHECK validates the code's syntax on write: lowercase snake case, at
-- most 64 characters. The vocabulary itself is the error registry
-- (docs/api/errors.md, "Build-job failure codes").
--
-- Additive: one new nullable column with a check that every existing (NULL)
-- row satisfies. Rollback: as for 0043, a v1.4.0 binary refuses to start at
-- this migration level; a v1.4 builder running beside v1.5 replicas writes
-- `error` without touching this column, which the read rule above covers.

ALTER TABLE index_build_jobs
    ADD COLUMN IF NOT EXISTS error_code TEXT;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'index_build_jobs_error_code_syntax'
           AND conrelid = 'index_build_jobs'::regclass
    ) THEN
        ALTER TABLE index_build_jobs
            ADD CONSTRAINT index_build_jobs_error_code_syntax
            CHECK (error_code IS NULL OR error_code ~ '^[a-z][a-z0-9_]{0,63}$');
    END IF;
END
$$;
