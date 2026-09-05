// SPDX-License-Identifier: Apache-2.0
//! Building observations from a source read — the step between the adapter and
//! [`crate::reconcile`].
//!
//! `reconcile` compares an [`ObservationBatch`] against the ledger. Something
//! has to *produce* that batch, and this is it: read a record batch through the
//! adapter, and render one observation per (row × declared property).
//!
//! Three decisions are worth stating, because each is a way to be quietly
//! wrong:
//!
//! - **One observation per property, not per row.** A row carrying three mapped
//!   properties makes three observations, because each is separately
//!   comparable against a separate ledger claim and separately in or out of an
//!   authority scope. Bundling them would force the comparison to be
//!   all-or-nothing.
//!
//! - **A NULL is not an observation.** The source saying "no value" is not the
//!   same as the source asserting a value, and turning it into one would make
//!   every unset column contradict every claim. Nulls are counted and skipped.
//!   (`missing_in_source` is `reconcile`'s job, from the absence of a
//!   comparison, not something this step fabricates.)
//!
//! - **The subject template is filled from KEY columns only.** The validator
//!   already refuses a template naming a non-key column; this is the runtime
//!   half of the same rule, and it is what makes `row_key` and the subject
//!   agree for the life of the row.

use munarium_matrix_adapter::{EffectiveIdentity, Limits, ReadMode, SourceAdapter};
use munarium_matrix_core::checkpoint::{Checkpoint, SyncMode};
use munarium_matrix_core::{Refusal, Value};
use munarium_matrix_types::assets::{ClaimMappingDoc, Resolver};
use munarium_matrix_types::contract::{
    ConnectorOrigin, EntityCandidate, Observation, ObservationBatch, TypedValueDto, ValidTime,
};

/// What one observation pass read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ObserveStats {
    pub rows_read: u64,
    /// Cells that were NULL and so produced no observation. Reported rather
    /// than silently dropped: "the column is empty everywhere" and "the mapping
    /// names the wrong column" look identical without this number.
    pub nulls_skipped: u64,
    pub rows_excluded: u64,
    /// True when the read returned every row the entity has — nothing
    /// excluded, nothing truncated (the postgres adapter reports truncation
    /// through `excluded`). This is what licenses `missing_in_source`: a
    /// claim that a row is absent is only honest about a read that would have
    /// returned it.
    pub complete: bool,
}

pub struct ObserveContext<'a> {
    pub tenant: &'a str,
    pub source_id: &'a str,
    pub batch_id: &'a str,
    pub run_id: Option<&'a str>,
    pub limits: Limits,
    pub identity: &'a EffectiveIdentity,
}

