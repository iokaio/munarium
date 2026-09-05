-- 0023 (2026-08-28, S-2.1): the sealed evidence plane.
--
-- An evidence artifact is the exact typed result an answer was computed from.
-- Three tables, each answering one question:
--
--   evidence_artifacts  what was sealed, who may read it, and until when
--   evidence_grants     a single-use capability to upload bytes for an id
--   evidence_access     who resolved what, and how it went
--
-- Two things are deliberately NOT here. The artifact BYTES live in the object
-- store under the reserved `evidence/<id>` keyspace, because they can be
-- hundreds of megabytes and the metadata is what needs indexing. And the ROW
-- CONTENTS never appear in evidence_access: an audit table holding the
-- regulated data it audits is a second copy of the problem.
--
-- Additive: no existing table is touched, so a deployment that never seals
-- evidence carries three empty tables and nothing else changes.

CREATE TABLE IF NOT EXISTS evidence_artifacts (
    tenant_id           TEXT        NOT NULL,
    evidence_id         TEXT        NOT NULL,
    -- pending -> committed -> purged. Only `committed` resolves; `pending`
    -- means a grant was issued and the bytes never arrived, which is not
    -- evidence of anything yet.
    state               TEXT        NOT NULL,
    -- The full manifest as the contract defines it. Load-bearing fields are
    -- ALSO lifted into columns below: the JSON is the record, the columns are
    -- what authorization and retention are decided on, and a decision must
    -- never depend on a JSON path lookup that a malformed document could bend.
    manifest            JSONB       NOT NULL,
    -- The domain idempotency tuple, hashed:
    -- (tenant, logical_result_hash, policy_version, authorization_class).
    -- Note the absence of artifact_hash — re-serializing one logical result
    -- must not mint a second artifact.
    domain_key          TEXT        NOT NULL,
    logical_result_hash TEXT        NOT NULL,
    artifact_hash       TEXT        NOT NULL,
    bytes_len           BIGINT      NOT NULL,
    media_type          TEXT        NOT NULL,
    kind                TEXT        NOT NULL,
    -- Authorization equivalence class. A resolving session must dominate it:
    -- level >= access_level AND holds every compartment.
    access_level        INT         NOT NULL,
    compartments        TEXT[]      NOT NULL DEFAULT '{}',
    -- Retention. `purged_at` set means the bytes are gone but the row stays,
    -- so a citation resolves `evidence-expired` and never `not-found`.
    expires_at          TIMESTAMPTZ,
    legal_hold          BOOLEAN     NOT NULL DEFAULT FALSE,
    purged_at           TIMESTAMPTZ,
    blob_path           TEXT        NOT NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    committed_at        TIMESTAMPTZ,
    PRIMARY KEY (tenant_id, evidence_id)
);

-- The idempotency index. UNIQUE, so a concurrent double-seal of the same
-- logical result under the same class cannot produce two artifacts: the
-- second INSERT loses and the caller is handed the first one's id.
CREATE UNIQUE INDEX IF NOT EXISTS evidence_artifacts_domain_key
    ON evidence_artifacts (tenant_id, domain_key);

-- The retention janitor's scan (S-2.1 package 2). Partial, because the only
-- rows it ever wants are committed, unexpired-by-hold and not yet purged.
CREATE INDEX IF NOT EXISTS evidence_artifacts_due
    ON evidence_artifacts (tenant_id, expires_at)
    WHERE state = 'committed' AND purged_at IS NULL AND legal_hold = FALSE;

CREATE TABLE IF NOT EXISTS evidence_grants (
    tenant_id   TEXT        NOT NULL,
    grant_id    TEXT        NOT NULL,
    evidence_id TEXT        NOT NULL,
    expires_at  TIMESTAMPTZ NOT NULL,
    -- Single-use: set on the first spend. The second attempt is refused even
    -- inside the TTL, which is what makes a leaked grant a bounded loss.
    used_at     TIMESTAMPTZ,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, grant_id)
);

CREATE INDEX IF NOT EXISTS evidence_grants_by_evidence
    ON evidence_grants (tenant_id, evidence_id);

CREATE TABLE IF NOT EXISTS evidence_access (
    tenant_id   TEXT        NOT NULL,
    evidence_id TEXT        NOT NULL,
    uid         TEXT        NOT NULL,
    -- manifest | rows
    kind        TEXT        NOT NULL,
    row_from    BIGINT,
    row_limit   BIGINT,
    -- ok | denied | expired | on-hold
    outcome     TEXT        NOT NULL,
    at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS evidence_access_by_evidence
    ON evidence_access (tenant_id, evidence_id, at DESC);
