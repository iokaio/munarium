// SPDX-License-Identifier: Apache-2.0
//! The pure Context Composer. Section order (carried over verbatim):
//! `Point-in-time pin` (only when pinned) -> `Canon digest` -> `Accepted facts`
//! -> `Locked details` -> `Open promises` -> `Frequency budgets`.
//!
//! Under a token budget it degrades digest resolution tier by tier — the
//! active scope's group last — BEFORE oldest-first fact trimming. Zero I/O:
//! `compose` is a pure function of the snapshot.

use crate::counters::directives;
use crate::digests::group_of;
use crate::types::{MeshSnapshot, PromiseStatus};
use sha2::{Digest as _, Sha256};

#[derive(Debug, Clone, PartialEq)]
pub struct ComposedContext {
    pub sections: Vec<(String, String)>,
}

impl ComposedContext {
    pub fn text(&self) -> String {
        self.sections
            .iter()
            .map(|(title, body)| format!("## {title}\n{body}"))
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// The cheap token estimate: len // 4.
    pub fn estimated_tokens(&self) -> u64 {
        (self.text().len() / 4) as u64
    }

    /// Feeds invocation caching: same snapshot view => same hash.
    pub fn content_hash(&self) -> String {
        hex::encode(Sha256::digest(self.text().as_bytes()))
    }
}

/// Digest degradation levels, coarser as the number rises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DigestLevel {
    AllTier0,
    ActiveGroupTier0RestTier1,
    AllTier1,
    RollupOnly,
}

const LEVELS: [DigestLevel; 4] = [
    DigestLevel::AllTier0,
    DigestLevel::ActiveGroupTier0RestTier1,
    DigestLevel::AllTier1,
    DigestLevel::RollupOnly,
];

pub fn compose(
    snapshot: &MeshSnapshot,
    scope: Option<&str>,
    budget_tokens: Option<u64>,
) -> ComposedContext {
    // Try each digest level in order; within a level, trim facts oldest-first
    // only at the LAST level (facts are trimmed only after digests are as
    // coarse as they can get).
    for (i, level) in LEVELS.iter().enumerate() {
        let last = i == LEVELS.len() - 1;
        let ctx = compose_at(snapshot, scope, *level, None);
        match budget_tokens {
            None => return ctx,
            Some(budget) if ctx.estimated_tokens() <= budget => return ctx,
            Some(budget) if last => {
                // Oldest-first fact trimming as the final resort: find the
                // LARGEST `keep` (newest facts kept) whose rendering fits.
                // The estimate is monotone in `keep` — more facts, more text
                // — so this is a binary search over [0, len), not a descent
                // one fact at a time: that descent re-rendered the whole
                // context per step, O(n²) in the lineage, and `budget_tokens`
                // arrives straight from an authenticated REST query
                // parameter.
                let len = snapshot.facts.len();
                // Invariant: every keep in [0, lo) fits or is 0; hi never fits
                // (len is known not to fit — that is how we got here).
                let (mut lo, mut hi) = (0usize, len);
                while lo < hi {
                    let mid = lo + (hi - lo) / 2;
                    if compose_at(snapshot, scope, *level, Some(mid)).estimated_tokens() <= budget {
                        lo = mid + 1;
                    } else {
                        hi = mid;
                    }
                }
                // `lo` is one past the largest fitting keep, or 0 when none
                // fits — in which case the caller gets the minimum, as before.
                return compose_at(snapshot, scope, *level, Some(lo.saturating_sub(1)));
            }
            Some(_) => continue,
        }
    }
    unreachable!("LEVELS is non-empty")
}

fn compose_at(
    snapshot: &MeshSnapshot,
    scope: Option<&str>,
    level: DigestLevel,
    fact_keep_newest: Option<usize>,
) -> ComposedContext {
    let mut sections: Vec<(String, String)> = Vec::new();

    // 1. pin header
    if let Some(pin) = snapshot.as_of_seq {
        let date = snapshot
            .as_of_date
            .as_deref()
            .map(|d| format!(" (as of {d})"))
            .unwrap_or_default();
        sections.push((
            "Point-in-time pin".into(),
            format!("This brief reflects the ledger as of seq {pin}{date}. Later facts are not visible."),
        ));
    }

    // 2. canon digest at the chosen degradation level
    let digest_body = digest_body(snapshot, scope, level);
    if !digest_body.is_empty() {
        sections.push(("Canon digest".into(), digest_body));
    }

    // 3. accepted facts (optionally trimmed oldest-first)
    let facts: Vec<String> = {
        let all = &snapshot.facts;
        let start = fact_keep_newest
            .map(|keep| all.len().saturating_sub(keep))
            .unwrap_or(0);
        all[start..].iter().map(|f| f.normalized_text()).collect()
    };
    if !facts.is_empty() {
        sections.push(("Accepted facts".into(), facts.join("\n")));
    }

    // 4. locked details — anchors are never degraded or trimmed
    if !snapshot.anchors.is_empty() {
        let lines: Vec<String> = snapshot
            .anchors
            .values()
            .map(|a| format!("{}={} [locked]", a.detail_key, a.locked_value))
            .collect();
        sections.push(("Locked details".into(), lines.join("\n")));
    }

    // 5. open promises
    let open: Vec<String> = snapshot
        .promises
        .iter()
        .filter(|p| p.status == PromiseStatus::Open)
        .map(|p| {
            let due = p
                .due_scope
                .as_deref()
                .map(|d| format!(" (due: {d})"))
                .unwrap_or_default();
            format!("[{}] {}{}", p.key, p.description, due)
        })
        .collect();
    if !open.is_empty() {
        sections.push(("Open promises".into(), open.join("\n")));
    }

    // 6. frequency budgets
    let budget_directives = directives(&snapshot.counters);
    if !budget_directives.is_empty() {
        sections.push(("Frequency budgets".into(), budget_directives));
    }

    ComposedContext { sections }
}

