-- 0026 (2026-08-30, datastore Phase 3): the artifact catalog and its plumbing.
--
-- PostgreSQL stays the system of record. These tables are the DURABLE TRUTH
-- about immutable derived search artifacts that live elsewhere: what exists,
-- which one currently serves, who is building one, which scopes are routed to
-- the new engine, and what the fleet is supposed to look like when a cutover
-- is judged safe.
--
-- Nine tables, each answering one question:
--
--   index_artifacts                 what physical artifacts exist, and are they usable
--   index_artifact_bindings         which one is staged / shadow / serving, now
--   index_artifact_binding_events   how it got that way, append-only
--   index_execution_artifacts       which artifact a durably-audited operation used
--   retrieval_rollout               which scopes the datastore serves
--   retrieval_plane_expectations    how many nodes OUGHT to exist before a cutover
--   index_build_attempts            who is building what, with a lease
--   retrieval_node_snapshots        what each process is holding (soft state)
--   index_artifact_residency_snapshots  which artifacts are resident where (soft state)
--   index_artifact_parts            optional: artifact bytes in Postgres
--
-- Three distinctions run through all of it, and blurring any one is how this
-- design fails:
--
--   LOGICAL vs PHYSICAL. A collection's active `index_versions` pointer says
--   which CORPUS is current. A binding says which ARTIFACT implements it. An
--   engine upgrade changes the binding and leaves every session pin intact;
--   conflating them would make a Tantivy version bump look like a reindex.
--
--   TRUTH vs SOFT STATE. The first seven tables are authoritative. The two
--   snapshot tables are not: L0 and L1 are process-local, and these rows are a
--   bounded, lagging report so `/admin` and the cutover gate can SEE the
--   fleet. Correctness never depends on them -- exact-version resolution,
--   manifest verification and per-request open enforce it independently.
--
--   CONTENT vs AUTHORITY. `artifact_id` is a content hash. The same corpus in
--   two tenants legitimately produces the same hash, so every key here carries
--   `tenant_id` separately and no lookup accepts a bare artifact hash as
--   permission to read anything.
--
-- Additive: no existing table is touched. A deployment that never enables the
-- datastore carries ten empty tables and behaves exactly as before.

-- ---------------------------------------------------------------------------
-- The catalog.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS index_artifacts (
    tenant_id            TEXT        NOT NULL,
    -- The LOGICAL version this artifact implements. An existing `idx-` id for
    -- a mirror build, or an `idx2-` id for a direct one.
    index_version_id     TEXT        NOT NULL,
    -- sha256 of the canonical manifest.json. The ONE physical content
    -- identifier; a second physical hash must never be introduced.
    artifact_id          TEXT        NOT NULL,
    -- Adapter/compatibility family (`tantivy`, ...). The exact upstream
    -- revision lives in the physical plan, not here: this column is what a
    -- reader-compatibility check reads, and it must stay coarse enough to
    -- index on.
    engine_id            TEXT        NOT NULL,
    -- sealed -> verified -> retired, or failed. PRE-seal state belongs to the
    -- attempt row: an artifact that does not exist yet has nothing to catalog.
    state                TEXT        NOT NULL,
    format_version       INT         NOT NULL,
    -- Prefix or manifest URI, WITHOUT credentials. A SAS token in a catalog
    -- row is a credential in every backup and every admin page that renders it.
    artifact_uri         TEXT        NOT NULL,
    artifact_plan        JSONB       NOT NULL,
    artifact_plan_sha256 TEXT        NOT NULL,
    -- An AUDIT PROJECTION of the canonical L2 manifest, never a reader input.
    -- The open path fetches the L2 bytes and checks sha256 == artifact_id; a
    -- reader that trusted this column could be pointed at any files at all by
    -- anyone with UPDATE on this table.
    artifact_manifest    JSONB       NOT NULL,
    bytes_len            BIGINT      NOT NULL,
    file_count           INT         NOT NULL,
    -- Where the non-content metadata lives, because the manifest is content-
    -- pure. This is the half of "who built this and when" that had to go
    -- somewhere for byte-identical rebuilds to converge on one id.
    built_by             TEXT,
    verified_by          TEXT,
    attempt_id           TEXT,
    -- Bounded operational diagnostics. NEVER source or query text: this table
    -- is read by an admin page and copied into every backup.
    failure_code         TEXT,
    failure_detail       TEXT,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    sealed_at            TIMESTAMPTZ,
    verified_at          TIMESTAMPTZ,
    retired_at           TIMESTAMPTZ,
    last_verified_at     TIMESTAMPTZ,
    PRIMARY KEY (tenant_id, index_version_id, artifact_id)
);

