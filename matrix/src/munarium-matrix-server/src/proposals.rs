// SPDX-License-Identifier: Apache-2.0
//! The proposal ledger over the Matrix store.
//!
//! `munarium-matrix-workers` defines the `ProposalLedger` trait and does not
//! depend on the store; this is the one implementation the binary uses, so
//! the workers stay testable against an in-memory ledger.

use crate::state::AppState;
use munarium_matrix_core::Refusal;
use munarium_matrix_store::ProposalRow;
use munarium_matrix_workers::{ProposalLedger, ProposalRecord};

pub struct StoreLedger<'a> {
    pub state: &'a AppState,
}

fn unavailable(e: impl std::fmt::Display) -> Refusal {
    Refusal::source_unavailable(format!("proposal ledger: {e}"))
}

#[async_trait::async_trait]
impl ProposalLedger for StoreLedger<'_> {
    async fn seen(&self, tenant: &str, idempotency_key: &str) -> Result<Option<String>, Refusal> {
        self.state
            .store
            .proposal_seen(tenant, idempotency_key)
            .await
            .map_err(unavailable)
    }

    async fn record(&self, tenant: &str, rec: &ProposalRecord) -> Result<(), Refusal> {
        self.state
            .store
            .record_proposal(
                tenant,
                &ProposalRow {
                    idempotency_key: rec.idempotency_key.clone(),
                    mapping_ref: rec.mapping.clone(),
                    version_id: rec.version_id.clone(),
                    subject: rec.subject.clone(),
                    property: rec.property.clone(),
                    value: rec.value.clone(),
                    claim_type: rec.claim_type.clone(),
                    supersedes_id: rec.supersedes_id.clone(),
                    prior_value: rec.prior_value.clone(),
                    claim_id: rec.claim_id.clone(),
                    status: rec.status.clone(),
                    row_key: rec.row_key.clone(),
                    evidence_id: rec.evidence_id.clone(),
                    rolled_back_by: None,
                },
            )
            .await
            .map_err(unavailable)
    }
}

/// A store row as the workers' record shape — the rollback path's input.
pub fn to_record(p: &ProposalRow) -> ProposalRecord {
    ProposalRecord {
        idempotency_key: p.idempotency_key.clone(),
        mapping: p.mapping_ref.clone(),
        version_id: p.version_id.clone(),
        subject: p.subject.clone(),
        property: p.property.clone(),
        value: p.value.clone(),
        claim_type: p.claim_type.clone(),
        supersedes_id: p.supersedes_id.clone(),
        prior_value: p.prior_value.clone(),
        claim_id: p.claim_id.clone(),
        status: p.status.clone(),
        row_key: p.row_key.clone(),
        evidence_id: p.evidence_id.clone(),
    }
}
