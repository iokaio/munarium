// SPDX-License-Identifier: Apache-2.0
//! Deterministic materialization: interview answers -> a shape + runbook
//! document set. Documents are built as `serde_json::Value` trees and
//! emitted through `serde_yaml` — NEVER by re-serializing `RunbookDoc`
//! (`steps_raw` is private and `steps` is `#[serde(skip)]`, so a
//! round-tripped doc would drop its steps). Every emitted document is
//! proven by re-parsing through `parse_shape` / `parse_runbook` before it
//! leaves this module.
//!
//! Unanswered required questions become `todos` entries plus placeholder
//! values, so a fresh draft validates with "red TODOs expected" rather
//! than crashing — the authoring wizard's contract.

use crate::catalog::{self, PatternEntry};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct Materialized {
    /// path (e.g. "runbooks/<name>.yaml", "shapes/<name>-documents.yaml") -> YAML.
    pub documents: BTreeMap<String, String>,
    /// Human-readable list of what still needs answering.
    pub todos: Vec<String>,
}

/// One area (folder) of the corpus, from the `prefix.areas` answer.
struct Area {
    path: String, // normalized: no leading '/', ends with '/'
    description: String,
}

/// Copy a pattern's exemplar documents into a draft: the runbook renamed to
/// the draft's name (version reset to 1), shapes verbatim — shapes are
/// shared, not copied, so their names stay canonical.
pub fn seed_documents(
    name: &str,
    pattern: &PatternEntry,
) -> Result<BTreeMap<String, String>, String> {
    let mut docs = BTreeMap::new();
    let runbook_yaml = catalog::exemplar_runbook(pattern.start_from)
        .ok_or_else(|| format!("exemplar '{}' is not embedded", pattern.start_from))?;
    let mut tree: Value =
        serde_yaml::from_str(runbook_yaml).map_err(|e| format!("exemplar yaml: {e}"))?;
    tree["metadata"]["name"] = json!(name);
    tree["metadata"]["version"] = json!(1);
    let renamed = serde_yaml::to_string(&tree).map_err(|e| format!("emit: {e}"))?;
    munarium_runbooks::parse_runbook(&renamed).map_err(|e| format!("seeded runbook: {e}"))?;
    docs.insert(
        format!("runbooks/{name}.yaml"),
        format!(
            "# Seeded from the '{}' exemplar (pattern: {}). Edit the collections and\n\
             # bindings to your corpus — the exemplar's prefixes name ITS corpus tree.\n{}",
            pattern.start_from, pattern.id, renamed
        ),
    );
    for shape_name in pattern.shape_names {
        let yaml = catalog::exemplar_shape(shape_name)
            .ok_or_else(|| format!("exemplar shape '{shape_name}' is not embedded"))?;
        docs.insert(format!("shapes/{shape_name}.yaml"), yaml.to_string());
    }
    Ok(docs)
}