/// Read one batch through the adapter and render its observations.
pub async fn observe(
    adapter: &dyn SourceAdapter,
    mapping: &ClaimMappingDoc,
    checkpoint: &Checkpoint,
    ctx: &ObserveContext<'_>,
) -> Result<(ObservationBatch, ObserveStats, Option<Checkpoint>), Refusal> {
    let spec = &mapping.spec;

    // The projection is everything the mapping reads: key columns, property
    // columns, and the temporal columns. Asking for exactly this and no more is
    // what keeps an undeclared column out of the process entirely.
    let mut projection: Vec<String> = spec.entity.key.clone();
    for p in spec.properties.values() {
        if !projection.contains(&p.column) {
            projection.push(p.column.clone());
        }
    }
    let valid_time_column = spec.temporal.valid_time.column.clone();
    if !projection.contains(&valid_time_column) {
        projection.push(valid_time_column.clone());
    }
    // The alias column is read only when a resolver actually consults it. A
    // projection that reaches for a column no code reads would widen what
    // leaves the source for nothing, and G6 is about exactly that.
    let alias_table = match spec.entity.identity.resolver {
        Resolver::TerminologyAlias => spec.entity.identity.aliases.as_ref(),
        Resolver::EntityKey => None,
    };
    if let Some(t) = alias_table {
        if !projection.contains(&t.column) {
            projection.push(t.column.clone());
        }
    }

    let batch = adapter
        .read_batch(
            &spec.entity.table,
            &projection,
            checkpoint,
            // Mode C reads a snapshot, so there is no watermark to declare.
            ReadMode::of(SyncMode::Snapshot),
            ctx.identity,
            ctx.limits,
        )
        .await?;

    let index = |name: &str| batch.columns.iter().position(|c| c.name == name);

    // Fail closed on a mapping that names a column the source did not return.
    // Rendering observations from a short row would silently shift every value
    // one column left.
    for name in &projection {
        if index(name).is_none() {
            return Err(Refusal::schema_drift(format!(
                "mapping '{}' reads column '{name}' but the source returned {:?}",
                mapping.metadata.asset_ref(),
                batch.columns.iter().map(|c| &c.name).collect::<Vec<_>>()
            )));
        }
    }

    let valid_time_idx = index(&valid_time_column).expect("checked above");
    let mut stats = ObserveStats {
        rows_read: batch.records.len() as u64,
        rows_excluded: batch.excluded,
        complete: batch.excluded == 0,
        ..Default::default()
    };
    let mut observations = Vec::new();

    // --- alias pre-pass ----------------------------------------------------
    //
    // Which ledger subject each row's alias column names, and — the part that
    // needs a whole-batch view — which of those subjects is CONTESTED: claimed
    // by two or more rows inside one scope. A contested alias is the T0
    // fixture's trap 9: two holders on one cap table whose declared forms name
    // the same ledger entity. The register knows they are different rows; the
    // ledger's name cannot tell them apart, and guessing is how a
    // reconciliation corrupts a cap table.
    //
    // Scope matters. The same person legitimately holds shares in two
    // companies, so a collision only counts inside one `scopeTemplate` value.
    let alias_hits: Vec<Option<(String, Option<String>)>> = match alias_table {
        None => vec![None; batch.records.len()],
        Some(table) => {
            let col = index(&table.column).ok_or_else(|| {
                Refusal::schema_drift(format!(
                    "mapping '{}' resolves aliases from column '{}' but the source did not                      return it",
                    mapping.metadata.asset_ref(),
                    table.column
                ))
            })?;
            batch
                .records
                .iter()
                .map(|record| {
                    let raw = record.cells.get(col).unwrap_or(&Value::Null);
                    let text = raw.canonical_text()?;
                    let entry = table.lookup(&text)?;
                    let scope = entry.scope_path.clone().or_else(|| {
                        spec.entity.scope_template.as_ref().and_then(|t| {
                            fill_template(t, |c| {
                                index(c).map(|i| {
                                    render_key(record.cells.get(i).unwrap_or(&Value::Null))
                                })
                            })
                        })
                    });
                    Some((entry.subject.clone(), scope))
                })
                .collect()
        }
    };
    let mut claimants: std::collections::BTreeMap<(Option<String>, String), usize> =
        Default::default();
    for (i, hit) in alias_hits.iter().enumerate() {
        if let Some((subject, scope)) = hit {
            // Count ROWS, not observations: a row with three mapped properties
            // contests an alias once, not three times.
            let _ = i;
            *claimants
                .entry((scope.clone(), subject.clone()))
                .or_default() += 1;
        }
    }

    for (row_index, record) in batch.records.iter().enumerate() {
        let cell = |i: usize| record.cells.get(i).unwrap_or(&Value::Null);

        let subject = fill_template(&spec.entity.subject_template, |col| {
            index(col).map(|i| render_key(cell(i)))
        })
        .ok_or_else(|| {
            Refusal::invalid(
                "subject_template_unfillable",
                format!(
                    "mapping '{}' subject template '{}' names a column that is not in the \
                     entity key",
                    mapping.metadata.asset_ref(),
                    spec.entity.subject_template
                ),
            )
        })?;
        let scope_path = match &spec.entity.scope_template {
            Some(t) => fill_template(t, |col| index(col).map(|i| render_key(cell(i)))),
            None => None,
        };

        // The row key is rendered HERE from the declared key columns, not
        // taken from the adapter. The postgres adapter keys a record by its
        // first projected column and leaves the real key set to the caller;
        // the landing adapter joins the manifest's keys with `|`. A citation
        // must not change shape with the adapter that produced the row, so
        // every observation carries the same form: key values, in declared
        // order, joined with `|`.
        let row_key = spec
            .entity
            .key
            .iter()
            .map(|k| index(k).map(|i| render_key(cell(i))).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("|");

        let valid_from = as_instant(cell(valid_time_idx));
        // Transaction time is the source's, when it gives one. An observation
        // stamped with OUR clock would make replay non-deterministic and would
        // reorder history on a slow run.
        let transaction_time = record
            .event_position
            .as_deref()
            .and_then(parse_instant)
            .or(valid_from);

        for (property, pspec) in &spec.properties {
            let idx = index(&pspec.column).expect("checked above");
            let value = cell(idx);
            if matches!(value, Value::Null) {
                stats.nulls_skipped += 1;
                continue;
            }
            observations.push(Observation {
                entity_candidates: entity_candidates(
                    &subject,
                    scope_path.as_deref(),
                    alias_hits[row_index].as_ref(),
                    alias_table,
                    &claimants,
                ),
                property: property.clone(),
                value: TypedValueDto {
                    ty: pspec.ty,
                    // Safe: NULLs were skipped above, and canonical_text is None
                    // only for NULL. That is the type-level distinction the value
                    // model draws, and it is why this cannot silently emit "NULL".
                    // A canon@1 STRING, which is what `reconcile::typed_text`
                    // reads back and re-formats. Emitting a JSON number here
                    // would lose a decimal's scale on the way through serde and
                    // make 900000.50 compare unequal to itself.
                    value: serde_json::Value::String(
                        value.canonical_text().expect("non-null checked above"),
                    ),
                    scale: pspec.scale,
                    element_type: None,
                },
                valid_time: valid_from.map(|from| ValidTime {
                    from: Some(from),
                    to: None,
                }),
                transaction_time,
                change_kind: record.change_kind,
                origin: ConnectorOrigin {
                    kind: "connector".into(),
                    source_id: ctx.source_id.to_string(),
                    mapping_version: mapping.metadata.asset_ref(),
                    row_key: row_key.clone(),
                    event_position: record.event_position.clone(),
                    observed_at: transaction_time,
                    evidence_id: None,
                },
            });
        }
    }

    let batch_doc = ObservationBatch {
        contract_version: munarium_matrix_core::CONTRACT_VERSION.to_string(),
        mapping: mapping.metadata.asset_ref(),
        batch_id: ctx.batch_id.to_string(),
        source_id: Some(ctx.source_id.to_string()),
        run_id: ctx.run_id.map(str::to_string),
        sealed_evidence_id: None,
        observations,
    };
    Ok((batch_doc, stats, batch.next_checkpoint))
}

/// Substitute `{column}` placeholders. Returns `None` if any placeholder cannot
/// be filled — a half-filled subject would be a different entity every run.
/// The candidate list for one observation, best first.
///
/// The key-derived subject is exact by construction and always present. An
/// alias candidate is added beside it — never in place of it — so the pipeline
/// keeps the source's own identity even while it reaches for the ledger's.
///
/// A CONTESTED alias is emitted at the key candidate's own confidence. That is
/// deliberate and it is the whole mechanism: `compare` refuses a TIE, and a tie
/// is how this list says *I cannot choose* in the vocabulary the contract
/// already has. Ranking two identities the pipeline cannot rank would be the
/// guess the guarantee forbids.
fn entity_candidates(
    subject: &str,
    scope_path: Option<&str>,
    hit: Option<&(String, Option<String>)>,
    table: Option<&munarium_matrix_types::assets::AliasTable>,
    claimants: &std::collections::BTreeMap<(Option<String>, String), usize>,
) -> Vec<EntityCandidate> {
    let mut out = vec![EntityCandidate {
        subject: subject.to_string(),
        scope_path: scope_path.map(str::to_string),
        confidence: 1.0,
        resolver: Some("entity_key".into()),
    }];

    if let (Some((alias_subject, alias_scope)), Some(table)) = (hit, table) {
        // An alias that resolves to the row's own key subject adds nothing but
        // noise, and would make every aliased row look contested with itself.
        if alias_subject != subject {
            let contested = claimants
                .get(&(alias_scope.clone(), alias_subject.clone()))
                .is_some_and(|n| *n > 1);
            out.push(EntityCandidate {
                subject: alias_subject.clone(),
                scope_path: alias_scope
                    .clone()
                    .or_else(|| scope_path.map(str::to_string)),
                confidence: if contested { 1.0 } else { table.confidence },
                resolver: Some("terminology_alias".into()),
            });
        }
    }
    out.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));
    out
}

