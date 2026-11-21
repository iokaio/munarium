-- SPDX-License-Identifier: Apache-2.0

ALTER TABLE server_replica_heartbeats
    ADD COLUMN mcp_generation_policy_sha256 text
    CHECK (mcp_generation_policy_sha256 ~ '^[0-9a-f]{64}$');