CREATE INDEX IF NOT EXISTS index_artifacts_tenant_state_idx
    ON index_artifacts (tenant_id, state);
CREATE INDEX IF NOT EXISTS index_artifacts_tenant_version_idx
    ON index_artifacts (tenant_id, index_version_id);
-- Reuse lookup: "has this exact plan already been built for this version?"
CREATE INDEX IF NOT EXISTS index_artifacts_plan_idx
    ON index_artifacts (tenant_id, index_version_id, artifact_plan_sha256);

-- ---------------------------------------------------------------------------
-- Current bindings, and their history.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS index_artifact_bindings (
    tenant_id        TEXT        NOT NULL,
    index_version_id TEXT        NOT NULL,
    -- staged | shadow | serving. Exactly one row per slot per version, which
    -- is what makes promotion a compare-and-swap rather than a merge.
    slot             TEXT        NOT NULL,
    artifact_id      TEXT        NOT NULL,
    -- Monotonic per (tenant, version, slot). A promotion supplies the
    -- generation it READ, so a concurrent change loses rather than silently
    -- overwrites.
    generation       BIGINT      NOT NULL,
    selected_by      TEXT        NOT NULL,
    reason           TEXT,
    selected_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, index_version_id, slot),
    FOREIGN KEY (tenant_id, index_version_id, artifact_id)
        REFERENCES index_artifacts (tenant_id, index_version_id, artifact_id)
);

-- Append-only. There is deliberately no `previous_artifact_id` column on the
-- binding: one mutable "previous" cannot express several engine promotions
-- inside one retention horizon, and garbage collection has to know about all
-- of them. Rows are never rewritten to emulate current state.
CREATE TABLE IF NOT EXISTS index_artifact_binding_events (
    tenant_id         TEXT        NOT NULL,
    event_id          TEXT        NOT NULL,
    index_version_id  TEXT        NOT NULL,
    slot              TEXT        NOT NULL,
    -- Nullable in each direction: an insert has no `from`, a clear has no `to`.
    from_artifact_id  TEXT,
    to_artifact_id    TEXT,
    from_generation   BIGINT,
    to_generation     BIGINT,
    operation         TEXT        NOT NULL,
    actor             TEXT        NOT NULL,
    reason            TEXT,
    correlation_id    TEXT,
    occurred_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- When the DISPLACED artifact's bytes may be collected. Garbage collection
    -- consults this, current bindings, and strict-replay references -- an
    -- artifact no slot names may still be inside someone's pin horizon.
    retain_until      TIMESTAMPTZ,
    PRIMARY KEY (tenant_id, event_id)
);

CREATE INDEX IF NOT EXISTS index_artifact_binding_events_version_idx
    ON index_artifact_binding_events (tenant_id, index_version_id, occurred_at DESC);
CREATE INDEX IF NOT EXISTS index_artifact_binding_events_retain_idx
    ON index_artifact_binding_events (retain_until)
    WHERE retain_until IS NOT NULL;

-- Which artifact a DURABLY AUDITED operation actually used.
--
-- Written only for operations that already have an execution identity --
-- runbook executions, retained shadow comparisons. An ordinary search does NOT
-- write here and promises logical-version replay only; the public envelope's
-- index_version is the logical corpus version, and that wire contract does not
-- change. Its foreign key is what stops collection of an artifact some
-- retained execution still promises to replay.
CREATE TABLE IF NOT EXISTS index_execution_artifacts (
    tenant_id          TEXT        NOT NULL,
    execution_kind     TEXT        NOT NULL,
    execution_id       TEXT        NOT NULL,
    correlation_id     TEXT,
    index_version_id   TEXT        NOT NULL,
    artifact_id        TEXT        NOT NULL,
    engine_id          TEXT        NOT NULL,
    binding_generation BIGINT      NOT NULL,
    resolved_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, execution_kind, execution_id, index_version_id),
    FOREIGN KEY (tenant_id, index_version_id, artifact_id)
        REFERENCES index_artifacts (tenant_id, index_version_id, artifact_id)
);

CREATE INDEX IF NOT EXISTS index_execution_artifacts_artifact_idx
    ON index_execution_artifacts (tenant_id, index_version_id, artifact_id);

