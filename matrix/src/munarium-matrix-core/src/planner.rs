// SPDX-License-Identifier: Apache-2.0
//! The conversational-planner seam.
//!
//! One vendor implements this today — Databricks AI/BI Genie — and the types
//! here are deliberately vendor-neutral anyway, for the same reason
//! `SemanticAsk` is: the worker that decides what a proposal is *allowed* to
//! do must not depend on whose planner made it. A `munarium-matrix-workers`
//! that imported the Databricks crate would make the policy vendor-specific,
//! and the policy is the part that has to be the same everywhere.
//!
//! It lives in the KERNEL rather than in the adapter crate because the ASSET
//! VALIDATOR needs [`PlannerSpec`] too, and `munarium-matrix-types` cannot
//! depend on `munarium-matrix-adapter` — that edge runs the other way. The
//! alternative was a second copy of the shape inside the validator, which is
//! how two definitions of one thing start disagreeing.
//!
//! The shape mirrors `semantic_execute`: `Ok(None)` from
//! [`SourceAdapter::planner_ask`](crate::SourceAdapter::planner_ask) means "I
//! have no planner surface", which is what every adapter but one says.
//!
//! # The one thing this seam exists to prevent
//!
//! A planner proposes; **Matrix decides**. A proposal is never executed
//! because a model was confident about it — Databricks itself asks users to
//! review a trusted-asset match, because even a trusted asset can be matched
//! to the wrong question. So a `PlannerMessage` carries no authority: it is
//! evidence about what a planner said, and everything downstream treats it
//! that way.

use serde::{Deserialize, Serialize};

use crate::{Refusal, RefusalClass, Result};

/// How a deployment declares which planner space may be reached and what a
/// proposal is allowed to resolve to.
///
/// The allowlist is **required and never empty**. A planner-assist mode with
/// no allowlist would be "run whatever the model wrote", which is the one
/// thing this system exists not to do — and defaulting it to "everything"
/// would make the safe posture the one nobody configures.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlannerSpec {
    /// The planner space. A governed object in the vendor's workspace; Matrix
    /// never creates one.
    pub space_id: String,
    /// Trusted asset identifiers a proposal may resolve to. Matched exactly —
    /// no prefixes, no globs. A glob here would be an allowlist that grows
    /// whenever somebody names a new asset conveniently.
    #[serde(default)]
    pub trusted_assets: Vec<String>,
    /// Tables a *generated* statement may read. Its presence is what admits
    /// generated SQL at all; the SQL compiler's allowlist walk then does the
    /// real work, which is why planner-assist gains no capability the contract
    /// path does not already have.
    #[serde(default)]
    pub allowed_tables: Vec<String>,
    /// Evaluation mode. Off by default: calling a model surface costs money
    /// and produces output that must never be mistaken for a contract's, so it
    /// is opted into rather than out of.
    #[serde(default)]
    pub evaluation_enabled: bool,
}

impl PlannerSpec {
    pub fn validate(&self) -> Result<()> {
        if self.space_id.trim().is_empty() {
            return Err(Refusal::new(
                RefusalClass::Invalid,
                "asset_invalid",
                "a planner block needs a spaceId",
            ));
        }
        if self.trusted_assets.is_empty() && self.allowed_tables.is_empty() {
            return Err(Refusal::new(
                RefusalClass::Invalid,
                "asset_invalid",
                "a planner block must declare `trustedAssets` or `allowedTables`: \
                 planner-assist with neither is 'run whatever the model wrote', which is \
                 the one thing this system exists not to do",
            ));
        }
        Ok(())
    }

    /// Exact match. The table allowlist is deliberately NOT checked here —
    /// that is the SQL compiler's job, and duplicating it would create a
    /// second opinion about what a statement reads.
    pub fn permits_asset(&self, asset_id: &str) -> bool {
        self.trusted_assets.iter().any(|a| a == asset_id)
    }
}

/// What a planner interaction pins, and what it cannot.
///
/// Every field is something a planner API actually returns. There is no
/// permanently-empty `space_configuration_fingerprint`: a slot that is always
/// `None` reads as a gap someone will fill, when in fact no API exposes it.
/// [`pinned`](Self::pinned) says so once, plainly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannerPin {
    pub space_id: String,
    pub conversation_id: String,
    pub message_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachment_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statement_id: Option<String>,
    /// `sha256:<hex>` over the query text, LF-normalised. This is the part
    /// that IS reproducible: the same bytes re-executed at the same snapshot
    /// give the same rows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_hash: Option<String>,
    /// Whether the PLAN — the decision that produced this query — is pinned.
    ///
    /// False everywhere today, and a field rather than a constant: the day a
    /// vendor API returns a space's configuration fingerprint, this becomes
    /// true for spaces that expose one and the envelope's shape does not
    /// change.
    pub pinned: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trusted_asset_id: Option<String>,
}

