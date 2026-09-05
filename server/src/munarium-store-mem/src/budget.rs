// SPDX-License-Identifier: Apache-2.0
//! `MemBudgetStore` — in-memory daily token budgets.
//!
//! Semantics contract: identical to `munarium-store-pg`'s `PgBudgetStore`;
//! the store-parity tests run the same scenarios against both. The whole
//! store is one mutex, so the reserve check-and-insert is trivially atomic —
//! the property the Postgres side needs an advisory lock to get.

use async_trait::async_trait;
use munarium_core::budget::{BudgetLedgerRow, BudgetOutcome, BudgetReservation, BudgetStore};
use munarium_core::Result;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

#[derive(Debug, Clone)]
struct Row {
    id: String,
    tenant: String,
    config: String,
    tier: String,
    day: String,
    units: u64,
    state: RowState,
    created_unix: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowState {
    Held,
    Settled,
    Released,
}

#[derive(Default)]
pub struct MemBudgetStore {
    rows: Mutex<Vec<Row>>,
}

impl MemBudgetStore {
    pub fn new() -> Self {
        Self::default()
    }
}

fn today_utc() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[async_trait]
impl BudgetStore for MemBudgetStore {
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
        let mut rows = self.rows.lock().await;
        let day = today_utc();
        let active: u64 = rows
            .iter()
            .filter(|r| {
                r.tenant == tenant
                    && r.config == config
                    && r.tier == tier
                    && r.day == day
                    && r.state != RowState::Released
            })
            .map(|r| r.units)
            .sum();
        if active + units > limit {
            return Ok(BudgetOutcome::Exhausted {
                requested: units,
                remaining: limit.saturating_sub(active),
                limit,
            });
        }
        let reservation = BudgetReservation {
            id: uuid::Uuid::new_v4().simple().to_string(),
            tenant: tenant.to_string(),
            config: config.to_string(),
            tier: tier.to_string(),
            day: day.clone(),
            units,
        };
        rows.push(Row {
            id: reservation.id.clone(),
            tenant: tenant.to_string(),
            config: config.to_string(),
            tier: tier.to_string(),
            day,
            units,
            state: RowState::Held,
            created_unix: now_unix(),
        });
        Ok(BudgetOutcome::Granted(reservation))
    }

    async fn settle(
        &self,
        reservation: &BudgetReservation,
        actual_units: Option<u64>,
    ) -> Result<()> {
        let mut rows = self.rows.lock().await;
        if let Some(row) = rows
            .iter_mut()
            .find(|r| r.id == reservation.id && r.state == RowState::Held)
        {
            row.state = RowState::Settled;
            if let Some(actual) = actual_units {
                row.units = actual;
            }
        }
        Ok(())
    }

    async fn release(&self, reservation: &BudgetReservation) -> Result<()> {
        let mut rows = self.rows.lock().await;
        if let Some(row) = rows
            .iter_mut()
            .find(|r| r.id == reservation.id && r.state == RowState::Held)
        {
            row.state = RowState::Released;
        }
        Ok(())
    }

    async fn sweep_stale(&self, older_than_secs: u64) -> Result<u64> {
        let cutoff = now_unix().saturating_sub(older_than_secs);
        let mut rows = self.rows.lock().await;
        let mut swept = 0;
        // `<=`, not `<`: this clock is whole seconds, so a row created this
        // second must still count as reaching a zero threshold — the Postgres
        // store gets the same inclusivity for free from microsecond precision.
        for row in rows
            .iter_mut()
            .filter(|r| r.state == RowState::Held && r.created_unix <= cutoff)
        {
            row.state = RowState::Settled;
            swept += 1;
        }
        Ok(swept)
    }

