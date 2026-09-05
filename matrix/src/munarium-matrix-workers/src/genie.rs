// SPDX-License-Identifier: Apache-2.0
//! The planner-assist and evaluation policy.
//!
//! Vendor-neutral by construction: everything here reasons about a
//! `PlannerMessage`, and the one vendor that produces one today (Databricks
//! AI/BI Genie) is not named in the logic. That is deliberate — the *policy*
//! is the part that has to be the same for whoever's planner arrives next.
//!
//! ```text
//!   question ─▶ planner space ─▶ message
//!                                  │
//!                    ┌─────────────┴──────────────┐
//!            no query attachment          a query attachment
//!                    │                            │
//!   evaluation: record the prose        allowlist check
//!   planner-assist: REFUSE                        │
//!                                    admitted ─▶ the caller runs it
//!                                                through a CONTRACT
//! ```
//!
//! # Why nothing here executes anything
//!
//! An admitted proposal is not run by this module. It goes back to the caller
//! to be executed through the ordinary contract path, because that is where
//! the SQL compiler's allowlist walk, the budget, the effective identity and
//! the seal live. A worker that ran the SQL itself would be a **second
//! execution path with its own limits** — which is the shape every other
//! surface in this system was built to avoid.
//!
//! A consequence worth stating plainly: **a planner's SQL frequently will not
//! survive that path.** The allowlist walk refuses `SELECT *`, subqueries,
//! non-deterministic functions and undeclared tables, and a generative surface
//! writes all of those. That is not a defect. A query Matrix cannot verify is
//! a query Matrix will not seal, and reporting the refusal is more useful than
//! sealing something nobody can check.
//!
//! # `genie_plan_unpinned` is a label, not a failure
//!
//! Reproducing a planner's answer needs the space's own configuration — its
//! instructions, its example SQL, its trusted assets — and no vendor API
//! returns that. So the pin carries what it CAN (space, conversation, message,
//! attachment, statement, and a hash of the query text) and marks the plan
//! unpinned. The result stays real, sealed and replayable; what is missing is
//! a claim about the *decision*. Collapsing that distinction — by refusing
//! outright, or by presenting a planner's answer beside a contract's without
//! saying — is the thing this module exists to prevent.

use munarium_matrix_adapter::planner::{PlannerMessage, PlannerPin, PlannerSpec};
use munarium_matrix_adapter::{Limits, SourceAdapter};
use munarium_matrix_core::{Refusal, RefusalClass, Result};

/// Which mode the caller asked for.
///
/// Not a boolean. `Evaluation` and `PlannerAssist` differ in what they
/// **refuse**, and a flag named `strict` would leave a reader guessing which
/// way it points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlannerMode {
    /// Measure the planner. Prose with no query is a recorded outcome, not an
    /// error — "it declined to answer from data" is a result worth scoring.
    Evaluation,
    /// Use the planner as a planner. A proposal that names no allowlisted
    /// asset, or carries no query at all, is refused and nothing runs.
    PlannerAssist,
}

/// What a planner interaction produced.
///
/// `prose` and `admitted_sql` are separate fields on purpose. A planner's
/// answer in words is a thing to SCORE, never a thing to cite, and an envelope
/// that merged the two would invite a caller to quote the prose as though a
/// seal backed it.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannerOutcome {
    pub pin: PlannerPin,
    /// What the planner said, in words. Never citable.
    pub prose: Option<String>,
    /// The SQL it proposed, verbatim — whether or not it was admitted.
    /// Recorded even when refused, because "what did it try to run" is the
    /// question an operator asks first.
    pub proposed_sql: Option<String>,
    /// The SQL the allowlist admitted. `None` means nothing may run.
    pub admitted_sql: Option<String>,
    /// Why nothing was admitted, when nothing was. Never `None` beside a
    /// `None` admission — an outcome that says neither is an outcome refusing
    /// to say what happened.
    pub refusal: Option<Refusal>,
}

impl PlannerOutcome {
    /// A JSON envelope for a report or an API response.
    ///
    /// The unpinned note is spelled out rather than left to be inferred from
    /// `plan_pinned: false`. The distinction this whole surface turns on
    /// deserves a sentence.
    pub fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "pin": self.pin,
            "plan_pinned": self.pin.pinned,
            "prose": self.prose,
            "proposed_sql": self.proposed_sql,
            "admitted": self.admitted_sql.is_some(),
            "refusal": self.refusal.as_ref().map(|r| serde_json::json!({
                "class": r.class.as_str(),
                "code": r.code,
                "message": r.message,
            })),
            "note": if self.pin.pinned {
                "the plan behind this query is pinned"
            } else {
                "the plan behind this query is NOT pinned: sealed bytes are replayable, \
                 the decision that produced the query is not"
            },
        })
    }
}

