// SPDX-License-Identifier: Apache-2.0
//! Ledger semantics: claim normalization and pin-aware supersession
//! resolution. The resolution function here is the REFERENCE implementation —
//! the in-memory backend calls it directly, and the Postgres backend's SQL
//! must agree with it (property-tested in the conformance suite).

use crate::types::{Claim, ClaimStatus, Seq};
use std::collections::HashSet;

/// Canonical text form `subject.key=value` — the shape all gates parse.
pub fn normalize_claim(subject: &str, key: &str, value: &str) -> String {
    format!("{}.{}={}", subject.trim(), key.trim(), value.trim())
}

/// Inverse of `normalize_claim`; tolerant of free text.
/// No `=` -> `("", "", text)`; no `.` in the head -> empty subject.
pub fn split_normalized(text: &str) -> (String, String, String) {
    let Some((head, value)) = text.split_once('=') else {
        return (String::new(), String::new(), text.trim().to_string());
    };
    let head = head.trim();
    match head.rsplit_once('.') {
        Some((subject, key)) => (
            subject.trim().to_string(),
            key.trim().to_string(),
            value.trim().to_string(),
        ),
        None => (String::new(), head.to_string(), value.trim().to_string()),
    }
}

/// Case- and whitespace-insensitive value comparison, as the gates use.
pub fn values_equivalent(a: &str, b: &str) -> bool {
    fold(a) == fold(b)
}

fn fold(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Query shape for `slice_facts`.
#[derive(Debug, Clone, Default)]
pub struct FactQuery {
    pub scope_prefix: Option<String>,
    pub as_of_seq: Option<Seq>,
    /// Empty = default `[Accepted]`.
    pub statuses: Vec<ClaimStatus>,
    /// Keeps the NEWEST n after resolution.
    pub limit: Option<usize>,
}

/// Pin-aware supersession resolution over a lineage's raw claims (any order).
///
/// The load-bearing rule: the superseded-set is itself filtered by
/// `seq <= as_of_seq`, so a claim superseded only AFTER the pin still reads
/// as current at the pin.
pub fn resolve_slice(mut claims: Vec<Claim>, q: &FactQuery) -> Vec<Claim> {
    let statuses: Vec<ClaimStatus> = if q.statuses.is_empty() {
        vec![ClaimStatus::Accepted]
    } else {
        q.statuses.clone()
    };

    // 1. pin
    if let Some(pin) = q.as_of_seq {
        claims.retain(|c| c.seq <= pin);
    }

    // 2. superseded-set from the PINNED view (any status may supersede)
    let superseded: HashSet<&str> = claims
        .iter()
        .filter_map(|c| c.supersedes_id.as_deref())
        .collect();

    // 3. visible = status-filtered, scope-filtered, not superseded at the pin
    let mut visible: Vec<Claim> = claims
        .iter()
        .filter(|c| statuses.contains(&c.status))
        .filter(|c| !superseded.contains(c.id.as_str()))
        .filter(|c| match (&q.scope_prefix, &c.scope_path) {
            (None, _) => true,
            (Some(prefix), Some(scope)) => {
                scope == prefix || scope.starts_with(&format!("{prefix}."))
            }
            (Some(_), None) => false,
        })
        .cloned()
        .collect();

    visible.sort_by_key(|c| c.seq);

    // 4. limit keeps the NEWEST n (order stays seq-ascending)
    if let Some(limit) = q.limit {
        if visible.len() > limit {
            visible.drain(..visible.len() - limit);
        }
    }
    visible
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ClaimType, Provenance};

    fn claim(
        id: &str,
        seq: Seq,
        subj: &str,
        key: &str,
        val: &str,
        supersedes: Option<&str>,
    ) -> Claim {
        Claim {
            id: id.into(),
            version_id: "v1".into(),
            seq,
            claim_type: if supersedes.is_some() {
                ClaimType::Correction
            } else {
                ClaimType::Fact
            },
            subject: subj.into(),
            key: key.into(),
            value: val.into(),
            scope_path: Some("ch1".into()),
            status: ClaimStatus::Accepted,
            provenance: Provenance::Witnessed,
            supersedes_id: supersedes.map(Into::into),
            entity_id: None,
            evidence: None,
            confidence: None,
            shape_ref: None,
            origin: None,
        }
    }

    #[test]
    fn normalize_roundtrip() {
        let n = normalize_claim("hero", "eye_color", "green");
        assert_eq!(n, "hero.eye_color=green");
        assert_eq!(
            split_normalized(&n),
            ("hero".into(), "eye_color".into(), "green".into())
        );
    }

    #[test]
    fn split_tolerates_free_text() {
        assert_eq!(
            split_normalized("no equals here"),
            ("".into(), "".into(), "no equals here".into())
        );
        assert_eq!(
            split_normalized("nodot=x"),
            ("".into(), "nodot".into(), "x".into())
        );
        // composite key splits at the LAST dot (a lesson paid for in measurement)
        assert_eq!(
            split_normalized("release.date.v4=2026"),
            ("release.date".into(), "v4".into(), "2026".into())
        );
    }

    #[test]
    fn supersession_resolves_at_head() {
        let claims = vec![
            claim("c1", 1, "hero", "eyes", "green", None),
            claim("c2", 2, "hero", "eyes", "blue", Some("c1")),
        ];
        let out = resolve_slice(claims, &FactQuery::default());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "c2");
    }

    /// THE pin semantic: superseded only after the pin => still current at the pin.
    #[test]
    fn supersession_respects_pin() {
        let claims = vec![
            claim("c1", 1, "hero", "eyes", "green", None),
            claim("c2", 5, "hero", "eyes", "blue", Some("c1")),
        ];
        let out = resolve_slice(
            claims,
            &FactQuery {
                as_of_seq: Some(3),
                ..Default::default()
            },
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "c1");
        assert_eq!(out[0].value, "green");
    }

    #[test]
    fn scope_prefix_matches_exact_and_descendants() {
        let mut a = claim("a", 1, "s", "k1", "v", None);
        a.scope_path = Some("book.ch1".into());
        let mut b = claim("b", 2, "s", "k2", "v", None);
        b.scope_path = Some("book.ch1.scene2".into());
        let mut c = claim("c", 3, "s", "k3", "v", None);
        c.scope_path = Some("book.ch10".into()); // NOT a descendant of book.ch1
        let out = resolve_slice(
            vec![a, b, c],
            &FactQuery {
                scope_prefix: Some("book.ch1".into()),
                ..Default::default()
            },
        );
        assert_eq!(
            out.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }

    #[test]
    fn limit_keeps_newest() {
        let claims = (1..=5)
            .map(|i| claim(&format!("c{i}"), i, "s", &format!("k{i}"), "v", None))
            .collect();
        let out = resolve_slice(
            claims,
            &FactQuery {
                limit: Some(2),
                ..Default::default()
            },
        );
        assert_eq!(out.iter().map(|x| x.seq).collect::<Vec<_>>(), vec![4, 5]);
    }

    #[test]
    fn disputed_hidden_by_default_visible_on_request() {
        let mut d = claim("d", 1, "s", "k", "v", None);
        d.status = ClaimStatus::Disputed;
        let out = resolve_slice(vec![d.clone()], &FactQuery::default());
        assert!(out.is_empty());
        let out = resolve_slice(
            vec![d],
            &FactQuery {
                statuses: vec![ClaimStatus::Disputed],
                ..Default::default()
            },
        );
        assert_eq!(out.len(), 1);
    }
}
