-- SPDX-License-Identifier: Apache-2.0
-- 0083 (2026-11-20, ClearView ART1/ART2 amendment): enrolled artifact damage
-- verifier keys.
--
-- The maintainer decision of 20 November 2026 rests verifier independence on
-- a per-replica authenticated workload identity instead of a configured
-- instance id over the shared database credential. Each serving replica
-- holds an Ed25519 verifier signing key (MUNARIUM_ARTIFACT_VERIFIER_KEY_FILE)
-- and signs every authoritative observation. The operator enrolls the PUBLIC
-- key here with `munarium-server artifact-damage enroll-verifier`; the ledger
-- counts an observation toward a block or a clearance only when its
-- signature verifies against an enrolled key that is not revoked, and the
-- observation's verifier id is that key's fingerprint.
--
--   artifact_verifier_keys  one row per enrolled key, environment-scoped
--                           like the serving replicas themselves. Public
--                           material only: never a private key.
--
-- Revocation is permanent: `revoke-verifier` sets revoked_at, and enrollment
-- refuses a revoked key. Committed incidents keep their recorded verifier
-- fingerprints whatever happens to the keys later.
--
-- Specification: server/docs/design/artifact-damage.md ("Verifier identity").
--
-- Additive: one new table. No existing row is touched. Until a key is
-- enrolled, `observe` and `enforce` replicas isolate locally and record
-- their observations, but nothing blocks or clears.

CREATE TABLE IF NOT EXISTS artifact_verifier_keys (
    -- `ed25519:` and the lowercase hex SHA-256 of the raw public key.
    fingerprint   TEXT        NOT NULL PRIMARY KEY,
    -- Unpadded base64url of the 32-byte Ed25519 public key.
    public_key    TEXT        NOT NULL UNIQUE,
    -- Operator label for the replica or workload. Not an identity.
    label         TEXT        NOT NULL,
    enrolled_by   TEXT        NOT NULL,
    enrolled_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at    TIMESTAMPTZ,
    revoked_by    TEXT,
    revoke_reason TEXT,
    CONSTRAINT artifact_verifier_keys_fingerprint
        CHECK (fingerprint ~ '^ed25519:[0-9a-f]{64}$'),
    CONSTRAINT artifact_verifier_keys_public_key
        CHECK (public_key ~ '^[A-Za-z0-9_-]{43}$'),
    CONSTRAINT artifact_verifier_keys_revocation
        CHECK ((revoked_at IS NULL) = (revoked_by IS NULL)
               AND (revoked_at IS NOT NULL OR revoke_reason IS NULL)),
    CONSTRAINT artifact_verifier_keys_text
        CHECK (char_length(label) BETWEEN 1 AND 128
               AND char_length(enrolled_by) BETWEEN 1 AND 128
               AND (revoke_reason IS NULL OR char_length(revoke_reason) <= 256))
);
