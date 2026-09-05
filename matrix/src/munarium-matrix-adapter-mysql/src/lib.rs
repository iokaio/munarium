// SPDX-License-Identifier: Apache-2.0
//! The MySQL adapter.
//!
//! The second SQL warehouse behind the same seam as Postgres, and the point of
//! building it is to find out what the seam actually assumed. Three things it
//! did, which this adapter had to make explicit:
//!
//! 1. **Quoting is per engine.** Postgres quotes `"ident"`; MySQL quotes
//!    `` `ident` ``. The compiler already carries the dialect, and the
//!    semantic path already had `Quoting`; sync's table rendering did not, and
//!    it is passed in here rather than assumed.
//! 2. **A snapshot marker is not universal.** Postgres has
//!    `pg_current_snapshot()`, which is a position a later read can be
//!    compared against. MySQL's equivalent is a GTID set, which exists only
//!    when GTID mode is on — off by default, and off in the compose fixture.
//!    So this adapter reports the marker when the server has one and `None`
//!    when it does not, and its `replay_level` is `sealed_result`: the
//!    honest floor, not a claim that a re-read would land on the same rows.
//! 3. **Row-level security is not a given.** Postgres RLS is what makes the
//!    T0 fixture's `matrix_reader` see one region; MySQL has no row-level
//!    policy engine, so per-class principals here mean per-class GRANTs and
//!    views, and `introspect` reports the posture it can actually prove —
//!    not a policy the engine does not have.
//!
//! Mode A is snapshot and watermark, as Postgres: a watermark read is an
//! ordered page after `(watermark, tie_break)` with an inclusive-exclusive
//! boundary, and a delete is invisible to it — which is why this adapter
//! declares no CDC and the plan's binlog path stays unbuilt rather than
//! implied.

// A `Refusal` is a typed answer, not an exception; every adapter returns one
// by value.
#![allow(clippy::result_large_err)]

use munarium_matrix_adapter::{
    BoundParameters, Capabilities, ColumnShape, EffectiveIdentity, ExecutedResult, Limits,
    PolicyStrategy, PostureCheck, ProbeResult, ReadMode, RecordBatch, RolePosture,
    SchemaFingerprint, SourceAdapter, SourceRecord, TableShape, Watermark,
};
use munarium_matrix_core::checkpoint::{Checkpoint, SyncMode};
use munarium_matrix_core::result::{Column, ResultSchema, Row, RowIdRule, TypedResult};
use munarium_matrix_core::value::{ColumnType, Value};
use munarium_matrix_core::{Refusal, RefusalClass, Result};
use munarium_matrix_types::contract::ChangeKind;
use rust_decimal::Decimal;
use sqlx::mysql::{MySqlPool, MySqlPoolOptions, MySqlRow};
use sqlx::{Column as _, Executor as _, Row as _, TypeInfo};

pub struct MySqlAdapter {
    source_id: String,
    pool: MySqlPool,
    /// The schema (MySQL calls it a database) this source's tables live in.
    schema: String,
    principal: String,
}

/// MySQL quotes with backticks and escapes an embedded backtick by doubling.
pub fn quote_ident(ident: &str) -> String {
    format!("`{}`", ident.replace('`', "``"))
}

impl MySqlAdapter {
    pub async fn connect(source_id: &str, url: &str, schema: &str, max_conns: u32) -> Result<Self> {
        let principal = url
            .split("://")
            .nth(1)
            .and_then(|rest| rest.split('@').next())
            .and_then(|userinfo| userinfo.split(':').next())
            .unwrap_or("mysql")
            .to_string();
        let pool = MySqlPoolOptions::new()
            .max_connections(max_conns)
            .connect(url)
            .await
            .map_err(|e| Refusal::source_unavailable(format!("connect: {e}")))?;
        Ok(Self {
            source_id: source_id.to_string(),
            pool,
            schema: schema.to_string(),
            principal,
        })
    }

