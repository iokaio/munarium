-- SPDX-License-Identifier: Apache-2.0
-- Late corrections preserve the reservation's original day and evidence.
ALTER TABLE token_budget_reservations
    ADD COLUMN evidence_revision BIGINT NOT NULL DEFAULT 0 CHECK (evidence_revision >= 0),
    ADD COLUMN estimator_revision TEXT;
CREATE UNIQUE INDEX token_budget_reservations_tenant_id
    ON token_budget_reservations (tenant_id, id);
CREATE TABLE token_budget_adjustments (
    tenant_id TEXT NOT NULL,
    reservation_id TEXT NOT NULL,
    id TEXT NOT NULL,
    revision BIGINT NOT NULL CHECK (revision > 0),
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    adjustment JSONB NOT NULL,
    PRIMARY KEY (tenant_id, reservation_id, id),
    UNIQUE (tenant_id, reservation_id, revision),
    FOREIGN KEY (tenant_id, reservation_id)
        REFERENCES token_budget_reservations (tenant_id, id)
);
CREATE FUNCTION token_budget_adjustment_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'token budget adjustments are append-only';
END;
$$;
CREATE TRIGGER token_budget_adjustments_immutable BEFORE UPDATE OR DELETE ON token_budget_adjustments
    FOR EACH ROW EXECUTE FUNCTION token_budget_adjustment_immutable();
