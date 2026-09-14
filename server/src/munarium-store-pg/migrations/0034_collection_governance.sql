-- SPDX-License-Identifier: Apache-2.0
-- Authoring snapshots are append-only. Query clients cannot supply a snapshot.
CREATE TABLE collection_governance (
    tenant_id TEXT NOT NULL,
    collection_id TEXT NOT NULL,
    revision BIGINT NOT NULL CHECK (revision > 0),
    policy JSONB NOT NULL,
    actor_uid TEXT NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, collection_id, revision),
    FOREIGN KEY (tenant_id, collection_id) REFERENCES collections(tenant_id, id)
);
CREATE FUNCTION immutable_collection_governance() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'collection governance revisions are append-only';
END;
$$;
CREATE TRIGGER collection_governance_append_only
BEFORE UPDATE OR DELETE ON collection_governance
FOR EACH ROW EXECUTE FUNCTION immutable_collection_governance();