-- ---------------------------------------------------------------------------
-- The rollout selector: which scopes the datastore serves.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS retrieval_rollout (
    tenant_id                TEXT        NOT NULL,
    scope_kind               TEXT        NOT NULL,
    scope_id                 TEXT        NOT NULL,
    -- postgres | datastore. The default everywhere is postgres, including for
    -- a scope with no row at all.
    serving                  TEXT        NOT NULL DEFAULT 'postgres',
    shadow_sample_rate       DOUBLE PRECISION,
    -- Lets a still-PostgreSQL-served scope hydrate a candidate BEFORE the
    -- selector flips, so the cutover does not begin with a cold fleet.
    -- Clearing it never changes serving.
    prewarm_staged           BOOLEAN     NOT NULL DEFAULT false,
    -- active | active_and_pinned | active_pinned_and_horizon.
    -- The last is the default because it is the only one that survives a
    -- session resolving a version that stopped being active mid-conversation.
    required_versions_policy TEXT        NOT NULL DEFAULT 'active_pinned_and_horizon',
    generation               BIGINT      NOT NULL DEFAULT 0,
    changed_by               TEXT        NOT NULL,
    reason                   TEXT,
    changed_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, scope_kind, scope_id)
);

CREATE INDEX IF NOT EXISTS retrieval_rollout_serving_idx
    ON retrieval_rollout (serving);

-- What the fleet OUGHT to be.
--
-- Heartbeat rows can prove a process exists; their absence cannot prove how
-- many ought to. Without this table a cutover would treat whichever nodes
-- happen to be reporting as the desired fleet, and a crashed replica would
-- make a promotion look safer rather than less safe.
CREATE TABLE IF NOT EXISTS retrieval_plane_expectations (
    environment_id        TEXT        NOT NULL,
    -- rest | grpc | builder | retrieval
    plane                 TEXT        NOT NULL,
    deployment_revision   TEXT        NOT NULL,
    -- Counts are evaluated BEFORE any fraction: "50% of the nodes that showed
    -- up" is satisfied by one node out of one.
    minimum_fresh_nodes   INT         NOT NULL,
    minimum_open_nodes    INT         NOT NULL,
    minimum_open_fraction DOUBLE PRECISION,
    required_mode         TEXT        NOT NULL,
    compatibility_policy  TEXT,
    -- What the platform actually says, recorded by deployment automation.
    -- Terraform defaults are not accepted evidence: the Container Apps
    -- lifecycle deliberately ignores min_replicas, so the declared value and
    -- the effective one can differ.
    observed_min_replicas INT,
    generation            BIGINT      NOT NULL DEFAULT 0,
    actor                 TEXT        NOT NULL,
    reason                TEXT,
    deployment_correlation_id TEXT,
    changed_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    verified_at           TIMESTAMPTZ,
    verification_source   TEXT,
    PRIMARY KEY (environment_id, plane, deployment_revision),
    -- A zero-node expectation is not an expectation; it is a cutover with no
    -- gate. Refused in the schema so no code path can write one.
    CONSTRAINT retrieval_plane_expectations_positive
        CHECK (minimum_fresh_nodes > 0 AND minimum_open_nodes > 0),
    -- A percentage with no positive floor is satisfied by one node out of one.
    CONSTRAINT retrieval_plane_expectations_fraction
        CHECK (minimum_open_fraction IS NULL
               OR (minimum_open_fraction > 0 AND minimum_open_fraction <= 1))
);

-- ---------------------------------------------------------------------------
-- Build attempts: pre-seal state, ownership, reconciliation.
-- ---------------------------------------------------------------------------
--
-- This replaces a long-held advisory lock. A lease expires and can be
-- reclaimed, is visible to the reconciler and to `/admin`, and does not pin a
-- pool connection for the duration of a build -- which a lock held across
-- extraction and index construction necessarily would.
CREATE TABLE IF NOT EXISTS index_build_attempts (
    tenant_id            TEXT        NOT NULL,
    attempt_id           TEXT        NOT NULL,
    index_version_id     TEXT        NOT NULL,
    artifact_plan_sha256 TEXT        NOT NULL,
    -- mirror | direct | backfill
    mode                 TEXT        NOT NULL,
    -- running | sealed | succeeded | converged | failed | cancelled | expired.
    -- `converged` is its own state on purpose: an attempt that discovered an
    -- identical artifact already cataloged did not fail, and recording it as a
    -- failure would make a healthy rebuild look like an incident.
    state                TEXT        NOT NULL,
    owner_node_id        TEXT        NOT NULL,
    lease_expires_at     TIMESTAMPTZ NOT NULL,
    last_heartbeat_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    attempt_no           INT         NOT NULL DEFAULT 1,
    -- Redacted in any UI: a filesystem path is infrastructure detail.
    l1_staging_path      TEXT,
    l2_staging_prefix    TEXT,
    artifact_id          TEXT,
    failure_code         TEXT,
    failure_detail       TEXT,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    sealed_at            TIMESTAMPTZ,
    finished_at          TIMESTAMPTZ,
    PRIMARY KEY (tenant_id, attempt_id)
);