    async fn ledger(&self, tenant: &str) -> Result<Vec<BudgetLedgerRow>> {
        let rows = self.rows.lock().await;
        let day = today_utc();
        let mut grouped: std::collections::BTreeMap<(String, String), BudgetLedgerRow> =
            std::collections::BTreeMap::new();
        for r in rows
            .iter()
            .filter(|r| r.tenant == tenant && r.day == day && r.state != RowState::Released)
        {
            let entry = grouped
                .entry((r.config.clone(), r.tier.clone()))
                .or_insert_with(|| BudgetLedgerRow {
                    config: r.config.clone(),
                    tier: r.tier.clone(),
                    day: day.clone(),
                    held_units: 0,
                    settled_units: 0,
                    reservations: 0,
                });
            match r.state {
                RowState::Held => entry.held_units += r.units,
                RowState::Settled => entry.settled_units += r.units,
                RowState::Released => unreachable!("released rows are filtered above"),
            }
            entry.reservations += 1;
        }
        Ok(grouped.into_values().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unlimited_scope_writes_nothing() {
        let store = MemBudgetStore::new();
        let out = store
            .reserve("t", "cfg", "frontier", 100, None)
            .await
            .unwrap();
        assert_eq!(out, BudgetOutcome::Unlimited);
        assert!(store.ledger("t").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn reserve_grants_until_the_ceiling_then_refuses_with_remaining() {
        let store = MemBudgetStore::new();
        let first = store
            .reserve("t", "cfg", "frontier", 600, Some(1000))
            .await
            .unwrap();
        assert!(matches!(first, BudgetOutcome::Granted(_)));
        match store
            .reserve("t", "cfg", "frontier", 600, Some(1000))
            .await
            .unwrap()
        {
            BudgetOutcome::Exhausted {
                requested,
                remaining,
                limit,
            } => {
                assert_eq!(requested, 600);
                assert_eq!(remaining, 400);
                assert_eq!(limit, 1000);
            }
            other => panic!("expected Exhausted, got {other:?}"),
        }
        // A different tier of the same config is its own scope.
        assert!(matches!(
            store
                .reserve("t", "cfg", "fast", 600, Some(1000))
                .await
                .unwrap(),
            BudgetOutcome::Granted(_)
        ));
    }

    #[tokio::test]
    async fn settle_corrects_to_actuals_and_release_refunds() {
        let store = MemBudgetStore::new();
        let BudgetOutcome::Granted(r1) = store
            .reserve("t", "cfg", "capable", 900, Some(1000))
            .await
            .unwrap()
        else {
            panic!("expected grant");
        };
        // Actuals were far under the estimate: settling frees the headroom.
        store.settle(&r1, Some(100)).await.unwrap();
        assert!(matches!(
            store
                .reserve("t", "cfg", "capable", 800, Some(1000))
                .await
                .unwrap(),
            BudgetOutcome::Granted(_)
        ));
        // Release refunds entirely.
        let BudgetOutcome::Granted(r2) = store
            .reserve("t2", "cfg", "capable", 1000, Some(1000))
            .await
            .unwrap()
        else {
            panic!("expected grant");
        };
        store.release(&r2).await.unwrap();
        assert!(matches!(
            store
                .reserve("t2", "cfg", "capable", 1000, Some(1000))
                .await
                .unwrap(),
            BudgetOutcome::Granted(_)
        ));
        // Settle after release is a no-op, not a resurrection.
        store.settle(&r2, Some(50)).await.unwrap();
        assert!(store
            .ledger("t2")
            .await
            .unwrap()
            .iter()
            .all(|row| row.held_units + row.settled_units <= 1000));
    }

    #[tokio::test]
    async fn sweep_stamps_stale_held_spent_never_free() {
        let store = MemBudgetStore::new();
        let BudgetOutcome::Granted(_) = store
            .reserve("t", "cfg", "frontier", 700, Some(1000))
            .await
            .unwrap()
        else {
            panic!("expected grant");
        };
        // Nothing is stale yet.
        assert_eq!(store.sweep_stale(3600).await.unwrap(), 0);
        // With a zero threshold the held row is stale NOW; it settles at its
        // estimate and still counts against the ceiling.
        assert_eq!(store.sweep_stale(0).await.unwrap(), 1);
        match store
            .reserve("t", "cfg", "frontier", 400, Some(1000))
            .await
            .unwrap()
        {
            BudgetOutcome::Exhausted { remaining, .. } => assert_eq!(remaining, 300),
            other => panic!("expected Exhausted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn ledger_groups_by_config_and_tier() {
        let store = MemBudgetStore::new();
        for (tier, units) in [("fast", 10), ("fast", 20), ("frontier", 5)] {
            let BudgetOutcome::Granted(r) = store
                .reserve("t", "cfg", tier, units, Some(1000))
                .await
                .unwrap()
            else {
                panic!("expected grant");
            };
            if tier == "fast" {
                store.settle(&r, None).await.unwrap();
            }
        }
        let rows = store.ledger("t").await.unwrap();
        assert_eq!(rows.len(), 2);
        let fast = rows.iter().find(|r| r.tier == "fast").expect("fast row");
        assert_eq!(fast.settled_units, 30);
        assert_eq!(fast.held_units, 0);
        assert_eq!(fast.reservations, 2);
        let frontier = rows
            .iter()
            .find(|r| r.tier == "frontier")
            .expect("frontier row");
        assert_eq!(frontier.held_units, 5);
        assert_eq!(frontier.settled_units, 0);
    }
}
