// SPDX-License-Identifier: Apache-2.0
//! Set-level validation: the cross-document layer neither per-document
//! validator can see. A draft is a SET — shapes plus a runbook that binds
//! them — and the mistakes that survive per-document validation are
//! exactly the cross-document ones: a collection naming a shape nobody
//! publishes, a shape version colliding with what production already
//! holds, an answer key bound into a retrieval index.
//!
//! Pure: the caller supplies the published-shape map (`ref -> yaml_hash`)
//! so the crate never touches a registry.

use crate::{doc_kind, finding, DocKind, Finding, Severity};
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone)]
pub struct SetValidation {
    /// Per-document findings (parse + the document's own validator), keyed
    /// by the document's path in the set.
    pub per_doc: BTreeMap<String, Vec<Finding>>,
    /// Cross-document findings (`set.*` codes).
    pub set: Vec<Finding>,
    /// False when any Error-severity finding exists anywhere in the set.
    pub valid: bool,
}

struct ParsedShape {
    path: String,
    shape_ref: String,
    name: String,
    yaml_hash: String,
}

struct ParsedRunbook {
    path: String,
    doc: munarium_runbooks::RunbookDoc,
}

/// Validate a whole document set. `published_shapes` maps `name@version` to
/// the published yaml_hash on the target server (empty when authoring
/// offline). Document kinds are PARSED (`crate::doc_kind`); YAML too broken
/// to parse falls back to a substring hint only to pick which parser's
/// error message to report.
pub fn validate_set(
    docs: &BTreeMap<String, String>,
    published_shapes: &HashMap<String, String>,
) -> SetValidation {
    let mut per_doc: BTreeMap<String, Vec<Finding>> = BTreeMap::new();
    let mut set: Vec<Finding> = Vec::new();
    let mut shapes: Vec<ParsedShape> = Vec::new();
    let mut runbooks: Vec<ParsedRunbook> = Vec::new();
    let mut claims_runbook = false;

    // ---- per-document pass ---------------------------------------------------
    for (path, yaml) in docs {
        let findings = per_doc.entry(path.clone()).or_default();
        // Parse-based kind; the substring fallback exists ONLY so a
        // syntactically broken document still gets the right parser's
        // error message instead of a generic one.
        let kind = match doc_kind(yaml) {
            DocKind::Unknown if yaml.contains("kind: Runbook") => DocKind::Runbook,
            DocKind::Unknown if yaml.contains("kind: Shape") => DocKind::Shape,
            k => k,
        };
        if kind == DocKind::Shape {
            match munarium_shapes::parse_shape(yaml) {
                Ok(shape) => {
                    findings.extend(
                        munarium_shapes::validate::validate_shape(&shape)
                            .into_iter()
                            .map(Finding::from),
                    );
                    if !valid_name(&shape.doc.metadata.name) {
                        findings.push(finding(
                            Severity::Error,
                            "set.name-invalid",
                            format!(
                                "shape name '{}' must match ^[a-z0-9][a-z0-9-]*$ \
                                 ('@' is the name/version separator in refs)",
                                shape.doc.metadata.name
                            ),
                            "metadata.name".into(),
                        ));
                    }
                    shapes.push(ParsedShape {
                        path: path.clone(),
                        shape_ref: shape.shape_ref(),
                        name: shape.doc.metadata.name.clone(),
                        yaml_hash: shape.yaml_hash.clone(),
                    });
                }
                Err(e) => findings.push(finding(Severity::Error, "parse", e, "$".into())),
            }
        } else if kind == DocKind::Runbook {
            claims_runbook = true;
            match munarium_runbooks::parse_runbook(yaml) {
                Ok(doc) => {
                    findings.extend(
                        munarium_runbooks::validate::validate_runbook(&doc)
                            .into_iter()
                            .map(Finding::from),
                    );
                    if !valid_name(&doc.metadata.name) {
                        findings.push(finding(
                            Severity::Error,
                            "set.name-invalid",
                            format!(
                                "runbook name '{}' must match ^[a-z0-9][a-z0-9-]*$",
                                doc.metadata.name
                            ),
                            "metadata.name".into(),
                        ));
                    }
                    runbooks.push(ParsedRunbook {
                        path: path.clone(),
                        doc,
                    });
                }
                Err(e) => findings.push(finding(Severity::Error, "parse", e, "$".into())),
            }
        } else {
            findings.push(finding(
                Severity::Error,
                "parse",
                "document declares neither kind: Shape nor kind: Runbook".into(),
                "$".into(),
            ));
        }
    }

    // ---- set pass -----------------------------------------------------------
    if !claims_runbook {
        set.push(finding(
            Severity::Error,
            "set.no-runbook",
            "the set contains no runbook — nothing binds these shapes to a corpus".into(),
            "$".into(),
        ));
    }

    // Name collisions: same kind + name@version, different bytes.
    let mut seen_shape: HashMap<String, (String, String)> = HashMap::new(); // ref -> (path, hash)
    for s in &shapes {
        if let Some((other_path, other_hash)) = seen_shape.get(&s.shape_ref) {
            if *other_hash != s.yaml_hash {
                set.push(finding(
                    Severity::Error,
                    "set.name-collision",
                    format!(
                        "shape '{}' appears in both '{other_path}' and '{}' with \
                         different content — one ref, one content",
                        s.shape_ref, s.path
                    ),
                    s.path.clone(),
                ));
            }
        } else {
            seen_shape.insert(s.shape_ref.clone(), (s.path.clone(), s.yaml_hash.clone()));
        }
        // Additive-versioning preflight against the target server.
        if let Some(published_hash) = published_shapes.get(&s.shape_ref) {
            if *published_hash != s.yaml_hash {
                set.push(finding(
                    Severity::Error,
                    "set.shape-version-conflict",
                    format!(
                        "shape '{}' is already published with different content — \
                         additive versioning refuses the overwrite; bump the version",
                        s.shape_ref
                    ),
                    s.path.clone(),
                ));
            }
        }
    }
    let mut seen_runbook: HashMap<String, String> = HashMap::new(); // ref -> path
    for r in &runbooks {
        let rref = r.doc.runbook_ref();
        if let Some(other) = seen_runbook.get(&rref) {
            set.push(finding(
                Severity::Error,
                "set.name-collision",
                format!(
                    "runbook '{rref}' appears in both '{other}' and '{}'",
                    r.path
                ),
                r.path.clone(),
            ));
        } else {
            seen_runbook.insert(rref, r.path.clone());
        }
    }

    // Shape resolution + usage.
    let mut used_shapes: Vec<&str> = Vec::new();
    for r in &runbooks {
        for (i, col) in r.doc.spec.collections.iter().enumerate() {
            let resolved = if col.shape.contains('@') {
                shapes.iter().any(|s| s.shape_ref == col.shape)
                    || published_shapes.contains_key(&col.shape)
            } else {
                // Unversioned ref (already warned per-document): resolve by name.
                shapes.iter().any(|s| s.name == col.shape)
                    || published_shapes
                        .keys()
                        .any(|k| k.split('@').next() == Some(col.shape.as_str()))
            };
            if resolved {
                used_shapes.push(col.shape.as_str());
            } else {
                set.push(finding(
                    Severity::Error,
                    "set.shape-unresolved",
                    format!(
                        "collection '{}' binds shape '{}', which is neither in this set \
                         nor published on the server — apply would fail at \
                         collection materialization",
                        col.name, col.shape
                    ),
                    format!("{}: spec.collections[{i}].shape", r.path),
                ));
            }
        }
        check_prefix_shadowing(r, &mut set);
        check_answer_key_bindings(r, &mut set);
    }
    for s in &shapes {
        let used = used_shapes
            .iter()
            .any(|u| *u == s.shape_ref || *u == s.name);
        if !used && claims_runbook {
            set.push(finding(
                Severity::Warn,
                "set.shape-unused",
                format!(
                    "shape '{}' is referenced by no collection in this set",
                    s.shape_ref
                ),
                s.path.clone(),
            ));
        }
    }

    let valid = !set.iter().any(|f| f.severity == Severity::Error)
        && !per_doc
            .values()
            .flatten()
            .any(|f| f.severity == Severity::Error);
    SetValidation {
        per_doc,
        set,
        valid,
    }
}

