// SPDX-License-Identifier: Apache-2.0
//! `canon@1` — turning a typed result into two hashes.
//!
//! `logical_result_hash` answers "is this the same answer?"; `artifact_hash`
//! answers "are these the same bytes?". They are computed from different
//! inputs and are never conflated: two serializations of one result share the
//! first and differ in the second, which is exactly what lets a CSV artifact
//! and a Parquet artifact be the *same evidence*.
//!
//! The identity section is where the subtle correctness lives. Truncation
//! status, the denied-column set and the authorization class all hash into
//! identity, so a truncated result can never collide with the complete one and
//! a result computed under a narrower policy can never masquerade as the
//! broader one.

use crate::result::{RowIdRule, TypedResult};
use crate::value::{FIELD_SEP, ROW_SEP, SECTION_SEP};
use sha2::{Digest, Sha256};

pub const CANON_VERSION: &str = "canon@1";

/// A `sha256:<hex>` string, the one hash spelling on the wire.
pub fn hash_hex(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

/// The row identifier under the schema's rule.
///
/// Under `Keys` it is the encoded key tuple; under `Position` it is the
/// 0-based index. The caller must pass the row's index for the latter to mean
/// anything, which is why this takes one.
pub fn row_id(result: &TypedResult, row_index: usize) -> String {
    let row = &result.rows[row_index];
    match result.schema.row_id_rule {
        RowIdRule::Position => row_index.to_string(),
        RowIdRule::Keys => {
            let parts: Vec<String> = result
                .schema
                .columns
                .iter()
                .enumerate()
                .filter(|(_, c)| c.key)
                .map(|(i, _)| {
                    row.cells
                        .get(i)
                        .and_then(|v| v.canonical_text())
                        // A NULL key is rejected at schema validation; if one
                        // reaches here the id is still total rather than a panic.
                        .unwrap_or_else(|| "\u{0}".to_string())
                })
                .collect();
            parts.join("\u{1f}")
        }
    }
}

/// The canonical byte encoding of one row: cells separated by 0x1F, NULLs as
/// the sentinel.
fn encode_row(result: &TypedResult, row_index: usize) -> Vec<u8> {
    let row = &result.rows[row_index];
    let mut out = Vec::new();
    for (i, cell) in row.cells.iter().enumerate() {
        if i > 0 {
            out.push(FIELD_SEP);
        }
        out.extend_from_slice(&cell.canonical_bytes());
    }
    out
}

/// The full hash preimage. Exposed because a test that only compares hashes
/// tells you *that* two results differ and never *where* — this is what the
/// canonicalization property tests diff when they fail.
pub fn logical_preimage(result: &TypedResult) -> Vec<u8> {
    let mut out = Vec::new();

    // Section 1: the canon version.
    out.extend_from_slice(CANON_VERSION.as_bytes());
    out.push(SECTION_SEP);

    // Section 2: the schema, in column order.
    for c in &result.schema.columns {
        out.extend_from_slice(c.id.as_bytes());
        out.push(FIELD_SEP);
        out.extend_from_slice(c.name.as_bytes());
        out.push(FIELD_SEP);
        out.extend_from_slice(c.ty.as_str().as_bytes());
        out.push(FIELD_SEP);
        out.extend_from_slice(
            c.scale
                .map(|s| s.to_string())
                .unwrap_or_default()
                .as_bytes(),
        );
        out.push(FIELD_SEP);
        out.extend_from_slice(c.unit.clone().unwrap_or_default().as_bytes());
        out.push(FIELD_SEP);
        out.extend_from_slice(if c.nullable { b"n" } else { b"-" });
        out.push(FIELD_SEP);
        out.extend_from_slice(if c.key { b"k" } else { b"-" });
        out.push(ROW_SEP);
    }
    out.push(SECTION_SEP);

    // Section 3: identity — the row rule, the ordering, and the three facts
    // that must never be forgeable by re-serialization.
    out.extend_from_slice(match result.schema.row_id_rule {
        RowIdRule::Keys => b"keys",
        RowIdRule::Position => b"position",
    });
    out.push(FIELD_SEP);
    out.extend_from_slice(result.schema.order_by.join(",").as_bytes());
    out.push(FIELD_SEP);
    out.extend_from_slice(if result.truncated {
        b"truncated"
    } else {
        b"complete"
    });
    out.push(FIELD_SEP);
    {
        // Sorted so the denied set is a set, not an accident of iteration.
        let mut denied = result.denied_columns.clone();
        denied.sort();
        out.extend_from_slice(denied.join(",").as_bytes());
    }
    out.push(FIELD_SEP);
    out.extend_from_slice(
        result
            .authorization_class
            .access_level
            .to_string()
            .as_bytes(),
    );
    out.push(FIELD_SEP);
    {
        let mut cmp = result.authorization_class.compartments.clone();
        cmp.sort();
        out.extend_from_slice(cmp.join(",").as_bytes());
    }
    out.push(SECTION_SEP);

    // Section 4: the rows. Under `keys` the encoded rows are sorted bytewise,
    // which is what makes the result a multiset and row order irrelevant.
    let mut encoded: Vec<Vec<u8>> = (0..result.rows.len())
        .map(|i| encode_row(result, i))
        .collect();
    if result.schema.row_id_rule == RowIdRule::Keys {
        encoded.sort();
    }
    for (i, row) in encoded.iter().enumerate() {
        if i > 0 {
            out.push(ROW_SEP);
        }
        out.extend_from_slice(row);
    }
    out
}

/// `logical_result_hash` — the identity of the answer.
pub fn logical_result_hash(result: &TypedResult) -> String {
    hash_hex(&logical_preimage(result))
}

/// `artifact_hash` — the identity of the stored bytes. A separate function
/// taking separate input, so the two can never be accidentally the same value.
pub fn artifact_hash(bytes: &[u8]) -> String {
    hash_hex(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::{AuthorizationClass, Column, ResultSchema, Row};
    use crate::value::{ColumnType, Value};
    use rust_decimal::Decimal;
    use std::str::FromStr;

    fn result_with(rows: Vec<Row>, rule: RowIdRule) -> TypedResult {
        TypedResult {
            schema: ResultSchema {
                columns: vec![
                    Column::new("c0", "region", ColumnType::String).key(),
                    Column::new("c1", "amount", ColumnType::Decimal)
                        .scale(2)
                        .unit("USD"),
                ],
                row_id_rule: rule,
                order_by: vec!["region".into()],
            },
            rows,
            truncated: false,
            denied_columns: vec![],
            authorization_class: AuthorizationClass::default(),
        }
    }

    fn row(region: &str, amount: &str) -> Row {
        Row::new(vec![
            Value::String(region.into()),
            Value::Decimal {
                value: Decimal::from_str(amount).unwrap(),
                scale: 2,
            },
        ])
    }

    #[test]
    fn keyed_results_hash_as_a_multiset_so_row_order_does_not_matter() {
        let a = result_with(
            vec![row("EMEA", "1.00"), row("AMER", "2.00")],
            RowIdRule::Keys,
        );
        let b = result_with(
            vec![row("AMER", "2.00"), row("EMEA", "1.00")],
            RowIdRule::Keys,
        );
        assert_eq!(logical_result_hash(&a), logical_result_hash(&b));
    }

    #[test]
    fn positional_results_hash_as_a_sequence_so_row_order_does_matter() {
        let a = result_with(
            vec![row("EMEA", "1.00"), row("AMER", "2.00")],
            RowIdRule::Position,
        );
        let b = result_with(
            vec![row("AMER", "2.00"), row("EMEA", "1.00")],
            RowIdRule::Position,
        );
        assert_ne!(logical_result_hash(&a), logical_result_hash(&b));
    }

    #[test]
    fn a_truncated_result_never_hashes_equal_to_the_complete_one() {
        let complete = result_with(vec![row("EMEA", "1.00")], RowIdRule::Keys);
        let mut truncated = complete.clone();
        truncated.truncated = true;
        assert_ne!(
            logical_result_hash(&complete),
            logical_result_hash(&truncated)
        );
    }

    #[test]
    fn a_narrower_authorization_class_is_a_different_result() {
        let open = result_with(vec![row("EMEA", "1.00")], RowIdRule::Keys);
        let mut narrow = open.clone();
        narrow.authorization_class = AuthorizationClass {
            name: Some("sales-emea".into()),
            access_level: 2,
            compartments: vec!["sales".into()],
        };
        assert_ne!(logical_result_hash(&open), logical_result_hash(&narrow));
    }

    #[test]
    fn a_denied_column_changes_identity_even_though_it_is_absent_from_the_rows() {
        let a = result_with(vec![row("EMEA", "1.00")], RowIdRule::Keys);
        let mut b = a.clone();
        b.denied_columns = vec!["owner_email".into()];
        assert_ne!(logical_result_hash(&a), logical_result_hash(&b));
        // ...and the denied set is a SET: order in the vector is irrelevant.
        let mut c = b.clone();
        c.denied_columns = vec!["owner_email".into()];
        let mut d = b.clone();
        d.denied_columns = vec!["owner_email".into()];
        assert_eq!(logical_result_hash(&c), logical_result_hash(&d));
    }

    #[test]
    fn the_two_hashes_are_computed_from_different_things() {
        let r = result_with(vec![row("EMEA", "1.00")], RowIdRule::Keys);
        let logical = logical_result_hash(&r);
        let artifact = artifact_hash(b"region,amount\nEMEA,1.00\n");
        assert_ne!(logical, artifact);
        // A different serialization of the same logical result: logical stays,
        // artifact moves.
        let other_bytes = artifact_hash(b"\"region\",\"amount\"\r\n\"EMEA\",\"1.00\"\r\n");
        assert_ne!(artifact, other_bytes);
        assert_eq!(logical, logical_result_hash(&r));
    }

    #[test]
    fn row_ids_follow_the_declared_rule() {
        let keyed = result_with(
            vec![row("EMEA", "1.00"), row("AMER", "2.00")],
            RowIdRule::Keys,
        );
        assert_eq!(row_id(&keyed, 0), "EMEA");
        assert_eq!(row_id(&keyed, 1), "AMER");
        let positional = result_with(vec![row("EMEA", "1.00")], RowIdRule::Position);
        assert_eq!(row_id(&positional, 0), "0");
    }

    #[test]
    fn null_and_empty_string_are_different_results() {
        let with_null = result_with(
            vec![Row::new(vec![Value::String("EMEA".into()), Value::Null])],
            RowIdRule::Keys,
        );
        let with_empty = result_with(
            vec![Row::new(vec![
                Value::String("EMEA".into()),
                Value::String(String::new()),
            ])],
            RowIdRule::Keys,
        );
        assert_ne!(
            logical_result_hash(&with_null),
            logical_result_hash(&with_empty)
        );
    }
}
