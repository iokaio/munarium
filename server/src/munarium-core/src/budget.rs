// SPDX-License-Identifier: Apache-2.0
//! Daily token budget ledger — the spending-cap plane.
//!
//! Ported from Matrix's budget metering (`munarium-matrix-store::budget`),
//! which paid for the two lessons this trait encodes:
//!
//! - **Reserve → work → settle-or-release.** A ceiling checked without a
//!   reservation is not atomic under READ COMMITTED: each statement takes its
//!   own snapshot, so ten concurrent requests for 2 units against a ceiling of
//!   10 granted six on Matrix before the advisory lock landed. The Postgres
//!   implementation takes `pg_advisory_xact_lock` on the `(tenant, config,
//!   tier)` scope and commits the reservation row before reporting the grant.
//! - **A lost settle leaves the budget SPENT, never free.** A crashed process
//!   may have reached the provider; refunding its reservation would let the
//!   ledger disagree with the bill in the direction nobody checks.
//!
//! Differences from the Matrix original, both deliberate: the window is the
//! **UTC day** (`date_trunc('day', ...)`), because these are daily spending
//! caps, and units are **tokens** (input + output combined), reserved at the
//! same effective-request estimate the rpm/tpm `RateBudget` uses, including
//! system text, tools, schema and the normalized output ceiling, and settled
//! to the provider's actual counts — the
//! `actual_units` argument Matrix defined and never used.
//!
//! Separate from [`crate::storage::StorageBackend`] for the same reason the
//! evidence plane is: a reservation's window is not ledger data, and any
//! report over this table must use the same window expression the enforcer
//! writes, or the report and the ceiling will disagree.

use crate::{KernelError, Result};
use serde::{Deserialize, Serialize};

/// One granted reservation — the handle `settle`/`release` act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetReservation {
    pub id: String,
    pub tenant: String,
    /// Provider config name (`demo-anthropic`, `default-openai`, …).
    pub config: String,
    /// Tier string (`fast` | `capable` | `frontier`).
    pub tier: String,
    /// The UTC day the reservation counts against, `YYYY-MM-DD`, as the
    /// store's own clock computed it.
    pub day: String,
    /// Reserved units (tokens) — the estimate until settled.
    pub units: u64,
}

/// Outcome of a reservation attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetOutcome {
    /// No cap configured for this scope — nothing was written.
    Unlimited,
    Granted(BudgetReservation),
    Exhausted {
        requested: u64,
        remaining: u64,
        limit: u64,
    },
}

/// One (config, tier) row of today's ledger — operator-facing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetLedgerRow {
    pub config: String,
    pub tier: String,
    pub day: String,
    /// Units still held (reserved, not yet settled or released).
    pub held_units: u64,
    /// Units settled (actual where the caller reported them, else estimate).
    pub settled_units: u64,
    pub reservations: u64,
}

/// Evidence for one reservation. Missing original units or usage means unknown.
/// The reservation ID identifies the evidence; settlement is first-writer-wins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetEvidence {
    pub reservation_id: String,
    pub original_units: Option<u64>,
    pub accounted_units: u64,
    pub state: String,
    pub usage: Option<crate::provider::UsageEvidence>,
    pub revision: u64,
    pub config: String,
    pub tier: String,
    pub day: String,
    pub estimator_revision: Option<String>,
}

/// An operator-attested correction, distinct from first-writer settlement.
/// IDs bind an exact request; revisions serialize concurrent late evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetCorrection {
    pub id: String,
    pub expected_revision: u64,
    pub accounted_units: u64,
    pub usage: crate::provider::UsageEvidence,
    pub evidence_ref: String,
}

impl BudgetCorrection {
    pub fn validate(&self, previous: &BudgetEvidence) -> Result<()> {
        for value in [&self.id, &self.evidence_ref] {
            if value.is_empty()
                || value.len() > 160
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            {
                return Err(KernelError::InvalidInput("budget evidence requires a safe nonempty identifier (letters, digits, dot, underscore, dash; at most 160 bytes)".into()));
            }
        }
        if previous.state != "settled" {
            return Err(KernelError::InvalidInput(
                "only a settled reservation can be reconciled".into(),
            ));
        }
        if self.expected_revision != previous.revision {
            return Err(KernelError::HeadConflict {
                expected: self.expected_revision,
                actual: previous.revision,
            });
        }
        let minimum = self.usage.accounted_units(
            previous
                .original_units
                .unwrap_or(0)
                .max(previous.accounted_units),
        )?;
        if self.accounted_units < minimum {
            return Err(KernelError::InvalidInput(
                "budget correction undercounts observed usage or unresolved liability".into(),
            ));
        }
        Ok(())
    }
}

