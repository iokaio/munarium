// SPDX-License-Identifier: Apache-2.0
//! Strict verification, before anything is opened.
//!
//! Manifests and engine files are treated as untrusted even when fetched from
//! the configured account (§14). Everything here runs before an engine sees a
//! byte and before an untrusted length is used to reserve memory.

use std::collections::HashSet;

use crate::model::{ArtifactManifest, Component};
use crate::Error;

/// Limits applied to declared values BEFORE any allocation. A count in a
/// manifest is an allocation instruction unless it is bounded first.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_components: usize,
    pub max_component_bytes: u64,
    pub max_total_bytes: u64,
    pub max_chunks: u64,
    pub max_dimensions: u32,
}

impl Default for Limits {
    fn default() -> Self {
        // Deliberately finite rather than generous-and-round: a limit exists to
        // turn a hostile or corrupt manifest into a refusal instead of an
        // out-of-memory kill, and the operator can raise any of them knowingly.
        Self {
            max_components: 10_000,
            max_component_bytes: 64 * 1024 * 1024 * 1024,
            max_total_bytes: 256 * 1024 * 1024 * 1024,
            max_chunks: 1_000_000_000,
            max_dimensions: 65_536,
        }
    }
}

/// The reader's own supported envelope range. Startup and every binding change
/// reject an unsupported artifact BEFORE traffic is switched to it (§5.3).
#[derive(Debug, Clone)]
pub struct ReaderCapabilities {
    pub format_min: u32,
    pub format_max: u32,
    pub features: HashSet<String>,
}

impl ReaderCapabilities {
    pub fn v1() -> Self {
        #[allow(unused_mut)]
        let mut features: HashSet<String> = ["records.v1"].into_iter().map(String::from).collect();
        // Advertised ONLY when the engine is compiled in, so an approximate
        // artifact is refused at verification by a reader that cannot open
        // it — before any traffic is switched (§5.3).
        #[cfg(feature = "vector-diskann")]
        features.insert(crate::vector_diskann::FEATURE_BIT.to_string());
        Self {
            format_min: 1,
            format_max: 1,
            features,
        }
    }
}