    pub fn from_pool(source_id: &str, pool: MySqlPool, schema: &str, principal: &str) -> Self {
        Self {
            source_id: source_id.to_string(),
            pool,
            schema: schema.to_string(),
            principal: principal.to_string(),
        }
    }

    /// The closed canon@1 set, from MySQL's own type names.
    pub fn logical_type(name: &str) -> Option<ColumnType> {
        Some(match name.to_uppercase().as_str() {
            "BOOLEAN" | "BOOL" => ColumnType::Bool,
            // MySQL reports TINYINT for BOOLEAN, so a one-bit column is an
            // integer here unless the schema says otherwise. Calling it a
            // bool would be a guess, and a guess in a type is a wrong answer
            // with a scale attached.
            "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "INTEGER" | "BIGINT"
            | "TINYINT UNSIGNED" | "SMALLINT UNSIGNED" | "INT UNSIGNED" | "BIGINT UNSIGNED" => {
                ColumnType::Int64
            }
            "DECIMAL" | "NUMERIC" => ColumnType::Decimal,
            "FLOAT" | "DOUBLE" => ColumnType::Float64,
            "CHAR" | "VARCHAR" | "TEXT" | "TINYTEXT" | "MEDIUMTEXT" | "LONGTEXT" | "ENUM" => {
                ColumnType::String
            }
            "BINARY" | "VARBINARY" | "BLOB" | "TINYBLOB" | "MEDIUMBLOB" | "LONGBLOB" => {
                ColumnType::Bytes
            }
            "DATE" => ColumnType::Date,
            // MySQL's TIMESTAMP is stored UTC and returned in the session
            // zone; DATETIME carries no zone at all. They are different
            // types in canon@1 for exactly that reason.
            "TIMESTAMP" => ColumnType::TimestampTz,
            "DATETIME" => ColumnType::TimestampNaive,
            "JSON" => ColumnType::Json,
            _ => return None,
        })
    }

