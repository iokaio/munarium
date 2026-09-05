-- The asset registry. Everything Matrix knows how to do is a row here.
--
-- Two disciplines, both load-bearing:
--
--   * Rows are VERSIONED and append-only by convention: `(tenant, kind, name,
--     version)` is the key, and a re-apply of the same version with different
--     bytes is refused by the code (not by a constraint, so the error can say
--     "bump the version" instead of "duplicate key"). Old versions stay
--     resolvable forever because they are provenance: a sealed artifact names
--     the contract version that produced it, and that name must still resolve
--     in five years.
--   * `yaml` is the applied bytes, verbatim. The parsed form is derived and
--     never stored, so a parser improvement cannot retroactively change what
--     an operator applied.
--
-- Everything lives in schema `matrix`, owned by role `matrix_owner`, which has
-- no privileges in `public`. The isolation test asserts that.

-- The schema itself is NOT created here. `MatrixStore::migrate` ensures it
-- first, because sqlx writes `_sqlx_migrations` before applying migration 1 and
-- that table has to land somewhere `matrix_owner` may write.
--
-- It cannot be created here in any case: `CREATE SCHEMA IF NOT EXISTS`
-- evaluates the CREATE privilege on the DATABASE before it short-circuits, so a
-- least-privilege `matrix_owner` that already OWNS this schema is refused for a
-- statement that would have done nothing. `migrate` checks `pg_namespace`
-- first and only creates when genuinely absent.

CREATE TABLE IF NOT EXISTS matrix.assets (
    tenant_id   TEXT        NOT NULL,
    kind        TEXT        NOT NULL,          -- DataSource | QueryContract | ClaimMapping
    name        TEXT        NOT NULL,
    version     INTEGER     NOT NULL,
    yaml        TEXT        NOT NULL,
    yaml_hash   TEXT        NOT NULL,          -- sha256 of the applied bytes
    -- Denormalized for listing and for the source -> contract/mapping join,
    -- which is otherwise a YAML parse per row.
    source_name TEXT,
    status      TEXT        NOT NULL DEFAULT 'active',   -- active | superseded
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, kind, name, version)
);

CREATE INDEX IF NOT EXISTS idx_assets_kind_name
    ON matrix.assets (tenant_id, kind, name, version DESC);
CREATE INDEX IF NOT EXISTS idx_assets_source
    ON matrix.assets (tenant_id, source_name);

-- Values a parameter is allowed to take, pinned at introspect time rather than
-- re-read per request. Re-reading would make the allowed set a moving target
-- and turn "not_covered" into a race.
CREATE TABLE IF NOT EXISTS matrix.parameter_domains (
    tenant_id     TEXT        NOT NULL,
    contract_name TEXT        NOT NULL,
    contract_version INTEGER  NOT NULL,
    parameter     TEXT        NOT NULL,
    values_json   JSONB       NOT NULL,
    pinned_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, contract_name, contract_version, parameter)
);

-- The source schema fingerprint a checkpoint was taken under. Drift is
-- detected by comparing against this, and drift is fail-closed.
CREATE TABLE IF NOT EXISTS matrix.schema_fingerprints (
    tenant_id   TEXT        NOT NULL,
    source_name TEXT        NOT NULL,
    entity      TEXT        NOT NULL,
    fingerprint TEXT        NOT NULL,
    columns_json JSONB      NOT NULL,
    observed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, source_name, entity)
);
