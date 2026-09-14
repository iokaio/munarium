-- SPDX-License-Identifier: Apache-2.0
-- Immutable source locations for newly built indexes. Older indexes retain
-- their existing identities and report no location until explicitly rebuilt.
CREATE TABLE chunk_provenance (
    tenant_id TEXT NOT NULL,
    index_version_id TEXT NOT NULL,
    chunk_id TEXT NOT NULL,
    source_path TEXT NOT NULL,
    metadata JSONB NOT NULL,
    PRIMARY KEY (tenant_id, index_version_id, chunk_id),
    FOREIGN KEY (tenant_id, index_version_id) REFERENCES index_versions(tenant_id, id) ON DELETE CASCADE
);
