-- 0080 (ClearView IR-1 to IR-4, I1, I2): the optional inference endpoint.
--
-- Additive only. No existing table or column changes. A deployment that never
-- applies an `openai-compatible` provider configuration, never probes it and
-- never binds a collection carries these empty tables and behaves exactly as
-- v1.4: no enrollment exists, so no read consults an endpoint, and no index
-- id changes (COMPAT-T4).

-- IR-1: conformance probe results. A probe is valid only for the provider
-- configuration text it ran against (`config_hash`); any change to that text
-- is drift, and the stages report `probe_failed` until a new probe passes.
-- `checks` holds fixed check names, outcomes and content-free reason codes.
CREATE TABLE IF NOT EXISTS inference_probes (
    tenant_id            TEXT        NOT NULL,
    probe_id             TEXT        NOT NULL,
    provider_config      TEXT        NOT NULL,
    config_hash          TEXT        NOT NULL,
    endpoint_fingerprint TEXT        NOT NULL,
    embed_model          TEXT,
    rerank_model         TEXT,
    embed_dimensions     INTEGER,
    passed               BOOLEAN     NOT NULL,
    checks               JSONB       NOT NULL,
    probed_by            TEXT        NOT NULL,
    probed_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, probe_id)
);

CREATE INDEX IF NOT EXISTS inference_probes_latest_idx
    ON inference_probes (tenant_id, provider_config, probed_at DESC);

-- IR-2/IR-4: one binding per collection, compare-and-set on `revision`.
-- `state` is bound (configured; shadow only), enrolled (serving) or
-- rolled_back (the prior approved index restored). `embedding`, `rerank` and
-- `shadow` hold the operator's stage settings: model names, instructions,
-- caps, policies and shadow switches. Never credentials.
CREATE TABLE IF NOT EXISTS collection_inference_bindings (
    tenant_id         TEXT        NOT NULL,
    collection_id     TEXT        NOT NULL,
    revision          BIGINT      NOT NULL,
    state             TEXT        NOT NULL,
    provider_config   TEXT        NOT NULL,
    embedding         JSONB,
    rerank            JSONB,
    shadow            JSONB       NOT NULL DEFAULT '{}'::jsonb,
    timeout_ms        INTEGER     NOT NULL,
    semantic_index_id TEXT,
    prior_index_id    TEXT,
    updated_by        TEXT        NOT NULL,
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, collection_id),
    CONSTRAINT collection_inference_bindings_state
        CHECK (state IN ('bound', 'enrolled', 'rolled_back')),
    CONSTRAINT collection_inference_bindings_revision CHECK (revision >= 1)
);

-- Append-only history of every binding change: who did what at which
-- revision. The binding row is the current state; this is the audit trail.
CREATE TABLE IF NOT EXISTS collection_inference_binding_history (
    tenant_id     TEXT        NOT NULL,
    collection_id TEXT        NOT NULL,
    revision      BIGINT      NOT NULL,
    action        TEXT        NOT NULL,
    state         TEXT        NOT NULL,
    binding       JSONB       NOT NULL,
    actor         TEXT        NOT NULL,
    recorded_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, collection_id, revision)
);

-- IR-2: one semantic vector per chunk of a semantic index version, beside
-- the unchanged VECTOR(256) hash columns. Exact scan per index version.
CREATE TABLE IF NOT EXISTS semantic_chunk_vectors (
    tenant_id        TEXT         NOT NULL,
    collection_id    TEXT         NOT NULL,
    index_version_id TEXT         NOT NULL,
    chunk_id         TEXT         NOT NULL,
    embedding        VECTOR(1024) NOT NULL,
    PRIMARY KEY (tenant_id, index_version_id, chunk_id)
);

CREATE INDEX IF NOT EXISTS semantic_chunk_vectors_collection_idx
    ON semantic_chunk_vectors (tenant_id, collection_id, index_version_id);

-- I2: every physical endpoint attempt, reserved before dispatch. The payer is
-- the collection's tenant (the bound configuration's budget); `caller` is the
-- initiating principal, carried through queued work; `collection_ids` lists
-- every collection a shared call served. `estimate_units` and
-- `accounted_units` are tokens; `usage_source` is `provider_reported` or
-- `missing` (unknown usage is charged at the estimate and stays visible).
CREATE TABLE IF NOT EXISTS inference_attempts (
    tenant_id       TEXT        NOT NULL,
    attempt_id      TEXT        NOT NULL,
    purpose         TEXT        NOT NULL,
    stage           TEXT        NOT NULL,
    provider_config TEXT        NOT NULL,
    model           TEXT        NOT NULL,
    caller          TEXT        NOT NULL,
    collection_ids  TEXT[]      NOT NULL DEFAULT '{}',
    job_id          TEXT,
    reservation_id  TEXT,
    estimate_units  BIGINT      NOT NULL,
    accounted_units BIGINT,
    usage_source    TEXT,
    outcome         TEXT        NOT NULL DEFAULT 'started',
    reason          TEXT,
    started_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at     TIMESTAMPTZ,
    PRIMARY KEY (tenant_id, attempt_id),
    CONSTRAINT inference_attempts_purpose
        CHECK (purpose IN ('serving', 'shadow', 'build', 'probe'))
);

CREATE INDEX IF NOT EXISTS inference_attempts_day_idx
    ON inference_attempts (tenant_id, started_at);

-- I1: a governed read's stage that did not apply: a disclosed fallback under
-- `prefer`, or a refusal under `require`. Content-free: stage, state, reason
-- code and the policy revision in force.
CREATE TABLE IF NOT EXISTS inference_stage_events (
    tenant_id       TEXT        NOT NULL,
    event_id        TEXT        NOT NULL,
    collection_id   TEXT        NOT NULL,
    stage           TEXT        NOT NULL,
    policy          TEXT        NOT NULL,
    policy_revision BIGINT      NOT NULL,
    outcome         TEXT        NOT NULL,
    state           TEXT        NOT NULL,
    reason          TEXT        NOT NULL,
    caller          TEXT        NOT NULL,
    recorded_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, event_id)
);

-- IR-2/IR-3: shadow comparisons. Only a query fingerprint is stored, never
-- the query; the measures compare chunk identities, never text.
CREATE TABLE IF NOT EXISTS inference_shadow_comparisons (
    tenant_id          TEXT        NOT NULL,
    comparison_id      TEXT        NOT NULL,
    collection_id      TEXT        NOT NULL,
    kind               TEXT        NOT NULL,
    query_fingerprint  TEXT        NOT NULL,
    served_index_id    TEXT        NOT NULL,
    candidate_index_id TEXT,
    outcome            TEXT        NOT NULL,
    reason             TEXT,
    k                  INTEGER     NOT NULL,
    overlap_at_k       INTEGER,
    top1_agrees        BOOLEAN,
    moved_positions    INTEGER,
    caller             TEXT        NOT NULL,
    recorded_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, comparison_id)
);
