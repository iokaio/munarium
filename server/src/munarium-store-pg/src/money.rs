// SPDX-License-Identifier: Apache-2.0
//! Tenant-scoped append-only invocation accounting. Recording is independent of
//! whether token admission has a cap or a tariff exists.
use crate::storage_err;
use chrono::{DateTime, Utc};
use munarium_core::money::{calculate, Calculation, MoneyUsage, PriceSnapshot};
use munarium_core::{KernelError, Result};
use serde::{Deserialize, Serialize};
use sqlx::{types::Json, PgPool};
use std::collections::BTreeMap;

#[derive(Clone)]
pub struct MoneyStore(pub PgPool);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub id: String,
    pub attempt_id: String,
    /// Optimistic append: 0 for the first observation, otherwise latest revision.
    pub previous_revision: i64,
    pub usage: MoneyUsage,
    pub accounted_units: u64,
    pub resolved: bool,
    /// Safe operator evidence reference, never an invoice body or credential.
    pub evidence_ref: String,
    /// Only for previously unpriced work, with explicit applicability evidence.
    pub price_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AttemptReport {
    pub id: String,
    pub invocation_id: String,
    pub provider: String,
    pub route: String,
    pub model: String,
    pub submitted_at: DateTime<Utc>,
    pub revision: i64,
    pub accounted_units: String,
    pub usage: MoneyUsage,
    pub unresolved: bool,
    pub calculation: Calculation,
}

#[derive(Debug, Default, Serialize)]
pub struct Coverage {
    pub attempts: u64,
    pub priced: u64,
    pub missing_price: u64,
    pub missing_usage: u64,
    pub unknown_categories: u64,
    pub unresolved: u64,
}

#[derive(Debug, Serialize)]
pub struct MoneyReport {
    pub scope: &'static str,
    pub entire_tenant_bill: bool,
    /// Legacy binaries, memory stores, direct custom-provider and health probes
    /// cannot be enumerated by this ledger. Unknown must not serialize as zero.
    pub unrecorded_attempts: Option<u64>,
    pub coverage: Coverage,
    pub known_subtotals_micro_units: BTreeMap<String, String>,
    pub accounted_units: String,
    pub observed_input_tokens: String,
    pub observed_output_tokens: String,
    pub attempts: Vec<AttemptReport>,
}

impl MoneyStore {
    pub async fn prices(&self, tenant: &str) -> Result<Vec<PriceSnapshot>> {
        let rows: Vec<(Json<PriceSnapshot>,)> =
            sqlx::query_as("SELECT snapshot FROM monetary_prices WHERE tenant_id = $1 ORDER BY id")
                .bind(tenant)
                .fetch_all(&self.0)
                .await
                .map_err(storage_err)?;
        Ok(rows.into_iter().map(|r| r.0 .0).collect())
    }

    pub async fn add_price(&self, tenant: &str, price: &PriceSnapshot) -> Result<()> {
        price.validate()?;
        let mut tx = self.0.begin().await.map_err(storage_err)?;
        // Serialize catalog validation/insert across replicas.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 14))")
            .bind(tenant)
            .execute(&mut *tx)
            .await
            .map_err(storage_err)?;
        let existing: Vec<(Json<PriceSnapshot>,)> =
            sqlx::query_as("SELECT snapshot FROM monetary_prices WHERE tenant_id = $1")
                .bind(tenant)
                .fetch_all(&mut *tx)
                .await
                .map_err(storage_err)?;
        for (Json(old),) in existing {
            if old == *price {
                return Ok(());
            }
            if old.id == price.id
                || (old.provider == price.provider
                    && old.route == price.route
                    && old.model == price.model
                    && old.valid_from < price.valid_until
                    && price.valid_from < old.valid_until)
            {
                return Err(KernelError::InvalidInput(
                    "price identity reused or validity windows overlap".into(),
                ));
            }
        }
        sqlx::query("INSERT INTO monetary_prices (tenant_id,id,snapshot) VALUES ($1,$2,$3)")
            .bind(tenant)
            .bind(&price.id)
            .bind(Json(price))
            .execute(&mut *tx)
            .await
            .map_err(storage_err)?;
        tx.commit().await.map_err(storage_err)
    }

