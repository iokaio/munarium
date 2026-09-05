// SPDX-License-Identifier: Apache-2.0
//! Conversions between the contract types and the `matrix.v1` messages.
//!
//! Every conversion here is total in the proto → contract direction only
//! where the contract type is; an `UNSPECIFIED` enum or a missing required
//! message is an error, never a default, because a default would be a value
//! the caller did not send.

use crate::v1 as pb;
use munarium_matrix_core::derivation::ComputedDerivation;
use munarium_matrix_core::{Refusal, RefusalClass};
use munarium_matrix_types::contract as c;

/// A conversion failure: the message did not carry a valid contract value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvertError(pub String);

impl std::fmt::Display for ConvertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConvertError {}

fn err(m: impl Into<String>) -> ConvertError {
    ConvertError(m.into())
}

// ---------------------------------------------------------------------------
// JSON <-> google.protobuf.Value
// ---------------------------------------------------------------------------

pub fn json_to_pb(v: &serde_json::Value) -> prost_types::Value {
    use prost_types::value::Kind;
    let kind = match v {
        serde_json::Value::Null => Kind::NullValue(0),
        serde_json::Value::Bool(b) => Kind::BoolValue(*b),
        // A JSON number becomes a double. The contract never puts an exact
        // value in a bare number — decimals travel as strings — so this is
        // the lossy path only for values that were already inexact.
        serde_json::Value::Number(n) => Kind::NumberValue(n.as_f64().unwrap_or(f64::NAN)),
        serde_json::Value::String(s) => Kind::StringValue(s.clone()),
        serde_json::Value::Array(a) => Kind::ListValue(prost_types::ListValue {
            values: a.iter().map(json_to_pb).collect(),
        }),
        serde_json::Value::Object(o) => Kind::StructValue(json_to_struct_map(o)),
    };
    prost_types::Value { kind: Some(kind) }
}

fn json_to_struct_map(o: &serde_json::Map<String, serde_json::Value>) -> prost_types::Struct {
    prost_types::Struct {
        fields: o.iter().map(|(k, v)| (k.clone(), json_to_pb(v))).collect(),
    }
}

pub fn json_to_struct(v: &serde_json::Value) -> Result<prost_types::Struct, ConvertError> {
    match v {
        serde_json::Value::Object(o) => Ok(json_to_struct_map(o)),
        other => Err(err(format!("expected a JSON object, got {other}"))),
    }
}

pub fn pb_to_json(v: &prost_types::Value) -> serde_json::Value {
    use prost_types::value::Kind;
    match &v.kind {
        None | Some(Kind::NullValue(_)) => serde_json::Value::Null,
        Some(Kind::BoolValue(b)) => serde_json::Value::Bool(*b),
        // `google.protobuf.Value` has ONE number kind, a double. A JSON
        // integer crosses as `2.0`, and serde will not read `2.0` into an
        // `i32` — the manifest's `scale: 2` came back "floating point `2.0`,
        // expected i32" on the first drift run. A whole double inside the
        // exactly-representable range was an integer on the way in and is
        // an integer on the way out; nothing in the contract puts an exact
        // value in a bare number (decimals are strings), so this loses
        // nothing that was not already inexact.
        Some(Kind::NumberValue(n)) => {
            if n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 {
                serde_json::Value::Number(serde_json::Number::from(*n as i64))
            } else {
                serde_json::Number::from_f64(*n)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null)
            }
        }
        Some(Kind::StringValue(s)) => serde_json::Value::String(s.clone()),
        Some(Kind::ListValue(l)) => {
            serde_json::Value::Array(l.values.iter().map(pb_to_json).collect())
        }
        Some(Kind::StructValue(s)) => struct_to_json(s),
    }
}

pub fn struct_to_json(s: &prost_types::Struct) -> serde_json::Value {
    serde_json::Value::Object(
        s.fields
            .iter()
            .map(|(k, v)| (k.clone(), pb_to_json(v)))
            .collect(),
    )
}

