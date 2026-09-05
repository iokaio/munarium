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
//! same estimate the rpm/tpm `RateBudget` already uses (`prompt/4 +
//! max_tokens`) and settled to the provider's actual counts — the
//! `actual_units` argument Matrix defined and never used.
//!
//! Separate from [`crate::storage::StorageBackend`] for the same reason the
//! evidence plane is: a reservation's window is not ledger data, and any
//! report over this table must use the same window expression the enforcer
//! writes, or the report and the ceiling will disagree.

use crate::Result;

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

    /// Mark a reservation settled, correcting `units` to `actual_units` when
    /// the caller can report what the work really cost. Idempotent: settling
    /// a non-`held` reservation is a no-op.
    async fn settle(
        &self,
        reservation: &BudgetReservation,
        actual_units: Option<u64>,
    ) -> Result<()>;

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