/// A LESS-restricted collection whose prefix string-prefixes a MORE-restricted
/// collection's prefix serves the restricted documents to the wider audience.
/// Overlap itself is legitimate (a source may bind into several collections —
/// runbooks/README); only the sensitivity-inverting case warns.
fn check_prefix_shadowing(r: &ParsedRunbook, set: &mut Vec<Finding>) {
    let cols = &r.doc.spec.collections;
    for (i, a) in cols.iter().enumerate() {
        let Some(ap) = a.sources.as_ref().and_then(|s| s.filename_prefix.as_ref()) else {
            continue;
        };
        for (j, b) in cols.iter().enumerate() {
            if i == j {
                continue;
            }
            let Some(bp) = b.sources.as_ref().and_then(|s| s.filename_prefix.as_ref()) else {
                continue;
            };
            // a covers b's documents…
            if !bp.starts_with(ap.as_str()) {
                continue;
            }
            // …and a is strictly less restricted than b.
            let a_cmp: std::collections::HashSet<&String> = a.compartments.iter().collect();
            let b_cmp: std::collections::HashSet<&String> = b.compartments.iter().collect();
            let a_weaker_level = a.access_level <= b.access_level;
            let a_subset = a_cmp.is_subset(&b_cmp);
            let strictly = a.access_level < b.access_level || a_cmp.len() < b_cmp.len();
            if a_weaker_level && a_subset && strictly {
                set.push(finding(
                    Severity::Warn,
                    "set.prefix-shadows-restricted",
                    format!(
                        "collection '{}' (level {}, compartments {:?}) binds '{ap}', a \
                         prefix of the more-restricted '{}' (level {}, {:?}) binding \
                         '{bp}' — the restricted documents are also served to the wider \
                         audience; make the overlap a decision, not an accident",
                        a.name,
                        a.access_level,
                        a.compartments,
                        b.name,
                        b.access_level,
                        b.compartments
                    ),
                    format!("{}: spec.collections[{i}].sources.filenamePrefix", r.path),
                ));
            }
        }
    }
}

