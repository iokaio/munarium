// SPDX-License-Identifier: Apache-2.0
//! The evidence-provider seam — one shape for every kind of evidence
//! an answer can be built from.
//!
//! Today a turn retrieves documents. With Munarium Matrix it may also read a
//! governed table, an exact count, or a slice of the ledger's own facts. Those
//! are not the same kind of thing, and the mistake this module exists to
//! prevent is flattening them into one: a rendered database row is not a
//! passage that can be quoted, and a truncated table is not a basis for
//! "there are N of them".
//!
//! # The three ideas
//!
//! **A block is a closed set.** [`EvidenceBlock`] has exactly five variants,
//! and a consumer must handle all five. Adding a sixth is a deliberate,
//! compile-checked event rather than a `_ => {}` arm quietly ignoring a new
//! kind of evidence.
//!
//! **A truncated block cannot support a completeness claim.** G4 is enforced
//! by [`EvidenceBlock::supports_completeness`] rather than by remembering to
//! check a flag at each call site.
//!
//! **A refusal is a block, not an error.** A layer that declines — policy,
//! staleness, an unreachable source — still contributes something the
//! composition must reason about, and often must *disclose*. Turning it into
//! an `Err` would throw away the reason at exactly the moment the answer needs
//! to explain itself.
//!
//! Pure by construction: no I/O, no HTTP, no SQL. The providers live above
//! this, in `munarium-server`.

use serde::{Deserialize, Serialize};

use crate::retrieval::SearchHit;
use crate::types::Claim;
use crate::Result;

/// How much weight an answer may give a layer's evidence.
///
/// Mirrors `munarium_shapes::AuthorityRole` deliberately rather than importing
/// it: `munarium-shapes` depends on `jsonschema`, and this crate is the pure
/// kernel. The two are kept in step by
/// `runbooks::authority_role_matches_shape_ceiling`'s tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerRole {
    /// Corroborates; never decides alone.
    Supporting = 0,
    /// The ordinary answer-bearing role.
    Primary = 1,
    /// Decides a conflict.
    Controlling = 2,
}

impl AnswerRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Supporting => "supporting",
            Self::Primary => "primary",
            Self::Controlling => "controlling",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "supporting" => Some(Self::Supporting),
            "primary" => Some(Self::Primary),
            "controlling" => Some(Self::Controlling),
            _ => None,
        }
    }
}

/// Whether a layer's evidence is required for the answer to stand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerRequirement {
    /// The turn refuses if this layer cannot produce evidence.
    Required,
    /// Contributes when available; silence is fine.
    Optional,
    /// Consulted only when an earlier layer produced nothing.
    Fallback,
}

/// One layer of a research profile, resolved and ready to execute.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceLayer {
    /// Stable name, used in SSE stages and the hierarchy decision.
    pub name: String,
    /// The pinned sources this layer reads — collection names or data-view
    /// refs. Pinned at apply time, never resolved at turn time, so a turn
    /// cannot silently widen its own reach.
    pub sources: Vec<String>,
    pub requirement: LayerRequirement,
    pub role: AnswerRole,
    /// Character budget for this layer's contribution to the composed context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_char_budget: Option<usize>,
    /// A complete table must survive composition intact or not be used at all:
    /// half a table is not a smaller true answer, it is a false one.
    #[serde(default)]
    pub preserve_complete_result: bool,
    /// Milliseconds this layer may take before it is abandoned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u64>,
}

/// What a turn intends to find out — produced by the SERVER's model resolver
/// under the `intent` task, never by Matrix.
///
/// The `explicit` flag matters for testing: compose conformance must run
/// keyless, so a caller may supply an intent directly instead of paying for a
/// model call. Recording which one happened keeps a measured result honest
/// about whether a planner was involved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryIntent {
    pub question: String,
    /// Free-form typed intent kind, e.g. `lookup`, `aggregation`, `comparison`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// True when supplied by the caller rather than produced by a model.
    #[serde(default)]
    pub explicit: bool,
    /// What the `intent` model task chose to ask each semantic data view —
    /// measures, dimensions and equality filters from the view's declared
    /// lists — keyed by the runbook's data-view name.
    /// Empty for a turn with no semantic views, or with no intent task.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub selections: std::collections::BTreeMap<String, SemanticSelection>,
}

