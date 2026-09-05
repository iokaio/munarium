// SPDX-License-Identifier: Apache-2.0
//! In-memory StorageBackend. Semantics mirror the reference kernel:
//! seq allocation happens under one write lock (the BEGIN IMMEDIATE
//! equivalent), supersession resolution delegates to the reference
//! `ledger::resolve_slice`, and every store stamps seq from the single
//! lineage counting domain so one pin bounds everything.

pub mod budget;
pub mod evidence;
pub mod sources;
pub use budget::MemBudgetStore;
pub use evidence::MemEvidenceStore;
pub use sources::MemSourceStore;

use async_trait::async_trait;
use munarium_core::ledger::{resolve_slice, FactQuery};
use munarium_core::promises::status_as_of;
use munarium_core::storage::{NewClaim, StorageBackend};
use munarium_core::types::*;
use munarium_core::{KernelError, Result};
use std::collections::{BTreeMap, HashMap};
use tokio::sync::RwLock;

#[derive(Default)]
struct State {
    /// version_id -> parent_id
    versions: HashMap<String, Option<String>>,
    version_meta: HashMap<String, serde_json::Value>,
    /// claims per version (append order)
    claims: HashMap<String, Vec<Claim>>,
    anchors: HashMap<String, Vec<Anchor>>,
    promises: HashMap<String, Vec<Promise>>,
    /// (version, key, scope) -> (count, budget, seq_stamp)
    counters: HashMap<String, Vec<CounterRow>>,
    digests: HashMap<String, Vec<Digest>>,
    /// Persisted gate findings per version (2026-08-17), append order.
    findings: HashMap<String, Vec<munarium_core::storage::StoredFinding>>,
}

struct CounterRow {
    key: String,
    #[allow(dead_code)]
    scope_path: String,
    count: u64,
    budget: Option<u64>,
    seq: Seq,
}

#[derive(Default)]
pub struct MemStore {
    state: RwLock<State>,
}

impl MemStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl State {
    fn lineage_of(&self, version_id: &str) -> Result<Vec<String>> {
        let mut chain = Vec::new();
        let mut cursor = Some(version_id.to_string());
        while let Some(v) = cursor {
            if !self.versions.contains_key(&v) {
                return Err(KernelError::NotFound {
                    kind: "version",
                    id: v,
                });
            }
            chain.push(v.clone());
            cursor = self.versions.get(&v).cloned().flatten();
        }
        chain.reverse(); // root -> leaf
        Ok(chain)
    }

    fn lineage_claims(&self, version_id: &str) -> Result<Vec<Claim>> {
        let mut all = Vec::new();
        for v in self.lineage_of(version_id)? {
            if let Some(cs) = self.claims.get(&v) {
                all.extend(cs.iter().cloned());
            }
        }
        Ok(all)
    }

    /// MAX(seq) across the lineage over EVERY seq-stamped store — the single
    /// monotonic counting domain.
    fn head_of(&self, version_id: &str) -> Result<Seq> {
        let mut head = 0;
        for v in self.lineage_of(version_id)? {
            for c in self.claims.get(&v).into_iter().flatten() {
                head = head.max(c.seq);
            }
            for a in self.anchors.get(&v).into_iter().flatten() {
                head = head.max(a.seq);
            }
            for p in self.promises.get(&v).into_iter().flatten() {
                head = head.max(p.seq).max(p.fulfilled_seq.unwrap_or(0));
            }
            for r in self.counters.get(&v).into_iter().flatten() {
                head = head.max(r.seq);
            }
        }
        Ok(head)
    }
}

#[async_trait]
impl StorageBackend for MemStore {
    async fn create_version(
        &self,
        parent_id: Option<&str>,
        metadata: Option<serde_json::Value>,
    ) -> Result<String> {
        let mut s = self.state.write().await;
        if let Some(p) = parent_id {
            if !s.versions.contains_key(p) {
                return Err(KernelError::NotFound {
                    kind: "version",
                    id: p.to_string(),
                });
            }
        }
        let id = format!("memv-{}", uuid::Uuid::new_v4().simple());
        s.versions.insert(id.clone(), parent_id.map(String::from));
        if let Some(m) = metadata {
            s.version_meta.insert(id.clone(), m);
        }
        Ok(id)
    }

    async fn lineage(&self, version_id: &str) -> Result<Vec<String>> {
        self.state.read().await.lineage_of(version_id)
    }

