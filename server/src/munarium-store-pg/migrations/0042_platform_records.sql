-- SPDX-License-Identifier: Apache-2.0
-- Reserved ledger custody is independent of ordinary tenant data-plane handles.
CREATE TABLE platform_record_ledgers (
    tenant_id TEXT PRIMARY KEY REFERENCES platform_authority(tenant_id),
    storage_tenant TEXT NOT NULL UNIQUE REFERENCES tenants(id),
    version_id TEXT NOT NULL
);
CREATE FUNCTION platform_record_ledger_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'platform record ledger binding is immutable';
END;
$$;
CREATE TRIGGER platform_record_ledger_no_change BEFORE UPDATE OR DELETE
ON platform_record_ledgers FOR EACH ROW EXECUTE FUNCTION platform_record_ledger_immutable();
