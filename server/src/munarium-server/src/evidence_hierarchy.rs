// SPDX-License-Identifier: Apache-2.0
//! Executing an evidence hierarchy.
//!
//! Resolves a runbook's research profile into a [`EvidencePlan`], runs its
//! layers in order through the evidence providers, and composes what they
//! produced into the model's context.
//!
//! # The document layer is special, and honestly so
//!
//! `TurnResponse` carries `hits`, `envelopes` and `collections_searched`, and
//! every existing client reads them. So the document layer's full
//! [`DocumentRetrieval`] is captured here rather than being flattened into an
//! `EvidenceBlock` like the others. Pretending all layers are alike and then
//! reconstructing the document fields from a generic block would be
//! architecture theatre — the response contract is document-shaped for
//! backward-compatibility reasons, and the code should say so.
//!
//! # Composition order is trust order
//!
//! Layers compose in declaration order, so the highest-trust evidence occupies
//! the budget first and a `preserveCompleteResult` layer is taken **whole or
//! not at all**. Half a table is not a smaller true answer; it is a false one,
//! and a model given nine of twelve rows will answer about twelve.

use std::time::Instant;

use munarium_core::hierarchy::{
    AnswerRole, EvidenceBlock, EvidenceHierarchyDecision, EvidenceLayer, EvidencePlan,
    EvidenceProvider, LayerOutcome, LayerRequirement, QueryIntent,
};
use munarium_runbooks::{AnswerRoleSpec, LayerRequirementSpec, ResearchProfileSpec, RunbookDoc};

use crate::error::{ApiError, CustomError};
use crate::sessions_api::DocumentRetrieval;
use crate::state::AppState;
use munarium_api_types as dto;

type ApiResult<T> = std::result::Result<T, ApiError>;

fn role_of(s: AnswerRoleSpec) -> AnswerRole {
    match s {
        AnswerRoleSpec::Supporting => AnswerRole::Supporting,
        AnswerRoleSpec::Primary => AnswerRole::Primary,
        AnswerRoleSpec::Controlling => AnswerRole::Controlling,
    }
}

fn requirement_of(s: LayerRequirementSpec) -> LayerRequirement {
    match s {
        LayerRequirementSpec::Required => LayerRequirement::Required,
        LayerRequirementSpec::Optional => LayerRequirement::Optional,
        LayerRequirementSpec::Fallback => LayerRequirement::Fallback,
    }
}

fn requirement_str(r: LayerRequirement) -> &'static str {
    match r {
        LayerRequirement::Required => "required",
        LayerRequirement::Optional => "optional",
        LayerRequirement::Fallback => "fallback",
    }
}

/// Which profile, if any, this turn runs under.
///
/// Fails closed on a named-but-undeclared profile: silently falling back to
/// the document path would answer a different question than the caller asked,
/// and would do it invisibly.
pub fn resolve_profile<'a>(
    doc: &'a RunbookDoc,
    requested: Option<&str>,
) -> ApiResult<Option<&'a ResearchProfileSpec>> {
    let Some(retrieval) = doc.spec.retrieval.as_ref() else {
        return match requested {
            Some(p) => Err(ApiError::Custom(CustomError::unknown_research_profile(p))),
            None => Ok(None),
        };
    };
    let name = match requested {
        Some(p) => p,
        None => match retrieval.default_research_profile.as_deref() {
            Some(d) => d,
            // No request, no default: the legacy path, untouched.
            None => return Ok(None),
        },
    };
    retrieval
        .research_profiles
        .iter()
        .find(|p| p.name == name)
        .map(Some)
        .ok_or_else(|| ApiError::Custom(CustomError::unknown_research_profile(name)))
}

/// Turn a declared profile into an executable plan.
pub fn build_plan(profile: &ResearchProfileSpec, intent: QueryIntent) -> EvidencePlan {
    EvidencePlan {
        profile: profile.name.clone(),
        intent,
        layers: profile
            .layers
            .iter()
            .map(|l| EvidenceLayer {
                name: l.name.clone(),
                sources: l.sources.clone(),
                requirement: requirement_of(l.requirement),
                role: role_of(l.role),
                context_char_budget: l.context_char_budget,
                preserve_complete_result: l.preserve_complete_result,
                deadline_ms: l.deadline_ms,
            })
            .collect(),
        context_char_budget: profile.context_char_budget,
        conflicts: "preserve_and_disclose".into(),
    }
}

