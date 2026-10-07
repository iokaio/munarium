// SPDX-License-Identifier: Apache-2.0
//! Shared behavioral contract; signing material is generated per test, never retained.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use ed25519_dalek::{Signer, SigningKey};
use munarium_core::platform_authority::*;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub fn key() -> SigningKey {
    let mut seed = [0u8; 32];
    seed[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    seed[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    SigningKey::from_bytes(&seed)
}
pub fn trusted(key: &SigningKey) -> AuthorityKey {
    AuthorityKey {
        public_key: B64.encode(key.verifying_key().as_bytes()),
        issuer: "operator-issuer".into(),
        presenter: "operator-enrollment".into(),
        subjects: BTreeSet::from(["enrolled-operator".into()]),
    }
}
pub fn config(tenant: &str, key: &SigningKey) -> AuthorityConfig {
    AuthorityConfig {
        deployment: "test-deployment".into(),
        tenant: tenant.into(),
        audience: "svc-server".into(),
        epoch: 1,
        bootstrap_keys: BTreeMap::from([("bootstrap-key".into(), trusted(key))]),
    }
}
pub fn artifact() -> GovernanceArtifact {
    GovernanceArtifact {
        schema_version: 1,
        bindings: BTreeMap::from([("decision-policy".into(), json!({"mode":"advise"}))]),
        retire_bootstrap: false,
        successor_keys: BTreeMap::new(),
    }
}
pub fn admission(state: &AuthorityState) -> AuthorityAdmission {
    AuthorityAdmission {
        tenant: state.config.tenant.clone(),
        peer_service: "operator-enrollment".into(),
        now: 1000,
        fence: state.fence(),
    }
}
pub fn payload(state: &AuthorityState, artifact: &GovernanceArtifact, nonce: &str) -> Value {
    json!({"schema_version":1,"deployment":state.config.deployment,"tenant":state.config.tenant,
        "audience":state.config.audience,"issuer":"operator-issuer","subject":"enrolled-operator",
        "subject_kind":"human","action":"install-governance","artifact_digest":artifact.digest().unwrap(),
        "expected_revision":state.revision,"expected_head":state.head,"epoch":state.config.epoch,
        "nonce":nonce,"iat":990,"nbf":990,"exp":1050})
}
pub fn sign(key: &SigningKey, kid: &str, payload: &Value) -> String {
    let header = json!({"alg":"Ed25519","kid":kid,"typ":"munarium-authority+jws"});
    let message = format!(
        "{}.{}",
        B64.encode(serde_json::to_vec(&header).unwrap()),
        B64.encode(serde_json::to_vec(payload).unwrap())
    );
    format!(
        "{message}.{}",
        B64.encode(key.sign(message.as_bytes()).to_bytes())
    )
}
pub async fn contract(store: &dyn AuthorityStore, tenant: &str) {
    assert!(store.snapshot().await.is_err());
    let signer = key();
    let config = config(tenant, &signer);
    let initial = store.enroll(config.clone()).await.unwrap();
    assert_eq!(store.enroll(config.clone()).await.unwrap(), initial);
    let mut replacement = config.clone();
    replacement.epoch += 1;
    assert!(store.enroll(replacement).await.is_err());
    let artifact = artifact();
    let admission = admission(&initial);
    let valid = payload(&initial, &artifact, "first");
    // Signed yet unauthorized assertions must leave both state and nonce unconsumed.
    for (field, value) in [
        ("subject_kind", json!("agent")),
        ("subject", json!("unenrolled-human")),
        ("tenant", json!("other-tenant")),
        ("audience", json!("svc-registry")),
        ("deployment", json!("other-deployment")),
        ("issuer", json!("other-issuer")),
        ("epoch", json!(2)),
        ("exp", json!(1000)),
        ("iat", json!(1001)),
        ("nbf", json!(1001)),
        ("exp", json!(1400)),
        ("expected_head", json!(1)),
        ("expected_revision", json!("sha256:wrong")),
        ("delegation", json!([])),
    ] {
        let mut changed = valid.clone();
        changed[field] = value;
        assert!(
            store
                .apply(
                    &admission,
                    &sign(&signer, "bootstrap-key", &changed),
                    &artifact
                )
                .await
                .is_err(),
            "{field}"
        );
        assert_eq!(store.snapshot().await.unwrap(), initial);
    }
    let forged = sign(&key(), "bootstrap-key", &valid);
    assert!(store.apply(&admission, &forged, &artifact).await.is_err());
    assert!(store
        .apply(&admission, &sign(&signer, "unknown", &valid), &artifact)
        .await
        .is_err());
    let signed = sign(&signer, "bootstrap-key", &valid);
    let mut wrong_peer = super_admission(&initial);
    wrong_peer.peer_service = "svc-harness".into();
    assert!(store.apply(&wrong_peer, &signed, &artifact).await.is_err());
    let (a, b) = tokio::join!(
        store.apply(&admission, &signed, &artifact),
        store.apply(&admission, &signed, &artifact)
    );
    let receipt = a.unwrap();
    assert_eq!(receipt, b.unwrap());
    let current = store.snapshot().await.unwrap();
    assert_eq!(current.head, 1);
    assert_eq!(current.artifact.as_ref(), Some(&artifact));
    // Same nonce with different signed bytes conflicts, including after clock expiry.
    let mut changed = valid.clone();
    changed["exp"] = json!(1051);
    assert!(store
        .apply(
            &admission,
            &sign(&signer, "bootstrap-key", &changed),
            &artifact
        )
        .await
        .is_err());
    let mut stale = valid.clone();
    stale["nonce"] = json!("stale");
    assert!(store
        .apply(
            &admission,
            &sign(&signer, "bootstrap-key", &stale),
            &artifact
        )
        .await
        .is_err());
    let mut after_expiry = super_admission(&current);
    after_expiry.now = 2000;
    assert_eq!(
        store
            .apply(&after_expiry, &signed, &artifact)
            .await
            .unwrap(),
        receipt
    );
    // Exactly one of distinct racing transitions from one revision can commit.
    let x = sign(
        &signer,
        "bootstrap-key",
        &payload(&current, &artifact, "race-a"),
    );
    let y = sign(
        &signer,
        "bootstrap-key",
        &payload(&current, &artifact, "race-b"),
    );
    let (x, y) = tokio::join!(
        store.apply(&admission, &x, &artifact),
        store.apply(&admission, &y, &artifact)
    );
    assert_eq!(usize::from(x.is_ok()) + usize::from(y.is_ok()), 1);
    let current = store.snapshot().await.unwrap();
    assert_eq!(current.head, 2);
    let successor = key();
    let mut retirement = artifact.clone();
    retirement.retire_bootstrap = true;
    retirement
        .successor_keys
        .insert("successor".into(), trusted(&successor));
    let signed_retirement = sign(
        &signer,
        "bootstrap-key",
        &payload(&current, &retirement, "retire"),
    );
    let retired_receipt = store
        .apply(&admission, &signed_retirement, &retirement)
        .await
        .unwrap();
    let retired = store.snapshot().await.unwrap();
    assert!(retired.bootstrap_retired);
    assert_eq!(retired.governing_keys, retirement.successor_keys);
    assert_eq!(store.enroll(config).await.unwrap(), retired);
    assert_eq!(
        store
            .apply(&super_admission(&retired), &signed_retirement, &retirement)
            .await
            .unwrap(),
        retired_receipt
    );
    let retired_key = sign(
        &signer,
        "bootstrap-key",
        &payload(&retired, &retirement, "retired-key"),
    );
    assert!(store
        .apply(&admission, &retired_key, &retirement)
        .await
        .is_err());
    let undo = sign(
        &successor,
        "successor",
        &payload(&retired, &artifact, "undo-retirement"),
    );
    assert!(store.apply(&admission, &undo, &artifact).await.is_err());
    let mut reinstated = retirement.clone();
    reinstated
        .successor_keys
        .insert("renamed-bootstrap".into(), trusted(&signer));
    let reenable = sign(
        &successor,
        "successor",
        &payload(&retired, &reinstated, "reenable"),
    );
    assert!(store
        .apply(&admission, &reenable, &reinstated)
        .await
        .is_err());
    let next = sign(
        &successor,
        "successor",
        &payload(&retired, &retirement, "successor-write"),
    );
    let mut restored = super_admission(&retired);
    restored.fence.minimum_head += 1;
    assert!(store.apply(&restored, &next, &retirement).await.is_err());
    let mut foreign = super_admission(&retired);
    foreign.fence.tenant = "other-tenant".into();
    assert!(store.apply(&foreign, &next, &retirement).await.is_err());
    assert!(initial.check_fence(&retired.fence()).is_err());
    assert!(store
        .apply(&super_admission(&retired), &next, &retirement)
        .await
        .is_ok());
    assert_eq!(store.snapshot().await.unwrap().head, 4);
}
fn super_admission(state: &AuthorityState) -> AuthorityAdmission {
    admission(state)
}
