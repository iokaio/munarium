// SPDX-License-Identifier: Apache-2.0
//! Research profiles and data views — the declarative half of the
//! evidence hierarchy.
//!
//! A runbook says *what evidence exists and in what order it is trusted*;
//! `munarium-core`'s `hierarchy` says what a layer's output can be used for.
//! Nothing here executes anything.
//!
//! # Why this validates so hard at apply time
//!
//! Every check in [`validate_research`] is a check that would otherwise fire
//! mid-turn, in front of a user, with money already spent. A profile naming a
//! collection that does not exist is not a runtime condition to handle
//! gracefully — it is a runbook that was never correct, and the moment to say
//! so is when someone applies it.
//!
//! The sharpest one is the `preserveCompleteResult` budget check. A layer can
//! declare that its table must survive composition **whole or not at all**,
//! because half a table is not a smaller true answer, it is a false one. If
//! that layer is `required` and its `maxBytes` cannot fit inside
//! `completion.contextCharBudget`, then every turn that runbook ever serves
//! must refuse. That is a contradiction in the document, and it is caught here
//! rather than discovered one turn at a time.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// A pre-declared Munarium Matrix query contract this runbook may read.
///
/// **The model never writes SQL.** A turn selects a data view by name from
/// this list, and Matrix executes the contract that name is bound to. The
/// injection surface is not defended against — it does not exist.
/// Which kind of Matrix asset a runbook data view binds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DataViewKind {
    /// `POST /v1/contracts/{name}/execute` with a structured query intent.
    #[default]
    Contract,
    /// `POST /v1/metricviews/{name}/execute` with a semantic intent.
    MetricView,
    /// `POST /v1/dataviews/{name}/execute` with a semantic intent.
    DataView,
}

impl DataViewKind {
    /// The Matrix route family for this kind.
    pub fn route(self) -> &'static str {
        match self {
            DataViewKind::Contract => "contracts",
            DataViewKind::MetricView => "metricviews",
            DataViewKind::DataView => "dataviews",
        }
    }
    pub fn is_semantic(self) -> bool {
        !matches!(self, DataViewKind::Contract)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataViewSpec {
    /// Referred to from a layer as `matrix:<name>`.
    pub name: String,
    /// The Matrix contract, `name@version`. Pinned: a contract that changed
    /// under a runbook is a different question being answered.
    pub contract: String,
    /// A metric view or a native data view when `kind` says so, in which
    /// case the turn asks it with a semantic intent the `intent` model task
    /// produced; a query contract by default.
    #[serde(default)]
    pub kind: DataViewKind,
    /// Human description, surfaced to the intent resolver so it can choose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Values for the contract's declared parameters, as
    /// `name: {type, value}`.
    ///
    /// Bound in the RUNBOOK, never at turn time. A contract with a required
    /// parameter is unreachable without this — and letting a turn supply one
    /// would hand the caller a knob on a query the whole point of which is
    /// that it was declared in advance.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameters: BTreeMap<String, DataViewParam>,
    /// Access level a session must dominate to read this view.
    #[serde(default)]
    pub access_level: i32,
    /// Need-to-know tags a session must carry (all of them).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compartments: Vec<String>,
}

/// One bound contract parameter. `type` is the contract's declared type
/// (`date`, `string`, `int64`, `decimal`, …) and `value` is always TEXT —
/// a decimal parameter that round-tripped through a JSON number would arrive
/// at the source having lost the precision the contract was written to keep.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataViewParam {
    #[serde(rename = "type")]
    pub kind: String,
    pub value: String,
}

/// Evidence labelling for a collection.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionEvidenceSpec {
    /// Free labels stamped onto evidence sealed from this collection, so a
    /// report can group by them without re-deriving provenance.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerRequirementSpec {
    /// The turn refuses if this layer produces no evidence.
    Required,
    #[default]
    Optional,
    /// Consulted only if an earlier layer produced nothing.
    Fallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerRoleSpec {
    Supporting,
    #[default]
    Primary,
    Controlling,
}

