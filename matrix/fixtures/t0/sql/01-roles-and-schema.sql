-- Compose bootstrap: the Matrix owner role and its schema, then the fixture
-- `crm` database that stands in for a customer system of record.
--
-- Two things here are deliberately awkward, because they are the point:
--
--   * `matrix_owner` owns schema `matrix` and has NO privileges in `public`.
--     The store's isolation test asserts that a write to `public` fails, so
--     this grant list is a tested contract rather than a convention.
--   * `crm` carries a compliant reader AND a non-compliant one. Every
--     introspect refusal in the suite runs against a real role with real
--     privileges instead of a mock that always agrees.

CREATE ROLE matrix_owner LOGIN PASSWORD 'matrix-owner-dev';

CREATE SCHEMA IF NOT EXISTS matrix AUTHORIZATION matrix_owner;

-- Explicitly take away the default `public` rights. Without this, PUBLIC can
-- create objects in `public` on PostgreSQL below 15, and the isolation test
-- would pass for the wrong reason.
REVOKE ALL ON SCHEMA public FROM PUBLIC;
REVOKE ALL ON SCHEMA public FROM matrix_owner;
GRANT USAGE ON SCHEMA public TO matrix_owner;
