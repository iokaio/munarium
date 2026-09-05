-- 0006 (Phase 6, WP-6.3): native data views share the verification record
-- with metric views, so the record says WHICH kind of asset a fingerprint
-- belongs to. A metric view and a data view may share a name; their
-- definitions are different objects. Additive: existing rows are metric views.

ALTER TABLE matrix.metric_view_verifications
    ADD COLUMN IF NOT EXISTS kind TEXT NOT NULL DEFAULT 'MetricView';
