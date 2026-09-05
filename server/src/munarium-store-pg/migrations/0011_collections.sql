-- M8: first-class collections — indexes as separate, compartmentalized data
-- collections. Physical isolation is LIST partitioning of collection_chunks
-- by collection_id: one partition per collection, created at collection
-- creation (runtime DDL under an advisory lock). The partition is the unit
-- the DBA detaches/drops in the manual deletion runbook
-- (docs/ops/index-deletion-runbook.md) — no API can delete index data.
--
-- The legacy shape-scoped tables (sources.shape_ref binding, index_chunks)
-- stay in place serving the deprecated single-shape path; additive-only.
CREATE TABLE IF NOT EXISTS collections (
    tenant_id    TEXT NOT NULL,
    id           TEXT NOT NULL,                 -- 'col-…' (globally unique; the LIST key)
    name         TEXT NOT NULL,                 -- tenant-unique human name
    shape_ref    TEXT NOT NULL,
    access_level INTEGER NOT NULL DEFAULT 0,    -- token.lvl must dominate
    compartments TEXT[] NOT NULL DEFAULT '{}',  -- token.cmp must be a superset
    status       TEXT NOT NULL DEFAULT 'active',-- active | retired (soft; rows never deleted)
    description  TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, id),
    UNIQUE (tenant_id, name)
);

-- Many-to-many: one source can feed several collections. Keyed by source_id
-- (the logical path), not content_hash — two paths holding identical bytes
-- are two sources and bind independently.
CREATE TABLE IF NOT EXISTS collection_sources (
    tenant_id     TEXT NOT NULL,
    collection_id TEXT NOT NULL,
    source_id     TEXT NOT NULL,
    bound_by_uid  TEXT,
    bound_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, collection_id, source_id)
);

-- Collection-scoped index versions reuse the existing table; NULL = legacy
-- shape-scoped rows. Exactly one active index per (tenant, collection).
ALTER TABLE index_versions ADD COLUMN IF NOT EXISTS collection_id TEXT;
CREATE UNIQUE INDEX IF NOT EXISTS idx_index_versions_active_collection
    ON index_versions (tenant_id, collection_id) WHERE active AND collection_id IS NOT NULL;

-- The partitioned chunk store. Parent-declared indexes cascade to every
-- partition (each collection gets its own GIN + HNSW — smaller ANN graphs,
-- partition pruning at query time). No DEFAULT partition on purpose: an
-- insert for an unknown collection must fail loudly.
CREATE TABLE IF NOT EXISTS collection_chunks (
    tenant_id        TEXT NOT NULL,
    collection_id    TEXT NOT NULL,
    index_version_id TEXT NOT NULL,
    chunk_id         TEXT NOT NULL,      -- '{source_id}#{ordinal}'
    source_id        TEXT NOT NULL,
    source_hash      TEXT NOT NULL,      -- denormalized for the provenance envelope
    ordinal          INTEGER NOT NULL,
    text             TEXT NOT NULL,
    ts               TSVECTOR GENERATED ALWAYS AS (to_tsvector('english', text)) STORED,
    embedding        VECTOR(256),
    PRIMARY KEY (tenant_id, collection_id, index_version_id, chunk_id)
) PARTITION BY LIST (collection_id);
CREATE INDEX IF NOT EXISTS idx_collection_chunks_ts
    ON collection_chunks USING GIN (ts);
CREATE INDEX IF NOT EXISTS idx_collection_chunks_vec
    ON collection_chunks USING hnsw (embedding vector_cosine_ops);
