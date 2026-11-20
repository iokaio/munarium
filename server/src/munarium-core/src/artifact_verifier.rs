// SPDX-License-Identifier: Apache-2.0
//! Per-replica authenticated verifier identity for durable artifact damage
//! (ClearView ART1/ART2, hub ADR 0028 as amended on 20 November 2026).
//!
//! Each serving replica holds an Ed25519 verifier signing key. The operator
//! enrolls the public key; the ledger counts an authoritative observation
//! toward a fleet block or a clearance only when it is signed by an enrolled,
//! unrevoked key, and the verifier identity *is* that key's fingerprint. Two
//! replicas that share a key are therefore one verifier.
//!
//! This module is pure: key encoding, the canonical observation encoding,
//! signing and verification. Enrollment lives in the PostgreSQL ledger.
//! Public material and signatures use unpadded base64url; a secret seed is
//! encoded the same way and must never be logged or tracked.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};

use crate::{KernelError, Result};

/// Prefix of a verifier fingerprint: `ed25519:` and 64 lowercase hex digits
/// of the SHA-256 of the raw 32-byte public key.
pub const FINGERPRINT_PREFIX: &str = "ed25519:";
/// Domain separation for observation signatures.
const DOMAIN: &[u8] = b"munarium.artifact-damage.observation.v1";
/// Domain separation for seed generation.
const SEED_DOMAIN: &[u8] = b"munarium.artifact-damage.verifier-seed.v1";

fn decode_exact(text: &str) -> Option<Vec<u8>> {
    let bytes = B64.decode(text).ok()?;
    // Canonical encodings only, so one key has one spelling.
    (B64.encode(&bytes) == text).then_some(bytes)
}

/// A replica's verifier signing key. `Debug` never shows the secret.
pub struct VerifierSigningKey {
    key: SigningKey,
}

impl std::fmt::Debug for VerifierSigningKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifierSigningKey")
            .field("fingerprint", &self.fingerprint())
            .finish_non_exhaustive()
    }
}

impl VerifierSigningKey {
    /// A fresh key. The seed is the SHA-256 of four version-4 UUIDs (488
    /// random bits from the operating system's generator), domain-separated.
    pub fn generate() -> Self {
        let mut h = Sha256::new();
        h.update(SEED_DOMAIN);
        for _ in 0..4 {
            h.update(uuid::Uuid::new_v4().as_bytes());
        }
        Self {
            key: SigningKey::from_bytes(&h.finalize().into()),
        }
    }

    /// Parse the key file's contents: the unpadded base64url 32-byte seed,
    /// surrounding whitespace ignored.
    pub fn from_encoded(text: &str) -> Result<Self> {
        let seed: [u8; 32] = decode_exact(text.trim())
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| {
                KernelError::InvalidInput(
                    "a verifier key file holds one unpadded base64url 32-byte Ed25519 seed".into(),
                )
            })?;
        Ok(Self {
            key: SigningKey::from_bytes(&seed),
        })
    }

    /// The secret seed, encoded for the key file. Never log it.
    pub fn encode_secret(&self) -> String {
        B64.encode(self.key.to_bytes())
    }

    /// The public key to enroll.
    pub fn public_key(&self) -> String {
        B64.encode(self.key.verifying_key().as_bytes())
    }

    /// The verifier identity.
    pub fn fingerprint(&self) -> String {
        fingerprint_of(&self.key.verifying_key())
    }

    pub fn sign(&self, claims: &ObservationClaims<'_>) -> String {
        B64.encode(self.key.sign(&claims.canonical_bytes()).to_bytes())
    }
}

/// Parse an enrolled public key, refusing non-canonical encodings and weak
/// (small-order) keys.
pub fn parse_public_key(text: &str) -> Result<VerifyingKey> {
    let invalid =
        || KernelError::InvalidInput("public key must be an unpadded base64url Ed25519 key".into());
    let raw: [u8; 32] = decode_exact(text)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(invalid)?;
    let key = VerifyingKey::from_bytes(&raw).map_err(|_| invalid())?;
    if key.is_weak() {
        return Err(invalid());
    }
    Ok(key)
}

pub fn fingerprint_of(key: &VerifyingKey) -> String {
    format!(
        "{FINGERPRINT_PREFIX}{}",
        hex::encode(Sha256::digest(key.as_bytes()))
    )
}

/// What a verifier signs about one authoritative observation. Every field
/// the ledger relies on is covered, so changing any of them after signing
/// breaks the signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationClaims<'a> {
    pub tenant_id: &'a str,
    pub index_version_id: &'a str,
    pub artifact_id: &'a str,
    pub object_key: &'a str,
    pub expected_sha256: &'a str,
    /// The store's object generation, empty when it reports none.
    pub storage_generation: &'a str,
    /// The pin's integrity generation the verifier observed at.
    pub pin_generation: i64,
    /// `match`, `mismatch` or `unavailable`.
    pub outcome: &'a str,
    pub observed_sha256: Option<&'a str>,
    /// The verifier identity: the signing key's fingerprint.
    pub verifier_id: &'a str,
    pub run_id: &'a str,
    pub fetch_id: &'a str,
    /// The fetch start, in microseconds since the Unix epoch.
    pub fetched_at_micros: i64,
    pub repair_attempt_id: Option<&'a str>,
}

