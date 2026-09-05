// SPDX-License-Identifier: Apache-2.0
//! Deterministic runbook validation: pure structural/semantic checks a
//! server can run with zero model calls. The AI-assisted suggestion pass
//! layers on top of these findings; it never replaces them.

use crate::{RunbookDoc, StepSpec, TASK_LEVELS};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// The runbook should not be applied/run as-is.
    Error,
    /// Legal, but likely not what the author wants.
    Warn,
    /// Advisory.
    Info,
}

#[derive(Debug, Clone, Serialize)]
pub struct ValidationFinding {
    pub severity: Severity,
    /// Stable dotted code, e.g. "steps.cutover-before-build".
    pub code: String,
    pub message: String,
    /// YAML-ish path locating the finding, e.g. "spec.collections[1].name".
    pub path: String,
}

fn finding(severity: Severity, code: &str, message: String, path: String) -> ValidationFinding {
    ValidationFinding {
        severity,
        code: code.to_string(),
        message,
        path,
    }
}

/// Pure, deterministic validation of a parsed runbook. Parsing itself
/// already rejects structurally-broken documents; this reports the
/// semantic layer.
pub fn validate_runbook(doc: &RunbookDoc) -> Vec<ValidationFinding> {
    let mut out = Vec::new();
    let spec = &doc.spec;

    if doc.metadata.name.trim().is_empty() {
        out.push(finding(
            Severity::Error,
            "metadata.name-empty",
            "runbook name must be non-empty".into(),
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

    // A configured embedding model is accepted and policy-checked but NOT
    // yet consumed: index builds embed with the built-in keyless
    // `local-hash@1` embedder. Say so at validate time (2026-08-17 — the
    // gap) instead of letting an operator
    // believe their embedding provider is in the loop.
    if let Some(models) = &spec.models {
        if models.tasks.get("embedding").is_some_and(|m| !m.is_empty()) {
            out.push(finding(
                Severity::Info,
                "models.embedding-not-consumed",
                "models.embedding is accepted and validated but not yet consumed — index \
                 builds use the built-in local embedder; provider-backed embeddings are a \
                 tracked follow-up"
                    .into(),
                "spec.models.embedding".into(),
            ));
        }
    }

    // ---- step ordering -----------------------------------------------------
    let pos = |name: &str| spec.steps.iter().position(|s| s.name() == name);
    let build = pos("buildIndex");
    if build.is_none() {
        out.push(finding(
            Severity::Warn,
            "steps.no-build",
            "no buildIndex step — this runbook never (re)indexes anything".into(),
            "spec.steps".into(),
        ));
    }
    for (dependent, code) in [
        ("verify", "steps.verify-before-build"),
        ("cutover", "steps.cutover-before-build"),
        ("retireOld", "steps.retire-before-build"),
    ] {
        if let (Some(d), b) = (pos(dependent), build) {
            if b.is_none() || d < b.unwrap() {
                out.push(finding(
                    Severity::Error,
                    code,
                    format!("{dependent} requires a prior buildIndex step"),
                    format!("spec.steps[{d}]"),
                ));
            }
        }
    }
    if let (Some(c), Some(v)) = (pos("cutover"), pos("verify")) {
        if c < v {
            out.push(finding(
                Severity::Warn,
                "steps.cutover-before-verify",
                "cutover precedes verify — the index goes live before verification".into(),
                format!("spec.steps[{c}]"),
            ));
        }
    }
    if let Some(c) = pos("cutover") {
        if !spec.steps[c].requires_approval() {
            out.push(finding(
                Severity::Info,
                "steps.cutover-unapproved",
                "cutover has no approval gate; the index goes live without a human in the loop"
                    .into(),
                format!("spec.steps[{c}]"),
            ));
        }
    }
    for (i, step) in spec.steps.iter().enumerate() {
        if let StepSpec::RetireOld { keep_versions } = step {
            if *keep_versions == 0 {
                out.push(finding(
                    Severity::Warn,
                    "steps.retire-keeps-none",
                    "keep_versions: 0 reclaims every inactive version's chunks immediately — rollback needs a rebuild".into(),
                    format!("spec.steps[{i}]"),
                ));
            }
        }
    }

    // ---- collections (v2) --------------------------------------------------
    let mut seen = std::collections::HashSet::new();
    for (i, col) in spec.collections.iter().enumerate() {
        let path = format!("spec.collections[{i}]");
        if col.name.trim().is_empty() {
            out.push(finding(
                Severity::Error,
                "collections.name-empty",
                "collection name must be non-empty".into(),
                format!("{path}.name"),
            ));
        }
        if !seen.insert(col.name.clone()) {
            out.push(finding(
                Severity::Error,
                "collections.name-duplicate",
                format!("duplicate collection name '{}'", col.name),
                format!("{path}.name"),
            ));
        }
        if col.access_level < 0 {
            out.push(finding(
                Severity::Error,
                "collections.negative-level",
                "access_level must be >= 0".into(),
                format!("{path}.accessLevel"),
            ));
        }
        if !col.shape.contains('@') {
            out.push(finding(
                Severity::Warn,
                "collections.shape-unversioned",
                format!(
                    "shape '{}' has no @version — pin the shape version",
                    col.shape
                ),
                format!("{path}.shape"),
            ));
        }
        match &col.sources {
            None => out.push(finding(
                Severity::Info,
                "collections.no-source-binding",
                format!(
                    "collection '{}' declares no source binding — sources must be bound explicitly at ingest",
                    col.name
                ),
                format!("{path}.sources"),
            )),
            Some(b) if b.is_empty() => out.push(finding(
                Severity::Warn,
                "collections.empty-source-binding",
                "sources: {} matches nothing; drop it or add a matcher".into(),
                format!("{path}.sources"),
            )),
            Some(b) => {
                for (j, mt) in b.media_types.iter().enumerate() {
                    if !mt.contains('/') {
                        out.push(finding(
                            Severity::Warn,
                            "collections.bad-media-type",
                            format!("'{mt}' does not look like a media type (type/subtype)"),
                            format!("{path}.sources.mediaTypes[{j}]"),
                        ));
                    }
                }
            }
        }
    }
    // Overlapping levels warning: identical (level, compartments) across all
    // collections means the compartmentalization does nothing.
    if spec.collections.len() > 1 {
        let distinct: std::collections::HashSet<(i32, Vec<String>)> = spec
            .collections
            .iter()
            .map(|c| {
                let mut cmp = c.compartments.clone();
                cmp.sort();
                (c.access_level, cmp)
            })
            .collect();
        if distinct.len() == 1 {
            out.push(finding(
                Severity::Info,
                "collections.uniform-access",
                "every collection has the same access level and compartments — one runbook serving multiple clearance levels usually wants them to differ".into(),
                "spec.collections".into(),
            ));
        }
    }

    // ---- sources (v2) ------------------------------------------------------
    if let Some(sources) = &spec.sources {
        if let Some(prefix) = &sources.prefix {
            if prefix.starts_with('/') || prefix.contains("..") || prefix.contains('\\') {
                out.push(finding(
                    Severity::Error,
                    "sources.prefix-invalid",
                    format!(
                        "prefix '{prefix}' is not a valid blob path prefix \
                         (no leading '/', '..', or backslashes)"
                    ),
                    "spec.sources.prefix".into(),
                ));
            } else if !prefix.is_empty() && !prefix.ends_with('/') {
                // Matching is a literal starts_with, so "north" would also
                // match "northgate-archive/" — almost never the intent.
                out.push(finding(
                    Severity::Warn,
                    "sources.prefix-unterminated",
                    format!(
                        "prefix '{prefix}' does not end in '/'; matching is a literal \
                         starts_with, so it also matches sibling paths that merely begin \
                         with those characters"
                    ),
                    "spec.sources.prefix".into(),
                ));
            }
            // Every collection must actually sit under the declared prefix,
            // or the runbook claims its documents live somewhere its own
            // bindings can never match.
            for (i, col) in spec.collections.iter().enumerate() {
                let Some(binding) = &col.sources else {
                    continue;
                };
                let Some(fp) = &binding.filename_prefix else {
                    continue;
                };
                if !fp.starts_with(prefix.as_str()) {
                    out.push(finding(
                        Severity::Error,
                        "sources.prefix-mismatch",
                        format!(
                            "collection '{}' binds '{fp}', which is not under the declared \
                             sources prefix '{prefix}' — this runbook can never match it",
                            col.name
                        ),
                        format!("spec.collections[{i}].sources.filenamePrefix"),
                    ));
                }
            }
        }
        if sources.container.is_none() {
            out.push(finding(
                Severity::Info,
                "sources.container-unset",
                "no container declared; the server's MUNARIUM_AZURE_BLOB_CONTAINER default applies"
                    .into(),
                "spec.sources.container".into(),
            ));
        }
    }

    // ---- retrieval ---------------------------------------------------------
    if let Some(r) = &spec.retrieval {
        let path = "spec.retrieval";
        if r.top_k == 0 || r.top_k > 100 {
            out.push(finding(
                Severity::Error,
                "retrieval.top-k-range",
                format!("topK {} outside 1..=100", r.top_k),
                format!("{path}.topK"),
            ));
        }
        if !(1.0..=1000.0).contains(&r.rrf_k) {
            out.push(finding(
                Severity::Error,
                "retrieval.rrf-k-range",
                format!("rrfK {} outside 1..=1000", r.rrf_k),
                format!("{path}.rrfK"),
            ));
        }
        if r.candidate_n < 1 || r.candidate_n > 1000 {
            out.push(finding(
                Severity::Error,
                "retrieval.candidate-n-range",
                format!("candidateN {} outside 1..=1000", r.candidate_n),
                format!("{path}.candidateN"),
            ));
        }
        if (r.candidate_n as usize) < r.top_k {
            out.push(finding(
                Severity::Warn,
                "retrieval.candidates-below-top-k",
                "candidateN < topK — fusion can never fill topK hits".into(),
                path.to_string(),
            ));
        }
        if !r.query_expansion_weight.is_finite() || !(0.0..=1.0).contains(&r.query_expansion_weight)
        {
            out.push(finding(
                Severity::Error,
                "retrieval.query-expansion-weight-range",
                "queryExpansionWeight must be finite and in 0..=1".into(),
                format!("{path}.queryExpansionWeight"),
            ));
        }
        if !r.stop_term_fraction.is_finite()
            || (r.stop_term_fraction != 0.0 && !(0.05..=0.9).contains(&r.stop_term_fraction))
        {
            out.push(finding(
                Severity::Error,
                "retrieval.stop-term-fraction-range",
                "stopTermFraction must be 0 (off) or in 0.05..=0.9".into(),
                format!("{path}.stopTermFraction"),
            ));
        }
        if !(1..=2).contains(&r.minimum_should_match) {
            out.push(finding(
                Severity::Error,
                "retrieval.minimum-should-match-range",
                "minimumShouldMatch must be 1 (any query word) or 2 (at least two)".into(),
                format!("{path}.minimumShouldMatch"),
            ));
        }
        if !(1..=16).contains(&r.search_concurrency) {
            out.push(finding(
                Severity::Error,
                "retrieval.search-concurrency-range",
                "searchConcurrency must be in 1..=16 (each in-flight search holds a pooled connection)".into(),
                format!("{path}.searchConcurrency"),
            ));
        }
        if let Some(selection) = &r.collection_selection {
            if !selection.phrase_boost.is_finite()
                || !(0.0..=10.0).contains(&selection.phrase_boost)
            {
                out.push(finding(
                    Severity::Error,
                    "retrieval.collection-selection-phrase-boost-range",
                    "phraseBoost must be in 0..=10".into(),
                    format!("{path}.collectionSelection.phraseBoost"),
                ));
            }
        }
        if let Some(fusion) = &r.fusion {
            let fusion_path = format!("{path}.fusion");
            for (field, value) in [
                ("lexicalWeight", fusion.lexical_weight),
                ("vectorWeight", fusion.vector_weight),
                (
                    "collectionEvidenceWeight",
                    fusion.collection_evidence_weight,
                ),
                ("unselectedPoolWeight", fusion.unselected_pool_weight),
            ] {
                if !value.is_finite() || !(0.0..=10.0).contains(&value) {
                    out.push(finding(
                        Severity::Error,
                        "retrieval.fusion-weight-range",
                        format!("{field} must be in 0..=10"),
                        format!("{fusion_path}.{field}"),
                    ));
                }
            }
            if fusion.lexical_weight == 0.0 && fusion.vector_weight == 0.0 {
                out.push(finding(
                    Severity::Error,
                    "retrieval.fusion-no-leg",
                    "lexicalWeight and vectorWeight cannot both be 0 — nothing would rank the hits"
                        .into(),
                    fusion_path.clone(),
                ));
            }
            if fusion.collection_evidence_weight > 0.0 && r.collection_selection.is_none() {
                out.push(finding(
                    Severity::Warn,
                    "retrieval.fusion-evidence-without-selection",
                    "collectionEvidenceWeight has no effect without collectionSelection (no evidence ranking is produced)".into(),
                    format!("{fusion_path}.collectionEvidenceWeight"),
                ));
            }
        }
        if let Some(expansion) = &r.model_query_expansion {
            let expansion_path = format!("{path}.modelQueryExpansion");
            if !(1..=32).contains(&expansion.max_terms) {
                out.push(finding(
                    Severity::Error,
                    "retrieval.model-query-expansion-max-terms-range",
                    "maxTerms must be in 1..=32".into(),
                    format!("{expansion_path}.maxTerms"),
                ));
            }
            if expansion
                .max_tokens
                .is_some_and(|t| !(32..=512).contains(&t))
            {
                out.push(finding(
                    Severity::Error,
                    "retrieval.model-query-expansion-max-tokens-range",
                    "maxTokens must be in 32..=512".into(),
                    format!("{expansion_path}.maxTokens"),
                ));
            }
        }
        if let Some(selection) = &r.collection_selection {
            let selection_path = format!("{path}.collectionSelection");
            if !(1..=64).contains(&selection.max_collections) {
                out.push(finding(
                    Severity::Error,
                    "retrieval.collection-selection-max-collections-range",
                    "maxCollections must be in 1..=64".into(),
                    format!("{selection_path}.maxCollections"),
                ));
            }
            if !(1..=1000).contains(&selection.probe_candidate_n) {
                out.push(finding(
                    Severity::Error,
                    "retrieval.collection-selection-probe-candidates-range",
                    "probeCandidateN must be in 1..=1000".into(),
                    format!("{selection_path}.probeCandidateN"),
                ));
            }
            if !(1..=1000).contains(&selection.candidate_pool_per_collection) {
                out.push(finding(
                    Severity::Error,
                    "retrieval.collection-selection-candidate-pool-range",
                    "candidatePoolPerCollection must be in 1..=1000".into(),
                    format!("{selection_path}.candidatePoolPerCollection"),
                ));
            } else if selection.candidate_pool_per_collection < r.top_k {
                out.push(finding(
                    Severity::Warn,
                    "retrieval.collection-selection-pool-below-top-k",
                    "candidatePoolPerCollection < topK can discard useful per-collection candidates before global fusion".into(),
                    selection_path,
                ));
            }
        }
        if r.collection_routes.len() > 32 {
            out.push(finding(
                Severity::Error,
                "retrieval.too-many-collection-routes",
                format!(
                    "{} collectionRoutes exceeds the maximum of 32",
                    r.collection_routes.len()
                ),
                format!("{path}.collectionRoutes"),
            ));
        }
        let collection_names: std::collections::HashSet<&str> =
            spec.collections.iter().map(|c| c.name.as_str()).collect();
        for (i, route) in r.collection_routes.iter().enumerate() {
            let route_path = format!("{path}.collectionRoutes[{i}]");
            if route.when_all.is_empty() {
                out.push(finding(
                    Severity::Error,
                    "retrieval.collection-route-no-trigger",
                    "whenAll must contain at least one trigger term".into(),
                    format!("{route_path}.whenAll"),
                ));
            }
            if route.collections.is_empty() {
                out.push(finding(
                    Severity::Error,
                    "retrieval.collection-route-no-collections",
                    "collections must contain at least one runbook collection".into(),
                    format!("{route_path}.collections"),
                ));
            }
            for (j, term) in route.when_all.iter().enumerate() {
                if term.trim().is_empty() || term.len() > 80 {
                    out.push(finding(
                        Severity::Error,
                        "retrieval.collection-route-bad-term",
                        "terms must be non-empty and at most 80 bytes".into(),
                        format!("{route_path}.whenAll[{j}]"),
                    ));
                }
            }
            for (j, collection) in route.collections.iter().enumerate() {
                if !collection_names.contains(collection.as_str()) {
                    out.push(finding(
                        Severity::Error,
                        "retrieval.collection-route-unknown-collection",
                        format!("collection '{collection}' is not bound by this runbook"),
                        format!("{route_path}.collections[{j}]"),
                    ));
                }
            }
        }
        if r.query_expansions.len() > 32 {
            out.push(finding(
                Severity::Error,
                "retrieval.too-many-query-expansions",
                format!(
                    "{} queryExpansions exceeds the maximum of 32",
                    r.query_expansions.len()
                ),
                format!("{path}.queryExpansions"),
            ));
        }
        for (i, rule) in r.query_expansions.iter().enumerate() {
            let rule_path = format!("{path}.queryExpansions[{i}]");
            if rule.when_any.is_empty() {
                out.push(finding(
                    Severity::Error,
                    "retrieval.query-expansion-no-trigger",
                    "whenAny must contain at least one trigger term".into(),
                    format!("{rule_path}.whenAny"),
                ));
            }
            if rule.add_terms.is_empty() {
                out.push(finding(
                    Severity::Error,
                    "retrieval.query-expansion-no-additions",
                    "addTerms must contain at least one term".into(),
                    format!("{rule_path}.addTerms"),
                ));
            }
            for (field, terms) in [
                ("whenAny", rule.when_any.as_slice()),
                ("addTerms", rule.add_terms.as_slice()),
            ] {
                if terms.len() > 64 {
                    out.push(finding(
                        Severity::Error,
                        "retrieval.query-expansion-too-many-terms",
                        format!("{field} exceeds the maximum of 64 terms"),
                        format!("{rule_path}.{field}"),
                    ));
                }
                for (j, term) in terms.iter().enumerate() {
                    if term.trim().is_empty() || term.len() > 80 {
                        out.push(finding(
                            Severity::Error,
                            "retrieval.query-expansion-bad-term",
                            "terms must be non-empty and at most 80 bytes".into(),
                            format!("{rule_path}.{field}[{j}]"),
                        ));
                    }
                }
            }
        }
        if r.content_demotions.len() > 32 {
            out.push(finding(
                Severity::Error,
                "retrieval.too-many-content-demotions",
                format!(
                    "{} contentDemotions exceeds the maximum of 32",
                    r.content_demotions.len()
                ),
                format!("{path}.contentDemotions"),
            ));
        }
        for (i, rule) in r.content_demotions.iter().enumerate() {
            let rule_path = format!("{path}.contentDemotions[{i}]");
            if rule.contains.trim().is_empty() || rule.contains.len() > 512 {
                out.push(finding(
                    Severity::Error,
                    "retrieval.content-demotion-bad-marker",
                    "contains must be non-empty and at most 512 bytes".into(),
                    format!("{rule_path}.contains"),
                ));
            }
            if !rule.lexical_multiplier.is_finite()
                || !(0.0..=1.0).contains(&rule.lexical_multiplier)
            {
                out.push(finding(
                    Severity::Error,
                    "retrieval.content-demotion-bad-lexical-multiplier",
                    "lexicalMultiplier must be finite and in 0..=1".into(),
                    format!("{rule_path}.lexicalMultiplier"),
                ));
            }
            if !rule.vector_distance_penalty.is_finite()
                || !(0.0..=10.0).contains(&rule.vector_distance_penalty)
            {
                out.push(finding(
                    Severity::Error,
                    "retrieval.content-demotion-bad-vector-penalty",
                    "vectorDistancePenalty must be finite and in 0..=10".into(),
                    format!("{rule_path}.vectorDistancePenalty"),
                ));
            }
            for (j, name) in rule.except_collections.iter().enumerate() {
                if !spec.collections.iter().any(|c| &c.name == name) {
                    out.push(finding(
                        Severity::Error,
                        "retrieval.content-demotion-unknown-collection",
                        format!("exceptCollections names '{name}', which spec.collections does not declare"),
                        format!("{rule_path}.exceptCollections[{j}]"),
                    ));
                }
            }
            if !rule.except_collections.is_empty()
                && rule.except_collections.len() >= spec.collections.len()
            {
                out.push(finding(
                    Severity::Warn,
                    "retrieval.content-demotion-excepts-all",
                    "exceptCollections covers every collection — the rule never applies".into(),
                    format!("{rule_path}.exceptCollections"),
                ));
            }
            if rule.lexical_multiplier == 1.0 && rule.vector_distance_penalty == 0.0 {
                out.push(finding(
                    Severity::Warn,
                    "retrieval.content-demotion-no-op",
                    "content demotion has no effect".into(),
                    rule_path,
                ));
            }
        }
    }

    // ---- models ------------------------------------------------------------
    if let Some(models) = &spec.models {
        for (task, spec_m) in &models.tasks {
            let path = format!("spec.models.tasks.{task}");
            if !TASK_LEVELS.contains(&task.as_str()) {
                out.push(finding(
                    Severity::Error,
                    "models.unknown-task",
                    format!(
                        "unknown task level '{task}' (known: {})",
                        TASK_LEVELS.join(", ")
                    ),
                    path.clone(),
                ));
            }
            if spec_m.is_empty() {
                out.push(finding(
                    Severity::Error,
                    "models.empty-spec",
                    "a model spec needs provider, model, or tier".into(),
                    path.clone(),
                ));
            }
            if let Some(t) = &spec_m.tier {
                if t != "fast" && t != "capable" && t != "frontier" {
                    out.push(finding(
                        Severity::Error,
                        "models.bad-tier",
                        format!("tier '{t}' must be fast|capable|frontier"),
                        format!("{path}.tier"),
                    ));
                }
            }
        }
        if let Some(d) = &models.default {
            if d.is_empty() {
                out.push(finding(
                    Severity::Error,
                    "models.empty-spec",
                    "models.default needs provider, model, or tier".into(),
                    "spec.models.default".into(),
                ));
            }
        }
    }

    // ---- completion --------------------------------------------------------
    if let Some(c) = &spec.completion {
        for var in ["{context}", "{query}"] {
            if !c.prompt_template.contains(var) {
                out.push(finding(
                    Severity::Warn,
                    "completion.template-missing-var",
                    format!("promptTemplate does not reference {var}"),
                    "spec.completion.promptTemplate".into(),
                ));
            }
        }
        let has_model = spec
            .models
            .as_ref()
            .map(|m| m.tasks.contains_key("completion") || m.default.is_some())
            .unwrap_or(false);
        if !has_model {
            out.push(finding(
                Severity::Info,
                "completion.no-model-default",
                "no completion model default — turns will fall back to the tenant's default provider chain".into(),
                "spec.models".into(),
            ));
        }
        if let Some(budget) = c.context_char_budget {
            if !(4_000..=400_000).contains(&budget) {
                out.push(finding(
                    Severity::Error,
                    "completion.context-char-budget-range",
                    "contextCharBudget must be in 4000..=400000 characters".into(),
                    "spec.completion.contextCharBudget".into(),
                ));
            }
        }
        if let Some(tokens) = c.max_tokens {
            if !(256..=16_384).contains(&tokens) {
                out.push(finding(
                    Severity::Error,
                    "completion.max-tokens-range",
                    "maxTokens must be in 256..=16384 (the truncation retry pays 4x this)".into(),
                    "spec.completion.maxTokens".into(),
                ));
            }
        }
    }

    out
}

/// True when no Error-severity finding is present.
pub fn is_valid(findings: &[ValidationFinding]) -> bool {
    !findings.iter().any(|f| f.severity == Severity::Error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_runbook;

    fn v2(yaml_steps: &str) -> RunbookDoc {
        parse_runbook(&format!(
            r#"
apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: {{ name: t, version: 1 }}
spec:
  collections:
    - {{ name: a, shape: s@1, accessLevel: 0 }}
    - {{ name: b, shape: s@1, accessLevel: 2, compartments: [eng] }}
  steps:
{yaml_steps}
"#
        ))
        .expect("parses")
    }

    #[test]
    fn clean_v2_has_no_errors() {
        let doc = v2("    - resolveSources: {}\n    - buildIndex: {}\n    - verify: {}\n    - cutover: { approval: required }\n    - retireOld: { keep_versions: 2 }");
        let findings = validate_runbook(&doc);
        assert!(is_valid(&findings), "{findings:?}");
    }

    #[test]
    fn cutover_before_build_is_an_error() {
        let doc = v2("    - cutover: {}\n    - buildIndex: {}");
        let findings = validate_runbook(&doc);
        assert!(!is_valid(&findings));
        assert!(findings
            .iter()
            .any(|f| f.code == "steps.cutover-before-build"));
    }

    #[test]
    fn duplicate_names_bad_task_and_ranges_are_errors() {
        let doc = parse_runbook(
            r#"
apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: { name: t, version: 1 }
spec:
  collections:
    - { name: a, shape: s@1 }
    - { name: a, shape: s@1 }
  retrieval: { topK: 0 }
  models:
    tasks:
      chatops: { provider: p }
      completion: { tier: warp }
  steps:
    - buildIndex: {}
"#,
        )
        .expect("parses");
        let findings = validate_runbook(&doc);
        let codes: Vec<&str> = findings.iter().map(|f| f.code.as_str()).collect();
        assert!(codes.contains(&"collections.name-duplicate"));
        assert!(codes.contains(&"retrieval.top-k-range"));
        assert!(codes.contains(&"models.unknown-task"));
        assert!(codes.contains(&"models.bad-tier"));
        assert!(!is_valid(&findings));
    }

    #[test]
    fn invalid_retrieval_policies_are_rejected() {
        let doc = parse_runbook(
            r#"
apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: { name: t, version: 1 }
spec:
  collections: [{ name: a, shape: s@1 }]
  retrieval:
    queryExpansionWeight: 2
    minimumShouldMatch: 3
    stopTermFraction: 0.01
    searchConcurrency: 0
    modelQueryExpansion: { maxTerms: 0, maxTokens: 1 }
    collectionSelection:
      { maxCollections: 0, probeCandidateN: 0, candidatePoolPerCollection: 0, phraseBoost: -1 }
    fusion: { lexicalWeight: 0, vectorWeight: 0, collectionEvidenceWeight: 11, unselectedPoolWeight: 11 }
    collectionRoutes:
      - { whenAll: [], collections: [missing] }
    queryExpansions:
      - { whenAny: [], addTerms: [] }
    contentDemotions:
      - { contains: "", lexicalMultiplier: 2, vectorDistancePenalty: -1, exceptCollections: [nope] }
  completion:
    promptTemplate: "{context} {query}"
    contextCharBudget: 10
    maxTokens: 17000
  steps:
    - buildIndex: {}
"#,
        )
        .expect("parses");
        let findings = validate_runbook(&doc);
        let codes: Vec<&str> = findings.iter().map(|f| f.code.as_str()).collect();
        assert!(codes.contains(&"completion.context-char-budget-range"));
        assert!(codes.contains(&"completion.max-tokens-range"));
        assert!(codes.contains(&"retrieval.query-expansion-weight-range"));
        assert!(codes.contains(&"retrieval.model-query-expansion-max-terms-range"));
        assert!(codes.contains(&"retrieval.model-query-expansion-max-tokens-range"));
        assert!(codes.contains(&"retrieval.collection-selection-max-collections-range"));
        assert!(codes.contains(&"retrieval.collection-selection-probe-candidates-range"));
        assert!(codes.contains(&"retrieval.collection-selection-candidate-pool-range"));
        assert!(codes.contains(&"retrieval.collection-selection-phrase-boost-range"));
        assert!(codes.contains(&"retrieval.minimum-should-match-range"));
        assert!(codes.contains(&"retrieval.stop-term-fraction-range"));
        assert!(codes.contains(&"retrieval.search-concurrency-range"));
        assert!(codes.contains(&"retrieval.fusion-weight-range"));
        assert!(codes.contains(&"retrieval.fusion-no-leg"));
        assert!(codes.contains(&"retrieval.collection-route-no-trigger"));
        assert!(codes.contains(&"retrieval.collection-route-unknown-collection"));
        assert!(codes.contains(&"retrieval.query-expansion-no-trigger"));
        assert!(codes.contains(&"retrieval.query-expansion-no-additions"));
        assert!(codes.contains(&"retrieval.content-demotion-bad-marker"));
        assert!(codes.contains(&"retrieval.content-demotion-bad-lexical-multiplier"));
        assert!(codes.contains(&"retrieval.content-demotion-bad-vector-penalty"));
        assert!(codes.contains(&"retrieval.content-demotion-unknown-collection"));
        assert!(!is_valid(&findings));
    }

    #[test]
    fn uniform_access_is_flagged_info() {
        let doc = parse_runbook(
            r#"
apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: { name: t, version: 1 }
spec:
  collections:
    - { name: a, shape: s@1, accessLevel: 1 }
    - { name: b, shape: s@1, accessLevel: 1 }
  steps:
    - buildIndex: {}
"#,
        )
        .expect("parses");
        let findings = validate_runbook(&doc);
        assert!(findings
            .iter()
            .any(|f| f.code == "collections.uniform-access" && f.severity == Severity::Info));
    }
}

#[cfg(test)]
mod sources_tests {
    use super::*;
    use crate::parse_runbook;

    fn rb(sources: &str, prefix_a: &str) -> RunbookDoc {
        parse_runbook(&format!(
            r#"
apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: {{ name: t, version: 1 }}
spec:
{sources}
  collections:
    - name: a
      shape: s@1
      sources: {{ filenamePrefix: '{prefix_a}' }}
  steps:
    - buildIndex: {{}}
"#
        ))
        .expect("parses")
    }

    #[test]
    fn a_binding_outside_the_declared_prefix_is_an_error() {
        // The runbook says its documents live under northgate/, but binds a
        // path that can never appear there — a contradiction worth blocking.
        let doc = rb(
            "  sources: { container: sources, prefix: \"northgate/\" }",
            "vale/03/",
        );
        let findings = validate_runbook(&doc);
        assert!(!is_valid(&findings), "{findings:?}");
        assert!(findings.iter().any(|f| f.code == "sources.prefix-mismatch"));
    }

    #[test]
    fn a_binding_under_the_prefix_is_clean() {
        let doc = rb(
            "  sources: { container: sources, prefix: \"northgate/\" }",
            "northgate/03_finance/",
        );
        let findings = validate_runbook(&doc);
        assert!(is_valid(&findings), "{findings:?}");
        assert!(!findings.iter().any(|f| f.code == "sources.container-unset"));
    }

    #[test]
    fn an_unterminated_prefix_warns_because_matching_is_literal() {
        let doc = rb("  sources: { prefix: \"north\" }", "northgate/");
        let findings = validate_runbook(&doc);
        // Legal (it does match), but almost never what the author means.
        assert!(is_valid(&findings));
        assert!(findings
            .iter()
            .any(|f| f.code == "sources.prefix-unterminated"));
        // And with no container declared, say so.
        assert!(findings.iter().any(|f| f.code == "sources.container-unset"));
    }

    #[test]
    fn traversal_in_a_prefix_is_refused() {
        for bad in ["/abs/", "../escape/", r"back\slash/"] {
            // Single-quoted YAML: no escape processing, so a literal
            // backslash reaches the validator as written.
            let doc = rb(&format!("  sources: {{ prefix: '{bad}' }}"), bad);
            let findings = validate_runbook(&doc);
            assert!(
                findings.iter().any(|f| f.code == "sources.prefix-invalid"),
                "expected rejection for {bad}"
            );
        }
    }

    #[test]
    fn runbooks_without_a_sources_block_are_unaffected() {
        let doc = rb("", "anything/");
        let findings = validate_runbook(&doc);
        assert!(is_valid(&findings));
        assert!(!findings.iter().any(|f| f.code.starts_with("sources.")));
    }
}
