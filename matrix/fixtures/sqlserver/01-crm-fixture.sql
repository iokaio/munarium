-- The SQL Server fixture.
--
-- It mirrors the MySQL `crm` fixture where the engines agree and differs where
-- they do not, because the differences are what this tier exists to find:
--
--   * `amount` is DECIMAL(18,2) and holds `900000.50`, so the trailing zero
--     has to survive a third driver (TDS, decoded through tiberius).
--   * `shapes.footprint` is GEOGRAPHY and `shapes.list_price` is MONEY. Both
--     are types canon@1 deliberately does not model, for different reasons:
--     GEOGRAPHY has no logical shape in the closed set, and MONEY is an
--     exact 4-decimal currency type that this DRIVER decodes as an IEEE-754
--     double (see `money.rs` in tiberius: `read_i32_le() as f64 / 1e4`).
--     Silently accepting either would put a value in sealed evidence under a
--     type nothing downstream can verify — a currency as a float being the
--     precise failure canon@1 exists to prevent.
--   * SQL Server DOES have row-level security, unlike MySQL. A filter
--     predicate restricts `matrix_reader` to EMEA, so this tier measures the
--     same restricted view the Postgres fixture does, and `introspect`
--     reports row security as PRESENT here and ABSENT there — which is what
--     makes "reported, never omitted" mean something.
--   * Snapshot isolation and change tracking are both ON, so the adapter's
--     snapshot marker has something to report. Both are database settings a
--     customer may not have; the adapter reports `None` when they are off.

IF DB_ID('crm') IS NULL
    EXEC('CREATE DATABASE crm');
GO

-- Snapshot isolation is a DATABASE setting, not a session one: without it the
-- session can ask for SNAPSHOT and the first read fails with 3952. The adapter
-- therefore asks the catalog whether it is allowed rather than trying and
-- recovering.
ALTER DATABASE crm SET ALLOW_SNAPSHOT_ISOLATION ON;
GO

-- Change tracking gives the database a monotonic version. The adapter reports
-- it as a snapshot marker ONLY from inside a snapshot-isolated transaction,
-- where the version and the rows come from one consistent view; outside one it
-- would be a position that raced the read.
ALTER DATABASE crm SET CHANGE_TRACKING = ON (CHANGE_RETENTION = 2 DAYS, AUTO_CLEANUP = ON);
GO

USE crm;
GO

-- `CREATE SCHEMA` must be the first statement in its batch, so it is wrapped
-- rather than guarded in place. The whole file is written to be re-runnable:
-- a fixture that only works against a virgin container is a fixture nobody can
-- reload after breaking something.
IF SCHEMA_ID('sec') IS NULL EXEC('CREATE SCHEMA sec AUTHORIZATION dbo');
GO
IF OBJECT_ID('sec.region_filter', 'SP') IS NOT NULL DROP SECURITY POLICY sec.region_filter;
GO
IF OBJECT_ID('sec.fn_region_filter', 'IF') IS NOT NULL DROP FUNCTION sec.fn_region_filter;
GO
IF OBJECT_ID('dbo.opportunities', 'U') IS NOT NULL DROP TABLE dbo.opportunities;
GO
IF OBJECT_ID('dbo.shapes', 'U') IS NOT NULL DROP TABLE dbo.shapes;
GO

CREATE TABLE dbo.opportunities (
    id         BIGINT        NOT NULL PRIMARY KEY,
    name       NVARCHAR(200) NOT NULL,
    stage      NVARCHAR(40)  NOT NULL,
    amount     DECIMAL(18,2) NOT NULL,
    region     NVARCHAR(10)  NOT NULL,
    updated_at DATETIME2     NOT NULL
);
GO

INSERT INTO dbo.opportunities (id, name, stage, amount, region, updated_at) VALUES
    (1, N'Acme renewal',      N'Negotiation', 1500000.00, N'EMEA', '2026-06-29T23:30:00'),
    (2, N'Beta expansion',    N'Proposal',     250000.50, N'EMEA', '2026-06-30T00:30:00'),
    (3, N'Gamma pilot',       N'Closed Won',   100000.00, N'AMER', '2026-05-01T12:00:00'),
    (4, N'Eta consolidation', N'Discovery',    120000.00, N'EMEA', '2026-06-28T16:45:00'),
    -- The trailing-zero trap, as in the Postgres and MySQL fixtures.
    (5, N'Theta upgrade',     N'Negotiation',  900000.50, N'EMEA', '2026-06-27T10:00:00');
GO

ALTER TABLE dbo.opportunities ENABLE CHANGE_TRACKING;
GO

-- Two columns whose types canon@1 does not model. Their presence is the test.
CREATE TABLE dbo.shapes (
    id         BIGINT     NOT NULL PRIMARY KEY,
    footprint  GEOGRAPHY  NOT NULL,
    list_price MONEY      NOT NULL
);
GO
INSERT INTO dbo.shapes (id, footprint, list_price)
VALUES (1, geography::Point(47.6, -122.3, 4326), 1234.5678);
GO

-- --------------------------------------------------------------------------
-- Row-level security. SQL Server has a real policy engine, so the fixture uses
-- it: `matrix_reader` sees EMEA only. `dbo` (which the SA login maps to) is a
-- member of db_owner and sees everything, so the fixture can still be loaded
-- and inspected.
-- --------------------------------------------------------------------------
CREATE FUNCTION sec.fn_region_filter(@region AS NVARCHAR(10))
    RETURNS TABLE
    WITH SCHEMABINDING
AS
    RETURN SELECT 1 AS is_visible
     WHERE @region = N'EMEA' OR IS_MEMBER('db_owner') = 1;
GO

CREATE SECURITY POLICY sec.region_filter
    ADD FILTER PREDICATE sec.fn_region_filter(region) ON dbo.opportunities
    WITH (STATE = ON);
GO

-- --------------------------------------------------------------------------
-- The read-only principal.
-- --------------------------------------------------------------------------
USE master;
GO
IF SUSER_ID('matrix_reader') IS NULL
    CREATE LOGIN matrix_reader WITH PASSWORD = 'Matrix-Reader-Dev1!', CHECK_POLICY = OFF;
GO
USE crm;
GO
IF USER_ID('matrix_reader') IS NULL
    CREATE USER matrix_reader FOR LOGIN matrix_reader;
GO
ALTER ROLE db_datareader ADD MEMBER matrix_reader;
GO

-- Metadata, not data. On Postgres `pg_catalog` is world-readable, so a reader
-- can prove its own posture for free; on SQL Server catalog views are filtered
-- by metadata visibility, so a reader with SELECT on a table still cannot see
-- that a SECURITY POLICY constrains it. Without this grant `introspect` would
-- report "no row security" for a table that has it — an absence of evidence
-- read as evidence of absence, on exactly the check that matters most.
GRANT VIEW DEFINITION TO matrix_reader;
GO
GRANT VIEW CHANGE TRACKING ON dbo.opportunities TO matrix_reader;
GO
