// SPDX-License-Identifier: Apache-2.0
//! Guided runbook-set authoring.
//!
//! A licensee with only a server deployment has the enforcement half of
//! runbook design (the deterministic validators, the `?suggest=true` AI
//! review) but not the authoring half. This crate is that half, pure and
//! model-free:
//!
//! - [`catalog`] — the seven measured application patterns
//!   (dev-guide §19), each naming the committed exemplar to start from,
//!   with every `runbooks/{experiments,shapes,pipelines}` sample embedded.
//! - [`interview`] — the design questions of dev-guide §16 in §16's order
//!   (ordered by how hard each decision is to revise), with the guidance
//!   prose attached to the question instead of a chapter away.
//! - [`materialize`] — deterministic answers → shape + runbook YAML,
//!   proven by re-parsing through `parse_shape`/`parse_runbook`.
//! - [`setcheck`] — cross-document validation over a whole draft set
//!   (`set.*` codes), layered on the per-document validators.
//! - [`bundle`] — the hash-manifested export format CI applies to a
//!   production instance through the existing `/v1/shapes` +
//!   `/v1/runbooks` routes.
//!
//! The server layer (`munarium-server/src/authoring_api.rs`) owns drafts
//! persistence and the BYOK assist call; nothing here does I/O.

pub mod bundle;
pub mod catalog;
pub mod interview;
pub mod materialize;
pub mod setcheck;

pub use munarium_runbooks::validate::Severity;
use serde::Serialize;

/// One finding, unified across shape / runbook / set validation so a whole
/// draft set reports through a single vocabulary. Same fields as
/// `munarium_runbooks::validate::ValidationFinding`.
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub severity: Severity,
    /// Stable dotted code, e.g. "set.shape-unresolved".
    pub code: String,
    pub message: String,
    /// YAML-ish path locating the finding within its document ("$" = whole doc).
    pub path: String,
}

impl From<munarium_runbooks::validate::ValidationFinding> for Finding {
    fn from(f: munarium_runbooks::validate::ValidationFinding) -> Self {
        Finding {
            severity: f.severity,
            code: f.code,
            message: f.message,
            path: f.path,
        }
    }
}

impl From<munarium_shapes::validate::ShapeFinding> for Finding {
    fn from(f: munarium_shapes::validate::ShapeFinding) -> Self {
        Finding {
            severity: match f.severity {
                munarium_shapes::validate::Severity::Error => Severity::Error,
                munarium_shapes::validate::Severity::Warn => Severity::Warn,
                munarium_shapes::validate::Severity::Info => Severity::Info,
            },
            code: f.code,
            message: f.message,
            path: f.path,
        }
    }
}

pub(crate) fn finding(severity: Severity, code: &str, message: String, path: String) -> Finding {
    Finding {
        severity,
        code: code.to_string(),
        message,
        path,
    }
}

/// sha256 hex of a document's bytes — the one hashing convention the bundle,
/// the set checks, and the server all share.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    hex::encode(sha2::Sha256::digest(bytes))
}

/// A document's declared kind, read by PARSING the YAML — never by
/// substring sniffing. A runbook whose completion template mentions
/// "kind: Shape" must not be routed as a shape. `Unknown` covers YAML that
/// does not parse or declares neither kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    Shape,
    Runbook,
    Unknown,
}

pub fn doc_kind(yaml: &str) -> DocKind {
    #[derive(serde::Deserialize)]
    struct KindOnly {
        #[serde(default)]
        kind: String,
    }
    match serde_yaml::from_str::<KindOnly>(yaml) {
        Ok(k) if k.kind == "Shape" => DocKind::Shape,
        Ok(k) if k.kind == "Runbook" => DocKind::Runbook,
        _ => DocKind::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_kind_parses_rather_than_sniffs() {
        assert_eq!(
            doc_kind("apiVersion: v1\nkind: Shape\nmetadata: { name: s, version: 1 }\n"),
            DocKind::Shape
        );
        // A runbook whose prompt template MENTIONS "kind: Shape" is a runbook.
        let runbook = "kind: Runbook\nmetadata: { name: t, version: 1 }\nspec:\n  \
                       completion:\n    promptTemplate: |\n      Documents here follow \
                       kind: Shape schemas. {context} {query}\n";
        assert_eq!(doc_kind(runbook), DocKind::Runbook);
        assert_eq!(doc_kind(":::not yaml"), DocKind::Unknown);
        assert_eq!(doc_kind("kind: ProviderConfig\n"), DocKind::Unknown);
    }
}
