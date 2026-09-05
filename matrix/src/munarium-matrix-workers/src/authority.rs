// SPDX-License-Identifier: Apache-2.0
//! Scoped authority.
//!
//! A mapping in authoritative mode does not get to write the ledger wholesale.
//! It gets to write **inside a declared scope**: per property, per valid-time
//! interval, under a declared precedence, and anything outside a scope stays
//! shadow — findings only, canon untouched.
//!
//! The precedence default is `document_over_source` on purpose. A document the
//! customer signed outranks a row in a system of record until an operator
//! says otherwise for a specific property. That is the conservative reading,
//! and it is the one that cannot rewrite history by accident.

use chrono::{DateTime, NaiveDate, Utc};
use munarium_matrix_types::assets::{AuthorityScope, ClaimMappingDoc, MappingMode, Precedence};

/// Why a proposal was, or was not, made. Every branch is a reason a reviewer
/// can read, not a boolean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorityDecision {
    /// The mapping is shadow (or not promoted): propose nothing.
    Shadow,
    /// No declared scope covers this property at this valid time.
    OutOfScope,
    /// A document-derived claim already holds this key and the scope's
    /// precedence keeps the document on top: file the finding, propose nothing.
    DocumentOutranks,
    /// A backdated change never bypasses review, whatever the scope says.
    RequiresReview,
    /// Inside scope, precedence allows it: propose.
    Propose,
}

fn parse_when(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.with_timezone(&Utc));
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|n| n.and_utc())
}

/// The scope covering `property` at `valid_at`, if any.
///
/// `valid_from` is inclusive and `valid_to` exclusive, the half-open interval
/// every other date range in the plane uses. An observation with no valid time
/// matches only a scope with no bounds — an unbounded claim cannot be placed
/// inside a bounded interval, so it does not get to borrow one.
pub fn scope_for<'a>(
    mapping: &'a ClaimMappingDoc,
    property: &str,
    valid_at: Option<DateTime<Utc>>,
) -> Option<&'a AuthorityScope> {
    mapping.spec.authority.iter().find(|s| {
        if s.property != property {
            return false;
        }
        let from = s.valid_from.as_deref().and_then(parse_when);
        let to = s.valid_to.as_deref().and_then(parse_when);
        match valid_at {
            None => from.is_none() && to.is_none(),
            Some(t) => from.is_none_or(|f| t >= f) && to.is_none_or(|u| t < u),
        }
    })
}

/// Everything the decision needs, gathered by the caller.
#[derive(Debug, Clone, Copy)]
pub struct AuthorityContext<'a> {
    pub mapping: &'a ClaimMappingDoc,
    /// The operator promoted this mapping (a recorded decision).
    pub promoted: bool,
    pub property: &'a str,
    pub valid_at: Option<DateTime<Utc>>,
    /// The ledger already holds a claim for this key.
    pub ledger_has_claim: bool,
    /// That claim carries a connector origin — i.e. it is OURS, not a
    /// document's, so document precedence does not protect it.
    pub ledger_claim_is_connector: bool,
    pub backdated: bool,
}

/// The mapping's declared business rule for this property and change kind.
pub fn classify_change_kind(
    mapping: &ClaimMappingDoc,
    property: &str,
    change_kind: munarium_matrix_types::contract::ChangeKind,
) -> munarium_matrix_types::assets::ChangeKindDecision {
    crate::reconcile::classify_change(mapping, property, change_kind)
}

pub fn decide(ctx: &AuthorityContext<'_>) -> AuthorityDecision {
    if ctx.mapping.spec.mode != MappingMode::Authoritative || !ctx.promoted {
        return AuthorityDecision::Shadow;
    }
    // Scope is the FIRST gate, before any per-change reasoning: a property no
    // scope covers is shadow whatever its change rule says, and reporting it
    // as "requires review" would imply an operator could review it into
    // canon — they cannot, not without declaring the scope.
    let Some(scope) = scope_for(ctx.mapping, ctx.property, ctx.valid_at) else {
        return AuthorityDecision::OutOfScope;
    };
    if ctx.backdated {
        return AuthorityDecision::RequiresReview;
    }
    if ctx.ledger_has_claim
        && !ctx.ledger_claim_is_connector
        && scope.precedence == Precedence::DocumentOverSource
    {
        return AuthorityDecision::DocumentOutranks;
    }
    AuthorityDecision::Propose
}

