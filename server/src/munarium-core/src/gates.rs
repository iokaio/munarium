// SPDX-License-Identifier: Apache-2.0
//! The deterministic gates — governance below the application layer, ported
//! from the research implementation with its dotted rule ids intact.
//!
//! Five always-on gates run in `run_gates`. The sixth — chronology
//! (`check_chronology`, `gate.chronology-order|-deadline|-duration`) — is
//! declaratively armed: callers invoke it right after `run_gates` only when
//! the suite/tenant declares a chronology asset, and it fires only on CERTAIN
//! violations. Blocked claims are recorded `disputed`, never dropped; that
//! decision lives with the caller (`accept` paths), not here.

use crate::chrono_gate::{evaluate, parse_when, ChronoEvent, ChronologyRules};
use crate::ledger::values_equivalent;
use crate::similarity::similarity_ratio;
use crate::types::{Candidate, GateFinding, MeshSnapshot, Severity};
use serde_json::json;
use std::collections::HashSet;

pub const RULE_ANCHOR: &str = "gate.anchor-consistency";
pub const RULE_LEDGER: &str = "gate.ledger-conflict";
pub const RULE_ORPHAN: &str = "gate.orphaned-reference";
pub const RULE_META: &str = "gate.meta-leakage";
pub const RULE_SIMILARITY: &str = "gate.lexical-similarity";

pub const SIMILARITY_THRESHOLD: f64 = 0.9;

const META_MARKERS: &[&str] = &[
    "as an ai",
    "as a language model",
    "i cannot assist",
    "i'm sorry, but",
    "[insert",
    "lorem ipsum",
];

/// Gate 1 (block): a claim OR correction whose value differs from a locked
/// anchor for the same `subject.key`.
pub fn check_anchor_consistency(
    snapshot: &MeshSnapshot,
    candidate: &Candidate,
) -> Vec<GateFinding> {
    let mut findings = Vec::new();
    for proposed in candidate.claims.iter().chain(candidate.corrections.iter()) {
        let key = proposed.claim_key();
        if let Some(anchor) = snapshot.anchors.get(&key) {
            if !values_equivalent(&proposed.value, &anchor.locked_value) {
                findings.push(GateFinding {
                    rule_id: RULE_ANCHOR.into(),
                    severity: Severity::Block,
                    message: format!(
                        "claim '{key}={}' contradicts locked anchor value '{}'",
                        proposed.value, anchor.locked_value
                    ),
                    scope_path: candidate.scope_path.clone(),
                    detail: Some(json!({
                        "claim_key": key,
                        "proposed_value": proposed.value,
                        "locked_value": anchor.locked_value,
                        "anchor_id": anchor.id,
                    })),
                });
            }
        }
    }
    findings
}

/// Gate 2 (block): a plain claim contradicts the latest accepted value for its
/// claim_key. Declared supersessions (corrections, and any claim naming
/// `supersedes_id`) are exempt — they are how values legitimately change.
pub fn check_ledger_conflict(snapshot: &MeshSnapshot, candidate: &Candidate) -> Vec<GateFinding> {
    let mut findings = Vec::new();
    for proposed in &candidate.claims {
        if proposed.supersedes_id.is_some() {
            continue;
        }
        let key = proposed.claim_key();
        // facts are seq-ascending resolved-current; last one wins
        if let Some(existing) = snapshot.facts.iter().rev().find(|f| f.claim_key() == key) {
            if !values_equivalent(&proposed.value, &existing.value) {
                findings.push(GateFinding {
                    rule_id: RULE_LEDGER.into(),
                    severity: Severity::Block,
                    message: format!(
                        "claim '{key}={}' conflicts with accepted canon '{key}={}' (use a correction to supersede)",
                        proposed.value, existing.value
                    ),
                    scope_path: candidate.scope_path.clone(),
                    detail: Some(json!({
                        "claim_key": key,
                        "proposed_value": proposed.value,
                        "canon_value": existing.value,
                        "canon_claim_id": existing.id,
                        "canon_seq": existing.seq,
                    })),
                });
            }
        }
    }
    findings
}