/// Append-only evidence: corrections preserve both the prior estimate and the
/// result returned to the original caller, including on an idempotent replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetAdjustment {
    pub correction: BudgetCorrection,
    pub previous: BudgetEvidence,
    pub result: BudgetEvidence,
}

/// Estimated liability and the optional identity of the estimator that produced it.
#[derive(Debug, Clone, Copy)]
pub struct BudgetEstimate<'a> {
    pub units: u64,
    pub revision: Option<&'a str>,
}

/// Persistence for daily token budgets.
#[async_trait::async_trait]
pub trait BudgetStore: Send + Sync {
    /// Reserve `units` against the scope's daily ceiling. `limit = None`
    /// short-circuits to [`BudgetOutcome::Unlimited`] with no write. The
    /// active sum (`held` + `settled`) plus `units` must stay at or under
    /// `limit` for a grant; the check and the insert are one atomic step.
    async fn reserve(
        &self,
        tenant: &str,
        config: &str,
        tier: &str,
        units: u64,
        limit: Option<u64>,
    ) -> Result<BudgetOutcome>;

    /// Persist estimator identity atomically with the reservation when supported.
    /// Legacy callers/stores retain unknown estimator provenance.
    async fn reserve_estimated(
        &self,
        tenant: &str,
        config: &str,
        tier: &str,
        estimate: BudgetEstimate<'_>,
        limit: Option<u64>,
    ) -> Result<BudgetOutcome> {
        self.reserve(tenant, config, tier, estimate.units, limit)
            .await
    }

    /// Mark a reservation settled, correcting `units` to `actual_units` when
    /// the caller supplies an accounted amount. The argument name is retained
    /// for compatibility: an amount can also be a conservative charge for
    /// incomplete usage, not necessarily a fully observed total. `None` retains
    /// the original reservation. Idempotent: settling
    /// a non-`held` reservation is a no-op.
    async fn settle(
        &self,
        reservation: &BudgetReservation,
        actual_units: Option<u64>,
    ) -> Result<()>;

    /// Atomically settle an amount and its evidence. Legacy stores retain their
    /// existing behavior through this default; they cannot attest durable quality.
    async fn settle_with_evidence(
        &self,
        reservation: &BudgetReservation,
        accounted_units: Option<u64>,
        _usage: Option<crate::provider::UsageEvidence>,
    ) -> Result<()> {
        self.settle(reservation, accounted_units).await
    }

    /// Read tenant-scoped evidence. Unsupported legacy stores return None.
    async fn evidence(&self, _tenant: &str, _id: &str) -> Result<Option<BudgetEvidence>> {
        Ok(None)
    }

    async fn reconcile(
        &self,
        _tenant: &str,
        _id: &str,
        _correction: &BudgetCorrection,
    ) -> Result<BudgetEvidence> {
        Err(KernelError::InvalidInput(
            "budget reconciliation is not supported by this store".into(),
        ))
    }

    async fn adjustments(&self, _tenant: &str, _id: &str) -> Result<Vec<BudgetAdjustment>> {
        Err(KernelError::InvalidInput(
            "budget reconciliation is not supported by this store".into(),
        ))
    }

    /// Bounded, tenant-scoped usage evidence for one original UTC accounting day.
    /// A window over 10,000 reservations is an error, never a partial report.
    async fn evidence_for_day(&self, _tenant: &str, _day: &str) -> Result<Vec<BudgetEvidence>> {
        Err(KernelError::InvalidInput(
            "budget evidence reports are not supported by this store".into(),
        ))
    }

    /// Refund a reservation whose work never started. Idempotent like
    /// `settle`. Never call this after the provider may have been reached.
    async fn release(&self, reservation: &BudgetReservation) -> Result<()>;

    /// Stamp stale `held` reservations (older than `older_than_secs`) as
    /// settled at their reserved estimate — the crashed-process direction is
    /// spent, not free. Returns how many rows it touched.
    async fn sweep_stale(&self, older_than_secs: u64) -> Result<u64>;

    /// Today's ledger for a tenant, grouped by (config, tier), using the same
    /// window expression `reserve` writes.
    async fn ledger(&self, tenant: &str) -> Result<Vec<BudgetLedgerRow>>;
}