fn ts_to_chrono(t: &prost_types::Timestamp) -> Result<chrono::DateTime<chrono::Utc>, ConvertError> {
    chrono::DateTime::from_timestamp(t.seconds, t.nanos.max(0) as u32).ok_or_else(|| {
        err(format!(
            "timestamp out of range: {}s {}ns",
            t.seconds, t.nanos
        ))
    })
}

fn chrono_to_ts(d: &chrono::DateTime<chrono::Utc>) -> prost_types::Timestamp {
    prost_types::Timestamp {
        seconds: d.timestamp(),
        nanos: d.timestamp_subsec_nanos() as i32,
    }
}

/// A serde-named enum value as its wire string (`"decimal"`, `"sum"`).
fn serde_name<T: serde::Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => s,
        other => format!("{other:?}"),
    }
}

fn serde_parse<T: serde::de::DeserializeOwned>(s: &str, what: &str) -> Result<T, ConvertError> {
    serde_json::from_value(serde_json::Value::String(s.to_string()))
        .map_err(|e| err(format!("{what} '{s}' is not a contract value: {e}")))
}

// ---------------------------------------------------------------------------
// TypedValue
// ---------------------------------------------------------------------------

impl From<&c::TypedValueDto> for pb::TypedValue {
    fn from(t: &c::TypedValueDto) -> Self {
        pb::TypedValue {
            r#type: serde_name(&t.ty),
            value: Some(json_to_pb(&t.value)),
            scale: t.scale,
            element_type: t.element_type.as_ref().map(serde_name),
        }
    }
}

