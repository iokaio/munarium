-- 0016 (2026-08-17): N-replica debugging — which instance served the call.
-- The interaction writer stamps every row with the instance's identity
-- (MMESH_INSTANCE_ID -> HOSTNAME -> COMPUTERNAME -> random). Nullable and
-- unindexed on purpose: it is a debugging dimension, not a query key, and
-- rows written by pre-0016 binaries simply have no instance recorded.
ALTER TABLE interactions ADD COLUMN IF NOT EXISTS instance_id TEXT;
