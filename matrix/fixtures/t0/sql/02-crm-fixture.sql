-- The T0 fixture: a small operational database with every trap the adversarial
-- suite exists to falsify, planted on purpose and documented where it sits.
--
-- Read this file as the answer key to the adversarial suite. Each trap has a
-- comment saying WHAT it breaks if the implementation is naive.
--
-- The CALLER chooses the database. On compose the init runs against `matrix`;
-- on a deployment the fixture may get its own `crm` database. An earlier
-- `\connect matrix` here silently put the fixture in the wrong database on a
-- deployment — found on the first live run.

CREATE SCHEMA IF NOT EXISTS crm;

-- ---------------------------------------------------------------------------
-- opportunities: the mode-B contract's table
-- ---------------------------------------------------------------------------
CREATE TABLE crm.opportunities (
    id          BIGINT PRIMARY KEY,
    name        TEXT NOT NULL,
    stage       TEXT NOT NULL,
    -- numeric(18,2), not float: a currency column that goes through a double
    -- is wrong in the last cent, and the whole exercise is that the last cent
    -- is right.
    amount      NUMERIC(18,2) NOT NULL,
    region      TEXT NOT NULL,
    owner_id    BIGINT,
    -- Trap: a column the policy denies. It must never appear in a compiled
    -- statement, a log line, a journal row or a sealed manifest.
    owner_email TEXT,
    -- Trap: NULL vs the empty string. Two rows below differ only in this, and
    -- a reconciliation that conflates them files a false discrepancy.
    notes       TEXT,
    -- Trap: timezone boundary. Two rows sit either side of midnight UTC when
    -- read in a non-UTC zone, so a date filter that ignores offsets moves them.
    updated_at  TIMESTAMPTZ NOT NULL,
    closed_at   DATE
);

INSERT INTO crm.opportunities
    (id, name, stage, amount, region, owner_id, owner_email, notes, updated_at, closed_at) VALUES
    (1, 'Acme renewal',        'Negotiation', 1500000.00, 'EMEA', 10, 'a@example.com', NULL,  '2026-06-29T23:30:00Z', NULL),
    (2, 'Beta expansion',      'Proposal',     250000.50, 'EMEA', 11, 'b@example.com', '',    '2026-06-30T00:30:00Z', NULL),
    (3, 'Gamma pilot',         'Closed Won',   100000.00, 'AMER', 12, 'c@example.com', 'won', '2026-05-01T12:00:00Z', '2026-05-01'),
    (4, 'Delta refresh',       'Discovery',    750000.25, 'AMER', 12, 'c@example.com', NULL,  '2026-06-15T09:00:00Z', NULL),
    (5, 'Epsilon trial',       'Closed Lost',   50000.00, 'APAC', 13, 'd@example.com', NULL,  '2026-04-02T08:00:00Z', '2026-04-02'),
    (6, 'Zeta migration',      'Proposal',     430000.75, 'APAC', 13, 'd@example.com', NULL,  '2026-06-28T16:45:00Z', NULL),
    -- Trap: two rows share `updated_at` to the microsecond. An exclusive
    -- watermark WITHOUT a tie-break drops one of them forever.
    (7, 'Eta consolidation',   'Discovery',    120000.00, 'EMEA', 11, 'b@example.com', NULL,  '2026-06-28T16:45:00Z', NULL),
    -- Trap: a scale that a naive reader renders as 900000.5 rather than
    -- 900000.50, changing the logical result hash.
    (8, 'Theta upgrade',       'Negotiation',  900000.50, 'EMEA', 10, 'a@example.com', NULL,  '2026-06-27T10:00:00Z', NULL);

-- ---------------------------------------------------------------------------
-- Row-level security: the same table serves two authorization classes.
-- ---------------------------------------------------------------------------
ALTER TABLE crm.opportunities ENABLE ROW LEVEL SECURITY;

CREATE ROLE matrix_reader LOGIN PASSWORD 'matrix-reader-dev';
GRANT USAGE ON SCHEMA crm TO matrix_reader;
-- Column-level: the reader is not granted `owner_email` at all, so a
-- projection that reaches for it fails at the source rather than being
-- filtered afterwards. There is no seal-time masking.
GRANT SELECT (id, name, stage, amount, region, owner_id, notes, updated_at, closed_at)
    ON crm.opportunities TO matrix_reader;

CREATE POLICY emea_only ON crm.opportunities
    FOR SELECT TO matrix_reader
    USING (region = 'EMEA');

