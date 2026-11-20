-- SPDX-License-Identifier: Apache-2.0
-- 0082 (2026-11-20, ClearView ART1/ART2 amendment): the assurance level of
-- every fleet block and every clearance.
--
-- The maintainer decision of 20 November 2026 lets a deployment that declares
-- itself single-replica (MUNARIUM_ARTIFACT_DAMAGE_REPLICAS=single) block and
-- clear with one verifier. Independence between verifiers is then replaced by
-- temporal and fetch independence from the same verifier: two distinct runs,
-- two distinct direct fetches, spaced by at least the configured interval.
-- Such evidence is weaker, so every incident records which rule produced it:
--
--   block_assurance  'independent'    two different verifier identities
--                                     (the 0060 rule, unchanged)
--                    'single_replica' one verifier identity, two runs, two
--                                     fetches at least 60 seconds apart
--   clear_assurance  the same two values for the clearance; NULL exactly
--                    while the incident is not cleared
--
-- Specification: server/docs/design/artifact-damage.md ("Single-replica
-- assurance").
--
-- Additive: two new columns. Every existing incident was confirmed under the
-- 0060 independence CHECK, so the 'independent' default is true of it, and
-- every existing clearance is backfilled to 'independent' for the same
-- reason. The 0060 CHECKs on independence and clearance are replaced by
-- versions that keep the identical condition for 'independent' rows and admit
-- a same-verifier row only when it is marked 'single_replica' and its two
-- runs and fetches differ and are spaced. The 60 seconds is the configuration
-- floor; the configured interval (default 300) is enforced by the ledger.
--
-- Rollback: a binary without this migration's code writes no assurance. Its
-- blocks take the 'independent' default and still satisfy the independent
-- rule. Its clearances violate the clear_assurance CHECK and fail, leaving
-- the block in force: the safe direction.

ALTER TABLE artifact_integrity_incidents
    ADD COLUMN IF NOT EXISTS block_assurance TEXT NOT NULL DEFAULT 'independent',
    ADD COLUMN IF NOT EXISTS clear_assurance TEXT;

UPDATE artifact_integrity_incidents
   SET clear_assurance = 'independent'
 WHERE state = 'cleared' AND clear_assurance IS NULL;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'artifact_integrity_incidents_assurance'
           AND conrelid = 'artifact_integrity_incidents'::regclass
    ) THEN
        ALTER TABLE artifact_integrity_incidents
            DROP CONSTRAINT IF EXISTS artifact_integrity_incidents_independent;
        ALTER TABLE artifact_integrity_incidents
            DROP CONSTRAINT IF EXISTS artifact_integrity_incidents_clearance;

        ALTER TABLE artifact_integrity_incidents
            ADD CONSTRAINT artifact_integrity_incidents_assurance
            CHECK (block_assurance IN ('independent', 'single_replica')
                   AND (clear_assurance IS NULL
                        OR clear_assurance IN ('independent', 'single_replica'))
                   AND ((state = 'cleared') = (clear_assurance IS NOT NULL)));

        -- Independence is still a schema property: unchanged for
        -- 'independent' rows, and replaced by run, fetch and time
        -- separation only for rows marked 'single_replica'.
        ALTER TABLE artifact_integrity_incidents
            ADD CONSTRAINT artifact_integrity_incidents_independent
            CHECK (first_fetch_id <> second_fetch_id
                   AND CASE block_assurance
                       WHEN 'independent' THEN
                           first_verifier_id <> second_verifier_id
                       WHEN 'single_replica' THEN
                           first_verifier_id = second_verifier_id
                           AND first_run_id <> second_run_id
                           AND second_fetched_at
                               >= first_fetched_at + interval '60 seconds'
                       ELSE false
                   END);

        ALTER TABLE artifact_integrity_incidents
            ADD CONSTRAINT artifact_integrity_incidents_clearance
            CHECK ((state = 'cleared') = (cleared_at IS NOT NULL)
                   AND (state <> 'cleared'
                        OR (clearing_attempt_id IS NOT NULL
                            AND clearing_workflow_id IS NOT NULL
                            AND cleared_generation IS NOT NULL
                            AND clear_first_verifier_id IS NOT NULL
                            AND clear_second_verifier_id IS NOT NULL
                            AND clear_first_fetch_id <> clear_second_fetch_id
                            AND clear_first_observed_sha256 = expected_sha256
                            AND clear_second_observed_sha256 = expected_sha256
                            AND CASE clear_assurance
                                WHEN 'independent' THEN
                                    clear_first_verifier_id <> clear_second_verifier_id
                                WHEN 'single_replica' THEN
                                    clear_first_verifier_id = clear_second_verifier_id
                                    AND clear_first_run_id IS NOT NULL
                                    AND clear_second_run_id IS NOT NULL
                                    AND clear_first_fetched_at IS NOT NULL
                                    AND clear_second_fetched_at IS NOT NULL
                                    AND clear_first_run_id <> clear_second_run_id
                                    AND clear_second_fetched_at
                                        >= clear_first_fetched_at + interval '60 seconds'
                                ELSE false
                            END)));
    END IF;
END
$$;
