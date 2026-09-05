-- Memory versions form the lineage chain (parent_id); lineage_root_id is
-- denormalized at create so the whole chain shares one seq-allocation lock row.
CREATE TABLE IF NOT EXISTS memory_versions (
    tenant_id        TEXT NOT NULL REFERENCES tenants(id),
    id               TEXT NOT NULL,
    parent_id        TEXT,
    lineage_root_id  TEXT NOT NULL,
    status           TEXT NOT NULL DEFAULT 'active',
    metadata         JSONB,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, id)
);

-- One row per lineage root: the seq-allocation mutex. Appends SELECT ... FOR
-- UPDATE this row (the Postgres equivalent of the lab's BEGIN IMMEDIATE),
-- then compute the chain head inside the same transaction.
CREATE TABLE IF NOT EXISTS lineage_heads (
    tenant_id        TEXT NOT NULL,
    lineage_root_id  TEXT NOT NULL,
    current_seq      BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (tenant_id, lineage_root_id)
);

-- The append-only event ledger (production shape per architecture.md §4.2):
-- range-partitioned on tenant_seq so indexes stay small and cold partitions
-- archive trivially. Partition maintenance is an mmeshctl command.
CREATE TABLE IF NOT EXISTS ledger_events (
    tenant_seq   BIGINT GENERATED ALWAYS AS IDENTITY,
    tenant_id    TEXT NOT NULL,
    version_id   TEXT NOT NULL,
    seq          BIGINT NOT NULL,
    event_type   TEXT NOT NULL,
    body         JSONB NOT NULL,
    body_hash    TEXT,
    shape_ref    TEXT,
    recorded_at  TIMESTAMPTZ NOT NULL DEFAULT now()
) PARTITION BY RANGE (tenant_seq);

CREATE TABLE IF NOT EXISTS ledger_events_p0
    PARTITION OF ledger_events FOR VALUES FROM (0) TO (10000000);
CREATE TABLE IF NOT EXISTS ledger_events_default
    PARTITION OF ledger_events DEFAULT;

CREATE INDEX IF NOT EXISTS idx_ledger_events_version
    ON ledger_events (tenant_id, version_id, seq);
