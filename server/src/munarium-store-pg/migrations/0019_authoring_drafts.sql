-- 0019 (2026-08-19): guided authoring drafts — the server-side workspace for
-- composing a shapes+runbook set before it is exported as a bundle or
-- applied. Raw interview answers and the materialized YAML are the stored
-- forms; both are re-validated at every use (the same posture shapes and
-- runbooks take: stored state is never trusted over a fresh check). Soft
-- delete only, like every other lifecycle in this store.
CREATE TABLE IF NOT EXISTS authoring_drafts (
    tenant_id   TEXT NOT NULL,
    draft_id    TEXT NOT NULL,                       -- 'draft-' || uuidv7 suffix
    name        TEXT NOT NULL,                       -- the runbook metadata.name
    pattern_id  TEXT,                                -- NULL = blank start
    state       TEXT NOT NULL DEFAULT 'interview',   -- interview|drafted|validated|exported (display only)
    answers     JSONB NOT NULL DEFAULT '{}'::jsonb,  -- flat map keyed by interview question id
    documents   JSONB NOT NULL DEFAULT '{}'::jsonb,  -- {"shapes/x.yaml": "<yaml>", ...}
    findings    JSONB,                               -- last validation snapshot (informational cache)
    assist_note TEXT,                                -- last AI-assist degrade note, if any
    created_by  TEXT NOT NULL,                       -- x-mmesh-uid
    status      TEXT NOT NULL DEFAULT 'active',      -- active|deleted (soft delete)
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, draft_id)
);
CREATE INDEX IF NOT EXISTS authoring_drafts_tenant_updated
    ON authoring_drafts (tenant_id, updated_at DESC);
