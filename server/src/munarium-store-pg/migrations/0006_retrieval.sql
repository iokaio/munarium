-- Retrieval tier (M3): content-addressed sources, versioned immutable
-- indexes, hybrid lexical+vector chunks. Indexes never mutate once built;
-- cutover is the index_versions.active pointer flip; old versions stay
-- resolvable so provenance envelopes keep verifying.
CREATE EXTENSION IF NOT EXISTS vector;

-- Source identity is the LOGICAL PATH (filename), not the content hash.
-- Collections bind by `filenamePrefix` (a literal starts_with), so the same
-- bytes at two paths must be two independently bindable, rebuildable,
-- retirable sources. content_hash stays as INTEGRITY: verified against any
-- declared sha-256 before commit, recorded, and surfaced in provenance.
--
-- Bytes live in the object store (mmesh-store-az) addressed by the same
-- logical path; the `bytes` column is the fallback 'pg' SourceStore backend
-- that keeps compose/test running with no Azure account.
CREATE TABLE IF NOT EXISTS sources (
    tenant_id         TEXT NOT NULL,
    source_id         TEXT NOT NULL,     -- 'src-' + sha256(tenant/filename)[..16]
    filename          TEXT NOT NULL,     -- the logical path; identity AND blob name
    content_hash      TEXT NOT NULL,     -- hex sha-256, verified before commit
    media_type        TEXT NOT NULL,
    shape_ref         TEXT,
    blob_uri          TEXT NOT NULL,     -- where SourceStore put the bytes
    storage_backend   TEXT NOT NULL,     -- az | pg | mem
    extraction_status TEXT,              -- NULL = not yet built | ok | empty | failed
    extraction_method TEXT,              -- text | docx | pdf-text | ocr
    bytes_len         BIGINT NOT NULL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, source_id)
);
CREATE UNIQUE INDEX IF NOT EXISTS sources_tenant_filename ON sources (tenant_id, filename);
CREATE INDEX IF NOT EXISTS sources_tenant_hash ON sources (tenant_id, content_hash);

-- The 'pg' SourceStore backend: document bytes in Postgres. This is the
-- fallback that keeps `docker compose up` and `cargo test` working with no
-- Azure account and no Azurite. Production uses 'az' (mmesh-store-az) and
-- this table stays empty. Kept SEPARATE from `sources` so the object-store
-- seam is genuinely independent of the metadata row.
CREATE TABLE IF NOT EXISTS source_blobs (
    tenant_id TEXT NOT NULL,
    blob_name TEXT NOT NULL,             -- '{tenant}/{logical path}'
    bytes     BYTEA NOT NULL,
    PRIMARY KEY (tenant_id, blob_name)
);

CREATE TABLE IF NOT EXISTS index_versions (
    tenant_id     TEXT NOT NULL,
    id            TEXT NOT NULL,          -- hash(shape_ref, chunker, embedder, source_set)
    shape_ref     TEXT NOT NULL,
    manifest      JSONB NOT NULL,
    watermark_seq BIGINT NOT NULL DEFAULT 0,
    active        BOOLEAN NOT NULL DEFAULT false,
    built_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, id)
);
CREATE INDEX IF NOT EXISTS idx_index_versions_shape
    ON index_versions (tenant_id, shape_ref, active);

CREATE TABLE IF NOT EXISTS index_chunks (
    tenant_id        TEXT NOT NULL,
    index_version_id TEXT NOT NULL,
    chunk_id         TEXT NOT NULL,       -- '{source_id}#{ordinal}'
    source_id        TEXT NOT NULL,
    source_hash      TEXT NOT NULL,       -- denormalized for the provenance envelope
    ordinal          INTEGER NOT NULL,
    text             TEXT NOT NULL,
    ts               TSVECTOR GENERATED ALWAYS AS (to_tsvector('english', text)) STORED,
    embedding        VECTOR(256),
    PRIMARY KEY (tenant_id, index_version_id, chunk_id)
);
CREATE INDEX IF NOT EXISTS idx_index_chunks_ts
    ON index_chunks USING GIN (ts);
CREATE INDEX IF NOT EXISTS idx_index_chunks_vec
    ON index_chunks USING hnsw (embedding vector_cosine_ops);
