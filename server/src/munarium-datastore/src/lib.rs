// SPDX-License-Identifier: Apache-2.0
//! # Munarium Datastore
//!
//! Immutable, content-verified search artifacts: build, seal, verify, open,
//! query. A derived-index tier, **not** a replacement database — PostgreSQL
//! remains the system of record for tenants, sources, collections, build state,
//! active pointers and audit.
//!
//! ## The boundary
//!
//! This crate must remain independently usable: no Axum, tonic, SQLx,
//! PostgreSQL, server configuration, authentication or runbooks anywhere in its
//! dependency graph, and no dependency on `munarium-core`. CI checks it
//! (`server-ci.yml`, `gates.ps1`), because a boundary nothing
//! enforces is a boundary that lasts until the first convenient import.
//!
//! It does **not** own tenant authorization, collection membership, active
//! pointers, bindings, runbooks, sessions, extraction, chunking, or embedding
//! provider calls. The build input is a stream of ALREADY PREPARED chunks,
//! which is what keeps those responsibilities out: a crate that cannot see a
//! provider cannot acquire a provider concern.
//!
//! ## Identity
//!
//! - `index_version_id` = `idx2-` + sha256(canonical `BuildSpec`) — LOGICAL,
//!   and free of the engine, so an engine upgrade does not invalidate a
//!   session's pin.
//! - `artifact_id` = sha256(canonical `manifest.json`) — the ONE physical
//!   content identifier.
//!
//! `artifact_id` is a content identifier and **never an authority**. A
//! byte-identical manifest can legitimately occur in two tenants, so runtime
//! residency, hydration, eviction and quarantine all key on the full
//! [`ArtifactCacheKey`], and durable catalog keys carry the tenant separately.

pub mod canonical;
pub mod fusion;
pub mod hydrate;
#[cfg(feature = "lexical-tantivy")]
pub mod lexical;
pub mod model;
pub mod records;
pub mod routing;
pub mod shard;
pub mod stopwords;
pub mod store;
#[cfg(feature = "lexical-tantivy")]
pub mod tokenizer;
pub mod vector;
#[cfg(feature = "vector-diskann")]
pub mod vector_diskann;
pub mod verify;

use std::fmt;

/// What can go wrong before or during an open.
///
/// Variants are the CLASSES a caller acts on differently, not one per call
/// site: `Integrity` means quarantine the artifact, `Unsupported` means this
/// reader cannot serve it but a newer one might, `Limit` means the artifact is
/// bigger than this node was configured to accept. A single opaque error would
/// collapse three different operator responses into one.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A hash or a declared length did not match the bytes. Quarantine.
    #[error("integrity: {0}")]
    Integrity(String),
    /// Well-formed but not servable by THIS reader.
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// Refused before allocating, because a declared size exceeded a limit.
    #[error("limit: {0}")]
    Limit(String),
    /// A path that is not a normalized relative path.
    #[error("path: {0}")]
    Path(String),
    /// Structurally invalid: a manifest that contradicts itself.
    #[error("invalid: {0}")]
    Invalid(String),
    /// Canonicalization refused the value.
    #[error("canonical: {0}")]
    Canonical(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// One prepared chunk, the sole build input.
///
/// Server fans one extraction/embedding pass into the reference and datastore
/// sinks, so provider work happens once. `PreparedChunk` is the simple v1
/// shape; a borrowed or columnar `add_batch` may follow AFTER profiling
/// million-chunk builds, and would have to preserve the same stable order,
/// hashes, validation and logical version id.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedChunk {
    pub chunk_id: String,
    pub source_id: String,
    pub source_path: String,
    pub node_id: Option<String>,
    pub ordinal: u32,
    pub text: String,
    pub text_sha256: [u8; 32],
    pub embedding: Option<Vec<f32>>,
    pub metadata: std::collections::BTreeMap<String, String>,
}

