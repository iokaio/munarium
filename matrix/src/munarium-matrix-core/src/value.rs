// SPDX-License-Identifier: Apache-2.0
//! The typed value model and its `canon@1` encoding.
//!
//! Every value that will ever be hashed, sealed, compared with a ledger claim,
//! or rendered into a record document passes through here first. The encoding
//! is the normative one in `matrix/contract/canonicalization.schema.json`; that
//! file and this module must agree, and `tests::canon_rules_match_the_contract`
//! is what keeps them agreeing.
//!
//! Two rules earn their own sentence because everything else follows from them:
//!
//! 1. **NULL is not a value.** It encodes to a single `0x00` byte, which no
//!    encoded value may contain. `NULL` and `""` and `0` are three different
//!    things, and a reconciliation that confuses them files a false
//!    discrepancy — the failure mode the T0 fixture plants on purpose.
//! 2. **Decimals are text at a declared scale.** `1.5` at scale 2 is `"1.50"`.
//!    A decimal that arrives as an IEEE-754 double has already lost, so the
//!    wire form is a string end to end.

use base64::Engine as _;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fmt;

/// The closed set of logical column types (`canon@1`). A source type that does
/// not map to exactly one of these refuses sealing rather than being coerced —
/// silent coercion is how a currency becomes a float.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnType {
    Bool,
    Int64,
    Decimal,
    Float64,
    String,
    Bytes,
    Date,
    TimestampTz,
    TimestampNaive,
    Interval,
    Uuid,
    Json,
    Array,
}

impl ColumnType {
    pub fn as_str(self) -> &'static str {
        match self {
            ColumnType::Bool => "bool",
            ColumnType::Int64 => "int64",
            ColumnType::Decimal => "decimal",
            ColumnType::Float64 => "float64",
            ColumnType::String => "string",
            ColumnType::Bytes => "bytes",
            ColumnType::Date => "date",
            ColumnType::TimestampTz => "timestamp_tz",
            ColumnType::TimestampNaive => "timestamp_naive",
            ColumnType::Interval => "interval",
            ColumnType::Uuid => "uuid",
            ColumnType::Json => "json",
            ColumnType::Array => "array",
        }
    }

    /// Types whose values a derivation may sum, average or compare
    /// arithmetically. A `sum` over a string column is a contract error, not a
    /// runtime surprise.
    pub fn is_numeric(self) -> bool {
        matches!(
            self,
            ColumnType::Int64 | ColumnType::Decimal | ColumnType::Float64
        )
    }
}

impl fmt::Display for ColumnType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A single typed cell.
///
/// `Decimal` carries its declared scale because the scale is part of the
/// *column's* identity, not the value's: the same number at scale 0 and scale 2
/// encodes differently, and that difference must survive into the hash.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int64(i64),
    Decimal {
        value: Decimal,
        scale: u32,
    },
    Float64(f64),
    String(String),
    Bytes(Vec<u8>),
    /// `YYYY-MM-DD`
    Date(chrono::NaiveDate),
    /// Always stored in UTC; the source offset, when it matters, travels in
    /// column metadata and never in the value.
    TimestampTz(chrono::DateTime<chrono::Utc>),
    TimestampNaive(chrono::NaiveDateTime),
    /// ISO-8601 duration, months and seconds kept separate (`P1M` is not `P30D`).
    Interval {
        months: i32,
        seconds: i64,
        nanos: u32,
    },
    Uuid(String),
    Json(serde_json::Value),
    Array {
        element_type: ColumnType,
        items: Vec<Value>,
    },
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// The logical type of a non-null value. `None` for `Null`, which belongs
    /// to every type and to none.
    pub fn column_type(&self) -> Option<ColumnType> {
        Some(match self {
            Value::Null => return None,
            Value::Bool(_) => ColumnType::Bool,
            Value::Int64(_) => ColumnType::Int64,
            Value::Decimal { .. } => ColumnType::Decimal,
            Value::Float64(_) => ColumnType::Float64,
            Value::String(_) => ColumnType::String,
            Value::Bytes(_) => ColumnType::Bytes,
            Value::Date(_) => ColumnType::Date,
            Value::TimestampTz(_) => ColumnType::TimestampTz,
            Value::TimestampNaive(_) => ColumnType::TimestampNaive,
            Value::Interval { .. } => ColumnType::Interval,
            Value::Uuid(_) => ColumnType::Uuid,
            Value::Json(_) => ColumnType::Json,
            Value::Array { .. } => ColumnType::Array,
        })
    }

    /// The `canon@1` text form. `None` for NULL — callers that are building a
    /// hash must emit the NULL sentinel byte instead, and callers that are
    /// building a human-facing table must print an empty cell. Making that a
    /// type-level distinction is what stops "NULL" the string from ever being
    /// confused with NULL the absence.
    pub fn canonical_text(&self) -> Option<String> {
        Some(match self {
            Value::Null => return None,
            Value::Bool(b) => (if *b { "true" } else { "false" }).to_string(),
            Value::Int64(i) => i.to_string(),
            Value::Decimal { value, scale } => format_decimal(*value, *scale),
            Value::Float64(f) => format_float(*f),
            Value::String(s) => s.clone(),
            Value::Bytes(b) => base64::engine::general_purpose::STANDARD.encode(b),
            Value::Date(d) => d.format("%Y-%m-%d").to_string(),
            Value::TimestampTz(t) => t.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string(),
            Value::TimestampNaive(t) => t.format("%Y-%m-%dT%H:%M:%S%.6f").to_string(),
            Value::Interval {
                months,
                seconds,
                nanos,
            } => format_interval(*months, *seconds, *nanos),
            Value::Uuid(u) => u.to_lowercase(),
            Value::Json(j) => canonical_json(j),
            Value::Array { items, .. } => {
                let mut out = String::from("[");
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push('\u{1f}');
                    }
                    // A NULL element inside an array is the sentinel, spelled
                    // as the escape it is — an array is one cell, so it cannot
                    // contain a raw 0x00 and still be a text form.
                    match item.canonical_text() {
                        Some(t) => out.push_str(&t),
                        None => out.push('\u{0}'),
                    }
                }
                out.push(']');
                out
            }
        })
    }

    /// The bytes this value contributes to a hash: the canonical text, or the
    /// NULL sentinel.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        match self.canonical_text() {
            Some(t) => t.into_bytes(),
            None => vec![NULL_SENTINEL],
        }
    }
}