    async fn head(&self, version_id: &str) -> Result<Seq> {
        self.state.read().await.head_of(version_id)
    }

    async fn append_claim(
        &self,
        version_id: &str,
        claim: NewClaim,
        expected_head: Option<Seq>,
    ) -> Result<Claim> {
        // One write lock spans head-read + append: the BEGIN IMMEDIATE analog.
        let mut s = self.state.write().await;
        let head = s.head_of(version_id)?;
        if let Some(expected) = expected_head {
            if expected != head {
                return Err(KernelError::HeadConflict {
                    expected,
                    actual: head,
                });
            }
        }
        if let Some(sup) = &claim.supersedes_id {
            let exists = s.lineage_claims(version_id)?.iter().any(|c| &c.id == sup);
            if !exists {
                return Err(KernelError::NotFound {
                    kind: "claim",
                    id: sup.clone(),
                });
            }
        }
        let stored = Claim {
            id: format!("claim-{}", uuid::Uuid::new_v4().simple()),
            version_id: version_id.to_string(),
            seq: head + 1,
            claim_type: claim.claim_type,
            subject: claim.subject,
            key: claim.key,
            value: claim.value,
            scope_path: claim.scope_path,
            status: claim.status,
            provenance: claim.provenance,
            supersedes_id: claim.supersedes_id,
            entity_id: claim.entity_id,
            evidence: claim.evidence,
            confidence: claim.confidence,
            shape_ref: claim.shape_ref,
            origin: claim.origin,
        };
        s.claims
            .entry(version_id.to_string())
            .or_default()
            .push(stored.clone());
        Ok(stored)
    }

    async fn append_claims(
        &self,
        version_id: &str,
        claims: Vec<NewClaim>,
        expected_head: Option<Seq>,
    ) -> Result<Vec<Claim>> {
        if claims.is_empty() {
            return Ok(Vec::new());
        }
        // One write lock spans the whole batch: all claims land or none does.
        let mut s = self.state.write().await;
        let head = s.head_of(version_id)?;
        if let Some(expected) = expected_head {
            if expected != head {
                return Err(KernelError::HeadConflict {
                    expected,
                    actual: head,
                });
            }
        }
        // Validate every supersedes_id BEFORE the first mutation.
        let existing: std::collections::HashSet<String> = s
            .lineage_claims(version_id)?
            .iter()
            .map(|c| c.id.clone())
            .collect();
        for claim in &claims {
            if let Some(sup) = &claim.supersedes_id {
                if !existing.contains(sup) {
                    return Err(KernelError::NotFound {
                        kind: "claim",
                        id: sup.clone(),
                    });
                }
            }
        }
        let mut out = Vec::new();
        for (i, claim) in claims.into_iter().enumerate() {
            let stored = Claim {
                id: format!("claim-{}", uuid::Uuid::new_v4().simple()),
                version_id: version_id.to_string(),
                seq: head + 1 + i as Seq,
                claim_type: claim.claim_type,
                subject: claim.subject,
                key: claim.key,
                value: claim.value,
                scope_path: claim.scope_path,
                status: claim.status,
                provenance: claim.provenance,
                supersedes_id: claim.supersedes_id,
                entity_id: claim.entity_id,
                evidence: claim.evidence,
                confidence: claim.confidence,
                shape_ref: claim.shape_ref,
                origin: claim.origin,
            };
            s.claims
                .entry(version_id.to_string())
                .or_default()
                .push(stored.clone());
            out.push(stored);
        }
        Ok(out)
    }

    async fn slice_facts(&self, version_id: &str, q: &FactQuery) -> Result<Vec<Claim>> {
        let s = self.state.read().await;
        Ok(resolve_slice(s.lineage_claims(version_id)?, q))
    }

    async fn get_claim(&self, claim_id: &str) -> Result<Option<Claim>> {
        let s = self.state.read().await;
        Ok(s.claims
            .values()
            .flatten()
            .find(|c| c.id == claim_id)
            .cloned())
    }

    async fn superseded_by(&self, claim_id: &str) -> Result<Option<String>> {
        let s = self.state.read().await;
        // Earliest superseder by seq — deterministic, matching the pg store.
        Ok(s.claims
            .values()
            .flatten()
            .filter(|c| c.supersedes_id.as_deref() == Some(claim_id))
            .min_by_key(|c| c.seq)
            .map(|c| c.id.clone()))
    }

