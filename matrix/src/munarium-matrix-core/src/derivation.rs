// SPDX-License-Identifier: Apache-2.0
//! Declared derivations — the only arithmetic an answer is allowed to claim.
//!
//! G5 says every number in an answer resolves to a result cell **or a
//! recomputable derivation**. This module is the second half. A contract
//! declares which derivations exist; Matrix computes them from the sealed
//! cells and ships the values in the block; the server recomputes them when it
//! verifies an assertion. A number that is neither a cell nor one of these is
//! unverifiable and is removed from the answer.
//!
//! Everything is exact decimal arithmetic. A `sum` over a currency column that
//! went through an f64 would be wrong in the last cent, and the whole point of
//! the exercise is that the last cent is right.

use crate::result::{ResultSchema, TypedResult};
use crate::value::{ColumnType, Value};
use rust_decimal::prelude::*;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DerivationOp {
    Sum,
    Count,
    Ratio,
    Diff,
    PctChange,
    Min,
    Max,
    /// The difference between the values at two declared `as_of` parameters.
    /// Declared here, computed by the caller that holds both results.
    AsOfDiff,
}

impl DerivationOp {
    pub fn as_str(self) -> &'static str {
        match self {
            DerivationOp::Sum => "sum",
            DerivationOp::Count => "count",
            DerivationOp::Ratio => "ratio",
            DerivationOp::Diff => "diff",
            DerivationOp::PctChange => "pct_change",
            DerivationOp::Min => "min",
            DerivationOp::Max => "max",
            DerivationOp::AsOfDiff => "as_of_diff",
        }
    }
}

/// One declared derivation, as it appears in a `QueryContract`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Derivation {
    /// The name an answer cites (`derivation_ref`).
    pub name: String,
    pub op: DerivationOp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub over: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub numerator: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denominator: Option<String>,
    /// Result scale. Defaults to the source column's scale for sums and
    /// min/max; ratios and percentages need it declared because their natural
    /// scale is unbounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<u32>,
}

/// A computed derivation, ready to travel in an `EvidenceBlock`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComputedDerivation {
    #[serde(rename = "ref")]
    pub reference: String,
    pub op: DerivationOp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub over: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub numerator: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denominator: Option<String>,
    /// canon@1 text form, or `None` when the derivation is undefined (a ratio
    /// with a zero denominator). Undefined is reported, never rendered as 0.
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DerivationError {
    #[error("derivation '{0}' names column '{1}', which is not in the result")]
    UnknownColumn(String, String),
    #[error("derivation '{0}' is {1} over non-numeric column '{2}'")]
    NotNumeric(String, &'static str, String),
    #[error("derivation '{0}' ({1}) is missing a required operand")]
    MissingOperand(String, &'static str),
    #[error(
        "derivation '{0}' uses as_of_diff, which needs two results and is computed by the caller"
    )]
    NeedsTwoResults(String),
}

impl Derivation {
    /// Validate against a schema at contract-apply time, so a broken
    /// derivation is a validation error and never a turn-time surprise.
    pub fn validate(&self, schema: &ResultSchema) -> Result<(), DerivationError> {
        let numeric = |col: &str| -> Result<(), DerivationError> {
            let idx = schema
                .column_index(col)
                .ok_or_else(|| DerivationError::UnknownColumn(self.name.clone(), col.into()))?;
            if !schema.columns[idx].ty.is_numeric() {
                return Err(DerivationError::NotNumeric(
                    self.name.clone(),
                    self.op.as_str(),
                    col.into(),
                ));
            }
            Ok(())
        };
        let present = |o: &Option<String>, what: &'static str| -> Result<String, DerivationError> {
            o.clone()
                .ok_or_else(|| DerivationError::MissingOperand(self.name.clone(), what))
        };
        match self.op {
            DerivationOp::Sum | DerivationOp::Min | DerivationOp::Max => {
                numeric(&present(&self.over, "over")?)
            }
            DerivationOp::Count => {
                // count may be over any column (or none: count of rows).
                if let Some(c) = &self.over {
                    schema.column_index(c).ok_or_else(|| {
                        DerivationError::UnknownColumn(self.name.clone(), c.clone())
                    })?;
                }
                Ok(())
            }
            DerivationOp::Ratio | DerivationOp::Diff | DerivationOp::PctChange => {
                numeric(&present(&self.numerator, "numerator")?)?;
                numeric(&present(&self.denominator, "denominator")?)
            }
            DerivationOp::AsOfDiff => Err(DerivationError::NeedsTwoResults(self.name.clone())),
        }
    }
}

/// Exact decimal for a numeric cell; `None` for NULL or a non-numeric value.
fn numeric_cell(v: &Value) -> Option<Decimal> {
    match v {
        Value::Int64(i) => Some(Decimal::from(*i)),
        Value::Decimal { value, .. } => Some(*value),
        Value::Float64(f) => Decimal::from_f64(*f),
        _ => None,
    }
}

fn column_scale(schema: &ResultSchema, col: &str) -> Option<u32> {
    schema
        .column_index(col)
        .and_then(|i| schema.columns[i].scale)
}

