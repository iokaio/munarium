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

    /// Read a whole component together with the stored generation of the
    /// object the bytes came from (ClearView ART1/ART2).
    ///
    /// The generation is the store's own object version, an ETag or a
    /// version id, taken from the SAME read that returned the bytes, so it
    /// names exactly the object that was hashed. A store that reports no
    /// stable generation returns `None`, which is what the default does:
    /// the bytes of [`ArtifactStore::get_component`] with no generation.
    /// Callers must treat `None` as "unknown", never as equal to a known
    /// generation. Use [`storage_generation_token`] to form the value.
    fn get_component_with_generation(&self, path: &str) -> Result<StoredComponent, Error> {
        Ok(StoredComponent {
            bytes: self.get_component(path, None)?,
            generation: None,
        })
    }
}

/// A whole component and the stored generation it was read at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredComponent {
    pub bytes: Vec<u8>,
    /// See [`ArtifactStore::get_component_with_generation`].
    pub generation: Option<String>,
}

/// The longest generation token [`storage_generation_token`] returns. The
/// damage ledger accepts at most 128 bytes.
pub const STORAGE_GENERATION_MAX: usize = 128;

/// Form a bounded, comparable generation token from what a store reported.
///
/// A version id is preferred over an ETag: where a store reports one it
/// identifies the stored object exactly. The token is prefixed with its kind
/// (`version:` or `etag:`), so a version id never compares equal to an ETag
/// that happens to have the same text. A value that is empty after trimming
/// is absent. A value too long for the ledger, or one carrying control
/// characters, is replaced by `sha256:` and the hex digest of the reported
/// text, which still compares equal exactly when the reported values do.
pub fn storage_generation_token(version: Option<&str>, e_tag: Option<&str>) -> Option<String> {
    use sha2::{Digest, Sha256};
    fn pick(v: Option<&str>) -> Option<&str> {
        v.map(str::trim).filter(|v| !v.is_empty())
    }
    let (kind, raw) = match (pick(version), pick(e_tag)) {
        (Some(v), _) => ("version", v),
        (None, Some(e)) => ("etag", e),
        (None, None) => return None,
    };
    let token = format!("{kind}:{raw}");
    if token.len() <= STORAGE_GENERATION_MAX && !token.chars().any(char::is_control) {
        Some(token)
    } else {
        Some(format!(
            "sha256:{}",
            hex::encode(Sha256::digest(token.as_bytes()))
        ))
    }
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

    /// A local directory reports no stable generation: a whole read carries
    /// none, which is the trait's default and the pre-ClearView behavior.
    #[test]
    fn the_local_store_reports_no_generation() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalFileStore::new(dir.path()).unwrap();
        store.put_component("a.bin", b"abc").unwrap();
        let got = store.get_component_with_generation("a.bin").unwrap();
        assert_eq!(got.bytes, b"abc");
        assert_eq!(got.generation, None);
    }

    #[test]
    fn generation_tokens_are_kinded_bounded_and_prefer_the_version() {
        assert_eq!(storage_generation_token(None, None), None);
        assert_eq!(storage_generation_token(Some("  "), Some("")), None);
        assert_eq!(
            storage_generation_token(None, Some("\"0x8D\"")).as_deref(),
            Some("etag:\"0x8D\"")
        );
        assert_eq!(
            storage_generation_token(Some("v7"), Some("\"e\"")).as_deref(),
            Some("version:v7")
        );
        // The same text as a version and as an ETag is two generations.
        assert_ne!(
            storage_generation_token(Some("x"), None),
            storage_generation_token(None, Some("x"))
        );
        let long = "z".repeat(400);
        let a = storage_generation_token(Some(&long), None).unwrap();
        assert!(
            a.starts_with("sha256:") && a.len() <= STORAGE_GENERATION_MAX,
            "{a}"
        );
        assert_eq!(Some(a), storage_generation_token(Some(&long), None));
        let control = storage_generation_token(None, Some("a\nb")).unwrap();
        assert!(control.starts_with("sha256:"), "{control}");
        assert_ne!(
            storage_generation_token(Some(&long), None),
            storage_generation_token(Some(&"y".repeat(400)), None)
        );
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
