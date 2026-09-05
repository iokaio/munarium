// SPDX-License-Identifier: Apache-2.0
//! Rollback: undo a mapping's proposals WITHOUT rewriting
//! history.
//!
//! The ledger is append-only, so "undo" means: for every claim this mapping
//! proposed, propose a correction that restores the value the ledger held
//! before, superseding ours, with `origin.kind = "rollback"`. The original
//! proposal stays in the lineage, disputed by nothing and superseded by
//! something — exactly what happened, in the order it happened.
//!
//! A proposal that filled a gap (`prior_value = None`, the ledger had no
//! claim) has nothing to restore. It is reported as skipped, not superseded
//! with an empty value: inventing a value to roll back TO would be a second
//! mistake on top of the first.

use crate::reconcile::{ProposalLedger, ProposalRecord};
use munarium_matrix_core::Refusal;
use munarium_matrix_server_client::{ClaimOriginWire, ProposeClaimRequest, ServerClient};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RollbackOutcome {
    /// Corrections filed: one per (lineage, subject, property) CHAIN.
    pub superseded: u64,
    /// Proposals those corrections cover — every link of every chain.
    pub proposals_covered: u64,
    /// Proposals whose prior value was absent: nothing to restore.
    pub skipped_no_prior: u64,
    /// Proposals already rolled back by an earlier run.
    pub already_rolled_back: u64,
    /// Proposals the server recorded DISPUTED on the way back — a gate
    /// objected to the restoration. Recorded, surfaced, never dropped.
    pub disputed: u64,
    /// (original proposal key, rollback claim id) for EVERY proposal covered,
    /// so the caller can mark each one rolled back.
    pub items: Vec<(String, String)>,
}

pub struct RollbackRequest<'a> {
    pub tenant: &'a str,
    pub source_id: &'a str,
    pub mapping_ref: &'a str,
    pub decision_id: &'a str,
    pub proposals: &'a [ProposalRecord],
    pub ledger: &'a dyn ProposalLedger,
}

/// A rollback's own idempotency key: the proposal it undoes, plus the
/// decision that ordered it. Two operators rolling back the same proposal
/// under the same decision send one correction.
pub fn rollback_key(proposal_key: &str, decision_id: &str) -> String {
    munarium_matrix_core::artifact_hash(format!("rollback|{proposal_key}|{decision_id}").as_bytes())
}

/// What makes two proposals the same chain: the lineage, the subject and the
/// property they wrote.
type ChainKey = (String, String, String);

pub async fn rollback(
    server: &dyn ServerClient,
    req: &RollbackRequest<'_>,
) -> Result<RollbackOutcome, Refusal> {
    let mut out = RollbackOutcome::default();

    // One correction per CHAIN, not per proposal.
    //
    // A mapping that wrote the same property twice — 90000 → 90500 in one
    // pass, 90500 → 91000 in the next — holds two proposals whose priors
    // chain. Undoing each one separately, oldest first, files a correction
    // restoring 90000 that supersedes a claim already superseded, then a
    // correction restoring 90500 that supersedes the head: the ledger ends
    // with TWO unsuperseded claims for one key (the server's `resolve_slice`
    // returns every unsuperseded claim) and the value at head is the
    // connector's own first write, not the document's. Found by reading the
    // server's resolution rule, 2026-08-29; the single-proposal scenario could
    // not show it.
    //
    // So: group by (lineage, subject, property) in proposal order, restore the
    // OLDEST prior, supersede the NEWEST claim, and record every proposal in
    // the chain as covered by that one correction.
    let mut chains: Vec<(ChainKey, Vec<&ProposalRecord>)> = Vec::new();
    for p in req.proposals {
        if p.status != "accepted" {
            // A disputed proposal never became canon; there is nothing to undo.
            continue;
        }
        let k = (p.version_id.clone(), p.subject.clone(), p.property.clone());
        match chains.iter_mut().find(|(ck, _)| *ck == k) {
            Some((_, v)) => v.push(p),
            None => chains.push((k, vec![p])),
        }
    }

    for (_, chain) in chains {
        let first = chain[0];
        let last = chain[chain.len() - 1];
        let Some(prior) = &first.prior_value else {
            // The chain began by filling a gap: the ledger had no value before
            // this mapping wrote one, so there is nothing to restore. Inventing
            // a value to roll back TO would be a second mistake on top of the
            // first. Reported, never silently skipped.
            out.skipped_no_prior += chain.len() as u64;
            continue;
        };
        // Keyed on the proposal the correction supersedes — the head — plus
        // the decision, so two operators rolling back the same chain under one
        // decision send one correction, and a single-proposal chain keys
        // exactly as it always did.
        let key = rollback_key(&last.idempotency_key, req.decision_id);
        if req.ledger.seen(req.tenant, &key).await?.is_some() {
            out.already_rolled_back += chain.len() as u64;
            continue;
        }
        let rolled_back: Vec<&str> = chain.iter().map(|p| p.claim_id.as_str()).collect();
        let outcome = server
            .propose_claim(
                &ProposeClaimRequest {
                    version_id: last.version_id.clone(),
                    claim_type: "correction".into(),
                    subject: last.subject.clone(),
                    key: last.property.clone(),
                    value: prior.clone(),
                    scope_path: None,
                    supersedes_id: Some(last.claim_id.clone()),
                    evidence: Some(serde_json::json!({
                        "rollback_of": last.claim_id,
                        "rolled_back": rolled_back,
                        "decision_id": req.decision_id,
                        "restored_value": prior,
                    })),
                    origin: ClaimOriginWire {
                        kind: "rollback".into(),
                        source_id: req.source_id.to_string(),
                        mapping_version: req.mapping_ref.to_string(),
                        row_key: last.row_key.clone(),
                        event_position: None,
                        observed_at: None,
                        evidence_id: last.evidence_id.clone(),
                    },
                },
                &key,
            )
            .await
            .map_err(|e| e.to_refusal())?;
        out.superseded += 1;
        out.proposals_covered += chain.len() as u64;
        if outcome.status == "disputed" {
            out.disputed += 1;
        }
        for p in &chain {
            out.items
                .push((p.idempotency_key.clone(), outcome.claim_id.clone()));
        }
        req.ledger
            .record(
                req.tenant,
                &ProposalRecord {
                    idempotency_key: key,
                    mapping: req.mapping_ref.to_string(),
                    version_id: last.version_id.clone(),
                    subject: last.subject.clone(),
                    property: last.property.clone(),
                    value: prior.clone(),
                    claim_type: "correction".into(),
                    supersedes_id: Some(last.claim_id.clone()),
                    prior_value: Some(last.value.clone()),
                    claim_id: outcome.claim_id,
                    status: outcome.status,
                    row_key: last.row_key.clone(),
                    evidence_id: last.evidence_id.clone(),
                },
            )
            .await?;
    }
    Ok(out)
}
