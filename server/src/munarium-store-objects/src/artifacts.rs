// SPDX-License-Identifier: Apache-2.0
//! `ObjectArtifactStore` — Munarium artifacts over an object store.
//!
//! Lives here rather than in `munarium-datastore` on purpose. That crate must
//! stay independently usable, with no cloud SDK and no credential policy in its
//! graph (§4.1); it defines the `ArtifactStore` trait and this crate satisfies
//! it. The Azure client is built by exactly the same path as the source store,
//! so `MicrosoftAzureBuilder::from_env()` and its managed-identity behaviour are
//! shared rather than re-derived — that detail cost a live incident once and is
//! not worth re-learning.
//!
//! ## Why this is synchronous
//!
//! `ArtifactStore` is a synchronous trait. The datastore's query and open paths
//! are CPU-bound — Tantivy, mmap, graph traversal — and Server runs them on a
//! bounded blocking pool, so an async trait would push a runtime choice into a
//! crate that is supposed to be liftable.
//!
//! **The contract that follows: every method here must be called from a
//! blocking context** (`spawn_blocking`, a dedicated thread, or a synchronous
//! `main`), never from a runtime worker. `Handle::block_on` panics on a worker
//! thread, so a violation fails loudly at the first call rather than deadlocking
//! under load — which is the failure mode worth having.

use std::sync::Arc;

use object_store::path::Path as ObjectPath;
use object_store::{ObjectStore, ObjectStoreExt};
use tokio::runtime::Handle;

use munarium_datastore::store::{ArtifactStore, ByteRange};
use munarium_datastore::verify::normalize_component_path;
use munarium_datastore::Error as DsError;

use crate::ObjectSourceStore;

/// An artifact's components under one immutable prefix.
///
/// The prefix is supplied by Server and already carries the tenant, scope,
/// logical version and artifact id. This type never composes one itself: a
/// store that could derive its own prefix from an artifact id would be one
/// refactor away from letting a content hash address another tenant's bytes.
pub struct ObjectArtifactStore {
    store: Arc<dyn ObjectStore>,
    prefix: String,
    handle: Handle,
}

impl std::fmt::Debug for ObjectArtifactStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The prefix carries a tenant hash and a scope. Shown as a length
        // rather than a value, because this ends up in logs.
        f.debug_struct("ObjectArtifactStore")
            .field("prefix_len", &self.prefix.len())
            .finish_non_exhaustive()
    }
}

impl ObjectArtifactStore {
    /// Build from an already-constructed source store, reusing its client.
    ///
    /// Reuse is the point: the Azure credential path, the endpoint override and
    /// the token cache are all in that client, and a second construction here
    /// would be a second place for them to drift.
    pub fn from_source_store(
        source: &ObjectSourceStore,
        prefix: impl Into<String>,
    ) -> Result<Self, DsError> {
        Self::new(source.object_store(), prefix)
    }

    pub fn new(store: Arc<dyn ObjectStore>, prefix: impl Into<String>) -> Result<Self, DsError> {
        let prefix = prefix.into();
        let trimmed = prefix.trim_matches('/').to_string();
        if trimmed.is_empty() {
            return Err(DsError::Path(
                "an artifact prefix may not be empty; without one every artifact in the \
                 container shares a keyspace"
                    .into(),
            ));
        }
        if trimmed.contains("..") {
            return Err(DsError::Path(format!(
                "artifact prefix {prefix:?} contains '..'"
            )));
        }
        let handle = Handle::try_current().map_err(|_| {
            DsError::Invalid(
                "ObjectArtifactStore needs a Tokio runtime handle; construct it inside the \
                 runtime and call its methods from a blocking context"
                    .into(),
            )
        })?;
        Ok(Self {
            store,
            prefix: trimmed,
            handle,
        })
    }

    /// Join the prefix to a component path, re-normalizing the component.
    ///
    /// Normalization runs again here even though the manifest verifier already
    /// did it. This is the last point before a real object key is formed, and a
    /// check that only runs at parse time is one an alternative code path can
    /// skip.
    fn key(&self, component: &str) -> Result<ObjectPath, DsError> {
        let norm = normalize_component_path(component)?;
        Ok(ObjectPath::from(format!("{}/{}", self.prefix, norm)))
    }

    fn map_err(&self, op: &str, component: &str, e: object_store::Error) -> DsError {
        match e {
            object_store::Error::NotFound { .. } => {
                DsError::Integrity(format!("{op}: component {component:?} is absent"))
            }
            // Everything else is transport or permissions. Deliberately NOT an
            // Integrity error: that variant means "quarantine this artifact",
            // and a throttled request must not permanently unserve good bytes.
            other => DsError::Invalid(format!("{op} {component:?}: {other}")),
        }
    }

    /// Run one async operation from a blocking context.
    fn block<F, T>(&self, fut: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        self.handle.block_on(fut)
    }
}

impl ArtifactStore for ObjectArtifactStore {
    fn put_component(&self, path: &str, bytes: &[u8]) -> Result<(), DsError> {
        let key = self.key(path)?;
        let payload = object_store::PutPayload::from(bytes.to_vec());
        self.block(async {
            self.store
                .put(&key, payload)
                .await
                .map(|_| ())
                .map_err(|e| self.map_err("PUT", path, e))
        })
    }

