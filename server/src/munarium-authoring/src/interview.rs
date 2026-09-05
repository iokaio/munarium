// SPDX-License-Identifier: Apache-2.0
//! The authoring interview: dev-guide §16's design decisions as an ordered
//! question set, sections in §16's order — which is deliberately the order
//! of how hard each decision is to revise (prefix layout is immutable once
//! documents are uploaded; retrieval knobs are a new runbook version away).
//! Guidance prose is attached to each question so the author reads the
//! rule at the moment the decision is made; `doc_ref` names the chapter
//! that teaches it in full.
//!
//! Answers are one flat JSON object keyed by question id. `maps_to`
//! documents the slot an answer lands in; `materialize::build_documents`
//! owns the real mapping.

use crate::catalog::PatternEntry;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Section {
    pub id: &'static str,
    pub title: &'static str,
    /// The document section that teaches this decision in full.
    pub doc_ref: &'static str,
    pub questions: Vec<Question>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Question {
    pub id: &'static str,
    pub prompt: &'static str,
    pub guidance: &'static str,
    /// string | text | int | bool | enum | areas | fields | map
    pub kind: &'static str,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<&'static str>,
    /// Documentation of the slot this answer lands in, e.g.
    /// "runbook:spec.sources.prefix". Not an executed path.
    pub maps_to: &'static str,
}

#[allow(clippy::too_many_arguments)] // a question IS eight facts; a builder would be ceremony
fn q(
    id: &'static str,
    prompt: &'static str,
    guidance: &'static str,
    kind: &'static str,
    required: bool,
    default: Option<serde_json::Value>,
    choices: Vec<&'static str>,
    maps_to: &'static str,
) -> Question {
    Question {
        id,
        prompt,
        guidance,
        kind,
        required,
        default,
        choices,
        maps_to,
    }
}

/// The full interview for a draft. The completion section appears only for
/// patterns that carry a completion arm (or when no pattern was chosen).
pub fn interview(pattern: Option<&PatternEntry>) -> Vec<Section> {
    let mut sections = vec![
        Section {
            id: "identity",
            title: "What are you building?",
            doc_ref: "dev-guide §19 \"Choosing your pattern\"",
            questions: vec![
                q(
                    "identity.description",
                    "Describe the corpus and the question this application answers.",
                    "One or two sentences. This becomes the runbook's header comment and, if \
                     you use the AI assist, the corpus description it drafts from.",
                    "text",
                    true,
                    None,
                    vec![],
                    "runbook:header-comment",
                ),
                q(
                    "identity.pattern",
                    "Which application pattern fits?",
                    "Pick by smell: contradiction matters -> red-flag-review; naming hides it \
                     -> entity-intelligence; time matters -> living-knowledge-base; \
                     obligations matter -> assistant-memory; \"find everything\" -> \
                     audit-sweeps; otherwise ask-the-corpus or research-chat. Each pattern \
                     names a committed exemplar to copy from.",
                    "enum",
                    false,
                    None,
                    // The patterns THIS build serves (all seven with the experiment
                    // exemplars; a trimmed build offers only those whose
                    // exemplar it embeds), so the interview never offers a
                    // choice the catalog then refuses.
                    crate::catalog::patterns().iter().map(|p| p.id).collect(),
                    "pattern",
                ),
            ],
        },
        Section {
            id: "prefix-layout",
            title: "Prefix layout — IMMUTABLE once documents are uploaded",
            doc_ref: "dev-guide §16 \"Prefix design is access design\"",
            questions: vec![
                q(
                    "prefix.root",
                    "What path prefix will every document of this application live under?",
                    "A document's filename IS its identity and its blob path; there is no \
                     move or delete API, so restructuring means re-ingesting everything. End \
                     the prefix in '/': matching is a literal starts_with, so 'north' also \
                     matches 'northgate-archive/'. Ten minutes of layout design before the \
                     first upload is the cheapest insurance this platform sells.",
                    "string",
                    true,
                    None,
                    vec![],
                    "runbook:spec.sources.prefix",
                ),
                q(
                    "prefix.areas",
                    "List the folders (areas) under that prefix, one per governance boundary.",
                    "Each area becomes one collection binding '<root><area-path>'. Boundaries \
                     follow GOVERNANCE, not topics — ask \"who must NOT see this folder?\". \
                     No bound prefix should nest inside another unless the overlap is a \
                     decision, not an accident. A new sibling folder later is cheap; \
                     splitting an existing folder is not.",
                    "areas",
                    true,
                    None,
                    vec![],
                    "runbook:spec.collections[*].sources.filenamePrefix",
                ),
            ],
        },
        Section {
            id: "access",
            title: "Levels and compartments",
            doc_ref: "dev-guide §16 \"Levels and compartments\"",
            questions: vec![
                q(
                    "access.uniform_public",
                    "Is the whole corpus one audience (e.g. public documents)?",
                    "Uniform level 0 is HONEST for public corpora — accept the \
                     collections.uniform-access Info finding rather than inventing a \
                     clearance story. Answer false to assign per-area levels and compartments.",
                    "bool",
                    false,
                    Some(serde_json::json!(true)),
                    vec![],
                    "runbook:spec.collections[*].accessLevel",
                ),
                q(
                    "access.area_levels",
                    "Access level per area (0-3).",
                    "Use FEW levels — no committed runbook needs more than 0-3. Two \
                     same-seniority audiences that must not see each other are two \
                     COMPARTMENTS at one level, not levels 4 and 5.",
                    "map",
                    false,
                    None,
                    vec![],
                    "runbook:spec.collections[*].accessLevel",
                ),
                q(
                    "access.area_compartments",
                    "Compartment tags per area (need-to-know sets).",
                    "A compartment is a DATA-SENSITIVITY set, not a team — compartment-per-team \
                     rots at the first reorg. Multiple compartments on one collection mean \
                     AND: the caller must hold every one. \"Either support or legal\" is two \
                     collections over two prefixes.",
                    "map",
                    false,
                    None,
                    vec![],
                    "runbook:spec.collections[*].compartments",
                ),
            ],
        },
        Section {
            id: "retrieval",
            title: "Retrieval knobs (revisable) and chunking (index identity)",
            doc_ref: "dev-guide §16 \"The hybrid mechanics you inherit\"",
            questions: vec![
                q(
                    "retrieval.top_k",
                    "How many fused hits should a query return?",
                    "Default 10. retrieval: knobs are QUERY-TIME — changing them is a new \
                     runbook version, no rebuild.",
                    "int",
                    false,
                    Some(serde_json::json!(10)),
                    vec![],
                    "runbook:spec.retrieval.topK",
                ),
                q(
                    "retrieval.candidate_n",
                    "How many candidates should each retrieval leg contribute to fusion?",
                    "Default 50; wider corpora raise it so fusion has something to fuse \
                     (due-diligence 120, support-knowledge 150).",
                    "int",
                    false,
                    Some(serde_json::json!(100)),
                    vec![],
                    "runbook:spec.retrieval.candidateN",
                ),
                q(
                    "retrieval.rrf_k",
                    "RRF constant.",
                    "Default 60. Leave it unless you have a measured reason.",
                    "int",
                    false,
                    Some(serde_json::json!(60)),
                    vec![],
                    "runbook:spec.retrieval.rrfK",
                ),
                q(
                    "retrieval.max_chars",
                    "Maximum characters per indexed chunk.",
                    "This lives in the SHAPE and is part of index identity — changing it \
                     later is a rebuild, unlike the retrieval knobs above. Committed corpora \
                     use 900-1500.",
                    "int",
                    false,
                    Some(serde_json::json!(1200)),
                    vec![],
                    "shape:spec.chunking.max_chars",
                ),
                q(
                    "retrieval.embedding",
                    "Embedding source.",
                    "The free default (local-hash@1) is feature hashing, not a model — it \
                     will not match paraphrase. BYOK embeddings are a MEASURED choice \
                     costing a full rebuild plus a graded before/after; note that a \
                     configured embedding model is accepted but not yet consumed \
                     (models.embedding-not-consumed).",
                    "enum",
                    false,
                    Some(serde_json::json!("keyless-default")),
                    vec!["keyless-default", "byok"],
                    "runbook:spec.models.tasks.embedding",
                ),
            ],
        },
        Section {
            id: "extraction",
            title: "Media types and the fact vocabulary",
            doc_ref: "dev-guide §16 \"Extraction realities by corpus type\"",
            questions: vec![
                q(
                    "extraction.media_types",
                    "Media types per area — ONLY where the corpus genuinely mixes formats.",
                    "Prefix and media type AND together, so an unnecessary media constraint \
                     is a way to silently bind nothing. Declare it where formats genuinely \
                     discriminate (DOCX policies beside PDF SLAs), leave areas absent \
                     otherwise. After the first build over any new corpus type, sweep \
                     extraction_status for 'empty' — the invisible-document signal.",
                    "map",
                    false,
                    None,
                    vec![],
                    "runbook:spec.collections[*].sources.mediaTypes",
                ),
                q(
                    "extraction.fact_fields",
                    "Extra fact-body fields beyond subject/key/value (optional).",
                    "The core vocabulary is subject.key=value with folded subjects and \
                     DOT-FREE keys (subject.key splits at the LAST dot; dash/colon encode \
                     version-like parts). Add fields only if your extraction genuinely \
                     mints them.",
                    "fields",
                    false,
                    None,
                    vec![],
                    "shape:spec.fact.schema.properties",
                ),
            ],
        },
        Section {
            id: "lifecycle",
            title: "Index lifecycle",
            doc_ref: "dev-guide §16 \"The index lifecycle from the application seat\"",
            questions: vec![
                q(
                    "lifecycle.cutover_approval",
                    "Require a human approval before a rebuilt index goes live?",
                    "Every committed exemplar gates cutover: the side-by-side build is free \
                     to fail, and the approval is where a human decides the new index goes \
                     live. Answer false only for corpora where a bad index is cheap.",
                    "bool",
                    false,
                    Some(serde_json::json!(true)),
                    vec![],
                    "runbook:spec.steps.cutover.approval",
                ),
                q(
                    "lifecycle.keep_versions",
                    "How many retired index versions to keep for rollback?",
                    "Default 2 (every committed exemplar). keep_versions: 0 reclaims \
                     immediately — rollback then needs a rebuild.",
                    "int",
                    false,
                    Some(serde_json::json!(2)),
                    vec![],
                    "runbook:spec.steps.retireOld.keep_versions",
                ),
            ],
        },
    ];

    let completion_applies = pattern.map(|p| p.has_completion).unwrap_or(true);
    if completion_applies {
        sections.push(Section {
            id: "completion",
            title: "Completion (RAG answering)",
            doc_ref: "dev-guide §17 \"The completion path\" + \"The grounding lessons\"",
            questions: vec![
                q(
                    "completion.enabled",
                    "Should this application answer questions with a model (RAG completion)?",
                    "If false, the runbook builds and serves the index only; your own \
                     orchestration calls /v1/search.",
                    "bool",
                    false,
                    Some(serde_json::json!(true)),
                    vec![],
                    "runbook:spec.completion",
                ),
                q(
                    "completion.tier",
                    "Model tier for completion.",
                    "fast (haiku-class) is the honest default where measurement showed \
                     model-invariance; capable where nuance was measured to matter \
                     (financial-advisory, patent-analysis); frontier is the paid \
                     top tier — reserve it for runbooks whose evals earned it.",
                    "enum",
                    false,
                    Some(serde_json::json!("capable")),
                    vec!["fast", "capable", "frontier"],
                    "runbook:spec.models.tasks.completion.tier",
                ),
                q(
                    "completion.verification_quotes",
                    "Verify quoted spans resolve verbatim in served text?",
                    "The measured retry that cut quote failures 3x in experiment. One \
                     fix-or-unquote round before the answer stands.",
                    "bool",
                    false,
                    Some(serde_json::json!(true)),
                    vec![],
                    "runbook:spec.completion.verification.quotes",
                ),
                q(
                    "completion.verification_citations",
                    "Verify every citation names content actually served?",
                    "A search hit you did not read is not a citation — the chat failure \
                     that drove four kernel changes.",
                    "bool",
                    false,
                    Some(serde_json::json!(true)),
                    vec![],
                    "runbook:spec.completion.verification.citations",
                ),
                q(
                    "completion.allow_overrides",
                    "May API callers override the completion model?",
                    "The override policy protects a published runbook's spend attribution. \
                     'none' is closed (the default); 'all' permits any configured provider.",
                    "enum",
                    false,
                    Some(serde_json::json!("none")),
                    vec!["none", "all"],
                    "runbook:spec.models.allowOverrides",
                ),
            ],
        });
    }

    sections
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog;

    #[test]
    fn sections_follow_the_revisability_order() {
        let ids: Vec<&str> = interview(None).iter().map(|s| s.id).collect();
        assert_eq!(
            ids,
            vec![
                "identity",
                "prefix-layout",
                "access",
                "retrieval",
                "extraction",
                "lifecycle",
                "completion"
            ]
        );
    }

    #[test]
    fn completion_section_is_pattern_gated() {
        // The section follows the pattern's own flag, for every pattern this
        // build serves. red-flag-review (no completion arm) is embedded in
        // every build; with the experiment exemplars the catalog also carries
        // completion-bearing patterns, so both directions are exercised there.
        let red_flag = catalog::pattern("red-flag-review").unwrap();
        assert!(!red_flag.has_completion);
        for p in catalog::patterns() {
            assert_eq!(
                interview(Some(p)).iter().any(|s| s.id == "completion"),
                p.has_completion,
                "pattern {}",
                p.id
            );
        }
        assert!(catalog::patterns().iter().any(|p| p.has_completion));
    }

    #[test]
    fn pattern_enum_matches_the_catalog() {
        let sections = interview(None);
        let pattern_q = sections
            .iter()
            .flat_map(|s| &s.questions)
            .find(|q| q.id == "identity.pattern")
            .unwrap();
        let catalog_ids: Vec<&str> = catalog::patterns().iter().map(|p| p.id).collect();
        assert_eq!(pattern_q.choices, catalog_ids);
    }
}
