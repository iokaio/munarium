// SPDX-License-Identifier: Apache-2.0
//! Promotions and proposals.
//!
//! Two tables, one rule each. `mapping_promotions` records the operator's
//! decision to let a mapping write canon — at most one active row per mapping,
//! never deleted, closed by demotion. `claim_proposals` is the idempotency
//! ledger for what Matrix has already sent, keyed by content, so a replayed
//! run proposes nothing twice and a rollback knows what to restore.

use crate::{MatrixStore, Result, StoreError};
use sqlx::Row;

#[derive(Debug, Clone, PartialEq)]
pub struct Promotion {
    pub mapping_name: String,
    pub mapping_version: i32,
    pub decision_id: String,
    pub actor: String,
    pub reason: Option<String>,
    pub identity_precision: f64,
    pub value_conformance: f64,
    pub promoted_at: String,
    pub demoted_at: Option<String>,
    pub demote_decision_id: Option<String>,
}

/// What the last completed run measured — the promotion gate's inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappingRunStats {
    pub run_id: String,
    pub state: String,
    pub observations: i64,
    pub discrepancies: i64,
    pub ambiguous: i64,
    pub findings_filed: i64,
    pub proposals: i64,
    pub nonconforming: i64,
    pub ended_at: Option<String>,
}

impl MappingRunStats {
    /// 1 − ambiguous/observations. An ambiguous identity is a resolution the
    /// mapping could not make; a mapping that cannot say WHO must not say WHAT.
    pub fn identity_precision(&self) -> f64 {
        if self.observations == 0 {
            return 0.0;
        }
        1.0 - self.ambiguous as f64 / self.observations as f64
    }

    /// 1 − nonconforming/observations: the share of values that parsed as
    /// their declared type.
    pub fn value_conformance(&self) -> f64 {
        if self.observations == 0 {
            return 0.0;
        }
        1.0 - self.nonconforming as f64 / self.observations as f64
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalRow {
    pub idempotency_key: String,
    pub mapping_ref: String,
    pub version_id: String,
    pub subject: String,
    pub property: String,
    pub value: String,
    pub claim_type: String,
    pub supersedes_id: Option<String>,
    pub prior_value: Option<String>,
    pub claim_id: String,
    pub status: String,
    pub row_key: String,
    pub evidence_id: Option<String>,
    pub rolled_back_by: Option<String>,
}

impl MatrixStore {
    pub async fn latest_mapping_run(
        &self,
        tenant: &str,
        mapping: &str,
    ) -> Result<Option<MappingRunStats>> {
        let row = sqlx::query(
            "SELECT id, state, observations, discrepancies, ambiguous, findings_filed,
                    proposals, nonconforming, ended_at::text
               FROM matrix.mapping_runs
              WHERE tenant_id = $1 AND mapping_name = $2 AND ended_at IS NOT NULL
              ORDER BY ended_at DESC LIMIT 1",
        )
        .bind(tenant)
        .bind(mapping)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(|r| MappingRunStats {
            run_id: r.get("id"),
            state: r.get("state"),
            observations: r.get("observations"),
            discrepancies: r.get("discrepancies"),
            ambiguous: r.get("ambiguous"),
            findings_filed: r.get("findings_filed"),
            proposals: r.get("proposals"),
            nonconforming: r.get("nonconforming"),
            ended_at: r.get("ended_at"),
        }))
    }

    /// Every completed run for a mapping, newest first — the gate values over
    /// TIME rather than at one decision.
    ///
    /// The promotion row records what a mapping measured at the moment it was
    /// promoted, which answers "was this decision justified?" but not "are the
    /// thresholds right?". The second question needs the series: a mapping
    /// that clears 0.95 by a hair on every run is telling you something
    /// different from one that sits at 0.999, and neither shows up in a
    /// promotion row. The owner confirmed 0.95 **with monitoring**, so the
    /// series is the deliverable, not the number.
    ///
    /// No new table: `matrix.mapping_runs` already stores the inputs
    /// (`observations`, `ambiguous`, `nonconforming`) on every completed run.
    /// The gate values are derived, so history exists for runs that predate
    /// this reader — including runs of mappings that were never promoted at
    /// all, which are exactly the interesting ones when deciding whether a
    /// threshold is too strict.
    pub async fn mapping_run_history(
        &self,
        tenant: &str,
        mapping: &str,
        limit: i64,
    ) -> Result<Vec<MappingRunStats>> {
        let rows = sqlx::query(
            "SELECT id, state, observations, discrepancies, ambiguous, findings_filed,
                    proposals, nonconforming, ended_at::text
               FROM matrix.mapping_runs
              WHERE tenant_id = $1 AND mapping_name = $2 AND ended_at IS NOT NULL
              ORDER BY ended_at DESC LIMIT $3",
        )
        .bind(tenant)
        .bind(mapping)
        .bind(limit.clamp(1, 500))
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .iter()
            .map(|r| MappingRunStats {
                run_id: r.get("id"),
                state: r.get("state"),
                observations: r.get("observations"),
                discrepancies: r.get("discrepancies"),
                ambiguous: r.get("ambiguous"),
                findings_filed: r.get("findings_filed"),
                proposals: r.get("proposals"),
                nonconforming: r.get("nonconforming"),
                ended_at: r.get("ended_at"),
            })
            .collect())
    }

