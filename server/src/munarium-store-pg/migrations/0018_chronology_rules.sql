-- 0018 (2026-08-17): chronology rules assets — the arming surface for the
-- kernel's sixth gate (dev guide §13 entry 13; check_chronology shipped
-- complete in M0 and was unreachable from the wire until now). The raw
-- applied YAML is the stored form (parse-validated at apply time, parsed
-- again at use — the same posture as shapes/provider configs); a memory
-- version arms by naming an asset in its metadata:
--   {"chronology_rules": "<name>"}
CREATE TABLE IF NOT EXISTS chronology_rules (
    tenant_id  TEXT NOT NULL,
    name       TEXT NOT NULL,
    yaml       TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, name)
);
