// SPDX-License-Identifier: Apache-2.0
//! `MemSourceStore` — an in-memory `SourceStore` for tests and the memory
//! backend. Same contract as the Azure and Postgres backends, no I/O.

use async_trait::async_trait;
use munarium_core::sources::{SourceKey, SourceStore};
use munarium_core::{KernelError, Result};
use std::collections::HashMap;
use std::sync::RwLock;

#[derive(Default)]
pub struct MemSourceStore {
    blobs: RwLock<HashMap<String, Vec<u8>>>,
}

impl MemSourceStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.blobs.read().expect("blob lock").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[async_trait]
impl SourceStore for MemSourceStore {
    async fn put(&self, key: &SourceKey, _media_type: &str, bytes: &[u8]) -> Result<String> {
        let name = key.blob_name();
        self.blobs
            .write()
            .expect("blob lock")
            .insert(name.clone(), bytes.to_vec());
        Ok(format!("mem://{name}"))
    }

    async fn get(&self, key: &SourceKey) -> Result<Vec<u8>> {
        self.blobs
            .read()
            .expect("blob lock")
            .get(&key.blob_name())
            .cloned()
            .ok_or_else(|| KernelError::NotFound {
                kind: "source blob",
                id: key.blob_name(),
            })
    }

    async fn exists(&self, key: &SourceKey) -> Result<bool> {
        Ok(self
            .blobs
            .read()
            .expect("blob lock")
            .contains_key(&key.blob_name()))
    }

    async fn delete(&self, key: &SourceKey) -> Result<()> {
        self.blobs
            .write()
            .expect("blob lock")
            .remove(&key.blob_name());
        Ok(())
    }

    fn backend_id(&self) -> &'static str {
        "mem"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(path: &str) -> SourceKey {
        SourceKey::new("demo", path, "hash").expect("valid path")
    }

    #[tokio::test]
    async fn round_trips_and_overwrites_in_place() {
        let store = MemSourceStore::new();
        let k = key("a/b.md");
        assert!(!store.exists(&k).await.expect("exists"));

        let uri = store.put(&k, "text/markdown", b"one").await.expect("put");
        assert_eq!(uri, "mem://demo/a/b.md");
        assert_eq!(store.get(&k).await.expect("get"), b"one");

        // Same path, new bytes: an update, not a second blob.
        store.put(&k, "text/markdown", b"two").await.expect("put2");
        assert_eq!(store.get(&k).await.expect("get2"), b"two");
        assert_eq!(store.len(), 1);

        store.delete(&k).await.expect("delete");
        assert!(!store.exists(&k).await.expect("exists after delete"));
        // Deleting an absent blob is not an error.
        store.delete(&k).await.expect("idempotent delete");
        assert!(store.get(&k).await.is_err());
    }

    #[tokio::test]
    async fn identical_bytes_at_two_paths_are_two_blobs() {
        let store = MemSourceStore::new();
        let a = key("northgate/policy.md");
        let b = key("smoke/policy.md");
        store.put(&a, "text/markdown", b"same").await.expect("a");
        store.put(&b, "text/markdown", b"same").await.expect("b");
        assert_eq!(store.len(), 2, "paths address blobs, content does not");

        // Retiring one must not touch the other.
        store.delete(&a).await.expect("delete a");
        assert!(!store.exists(&a).await.expect("a gone"));
        assert!(store.exists(&b).await.expect("b remains"));
    }
}