/// One layer of a research profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResearchLayerSpec {
    pub name: String,
    /// Pinned sources: a collection name, `facts`, `scope:<prefix>`, or
    /// `matrix:<dataView>`. Resolved at APPLY time — a turn cannot widen its
    /// own reach by naming something new.
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub requirement: LayerRequirementSpec,
    #[serde(default)]
    pub role: AnswerRoleSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_char_budget: Option<usize>,
    /// Whole-or-nothing composition. See the module note.
    #[serde(default)]
    pub preserve_complete_result: bool,
    /// The largest this layer's result is expected to be. Only meaningful
    /// beside `preserveCompleteResult`, where it is what makes the budget
    /// contradiction checkable at apply time instead of at turn time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u64>,
}

/// An ordered hierarchy of evidence layers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResearchProfileSpec {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Ordered. Execution order IS the hierarchy.
    pub layers: Vec<ResearchLayerSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_char_budget: Option<usize>,
}

/// Every way a research profile can be wrong.
#[derive(Debug, Clone, PartialEq)]
pub enum ResearchError {
    DuplicateProfile(String),
    DuplicateLayer {
        profile: String,
        layer: String,
    },
    DuplicateDataView(String),
    EmptyProfile(String),
    EmptyLayerSources {
        profile: String,
        layer: String,
    },
    UnknownSource {
        profile: String,
        layer: String,
        source: String,
    },
    UnknownDefaultProfile(String),
    /// A required, whole-or-nothing layer that can never fit.
    PreserveExceedsBudget {
        profile: String,
        layer: String,
        max_bytes: usize,
        budget: usize,
    },
    /// A `preserveCompleteResult` layer with no `maxBytes` cannot be checked,
    /// so the contradiction above would only ever appear at turn time.
    PreserveWithoutMaxBytes {
        profile: String,
        layer: String,
    },
    /// Every layer is a fallback, so nothing ever runs first.
    AllFallback(String),
    MalformedName(String),
    /// A data view's `contract` is interpolated into the Matrix request
    /// PATH; anything but `name@version` could reroute the server's
    /// authenticated call inside Matrix.
    MalformedContract {
        view: String,
        contract: String,
    },
}

impl std::fmt::Display for ResearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateProfile(n) => write!(f, "duplicate research profile '{n}'"),
            Self::DuplicateLayer { profile, layer } => {
                write!(f, "profile '{profile}' declares layer '{layer}' twice")
            }
            Self::DuplicateDataView(n) => write!(f, "duplicate data view '{n}'"),
            Self::EmptyProfile(n) => write!(f, "research profile '{n}' declares no layers"),
            Self::EmptyLayerSources { profile, layer } => write!(
                f,
                "profile '{profile}' layer '{layer}' names no sources; a layer that reads nothing cannot contribute evidence"
            ),
            Self::UnknownSource { profile, layer, source } => write!(
                f,
                "profile '{profile}' layer '{layer}' names source '{source}', which is not a declared collection, data view, or fact scope"
            ),
            Self::UnknownDefaultProfile(n) => write!(
                f,
                "retrieval.defaultResearchProfile '{n}' is not a declared profile"
            ),
            Self::PreserveExceedsBudget { profile, layer, max_bytes, budget } => write!(
                f,
                "profile '{profile}' layer '{layer}' is required and preserves its complete result, but its maxBytes {max_bytes} cannot fit completion.contextCharBudget {budget}; every turn using this profile would refuse"
            ),
            Self::PreserveWithoutMaxBytes { profile, layer } => write!(
                f,
                "profile '{profile}' layer '{layer}' sets preserveCompleteResult but no maxBytes, so its context budget cannot be checked before a turn runs"
            ),
            Self::AllFallback(n) => write!(
                f,
                "research profile '{n}' declares only fallback layers, so no layer ever runs"
            ),
            Self::MalformedName(n) => write!(
                f,
                "name '{n}' must be non-empty and must not contain whitespace or ':'"
            ),
            Self::MalformedContract { view, contract } => write!(
                f,
                "data view '{view}' names contract '{contract}'; a contract ref is \
                 'name@version' with the name drawn from [A-Za-z0-9._-] and a numeric version"
            ),
        }
    }
}

