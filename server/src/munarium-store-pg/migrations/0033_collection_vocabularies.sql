-- SPDX-License-Identifier: Apache-2.0
CREATE TABLE vocabulary_settings (
    tenant_id TEXT PRIMARY KEY,
    settings JSONB NOT NULL,
    revision BIGINT NOT NULL DEFAULT 1
);
CREATE TABLE collection_vocabularies (
    tenant_id TEXT NOT NULL,
    collection_id TEXT NOT NULL,
    vocabulary JSONB NOT NULL,
    revision BIGINT NOT NULL DEFAULT 1,
    lease_id TEXT,
    lease_until TIMESTAMPTZ,
    attempted_at TIMESTAMPTZ,
    PRIMARY KEY (tenant_id, collection_id),
    FOREIGN KEY (tenant_id, collection_id) REFERENCES collections(tenant_id, id)
);