-- Single-flight: one RUNNING attempt per (tenant, version, plan). Partial, so
-- finished attempts accumulate freely as history -- the constraint is about
-- concurrent work, not about how many times something was built.
CREATE UNIQUE INDEX IF NOT EXISTS index_build_attempts_single_flight
    ON index_build_attempts (tenant_id, index_version_id, artifact_plan_sha256)
    WHERE state = 'running';

CREATE INDEX IF NOT EXISTS index_build_attempts_reclaim_idx
    ON index_build_attempts (state, lease_expires_at);

-- ---------------------------------------------------------------------------
-- Soft state. NOT authoritative -- see the header.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS retrieval_node_snapshots (
    environment_id       TEXT        NOT NULL,
    -- Opaque per-process id. Deliberately not a hostname: this table is read
    -- by an admin page, and infrastructure identifiers do not need to be there.
    node_id              TEXT        NOT NULL,
    plane                TEXT        NOT NULL,
    deployment_revision  TEXT        NOT NULL,
    retrieval_mode       TEXT        NOT NULL,
    compiled_engines     TEXT[]      NOT NULL DEFAULT '{}',
    format_min           INT         NOT NULL,
    format_max           INT         NOT NULL,
    rollout_generation   BIGINT,
    -- warming | ready | draining. A missing row is UNKNOWN, never "absent" and
    -- never "healthy": a fleet cannot be judged by who happens to answer.
    admission_state      TEXT        NOT NULL,
    -- HASHED scope keys only. The count is what an operator needs; the names
    -- would put tenant-derived identifiers in a shared table.
    blocking_scope_hashes TEXT[]     NOT NULL DEFAULT '{}',
    l0_used_bytes        BIGINT,
    l0_budget_bytes      BIGINT,
    l0_open_handles      INT,
    l1_used_bytes        BIGINT,
    l1_budget_bytes      BIGINT,
    l1_free_bytes        BIGINT,
    l1_sealed_shards     INT,
    local_root_health    TEXT,
    query_active         INT,
    query_queued         INT,
    query_rejected       BIGINT,
    hydrate_active       INT,
    hydrate_queued       INT,
    build_active         INT,
    started_at           TIMESTAMPTZ NOT NULL,
    last_seen_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (environment_id, node_id)
);

CREATE INDEX IF NOT EXISTS retrieval_node_snapshots_seen_idx
    ON retrieval_node_snapshots (last_seen_at);

-- Only artifacts that are RESIDENT or in progress. Absence is represented by
-- the absence of a fresh row, so a node that stops reporting does not appear
-- to be holding a stale set forever.
CREATE TABLE IF NOT EXISTS index_artifact_residency_snapshots (
    environment_id   TEXT        NOT NULL,
    node_id          TEXT        NOT NULL,
    tenant_id        TEXT        NOT NULL,
    index_version_id TEXT        NOT NULL,
    artifact_id      TEXT        NOT NULL,
    engine_id        TEXT        NOT NULL,
    -- hydrating | sealed | open | evicting | quarantined
    residency_state  TEXT        NOT NULL,
    local_bytes      BIGINT,
    open_count       INT,
    pin_count        INT,
    last_access_at   TIMESTAMPTZ,
    last_verified_at TIMESTAMPTZ,
    last_seen_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (environment_id, node_id, tenant_id, index_version_id, artifact_id)
);

CREATE INDEX IF NOT EXISTS index_artifact_residency_seen_idx
    ON index_artifact_residency_snapshots (last_seen_at);
CREATE INDEX IF NOT EXISTS index_artifact_residency_artifact_idx
    ON index_artifact_residency_snapshots (tenant_id, index_version_id, artifact_id);

-- ---------------------------------------------------------------------------
-- Optional: artifact bytes in Postgres.
-- ---------------------------------------------------------------------------
--
-- For local, CI and small self-contained installations, so `docker compose up`
-- and `cargo test` need no object store -- the same reasoning that put source
-- bytes in a table. NOT the Azure default: a size ceiling is enforced before
-- the first write, and exceeding it is a hard instruction to use object
-- storage rather than a silently oversized transaction.
CREATE TABLE IF NOT EXISTS index_artifact_parts (
    tenant_id   TEXT   NOT NULL,
    artifact_id TEXT   NOT NULL,
    object_path TEXT   NOT NULL,
    part_no     INT    NOT NULL,
    part_sha256 TEXT   NOT NULL,
    bytes_len   BIGINT NOT NULL,
    bytes       BYTEA  NOT NULL,
    PRIMARY KEY (tenant_id, artifact_id, object_path, part_no)
);
