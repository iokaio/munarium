-- SPDX-License-Identifier: Apache-2.0
-- Units remain the accounted amount for old readers/writers. Never infer
-- original reservations or observed quality from these historically mutable units.
ALTER TABLE token_budget_reservations
    ADD COLUMN original_units BIGINT CHECK (original_units >= 0),
    ADD COLUMN usage_evidence JSONB;
-- NULL defaults intentionally support historical rows and old writers.
