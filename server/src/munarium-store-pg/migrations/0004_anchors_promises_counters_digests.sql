-- Every store carries a seq stamp from the one lineage counting domain, so a
-- single as_of_seq pin bounds facts, anchors, promises, and counters together.
CREATE TABLE IF NOT EXISTS anchors (
    tenant_id       TEXT NOT NULL,
    id              TEXT NOT NULL,
    version_id      TEXT NOT NULL,
    detail_key      TEXT NOT NULL,
    locked_value    TEXT NOT NULL,
    locked_at_scope TEXT,
    status          TEXT NOT NULL DEFAULT 'locked',
    seq             BIGINT NOT NULL,
    evidence        JSONB,
    PRIMARY KEY (tenant_id, id)
);
CREATE INDEX IF NOT EXISTS idx_anchors_version ON anchors (tenant_id, version_id, seq);

CREATE TABLE IF NOT EXISTS promises (
    tenant_id     TEXT NOT NULL,
    id            TEXT NOT NULL,
    version_id    TEXT NOT NULL,
    key           TEXT NOT NULL,
    kind          TEXT NOT NULL,
    description   TEXT NOT NULL,
    origin_scope  TEXT,
    due_scope     TEXT,
    status        TEXT NOT NULL DEFAULT 'open',
    seq           BIGINT NOT NULL,
    fulfilled_seq BIGINT,
    PRIMARY KEY (tenant_id, id)
);
CREATE INDEX IF NOT EXISTS idx_promises_version ON promises (tenant_id, version_id, seq);

CREATE TABLE IF NOT EXISTS mesh_counters (
    tenant_id  TEXT NOT NULL,
    version_id TEXT NOT NULL,
    key        TEXT NOT NULL,
    scope_path TEXT NOT NULL,
    count      BIGINT NOT NULL DEFAULT 0,
    budget     BIGINT,
    seq        BIGINT NOT NULL,
    PRIMARY KEY (tenant_id, version_id, key, scope_path)
);

-- Stored digest rungs (no history — which is exactly why pinned reads REBUILD
-- from pinned facts instead of serving these).
CREATE TABLE IF NOT EXISTS digests (
    tenant_id      TEXT NOT NULL,
    version_id     TEXT NOT NULL,
    tier           SMALLINT NOT NULL,
    scope_path     TEXT NOT NULL DEFAULT '',
    content        TEXT NOT NULL,
    content_hash   TEXT NOT NULL,
    built_from_seq BIGINT NOT NULL,
    PRIMARY KEY (tenant_id, version_id, tier, scope_path)
);
