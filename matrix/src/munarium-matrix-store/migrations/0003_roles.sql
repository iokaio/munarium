-- Per-role work queues and the sync checkpoints.
--
-- One queue table per role rather than one table with a `role` column: the
-- roles scale independently and a hung sync must not be able to starve the
-- query role, which is easier to guarantee when they do not share a hot row.
--
-- Claiming is `FOR UPDATE SKIP LOCKED`, the standard Postgres queue idiom: N
-- workers claim disjoint rows without a coordinator.

CREATE TABLE IF NOT EXISTS matrix.sync_jobs (
    id           TEXT        PRIMARY KEY,      -- 'sjb-' + uuid7
    tenant_id    TEXT        NOT NULL,
    source_name  TEXT        NOT NULL,
    entity       TEXT        NOT NULL,
    state        TEXT        NOT NULL DEFAULT 'queued',  -- queued | running | done | failed | refused
    attempts     INTEGER     NOT NULL DEFAULT 0,
    claimed_by   TEXT,
    claimed_at   TIMESTAMPTZ,
    -- Set when the run ends; the run summary itself lives in sync_runs.
    run_id       TEXT,
    scheduled_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_sync_jobs_claimable
    ON matrix.sync_jobs (scheduled_at)
    WHERE state = 'queued';

CREATE TABLE IF NOT EXISTS matrix.sync_runs (
    id                 TEXT        PRIMARY KEY,   -- 'srn-' + uuid7
    tenant_id          TEXT        NOT NULL,
    source_name        TEXT        NOT NULL,
    entity             TEXT        NOT NULL,
    state              TEXT        NOT NULL,      -- running | done | failed | refused
    mode               TEXT        NOT NULL,      -- manifest | snapshot | watermark | cdf | cdc
    records_read       BIGINT      NOT NULL DEFAULT 0,
    records_rendered   BIGINT      NOT NULL DEFAULT 0,
    -- Excluded by policy or drift. Reported, never silently dropped: G4 says a
    -- collection states the rows it covers AND the rows it excludes.
    records_excluded   BIGINT      NOT NULL DEFAULT 0,
    documents_uploaded BIGINT      NOT NULL DEFAULT 0,
    documents_skipped  BIGINT      NOT NULL DEFAULT 0,
    count_evidence_id  TEXT,
    watermark          TEXT,
    refusal_json       JSONB,
    started_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at           TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_sync_runs_source
    ON matrix.sync_runs (tenant_id, source_name, started_at DESC);

CREATE TABLE IF NOT EXISTS matrix.sync_checkpoints (
    tenant_id          TEXT        NOT NULL,
    source_name        TEXT        NOT NULL,
    entity             TEXT        NOT NULL,
    -- The render/mapping version. A bump invalidates the checkpoint ON PURPOSE:
    -- the same rows must be re-rendered, not skipped as already present.
    version            TEXT        NOT NULL,
    watermark          TEXT,
    tie_break          TEXT,
    event_position     TEXT,
    schema_fingerprint TEXT,
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, source_name, entity, version)
);

CREATE TABLE IF NOT EXISTS matrix.mapping_jobs (
    id           TEXT        PRIMARY KEY,
    tenant_id    TEXT        NOT NULL,
    mapping_name TEXT        NOT NULL,
    state        TEXT        NOT NULL DEFAULT 'queued',
    attempts     INTEGER     NOT NULL DEFAULT 0,
    claimed_by   TEXT,
    claimed_at   TIMESTAMPTZ,
    run_id       TEXT,
    scheduled_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_mapping_jobs_claimable
    ON matrix.mapping_jobs (scheduled_at)
    WHERE state = 'queued';

CREATE TABLE IF NOT EXISTS matrix.mapping_runs (
    id             TEXT        PRIMARY KEY,
    tenant_id      TEXT        NOT NULL,
    mapping_name   TEXT        NOT NULL,
    state          TEXT        NOT NULL,
    observations   BIGINT      NOT NULL DEFAULT 0,
    discrepancies  BIGINT      NOT NULL DEFAULT 0,
    ambiguous      BIGINT      NOT NULL DEFAULT 0,
    findings_filed BIGINT      NOT NULL DEFAULT 0,
    batch_evidence_id TEXT,
    refusal_json   JSONB,
    started_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at       TIMESTAMPTZ
);

-- Observations that have been PROCESSED, keyed by the checkpoint idempotency
-- key. This is what makes "event replay is idempotent" a fact: a replayed
-- event finds its key here and produces no second observation.
CREATE TABLE IF NOT EXISTS matrix.observed_events (
    tenant_id       TEXT        NOT NULL,
    idempotency_key TEXT        NOT NULL,
    mapping_name    TEXT        NOT NULL,
    run_id          TEXT        NOT NULL,
    observed_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, idempotency_key)
);

-- Documents already uploaded for a sync, same idea one plane over: a replayed
-- checkpoint re-renders the same bytes and the manifest diff finds nothing to
-- do, but this table means we do not even ask.
CREATE TABLE IF NOT EXISTS matrix.uploaded_documents (
    tenant_id       TEXT        NOT NULL,
    idempotency_key TEXT        NOT NULL,
    source_name     TEXT        NOT NULL,
    path            TEXT        NOT NULL,
    content_hash    TEXT        NOT NULL,
    uploaded_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, idempotency_key)
);

CREATE INDEX IF NOT EXISTS idx_uploaded_documents_path
    ON matrix.uploaded_documents (tenant_id, source_name, path);
