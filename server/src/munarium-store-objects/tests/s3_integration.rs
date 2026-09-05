// SPDX-License-Identifier: Apache-2.0
//! Gated MinIO smoke for the s3 backend — network-free CI never runs it.
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
use munarium_store_objects::{ObjectSourceStore, S3Config};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[tokio::test]
async fn s3_integration_roundtrip() {
    let Ok(endpoint) = std::env::var("MUNARIUM_TEST_S3_ENDPOINT") else {
        eprintln!("skipping s3_integration_roundtrip: MUNARIUM_TEST_S3_ENDPOINT is not set");
        return;
    };
    let store = ObjectSourceStore::s3(S3Config {
        bucket: env_or("MUNARIUM_TEST_S3_BUCKET", "sources"),
        region: env_or("MUNARIUM_TEST_S3_REGION", "us-east-1"),
        endpoint: Some(endpoint),
        force_path_style: true,
        access_key_id: Some(env_or("MUNARIUM_TEST_S3_ACCESS_KEY", "minioadmin")),
        secret_access_key: Some(env_or("MUNARIUM_TEST_S3_SECRET_KEY", "minioadmin")),
    })
    .expect("store");

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
