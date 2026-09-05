// SPDX-License-Identifier: Apache-2.0
//! Deterministic shape validation: pure structural/semantic findings a
//! server can compute with zero model calls, mirroring
//! `munarium_runbooks::validate` (same severity vocabulary, same stable dotted
//! codes). `parse_shape` already hard-fails structurally broken documents;
//! this reports the semantic layer — including the honesty findings for
//! spec fields the engine parses but does not yet consume, the same
//! treatment `models.embedding-not-consumed` gives runbooks.

use crate::Shape;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// The shape should not be published as-is.
    Error,
    /// Legal, but likely not what the author wants.
    Warn,
    /// Advisory.
    Info,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShapeFinding {
    pub severity: Severity,
    /// Stable dotted code, e.g. "fact.key-pattern-allows-dots".
    pub code: String,
    pub message: String,
    /// YAML-ish path locating the finding, e.g. "spec.chunking.max_chars".
    pub path: String,
}

fn finding(severity: Severity, code: &str, message: String, path: String) -> ShapeFinding {
    ShapeFinding {
        severity,
        code: code.to_string(),
        message,
        path,
    }
}

/// Pure, deterministic validation of a parsed shape.
pub fn validate_shape(shape: &Shape) -> Vec<ShapeFinding> {
    let mut out = Vec::new();
    let doc = &shape.doc;

    if doc.metadata.name.trim().is_empty() {
        out.push(finding(
            Severity::Error,
            "metadata.name-empty",
            "shape name must be non-empty".into(),
            "metadata.name".into(),
        ));
    }
    if doc.metadata.version == 0 {
        out.push(finding(
            Severity::Warn,
            "metadata.version-zero",
            "version 0 is legal but unconventional; versions usually start at 1".into(),
            "metadata.version".into(),
        ));
    }

    // ---- fact schema ---------------------------------------------------
    match &doc.spec.fact {
        None => out.push(finding(
            Severity::Warn,
            "fact.schema-missing",
            "no fact schema — claims bearing this shape_ref validate against nothing".into(),
            "spec.fact".into(),
        )),
        Some(fact) => {
            if fact.schema.get("type").and_then(|t| t.as_str()) != Some("object") {
                out.push(finding(
                    Severity::Warn,
                    "fact.schema-not-object",
                    "fact.schema does not declare type: object — claim bodies are objects, \
                     so anything else constrains less than it appears to"
                        .into(),
                    "spec.fact.schema".into(),
                ));
            }
            // A dotted KEY silently steals from the subject: `subject.key`
            // splits at the LAST dot (munarium-core/src/ledger.rs), a bug paid
            // for once on a real corpus. Warn whenever the
            // key property's schema demonstrably ACCEPTS a dotted value —
            // absent or unconstraining patterns included.
            if key_schema_accepts_dots(&fact.schema) {
                out.push(finding(
                    Severity::Warn,
                    "fact.key-pattern-allows-dots",
                    "the key property accepts dotted values — `subject.key` splits at the \
                     LAST dot, so a dotted key silently steals from the subject; exclude \
                     '.' in the key pattern (dash/colon encode version-like parts)"
                        .into(),
                    "spec.fact.schema.properties.key".into(),
                ));
            }
            if fact
                .supersession
                .as_ref()
                .is_some_and(|s| !s.identity.is_empty())
            {
                out.push(finding(
                    Severity::Info,
                    "fact.identity-not-consumed",
                    "fact.supersession.identity is accepted but not yet consumed — the \
                     ledger's supersession identity is (subject, key) by construction; \
                     declaring it here documents intent only"
                        .into(),
                    "spec.fact.supersession.identity".into(),
                ));
            }
        }
    }

    // ---- chunking --------------------------------------------------------
    if let Some(chunking) = &doc.spec.chunking {
        if chunking.strategy != "para@1" {
            out.push(finding(
                Severity::Info,
                "chunking.strategy-inert",
                format!(
                    "chunking.strategy '{}' is accepted but not yet consumed — index \
                     builds always chunk para@1; only max_chars is honored",
                    chunking.strategy
                ),
                "spec.chunking.strategy".into(),
            ));
        }
        // parse_shape already hard-fails < 16; this is the "legal but
        // suspect" band on either side of every committed sample (900-2000).
        if chunking.max_chars < 200 || chunking.max_chars > 20_000 {
            out.push(finding(
                Severity::Warn,
                "chunking.max-chars-suspect",
                format!(
                    "max_chars {} is outside the 200..=20000 band every measured corpus \
                     uses — tiny chunks fragment retrieval context, huge ones dilute \
                     ranking",
                    chunking.max_chars
                ),
                "spec.chunking.max_chars".into(),
            ));
        }
    }

    // ---- indexing ----------------------------------------------------------
    if doc.spec.indexing.is_some() {
        out.push(finding(
            Severity::Info,
            "indexing.not-consumed",
            "spec.indexing is accepted but not yet consumed — retrieval fusion knobs \
             live in the runbook's spec.retrieval (query-time, no rebuild); this block \
             documents intent only"
                .into(),
            "spec.indexing".into(),
        ));
    }

    out
}