fn name_ok(n: &str) -> bool {
    !n.trim().is_empty() && !n.contains(':') && !n.chars().any(char::is_whitespace)
}

/// `name@version`: a path-safe name and a numeric version. The contract ref
/// is spliced into `{base}/v1/{route}/{contract}/execute`, so `..`, `/`, `?`
/// and `#` are exactly the characters that must never appear in it.
fn contract_ref_ok(c: &str) -> bool {
    let Some((name, version)) = c.split_once('@') else {
        return false;
    };
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
        && name != "."
        && name != ".."
        && !version.is_empty()
        && version.chars().all(|ch| ch.is_ascii_digit())
}

/// Fail closed. Every error here would otherwise surface mid-turn.
///
/// `collections` and `data_views` are the universe of pinnable sources;
/// `context_char_budget` is `completion.contextCharBudget` if the runbook
/// declares one.
pub fn validate_research(
    profiles: &[ResearchProfileSpec],
    data_views: &[DataViewSpec],
    default_profile: Option<&str>,
    collections: &[String],
    context_char_budget: Option<usize>,
) -> Result<(), ResearchError> {
    let mut view_names = BTreeSet::new();
    for v in data_views {
        if !name_ok(&v.name) {
            return Err(ResearchError::MalformedName(v.name.clone()));
        }
        if !contract_ref_ok(&v.contract) {
            return Err(ResearchError::MalformedContract {
                view: v.name.clone(),
                contract: v.contract.clone(),
            });
        }
        if !view_names.insert(v.name.as_str()) {
            return Err(ResearchError::DuplicateDataView(v.name.clone()));
        }
    }

    let mut profile_names = BTreeSet::new();
    for p in profiles {
        if !name_ok(&p.name) {
            return Err(ResearchError::MalformedName(p.name.clone()));
        }
        if !profile_names.insert(p.name.as_str()) {
            return Err(ResearchError::DuplicateProfile(p.name.clone()));
        }
        if p.layers.is_empty() {
            return Err(ResearchError::EmptyProfile(p.name.clone()));
        }
        if p.layers
            .iter()
            .all(|l| l.requirement == LayerRequirementSpec::Fallback)
        {
            return Err(ResearchError::AllFallback(p.name.clone()));
        }

        let mut layer_names = BTreeSet::new();
        for l in &p.layers {
            if l.name.trim().is_empty() {
                return Err(ResearchError::MalformedName(l.name.clone()));
            }
            if !layer_names.insert(l.name.as_str()) {
                return Err(ResearchError::DuplicateLayer {
                    profile: p.name.clone(),
                    layer: l.name.clone(),
                });
            }
            if l.sources.is_empty() {
                return Err(ResearchError::EmptyLayerSources {
                    profile: p.name.clone(),
                    layer: l.name.clone(),
                });
            }
            for s in &l.sources {
                let known = if let Some(view) = s.strip_prefix("matrix:") {
                    view_names.contains(view)
                } else if s.starts_with("facts:") {
                    // A fact layer must name the memory version it reads.
                    // Sessions carry no version binding today, so a bare
                    // `facts` could only ever refuse at turn time — and a
                    // runbook that validates but always refuses is the same
                    // vacuous-green trap in reverse. It is named as
                    // `facts:<version_id>` and checked here.
                    s.len() > "facts:".len()
                } else if s.starts_with("scope:") {
                    s.len() > "scope:".len()
                } else {
                    collections.iter().any(|c| c == s)
                };
                if !known {
                    return Err(ResearchError::UnknownSource {
                        profile: p.name.clone(),
                        layer: l.name.clone(),
                        source: s.clone(),
                    });
                }
            }

            if l.preserve_complete_result {
                let Some(max_bytes) = l.max_bytes else {
                    return Err(ResearchError::PreserveWithoutMaxBytes {
                        profile: p.name.clone(),
                        layer: l.name.clone(),
                    });
                };
                // The layer's own budget wins if it declares one, else the
                // profile's, else the runbook's completion budget.
                let budget = l
                    .context_char_budget
                    .or(p.context_char_budget)
                    .or(context_char_budget);
                if let Some(budget) = budget {
                    // Only a REQUIRED layer makes this fatal. An optional
                    // whole-or-nothing layer that does not fit simply
                    // contributes nothing, which is a legitimate design.
                    if l.requirement == LayerRequirementSpec::Required && max_bytes > budget {
                        return Err(ResearchError::PreserveExceedsBudget {
                            profile: p.name.clone(),
                            layer: l.name.clone(),
                            max_bytes,
                            budget,
                        });
                    }
                }
            }
        }
    }

    if let Some(d) = default_profile {
        if !profile_names.contains(d) {
            return Err(ResearchError::UnknownDefaultProfile(d.to_string()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(name: &str, sources: &[&str]) -> ResearchLayerSpec {
        ResearchLayerSpec {
            name: name.into(),
            sources: sources.iter().map(|s| s.to_string()).collect(),
            requirement: LayerRequirementSpec::Optional,
            role: AnswerRoleSpec::Primary,
            context_char_budget: None,
            preserve_complete_result: false,
            max_bytes: None,
            deadline_ms: None,
        }
    }

    fn profile(name: &str, layers: Vec<ResearchLayerSpec>) -> ResearchProfileSpec {
        ResearchProfileSpec {
            name: name.into(),
            description: None,
            layers,
            context_char_budget: None,
        }
    }

    fn view(name: &str) -> DataViewSpec {
        DataViewSpec {
            name: name.into(),
            contract: "revenue@1".into(),
            kind: DataViewKind::Contract,
            parameters: BTreeMap::new(),
            description: None,
            access_level: 0,
            compartments: vec![],
        }
    }

    fn cols() -> Vec<String> {
        vec!["contracts".into(), "minutes".into()]
    }

    #[test]
    fn a_well_formed_profile_validates() {
        let p = profile(
            "diligence",
            vec![
                layer("register", &["matrix:revenue"]),
                layer("documents", &["contracts", "minutes"]),
                layer("ledger", &["facts:ver-1"]),
            ],
        );
        assert_eq!(
            validate_research(&[p], &[view("revenue")], Some("diligence"), &cols(), None),
            Ok(())
        );
    }

    #[test]
    fn an_unknown_source_is_refused_at_apply() {
        let p = profile("d", vec![layer("l", &["nonexistent"])]);
        match validate_research(&[p], &[], None, &cols(), None) {
            Err(ResearchError::UnknownSource { source, .. }) => assert_eq!(source, "nonexistent"),
            other => panic!("expected UnknownSource, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_matrix_view_is_refused_even_though_the_prefix_is_right() {
        let p = profile("d", vec![layer("l", &["matrix:not_declared"])]);
        match validate_research(&[p], &[view("revenue")], None, &cols(), None) {
            Err(ResearchError::UnknownSource { source, .. }) => {
                assert_eq!(source, "matrix:not_declared")
            }
            other => panic!("expected UnknownSource, got {other:?}"),
        }
    }

    #[test]
    fn a_required_whole_table_that_cannot_fit_the_budget_is_a_contradiction() {
        // The sharpest check: this runbook would refuse EVERY turn. Finding
        // that out one turn at a time, after paying for each, is the failure
        // mode the check exists to prevent.
        let mut l = layer("register", &["matrix:revenue"]);
        l.requirement = LayerRequirementSpec::Required;
        l.preserve_complete_result = true;
        l.max_bytes = Some(64_000);
        let p = profile("d", vec![l]);
        match validate_research(&[p], &[view("revenue")], None, &cols(), Some(16_000)) {
            Err(ResearchError::PreserveExceedsBudget {
                max_bytes, budget, ..
            }) => {
                assert_eq!(max_bytes, 64_000);
                assert_eq!(budget, 16_000);
            }
            other => panic!("expected PreserveExceedsBudget, got {other:?}"),
        }
    }

    #[test]
    fn the_same_layer_optional_is_allowed() {
        // Optional and whole-or-nothing is a legitimate design: it contributes
        // when it fits and stays silent when it does not. Only `required`
        // turns a too-large table into a guaranteed refusal.
        let mut l = layer("register", &["matrix:revenue"]);
        l.requirement = LayerRequirementSpec::Optional;
        l.preserve_complete_result = true;
        l.max_bytes = Some(64_000);
        let p = profile("d", vec![l]);
        assert_eq!(
            validate_research(&[p], &[view("revenue")], None, &cols(), Some(16_000)),
            Ok(())
        );
    }

    #[test]
    fn a_layer_budget_overrides_the_completion_budget() {
        let mut l = layer("register", &["matrix:revenue"]);
        l.requirement = LayerRequirementSpec::Required;
        l.preserve_complete_result = true;
        l.max_bytes = Some(64_000);
        l.context_char_budget = Some(100_000);
        let p = profile("d", vec![l]);
        assert_eq!(
            validate_research(&[p], &[view("revenue")], None, &cols(), Some(16_000)),
            Ok(()),
            "the layer asked for room and got it"
        );
    }

    #[test]
    fn preserve_without_max_bytes_is_refused_because_it_cannot_be_checked() {
        let mut l = layer("register", &["matrix:revenue"]);
        l.preserve_complete_result = true;
        let p = profile("d", vec![l]);
        assert!(matches!(
            validate_research(&[p], &[view("revenue")], None, &cols(), Some(16_000)),
            Err(ResearchError::PreserveWithoutMaxBytes { .. })
        ));
    }

    #[test]
    fn an_all_fallback_profile_never_runs_anything() {
        let mut l = layer("l", &["contracts"]);
        l.requirement = LayerRequirementSpec::Fallback;
        let p = profile("d", vec![l]);
        assert!(matches!(
            validate_research(&[p], &[], None, &cols(), None),
            Err(ResearchError::AllFallback(_))
        ));
    }

    #[test]
    fn a_sourceless_layer_is_refused() {
        let p = profile("d", vec![layer("l", &[])]);
        assert!(matches!(
            validate_research(&[p], &[], None, &cols(), None),
            Err(ResearchError::EmptyLayerSources { .. })
        ));
    }

    #[test]
    fn duplicates_and_a_dangling_default_are_all_refused() {
        let dup = vec![
            profile("d", vec![layer("l", &["contracts"])]),
            profile("d", vec![layer("l", &["contracts"])]),
        ];
        assert!(matches!(
            validate_research(&dup, &[], None, &cols(), None),
            Err(ResearchError::DuplicateProfile(_))
        ));

        let dup_layer = profile(
            "d",
            vec![layer("l", &["contracts"]), layer("l", &["minutes"])],
        );
        assert!(matches!(
            validate_research(&[dup_layer], &[], None, &cols(), None),
            Err(ResearchError::DuplicateLayer { .. })
        ));

        assert!(matches!(
            validate_research(&[], &[view("v"), view("v")], None, &cols(), None),
            Err(ResearchError::DuplicateDataView(_))
        ));

        let p = profile("d", vec![layer("l", &["contracts"])]);
        assert!(matches!(
            validate_research(&[p], &[], Some("missing"), &cols(), None),
            Err(ResearchError::UnknownDefaultProfile(_))
        ));
    }

    #[test]
    fn a_colon_in_a_name_is_refused_because_it_collides_with_the_source_prefix() {
        // `matrix:x` is how a layer names a data view; a view literally named
        // `matrix:x` would make source resolution ambiguous.
        let p = profile("bad:name", vec![layer("l", &["contracts"])]);
        assert!(matches!(
            validate_research(&[p], &[], None, &cols(), None),
            Err(ResearchError::MalformedName(_))
        ));
    }

    #[test]
    fn a_fact_layer_naming_its_version_resolves() {
        let p = profile(
            "d",
            vec![layer("l", &["facts:ver-123", "scope:northgate/contracts"])],
        );
        assert_eq!(validate_research(&[p], &[], None, &cols(), None), Ok(()));
    }

    #[test]
    fn a_bare_facts_layer_is_refused_because_it_could_only_ever_refuse() {
        // Sessions carry no memory-version binding, so `facts` with no
        // version names nothing readable. Accepting it would ship a runbook
        // that validates and then refuses every turn.
        let p = profile("d", vec![layer("l", &["facts"])]);
        assert!(matches!(
            validate_research(&[p], &[], None, &cols(), None),
            Err(ResearchError::UnknownSource { .. })
        ));
    }
}
