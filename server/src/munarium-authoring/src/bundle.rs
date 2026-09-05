// SPDX-License-Identifier: Apache-2.0
//! The export bundle: a self-contained, hash-manifested JSON document
//! carrying a validated shape+runbook set from an authoring server to a
//! production instance. Deliberately dependency-light — no tar, no zip:
//! `files` holds the YAML verbatim, `hashes` the per-file sha256, and
//! `manifest_hash` a deterministic digest over the sorted (path, hash)
//! pairs, so any drift between export and apply is detectable on any
//! machine with sha256. Applying needs NO new server surface: mmctl
//! (or curl in CI) POSTs each file in `apply_order` through the existing
//! kind-sniffed `/v1/shapes` and `/v1/runbooks` routes.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const BUNDLE_KIND: &str = "MunariumAuthoringBundle";
pub const BUNDLE_API_VERSION: &str = "munarium.ioka.io/v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bundle {
    pub kind: String,
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub tool: ToolInfo,
    pub draft_id: String,
    pub name: String,
    pub created_at: String,
    /// path -> YAML, verbatim.
    pub files: BTreeMap<String, String>,
    /// path -> sha256 hex of the file's bytes.
    pub hashes: BTreeMap<String, String>,
    /// Shapes before runbooks: a collection cannot bind an unpublished shape.
    pub apply_order: Vec<String>,
    /// sha256 over the byte-sorted "path\0hash\n" lines.
    pub manifest_hash: String,
    pub validation: ValidationSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ValidationSummary {
    pub valid: bool,
    pub errors: usize,
    pub warns: usize,
    pub infos: usize,
}

/// Deterministic manifest digest: order-independent of the JSON map, defined
/// over the byte-sorted `path\0hash\n` concatenation.
pub fn manifest_hash(hashes: &BTreeMap<String, String>) -> String {
    let mut buf = String::new();
    for (path, hash) in hashes {
        buf.push_str(path);
        buf.push('\0');
        buf.push_str(hash);
        buf.push('\n');
    }
    crate::sha256_hex(buf.as_bytes())
}

/// Build a bundle from a document set. `created_at` is caller-supplied (the
/// crate has no clock). Shapes apply before runbooks, each group in path
/// order.
pub fn build_bundle(
    draft_id: &str,
    name: &str,
    created_at: &str,
    tool_version: &str,
    docs: &BTreeMap<String, String>,
    validation: ValidationSummary,
) -> Bundle {
    let hashes: BTreeMap<String, String> = docs
        .iter()
        .map(|(p, y)| (p.clone(), crate::sha256_hex(y.as_bytes())))
        .collect();
    let mut apply_order: Vec<String> = Vec::new();
    for (path, yaml) in docs {
        if crate::doc_kind(yaml) == crate::DocKind::Shape {
            apply_order.push(path.clone());
        }
    }
    for (path, yaml) in docs {
        if crate::doc_kind(yaml) != crate::DocKind::Shape {
            apply_order.push(path.clone());
        }
    }
    let manifest = manifest_hash(&hashes);
    Bundle {
        kind: BUNDLE_KIND.into(),
        api_version: BUNDLE_API_VERSION.into(),
        tool: ToolInfo {
            name: "munarium-server".into(),
            version: tool_version.into(),
        },
        draft_id: draft_id.into(),
        name: name.into(),
        created_at: created_at.into(),
        files: docs.clone(),
        hashes,
        apply_order,
        manifest_hash: manifest,
        validation,
    }
}

/// Verify a bundle's internal consistency: kind, per-file hashes, manifest
/// hash, and that apply_order covers exactly the files. Returns the first
/// problem as a message.
pub fn verify_bundle(bundle: &Bundle) -> Result<(), String> {
    if bundle.kind != BUNDLE_KIND {
        return Err(format!("kind must be {BUNDLE_KIND}, got '{}'", bundle.kind));
    }
    if bundle.files.len() != bundle.hashes.len() {
        return Err("files and hashes disagree on the file set".into());
    }
    for (path, yaml) in &bundle.files {
        let actual = crate::sha256_hex(yaml.as_bytes());
        match bundle.hashes.get(path) {
            Some(declared) if *declared == actual => {}
            Some(declared) => {
                return Err(format!(
                    "'{path}' content does not match its declared hash \
                     (declared {declared}, actual {actual}) — bundle content drifted"
                ))
            }
            None => return Err(format!("'{path}' has no declared hash")),
        }
    }
    if manifest_hash(&bundle.hashes) != bundle.manifest_hash {
        return Err("manifest_hash does not match the declared hashes".into());
    }
    let mut ordered: Vec<&String> = bundle.apply_order.iter().collect();
    ordered.sort();
    let mut files: Vec<&String> = bundle.files.keys().collect();
    files.sort();
    if ordered != files {
        return Err("apply_order does not cover exactly the bundled files".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn docs() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("runbooks/t.yaml".into(), "kind: Runbook\n".into()),
            ("shapes/a.yaml".into(), "kind: Shape\n".into()),
            ("shapes/b.yaml".into(), "kind: Shape\n".into()),
        ])
    }

    fn summary() -> ValidationSummary {
        ValidationSummary {
            valid: true,
            errors: 0,
            warns: 0,
            infos: 0,
        }
    }

    #[test]
    fn shapes_apply_before_runbooks_and_verify_round_trips() {
        let b = build_bundle(
            "draft-1",
            "t",
            "2026-08-19T00:00:00Z",
            "0.1.2",
            &docs(),
            summary(),
        );
        assert_eq!(
            b.apply_order,
            vec!["shapes/a.yaml", "shapes/b.yaml", "runbooks/t.yaml"]
        );
        verify_bundle(&b).expect("verifies");
        // JSON round-trip preserves everything.
        let json = serde_json::to_string(&b).unwrap();
        let back: Bundle = serde_json::from_str(&json).unwrap();
        verify_bundle(&back).expect("verifies after round-trip");
    }

    #[test]
    fn manifest_hash_is_order_independent_and_content_sensitive() {
        let b = build_bundle("d", "t", "now", "v", &docs(), summary());
        // Rebuilding from the same docs yields the same manifest.
        let b2 = build_bundle("other", "t2", "later", "v2", &docs(), summary());
        assert_eq!(b.manifest_hash, b2.manifest_hash);
        // Any content change moves it.
        let mut changed = docs();
        changed.insert("shapes/a.yaml".into(), "kind: Shape\n# edited\n".into());
        let b3 = build_bundle("d", "t", "now", "v", &changed, summary());
        assert_ne!(b.manifest_hash, b3.manifest_hash);
    }

    #[test]
    fn tampering_is_detected() {
        let mut b = build_bundle("d", "t", "now", "v", &docs(), summary());
        b.files
            .insert("shapes/a.yaml".into(), "kind: Shape\n# tampered\n".into());
        let err = verify_bundle(&b).unwrap_err();
        assert!(err.contains("drifted"), "{err}");

        let mut b = build_bundle("d", "t", "now", "v", &docs(), summary());
        b.manifest_hash = "0".repeat(64);
        assert!(verify_bundle(&b).is_err());

        let mut b = build_bundle("d", "t", "now", "v", &docs(), summary());
        b.apply_order.pop();
        assert!(verify_bundle(&b).is_err());
    }
}
