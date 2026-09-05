// SPDX-License-Identifier: Apache-2.0
//! A contract-conformant in-memory server.
//!
//! This is not a toy. It enforces the parts of the server's contract that
//! Matrix's correctness depends on, so a test that passes here is testing
//! something real:
//!
//! - **Seal idempotency by logical hash.** Sealing the same logical result
//!   twice returns the SAME evidence id. That is the property the turn path
//!   relies on when a retry re-seals.
//! - **Access domination on read.** `get_evidence` refuses when the reader
//!   does not dominate the artifact's authorization class.
//! - **Bulk-upload manifest diff.** A second upload of identical bytes at the
//!   same paths reports them as `skipped_existing`, which is how the replayed
//!   checkpoint test proves "zero new documents".
//! - **Findings are warn-only.** A `block` severity is refused, because only a
//!   gate may block.

use crate::*;
use munarium_matrix_types::contract::EvidenceManifest;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Debug, Default)]
struct State {
    /// evidence_id -> (manifest, bytes)
    evidence: BTreeMap<String, (EvidenceManifest, Vec<u8>)>,
    /// logical_result_hash -> evidence_id (the domain idempotency rule)
    by_logical: BTreeMap<String, String>,
    /// path -> content hash
    documents: BTreeMap<String, String>,
    facts: BTreeMap<String, Vec<LedgerFact>>,
    findings: Vec<FindingRequest>,
    /// idempotency key -> the first outcome, replayed verbatim.
    proposals: BTreeMap<String, ProposeOutcome>,
    /// Every proposal request, in order — what the scenarios assert on.
    proposed: Vec<ProposeClaimRequest>,
    /// When set, the next proposal is recorded DISPUTED with this rule id.
    dispute_next: Option<String>,
    next: u64,
    /// claim id -> the claim it supersedes, so `slice_facts` can resolve.
    supersedes: std::collections::HashMap<String, String>,
}

/// An in-memory munarium-server, good enough to be worth testing against.
#[derive(Clone, Debug)]
pub struct MockServer {
    state: Arc<Mutex<State>>,
    /// Who is reading. Sealing is unrestricted (Matrix is trusted to seal what
    /// it read); READING is dominated, which is the direction that matters.
    reader_level: i32,
    reader_compartments: Vec<String>,
    version: String,
    /// Force the next call to fail — for testing the unavailable path.
    pub fail_next: Arc<Mutex<Option<ServerError>>>,
}

impl Default for MockServer {
    fn default() -> Self {
        Self::new()
    }
}