/// Normalize and validate one component path.
///
/// Rejects absolute paths, `..`, drive prefixes, UNC and alternate data
/// streams, backslashes, empty and `.` segments, and control characters. The
/// returned path is the normalized relative form used for both the local file
/// and the object key, so the two cannot disagree about what a path means.
pub fn normalize_component_path(raw: &str) -> Result<String, Error> {
    if raw.is_empty() {
        return Err(Error::Path("empty component path".into()));
    }
    if raw.contains('\0') || raw.chars().any(|c| (c as u32) < 0x20) {
        return Err(Error::Path(format!("control character in path: {raw:?}")));
    }
    if raw.contains('\\') {
        return Err(Error::Path(format!(
            "backslash in path (paths are POSIX-relative, never host-shaped): {raw:?}"
        )));
    }
    if raw.starts_with('/') {
        return Err(Error::Path(format!("absolute path: {raw:?}")));
    }
    // `C:` or `C:/...`. Checked explicitly rather than by looking for ':'
    // anywhere, so a legitimate colon inside a filename is not refused.
    let bytes = raw.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return Err(Error::Path(format!("drive-qualified path: {raw:?}")));
    }
    // An alternate data stream rides on a second colon after a name.
    if raw.contains(':') {
        return Err(Error::Path(format!(
            "colon in path (alternate data stream or drive): {raw:?}"
        )));
    }

    let mut parts = Vec::new();
    for seg in raw.split('/') {
        match seg {
            "" => return Err(Error::Path(format!("empty path segment: {raw:?}"))),
            "." => return Err(Error::Path(format!("'.' segment: {raw:?}"))),
            ".." => {
                return Err(Error::Path(format!(
                    "'..' escapes the artifact prefix: {raw:?}"
                )))
            }
            s => parts.push(s),
        }
    }
    Ok(parts.join("/"))
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Validate a manifest's self-consistency and its fit with this reader.
///
/// Does not touch the filesystem or the network: this is the check that must
/// pass before either is used.
pub fn validate_manifest(
    manifest: &ArtifactManifest,
    reader: &ReaderCapabilities,
    limits: &Limits,
) -> Result<(), Error> {
    if manifest.manifest_version != 1 {
        return Err(Error::Unsupported(format!(
            "manifest_version {} (this reader understands 1)",
            manifest.manifest_version
        )));
    }
    if manifest.format_version < reader.format_min || manifest.format_version > reader.format_max {
        return Err(Error::Unsupported(format!(
            "envelope format_version {} outside this reader's range {}..={}",
            manifest.format_version, reader.format_min, reader.format_max
        )));
    }
    // Declared reader range must actually include this reader, and a MAJOR
    // format is not assumed readable by an older binary.
    if manifest.reader.min_version > reader.format_max
        || manifest.reader.max_version < reader.format_min
    {
        return Err(Error::Unsupported(format!(
            "artifact declares reader range {}..={}, incompatible with {}..={}",
            manifest.reader.min_version,
            manifest.reader.max_version,
            reader.format_min,
            reader.format_max
        )));
    }
    for feature in &manifest.reader.required_features {
        if !reader.features.contains(feature) {
            return Err(Error::Unsupported(format!(
                "artifact requires unknown feature {feature:?}"
            )));
        }
    }

    for (label, value) in [
        ("build_spec_sha256", &manifest.build_spec_sha256),
        ("artifact_plan_sha256", &manifest.artifact_plan_sha256),
    ] {
        if !is_hex64(value) {
            return Err(Error::Invalid(format!(
                "{label} is not 64 lowercase hex characters"
            )));
        }
    }

    if manifest.engines.is_empty() {
        return Err(Error::Invalid("manifest lists no engines".into()));
    }
    if manifest.components.is_empty() {
        return Err(Error::Invalid("manifest lists no components".into()));
    }
    if manifest.components.len() > limits.max_components {
        return Err(Error::Limit(format!(
            "{} components exceeds the limit of {}",
            manifest.components.len(),
            limits.max_components
        )));
    }
    if manifest.probes.is_empty() {
        return Err(Error::Invalid(
            "manifest declares no verification probes; checksums alone cannot show the index answers"
                .into(),
        ));
    }

    let mut seen: HashSet<String> = HashSet::new();
    let mut total: u64 = 0;
    for c in &manifest.components {
        let norm = normalize_component_path(&c.path)?;
        if norm != c.path {
            return Err(Error::Path(format!(
                "component path {:?} is not already normalized (normalizes to {norm:?})",
                c.path
            )));
        }
        if !seen.insert(norm.clone()) {
            return Err(Error::Path(format!("duplicate component path: {norm:?}")));
        }
        if !is_hex64(&c.sha256) {
            return Err(Error::Invalid(format!(
                "component {norm:?} sha256 is not 64 lowercase hex characters"
            )));
        }
        if c.bytes_len > limits.max_component_bytes {
            return Err(Error::Limit(format!(
                "component {norm:?} declares {} bytes, over the {} limit",
                c.bytes_len, limits.max_component_bytes
            )));
        }
        total = total.checked_add(c.bytes_len).ok_or_else(|| {
            Error::Limit("declared component sizes overflow a u64 in total".into())
        })?;
    }
    if total > limits.max_total_bytes {
        return Err(Error::Limit(format!(
            "declared total {total} bytes over the {} limit",
            limits.max_total_bytes
        )));
    }

    if manifest.counts.chunks > limits.max_chunks {
        return Err(Error::Limit(format!(
            "declares {} chunks, over the {} limit",
            manifest.counts.chunks, limits.max_chunks
        )));
    }
    if let Some(d) = manifest.counts.dimensions {
        if d == 0 || d > limits.max_dimensions {
            return Err(Error::Limit(format!(
                "declares {d} dimensions, outside 1..={}",
                limits.max_dimensions
            )));
        }
    }
    // A vector count without a dimension (or the reverse) describes an artifact
    // no reader can open, and is a defect in whatever wrote it.
    match (manifest.counts.vectors, manifest.counts.dimensions) {
        (Some(_), None) => {
            return Err(Error::Invalid("vectors declared with no dimensions".into()))
        }
        (None, Some(_)) => {
            return Err(Error::Invalid("dimensions declared with no vectors".into()))
        }
        _ => {}
    }

    if let Some(rm) = &manifest.range_map {
        let norm = normalize_component_path(&rm.path)?;
        if !seen.contains(&norm) {
            return Err(Error::Invalid(format!(
                "range map {norm:?} is not listed as a component, so its bytes are unhashed"
            )));
        }
    }
    Ok(())
}

