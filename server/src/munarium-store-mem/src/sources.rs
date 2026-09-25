// SPDX-License-Identifier: Apache-2.0
//! `MemSourceStore` — an in-memory `SourceStore` for tests and the memory
//! backend. Same contract as the Azure and Postgres backends, no I/O.

use async_trait::async_trait;
use munarium_core::sources::{SourceKey, SourceStore};
use munarium_core::{KernelError, Result};
use std::collections::HashMap;
use std::sync::{PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// The blob map's lock is poisoned only if a thread panicked while holding it.
/// The trait's callers get a storage error rather than a second panic, and the
/// blobs stay untouched until the process restarts (P15/R32).
fn poisoned<T>(_: PoisonError<T>) -> KernelError {
    KernelError::Storage("in-memory source store lock is poisoned".into())
}

#[derive(Default)]
pub struct MemSourceStore {
    blobs: RwLock<HashMap<String, Vec<u8>>>,
}

impl MemSourceStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of stored blobs. A count is a diagnostic, not a storage
    /// outcome, so it still answers after the lock is poisoned: every critical
    /// section here is a single map operation, so the map is never left half
    /// updated.
    pub fn len(&self) -> usize {
        self.blobs
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    fn read(&self) -> Result<RwLockReadGuard<'_, HashMap<String, Vec<u8>>>> {
        self.blobs.read().map_err(poisoned)
    }

    fn write(&self) -> Result<RwLockWriteGuard<'_, HashMap<String, Vec<u8>>>> {
        self.blobs.write().map_err(poisoned)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[async_trait]
impl SourceStore for MemSourceStore {
    async fn put(&self, key: &SourceKey, _media_type: &str, bytes: &[u8]) -> Result<String> {
        let name = key.blob_name();
        self.write()?.insert(name.clone(), bytes.to_vec());
        Ok(format!("mem://{name}"))
    }

    async fn get(&self, key: &SourceKey) -> Result<Vec<u8>> {
        self.read()?
            .get(&key.blob_name())
            .cloned()
            .ok_or_else(|| KernelError::NotFound {
                kind: "source blob",
                id: key.blob_name(),
            })
    }

    async fn exists(&self, key: &SourceKey) -> Result<bool> {
        Ok(self.read()?.contains_key(&key.blob_name()))
    }

    async fn delete(&self, key: &SourceKey) -> Result<()> {
        self.write()?.remove(&key.blob_name());
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

    fn poisoned() -> MemSourceStore {
        let store = std::sync::Arc::new(MemSourceStore::new());
        let held = store.clone();
        let _ = std::thread::spawn(move || {
            let _guard = held.blobs.write().unwrap();
            panic!("poison the blob lock");
        })
        .join();
        assert!(store.blobs.is_poisoned());
        std::sync::Arc::into_inner(store).expect("sole owner")
    }

    #[tokio::test]
    async fn a_poisoned_blob_lock_is_a_storage_error_not_a_panic() {
        // P15/R32: every trait method used `.expect("blob lock")`, so one
        // panic while a guard was held turned each later call into a panic.
        let store = poisoned();
        let k = key("a/b.md");
        let storage = |r: Result<()>| matches!(r, Err(KernelError::Storage(_)));
        assert!(storage(
            store.put(&k, "text/markdown", b"x").await.map(|_| ())
        ));
        assert!(storage(store.get(&k).await.map(|_| ())));
        assert!(storage(store.exists(&k).await.map(|_| ())));
        assert!(storage(store.delete(&k).await));
        // The count is a diagnostic, not a storage outcome: it still answers.
        assert_eq!(store.len(), 0);
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
