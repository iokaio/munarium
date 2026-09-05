// SPDX-License-Identifier: Apache-2.0
//! Typed parameter binding.
//!
//! The property this module exists to guarantee, and which the tests assert
//! directly: **no bound value ever appears in statement text.** Parameters are
//! validated against the contract's declared types and allowed sets, converted
//! to typed values, and handed to the driver as placeholders. String
//! concatenation is not an implementation.

use munarium_matrix_core::value::{ColumnType, Value};
use munarium_matrix_core::{Refusal, RefusalClass};
use munarium_matrix_types::assets::ParameterSpec;
use munarium_matrix_types::contract::TypedValueDto;
use rust_decimal::Decimal;
use std::collections::BTreeMap;
use std::str::FromStr;

/// Parameters bound in the order the statement's placeholders expect them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BoundParameters {
    /// `$1..$n` in order.
    pub positional: Vec<Value>,
    /// Name -> position, for the compiler's placeholder rewrite.
    pub index: BTreeMap<String, usize>,
}

impl BoundParameters {
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.index.get(name).and_then(|i| self.positional.get(*i))
    }

    /// A hash over the bound values — part of evidence provenance, so a
    /// different parameter binding is a different logical result.
    pub fn hash(&self) -> String {
        let mut buf = Vec::new();
        for (name, idx) in &self.index {
            buf.extend_from_slice(name.as_bytes());
            buf.push(0x1f);
            if let Some(v) = self.positional.get(*idx) {
                buf.extend_from_slice(&v.canonical_bytes());
            }
            buf.push(0x1e);
        }
        munarium_matrix_core::canon::hash_hex(&buf)
    }
}

/// Convert one wire value to a typed [`Value`], refusing anything that does
/// not match the declared type exactly. No coercion: a string where a date was
/// declared is `invalid`, not a parse attempt.
pub fn convert(
    name: &str,
    dto: &TypedValueDto,
    declared: ColumnType,
    scale: Option<u32>,
) -> Result<Value, Refusal> {
    let bad = |why: &str| {
        Refusal::new(
            RefusalClass::Invalid,
            "not_covered",
            format!("parameter '{name}': {why}"),
        )
    };
    if dto.ty != declared {
        return Err(bad(&format!("declared as {declared}, received {}", dto.ty)));
    }
    if dto.value.is_null() {
        return Ok(Value::Null);
    }
    let as_str = || -> Result<String, Refusal> {
        dto.value
            .as_str()
            .map(String::from)
            // A JSON number for a decimal is the classic precision loss; accept
            // it only for types where a double is lossless.
            .or_else(|| match declared {
                ColumnType::Int64 | ColumnType::Float64 => Some(dto.value.to_string()),
                _ => None,
            })
            .ok_or_else(|| bad("expected a JSON string (exact types travel as text)"))
    };

    Ok(match declared {
        ColumnType::Bool => Value::Bool(
            dto.value
                .as_bool()
                .ok_or_else(|| bad("expected a JSON boolean"))?,
        ),
        ColumnType::Int64 => Value::Int64(
            as_str()?
                .parse::<i64>()
                .map_err(|e| bad(&format!("not an int64: {e}")))?,
        ),
        ColumnType::Decimal => {
            let scale = scale.ok_or_else(|| bad("decimal parameter has no declared scale"))?;
            let d =
                Decimal::from_str(&as_str()?).map_err(|e| bad(&format!("not a decimal: {e}")))?;
            Value::Decimal { value: d, scale }
        }
        ColumnType::Float64 => Value::Float64(
            as_str()?
                .parse::<f64>()
                .map_err(|e| bad(&format!("not a float: {e}")))?,
        ),
        ColumnType::String => Value::String(as_str()?),
        ColumnType::Bytes => {
            use base64::Engine as _;
            Value::Bytes(
                base64::engine::general_purpose::STANDARD
                    .decode(as_str()?)
                    .map_err(|e| bad(&format!("not base64: {e}")))?,
            )
        }
        ColumnType::Date => Value::Date(
            chrono::NaiveDate::parse_from_str(&as_str()?, "%Y-%m-%d")
                .map_err(|e| bad(&format!("not an ISO date: {e}")))?,
        ),
        ColumnType::TimestampTz => Value::TimestampTz(
            chrono::DateTime::parse_from_rfc3339(&as_str()?)
                .map_err(|e| bad(&format!("not RFC 3339: {e}")))?
                .with_timezone(&chrono::Utc),
        ),
        ColumnType::TimestampNaive => Value::TimestampNaive(
            chrono::NaiveDateTime::parse_from_str(&as_str()?, "%Y-%m-%dT%H:%M:%S%.f")
                .map_err(|e| bad(&format!("not a naive timestamp: {e}")))?,
        ),
        ColumnType::Uuid => {
            let s = as_str()?;
            if s.len() != 36 || !s.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
                return Err(bad("not a hyphenated uuid"));
            }
            Value::Uuid(s.to_lowercase())
        }
        ColumnType::Json => Value::Json(dto.value.clone()),
        ColumnType::Interval | ColumnType::Array => {
            return Err(bad("this type is not supported as a parameter"))
        }
    })
}

