-- 0004 (Phase 5): authoritative reconciliation is OPT-IN per mapping version
-- and recorded as a decision, and every claim Matrix proposes is remembered
-- here so a re-run can tell "already proposed" from "new" without asking the
-- ledger to remember on our behalf.
--
-- Additive, like every migration in this schema.

-- One row per promotion. A promotion is for ONE mapping version: re-applying
-- the mapping at a new version demotes implicitly, because the scopes the
-- operator approved may have changed. `demoted_at` closes the row; the row is
-- never deleted, so the history of who promoted what, and when, survives.
CREATE TABLE IF NOT EXISTS matrix.mapping_promotions (
    tenant_id           TEXT             NOT NULL,
    mapping_name        TEXT             NOT NULL,
    mapping_version     INTEGER          NOT NULL,
    decision_id         TEXT             NOT NULL,
    actor               TEXT             NOT NULL,
    reason              TEXT,
    -- The gate values at promotion time, so a later reader can see what the
    -- operator saw, not what the numbers are now.
    identity_precision  DOUBLE PRECISION NOT NULL,
    value_conformance   DOUBLE PRECISION NOT NULL,
    promoted_at         TIMESTAMPTZ      NOT NULL DEFAULT now(),
    demoted_at          TIMESTAMPTZ,
    demote_decision_id  TEXT,
    PRIMARY KEY (tenant_id, mapping_name, promoted_at)
);

-- At most ONE active promotion per mapping.
CREATE UNIQUE INDEX IF NOT EXISTS uq_mapping_promotions_active
    ON matrix.mapping_promotions (tenant_id, mapping_name)
    WHERE demoted_at IS NULL;

-- Every claim proposed, keyed by the content identity Matrix computed. The
-- idempotency key is (mapping version, row key, property, canonical value,
-- source position); a replayed run finds its key here and sends nothing.
CREATE TABLE IF NOT EXISTS matrix.claim_proposals (
    tenant_id        TEXT        NOT NULL,
    idempotency_key  TEXT        NOT NULL,
    mapping_ref      TEXT        NOT NULL,
    version_id       TEXT        NOT NULL,
    subject          TEXT        NOT NULL,
    property         TEXT        NOT NULL,
    value            TEXT        NOT NULL,
    claim_type       TEXT        NOT NULL,
    supersedes_id    TEXT,
    -- What the ledger held before: what a rollback restores. NULL when the
    -- proposal filled a gap, in which case there is nothing to restore.
    prior_value      TEXT,
    claim_id         TEXT        NOT NULL,
    status           TEXT        NOT NULL,
    row_key          TEXT        NOT NULL,
    evidence_id      TEXT,
    rolled_back_by   TEXT,
    proposed_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, idempotency_key)
);

CREATE INDEX IF NOT EXISTS idx_claim_proposals_mapping
    ON matrix.claim_proposals (tenant_id, mapping_ref, proposed_at);

-- The two promotion-gate counters a run reports. Additive columns on the
-- existing run row.
ALTER TABLE matrix.mapping_runs ADD COLUMN IF NOT EXISTS proposals     BIGINT NOT NULL DEFAULT 0;
ALTER TABLE matrix.mapping_runs ADD COLUMN IF NOT EXISTS nonconforming BIGINT NOT NULL DEFAULT 0;
