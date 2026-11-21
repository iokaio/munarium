// SPDX-License-Identifier: Apache-2.0
//! Gated S3 smoke against an isolated object store.
//!
//! Bring up the target and bucket once:
//!
//! ```powershell
//! docker compose --profile s3 up -d minio
//! docker compose exec minio mc alias set local http://127.0.0.1:9000 minioadmin minioadmin
//! docker compose exec minio mc mb --ignore-existing local/sources
//! $env:MUNARIUM_TEST_S3_ENDPOINT = 'http://127.0.0.1:9000'
//! cargo test -p munarium-store-objects --test s3_integration
//! ```
//!
//! Unset `MUNARIUM_TEST_S3_ENDPOINT` and the test skips vacuously, the same
//! contract as the pg-gated integration tests in munarium-retrieval-pg.

use munarium_core::sources::{SourceKey, SourceStore};
use munarium_core::KernelError;
use munarium_datastore::store::ArtifactStore;
use munarium_store_objects::{ObjectArtifactStore, ObjectSourceStore, S3Config};
use object_store::{path::Path, ObjectStoreExt};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn test_store() -> Option<ObjectSourceStore> {
    let Ok(endpoint) = std::env::var("MUNARIUM_TEST_S3_ENDPOINT") else {
        eprintln!("skipping live S3: MUNARIUM_TEST_S3_ENDPOINT is not set");
        return None;
    };
    Some(
        ObjectSourceStore::s3(S3Config {
            bucket: env_or("MUNARIUM_TEST_S3_BUCKET", "sources"),
            region: env_or("MUNARIUM_TEST_S3_REGION", "us-east-1"),
            endpoint: Some(endpoint),
            force_path_style: true,
            access_key_id: Some(env_or("MUNARIUM_TEST_S3_ACCESS_KEY", "minioadmin")),
            secret_access_key: Some(env_or("MUNARIUM_TEST_S3_SECRET_KEY", "minioadmin")),
        })
        .expect("store"),
    )
}

#[tokio::test]
async fn s3_integration_roundtrip() {
    let Some(store) = test_store() else {
        return;
    };

    let key = SourceKey::new("it-tenant", "smoke/nested/doc.md", "hash").expect("key");
    // Clean slate even after an interrupted previous run.
    store.delete(&key).await.expect("pre-delete");
    assert!(!store.exists(&key).await.expect("exists before put"));

    let uri = store
        .put(&key, "text/markdown", b"s3 smoke bytes")
        .await
        .expect("put (is the bucket created? mc mb local/sources)");
    assert!(uri.starts_with("s3://"), "recorded URI: {uri}");
    assert!(
        !uri.contains('?'),
        "recorded URI must carry no credential: {uri}"
    );

    assert!(store.exists(&key).await.expect("exists after put"));
    assert_eq!(store.get(&key).await.expect("get"), b"s3 smoke bytes");

    store.delete(&key).await.expect("delete");
    match store.get(&key).await {
        Err(KernelError::NotFound { .. }) => {}
        other => panic!("expected NotFound after delete, got {other:?}"),
    }
    // Idempotent by contract.
    store.delete(&key).await.expect("second delete");
}

#[tokio::test]
async fn s3_artifact_reads_bind_bytes_to_storage_generation() {
    let Some(store) = test_store() else {
        return;
    };
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let prefix = format!("artifact-generation-smoke/{suffix}");
    let artifacts =
        ObjectArtifactStore::from_source_store(&store, &prefix).expect("artifact store");
    let component = "verified-component";
    let result = tokio::task::spawn_blocking(move || {
        artifacts.put_component(component, b"published component")?;
        let published = artifacts.get_component_with_generation(component)?;
        let refetched = artifacts.get_component_with_generation(component)?;
        artifacts.put_component(component, b"replacement component")?;
        let replaced = artifacts.get_component_with_generation(component)?;
        Ok::<_, munarium_datastore::Error>((published, refetched, replaced))
    })
    .await;
    store
        .object_store()
        .delete(&Path::from(format!("{prefix}/{component}")))
        .await
        .expect("cleanup");
    let (published, refetched, replaced) = result.expect("blocking read").expect("S3 artifact");
    assert_eq!(published.bytes, b"published component");
    assert!(published.generation.is_some());
    assert_eq!(published, refetched);
    assert_eq!(replaced.bytes, b"replacement component");
    assert!(replaced.generation.is_some());
    assert_ne!(published.generation, replaced.generation);
}
