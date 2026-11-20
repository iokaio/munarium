-- 0060 (2026-11-19, ClearView Phase 8: R10, ART1, ART2): durable artifact
-- damage, fleet blocks and verified repair clearance.
--
-- The specification is server/docs/design/artifact-damage.md. In short:
--
--   artifact_integrity_pins          one row per tenant-scoped artifact pin
--                                    with a finding: its integrity generation
--                                    and whether it is blocked. The release
--                                    fence reads and share-locks this row.
--   artifact_integrity_candidates    the first unconfirmed authoritative
--                                    mismatch per pinned object, at the pin's
--                                    current generation. Bounded by the pin's
--                                    component count.
--   artifact_integrity_incidents     each committed fleet block, with its two
--                                    confirming observations and, once
--                                    cleared, its two verifications, the
--                                    repair attempt and the workflow identity.
--   artifact_repair_attempts         a preauthorized repair workflow's
--                                    clearance authority, scoped to one
--                                    incident.
--   artifact_integrity_observations  the bounded observation history. The
--                                    decisions above never depend on it, so a
--                                    full history refuses new entries without
--                                    evicting any and still lets a pin be
--                                    blocked and cleared.
--
-- No row anywhere means unknown integrity, never a clean audit.
--
-- Additive: no existing table is touched. With MUNARIUM_ARTIFACT_DAMAGE_MODE
-- unset (the default `off`) nothing reads or writes these tables, except the
-- datastore selector's completeness check, which finds no rows.

CREATE TABLE IF NOT EXISTS artifact_integrity_pins (
    tenant_id          TEXT        NOT NULL,
    index_version_id   TEXT        NOT NULL,
    artifact_id        TEXT        NOT NULL,
    -- Advances on every block and every clearance, never otherwise. A reader
    -- admitted at one generation may release its result only while the pin is
    -- still at that generation and unblocked.
    generation         BIGINT      NOT NULL DEFAULT 0,
    -- observed | blocked | cleared
    state              TEXT        NOT NULL DEFAULT 'observed',
    active_incident_id TEXT,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, index_version_id, artifact_id),
    FOREIGN KEY (tenant_id, index_version_id, artifact_id)
        REFERENCES index_artifacts (tenant_id, index_version_id, artifact_id),
    CONSTRAINT artifact_integrity_pins_state
        CHECK (state IN ('observed', 'blocked', 'cleared')),
    CONSTRAINT artifact_integrity_pins_block
        CHECK ((state = 'blocked') = (active_incident_id IS NOT NULL)),
    CONSTRAINT artifact_integrity_pins_generation CHECK (generation >= 0)
);

CREATE TABLE IF NOT EXISTS artifact_integrity_candidates (
    tenant_id          TEXT        NOT NULL,
    index_version_id   TEXT        NOT NULL,
    artifact_id        TEXT        NOT NULL,
    -- The pinned object: `manifest.json` or a manifest component path.
    object_key         TEXT        NOT NULL,
    expected_sha256    TEXT        NOT NULL,
    -- The store's object generation when it reports one; empty when not.
    storage_generation TEXT        NOT NULL DEFAULT '',
    -- The pin generation this candidate was recorded at. A candidate from an
    -- earlier generation never confirms a block.
    pin_generation     BIGINT      NOT NULL,
    verifier_id        TEXT        NOT NULL,
    verifier_run_id    TEXT        NOT NULL,
    fetch_id           TEXT        NOT NULL,
    fetched_at         TIMESTAMPTZ NOT NULL,
    observed_sha256    TEXT        NOT NULL,
    recorded_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, index_version_id, artifact_id, object_key,
                 expected_sha256, storage_generation),
    FOREIGN KEY (tenant_id, index_version_id, artifact_id)
        REFERENCES artifact_integrity_pins (tenant_id, index_version_id, artifact_id)
);

CREATE TABLE IF NOT EXISTS artifact_integrity_incidents (
    tenant_id                   TEXT        NOT NULL,
    incident_id                 TEXT        NOT NULL,
    index_version_id            TEXT        NOT NULL,
    artifact_id                 TEXT        NOT NULL,
    object_key                  TEXT        NOT NULL,
    expected_sha256             TEXT        NOT NULL,
    storage_generation          TEXT        NOT NULL DEFAULT '',
    -- blocked | cleared
    state                       TEXT        NOT NULL,
    blocked_generation          BIGINT      NOT NULL,
    first_verifier_id           TEXT        NOT NULL,
    first_run_id                TEXT        NOT NULL,
    first_fetch_id              TEXT        NOT NULL,
    first_fetched_at            TIMESTAMPTZ NOT NULL,
    first_observed_sha256       TEXT        NOT NULL,
    second_verifier_id          TEXT        NOT NULL,
    second_run_id               TEXT        NOT NULL,
    second_fetch_id             TEXT        NOT NULL,
    second_fetched_at           TIMESTAMPTZ NOT NULL,
    second_observed_sha256      TEXT        NOT NULL,
    blocked_at                  TIMESTAMPTZ NOT NULL DEFAULT now(),
    cleared_generation          BIGINT,
    cleared_at                  TIMESTAMPTZ,
    clearing_attempt_id         TEXT,
    clearing_workflow_id        TEXT,
    clear_first_verifier_id     TEXT,
    clear_first_run_id          TEXT,
    clear_first_fetch_id        TEXT,
    clear_first_fetched_at      TIMESTAMPTZ,
    clear_first_observed_sha256 TEXT,
    clear_second_verifier_id    TEXT,
    clear_second_run_id         TEXT,
    clear_second_fetch_id       TEXT,
    clear_second_fetched_at     TIMESTAMPTZ,
    clear_second_observed_sha256 TEXT,
    PRIMARY KEY (tenant_id, incident_id),
    FOREIGN KEY (tenant_id, index_version_id, artifact_id)
        REFERENCES artifact_integrity_pins (tenant_id, index_version_id, artifact_id),
    CONSTRAINT artifact_integrity_incidents_state
        CHECK (state IN ('blocked', 'cleared')),
    -- Independence is a schema property as well as a code path.
    CONSTRAINT artifact_integrity_incidents_independent
        CHECK (first_verifier_id <> second_verifier_id
               AND first_fetch_id <> second_fetch_id),
    CONSTRAINT artifact_integrity_incidents_clearance
        CHECK ((state = 'cleared') = (cleared_at IS NOT NULL)
               AND (state <> 'cleared'
                    OR (clearing_attempt_id IS NOT NULL
                        AND clearing_workflow_id IS NOT NULL
                        AND cleared_generation IS NOT NULL
                        AND clear_first_verifier_id IS NOT NULL
                        AND clear_second_verifier_id IS NOT NULL
                        AND clear_first_verifier_id <> clear_second_verifier_id
                        AND clear_first_fetch_id <> clear_second_fetch_id
                        AND clear_first_observed_sha256 = expected_sha256
                        AND clear_second_observed_sha256 = expected_sha256)))
);

