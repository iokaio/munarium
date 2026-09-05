// SPDX-License-Identifier: Apache-2.0
//! Source identity and the object-store seam.
//!
//! A *source* is a raw document the retrieval tier indexes. Its identity is
//! the **logical path** (the caller-supplied filename), NOT its content hash.
//! That distinction is load-bearing:
//!
//! - Runbook collections bind by `filenamePrefix`, a literal `starts_with`
//!   over the path. Identity therefore has to be the path, or a document
//!   uploaded to two prefixes could only ever bind to one of them.
//! - The same bytes may legitimately live at two paths — a policy document in
//!   both a smoke slice and a production data room — and each must be bound,
//!   rebuilt, and retired on its own schedule.
//!
//! `content_hash` remains as *integrity*: it is verified against any declared
//! sha-256 before commit, recorded on the row, and surfaced in provenance. It
//! is simply no longer the primary key.

use crate::{KernelError, Result};
use async_trait::async_trait;
use sha2::Digest as _;

/// Deterministic, stable, and derivable client-side without a round trip:
/// `"src-" + sha256("{tenant}/{path}")[..16]`.
pub fn source_id(tenant: &str, path: &str) -> String {
    let digest = sha2::Sha256::digest(format!("{tenant}/{path}").as_bytes());
    format!("src-{}", &hex::encode(digest)[..16])
}

/// A source's address: which tenant, which logical path, and the hash of the
/// bytes that path currently holds.
///
/// Constructing one validates the path, so every caller that reaches the
/// object store has already been through the check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceKey {
    pub tenant: String,
    pub path: String,
    pub content_hash: String,
}

impl SourceKey {
    pub fn new(tenant: &str, path: &str, content_hash: &str) -> Result<Self> {
        validate_path(path)?;
        if tenant.is_empty() || tenant.contains('/') {
            return Err(KernelError::InvalidInput(format!(
                "tenant '{tenant}' must be non-empty and must not contain '/'"
            )));
        }
        Ok(Self {
            tenant: tenant.to_string(),
            path: path.to_string(),
            content_hash: content_hash.to_string(),
        })
    }

    /// The blob name: `{tenant}/{path}`. The ONE place path composition
    /// happens, so the tenant prefix cannot be escaped from anywhere else.
    pub fn blob_name(&self) -> String {
        format!("{}/{}", self.tenant, self.path)
    }

    pub fn source_id(&self) -> String {
        source_id(&self.tenant, &self.path)
    }
}

/// Where document bytes actually live.
///
/// Deliberately narrow: bytes in, bytes out, addressed by logical path. The
/// metadata row (`sources`) is somebody else's job, which is what lets the
/// Azure backend, the Postgres fallback, and the in-memory test double be
/// swapped without any of them knowing about the ledger or the index.
#[async_trait]
pub trait SourceStore: Send + Sync {
    /// Write bytes at the key's logical path, overwriting whatever was there.
    /// Returns the backend-resolved URI recorded on the source row.
    async fn put(&self, key: &SourceKey, media_type: &str, bytes: &[u8]) -> Result<String>;

    async fn get(&self, key: &SourceKey) -> Result<Vec<u8>>;

    async fn exists(&self, key: &SourceKey) -> Result<bool>;

    /// Idempotent: deleting an absent blob is Ok.
    async fn delete(&self, key: &SourceKey) -> Result<()>;

    /// `az` | `pg` | `mem` — recorded on the source row so an operator can
    /// tell where a document's bytes went without guessing.
    fn backend_id(&self) -> &'static str;
}

/// Refuse a DOCUMENT path under the reserved evidence keyspace.
///
/// Sealed artifacts share this object store, so a document at `evidence/...`
/// could collide with an artifact's blob — and, worse, could tempt a reader
/// into inferring authorization from a path. Authorization comes from the
/// evidence row, never from where the bytes sit; reserving the prefix is what
/// keeps that true rather than merely intended.
///
/// Deliberately NOT folded into [`validate_path`]: the evidence writer builds
/// legitimate `SourceKey`s under this prefix, and a check it had to bypass
/// would be a back door in the one place a back door is least affordable.
/// This is a separate predicate that only the document ingress calls.
pub fn refuse_reserved_document_path(path: &str) -> Result<()> {
    if path.starts_with(crate::evidence::EVIDENCE_PATH_PREFIX) {
        return Err(KernelError::InvalidInput(format!(
            "source path '{path}' is under the reserved '{}' prefix; that keyspace belongs \
             to sealed evidence artifacts and cannot hold documents",
            crate::evidence::EVIDENCE_PATH_PREFIX
        )));
    }
    Ok(())
}