    /// Record a promotion. Refuses (Conflict) when one is already active: an
    /// operator who wants to re-promote demotes first, so the history has two
    /// decisions in it, not one overwritten one.
    #[allow(clippy::too_many_arguments)]
    pub async fn promote_mapping(
        &self,
        tenant: &str,
        mapping: &str,
        mapping_version: i32,
        decision_id: &str,
        actor: &str,
        reason: Option<&str>,
        identity_precision: f64,
        value_conformance: f64,
    ) -> Result<()> {
        let r = sqlx::query(
            "INSERT INTO matrix.mapping_promotions
               (tenant_id, mapping_name, mapping_version, decision_id, actor, reason,
                identity_precision, value_conformance)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8)
             ON CONFLICT DO NOTHING",
        )
        .bind(tenant)
        .bind(mapping)
        .bind(mapping_version)
        .bind(decision_id)
        .bind(actor)
        .bind(reason)
        .bind(identity_precision)
        .bind(value_conformance)
        .execute(self.pool())
        .await;
        match r {
            Ok(res) if res.rows_affected() == 1 => Ok(()),
            Ok(_) => Err(StoreError::Conflict(format!(
                "mapping '{mapping}' already has an active promotion; demote it first"
            ))),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Err(StoreError::Conflict(
                format!("mapping '{mapping}' already has an active promotion; demote it first"),
            )),
            Err(e) => Err(e.into()),
        }
    }

    /// Close the active promotion. Returns false when there was none.
    pub async fn demote_mapping(
        &self,
        tenant: &str,
        mapping: &str,
        decision_id: &str,
    ) -> Result<bool> {
        let r = sqlx::query(
            "UPDATE matrix.mapping_promotions
                SET demoted_at = now(), demote_decision_id = $3
              WHERE tenant_id = $1 AND mapping_name = $2 AND demoted_at IS NULL",
        )
        .bind(tenant)
        .bind(mapping)
        .bind(decision_id)
        .execute(self.pool())
        .await?;
        Ok(r.rows_affected() == 1)
    }

    pub async fn active_promotion(&self, tenant: &str, mapping: &str) -> Result<Option<Promotion>> {
        let row = sqlx::query(
            "SELECT mapping_name, mapping_version, decision_id, actor, reason,
                    identity_precision, value_conformance, promoted_at::text,
                    demoted_at::text, demote_decision_id
               FROM matrix.mapping_promotions
              WHERE tenant_id = $1 AND mapping_name = $2 AND demoted_at IS NULL",
        )
        .bind(tenant)
        .bind(mapping)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(|r| Promotion {
            mapping_name: r.get("mapping_name"),
            mapping_version: r.get("mapping_version"),
            decision_id: r.get("decision_id"),
            actor: r.get("actor"),
            reason: r.get("reason"),
            identity_precision: r.get("identity_precision"),
            value_conformance: r.get("value_conformance"),
            promoted_at: r.get("promoted_at"),
            demoted_at: r.get("demoted_at"),
            demote_decision_id: r.get("demote_decision_id"),
        }))
    }

    pub async fn proposal_seen(&self, tenant: &str, key: &str) -> Result<Option<String>> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT claim_id FROM matrix.claim_proposals WHERE tenant_id = $1 AND idempotency_key = $2",
        )
        .bind(tenant)
        .bind(key)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(|r| r.0))
    }

    pub async fn record_proposal(&self, tenant: &str, p: &ProposalRow) -> Result<()> {
        sqlx::query(
            "INSERT INTO matrix.claim_proposals
               (tenant_id, idempotency_key, mapping_ref, version_id, subject, property, value,
                claim_type, supersedes_id, prior_value, claim_id, status, row_key, evidence_id)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)
             ON CONFLICT (tenant_id, idempotency_key) DO NOTHING",
        )
        .bind(tenant)
        .bind(&p.idempotency_key)
        .bind(&p.mapping_ref)
        .bind(&p.version_id)
        .bind(&p.subject)
        .bind(&p.property)
        .bind(&p.value)
        .bind(&p.claim_type)
        .bind(&p.supersedes_id)
        .bind(&p.prior_value)
        .bind(&p.claim_id)
        .bind(&p.status)
        .bind(&p.row_key)
        .bind(&p.evidence_id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// A mapping's proposals, oldest first — the input to a rollback.
    pub async fn proposals_for_mapping(
        &self,
        tenant: &str,
        mapping_ref: &str,
    ) -> Result<Vec<ProposalRow>> {
        let rows = sqlx::query(
            "SELECT idempotency_key, mapping_ref, version_id, subject, property, value,
                    claim_type, supersedes_id, prior_value, claim_id, status, row_key,
                    evidence_id, rolled_back_by
               FROM matrix.claim_proposals
              WHERE tenant_id = $1 AND mapping_ref = $2
              ORDER BY proposed_at, idempotency_key",
        )
        .bind(tenant)
        .bind(mapping_ref)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| ProposalRow {
                idempotency_key: r.get("idempotency_key"),
                mapping_ref: r.get("mapping_ref"),
                version_id: r.get("version_id"),
                subject: r.get("subject"),
                property: r.get("property"),
                value: r.get("value"),
                claim_type: r.get("claim_type"),
                supersedes_id: r.get("supersedes_id"),
                prior_value: r.get("prior_value"),
                claim_id: r.get("claim_id"),
                status: r.get("status"),
                row_key: r.get("row_key"),
                evidence_id: r.get("evidence_id"),
                rolled_back_by: r.get("rolled_back_by"),
            })
            .collect())
    }

    pub async fn mark_rolled_back(
        &self,
        tenant: &str,
        idempotency_key: &str,
        rollback_claim_id: &str,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE matrix.claim_proposals SET rolled_back_by = $3
              WHERE tenant_id = $1 AND idempotency_key = $2",
        )
        .bind(tenant)
        .bind(idempotency_key)
        .bind(rollback_claim_id)
        .execute(self.pool())
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(observations: i64, ambiguous: i64, nonconforming: i64) -> MappingRunStats {
        MappingRunStats {
            run_id: "r1".into(),
            state: "completed".into(),
            observations,
            discrepancies: 0,
            ambiguous,
            findings_filed: 0,
            proposals: 0,
            nonconforming,
            ended_at: Some("2026-08-28T12:00:00Z".into()),
        }
    }

    #[test]
    fn the_gate_ratios_are_one_minus_the_failure_share() {
        let s = stats(100, 5, 1);
        assert!((s.identity_precision() - 0.95).abs() < 1e-9);
        assert!((s.value_conformance() - 0.99).abs() < 1e-9);
    }

    #[test]
    fn a_run_that_observed_nothing_scores_zero_not_one() {
        // The tempting bug: 1 - 0/0. An empty run has no evidence that the
        // mapping is safe, so it must NOT clear a gate by vacuous perfection —
        // "nothing went wrong" and "nothing happened" are different claims.
        let s = stats(0, 0, 0);
        assert_eq!(s.identity_precision(), 0.0);
        assert_eq!(s.value_conformance(), 0.0);
    }

    #[test]
    fn the_default_thresholds_sit_exactly_at_a_representable_boundary() {
        // 0.95 confirmed by the owner (Q8, 2026-08-28). This pins the boundary
        // case that a >= comparison must admit: exactly 5 ambiguous in 100 is
        // a PASS, not a near-miss, and float arithmetic must not turn it into
        // one. If this ever fails, the gate silently became stricter than the
        // number everyone agreed to.
        let s = stats(100, 5, 1);
        assert!(s.identity_precision() >= 0.95, "{}", s.identity_precision());
        assert!(s.value_conformance() >= 0.99, "{}", s.value_conformance());

        // And one more ambiguous row is a genuine fail.
        let s = stats(100, 6, 1);
        assert!(s.identity_precision() < 0.95);
    }
}