/// Ask a planner a question and decide what may run.
///
/// The adapter must declare a planner surface; one that does not is
/// `not_covered` **by name** rather than by a silent no-op, because "nothing
/// happened" and "this source has no planner" look identical to a caller who
/// is told neither.
pub async fn ask(
    adapter: &dyn SourceAdapter,
    spec: &PlannerSpec,
    mode: PlannerMode,
    question: &str,
    limits: Limits,
) -> Result<PlannerOutcome> {
    if mode == PlannerMode::Evaluation && !spec.evaluation_enabled {
        return Err(Refusal::not_covered(format!(
            "space '{}' does not enable evaluation mode. Calling a model surface costs \
             money and produces output that must never be mistaken for a contract's, so \
             it is opted into rather than out of.",
            spec.space_id
        )));
    }

    let Some(message) = adapter
        .planner_ask(&spec.space_id, question, limits)
        .await?
    else {
        return Err(Refusal::not_covered(format!(
            "adapter '{}' has no planner surface; only a source that declares one can be \
             asked a question in words",
            adapter.kind()
        )));
    };

    Ok(decide(&message, spec, mode))
}

/// The decision, separated from the call so it can be tested exhaustively
/// without a network.
///
/// This is the part that matters, and it is pure: given what a planner said
/// and what a deployment declared, what may run?
pub fn decide(message: &PlannerMessage, spec: &PlannerSpec, mode: PlannerMode) -> PlannerOutcome {
    let pin = message.pin(&spec.space_id);
    let prose = message.prose.clone();
    let proposed = message.proposed_sql.clone();

    let Some(sql) = proposed.clone() else {
        // No query attachment. Evaluation records it; planner-assist has
        // nothing to check and nothing to seal.
        let refusal = match mode {
            PlannerMode::Evaluation => Refusal::not_covered(
                "the planner answered in prose with no query, so there is nothing to seal. \
                 Recorded for scoring.",
            ),
            PlannerMode::PlannerAssist => Refusal::not_covered(
                "the planner proposed no query, so there is nothing for the allowlist to \
                 admit",
            ),
        };
        return PlannerOutcome {
            pin,
            prose,
            proposed_sql: None,
            admitted_sql: None,
            refusal: Some(refusal),
        };
    };

    // Evaluation measures what the planner DOES. Applying the allowlist here
    // would measure the allowlist instead — but nothing is admitted either,
    // because an evaluation is not a licence to run.
    if mode == PlannerMode::Evaluation {
        return PlannerOutcome {
            pin,
            prose,
            proposed_sql: Some(sql),
            admitted_sql: None,
            refusal: Some(Refusal::not_covered(
                "evaluation mode records what the planner proposed and admits nothing: \
                 measuring a planner and trusting it are different acts",
            )),
        };
    }

    match message.trusted_asset_id.as_deref() {
        Some(asset_id) if spec.permits_asset(asset_id) => PlannerOutcome {
            pin,
            prose,
            proposed_sql: Some(sql.clone()),
            admitted_sql: Some(sql),
            refusal: None,
        },
        Some(asset_id) => PlannerOutcome {
            pin,
            prose,
            proposed_sql: Some(sql),
            admitted_sql: None,
            refusal: Some(munarium_matrix_adapter::planner::asset_not_allowed(
                asset_id, spec,
            )),
        },
        // Generated SQL, not a reviewed asset. Admitted only where the
        // deployment declared tables for it; the SQL compiler then does the
        // real work, and this check exists so a space that never intended
        // generated SQL does not get it by default.
        None if spec.allowed_tables.is_empty() => PlannerOutcome {
            pin,
            prose,
            proposed_sql: Some(sql),
            admitted_sql: None,
            refusal: Some(Refusal::new(
                RefusalClass::Denied,
                "genie_asset_not_allowed",
                format!(
                    "the planner generated SQL rather than resolving a trusted asset, and \
                     space '{}' declares no `allowedTables`. Generated SQL is admitted only \
                     where a deployment said so.",
                    spec.space_id
                ),
            )),
        },
        None => PlannerOutcome {
            pin,
            prose,
            proposed_sql: Some(sql.clone()),
            admitted_sql: Some(sql),
            refusal: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(trusted: &[&str], tables: &[&str], evaluation: bool) -> PlannerSpec {
        PlannerSpec {
            space_id: "01ef".into(),
            trusted_assets: trusted.iter().map(|s| s.to_string()).collect(),
            allowed_tables: tables.iter().map(|s| s.to_string()).collect(),
            evaluation_enabled: evaluation,
        }
    }

    fn message(sql: Option<&str>, asset: Option<&str>) -> PlannerMessage {
        PlannerMessage {
            conversation_id: "c".into(),
            message_id: "m".into(),
            attachment_id: Some("a".into()),
            statement_id: sql.map(|_| "s".into()),
            proposed_sql: sql.map(str::to_string),
            trusted_asset_id: asset.map(str::to_string),
            prose: Some("here you go".into()),
        }
    }

    #[test]
    fn a_trusted_asset_on_the_allowlist_is_admitted() {
        let out = decide(
            &message(Some("SELECT 1"), Some("asset-ok")),
            &spec(&["asset-ok"], &[], false),
            PlannerMode::PlannerAssist,
        );
        assert_eq!(out.admitted_sql.as_deref(), Some("SELECT 1"));
        assert!(out.refusal.is_none());
        // Admitted is not executed: the caller still runs it through a
        // contract, where the compiler's allowlist walk and the budget apply.
        assert!(!out.pin.pinned);
    }

    #[test]
    fn a_trusted_asset_off_the_allowlist_is_denied_and_the_sql_is_still_recorded() {
        let out = decide(
            &message(Some("SELECT 1"), Some("asset-other")),
            &spec(&["asset-ok"], &[], false),
            PlannerMode::PlannerAssist,
        );
        assert!(out.admitted_sql.is_none());
        // "What did it try to run" is the first question an operator asks.
        assert_eq!(out.proposed_sql.as_deref(), Some("SELECT 1"));
        let r = out.refusal.unwrap();
        assert_eq!(r.code, "genie_asset_not_allowed");
        assert_eq!(r.class, RefusalClass::Denied);
    }

    #[test]
    fn generated_sql_needs_a_declared_table_allowlist() {
        // No `id` means the planner wrote it rather than resolving something
        // a human reviewed. A space that never intended that does not get it.
        let out = decide(
            &message(Some("SELECT * FROM t"), None),
            &spec(&["asset-ok"], &[], false),
            PlannerMode::PlannerAssist,
        );
        assert!(out.admitted_sql.is_none());
        assert_eq!(out.refusal.unwrap().code, "genie_asset_not_allowed");

        let out = decide(
            &message(Some("SELECT * FROM t"), None),
            &spec(&[], &["opportunities"], false),
            PlannerMode::PlannerAssist,
        );
        // Admitted HERE, and still subject to the compiler downstream — which
        // will refuse this particular `SELECT *`. Two different gates, and
        // this one is not pretending to be the other.
        assert_eq!(out.admitted_sql.as_deref(), Some("SELECT * FROM t"));
    }

    #[test]
    fn prose_with_no_query_is_recorded_in_evaluation_and_refused_in_assist() {
        let s = spec(&["asset-ok"], &[], true);
        let m = PlannerMessage {
            conversation_id: "c".into(),
            message_id: "m".into(),
            prose: Some("I could not find a table for that.".into()),
            ..Default::default()
        };

        let eval = decide(&m, &s, PlannerMode::Evaluation);
        assert_eq!(
            eval.prose.as_deref(),
            Some("I could not find a table for that.")
        );
        assert!(eval
            .refusal
            .unwrap()
            .message
            .contains("Recorded for scoring"));

        let assist = decide(&m, &s, PlannerMode::PlannerAssist);
        assert!(assist.admitted_sql.is_none());
        assert!(assist
            .refusal
            .unwrap()
            .message
            .contains("nothing for the allowlist to admit"));
    }

    #[test]
    fn evaluation_never_admits_anything_even_for_an_allowlisted_asset() {
        // Measuring a planner and trusting it are different acts. An
        // evaluation that quietly admitted its own subject would be an
        // evaluation that changed what it measured.
        let out = decide(
            &message(Some("SELECT 1"), Some("asset-ok")),
            &spec(&["asset-ok"], &[], true),
            PlannerMode::Evaluation,
        );
        assert!(out.admitted_sql.is_none());
        assert_eq!(out.proposed_sql.as_deref(), Some("SELECT 1"));
        assert!(out.refusal.unwrap().message.contains("admits nothing"));
    }

    #[test]
    fn an_outcome_always_says_why_when_it_admits_nothing() {
        // The invariant that keeps the envelope honest: never a `None`
        // admission beside a `None` refusal.
        let s = spec(&["asset-ok"], &[], true);
        for (m, mode) in [
            (
                message(Some("SELECT 1"), Some("nope")),
                PlannerMode::PlannerAssist,
            ),
            (message(None, None), PlannerMode::PlannerAssist),
            (message(None, None), PlannerMode::Evaluation),
            (
                message(Some("SELECT 1"), Some("asset-ok")),
                PlannerMode::Evaluation,
            ),
        ] {
            let out = decide(&m, &s, mode);
            assert_eq!(
                out.admitted_sql.is_none(),
                out.refusal.is_some(),
                "an outcome must say why it admitted nothing"
            );
        }
    }

    #[test]
    fn the_envelope_states_the_unpinned_distinction_in_words() {
        let out = decide(
            &message(Some("SELECT 1"), Some("asset-ok")),
            &spec(&["asset-ok"], &[], false),
            PlannerMode::PlannerAssist,
        );
        let v = out.describe();
        assert_eq!(v["plan_pinned"], serde_json::json!(false));
        let note = v["note"].as_str().unwrap();
        assert!(note.contains("replayable"));
        assert!(note.contains("NOT pinned"));
        // Prose stays its own field: an envelope that merged it with evidence
        // would invite quoting it as though a seal backed it.
        assert!(v["prose"].is_string());
    }
}