fn column_unit(schema: &ResultSchema, col: &str) -> Option<String> {
    schema
        .column_index(col)
        .and_then(|i| schema.columns[i].unit.clone())
}

/// Compute one derivation over a result.
///
/// NULL cells are **skipped**, not treated as zero: a sum over a column with
/// no non-null values is `None` (undefined), not `0`. That distinction is what
/// stops an empty quarter from being reported as zero revenue.
pub fn compute(
    d: &Derivation,
    result: &TypedResult,
) -> Result<ComputedDerivation, DerivationError> {
    let schema = &result.schema;
    let column_values = |col: &str| -> Result<Vec<Decimal>, DerivationError> {
        let idx = schema
            .column_index(col)
            .ok_or_else(|| DerivationError::UnknownColumn(d.name.clone(), col.into()))?;
        Ok(result
            .rows
            .iter()
            .filter_map(|r| r.cells.get(idx))
            .filter_map(numeric_cell)
            .collect())
    };

    let (value, unit, scale): (Option<Decimal>, Option<String>, Option<u32>) = match d.op {
        DerivationOp::Sum => {
            let col = d
                .over
                .clone()
                .ok_or_else(|| DerivationError::MissingOperand(d.name.clone(), "over"))?;
            let vals = column_values(&col)?;
            let v = if vals.is_empty() {
                None
            } else {
                Some(vals.iter().fold(Decimal::ZERO, |a, b| a + b))
            };
            (
                v,
                column_unit(schema, &col),
                d.scale.or_else(|| column_scale(schema, &col)),
            )
        }
        DerivationOp::Count => {
            let n = match &d.over {
                // count of non-null values in a column
                Some(col) => {
                    let idx = schema.column_index(col).ok_or_else(|| {
                        DerivationError::UnknownColumn(d.name.clone(), col.clone())
                    })?;
                    result
                        .rows
                        .iter()
                        .filter(|r| r.cells.get(idx).map(|c| !c.is_null()).unwrap_or(false))
                        .count()
                }
                // count of rows
                None => result.rows.len(),
            };
            (Some(Decimal::from(n)), None, Some(0))
        }
        DerivationOp::Min | DerivationOp::Max => {
            let col = d
                .over
                .clone()
                .ok_or_else(|| DerivationError::MissingOperand(d.name.clone(), "over"))?;
            let vals = column_values(&col)?;
            let v = if d.op == DerivationOp::Min {
                vals.into_iter().min()
            } else {
                vals.into_iter().max()
            };
            (
                v,
                column_unit(schema, &col),
                d.scale.or_else(|| column_scale(schema, &col)),
            )
        }
        DerivationOp::Ratio | DerivationOp::Diff | DerivationOp::PctChange => {
            let num_col = d
                .numerator
                .clone()
                .ok_or_else(|| DerivationError::MissingOperand(d.name.clone(), "numerator"))?;
            let den_col = d
                .denominator
                .clone()
                .ok_or_else(|| DerivationError::MissingOperand(d.name.clone(), "denominator"))?;
            let num: Decimal = column_values(&num_col)?.iter().sum();
            let den: Decimal = column_values(&den_col)?.iter().sum();
            let v = match d.op {
                DerivationOp::Diff => Some(num - den),
                // A zero denominator is UNDEFINED, not zero and not an error:
                // the answer must be able to say "not defined for this slice".
                DerivationOp::Ratio if den.is_zero() => None,
                DerivationOp::Ratio => Some(num / den),
                DerivationOp::PctChange if den.is_zero() => None,
                DerivationOp::PctChange => Some((num - den) / den * Decimal::from(100)),
                _ => unreachable!(),
            };
            let unit = match d.op {
                DerivationOp::Diff => column_unit(schema, &num_col),
                DerivationOp::PctChange => Some("%".to_string()),
                _ => None,
            };
            (v, unit, d.scale)
        }
        DerivationOp::AsOfDiff => return Err(DerivationError::NeedsTwoResults(d.name.clone())),
    };

    let text = value.map(|mut v| {
        if let Some(s) = scale {
            v.rescale(s);
        }
        // Route through the one canonical formatter so a derivation and a cell
        // holding the same number are the same string.
        Value::Decimal {
            value: v,
            scale: scale.unwrap_or(v.scale()),
        }
        .canonical_text()
        .unwrap_or_default()
    });

    Ok(ComputedDerivation {
        reference: d.name.clone(),
        op: d.op,
        over: d.over.clone(),
        numerator: d.numerator.clone(),
        denominator: d.denominator.clone(),
        value: text,
        unit,
        scale,
    })
}

/// Compute every declared derivation, stopping at the first invalid one.
pub fn compute_all(
    declared: &[Derivation],
    result: &TypedResult,
) -> Result<Vec<ComputedDerivation>, DerivationError> {
    declared.iter().map(|d| compute(d, result)).collect()
}