-- At most one active block per pin.
CREATE UNIQUE INDEX IF NOT EXISTS artifact_integrity_incidents_one_active
    ON artifact_integrity_incidents (tenant_id, index_version_id, artifact_id)
    WHERE state = 'blocked';

CREATE TABLE IF NOT EXISTS artifact_repair_attempts (
    tenant_id                      TEXT        NOT NULL,
    attempt_id                     TEXT        NOT NULL,
    incident_id                    TEXT        NOT NULL,
    index_version_id               TEXT        NOT NULL,
    artifact_id                    TEXT        NOT NULL,
    -- The preauthorized workflow holding clearance authority for this
    -- incident. Permission to change object bytes is separate.
    workflow_id                    TEXT        NOT NULL,
    authorized_by                  TEXT        NOT NULL,
    reason                         TEXT,
    -- verifying | cleared | invalidated | superseded
    state                          TEXT        NOT NULL DEFAULT 'verifying',
    -- Verification window opens here: an observation fetched earlier is a
    -- pre-repair observation and cannot count.
    authorized_at                  TIMESTAMPTZ NOT NULL DEFAULT now(),
    first_match_verifier_id        TEXT,
    first_match_run_id             TEXT,
    first_match_fetch_id           TEXT,
    first_match_fetched_at         TIMESTAMPTZ,
    first_match_observed_sha256    TEXT,
    first_match_storage_generation TEXT,
    finished_at                    TIMESTAMPTZ,
    finish_reason                  TEXT,
    PRIMARY KEY (tenant_id, attempt_id),
    FOREIGN KEY (tenant_id, incident_id)
        REFERENCES artifact_integrity_incidents (tenant_id, incident_id),
    CONSTRAINT artifact_repair_attempts_state
        CHECK (state IN ('verifying', 'cleared', 'invalidated', 'superseded'))
);

-- One verifying attempt per incident.
CREATE UNIQUE INDEX IF NOT EXISTS artifact_repair_attempts_one_verifying
    ON artifact_repair_attempts (tenant_id, incident_id)
    WHERE state = 'verifying';
CREATE INDEX IF NOT EXISTS artifact_repair_attempts_state_idx
    ON artifact_repair_attempts (state);

CREATE TABLE IF NOT EXISTS artifact_integrity_observations (
    tenant_id          TEXT        NOT NULL,
    observation_id     TEXT        NOT NULL,
    index_version_id   TEXT        NOT NULL,
    artifact_id        TEXT        NOT NULL,
    -- local_isolation | authoritative
    kind               TEXT        NOT NULL,
    object_key         TEXT,
    expected_sha256    TEXT,
    storage_generation TEXT        NOT NULL DEFAULT '',
    -- isolated | match | mismatch | unavailable
    outcome            TEXT        NOT NULL,
    observed_sha256    TEXT,
    verifier_id        TEXT        NOT NULL,
    verifier_run_id    TEXT        NOT NULL,
    -- One fetch counts once, whatever name it is submitted under.
    fetch_id           TEXT,
    fetched_at         TIMESTAMPTZ,
    pin_generation     BIGINT      NOT NULL,
    incident_id        TEXT,
    repair_attempt_id  TEXT,
    -- What the ledger decided for this observation.
    decision           TEXT        NOT NULL,
    -- Bounded operational diagnostics. NEVER corpus or query text.
    detail             TEXT,
    recorded_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, observation_id),
    FOREIGN KEY (tenant_id, index_version_id, artifact_id)
        REFERENCES index_artifacts (tenant_id, index_version_id, artifact_id),
    CONSTRAINT artifact_integrity_observations_kind
        CHECK (kind IN ('local_isolation', 'authoritative')),
    CONSTRAINT artifact_integrity_observations_outcome
        CHECK (outcome IN ('isolated', 'match', 'mismatch', 'unavailable')),
    CONSTRAINT artifact_integrity_observations_detail
        CHECK (detail IS NULL OR char_length(detail) <= 256)
);

CREATE UNIQUE INDEX IF NOT EXISTS artifact_integrity_observations_fetch
    ON artifact_integrity_observations (tenant_id, fetch_id)
    WHERE fetch_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS artifact_integrity_observations_pin_idx
    ON artifact_integrity_observations (tenant_id, index_version_id, artifact_id, recorded_at);
