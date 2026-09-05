-- 0017 (2026-08-17): persist gate findings. Until now findings were
-- computed on every gated write and serialized ONLY into the write
-- response — a dropped response lost the evidence pane behind a disputed
-- claim (the dev guide's §13 entry 12). Rows are stamped with the head seq
-- their write settled at, so one as_of_seq pin bounds this store like
-- every other. Additive, append-only; severity is the gate vocabulary
-- (info | warn | block).
CREATE TABLE IF NOT EXISTS gate_findings (
    tenant_id   TEXT NOT NULL,
    version_id  TEXT NOT NULL,
    seq         BIGINT NOT NULL,
    rule_id     TEXT NOT NULL,
    severity    TEXT NOT NULL,
    message     TEXT NOT NULL,
    scope_path  TEXT,
    detail      JSONB,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_gate_findings_version
    ON gate_findings (tenant_id, version_id, seq);
