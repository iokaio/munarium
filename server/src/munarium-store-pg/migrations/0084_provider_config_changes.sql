-- SPDX-License-Identifier: Apache-2.0
-- 0084 (ClearView IR-1): when a provider configuration's text last changed.
--
-- Additive only. An inference probe is valid only for the configuration text
-- it ran against AND only when it started after that text was last changed.
-- Matching the text hash alone let a revert (A, then B, then A again) revive
-- the probe that passed against the first A, although the endpoint may have
-- drifted in between. Every apply that changes the text sets this column;
-- re-applying identical text leaves it. Existing rows take the time this
-- migration runs, so a probe recorded before it must be repeated: the
-- conservative direction. Content-free: a timestamp.
ALTER TABLE provider_configs
    ADD COLUMN IF NOT EXISTS text_changed_at TIMESTAMPTZ NOT NULL DEFAULT now();
