// SPDX-License-Identifier: Apache-2.0
//! PostgreSQL daily token budgets (migration `0029_token_budgets`).
//!
//! Semantics contract: identical to `munarium-store-mem`'s — the store-parity
//! tests run the same scenarios against both.
//!
//! The reserve path is the Matrix budget mechanism verbatim, window renamed
//! from hour to UTC day:
//!
//! - A single `INSERT ... SELECT ... WHERE (SELECT sum(units)) + n <= limit`
//!   LOOKS atomic and is not: under READ COMMITTED each statement takes its
//!   own snapshot and cannot see other transactions' uncommitted inserts.
//!   Matrix measured the failure — limit 10, ten concurrent requests for 2
//!   units, six granted. The fix is `pg_advisory_xact_lock` on the
//!   `(tenant, config, tier)` scope at the top of the transaction:
//!   transaction-scoped, so COMMIT/ROLLBACK/client-death all release it.
//! - The transaction COMMITS before the outcome is reported — an uncommitted
//!   grant is invisible to the next caller, which is the race the lock exists
//!   to close.
//! - `sum(bigint)` is NUMERIC in Postgres; the explicit `::bigint` cast is
//!   load-bearing on the refusal path that happy-path tests never reach.

use async_trait::async_trait;
use munarium_core::budget::{BudgetLedgerRow, BudgetOutcome, BudgetReservation, BudgetStore};
use munarium_core::Result;
use sqlx::{PgPool, Row};

use crate::storage_err;

#[derive(Clone)]
pub struct PgBudgetStore {
    pool: PgPool,
}

impl PgBudgetStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Active (non-released) units for a scope's current UTC day. Every reader
/// and the enforcer share this one expression for the window, so the report
/// can never disagree with the ceiling about which day it is.
const ACTIVE_SUM: &str = "SELECT COALESCE(SUM(units), 0)::bigint FROM token_budget_reservations
     WHERE tenant_id = $1 AND config_name = $2 AND tier = $3
       AND day = (now() AT TIME ZONE 'utc')::date
       AND state <> 'released'";

#[async_trait]
impl BudgetStore for PgBudgetStore {
    async fn reserve(
        &self,
        tenant: &str,
        config: &str,
        tier: &str,
        units: u64,
        limit: Option<u64>,
    ) -> Result<BudgetOutcome> {
        let Some(limit) = limit else {
            return Ok(BudgetOutcome::Unlimited);
        };
        let mut tx = self.pool.begin().await.map_err(storage_err)?;
        // Serialize per scope; the scope IS the rationed resource, so this is
        // exactly the contention we want and no more.
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended($1 || '/' || $2 || '/' || $3, 0))",
        )
        .bind(tenant)
        .bind(config)
        .bind(tier)
        .execute(&mut *tx)
        .await
        .map_err(storage_err)?;
        let active: i64 = sqlx::query_scalar(ACTIVE_SUM)
            .bind(tenant)
            .bind(config)
            .bind(tier)
            .fetch_one(&mut *tx)
            .await
            .map_err(storage_err)?;
        let active = active.max(0) as u64;
        if active + units > limit {
            // Nothing was written; commit only ends the lock's transaction.
            tx.commit().await.map_err(storage_err)?;
            return Ok(BudgetOutcome::Exhausted {
                requested: units,
                remaining: limit.saturating_sub(active),
                limit,
            });
        }
        let id = uuid::Uuid::new_v4().simple().to_string();
        let day: String = sqlx::query_scalar(
            "INSERT INTO token_budget_reservations
                (id, tenant_id, config_name, tier, day, units, state)
             VALUES ($1, $2, $3, $4, (now() AT TIME ZONE 'utc')::date, $5, 'held')
             RETURNING day::text",
        )
        .bind(&id)
        .bind(tenant)
        .bind(config)
        .bind(tier)
        .bind(units as i64)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage_err)?;
        // Commit BEFORE reporting the grant: an uncommitted reservation is
        // invisible to the next caller's sum.
        tx.commit().await.map_err(storage_err)?;
        Ok(BudgetOutcome::Granted(BudgetReservation {
            id,
            tenant: tenant.to_string(),
            config: config.to_string(),
            tier: tier.to_string(),
            day,
            units,
        }))
    }

    async fn settle(
        &self,
        reservation: &BudgetReservation,
        actual_units: Option<u64>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE token_budget_reservations
             SET state = 'settled',
                 units = COALESCE($2, units),
                 settled_at = now()
             WHERE id = $1 AND state = 'held'",
        )
        .bind(&reservation.id)
        .bind(actual_units.map(|u| u as i64))
        .execute(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(())
    }

    async fn release(&self, reservation: &BudgetReservation) -> Result<()> {
        sqlx::query(
            "UPDATE token_budget_reservations
             SET state = 'released', settled_at = now()
             WHERE id = $1 AND state = 'held'",
        )
        .bind(&reservation.id)
        .execute(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(())
    }

    async fn sweep_stale(&self, older_than_secs: u64) -> Result<u64> {
        // Spent, not free: a crashed holder may have reached the provider, so
        // its estimate stands as the settled amount.
        let result = sqlx::query(
            "UPDATE token_budget_reservations
             SET state = 'settled', settled_at = now()
             WHERE state = 'held' AND created_at < now() - make_interval(secs => $1)",
        )
        .bind(older_than_secs as f64)
        .execute(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(result.rows_affected())
    }

    async fn ledger(&self, tenant: &str) -> Result<Vec<BudgetLedgerRow>> {
        let rows = sqlx::query(
            "SELECT config_name, tier, day::text AS day,
                    COALESCE(SUM(units) FILTER (WHERE state = 'held'), 0)::bigint AS held_units,
                    COALESCE(SUM(units) FILTER (WHERE state = 'settled'), 0)::bigint AS settled_units,
                    COUNT(*)::bigint AS reservations
             FROM token_budget_reservations
             WHERE tenant_id = $1
               AND day = (now() AT TIME ZONE 'utc')::date
               AND state <> 'released'
             GROUP BY config_name, tier, day
             ORDER BY config_name, tier",
        )
        .bind(tenant)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(rows
            .iter()
            .map(|r| BudgetLedgerRow {
                config: r.get("config_name"),
                tier: r.get("tier"),
                day: r.get("day"),
                held_units: r.get::<i64, _>("held_units").max(0) as u64,
                settled_units: r.get::<i64, _>("settled_units").max(0) as u64,
                reservations: r.get::<i64, _>("reservations").max(0) as u64,
            })
            .collect())
    }
}
