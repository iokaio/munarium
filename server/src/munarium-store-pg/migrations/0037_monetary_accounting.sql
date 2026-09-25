-- SPDX-License-Identifier: Apache-2.0
-- Additive, opt-in tariff catalog. History is append-only; no legacy usage or
-- prices can be reconstructed by this migration.
CREATE TABLE monetary_prices (
    tenant_id TEXT NOT NULL,
    id TEXT NOT NULL,
    snapshot JSONB NOT NULL,
    PRIMARY KEY (tenant_id, id)
);
CREATE TABLE monetary_attempts (
    tenant_id TEXT NOT NULL,
    id TEXT NOT NULL,
    invocation_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    route TEXT NOT NULL,
    model TEXT NOT NULL,
    submitted_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    accounted_units NUMERIC(20,0) NOT NULL CHECK (accounted_units >= 0),
    price_id TEXT,
    PRIMARY KEY (tenant_id, id),
    FOREIGN KEY (tenant_id, price_id) REFERENCES monetary_prices(tenant_id, id)
);
CREATE INDEX monetary_attempts_window ON monetary_attempts(tenant_id, submitted_at, id);
CREATE TABLE monetary_observations (
    tenant_id TEXT NOT NULL,
    id TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    revision BIGINT NOT NULL CHECK (revision > 0),
    observed_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    observation JSONB NOT NULL,
    calculation JSONB NOT NULL,
    PRIMARY KEY (tenant_id, id),
    UNIQUE (tenant_id, attempt_id, revision),
    FOREIGN KEY (tenant_id, attempt_id) REFERENCES monetary_attempts(tenant_id, id)
);
-- Protect immutable facts from accidental updates/deletes, including cascades.
CREATE FUNCTION monetary_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'monetary accounting records are append-only';
END;
$$;
CREATE TRIGGER monetary_prices_immutable BEFORE UPDATE OR DELETE ON monetary_prices
    FOR EACH ROW EXECUTE FUNCTION monetary_immutable();
CREATE TRIGGER monetary_attempts_immutable BEFORE UPDATE OR DELETE ON monetary_attempts
    FOR EACH ROW EXECUTE FUNCTION monetary_immutable();
CREATE TRIGGER monetary_observations_immutable BEFORE UPDATE OR DELETE ON monetary_observations
    FOR EACH ROW EXECUTE FUNCTION monetary_immutable();