    /// Commit the pin before submitting an HTTP attempt. A crash after this
    /// commit remains conservatively unresolved, including a pre-send crash.
    pub async fn begin(
        &self,
        tenant: &str,
        invocation: &str,
        provider: &str,
        route: &str,
        model: &str,
        estimate: u64,
    ) -> Result<String> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let mut tx = self.0.begin().await.map_err(storage_err)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 14))")
            .bind(tenant)
            .execute(&mut *tx)
            .await
            .map_err(storage_err)?;
        let submitted: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await
            .map_err(storage_err)?;
        let prices: Vec<(Json<PriceSnapshot>,)> =
            sqlx::query_as("SELECT snapshot FROM monetary_prices WHERE tenant_id = $1")
                .bind(tenant)
                .fetch_all(&mut *tx)
                .await
                .map_err(storage_err)?;
        let price = prices.iter().map(|p| &p.0 .0).find(|p| {
            p.provider == provider
                && p.route == route
                && p.model == model
                && p.valid_from <= submitted
                && submitted < p.valid_until
        });
        sqlx::query("INSERT INTO monetary_attempts (tenant_id,id,invocation_id,provider,route,model,submitted_at,accounted_units,price_id) VALUES ($1,$2,$3,$4,$5,$6,$7,$8::text::numeric,$9)")
            .bind(tenant).bind(&id).bind(invocation).bind(provider).bind(route).bind(model).bind(submitted).bind(estimate.to_string()).bind(price.map(|p| &p.id))
            .execute(&mut *tx).await.map_err(storage_err)?;
        tx.commit().await.map_err(storage_err)?;
        Ok(id)
    }

    pub async fn observe(&self, tenant: &str, observation: &Observation) -> Result<Calculation> {
        if observation.id.is_empty()
            || observation.evidence_ref.is_empty()
            || observation.previous_revision < 0
        {
            return Err(KernelError::InvalidInput(
                "observation needs identity, evidence and previous revision".into(),
            ));
        }
        let mut tx = self.0.begin().await.map_err(storage_err)?;
        #[allow(clippy::type_complexity)]
        let row: Option<(String, String, String, DateTime<Utc>, Option<String>, String)> = sqlx::query_as("SELECT provider,route,model,submitted_at,price_id,accounted_units::text FROM monetary_attempts WHERE tenant_id=$1 AND id=$2 FOR UPDATE")
            .bind(tenant).bind(&observation.attempt_id).fetch_optional(&mut *tx).await.map_err(storage_err)?;
        let Some((provider, route, model, submitted, pinned, estimate)) = row else {
            return Err(KernelError::NotFound {
                kind: "monetary-attempt",
                id: observation.attempt_id.clone(),
            });
        };
        let observed = observation
            .usage
            .input
            .unwrap_or(0)
            .checked_add(observation.usage.output.unwrap_or(0))
            .ok_or_else(|| KernelError::InvalidInput("observed token total overflow".into()))?;
        let incomplete = !observation.resolved
            || observation.usage.input.is_none()
            || observation.usage.output.is_none()
            || observation.usage.unknown_categories;
        let estimate: u64 = estimate
            .parse()
            .map_err(|_| KernelError::Storage("invalid stored estimate".into()))?;
        if observation.accounted_units < observed
            || (incomplete && observation.accounted_units < estimate)
        {
            return Err(KernelError::InvalidInput(
                "accounted units must cover observed usage and unresolved estimate".into(),
            ));
        }
        let duplicate: Option<(Json<Observation>, Json<Calculation>)> = sqlx::query_as("SELECT observation,calculation FROM monetary_observations WHERE tenant_id=$1 AND id=$2")
            .bind(tenant).bind(&observation.id).fetch_optional(&mut *tx).await.map_err(storage_err)?;
        if let Some((Json(old), Json(calculation))) = duplicate {
            return if old == *observation {
                Ok(calculation)
            } else {
                Err(KernelError::InvalidInput(
                    "observation identity reused".into(),
                ))
            };
        }
        let latest: Option<(i64, Json<Calculation>)> = sqlx::query_as("SELECT revision,calculation FROM monetary_observations WHERE tenant_id=$1 AND attempt_id=$2 ORDER BY revision DESC LIMIT 1")
            .bind(tenant).bind(&observation.attempt_id).fetch_optional(&mut *tx).await.map_err(storage_err)?;
        let revision = latest.as_ref().map_or(0, |r| r.0);
        if observation.previous_revision != revision {
            return Err(KernelError::InvalidInput(
                "stale monetary observation revision".into(),
            ));
        }
        let pinned = pinned.or_else(|| latest.and_then(|r| r.1 .0.price_id));
        if pinned.is_some() && observation.price_id.is_some() && pinned != observation.price_id {
            return Err(KernelError::InvalidInput(
                "an attempt's price pin cannot change".into(),
            ));
        }
        let selected = pinned.or_else(|| observation.price_id.clone());
        let price: Option<Json<PriceSnapshot>> = match selected {
            Some(id) => Some(
                sqlx::query_scalar(
                    "SELECT snapshot FROM monetary_prices WHERE tenant_id=$1 AND id=$2",
                )
                .bind(tenant)
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(storage_err)?
                .ok_or_else(|| KernelError::InvalidInput("unknown price snapshot".into()))?,
            ),
            None => None,
        };
        if price.as_ref().is_some_and(|p| {
            p.provider != provider
                || p.route != route
                || p.model != model
                || submitted < p.valid_from
                || submitted >= p.valid_until
        }) {
            return Err(KernelError::InvalidInput(
                "price is not applicable at submission".into(),
            ));
        }
        let calculation = calculate(price.as_ref().map(|p| &p.0), submitted, &observation.usage)?;
        let next = revision
            .checked_add(1)
            .ok_or_else(|| KernelError::InvalidInput("observation revision overflow".into()))?;
        sqlx::query("INSERT INTO monetary_observations (tenant_id,id,attempt_id,revision,observation,calculation) VALUES ($1,$2,$3,$4,$5,$6)")
            .bind(tenant).bind(&observation.id).bind(&observation.attempt_id).bind(next).bind(Json(observation)).bind(Json(&calculation))
            .execute(&mut *tx).await.map_err(storage_err)?;
        tx.commit().await.map_err(storage_err)?;
        Ok(calculation)
    }

    /// A bounded, explicit window. No silent LIMIT truncation: refuse oversized
    /// windows and let the caller narrow them. One query gives a consistent view.
    pub async fn report(
        &self,
        tenant: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<MoneyReport> {
        if from >= to {
            return Err(KernelError::InvalidInput("from must precede to".into()));
        }
        #[allow(clippy::type_complexity)]
        let rows: Vec<(String,String,String,String,String,DateTime<Utc>,String,Option<Json<PriceSnapshot>>,Option<i64>,Option<Json<Observation>>,Option<Json<Calculation>>)> = sqlx::query_as(
            "SELECT a.id,a.invocation_id,a.provider,a.route,a.model,a.submitted_at,a.accounted_units::text,p.snapshot,o.revision,o.observation,o.calculation
             FROM monetary_attempts a LEFT JOIN monetary_prices p ON p.tenant_id=a.tenant_id AND p.id=a.price_id
             LEFT JOIN LATERAL (SELECT revision,observation,calculation FROM monetary_observations WHERE tenant_id=a.tenant_id AND attempt_id=a.id ORDER BY revision DESC LIMIT 1) o ON true
             WHERE a.tenant_id=$1 AND a.submitted_at >= $2 AND a.submitted_at < $3 ORDER BY a.submitted_at,a.id LIMIT 10001")
            .bind(tenant).bind(from).bind(to).fetch_all(&self.0).await.map_err(storage_err)?;
        if rows.len() > 10000 {
            return Err(KernelError::InvalidInput(
                "monetary report exceeds 10000 attempts; narrow the time window".into(),
            ));
        }
        let mut coverage = Coverage::default();
        let mut totals: BTreeMap<String, u64> = BTreeMap::new();
        let mut units = 0u128;
        let mut observed_input = 0u128;
        let mut observed_output = 0u128;
        let mut attempts = Vec::new();
        for (
            id,
            invocation_id,
            provider,
            route,
            model,
            submitted_at,
            estimate,
            price,
            revision,
            obs,
            calc,
        ) in rows
        {
            let usage = obs
                .as_ref()
                .map_or_else(MoneyUsage::default, |o| o.usage.clone());
            observed_input += u128::from(usage.input.unwrap_or(0));
            observed_output += u128::from(usage.output.unwrap_or(0));
            let unresolved = obs.as_ref().is_none_or(|o| !o.resolved);
            let accounted = obs
                .as_ref()
                .map_or(estimate, |o| o.accounted_units.to_string());
            units = units
                .checked_add(
                    accounted
                        .parse::<u128>()
                        .map_err(|e| KernelError::Storage(e.to_string()))?,
                )
                .ok_or_else(|| KernelError::Storage("accounted units overflow".into()))?;
            let calculation = match calc {
                Some(c) => c.0,
                None => calculate(price.as_ref().map(|p| &p.0), submitted_at, &usage)?,
            };
            coverage.attempts += 1;
            coverage.unresolved += u64::from(unresolved);
            coverage.missing_price += u64::from(calculation.missing_price);
            coverage.missing_usage += u64::from(calculation.missing_usage);
            coverage.unknown_categories += u64::from(calculation.unknown_categories);
            if let (Some(currency), Some(amount)) =
                (&calculation.currency, &calculation.micro_units)
            {
                coverage.priced += 1;
                let total = totals.entry(currency.clone()).or_default();
                *total = total
                    .checked_add(
                        amount
                            .parse::<u64>()
                            .map_err(|e| KernelError::Storage(e.to_string()))?,
                    )
                    .ok_or_else(|| KernelError::Storage("monetary report total overflow".into()))?;
            }
            attempts.push(AttemptReport {
                id,
                invocation_id,
                provider,
                route,
                model,
                submitted_at,
                revision: revision.unwrap_or(0),
                accounted_units: accounted,
                usage,
                unresolved,
                calculation,
            });
        }
        Ok(MoneyReport {
            scope: "recorded_gateway_http_attempts",
            entire_tenant_bill: false,
            unrecorded_attempts: None,
            coverage,
            known_subtotals_micro_units: totals
                .into_iter()
                .map(|(k, v)| (k, v.to_string()))
                .collect(),
            accounted_units: units.to_string(),
            observed_input_tokens: observed_input.to_string(),
            observed_output_tokens: observed_output.to_string(),
            attempts,
        })
    }
}