/// One data view's semantic selection: names only, never SQL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SemanticSelection {
    pub measures: Vec<String>,
    #[serde(default)]
    pub dimensions: Vec<String>,
    #[serde(default)]
    pub filters: Vec<SemanticFilterSelection>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticFilterSelection {
    pub dimension: String,
    pub value: String,
    /// The dimension's declared type, carried so the value is bound as it.
    #[serde(default = "default_filter_type")]
    pub ty: String,
}

fn default_filter_type() -> String {
    "string".into()
}

/// The resolved plan for one turn: which layers, in what order, under what
/// budgets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidencePlan {
    /// The research profile this plan came from.
    pub profile: String,
    pub intent: QueryIntent,
    /// Ordered. Execution order IS the hierarchy: earlier layers outrank later
    /// ones when composition has to choose.
    pub layers: Vec<EvidenceLayer>,
    /// Total context budget across all layers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_char_budget: Option<usize>,
    /// Conflicts between layers are preserved and disclosed, never silently
    /// resolved in favour of the higher layer. v1 has exactly this policy; the
    /// field exists so a future alternative is a visible change.
    #[serde(default = "default_conflict_policy")]
    pub conflicts: String,
}

fn default_conflict_policy() -> String {
    "preserve_and_disclose".into()
}

/// Why a layer produced nothing usable. Typed, so composition can disclose the
/// reason without leaking what it could not show.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceRefusal {
    /// Kebab-case, matching the server's problem registry.
    pub code: String,
    /// Safe to show a caller. Must never name a source the caller could not
    /// otherwise see — the hidden-required-layer rule.
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// A count with the coverage that makes it meaningful.
///
/// A bare number is not evidence: "1,204" is only an answer if you also know
/// what was counted and what was excluded. Mode A seals exactly this.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CountBlock {
    pub value: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows_covered: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows_excluded: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusion_reason: Option<String>,
    /// The sealed artifact this count came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_id: Option<String>,
}

/// A typed result table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableBlock {
    /// Column names, in order.
    pub columns: Vec<String>,
    /// Row cells as canonical text — never JSON numbers, because a
    /// `decimal(38,2)` does not survive an IEEE-754 double.
    pub rows: Vec<Vec<Option<String>>>,
    /// The id of each row, positionally aligned with `rows`.
    ///
    /// Taken from the SEALER, never invented here. Matrix's
    /// `identity.row_id_rule` is `keys` for a keyed result, so a row's id is
    /// its key (`"EMEA"`), not its position — and a citation
    /// `[evidence/<id>#EMEA]` has to resolve against the id the artifact can
    /// actually replay. Numbering them here would reject every correct
    /// citation.
    ///
    /// Empty for a block with no sealed identity, in which case the renderer
    /// falls back to 1-based positions.
    #[serde(default)]
    pub row_ids: Vec<String>,
    /// G4. A truncated table cannot back a completeness claim, and
    /// [`EvidenceBlock::supports_completeness`] is what enforces that.
    pub truncated: bool,
    /// The sealed artifact, so every cell is citable as
    /// `[evidence/<id>#<row_id>]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_id: Option<String>,
}

/// What a layer contributed. **Closed** — a new variant is a deliberate,
/// compile-checked change to what an answer can be built from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvidenceBlock {
    /// The existing document-retrieval path.
    DocumentHits { hits: Vec<SearchHit> },
    /// A complete (or explicitly truncated) typed table.
    CompleteTable(TableBlock),
    /// An exact count with its coverage.
    Count(CountBlock),
    /// A pinned slice of the ledger's own facts.
    FactSlice { claims: Vec<Claim> },
    /// The layer declined, and the reason is part of the answer.
    Refusal(EvidenceRefusal),
}

impl EvidenceBlock {
    /// May an answer make a completeness claim ("all", "there are N",
    /// "none besides these") on this block?
    ///
    /// The whole of G4 in one predicate. A truncated table and a refusal are
    /// the obvious noes; **document hits are also a no**, and that is the one
    /// worth stating: retrieval returns the top-k it found, never a proof that
    /// nothing else exists. Treating a good search as exhaustive is how a
    /// system says "there are no other contracts" when it means "I found
    /// three".
    pub fn supports_completeness(&self) -> bool {
        match self {
            Self::CompleteTable(t) => !t.truncated,
            Self::Count(_) => true,
            Self::DocumentHits { .. } => false,
            Self::FactSlice { .. } => false,
            Self::Refusal(_) => false,
        }
    }

