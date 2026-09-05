-- M12 hardening: the default audit page (GET /v1/reports/audit with no
-- uid/session/runbook filter) scans by (tenant_id, created_at DESC). The
-- 0010 indexes all interpose a filter column between tenant_id and
-- created_at, so none serves the unfiltered newest-first scan. This index
-- does, and it also backs the usage/cost report time-range scans.
CREATE INDEX IF NOT EXISTS idx_interactions_tenant_created
    ON interactions (tenant_id, created_at DESC);