impl ObservationClaims<'_> {
    /// Domain-separated, length-prefixed encoding: unambiguous whatever the
    /// field contents.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        fn field(out: &mut Vec<u8>, value: &[u8]) {
            out.extend_from_slice(&(value.len() as u32).to_be_bytes());
            out.extend_from_slice(value);
        }
        fn optional(out: &mut Vec<u8>, value: Option<&str>) {
            match value {
                None => out.push(0),
                Some(v) => {
                    out.push(1);
                    field(out, v.as_bytes());
                }
            }
        }
        let mut out = Vec::with_capacity(512);
        field(&mut out, DOMAIN);
        for text in [
            self.tenant_id,
            self.index_version_id,
            self.artifact_id,
            self.object_key,
            self.expected_sha256,
            self.storage_generation,
        ] {
            field(&mut out, text.as_bytes());
        }
        out.extend_from_slice(&self.pin_generation.to_be_bytes());
        field(&mut out, self.outcome.as_bytes());
        optional(&mut out, self.observed_sha256);
        for text in [self.verifier_id, self.run_id, self.fetch_id] {
            field(&mut out, text.as_bytes());
        }
        out.extend_from_slice(&self.fetched_at_micros.to_be_bytes());
        optional(&mut out, self.repair_attempt_id);
        out
    }
}

/// Verify an observation signature against an enrolled public key. The
/// claims' verifier identity must be that key's fingerprint.
pub fn verify_observation(
    public_key: &str,
    claims: &ObservationClaims<'_>,
    signature: &str,
) -> std::result::Result<(), &'static str> {
    let key = parse_public_key(public_key).map_err(|_| "the enrolled key is malformed")?;
    if fingerprint_of(&key) != claims.verifier_id {
        return Err("the verifier id is not the signing key's fingerprint");
    }
    let raw = decode_exact(signature).ok_or("the signature is malformed")?;
    let signature = Signature::from_slice(&raw).map_err(|_| "the signature is malformed")?;
    key.verify_strict(&claims.canonical_bytes(), &signature)
        .map_err(|_| "the signature does not verify")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims(verifier: &str) -> ObservationClaims<'_> {
        ObservationClaims {
            tenant_id: "t",
            index_version_id: "v",
            artifact_id: "a",
            object_key: "o",
            expected_sha256: "e",
            storage_generation: "",
            pin_generation: 0,
            outcome: "mismatch",
            observed_sha256: Some("x"),
            verifier_id: verifier,
            run_id: "r",
            fetch_id: "f",
            fetched_at_micros: 1,
            repair_attempt_id: None,
        }
    }

    #[test]
    fn a_signature_verifies_only_for_its_key_and_claims() {
        let key = VerifierSigningKey::generate();
        let fp = key.fingerprint();
        assert!(fp.starts_with(FINGERPRINT_PREFIX) && fp.len() == 72);
        let c = claims(&fp);
        let sig = key.sign(&c);
        assert_eq!(verify_observation(&key.public_key(), &c, &sig), Ok(()));

        // Any changed claim, another key, or a forged signature fails.
        let mut changed = c.clone();
        changed.pin_generation = 1;
        assert!(verify_observation(&key.public_key(), &changed, &sig).is_err());
        let mut changed = c.clone();
        changed.tenant_id = "other";
        assert!(verify_observation(&key.public_key(), &changed, &sig).is_err());
        let mut changed = c.clone();
        changed.repair_attempt_id = Some("rep");
        assert!(verify_observation(&key.public_key(), &changed, &sig).is_err());
        let other = VerifierSigningKey::generate();
        assert!(verify_observation(&other.public_key(), &c, &sig).is_err());
        assert!(
            verify_observation(&other.public_key(), &claims(&other.fingerprint()), &sig).is_err()
        );
        // A valid signature by another key over claims naming this key's
        // identity: only the fingerprint binding refuses it.
        let impersonation = other.sign(&c);
        assert_eq!(
            verify_observation(&other.public_key(), &c, &impersonation),
            Err("the verifier id is not the signing key's fingerprint")
        );
        let forged = B64.encode([7u8; 64]);
        assert!(verify_observation(&key.public_key(), &c, &forged).is_err());
    }

    #[test]
    fn keys_round_trip_and_refuse_bad_encodings() {
        let key = VerifierSigningKey::generate();
        let again =
            VerifierSigningKey::from_encoded(&format!("{}\n", key.encode_secret())).unwrap();
        assert_eq!(again.fingerprint(), key.fingerprint());
        assert_ne!(
            VerifierSigningKey::generate().fingerprint(),
            key.fingerprint()
        );
        assert!(!format!("{key:?}").contains(&key.encode_secret()));
        for bad in [
            "",
            "abc",
            &format!("{}=", key.encode_secret()),
            &B64.encode([1u8; 31]),
        ] {
            assert!(VerifierSigningKey::from_encoded(bad).is_err(), "{bad:?}");
        }
        assert!(parse_public_key(&key.public_key()).is_ok());
        // The identity point is a weak key.
        let mut identity = [0u8; 32];
        identity[0] = 1;
        assert!(parse_public_key(&B64.encode(identity)).is_err());
        assert!(parse_public_key("not base64!").is_err());
    }
}
