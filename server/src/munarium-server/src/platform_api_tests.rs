// SPDX-License-Identifier: Apache-2.0
use super::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use ed25519_dalek::{Signer, SigningKey};
use serde_json::json;

fn signer() -> SigningKey {
    let mut bytes = [0; 32];
    bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    SigningKey::from_bytes(&bytes)
}
fn enrollment(key: &SigningKey) -> AuthorityConfig {
    AuthorityConfig {
        deployment: "test".into(),
        tenant: "tenant-test".into(),
        audience: "svc-server".into(),
        epoch: 1,
        bootstrap_keys: BTreeMap::from([(
            "root".into(),
            AuthorityKey {
                public_key: B64.encode(key.verifying_key().as_bytes()),
                issuer: "test-issuer".into(),
                presenter: "operator".into(),
                subjects: BTreeSet::from(["human-one".into()]),
            },
        )]),
    }
}
fn peer() -> AuthenticatedPeer {
    AuthenticatedPeer(Peer {
        service: "operator".into(),
        tenants: BTreeSet::from(["tenant-test".into()]),
        scopes: BTreeSet::from(["govern".into(), "read".into()]),
    })
}
fn request(
    key: &SigningKey,
    state: &AuthorityState,
    artifact: &GovernanceArtifact,
    nonce: &str,
) -> String {
    let header = json!({"alg":"Ed25519","kid":"root","typ":"munarium-authority+jws"});
    let payload = json!({"schema_version":1,"deployment":"test","tenant":"tenant-test","audience":"svc-server",
        "issuer":"test-issuer","subject":"human-one","subject_kind":"human","action":"install-governance",
        "artifact_digest":artifact.digest().unwrap(),"expected_revision":state.revision,"expected_head":state.head,
        "epoch":1,"nonce":nonce,"iat":990,"nbf":990,"exp":1050});
    let signed = format!(
        "{}.{}",
        B64.encode(serde_json::to_vec(&header).unwrap()),
        B64.encode(serde_json::to_vec(&payload).unwrap())
    );
    format!(
        "{signed}.{}",
        B64.encode(key.sign(signed.as_bytes()).to_bytes())
    )
}
async fn fixture() -> (
    tempfile::TempDir,
    TenantAuthority,
    SigningKey,
    AuthorityState,
) {
    let directory = tempfile::tempdir().unwrap();
    let key = signer();
    let store = Arc::new(MemAuthorityStore::new("tenant-test"));
    let state = store.enroll(enrollment(&key)).await.unwrap();
    let checkpoint = directory.path().join("checkpoint.json");
    let mut file = File::create(&checkpoint).unwrap();
    write_checkpoint(
        &mut file,
        &Checkpoint {
            fence: state.fence(),
            revision: state.revision.clone(),
            prepared: None,
        },
    )
    .unwrap();
    (
        directory,
        TenantAuthority {
            store,
            checkpoint,
            serial: tokio::sync::Mutex::new(()),
        },
        key,
        state,
    )
}
fn artifact() -> GovernanceArtifact {
    GovernanceArtifact {
        schema_version: 1,
        bindings: BTreeMap::new(),
        retire_bootstrap: false,
        successor_keys: BTreeMap::new(),
    }
}

#[tokio::test]
async fn checkpoint_precedes_activation_and_fences_restored_database() {
    let (_directory, authority, key, initial) = fixture().await;
    let artifact = artifact();
    let signed = request(&key, &initial, &artifact, "first");
    let receipt = authority
        .apply(&peer(), &signed, &artifact, 1000)
        .await
        .unwrap();
    assert_eq!(receipt.head, 1);
    assert_eq!(
        authority
            .apply(&peer(), &signed, &artifact, 2000)
            .await
            .unwrap(),
        receipt
    );
    let mut file = File::open(&authority.checkpoint).unwrap();
    let checkpoint = read_checkpoint(&mut file).unwrap();
    assert_eq!(checkpoint.fence.minimum_head, 1);
    assert_eq!(checkpoint.revision, receipt.revision);
    assert!(checkpoint.prepared.is_none());
    let restored = Arc::new(MemAuthorityStore::new("tenant-test"));
    restored.enroll(initial.config.clone()).await.unwrap();
    let restored = TenantAuthority {
        store: restored,
        checkpoint: authority.checkpoint.clone(),
        serial: tokio::sync::Mutex::new(()),
    };
    assert!(restored.snapshot().await.is_err());
    assert!(restored
        .apply(&peer(), &signed, &artifact, 1000)
        .await
        .is_err());
}

