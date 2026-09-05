-- The MySQL fixture.
--
-- It mirrors the Postgres `crm` fixture where the ENGINES agree and differs
-- where they do not, because the differences are what this tier exists to
-- find:
--
--   * `amount` is DECIMAL(18,2) and holds `900000.50`, so the trailing zero
--     has to survive a second driver.
--   * `shapes.footprint` is GEOMETRY — a type canon@1 deliberately does not
--     model. The adapter must REFUSE a read of it naming the column rather
--     than guess a mapping.
--   * There is no row-level security, because MySQL has no policy engine.
--     The Postgres fixture's `matrix_reader` sees one region through RLS;
--     here the same isolation would be a per-class GRANT and a view, and the
--     posture check reports the absence rather than hiding it.
--   * GTID is off (the server's default), so a read has no engine position
--     and the adapter reports none.

CREATE DATABASE IF NOT EXISTS crm;
USE crm;

CREATE TABLE IF NOT EXISTS opportunities (
    id         BIGINT PRIMARY KEY,
    name       VARCHAR(200)   NOT NULL,
    stage      VARCHAR(40)    NOT NULL,
    amount     DECIMAL(18, 2) NOT NULL,
    region     VARCHAR(10)    NOT NULL,
    updated_at DATETIME       NOT NULL
);

TRUNCATE TABLE opportunities;
INSERT INTO opportunities (id, name, stage, amount, region, updated_at) VALUES
    (1, 'Acme renewal',      'Negotiation', 1500000.00, 'EMEA', '2026-06-29 23:30:00'),
    (2, 'Beta expansion',    'Proposal',     250000.50, 'EMEA', '2026-06-30 00:30:00'),
    (3, 'Gamma pilot',       'Closed Won',   100000.00, 'AMER', '2026-05-01 12:00:00'),
    (4, 'Eta consolidation', 'Discovery',    120000.00, 'EMEA', '2026-06-28 16:45:00'),
    -- The trailing-zero trap, as in the Postgres fixture.
    (5, 'Theta upgrade',     'Negotiation',  900000.50, 'EMEA', '2026-06-27 10:00:00');

-- A column whose type canon@1 does not model. Its presence is the test: an
-- adapter that guessed would put a value into sealed evidence under a type
-- nothing downstream can verify.
CREATE TABLE IF NOT EXISTS shapes (
    id        BIGINT PRIMARY KEY,
    footprint GEOMETRY NOT NULL
);
TRUNCATE TABLE shapes;
INSERT INTO shapes (id, footprint) VALUES (1, ST_GeomFromText('POINT(1 1)'));

GRANT SELECT ON crm.* TO 'matrix'@'%';
FLUSH PRIVILEGES;
