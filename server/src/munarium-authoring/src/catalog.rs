// SPDX-License-Identifier: Apache-2.0
//! The served pattern catalog: seven application patterns, each naming the
//! committed exemplar runbook to start from, plus compile-time embeds of every
//! sample under `server/runbooks/`. The embeds make the catalog
//! self-contained — an operator's server carries its own exemplars — and the
//! parse-all unit test turns a `server/runbooks/` move into a build failure
//! instead of a runtime 500.

/// One application pattern. `start_from` names the committed exemplar runbook;
/// `shape_names` its shape dependencies; `guidance` says what the pattern is
/// strongest at and what to design against.
#[derive(Debug, Clone, Copy)]
pub struct PatternEntry {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    /// The committed exemplar runbook to copy from (an `exemplar_runbook` key).
    pub start_from: &'static str,
    /// What this pattern is strongest at, and the failure mode to design against.
    pub guidance: &'static str,
    /// Shapes the exemplar binds (each an `exemplar_shape` key).
    pub shape_names: &'static [&'static str],
    /// Whether this pattern carries a RAG completion arm (gates the
    /// interview's completion section).
    pub has_completion: bool,
    /// Design notes — the decisions the deterministic validator cannot police.
    pub decision_notes: &'static [&'static str],
}

/// The patterns this build can serve: those whose `start_from` exemplar is
/// embedded, so the catalog never names an exemplar the server cannot produce.
pub fn patterns() -> &'static [PatternEntry] {
    static SERVED: std::sync::OnceLock<Vec<PatternEntry>> = std::sync::OnceLock::new();
    SERVED.get_or_init(|| {
        PATTERNS
            .iter()
            .copied()
            .filter(|p| exemplar_runbook(p.start_from).is_some())
            .collect()
    })
}

pub fn pattern(id: &str) -> Option<&'static PatternEntry> {
    patterns().iter().find(|p| p.id == id)
}

