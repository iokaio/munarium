// SPDX-License-Identifier: Apache-2.0
//! Result shape: columns, rows, and the identity rule that makes a result
//! sealable. A result that cannot say how its rows are identified is refused
//! *before* it is executed — see [`ResultSchema::validate`].

use crate::value::{ColumnType, Value};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Additivity {
    /// Summable across every dimension (an amount).
    Additive,
    /// Summable across some dimensions but not time (a balance).
    SemiAdditive,
    /// Never summable (a ratio, a rate).
    NonAdditive,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Column {
    /// Stable within the contract; survives a rename of the source column, so
    /// a rename does not silently change evidence identity.
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub ty: ColumnType,
    pub nullable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additivity: Option<Additivity>,
    #[serde(default)]
    pub key: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub element_type: Option<ColumnType>,
}

impl Column {
    pub fn new(id: &str, name: &str, ty: ColumnType) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            ty,
            nullable: false,
            scale: None,
            unit: None,
            additivity: None,
            key: false,
            element_type: None,
        }
    }
    pub fn key(mut self) -> Self {
        self.key = true;
        self
    }
    pub fn nullable(mut self) -> Self {
        self.nullable = true;
        self
    }
    pub fn scale(mut self, s: u32) -> Self {
        self.scale = Some(s);
        self
    }
    pub fn unit(mut self, u: &str) -> Self {
        self.unit = Some(u.to_string());
        self
    }
    pub fn additive(mut self) -> Self {
        self.additivity = Some(Additivity::Additive);
        self
    }
}

/// How a row is named. This is the single most consequential field in the
/// contract: it decides whether a result hashes as a set or as a sequence, and
/// therefore whether re-running a query that returns the same rows in a
/// different order produces the same evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RowIdRule {
    /// `row_id` is the encoded key tuple; the result hashes as a MULTISET.
    Keys,
    /// `row_id` is the 0-based position; the result hashes as a SEQUENCE and
    /// is legal only under a total `order_by`.
    Position,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultSchema {
    pub columns: Vec<Column>,
    pub row_id_rule: RowIdRule,
    #[serde(default)]
    pub order_by: Vec<String>,
}

impl ResultSchema {
    pub fn key_columns(&self) -> Vec<&Column> {
        self.columns.iter().filter(|c| c.key).collect()
    }

    pub fn column_index(&self, name_or_id: &str) -> Option<usize> {
        self.columns
            .iter()
            .position(|c| c.name == name_or_id || c.id == name_or_id)
    }