/// Build the document set from interview answers. `name` is the draft's
/// (and runbook's) name, fixed at draft creation.
pub fn build_documents(
    name: &str,
    pattern: Option<&PatternEntry>,
    answers: &Value,
) -> Result<Materialized, String> {
    let mut todos = Vec::new();
    let shape_name = format!("{name}-documents");
    let shape_ref = format!("{shape_name}@1");

    // ---- answers -----------------------------------------------------------
    let description = str_answer(answers, "identity.description").unwrap_or_else(|| {
        todos.push("identity.description: describe the corpus and the question it answers".into());
        "TODO: describe the corpus and the question this application answers".into()
    });
    let root = match str_answer(answers, "prefix.root") {
        Some(r) => normalize_prefix(&r),
        None => {
            todos.push(format!(
                "prefix.root: choose the path prefix (IMMUTABLE once uploaded); defaulting to '{name}/'"
            ));
            format!("{name}/")
        }
    };
    let areas = parse_areas(answers);
    let levels = map_answer(answers, "access.area_levels");
    let compartments = map_answer(answers, "access.area_compartments");
    // Unanswered uniform_public defaults true ONLY when no per-area access
    // answers exist — supplying area_levels and having them silently
    // flattened to level 0 would be the worst kind of wizard.
    let uniform = bool_answer(answers, "access.uniform_public")
        .unwrap_or_else(|| levels.is_none() && compartments.is_none());
    let media_types = map_answer(answers, "extraction.media_types");
    let top_k = int_answer(answers, "retrieval.top_k").unwrap_or(10);
    let rrf_k = int_answer(answers, "retrieval.rrf_k").unwrap_or(60);
    let candidate_n = int_answer(answers, "retrieval.candidate_n").unwrap_or(100);
    let max_chars = match int_answer(answers, "retrieval.max_chars") {
        // parse_shape hard-fails < 16 (a tiny max_chars degrades the index
        // to per-character chunks); keep the draft materializable.
        Some(mc) if mc < 16 => {
            todos.push(format!(
                "retrieval.max_chars: {mc} is below the parser's minimum of 16; defaulting to 1200"
            ));
            1200
        }
        Some(mc) => mc,
        None => 1200,
    };
    let byok_embedding = str_answer(answers, "retrieval.embedding").as_deref() == Some("byok");
    let cutover_approval = bool_answer(answers, "lifecycle.cutover_approval").unwrap_or(true);
    let keep_versions = match int_answer(answers, "lifecycle.keep_versions") {
        // keep_versions is u32 in StepSpec — a negative answer would fail
        // the materialized document's own parse.
        Some(kv) if kv < 0 => {
            todos.push("lifecycle.keep_versions: must be >= 0; defaulting to 2".into());
            2
        }
        Some(kv) => kv,
        None => 2,
    };
    let completion_applies = pattern.map(|p| p.has_completion).unwrap_or(true);
    let completion_enabled =
        completion_applies && bool_answer(answers, "completion.enabled").unwrap_or(true);
    let completion_tier =
        str_answer(answers, "completion.tier").unwrap_or_else(|| "capable".into());
    let verify_quotes = bool_answer(answers, "completion.verification_quotes").unwrap_or(true);
    let verify_citations =
        bool_answer(answers, "completion.verification_citations").unwrap_or(true);
    let allow_overrides =
        str_answer(answers, "completion.allow_overrides").unwrap_or_else(|| "none".into());

    // ---- collections ---------------------------------------------------------
    let mut collections = Vec::new();
    if areas.is_empty() {
        todos.push(
            "prefix.areas: list the corpus folders, one per governance boundary — \
             defaulting to a single whole-prefix collection"
                .into(),
        );
        collections.push(json!({
            "name": format!("{name}-index"),
            "shape": shape_ref,
            "accessLevel": 0,
            "sources": { "filenamePrefix": root },
        }));
    } else {
        for area in &areas {
            let key = area.path.trim_matches('/');
            let mut col = json!({
                "name": format!("{name}-{}", slug(key)),
                "shape": shape_ref,
                "accessLevel": if uniform { 0 } else { lookup_int(&levels, key).unwrap_or_else(|| {
                    todos.push(format!("access.area_levels: no level for area '{key}'; defaulting to 0"));
                    0
                }) },
                "sources": { "filenamePrefix": format!("{root}{}", area.path) },
            });
            if !uniform {
                if let Some(tags) = lookup_list(&compartments, key) {
                    if !tags.is_empty() {
                        col["compartments"] = json!(tags);
                    }
                }
            }
            if let Some(mts) = lookup_list(&media_types, key) {
                if !mts.is_empty() {
                    col["sources"]["mediaTypes"] = json!(mts);
                }
            }
            collections.push(col);
        }
    }

    // ---- models ---------------------------------------------------------------
    let mut tasks = json!({ "validation": { "provider": "default", "tier": "fast" } });
    if completion_enabled {
        tasks["completion"] = json!({ "provider": "default", "tier": completion_tier });
    }
    if byok_embedding {
        // Accepted and policy-checked but not yet consumed — the runbook
        // validator's models.embedding-not-consumed Info says so honestly.
        tasks["embedding"] = json!({ "provider": "default" });
    }
    let models = json!({
        "default": { "provider": "default", "tier": "capable" },
        "tasks": tasks,
        "allowOverrides": allow_overrides == "all",
    });

    // ---- steps ------------------------------------------------------------------
    let cutover = if cutover_approval {
        json!({ "cutover": { "approval": "required" } })
    } else {
        json!({ "cutover": {} })
    };
    let steps = json!([
        { "resolveSources": {} },
        { "buildIndex": {} },
        { "verify": {} },
        cutover,
        { "retireOld": { "keep_versions": keep_versions } },
    ]);

    // ---- runbook ---------------------------------------------------------------
    let mut spec = json!({
        "sources": { "container": "sources", "prefix": root },
        "collections": collections,
        "retrieval": { "topK": top_k, "rrfK": rrf_k, "candidateN": candidate_n },
        "models": models,
        "steps": steps,
    });
    if completion_enabled {
        spec["completion"] = json!({
            "promptTemplate": completion_template(),
            "verification": {
                "quotes": verify_quotes,
                "citations": verify_citations,
                "maxRetries": 1,
            },
        });
    }
    let runbook_tree = json!({
        "apiVersion": "munarium.ioka.io/v1",
        "kind": "Runbook",
        "metadata": { "name": name, "version": 1 },
        "spec": spec,
    });
    let runbook_yaml = serde_yaml::to_string(&runbook_tree).map_err(|e| format!("emit: {e}"))?;
    let runbook_yaml = format!(
        "{}{}",
        header_comment(name, pattern, &description, &areas),
        runbook_yaml
    );
    munarium_runbooks::parse_runbook(&runbook_yaml)
        .map_err(|e| format!("materialized runbook does not parse: {e}"))?;

    // ---- shape ------------------------------------------------------------------
    let mut properties = json!({
        // Folded subjects, DOT-FREE keys: subject.key splits at the LAST dot,
        // so a dotted key silently steals from the subject (dash/colon encode
        // version-like parts).
        "subject": { "type": "string", "pattern": "^[a-z][a-z0-9_]{0,63}$" },
        "key": { "type": "string", "pattern": "^[a-z][a-z0-9_:-]{0,63}$" },
        "value": { "type": "string", "minLength": 1, "maxLength": 512 },
    });
    let mut required = vec![
        "subject".to_string(),
        "key".to_string(),
        "value".to_string(),
    ];
    for field in parse_fields(answers) {
        if !valid_field_name(&field.0) {
            todos.push(format!(
                "extraction.fact_fields: '{}' is not a valid field name \
                 (lowercase [a-z][a-z0-9_]*); skipped",
                field.0
            ));
            continue;
        }
        // The core vocabulary is not overridable: a field named `key` would
        // silently replace the dot-free-key pattern.
        if ["subject", "key", "value"].contains(&field.0.as_str()) {
            todos.push(format!(
                "extraction.fact_fields: '{}' is the core vocabulary and cannot be \
                 redefined; skipped",
                field.0
            ));
            continue;
        }
        properties[&field.0] = json!({ "type": field.1 });
        if field.2 {
            required.push(field.0.clone());
        }
    }
    let shape_tree = json!({
        "apiVersion": "munarium.ioka.io/v1",
        "kind": "Shape",
        "metadata": { "name": shape_name, "version": 1 },
        "spec": {
            "fact": {
                "schema": {
                    "type": "object",
                    "properties": properties,
                    "required": required,
                },
            },
            "chunking": { "max_chars": max_chars },
        },
    });
    let shape_yaml = serde_yaml::to_string(&shape_tree).map_err(|e| format!("emit: {e}"))?;
    munarium_shapes::parse_shape(&shape_yaml)
        .map_err(|e| format!("materialized shape does not parse: {e}"))?;

    let mut documents = BTreeMap::new();
    documents.insert(format!("runbooks/{name}.yaml"), runbook_yaml);
    documents.insert(format!("shapes/{shape_name}.yaml"), shape_yaml);
    Ok(Materialized { documents, todos })
}

