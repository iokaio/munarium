-- 0005 (Phase 6, WP-6.1): a metric view's verification record.
--
-- A metric view is a semantic layer the SOURCE owns, so what Matrix verified
-- is not only "the questions still answer the same way" but "the definition
-- was THIS one when they did". The fingerprint recorded here is what an
-- execute compares the live definition against; a mismatch is refused as
-- `metric_view_changed` until someone verifies again. Rows are never
-- deleted: the history of when a definition was last known-good is part of
-- the evidence story.
--
-- Additive, like every migration in this schema.

CREATE TABLE IF NOT EXISTS matrix.metric_view_verifications (
    tenant_id     TEXT        NOT NULL,
    view_name     TEXT        NOT NULL,
    view_version  INTEGER     NOT NULL,
    -- `sha256:<hex>` over the definition the source reported, LF-normalised.
    fingerprint   TEXT        NOT NULL,
    passed        INTEGER     NOT NULL,
    failed        INTEGER     NOT NULL,
    verified_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, view_name, view_version, verified_at)
);

CREATE INDEX IF NOT EXISTS ix_metric_view_verifications_latest
    ON matrix.metric_view_verifications (tenant_id, view_name, view_version, verified_at DESC);
