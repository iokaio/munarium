// SPDX-License-Identifier: Apache-2.0
//! The StorageBackend trait — the seam every store implements
//! (munarium-store-mem, munarium-store-pg). Tenant scoping is structural: a backend
//! handle is already scoped to one tenant when it reaches the kernel; no
//! method takes a tenant parameter, so no call can forget the predicate.

use crate::ledger::FactQuery;
use crate::types::*;
use crate::Result;
use async_trait::async_trait;
use std::collections::BTreeMap;

/// Input for a new claim append (id and seq are allocated by the backend).
#[derive(Debug, Clone)]
pub struct NewClaim {
    pub claim_type: ClaimType,
    pub subject: String,
    pub key: String,
    pub value: String,
    pub scope_path: Option<String>,
    pub status: ClaimStatus,
    pub provenance: Provenance,
    pub supersedes_id: Option<String>,
    pub entity_id: Option<String>,
    pub evidence: Option<serde_json::Value>,
    pub confidence: Option<f64>,
    pub shape_ref: Option<String>,
    /// Connector origin; `None` unless a connector proposed it.
    pub origin: Option<ClaimOrigin>,
}

impl NewClaim {
    pub fn fact(subject: &str, key: &str, value: &str) -> Self {
        Self {
            claim_type: ClaimType::Fact,
            subject: subject.into(),
            key: key.into(),
            value: value.into(),
            scope_path: None,
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
}

/// Filter for the persisted-findings read (2026-08-17). `as_of_seq` bounds
/// by the seq the findings were RECORDED at, so one pin bounds this store
/// like every other.
#[derive(Debug, Clone, Default)]
pub struct FindingsQuery {
    pub as_of_seq: Option<Seq>,
    pub severity: Option<Severity>,
    pub rule_id: Option<String>,
    /// Prefix match on the rule id — `matrix.` selects every connector
    /// finding, `gate.` every kernel gate. Combines with `rule_id`
    /// by AND, which is only useful when both are set consistently.
    pub rule_prefix: Option<String>,
    pub limit: Option<usize>,
}

/// One persisted gate finding, stamped with the head seq its write settled
/// at.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredFinding {
    pub seq: Seq,
    pub finding: GateFinding,
}

#[async_trait]
pub trait StorageBackend: Send + Sync {
    // --- versions / lineage -------------------------------------------------
    async fn create_version(
        &self,
        parent_id: Option<&str>,
        metadata: Option<serde_json::Value>,
    ) -> Result<String>;

    /// Version ids root -> leaf inclusive.
    async fn lineage(&self, version_id: &str) -> Result<Vec<String>>;

    /// MAX(seq) across the whole lineage — the single monotonic domain.
    async fn head(&self, version_id: &str) -> Result<Seq>;

    // --- ledger ------------------------------------------------------------
    /// Appends one claim, allocating `seq = head + 1` atomically (the
    /// BEGIN IMMEDIATE / SELECT FOR UPDATE equivalent). `expected_head`
    /// Some(n) => optimistic check; mismatch is `KernelError::HeadConflict`.
    async fn append_claim(
        &self,
        version_id: &str,
        claim: NewClaim,
        expected_head: Option<Seq>,
    ) -> Result<Claim>;

    /// Appends a batch as ONE unit: every claim lands or none does, seqs are
    /// consecutive from head + 1, and `expected_head` is checked under the
    /// same lock that allocates them. Real storage backends MUST override
    /// this with a transactional implementation; the default per-claim loop
    /// (chained through each claim's resulting seq) exists only for thin
    /// wire adapters whose far end is itself atomic.
    async fn append_claims(
        &self,
        version_id: &str,
        claims: Vec<NewClaim>,
        expected_head: Option<Seq>,
    ) -> Result<Vec<Claim>> {
        let mut expected = expected_head;
        let mut out = Vec::new();
        for claim in claims {
            let stored = self.append_claim(version_id, claim, expected).await?;
            expected = Some(stored.seq);
            out.push(stored);
        }
        Ok(out)
    }

    /// Lineage read with pin-aware supersession resolution
    /// (`ledger::resolve_slice` is the reference semantics).
    async fn slice_facts(&self, version_id: &str, q: &FactQuery) -> Result<Vec<Claim>>;

    async fn get_claim(&self, claim_id: &str) -> Result<Option<Claim>>;

    /// Whether any visible claim supersedes this id (and which one).
    async fn superseded_by(&self, claim_id: &str) -> Result<Option<String>>;

    // --- anchors -------------------------------------------------------------
    /// Locks `subject.key` at `value`; stamps seq = head + 1 (the seq its
    /// companion claim will get).
    async fn lock_anchor(
        &self,
        version_id: &str,
        subject: &str,
        key: &str,
        value: &str,
        scope_path: Option<&str>,
        evidence: Option<serde_json::Value>,
    ) -> Result<Anchor>;