impl MockServer {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
            reader_level: i32::MAX,
            reader_compartments: vec![],
            version: "0.4.0".to_string(),
            fail_next: Arc::new(Mutex::new(None)),
        }
    }

    /// A reader with a specific clearance — the under-cleared-session test.
    pub fn as_reader(&self, level: i32, compartments: &[&str]) -> Self {
        Self {
            state: self.state.clone(),
            reader_level: level,
            reader_compartments: compartments.iter().map(|s| s.to_string()).collect(),
            version: self.version.clone(),
            fail_next: self.fail_next.clone(),
        }
    }

    pub fn with_version(mut self, v: &str) -> Self {
        self.version = v.to_string();
        self
    }

    pub fn seed_facts(&self, version_id: &str, facts: Vec<LedgerFact>) {
        self.state
            .lock()
            .unwrap()
            .facts
            .insert(version_id.to_string(), facts);
    }

    /// Every claim Matrix proposed, in order.
    /// Every fact ever written to a lineage, superseded ones included — the
    /// history, as opposed to the resolved view `slice_facts` returns.
    pub fn all_facts(&self, version_id: &str) -> Vec<LedgerFact> {
        self.state
            .lock()
            .unwrap()
            .facts
            .get(version_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn proposed_claims(&self) -> Vec<ProposeClaimRequest> {
        self.state.lock().unwrap().proposed.clone()
    }

    /// Make the next proposal come back DISPUTED under `rule_id`, the way a
    /// gate would. Consumed by the next call.
    pub fn dispute_next(&self, rule_id: &str) {
        self.state.lock().unwrap().dispute_next = Some(rule_id.to_string());
    }

    pub fn filed_findings(&self) -> Vec<FindingRequest> {
        self.state.lock().unwrap().findings.clone()
    }

    pub fn document_count(&self) -> usize {
        self.state.lock().unwrap().documents.len()
    }

    pub fn evidence_count(&self) -> usize {
        self.state.lock().unwrap().evidence.len()
    }

    /// The stored bytes, for a test that wants to check what was actually
    /// sealed rather than what we meant to seal.
    pub fn evidence_bytes(&self, id: &str) -> Option<Vec<u8>> {
        self.state
            .lock()
            .unwrap()
            .evidence
            .get(id)
            .map(|(_, b)| b.clone())
    }

    fn take_failure(&self) -> Option<ServerError> {
        self.fail_next.lock().unwrap().take()
    }

    fn mint(&self, state: &mut State) -> String {
        state.next += 1;
        format!("ev-mock{:028}", state.next)
    }
}

#[async_trait]
impl ServerClient for MockServer {
    async fn server_version(&self) -> Result<String> {
        if let Some(e) = self.take_failure() {
            return Err(e);
        }
        Ok(self.version.clone())
    }

    async fn seal_evidence(
        &self,
        manifest: &EvidenceManifest,
        bytes: &[u8],
        _idempotency_key: Option<&str>,
    ) -> Result<String> {
        if let Some(e) = self.take_failure() {
            return Err(e);
        }
        // The server verifies both hashes before commit; so does this, because
        // a client that computes them wrongly must fail here and not in
        // production.
        let actual = munarium_matrix_core::artifact_hash(bytes);
        if actual != manifest.artifact_hash {
            return Err(ServerError::Problem {
                status: 422,
                slug: "hash-mismatch".into(),
                detail: format!(
                    "artifact_hash {} does not match the bytes ({actual})",
                    manifest.artifact_hash
                ),
            });
        }
        let mut state = self.state.lock().unwrap();
        // Domain idempotency: (tenant, logical hash, policy, class). Same
        // logical result, same id.
        let idem_key = format!(
            "{}|{}|{}|{}",
            manifest.tenant,
            manifest.logical_result_hash,
            manifest.versions.policy.clone().unwrap_or_default(),
            manifest
                .authorization_class
                .name
                .clone()
                .unwrap_or_default()
        );
        if let Some(existing) = state.by_logical.get(&idem_key) {
            return Ok(existing.clone());
        }
        let id = self.mint(&mut state);
        let mut stored = manifest.clone();
        stored.evidence_id = Some(id.clone());
        state.evidence.insert(id.clone(), (stored, bytes.to_vec()));
        state.by_logical.insert(idem_key, id.clone());
        Ok(id)
    }

    async fn get_evidence(&self, evidence_id: &str) -> Result<EvidenceManifest> {
        if let Some(e) = self.take_failure() {
            return Err(e);
        }
        let state = self.state.lock().unwrap();
        let (manifest, _) = state
            .evidence
            .get(evidence_id)
            .ok_or(ServerError::Problem {
                status: 404,
                slug: "not-found".into(),
                detail: format!("no evidence {evidence_id}"),
            })?;
        // Expiry and hold resolve to their own typed outcomes, never to 404 —
        // "it existed and you cannot have it" is different from "it never was".
        if let Some(r) = &manifest.retention {
            if r.legal_hold {
                return Err(ServerError::Problem {
                    status: 409,
                    slug: "evidence-on-hold".into(),
                    detail: "artifact is under legal hold".into(),
                });
            }
            if r.purged_at.is_some() {
                return Err(ServerError::Problem {
                    status: 410,
                    slug: "evidence-expired".into(),
                    detail: "artifact was purged at the end of its retention".into(),
                });
            }
        }
        if !manifest
            .authorization_class
            .dominated_by(self.reader_level, &self.reader_compartments)
        {
            // 403, not 404: the caller knows the citation exists because the
            // answer cited it. Pretending otherwise would be a lie the operator
            // then has to debug.
            return Err(ServerError::Problem {
                status: 403,
                slug: "evidence-class-denied".into(),
                detail: "session does not dominate this artifact's authorization class".into(),
            });
        }
        Ok(manifest.clone())
    }

    async fn bulk_upload(
        &self,
        _label: &str,
        documents: &[UploadDocument],
    ) -> Result<UploadOutcome> {
        if let Some(e) = self.take_failure() {
            return Err(e);
        }
        let mut state = self.state.lock().unwrap();
        let mut out = UploadOutcome::default();
        for d in documents {
            let hash = d.content_hash();
            match state.documents.get(&d.path) {
                // Identical bytes already at this path: nothing to do. This is
                // the manifest diff, and it is what a replayed checkpoint hits.
                Some(existing) if existing == &hash => out.skipped_existing += 1,
                _ => {
                    state.documents.insert(d.path.clone(), hash);
                    out.stored += 1;
                }
            }
        }
        Ok(out)
    }

    async fn slice_facts(
        &self,
        version_id: &str,
        as_of_seq: Option<u64>,
    ) -> Result<Vec<LedgerFact>> {
        if let Some(e) = self.take_failure() {
            return Err(e);
        }
        let state = self.state.lock().unwrap();
        let facts = state.facts.get(version_id).cloned().unwrap_or_default();
        // Resolved like the server's `resolve_slice`: the pinned view, minus
        // every claim something in that view supersedes. Until 2026-08-29 this
        // returned superseded claims as current, so a chain of proposals read
        // back as several facts for one key and the rollback that would have
        // left the real server in that state passed here.
        let pinned: Vec<LedgerFact> = match as_of_seq {
            Some(seq) => facts.into_iter().filter(|f| f.seq <= seq).collect(),
            None => facts,
        };
        let superseded: std::collections::HashSet<String> = pinned
            .iter()
            .filter_map(|f| f.claim_id.as_deref())
            .filter_map(|id| state.supersedes.get(id).cloned())
            .collect();
        Ok(pinned
            .into_iter()
            .filter(|f| {
                f.claim_id
                    .as_deref()
                    .is_none_or(|id| !superseded.contains(id))
            })
            .collect())
    }

    async fn head_seq(&self, version_id: &str) -> Result<u64> {
        let state = self.state.lock().unwrap();
        Ok(state
            .facts
            .get(version_id)
            .and_then(|f| f.iter().map(|f| f.seq).max())
            .unwrap_or(0))
    }

    async fn file_finding(&self, req: &FindingRequest) -> Result<String> {
        if let Some(e) = self.take_failure() {
            return Err(e);
        }
        if req.severity == "block" {
            return Err(ServerError::Problem {
                status: 422,
                slug: "invalid-severity".into(),
                detail: "only a gate may file a block; external findings are warn or info".into(),
            });
        }
        let mut state = self.state.lock().unwrap();
        state.findings.push(req.clone());
        Ok(format!("fnd-mock{:04}", state.findings.len()))
    }

    async fn propose_claim(
        &self,
        req: &ProposeClaimRequest,
        idempotency_key: &str,
    ) -> Result<ProposeOutcome> {
        if let Some(e) = self.take_failure() {
            return Err(e);
        }
        let mut state = self.state.lock().unwrap();
        // Idempotent by key, like the server: a replay returns the FIRST
        // outcome and appends nothing.
        if let Some(out) = state.proposals.get(idempotency_key) {
            return Ok(out.clone());
        }
        // A correction/update must name a claim that exists in the lineage —
        // the server refuses otherwise, and so does this mock.
        if let Some(sup) = &req.supersedes_id {
            let known = state
                .facts
                .get(&req.version_id)
                .map(|fs| {
                    fs.iter()
                        .any(|f| f.claim_id.as_deref() == Some(sup.as_str()))
                })
                .unwrap_or(false);
            if !known {
                return Err(ServerError::Problem {
                    status: 404,
                    slug: "not-found".into(),
                    detail: format!("supersedes_id '{sup}' is not in the lineage"),
                });
            }
        }
        let dispute = state.dispute_next.take();
        state.next += 1;
        let claim_id = format!("claim-mock{:04}", state.next);
        let seq = state
            .facts
            .get(&req.version_id)
            .map(|f| f.len() as u64)
            .unwrap_or(0)
            + 1;
        let status = if dispute.is_some() {
            "disputed"
        } else {
            "accepted"
        };
        state
            .facts
            .entry(req.version_id.clone())
            .or_default()
            .push(LedgerFact {
                claim_id: Some(claim_id.clone()),
                subject: req.subject.clone(),
                key: req.key.clone(),
                value: req.value.clone(),
                seq,
                status: Some(status.into()),
                provenance: Some("witnessed".into()),
                // The server stores the origin and the facts read returns
                // its kind; a mock that dropped it could never exercise the
                // "is this claim the connector's own" rule.
                origin_kind: Some(req.origin.kind.clone()),
            });
        if let Some(sup) = &req.supersedes_id {
            state.supersedes.insert(claim_id.clone(), sup.clone());
        }
        let out = ProposeOutcome {
            claim_id,
            status: status.into(),
            head_seq: seq,
            findings: dispute.into_iter().collect(),
        };
        state
            .proposals
            .insert(idempotency_key.to_string(), out.clone());
        state.proposed.push(req.clone());
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use munarium_matrix_core::result::AuthorizationClass;
    use munarium_matrix_types::contract::*;

    fn manifest(bytes: &[u8], logical: &str, class: AuthorizationClass) -> EvidenceManifest {
        EvidenceManifest {
            contract_version: "0.1.0".into(),
            canon: "canon@1".into(),
            evidence_id: None,
            tenant: "acme".into(),
            kind: ArtifactKind::Table,
            logical_result_hash: logical.to_string(),
            artifact_hash: munarium_matrix_core::artifact_hash(bytes),
            bytes_len: bytes.len() as u64,
            media_type: "text/csv; charset=utf-8".into(),
            source: ManifestSource {
                source_id: "crm".into(),
                source_version: 1,
                adapter: "postgres".into(),
                adapter_version: None,
                engine: None,
                driver: None,
            },
            versions: ManifestVersions::default(),
            plan: None,
            schema: ManifestSchema { columns: vec![] },
            identity: ManifestIdentity {
                row_id_rule: munarium_matrix_core::RowIdRule::Keys,
                order_by: vec![],
                rows: 1,
            },
            completeness: ManifestCompleteness {
                truncated: false,
                declared_max_rows: None,
                rows_covered: None,
                rows_excluded: None,
                exclusion_reason: None,
            },
            redaction: ManifestRedaction::default(),
            snapshot_vector: vec![SnapshotMarker {
                source_id: "crm".into(),
                marker: None,
                isolation: None,
                started_at: None,
                ended_at: None,
                replay_level: "sealed_result".into(),
                replay_expires_at: None,
            }],
            freshness: None,
            execution: ManifestExecution {
                started_at: chrono::Utc::now(),
                ended_at: chrono::Utc::now(),
                effective_principal: None,
                statement_id: None,
            },
            authorization_class: class,
            retention: None,
        }
    }

    #[tokio::test]
    async fn sealing_the_same_logical_result_twice_returns_one_id() {
        let s = MockServer::new();
        let bytes = b"region,amount\nEMEA,1.00\n";
        let m = manifest(bytes, "sha256:aaa", AuthorizationClass::default());
        let a = s.seal_evidence(&m, bytes, None).await.unwrap();
        let b = s.seal_evidence(&m, bytes, None).await.unwrap();
        assert_eq!(
            a, b,
            "domain idempotency: same logical result, same artifact"
        );
        assert_eq!(s.evidence_count(), 1);
    }

    #[tokio::test]
    async fn a_wrong_artifact_hash_is_refused_before_it_is_stored() {
        let s = MockServer::new();
        let mut m = manifest(b"abc", "sha256:aaa", AuthorizationClass::default());
        m.artifact_hash =
            "sha256:0000000000000000000000000000000000000000000000000000000000000000".into();
        let err = s.seal_evidence(&m, b"abc", None).await.unwrap_err();
        assert!(
            matches!(err, ServerError::Problem { status: 422, .. }),
            "{err:?}"
        );
        assert_eq!(s.evidence_count(), 0);
    }

    #[tokio::test]
    async fn an_under_cleared_reader_is_denied_not_told_it_does_not_exist() {
        let s = MockServer::new();
        let bytes = b"x";
        let class = AuthorizationClass {
            name: Some("sales-emea".into()),
            access_level: 3,
            compartments: vec!["sales".into()],
        };
        let id = s
            .seal_evidence(&manifest(bytes, "sha256:bbb", class), bytes, None)
            .await
            .unwrap();

        let low = s.as_reader(1, &["sales"]);
        let err = low.get_evidence(&id).await.unwrap_err();
        assert!(
            matches!(err, ServerError::Problem { status: 403, .. }),
            "{err:?}"
        );

        let no_compartment = s.as_reader(9, &[]);
        assert!(no_compartment.get_evidence(&id).await.is_err());

        let ok = s.as_reader(3, &["sales", "extra"]);
        assert!(ok.get_evidence(&id).await.is_ok());
    }

    #[tokio::test]
    async fn re_uploading_identical_documents_stores_nothing_new() {
        let s = MockServer::new();
        let docs = vec![UploadDocument {
            path: "crm/opportunities/42.md".into(),
            bytes: b"# opportunities 42\n".to_vec(),
            media_type: "text/markdown".into(),
            metadata: vec![],
        }];
        let first = s.bulk_upload("run-1", &docs).await.unwrap();
        assert_eq!(first.stored, 1);
        assert_eq!(first.skipped_existing, 0);

        let second = s.bulk_upload("run-2", &docs).await.unwrap();
        assert_eq!(
            second.stored, 0,
            "a replayed checkpoint must upload nothing"
        );
        assert_eq!(second.skipped_existing, 1);
        assert_eq!(s.document_count(), 1);
    }

    #[tokio::test]
    async fn a_block_severity_finding_is_refused() {
        let s = MockServer::new();
        let req = FindingRequest {
            version_id: "memv-1".into(),
            rule_id: "matrix.discrepancy-candidate".into(),
            severity: "block".into(),
            message: "x".into(),
            scope_path: None,
            detail: serde_json::json!({}),
        };
        assert!(s.file_finding(&req).await.is_err(), "only a gate may block");
        let warn = FindingRequest {
            severity: "warn".into(),
            ..req
        };
        assert!(s.file_finding(&warn).await.is_ok());
        assert_eq!(s.filed_findings().len(), 1);
    }

    #[tokio::test]
    async fn facts_respect_the_pin() {
        let s = MockServer::new();
        s.seed_facts(
            "memv-1",
            vec![
                LedgerFact {
                    claim_id: None,
                    subject: "a".into(),
                    key: "k".into(),
                    value: "1".into(),
                    seq: 1,
                    status: None,
                    provenance: None,
                    origin_kind: None,
                },
                LedgerFact {
                    claim_id: None,
                    subject: "a".into(),
                    key: "k".into(),
                    value: "2".into(),
                    seq: 5,
                    status: None,
                    provenance: None,
                    origin_kind: None,
                },
            ],
        );
        assert_eq!(s.head_seq("memv-1").await.unwrap(), 5);
        assert_eq!(s.slice_facts("memv-1", Some(1)).await.unwrap().len(), 1);
        assert_eq!(s.slice_facts("memv-1", None).await.unwrap().len(), 2);
    }
}
