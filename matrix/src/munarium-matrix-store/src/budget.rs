// SPDX-License-Identifier: Apache-2.0
//! The budget ledger — a **reservation** table, not a counter.
//!
//! The naive version is `SELECT sum(...)` then `INSERT`, and it is wrong: two
//! concurrent executions both read a total under the ceiling and both proceed,
//! so a budget of 100 admits 150. Here the insert and the check happen in one
//! statement, and the row is written *before* the work runs. A reservation is
//! then settled (kept) or released (rolled back) — so a refused execution does
//! not spend budget, and a crashed one spends it until the sweep reclaims it,
//! which is the safe direction to fail.

use crate::{new_id, MatrixStore, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reservation {
    pub id: String,
    pub units: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetOutcome {
    /// The units are held; call `settle` or `release` when the work ends.
    Granted(Reservation),
    /// The ceiling was already reached. Carries what was asked and what is
    /// left, so the refusal message can be specific.
    Exhausted {
        requested: i64,
        remaining: i64,
        limit: i64,
    },
    /// No ceiling configured for this source.
    Unlimited,
}

impl MatrixStore {
    /// Reserve `units` against `source`'s hourly budget.
    ///
    /// **Why the advisory lock.** The obvious implementation — one
    /// `INSERT ... SELECT ... WHERE (aggregate) + n <= limit` — looks atomic
    /// and is not. Under READ COMMITTED, which is Postgres's default and
    /// therefore what this runs as, each statement takes its own snapshot and
    /// cannot see rows other transactions have inserted but not committed. Ten
    /// concurrent callers each read a total that excludes the other nine, and
    /// every one of them passes a check that only one should.
    ///
    /// That is not theoretical. With a ceiling of 10 and ten concurrent
    /// requests for 2 units, the single-statement form granted **six** — 12
    /// units against a limit of 10. It was found on 2026-08-28, by a
    /// conformance scenario that had been silently skipping because its setup
    /// swallowed a connection failure and returned "no database" instead.
    ///
    /// So the check and the insert are serialized per `(tenant, source)` by a
    /// transaction-scoped advisory lock. The lock is released by COMMIT or
    /// ROLLBACK — including a client that dies mid-transaction — so it cannot
    /// leak. Contention is per source per caller, which is exactly the resource
    /// being rationed; two different sources never wait on each other.
    pub async fn reserve_budget(
        &self,
        tenant: &str,
        source: &str,
        units: i64,
        limit_per_hour: Option<u64>,
    ) -> Result<BudgetOutcome> {
        let Some(limit) = limit_per_hour else {
            return Ok(BudgetOutcome::Unlimited);
        };
        let limit = limit as i64;
        let id = new_id("bdg");

        let mut tx = self.pool().begin().await?;

        // `hashtextextended` over the exact pair, so the lock's scope is the
        // budget's scope. A collision between two unrelated sources costs a
        // little serialization and is never incorrect.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1 || '/' || $2, 0))")
            .bind(tenant)
            .bind(source)
            .execute(&mut *tx)
            .await?;

        let inserted: Option<(String,)> = sqlx::query_as(
            "INSERT INTO matrix.budget_reservations
               (id, tenant_id, source_name, window_start, units, state)
             SELECT $1, $2, $3, date_trunc('hour', now()), $4, 'held'
              WHERE COALESCE((
                    SELECT sum(units) FROM matrix.budget_reservations
                     WHERE tenant_id = $2 AND source_name = $3
                       AND window_start = date_trunc('hour', now())
                       AND state <> 'released'), 0) + $4 <= $5
             RETURNING id",
        )
        .bind(&id)
        .bind(tenant)
        .bind(source)
        .bind(units)
        .bind(limit)
        .fetch_optional(&mut *tx)
        .await?;

        // Commit BEFORE reporting the outcome: a granted reservation that is
        // still uncommitted is invisible to the next caller, which is the whole
        // bug this lock exists to fix.
        tx.commit().await?;

        match inserted {
            Some((id,)) => Ok(BudgetOutcome::Granted(Reservation { id, units })),
            None => {
                // `sum(bigint)` is NUMERIC in Postgres, so the cast is not
                // decoration: without it the decode fails at runtime, and only
                // on a path that a happy-path test never reaches. Found on the
                // first live cycle (2026-08-28).
                let used: (Option<i64>,) = sqlx::query_as(
                    "SELECT sum(units)::bigint FROM matrix.budget_reservations
                      WHERE tenant_id = $1 AND source_name = $2
                        AND window_start = date_trunc('hour', now()) AND state <> 'released'",
                )
                .bind(tenant)
                .bind(source)
                .fetch_one(self.pool())
                .await?;
                let used = used.0.unwrap_or(0);
                Ok(BudgetOutcome::Exhausted {
                    requested: units,
                    remaining: (limit - used).max(0),
                    limit,
                })
            }
        }
    }

    /// Keep the reservation, optionally correcting the units to what the work
    /// actually cost.
    pub async fn settle_budget(
        &self,
        reservation: &Reservation,
        actual_units: Option<i64>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE matrix.budget_reservations
                SET state = 'settled', settled_at = now(), units = COALESCE($2, units)
              WHERE id = $1",
        )
        .bind(&reservation.id)
        .bind(actual_units)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Give the units back — the work did not happen (a refusal before the
    /// source was touched, a cancelled statement).
    pub async fn release_budget(&self, reservation: &Reservation) -> Result<()> {
        sqlx::query(
            "UPDATE matrix.budget_reservations
                SET state = 'released', settled_at = now() WHERE id = $1",
        )
        .bind(&reservation.id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Reclaim reservations that were never settled — the crash case. Held
    /// rows older than the window cannot belong to live work.
    pub async fn sweep_stale_reservations(&self, older_than_hours: i64) -> Result<u64> {
        let r = sqlx::query(
            "UPDATE matrix.budget_reservations
                SET state = 'released', settled_at = now()
              WHERE state = 'held' AND created_at < now() - make_interval(hours => $1)",
        )
        .bind(older_than_hours as i32)
        .execute(self.pool())
        .await?;
        Ok(r.rows_affected())
    }
}