    /// Did this layer produce anything an answer can use?
    pub fn is_empty(&self) -> bool {
        match self {
            Self::DocumentHits { hits } => hits.is_empty(),
            Self::CompleteTable(t) => t.rows.is_empty(),
            Self::Count(_) => false,
            Self::FactSlice { claims } => claims.is_empty(),
            Self::Refusal(_) => true,
        }
    }

    pub fn is_refusal(&self) -> bool {
        matches!(self, Self::Refusal(_))
    }

    /// The sealed artifact behind this block, when there is one.
    pub fn evidence_id(&self) -> Option<&str> {
        match self {
            Self::CompleteTable(t) => t.evidence_id.as_deref(),
            Self::Count(c) => c.evidence_id.as_deref(),
            _ => None,
        }
    }

    /// A short, stable label for logs, SSE stages and the hierarchy decision.
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::DocumentHits { .. } => "document_hits",
            Self::CompleteTable(_) => "complete_table",
            Self::Count(_) => "count",
            Self::FactSlice { .. } => "fact_slice",
            Self::Refusal(_) => "refusal",
        }
    }
}

/// One layer's outcome, as recorded on the turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerOutcome {
    pub layer: String,
    pub role: AnswerRole,
    pub requirement: LayerRequirement,
    /// The block kind, or `refusal`.
    pub block: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_id: Option<String>,
    pub supports_completeness: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal_code: Option<String>,
    pub elapsed_ms: u64,
}

/// What the hierarchy actually did, persisted per turn.
///
/// This is the audit answer to "why did the model see what it saw?", and it is
/// deliberately about the DECISION rather than the content: which profile, which
/// pinned sources, which layers ran, which refused, whether any completeness
/// claim was permissible. Evidence rows never appear here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceHierarchyDecision {
    pub profile: String,
    pub intent_kind: Option<String>,
    /// True when the intent was supplied rather than modelled — so a keyless
    /// test result never looks like a planner result.
    pub intent_explicit: bool,
    pub layers: Vec<LayerOutcome>,
    /// Whether ANY block in the hierarchy could support a completeness claim.
    pub completeness_available: bool,
    /// Conflicts detected between layers and deliberately preserved.
    #[serde(default)]
    pub disclosed_conflicts: usize,
    pub conflicts_policy: String,
}

impl EvidenceHierarchyDecision {
    /// Did a required layer fail to produce evidence?
    ///
    /// The turn must refuse when this is true — that is what `required` means.
    /// The refusal must NOT name the layer's sources: a caller who cannot see
    /// a source must not learn of it from the shape of a refusal, which is the
    /// hidden-required-layer rule.
    pub fn required_layer_failed(&self) -> Option<&LayerOutcome> {
        self.layers
            .iter()
            .find(|l| l.requirement == LayerRequirement::Required && l.block == "refusal")
    }
}

/// A source of evidence for one layer.
///
/// The seam. Documents, governed tables, counts and ledger facts all arrive
/// through this one shape, which is what lets a research profile order them
/// without the pipeline knowing what any of them are.
///
/// **`fetch` returns `Ok(Refusal)`, not `Err`, for anything the caller should
/// know about.** A provider that cannot answer — denied, stale, unreachable,
/// out of time — has still told the turn something, and often something the
/// answer must disclose. `Err` is reserved for a bug: a malformed plan, a
/// poisoned lock, an invariant broken inside the provider itself.
#[async_trait::async_trait]
pub trait EvidenceProvider: Send + Sync {
    /// Stable id, used in the hierarchy decision and SSE `layer_source`.
    fn id(&self) -> &str;

    /// Can this provider serve the layer's pinned sources? Checked when a
    /// profile is applied, so a broken binding fails at apply rather than
    /// mid-turn.
    fn can_serve(&self, source: &str) -> bool;

    async fn fetch(&self, layer: &EvidenceLayer, intent: &QueryIntent) -> Result<EvidenceBlock>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(truncated: bool, rows: usize) -> EvidenceBlock {
        EvidenceBlock::CompleteTable(TableBlock {
            columns: vec!["region".into()],
            rows: (0..rows).map(|i| vec![Some(format!("r{i}"))]).collect(),
            row_ids: Vec::new(),
            truncated,
            evidence_id: Some("ev-1".into()),
        })
    }

