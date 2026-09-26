// SPDX-License-Identifier: Apache-2.0
//! `MemBudgetStore` — in-memory daily token budgets.
//!
//! Semantics contract: identical to `munarium-store-pg`'s `PgBudgetStore`;
//! the store-parity tests run the same scenarios against both. The whole
//! store is one mutex, so the reserve check-and-insert is trivially atomic —
//! the property the Postgres side needs an advisory lock to get.

use crate::determinism::{random_ids, system_clock, Clock, IdGenerator};
use async_trait::async_trait;
use munarium_core::budget::{
    BudgetAdjustment, BudgetCorrection, BudgetEvidence, BudgetLedgerRow, BudgetOutcome,
    BudgetReservation, BudgetStore,
};
use munarium_core::Result;
use tokio::sync::Mutex;

#[derive(Debug, Clone)]
struct Row {
    id: String,
    tenant: String,
    config: String,
    tier: String,
    day: String,
    units: u64,
    original_units: u64,
    usage: Option<munarium_core::provider::UsageEvidence>,
    state: RowState,
    created_unix: i64,
    adjustments: Vec<BudgetAdjustment>,
    estimator_revision: Option<String>,
}

impl Row {
    fn evidence(&self) -> BudgetEvidence {
        BudgetEvidence {
            reservation_id: self.id.clone(),
            original_units: Some(self.original_units),
            accounted_units: self.units,
            state: match self.state {
                RowState::Held => "held",
                RowState::Settled => "settled",
                RowState::Released => "released",
            }
            .into(),
            usage: self.usage,
            revision: self.adjustments.len() as u64,
            config: self.config.clone(),
            tier: self.tier.clone(),
            day: self.day.clone(),
            estimator_revision: self.estimator_revision.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowState {
    Held,
    Settled,
    Released,
}

pub struct MemBudgetStore {
    rows: Mutex<Vec<Row>>,
    clock: Clock,
    ids: IdGenerator,
}

impl Default for MemBudgetStore {
    fn default() -> Self {
        Self::with_dependencies(system_clock(), random_ids())
    }
}

impl MemBudgetStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Inject UTC wall time and unique IDs. Sweeps retain whole-second age
    /// semantics. A backward clock delays expiry until wall time catches up;
    /// settlement always retains the reservation's original calendar day.
    pub fn with_dependencies(clock: Clock, ids: IdGenerator) -> Self {
        Self {
            rows: Mutex::new(Vec::new()),
            clock,
            ids,
        }
    }
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
        self.reserve_estimated(
            tenant,
            config,
            tier,
            munarium_core::budget::BudgetEstimate {
                units,
                revision: None,
            },
            limit,
        )
        .await
    }

    async fn reserve_estimated(
        &self,
        tenant: &str,
        config: &str,
        tier: &str,
        estimate: munarium_core::budget::BudgetEstimate<'_>,
        limit: Option<u64>,
    ) -> Result<BudgetOutcome> {
        let units = estimate.units;
        let Some(limit) = limit else {
            return Ok(BudgetOutcome::Unlimited);
        };
        let mut rows = self.rows.lock().await;
        let now = (self.clock)();
        let day = now.format("%Y-%m-%d").to_string();
        let active = rows
            .iter()
            .filter(|r| {
                r.tenant == tenant
                    && r.config == config
                    && r.tier == tier
                    && r.day == day
                    && r.state != RowState::Released
            })
            .try_fold(0u64, |total, r| total.checked_add(r.units))
            .ok_or_else(|| {
                munarium_core::KernelError::Storage("budget total exceeds u64".into())
            })?;
        if active.checked_add(units).is_none_or(|total| total > limit) {
            return Ok(BudgetOutcome::Exhausted {
                requested: units,
                remaining: limit.saturating_sub(active),
                limit,
            });
        }
        let reservation = BudgetReservation {
            id: (self.ids)(),
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
            original_units: units,
            usage: None,
            state: RowState::Held,
            created_unix: now.timestamp(),
            adjustments: Vec::new(),
            estimator_revision: estimate.revision.map(str::to_owned),
        });
        Ok(BudgetOutcome::Granted(reservation))
    }

    async fn settle(
        &self,
        reservation: &BudgetReservation,
        actual_units: Option<u64>,
    ) -> Result<()> {
        self.settle_with_evidence(reservation, actual_units, None)
            .await
    }