    fn columns_of(row: &MySqlRow, declared: Option<&[Column]>) -> Result<Vec<Column>> {
        row.columns()
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let name = c.name().to_string();
                let ty = Self::logical_type(c.type_info().name()).ok_or_else(|| {
                    Refusal::schema_drift(format!(
                        "column '{name}' has MySQL type {} which canon@1 does not model; \
                         cast it in the contract's statement or exclude it",
                        c.type_info().name()
                    ))
                })?;
                let declared_col = declared.and_then(|d| d.iter().find(|x| x.name == name));
                Ok(Column {
                    id: format!("c{i}"),
                    name,
                    ty: declared_col.map(|d| d.ty).unwrap_or(ty),
                    nullable: declared_col.map(|d| d.nullable).unwrap_or(true),
                    scale: declared_col.and_then(|d| d.scale),
                    unit: declared_col.and_then(|d| d.unit.clone()),
                    additivity: declared_col.and_then(|d| d.additivity),
                    key: declared_col.map(|d| d.key).unwrap_or(false),
                    element_type: None,
                })
            })
            .collect()
    }

    fn decode_cell(
        row: &MySqlRow,
        idx: usize,
        ty: ColumnType,
        scale: Option<u32>,
    ) -> Result<Value> {
        let bad = |e: sqlx::Error| {
            Refusal::schema_drift(format!("column {idx} did not decode as {ty}: {e}"))
        };
        Ok(match ty {
            ColumnType::Bool => match row.try_get::<Option<bool>, _>(idx).map_err(bad)? {
                Some(v) => Value::Bool(v),
                None => Value::Null,
            },
            ColumnType::Int64 => match row.try_get::<Option<i64>, _>(idx) {
                Ok(Some(v)) => Value::Int64(v),
                Ok(None) => Value::Null,
                // An UNSIGNED BIGINT does not fit i64. Reading it as u64 and
                // refusing the ones that overflow is the only honest option:
                // a silently wrapped id is a citation to the wrong row.
                Err(_) => match row.try_get::<Option<u64>, _>(idx).map_err(bad)? {
                    Some(v) => Value::Int64(i64::try_from(v).map_err(|_| {
                        Refusal::schema_drift(format!(
                            "column {idx} holds {v}, which is outside the signed 64-bit range \
                             canon@1 models; a wrapped value would cite the wrong row"
                        ))
                    })?),
                    None => Value::Null,
                },
            },
            ColumnType::Decimal => match row.try_get::<Option<Decimal>, _>(idx).map_err(bad)? {
                Some(v) => Value::Decimal {
                    value: v,
                    scale: scale.unwrap_or_else(|| v.scale()),
                },
                None => Value::Null,
            },
            ColumnType::Float64 => match row.try_get::<Option<f64>, _>(idx).map_err(bad)? {
                Some(v) => Value::Float64(v),
                None => Value::Null,
            },
            ColumnType::String => match row.try_get::<Option<String>, _>(idx).map_err(bad)? {
                Some(v) => Value::String(v),
                None => Value::Null,
            },
            ColumnType::Bytes => match row.try_get::<Option<Vec<u8>>, _>(idx).map_err(bad)? {
                Some(v) => Value::Bytes(v),
                None => Value::Null,
            },
            ColumnType::Date => match row
                .try_get::<Option<chrono::NaiveDate>, _>(idx)
                .map_err(bad)?
            {
                Some(v) => Value::Date(v),
                None => Value::Null,
            },
            ColumnType::TimestampTz => match row
                .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>(idx)
                .map_err(bad)?
            {
                Some(v) => Value::TimestampTz(v),
                None => Value::Null,
            },
            ColumnType::TimestampNaive => match row
                .try_get::<Option<chrono::NaiveDateTime>, _>(idx)
                .map_err(bad)?
            {
                Some(v) => Value::TimestampNaive(v),
                None => Value::Null,
            },
            ColumnType::Json => match row
                .try_get::<Option<serde_json::Value>, _>(idx)
                .map_err(bad)?
            {
                Some(v) => Value::Json(v),
                None => Value::Null,
            },
            other => {
                return Err(Refusal::schema_drift(format!(
                    "canon@1 type {other} has no MySQL decoding in this adapter"
                )))
            }
        })
    }

    /// The GTID set, when the server has GTID mode on. `None` otherwise —
    /// and `None` is the common case, because GTID is off by default. An
    /// adapter that invented a marker here would let a manifest claim a
    /// position no later read could be compared against.
    async fn gtid_marker(&self) -> Option<String> {
        let (mode,): (String,) = sqlx::query_as("SELECT @@GLOBAL.gtid_mode")
            .fetch_one(&self.pool)
            .await
            .ok()?;
        if !mode.eq_ignore_ascii_case("ON") {
            return None;
        }
        let (gtid,): (String,) = sqlx::query_as("SELECT @@GLOBAL.gtid_executed")
            .fetch_one(&self.pool)
            .await
            .ok()?;
        (!gtid.trim().is_empty()).then(|| format!("gtid:{}", gtid.replace('\n', "")))
    }
}

/// MySQL's error numbers, mapped to the closed refusal set.
pub fn classify_mysql_error(e: &sqlx::Error) -> Refusal {
    if let sqlx::Error::Database(db) = e {
        let code = db.code().unwrap_or_default().to_string();
        let message = db.message().to_string();
        return match code.as_str() {
            // 1044 access denied to database, 1045 access denied to user,
            // 1142 command denied, 1143 column access denied.
            "1044" | "1045" | "1142" | "1143" => Refusal::policy_denied(message),
            // 1146 no such table, 1054 no such column: the contract names
            // something the source does not have. Drift, not unavailability.
            "1146" | "1054" => Refusal::schema_drift(message),
            // 1205 lock wait timeout, 1213 deadlock, 3024 max execution time.
            "1205" | "1213" | "3024" => {
                Refusal::new(RefusalClass::Exhausted, "deadline_exceeded", message)
            }
            _ => Refusal::source_unavailable(message),
        };
    }
    Refusal::source_unavailable(e.to_string())
}

