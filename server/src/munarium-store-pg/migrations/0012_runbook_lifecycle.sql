-- M9/M11: runbook lifecycle. Removal is a double-pass soft transition —
-- the yaml is retained forever; a removed runbook is merely inaccessible
-- (hidden from list/info, rejected by sessions). No DDL here ever deletes.
ALTER TABLE runbooks ADD COLUMN IF NOT EXISTS status TEXT NOT NULL DEFAULT 'active';
                              -- active | remove_requested | removed
ALTER TABLE runbooks ADD COLUMN IF NOT EXISTS removal_id TEXT;
ALTER TABLE runbooks ADD COLUMN IF NOT EXISTS removal_requested_at TIMESTAMPTZ;
ALTER TABLE runbooks ADD COLUMN IF NOT EXISTS removal_requested_by TEXT;
ALTER TABLE runbooks ADD COLUMN IF NOT EXISTS removed_at TIMESTAMPTZ;
ALTER TABLE runbooks ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT now();
