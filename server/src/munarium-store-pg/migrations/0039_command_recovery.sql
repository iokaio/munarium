-- SPDX-License-Identifier: Apache-2.0
-- Opt-in, durable pre-execution claims. Unresolved work never expires or
-- becomes available for automatic re-execution merely because its owner died.
CREATE TABLE command_recovery_policies (
    tenant_id TEXT PRIMARY KEY,
    activated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE command_receipts (
    tenant_id TEXT NOT NULL,
    key TEXT NOT NULL,
    operation TEXT NOT NULL,
    request_hash TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('unresolved', 'completed')),
    response_body TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    PRIMARY KEY (tenant_id, key),
    CHECK ((state = 'completed') = (response_body IS NOT NULL AND completed_at IS NOT NULL))
);
CREATE INDEX command_receipts_completed ON command_receipts (completed_at)
    WHERE state = 'completed';