    async fn settle_with_evidence(
        &self,
        reservation: &BudgetReservation,
        actual_units: Option<u64>,
        usage: Option<munarium_core::provider::UsageEvidence>,
    ) -> Result<()> {
        let mut rows = self.rows.lock().await;
        if let Some(row) = rows.iter_mut().find(|r| {
            r.id == reservation.id && r.tenant == reservation.tenant && r.state == RowState::Held
        }) {
            row.state = RowState::Settled;
            row.usage = usage;
            if let Some(actual) = actual_units {
                row.units = actual;
            }
        }
        Ok(())
    }

    async fn evidence(&self, tenant: &str, id: &str) -> Result<Option<BudgetEvidence>> {
        Ok(self
            .rows
            .lock()
            .await
            .iter()
            .find(|r| r.tenant == tenant && r.id == id)
            .map(Row::evidence))
    }

    async fn reconcile(
        &self,
        tenant: &str,
        id: &str,
        correction: &BudgetCorrection,
    ) -> Result<BudgetEvidence> {
        let mut rows = self.rows.lock().await;
        let row = rows
            .iter_mut()
            .find(|r| r.tenant == tenant && r.id == id)
            .ok_or_else(|| munarium_core::KernelError::NotFound {
                kind: "budget-reservation",
                id: id.into(),
            })?;
        if let Some(existing) = row
            .adjustments
            .iter()
            .find(|a| a.correction.id == correction.id)
        {
            return if existing.correction == *correction {
                Ok(existing.result.clone())
            } else {
                Err(munarium_core::KernelError::IdempotencyMismatch)
            };
        }
        let previous = row.evidence();
        correction.validate(&previous)?;
        let mut result = previous.clone();
        result.accounted_units = correction.accounted_units;
        result.usage = Some(correction.usage);
        result.revision = previous.revision.checked_add(1).ok_or_else(|| {
            munarium_core::KernelError::Storage("budget revision overflow".into())
        })?;
        row.units = correction.accounted_units;
        row.usage = Some(correction.usage);
        row.adjustments.push(BudgetAdjustment {
            correction: correction.clone(),
            previous,
            result: result.clone(),
        });
        Ok(result)
    }

    async fn adjustments(&self, tenant: &str, id: &str) -> Result<Vec<BudgetAdjustment>> {
        let rows = self.rows.lock().await;
        let row = rows
            .iter()
            .find(|r| r.tenant == tenant && r.id == id)
            .ok_or_else(|| munarium_core::KernelError::NotFound {
                kind: "budget-reservation",
                id: id.into(),
            })?;
        Ok(row.adjustments.clone())
    }

    async fn evidence_for_day(&self, tenant: &str, day: &str) -> Result<Vec<BudgetEvidence>> {
        let rows = self.rows.lock().await;
        let mut out: Vec<_> = rows
            .iter()
            .filter(|r| r.tenant == tenant && r.day == day)
            .take(10001)
            .map(Row::evidence)
            .collect();
        if out.len() > 10000 {
            return Err(munarium_core::KernelError::InvalidInput(
                "budget report exceeds 10000 reservations".into(),
            ));
        }
        out.sort_by(|a, b| a.reservation_id.cmp(&b.reservation_id));
        Ok(out)
    }

    async fn release(&self, reservation: &BudgetReservation) -> Result<()> {
        let mut rows = self.rows.lock().await;
        if let Some(row) = rows.iter_mut().find(|r| {
            r.id == reservation.id && r.tenant == reservation.tenant && r.state == RowState::Held
        }) {
            row.state = RowState::Released;
        }
        Ok(())
    }