/// Gate 3 (warn): a correction targets a subject / subject.key that canon
/// never established.
pub fn check_orphaned_reference(
    snapshot: &MeshSnapshot,
    candidate: &Candidate,
) -> Vec<GateFinding> {
    let known_keys: HashSet<String> = snapshot.facts.iter().map(|f| f.claim_key()).collect();
    let known_subjects: HashSet<&str> = snapshot.facts.iter().map(|f| f.subject.as_str()).collect();

    let mut findings = Vec::new();
    for correction in &candidate.corrections {
        let key = correction.claim_key();
        let orphaned = if known_subjects.contains(correction.subject.as_str()) {
            !known_keys.contains(&key)
        } else {
            true
        };
        if orphaned {
            findings.push(GateFinding {
                rule_id: RULE_ORPHAN.into(),
                severity: Severity::Warn,
                message: format!("correction targets '{key}' but no such canon was established"),
                scope_path: candidate.scope_path.clone(),
                detail: Some(json!({ "claim_key": key })),
            });
        }
    }
    findings
}

/// Gate 4 (warn): boilerplate / AI-assistant markers in the output text.
pub fn check_meta_leakage(_snapshot: &MeshSnapshot, candidate: &Candidate) -> Vec<GateFinding> {
    let lowered = candidate.text.to_lowercase();
    META_MARKERS
        .iter()
        .filter(|marker| lowered.contains(**marker))
        .map(|marker| GateFinding {
            rule_id: RULE_META.into(),
            severity: Severity::Warn,
            message: format!("output contains meta-leakage marker '{marker}'"),
            scope_path: candidate.scope_path.clone(),
            detail: Some(json!({ "marker": marker })),
        })
        .collect()
}

/// Gate 5 (warn): near-duplicate of a previous unit's text (repetition).
pub fn check_lexical_similarity(
    _snapshot: &MeshSnapshot,
    candidate: &Candidate,
) -> Vec<GateFinding> {
    if candidate.text.trim().is_empty() {
        return Vec::new();
    }
    let mut findings = Vec::new();
    for (idx, prev) in candidate.previous_texts.iter().enumerate() {
        let ratio = similarity_ratio(&candidate.text, prev);
        if ratio >= SIMILARITY_THRESHOLD {
            findings.push(GateFinding {
                rule_id: RULE_SIMILARITY.into(),
                severity: Severity::Warn,
                message: format!(
                    "output is {:.0}% similar to previous unit {idx} (threshold {:.0}%)",
                    ratio * 100.0,
                    SIMILARITY_THRESHOLD * 100.0
                ),
                scope_path: candidate.scope_path.clone(),
                detail: Some(json!({ "previous_index": idx, "ratio": ratio })),
            });
        }
    }
    findings
}

