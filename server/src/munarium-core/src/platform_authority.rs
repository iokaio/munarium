// SPDX-License-Identifier: Apache-2.0
//! Separate platform governance authority. Storage commits the verified transition,
//! nonce and receipt together; ordinary memory writers do not implement this interface.
use crate::{platform::canonical_record, platform::record_digest, KernelError, Result};
use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const MAX_INTEGER: u64 = 9_007_199_254_740_991;

fn denied() -> KernelError {
    KernelError::Forbidden("platform governance authority refused".into())
}
fn invalid() -> KernelError {
    KernelError::InvalidInput("invalid platform authority record".into())
}
fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b":._/-".contains(&b))
}
fn digest<T: Serialize>(domain: &str, value: &T) -> Result<String> {
    record_digest(domain, &serde_json::to_value(value).map_err(|_| invalid())?)
}
fn decode(s: &str) -> Result<Vec<u8>> {
    let bytes = B64.decode(s).map_err(|_| denied())?;
    if B64.encode(&bytes) != s {
        return Err(denied());
    }
    Ok(bytes)
}

/// Operator-enrolled, non-agent governance signer. Public material only.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthorityKey {
    pub public_key: String,
    pub issuer: String,
    /// Actual mTLS service identity allowed to present this signer's assertions.
    pub presenter: String,
    pub subjects: BTreeSet<String>,
}
impl AuthorityKey {
    fn validate(&self) -> Result<()> {
        let raw: [u8; 32] = decode(&self.public_key)?
            .try_into()
            .map_err(|_| invalid())?;
        let key = VerifyingKey::from_bytes(&raw).map_err(|_| invalid())?;
        if key.is_weak()
            || !identifier(&self.issuer)
            || !identifier(&self.presenter)
            || self.subjects.is_empty()
            || self.subjects.len() > 128
            || self.subjects.iter().any(|s| !identifier(s))
        {
            return Err(invalid());
        }
        Ok(())
    }
}

/// Initial enrollment comes from operator configuration, never an HTTP request.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthorityConfig {
    pub deployment: String,
    pub tenant: String,
    pub audience: String,
    pub epoch: u64,
    pub bootstrap_keys: BTreeMap<String, AuthorityKey>,
}
impl AuthorityConfig {
    pub fn validate(&self) -> Result<()> {
        if !identifier(&self.deployment)
            || !identifier(&self.tenant)
            || !identifier(&self.audience)
            || self.epoch == 0
            || self.epoch > MAX_INTEGER
        {
            return Err(invalid());
        }
        validate_keys(&self.bootstrap_keys)
    }
}
fn validate_keys(keys: &BTreeMap<String, AuthorityKey>) -> Result<()> {
    if keys.is_empty() || keys.len() > 32 {
        return Err(invalid());
    }
    let mut public = BTreeSet::new();
    for (id, key) in keys {
        if !identifier(id) || !public.insert(&key.public_key) {
            return Err(invalid());
        }
        key.validate()?;
    }
    Ok(())
}

/// Whole immutable governing artifact. Bindings are validated by their owning service
/// before use; unknown or incompatible binding kinds cannot confer permission.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GovernanceArtifact {
    pub schema_version: u8,
    pub bindings: BTreeMap<String, Value>,
    pub retire_bootstrap: bool,
    pub successor_keys: BTreeMap<String, AuthorityKey>,
}
impl GovernanceArtifact {
    pub fn digest(&self) -> Result<String> {
        if self.schema_version != 1
            || self.bindings.len() > 256
            || self.bindings.keys().any(|k| !identifier(k))
        {
            return Err(invalid());
        }
        if self.retire_bootstrap {
            validate_keys(&self.successor_keys)?;
        } else if !self.successor_keys.is_empty() {
            return Err(invalid());
        }
        digest("munarium:governance-artifact:v1", self)
    }
}

/// Signed transition payload. Its type cannot carry delegation or an agent origin.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityAttestation {
    pub schema_version: u8,
    pub deployment: String,
    pub tenant: String,
    pub audience: String,
    pub issuer: String,
    pub subject: String,
    pub subject_kind: String,
    pub action: String,
    pub artifact_digest: String,
    pub expected_revision: String,
    pub expected_head: u64,
    pub epoch: u64,
    pub nonce: String,
    pub iat: i64,
    pub nbf: i64,
    pub exp: i64,
}