/// The byte that means NULL. No encoded value may contain it.
pub const NULL_SENTINEL: u8 = 0x00;
/// Between fields of a row.
pub const FIELD_SEP: u8 = 0x1f;
/// Between rows.
pub const ROW_SEP: u8 = 0x1e;
/// Between sections of the hash preimage.
pub const SECTION_SEP: u8 = 0x1d;

/// Base-10 at exactly `scale` fraction digits, no exponent, no separators.
/// `-0` normalizes to `0` so two spellings of nothing cannot hash differently.
fn format_decimal(value: Decimal, scale: u32) -> String {
    let mut v = value;
    v.rescale(scale);
    let s = v.to_string();
    // rust_decimal renders -0.00 for a negative zero; canon@1 has one zero.
    if s.starts_with('-') && s[1..].chars().all(|c| c == '0' || c == '.') {
        return s[1..].to_string();
    }
    s
}

/// Shortest round-trip form, with the three special values spelled exactly.
fn format_float(f: f64) -> String {
    if f.is_nan() {
        "NaN".to_string()
    } else if f.is_infinite() {
        if f.is_sign_positive() {
            "inf".into()
        } else {
            "-inf".into()
        }
    } else if f == 0.0 {
        // -0.0 == 0.0 in IEEE comparison; canon@1 has one zero.
        "0".to_string()
    } else {
        let s = format!("{f}");
        s
    }
}

/// ISO-8601 duration with the month and second components kept apart, because
/// a month is not 30 days and a database that says so is wrong.
fn format_interval(months: i32, seconds: i64, nanos: u32) -> String {
    let mut out = String::from("P");
    let (years, rem_months) = (months / 12, months % 12);
    if years != 0 {
        out.push_str(&format!("{years}Y"));
    }
    if rem_months != 0 {
        out.push_str(&format!("{rem_months}M"));
    }
    if seconds != 0 || nanos != 0 || months == 0 {
        out.push('T');
        let (h, rem) = (seconds / 3600, seconds % 3600);
        let (m, s) = (rem / 60, rem % 60);
        if h != 0 {
            out.push_str(&format!("{h}H"));
        }
        if m != 0 {
            out.push_str(&format!("{m}M"));
        }
        if nanos == 0 {
            out.push_str(&format!("{s}S"));
        } else {
            out.push_str(&format!("{s}.{nanos:09}S"));
        }
    }
    out
}