/// Reject anything that could escape the tenant prefix or produce an
/// ambiguous blob name. `filename` is entirely caller-supplied — the server
/// never rewrites it — so this is a security boundary, not tidiness.
pub fn validate_path(path: &str) -> Result<()> {
    let bad = |detail: &str| {
        Err(KernelError::InvalidInput(format!(
            "source path '{path}' is invalid: {detail}"
        )))
    };
    if path.is_empty() {
        return bad("empty");
    }
    if path.len() > 1024 {
        return bad("longer than 1024 bytes");
    }
    if path.starts_with('/') {
        return bad("absolute paths are not allowed");
    }
    if path.contains('\\') {
        return bad("backslashes are not allowed; use '/' separators");
    }
    if path.contains('\0') {
        return bad("contains a NUL byte");
    }
    // Windows drive-letter absolute paths ("C:/docs/x.md").
    if path.len() >= 2 && path.as_bytes()[1] == b':' {
        return bad("drive-qualified paths are not allowed");
    }
    for segment in path.split('/') {
        if segment.is_empty() {
            return bad("contains an empty path segment");
        }
        if segment == "." || segment == ".." {
            return bad("contains a '.' or '..' segment");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_id_is_deterministic_and_path_scoped() {
        assert_eq!(source_id("demo", "a/b.md"), source_id("demo", "a/b.md"));
        // The whole point: same bytes at two paths are two sources.
        assert_ne!(source_id("demo", "a/b.md"), source_id("demo", "c/b.md"));
        // And tenants never collide.
        assert_ne!(source_id("demo", "a/b.md"), source_id("other", "a/b.md"));
        assert!(source_id("demo", "a/b.md").starts_with("src-"));
        assert_eq!(source_id("demo", "a/b.md").len(), 20);
    }

    #[test]
    fn the_evidence_keyspace_is_refused_for_documents() {
        // The collision this prevents: a document whose path is an artifact's
        // blob name. Authorization is never inferred from a path, but a
        // COLLISION would still let one overwrite the other.
        let err = refuse_reserved_document_path("evidence/ev-abc").expect_err("must refuse");
        assert!(format!("{err}").contains("reserved"), "{err}");
        assert!(refuse_reserved_document_path("evidence/nested/x.md").is_err());

        // Ordinary paths are untouched, including ones that merely MENTION
        // evidence — the rule is a prefix, not a substring.
        refuse_reserved_document_path("northgate/evidence-summary.md").expect("ok");
        refuse_reserved_document_path("evidencelike/x.md").expect("ok");
        refuse_reserved_document_path("a/b/evidence/c.md").expect("ok");
    }

    #[test]
    fn blob_name_is_tenant_prefixed() {
        let key = SourceKey::new("demo", "northgate/03_finance/x.md", "abc").expect("valid");
        assert_eq!(key.blob_name(), "demo/northgate/03_finance/x.md");
    }

    #[test]
    fn traversal_and_absolute_paths_are_rejected() {
        for bad in [
            "../evil.md",
            "a/../../evil.md",
            "/etc/passwd",
            "C:/windows/x.md",
            "a\\b.md",
            "a//b.md",
            "./a.md",
            "",
        ] {
            assert!(
                SourceKey::new("demo", bad, "h").is_err(),
                "expected rejection for {bad:?}"
            );
        }
    }

    #[test]
    fn ordinary_paths_are_accepted() {
        for good in [
            "x.md",
            "northgate/03_finance/audited-fy2023.md",
            "rev/newspapers/sn83030214-1776-07-04.txt",
            "a.b/c-d_e/f.g.h",
        ] {
            assert!(
                SourceKey::new("demo", good, "h").is_ok(),
                "expected acceptance for {good:?}"
            );
        }
    }
}