    fn get_component(&self, path: &str, range: Option<ByteRange>) -> Result<Vec<u8>, DsError> {
        let key = self.key(path)?;
        self.block(async {
            match range {
                None => {
                    let got = self
                        .store
                        .get(&key)
                        .await
                        .map_err(|e| self.map_err("GET", path, e))?;
                    got.bytes()
                        .await
                        .map(|b| b.to_vec())
                        .map_err(|e| self.map_err("GET body", path, e))
                }
                Some(r) => {
                    if r.end < r.start {
                        return Err(DsError::Invalid(format!(
                            "inverted range {}..{} for {path:?}",
                            r.start, r.end
                        )));
                    }
                    // The range API exists from day one so a later verified
                    // block reader needs no trait change. Whole-
                    // artifact hydration is still the v1 serving path.
                    self.store
                        .get_range(&key, r.start..r.end)
                        .await
                        .map(|b| b.to_vec())
                        .map_err(|e| self.map_err("GET range", path, e))
                }
            }
        })
    }

    fn head_component(&self, path: &str) -> Result<u64, DsError> {
        let key = self.key(path)?;
        self.block(async {
            self.store
                .head(&key)
                .await
                .map(|m| m.size)
                .map_err(|e| self.map_err("HEAD", path, e))
        })
    }

    fn exists(&self, path: &str) -> Result<bool, DsError> {
        let key = self.key(path)?;
        self.block(async {
            match self.store.head(&key).await {
                Ok(_) => Ok(true),
                Err(object_store::Error::NotFound { .. }) => Ok(false),
                Err(e) => Err(self.map_err("HEAD", path, e)),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use object_store::memory::InMemory;

    fn store(prefix: &str) -> ObjectArtifactStore {
        ObjectArtifactStore::new(Arc::new(InMemory::new()), prefix).unwrap()
    }

    /// Every method runs from `spawn_blocking`, which is the documented
    /// contract. Testing it any other way would test something the production
    /// path never does.
    async fn blocking<T, F>(f: F) -> T
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        tokio::task::spawn_blocking(f).await.unwrap()
    }

    #[tokio::test]
    async fn round_trips_a_component_under_its_prefix() {
        let s = store("v1/tenant-hash/collection/col-1/idx2-v/artifact-a");
        blocking(move || {
            s.put_component("records/chunks.bin", b"hello").unwrap();
            assert!(s.exists("records/chunks.bin").unwrap());
            assert_eq!(s.head_component("records/chunks.bin").unwrap(), 5);
            assert_eq!(
                s.get_component("records/chunks.bin", None).unwrap(),
                b"hello"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn reads_a_byte_range() {
        let s = store("v1/a");
        blocking(move || {
            s.put_component("a.bin", b"0123456789").unwrap();
            let got = s
                .get_component("a.bin", Some(ByteRange { start: 2, end: 5 }))
                .unwrap();
            assert_eq!(got, b"234");
        })
        .await;
    }

    /// Two artifacts under different prefixes never see each other, which is
    /// what makes the prefix — not the artifact id — the isolation boundary.
    #[tokio::test]
    async fn prefixes_isolate_two_artifacts_in_one_container() {
        let backing: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
        let a = ObjectArtifactStore::new(backing.clone(), "v1/tenant-a/art").unwrap();
        let b = ObjectArtifactStore::new(backing, "v1/tenant-b/art").unwrap();
        blocking(move || {
            a.put_component("m.json", b"a-bytes").unwrap();
            assert!(!b.exists("m.json").unwrap());
            b.put_component("m.json", b"b-bytes").unwrap();
            assert_eq!(a.get_component("m.json", None).unwrap(), b"a-bytes");
            assert_eq!(b.get_component("m.json", None).unwrap(), b"b-bytes");
        })
        .await;
    }

    /// The store re-normalizes rather than trusting its caller: this is the
    /// last point before an object key is formed.
    #[tokio::test]
    async fn traversal_is_refused_even_though_the_verifier_already_checked() {
        let s = store("v1/a");
        blocking(move || {
            for bad in ["../escape", "/etc/passwd", "a/../../b", "x\\y"] {
                assert!(
                    s.get_component(bad, None).is_err(),
                    "{bad} should be refused"
                );
                assert!(
                    s.put_component(bad, b"x").is_err(),
                    "{bad} should be refused"
                );
            }
        })
        .await;
    }

    #[tokio::test]
    async fn an_empty_or_traversing_prefix_is_refused() {
        let backing: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
        assert!(ObjectArtifactStore::new(backing.clone(), "").is_err());
        assert!(ObjectArtifactStore::new(backing.clone(), "///").is_err());
        assert!(ObjectArtifactStore::new(backing, "v1/../etc").is_err());
    }

    /// A missing component is an integrity problem (the manifest promised it);
    /// a transport failure is not, because Integrity means "quarantine" and a
    /// throttled request must not permanently unserve good bytes.
    #[tokio::test]
    async fn an_absent_component_reports_integrity_not_transport() {
        let s = store("v1/a");
        blocking(move || {
            let err = s.get_component("missing.bin", None).unwrap_err();
            assert!(matches!(err, DsError::Integrity(_)), "{err}");
            assert!(!s.exists("missing.bin").unwrap());
        })
        .await;
    }

    /// Debug must not print the prefix: it carries a tenant hash and a scope,
    /// and this type ends up in logs.
    #[tokio::test]
    async fn debug_does_not_print_the_prefix() {
        let s = store("v1/tenant-secret-hash/collection/col-1");
        let shown = format!("{s:?}");
        assert!(!shown.contains("tenant-secret-hash"), "{shown}");
        assert!(!shown.contains("col-1"), "{shown}");
    }
}