/// Confirm that manifest bytes hash to the artifact id they are claimed under.
///
/// The open path must call this on the bytes it FETCHED, never on a
/// re-serialization of a parsed manifest: re-serializing would verify this
/// process's encoder against itself and prove nothing about what was stored.
pub fn verify_manifest_bytes(bytes: &[u8], expected_artifact_id: &str) -> Result<(), Error> {
    use sha2::{Digest, Sha256};
    let got = hex::encode(Sha256::digest(bytes));
    if got != expected_artifact_id {
        return Err(Error::Integrity(format!(
            "manifest bytes hash to {got}, not the expected artifact id {expected_artifact_id}"
        )));
    }
    Ok(())
}

/// Check one component's bytes against its declared length and hash.
pub fn verify_component(component: &Component, bytes: &[u8]) -> Result<(), Error> {
    use sha2::{Digest, Sha256};
    if bytes.len() as u64 != component.bytes_len {
        return Err(Error::Integrity(format!(
            "component {:?} is {} bytes, manifest declares {}",
            component.path,
            bytes.len(),
            component.bytes_len
        )));
    }
    let got = hex::encode(Sha256::digest(bytes));
    if got != component.sha256 {
        return Err(Error::Integrity(format!(
            "component {:?} hashes to {got}, manifest declares {}",
            component.path, component.sha256
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn good_paths_normalize_to_themselves() {
        for p in ["manifest.json", "records/chunks.bin", "lexical/meta.json"] {
            assert_eq!(normalize_component_path(p).unwrap(), p);
        }
    }

    #[test]
    fn hostile_paths_are_refused() {
        let cases = [
            ("../../etc/passwd", "'..'"),
            ("a/../../b", "'..'"),
            ("/etc/passwd", "absolute"),
            ("C:/windows", "drive"),
            ("c:", "drive"),
            ("lexical\\meta.json", "backslash"),
            ("a//b", "empty path segment"),
            ("./a", "'.'"),
            ("a/./b", "'.'"),
            ("file.txt:stream", "colon"),
            ("", "empty component path"),
        ];
        for (path, expect) in cases {
            let err = normalize_component_path(path)
                .unwrap_err()
                .to_string()
                .to_lowercase();
            assert!(
                err.contains(&expect.to_lowercase()),
                "path {path:?} gave {err:?}, expected mention of {expect:?}"
            );
        }
    }

    #[test]
    fn a_control_character_is_refused() {
        assert!(normalize_component_path("a\u{7}b").is_err());
        assert!(normalize_component_path("a\nb").is_err());
    }

    #[test]
    fn manifest_bytes_must_hash_to_the_claimed_id() {
        let bytes = b"{}";
        use sha2::Digest as _;
        let real = hex::encode(sha2::Sha256::digest(bytes));
        assert!(verify_manifest_bytes(bytes, &real).is_ok());
        let err = verify_manifest_bytes(bytes, &"0".repeat(64)).unwrap_err();
        assert!(err.to_string().contains("hash to"), "{err}");
    }
}