#[cfg(test)]
mod tests {
    use super::*;
    use munarium_matrix_types::parse_asset;

    fn mapping(authority_yaml: &str) -> ClaimMappingDoc {
        let yaml = format!(
            "apiVersion: munarium.ioka.io/v1\nkind: ClaimMapping\n\
             metadata: {{ name: holdings, version: 2 }}\nspec:\n  source: crm\n  mode: authoritative\n\
             \x20 entity: {{ table: holdings, key: [holder_id], subjectTemplate: \"shareholder.{{holder_id}}\" }}\n\
             \x20 properties:\n    shares: {{ column: shares, type: decimal, scale: 0 }}\n\
             \x20   share_class: {{ column: share_class, type: string }}\n\
             \x20 temporal: {{ validTime: {{ column: effective_date }} }}\n{authority_yaml}"
        );
        match parse_asset(&yaml).expect("fixture parses") {
            munarium_matrix_types::Asset::ClaimMapping(m) => *m,
            _ => unreachable!(),
        }
    }

    fn at(s: &str) -> Option<DateTime<Utc>> {
        parse_when(s)
    }

    fn ctx<'a>(m: &'a ClaimMappingDoc, property: &'a str) -> AuthorityContext<'a> {
        AuthorityContext {
            mapping: m,
            promoted: true,
            property,
            valid_at: at("2026-04-01"),
            ledger_has_claim: false,
            ledger_claim_is_connector: false,
            backdated: false,
        }
    }

    #[test]
    fn an_unpromoted_authoritative_mapping_is_shadow() {
        let m =
            mapping("  authority:\n    - { property: shares, precedence: source_over_document }\n");
        let mut c = ctx(&m, "shares");
        c.promoted = false;
        assert_eq!(
            decide(&c),
            AuthorityDecision::Shadow,
            "mode alone must never write"
        );
    }

    #[test]
    fn outside_every_scope_is_shadow_by_property_and_by_time() {
        let m = mapping(
            "  authority:\n    - { property: shares, validFrom: \"2026-01-01\", validTo: \"2026-07-01\", precedence: source_over_document }\n",
        );
        assert_eq!(
            decide(&ctx(&m, "share_class")),
            AuthorityDecision::OutOfScope
        );
        let mut late = ctx(&m, "shares");
        late.valid_at = at("2026-07-01"); // the exclusive bound
        assert_eq!(decide(&late), AuthorityDecision::OutOfScope);
        let mut inside = ctx(&m, "shares");
        inside.valid_at = at("2026-06-30");
        assert_eq!(decide(&inside), AuthorityDecision::Propose);
    }

    #[test]
    fn a_document_claim_outranks_the_source_by_default() {
        let m = mapping("  authority:\n    - { property: shares }\n");
        let mut c = ctx(&m, "shares");
        c.ledger_has_claim = true;
        assert_eq!(
            decide(&c),
            AuthorityDecision::DocumentOutranks,
            "the default precedence keeps a signed document on top"
        );
        // ...unless the existing claim is OUR OWN earlier proposal.
        c.ledger_claim_is_connector = true;
        assert_eq!(decide(&c), AuthorityDecision::Propose);
        // ...or the operator declared the source authoritative.
        let m2 =
            mapping("  authority:\n    - { property: shares, precedence: source_over_document }\n");
        let mut c2 = ctx(&m2, "shares");
        c2.ledger_has_claim = true;
        assert_eq!(decide(&c2), AuthorityDecision::Propose);
    }

    #[test]
    fn backdated_requires_review_whatever_the_scope_says() {
        let m =
            mapping("  authority:\n    - { property: shares, precedence: source_over_document }\n");
        let mut c = ctx(&m, "shares");
        c.backdated = true;
        assert_eq!(decide(&c), AuthorityDecision::RequiresReview);
    }

    #[test]
    fn an_observation_without_a_valid_time_matches_only_an_unbounded_scope() {
        let bounded =
            mapping("  authority:\n    - { property: shares, validFrom: \"2026-01-01\" }\n");
        let mut c = ctx(&bounded, "shares");
        c.valid_at = None;
        assert_eq!(decide(&c), AuthorityDecision::OutOfScope);
        let unbounded = mapping("  authority:\n    - { property: shares }\n");
        let mut c2 = ctx(&unbounded, "shares");
        c2.valid_at = None;
        assert_eq!(decide(&c2), AuthorityDecision::Propose);
    }
}