    /// detail_key -> Anchor over the lineage; later version wins; locked only;
    /// pin-bounded.
    async fn anchors(
        &self,
        version_id: &str,
        as_of_seq: Option<Seq>,
    ) -> Result<BTreeMap<String, Anchor>>;

    // --- promises ---------------------------------------------------------
    async fn register_promise(
        &self,
        version_id: &str,
        key: &str,
        kind: &str,
        description: &str,
        origin_scope: Option<&str>,
        due_scope: Option<&str>,
    ) -> Result<Promise>;

    /// Lineage promises with pin semantics applied: post-pin registrations
    /// hidden; post-pin fulfillment reads back OPEN.
    async fn promises(&self, version_id: &str, as_of_seq: Option<Seq>) -> Result<Vec<Promise>>;

    /// Fulfills the first open promise with this key. Ok(false) = none open.
    async fn fulfill_promise(&self, version_id: &str, key: &str) -> Result<bool>;

    // --- counters --------------------------------------------------------
    async fn record_counts(
        &self,
        version_id: &str,
        key: &str,
        scope_path: &str,
        count: u64,
        budget: Option<u64>,
    ) -> Result<()>;

    /// SUM(count) / MAX(budget) grouped by key over the lineage, pin-bounded.
    async fn counter_totals(
        &self,
        version_id: &str,
        as_of_seq: Option<Seq>,
    ) -> Result<Vec<CounterTotal>>;

    // --- digests ------------------------------------------------------------
    /// Upserts stored rung text (no history — which is exactly why pinned
    /// reads REBUILD instead of serving these).
    async fn upsert_digest(&self, digest: &Digest) -> Result<()>;

    /// Stored rungs for a lineage. Callers must NOT use these under a pin.
    async fn digests(&self, version_id: &str) -> Result<Vec<Digest>>;

    /// The metadata the version was created with (None when absent). Added
    /// 2026-08-17 for the chronology arming surface — a version arms the
    /// sixth gate by naming a rules asset under `chronology_rules`. Default:
    /// unsupported (wire-client adapters never need it; the server reads it
    /// against the real backends).
    async fn version_metadata(&self, _version_id: &str) -> Result<Option<serde_json::Value>> {
        Err(crate::KernelError::Storage(
            "version metadata reads are not supported by this backend".into(),
        ))
    }

    // --- gate findings (2026-08-17) -----------------------------------------
    /// Persist the findings a gated write produced, stamped at the seq the
    /// write settled at. Until this landed, findings rode ONLY the write
    /// response — an ingestion worker that dropped the response lost the
    /// evidence pane. Default: unsupported — the ledger backends override;
    /// wire-client adapters (conformance) keep the default because findings
    /// are recorded server-side on their writes.
    async fn record_findings(
        &self,
        _version_id: &str,
        _seq: Seq,
        _findings: &[GateFinding],
    ) -> Result<()> {
        Err(crate::KernelError::Storage(
            "findings persistence is not supported by this backend".into(),
        ))
    }

    /// The persisted findings for a version's lineage, oldest first.
    /// Same default posture as `record_findings`.
    async fn findings(&self, _version_id: &str, _q: &FindingsQuery) -> Result<Vec<StoredFinding>> {
        Err(crate::KernelError::Storage(
            "findings persistence is not supported by this backend".into(),
        ))
    }
}

/// Loads the point-in-time snapshot the composer and gates read: one pin
/// bounds facts, anchors, promises, and counters together; digests under a
/// pin are REBUILT deterministically from the pinned facts.
pub async fn load_snapshot(
    store: &dyn StorageBackend,
    version_id: &str,
    scope_prefix: Option<&str>,
    fact_limit: Option<usize>,
    as_of_seq: Option<Seq>,
) -> Result<MeshSnapshot> {
    let facts = store
        .slice_facts(
            version_id,
            &FactQuery {
                scope_prefix: scope_prefix.map(String::from),
                as_of_seq,
                statuses: vec![],
                limit: fact_limit,
            },
        )
        .await?;

    let digests = if as_of_seq.is_some() {
        // Rebuild from the FULL pinned fact view (not the scope-filtered one).
        let all_pinned = store
            .slice_facts(
                version_id,
                &FactQuery {
                    as_of_seq,
                    ..Default::default()
                },
            )
            .await?;
        crate::digests::build_ladder(version_id, &all_pinned)
    } else {
        let stored = store.digests(version_id).await?;
        if stored.is_empty() {
            let all = store.slice_facts(version_id, &FactQuery::default()).await?;
            crate::digests::build_ladder(version_id, &all)
        } else {
            stored
        }
    };

    Ok(MeshSnapshot {
        version_id: version_id.to_string(),
        facts,
        anchors: store.anchors(version_id, as_of_seq).await?,
        digests,
        promises: store.promises(version_id, as_of_seq).await?,
        counters: store.counter_totals(version_id, as_of_seq).await?,
        entities: Vec::new(),
        as_of_seq,
        as_of_date: None,
    })
}
