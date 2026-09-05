// SPDX-License-Identifier: Apache-2.0
//! Semantic intents over a metric view.
//!
//! A metric view is a semantic layer the SOURCE owns — Unity Catalog metric
//! views centralise measures, dimensions, joins and filters in validated YAML
//! and are queried with `MEASURE()`. Matrix does not copy the metric formula
//! into its own asset: it references the view by catalog identity, declares
//! which measures and dimensions a caller may ask for, and compiles a bounded
//! intent — measures, dimensions, equality filters — into the one SQL shape
//! metric views answer. The model never writes this SQL either; it names
//! measures and dimensions from a closed list.
//!
//! What makes the result evidence rather than a number: the dimensions ARE
//! the row key, so every row is citable by its grain; a zero-dimension ask
//! is keyed by a constant `grain = 'total'` column, because a one-row result
//! with no key cannot be sealed under the contract; and the plan hash covers
//! the view identity, the measures, the dimensions and the filter SHAPE (which
//! dimensions, which operators — never the values, which are bound
//! parameters hashed separately by the seal).
//!
//! The fingerprint discipline lives beside it: [`fingerprint`] hashes the
//! definition the source reports for the view, and the workers refuse
//! `metric_view_changed` when the definition at execute time is not the one
//! that was verified. A semantic change upstream becomes a governed drift
//! event instead of a silently different number.

use crate::refusal::{Refusal, RefusalClass};
use crate::result::{Additivity, Column, ResultSchema, RowIdRule};
use crate::value::ColumnType;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// The plan kind sealed into a manifest for a semantic execution.
pub const SEMANTIC_PLAN_KIND: &str = "semantic@1";

/// The key column a zero-dimension result is keyed by.
pub const TOTAL_GRAIN_COLUMN: &str = "grain";
/// Its one value.
pub const TOTAL_GRAIN_VALUE: &str = "total";

#[derive(Debug, Clone, PartialEq)]
pub struct MeasureDef {
    pub ty: ColumnType,
    pub scale: Option<u32>,
    pub unit: Option<String>,
    pub additivity: Option<Additivity>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DimensionDef {
    pub ty: ColumnType,
}

/// How identifiers are quoted in the engine the statement is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quoting {
    /// Databricks / Spark SQL, MySQL, BigQuery: `` `name` ``.
    Backtick,
    /// Postgres, Snowflake and the SQL standard: `"name"`.
    Double,
    /// SQL Server: `[name]`.
    Bracket,
}

/// How bound values are referenced in the statement text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceholderStyle {
    /// `:f0` — engines that bind by name (Databricks).
    Named,
    /// `$1` — engines that bind by position (Postgres).
    Positional,
    /// `?` — engines that bind by position but do not number (MySQL,
    /// Snowflake). Distinct from [`Positional`](Self::Positional) because the
    /// TEXT differs; the binding order is the same.
    Question,
    /// `@f0` — engines that bind by name with an at-sign (BigQuery, SQL
    /// Server).
    AtName,
}

/// The aggregate a native measure applies over its column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeOp {
    Sum,
    Count,
    Min,
    Max,
    Avg,
}

impl NativeOp {
    pub fn sql(self) -> &'static str {
        match self {
            NativeOp::Sum => "SUM",
            NativeOp::Count => "COUNT",
            NativeOp::Min => "MIN",
            NativeOp::Max => "MAX",
            NativeOp::Avg => "AVG",
        }
    }
}

/// A native measure: one aggregate over one column of one fact table.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeMeasure {
    pub op: NativeOp,
    /// `None` only for `count`, which counts rows.
    pub column: Option<String>,
}

/// Where the measures' formulas live.
#[derive(Debug, Clone, PartialEq)]
pub enum SemanticBackend {
    /// A metric view the source owns: `MEASURE(name)` over the view.
    MetricView,
    /// A single fact table with the aggregates declared in the asset
    /// (the minimal native `DataView`): no joins, so the grain is
    /// the table's and fan-out cannot happen.
    Native {
        measures: BTreeMap<String, NativeMeasure>,
        /// dimension name → source column.
        dimensions: BTreeMap<String, String>,
    },
}