/// Gate 6 (declaratively armed): calendar and ordering governance.
///
/// Builds the effective timeline — the snapshot's accepted facts (seq order,
/// later wins) overlaid with the candidate's claims and corrections, so a
/// correction that fixes a date clears the violation in the same evaluation —
/// then evaluates the declarative rules. A claim joins the timeline when its
/// key matches the rules' `temporal_keys` patterns or an absolute rule target
/// names it, and its value parses as a `When` (unparseable values drop the
/// key — never guessed). `now` is the deadline-absence clock (latest unit
/// as_of); None disables absence checks. Severity is warn unless the rule
/// declares block; findings carry the complete event chain.
pub fn check_chronology(
    snapshot: &MeshSnapshot,
    candidate: &Candidate,
    rules: &ChronologyRules,
    now: Option<chrono::NaiveDate>,
) -> Vec<GateFinding> {
    let absolute = rules.absolute_targets();
    let mut timeline: std::collections::BTreeMap<String, ChronoEvent> =
        std::collections::BTreeMap::new();

    let consider = |timeline: &mut std::collections::BTreeMap<String, ChronoEvent>,
                    subject: &str,
                    key: &str,
                    value: &str,
                    claim_id: Option<String>,
                    origin: &'static str| {
        let claim_key = format!("{subject}.{key}");
        if !(rules.is_temporal_key(key) || absolute.contains(&claim_key.to_lowercase())) {
            return;
        }
        match parse_when(value) {
            Some(when) => {
                timeline.insert(
                    claim_key,
                    ChronoEvent {
                        subject: subject.to_string(),
                        key: key.to_string(),
                        value: value.to_string(),
                        when,
                        claim_id,
                        origin,
                    },
                );
            }
            // Latest assertion unparseable: drop the key rather than
            // judging a stale value.
            None => {
                timeline.remove(&claim_key);
            }
        }
    };

    for fact in &snapshot.facts {
        // ordered by seq, so later facts win
        if !fact.subject.is_empty() || !fact.key.is_empty() {
            consider(
                &mut timeline,
                &fact.subject,
                &fact.key,
                &fact.value,
                Some(fact.id.clone()),
                "ledger",
            );
        }
    }
    for claim in candidate.claims.iter().chain(candidate.corrections.iter()) {
        consider(
            &mut timeline,
            &claim.subject,
            &claim.key,
            &claim.value,
            None,
            "candidate",
        );
    }
    if timeline.is_empty() {
        return Vec::new();
    }

    let events: Vec<ChronoEvent> = timeline.into_values().collect();
    evaluate(&events, rules, now)
        .into_iter()
        .map(|v| {
            let rule_id = match v.kind {
                "deadline" => "gate.chronology-deadline",
                "duration" => "gate.chronology-duration",
                _ => "gate.chronology-order", // order | contains | overlap
            };
            let severity = if v.severity == "block" {
                Severity::Block
            } else {
                Severity::Warn
            };
            let mut detail = serde_json::Map::new();
            detail.insert("kind".into(), v.kind.into());
            detail.insert("rule".into(), v.rule.clone());
            detail.insert("chain".into(), serde_json::Value::Array(v.chain.clone()));
            for (k, val) in &v.extras {
                detail.insert(k.clone(), val.clone());
            }
            // The newest candidate-side contributor is the claim under
            // review — naming it lets a block finding dispute it.
            if let Some(last) = v.candidate_claims.last() {
                detail.insert("claim_key".into(), last.clone().into());
            }
            GateFinding {
                rule_id: rule_id.into(),
                severity,
                message: v.message,
                scope_path: candidate.scope_path.clone(),
                detail: Some(serde_json::Value::Object(detail)),
            }
        })
        .collect()
}

/// Run gates 1-5 and dedup: a ledger-conflict finding for a claim_key that
/// already drew an anchor-consistency finding is dropped (the anchor finding
/// subsumes it).
pub fn run_gates(snapshot: &MeshSnapshot, candidate: &Candidate) -> Vec<GateFinding> {
    let mut findings = check_anchor_consistency(snapshot, candidate);

    let anchored_keys: HashSet<String> = findings
        .iter()
        .filter_map(|f| f.detail.as_ref())
        .filter_map(|d| d.get("claim_key"))
        .filter_map(|k| k.as_str().map(String::from))
        .collect();

    findings.extend(
        check_ledger_conflict(snapshot, candidate)
            .into_iter()
            .filter(|f| {
                f.detail
                    .as_ref()
                    .and_then(|d| d.get("claim_key"))
                    .and_then(|k| k.as_str())
                    .map(|k| !anchored_keys.contains(k))
                    .unwrap_or(true)
            }),
    );

    findings.extend(check_orphaned_reference(snapshot, candidate));
    findings.extend(check_meta_leakage(snapshot, candidate));
    findings.extend(check_lexical_similarity(snapshot, candidate));
    findings
}

/// Backfill mode: contradiction blocks downgrade to warns (conflicts in an
/// existing corpus are surfaced, never blocking). Mirrors `mesh/recon.py`.
pub fn downgrade_blocks(findings: &mut [GateFinding]) {
    for f in findings.iter_mut() {
        if f.severity == Severity::Block {
            f.severity = Severity::Warn;
        }
    }
}

