// SPDX-License-Identifier: Apache-2.0
//! `ArtifactStore` — where sealed components live.
//!
//! The trait is deliberately range-capable from the start even though whole-
//! artifact hydration is the v1 serving path (§10.2). A later verified
//! remote-range reader needs `get_component(.., Some(range))`, and adding it
//! afterwards would be a breaking change to every implementation; defining it
//! now costs one unused parameter.
//!
//! The trait is synchronous. This crate is engine-side and CPU-bound —
//! Tantivy, mmap and graph traversal are all synchronous — and Server runs it
//! on a bounded blocking pool. An async trait here would push a runtime choice
//! into a crate that is supposed to be liftable.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::verify::normalize_component_path;
use crate::Error;

/// A byte range, half-open like every other range in Rust.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

/// Where an artifact's components are read from and written to.
///
/// Keys are ALWAYS the artifact-relative component paths from the manifest,
/// normalized. An implementation combines them with a tenant- and
/// scope-constrained prefix it was constructed with; no method takes a bare
/// artifact hash as authority to read or delete, because a content hash is not
/// an authorization boundary.
pub trait ArtifactStore: Send + Sync {
    fn put_component(&self, path: &str, bytes: &[u8]) -> Result<(), Error>;
    fn get_component(&self, path: &str, range: Option<ByteRange>) -> Result<Vec<u8>, Error>;
    fn head_component(&self, path: &str) -> Result<u64, Error>;
    fn exists(&self, path: &str) -> Result<bool, Error>;
}

/// The local-filesystem store, behind the `artifact-file` feature.
///
/// Cloud object stores are supplied by a thin adapter or by Server using a
/// generic client, which keeps Azure/AWS/GCP credential policy out of this
/// crate entirely.
#[derive(Debug, Clone)]
pub struct LocalFileStore {
    root: PathBuf,
}

impl LocalFileStore {
    /// Root the store at a directory. The directory is created if absent.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, Error> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve a component path under the root.
    ///
    /// Normalization is re-applied here rather than trusted from the caller.
    /// This is the last point before a real filesystem operation, and a check
    /// that only runs at parse time is a check an alternative code path can
    /// skip.
    fn resolve(&self, path: &str) -> Result<PathBuf, Error> {
        let norm = normalize_component_path(path)?;
        Ok(self.root.join(norm))
    }
}

impl ArtifactStore for LocalFileStore {
    fn put_component(&self, path: &str, bytes: &[u8]) -> Result<(), Error> {
        let target = self.resolve(path)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&target, bytes)?;
        Ok(())
    }

    fn get_component(&self, path: &str, range: Option<ByteRange>) -> Result<Vec<u8>, Error> {
        let target = self.resolve(path)?;
        match range {
            None => Ok(fs::read(&target)?),
            Some(r) => {
                if r.end < r.start {
                    return Err(Error::Invalid(format!(
                        "inverted range {}..{} for {path:?}",
                        r.start, r.end
                    )));
                }
                use std::io::{Seek, SeekFrom};
                let mut f = fs::File::open(&target)?;
                f.seek(SeekFrom::Start(r.start))?;
                let mut buf = vec![0u8; (r.end - r.start) as usize];
                f.read_exact(&mut buf)?;
                Ok(buf)
            }
        }
    }

    fn head_component(&self, path: &str) -> Result<u64, Error> {
        Ok(fs::metadata(self.resolve(path)?)?.len())
    }

    fn exists(&self, path: &str) -> Result<bool, Error> {
        Ok(self.resolve(path)?.exists())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_component() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalFileStore::new(dir.path()).unwrap();
        store.put_component("records/chunks.bin", b"hello").unwrap();
        assert!(store.exists("records/chunks.bin").unwrap());
        assert_eq!(store.head_component("records/chunks.bin").unwrap(), 5);
        assert_eq!(
            store.get_component("records/chunks.bin", None).unwrap(),
            b"hello"
        );
    }

    #[test]
    fn reads_a_range() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalFileStore::new(dir.path()).unwrap();
        store.put_component("a.bin", b"0123456789").unwrap();
        let got = store
            .get_component("a.bin", Some(ByteRange { start: 2, end: 5 }))
            .unwrap();
        assert_eq!(got, b"234");
    }

    /// The store re-normalizes rather than trusting its caller: this is the
    /// last check before a real filesystem write, and a traversal that only
    /// the parser catches is one an alternative path can skip.
    #[test]
    fn refuses_traversal_even_though_the_caller_should_have_checked() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalFileStore::new(dir.path()).unwrap();
        for bad in ["../escape.bin", "/etc/passwd", "a/../../b"] {
            assert!(
                store.put_component(bad, b"x").is_err(),
                "{bad} should be refused"
            );
            assert!(store.get_component(bad, None).is_err());
        }
    }
}