#[async_trait::async_trait]
impl SourceAdapter for MySqlAdapter {
    fn kind(&self) -> &'static str {
        "mysql"
    }

    fn adapter_version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            sync_modes: vec![SyncMode::Snapshot, SyncMode::Watermark],
            policy_strategies: vec![PolicyStrategy::PerClassPrincipals],
            query_contracts: true,
            metric_views: false,
            // A data view is one fact table and declared aggregates; MySQL
            // serves that as well as Postgres does.
            data_views: true,
            semantic_provider: None,
            dialect: Some("mysql".to_string()),
            // Reported per read, when GTID mode is on. The capability says
            // what the ENGINE can offer; the manifest says what this read
            // actually got.
            snapshot_marker: Some("gtid".to_string()),
            replay_level: "sealed_result".into(),
            cancellation: false,
            source_side_limits: true,
        }
    }

    async fn probe(&self) -> Result<ProbeResult> {
        let started = std::time::Instant::now();
        match sqlx::query("SELECT 1").execute(&self.pool).await {
            Ok(_) => Ok(ProbeResult {
                reachable: true,
                latency_ms: Some(started.elapsed().as_millis() as u64),
                detail: None,
            }),
            Err(e) => Ok(ProbeResult {
                reachable: false,
                latency_ms: None,
                detail: Some(classify_mysql_error(&e).message),
            }),
        }
    }

    /// What this connection's role can actually do, proved by asking the
    /// server rather than by trusting the asset.
    async fn introspect(&self) -> Result<(RolePosture, SchemaFingerprint)> {
        let unavailable = |e: sqlx::Error| classify_mysql_error(&e);

        // SUPER (or its finer-grained successors) is the MySQL analogue of a
        // superuser: a role that holds it can read anything and change the
        // server, which is exactly what a read-only source must not be.
        // MySQL 8 returns `SHOW GRANTS` rows as VARBINARY, not VARCHAR: asking
        // sqlx for a String fails with a type mismatch, which a real server said
        // the first time this ran. Bytes, then lossy UTF-8 — a grant line is
        // ASCII in practice, and one odd byte must not abort a posture check.
        let grants: Vec<(Vec<u8>,)> = sqlx::query_as("SHOW GRANTS FOR CURRENT_USER()")
            .fetch_all(&self.pool)
            .await
            .map_err(unavailable)?;
        let all: String = grants
            .iter()
            .map(|(g,)| String::from_utf8_lossy(g).to_uppercase())
            .collect::<Vec<_>>()
            .join("\n");
        let is_super = all.contains("SUPER") || all.contains("ALL PRIVILEGES ON *.*");
        // Any write privilege on the source schema.
        let holds_dml = ["INSERT", "UPDATE", "DELETE", "DROP", "ALTER", "CREATE"]
            .iter()
            .any(|verb| all.contains(&format!("{verb} ")) || all.contains(&format!(", {verb}")));

        // `information_schema` columns come back VARBINARY in MySQL 8, like
        // `SHOW GRANTS`: asking for a String is a type mismatch at decode
        // time, not at compile time, so only a real server finds it. Casting
        // in SQL is clearer than decoding bytes in four places.
        let rows: Vec<(String, String, String, String)> = sqlx::query_as(
            "SELECT CAST(TABLE_NAME AS CHAR), CAST(COLUMN_NAME AS CHAR),
                    CAST(DATA_TYPE AS CHAR), CAST(IS_NULLABLE AS CHAR)
               FROM information_schema.COLUMNS
              WHERE TABLE_SCHEMA = ?
              ORDER BY TABLE_NAME, ORDINAL_POSITION",
        )
        .bind(&self.schema)
        .fetch_all(&self.pool)
        .await
        .map_err(unavailable)?;

        let posture = RolePosture {
            principal: self.principal.clone(),
            checks: vec![
                PostureCheck::new("not_superuser", true, !is_super).with_detail(
                    "SUPER, or ALL PRIVILEGES on *.*, bypasses every grant the source relies on",
                ),
                PostureCheck::new("read_only", true, !holds_dml)
                    .with_detail("the connection user must hold no DML on the source schema"),
                // MySQL has no row-level policy engine. The check is REPORTED
                // and fails, rather than omitted: a reader comparing postures
                // across engines must see that this protection is absent here
                // and is supplied by per-class grants and views instead.
                PostureCheck::new("subject_to_row_security", false, false).with_detail(
                    "MySQL has no row-level policy engine; per-class isolation on this \
                     source is whatever its GRANTs and views provide",
                ),
            ],
        };

        let mut tables: Vec<TableShape> = Vec::new();
        for (table, column, data_type, nullable) in rows {
            let shape = ColumnShape {
                name: column,
                source_type: data_type.clone(),
                logical_type: Self::logical_type(&data_type),
                nullable: nullable == "YES",
            };
            match tables.iter_mut().find(|t| t.name == table) {
                Some(t) => t.columns.push(shape),
                None => tables.push(TableShape {
                    name: table,
                    columns: vec![shape],
                    row_security_enabled: false,
                }),
            }
        }
        let fingerprint = SchemaFingerprint::compute(&tables);
        Ok((
            posture,
            SchemaFingerprint {
                fingerprint,
                tables,
            },
        ))
    }

    async fn read_batch(
        &self,
        entity: &str,
        projection: &[String],
        checkpoint: &Checkpoint,
        read: ReadMode<'_>,
        _identity: &EffectiveIdentity,
        limits: Limits,
    ) -> Result<RecordBatch> {
        let mode = read.mode;
        let watermark = read.watermark;
        self.capabilities().require_sync(mode)?;
        if projection.is_empty() {
            return Err(Refusal::invalid(
                "not_covered",
                "a mysql read needs an explicit projection: selecting * would read columns \
                 the policy denies and would move every time the source adds one",
            ));
        }
        let cols = projection
            .iter()
            .map(|c| quote_ident(c))
            .collect::<Vec<_>>()
            .join(", ");
        let table = format!("{}.{}", quote_ident(&self.schema), quote_ident(entity));
        let probe_limit = limits.max_rows.saturating_add(1);
        let marker = self.gtid_marker().await;

        // The watermark columns come from the DataSource's declaration, not
        // from this adapter (see `Watermark::resolve`). This and the advancing
        // checkpoint below were both wrong until 2026-08-30, in exactly the
        // way Postgres was: `(updated_at, id)` hard-coded, the FIRST read
        // unordered, and `next_checkpoint` handing back the checkpoint it came
        // in with -- so every "incremental" run re-read the whole table and
        // looked like convergence because nothing had changed.
        let wm = Watermark::resolve(mode, watermark)?;
        if let Some(w) = &wm {
            w.require_projected(projection)?;
        }
        let order_by = wm
            .map(|w| match w.tie_break {
                Some(t) => format!("{}, {}", quote_ident(w.column), quote_ident(t)),
                None => quote_ident(w.column),
            })
            .unwrap_or_default();
        let (sql, binds) = match (wm, &checkpoint.watermark) {
            (Some(w), Some(_)) => {
                let cmp = w.cmp();
                match w.tie_break {
                    Some(t) => (
                        format!(
                            "SELECT {cols} FROM {table} WHERE ({wmc}, {tb}) {cmp} (?, ?) \
                             ORDER BY {order_by} LIMIT {probe_limit}",
                            wmc = quote_ident(w.column),
                            tb = quote_ident(t),
                        ),
                        2,
                    ),
                    None => (
                        format!(
                            "SELECT {cols} FROM {table} WHERE {wmc} {cmp} ? \
                             ORDER BY {order_by} LIMIT {probe_limit}",
                            wmc = quote_ident(w.column),
                        ),
                        1,
                    ),
                }
            }
            // The first watermark read: everything, in watermark order, so the
            // last row kept is the checkpoint the next run continues from.
            (Some(_), None) => (
                format!("SELECT {cols} FROM {table} ORDER BY {order_by} LIMIT {probe_limit}"),
                0,
            ),
            _ => (format!("SELECT {cols} FROM {table} LIMIT {probe_limit}"), 0),
        };
        let query = match binds {
            2 => sqlx::query(&sql)
                .bind(checkpoint.watermark.clone().unwrap_or_default())
                .bind(checkpoint.tie_break.clone().unwrap_or_default()),
            1 => sqlx::query(&sql).bind(checkpoint.watermark.clone().unwrap_or_default()),
            _ => sqlx::query(&sql),
        };
        let mut rows = query
            .fetch_all(&self.pool)
            .await
            .map_err(|e| classify_mysql_error(&e))?;

        let truncated = rows.len() as u64 > limits.max_rows;
        if truncated {
            rows.truncate(limits.max_rows as usize);
        }
        let columns = match rows.first() {
            Some(r) => Self::columns_of(r, None)?,
            None => vec![],
        };
        let mut records = Vec::with_capacity(rows.len());
        for r in &rows {
            let mut cells = Vec::with_capacity(columns.len());
            for (i, c) in columns.iter().enumerate() {
                cells.push(Self::decode_cell(r, i, c.ty, c.scale)?);
            }
            let row_key = cells
                .first()
                .and_then(|v| v.canonical_text())
                .unwrap_or_default();
            records.push(SourceRecord {
                cells,
                row_key,
                event_position: marker.clone(),
                change_kind: ChangeKind::Snapshot,
            });
        }
        // ADVANCE to the last row kept -- after truncation, so a run that hit
        // its ceiling resumes from the last row it rendered. An empty read
        // keeps the checkpoint it came in with: nothing newer exists to name.
        let (next_wm, next_tb) = match (wm, records.last()) {
            (Some(w), Some(last)) => {
                let text_of = |name: &str| {
                    columns
                        .iter()
                        .position(|c| c.name == name)
                        .and_then(|i| last.cells.get(i))
                        .and_then(|v| v.canonical_text())
                };
                (
                    text_of(w.column).or_else(|| checkpoint.watermark.clone()),
                    w.tie_break
                        .and_then(text_of)
                        .or_else(|| checkpoint.tie_break.clone()),
                )
            }
            _ => (checkpoint.watermark.clone(), checkpoint.tie_break.clone()),
        };
        Ok(RecordBatch {
            records,
            columns,
            next_checkpoint: Some(Checkpoint {
                source_id: self.source_id.clone(),
                entity: entity.to_string(),
                version: checkpoint.version.clone(),
                watermark: next_wm,
                tie_break: next_tb,
                event_position: marker.clone(),
                schema_fingerprint: checkpoint.schema_fingerprint.clone(),
            }),
            excluded: if truncated { 1 } else { 0 },
            snapshot_marker: marker,
        })
    }

    async fn execute(
        &self,
        statement: &str,
        parameters: &BoundParameters,
        _identity: &EffectiveIdentity,
        limits: Limits,
    ) -> Result<ExecutedResult> {
        let started_at = chrono::Utc::now();
        // MySQL binds by `?` in order. The compiler rewrote the contract's
        // named parameters to `$1`-style placeholders for Postgres, so the
        // positional form is renumbered here rather than in the compiler:
        // the plan hash must not depend on which engine runs the statement.
        let sql = positional_to_question_marks(statement, parameters.positional.len());

        // MySQL refuses `SET TRANSACTION` once a transaction is open —
        // "Transaction characteristics can't be changed while a transaction is
        // in progress" (1568/25001), which is what a real server said the
        // first time this ran. Postgres accepts it as the first statement
        // INSIDE the transaction; MySQL wants it before. So the session is
        // configured on one pooled connection and the transaction is opened on
        // that SAME connection: a pool hands out a different session each
        // time, and settings applied to another one would be a no-op nobody
        // would notice.
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| Refusal::source_unavailable(format!("acquire: {e}")))?;
        // REPEATABLE READ is InnoDB's default and gives the statement a
        // consistent snapshot; READ ONLY lets the engine skip transaction ids
        // and is belt and braces beside the compiler's SELECT-only walk.
        sqlx::query("SET SESSION TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *conn)
            .await
            .map_err(|e| Refusal::source_unavailable(format!("set transaction: {e}")))?;
        sqlx::query(&format!(
            "SET SESSION MAX_EXECUTION_TIME = {}",
            limits.timeout_ms
        ))
        .execute(&mut *conn)
        .await
        .map_err(|e| Refusal::source_unavailable(format!("max_execution_time: {e}")))?;
        // `raw_sql`, not `query`: sqlx PREPARES by default and MySQL answers
        // 1295 "This command is not supported in the prepared statement
        // protocol yet" for transaction control. The second thing a real
        // server taught this adapter.
        // A bare `&str` executes as a SIMPLE query; `sqlx::query` PREPARES,
        // and MySQL answers 1295 "This command is not supported in the
        // prepared statement protocol yet" for transaction control. The
        // second thing a real server taught this adapter.
        conn.execute("START TRANSACTION READ ONLY")
            .await
            .map_err(|e| Refusal::source_unavailable(format!("start transaction: {e}")))?;

        let mut q = sqlx::query(&sql);
        for v in &parameters.positional {
            q = bind_value(q, v);
        }
        let rows = match q.fetch_all(&mut *conn).await {
            Ok(rows) => rows,
            Err(e) => {
                // Close the read-only transaction before the connection goes
                // back to the pool; one left mid-transaction is the next
                // caller's problem.
                let _ = conn.execute("ROLLBACK").await;
                return Err(classify_mysql_error(&e));
            }
        };
        let _ = conn.execute("COMMIT").await;
        let ended_at = chrono::Utc::now();

        let mut rows = rows;
        let truncated = rows.len() as u64 > limits.max_rows;
        if truncated {
            rows.truncate(limits.max_rows as usize);
        }
        let columns = match rows.first() {
            Some(r) => Self::columns_of(r, None)?,
            None => vec![],
        };
        let mut out_rows = Vec::with_capacity(rows.len());
        for r in &rows {
            let mut cells = Vec::with_capacity(columns.len());
            for (i, c) in columns.iter().enumerate() {
                cells.push(Self::decode_cell(r, i, c.ty, c.scale)?);
            }
            out_rows.push(Row { cells });
        }

        Ok(ExecutedResult {
            result: TypedResult {
                schema: ResultSchema {
                    columns,
                    row_id_rule: RowIdRule::Position,
                    order_by: vec![],
                },
                rows: out_rows,
                truncated,
                denied_columns: vec![],
                authorization_class: Default::default(),
            },
            snapshot_marker: self.gtid_marker().await,
            isolation: Some("repeatable read".into()),
            engine: Some("mysql".into()),
            statement_id: None,
            started_at,
            ended_at,
        })
    }

    /// A table's definition as `information_schema` reports it — the same
    /// shape the Postgres adapter returns, so a native data view's
    /// fingerprint means the same thing on both engines.
    async fn definition_of(&self, object: &str, _limits: Limits) -> Result<String> {
        let (schema, table) = match object.split_once('.') {
            Some((s, t)) => (s.to_string(), t.to_string()),
            None => (self.schema.clone(), object.to_string()),
        };
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT CAST(COLUMN_NAME AS CHAR), CAST(DATA_TYPE AS CHAR), CAST(IS_NULLABLE AS CHAR)
               FROM information_schema.COLUMNS
              WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?
              ORDER BY ORDINAL_POSITION",
        )
        .bind(&schema)
        .bind(&table)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| classify_mysql_error(&e))?;
        if rows.is_empty() {
            return Err(Refusal::not_covered(format!(
                "{object} has no columns visible to this principal, or does not exist"
            )));
        }
        Ok(rows
            .iter()
            .map(|(c, t, n)| format!("{c}:{t}:{n}"))
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

fn bind_value<'q>(
    q: sqlx::query::Query<'q, sqlx::MySql, sqlx::mysql::MySqlArguments>,
    v: &'q Value,
) -> sqlx::query::Query<'q, sqlx::MySql, sqlx::mysql::MySqlArguments> {
    match v {
        Value::Null => q.bind(Option::<String>::None),
        Value::Bool(b) => q.bind(*b),
        Value::Int64(i) => q.bind(*i),
        Value::Decimal { value, .. } => q.bind(*value),
        Value::Float64(f) => q.bind(*f),
        Value::String(s) => q.bind(s.clone()),
        Value::Bytes(b) => q.bind(b.clone()),
        Value::Date(d) => q.bind(*d),
        Value::TimestampTz(t) => q.bind(*t),
        Value::TimestampNaive(t) => q.bind(*t),
        // Everything else reaches the engine as its canonical text, which is
        // what the contract declared it as anyway.
        other => q.bind(other.canonical_text().unwrap_or_default()),
    }
}