    async fn lock_anchor(
        &self,
        version_id: &str,
        subject: &str,
        key: &str,
        value: &str,
        scope_path: Option<&str>,
        evidence: Option<serde_json::Value>,
    ) -> Result<Anchor> {
        let mut s = self.state.write().await;
        let seq = s.head_of(version_id)? + 1;
        let anchor = Anchor {
            id: format!("anchor-{}", uuid::Uuid::new_v4().simple()),
            version_id: version_id.to_string(),
            detail_key: format!("{subject}.{key}"),
            locked_value: value.to_string(),
            locked_at_scope: scope_path.map(String::from),
            status: AnchorStatus::Locked,
            seq,
            evidence,
        };
        s.anchors
            .entry(version_id.to_string())
            .or_default()
            .push(anchor.clone());
        Ok(anchor)
    }

    async fn anchors(
        &self,
        version_id: &str,
        as_of_seq: Option<Seq>,
    ) -> Result<BTreeMap<String, Anchor>> {
        let s = self.state.read().await;
        let mut out = BTreeMap::new();
        // root -> leaf order: later version wins by overwriting
        for v in s.lineage_of(version_id)? {
            for a in s.anchors.get(&v).into_iter().flatten() {
                if a.status != AnchorStatus::Locked {
                    continue;
                }
                if let Some(pin) = as_of_seq {
                    if a.seq > pin {
                        continue;
                    }
                }
                out.insert(a.detail_key.clone(), a.clone());
            }
        }
        Ok(out)
    }

    async fn register_promise(
        &self,
        version_id: &str,
        key: &str,
        kind: &str,
        description: &str,
        origin_scope: Option<&str>,
        due_scope: Option<&str>,
    ) -> Result<Promise> {
        let mut s = self.state.write().await;
        // Registration advances the ledger clock (head + 1), matching every
        // other seq-stamped store, so registrations stay orderable under a pin.
        let seq = s.head_of(version_id)? + 1;
        let p = Promise {
            id: format!("prom-{}", uuid::Uuid::new_v4().simple()),
            version_id: version_id.to_string(),
            key: key.to_string(),
            kind: kind.to_string(),
            description: description.to_string(),
            origin_scope: origin_scope.map(String::from),
            due_scope: due_scope.map(String::from),
            status: PromiseStatus::Open,
            seq,
            fulfilled_seq: None,
        };
        s.promises
            .entry(version_id.to_string())
            .or_default()
            .push(p.clone());
        Ok(p)
    }

    async fn promises(&self, version_id: &str, as_of_seq: Option<Seq>) -> Result<Vec<Promise>> {
        let s = self.state.read().await;
        let mut out = Vec::new();
        for v in s.lineage_of(version_id)? {
            for p in s.promises.get(&v).into_iter().flatten() {
                if let Some(pin) = as_of_seq {
                    if p.seq > pin {
                        continue; // post-pin registration hidden
                    }
                }
                let mut view = p.clone();
                view.status = status_as_of(p, as_of_seq); // post-pin fulfillment reads open
                if view.status == PromiseStatus::Open {
                    view.fulfilled_seq = None;
                }
                out.push(view);
            }
        }
        out.sort_by(|a, b| a.seq.cmp(&b.seq).then(a.key.cmp(&b.key)));
        Ok(out)
    }

