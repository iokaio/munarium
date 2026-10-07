// SPDX-License-Identifier: Apache-2.0
//! Pinned owner-maintained Warden principal verification for Server.
#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented
    )
)]
#[path = "upstream/identity-core/lib.rs"]
mod upstream;
pub use upstream::*;

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};
    #[test]
    fn exported_owner_source_matches_its_lock() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("upstream");
        let lock: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("source-lock.json")).unwrap()).unwrap();
        let files = lock["files"].as_object().unwrap();
        assert_eq!(files.len(), 6);
        for (path, digest) in files {
            assert_eq!(
                format!(
                    "{:x}",
                    Sha256::digest(std::fs::read(root.join(path)).unwrap())
                ),
                digest.as_str().unwrap()
            );
        }
    }
}