static PATTERNS: &[PatternEntry] = &[
    PatternEntry {
        id: "ask-the-corpus",
        name: "Ask the corpus",
        description: "One question in, clearance-filtered evidence retrieved, a cited answer \
                      out — or an honest \"the corpus does not establish this\". No \
                      conversation, no accumulation; the workhorse pattern.",
        start_from: "financial-advisory",
        guidance: "Strongest when a question is answerable from a bounded set of documents. \
                   Design against the confident answer the corpus does not actually establish \
                   — insufficiency is a correct outcome, not a failure.",
        shape_names: &["advisory-records"],
        has_completion: true,
        decision_notes: &[
            "State the coverage rule in the completion template: a question that asks for an \
             enumerable set must demand the whole set, or a partial answer scores as complete.",
            "Uniform level 0 is honest for public corpora — accept the \
             collections.uniform-access Info rather than inventing a clearance story.",
        ],
    },
    PatternEntry {
        id: "research-chat",
        name: "Research chat",
        description: "Grounded answering made conversational: a session over permitted \
                      collections, follow-ups leaning on antecedents, history condensed by \
                      the client, every turn's citations still held to resolve-or-insufficient.",
        start_from: "regulatory-compliance",
        guidance: "Each turn is independently grounded, so the pattern survives condensed \
                   history. Design against the model citing a document that search surfaced \
                   but the turn never actually read.",
        shape_names: &["regulatory-documents"],
        has_completion: true,
        decision_notes: &[
            "A search hit you did not read is not a citation — require the turn to fetch what \
             it cites before the citation counts.",
            "Sessions pin name@version at creation, so a mid-session runbook upgrade cannot \
             change visibility.",
        ],
    },
    PatternEntry {
        id: "red-flag-review",
        name: "Red-flag review",
        description: "The corpus is interrogated by an extraction pass, source by source; \
                      claims meet the ledger; the product is the QUEUE — every place the \
                      corpus disagrees with itself, both values and both sources attached, \
                      awaiting a human verdict.",
        start_from: "due-diligence",
        guidance: "The deliverable is the queue, not an answer. Finding recall depends far \
                   more on how subjects are normalized than on which model runs the pass.",
        shape_names: &["dataroom-documents"],
        has_completion: false,
        decision_notes: &[
            "Collections are drawn on real governance boundaries — the compartment layout IS \
             the review-team access model.",
            "Subjects are folded identifiers, so two spellings collide instead of hiding a \
             conflict. Normalizing subjects is the highest-leverage change available to you.",
        ],
    },
    PatternEntry {
        id: "living-knowledge-base",
        name: "Living knowledge base",
        description: "A knowledge corpus that keeps moving: release notes supersede KB \
                      articles, tickets contradict docs. Canon answers \"what is true now\"; \
                      retrieval answers \"show me the language behind that\"; corrections \
                      move canon without deleting history.",
        start_from: "support-knowledge",
        guidance: "For corpora where the newest document wins and the superseded one still \
                   has to be explainable. Design against silent staleness: an outdated answer \
                   that still reads as current.",
        shape_names: &["knowledge-sources"],
        has_completion: true,
        decision_notes: &[
            "A prefix per source system, because ten systems have ten owners.",
            "Keys carry no dots: subject.key splits at the LAST dot, so a version-bearing \
             key must be dash-encoded (release_date::4-2-1).",
        ],
    },
    PatternEntry {
        id: "entity-intelligence",
        name: "Entity-centric intelligence",
        description: "Sources describe the same actors under different names. The value is \
                      the REGISTRY: one canonical entity per real-world actor, every alias \
                      attached, facts and findings converging instead of fragmenting.",
        start_from: "threat-intelligence",
        guidance: "The registry is the deliverable. Design against over-merging — collapsing \
                   two real actors into one entity is a worse error than leaving them apart.",
        shape_names: &["threat-reports"],
        has_completion: false,
        decision_notes: &[
            "Seed a handful of multi-alias actors and one over-merge trap in a test corpus \
             before trusting the alias map on real data.",
            "Value normalization folds defanged vs fanged IOCs so false conflicts disappear \
             while genuine conflicts survive.",
        ],
    },
    PatternEntry {
        id: "audit-sweeps",
        name: "Comprehensive audit sweeps",
        description: "Not a question but a mandate: find everything wrong in this corpus. \
                      One open-ended prompt is the failure mode; the pattern is \
                      decomposition — plan targeted sub-questions, run each as a grounded \
                      ask, audit the plan for coverage, merge under provenance.",
        start_from: "sweep-coverage",
        guidance: "One open-ended prompt is the failure mode; decomposition is the pattern. \
                   Coverage comes from auditing the PLAN, not from writing a longer prompt.",
        shape_names: &["dataroom-documents"],
        has_completion: false,
        decision_notes: &[
            "Two sweep runbooks can share one collection: build the index once and let the \
             applications differ only in retrieval policy.",
            "Shapes are shared, not copied — several runbooks can read one corpus through \
             different retrieval architectures.",
        ],
    },
    PatternEntry {
        id: "assistant-memory",
        name: "Long-horizon assistant memory",
        description: "An engagement that outlives any conversation: each new document is a \
                      UNIT — compose the brief from memory so far, process, gate the \
                      extracted claims, accept into a new version. A lineage whose every \
                      state is reproducible.",
        start_from: "insurance-claims",
        guidance: "For engagements longer than any one conversation. Every state must be \
                   reproducible from its lineage, so settle the unit boundary before the \
                   prompt.",
        shape_names: &["claim-files"],
        has_completion: false,
        decision_notes: &[
            "Unit as_of dates power date-pinned ledger reads — stamp them from the start.",
            "Witnessed extraction blocks on contradiction; backfill downgrades to \
             warn+disputed. Pick the mode per corpus era, not per taste.",
        ],
    },
];

macro_rules! embeds {
    ($( $name:literal => $path:literal ),+ $(,)?) => {
        &[ $( ($name, include_str!($path)) ),+ ]
    };
}