    async fn fulfill_promise(&self, version_id: &str, key: &str) -> Result<bool> {
        let mut s = self.state.write().await;
        // Fulfillment lands at head + 1: a pin taken at the current head must
        // still read the promise as open.
        let fulfilled_seq = s.head_of(version_id)? + 1;
        for v in s.lineage_of(version_id)? {
            if let Some(list) = s.promises.get_mut(&v) {
                if let Some(p) = list
                    .iter_mut()
                    .find(|p| p.key == key && p.status == PromiseStatus::Open)
                {
                    p.status = PromiseStatus::Fulfilled;
                    p.fulfilled_seq = Some(fulfilled_seq);
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    async fn record_counts(
        &self,
        version_id: &str,
        key: &str,
        scope_path: &str,
        count: u64,
        budget: Option<u64>,
    ) -> Result<()> {
        let mut s = self.state.write().await;
        let seq = s.head_of(version_id)? + 1;
        let rows = s.counters.entry(version_id.to_string()).or_default();
        if let Some(row) = rows
            .iter_mut()
            .find(|r| r.key == key && r.scope_path == scope_path)
        {
            // Re-stamp seq: an updated count is a new observation at the
            // current head — keeping the original stamp leaked future values
            // into pinned reads.
            row.count = count;
            row.seq = seq;
            if budget.is_some() {
                row.budget = budget;
            }
        } else {
            rows.push(CounterRow {
                key: key.to_string(),
                scope_path: scope_path.to_string(),
                count,
                budget,
                seq,
            });
        }
        Ok(())
    }

    async fn counter_totals(
        &self,
        version_id: &str,
        as_of_seq: Option<Seq>,
    ) -> Result<Vec<CounterTotal>> {
        let s = self.state.read().await;
        let mut agg: BTreeMap<String, (u64, Option<u64>)> = BTreeMap::new();
        for v in s.lineage_of(version_id)? {
            for r in s.counters.get(&v).into_iter().flatten() {
                if let Some(pin) = as_of_seq {
                    if r.seq > pin {
                        continue;
                    }
                }
                let entry = agg.entry(r.key.clone()).or_insert((0, None));
                entry.0 += r.count;
                entry.1 = match (entry.1, r.budget) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (a, b) => a.or(b),
                };
            }
        }
        Ok(agg
            .into_iter()
            .map(|(key, (total, budget))| CounterTotal { key, total, budget })
            .collect())
    }

    async fn upsert_digest(&self, digest: &Digest) -> Result<()> {
        let mut s = self.state.write().await;
        let list = s.digests.entry(digest.version_id.clone()).or_default();
        if let Some(existing) = list
            .iter_mut()
            .find(|d| d.tier == digest.tier && d.scope_path == digest.scope_path)
        {
            *existing = digest.clone();
        } else {
            list.push(digest.clone());
        }
        Ok(())
    }

    async fn digests(&self, version_id: &str) -> Result<Vec<Digest>> {
        let s = self.state.read().await;
        let mut out = Vec::new();
        for v in s.lineage_of(version_id)? {
            out.extend(s.digests.get(&v).into_iter().flatten().cloned());
        }
        Ok(out)
    }

    async fn version_metadata(&self, version_id: &str) -> Result<Option<serde_json::Value>> {
        let s = self.state.read().await;
        if !s.versions.contains_key(version_id) {
            return Err(munarium_core::KernelError::NotFound {
                kind: "version",
                id: version_id.to_string(),
            });
        }
        Ok(s.version_meta.get(version_id).cloned())
    }

    async fn record_findings(
        &self,
        version_id: &str,
        seq: Seq,
        findings: &[munarium_core::types::GateFinding],
    ) -> Result<()> {
        let mut s = self.state.write().await;
        s.lineage_of(version_id)?; // unknown version -> the same NotFound as every store
        let rows = s.findings.entry(version_id.to_string()).or_default();
        rows.extend(
            findings
                .iter()
                .map(|f| munarium_core::storage::StoredFinding {
                    seq,
                    finding: f.clone(),
                }),
        );
        Ok(())
    }

    async fn findings(
        &self,
        version_id: &str,
        q: &munarium_core::storage::FindingsQuery,
    ) -> Result<Vec<munarium_core::storage::StoredFinding>> {
        let s = self.state.read().await;
        let mut out = Vec::new();
        for v in s.lineage_of(version_id)? {
            out.extend(s.findings.get(&v).into_iter().flatten().cloned());
        }
        out.sort_by_key(|f| f.seq);
        out.retain(|f| {
            q.as_of_seq.is_none_or(|pin| f.seq <= pin)
                && q.severity.is_none_or(|sev| f.finding.severity == sev)
                && q.rule_id.as_deref().is_none_or(|r| f.finding.rule_id == r)
                && q.rule_prefix
                    .as_deref()
                    .is_none_or(|p| f.finding.rule_id.starts_with(p))
        });
        // The same default ceiling the Postgres store binds when no limit is
        // given, so a lineage with more than 1000 findings reads the same
        // through both backends.
        out.truncate(q.limit.unwrap_or(1000));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn store_with_version() -> (MemStore, String) {
        let store = MemStore::new();
        let v = store.create_version(None, None).await.unwrap();
        (store, v)
    }

    #[tokio::test]
    async fn append_allocates_monotonic_seq_and_checks_head() {
        let (store, v) = store_with_version().await;
        let c1 = store
            .append_claim(&v, NewClaim::fact("hero", "eyes", "green"), Some(0))
            .await
            .unwrap();
        assert_eq!(c1.seq, 1);

        // stale expected_head => HeadConflict (normal, retryable)
        let err = store
            .append_claim(&v, NewClaim::fact("hero", "home", "harbor"), Some(0))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            KernelError::HeadConflict {
                expected: 0,
                actual: 1
            }
        ));

        let c2 = store
            .append_claim(&v, NewClaim::fact("hero", "home", "harbor"), Some(1))
            .await
            .unwrap();
        assert_eq!(c2.seq, 2);
    }

    #[tokio::test]
    async fn lineage_reads_span_versions_and_supersession_resolves() {
        let (store, v1) = store_with_version().await;
        let c1 = store
            .append_claim(&v1, NewClaim::fact("hero", "eyes", "green"), None)
            .await
            .unwrap();
        let v2 = store.create_version(Some(&v1), None).await.unwrap();
        let mut correction = NewClaim::fact("hero", "eyes", "blue");
        correction.claim_type = ClaimType::Correction;
        correction.supersedes_id = Some(c1.id.clone());
        store.append_claim(&v2, correction, None).await.unwrap();

        let facts = store.slice_facts(&v2, &FactQuery::default()).await.unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].value, "blue");

        // pinned before the correction: original still current
        let pinned = store
            .slice_facts(
                &v2,
                &FactQuery {
                    as_of_seq: Some(1),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(pinned[0].value, "green");
    }

    #[tokio::test]
    async fn one_pin_bounds_anchors_and_promises_together() {
        let (store, v) = store_with_version().await;
        store
            .append_claim(&v, NewClaim::fact("hero", "eyes", "green"), None)
            .await
            .unwrap(); // seq 1
        store
            .register_promise(&v, "reveal", "setup", "open the letter", None, None)
            .await
            .unwrap(); // seq 2 (registration advances the clock like every store)
        store
            .lock_anchor(&v, "hero", "eyes", "green", None, None)
            .await
            .unwrap(); // seq 3
        store
            .append_claim(&v, NewClaim::fact("hero", "home", "harbor"), None)
            .await
            .unwrap(); // seq 4
        store.fulfill_promise(&v, "reveal").await.unwrap(); // fulfilled_seq 5

        // pin at 1: neither the promise (seq 2) nor the anchor (seq 3) exists
        // yet — registration must NOT leak backwards past its own tick
        assert!(store.anchors(&v, Some(1)).await.unwrap().is_empty());
        assert!(store.promises(&v, Some(1)).await.unwrap().is_empty());

        // pin at 2: promise registered and OPEN; anchor still invisible
        assert!(store.anchors(&v, Some(2)).await.unwrap().is_empty());
        let promises = store.promises(&v, Some(2)).await.unwrap();
        assert_eq!(promises.len(), 1);
        assert_eq!(
            promises[0].status,
            PromiseStatus::Open,
            "post-pin fulfillment must read open"
        );

        // pin at 4: everything registered, fulfillment (seq 5) still open
        assert_eq!(store.anchors(&v, Some(4)).await.unwrap().len(), 1);
        assert_eq!(
            store.promises(&v, Some(4)).await.unwrap()[0].status,
            PromiseStatus::Open
        );

        // head: anchor visible, promise fulfilled
        assert_eq!(store.anchors(&v, None).await.unwrap().len(), 1);
        assert_eq!(
            store.promises(&v, None).await.unwrap()[0].status,
            PromiseStatus::Fulfilled
        );
    }

    #[tokio::test]
    async fn snapshot_rebuilds_digests_under_pin() {
        use munarium_core::storage::load_snapshot;
        let (store, v) = store_with_version().await;
        let mut c = NewClaim::fact("hero", "eyes", "green");
        c.scope_path = Some("ch1".into());
        store.append_claim(&v, c, None).await.unwrap(); // seq 1
        let mut c = NewClaim::fact("hero", "home", "harbor");
        c.scope_path = Some("ch1".into());
        store.append_claim(&v, c, None).await.unwrap(); // seq 2

        // stored digest reflecting head — must NOT be served under a pin
        let head_facts = store.slice_facts(&v, &FactQuery::default()).await.unwrap();
        for d in munarium_core::digests::build_ladder(&v, &head_facts) {
            store.upsert_digest(&d).await.unwrap();
        }

        let pinned = load_snapshot(&store, &v, None, None, Some(1))
            .await
            .unwrap();
        let tier0: Vec<_> = pinned.digests.iter().filter(|d| d.tier == 0).collect();
        assert_eq!(tier0.len(), 1);
        assert!(
            !tier0[0].content.contains("home"),
            "pinned digest must be rebuilt, not served"
        );
    }
}
