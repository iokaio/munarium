// SPDX-License-Identifier: Apache-2.0
//! Turning a typed result into a sealed artifact.
//!
//! Shared by every mode, because the sealing rules must not vary by caller:
//! serialize canonically, hash the bytes, hash the logical result, build the
//! manifest, seal. The one thing a caller chooses is *what* it is sealing.

use munarium_matrix_core::{
    artifact_hash, logical_result_hash, ColumnType, Refusal, TypedResult, Value,
};
use munarium_matrix_server_client::{ServerClient, ServerError};
use munarium_matrix_types::contract::*;

/// The canonical CSV serialization.
///
/// Not "a CSV export" — a *canonical* one: LF endings, always-quoted fields,
/// header from the column names, NULL as an unquoted empty field so it is
/// distinguishable from the quoted empty string. Two different serializations
/// of one logical result share a `logical_result_hash` and differ in
/// `artifact_hash`, and this function is what makes the second one stable.
pub fn canonical_csv(result: &TypedResult) -> Vec<u8> {
    let mut out = Vec::new();
    // Header.
    let header: Vec<String> = result
        .schema
        .columns
        .iter()
        .map(|c| quote_csv(&c.name))
        .collect();
    out.extend_from_slice(header.join(",").as_bytes());
    out.push(b'\n');

    for row in &result.rows {
        let cells: Vec<String> = row
            .cells
            .iter()
            .map(|v| match v.canonical_text() {
                // An unquoted empty field is NULL; `""` is the empty string.
                // CSV cannot express the difference any other way, and the
                // difference is exactly what a reconciliation depends on.
                None => String::new(),
                Some(t) => quote_csv(&t),
            })
            .collect();
        out.extend_from_slice(cells.join(",").as_bytes());
        out.push(b'\n');
    }
    out
}