/// What a metric-view asset permits: the closed lists an intent is checked
/// against. Built from the asset by the types crate; core never parses YAML.
#[derive(Debug, Clone, PartialEq)]
pub struct SemanticScope {
    /// The view's identity as declared — `catalog.schema.name`, or
    /// `schema.name` when the source's catalog is implied. Quoted per part.
    pub view: String,
    pub measures: BTreeMap<String, MeasureDef>,
    pub dimensions: BTreeMap<String, DimensionDef>,
    /// Dimensions a filter may name. Empty means every declared dimension.
    pub filterable: Vec<String>,
    /// Most dimensions one intent may group by. 0 means no ceiling.
    pub max_dimensions: usize,
    pub backend: SemanticBackend,
    pub quoting: Quoting,
    pub placeholders: PlaceholderStyle,
}

impl SemanticScope {
    /// A metric-view scope: Databricks quoting and named placeholders, which
    /// is the only engine with metric views today.
    pub fn metric_view(
        view: impl Into<String>,
        measures: BTreeMap<String, MeasureDef>,
        dimensions: BTreeMap<String, DimensionDef>,
        filterable: Vec<String>,
        max_dimensions: usize,
    ) -> Self {
        Self {
            view: view.into(),
            measures,
            dimensions,
            filterable,
            max_dimensions,
            backend: SemanticBackend::MetricView,
            quoting: Quoting::Backtick,
            placeholders: PlaceholderStyle::Named,
        }
    }