/// The default RAG completion template, carrying the measured lessons
/// (runbooks/README: "completion templates carry measured lessons").
fn completion_template() -> String {
    "You answer questions about this document corpus using ONLY the retrieved \
     evidence below. Cite every claim as doc#node. If the evidence does not \
     establish an answer, say so plainly — an honest \"the corpus does not \
     establish this\" beats a guess. A search hit you did not read is not a \
     citation. When a question asks about an enumerable set (all X, every Y), \
     enumerate the set from the evidence rather than sampling it.\n\n\
     Evidence:\n{context}\n\nQuestion: {query}\n"
        .to_string()
}

fn header_comment(
    name: &str,
    pattern: Option<&PatternEntry>,
    description: &str,
    areas: &[Area],
) -> String {
    let mut out = String::new();
    for line in description.lines() {
        out.push_str(&format!("# {line}\n"));
    }
    if !areas.is_empty() {
        out.push_str("#\n# Areas (one collection per governance boundary):\n");
        for a in areas {
            if a.description.is_empty() {
                out.push_str(&format!("#   {}\n", a.path));
            } else {
                out.push_str(&format!("#   {} — {}\n", a.path, a.description));
            }
        }
    }
    match pattern {
        Some(p) => out.push_str(&format!(
            "#\n# Materialized by munarium authoring (pattern: {}; exemplar: {}).\n",
            p.id, p.start_from
        )),
        None => out.push_str("#\n# Materialized by munarium authoring.\n"),
    }
    out.push_str(&format!(
        "#   mmctl apply -f shapes/{name}-documents.yaml\n#   mmctl apply -f runbooks/{name}.yaml\n"
    ));
    out
}