/// Every committed exemplar runbook, embedded at compile time.
static EXEMPLAR_RUNBOOKS: &[(&str, &str)] = embeds![
    "customer-support" => "../../../runbooks/applications/customer-support.yaml",
    "due-diligence" => "../../../runbooks/applications/due-diligence.yaml",
    "financial-advisory" => "../../../runbooks/applications/financial-advisory.yaml",
    "history-revolution" => "../../../runbooks/applications/history-revolution.yaml",
    "insurance-claims" => "../../../runbooks/applications/insurance-claims.yaml",
    "legal-appeal" => "../../../runbooks/applications/legal-appeal.yaml",
    "legal-contracts" => "../../../runbooks/applications/legal-contracts.yaml",
    "patent-analysis" => "../../../runbooks/applications/patent-analysis.yaml",
    "regulatory-compliance" => "../../../runbooks/applications/regulatory-compliance.yaml",
    "support-knowledge" => "../../../runbooks/applications/support-knowledge.yaml",
    "sweep-coverage" => "../../../runbooks/applications/sweep-coverage.yaml",
    "sweep-v2" => "../../../runbooks/applications/sweep-v2.yaml",
    "threat-intelligence" => "../../../runbooks/applications/threat-intelligence.yaml",
    "tickets-reindex" => "../../../runbooks/pipelines/tickets-reindex.yaml",
];

/// Every committed shape, embedded at compile time.
static EXEMPLAR_SHAPES: &[(&str, &str)] = embeds![
    "advisory-records" => "../../../runbooks/shapes/advisory-records.yaml",
    "archival-documents" => "../../../runbooks/shapes/archival-documents.yaml",
    "case-filings" => "../../../runbooks/shapes/case-filings.yaml",
    "claim-files" => "../../../runbooks/shapes/claim-files.yaml",
    "commercial-contracts" => "../../../runbooks/shapes/commercial-contracts.yaml",
    "dataroom-documents" => "../../../runbooks/shapes/dataroom-documents.yaml",
    "helpdesk-tickets" => "../../../runbooks/shapes/helpdesk-tickets.yaml",
    "knowledge-sources" => "../../../runbooks/shapes/knowledge-sources.yaml",
    "patent-documents" => "../../../runbooks/shapes/patent-documents.yaml",
    "regulatory-documents" => "../../../runbooks/shapes/regulatory-documents.yaml",
    "support-tickets" => "../../../runbooks/shapes/support-tickets.yaml",
    "threat-reports" => "../../../runbooks/shapes/threat-reports.yaml",
];

pub fn exemplar_runbook(name: &str) -> Option<&'static str> {
    EXEMPLAR_RUNBOOKS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, y)| *y)
}

pub fn exemplar_shape(name: &str) -> Option<&'static str> {
    EXEMPLAR_SHAPES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, y)| *y)
}

pub fn exemplar_runbooks() -> &'static [(&'static str, &'static str)] {
    EXEMPLAR_RUNBOOKS
}

pub fn exemplar_shapes() -> &'static [(&'static str, &'static str)] {
    EXEMPLAR_SHAPES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_served_pattern_resolves_to_embedded_exemplars() {
        assert_eq!(patterns().len(), 7);
        for p in patterns() {
            assert!(
                exemplar_runbook(p.start_from).is_some(),
                "pattern '{}' start_from '{}' is not embedded",
                p.id,
                p.start_from
            );
            for s in p.shape_names {
                assert!(
                    exemplar_shape(s).is_some(),
                    "pattern '{}' shape '{s}' is not embedded",
                    p.id
                );
            }
        }
    }

    #[test]
    fn every_embedded_sample_parses_and_runbooks_validate_error_free() {
        // Complements munarium-runbooks/tests/sample_runbooks.rs (which walks the
        // directory): this guards the include_str! embeds themselves, so a
        // move of server/runbooks/ fails the BUILD, and a newly committed
        // sample that is not embedded fails the count below.
        assert!(
            exemplar_runbooks().len() >= 14,
            "embed the new runbook sample"
        );
        assert!(exemplar_shapes().len() >= 12, "embed the new shape sample");
        for (name, yaml) in exemplar_runbooks() {
            let doc = munarium_runbooks::parse_runbook(yaml)
                .unwrap_or_else(|e| panic!("exemplar runbook '{name}': {e}"));
            let findings = munarium_runbooks::validate::validate_runbook(&doc);
            assert!(
                munarium_runbooks::validate::is_valid(&findings),
                "exemplar runbook '{name}' has error findings: {findings:?}"
            );
        }
        for (name, yaml) in exemplar_shapes() {
            let shape = munarium_shapes::parse_shape(yaml)
                .unwrap_or_else(|e| panic!("exemplar shape '{name}': {e}"));
            let findings = munarium_shapes::validate::validate_shape(&shape);
            assert!(
                munarium_shapes::validate::is_valid(&findings),
                "exemplar shape '{name}' has error findings: {findings:?}"
            );
        }
    }
}