impl PlannerPin {
    /// The refusal that accompanies an unpinned plan.
    ///
    /// `not_covered`, not an error: the result is real and sealed, and what is
    /// missing is a claim about REPRODUCIBILITY that nobody can make. A caller
    /// that requires a pinned plan checks this; one running an evaluation does
    /// not.
    pub fn unpinned_refusal(&self) -> Refusal {
        Refusal::new(
            RefusalClass::NotCovered,
            "genie_plan_unpinned",
            format!(
                "space '{}' does not expose a configuration fingerprint, so the PLAN that \
                 produced this query cannot be pinned. The result itself is sealed and \
                 replayable; the decision behind it is not.",
                self.space_id
            ),
        )
    }
}

/// A planner's answer, in adapter-neutral terms.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlannerMessage {
    pub conversation_id: String,
    pub message_id: String,
    pub attachment_id: Option<String>,
    pub statement_id: Option<String>,
    /// The query the planner proposes. `None` means it answered in prose —
    /// which is a real outcome to record and nothing to seal.
    pub proposed_sql: Option<String>,
    /// Set when the answer resolved to a *trusted asset* rather than to
    /// generated SQL. This is the field the allowlist checks.
    pub trusted_asset_id: Option<String>,
    pub prose: Option<String>,
}

impl PlannerMessage {
    pub fn pin(&self, space_id: &str) -> PlannerPin {
        PlannerPin {
            space_id: space_id.to_string(),
            conversation_id: self.conversation_id.clone(),
            message_id: self.message_id.clone(),
            attachment_id: self.attachment_id.clone(),
            statement_id: self.statement_id.clone(),
            query_hash: self.proposed_sql.as_deref().map(hash_query),
            // Never true today. See the field's own doc.
            pinned: false,
            trusted_asset_id: self.trusted_asset_id.clone(),
        }
    }
}

/// `sha256:<hex>` over LF-normalised query text.
///
/// The metric-view definition fingerprint, reused rather than reimplemented:
/// same normalisation, same prefix, one place to change. Two functions that
/// both promise a checkout on Windows hashes like one on Linux are two chances
/// to stop promising it.
pub fn hash_query(sql: &str) -> String {
    crate::semantic::fingerprint(sql)
}

/// A proposal that resolved outside the allowlist.
pub fn asset_not_allowed(asset_id: &str, spec: &PlannerSpec) -> Refusal {
    Refusal::new(
        RefusalClass::Denied,
        "genie_asset_not_allowed",
        format!(
            "the planner proposed trusted asset '{asset_id}', which space '{}' does not \
             permit. Permitted: {:?}. A proposal is a suggestion; the allowlist is the \
             decision.",
            spec.space_id, spec.trusted_assets
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> PlannerSpec {
        PlannerSpec {
            space_id: "01ef".into(),
            trusted_assets: vec!["asset-open-pipeline".into()],
            allowed_tables: vec!["opportunities".into()],
            evaluation_enabled: false,
        }
    }

    #[test]
    fn a_space_with_no_allowlist_at_all_is_refused_and_says_why() {
        let mut s = spec();
        s.trusted_assets.clear();
        s.allowed_tables.clear();
        let e = s.validate().unwrap_err();
        // The message has to carry the reasoning: the safe posture is the one
        // nobody configures unless the refusal explains itself.
        assert!(e.message.contains("run whatever the model wrote"));
    }

    #[test]
    fn the_allowlist_is_exact_and_never_a_prefix() {
        let s = spec();
        assert!(s.permits_asset("asset-open-pipeline"));
        assert!(!s.permits_asset("asset-open-pipeline-v2"));
        assert!(!s.permits_asset("asset-something-else"));
        let r = asset_not_allowed("asset-something-else", &s);
        assert_eq!(r.code, "genie_asset_not_allowed");
        assert_eq!(r.class, RefusalClass::Denied);
        assert!(r.message.contains("asset-open-pipeline"));
    }

    #[test]
    fn a_plan_is_unpinned_and_the_refusal_states_both_halves() {
        let msg = PlannerMessage {
            conversation_id: "c1".into(),
            message_id: "m1".into(),
            attachment_id: Some("a1".into()),
            statement_id: Some("s1".into()),
            proposed_sql: Some("SELECT 1".into()),
            trusted_asset_id: Some("asset-open-pipeline".into()),
            prose: None,
        };
        let pin = msg.pin("01ef");
        // The bytes ARE pinned; the plan is not.
        assert!(pin.query_hash.is_some());
        assert!(!pin.pinned);
        let r = pin.unpinned_refusal();
        assert_eq!(r.code, "genie_plan_unpinned");
        assert!(
            r.message.contains("sealed and \n             replayable")
                || r.message.contains("sealed and replayable")
        );
    }

    #[test]
    fn a_prose_only_message_pins_no_query() {
        let msg = PlannerMessage {
            conversation_id: "c1".into(),
            message_id: "m1".into(),
            prose: Some("I could not find a table for that.".into()),
            ..Default::default()
        };
        let pin = msg.pin("01ef");
        assert!(pin.query_hash.is_none());
        assert!(pin.statement_id.is_none());
    }

    #[test]
    fn the_query_hash_ignores_line_endings() {
        assert_eq!(
            hash_query("SELECT 1\r\nFROM t\r\n"),
            hash_query("SELECT 1\nFROM t\n")
        );
        assert_ne!(hash_query("SELECT 1"), hash_query("SELECT 2"));
    }
}