fn fill_template(template: &str, mut lookup: impl FnMut(&str) -> Option<String>) -> Option<String> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let close = rest[open..].find('}')? + open;
        out.push_str(&lookup(&rest[open + 1..close])?);
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// A key rendered for a subject. Deliberately the canonical form, so the
/// subject a row produces today is the subject it produced last year.
fn render_key(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        other => other.canonical_text().unwrap_or_default(),
    }
}

fn as_instant(v: &Value) -> Option<chrono::DateTime<chrono::Utc>> {
    match v {
        Value::TimestampTz(t) => Some(*t),
        Value::Date(d) => d.and_hms_opt(0, 0, 0).map(|n| n.and_utc()),
        Value::TimestampNaive(n) => Some(n.and_utc()),
        Value::String(s) => parse_instant(s),
        _ => None,
    }
}

fn parse_instant(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use munarium_matrix_adapter::{
        Capabilities, ProbeResult, RecordBatch, RolePosture, SchemaFingerprint, SourceRecord,
    };
    use munarium_matrix_core::result::Column;
    use munarium_matrix_core::value::ColumnType;
    use munarium_matrix_types::contract::ChangeKind;

    const MAPPING: &str = r#"
apiVersion: munarium.ioka.io/v1
kind: ClaimMapping
metadata: { name: holdings, version: 1 }
spec:
  source: crm
  entity:
    table: holdings
    key: [holder_id]
    subjectTemplate: "shareholder.{holder_id}"
    scopeTemplate: "company.{holder_id}.captable"
  properties:
    shares: { column: shares, type: decimal, scale: 0 }
    share_class: { column: share_class, type: string }
  temporal:
    validTime: { column: effective_date }
"#;

    fn mapping() -> ClaimMappingDoc {
        match munarium_matrix_types::parse_asset(MAPPING).unwrap() {
            munarium_matrix_types::Asset::ClaimMapping(m) => *m,
            _ => unreachable!(),
        }
    }

    /// The same mapping with an alias table, scoped by company so a contest is
    /// per cap table rather than per name.
    const ALIAS_MAPPING: &str = r#"
apiVersion: munarium.ioka.io/v1
kind: ClaimMapping
metadata: { name: holdings, version: 1 }
spec:
  source: crm
  entity:
    table: holdings
    key: [holder_id, company_id]
    subjectTemplate: "shareholder.{holder_id}"
    scopeTemplate: "company.{company_id}.captable"
    identity:
      resolver: terminology_alias
      minConfidence: 0.95
      aliases:
        column: holder_name
        confidence: 0.96
        entries:
          - subject: shareholder.jane-rowntree
            forms: ["Jane Rowntree", "J. Rowntree"]
  properties:
    shares: { column: shares, type: decimal, scale: 0 }
  temporal:
    validTime: { column: effective_date }
"#;

    fn alias_mapping() -> ClaimMappingDoc {
        match munarium_matrix_types::parse_asset(ALIAS_MAPPING).unwrap() {
            munarium_matrix_types::Asset::ClaimMapping(m) => *m,
            _ => unreachable!(),
        }
    }

    fn alias_columns() -> Vec<Column> {
        vec![
            Column::new("holder_id", "holder_id", ColumnType::Int64),
            Column::new("company_id", "company_id", ColumnType::Int64),
            {
                let mut c = Column::new("shares", "shares", ColumnType::Decimal);
                c.scale = Some(0);
                c
            },
            Column::new("effective_date", "effective_date", ColumnType::Date),
            Column::new("holder_name", "holder_name", ColumnType::String),
        ]
    }

    fn alias_record(holder: i64, company: i64, shares: &str, name: &str) -> SourceRecord {
        SourceRecord {
            cells: vec![
                Value::Int64(holder),
                Value::Int64(company),
                Value::Decimal {
                    value: shares.parse().unwrap(),
                    scale: 0,
                },
                Value::Date(chrono::NaiveDate::from_ymd_opt(2026, 4, 1).unwrap()),
                Value::String(name.into()),
            ],
            row_key: format!("holder_id={holder},company_id={company}"),
            event_position: None,
            change_kind: ChangeKind::Snapshot,
        }
    }

    async fn alias_observe(records: Vec<SourceRecord>) -> ObservationBatch {
        let src = FakeSource {
            columns: alias_columns(),
            records,
        };
        observe(
            &src,
            &alias_mapping(),
            &Checkpoint::start("crm", "holdings", "1"),
            &ctx(),
        )
        .await
        .unwrap()
        .0
    }

    #[tokio::test]
    async fn the_row_key_is_the_declared_keys_joined_whatever_the_adapter_said() {
        let mut rec = alias_record(42, 7, "125000", "Jane Rowntree");
        rec.row_key = "whatever-the-adapter-chose".into();
        let batch = alias_observe(vec![rec]).await;
        assert_eq!(batch.observations[0].origin.row_key, "42|7");
    }

    #[tokio::test]
    async fn an_uncontested_alias_ranks_behind_the_exact_key_subject() {
        let batch = alias_observe(vec![alias_record(42, 7, "125000", "Jane Rowntree")]).await;
        let c = &batch.observations[0].entity_candidates;
        assert_eq!(
            c.len(),
            2,
            "key subject and alias subject, not one or three"
        );
        assert_eq!(c[0].subject, "shareholder.42");
        assert_eq!(c[0].confidence, 1.0);
        assert_eq!(c[0].resolver.as_deref(), Some("entity_key"));
        assert_eq!(c[1].subject, "shareholder.jane-rowntree");
        assert_eq!(c[1].confidence, 0.96);
        assert_eq!(c[1].resolver.as_deref(), Some("terminology_alias"));
    }

    #[tokio::test]
    async fn a_declared_form_matches_through_case_and_whitespace_only() {
        // `Jane  Rowntree` (two spaces) is the same declared form. `J. Rowntree`
        // matches because a human declared it, not because punctuation was
        // stripped — the row below proves the difference.
        let batch = alias_observe(vec![
            alias_record(58, 8, "40000", "Jane  Rowntree"),
            alias_record(99, 9, "1", "Jane Rowntre"),
        ])
        .await;
        assert_eq!(batch.observations[0].entity_candidates.len(), 2);
        assert_eq!(
            batch.observations[1].entity_candidates.len(),
            1,
            "a near-miss is not an alias; only the exact declared form is"
        );
    }

    #[tokio::test]
    async fn two_rows_on_one_cap_table_contest_an_alias_and_tie() {
        // T0 trap 9. Holders 51 and 58 sit on company 8 and their declared
        // forms name one ledger subject.
        let batch = alias_observe(vec![
            alias_record(51, 8, "40000", "J. Rowntree"),
            alias_record(58, 8, "40000", "Jane  Rowntree"),
        ])
        .await;
        for o in &batch.observations {
            let c = &o.entity_candidates;
            assert_eq!(c.len(), 2);
            assert_eq!(
                c[0].confidence, c[1].confidence,
                "a contested alias TIES with the key subject; the tie is what                  makes compare() refuse to merge"
            );
        }
    }

    #[tokio::test]
    async fn the_same_person_on_two_cap_tables_is_not_a_contest() {
        // Holding shares in two companies is ordinary. Only a collision INSIDE
        // one scope means the ledger's name cannot tell two rows apart.
        let batch = alias_observe(vec![
            alias_record(42, 7, "125000", "Jane Rowntree"),
            alias_record(58, 8, "40000", "Jane  Rowntree"),
        ])
        .await;
        for o in &batch.observations {
            let c = &o.entity_candidates;
            assert_eq!(c[1].confidence, 0.96, "uncontested, so it stays a hint");
        }
    }

    #[tokio::test]
    async fn the_alias_column_is_not_read_when_no_resolver_consults_it() {
        // `mapping()` uses the default `entity_key` resolver. Reading a column
        // nothing consults would widen what leaves the source for nothing.
        let src = FakeSource {
            columns: columns(),
            records: vec![record(42, Some("125000"), "A")],
        };
        let (batch, _, _) = observe(
            &src,
            &mapping(),
            &Checkpoint::start("crm", "holdings", "1"),
            &ctx(),
        )
        .await
        .unwrap();
        assert_eq!(batch.observations[0].entity_candidates.len(), 1);
    }

    struct FakeSource {
        columns: Vec<Column>,
        records: Vec<SourceRecord>,
    }

    #[async_trait::async_trait]
    impl SourceAdapter for FakeSource {
        fn kind(&self) -> &'static str {
            "fake"
        }
        fn adapter_version(&self) -> &'static str {
            "test"
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::minimal("sealed_result")
        }
        async fn probe(&self) -> Result<ProbeResult, Refusal> {
            Ok(ProbeResult {
                reachable: true,
                latency_ms: Some(0),
                detail: None,
            })
        }
        async fn introspect(&self) -> Result<(RolePosture, SchemaFingerprint), Refusal> {
            unimplemented!("not used by observe")
        }
        async fn read_batch(
            &self,
            _entity: &str,
            _projection: &[String],
            _checkpoint: &Checkpoint,
            _read: ReadMode<'_>,
            _identity: &EffectiveIdentity,
            _limits: Limits,
        ) -> Result<RecordBatch, Refusal> {
            Ok(RecordBatch {
                records: self.records.clone(),
                columns: self.columns.clone(),
                next_checkpoint: None,
                excluded: 0,
                snapshot_marker: Some("m1".into()),
            })
        }
        async fn execute(
            &self,
            _s: &str,
            _p: &munarium_matrix_adapter::BoundParameters,
            _i: &EffectiveIdentity,
            _l: Limits,
        ) -> Result<munarium_matrix_adapter::ExecutedResult, Refusal> {
            unimplemented!("not used by observe")
        }
    }

    fn columns() -> Vec<Column> {
        vec![
            Column::new("holder_id", "holder_id", ColumnType::Int64),
            {
                let mut c = Column::new("shares", "shares", ColumnType::Decimal);
                c.scale = Some(0);
                c
            },
            Column::new("share_class", "share_class", ColumnType::String),
            Column::new("effective_date", "effective_date", ColumnType::Date),
        ]
    }

    fn record(holder: i64, shares: Option<&str>, class: &str) -> SourceRecord {
        SourceRecord {
            cells: vec![
                Value::Int64(holder),
                match shares {
                    Some(s) => Value::Decimal {
                        value: s.parse().unwrap(),
                        scale: 0,
                    },
                    None => Value::Null,
                },
                Value::String(class.into()),
                Value::Date(chrono::NaiveDate::from_ymd_opt(2026, 4, 1).unwrap()),
            ],
            row_key: format!("holder_id={holder}"),
            event_position: None,
            change_kind: ChangeKind::Snapshot,
        }
    }

    fn ctx() -> ObserveContext<'static> {
        ObserveContext {
            tenant: "acme",
            source_id: "crm",
            batch_id: "b1",
            run_id: Some("r1"),
            limits: Limits {
                max_rows: 100,
                max_bytes: 1 << 20,
                timeout_ms: 1000,
            },
            identity: Box::leak(Box::new(EffectiveIdentity {
                class: None,
                credential_ref: None,
                principal: "test".into(),
            })),
        }
    }

    #[tokio::test]
    async fn one_row_makes_one_observation_per_declared_property() {
        let src = FakeSource {
            columns: columns(),
            records: vec![record(42, Some("125000"), "A")],
        };
        let (batch, stats, _) = observe(
            &src,
            &mapping(),
            &Checkpoint::start("crm", "holdings", "1"),
            &ctx(),
        )
        .await
        .unwrap();
        assert_eq!(stats.rows_read, 1);
        assert_eq!(
            batch.observations.len(),
            2,
            "two declared properties means two separately comparable observations"
        );
        let shares = batch
            .observations
            .iter()
            .find(|o| o.property == "shares")
            .unwrap();
        assert_eq!(shares.value.value, "125000");
        assert_eq!(shares.entity_candidates[0].subject, "shareholder.42");
        assert_eq!(
            shares.entity_candidates[0].scope_path.as_deref(),
            Some("company.42.captable")
        );
        assert_eq!(shares.entity_candidates[0].confidence, 1.0);
    }

    #[tokio::test]
    async fn a_null_makes_no_observation_and_is_counted() {
        let src = FakeSource {
            columns: columns(),
            records: vec![record(42, None, "A")],
        };
        let (batch, stats, _) = observe(
            &src,
            &mapping(),
            &Checkpoint::start("crm", "holdings", "1"),
            &ctx(),
        )
        .await
        .unwrap();
        assert_eq!(stats.nulls_skipped, 1);
        assert_eq!(batch.observations.len(), 1, "only share_class survives");
        assert!(
            batch.observations.iter().all(|o| o.property != "shares"),
            "a source saying 'no value' must not become an assertion of one"
        );
    }

    #[tokio::test]
    async fn a_column_the_source_did_not_return_is_drift_not_a_shifted_row() {
        // Drop `share_class` from the schema the source reports. Rendering
        // anyway would read the date into share_class and shift every value.
        let mut cols = columns();
        cols.retain(|c| c.name != "share_class");
        let src = FakeSource {
            columns: cols,
            records: vec![record(42, Some("1"), "A")],
        };
        let err = observe(
            &src,
            &mapping(),
            &Checkpoint::start("crm", "holdings", "1"),
            &ctx(),
        )
        .await
        .expect_err("must refuse");
        assert_eq!(err.code, "schema_drift");
    }

    #[test]
    fn a_template_naming_an_unknown_column_cannot_be_filled() {
        assert_eq!(
            fill_template("shareholder.{id}", |c| (c == "id").then(|| "7".into())),
            Some("shareholder.7".into())
        );
        assert_eq!(
            fill_template("shareholder.{nope}", |c| (c == "id").then(|| "7".into())),
            None,
            "a half-filled subject would be a different entity every run"
        );
    }

    #[tokio::test]
    async fn the_valid_time_column_is_read_into_the_observation() {
        let src = FakeSource {
            columns: columns(),
            records: vec![record(42, Some("1"), "A")],
        };
        let (batch, _, _) = observe(
            &src,
            &mapping(),
            &Checkpoint::start("crm", "holdings", "1"),
            &ctx(),
        )
        .await
        .unwrap();
        let vt = batch.observations[0].valid_time.expect("valid time");
        assert_eq!(
            vt.from.unwrap().to_rfc3339(),
            "2026-04-01T00:00:00+00:00",
            "valid time comes from the declared column, never from our clock"
        );
    }
}