/// Answer keys are never uploaded: a key inside the retrieval index is not
/// a measurement (runbooks/README). Flag bindings that look like one.
fn check_answer_key_bindings(r: &ParsedRunbook, set: &mut Vec<Finding>) {
    // Deliberately narrow: bare "answers" or "expected" would flag
    // legitimate corpora (Q&A forums, "expected_deliveries/"). These are
    // the naming conventions answer keys actually use.
    const MARKERS: &[&str] = &[
        "answer_key",
        "answer-key",
        "answerkey",
        "ground_truth",
        "ground-truth",
        "groundtruth",
        "seeded_findings",
    ];
    for (i, col) in r.doc.spec.collections.iter().enumerate() {
        let Some(fp) = col
            .sources
            .as_ref()
            .and_then(|s| s.filename_prefix.as_ref())
        else {
            continue;
        };
        let lower = fp.to_lowercase();
        if MARKERS.iter().any(|m| lower.contains(m)) {
            set.push(finding(
                Severity::Warn,
                "set.answer-key-filename",
                format!(
                    "collection '{}' binds '{fp}', which looks like an answer key — \
                     a key inside the retrieval index is not a measurement; keys \
                     belong to the grader, not the corpus",
                    col.name
                ),
                format!("{}: spec.collections[{i}].sources.filenamePrefix", r.path),
            ));
        }
    }
}