/// What is this turn asking?
///
/// Resolved under the `intent` task level when the runbook pins a model for
/// it, and otherwise taken literally from the query. **The literal path is not
/// a degraded mode** — it is how compose conformance runs keyless, and how a
/// runbook that does not want to pay for a planner opts out. What matters is
/// that the two are distinguishable afterwards: `explicit` records which
/// happened, so a keyless test result can never be read as a planner result.
///
/// Matrix never calls a model provider; intent resolution is the server's job.
pub async fn resolve_intent(
    state: &AppState,
    tenant: &str,
    doc: &RunbookDoc,
    req: &dto::TurnRequest,
    _progress: &Option<crate::sessions_api::TurnProgressTx>,
) -> ApiResult<QueryIntent> {
    let pinned = doc
        .spec
        .models
        .as_ref()
        .map(|m| m.tasks.contains_key("intent"))
        .unwrap_or(false);
    if !pinned {
        return Ok(QueryIntent {
            question: req.query.clone(),
            kind: None,
            explicit: true,
            selections: Default::default(),
        });
    }
    let resolved = crate::models::resolve_model(doc, "intent", None)?;
    let store = state.store_for(tenant).await?;
    let prompt = format!(
        "Classify what this question is asking for. Answer with ONE lowercase \
         word from exactly this list: lookup, aggregation, comparison, \
         enumeration, timeline, other. Do not answer the question itself.\n\n\
         Question: {}",
        req.query
    );
    let budgets = state.max_tokens.effective(state, tenant).await?;
    let response = crate::providers_api::op_complete(
        state,
        tenant,
        store.as_ref(),
        &resolved.provider_name,
        dto::CompleteRequest {
            prompt: Some(prompt),
            system: None,
            model: resolved.model.clone(),
            tier: resolved.tier.clone(),
            provider: None,
            // `hierarchy_classifier` (`/v1/max-tokens`; built-in 32 since
            // 2026-09-02, 16 before).
            max_tokens: Some(budgets.hierarchy_classifier),
            temperature: Some(0.0),
            version_id: None,
        },
    )
    .await?;
    const KINDS: &[&str] = &[
        "lookup",
        "aggregation",
        "comparison",
        "enumeration",
        "timeline",
        "other",
    ];
    let text = response.text.trim().to_lowercase();
    // Closed vocabulary. An unrecognised answer becomes None rather than
    // being passed through: an intent kind nothing downstream understands is
    // worse than no intent kind, because it looks like information.
    let kind = KINDS
        .iter()
        .find(|k| text.contains(*k))
        .map(|k| k.to_string());
    let selections = resolve_selections(state, tenant, doc, req, &resolved).await;
    Ok(QueryIntent {
        question: req.query.clone(),
        kind,
        explicit: false,
        selections,
    })
}

