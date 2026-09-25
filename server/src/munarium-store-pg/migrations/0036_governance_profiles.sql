-- SPDX-License-Identifier: Apache-2.0
-- NULL on historical rows denotes the implicit legacy-text-v1 profile.
ALTER TABLE claims ADD COLUMN governance_revision TEXT;
ALTER TABLE anchors ADD COLUMN governance_revision TEXT;

CREATE FUNCTION guard_governance_version() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE parent_meta JSONB;
BEGIN
    IF TG_OP = 'UPDATE' THEN
        IF NEW.metadata->'governance_policy' IS DISTINCT FROM OLD.metadata->'governance_policy'
           OR NEW.metadata->'governance_revision' IS DISTINCT FROM OLD.metadata->'governance_revision'
           OR NEW.metadata->'governance_transition' IS DISTINCT FROM OLD.metadata->'governance_transition'
           OR NEW.metadata->'governance_assessment' IS DISTINCT FROM OLD.metadata->'governance_assessment'
           OR NEW.parent_id IS DISTINCT FROM OLD.parent_id THEN
            RAISE EXCEPTION 'governance history is immutable; create a child version';
        END IF;
        RETURN NEW;
    END IF;
    IF NEW.parent_id IS NOT NULL THEN
        SELECT metadata INTO parent_meta FROM memory_versions WHERE tenant_id = NEW.tenant_id AND id = NEW.parent_id;
    END IF;
    IF NEW.metadata ? 'governance_policy' OR parent_meta ? 'governance_policy' THEN
        IF current_setting('munarium.governance_writer', true) IS DISTINCT FROM '1'
           OR NOT COALESCE(NEW.metadata ? 'governance_policy', false)
           OR NOT COALESCE(NEW.metadata ? 'governance_revision', false) THEN
            RAISE EXCEPTION 'governed versions require a governance-aware writer';
        END IF;
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER governance_version_guard BEFORE INSERT OR UPDATE ON memory_versions
FOR EACH ROW EXECUTE FUNCTION guard_governance_version();

CREATE FUNCTION stamp_governance_record() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE meta JSONB;
BEGIN
    SELECT metadata INTO meta FROM memory_versions WHERE tenant_id = NEW.tenant_id AND id = NEW.version_id;
    IF meta ? 'governance_policy' THEN
        IF current_setting('munarium.governance_writer', true) IS DISTINCT FROM '1' THEN
            RAISE EXCEPTION 'governed records require a governance-aware writer';
        END IF;
        NEW.governance_revision := meta->>'governance_revision';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER governance_claim_stamp BEFORE INSERT ON claims
FOR EACH ROW EXECUTE FUNCTION stamp_governance_record();
CREATE TRIGGER governance_anchor_stamp BEFORE INSERT ON anchors
FOR EACH ROW EXECUTE FUNCTION stamp_governance_record();