fn valid_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some('a'..='z') | Some('0'..='9'))
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHAPE: &str = r#"apiVersion: munarium.ioka.io/v1
kind: Shape
metadata: { name: docs, version: 1 }
spec:
  fact:
    schema:
      type: object
      properties:
        subject: { type: string, pattern: "^[a-z][a-z0-9_]{0,63}$" }
        key: { type: string, pattern: "^[a-z][a-z0-9_:-]{0,63}$" }
        value: { type: string, minLength: 1 }
      required: [subject, key, value]
  chunking: { max_chars: 1200 }
"#;

    fn runbook(collections: &str) -> String {
        format!(
            r#"apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: {{ name: t, version: 1 }}
spec:
  sources: {{ container: sources, prefix: "corp/" }}
  collections:
{collections}
  steps:
    - resolveSources: {{}}
    - buildIndex: {{}}
    - verify: {{}}
    - cutover: {{ approval: required }}
    - retireOld: {{ keep_versions: 2 }}
"#
        )
    }

    fn set_of(docs: &[(&str, &str)]) -> BTreeMap<String, String> {
        docs.iter()
            .map(|(p, y)| (p.to_string(), y.to_string()))
            .collect()
    }

    #[test]
    fn a_clean_set_is_valid() {
        let rb = runbook("    - { name: a, shape: docs@1, accessLevel: 0, sources: { filenamePrefix: \"corp/a/\" } }");
        let v = validate_set(
            &set_of(&[("shapes/docs.yaml", SHAPE), ("runbooks/t.yaml", &rb)]),
            &HashMap::new(),
        );
        assert!(v.valid, "{:?} {:?}", v.set, v.per_doc);
        assert!(v.set.is_empty(), "{:?}", v.set);
    }

    #[test]
    fn no_runbook_is_an_error() {
        let v = validate_set(&set_of(&[("shapes/docs.yaml", SHAPE)]), &HashMap::new());
        assert!(!v.valid);
        assert!(v.set.iter().any(|f| f.code == "set.no-runbook"));
    }

    #[test]
    fn an_unresolved_shape_is_an_error_and_published_shapes_resolve() {
        let rb = runbook("    - { name: a, shape: other@1, accessLevel: 0, sources: { filenamePrefix: \"corp/a/\" } }");
        let docs = set_of(&[("shapes/docs.yaml", SHAPE), ("runbooks/t.yaml", &rb)]);
        let v = validate_set(&docs, &HashMap::new());
        assert!(v.set.iter().any(|f| f.code == "set.shape-unresolved"));
        // …and the in-set shape is now unused.
        assert!(v.set.iter().any(|f| f.code == "set.shape-unused"));
        // Published on the server: resolved.
        let published = HashMap::from([("other@1".to_string(), "hash".to_string())]);
        let v = validate_set(&docs, &published);
        assert!(!v.set.iter().any(|f| f.code == "set.shape-unresolved"));
    }

    #[test]
    fn a_version_conflict_with_published_content_is_an_error() {
        let rb = runbook("    - { name: a, shape: docs@1, accessLevel: 0, sources: { filenamePrefix: \"corp/a/\" } }");
        let docs = set_of(&[("shapes/docs.yaml", SHAPE), ("runbooks/t.yaml", &rb)]);
        let published = HashMap::from([("docs@1".to_string(), "a-different-hash".to_string())]);
        let v = validate_set(&docs, &published);
        assert!(!v.valid);
        assert!(v.set.iter().any(|f| f.code == "set.shape-version-conflict"));
        // Identical content: no conflict.
        let same = HashMap::from([("docs@1".to_string(), crate::sha256_hex(SHAPE.as_bytes()))]);
        let v = validate_set(&docs, &same);
        assert!(!v.set.iter().any(|f| f.code == "set.shape-version-conflict"));
    }

    #[test]
    fn duplicate_refs_with_different_content_collide() {
        let mutated = SHAPE.replace("minLength: 1", "minLength: 2");
        let rb = runbook("    - { name: a, shape: docs@1, accessLevel: 0, sources: { filenamePrefix: \"corp/a/\" } }");
        let v = validate_set(
            &set_of(&[
                ("shapes/docs.yaml", SHAPE),
                ("shapes/docs2.yaml", &mutated),
                ("runbooks/t.yaml", &rb),
            ]),
            &HashMap::new(),
        );
        assert!(v.set.iter().any(|f| f.code == "set.name-collision"));
    }

    #[test]
    fn sensitivity_inverting_prefix_overlap_warns() {
        let rb = runbook(
            "    - { name: open, shape: docs@1, accessLevel: 0, sources: { filenamePrefix: \"corp/\" } }\n\
             \x20   - { name: hr, shape: docs@1, accessLevel: 3, compartments: [hr], sources: { filenamePrefix: \"corp/hr/\" } }",
        );
        let v = validate_set(
            &set_of(&[("shapes/docs.yaml", SHAPE), ("runbooks/t.yaml", &rb)]),
            &HashMap::new(),
        );
        assert!(v
            .set
            .iter()
            .any(|f| f.code == "set.prefix-shadows-restricted"));
        // Same restriction both sides (the sweep-coverage sharing pattern): silent.
        let rb = runbook(
            "    - { name: a, shape: docs@1, accessLevel: 1, sources: { filenamePrefix: \"corp/\" } }\n\
             \x20   - { name: b, shape: docs@1, accessLevel: 1, sources: { filenamePrefix: \"corp/x/\" } }",
        );
        let v = validate_set(
            &set_of(&[("shapes/docs.yaml", SHAPE), ("runbooks/t.yaml", &rb)]),
            &HashMap::new(),
        );
        assert!(!v
            .set
            .iter()
            .any(|f| f.code == "set.prefix-shadows-restricted"));
    }

    #[test]
    fn answer_key_bindings_warn() {
        let rb = runbook("    - { name: a, shape: docs@1, accessLevel: 0, sources: { filenamePrefix: \"corp/answer_key/\" } }");
        let v = validate_set(
            &set_of(&[("shapes/docs.yaml", SHAPE), ("runbooks/t.yaml", &rb)]),
            &HashMap::new(),
        );
        assert!(v.set.iter().any(|f| f.code == "set.answer-key-filename"));
    }

    #[test]
    fn bad_names_are_errors() {
        let bad = SHAPE.replace("name: docs", "name: Docs_v2");
        let rb = runbook("    - { name: a, shape: docs@1, accessLevel: 0, sources: { filenamePrefix: \"corp/a/\" } }");
        let v = validate_set(
            &set_of(&[("shapes/docs.yaml", &bad), ("runbooks/t.yaml", &rb)]),
            &HashMap::new(),
        );
        assert!(!v.valid);
        assert!(v.per_doc["shapes/docs.yaml"]
            .iter()
            .any(|f| f.code == "set.name-invalid"));
    }

    #[test]
    fn parse_failures_are_findings_not_panics() {
        let v = validate_set(
            &set_of(&[("runbooks/broken.yaml", "kind: Runbook\n:::garbage")]),
            &HashMap::new(),
        );
        assert!(!v.valid);
        assert!(v.per_doc["runbooks/broken.yaml"]
            .iter()
            .any(|f| f.code == "parse"));
    }

    #[test]
    fn a_runbook_mentioning_kind_shape_is_still_a_runbook() {
        // Kind is PARSED, not sniffed: the completion template's literal
        // "kind: Shape" must not route this document through parse_shape.
        let rb = format!(
            "{}\n  completion:\n    promptTemplate: |\n      Facts follow kind: Shape \
             schemas. Cite evidence. {{context}} {{query}}\n",
            runbook("    - { name: a, shape: docs@1, accessLevel: 0, sources: { filenamePrefix: \"corp/a/\" } }")
                .trim_end()
        );
        let v = validate_set(
            &set_of(&[("shapes/docs.yaml", SHAPE), ("runbooks/t.yaml", &rb)]),
            &HashMap::new(),
        );
        assert!(v.valid, "{:?} {:?}", v.set, v.per_doc);
        // ...and the set knows it HAS a runbook (no set.no-runbook).
        assert!(!v.set.iter().any(|f| f.code == "set.no-runbook"));
    }
}
