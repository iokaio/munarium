-- SPDX-License-Identifier: Apache-2.0
-- Separate from ordinary memory writes and their capability-token permissions.
CREATE TABLE platform_authority (
    tenant_id TEXT PRIMARY KEY REFERENCES tenants(id),
    state JSONB NOT NULL,
    CHECK (state->'config'->>'tenant' = tenant_id),
    CHECK ((state->>'head')::bigint >= 0)
);
CREATE TABLE platform_authority_receipts (
    tenant_id TEXT NOT NULL REFERENCES platform_authority(tenant_id),
    nonce TEXT NOT NULL,
    head BIGINT NOT NULL CHECK (head > 0),
    receipt JSONB NOT NULL,
    artifact JSONB NOT NULL,
    PRIMARY KEY (tenant_id, nonce),
    UNIQUE (tenant_id, head),
    CHECK (receipt->>'tenant' = tenant_id),
    CHECK (receipt->>'nonce' = nonce),
    CHECK ((receipt->>'head')::bigint = head)
);
CREATE FUNCTION guard_platform_authority() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' OR TG_TABLE_NAME = 'platform_authority_receipts' THEN
        RAISE EXCEPTION 'platform authority history is immutable';
    END IF;
    IF NEW.state->'config' IS DISTINCT FROM OLD.state->'config'
       OR (NEW.state->>'head')::bigint <> (OLD.state->>'head')::bigint + 1
       OR ((OLD.state->>'bootstrap_retired')::boolean
           AND NOT (NEW.state->>'bootstrap_retired')::boolean) THEN
        RAISE EXCEPTION 'invalid platform authority transition';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM platform_authority_receipts r
                   WHERE r.tenant_id = NEW.tenant_id
                     AND r.head = (NEW.state->>'head')::bigint
                     AND r.receipt->>'revision' = NEW.state->>'revision'
                     AND r.receipt->>'prior_revision' = OLD.state->>'revision'
                     AND r.artifact = NEW.state->'artifact') THEN
        RAISE EXCEPTION 'platform authority transition requires its immutable receipt';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER platform_authority_guard BEFORE UPDATE OR DELETE ON platform_authority
FOR EACH ROW EXECUTE FUNCTION guard_platform_authority();
CREATE TRIGGER platform_authority_receipt_guard BEFORE UPDATE OR DELETE ON platform_authority_receipts
FOR EACH ROW EXECUTE FUNCTION guard_platform_authority();