/// Column types a derivation may operate on — re-exported so callers do not
/// have to reach into `value` for the one predicate they need.
pub fn is_numeric(ty: ColumnType) -> bool {
    ty.is_numeric()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::{AuthorizationClass, Column, ResultSchema, Row, RowIdRule};
    use std::str::FromStr;

    fn dec(s: &str, scale: u32) -> Value {
        Value::Decimal {
            value: Decimal::from_str(s).unwrap(),
            scale,
        }
    }

    fn result(rows: Vec<Row>) -> TypedResult {
        TypedResult {
            schema: ResultSchema {
                columns: vec![
                    Column::new("c0", "region", ColumnType::String).key(),
                    Column::new("c1", "amount", ColumnType::Decimal)
                        .scale(2)
                        .unit("USD")
                        .additive(),
                    Column::new("c2", "count", ColumnType::Int64).nullable(),
                ],
                row_id_rule: RowIdRule::Keys,
                order_by: vec![],
            },
            rows,
            truncated: false,
            denied_columns: vec![],
            authorization_class: AuthorizationClass::default(),
        }
    }

    fn row(region: &str, amount: &str, count: Option<i64>) -> Row {
        Row::new(vec![
            Value::String(region.into()),
            dec(amount, 2),
            count.map(Value::Int64).unwrap_or(Value::Null),
        ])
    }

    #[test]
    fn sum_is_exact_decimal_not_float() {
        // Three values that a f64 sum gets wrong in the last cent.
        let r = result(vec![
            row("A", "0.10", Some(1)),
            row("B", "0.20", Some(2)),
            row("C", "0.30", Some(3)),
        ]);
        let d = Derivation {
            name: "total".into(),
            op: DerivationOp::Sum,
            over: Some("amount".into()),
            numerator: None,
            denominator: None,
            scale: None,
        };
        let c = compute(&d, &r).unwrap();
        assert_eq!(c.value.as_deref(), Some("0.60"));
        assert_eq!(c.unit.as_deref(), Some("USD"));
    }

    #[test]
    fn a_sum_over_no_values_is_undefined_not_zero() {
        let r = result(vec![]);
        let d = Derivation {
            name: "total".into(),
            op: DerivationOp::Sum,
            over: Some("amount".into()),
            numerator: None,
            denominator: None,
            scale: None,
        };
        assert_eq!(compute(&d, &r).unwrap().value, None);
    }

    #[test]
    fn count_over_a_column_skips_nulls_but_count_of_rows_does_not() {
        let r = result(vec![row("A", "1.00", Some(1)), row("B", "2.00", None)]);
        let over_col = Derivation {
            name: "n".into(),
            op: DerivationOp::Count,
            over: Some("count".into()),
            numerator: None,
            denominator: None,
            scale: None,
        };
        let over_rows = Derivation {
            name: "n".into(),
            op: DerivationOp::Count,
            over: None,
            numerator: None,
            denominator: None,
            scale: None,
        };
        assert_eq!(compute(&over_col, &r).unwrap().value.as_deref(), Some("1"));
        assert_eq!(compute(&over_rows, &r).unwrap().value.as_deref(), Some("2"));
    }

    #[test]
    fn a_ratio_with_a_zero_denominator_is_undefined_never_zero() {
        let r = result(vec![row("A", "0.00", Some(5))]);
        let d = Derivation {
            name: "share".into(),
            op: DerivationOp::Ratio,
            over: None,
            numerator: Some("count".into()),
            denominator: Some("amount".into()),
            scale: Some(4),
        };
        let c = compute(&d, &r).unwrap();
        assert_eq!(c.value, None, "a zero denominator must not render as 0");
    }

    #[test]
    fn derivations_over_non_numeric_columns_fail_at_validation_not_at_runtime() {
        let r = result(vec![]);
        let d = Derivation {
            name: "bad".into(),
            op: DerivationOp::Sum,
            over: Some("region".into()),
            numerator: None,
            denominator: None,
            scale: None,
        };
        assert_eq!(
            d.validate(&r.schema),
            Err(DerivationError::NotNumeric(
                "bad".into(),
                "sum",
                "region".into()
            ))
        );
    }

    #[test]
    fn an_unknown_column_is_caught_at_validation() {
        let r = result(vec![]);
        let d = Derivation {
            name: "bad".into(),
            op: DerivationOp::Sum,
            over: Some("nope".into()),
            numerator: None,
            denominator: None,
            scale: None,
        };
        assert_eq!(
            d.validate(&r.schema),
            Err(DerivationError::UnknownColumn("bad".into(), "nope".into()))
        );
    }

    #[test]
    fn pct_change_carries_a_percent_unit() {
        let r = result(vec![row("A", "150.00", Some(100))]);
        let d = Derivation {
            name: "growth".into(),
            op: DerivationOp::PctChange,
            over: None,
            numerator: Some("amount".into()),
            denominator: Some("count".into()),
            scale: Some(2),
        };
        let c = compute(&d, &r).unwrap();
        assert_eq!(c.value.as_deref(), Some("50.00"));
        assert_eq!(c.unit.as_deref(), Some("%"));
    }
}
