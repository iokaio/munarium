// SPDX-License-Identifier: Apache-2.0
//! Cloud and filesystem `SourceStore` backends over the Apache Arrow
//! [`object_store`] crate: AWS S3, Azure Blob, Google Cloud Storage, and the
//! local filesystem behind one adapter. This replaces the hand-rolled
//! `munarium-store-az` REST crate — `object_store` covers the same four verbs on
//! rustls with no cloud SDKs, and adds the other clouds for free.
//!
//! Contracts carried over from the predecessors, all test-enforced here:
//!
//! - **Recorded URIs never carry a credential.** The URI written to the
//!   `sources` row is constructed from the bucket/endpoint topology alone,
//!   never from a signed request.
//! - **Azure stays byte-identical.** `backend_id()` remains `"az"` and the
//!   recorded URI remains
//!   `https://{account}.blob.core.windows.net/{container}/{percent-encoded blob name}`,
//!   so rows written by `munarium-store-az` and rows written here are
//!   indistinguishable.
//! - **Credentials are ambient by default.** S3 walks the standard AWS chain
//!   (env vars, web-identity federation, IMDS instance profile); GCS walks
//!   `GOOGLE_APPLICATION_CREDENTIALS` then the metadata server; Azure falls
//!   back to IMDS managed identity (the same 169.254.169.254 endpoint the
//!   hand-rolled crate used, proven live on Container Apps) unless a SAS is
//!   supplied. Static credentials exist only for off-cloud tooling (MinIO,
//!   Azurite, CI) and resolve through the server's credential seam upstream.
//!
//! The in-memory backend intentionally does NOT live here:
//! `munarium_store_mem::MemSourceStore` is zero-dep and stays the `mem` backend;
//! `object_store::memory::InMemory` appears only in this crate's tests, as the
//! seam behind [`ObjectSourceStore::from_parts`].

pub mod artifacts;
pub use artifacts::ObjectArtifactStore;

