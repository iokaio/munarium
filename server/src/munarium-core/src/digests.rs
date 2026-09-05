// SPDX-License-Identifier: Apache-2.0
//! The digest ladder — deterministic, model-free compression rungs over the
//! fact ledger. tier 0 = per scope, tier 1 = per scope-prefix group, tier 2 =
//! whole-lineage rollup. Under an `as_of_seq` pin, rungs are REBUILT from the
//! pinned facts by these functions — stored digest text is never served under
//! a pin (digest text is upserted without history).

use crate::types::{Claim, Digest, Seq};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;

pub const TIER_LABELS: [&str; 3] = ["scope", "group", "rollup"];

fn hash(content: &str) -> String {
    hex::encode(Sha256::digest(content.as_bytes()))
}

/// The scope-prefix group a scope belongs to: its first dotted segment.
pub fn group_of(scope: &str) -> String {
    scope.split('.').next().unwrap_or(scope).to_string()
}

/// Tier-0: one rung per scope — the scope's current facts as canonical lines.
pub fn build_tier0(version_id: &str, facts: &[Claim]) -> Vec<Digest> {
    let mut by_scope: BTreeMap<String, Vec<&Claim>> = BTreeMap::new();
    for f in facts {
        by_scope
            .entry(f.scope_path.clone().unwrap_or_default())
            .or_default()
            .push(f);
    }
    by_scope
        .into_iter()
        .map(|(scope, claims)| {
            let built_from_seq = claims.iter().map(|c| c.seq).max().unwrap_or(0);
            let lines: Vec<String> = claims.iter().map(|c| c.normalized_text()).collect();
            let content = format!("[{scope}] {} facts\n{}", lines.len(), lines.join("\n"));
            Digest {
                version_id: version_id.to_string(),
                tier: 0,
                scope_path: scope,
                content_hash: hash(&content),
                content,
                built_from_seq,
            }
        })
        .collect()
}

/// Tier-1: one rung per scope-prefix group — keys only, values elided.
pub fn build_tier1(version_id: &str, facts: &[Claim]) -> Vec<Digest> {
    let mut by_group: BTreeMap<String, Vec<&Claim>> = BTreeMap::new();
    for f in facts {
        let scope = f.scope_path.clone().unwrap_or_default();
        by_group.entry(group_of(&scope)).or_default().push(f);
    }
    by_group
        .into_iter()
        .map(|(group, claims)| {
            let built_from_seq = claims.iter().map(|c| c.seq).max().unwrap_or(0);
            let mut keys: Vec<String> = claims.iter().map(|c| c.claim_key()).collect();
            keys.sort();
            keys.dedup();
            let scopes: std::collections::BTreeSet<_> =
                claims.iter().filter_map(|c| c.scope_path.clone()).collect();
            let content = format!(
                "[group {group}] {} facts across {} scopes; keys: {}",
                claims.len(),
                scopes.len(),
                keys.join(", ")
            );
            Digest {
                version_id: version_id.to_string(),
                tier: 1,
                scope_path: group,
                content_hash: hash(&content),
                content,
                built_from_seq,
            }
        })
        .collect()
}

/// Tier-2: the whole-lineage rollup rung.
pub fn build_tier2(version_id: &str, facts: &[Claim]) -> Digest {
    let built_from_seq: Seq = facts.iter().map(|c| c.seq).max().unwrap_or(0);
    let mut subjects: Vec<&str> = facts.iter().map(|c| c.subject.as_str()).collect();
    subjects.sort();
    subjects.dedup();
    let scopes: std::collections::BTreeSet<_> = facts
        .iter()
        .filter_map(|c| c.scope_path.as_deref())
        .collect();
    let content = format!(
        "[rollup] {} facts, {} scopes, {} subjects: {}",
        facts.len(),
        scopes.len(),
        subjects.len(),
        subjects.join(", ")
    );
    Digest {
        version_id: version_id.to_string(),
        tier: 2,
        scope_path: String::new(),
        content_hash: hash(&content),
        content,
        built_from_seq,
    }
}

/// The full ladder, rebuilt deterministically from (already pin-resolved) facts.
pub fn build_ladder(version_id: &str, facts: &[Claim]) -> Vec<Digest> {
    let mut out = build_tier0(version_id, facts);
    out.extend(build_tier1(version_id, facts));
    out.push(build_tier2(version_id, facts));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;

    fn fact(id: &str, seq: Seq, scope: &str, subj: &str, key: &str, val: &str) -> Claim {
        Claim {
            id: id.into(),
            version_id: "v1".into(),
            seq,
            claim_type: ClaimType::Fact,
            subject: subj.into(),
            key: key.into(),
            value: val.into(),
            scope_path: Some(scope.into()),
            status: ClaimStatus::Accepted,
            provenance: Provenance::Witnessed,
            supersedes_id: None,
            entity_id: None,
            evidence: None,
            confidence: None,
            shape_ref: None,
            origin: None,
        }
    }

    #[test]
    fn ladder_is_deterministic() {
        let facts = vec![
            fact("a", 1, "book.ch1", "hero", "eyes", "green"),
            fact("b", 2, "book.ch2", "hero", "home", "harbor"),
            fact("c", 3, "notes", "villain", "name", "Mora"),
        ];
        let l1 = build_ladder("v1", &facts);
        let l2 = build_ladder("v1", &facts);
        assert_eq!(l1, l2);
        // tiers: 3 scopes + 2 groups (book, notes) + 1 rollup
        assert_eq!(l1.iter().filter(|d| d.tier == 0).count(), 3);
        assert_eq!(l1.iter().filter(|d| d.tier == 1).count(), 2);
        assert_eq!(l1.iter().filter(|d| d.tier == 2).count(), 1);
    }

    #[test]
    fn rebuild_under_pin_differs_from_head() {
        let facts_head = vec![
            fact("a", 1, "ch1", "hero", "eyes", "green"),
            fact("b", 5, "ch1", "hero", "home", "harbor"),
        ];
        let facts_pinned: Vec<Claim> = facts_head.iter().filter(|f| f.seq <= 3).cloned().collect();
        let head = build_tier0("v1", &facts_head);
        let pinned = build_tier0("v1", &facts_pinned);
        assert_ne!(head[0].content_hash, pinned[0].content_hash);
        assert!(!pinned[0].content.contains("home"));
    }
}
