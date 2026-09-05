-- 0028 (2026-08-31, datastore Phase 8 live incident)
--
-- A version that has NEVER been activated can hold no session pins: a pin is
-- taken from the version a session found active, so the horizon term of
-- required_versions — which exists to cover pins after supersession — must
-- not count versions that were never active. Found live on dev: the first
-- direct build committed its idx2 version row (insert-only, artifact still
-- building), the version joined the datastore-routed scope's
-- serving-required set purely by built_at, the readiness warmer correctly
-- refused the incomplete set, /readyz went false on the only replica, ACA
-- pulled it from ingress — and the promote call that would have completed
-- the set needed the very API the wedge had taken down.
--
-- `activated_at` records the FIRST activation. The backfill rule is exact
-- for every database this migration can reach: legacy `idx-…` versions were
-- created by build paths that activate in the same operation, so they have
-- all been active; `idx2-…` versions activate explicitly through the CAS,
-- and none had been activated when this migration was written. The belt on
-- top: anything CURRENTLY active was by definition activated.
ALTER TABLE index_versions ADD COLUMN IF NOT EXISTS activated_at TIMESTAMPTZ;
UPDATE index_versions SET activated_at = built_at
 WHERE activated_at IS NULL AND id NOT LIKE 'idx2-%';
UPDATE index_versions SET activated_at = built_at
 WHERE activated_at IS NULL AND active;