    /// The rule from `canonicalization.schema.json`: a result declares keys or
    /// a total `order_by`, or it cannot be sealed.
    pub fn validate(&self) -> Result<(), SchemaError> {
        if self.columns.is_empty() {
            return Err(SchemaError::NoColumns);
        }
        let mut seen_ids = std::collections::BTreeSet::new();
        let mut seen_names = std::collections::BTreeSet::new();
        for c in &self.columns {
            if !seen_ids.insert(&c.id) {
                return Err(SchemaError::DuplicateColumn(c.id.clone()));
            }
            if !seen_names.insert(&c.name) {
                return Err(SchemaError::DuplicateColumn(c.name.clone()));
            }
            if c.ty == ColumnType::Decimal && c.scale.is_none() {
                return Err(SchemaError::DecimalWithoutScale(c.name.clone()));
            }
            if c.ty == ColumnType::Array && c.element_type.is_none() {
                return Err(SchemaError::ArrayWithoutElementType(c.name.clone()));
            }
        }
        match self.row_id_rule {
            RowIdRule::Keys => {
                if self.key_columns().is_empty() {
                    return Err(SchemaError::NotIdentifiable);
                }
                // A nullable key cannot identify a row: two rows with NULL in
                // the same key column are indistinguishable, and a "multiset of
                // rows keyed by NULL" is not a key at all.
                if let Some(c) = self.key_columns().into_iter().find(|c| c.nullable) {
                    return Err(SchemaError::NullableKey(c.name.clone()));
                }
            }
            RowIdRule::Position => {
                if self.order_by.is_empty() {
                    return Err(SchemaError::NotIdentifiable);
                }
                for name in &self.order_by {
                    if self.column_index(name).is_none() {
                        return Err(SchemaError::UnknownOrderColumn(name.clone()));
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SchemaError {
    #[error("a result schema needs at least one column")]
    NoColumns,
    #[error("duplicate column '{0}'")]
    DuplicateColumn(String),
    #[error("decimal column '{0}' must declare a scale — an undeclared scale has no canonical text form")]
    DecimalWithoutScale(String),
    #[error("array column '{0}' must declare an element type")]
    ArrayWithoutElementType(String),
    #[error(
        "result declares neither key columns nor a total orderBy, so its rows cannot be identified \
         and it cannot be sealed (canon@1)"
    )]
    NotIdentifiable,
    #[error("key column '{0}' is nullable — a NULL key identifies nothing")]
    NullableKey(String),
    #[error("orderBy names '{0}', which is not a result column")]
    UnknownOrderColumn(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub cells: Vec<Value>,
}

impl Row {
    pub fn new(cells: Vec<Value>) -> Self {
        Self { cells }
    }
}

/// The authorization equivalence class an artifact belongs to. A session
/// resolving a citation must **dominate** it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AuthorizationClass {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub access_level: i32,
    #[serde(default)]
    pub compartments: Vec<String>,
}

impl AuthorizationClass {
    /// Does `session` dominate this class? Level must be at least as high AND
    /// every compartment must be held. Deliberately not symmetric and
    /// deliberately not "any of" — need-to-know is a conjunction.
    pub fn dominated_by(&self, session_level: i32, session_compartments: &[String]) -> bool {
        session_level >= self.access_level
            && self
                .compartments
                .iter()
                .all(|c| session_compartments.iter().any(|s| s == c))
    }
}

/// A executed, typed result before it is sealed.
#[derive(Debug, Clone, PartialEq)]
pub struct TypedResult {
    pub schema: ResultSchema,
    pub rows: Vec<Row>,
    /// True when a limit stopped the read. A truncated result can never back a
    /// completeness or exactness claim (G4).
    pub truncated: bool,
    /// Columns the policy denied. They were never selected; naming them is how
    /// an operator sees why a column is missing.
    pub denied_columns: Vec<String>,
    pub authorization_class: AuthorizationClass,
}

impl TypedResult {
    pub fn validate(&self) -> Result<(), SchemaError> {
        self.schema.validate()?;
        let width = self.schema.columns.len();
        for (i, row) in self.rows.iter().enumerate() {
            if row.cells.len() != width {
                return Err(SchemaError::DuplicateColumn(format!(
                    "row {i} has {} cells, schema has {width}",
                    row.cells.len()
                )));
            }
        }
        Ok(())
    }

    /// Conform every decimal CELL to its column's declared scale.
    ///
    /// Engines differ in how much scale their wire keeps and identity must
    /// not: BigQuery renders a `NUMERIC(28,2)`'s `900000.50` as `900000.5`
    /// (measured 2026-08-31 — its query-response schema carries no scale at
    /// all), while Postgres and Databricks keep the column's. Reconciling the
    /// SCHEMA against the contract's declaration fixed the metadata and left
    /// the cells alone, so a sealed result could say `scale: 2` over a cell
    /// that renders at scale 1 — and the same logical row would hash
    /// differently by engine, which is the exact thing canon@1 exists to
    /// prevent.
    ///
    /// Widening is lossless bookkeeping and happens here. A cell carrying
    /// MORE numeric precision than the declaration is a refusal, not a
    /// rounding: rounding would seal a value the contract's identity cannot
    /// represent and the source never asserted at that scale.
    pub fn conform_decimal_scales(&mut self) -> Result<(), crate::Refusal> {
        for (idx, col) in self.schema.columns.iter().enumerate() {
            let Some(declared) = col.scale else { continue };
            for row in &mut self.rows {
                if let Some(crate::Value::Decimal { value, scale }) = row.cells.get_mut(idx) {
                    let mut widened = *value;
                    widened.rescale(declared);
                    if widened != *value {
                        return Err(crate::Refusal::schema_drift(format!(
                            "column '{}' is declared scale {declared} but the source returned \
                             '{value}', which does not fit it without rounding",
                            col.name
                        )));
                    }
                    *value = widened;
                    *scale = declared;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols() -> Vec<Column> {
        vec![
            Column::new("c0", "region", ColumnType::String).key(),
            Column::new("c1", "amount", ColumnType::Decimal)
                .scale(2)
                .unit("USD")
                .additive(),
        ]
    }

    #[test]
    fn a_minimal_rendering_widens_to_the_declared_scale() {
        use rust_decimal::Decimal;
        use std::str::FromStr as _;
        let mut r = TypedResult {
            schema: ResultSchema {
                columns: cols(),
                row_id_rule: RowIdRule::Keys,
                order_by: vec![],
            },
            rows: vec![Row {
                cells: vec![
                    crate::Value::String("EMEA".into()),
                    crate::Value::Decimal {
                        value: Decimal::from_str("900000.5").unwrap(),
                        scale: 1,
                    },
                ],
            }],
            truncated: false,
            denied_columns: vec![],
            authorization_class: Default::default(),
        };
        r.conform_decimal_scales().expect("widening is lossless");
        match &r.rows[0].cells[1] {
            crate::Value::Decimal { value, scale } => {
                assert_eq!(*scale, 2);
                assert_eq!(value.to_string(), "900000.50");
            }
            other => panic!("not a decimal: {other:?}"),
        }
    }

    #[test]
    fn excess_precision_is_a_refusal_never_a_rounding() {
        use rust_decimal::Decimal;
        use std::str::FromStr as _;
        let mut r = TypedResult {
            schema: ResultSchema {
                columns: cols(),
                row_id_rule: RowIdRule::Keys,
                order_by: vec![],
            },
            rows: vec![Row {
                cells: vec![
                    crate::Value::String("EMEA".into()),
                    crate::Value::Decimal {
                        value: Decimal::from_str("900000.505").unwrap(),
                        scale: 3,
                    },
                ],
            }],
            truncated: false,
            denied_columns: vec![],
            authorization_class: Default::default(),
        };
        let err = r
            .conform_decimal_scales()
            .expect_err("three digits into a scale-2 column");
        assert_eq!(err.code, "schema_drift");
        assert!(err.message.contains("amount"), "{}", err.message);
    }

    #[test]
    fn a_result_with_neither_keys_nor_ordering_is_refused() {
        let s = ResultSchema {
            columns: vec![Column::new("c0", "region", ColumnType::String)],
            row_id_rule: RowIdRule::Keys,
            order_by: vec![],
        };
        assert_eq!(s.validate(), Err(SchemaError::NotIdentifiable));

        let s = ResultSchema {
            columns: vec![Column::new("c0", "region", ColumnType::String)],
            row_id_rule: RowIdRule::Position,
            order_by: vec![],
        };
        assert_eq!(s.validate(), Err(SchemaError::NotIdentifiable));
    }

    #[test]
    fn a_nullable_key_identifies_nothing() {
        let s = ResultSchema {
            columns: vec![Column::new("c0", "region", ColumnType::String)
                .key()
                .nullable()],
            row_id_rule: RowIdRule::Keys,
            order_by: vec![],
        };
        assert_eq!(s.validate(), Err(SchemaError::NullableKey("region".into())));
    }

    #[test]
    fn a_decimal_without_a_scale_has_no_canonical_form() {
        let s = ResultSchema {
            columns: vec![
                Column::new("c0", "region", ColumnType::String).key(),
                Column::new("c1", "amount", ColumnType::Decimal),
            ],
            row_id_rule: RowIdRule::Keys,
            order_by: vec![],
        };
        assert_eq!(
            s.validate(),
            Err(SchemaError::DecimalWithoutScale("amount".into()))
        );
    }

    #[test]
    fn valid_schema_passes() {
        let s = ResultSchema {
            columns: cols(),
            row_id_rule: RowIdRule::Keys,
            order_by: vec![],
        };
        assert_eq!(s.validate(), Ok(()));
    }

    #[test]
    fn domination_needs_level_and_every_compartment() {
        let class = AuthorizationClass {
            name: Some("sales-emea".into()),
            access_level: 2,
            compartments: vec!["sales".into(), "emea".into()],
        };
        assert!(class.dominated_by(2, &["sales".into(), "emea".into(), "extra".into()]));
        assert!(
            !class.dominated_by(1, &["sales".into(), "emea".into()]),
            "level too low"
        );
        assert!(
            !class.dominated_by(3, &["sales".into()]),
            "missing compartment"
        );
        assert!(!class.dominated_by(3, &[]), "no compartments at all");
    }
}