/// Which claim_keys in this finding set carry a BLOCK — the accept path
/// records those claims `disputed`.
pub fn blocked_claim_keys(findings: &[GateFinding]) -> HashSet<String> {
    findings
        .iter()
        .filter(|f| f.severity == Severity::Block)
        .filter_map(|f| f.detail.as_ref())
        .filter_map(|d| d.get("claim_key"))
        .filter_map(|k| k.as_str().map(String::from))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;

    fn fact(id: &str, seq: Seq, subj: &str, key: &str, val: &str) -> Claim {
        Claim {
            id: id.into(),
            version_id: "v1".into(),
            seq,
            claim_type: ClaimType::Fact,
            subject: subj.into(),
            key: key.into(),
            value: val.into(),
            scope_path: Some("ch1".into()),
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

    fn proposed(claim_type: ClaimType, subj: &str, key: &str, val: &str) -> ProposedClaim {
        ProposedClaim {
            claim_type,
            subject: subj.into(),
            key: key.into(),
            value: val.into(),
            supersedes_id: None,
        }
    }

    fn snapshot_with(facts: Vec<Claim>, anchors: Vec<Anchor>) -> MeshSnapshot {
        MeshSnapshot {
            version_id: "v1".into(),
            facts,
            anchors: anchors
                .into_iter()
                .map(|a| (a.detail_key.clone(), a))
                .collect(),
            ..Default::default()
        }
    }

    fn anchor(subj_key: &str, value: &str) -> Anchor {
        Anchor {
            id: format!("anchor-{subj_key}"),
            version_id: "v1".into(),
            detail_key: subj_key.into(),
            locked_value: value.into(),
            locked_at_scope: None,
            status: AnchorStatus::Locked,
            seq: 1,
            evidence: None,
        }
    }

    #[test]
    fn anchor_gate_blocks_drift_case_insensitively() {
        let snap = snapshot_with(vec![], vec![anchor("hero.eyes", "Green")]);
        let cand = Candidate {
            claims: vec![proposed(ClaimType::Fact, "hero", "eyes", "blue")],
            ..Default::default()
        };
        let f = check_anchor_consistency(&snap, &cand);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, Severity::Block);

        // equivalent value (case/space) passes
        let cand_ok = Candidate {
            claims: vec![proposed(ClaimType::Fact, "hero", "eyes", "  green ")],
            ..Default::default()
        };
        assert!(check_anchor_consistency(&snap, &cand_ok).is_empty());
    }

    #[test]
    fn ledger_gate_blocks_plain_conflict_exempts_supersession() {
        let snap = snapshot_with(vec![fact("c1", 1, "hero", "eyes", "green")], vec![]);
        let cand = Candidate {
            claims: vec![proposed(ClaimType::Fact, "hero", "eyes", "blue")],
            ..Default::default()
        };
        assert_eq!(check_ledger_conflict(&snap, &cand).len(), 1);

        let mut superseding = proposed(ClaimType::Correction, "hero", "eyes", "blue");
        superseding.supersedes_id = Some("c1".into());
        let cand2 = Candidate {
            claims: vec![superseding],
            ..Default::default()
        };
        assert!(check_ledger_conflict(&snap, &cand2).is_empty());
    }

    #[test]
    fn orphan_gate_warns_on_unestablished_target() {
        let snap = snapshot_with(vec![fact("c1", 1, "hero", "eyes", "green")], vec![]);
        let cand = Candidate {
            corrections: vec![
                proposed(ClaimType::Correction, "villain", "name", "Mora"), // unknown subject
                proposed(ClaimType::Correction, "hero", "height", "tall"), // known subject, unknown key
                proposed(ClaimType::Correction, "hero", "eyes", "blue"),   // established — fine
            ],
            ..Default::default()
        };
        let f = check_orphaned_reference(&snap, &cand);
        assert_eq!(f.len(), 2);
        assert!(f.iter().all(|x| x.severity == Severity::Warn));
    }

    #[test]
    fn meta_gate_catches_markers() {
        let cand = Candidate {
            text: "As an AI, I cannot assist with that. Lorem ipsum.".into(),
            ..Default::default()
        };
        let f = check_meta_leakage(&MeshSnapshot::default(), &cand);
        assert_eq!(f.len(), 3);
    }

    #[test]
    fn similarity_gate_warns_at_ninety_percent() {
        let text = "The chapter opens with rain on the harbor and the bell ringing twice.";
        let cand = Candidate {
            text: text.replace("twice", "once"),
            previous_texts: vec![text.into()],
            ..Default::default()
        };
        assert_eq!(
            check_lexical_similarity(&MeshSnapshot::default(), &cand).len(),
            1
        );
    }

    /// The run_gates dedup: anchor finding suppresses the twin ledger-conflict.
    #[test]
    fn run_gates_dedups_anchor_and_ledger_twin() {
        let snap = snapshot_with(
            vec![fact("c1", 1, "hero", "eyes", "green")],
            vec![anchor("hero.eyes", "green")],
        );
        let cand = Candidate {
            claims: vec![proposed(ClaimType::Fact, "hero", "eyes", "blue")],
            ..Default::default()
        };
        let findings = run_gates(&snap, &cand);
        assert_eq!(
            findings.len(),
            1,
            "twin ledger-conflict must be dropped: {findings:?}"
        );
        assert_eq!(findings[0].rule_id, RULE_ANCHOR);
    }

    #[test]
    fn chronology_gate_full_path() {
        use crate::chrono_gate::{ChronologyRules, OrderRule};
        let rules = ChronologyRules {
            order: vec![OrderRule {
                before: "birth_date".into(),
                after: "death_date".into(),
                severity: "warn".into(),
            }],
            ..Default::default()
        };
        let snap = snapshot_with(vec![fact("c1", 1, "hero", "birth_date", "1950")], vec![]);

        // candidate asserts a death BEFORE the ledger birth: fires
        let cand = Candidate {
            scope_path: Some("ch2".into()),
            claims: vec![proposed(ClaimType::Fact, "hero", "death_date", "1940")],
            ..Default::default()
        };
        let f = check_chronology(&snap, &cand, &rules, None);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule_id, "gate.chronology-order");
        assert_eq!(f[0].severity, Severity::Warn);
        let detail = f[0].detail.as_ref().unwrap();
        assert_eq!(detail["claim_key"], "hero.death_date");
        assert_eq!(detail["chain"].as_array().unwrap().len(), 2);

        // an uncertain candidate date files nothing
        let cand = Candidate {
            claims: vec![proposed(
                ClaimType::Fact,
                "hero",
                "death_date",
                "circa 1940",
            )],
            ..Default::default()
        };
        assert!(check_chronology(&snap, &cand, &rules, None).is_empty());

        // a correction overlays the ledger fact in the SAME evaluation:
        // fixing the birth date clears the violation
        let mut fix = proposed(ClaimType::Correction, "hero", "birth_date", "1930");
        fix.supersedes_id = Some("c1".into());
        let cand = Candidate {
            claims: vec![proposed(ClaimType::Fact, "hero", "death_date", "1940")],
            corrections: vec![fix],
            ..Default::default()
        };
        assert!(check_chronology(&snap, &cand, &rules, None).is_empty());

        // unparseable latest value drops the key — never guessed
        let cand = Candidate {
            claims: vec![proposed(
                ClaimType::Fact,
                "hero",
                "death_date",
                "sometime later",
            )],
            ..Default::default()
        };
        assert!(check_chronology(&snap, &cand, &rules, None).is_empty());

        // a rule declaring block escalates severity
        let block_rules = ChronologyRules {
            order: vec![OrderRule {
                before: "birth_date".into(),
                after: "death_date".into(),
                severity: "block".into(),
            }],
            ..Default::default()
        };
        let cand = Candidate {
            claims: vec![proposed(ClaimType::Fact, "hero", "death_date", "1940")],
            ..Default::default()
        };
        let f = check_chronology(&snap, &cand, &block_rules, None);
        assert_eq!(f[0].severity, Severity::Block);
        // and blocked_claim_keys can dispute it via the claim_key detail
        assert!(blocked_claim_keys(&f).contains("hero.death_date"));
    }

    #[test]
    fn backfill_downgrades_blocks() {
        let snap = snapshot_with(vec![fact("c1", 1, "hero", "eyes", "green")], vec![]);
        let cand = Candidate {
            claims: vec![proposed(ClaimType::Fact, "hero", "eyes", "blue")],
            ..Default::default()
        };
        let mut findings = run_gates(&snap, &cand);
        downgrade_blocks(&mut findings);
        assert!(findings.iter().all(|f| f.severity != Severity::Block));
    }
}