/// True when the fact schema's `key` property demonstrably accepts a dotted
/// value. Absent property schemas accept anything, so they count. A key
/// subschema that fails to compile is skipped (the whole-document schema
/// already compiled in `parse_shape`, so this is defensive only).
fn key_schema_accepts_dots(schema: &serde_json::Value) -> bool {
    let Some(key_schema) = schema.get("properties").and_then(|p| p.get("key")).cloned() else {
        return true; // unconstrained key: dots pass
    };
    match jsonschema::validator_for(&key_schema) {
        Ok(v) => v.validate(&serde_json::json!("dotted.key")).is_ok(),
        Err(_) => false,
    }
}

/// True when no Error-severity finding is present.
pub fn is_valid(findings: &[ShapeFinding]) -> bool {
    !findings.iter().any(|f| f.severity == Severity::Error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_shape;

    fn shape(spec: &str) -> Shape {
        parse_shape(&format!(
            "apiVersion: munarium.ioka.io/v1\nkind: Shape\nmetadata: {{ name: t, version: 1 }}\nspec:\n{spec}"
        ))
        .expect("parses")
    }

    const CLEAN_SPEC: &str = r#"  fact:
    schema:
      type: object
      properties:
        subject: { type: string, pattern: "^[a-z][a-z0-9_]{0,63}$" }
        key: { type: string, pattern: "^[a-z][a-z0-9_:-]{0,63}$" }
        value: { type: string, minLength: 1 }
      required: [subject, key, value]
  chunking: { max_chars: 1200 }
"#;

    #[test]
    fn a_sample_style_shape_is_clean() {
        let findings = validate_shape(&shape(CLEAN_SPEC));
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn empty_name_is_an_error() {
        let s = parse_shape(
            "apiVersion: munarium.ioka.io/v1\nkind: Shape\nmetadata: { name: \"\", version: 1 }\nspec: {}",
        )
        .expect("parses");
        let findings = validate_shape(&s);
        assert!(!is_valid(&findings));
        assert!(findings.iter().any(|f| f.code == "metadata.name-empty"));
    }

    #[test]
    fn version_zero_warns() {
        let s = parse_shape(
            "apiVersion: munarium.ioka.io/v1\nkind: Shape\nmetadata: { name: t, version: 0 }\nspec: {}",
        )
        .expect("parses");
        let findings = validate_shape(&s);
        assert!(is_valid(&findings));
        assert!(findings.iter().any(|f| f.code == "metadata.version-zero"));
    }

    #[test]
    fn missing_fact_schema_warns() {
        let findings = validate_shape(&shape("  chunking: { max_chars: 1200 }"));
        assert!(findings.iter().any(|f| f.code == "fact.schema-missing"));
        assert!(is_valid(&findings));
    }

    #[test]
    fn non_object_schema_warns() {
        let findings = validate_shape(&shape("  fact:\n    schema: { type: string }\n"));
        assert!(findings.iter().any(|f| f.code == "fact.schema-not-object"));
    }

    #[test]
    fn a_key_pattern_admitting_dots_warns() {
        // No pattern at all: dots pass.
        let findings = validate_shape(&shape(
            "  fact:\n    schema:\n      type: object\n      properties:\n        key: { type: string }\n",
        ));
        assert!(findings
            .iter()
            .any(|f| f.code == "fact.key-pattern-allows-dots"));
        // Missing key property entirely: unconstrained, still warns.
        let findings = validate_shape(&shape(
            "  fact:\n    schema:\n      type: object\n      properties: {}\n",
        ));
        assert!(findings
            .iter()
            .any(|f| f.code == "fact.key-pattern-allows-dots"));
        // The sample pattern excludes dots: no warning.
        let findings = validate_shape(&shape(CLEAN_SPEC));
        assert!(!findings
            .iter()
            .any(|f| f.code == "fact.key-pattern-allows-dots"));
    }

    #[test]
    fn supersession_identity_is_flagged_inert() {
        let findings = validate_shape(&shape(
            "  fact:\n    schema:\n      type: object\n      properties:\n        key: { type: string, pattern: \"^[a-z-]+$\" }\n    supersession: { identity: [subject, key] }\n",
        ));
        assert!(findings
            .iter()
            .any(|f| f.code == "fact.identity-not-consumed" && f.severity == Severity::Info));
    }

    #[test]
    fn non_default_chunking_strategy_is_flagged_inert() {
        let findings = validate_shape(&shape(
            "  chunking: { strategy: \"sent@2\", max_chars: 1200 }",
        ));
        assert!(findings
            .iter()
            .any(|f| f.code == "chunking.strategy-inert" && f.severity == Severity::Info));
        // Default strategy stays silent.
        let findings = validate_shape(&shape("  chunking: { max_chars: 1200 }"));
        assert!(!findings.iter().any(|f| f.code == "chunking.strategy-inert"));
    }

    #[test]
    fn suspect_max_chars_warns_on_both_sides() {
        for mc in [64usize, 50_000] {
            let findings = validate_shape(&shape(&format!("  chunking: {{ max_chars: {mc} }}")));
            assert!(
                findings
                    .iter()
                    .any(|f| f.code == "chunking.max-chars-suspect"),
                "expected warn for {mc}"
            );
        }
    }

    #[test]
    fn indexing_block_is_flagged_inert() {
        let findings = validate_shape(&shape("  indexing: { rrf_k: 60, candidate_n: 120 }"));
        assert!(findings
            .iter()
            .any(|f| f.code == "indexing.not-consumed" && f.severity == Severity::Info));
    }
}