// ---- answer accessors -------------------------------------------------------

fn str_answer(answers: &Value, key: &str) -> Option<String> {
    answers
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

fn int_answer(answers: &Value, key: &str) -> Option<i64> {
    answers.get(key).and_then(|v| v.as_i64())
}

fn bool_answer(answers: &Value, key: &str) -> Option<bool> {
    answers.get(key).and_then(|v| v.as_bool())
}

fn map_answer<'a>(answers: &'a Value, key: &str) -> Option<&'a serde_json::Map<String, Value>> {
    answers.get(key).and_then(|v| v.as_object())
}

fn lookup<'a>(
    map: &Option<&'a serde_json::Map<String, Value>>,
    area_key: &str,
) -> Option<&'a Value> {
    let map = map.as_ref()?;
    map.get(area_key)
        .or_else(|| map.get(&format!("{area_key}/")))
}

fn lookup_int(map: &Option<&serde_json::Map<String, Value>>, area_key: &str) -> Option<i64> {
    lookup(map, area_key).and_then(|v| v.as_i64())
}

fn lookup_list(
    map: &Option<&serde_json::Map<String, Value>>,
    area_key: &str,
) -> Option<Vec<String>> {
    lookup(map, area_key).and_then(|v| v.as_array()).map(|a| {
        a.iter()
            .filter_map(|x| x.as_str())
            .map(String::from)
            .collect()
    })
}