fn quote_csv(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// Everything a manifest needs that the result itself does not carry.
#[derive(Debug, Clone)]
pub struct SealContext {
    pub tenant: String,
    pub kind: ArtifactKind,
    pub source_id: String,
    pub source_version: u32,
    pub adapter: String,
    pub adapter_version: Option<String>,
    pub engine: Option<String>,
    pub versions: ManifestVersions,
    pub plan: Option<ManifestPlan>,
    pub snapshot_marker: Option<String>,
    pub isolation: Option<String>,
    pub replay_level: String,
    pub effective_principal: Option<String>,
    pub statement_id: Option<String>,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub ended_at: chrono::DateTime<chrono::Utc>,
    pub retention_days: Option<u32>,
    pub declared_max_rows: Option<u64>,
    pub rows_covered: Option<u64>,
    pub rows_excluded: Option<u64>,
    pub exclusion_reason: Option<String>,
    pub freshness_watermark: Option<String>,
}

/// Build the manifest for a result. Pure — no I/O, so the whole shape is
/// testable without a server.
pub fn build_manifest(result: &TypedResult, bytes: &[u8], ctx: &SealContext) -> EvidenceManifest {
    EvidenceManifest {
        contract_version: munarium_matrix_core::CONTRACT_VERSION.to_string(),
        canon: munarium_matrix_core::CANON_VERSION.to_string(),
        evidence_id: None,
        tenant: ctx.tenant.clone(),
        kind: ctx.kind,
        logical_result_hash: logical_result_hash(result),
        artifact_hash: artifact_hash(bytes),
        bytes_len: bytes.len() as u64,
        media_type: "text/csv; charset=utf-8".to_string(),
        source: ManifestSource {
            source_id: ctx.source_id.clone(),
            source_version: ctx.source_version,
            adapter: ctx.adapter.clone(),
            adapter_version: ctx.adapter_version.clone(),
            engine: ctx.engine.clone(),
            driver: None,
        },
        versions: ctx.versions.clone(),
        plan: ctx.plan.clone(),
        schema: ManifestSchema {
            columns: result.schema.columns.clone(),
        },
        identity: ManifestIdentity {
            row_id_rule: result.schema.row_id_rule,
            order_by: result.schema.order_by.clone(),
            rows: result.rows.len() as u64,
        },
        completeness: ManifestCompleteness {
            truncated: result.truncated,
            declared_max_rows: ctx.declared_max_rows,
            rows_covered: ctx.rows_covered,
            rows_excluded: ctx.rows_excluded,
            exclusion_reason: ctx.exclusion_reason.clone(),
        },
        redaction: ManifestRedaction {
            denied_columns: result.denied_columns.clone(),
            masked: false,
        },
        snapshot_vector: vec![SnapshotMarker {
            source_id: ctx.source_id.clone(),
            marker: ctx.snapshot_marker.clone(),
            isolation: ctx.isolation.clone(),
            started_at: Some(ctx.started_at),
            ended_at: Some(ctx.ended_at),
            replay_level: ctx.replay_level.clone(),
            replay_expires_at: None,
        }],
        freshness: ctx.freshness_watermark.as_ref().map(|w| ManifestFreshness {
            watermark: Some(w.clone()),
            observed_at: Some(ctx.ended_at),
            lag_seconds: None,
        }),
        execution: ManifestExecution {
            started_at: ctx.started_at,
            ended_at: ctx.ended_at,
            effective_principal: ctx.effective_principal.clone(),
            statement_id: ctx.statement_id.clone(),
        },
        authorization_class: result.authorization_class.clone(),
        retention: ctx.retention_days.map(|days| ManifestRetention {
            expires_at: Some(ctx.ended_at + chrono::Duration::days(days as i64)),
            legal_hold: false,
            purged_at: None,
        }),
    }
}

/// Serialize, hash and seal. Returns the evidence id.
pub async fn seal(
    server: &dyn ServerClient,
    result: &TypedResult,
    ctx: &SealContext,
    idempotency_key: Option<&str>,
) -> Result<(String, EvidenceManifest), Refusal> {
    // A result that cannot identify its rows must never be sealed — the
    // citation would point at nothing stable.
    result
        .validate()
        .map_err(|e| Refusal::result_not_identifiable(e.to_string()))?;

    let bytes = canonical_csv(result);
    let manifest = build_manifest(result, &bytes, ctx);
    let id = server
        .seal_evidence(&manifest, &bytes, idempotency_key)
        .await
        .map_err(|e: ServerError| e.to_refusal())?;
    let mut sealed = manifest;
    sealed.evidence_id = Some(id.clone());
    Ok((id, sealed))
}

/// A one-cell count result, so an exact count is sealed as evidence rather
/// than reported as a number someone has to trust.
pub fn count_result(value: i64, class: munarium_matrix_core::AuthorizationClass) -> TypedResult {
    TypedResult {
        schema: munarium_matrix_core::ResultSchema {
            columns: vec![munarium_matrix_core::Column::new(
                "c0",
                "count",
                ColumnType::Int64,
            )],
            // A single-row count is positional: there is no key, and the
            // ordering is trivially total.
            row_id_rule: munarium_matrix_core::RowIdRule::Position,
            order_by: vec!["count".into()],
        },
        rows: vec![munarium_matrix_core::Row::new(vec![Value::Int64(value)])],
        truncated: false,
        denied_columns: vec![],
        authorization_class: class,
    }
}

/// Render an observation batch as a typed result, so mode C seals through the
/// **same** path as modes A and B.
///
/// This exists because of a defect worth recording. The server client used to
/// carry a separate `seal_observations` that POSTed `{"batch": ...}`, while the
/// server's contract — and every other seal — is `{"manifest": ...}`. The
/// `MockServer` accepted the batch shape, so the divergence was invisible until
/// Matrix met a real 0.4.0+ server, which answered `missing field 'manifest'`.
/// It is the third time a mock that did not enforce a peer's contract turned
/// that contract into a surprise (the others: `claim_id` vs `id`, and the
/// missing `X-Munarium-Uid`).
///
/// The fix is structural rather than a patched second path: an observation
/// batch IS an artifact — the contract even reserves `kind: observations` for
/// it — so rendering it as a `TypedResult` deletes the divergent path instead
/// of correcting it. One sealing path cannot drift from itself.
///
/// The row identity is `(row_key, property)`: an observation is *this property
/// of this source row*, and two observations sharing both would be the same
/// observation. Keyed identity also means the batch hashes as a multiset, so
/// re-ordering observations does not change the artifact — which is right,
/// because the order a connector happens to emit rows in is not part of what
/// was observed.
pub fn observation_batch_result(
    batch: &ObservationBatch,
    class: munarium_matrix_core::AuthorizationClass,
) -> TypedResult {
    use munarium_matrix_core::{Column, ResultSchema, Row, RowIdRule};

    let columns = vec![
        Column::new("c0", "row_key", ColumnType::String).key(),
        Column::new("c1", "property", ColumnType::String).key(),
        Column::new("c2", "value", ColumnType::String).nullable(),
        Column::new("c3", "change_kind", ColumnType::String),
        Column::new("c4", "subject", ColumnType::String).nullable(),
        Column::new("c5", "event_position", ColumnType::String).nullable(),
        Column::new("c6", "observed_at", ColumnType::String).nullable(),
    ];

    let rows = batch
        .observations
        .iter()
        .map(|o| {
            // The highest-confidence candidate names the row; ambiguity is a
            // reconciliation decision, not a sealing one, and the sealed
            // artifact must record what was OBSERVED rather than what was
            // later concluded about it.
            let subject = o
                .entity_candidates
                .iter()
                .max_by(|a, b| a.confidence.total_cmp(&b.confidence))
                .map(|c| c.subject.clone());
            let value = match o.value.value.as_str() {
                Some(s) => Value::String(s.to_string()),
                None if o.value.value.is_null() => Value::Null,
                None => Value::String(o.value.value.to_string()),
            };
            Row::new(vec![
                Value::String(o.origin.row_key.clone()),
                Value::String(o.property.clone()),
                value,
                Value::String(
                    serde_json::to_value(o.change_kind)
                        .ok()
                        .and_then(|v| v.as_str().map(String::from))
                        .unwrap_or_default(),
                ),
                subject.map(Value::String).unwrap_or(Value::Null),
                o.origin
                    .event_position
                    .clone()
                    .map(Value::String)
                    .unwrap_or(Value::Null),
                o.transaction_time
                    .map(|t| Value::String(t.to_rfc3339()))
                    .unwrap_or(Value::Null),
            ])
        })
        .collect();

    TypedResult {
        schema: ResultSchema {
            columns,
            row_id_rule: RowIdRule::Keys,
            order_by: vec![],
        },
        rows,
        truncated: false,
        denied_columns: vec![],
        authorization_class: class,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use munarium_matrix_core::{AuthorizationClass, Column, ResultSchema, Row, RowIdRule};
    use rust_decimal::Decimal;
    use std::str::FromStr;

    fn result() -> TypedResult {
        TypedResult {
            schema: ResultSchema {
                columns: vec![
                    Column::new("c0", "region", ColumnType::String).key(),
                    Column::new("c1", "amount", ColumnType::Decimal).scale(2),
                    Column::new("c2", "notes", ColumnType::String).nullable(),
                ],
                row_id_rule: RowIdRule::Keys,
                order_by: vec!["region".into()],
            },
            rows: vec![
                Row::new(vec![
                    Value::String("EMEA".into()),
                    Value::Decimal {
                        value: Decimal::from_str("1500").unwrap(),
                        scale: 2,
                    },
                    Value::Null,
                ]),
                Row::new(vec![
                    Value::String("AMER".into()),
                    Value::Decimal {
                        value: Decimal::from_str("250.5").unwrap(),
                        scale: 2,
                    },
                    Value::String(String::new()),
                ]),
            ],
            truncated: false,
            denied_columns: vec![],
            authorization_class: AuthorizationClass::default(),
        }
    }

    #[test]
    fn canonical_csv_distinguishes_null_from_the_empty_string() {
        let csv = String::from_utf8(canonical_csv(&result())).unwrap();
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines[0], r#""region","amount","notes""#);
        // NULL: an unquoted empty field.
        assert_eq!(lines[1], r#""EMEA","1500.00","#);
        // The empty string: quoted.
        assert_eq!(lines[2], r#""AMER","250.50","""#);
    }

    #[test]
    fn canonical_csv_uses_lf_only() {
        let bytes = canonical_csv(&result());
        assert!(
            !bytes.contains(&b'\r'),
            "CRLF would move every artifact hash"
        );
    }

    #[test]
    fn a_quote_inside_a_value_is_doubled_not_dropped() {
        let mut r = result();
        r.rows[0].cells[2] = Value::String("she said \"hi\"".into());
        let csv = String::from_utf8(canonical_csv(&r)).unwrap();
        assert!(csv.contains(r#""she said ""hi""""#), "{csv}");
    }

    fn ctx() -> SealContext {
        let now = chrono::Utc::now();
        SealContext {
            tenant: "acme".into(),
            kind: ArtifactKind::Table,
            source_id: "crm".into(),
            source_version: 1,
            adapter: "landing".into(),
            adapter_version: Some("landing@0.1.0".into()),
            engine: None,
            versions: ManifestVersions::default(),
            plan: None,
            snapshot_marker: Some("s1".into()),
            isolation: None,
            replay_level: "sealed_result".into(),
            effective_principal: Some("matrix_reader".into()),
            statement_id: None,
            started_at: now,
            ended_at: now,
            retention_days: Some(400),
            declared_max_rows: Some(500),
            rows_covered: Some(2),
            rows_excluded: Some(0),
            exclusion_reason: None,
            freshness_watermark: None,
        }
    }

    #[test]
    fn the_manifest_carries_both_hashes_and_they_differ() {
        let r = result();
        let bytes = canonical_csv(&r);
        let m = build_manifest(&r, &bytes, &ctx());
        assert!(m.logical_result_hash.starts_with("sha256:"));
        assert!(m.artifact_hash.starts_with("sha256:"));
        assert_ne!(m.logical_result_hash, m.artifact_hash);
        assert_eq!(m.bytes_len, bytes.len() as u64);
        assert_eq!(m.identity.rows, 2);
    }

    #[test]
    fn retention_becomes_a_real_expiry_date() {
        let r = result();
        let bytes = canonical_csv(&r);
        let c = ctx();
        let m = build_manifest(&r, &bytes, &c);
        let expires = m.retention.unwrap().expires_at.unwrap();
        assert_eq!((expires - c.ended_at).num_days(), 400);
    }

    #[tokio::test]
    async fn sealing_an_unidentifiable_result_is_refused_before_any_call() {
        use munarium_matrix_server_client::MockServer;
        let server = MockServer::new();
        let mut r = result();
        // Strip the key and the ordering: the rows can no longer be named.
        r.schema.columns[0].key = false;
        r.schema.order_by.clear();
        let err = seal(&server, &r, &ctx(), None).await.unwrap_err();
        assert_eq!(err.code, "result_not_identifiable");
        assert_eq!(server.evidence_count(), 0, "nothing may be sealed");
    }

    #[tokio::test]
    async fn a_sealed_count_is_evidence_not_a_number() {
        use munarium_matrix_server_client::{MockServer, ServerClient};
        let server = MockServer::new();
        let mut c = ctx();
        c.kind = ArtifactKind::Count;
        let (id, manifest) = seal(
            &server,
            &count_result(1284, AuthorizationClass::default()),
            &c,
            None,
        )
        .await
        .unwrap();
        assert_eq!(manifest.kind, ArtifactKind::Count);
        let read = server.get_evidence(&id).await.unwrap();
        assert_eq!(read.identity.rows, 1);
        // The count's bytes are the artifact: a citation resolves to them.
        let bytes = String::from_utf8(server.evidence_bytes(&id).unwrap()).unwrap();
        assert!(bytes.contains("1284"), "{bytes}");
    }
}
