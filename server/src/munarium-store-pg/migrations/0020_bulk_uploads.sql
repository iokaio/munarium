-- Bulk upload sessions: the chunked, resumable corpus-loading plane.
--
-- A session opens with a MANIFEST (filename + sha256 + size + media type per
-- document); the server diffs it against `sources` so a re-run after any
-- failure knows exactly which documents remain. Chunks then arrive through
-- the ordinary ingest path (same storage, same `sources` rows, same
-- collection matchers) with per-document idempotency — re-sending an entire
-- failed chunk re-writes nothing already stored. Collection binding is
-- unchanged: it reads `sources`, which this plane populates via the same
-- put_source path as single ingest.
--
-- Sessions are tenant-scoped bookkeeping, not a second storage path.

CREATE TABLE IF NOT EXISTS bulk_uploads (
    tenant_id    TEXT NOT NULL,
    bulk_id      TEXT NOT NULL,           -- 'blk-' + uuid7 simple
    label        TEXT,
    status       TEXT NOT NULL DEFAULT 'open',  -- open | completed | expired
    total        INTEGER NOT NULL,
    created_by   TEXT NOT NULL,           -- uid of the opener
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at   TIMESTAMPTZ NOT NULL,
    completed_at TIMESTAMPTZ,
    PRIMARY KEY (tenant_id, bulk_id)
);

CREATE TABLE IF NOT EXISTS bulk_upload_files (
    tenant_id  TEXT NOT NULL,
    bulk_id    TEXT NOT NULL,
    filename   TEXT NOT NULL,             -- logical path, same identity rules as sources
    sha256     TEXT NOT NULL,             -- declared hex sha-256; verified per chunk file
    bytes_len  BIGINT NOT NULL,
    media_type TEXT NOT NULL,
    -- pending          = declared, bytes not yet received this session
    -- stored           = ingested this session (new bytes written)
    -- skipped_existing = sources already held these exact bytes
    -- failed           = last attempt failed (error recorded); retryable
    status     TEXT NOT NULL DEFAULT 'pending',
    error      TEXT,
    source_id  TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, bulk_id, filename)
);

CREATE INDEX IF NOT EXISTS bulk_upload_files_status
    ON bulk_upload_files (tenant_id, bulk_id, status);