fn parse_areas(answers: &Value) -> Vec<Area> {
    answers
        .get("prefix.areas")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    let path = a.get("path")?.as_str()?.trim();
                    let path = normalize_prefix(path.trim_start_matches('/'));
                    // A path of "" or "/" would bind the whole root under a
                    // malformed collection name — not an area.
                    if path.is_empty() {
                        return None;
                    }
                    Some(Area {
                        path,
                        description: a
                            .get("description")
                            .and_then(|d| d.as_str())
                            .unwrap_or("")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// (name, json type, required)
fn parse_fields(answers: &Value) -> Vec<(String, String, bool)> {
    answers
        .get("extraction.fact_fields")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|f| {
                    let key = f.get("key")?.as_str()?.trim().to_string();
                    if key.is_empty() {
                        return None;
                    }
                    let ty = f
                        .get("type")
                        .and_then(|t| t.as_str())
                        .unwrap_or("string")
                        .to_string();
                    let required = f.get("required").and_then(|r| r.as_bool()).unwrap_or(false);
                    Some((key, ty, required))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// End every prefix in '/': matching is a literal starts_with, and the
/// interview's guidance is unambiguous, so the materializer normalizes
/// rather than reproducing the mistake for the validator to catch.
fn normalize_prefix(p: &str) -> String {
    let p = p.trim();
    if p.is_empty() || p.ends_with('/') {
        p.to_string()
    } else {
        format!("{p}/")
    }
}

fn slug(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn valid_field_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some('a'..='z'))
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog;

    fn canonical_answers() -> Value {
        json!({
            "identity.description": "Vendor security reviews for procurement.",
            "prefix.root": "vendors/",
            "prefix.areas": [
                { "path": "public/", "description": "published attestations" },
                { "path": "contracts/", "description": "signed agreements" },
                { "path": "incidents", "description": "incident reports" },
            ],
            "access.uniform_public": false,
            "access.area_levels": { "public": 0, "contracts": 2, "incidents": 3 },
            "access.area_compartments": { "contracts": ["legal"], "incidents": ["security"] },
            "extraction.media_types": { "contracts": ["application/pdf"] },
            "retrieval.candidate_n": 120,
            "completion.tier": "fast",
        })
    }

    #[test]
    fn canonical_answers_materialize_clean() {
        let pattern = catalog::pattern("ask-the-corpus");
        let m = build_documents("vendor-security", pattern, &canonical_answers()).expect("build");
        assert!(m.todos.is_empty(), "{:?}", m.todos);
        assert_eq!(m.documents.len(), 2);

        let runbook = &m.documents["runbooks/vendor-security.yaml"];
        let doc = munarium_runbooks::parse_runbook(runbook).expect("parses");
        let findings = munarium_runbooks::validate::validate_runbook(&doc);
        assert!(
            munarium_runbooks::validate::is_valid(&findings),
            "{findings:?}"
        );
        // No warns either: the guided path should emit what the committed
        // samples are held to (zero Error, zero Warn).
        assert!(
            !findings
                .iter()
                .any(|f| f.severity == munarium_runbooks::validate::Severity::Warn),
            "{findings:?}"
        );
        assert_eq!(doc.spec.collections.len(), 3);
        assert_eq!(doc.spec.collections[2].compartments, vec!["security"]);
        assert!(doc.spec.collections[2]
            .sources
            .as_ref()
            .unwrap()
            .filename_prefix
            .as_deref()
            .unwrap()
            .ends_with("incidents/"));

        let shape =
            munarium_shapes::parse_shape(&m.documents["shapes/vendor-security-documents.yaml"])
                .expect("parses");
        let findings = munarium_shapes::validate::validate_shape(&shape);
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn empty_answers_yield_placeholders_and_todos() {
        let m = build_documents("fresh", None, &json!({})).expect("build");
        assert!(m.todos.iter().any(|t| t.contains("identity.description")));
        assert!(m.todos.iter().any(|t| t.contains("prefix.root")));
        assert!(m.todos.iter().any(|t| t.contains("prefix.areas")));
        // Placeholders still parse and validate error-free (red TODOs, not red docs).
        let doc =
            munarium_runbooks::parse_runbook(&m.documents["runbooks/fresh.yaml"]).expect("parses");
        assert!(munarium_runbooks::validate::is_valid(
            &munarium_runbooks::validate::validate_runbook(&doc)
        ));
    }

    #[test]
    fn no_completion_for_patterns_without_one() {
        let red_flag = catalog::pattern("red-flag-review");
        let m = build_documents("review", red_flag, &canonical_answers()).expect("build");
        let doc =
            munarium_runbooks::parse_runbook(&m.documents["runbooks/review.yaml"]).expect("parses");
        assert!(doc.spec.completion.is_none());
    }

    #[test]
    fn supplying_area_levels_without_uniform_public_is_honored() {
        // The trap: area_levels answered, uniform_public forgotten. The
        // materializer must infer non-uniform rather than silently
        // flattening everything to level 0.
        let mut answers = canonical_answers();
        answers
            .as_object_mut()
            .unwrap()
            .remove("access.uniform_public");
        let m = build_documents("t", None, &answers).expect("build");
        let doc =
            munarium_runbooks::parse_runbook(&m.documents["runbooks/t.yaml"]).expect("parses");
        assert_eq!(
            doc.spec.collections[2].access_level, 3,
            "incidents area keeps level 3"
        );
        assert_eq!(doc.spec.collections[2].compartments, vec!["security"]);
        // Explicit uniform_public: true still wins over supplied maps.
        let mut answers = canonical_answers();
        answers["access.uniform_public"] = json!(true);
        let m = build_documents("t", None, &answers).expect("build");
        let doc =
            munarium_runbooks::parse_runbook(&m.documents["runbooks/t.yaml"]).expect("parses");
        assert!(doc.spec.collections.iter().all(|c| c.access_level == 0));
    }

    #[test]
    fn root_and_empty_area_paths_are_dropped() {
        let mut answers = canonical_answers();
        answers["prefix.areas"] = json!([
            { "path": "/", "description": "the whole root is not an area" },
            { "path": "  ", "description": "" },
            { "path": "real/", "description": "kept" },
        ]);
        let m = build_documents("t", None, &answers).expect("build");
        let doc =
            munarium_runbooks::parse_runbook(&m.documents["runbooks/t.yaml"]).expect("parses");
        assert_eq!(doc.spec.collections.len(), 1);
        assert_eq!(doc.spec.collections[0].name, "t-real");
    }

    #[test]
    fn core_vocabulary_fields_cannot_be_redefined() {
        let mut answers = canonical_answers();
        answers["extraction.fact_fields"] = json!([
            { "key": "key", "type": "integer", "required": true },
            { "key": "severity", "type": "string" },
        ]);
        let m = build_documents("t", None, &answers).expect("build");
        assert!(m.todos.iter().any(|t| t.contains("core vocabulary")));
        let shape_yaml = &m.documents["shapes/t-documents.yaml"];
        // The dot-free key pattern survives; the extra field lands.
        assert!(shape_yaml.contains("^[a-z][a-z0-9_:-]{0,63}$"));
        assert!(shape_yaml.contains("severity"));
    }

    #[test]
    fn invalid_extra_fields_are_skipped_with_a_todo() {
        let mut answers = canonical_answers();
        answers["extraction.fact_fields"] = json!([
            { "key": "severity", "type": "string", "required": true },
            { "key": "Bad.Name", "type": "string" },
        ]);
        let m = build_documents("t", None, &answers).expect("build");
        assert!(m.todos.iter().any(|t| t.contains("Bad.Name")));
        let shape_yaml = &m.documents["shapes/t-documents.yaml"];
        assert!(shape_yaml.contains("severity"));
        assert!(!shape_yaml.contains("Bad.Name"));
    }

    #[test]
    fn seeded_documents_are_renamed_and_parse() {
        // red-flag-review starts from due-diligence, which every build embeds
        // (every build embeds it), so this holds in any configuration.
        let pattern = catalog::pattern("red-flag-review").unwrap();
        let docs = seed_documents("kb", pattern).expect("seed");
        let runbook = &docs["runbooks/kb.yaml"];
        let doc = munarium_runbooks::parse_runbook(runbook).expect("parses");
        assert_eq!(doc.metadata.name, "kb");
        assert_eq!(doc.metadata.version, 1);
        assert!(docs.contains_key("shapes/dataroom-documents.yaml"));
    }
}