-- Trap: a role that looks fine until introspect asks. It is a table OWNER and
-- holds DML, so the posture check must refuse it — and the live suite runs
-- this against a real managed Postgres every cycle.
CREATE ROLE matrix_bad_reader LOGIN PASSWORD 'matrix-bad-reader-dev';
GRANT USAGE ON SCHEMA crm TO matrix_bad_reader;
GRANT SELECT, INSERT, UPDATE, DELETE ON crm.opportunities TO matrix_bad_reader;

-- Reassigning ownership needs the CURRENT role to be a member of the new
-- owner. On a laptop the bootstrap role is a superuser and this is a no-op;
-- on Azure Database for PostgreSQL the admin login is NOT a superuser, and
-- without this grant the ALTER fails with "permission denied for schema crm".
-- Found on the first live cycle (2026-08-28) — exactly the class of thing the
-- live tier exists to catch, since compose is happy either way.
GRANT matrix_bad_reader TO CURRENT_USER;
-- ...and the NEW owner needs CREATE on the schema that holds the table.
-- Postgres checks this even when the object already exists, which is easy to
-- miss because a superuser bypasses the whole question.
GRANT CREATE ON SCHEMA crm TO matrix_bad_reader;
ALTER TABLE crm.opportunities OWNER TO matrix_bad_reader;
-- Ownership implies BYPASSRLS for the owner's own table, which is exactly the
-- silent policy bypass the check exists to catch. Ownership also moves the
-- default privileges, so the reader's grants are re-issued below.
GRANT USAGE ON SCHEMA crm TO matrix_reader;
GRANT SELECT (id, name, stage, amount, region, owner_id, notes, updated_at, closed_at)
    ON crm.opportunities TO matrix_reader;

-- ---------------------------------------------------------------------------
-- holdings + companies: the mode-C mapping's tables
-- ---------------------------------------------------------------------------
CREATE TABLE crm.companies (
    id   BIGINT PRIMARY KEY,
    name TEXT NOT NULL
);
INSERT INTO crm.companies (id, name) VALUES
    (7, 'Northgate Industries'),
    (8, 'Copperline Holdings');

CREATE TABLE crm.holdings (
    holder_id      BIGINT NOT NULL,
    company_id     BIGINT NOT NULL REFERENCES crm.companies(id),
    holder_name    TEXT NOT NULL,
    -- numeric(38,0): share counts are integers that overflow a double.
    shares         NUMERIC(38,0) NOT NULL,
    share_class    TEXT NOT NULL,
    effective_date DATE NOT NULL,
    recorded_at    TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (holder_id, company_id)
);

INSERT INTO crm.holdings
    (holder_id, company_id, holder_name, shares, share_class, effective_date, recorded_at) VALUES
    (42, 7, 'Jane Rowntree',     125000, 'A', '2026-04-01', '2026-08-28T09:15:00Z'),
    -- Trap: the document corpus says 90000 for this holder. A planted
    -- disagreement between a signed document and the register — mode C must
    -- surface it with BOTH citations and never silently pick a side.
    (43, 7, 'Marcus Vane',        90500, 'A', '2026-04-01', '2026-08-28T09:15:00Z'),
    -- Trap: two holders whose names normalize to the same alias. Identity
    -- resolution must file `identity_ambiguous` and merge NOTHING.
    (51, 8, 'J. Rowntree',        40000, 'B', '2026-01-01', '2026-08-28T09:15:00Z'),
    (58, 8, 'Jane  Rowntree',     40000, 'B', '2026-01-01', '2026-08-28T09:15:00Z'),
    -- Trap: a BACKDATED legitimate update. `effective_date` is in the past
    -- relative to rows already recorded, so it is a new fact about an old
    -- period — not a correction of a wrong value. It must file
    -- `requires_review`, never an automatic correction.
    (44, 7, 'Priya Anand',        15000, 'A', '2025-11-15', '2026-08-28T09:20:00Z');

-- ---------------------------------------------------------------------------
-- Trap: a many-to-many that inflates a naive SUM.
--
-- Joining opportunities to their tags and summing `amount` double-counts every
-- opportunity with two tags. A contract that declares a grain and refuses an
-- unsafe join is the defense; a compiler that happily emits the join is the
-- bug this table exists to catch.
-- ---------------------------------------------------------------------------
CREATE TABLE crm.opportunity_tags (
    opportunity_id BIGINT NOT NULL REFERENCES crm.opportunities(id),
    tag            TEXT NOT NULL,
    PRIMARY KEY (opportunity_id, tag)
);
INSERT INTO crm.opportunity_tags (opportunity_id, tag) VALUES
    (1, 'strategic'), (1, 'renewal'),
    (2, 'expansion'),
    (8, 'strategic'), (8, 'upgrade');

GRANT USAGE ON SCHEMA crm TO matrix_reader;
GRANT SELECT ON crm.companies, crm.holdings, crm.opportunity_tags TO matrix_reader;
