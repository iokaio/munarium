// SPDX-License-Identifier: Apache-2.0
//! The generated Matrix pin agrees with the publisher's consumer export.
//!
//! `src/matrix_pin.rs` and `contract/matrix-consumer/matrix-consumer.json` are both
//! written by `server/tools/matrix_pin.py` from one run of the Matrix publisher
//! (ClearView M1 step 3). This test needs no Matrix checkout: it proves the
//! constants the Server compiles against are the ones in the tracked export, that
//! the export is the file the pin hashed, and that the export's own
//! `profile_digest` still covers its profile. Tying the export to the publisher
//! revision is `matrix_pin.py --check`'s job.

use std::path::{Path, PathBuf};

use munarium_core::evidence::CONTRACT_VERSION;
use munarium_core::matrix_pin;
use serde_json::Value;
use sha2::{Digest, Sha256};

fn export_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../contract/matrix-consumer/matrix-consumer.json")
}

fn export_bytes() -> Vec<u8> {
    std::fs::read(export_path()).expect(
        "contract/matrix-consumer/matrix-consumer.json must exist; generate it with \
         `py tools/matrix_pin.py --write --review <status>` from server/",
    )
}

fn parse(bytes: &[u8]) -> Value {
    let text = std::str::from_utf8(bytes).expect("the export is UTF-8");
    assert!(
        !text.starts_with('\u{feff}'),
        "the export begins with a BOM"
    );
    serde_json::from_str(text).expect("the export is JSON")
}

/// `json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)`,
/// the serialization the publisher documents for `profile_digest`.
fn canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                string(key, out);
                out.push(':');
                canonical(&map[key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical(item, out);
            }
            out.push(']');
        }
        Value::String(s) => string(s, out),
        Value::Number(n) => {
            assert!(
                n.is_i64() || n.is_u64(),
                "the profile carries only integers"
            );
            out.push_str(&n.to_string());
        }
        other => out.push_str(&other.to_string()),
    }
}

fn string(s: &str, out: &mut String) {
    let quoted = serde_json::to_string(s).expect("a string serializes");
    for c in quoted.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
}

fn profile_digest(profile: &Value) -> String {
    let mut text = String::new();
    canonical(profile, &mut text);
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// Every reason `bytes` is not the export this build was pinned to.
fn pin_problems(bytes: &[u8]) -> Vec<String> {
    let mut problems = Vec::new();
    if hex::encode(Sha256::digest(bytes)) != matrix_pin::EXPORT_SHA256 {
        problems.push("export sha256 differs from EXPORT_SHA256".to_string());
    }
    let export = parse(bytes);
    let profile = &export["profile"];
    let wire = &profile["wire_contract"];
    let checks = [
        (
            "profile_digest",
            export["profile_digest"].as_str(),
            matrix_pin::PROFILE_DIGEST,
        ),
        (
            "contract_version",
            wire["contract_version"].as_str(),
            matrix_pin::CONTRACT_VERSION,
        ),
        (
            "bundle_digest",
            wire["bundle_digest"].as_str(),
            matrix_pin::BUNDLE_DIGEST,
        ),
        (
            "verified_source_commit",
            wire["verified_source_commit"].as_str(),
            matrix_pin::VERIFIED_SOURCE_COMMIT,
        ),
    ];
    for (name, exported, pinned) in checks {
        if exported != Some(pinned) {
            problems.push(format!("{name}: export {exported:?}, pin {pinned:?}"));
        }
    }
    if export["profile_digest"].as_str() != Some(profile_digest(profile).as_str()) {
        problems.push("profile_digest does not cover the profile".to_string());
    }
    problems
}

#[test]
fn the_pin_matches_the_tracked_export() {
    let problems = pin_problems(&export_bytes());
    assert!(
        problems.is_empty(),
        "the generated pin and the tracked export disagree; regenerate both with \
         `py tools/matrix_pin.py --write`: {problems:?}"
    );
}

#[test]
fn the_evidence_plane_speaks_the_pinned_version() {
    assert_eq!(CONTRACT_VERSION, matrix_pin::CONTRACT_VERSION);
    assert_eq!(CONTRACT_VERSION.trim(), CONTRACT_VERSION);
    let parts: Vec<&str> = CONTRACT_VERSION.split('.').collect();
    assert_eq!(parts.len(), 3, "bare semver, got {CONTRACT_VERSION:?}");
    assert!(parts
        .iter()
        .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())));
}

#[test]
fn the_pin_names_full_revisions_and_digests() {
    for (name, value, len) in [
        ("BUNDLE_DIGEST", matrix_pin::BUNDLE_DIGEST, 64),
        ("EXPORT_SHA256", matrix_pin::EXPORT_SHA256, 64),
        ("PROFILE_DIGEST", matrix_pin::PROFILE_DIGEST, 64),
        (
            "VERIFIED_SOURCE_COMMIT",
            matrix_pin::VERIFIED_SOURCE_COMMIT,
            40,
        ),
        ("PUBLISHER_REVISION", matrix_pin::PUBLISHER_REVISION, 40),
    ] {
        assert!(
            value.len() == len
                && value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "{name} must be {len} lowercase hex, got {value:?}"
        );
    }
    assert!(!matrix_pin::PUBLISHER_REVIEW.is_empty());
}

// Negative controls: each edit to the export must be refused.

fn edited(edit: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut export = parse(&export_bytes());
    edit(&mut export);
    let mut bytes = serde_json::to_vec_pretty(&export).expect("serialize");
    bytes.push(b'\n');
    bytes
}

#[test]
fn a_changed_byte_is_refused() {
    let mut bytes = export_bytes();
    bytes.push(b' ');
    assert!(pin_problems(&bytes)
        .iter()
        .any(|p| p.contains("EXPORT_SHA256")));
}

#[test]
fn a_changed_route_is_refused() {
    let bytes = edited(|e| {
        e["profile"]["mmctl_matrix"]["commands"]["version"]["path"] = "/elsewhere".into();
    });
    let problems = pin_problems(&bytes);
    assert!(
        problems.iter().any(|p| p.contains("does not cover")),
        "{problems:?}"
    );
}

#[test]
fn a_changed_wire_version_or_digest_is_refused() {
    let bytes = edited(|e| {
        e["profile"]["wire_contract"]["contract_version"] = "0.2.0".into();
        e["profile"]["wire_contract"]["bundle_digest"] = "0".repeat(64).into();
    });
    let problems = pin_problems(&bytes);
    assert!(
        problems.iter().any(|p| p.starts_with("contract_version")),
        "{problems:?}"
    );
    assert!(
        problems.iter().any(|p| p.starts_with("bundle_digest")),
        "{problems:?}"
    );
}

#[test]
fn the_canonical_form_matches_the_publisher_rule() {
    let value: Value =
        serde_json::from_str(r#"{"b":[1,"\u00e9"],"a":{"d":"x","c":null}}"#).expect("json");
    let mut text = String::new();
    canonical(&value, &mut text);
    assert_eq!(text, r#"{"a":{"c":null,"d":"x"},"b":[1,"\u00e9"]}"#);
}
