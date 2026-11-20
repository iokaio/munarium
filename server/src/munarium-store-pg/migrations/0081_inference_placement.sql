-- SPDX-License-Identifier: Apache-2.0
-- 0081 (ClearView SC2): a collection binding's inference placement requirement.
--
-- Additive only. NULL, the value of every existing row, means no placement
-- check applies, exactly as in Phase 6. A non-NULL value holds the binding's
-- requirement: `isolation` (`dedicated` or `shared`) and the processing rules
-- (attribute to value) the bound configuration's declared placement must
-- match before any content is sent. Content-free: names and fixed values.
ALTER TABLE collection_inference_bindings ADD COLUMN IF NOT EXISTS placement JSONB;
