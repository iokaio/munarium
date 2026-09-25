// SPDX-License-Identifier: Apache-2.0
//! Optional accounting, independent of token admission. All tariffs are supplied
//! by the operator; no provider, including a local endpoint, is implicitly free.
use crate::{KernelError, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rate {
    pub micro_units: u64,
    pub per_tokens: u64,
}

/// Inclusive rates charge input/output totals, including their cache/reasoning
/// subsets. Partitioned rates charge disjoint categories after subtracting those
/// subsets. A context-dependent tariff needs an externally reconciled observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    Inclusive,
    Partitioned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriceSnapshot {
    pub id: String,
    pub provider: String,
    /// SHA-256 of the exact effective request URL and routing selection. No
    /// credential-bearing URL is copied into the accounting record.
    pub route: String,
    pub model: String,
    pub currency: String,
    pub valid_from: DateTime<Utc>,
    pub valid_until: DateTime<Utc>,
    pub basis: Basis,
    pub rates: BTreeMap<String, Rate>,
}

impl PriceSnapshot {
    pub fn validate(&self) -> Result<()> {
        if self.id.is_empty()
            || self.provider.is_empty()
            || self.model.is_empty()
            || self.route.len() != 64
            || !self
                .route
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || self.currency.len() != 3
            || !self.currency.bytes().all(|b| b.is_ascii_uppercase())
            || self.valid_from >= self.valid_until
            || self.rates.values().any(|r| r.per_tokens == 0)
            || self.rates.keys().any(|k| {
                ![
                    "input",
                    "output",
                    "cache_read",
                    "cache_write",
                    "reasoning",
                    "context",
                ]
                .contains(&k.as_str())
            })
            || (self.basis == Basis::Inclusive
                && self.rates.keys().any(|k| k != "input" && k != "output"))
        {
            return Err(KernelError::InvalidInput("invalid price snapshot".into()));
        }
        Ok(())
    }
}

/// Totals include cache input and reasoning output; subsets must never exceed
/// their parent. None is unknown, including historical/legacy provider evidence.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoneyUsage {
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
    pub reasoning: Option<u64>,
    pub context: Option<u64>,
    pub unknown_categories: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Calculation {
    pub price_id: Option<String>,
    pub currency: Option<String>,
    /// Decimal integer string: safe across JSON clients above 2^53.
    pub micro_units: Option<String>,
    pub missing_price: bool,
    pub missing_usage: bool,
    pub unknown_categories: bool,
}

pub fn calculate(
    price: Option<&PriceSnapshot>,
    submitted: DateTime<Utc>,
    usage: &MoneyUsage,
) -> Result<Calculation> {
    if usage.input.is_some_and(|input| {
        usage
            .cache_read
            .unwrap_or(0)
            .checked_add(usage.cache_write.unwrap_or(0))
            .is_none_or(|n| n > input)
    }) || usage
        .reasoning
        .zip(usage.output)
        .is_some_and(|(reasoning, output)| reasoning > output)
    {
        return Err(KernelError::InvalidInput(
            "usage subset exceeds parent total".into(),
        ));
    }
    let price = price.filter(|p| p.valid_from <= submitted && submitted < p.valid_until);
    let mut result = Calculation {
        price_id: price.map(|p| p.id.clone()),
        currency: price.map(|p| p.currency.clone()),
        micro_units: None,
        missing_price: price.is_none(),
        missing_usage: usage.input.is_none() || usage.output.is_none(),
        unknown_categories: usage.unknown_categories,
    };
    let Some(p) = price else { return Ok(result) };
    p.validate()?;
    let required = match p.basis {
        Basis::Inclusive => &["input", "output"][..],
        Basis::Partitioned => &[
            "input",
            "output",
            "cache_read",
            "cache_write",
            "reasoning",
            "context",
        ][..],
    };
    result.missing_price |= required.iter().any(|k| !p.rates.contains_key(*k));
    let (Some(input), Some(output)) = (usage.input, usage.output) else {
        return Ok(result);
    };
    let mut counts = BTreeMap::from([("input", input), ("output", output)]);
    if p.basis == Basis::Partitioned {
        let (Some(read), Some(write), Some(reasoning), Some(context)) = (
            usage.cache_read,
            usage.cache_write,
            usage.reasoning,
            usage.context,
        ) else {
            result.missing_usage = true;
            return Ok(result);
        };
        let ordinary = input
            .checked_sub(read)
            .and_then(|n| n.checked_sub(write))
            .ok_or_else(|| KernelError::InvalidInput("cache usage exceeds input total".into()))?;
        let visible = output.checked_sub(reasoning).ok_or_else(|| {
            KernelError::InvalidInput("reasoning usage exceeds output total".into())
        })?;
        counts = BTreeMap::from([
            ("input", ordinary),
            ("output", visible),
            ("cache_read", read),
            ("cache_write", write),
            ("reasoning", reasoning),
            ("context", context),
        ]);
    } else if usage.context.is_some_and(|n| n > 0) {
        result.unknown_categories = true;
    }
    // Round each nonempty disjoint category up to a micro-unit, then sum.
    // Explicit zero prices are valid. Even zero counts need a declared rate:
    // absence of a rate cannot stand in for an operator's free-price decision.
    let mut total = 0u128;
    for (category, tokens) in counts {
        let Some(rate) = p.rates.get(category) else {
            result.missing_price = true;
            continue;
        };
        let product = u128::from(tokens) * u128::from(rate.micro_units);
        let amount = product.div_ceil(u128::from(rate.per_tokens));
        total = total
            .checked_add(amount)
            .ok_or_else(|| KernelError::InvalidInput("monetary total overflow".into()))?;
    }
    if !result.missing_price && !result.unknown_categories {
        let total = u64::try_from(total).map_err(|_| {
            KernelError::InvalidInput("monetary total exceeds u64 micro-units".into())
        })?;
        result.micro_units = Some(total.to_string());
    }
    Ok(result)
}