    async fn sweep_stale(&self, older_than_secs: u64) -> Result<u64> {
        let cutoff = (self.clock)()
            .timestamp()
            .saturating_sub(i64::try_from(older_than_secs).unwrap_or(i64::MAX));
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
        let day = (self.clock)().format("%Y-%m-%d").to_string();
        let mut grouped: std::collections::BTreeMap<(String, String), BudgetLedgerRow> =
            std::collections::BTreeMap::new();
        for r in rows.iter().filter(|r| r.tenant == tenant && r.day == day) {
            // Released rows neither hold nor settle, and create no group.
            let settled = match r.state {
                RowState::Held => false,
                RowState::Settled => true,
                RowState::Released => continue,
            };
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
            let total = if settled {
                &mut entry.settled_units
            } else {
                &mut entry.held_units
            };
            *total = total.checked_add(r.units).ok_or_else(|| {
                munarium_core::KernelError::Storage("budget total exceeds u64".into())
            })?;
            entry.reservations += 1;
        }
        Ok(grouped.into_values().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reconciliation_keeps_original_day_after_midnight() {
        use munarium_core::provider::{UsageEvidence, UsageSource};
        use std::sync::{
            atomic::{AtomicI64, Ordering},
            Arc,
        };
        let time = Arc::new(AtomicI64::new(1_700_000_000));
        let clock = time.clone();
        let store = MemBudgetStore::with_dependencies(
            Arc::new(move || {
                chrono::DateTime::from_timestamp(clock.load(Ordering::SeqCst), 0).unwrap()
            }),
            random_ids(),
        );
        let BudgetOutcome::Granted(r) = store
            .reserve("t", "cfg", "fast", 100, Some(100))
            .await
            .unwrap()
        else {
            panic!("grant")
        };
        store.settle(&r, None).await.unwrap();
        time.fetch_add(86400, Ordering::SeqCst);
        let corrected = store
            .reconcile(
                "t",
                &r.id,
                &BudgetCorrection {
                    id: "late".into(),
                    expected_revision: 0,
                    accounted_units: 200,
                    usage: UsageEvidence {
                        input_tokens: Some(180),
                        output_tokens: Some(20),
                        source: UsageSource::ProviderReported,
                    },
                    evidence_ref: "receipt".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(corrected.day, r.day);
        assert!(store.ledger("t").await.unwrap().is_empty());
        assert_eq!(
            store.evidence_for_day("t", &r.day).await.unwrap(),
            vec![corrected]
        );
        assert!(matches!(
            store
                .reserve("t", "cfg", "fast", 100, Some(100))
                .await
                .unwrap(),
            BudgetOutcome::Granted(_)
        ));
    }

    #[tokio::test]
    async fn admission_does_not_wrap_after_large_settlement() {
        let store = MemBudgetStore::new();
        let BudgetOutcome::Granted(r) = store
            .reserve("large", "cfg", "fast", 1, Some(u64::MAX))
            .await
            .unwrap()
        else {
            panic!("grant");
        };
        store.settle(&r, Some(u64::MAX)).await.unwrap();
        assert!(matches!(
            store
                .reserve("large", "cfg", "fast", 1, Some(u64::MAX))
                .await
                .unwrap(),
            BudgetOutcome::Exhausted { remaining: 0, .. }
        ));
    }

    #[tokio::test]
    async fn aggregate_overflow_fails_admission_and_reporting() {
        let store = MemBudgetStore::new();
        let mut reservations = Vec::new();
        for _ in 0..2 {
            let BudgetOutcome::Granted(r) = store
                .reserve("overflow", "cfg", "fast", 1, Some(10))
                .await
                .unwrap()
            else {
                panic!("grant");
            };
            reservations.push(r);
        }
        for r in reservations {
            store.settle(&r, Some(u64::MAX)).await.unwrap();
        }
        assert!(store
            .reserve("overflow", "cfg", "fast", 1, Some(10))
            .await
            .is_err());
        assert!(store.ledger("overflow").await.is_err());
    }

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
    async fn ledger_omits_released_reservations_and_their_groups() {
        // Control for the P15 restructure of `ledger`: released rows neither
        // count nor create a group, exactly as when they were filtered out.
        let store = MemBudgetStore::new();
        let mut granted = Vec::new();
        for (tier, units) in [("fast", 10), ("fast", 20), ("frontier", 5)] {
            let BudgetOutcome::Granted(r) = store
                .reserve("t", "cfg", tier, units, Some(1000))
                .await
                .unwrap()
            else {
                panic!("expected grant");
            };
            granted.push(r);
        }
        store.release(&granted[0]).await.unwrap();
        store.release(&granted[2]).await.unwrap();
        let rows = store.ledger("t").await.unwrap();
        assert_eq!(rows.len(), 1, "the released-only frontier group is absent");
        assert_eq!(rows[0].tier, "fast");
        assert_eq!(rows[0].held_units, 20);
        assert_eq!(rows[0].settled_units, 0);
        assert_eq!(rows[0].reservations, 1);
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