/// `$1`, `$2`, … → `?`, in order.
///
/// The compiler renumbers a contract's named parameters positionally and
/// renders them Postgres-style; MySQL's placeholder is a bare `?` and its
/// binding is by position, so the SAME plan runs on both engines and the
/// plan hash — which is over the parsed AST — does not move. A `$` inside a
/// string literal is left alone.
pub fn positional_to_question_marks(sql: &str, count: usize) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut chars = sql.char_indices().peekable();
    let mut in_string = false;
    while let Some((i, c)) = chars.next() {
        if c == '\'' {
            in_string = !in_string;
            out.push(c);
            continue;
        }
        if c == '$' && !in_string {
            let digits: String = sql[i + 1..]
                .chars()
                .take_while(|d| d.is_ascii_digit())
                .collect();
            if let Ok(k) = digits.parse::<usize>() {
                if !digits.is_empty() && k >= 1 && k <= count {
                    for _ in 0..digits.len() {
                        chars.next();
                    }
                    out.push('?');
                    continue;
                }
            }
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_backticked_and_an_embedded_backtick_is_doubled() {
        assert_eq!(quote_ident("orders"), "`orders`");
        assert_eq!(quote_ident("we`ird"), "`we``ird`");
    }

    #[test]
    fn positional_placeholders_become_question_marks_in_order() {
        assert_eq!(
            positional_to_question_marks(
                "SELECT a FROM t WHERE b > $1 AND c = $2 AND d = 'x$3y'",
                2
            ),
            "SELECT a FROM t WHERE b > ? AND c = ? AND d = 'x$3y'"
        );
    }

    #[test]
    fn a_placeholder_beyond_the_bound_count_is_left_alone_rather_than_invented() {
        // If the compiler and the binder ever disagree, the engine must see
        // the discrepancy and refuse — not receive a `?` with nothing bound.
        assert_eq!(
            positional_to_question_marks("SELECT $1, $2", 1),
            "SELECT ?, $2"
        );
    }

    #[test]
    fn mysql_types_map_onto_the_closed_set_and_an_unknown_one_is_not_guessed() {
        assert_eq!(
            MySqlAdapter::logical_type("DECIMAL"),
            Some(ColumnType::Decimal)
        );
        assert_eq!(
            MySqlAdapter::logical_type("BIGINT"),
            Some(ColumnType::Int64)
        );
        // TIMESTAMP is zoned and DATETIME is not: different canon@1 types.
        assert_eq!(
            MySqlAdapter::logical_type("TIMESTAMP"),
            Some(ColumnType::TimestampTz)
        );
        assert_eq!(
            MySqlAdapter::logical_type("DATETIME"),
            Some(ColumnType::TimestampNaive)
        );
        assert_eq!(MySqlAdapter::logical_type("GEOMETRY"), None);
        assert_eq!(MySqlAdapter::logical_type("SET"), None);
    }
}