    #[test]
    fn a_truncated_table_cannot_support_completeness() {
        assert!(table(false, 2).supports_completeness());
        assert!(!table(true, 2).supports_completeness());
    }

    #[test]
    fn document_hits_never_support_completeness() {
        // The one worth stating: retrieval returns what it found, never a
        // proof that nothing else exists. "There are no other contracts" and
        // "I found three" are different claims.
        let b = EvidenceBlock::DocumentHits { hits: vec![] };
        assert!(!b.supports_completeness());
    }

    #[test]
    fn a_refusal_supports_nothing_and_is_empty() {
        let b = EvidenceBlock::Refusal(EvidenceRefusal {
            code: "source-unavailable".into(),
            message: "the source did not answer".into(),
            source: None,
        });
        assert!(!b.supports_completeness());
        assert!(b.is_empty());
        assert!(b.is_refusal());
        assert_eq!(b.kind_str(), "refusal");
    }

    #[test]
    fn a_count_supports_completeness_and_is_never_empty() {
        // A zero count is an ANSWER, not an absence of one.
        let b = EvidenceBlock::Count(CountBlock {
            value: 0,
            rows_covered: Some(0),
            rows_excluded: None,
            exclusion_reason: None,
            evidence_id: Some("ev-2".into()),
        });
        assert!(b.supports_completeness());
        assert!(!b.is_empty(), "a count of zero is still a count");
    }

    #[test]
    fn a_block_round_trips_through_json_by_kind() {
        for b in [
            table(false, 1),
            EvidenceBlock::Count(CountBlock {
                value: 7,
                rows_covered: None,
                rows_excluded: None,
                exclusion_reason: None,
                evidence_id: None,
            }),
            EvidenceBlock::Refusal(EvidenceRefusal {
                code: "policy-denied".into(),
                message: "no".into(),
                source: None,
            }),
        ] {
            let text = serde_json::to_string(&b).expect("serialize");
            assert!(text.contains("\"kind\""), "the tag must be on the wire");
            let back: EvidenceBlock = serde_json::from_str(&text).expect("deserialize");
            assert_eq!(b, back);
        }
    }

    #[test]
    fn a_failed_required_layer_is_findable() {
        let d = EvidenceHierarchyDecision {
            profile: "p".into(),
            intent_kind: None,
            intent_explicit: true,
            layers: vec![
                LayerOutcome {
                    layer: "docs".into(),
                    role: AnswerRole::Primary,
                    requirement: LayerRequirement::Optional,
                    block: "document_hits".into(),
                    evidence_id: None,
                    supports_completeness: false,
                    refusal_code: None,
                    elapsed_ms: 3,
                },
                LayerOutcome {
                    layer: "register".into(),
                    role: AnswerRole::Controlling,
                    requirement: LayerRequirement::Required,
                    block: "refusal".into(),
                    evidence_id: None,
                    supports_completeness: false,
                    refusal_code: Some("source-unavailable".into()),
                    elapsed_ms: 40,
                },
            ],
            completeness_available: false,
            disclosed_conflicts: 0,
            conflicts_policy: "preserve_and_disclose".into(),
        };
        let failed = d.required_layer_failed().expect("must find it");
        assert_eq!(failed.layer, "register");

        // An optional layer refusing is NOT a turn-level failure.
        let mut ok = d.clone();
        ok.layers[1].requirement = LayerRequirement::Optional;
        assert!(ok.required_layer_failed().is_none());
    }

    #[test]
    fn answer_roles_are_ordered_and_parse() {
        assert!(AnswerRole::Supporting < AnswerRole::Primary);
        assert!(AnswerRole::Primary < AnswerRole::Controlling);
        assert_eq!(
            AnswerRole::parse("controlling"),
            Some(AnswerRole::Controlling)
        );
        assert_eq!(AnswerRole::parse("nonsense"), None);
    }

    #[test]
    fn the_default_conflict_policy_is_preserve_and_disclose() {
        let plan: EvidencePlan =
            serde_json::from_str(r#"{"profile":"p","intent":{"question":"q"},"layers":[]}"#)
                .expect("deserialize");
        assert_eq!(plan.conflicts, "preserve_and_disclose");
        assert!(!plan.intent.explicit, "explicit defaults false");
    }
}