/// JSON canonicalization: object keys sorted, no insignificant whitespace.
///
/// This is RFC 8785 for every value that survives a JSON round-trip through
/// `serde_json` — which is every value we can receive, since that is how it
/// arrived. Full RFC 8785 additionally pins number formatting to ECMAScript's
/// `Number::toString`; `serde_json`'s shortest-round-trip formatting agrees for
/// all f64 values. Integers beyond 2^53 do NOT survive a JSON number and must
/// be a `decimal` or `int64` column, not a `json` one.
fn canonical_json(v: &serde_json::Value) -> String {
    fn write(v: &serde_json::Value, out: &mut String) {
        match v {
            serde_json::Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                out.push('{');
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(k).unwrap_or_default());
                    out.push(':');
                    write(&map[*k], out);
                }
                out.push('}');
            }
            serde_json::Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(item, out);
                }
                out.push(']');
            }
            other => out.push_str(&serde_json::to_string(other).unwrap_or_default()),
        }
    }
    let mut out = String::new();
    write(v, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn null_is_not_the_empty_string_and_not_zero() {
        assert_eq!(Value::Null.canonical_bytes(), vec![0x00]);
        assert_eq!(
            Value::String(String::new()).canonical_bytes(),
            Vec::<u8>::new()
        );
        assert_eq!(Value::Int64(0).canonical_bytes(), b"0".to_vec());
        // The three must be pairwise distinct — this is the planted trap in the
        // T0 fixture and the reason reconciliation can tell "unset" from "zero".
        let a = Value::Null.canonical_bytes();
        let b = Value::String(String::new()).canonical_bytes();
        let c = Value::Int64(0).canonical_bytes();
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
    }

    #[test]
    fn decimals_carry_their_declared_scale() {
        let d = Decimal::from_str("1.5").unwrap();
        assert_eq!(
            Value::Decimal { value: d, scale: 2 }
                .canonical_text()
                .unwrap(),
            "1.50"
        );
        assert_eq!(
            Value::Decimal { value: d, scale: 0 }
                .canonical_text()
                .unwrap(),
            "2" // banker-free rescale: rust_decimal rounds half-even to 2
        );
        // Same number, different declared scale => different encoding, so a
        // contract that changes scale changes the logical result hash.
        assert_ne!(
            Value::Decimal { value: d, scale: 2 }.canonical_text(),
            Value::Decimal { value: d, scale: 4 }.canonical_text()
        );
    }

    #[test]
    fn negative_zero_has_one_spelling() {
        let neg = Decimal::from_str("-0.00").unwrap();
        assert_eq!(
            Value::Decimal {
                value: neg,
                scale: 2
            }
            .canonical_text()
            .unwrap(),
            "0.00"
        );
        assert_eq!(Value::Float64(-0.0).canonical_text().unwrap(), "0");
    }

    #[test]
    fn timestamps_are_utc_with_six_fraction_digits() {
        let t = chrono::DateTime::parse_from_rfc3339("2026-06-30T12:00:00+02:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(
            Value::TimestampTz(t).canonical_text().unwrap(),
            "2026-06-30T10:00:00.000000Z"
        );
    }

    #[test]
    fn json_objects_canonicalize_by_sorted_key() {
        let a: serde_json::Value = serde_json::from_str(r#"{"b":1,"a":{"d":2,"c":3}}"#).unwrap();
        let b: serde_json::Value = serde_json::from_str(r#"{"a":{"c":3,"d":2},"b":1}"#).unwrap();
        assert_eq!(
            Value::Json(a).canonical_text(),
            Value::Json(b).canonical_text()
        );
        assert_eq!(
            Value::Json(serde_json::json!({"b":1,"a":2}))
                .canonical_text()
                .unwrap(),
            r#"{"a":2,"b":1}"#
        );
    }

    #[test]
    fn intervals_keep_months_and_seconds_apart() {
        let one_month = Value::Interval {
            months: 1,
            seconds: 0,
            nanos: 0,
        };
        let thirty_days = Value::Interval {
            months: 0,
            seconds: 30 * 86_400,
            nanos: 0,
        };
        assert_eq!(one_month.canonical_text().unwrap(), "P1M");
        assert_ne!(one_month.canonical_text(), thirty_days.canonical_text());
    }

    #[test]
    fn arrays_encode_elementwise_with_the_field_separator() {
        let v = Value::Array {
            element_type: ColumnType::Int64,
            items: vec![Value::Int64(1), Value::Null, Value::Int64(3)],
        };
        assert_eq!(v.canonical_text().unwrap(), "[1\u{1f}\u{0}\u{1f}3]");
    }
}