/// Validate and bind every parameter the contract declares.
///
/// Three refusals live here, and each is a real attack or a real bug:
/// an **undeclared** parameter (someone is trying to reach a knob the contract
/// does not expose), a **missing required** one, and a value **outside the
/// declared set**.
/// Bind values under caller-chosen names with declared types — the semantic
/// path, where a filter's placeholder is `:fN` and its type
/// is the dimension's. Same conversion as a contract parameter, same refusal
/// on a value that does not fit the type; no allowed-set check, because a
/// filter value is compared by the engine, never used to choose a statement.
pub fn bind_named(
    values: &[(String, TypedValueDto, ColumnType, Option<u32>)],
) -> Result<BoundParameters, Refusal> {
    let mut out = BoundParameters::default();
    for (name, dto, ty, scale) in values {
        let value = convert(name, dto, *ty, *scale)?;
        let idx = out.positional.len();
        out.positional.push(value);
        out.index.insert(name.clone(), idx);
    }
    Ok(out)
}

pub fn bind_parameters(
    declared: &BTreeMap<String, ParameterSpec>,
    supplied: &BTreeMap<String, TypedValueDto>,
    pinned_domains: &BTreeMap<String, Vec<String>>,
) -> Result<BoundParameters, Refusal> {
    for name in supplied.keys() {
        if !declared.contains_key(name) {
            return Err(Refusal::not_covered(format!(
                "parameter '{name}' is not declared by this contract"
            )));
        }
    }

    let mut out = BoundParameters::default();
    // Deterministic order: the compiler's placeholder numbering must not
    // depend on map iteration.
    for (name, spec) in declared {
        match supplied.get(name) {
            None if spec.required => {
                return Err(Refusal::new(
                    RefusalClass::Invalid,
                    "not_covered",
                    format!("required parameter '{name}' was not supplied"),
                ))
            }
            None => continue,
            Some(dto) => {
                let value = convert(name, dto, spec.ty, spec.scale)?;
                // Allowed sets: the inline list, or the domain pinned at
                // introspect time. Compared against the CANONICAL text so
                // "1.50" and "1.5" cannot disagree.
                let allowed = spec
                    .allowed_values
                    .as_ref()
                    .cloned()
                    .or_else(|| pinned_domains.get(name).cloned());
                if let Some(allowed) = allowed {
                    let text = value.canonical_text().unwrap_or_default();
                    if !allowed.iter().any(|a| a == &text) {
                        return Err(Refusal::not_covered(format!(
                            "parameter '{name}' value is outside the declared allowed set"
                        )));
                    }
                }
                let idx = out.positional.len();
                out.positional.push(value);
                out.index.insert(name.clone(), idx);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use munarium_matrix_types::assets::ParameterSpec;

    fn spec(ty: ColumnType, required: bool) -> ParameterSpec {
        ParameterSpec {
            ty,
            required,
            scale: None,
            allowed_values: None,
            allowed_values_from: None,
        }
    }

    fn dto(ty: ColumnType, v: serde_json::Value) -> TypedValueDto {
        TypedValueDto {
            ty,
            value: v,
            scale: None,
            element_type: None,
        }
    }

    #[test]
    fn an_undeclared_parameter_is_refused() {
        let declared = BTreeMap::from([("as_of".to_string(), spec(ColumnType::Date, true))]);
        let supplied = BTreeMap::from([
            (
                "as_of".to_string(),
                dto(ColumnType::Date, serde_json::json!("2026-06-30")),
            ),
            (
                "secret_knob".to_string(),
                dto(ColumnType::String, serde_json::json!("x")),
            ),
        ]);
        let err = bind_parameters(&declared, &supplied, &BTreeMap::new()).unwrap_err();
        assert!(err.message.contains("secret_knob"), "{}", err.message);
    }

    #[test]
    fn a_missing_required_parameter_is_refused() {
        let declared = BTreeMap::from([("as_of".to_string(), spec(ColumnType::Date, true))]);
        let err = bind_parameters(&declared, &BTreeMap::new(), &BTreeMap::new()).unwrap_err();
        assert!(err.message.contains("as_of"));
    }

    #[test]
    fn a_type_mismatch_is_refused_never_coerced() {
        let declared = BTreeMap::from([("as_of".to_string(), spec(ColumnType::Date, true))]);
        let supplied = BTreeMap::from([(
            "as_of".to_string(),
            dto(ColumnType::String, serde_json::json!("2026-06-30")),
        )]);
        let err = bind_parameters(&declared, &supplied, &BTreeMap::new()).unwrap_err();
        assert!(err.message.contains("declared as date"), "{}", err.message);
    }

    #[test]
    fn a_value_outside_the_pinned_domain_is_not_covered() {
        let declared = BTreeMap::from([("region".to_string(), spec(ColumnType::String, true))]);
        let supplied = BTreeMap::from([(
            "region".to_string(),
            dto(ColumnType::String, serde_json::json!("MARS")),
        )]);
        let domains = BTreeMap::from([(
            "region".to_string(),
            vec!["EMEA".to_string(), "AMER".to_string()],
        )]);
        let err = bind_parameters(&declared, &supplied, &domains).unwrap_err();
        assert_eq!(err.code, "not_covered");
        // ...and a value inside it binds.
        let ok = BTreeMap::from([(
            "region".to_string(),
            dto(ColumnType::String, serde_json::json!("EMEA")),
        )]);
        assert!(bind_parameters(&declared, &ok, &domains).is_ok());
    }

    #[test]
    fn a_decimal_parameter_keeps_its_precision() {
        let mut s = spec(ColumnType::Decimal, true);
        s.scale = Some(2);
        let declared = BTreeMap::from([("amount".to_string(), s)]);
        let supplied = BTreeMap::from([(
            "amount".to_string(),
            dto(ColumnType::Decimal, serde_json::json!("12345678901234.55")),
        )]);
        let bound = bind_parameters(&declared, &supplied, &BTreeMap::new()).unwrap();
        assert_eq!(
            bound.get("amount").unwrap().canonical_text().unwrap(),
            "12345678901234.55",
            "a decimal that went through an f64 would not survive this"
        );
    }

    #[test]
    fn binding_order_is_deterministic_so_placeholders_are_stable() {
        let declared = BTreeMap::from([
            ("b".to_string(), spec(ColumnType::String, true)),
            ("a".to_string(), spec(ColumnType::String, true)),
        ]);
        let supplied = BTreeMap::from([
            (
                "a".to_string(),
                dto(ColumnType::String, serde_json::json!("1")),
            ),
            (
                "b".to_string(),
                dto(ColumnType::String, serde_json::json!("2")),
            ),
        ]);
        let bound = bind_parameters(&declared, &supplied, &BTreeMap::new()).unwrap();
        // BTreeMap order: a before b, every time, on every platform.
        assert_eq!(bound.index["a"], 0);
        assert_eq!(bound.index["b"], 1);
    }

    #[test]
    fn the_parameter_hash_distinguishes_different_bindings() {
        let declared = BTreeMap::from([("region".to_string(), spec(ColumnType::String, true))]);
        let emea = BTreeMap::from([(
            "region".to_string(),
            dto(ColumnType::String, serde_json::json!("EMEA")),
        )]);
        let amer = BTreeMap::from([(
            "region".to_string(),
            dto(ColumnType::String, serde_json::json!("AMER")),
        )]);
        let a = bind_parameters(&declared, &emea, &BTreeMap::new())
            .unwrap()
            .hash();
        let b = bind_parameters(&declared, &amer, &BTreeMap::new())
            .unwrap()
            .hash();
        assert_ne!(a, b, "different parameters must be different evidence");
    }
}