/// The runtime residency and isolation key.
///
/// `isolation_domain` is OPAQUE here: this crate has no concept of a tenant and
/// must not acquire one. Server derives it from the already-authorized tenant
/// boundary and combines it with the logical version and the content id.
///
/// Every residency, lease, eviction, quarantine and cleanup lookup uses the
/// whole key. Cross-tenant cache coalescing is forbidden even when manifest
/// hashes match — which they legitimately will, since identical content in two
/// tenants is the same content. A caller must not be able to reach another
/// domain's data by presenting a known manifest hash.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ArtifactCacheKey {
    pub isolation_domain: String,
    pub logical_version_id: String,
    pub artifact_id: String,
}

impl ArtifactCacheKey {
    pub fn new(
        isolation_domain: impl Into<String>,
        logical_version_id: impl Into<String>,
        artifact_id: impl Into<String>,
    ) -> Self {
        Self {
            isolation_domain: isolation_domain.into(),
            logical_version_id: logical_version_id.into(),
            artifact_id: artifact_id.into(),
        }
    }

    /// A filesystem-safe relative path for the L1 leaf.
    ///
    /// Every element is checked rather than trusted: the domain and the logical
    /// version arrive from the caller, and a `..` in either would otherwise
    /// walk out of the cache root. That the caller is Server today is not a
    /// reason to skip it — this crate is meant to be usable without Server.
    pub fn l1_relative_path(&self) -> Result<String, Error> {
        for (label, part) in [
            ("isolation_domain", &self.isolation_domain),
            ("logical_version_id", &self.logical_version_id),
            ("artifact_id", &self.artifact_id),
        ] {
            if part.is_empty() {
                return Err(Error::Path(format!("{label} is empty")));
            }
            if !part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            {
                return Err(Error::Path(format!(
                    "{label} {part:?} may hold only [A-Za-z0-9_-]; it becomes a path element"
                )));
            }
        }
        Ok(format!(
            "{}/{}/{}",
            self.isolation_domain, self.logical_version_id, self.artifact_id
        ))
    }
}

impl fmt::Display for ArtifactCacheKey {
    /// Deliberately shows only a PREFIX of the artifact id and never the
    /// isolation domain in full: this ends up in logs and metrics, where a
    /// tenant-derived value must not be reconstructable.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let short = |s: &str| s.chars().take(12).collect::<String>();
        write!(
            f,
            "artifact({}…/{}/{}…)",
            short(&self.isolation_domain),
            self.logical_version_id,
            short(&self.artifact_id)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cache_key_refuses_path_traversal_in_every_element() {
        for bad in [
            ArtifactCacheKey::new("..", "idx2-abc", "deadbeef"),
            ArtifactCacheKey::new("dom", "../../etc", "deadbeef"),
            ArtifactCacheKey::new("dom", "idx2-abc", "a/b"),
            ArtifactCacheKey::new("", "idx2-abc", "deadbeef"),
        ] {
            assert!(
                bad.l1_relative_path().is_err(),
                "{bad:?} should not produce a path"
            );
        }
    }

    #[test]
    fn a_good_cache_key_produces_a_three_element_path() {
        let key = ArtifactCacheKey::new("dom-1", "idx2-abc", "deadbeef");
        assert_eq!(key.l1_relative_path().unwrap(), "dom-1/idx2-abc/deadbeef");
    }

    /// The property the isolation domain exists for: identical CONTENT in two
    /// domains is two distinct residency keys, so nothing they own is shared.
    #[test]
    fn identical_content_in_two_domains_is_two_keys() {
        let a = ArtifactCacheKey::new("dom-a", "idx2-v1", "samehash");
        let b = ArtifactCacheKey::new("dom-b", "idx2-v1", "samehash");
        assert_ne!(a, b);
        assert_ne!(a.l1_relative_path().unwrap(), b.l1_relative_path().unwrap());
    }

    #[test]
    fn display_does_not_leak_the_whole_isolation_domain() {
        let key = ArtifactCacheKey::new(
            "tenant-with-a-long-identifying-name",
            "idx2-v1",
            "0123456789abcdef0123456789abcdef",
        );
        let shown = key.to_string();
        assert!(!shown.contains("identifying-name"), "{shown}");
        assert!(shown.contains("idx2-v1"), "{shown}");
    }
}