use async_trait::async_trait;
use munarium_core::sources::{SourceKey, SourceStore};
use munarium_core::{KernelError, Result};
use object_store::aws::AmazonS3Builder;
use object_store::azure::MicrosoftAzureBuilder;
use object_store::gcp::GoogleCloudStorageBuilder;
use object_store::local::LocalFileSystem;
use object_store::path::Path as ObjectPath;
use object_store::{
    Attribute, Attributes, ObjectStore, ObjectStoreExt as _, PutOptions, PutPayload,
};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct S3Config {
    pub bucket: String,
    /// SigV4 region. Ignored by most S3-compatibles but still required by the
    /// signer, so config parsing supplies a value in every case.
    pub region: String,
    /// MinIO / Cloudflare R2 / any S3-compatible. `http://` implies
    /// allow_http (loopback tooling; never a cloud deployment).
    pub endpoint: Option<String>,
    /// Bucket-in-path addressing. MinIO requires it; AWS prefers
    /// virtual-hosted. Config parsing defaults this to `endpoint.is_some()`.
    pub force_path_style: bool,
    /// Static credentials for off-cloud tooling. `None` = the ambient AWS
    /// chain (env vars, web identity, IMDS instance profile) via `from_env`.
    pub access_key_id: Option<String>,
    /// Already resolved through the server's credential seam — this is the
    /// secret itself, never a reference.
    pub secret_access_key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GcsConfig {
    pub bucket: String,
    /// Service-account key JSON (the content, resolved upstream). `None` =
    /// ambient (GOOGLE_APPLICATION_CREDENTIALS or the metadata server).
    pub service_account_json: Option<String>,
}

#[derive(Debug, Clone)]
pub enum AzureAuth {
    /// The workload's managed identity (ACA/AKS). No secret exists. Falls
    /// back to the platform IMDS endpoint, exactly like the predecessor.
    ManagedIdentity {
        /// Optional user-assigned identity client id. None = system-assigned.
        client_id: Option<String>,
    },
    /// A container SAS, for tooling that runs outside Azure. The leading '?'
    /// is optional.
    Sas { token: String },
}

#[derive(Debug, Clone)]
pub struct AzureConfig {
    pub account: String,
    pub container: String,
    pub auth: AzureAuth,
    /// Override the blob endpoint (Azurite, sovereign clouds, tests).
    pub endpoint: Option<String>,
}

impl AzureConfig {
    /// `https://{account}.blob.core.windows.net` unless overridden.
    pub fn endpoint(&self) -> String {
        self.endpoint
            .clone()
            .unwrap_or_else(|| format!("https://{}.blob.core.windows.net", self.account))
    }
}

/// One adapter over any [`ObjectStore`]; which backend it is shows only in
/// `backend_id` and the recorded-URI shape.
pub struct ObjectSourceStore {
    store: Arc<dyn ObjectStore>,
    backend_id: &'static str,
    /// Everything before the blob name in a recorded URI, no trailing '/'.
    uri_prefix: String,
    /// Cloud stores persist a content-type attribute; the local filesystem
    /// has nowhere to put one and rejects attributes rather than dropping
    /// them, so it opts out.
    attach_content_type: bool,
    /// Azure recorded URIs percent-encode the blob path (predecessor
    /// compatibility — rows must stay byte-identical). Other backends record
    /// the raw logical path, like `mem://` and `pg://` always have.
    percent_encode_uri_path: bool,
}

/// reqwest 0.13's `rustls-no-provider` build path (chosen to keep aws-lc-rs
/// and its cmake requirement out of the musl build) panics unless a
/// process-level rustls crypto provider is installed. Install ring exactly
/// once; an Err from install_default means another component already
/// installed a provider, which is equally fine — one just has to exist.
fn ensure_rustls_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

impl ObjectSourceStore {
    pub fn s3(cfg: S3Config) -> Result<Self> {
        ensure_rustls_provider();
        if cfg.bucket.trim().is_empty() {
            return Err(KernelError::InvalidInput(
                "s3 bucket name is required".into(),
            ));
        }
        let mut builder = AmazonS3Builder::from_env()
            .with_bucket_name(&cfg.bucket)
            .with_region(&cfg.region)
            .with_virtual_hosted_style_request(!cfg.force_path_style);
        if let Some(endpoint) = &cfg.endpoint {
            builder = builder
                .with_endpoint(endpoint)
                .with_allow_http(endpoint.starts_with("http://"));
        }
        match (&cfg.access_key_id, &cfg.secret_access_key) {
            (Some(id), Some(secret)) => {
                builder = builder
                    .with_access_key_id(id)
                    .with_secret_access_key(secret);
            }
            (None, None) => {} // ambient chain
            _ => {
                // Config parsing refuses half a credential; this is the
                // defensive backstop for programmatic construction.
                return Err(KernelError::InvalidInput(
                    "s3 static credentials need both an access key id and a secret".into(),
                ));
            }
        }
        let store = builder
            .build()
            .map_err(|e| KernelError::Storage(format!("s3 store init: {e}")))?;
        Ok(Self {
            store: Arc::new(store),
            backend_id: "s3",
            uri_prefix: format!("s3://{}", cfg.bucket),
            attach_content_type: true,
            percent_encode_uri_path: false,
        })
    }

    pub fn azure(cfg: AzureConfig) -> Result<Self> {
        ensure_rustls_provider();
        if cfg.account.trim().is_empty() {
            return Err(KernelError::InvalidInput(
                "azure storage account name is required".into(),
            ));
        }
        if cfg.container.trim().is_empty() {
            return Err(KernelError::InvalidInput(
                "azure blob container name is required".into(),
            ));
        }
        // MUST be from_env(), not new(): Container Apps / App Service have no
        // classic IMDS at 169.254.169.254 — the platform injects
        // IDENTITY_ENDPOINT (+ IDENTITY_HEADER, read per-request by
        // object_store's credential provider) and from_env() is what picks
        // the endpoint up. With new() the managed-identity path black-holes
        // against link-local IMDS and every blob call times out (live
        // incident, 2026-08-11 dev smoke).
        let mut builder = MicrosoftAzureBuilder::from_env()
            .with_account(&cfg.account)
            .with_container_name(&cfg.container);
        if let Some(endpoint) = &cfg.endpoint {
            builder = builder
                .with_endpoint(endpoint.trim_end_matches('/').to_string())
                .with_allow_http(endpoint.starts_with("http://"));
        }
        match &cfg.auth {
            AzureAuth::ManagedIdentity { client_id } => {
                // With no key/SAS/secret configured the builder's credential
                // resolution falls through to IMDS managed identity — the
                // same endpoint the predecessor crate called directly.
                if let Some(id) = client_id {
                    builder = builder.with_client_id(id);
                }
            }
            AzureAuth::Sas { token } => {
                builder = builder.with_config(
                    object_store::azure::AzureConfigKey::SasKey,
                    token.trim_start_matches('?'),
                );
            }
        }
        let store = builder
            .build()
            .map_err(|e| KernelError::Storage(format!("azure store init: {e}")))?;
        Ok(Self {
            store: Arc::new(store),
            backend_id: "az",
            uri_prefix: format!("{}/{}", cfg.endpoint().trim_end_matches('/'), cfg.container),
            attach_content_type: true,
            percent_encode_uri_path: true,
        })
    }

    pub fn gcs(cfg: GcsConfig) -> Result<Self> {
        ensure_rustls_provider();
        if cfg.bucket.trim().is_empty() {
            return Err(KernelError::InvalidInput(
                "gcs bucket name is required".into(),
            ));
        }
        let mut builder = GoogleCloudStorageBuilder::from_env().with_bucket_name(&cfg.bucket);
        if let Some(json) = &cfg.service_account_json {
            builder = builder.with_service_account_key(json);
        }
        let store = builder
            .build()
            .map_err(|e| KernelError::Storage(format!("gcs store init: {e}")))?;
        Ok(Self {
            store: Arc::new(store),
            backend_id: "gcs",
            uri_prefix: format!("gs://{}", cfg.bucket),
            attach_content_type: true,
            percent_encode_uri_path: false,
        })
    }

    /// Local filesystem rooted at `root` (created if absent). Tenants become
    /// directories under it, so the tree browses exactly like the container.
    pub fn local(root: &str) -> Result<Self> {
        if root.trim().is_empty() {
            return Err(KernelError::InvalidInput(
                "file store root directory is required".into(),
            ));
        }
        std::fs::create_dir_all(root)
            .map_err(|e| KernelError::Storage(format!("file store root '{root}': {e}")))?;
        let store = LocalFileSystem::new_with_prefix(root)
            .map_err(|e| KernelError::Storage(format!("file store root '{root}': {e}")))?;
        // file:///abs/path with forward slashes; Windows roots become
        // file:///C:/... which is the standard file-URI shape.
        let normalized = root.replace('\\', "/");
        Ok(Self {
            store: Arc::new(store),
            backend_id: "file",
            uri_prefix: format!("file:///{}", normalized.trim_start_matches('/')),
            attach_content_type: false,
            percent_encode_uri_path: false,
        })
    }

    /// Test seam: wrap any [`ObjectStore`] (e.g. `object_store::memory::InMemory`).
    pub fn from_parts(
        store: Arc<dyn ObjectStore>,
        backend_id: &'static str,
        uri_prefix: String,
    ) -> Self {
        Self {
            store,
            backend_id,
            uri_prefix,
            attach_content_type: false,
            percent_encode_uri_path: false,
        }
    }

    fn object_path(key: &SourceKey) -> Result<ObjectPath> {
        // validate_path (in SourceKey::new) already rejected traversal,
        // absolute paths, empty segments — parse failure here is defensive.
        ObjectPath::parse(key.blob_name()).map_err(|e| {
            KernelError::InvalidInput(format!("source path '{}': {e}", key.blob_name()))
        })
    }

    /// The underlying client, so an artifact store can reuse it.
    ///
    /// Reuse rather than reconstruction is the point: the Azure credential
    /// path, the endpoint override and the token cache all live in this
    /// client, and a second construction elsewhere would be a second place for
    /// them to drift.
    pub fn object_store(&self) -> Arc<dyn ObjectStore> {
        self.store.clone()
    }

    pub fn recorded_uri(&self, key: &SourceKey) -> String {
        let name = key.blob_name();
        let path = if self.percent_encode_uri_path {
            encode_blob_path(&name)
        } else {
            name
        };
        format!("{}/{path}", self.uri_prefix)
    }

    fn storage_err(&self, verb: &str, key: &SourceKey, e: object_store::Error) -> KernelError {
        KernelError::Storage(format!(
            "{} {verb} {}: {e}",
            self.backend_id,
            key.blob_name()
        ))
    }
}

/// Percent-encode a blob path for use in a URL, keeping '/' as a separator so
/// the container listing stays foldered. Unreserved characters (RFC 3986)
/// plus the path-safe set pass through. (Ported verbatim from the predecessor
/// so recorded Azure URIs stay byte-identical.)
pub fn encode_blob_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.as_bytes() {
        let c = *byte as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~' | '/') {
            out.push(c);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[async_trait]
impl SourceStore for ObjectSourceStore {
    async fn put(&self, key: &SourceKey, media_type: &str, bytes: &[u8]) -> Result<String> {
        let path = Self::object_path(key)?;
        let mut opts = PutOptions::default();
        if self.attach_content_type && !media_type.is_empty() {
            let mut attributes = Attributes::new();
            attributes.insert(Attribute::ContentType, media_type.to_string().into());
            opts.attributes = attributes;
        }
        self.store
            .put_opts(&path, PutPayload::from(bytes.to_vec()), opts)
            .await
            .map_err(|e| self.storage_err("PUT", key, e))?;
        Ok(self.recorded_uri(key))
    }

    async fn get(&self, key: &SourceKey) -> Result<Vec<u8>> {
        let path = Self::object_path(key)?;
        let result = match self.store.get(&path).await {
            Ok(r) => r,
            Err(object_store::Error::NotFound { .. }) => {
                return Err(KernelError::NotFound {
                    kind: "source blob",
                    id: key.blob_name(),
                })
            }
            Err(e) => return Err(self.storage_err("GET", key, e)),
        };
        Ok(result
            .bytes()
            .await
            .map_err(|e| self.storage_err("GET", key, e))?
            .to_vec())
    }

    async fn exists(&self, key: &SourceKey) -> Result<bool> {
        let path = Self::object_path(key)?;
        match self.store.head(&path).await {
            Ok(_) => Ok(true),
            Err(object_store::Error::NotFound { .. }) => Ok(false),
            Err(e) => Err(self.storage_err("HEAD", key, e)),
        }
    }

    async fn delete(&self, key: &SourceKey) -> Result<()> {
        let path = Self::object_path(key)?;
        match self.store.delete(&path).await {
            // Deleting an absent blob is not an error — the contract is
            // idempotent.
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(e) => Err(self.storage_err("DELETE", key, e)),
        }
    }

    fn backend_id(&self) -> &'static str {
        self.backend_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use object_store::memory::InMemory;

    fn key(path: &str) -> SourceKey {
        SourceKey::new("demo", path, "hash").expect("valid path")
    }

    fn mem_store() -> ObjectSourceStore {
        ObjectSourceStore::from_parts(Arc::new(InMemory::new()), "s3", "s3://bucket".into())
    }

    #[tokio::test]
    async fn roundtrip_put_get_exists_delete() {
        let store = mem_store();
        let k = key("northgate/03_finance/audited-fy2023.md");
        assert!(!store.exists(&k).await.expect("exists"));
        let uri = store.put(&k, "text/markdown", b"hello").await.expect("put");
        assert_eq!(
            uri,
            "s3://bucket/demo/northgate/03_finance/audited-fy2023.md"
        );
        assert!(store.exists(&k).await.expect("exists"));
        assert_eq!(store.get(&k).await.expect("get"), b"hello");
        store.delete(&k).await.expect("delete");
        assert!(!store.exists(&k).await.expect("exists"));
    }

    #[tokio::test]
    async fn get_of_absent_blob_is_not_found_with_the_blob_name() {
        let store = mem_store();
        match store.get(&key("missing.md")).await {
            Err(KernelError::NotFound { kind, id }) => {
                assert_eq!(kind, "source blob");
                assert_eq!(id, "demo/missing.md");
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn delete_is_idempotent() {
        let store = mem_store();
        store.delete(&key("never-existed.md")).await.expect("first");
        store
            .delete(&key("never-existed.md"))
            .await
            .expect("second");
    }

    #[tokio::test]
    async fn put_overwrites_in_place() {
        let store = mem_store();
        let k = key("a.md");
        store.put(&k, "text/markdown", b"v1").await.expect("put v1");
        store.put(&k, "text/markdown", b"v2").await.expect("put v2");
        assert_eq!(store.get(&k).await.expect("get"), b"v2");
    }

    // ---- recorded-URI contract -------------------------------------------

    #[test]
    fn s3_recorded_uri_has_no_credential_even_with_static_keys() {
        let store = ObjectSourceStore::s3(S3Config {
            bucket: "sources".into(),
            region: "us-east-1".into(),
            endpoint: Some("http://127.0.0.1:9000".into()),
            force_path_style: true,
            access_key_id: Some("minioadmin".into()),
            secret_access_key: Some("SECRETSECRET".into()),
        })
        .expect("store");
        let uri = store.recorded_uri(&key("a.md"));
        assert_eq!(uri, "s3://sources/demo/a.md");
        assert!(!uri.contains("SECRET"));
        assert!(!uri.contains('?'));
    }

    #[test]
    fn s3_half_a_static_credential_is_refused() {
        let err = ObjectSourceStore::s3(S3Config {
            bucket: "sources".into(),
            region: "us-east-1".into(),
            endpoint: None,
            force_path_style: false,
            access_key_id: Some("AKIA".into()),
            secret_access_key: None,
        });
        assert!(err.is_err());
    }

    #[test]
    fn azure_recorded_uri_is_byte_identical_to_the_predecessor() {
        let store = ObjectSourceStore::azure(AzureConfig {
            account: "stexampledev01".into(),
            container: "sources".into(),
            auth: AzureAuth::ManagedIdentity { client_id: None },
            endpoint: None,
        })
        .expect("store");
        // The exact string munarium-store-az recorded (its unit test asserted
        // this shape) — existing rows must stay indistinguishable.
        assert_eq!(
            store.recorded_uri(&key("northgate/03_finance/audited-fy2023.md")),
            "https://stexampledev01.blob.core.windows.net/sources/demo/northgate/03_finance/audited-fy2023.md"
        );
        // Percent-encoding preserved, '/' kept as separator.
        assert_eq!(
            store.recorded_uri(&key("a/my file.md")),
            "https://stexampledev01.blob.core.windows.net/sources/demo/a/my%20file.md"
        );
    }

    #[test]
    fn azure_sas_never_reaches_the_recorded_uri() {
        let store = ObjectSourceStore::azure(AzureConfig {
            account: "stexampledev01".into(),
            container: "sources".into(),
            auth: AzureAuth::Sas {
                token: "?sv=2021&ss=b&sig=SECRET".into(),
            },
            endpoint: None,
        })
        .expect("store");
        let uri = store.recorded_uri(&key("a.md"));
        assert!(!uri.contains("sig"));
        assert!(!uri.contains('?'));
    }

    #[test]
    fn azure_endpoint_override_supports_azurite_and_sovereign_clouds() {
        let store = ObjectSourceStore::azure(AzureConfig {
            account: "devstoreaccount1".into(),
            container: "sources".into(),
            auth: AzureAuth::Sas {
                token: "sv=2021&sig=x".into(),
            },
            endpoint: Some("http://127.0.0.1:10000/devstoreaccount1".into()),
        })
        .expect("store");
        assert_eq!(
            store.recorded_uri(&key("a.md")),
            "http://127.0.0.1:10000/devstoreaccount1/sources/demo/a.md"
        );
    }

    #[test]
    fn azure_empty_account_or_container_is_refused_at_construction() {
        let base = AzureConfig {
            account: "acct".into(),
            container: "sources".into(),
            auth: AzureAuth::ManagedIdentity { client_id: None },
            endpoint: None,
        };
        let mut c = base.clone();
        c.account = "".into();
        assert!(ObjectSourceStore::azure(c).is_err());
        let mut c = base;
        c.container = "  ".into();
        assert!(ObjectSourceStore::azure(c).is_err());
    }

    #[test]
    fn paths_are_encoded_without_losing_folder_structure() {
        assert_eq!(encode_blob_path("a/b/c.md"), "a/b/c.md");
        assert_eq!(encode_blob_path("a/my file.md"), "a/my%20file.md");
        assert_eq!(encode_blob_path("a/100%.md"), "a/100%25.md");
        assert_eq!(encode_blob_path("a/q?x=1"), "a/q%3Fx%3D1");
        assert_eq!(encode_blob_path("café.md"), "caf%C3%A9.md");
    }

    // ---- local filesystem -------------------------------------------------

    #[tokio::test]
    async fn local_roundtrip_lands_real_folders() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_str().expect("utf8 root");
        let store = ObjectSourceStore::local(root).expect("store");
        assert_eq!(store.backend_id(), "file");
        let k = key("northgate/03_finance/audited-fy2023.md");
        let uri = store.put(&k, "text/markdown", b"bytes").await.expect("put");
        assert!(uri.starts_with("file:///"));
        assert!(uri.ends_with("/demo/northgate/03_finance/audited-fy2023.md"));
        assert_eq!(store.get(&k).await.expect("get"), b"bytes");
        // The tree is browsable on disk: tenant/path became directories.
        let on_disk = dir
            .path()
            .join("demo")
            .join("northgate")
            .join("03_finance")
            .join("audited-fy2023.md");
        assert!(on_disk.is_file(), "expected {on_disk:?} on disk");
        store.delete(&k).await.expect("delete");
        assert!(!store.exists(&k).await.expect("exists"));
    }

    #[test]
    fn local_creates_the_root_and_refuses_an_empty_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("not").join("yet").join("created");
        let store = ObjectSourceStore::local(nested.to_str().expect("utf8")).expect("store");
        assert!(nested.is_dir());
        assert_eq!(store.backend_id(), "file");
        assert!(ObjectSourceStore::local("").is_err());
    }
}