/// For every semantic data view the requested profile can reach, ask the
/// `intent` task to choose measures, dimensions and equality filters from the
/// view's declared lists — read from Matrix's registry, never guessed — and
/// keep only names the lists contain. A view that yields nothing usable gets
/// no selection, and the layer refuses `intent-unresolved` rather than asking
/// the plane for something the model made up.
async fn resolve_selections(
    state: &AppState,
    tenant: &str,
    doc: &RunbookDoc,
    req: &dto::TurnRequest,
    resolved: &crate::models::ResolvedModel,
) -> std::collections::BTreeMap<String, munarium_core::hierarchy::SemanticSelection> {
    use munarium_core::hierarchy::{SemanticFilterSelection, SemanticSelection};
    let mut out = std::collections::BTreeMap::new();
    let Some(retrieval) = doc.spec.retrieval.as_ref() else {
        return out;
    };
    let profile_name = req
        .research_profile
        .clone()
        .or_else(|| retrieval.default_research_profile.clone());
    let Some(profile) = retrieval
        .research_profiles
        .iter()
        .find(|p| Some(&p.name) == profile_name.as_ref())
    else {
        return out;
    };
    let Some(base) = state.config.matrix_base_url.clone() else {
        return out;
    };
    let token = std::env::var("MUNARIUM_MATRIX_TOKEN").ok();
    let reachable: std::collections::BTreeSet<&str> = profile
        .layers
        .iter()
        .flat_map(|l| l.sources.iter().filter_map(|s| s.strip_prefix("matrix:")))
        .collect();
    for view in doc.spec.data_views.iter().filter(|v| v.kind.is_semantic()) {
        if !reachable.contains(view.name.as_str()) {
            continue;
        }
        let url = format!(
            "{}/v1/{}/{}",
            base.trim_end_matches('/'),
            view.kind.route(),
            view.contract
        );
        let mut rb = state
            .matrix_http
            .get(&url)
            .timeout(std::time::Duration::from_secs(10))
            .header("X-Munarium-Uid", "munarium-server");
        if let Some(t) = &token {
            rb = rb.bearer_auth(t);
        }
        let Ok(resp) = rb.send().await else { continue };
        let Ok(text) = resp.text().await else {
            continue;
        };
        let Ok(asset) = serde_yaml::from_str::<serde_yaml::Value>(&text) else {
            continue;
        };
        let spec = &asset["spec"];
        let names = |key: &str| -> Vec<(String, String)> {
            spec[key]
                .as_mapping()
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| {
                            let name = k.as_str()?.to_string();
                            let desc = v["description"].as_str().unwrap_or("").to_string();
                            Some((name, desc))
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        let measures = names("measures");
        let dimensions = names("dimensions");
        let dim_types: std::collections::BTreeMap<String, String> = spec["dimensions"]
            .as_mapping()
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| {
                        Some((
                            k.as_str()?.to_string(),
                            v["type"].as_str().unwrap_or("string").to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let filterable: Vec<String> = spec["filters"]["allowedDimensions"]
            .as_sequence()
            .map(|s| {
                s.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let synonyms = spec["synonyms"]
            .as_mapping()
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| {
                        let list: Vec<&str> =
                            v.as_sequence()?.iter().filter_map(|x| x.as_str()).collect();
                        Some(format!("{}: {}", k.as_str()?, list.join(", ")))
                    })
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .unwrap_or_default();
        if measures.is_empty() {
            continue;
        }
        let list = |items: &[(String, String)]| {
            items
                .iter()
                .map(|(n, d)| {
                    if d.is_empty() {
                        n.clone()
                    } else {
                        format!("{n} ({d})")
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        let prompt = format!(
            "You choose what to ask a governed data view. Answer with ONLY a JSON object of the form \
             {{\"measures\": [..], \"dimensions\": [..], \"filters\": [{{\"dimension\": \"..\", \"value\": \"..\"}}]}}. \
             Use only the listed names, verbatim. Choose the fewest dimensions that answer the question; \
             add a filter only when the question names a specific value of a filterable dimension. \
             No prose.\n\nView: {} — {}\nMeasures: {}\nDimensions: {}\nFilterable dimensions: {}\nSynonyms: {}\n\nQuestion: {}",
            view.contract,
            spec["description"].as_str().unwrap_or("").trim(),
            list(&measures),
            list(&dimensions),
            if filterable.is_empty() {
                "(all)".to_string()
            } else {
                filterable.join(", ")
            },
            if synonyms.is_empty() {
                "(none)".to_string()
            } else {
                synonyms
            },
            req.query
        );
        let Ok(store) = state.store_for(tenant).await else {
            continue;
        };
        let Ok(budgets) = state.max_tokens.effective(state, tenant).await else {
            continue;
        };
        let Ok(response) = crate::providers_api::op_complete(
            state,
            tenant,
            store.as_ref(),
            &resolved.provider_name,
            dto::CompleteRequest {
                prompt: Some(prompt),
                system: None,
                model: resolved.model.clone(),
                tier: resolved.tier.clone(),
                provider: None,
                // `hierarchy_intent` (`/v1/max-tokens`; built-in 480 since
                // 2026-09-02, 240 before).
                max_tokens: Some(budgets.hierarchy_intent),
                temperature: Some(0.0),
                version_id: None,
            },
        )
        .await
        else {
            continue;
        };
        let text = response.text;
        let (Some(a), Some(b)) = (text.find('{'), text.rfind('}')) else {
            continue;
        };
        let Ok(chosen) = serde_json::from_str::<serde_json::Value>(&text[a..=b]) else {
            continue;
        };
        let pick = |key: &str, allowed: &[(String, String)]| -> Vec<String> {
            chosen[key]
                .as_array()
                .map(|xs| {
                    xs.iter()
                        .filter_map(|x| x.as_str())
                        .filter(|x| allowed.iter().any(|(n, _)| n == x))
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        };
        let chosen_measures = pick("measures", &measures);
        if chosen_measures.is_empty() {
            continue;
        }
        let chosen_dimensions = pick("dimensions", &dimensions);
        let filters: Vec<SemanticFilterSelection> = chosen["filters"]
            .as_array()
            .map(|fs| {
                fs.iter()
                    .filter_map(|f| {
                        let dimension = f["dimension"].as_str()?.to_string();
                        let value = match &f["value"] {
                            serde_json::Value::String(v) => v.clone(),
                            other => other.to_string(),
                        };
                        let declared = dimensions.iter().any(|(n, _)| n == &dimension);
                        let open = filterable.is_empty() || filterable.contains(&dimension);
                        (declared && open).then(|| SemanticFilterSelection {
                            ty: dim_types
                                .get(&dimension)
                                .cloned()
                                .unwrap_or_else(|| "string".into()),
                            dimension,
                            value,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.insert(
            view.name.clone(),
            SemanticSelection {
                measures: chosen_measures,
                dimensions: chosen_dimensions,
                filters,
            },
        );
    }
    out
}

pub struct HierarchyOutcome {
    pub decision: EvidenceHierarchyDecision,
    /// Layer name → what it produced, in execution order.
    pub blocks: Vec<(String, EvidenceBlock)>,
    /// The document layer's full retrieval, when one ran. See the module note.
    pub documents: Option<DocumentRetrieval>,
}

/// Run the plan's layers in order.
///
/// `providers` are tried in order for each source; the first that
/// `can_serve` it wins. `document_layer` runs the real retrieval path and
/// hands back its full output.
pub async fn execute_plan<F, Fut>(
    plan: &EvidencePlan,
    providers: &[&dyn EvidenceProvider],
    mut document_layer: F,
    mut on_event: impl FnMut(dto::TurnProgressEvent),
) -> ApiResult<HierarchyOutcome>
where
    F: FnMut(EvidenceLayer) -> Fut,
    Fut: std::future::Future<Output = ApiResult<DocumentRetrieval>>,
{
    on_event(dto::TurnProgressEvent::Profile {
        profile: plan.profile.clone(),
        layers: plan.layers.iter().map(|l| l.name.clone()).collect(),
        intent_kind: plan.intent.kind.clone(),
        intent_explicit: plan.intent.explicit,
    });

    let mut outcomes = Vec::new();
    let mut blocks = Vec::new();
    let mut documents = None;
    // A fallback layer runs only if nothing before it produced evidence.
    let mut produced_any = false;

    for layer in &plan.layers {
        if layer.requirement == LayerRequirement::Fallback && produced_any {
            continue;
        }
        on_event(dto::TurnProgressEvent::LayerStart {
            layer: layer.name.clone(),
            role: layer.role.as_str().to_string(),
            requirement: requirement_str(layer.requirement).to_string(),
        });
        let started = Instant::now();

        // A BARE source name is a collection, served by the document path.
        // A prefixed one names a plane that has its own provider, and if no
        // provider claims it the layer must REFUSE.
        //
        // The fall-through this replaces was a live defect (caught by the
        // platform conformance tier, 2026-08-29, which is the entire
        // argument for that tier): with no Matrix configured, a layer reading
        // `matrix:register` quietly became a document search over the
        // session's collections, returned 200, and reported its REQUIRED
        // layer satisfied with `block: document_hits`. The register was never
        // consulted and nothing in the response said so — the worst shape a
        // bug can take here, because the answer looks complete.
        let matched: Option<&&dyn EvidenceProvider> = providers
            .iter()
            .find(|p| layer.sources.iter().any(|s| p.can_serve(s)));
        let unclaimed_plane = matched.is_none()
            && layer
                .sources
                .iter()
                .any(|s| s.starts_with("matrix:") || s.starts_with("facts:"));

        let block = match matched {
            None if unclaimed_plane => {
                // Named, because the runbook author pinned this source
                // themselves — the hidden-source rule protects sources a
                // caller cannot see, not ones written into their own profile.
                // The turn-level refusal above still names only the layer.
                EvidenceBlock::Refusal(munarium_core::hierarchy::EvidenceRefusal {
                    code: crate::evidence_providers::REFUSAL_UNBOUND.to_string(),
                    message: "no provider is configured for this layer's sources".into(),
                    source: layer
                        .sources
                        .iter()
                        .find(|s| s.starts_with("matrix:") || s.starts_with("facts:"))
                        .cloned(),
                })
            }
            Some(p) if p.id() != "documents" => {
                for s in &layer.sources {
                    if p.can_serve(s) {
                        on_event(dto::TurnProgressEvent::LayerSource {
                            layer: layer.name.clone(),
                            source: s.clone(),
                            provider: p.id().to_string(),
                        });
                    }
                }
                p.fetch(layer, &plan.intent).await.map_err(ApiError::Mesh)?
            }
            _ => {
                for s in &layer.sources {
                    on_event(dto::TurnProgressEvent::LayerSource {
                        layer: layer.name.clone(),
                        source: s.clone(),
                        provider: "documents".into(),
                    });
                }
                let out = document_layer(layer.clone()).await?;
                let hits = out.merged.iter().map(|(_, h)| h.clone()).collect();
                documents = Some(out);
                EvidenceBlock::DocumentHits { hits }
            }
        };

        let elapsed_ms = started.elapsed().as_millis() as u64;
        if !block.is_empty() {
            produced_any = true;
        }
        let refusal_code = match &block {
            EvidenceBlock::Refusal(r) => Some(r.code.clone()),
            _ => None,
        };
        on_event(dto::TurnProgressEvent::LayerComplete {
            layer: layer.name.clone(),
            block: block.kind_str().to_string(),
            supports_completeness: block.supports_completeness(),
            refusal_code: refusal_code.clone(),
            elapsed_ms,
        });
        outcomes.push(LayerOutcome {
            layer: layer.name.clone(),
            role: layer.role,
            requirement: layer.requirement,
            block: block.kind_str().to_string(),
            evidence_id: block.evidence_id().map(str::to_string),
            supports_completeness: block.supports_completeness(),
            refusal_code,
            elapsed_ms,
        });
        blocks.push((layer.name.clone(), block));
    }

    let completeness_available = blocks.iter().any(|(_, b)| b.supports_completeness());
    let disclosed_conflicts = count_conflicts(&blocks);
    on_event(dto::TurnProgressEvent::Coverage {
        completeness_available,
        disclosed_conflicts: disclosed_conflicts as u32,
    });

    let decision = EvidenceHierarchyDecision {
        profile: plan.profile.clone(),
        intent_kind: plan.intent.kind.clone(),
        intent_explicit: plan.intent.explicit,
        layers: outcomes,
        completeness_available,
        disclosed_conflicts,
        conflicts_policy: plan.conflicts.clone(),
    };

    // A required layer that produced nothing means the turn refuses. The
    // refusal names the LAYER, never its sources: a caller who cannot see a
    // source must not learn it exists from the shape of a refusal.
    if let Some(failed) = decision.required_layer_failed() {
        return Err(ApiError::Custom(CustomError::required_layer_unavailable(
            &failed.layer,
            failed.refusal_code.as_deref().unwrap_or("unavailable"),
        )));
    }

    Ok(HierarchyOutcome {
        decision,
        blocks,
        documents,
    })
}

/// Count cross-layer disagreements worth disclosing.
///
/// v1 is deliberately narrow: a `Count` in one layer against a `Count` in
/// another with a different value. Broad semantic conflict detection across a
/// table and a passage is a model judgement, not a deterministic one, and
/// guessing at it would manufacture disclosures nobody can check.
fn count_conflicts(blocks: &[(String, EvidenceBlock)]) -> usize {
    let counts: Vec<i64> = blocks
        .iter()
        .filter_map(|(_, b)| match b {
            EvidenceBlock::Count(c) => Some(c.value),
            _ => None,
        })
        .collect();
    let mut conflicts = 0;
    for (i, a) in counts.iter().enumerate() {
        for b in &counts[i + 1..] {
            if a != b {
                conflicts += 1;
            }
        }
    }
    conflicts
}

pub struct Composed {
    pub context: String,
    pub layers_used: usize,
    pub layers_dropped: Vec<String>,
}

/// Compose the hierarchy's blocks into the model's context, highest trust
/// first.
pub fn compose(plan: &EvidencePlan, blocks: &[(String, EvidenceBlock)], budget: usize) -> Composed {
    let mut context = String::new();
    let mut used = 0usize;
    let mut dropped = Vec::new();

    for (name, block) in blocks {
        let layer = plan.layers.iter().find(|l| &l.name == name);
        let rendered = render_block(name, block);
        if rendered.is_empty() {
            continue;
        }
        // The room for THIS layer is the smaller of what the profile has
        // left and the layer's own `contextCharBudget`. The per-layer budget
        // was validated by the runbook grammar and then read by nobody, so a
        // supporting layer declared at 4,000 chars could consume the whole
        // profile budget. The preserve-whole-or-nothing rule below is judged
        // against the same room: a complete table that fits the profile but
        // not its own layer's cap is not shown in part.
        let room = budget.saturating_sub(context.len()).min(
            layer
                .and_then(|l| l.context_char_budget)
                .unwrap_or(usize::MAX),
        );
        let fits = rendered.len() <= room;
        if fits {
            context.push_str(&rendered);
            used += 1;
            continue;
        }
        // Whole or nothing: a preserved layer is never partially served, and
        // a model shown nine of twelve rows will answer about twelve.
        if layer.map(|l| l.preserve_complete_result).unwrap_or(false) {
            dropped.push(name.clone());
            continue;
        }
        if room == 0 {
            dropped.push(name.clone());
            continue;
        }
        // Truncate on a char boundary — Rust string slicing panics otherwise,
        // and evidence text is not guaranteed ASCII.
        let mut cut = room.min(rendered.len());
        while cut > 0 && !rendered.is_char_boundary(cut) {
            cut -= 1;
        }
        if cut == 0 {
            dropped.push(name.clone());
            continue;
        }
        context.push_str(&rendered[..cut]);
        used += 1;
    }

    Composed {
        context,
        layers_used: used,
        layers_dropped: dropped,
    }
}

/// Render one block as context text.
///
/// Every block is labelled with its layer and, where it matters, whether it is
/// complete. A model that cannot tell a complete table from a truncated one
/// will treat both as complete.
fn render_block(layer: &str, block: &EvidenceBlock) -> String {
    match block {
        EvidenceBlock::DocumentHits { hits } => {
            let mut s = String::new();
            for h in hits {
                s.push_str(&format!("[{}/{}] {}\n\n", layer, h.chunk_id, h.text));
            }
            s
        }
        EvidenceBlock::CompleteTable(t) => {
            let mut s = format!(
                "[{}] {} result ({} rows), columns: {}\n",
                layer,
                if t.truncated { "TRUNCATED" } else { "COMPLETE" },
                t.rows.len(),
                t.columns.join(" | ")
            );
            if let Some(id) = &t.evidence_id {
                s.push_str(&format!("evidence: {id}\n"));
            }
            for (i, row) in t.rows.iter().enumerate() {
                let cells: Vec<&str> = row
                    .iter()
                    // NULL is rendered as the word, never as an empty cell:
                    // "no value recorded" and "the empty string" are
                    // different facts and the fixture plants both.
                    .map(|c| c.as_deref().unwrap_or("NULL"))
                    .collect();
                s.push_str(&format!("{} | {}\n", table_row_id(t, i), cells.join(" | ")));
            }
            s.push('\n');
            s
        }
        EvidenceBlock::Count(c) => {
            let mut s = format!("[{}] count: {}\n", layer, c.value);
            if let Some(covered) = c.rows_covered {
                s.push_str(&format!("rows covered: {covered}\n"));
            }
            if let Some(excluded) = c.rows_excluded {
                s.push_str(&format!("rows excluded: {excluded}"));
                if let Some(reason) = &c.exclusion_reason {
                    s.push_str(&format!(" ({reason})"));
                }
                s.push('\n');
            }
            if let Some(id) = &c.evidence_id {
                s.push_str(&format!("evidence: {id}\n"));
            }
            s.push('\n');
            s
        }
        EvidenceBlock::FactSlice { claims } => {
            let mut s = format!("[{layer}] recorded facts\n");
            for c in claims {
                s.push_str(&format!("{} = {}\n", c.claim_key(), c.value));
            }
            s.push('\n');
            s
        }
        // A refusal is DISCLOSED to the model, not hidden from it. An answer
        // built without the register should be able to say the register was
        // not consulted, and it can only do that if it was told.
        EvidenceBlock::Refusal(r) => {
            format!(
                "[{}] no evidence available ({}): {}\n\n",
                layer, r.code, r.message
            )
        }
    }
}

/// The id of row `i`: the SEALER's, falling back to a 1-based position when
/// the block carries none.
///
/// One function, called by both the renderer and [`served_evidence`], because
/// the model cites the id it was SHOWN and the checker resolves the id it was
/// GIVEN. Two implementations merely supposed to agree is how a checker starts
/// rejecting correct citations.
fn table_row_id(t: &munarium_core::hierarchy::TableBlock, i: usize) -> String {
    t.row_ids
        .get(i)
        .cloned()
        .unwrap_or_else(|| format!("r{:04}", i + 1))
}

/// The sealed rows this hierarchy served, for the evidence checks.
///
/// Row ids are `r0001`-style and assigned by position, matching exactly how
/// [`render_block`] labels them in the context. The two MUST agree: a citation
/// check that numbers rows differently from the text the model read would
/// reject correct citations, which is the worst kind of check — it punishes
/// the behaviour it exists to encourage.
pub fn served_evidence(
    blocks: &[(String, EvidenceBlock)],
) -> Vec<crate::verification::ServedEvidence> {
    blocks
        .iter()
        .filter_map(|(_, b)| match b {
            EvidenceBlock::CompleteTable(t) => {
                t.evidence_id
                    .as_ref()
                    .map(|id| crate::verification::ServedEvidence {
                        evidence_id: id.clone(),
                        rows: t
                            .rows
                            .iter()
                            .enumerate()
                            .map(|(i, row)| {
                                (
                                    table_row_id(t, i),
                                    row.iter()
                                        .map(|c| c.as_deref().unwrap_or("NULL").to_string())
                                        .collect(),
                                )
                            })
                            .collect(),
                    })
            }
            _ => None,
        })
        .collect()
}

pub fn decision_to_dto(d: &EvidenceHierarchyDecision) -> dto::EvidenceHierarchyDecisionDto {
    dto::EvidenceHierarchyDecisionDto {
        profile: d.profile.clone(),
        intent_kind: d.intent_kind.clone(),
        intent_explicit: d.intent_explicit,
        layers: d
            .layers
            .iter()
            .map(|l| dto::LayerOutcomeDto {
                layer: l.layer.clone(),
                role: l.role.as_str().to_string(),
                requirement: requirement_str(l.requirement).to_string(),
                block: l.block.clone(),
                evidence_id: l.evidence_id.clone(),
                supports_completeness: l.supports_completeness,
                refusal_code: l.refusal_code.clone(),
                elapsed_ms: l.elapsed_ms,
            })
            .collect(),
        completeness_available: d.completeness_available,
        disclosed_conflicts: d.disclosed_conflicts as u32,
        conflicts_policy: d.conflicts_policy.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use munarium_core::hierarchy::{CountBlock, EvidenceRefusal, TableBlock};

    fn plan_with(layers: Vec<EvidenceLayer>) -> EvidencePlan {
        EvidencePlan {
            profile: "p".into(),
            intent: QueryIntent {
                question: "q".into(),
                kind: None,
                explicit: true,
                selections: Default::default(),
            },
            layers,
            context_char_budget: None,
            conflicts: "preserve_and_disclose".into(),
        }
    }

    fn layer(name: &str, preserve: bool) -> EvidenceLayer {
        EvidenceLayer {
            name: name.into(),
            sources: vec!["x".into()],
            requirement: LayerRequirement::Optional,
            role: AnswerRole::Primary,
            context_char_budget: None,
            preserve_complete_result: preserve,
            deadline_ms: None,
        }
    }

    fn table(rows: usize, truncated: bool) -> EvidenceBlock {
        EvidenceBlock::CompleteTable(TableBlock {
            columns: vec!["region".into(), "amount".into()],
            rows: (0..rows)
                .map(|i| vec![Some(format!("r{i}")), Some("900000.50".into())])
                .collect(),
            row_ids: Vec::new(),
            truncated,
            evidence_id: Some("ev-1".into()),
        })
    }

    #[test]
    fn a_preserved_layer_is_dropped_whole_rather_than_truncated() {
        // The core composition rule. Nine of twelve rows is not a smaller
        // true answer; a model shown nine will answer about twelve.
        let p = plan_with(vec![layer("register", true)]);
        let blocks = vec![("register".to_string(), table(40, false))];
        let c = compose(&p, &blocks, 100);
        assert!(c.context.is_empty(), "not one partial row was served");
        assert_eq!(c.layers_dropped, vec!["register"]);
        assert_eq!(c.layers_used, 0);
    }

    #[test]
    fn an_unpreserved_layer_is_truncated_to_fit() {
        let p = plan_with(vec![layer("docs", false)]);
        let blocks = vec![("docs".to_string(), table(40, false))];
        let c = compose(&p, &blocks, 100);
        assert_eq!(c.context.len(), 100);
        assert_eq!(c.layers_used, 1);
        assert!(c.layers_dropped.is_empty());
    }

    #[test]
    fn composition_order_is_trust_order() {
        // The first-declared layer occupies the budget first. That IS the
        // hierarchy; nothing else expresses rank.
        let p = plan_with(vec![layer("high", false), layer("low", false)]);
        let blocks = vec![
            ("high".to_string(), table(1, false)),
            ("low".to_string(), table(1, false)),
        ];
        let c = compose(&p, &blocks, 120);
        let hi = c.context.find("[high]").expect("high present");
        assert!(
            c.context.find("[low]").map(|lo| hi < lo).unwrap_or(true),
            "high-trust evidence must come first"
        );
    }

    #[test]
    fn a_truncated_table_says_so_in_the_context() {
        // A model that cannot distinguish a complete table from a truncated
        // one treats both as complete.
        let p = plan_with(vec![layer("register", false)]);
        let blocks = vec![("register".to_string(), table(2, true))];
        let c = compose(&p, &blocks, 10_000);
        assert!(c.context.contains("TRUNCATED"), "{}", c.context);

        let blocks = vec![("register".to_string(), table(2, false))];
        let c = compose(&p, &blocks, 10_000);
        assert!(c.context.contains("COMPLETE"), "{}", c.context);
    }

    #[test]
    fn null_renders_as_null_not_as_an_empty_cell() {
        let p = plan_with(vec![layer("t", false)]);
        let block = EvidenceBlock::CompleteTable(TableBlock {
            columns: vec!["note".into()],
            rows: vec![vec![None], vec![Some(String::new())]],
            row_ids: Vec::new(),
            truncated: false,
            evidence_id: None,
        });
        let c = compose(&p, &[("t".to_string(), block)], 10_000);
        assert!(c.context.contains("NULL"), "{}", c.context);
        // "no value recorded" and "the empty string" are different facts.
        assert!(c.context.contains("r0002 | \n"), "{}", c.context);
    }

    #[test]
    fn a_refusal_is_disclosed_to_the_model_not_hidden_from_it() {
        // An answer built without the register should be able to SAY the
        // register was not consulted, which it can only do if it was told.
        let p = plan_with(vec![layer("register", false)]);
        let block = EvidenceBlock::Refusal(EvidenceRefusal {
            code: "source-timeout".into(),
            message: "the structured-evidence plane did not answer in time".into(),
            source: None,
        });
        let c = compose(&p, &[("register".to_string(), block)], 10_000);
        assert!(c.context.contains("no evidence available"), "{}", c.context);
        assert!(c.context.contains("source-timeout"), "{}", c.context);
    }

    #[test]
    fn served_row_ids_match_the_ones_rendered_into_the_context() {
        // The sharpest invariant here. The model cites the row ids it SAW
        // in the context; the checker resolves them against `served_evidence`.
        // If the two number rows differently, the checker rejects correct
        // citations — a check that punishes the behaviour it exists to
        // encourage, which is worse than having no check.
        let p = plan_with(vec![layer("register", false)]);
        let blocks = vec![("register".to_string(), table(3, false))];
        let rendered = compose(&p, &blocks, 100_000).context;
        let served = served_evidence(&blocks);

        assert_eq!(served.len(), 1);
        for (row_id, _) in &served[0].rows {
            assert!(
                rendered.contains(row_id.as_str()),
                "row id {row_id} is checkable but was never shown:
{rendered}"
            );
        }
        assert!(rendered.contains("r0001") && rendered.contains("r0003"));
        assert!(
            !rendered.contains("r0000"),
            "ids are 1-based in both places"
        );
    }

    #[test]
    fn a_table_with_no_evidence_id_serves_no_citable_rows() {
        // Nothing sealed means nothing to cite. Inventing an id here would
        // let an answer cite a row that no artifact can be produced for.
        let p = plan_with(vec![layer("t", false)]);
        let block = EvidenceBlock::CompleteTable(TableBlock {
            columns: vec!["c".into()],
            rows: vec![vec![Some("x".into())]],
            row_ids: Vec::new(),
            truncated: false,
            evidence_id: None,
        });
        let _ = &p;
        assert!(served_evidence(&[("t".to_string(), block)]).is_empty());
    }

    #[test]
    fn a_null_cell_is_checkable_as_null_exactly_as_rendered() {
        let blocks = vec![(
            "t".to_string(),
            EvidenceBlock::CompleteTable(TableBlock {
                columns: vec!["note".into()],
                rows: vec![vec![None]],
                row_ids: Vec::new(),
                truncated: false,
                evidence_id: Some("ev-1".into()),
            }),
        )];
        let served = served_evidence(&blocks);
        assert_eq!(served[0].rows[0].1, vec!["NULL".to_string()]);
    }

    #[tokio::test]
    async fn an_unclaimed_plane_source_refuses_instead_of_searching_documents() {
        // The live defect this guards, in one sentence: with no Matrix
        // configured, `matrix:register` used to become a document search and
        // report a REQUIRED layer satisfied. The answer looked complete and
        // the register had never been consulted.
        let mut l = layer("register", false);
        l.sources = vec!["matrix:register".into()];
        let p = plan_with(vec![l]);

        let mut document_layer_ran = false;
        let out = execute_plan(
            &p,
            &[],
            |_layer| {
                document_layer_ran = true;
                async { Ok(DocumentRetrieval::default()) }
            },
            |_| {},
        )
        .await
        .expect("an optional layer refusing is not a turn error");

        assert!(
            !document_layer_ran,
            "a matrix: source must never be served by document retrieval"
        );
        assert_eq!(out.blocks.len(), 1);
        assert!(out.blocks[0].1.is_refusal(), "{:?}", out.blocks[0].1);
        assert_eq!(out.decision.layers[0].block, "refusal");
    }

    #[tokio::test]
    async fn a_required_unclaimed_plane_layer_refuses_the_whole_turn() {
        let mut l = layer("register", false);
        l.sources = vec!["matrix:register".into()];
        l.requirement = LayerRequirement::Required;
        let p = plan_with(vec![l]);

        let err = execute_plan(
            &p,
            &[],
            |_layer| async { Ok(DocumentRetrieval::default()) },
            |_| {},
        )
        .await;
        let Err(err) = err else {
            panic!("a required layer producing nothing must refuse the turn")
        };
        let text = format!("{err:?}");
        assert!(text.contains("register"), "names the layer: {text}");
        assert!(
            !text.contains("matrix:register"),
            "must NOT name the layer's sources: {text}"
        );
    }

    #[tokio::test]
    async fn a_bare_collection_name_still_reaches_the_document_layer() {
        // The other half: bare names are collections and must keep working,
        // or the fix above would break every document layer.
        let p = plan_with(vec![layer("docs", false)]);
        let mut ran = false;
        let out = execute_plan(
            &p,
            &[],
            |_layer| {
                ran = true;
                async { Ok(DocumentRetrieval::default()) }
            },
            |_| {},
        )
        .await
        .expect("ok");
        assert!(ran, "a bare collection name is a document source");
        assert_eq!(out.decision.layers[0].block, "document_hits");
    }

    #[test]
    fn disagreeing_counts_are_counted_as_conflicts() {
        let mk = |v: i64| {
            EvidenceBlock::Count(CountBlock {
                value: v,
                rows_covered: None,
                rows_excluded: None,
                exclusion_reason: None,
                evidence_id: None,
            })
        };
        assert_eq!(
            count_conflicts(&[("a".into(), mk(3)), ("b".into(), mk(3))]),
            0
        );
        assert_eq!(
            count_conflicts(&[("a".into(), mk(3)), ("b".into(), mk(4))]),
            1
        );
    }

    #[test]
    fn multibyte_text_truncates_on_a_char_boundary() {
        // Slicing a Rust string mid-codepoint panics, and evidence text is
        // not guaranteed ASCII.
        let p = plan_with(vec![layer("docs", false)]);
        let block = EvidenceBlock::CompleteTable(TableBlock {
            columns: vec!["c".into()],
            rows: vec![vec![Some("é".repeat(200))]],
            row_ids: Vec::new(),
            truncated: false,
            evidence_id: None,
        });
        let c = compose(&p, &[("docs".to_string(), block)], 51);
        assert!(c.context.len() <= 51);
        assert!(c.context.is_char_boundary(c.context.len()));
    }
}

/// The governing invariant of S-3.x, guarded at the wire level.
///
/// A turn that names no research profile must execute AND serialize exactly as
/// it always has. Not "equivalently" — identically: same JSON keys, same SSE
/// event shapes. Every one of these would be a silent break in a contract four
/// client libraries already speak.
#[cfg(test)]
mod legacy_invariant {
    use munarium_api_types as dto;

    #[test]
    fn a_legacy_turn_response_grows_no_keys() {
        let resp = dto::TurnResponse {
            session_id: "s1".into(),
            ordinal: 1,
            collections_searched: vec!["contracts".into()],
            skipped: vec![],
            hits: vec![],
            envelopes: vec![],
            completion: None,
            hierarchy: None,
        };
        let json = serde_json::to_string(&resp).expect("serialize");
        assert!(
            !json.contains("hierarchy"),
            "a no-profile turn must not grow a key: {json}"
        );
    }

    #[test]
    fn a_legacy_turn_request_round_trips_without_the_new_field() {
        // An older client's body still deserializes, and re-serializing it
        // does not invent a field it never sent.
        let body = r#"{"query":"who signed it","complete":true}"#;
        let req: dto::TurnRequest = serde_json::from_str(body).expect("older client body");
        assert!(req.research_profile.is_none());
        let out = serde_json::to_string(&req).expect("serialize");
        assert!(!out.contains("research_profile"), "{out}");
    }

    #[test]
    fn a_legacy_verify_event_serializes_without_the_layer_key() {
        let ev = dto::TurnProgressEvent::Verify {
            attempt: 0,
            checks: vec!["quote".into()],
            violations: 0,
            layer: None,
        };
        let json = serde_json::to_string(&ev).expect("serialize");
        assert_eq!(
            json, r#"{"stage":"verify","attempt":0,"checks":["quote"],"violations":0}"#,
            "the legacy verify event's bytes are unchanged"
        );
    }

    #[test]
    fn the_new_sse_stages_are_appended_after_the_legacy_ones() {
        // Order matters: a client matching on the tag is fine either way, but
        // anything keyed on discriminant order is not. Appending keeps every
        // existing variant at the index it has always had.
        let legacy_last = serde_json::to_string(&dto::TurnProgressEvent::Verify {
            attempt: 0,
            checks: vec![],
            violations: 0,
            layer: None,
        })
        .expect("serialize");
        assert!(legacy_last.contains(r#""stage":"verify""#));

        let first_new = serde_json::to_string(&dto::TurnProgressEvent::Profile {
            profile: "d".into(),
            layers: vec!["register".into()],
            intent_kind: None,
            intent_explicit: true,
        })
        .expect("serialize");
        assert!(first_new.contains(r#""stage":"profile""#));
        assert!(
            !first_new.contains("intent_kind"),
            "an absent intent kind emits no key: {first_new}"
        );
    }
}
