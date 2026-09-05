-- The logical-replication CDC fixture.
--
-- It is a SEPARATE table rather than a second posture on `crm.opportunities`,
-- because CDC needs schema properties that table cannot have without changing
-- its fingerprint and its nullability for every other scenario in the suite.
--
-- Everything here exists to make one measured fact testable: logical decoding
-- reads WAL, and WAL is written BEFORE any policy is consulted. Measured on a
-- real PostgreSQL 16 on 2026-08-30, a role restricted to EMEA by a row policy
-- and denied the `secret` column entirely saw, through a `test_decoding` slot,
-- the AMER row, the APAC row, and `secret[text]:'topsecret'`.
--
-- `pgoutput` is the only plugin that closes that hole, because it applies the
-- PUBLICATION's column list and row filter while decoding. Getting all three
-- of (a) a row filter on a non-key column, (b) a column list that withholds a
-- column and (c) working UPDATEs and DELETEs needs a replica identity that
-- covers the filter's columns — Postgres refuses the other combinations
-- outright:
--
--   "Column list used by the publication does not cover the replica identity"
--   "Column used in the publication WHERE expression is not part of the
--    replica identity"
--
-- Hence the unique index over (id, region) below. Both messages were provoked,
-- not read about.

CREATE TABLE crm.cdc_opportunities (
    id         BIGINT PRIMARY KEY,
    name       TEXT           NOT NULL,
    amount     NUMERIC(18, 2),
    -- NOT NULL because a replica-identity index may not contain a nullable
    -- column, and the publication's WHERE names this one.
    region     TEXT           NOT NULL,
    -- The column the reader is never granted. Its whole job is to be absent
    -- from the change stream; if it appears, the publication's column list is
    -- not doing what this fixture claims it does.
    secret     TEXT
);

INSERT INTO crm.cdc_opportunities (id, name, amount, region, secret) VALUES
    (1, 'Acme renewal',   1500000.00, 'EMEA', 'emea-secret'),
    -- The trailing-zero trap, here to prove it survives a THIRD transport:
    -- pgoutput proto v1 sends every value as text, so `900000.50` arrives with
    -- its zero and no float is constructed anywhere.
    (2, 'Theta upgrade',   900000.50, 'EMEA', 'emea-secret-2'),
    -- The row the policy hides. A change to it must produce NOTHING in the
    -- stream, which is the property that makes this path able to carry a
    -- policy at all.
    (3, 'Gamma pilot',     100000.00, 'AMER', 'amer-secret');

ALTER TABLE crm.cdc_opportunities ENABLE ROW LEVEL SECURITY;
CREATE POLICY cdc_emea_only ON crm.cdc_opportunities
    FOR SELECT TO matrix_reader
    USING (region = 'EMEA');

GRANT SELECT (id, name, amount, region) ON crm.cdc_opportunities TO matrix_reader;

-- The replica identity. It must cover every column the publication's WHERE
-- names, or the engine refuses UPDATE and DELETE on the table.
CREATE UNIQUE INDEX cdc_opportunities_replica_identity
    ON crm.cdc_opportunities (id, region);
ALTER TABLE crm.cdc_opportunities
    REPLICA IDENTITY USING INDEX cdc_opportunities_replica_identity;

-- The publication the `cdc` source reads. Its column list is what withholds
-- `secret` from the stream, and its WHERE is what expresses the row policy for
-- a channel the row policy itself does not reach. Matrix VERIFIES both and
-- creates neither: the equivalence between this WHERE and the RLS policy above
-- is an operator's assertion, because comparing two SQL expressions for
-- equivalence is undecidable.
CREATE PUBLICATION munarium_matrix_cdc
    FOR TABLE crm.cdc_opportunities (id, name, amount, region)
    WHERE (region = 'EMEA');

-- The deliberately WRONG one, so the refusal that catches it is reached by a
-- real object rather than by a mock. Same table, same column list, no WHERE:
-- a source reading this would stream the AMER row its own SELECT cannot see.
CREATE PUBLICATION munarium_matrix_cdcopen
    FOR TABLE crm.cdc_opportunities (id, name, amount, region);

-- REPLICATION is what lets a role read a slot. It is NOT superuser, does not
-- bypass row security and grants no DML — so the posture `introspect` proves
-- at connect time still holds, which is the whole reason this feature could be
-- built at all rather than refused.
ALTER ROLE matrix_reader REPLICATION;

-- NO replication slot is created here, on purpose. A slot makes the server
-- RETAIN WAL until something consumes it, so one nobody reads fills the disk
-- and stops the database — and it goes on doing that after Matrix is
-- uninstalled. The adapter refuses `cdc_slot_missing` with the exact statement
-- to run, and the conformance tier creates one itself so that refusal is a
-- tested path rather than a comment.