#[tokio::test]
async fn crash_after_preparing_only_allows_exact_transition_recovery() {
    let (_directory, authority, key, initial) = fixture().await;
    let artifact = artifact();
    let signed = request(&key, &initial, &artifact, "prepared");
    let admission = AuthorityAdmission {
        tenant: "tenant-test".into(),
        peer_service: "operator".into(),
        now: 1000,
        fence: initial.fence(),
    };
    let (next, receipt) = transition(&initial, &admission, &signed, &artifact, None).unwrap();
    let mut file = locked_checkpoint(&authority.checkpoint).unwrap();
    let mut checkpoint = read_checkpoint(&mut file).unwrap();
    checkpoint.prepared = Some(Prepared {
        attestation_digest: receipt.attestation_digest.clone(),
        next_revision: next.revision.clone(),
        next_fence: next.fence(),
    });
    write_checkpoint(&mut file, &checkpoint).unwrap();
    drop(file);
    assert!(authority.snapshot().await.is_err());
    let other = request(&key, &initial, &artifact, "other");
    assert!(authority
        .apply(&peer(), &other, &artifact, 1000)
        .await
        .is_err());
    assert_eq!(authority.store.snapshot().await.unwrap(), initial);
    assert_eq!(
        authority
            .apply(&peer(), &signed, &artifact, 1000)
            .await
            .unwrap(),
        receipt
    );
    assert_eq!(authority.snapshot().await.unwrap(), next);
}

#[tokio::test]
async fn crash_after_database_commit_reconciles_without_reactivation() {
    let (_directory, authority, key, initial) = fixture().await;
    let artifact = artifact();
    let signed = request(&key, &initial, &artifact, "committed");
    let admission = AuthorityAdmission {
        tenant: "tenant-test".into(),
        peer_service: "operator".into(),
        now: 1000,
        fence: initial.fence(),
    };
    let (next, receipt) = transition(&initial, &admission, &signed, &artifact, None).unwrap();
    let mut file = locked_checkpoint(&authority.checkpoint).unwrap();
    let mut checkpoint = read_checkpoint(&mut file).unwrap();
    checkpoint.prepared = Some(Prepared {
        attestation_digest: receipt.attestation_digest.clone(),
        next_revision: next.revision.clone(),
        next_fence: next.fence(),
    });
    write_checkpoint(&mut file, &checkpoint).unwrap();
    drop(file);
    authority
        .store
        .apply(&admission, &signed, &artifact)
        .await
        .unwrap();
    assert_eq!(authority.snapshot().await.unwrap(), next);
    assert_eq!(
        authority
            .apply(&peer(), &signed, &artifact, 2000)
            .await
            .unwrap(),
        receipt
    );
    assert_eq!(authority.snapshot().await.unwrap().head, 1);
}

#[tokio::test]
async fn missing_partial_or_locked_checkpoint_never_activates() {
    let (_directory, authority, key, initial) = fixture().await;
    let artifact = artifact();
    let signed = request(&key, &initial, &artifact, "checkpoint-fault");
    let locked = locked_checkpoint(&authority.checkpoint).unwrap();
    assert!(authority
        .apply(&peer(), &signed, &artifact, 1000)
        .await
        .is_err());
    drop(locked);
    std::fs::write(&authority.checkpoint, b"{\"fence\":").unwrap();
    assert!(authority.snapshot().await.is_err());
    assert!(authority
        .apply(&peer(), &signed, &artifact, 1000)
        .await
        .is_err());
    std::fs::remove_file(&authority.checkpoint).unwrap();
    assert!(authority
        .apply(&peer(), &signed, &artifact, 1000)
        .await
        .is_err());
    assert_eq!(authority.store.snapshot().await.unwrap(), initial);
}

#[test]
fn every_documented_governing_mutation_is_outside_ordinary_writer_inventory() {
    let spec = serde_json::to_value(crate::openapi::doc()).unwrap();
    let mut refused = 0;
    for (path, methods) in spec["paths"].as_object().unwrap() {
        for method in methods
            .as_object()
            .unwrap()
            .keys()
            .filter(|m| ["post", "put", "patch", "delete"].contains(&m.as_str()))
        {
            if !crate::platform_tls::ordinary_write(path) && !path.starts_with("/v1/platform/") {
                refused += 1;
            }
            if path.contains("governance")
                || path.contains("activate-index")
                || path.contains("runbooks")
                || path.contains("shapes")
            {
                assert!(
                    !crate::platform_tls::ordinary_write(path),
                    "{method} {path}"
                );
            }
        }
    }
    assert!(refused >= 20);
    assert!(!crate::platform_tls::ordinary_write(
        "/v1/future-governing-operation"
    ));
}

#[test]
fn platform_wire_contract_uses_signed_identity_instead_of_legacy_uid() {
    let spec = serde_json::to_value(crate::openapi::doc()).unwrap();
    for path in [
        "/v1/platform/{tenant}/authority",
        "/v1/platform/{tenant}/records",
        "/v1/platform/{tenant}/activation",
    ] {
        assert!(spec["paths"][path].get("parameters").is_none());
        for method in ["get", "post"] {
            if let Some(operation) = spec["paths"][path].get(method) {
                assert_eq!(
                    operation["security"],
                    serde_json::json!([{"platformMtls":[]}])
                );
            }
        }
    }
    assert_eq!(
        spec["paths"]["/v1/versions"]["parameters"][0]["name"],
        "X-Munarium-Uid"
    );
}