fn digest_body(snapshot: &MeshSnapshot, scope: Option<&str>, level: DigestLevel) -> String {
    let active_group = scope.map(group_of);
    let pick = |tier: u8| -> Vec<&crate::types::Digest> {
        let mut v: Vec<_> = snapshot.digests.iter().filter(|d| d.tier == tier).collect();
        v.sort_by(|a, b| a.scope_path.cmp(&b.scope_path));
        v
    };

    let chosen: Vec<&crate::types::Digest> = match level {
        DigestLevel::AllTier0 => pick(0),
        DigestLevel::ActiveGroupTier0RestTier1 => {
            let mut out: Vec<&crate::types::Digest> = pick(0)
                .into_iter()
                .filter(|d| {
                    active_group
                        .as_deref()
                        .map(|g| group_of(&d.scope_path) == g)
                        .unwrap_or(false)
                })
                .collect();
            out.extend(pick(1).into_iter().filter(|d| {
                active_group
                    .as_deref()
                    .map(|g| d.scope_path != g)
                    .unwrap_or(true)
            }));
            out
        }
        DigestLevel::AllTier1 => pick(1),
        DigestLevel::RollupOnly => pick(2),
    };

    chosen
        .iter()
        .map(|d| d.content.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digests::build_ladder;
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

    fn snapshot() -> MeshSnapshot {
        let facts: Vec<Claim> = (1..=20)
            .map(|i| {
                fact(
                    &format!("c{i}"),
                    i,
                    if i <= 10 { "book.ch1" } else { "book.ch2" },
                    "hero",
                    &format!("k{i}"),
                    &format!("value-{i} with some prose attached to it"),
                )
            })
            .collect();
        let digests = build_ladder("v1", &facts);
        MeshSnapshot {
            version_id: "v1".into(),
            facts,
            digests,
            ..Default::default()
        }
    }

    #[test]
    fn section_order_is_stable() {
        let mut snap = snapshot();
        snap.as_of_seq = Some(10);
        snap.promises.push(Promise {
            id: "p1".into(),
            version_id: "v1".into(),
            key: "reveal".into(),
            kind: "setup".into(),
            description: "the letter must be opened".into(),
            origin_scope: None,
            due_scope: Some("book.ch3".into()),
            status: PromiseStatus::Open,
            seq: 2,
            fulfilled_seq: None,
        });
        let ctx = compose(&snap, Some("book.ch1"), None);
        let titles: Vec<&str> = ctx.sections.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(
            titles,
            vec![
                "Point-in-time pin",
                "Canon digest",
                "Accepted facts",
                "Open promises"
            ]
        );
    }

    #[test]
    fn unbudgeted_compose_serves_all_tier0() {
        let ctx = compose(&snapshot(), Some("book.ch1"), None);
        let digest = &ctx
            .sections
            .iter()
            .find(|(t, _)| t == "Canon digest")
            .unwrap()
            .1;
        assert!(digest.contains("[book.ch1]"));
        assert!(digest.contains("[book.ch2]"));
    }

    #[test]
    fn budget_degrades_digests_before_trimming_facts() {
        let snap = snapshot();
        let full = compose(&snap, Some("book.ch1"), None);
        // a budget just below full forces degradation but must keep all facts
        let budget = full.estimated_tokens() - 20;
        let ctx = compose(&snap, Some("book.ch1"), Some(budget));
        assert!(ctx.estimated_tokens() <= budget);
        let facts = &ctx
            .sections
            .iter()
            .find(|(t, _)| t == "Accepted facts")
            .unwrap()
            .1;
        assert_eq!(
            facts.lines().count(),
            20,
            "facts must not trim while digests can degrade"
        );
    }

    #[test]
    fn tiny_budget_trims_facts_oldest_first() {
        let snap = snapshot();
        let ctx = compose(&snap, Some("book.ch1"), Some(120));
        assert!(ctx.estimated_tokens() <= 120);
        if let Some((_, facts)) = ctx.sections.iter().find(|(t, _)| t == "Accepted facts") {
            // whatever survived must be the NEWEST facts
            assert!(facts.contains("k20"));
            assert!(!facts.contains("k1="));
        }
    }

    #[test]
    fn content_hash_stable_and_pin_sensitive() {
        let snap = snapshot();
        let a = compose(&snap, None, None);
        let b = compose(&snap, None, None);
        assert_eq!(a.content_hash(), b.content_hash());
        let mut pinned = snapshot();
        pinned.as_of_seq = Some(5);
        assert_ne!(
            a.content_hash(),
            compose(&pinned, None, None).content_hash()
        );
    }
}
