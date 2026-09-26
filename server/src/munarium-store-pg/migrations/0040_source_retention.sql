-- SPDX-License-Identifier: Apache-2.0
-- Explicit tenant-wide source denial; original-byte cleanup is PostgreSQL-only.
CREATE TABLE source_retention (
    tenant_id TEXT NOT NULL,
    source_id TEXT NOT NULL,
    path TEXT NOT NULL,
    denied BOOLEAN NOT NULL DEFAULT false,
    hold BOOLEAN NOT NULL DEFAULT false,
    cleanup_state TEXT NOT NULL DEFAULT 'retained'
        CHECK (cleanup_state IN ('retained', 'pending', 'completed')),
    cleanup_blocked TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, source_id),
    UNIQUE (tenant_id, path),
    CHECK (cleanup_state = 'retained' OR denied)
);
CREATE INDEX source_retention_pending ON source_retention (updated_at)
    WHERE cleanup_state = 'pending';

-- Lock the same stable path as denial, holds and cleanup. Even a writer already
-- in flight cannot recreate PG bytes or membership after a denial commits.
CREATE FUNCTION enforce_source_retention() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE source_path TEXT;
BEGIN
    IF TG_TABLE_NAME = 'source_blobs' THEN
        source_path := substring(NEW.blob_name FROM char_length(NEW.tenant_id) + 2);
    ELSIF TG_TABLE_NAME = 'sources' THEN
        source_path := NEW.filename;
    ELSE
        SELECT filename INTO source_path FROM sources
            WHERE tenant_id=NEW.tenant_id AND source_id=NEW.source_id;
    END IF;
    IF source_path IS NULL OR source_path LIKE 'evidence/%' THEN
        RETURN NEW;
    END IF;
    PERFORM pg_advisory_xact_lock(hashtextextended('source-retention/' || NEW.tenant_id || '/' || source_path, 0));
    IF EXISTS(SELECT 1 FROM source_retention WHERE tenant_id=NEW.tenant_id AND path=source_path AND denied) THEN
        RAISE EXCEPTION 'source is denied by retention policy' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER source_retention_blob_write BEFORE INSERT OR UPDATE ON source_blobs
    FOR EACH ROW EXECUTE FUNCTION enforce_source_retention();
CREATE TRIGGER source_retention_metadata_write BEFORE INSERT OR UPDATE ON sources
    FOR EACH ROW EXECUTE FUNCTION enforce_source_retention();
CREATE TRIGGER source_retention_binding_write BEFORE INSERT OR UPDATE ON collection_sources
    FOR EACH ROW EXECUTE FUNCTION enforce_source_retention();