    /// Quoting and placeholders for a dialect name as the adapters report it.
    ///
    /// **Exhaustive, and it refuses what it does not know.** Until 2026-08-30
    /// this was two branches — Databricks, and a catch-all that handed
    /// everything else Postgres's double quotes and `$1`. That was correct
    /// while two adapters existed and silently wrong the moment four more
    /// arrived: a native `DataView` on MySQL compiled to
    /// `SELECT ... FROM "opportunities"`, which MySQL reads as a **string
    /// literal**, and on SQL Server and BigQuery the placeholders were a shape
    /// the driver does not bind.
    ///
    /// The catch-all is the real defect, not the missing rows. A default that
    /// produces a plausible statement for an engine nobody taught it about
    /// fails as a wrong ANSWER rather than as a refusal, and a wrong answer
    /// from a system whose whole purpose is verifiable evidence is the worst
    /// failure it has. So an unknown dialect is a `Refusal` that names it.
    pub fn try_with_dialect(mut self, dialect: &str) -> Result<Self, Refusal> {
        let (q, p) = match dialect.to_lowercase().as_str() {
            "databricks" => (Quoting::Backtick, PlaceholderStyle::Named),
            "postgres" | "postgresql" => (Quoting::Double, PlaceholderStyle::Positional),
            "mysql" => (Quoting::Backtick, PlaceholderStyle::Question),
            "snowflake" => (Quoting::Double, PlaceholderStyle::Question),
            "bigquery" => (Quoting::Backtick, PlaceholderStyle::AtName),
            "sqlserver" => (Quoting::Bracket, PlaceholderStyle::AtName),
            other => {
                return Err(Refusal::new(
                    RefusalClass::NotCovered,
                    "not_covered",
                    format!(
                        "no semantic dialect for '{other}': a view cannot be compiled for an \
                         engine whose quoting and placeholder style this build does not know. \
                         Guessing produces a statement that runs and means something else."
                    ),
                ))
            }
        };
        self.quoting = q;
        self.placeholders = p;
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FilterRef<'a> {
    pub dimension: &'a str,
    pub op: &'a str,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SemanticRequest<'a> {
    pub measures: &'a [String],
    pub dimensions: &'a [String],
    pub filters: Vec<FilterRef<'a>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompiledSemantic {
    /// The statement, with one `:fN` named placeholder per filter in filter
    /// order. The caller binds `parameter_names[N]` to the filter's value.
    pub sql: String,
    /// The declared result shape: dimensions (keys) then measures.
    pub schema: ResultSchema,
    /// Over the view, the measures, the dimensions and the filter shape.
    pub plan_hash: String,
    /// `f0`, `f1`, … — the names the placeholders bind by.
    pub parameter_names: Vec<String>,
    /// The dimension each parameter filters, in the same order.
    pub parameter_dimensions: Vec<String>,
}

impl Refusal {
    pub fn metric_not_covered(message: impl Into<String>) -> Self {
        Self::new(RefusalClass::NotCovered, "metric_not_covered", message)
    }
    /// The view's definition is not the one that was verified. Class
    /// `Invalid` like `schema_drift`: it comes from the engine, so it spends
    /// budget, and it is not retryable until someone re-verifies.
    pub fn metric_view_changed(message: impl Into<String>) -> Self {
        Self::new(RefusalClass::Invalid, "metric_view_changed", message)
    }
}

/// Backtick-quote one identifier part the way Databricks SQL expects,
/// doubling any backtick inside it.
pub fn quote_ident(part: &str) -> String {
    quote_ident_with(Quoting::Backtick, part)
}

/// Quote one identifier part for an engine, doubling the quote character
/// inside it.
pub fn quote_ident_with(q: Quoting, part: &str) -> String {
    match q {
        Quoting::Backtick => format!("`{}`", part.replace('`', "``")),
        Quoting::Double => format!("\"{}\"", part.replace('"', "\"\"")),
        // `]` is the only character that needs escaping inside a bracketed
        // identifier, and it doubles like the others.
        Quoting::Bracket => format!("[{}]", part.replace(']', "]]")),
    }
}

/// Quote a dotted identity part by part: `a.b.c` → `` `a`.`b`.`c` ``.
pub fn quote_identity(identity: &str) -> String {
    quote_identity_with(Quoting::Backtick, identity)
}

pub fn quote_identity_with(q: Quoting, identity: &str) -> String {
    identity
        .split('.')
        .map(|p| quote_ident_with(q, p))
        .collect::<Vec<_>>()
        .join(".")
}

/// `sha256:<hex>` over a view definition, LF-normalised and trimmed so a
/// re-serialisation that only moves whitespace at the ends is not a change.
pub fn fingerprint(definition: &str) -> String {
    let normalised = definition.replace("\r\n", "\n");
    let mut h = Sha256::new();
    h.update(normalised.trim().as_bytes());
    format!("sha256:{}", hex::encode(h.finalize()))
}

/// Compile a bounded semantic request against a scope.
///
/// Refuses `metric_not_covered` for anything outside the closed lists —
/// an undeclared measure or dimension, a duplicate, a filter on a dimension
/// the asset did not open to filtering, an operator other than `eq`, or more
/// dimensions than the asset's ceiling. Nothing here is a parse of caller
/// SQL: the statement is assembled from declared names only.
pub fn compile(
    scope: &SemanticScope,
    req: &SemanticRequest<'_>,
) -> Result<CompiledSemantic, Refusal> {
    if req.measures.is_empty() {
        return Err(Refusal::metric_not_covered(
            "a semantic intent names at least one measure",
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for m in req.measures {
        if !scope.measures.contains_key(m) {
            return Err(Refusal::metric_not_covered(format!(
                "measure '{m}' is not declared by this metric view; declared: {}",
                scope
                    .measures
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        if !seen.insert(m.as_str()) {
            return Err(Refusal::metric_not_covered(format!(
                "measure '{m}' is named twice"
            )));
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for d in req.dimensions {
        if !scope.dimensions.contains_key(d) {
            return Err(Refusal::metric_not_covered(format!(
                "dimension '{d}' is not declared by this metric view; declared: {}",
                scope
                    .dimensions
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        if !seen.insert(d.as_str()) {
            return Err(Refusal::metric_not_covered(format!(
                "dimension '{d}' is named twice"
            )));
        }
    }
    if scope.max_dimensions > 0 && req.dimensions.len() > scope.max_dimensions {
        return Err(Refusal::metric_not_covered(format!(
            "this metric view allows at most {} dimension(s) per question; {} were asked",
            scope.max_dimensions,
            req.dimensions.len()
        )));
    }
    for f in &req.filters {
        if !scope.dimensions.contains_key(f.dimension) {
            return Err(Refusal::metric_not_covered(format!(
                "filter dimension '{}' is not declared by this metric view",
                f.dimension
            )));
        }
        if !scope.filterable.is_empty() && !scope.filterable.iter().any(|x| x == f.dimension) {
            return Err(Refusal::metric_not_covered(format!(
                "dimension '{}' is not open to filtering; filterable: {}",
                f.dimension,
                scope.filterable.join(", ")
            )));
        }
        if f.op != "eq" {
            return Err(Refusal::metric_not_covered(format!(
                "filter operator '{}' is not supported; only `eq` is",
                f.op
            )));
        }
    }

    // --- projection: dimensions (keys), then one aggregate per measure —
    // `MEASURE(name)` over a metric view, or the declared aggregate over the
    // declared column of a native fact table.
    let q = scope.quoting;
    let qi = |part: &str| quote_ident_with(q, part);
    // The expression a dimension is grouped and ordered by: the dimension
    // itself on a metric view, its source column on a native table.
    let dim_expr = |d: &str| -> String {
        match &scope.backend {
            SemanticBackend::MetricView => qi(d),
            SemanticBackend::Native { dimensions, .. } => {
                qi(dimensions.get(d).map(String::as_str).unwrap_or(d))
            }
        }
    };
    let mut select = Vec::new();
    let mut columns = Vec::new();
    let mut order_by = Vec::new();
    if req.dimensions.is_empty() {
        select.push(format!(
            "'{TOTAL_GRAIN_VALUE}' AS {}",
            qi(TOTAL_GRAIN_COLUMN)
        ));
        columns.push(Column::new("c0", TOTAL_GRAIN_COLUMN, ColumnType::String).key());
        order_by.push(TOTAL_GRAIN_COLUMN.to_string());
    }
    for d in req.dimensions {
        let def = &scope.dimensions[d];
        select.push(format!("{} AS {}", dim_expr(d), qi(d)));
        // A dimension is a KEY column, and a key is never nullable: the seal
        // refuses a nullable key (`result_not_identifiable`), because a NULL
        // key identifies no row — measured on dev the first time a native
        // view was verified. A GROUP BY that produces a NULL group therefore
        // refuses at seal rather than citing a row nothing can name.
        let c = Column::new(&format!("c{}", columns.len()), d, def.ty).key();
        columns.push(c);
        order_by.push(d.clone());
    }
    for m in req.measures {
        let def = &scope.measures[m];
        let expr = match &scope.backend {
            SemanticBackend::MetricView => format!("MEASURE({})", qi(m)),
            SemanticBackend::Native { measures, .. } => {
                let nm = measures.get(m).ok_or_else(|| {
                    Refusal::metric_not_covered(format!(
                        "measure '{m}' declares no aggregate on this data view"
                    ))
                })?;
                match (&nm.op, &nm.column) {
                    (NativeOp::Count, None) => "COUNT(*)".to_string(),
                    (op, Some(col)) => format!("{}({})", op.sql(), qi(col)),
                    (op, None) => {
                        return Err(Refusal::metric_not_covered(format!(
                            "measure '{m}' is {} over no column",
                            op.sql()
                        )))
                    }
                }
            }
        };
        select.push(format!("{expr} AS {}", qi(m)));
        let mut c = Column::new(&format!("c{}", columns.len()), m, def.ty);
        c.nullable = true;
        c.scale = def.scale;
        c.unit = def.unit.clone();
        c.additivity = def.additivity;
        columns.push(c);
    }

    // --- filters: equality on a declared dimension, the value bound in the
    // engine's own style — by name or by position.
    let mut where_parts = Vec::new();
    let mut parameter_names = Vec::new();
    let mut parameter_dimensions = Vec::new();
    for (i, f) in req.filters.iter().enumerate() {
        let name = format!("f{i}");
        let placeholder = match scope.placeholders {
            PlaceholderStyle::Named => format!(":{name}"),
            PlaceholderStyle::Positional => format!("${}", i + 1),
            // `?` carries no name, so the ORDER of `parameter_names` is the
            // only thing that binds a value to a filter. It already is —
            // `parameter_names` is pushed in filter order below — but the
            // dependency is invisible at the call site, which is why it is
            // said here.
            PlaceholderStyle::Question => "?".to_string(),
            PlaceholderStyle::AtName => format!("@{name}"),
        };
        where_parts.push(format!("{} = {placeholder}", dim_expr(f.dimension)));
        parameter_names.push(name);
        parameter_dimensions.push(f.dimension.to_string());
    }

    let mut sql = format!(
        "SELECT {} FROM {}",
        select.join(", "),
        quote_identity_with(q, &scope.view)
    );
    if !where_parts.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&where_parts.join(" AND "));
    }
    if !req.dimensions.is_empty() {
        let group: Vec<String> = req.dimensions.iter().map(|d| dim_expr(d)).collect();
        sql.push_str(" GROUP BY ");
        sql.push_str(&group.join(", "));
        sql.push_str(" ORDER BY ");
        sql.push_str(&group.join(", "));
    }

    // --- plan hash: shape, never values.
    let mut h = Sha256::new();
    h.update(SEMANTIC_PLAN_KIND.as_bytes());
    h.update([0x1f]);
    h.update(match &scope.backend {
        SemanticBackend::MetricView => b"metric_view".as_slice(),
        SemanticBackend::Native { .. } => b"native".as_slice(),
    });
    h.update([0x1f]);
    h.update(scope.view.as_bytes());
    h.update([0x1f]);
    h.update(req.measures.join(",").as_bytes());
    h.update([0x1f]);
    h.update(req.dimensions.join(",").as_bytes());
    h.update([0x1f]);
    for f in &req.filters {
        h.update(f.dimension.as_bytes());
        h.update(b":");
        h.update(f.op.as_bytes());
        h.update([0x1e]);
    }
    let plan_hash = format!("sha256:{}", hex::encode(h.finalize()));

    Ok(CompiledSemantic {
        sql,
        schema: ResultSchema {
            columns,
            row_id_rule: RowIdRule::Keys,
            order_by,
        },
        plan_hash,
        parameter_names,
        parameter_dimensions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> SemanticScope {
        let mut measures = BTreeMap::new();
        measures.insert(
            "open_pipeline".to_string(),
            MeasureDef {
                ty: ColumnType::Decimal,
                scale: Some(2),
                unit: Some("USD".into()),
                additivity: Some(Additivity::Additive),
            },
        );
        measures.insert(
            "deal_count".to_string(),
            MeasureDef {
                ty: ColumnType::Int64,
                scale: None,
                unit: None,
                additivity: Some(Additivity::Additive),
            },
        );
        let mut dimensions = BTreeMap::new();
        dimensions.insert(
            "region".to_string(),
            DimensionDef {
                ty: ColumnType::String,
            },
        );
        dimensions.insert(
            "stage".to_string(),
            DimensionDef {
                ty: ColumnType::String,
            },
        );
        SemanticScope::metric_view(
            "mxtest.pipeline_metrics",
            measures,
            dimensions,
            vec!["region".into()],
            2,
        )
    }

    #[test]
    fn every_adapter_dialect_gets_its_own_quoting_and_placeholders() {
        // The six an adapter can report. MySQL is the one the catch-all got
        // wrong for a day: `"opportunities"` is a STRING LITERAL there, so the
        // statement ran and meant something else.
        for (dialect, q, p) in [
            ("databricks", Quoting::Backtick, PlaceholderStyle::Named),
            ("postgres", Quoting::Double, PlaceholderStyle::Positional),
            ("mysql", Quoting::Backtick, PlaceholderStyle::Question),
            ("snowflake", Quoting::Double, PlaceholderStyle::Question),
            ("bigquery", Quoting::Backtick, PlaceholderStyle::AtName),
            ("sqlserver", Quoting::Bracket, PlaceholderStyle::AtName),
        ] {
            let s = scope().try_with_dialect(dialect).expect(dialect);
            assert_eq!(s.quoting, q, "{dialect} quoting");
            assert_eq!(s.placeholders, p, "{dialect} placeholders");
        }
    }

    #[test]
    fn an_unknown_dialect_is_refused_rather_than_given_postgres() {
        // The whole point. A default that produces a plausible statement for
        // an engine nobody taught it about fails as a wrong ANSWER rather than
        // as a refusal, and a wrong answer is the worst failure this system
        // has.
        let e = scope().try_with_dialect("duckdb").unwrap_err();
        assert_eq!(e.class, RefusalClass::NotCovered);
        assert!(e.message.contains("duckdb"), "{}", e.message);
        assert!(e.message.contains("means something else"), "{}", e.message);
    }

    #[test]
    fn a_bracketed_identifier_doubles_its_closing_bracket() {
        assert_eq!(quote_ident_with(Quoting::Bracket, "od]d"), "[od]]d]");
        assert_eq!(quote_ident_with(Quoting::Backtick, "o`d"), "`o``d`");
        assert_eq!(quote_ident_with(Quoting::Double, "o\"d"), "\"o\"\"d\"");
    }

    fn native_scope() -> SemanticScope {
        let mut s = scope().try_with_dialect("postgres").expect("known dialect");
        s.view = "crm.opportunities".into();
        let mut measures = BTreeMap::new();
        measures.insert(
            "open_pipeline".to_string(),
            NativeMeasure {
                op: NativeOp::Sum,
                column: Some("amount".into()),
            },
        );
        measures.insert(
            "deal_count".to_string(),
            NativeMeasure {
                op: NativeOp::Count,
                column: None,
            },
        );
        let mut dimensions = BTreeMap::new();
        dimensions.insert("region".to_string(), "region".to_string());
        dimensions.insert("stage".to_string(), "stage".to_string());
        s.backend = SemanticBackend::Native {
            measures,
            dimensions,
        };
        s
    }

    #[test]
    fn a_native_view_aggregates_its_own_columns_with_the_engines_quoting_and_placeholders() {
        let c = compile(
            &native_scope(),
            &SemanticRequest {
                measures: &["open_pipeline".into(), "deal_count".into()],
                dimensions: &["region".into()],
                filters: vec![FilterRef {
                    dimension: "region",
                    op: "eq",
                }],
            },
        )
        .unwrap();
        assert_eq!(
            c.sql,
            "SELECT \"region\" AS \"region\", SUM(\"amount\") AS \"open_pipeline\", COUNT(*) AS \"deal_count\" \
             FROM \"crm\".\"opportunities\" WHERE \"region\" = $1 GROUP BY \"region\" ORDER BY \"region\""
        );
        assert_eq!(c.parameter_names, vec!["f0".to_string()]);
        let mv = compile(
            &scope(),
            &SemanticRequest {
                measures: &["open_pipeline".into()],
                dimensions: &["region".into()],
                filters: vec![],
            },
        )
        .unwrap();
        let nv = compile(
            &native_scope(),
            &SemanticRequest {
                measures: &["open_pipeline".into()],
                dimensions: &["region".into()],
                filters: vec![],
            },
        )
        .unwrap();
        assert_ne!(
            mv.plan_hash, nv.plan_hash,
            "a native plan is not a metric-view plan"
        );
    }

    #[test]
    fn a_grouped_ask_is_keyed_by_its_dimensions_and_measured_with_measure() {
        let c = compile(
            &scope(),
            &SemanticRequest {
                measures: &["open_pipeline".into()],
                dimensions: &["region".into()],
                filters: vec![],
            },
        )
        .unwrap();
        assert_eq!(
            c.sql,
            "SELECT `region` AS `region`, MEASURE(`open_pipeline`) AS `open_pipeline` \
             FROM `mxtest`.`pipeline_metrics` GROUP BY `region` ORDER BY `region`"
        );
        assert_eq!(c.schema.row_id_rule, RowIdRule::Keys);
        assert!(c.schema.columns[0].key && !c.schema.columns[1].key);
        assert_eq!(c.schema.columns[1].scale, Some(2));
        assert_eq!(c.schema.order_by, vec!["region".to_string()]);
        assert!(c.parameter_names.is_empty());
    }

    #[test]
    fn a_total_is_keyed_by_a_constant_grain_so_it_can_be_sealed() {
        let c = compile(
            &scope(),
            &SemanticRequest {
                measures: &["deal_count".into()],
                dimensions: &[],
                filters: vec![],
            },
        )
        .unwrap();
        assert_eq!(
            c.sql,
            "SELECT 'total' AS `grain`, MEASURE(`deal_count`) AS `deal_count` \
             FROM `mxtest`.`pipeline_metrics`"
        );
        assert_eq!(c.schema.columns[0].name, "grain");
        assert!(c.schema.columns[0].key);
    }

    #[test]
    fn a_filter_binds_by_name_and_is_part_of_the_plan_shape_not_its_value() {
        let with = compile(
            &scope(),
            &SemanticRequest {
                measures: &["open_pipeline".into()],
                dimensions: &["stage".into()],
                filters: vec![FilterRef {
                    dimension: "region",
                    op: "eq",
                }],
            },
        )
        .unwrap();
        assert!(with.sql.contains(" WHERE `region` = :f0 GROUP BY `stage`"));
        assert_eq!(with.parameter_names, vec!["f0".to_string()]);
        assert_eq!(with.parameter_dimensions, vec!["region".to_string()]);
        let without = compile(
            &scope(),
            &SemanticRequest {
                measures: &["open_pipeline".into()],
                dimensions: &["stage".into()],
                filters: vec![],
            },
        )
        .unwrap();
        assert_ne!(
            with.plan_hash, without.plan_hash,
            "the filter shape is plan"
        );
    }

    #[test]
    fn everything_outside_the_closed_lists_is_metric_not_covered() {
        let s = scope();
        type Case = (Vec<String>, Vec<String>, Vec<(&'static str, &'static str)>);
        let owned: Vec<Case> = vec![
            (vec![], vec![], vec![]),
            (vec!["margin".into()], vec![], vec![]),
            (vec!["deal_count".into()], vec!["owner".into()], vec![]),
            (vec!["deal_count".into()], vec![], vec![("stage", "eq")]),
            (vec!["deal_count".into()], vec![], vec![("region", "like")]),
            (
                vec!["deal_count".into()],
                vec!["region".into(), "stage".into(), "region".into()],
                vec![],
            ),
        ];
        for (measures, dimensions, filters) in &owned {
            let req = SemanticRequest {
                measures,
                dimensions,
                filters: filters
                    .iter()
                    .map(|(d, op)| FilterRef { dimension: d, op })
                    .collect(),
            };
            let e = compile(&s, &req).unwrap_err();
            assert_eq!(e.code, "metric_not_covered", "{req:?}");
            assert_eq!(e.class, RefusalClass::NotCovered);
        }
    }

    #[test]
    fn identifiers_are_quoted_and_backticks_inside_them_doubled() {
        assert_eq!(quote_identity("a.b`c.d"), "`a`.`b``c`.`d`");
    }

    #[test]
    fn a_fingerprint_ignores_line_endings_and_edge_whitespace_only() {
        let a = fingerprint("CREATE VIEW v WITH METRICS\nLANGUAGE YAML AS $$x$$\n");
        let b = fingerprint("CREATE VIEW v WITH METRICS\r\nLANGUAGE YAML AS $$x$$");
        let c = fingerprint("CREATE VIEW v WITH METRICS\nLANGUAGE YAML AS $$y$$");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.starts_with("sha256:"));
    }
}