impl TryFrom<&pb::TypedValue> for c::TypedValueDto {
    type Error = ConvertError;
    fn try_from(t: &pb::TypedValue) -> Result<Self, ConvertError> {
        Ok(c::TypedValueDto {
            ty: serde_parse(&t.r#type, "type")?,
            value: t
                .value
                .as_ref()
                .map(pb_to_json)
                .unwrap_or(serde_json::Value::Null),
            scale: t.scale,
            element_type: t
                .element_type
                .as_deref()
                .map(|s| serde_parse(s, "element_type"))
                .transpose()?,
        })
    }
}

// ---------------------------------------------------------------------------
// QueryIntent
// ---------------------------------------------------------------------------

impl From<&c::QueryIntent> for pb::QueryIntent {
    fn from(i: &c::QueryIntent) -> Self {
        pb::QueryIntent {
            contract_version: i.contract_version.clone(),
            kind: match i.kind {
                c::IntentKind::StructuredQuery => pb::IntentKind::StructuredQuery as i32,
                c::IntentKind::Semantic => pb::IntentKind::Semantic as i32,
            },
            request_id: i.request_id.clone(),
            contract: i.contract.clone(),
            semantic: i.semantic.as_ref().map(|s| pb::SemanticIntent {
                provider: s.provider.clone(),
                measures: s.measures.clone(),
                dimensions: s.dimensions.clone(),
                filters: s
                    .filters
                    .iter()
                    .map(|f| pb::SemanticFilter {
                        dimension: f.dimension.clone(),
                        op: f.op.clone(),
                        value: Some((&f.value).into()),
                    })
                    .collect(),
                grain: s.grain.clone(),
            }),
            parameters: i
                .parameters
                .iter()
                .map(|(k, v)| (k.clone(), v.into()))
                .collect(),
            authorization: Some(pb::AuthorizationSnapshot {
                tenant: i.authorization.tenant.clone(),
                uid: i.authorization.uid.clone(),
                access_level: i.authorization.access_level,
                compartments: i.authorization.compartments.clone(),
                session_id: i.authorization.session_id.clone(),
                runbook_ref: i.authorization.runbook_ref.clone(),
            }),
            limits: Some(pb::IntentLimits {
                max_rows: i.limits.max_rows,
                max_bytes: i.limits.max_bytes,
                max_cells: i.limits.max_cells,
            }),
            deadline_at: i.deadline_at.as_ref().map(chrono_to_ts),
            freshness: i.freshness.as_ref().map(|f| pb::FreshnessObligation {
                max_staleness_seconds: f.max_staleness_seconds,
                on_violation: match f.on_violation {
                    c::FreshnessAction::Refuse => pb::FreshnessAction::Refuse as i32,
                    c::FreshnessAction::Disclose => pb::FreshnessAction::Disclose as i32,
                },
            }),
            seal: Some(pb::SealPolicy {
                required: i.seal.required,
                retention_days: i.seal.retention_days,
                idempotency_key: i.seal.idempotency_key.clone(),
            }),
        }
    }
}

impl TryFrom<pb::QueryIntent> for c::QueryIntent {
    type Error = ConvertError;
    fn try_from(i: pb::QueryIntent) -> Result<Self, ConvertError> {
        let kind = match pb::IntentKind::try_from(i.kind) {
            Ok(pb::IntentKind::StructuredQuery) => c::IntentKind::StructuredQuery,
            Ok(pb::IntentKind::Semantic) => c::IntentKind::Semantic,
            _ => return Err(err("intent.kind is required (structured_query | semantic)")),
        };
        let auth = i
            .authorization
            .ok_or_else(|| err("intent.authorization is required"))?;
        let limits = i.limits.ok_or_else(|| err("intent.limits is required"))?;
        let mut parameters = std::collections::BTreeMap::new();
        for (k, v) in &i.parameters {
            parameters.insert(k.clone(), c::TypedValueDto::try_from(v)?);
        }
        let semantic = match i.semantic {
            None => None,
            Some(s) => Some(c::SemanticIntent {
                provider: s.provider,
                measures: s.measures,
                dimensions: s.dimensions,
                filters: s
                    .filters
                    .into_iter()
                    .map(|f| {
                        Ok(c::SemanticFilter {
                            dimension: f.dimension,
                            op: f.op,
                            value: c::TypedValueDto::try_from(
                                f.value
                                    .as_ref()
                                    .ok_or_else(|| err("semantic filter value is required"))?,
                            )?,
                        })
                    })
                    .collect::<Result<Vec<_>, ConvertError>>()?,
                grain: s.grain,
            }),
        };
        let freshness = match i.freshness {
            None => None,
            Some(f) => Some(c::FreshnessObligation {
                max_staleness_seconds: f.max_staleness_seconds,
                on_violation: match pb::FreshnessAction::try_from(f.on_violation) {
                    Ok(pb::FreshnessAction::Refuse) => c::FreshnessAction::Refuse,
                    Ok(pb::FreshnessAction::Disclose) => c::FreshnessAction::Disclose,
                    _ => {
                        return Err(err(
                            "freshness.on_violation is required (refuse | disclose)",
                        ))
                    }
                },
            }),
        };
        Ok(c::QueryIntent {
            contract_version: if i.contract_version.is_empty() {
                munarium_matrix_core::CONTRACT_VERSION.to_string()
            } else {
                i.contract_version
            },
            kind,
            request_id: i.request_id,
            contract: i.contract,
            semantic,
            parameters,
            authorization: c::AuthorizationSnapshot {
                tenant: auth.tenant,
                uid: auth.uid,
                access_level: auth.access_level,
                compartments: auth.compartments,
                session_id: auth.session_id,
                runbook_ref: auth.runbook_ref,
            },
            limits: c::IntentLimits {
                max_rows: limits.max_rows,
                max_bytes: limits.max_bytes,
                max_cells: limits.max_cells,
            },
            deadline_at: i.deadline_at.as_ref().map(ts_to_chrono).transpose()?,
            freshness,
            seal: i
                .seal
                .map(|s| c::SealPolicy {
                    required: s.required,
                    retention_days: s.retention_days,
                    idempotency_key: s.idempotency_key,
                })
                .unwrap_or_default(),
        })
    }
}

// ---------------------------------------------------------------------------
// Refusal
// ---------------------------------------------------------------------------

fn class_to_pb(c: RefusalClass) -> i32 {
    (match c {
        RefusalClass::NotCovered => pb::RefusalClass::NotCovered,
        RefusalClass::Unavailable => pb::RefusalClass::Unavailable,
        RefusalClass::Denied => pb::RefusalClass::Denied,
        RefusalClass::Incomplete => pb::RefusalClass::Incomplete,
        RefusalClass::Invalid => pb::RefusalClass::Invalid,
        RefusalClass::Exhausted => pb::RefusalClass::Exhausted,
    }) as i32
}

fn class_from_pb(v: i32) -> Result<RefusalClass, ConvertError> {
    Ok(match pb::RefusalClass::try_from(v) {
        Ok(pb::RefusalClass::NotCovered) => RefusalClass::NotCovered,
        Ok(pb::RefusalClass::Unavailable) => RefusalClass::Unavailable,
        Ok(pb::RefusalClass::Denied) => RefusalClass::Denied,
        Ok(pb::RefusalClass::Incomplete) => RefusalClass::Incomplete,
        Ok(pb::RefusalClass::Invalid) => RefusalClass::Invalid,
        Ok(pb::RefusalClass::Exhausted) => RefusalClass::Exhausted,
        _ => return Err(err("refusal.class is required and CLOSED")),
    })
}

impl From<&Refusal> for pb::Refusal {
    fn from(r: &Refusal) -> Self {
        pb::Refusal {
            contract_version: munarium_matrix_core::CONTRACT_VERSION.to_string(),
            class: class_to_pb(r.class),
            code: r.code.clone(),
            message: r.message.clone(),
            source_id: r.source_id.clone(),
            retry_after_seconds: r.retry_after_seconds,
            detail: r.detail.as_ref().map(json_to_pb),
        }
    }
}

impl TryFrom<&pb::Refusal> for Refusal {
    type Error = ConvertError;
    fn try_from(r: &pb::Refusal) -> Result<Self, ConvertError> {
        Ok(Refusal {
            class: class_from_pb(r.class)?,
            code: r.code.clone(),
            message: r.message.clone(),
            source_id: r.source_id.clone(),
            retry_after_seconds: r.retry_after_seconds,
            detail: r.detail.as_ref().map(pb_to_json),
        })
    }
}

// ---------------------------------------------------------------------------
// EvidenceBlock
// ---------------------------------------------------------------------------

fn derivation_to_pb(d: &ComputedDerivation) -> pb::Derivation {
    pb::Derivation {
        r#ref: d.reference.clone(),
        op: serde_name(&d.op),
        over: d.over.clone(),
        numerator: d.numerator.clone(),
        denominator: d.denominator.clone(),
        value: d.value.clone(),
        unit: d.unit.clone(),
        scale: d.scale,
    }
}

fn derivation_from_pb(d: &pb::Derivation) -> Result<ComputedDerivation, ConvertError> {
    Ok(ComputedDerivation {
        reference: d.r#ref.clone(),
        op: serde_parse(&d.op, "derivation op")?,
        over: d.over.clone(),
        numerator: d.numerator.clone(),
        denominator: d.denominator.clone(),
        value: d.value.clone(),
        unit: d.unit.clone(),
        scale: d.scale,
    })
}

fn manifest_to_struct(m: &c::EvidenceManifest) -> Result<prost_types::Struct, ConvertError> {
    let v = serde_json::to_value(m).map_err(|e| err(format!("manifest: {e}")))?;
    json_to_struct(&v)
}

fn manifest_from_struct(s: &prost_types::Struct) -> Result<c::EvidenceManifest, ConvertError> {
    serde_json::from_value(struct_to_json(s))
        .map_err(|e| err(format!("manifest is not a contract manifest: {e}")))
}

impl TryFrom<&c::EvidenceBlock> for pb::EvidenceBlock {
    type Error = ConvertError;
    fn try_from(b: &c::EvidenceBlock) -> Result<Self, ConvertError> {
        use pb::evidence_block::Kind;
        let (contract_version, kind) = match b {
            c::EvidenceBlock::CompleteTable {
                contract_version,
                evidence_id,
                manifest,
                rows,
                truncated,
                derivations,
            } => (
                contract_version.clone(),
                Kind::CompleteTable(pb::CompleteTable {
                    evidence_id: evidence_id.clone(),
                    manifest: Some(manifest_to_struct(manifest)?),
                    rows: rows
                        .iter()
                        .map(|r| pb::BlockRow {
                            row_id: r.row_id.clone(),
                            cells: r
                                .cells
                                .iter()
                                .map(|cell| pb::Cell {
                                    value: cell.clone(),
                                })
                                .collect(),
                        })
                        .collect(),
                    truncated: *truncated,
                    derivations: derivations.iter().map(derivation_to_pb).collect(),
                }),
            ),
            c::EvidenceBlock::Count {
                contract_version,
                evidence_id,
                manifest,
                value,
                of,
                exact,
            } => (
                contract_version.clone(),
                Kind::Count(pb::Count {
                    evidence_id: evidence_id.clone(),
                    manifest: Some(manifest_to_struct(manifest)?),
                    value: value.clone(),
                    of: of.clone(),
                    exact: *exact,
                }),
            ),
            c::EvidenceBlock::DocumentHits {
                contract_version,
                hits,
            } => (
                contract_version.clone(),
                Kind::DocumentHits(pb::DocumentHits {
                    hits: hits.iter().map(json_to_pb).collect(),
                }),
            ),
            c::EvidenceBlock::FactSlice {
                contract_version,
                version_id,
                as_of_seq,
                facts,
            } => (
                contract_version.clone(),
                Kind::FactSlice(pb::FactSlice {
                    version_id: version_id.clone(),
                    as_of_seq: *as_of_seq,
                    facts: facts.iter().map(json_to_pb).collect(),
                }),
            ),
            c::EvidenceBlock::Refusal {
                contract_version,
                refusal,
            } => (contract_version.clone(), Kind::Refusal(refusal.into())),
        };
        Ok(pb::EvidenceBlock {
            contract_version,
            kind: Some(kind),
        })
    }
}

impl TryFrom<&pb::EvidenceBlock> for c::EvidenceBlock {
    type Error = ConvertError;
    fn try_from(b: &pb::EvidenceBlock) -> Result<Self, ConvertError> {
        use pb::evidence_block::Kind;
        let contract_version = if b.contract_version.is_empty() {
            munarium_matrix_core::CONTRACT_VERSION.to_string()
        } else {
            b.contract_version.clone()
        };
        Ok(
            match b
                .kind
                .as_ref()
                .ok_or_else(|| err("evidence block kind is required"))?
            {
                Kind::CompleteTable(t) => c::EvidenceBlock::CompleteTable {
                    contract_version,
                    evidence_id: t.evidence_id.clone(),
                    manifest: Box::new(manifest_from_struct(
                        t.manifest
                            .as_ref()
                            .ok_or_else(|| err("complete_table.manifest is required"))?,
                    )?),
                    rows: t
                        .rows
                        .iter()
                        .map(|r| c::BlockRow {
                            row_id: r.row_id.clone(),
                            cells: r.cells.iter().map(|cell| cell.value.clone()).collect(),
                        })
                        .collect(),
                    truncated: t.truncated,
                    derivations: t
                        .derivations
                        .iter()
                        .map(derivation_from_pb)
                        .collect::<Result<Vec<_>, _>>()?,
                },
                Kind::Count(n) => c::EvidenceBlock::Count {
                    contract_version,
                    evidence_id: n.evidence_id.clone(),
                    manifest: Box::new(manifest_from_struct(
                        n.manifest
                            .as_ref()
                            .ok_or_else(|| err("count.manifest is required"))?,
                    )?),
                    value: n.value.clone(),
                    of: n.of.clone(),
                    exact: n.exact,
                },
                Kind::DocumentHits(h) => c::EvidenceBlock::DocumentHits {
                    contract_version,
                    hits: h.hits.iter().map(pb_to_json).collect(),
                },
                Kind::FactSlice(f) => c::EvidenceBlock::FactSlice {
                    contract_version,
                    version_id: f.version_id.clone(),
                    as_of_seq: f.as_of_seq,
                    facts: f.facts.iter().map(pb_to_json).collect(),
                },
                Kind::Refusal(r) => c::EvidenceBlock::Refusal {
                    contract_version,
                    refusal: Refusal::try_from(r)?,
                },
            },
        )
    }
}

/// A progress event stamped now.
pub fn progress(stage: &str) -> pb::ExecuteEvent {
    pb::ExecuteEvent {
        event: Some(pb::execute_event::Event::Progress(pb::Progress {
            stage: stage.to_string(),
            at: Some(chrono_to_ts(&chrono::Utc::now())),
        })),
    }
}