/// Context from authenticated transport and current trusted restore state.
/// Deliberately not deserializable from an ordinary request.
pub struct AuthorityAdmission {
    pub tenant: String,
    pub peer_service: String,
    pub now: i64,
    pub fence: AuthorityFence,
}

/// Independently retained minimum authority state; absence/unavailability fails closed.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthorityFence {
    pub deployment: String,
    pub tenant: String,
    pub epoch: u64,
    pub minimum_head: u64,
    pub bootstrap_retired: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AuthorityState {
    pub config: AuthorityConfig,
    pub head: u64,
    pub revision: String,
    pub bootstrap_retired: bool,
    pub governing_keys: BTreeMap<String, AuthorityKey>,
    pub artifact: Option<GovernanceArtifact>,
}
impl AuthorityState {
    pub fn enrolled(config: AuthorityConfig) -> Result<Self> {
        config.validate()?;
        let revision = digest("munarium:authority-enrollment:v1", &config)?;
        Ok(Self {
            config,
            head: 0,
            revision,
            bootstrap_retired: false,
            governing_keys: BTreeMap::new(),
            artifact: None,
        })
    }
    pub fn fence(&self) -> AuthorityFence {
        AuthorityFence {
            deployment: self.config.deployment.clone(),
            tenant: self.config.tenant.clone(),
            epoch: self.config.epoch,
            minimum_head: self.head,
            bootstrap_retired: self.bootstrap_retired,
        }
    }
    pub fn check_fence(&self, fence: &AuthorityFence) -> Result<()> {
        if fence.deployment != self.config.deployment
            || fence.tenant != self.config.tenant
            || fence.epoch != self.config.epoch
            || self.head < fence.minimum_head
            || (fence.bootstrap_retired && !self.bootstrap_retired)
        {
            return Err(denied());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthorityReceipt {
    pub deployment: String,
    pub tenant: String,
    pub epoch: u64,
    pub nonce: String,
    pub attestation_digest: String,
    pub artifact_digest: String,
    pub prior_revision: String,
    pub revision: String,
    pub head: u64,
    pub subject: String,
    pub issuer: String,
    pub key_id: String,
    pub presenter: String,
    pub bootstrap_retired: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    alg: String,
    kid: String,
    typ: String,
}

/// Verified transition for atomic storage. Verification must run under the same lock
/// as persistence. The returned state must never be installed without its receipt.
pub fn transition(
    state: &AuthorityState,
    admission: &AuthorityAdmission,
    signed: &str,
    artifact: &GovernanceArtifact,
    prior: Option<&AuthorityReceipt>,
) -> Result<(AuthorityState, AuthorityReceipt)> {
    state.check_fence(&admission.fence)?;
    if admission.tenant != state.config.tenant || signed.len() > 16_384 {
        return Err(denied());
    }
    let parts: Vec<_> = signed.split('.').collect();
    if parts.len() != 3 {
        return Err(denied());
    }
    let header: Header =
        serde_json::from_value(canonical_record(&decode(parts[0])?)?).map_err(|_| denied())?;
    if header.alg != "Ed25519" || header.typ != "munarium-authority+jws" {
        return Err(denied());
    }
    let payload: AuthorityAttestation =
        serde_json::from_value(canonical_record(&decode(parts[1])?)?).map_err(|_| denied())?;
    let attestation_digest = digest(
        "munarium:authority-attestation:v1",
        &serde_json::json!({"jws":signed}),
    )?;
    let artifact_digest = artifact.digest()?;
    // A receipt is an authenticated read of a completed transition, not renewed power.
    // Its original enrolled presenter is still required, even after signer retirement.
    if let Some(prior) = prior {
        if prior.nonce != payload.nonce
            || prior.tenant != admission.tenant
            || prior.presenter != admission.peer_service
        {
            return Err(denied());
        }
        if prior.attestation_digest != attestation_digest
            || prior.artifact_digest != artifact_digest
        {
            return Err(KernelError::IdempotencyMismatch);
        }
        return Ok((state.clone(), prior.clone()));
    }
    let keys = if state.bootstrap_retired {
        &state.governing_keys
    } else {
        &state.config.bootstrap_keys
    };
    let key = keys.get(&header.kid).ok_or_else(denied)?;
    let public: [u8; 32] = decode(&key.public_key)?.try_into().map_err(|_| denied())?;
    VerifyingKey::from_bytes(&public)
        .map_err(|_| denied())?
        .verify_strict(
            format!("{}.{}", parts[0], parts[1]).as_bytes(),
            &Signature::from_slice(&decode(parts[2])?).map_err(|_| denied())?,
        )
        .map_err(|_| denied())?;
    if payload.schema_version != 1
        || payload.deployment != state.config.deployment
        || payload.tenant != state.config.tenant
        || payload.audience != state.config.audience
        || payload.issuer != key.issuer
        || !key.subjects.contains(&payload.subject)
        || payload.subject_kind != "human"
        || payload.action != "install-governance"
        || payload.epoch != state.config.epoch
        || payload.artifact_digest != artifact_digest
        || key.presenter != admission.peer_service
        || !identifier(&payload.nonce)
        || payload.iat < 0
        || payload.iat > admission.now
        || payload.nbf < payload.iat
        || payload.nbf > admission.now
        || payload.exp <= admission.now
        || payload.exp <= payload.nbf
        || payload.exp.checked_sub(payload.iat).is_none_or(|n| n > 300)
        || payload.exp as u64 > MAX_INTEGER
    {
        return Err(denied());
    }
    if payload.expected_head != state.head {
        return Err(KernelError::HeadConflict {
            expected: payload.expected_head,
            actual: state.head,
        });
    }
    if payload.expected_revision != state.revision {
        return Err(denied());
    }
    // Retired bootstrap cannot reappear, including under a different key identifier.
    if state.bootstrap_retired && !artifact.retire_bootstrap {
        return Err(denied());
    }
    if artifact.successor_keys.values().any(|k| {
        state
            .config
            .bootstrap_keys
            .values()
            .any(|old| old.public_key == k.public_key)
    }) {
        return Err(denied());
    }
    let head = state
        .head
        .checked_add(1)
        .filter(|n| *n <= MAX_INTEGER)
        .ok_or_else(invalid)?;
    let mut next = state.clone();
    next.head = head;
    next.bootstrap_retired |= artifact.retire_bootstrap;
    if next.bootstrap_retired {
        next.governing_keys = artifact.successor_keys.clone();
    }
    next.artifact = Some(artifact.clone());
    next.revision = digest(
        "munarium:authority-revision:v1",
        &serde_json::json!({
            "prior":state.revision, "head":head, "attestation":attestation_digest, "artifact":artifact_digest
        }),
    )?;
    let receipt = AuthorityReceipt {
        deployment: state.config.deployment.clone(),
        tenant: state.config.tenant.clone(),
        epoch: state.config.epoch,
        nonce: payload.nonce,
        attestation_digest,
        artifact_digest,
        prior_revision: state.revision.clone(),
        revision: next.revision.clone(),
        head,
        subject: payload.subject,
        issuer: payload.issuer,
        key_id: header.kid,
        presenter: admission.peer_service.clone(),
        bootstrap_retired: next.bootstrap_retired,
    };
    Ok((next, receipt))
}

/// Extract the bounded nonce for indexed lookup, without treating it as verified identity.
pub fn untrusted_nonce(signed: &str) -> Result<String> {
    if signed.len() > 16_384 {
        return Err(invalid());
    }
    let parts: Vec<_> = signed.split('.').collect();
    if parts.len() != 3 {
        return Err(invalid());
    }
    let payload: AuthorityAttestation =
        serde_json::from_value(canonical_record(&decode(parts[1])?)?).map_err(|_| invalid())?;
    if !identifier(&payload.nonce) {
        return Err(invalid());
    }
    Ok(payload.nonce)
}

#[async_trait]
pub trait AuthorityStore: Send + Sync {
    /// Enroll once, or confirm identical operator configuration. Never reset state.
    async fn enroll(&self, config: AuthorityConfig) -> Result<AuthorityState>;
    async fn snapshot(&self) -> Result<AuthorityState>;
    async fn apply(
        &self,
        admission: &AuthorityAdmission,
        signed: &str,
        artifact: &GovernanceArtifact,
    ) -> Result<AuthorityReceipt>;
}
