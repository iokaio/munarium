// SPDX-License-Identifier: Apache-2.0
//! The PostgreSQL adapter.
//!
//! Three things here are load-bearing and worth reading before changing
//! anything:
//!
//! 1. **The role posture is PROVEN, not configured.** `introspect` asks the
//!    catalog whether this role is a superuser, whether it can bypass RLS,
//!    whether it owns the tables it reads, and whether it holds any DML. A
//!    role that fails is refused before it can read a single row — because a
//!    "read-only" connection string is a claim and `pg_roles` is a fact.
//!
//! 2. **Every read runs in a READ ONLY, REPEATABLE READ transaction** with a
//!    `statement_timeout`. That gives a real snapshot marker
//!    (`pg_current_snapshot()`), makes the deadline enforceable server-side,
//!    and means a mid-read commit elsewhere cannot change what we sealed.
//!
//! 3. **Parameters are bound, never interpolated.** The compiler hands over a
//!    statement with `$1..$n` placeholders and a positional value list; this
//!    module binds them through sqlx. No statement text is ever built from a
//!    caller's value.

#![forbid(unsafe_code)]
#![allow(clippy::result_large_err)]

pub mod pgoutput;

use async_trait::async_trait;
use munarium_matrix_adapter::*;
use munarium_matrix_core::checkpoint::{Checkpoint, SyncMode};
use munarium_matrix_core::value::{ColumnType, Value};
use munarium_matrix_core::{
    AuthorizationClass, Column, Refusal, RefusalClass, ResultSchema, Row, RowIdRule, TypedResult,
};
use munarium_matrix_types::contract::ChangeKind;
use rust_decimal::Decimal;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgRow};
use sqlx::{Column as _, PgPool, Row as _, TypeInfo};
use std::str::FromStr;

pub const ADAPTER_VERSION: &str = "postgres@0.1.0";

pub struct PostgresAdapter {
    source_id: String,
    pool: PgPool,
    /// The schema this source's entities live in.
    schema: String,
    /// What the source will report as the effective principal.
    principal: String,
    /// The replication slot and publication a `cdc` sync reads: the
    /// `munarium_matrix_<source>` convention unless the DataSource's
    /// `sync.cdc` names otherwise (2026-08-30).
    cdc: CdcObjects,
}

impl PostgresAdapter {
    /// Connect with a resolved credential. The URL is never stored: only the
    /// pool, which cannot be printed back into a log line.
    pub async fn connect(source_id: &str, url: &str, schema: &str, max_conns: u32) -> Result<Self> {
        let opts = PgConnectOptions::from_str(url)
            .map_err(|e| Refusal::invalid("not_covered", format!("connection settings: {e}")))?;
        let principal = opts.get_username().to_string();
        let pool = PgPoolOptions::new()
            .max_connections(max_conns)
            .connect_with(opts)
            .await
            .map_err(|e| Refusal::source_unavailable(format!("connect: {e}")))?;
        Ok(Self {
            cdc: cdc_objects(source_id),
            source_id: source_id.to_string(),
            pool,
            schema: schema.to_string(),
            principal,
        })
    }

    /// Read a slot and publication the operator named instead of the
    /// convention. `None` keeps the convention for that object, so a source
    /// may rename one and not the other.
    pub fn with_cdc_objects(mut self, slot: Option<&str>, publication: Option<&str>) -> Self {
        self.cdc = self.cdc.overridden(slot, publication);
        self
    }

    pub fn from_pool(source_id: &str, pool: PgPool, schema: &str, principal: &str) -> Self {
        Self {
            cdc: cdc_objects(source_id),
            source_id: source_id.to_string(),
            pool,
            schema: schema.to_string(),
            principal: principal.to_string(),
        }
    }

    /// Map a Postgres type name onto the closed canon@1 set. `None` means the
    /// column cannot be used — reported honestly rather than stringified.
    pub fn logical_type(pg_type: &str) -> Option<ColumnType> {
        Some(match pg_type.to_lowercase().as_str() {
            "bool" | "boolean" => ColumnType::Bool,
            "int2" | "int4" | "int8" | "smallint" | "integer" | "bigint" => ColumnType::Int64,
            "numeric" | "decimal" | "money" => ColumnType::Decimal,
            "float4" | "float8" | "real" | "double precision" => ColumnType::Float64,
            "text" | "varchar" | "character varying" | "char" | "bpchar" | "name" => {
                ColumnType::String
            }
            "bytea" => ColumnType::Bytes,
            "date" => ColumnType::Date,
            "timestamptz" | "timestamp with time zone" => ColumnType::TimestampTz,
            "timestamp" | "timestamp without time zone" => ColumnType::TimestampNaive,
            "uuid" => ColumnType::Uuid,
            "json" | "jsonb" => ColumnType::Json,
            "interval" => ColumnType::Interval,
            _ => return None,
        })
    }

    /// Decode one cell. `numeric` arrives as `rust_decimal`, never as a float:
    /// that is the whole reason `sqlx`'s `rust_decimal` feature is on.
    fn decode_cell(row: &PgRow, idx: usize, ty: ColumnType, scale: Option<u32>) -> Result<Value> {
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
                // int2/int4 columns need their own width.
                Err(_) => match row.try_get::<Option<i32>, _>(idx).map_err(bad)? {
                    Some(v) => Value::Int64(v as i64),
                    None => Value::Null,
                },
            },
            ColumnType::Decimal => match row.try_get::<Option<Decimal>, _>(idx).map_err(bad)? {
                Some(v) => Value::Decimal {
                    value: v,
                    // The declared scale wins: it is part of the contract's
                    // identity, and the column's storage scale may be wider.
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
            ColumnType::Date => {
                match row
                    .try_get::<Option<chrono::NaiveDate>, _>(idx)
                    .map_err(bad)?
                {
                    Some(v) => Value::Date(v),
                    None => Value::Null,
                }
            }
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
            ColumnType::Uuid => match row.try_get::<Option<uuid::Uuid>, _>(idx).map_err(bad)? {
                Some(v) => Value::Uuid(v.to_string()),
                None => Value::Null,
            },
            ColumnType::Json => {
                match row
                    .try_get::<Option<serde_json::Value>, _>(idx)
                    .map_err(bad)?
                {
                    Some(v) => Value::Json(v),
                    None => Value::Null,
                }
            }
            ColumnType::Interval | ColumnType::Array => {
                return Err(Refusal::not_covered(format!(
                    "the postgres adapter does not decode {ty} columns yet"
                )))
            }
        })
    }

    /// Columns of a result set, typed from the driver's own type names.
    fn columns_of(row: &PgRow, declared: Option<&[Column]>) -> Result<Vec<Column>> {
        row.columns()
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let name = c.name().to_string();
                // A declared column wins: the contract's type, scale and unit
                // are part of evidence identity and must not be re-derived
                // from whatever the driver guessed.
                if let Some(d) = declared.and_then(|d| d.iter().find(|dc| dc.name == name)) {
                    return Ok(d.clone());
                }
                let pg = c.type_info().name().to_string();
                let ty = Self::logical_type(&pg).ok_or_else(|| {
                    Refusal::not_covered(format!(
                        "column '{name}' has source type '{pg}', which maps to no canon@1 type"
                    ))
                })?;
                Ok(Column {
                    id: format!("c{i}"),
                    name,
                    ty,
                    nullable: true,
                    scale: None,
                    unit: None,
                    additivity: None,
                    key: false,
                    element_type: None,
                })
            })
            .collect()
    }
}

/// Quote an identifier for interpolation into a generated statement.
///
/// Used ONLY for identifiers that came from a validated asset (a table name, a
/// projection column) — never for a value. Doubling embedded quotes is the
/// standard escape and makes a crafted identifier inert.
fn quote_ident(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

#[async_trait]
impl SourceAdapter for PostgresAdapter {
    fn kind(&self) -> &'static str {
        "postgres"
    }

    fn adapter_version(&self) -> &'static str {
        ADAPTER_VERSION
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            // Cdc is the logical-replication path. It is declared
            // beside the others rather than instead of them: a source whose
            // operator will not run a replication slot still materializes by
            // watermark, and the refusal for the objects CDC needs is only
            // reached by a source that asked for CDC.
            sync_modes: vec![SyncMode::Snapshot, SyncMode::Watermark, SyncMode::Cdc],
            // RLS makes source-native the right strategy; per-class principals
            // also work when a deployment prefers separate roles.
            policy_strategies: vec![
                PolicyStrategy::SourceNative,
                PolicyStrategy::PerClassPrincipals,
            ],
            query_contracts: true,
            metric_views: false,
            data_views: true,
            semantic_provider: None,
            dialect: Some("postgres".into()),
            snapshot_marker: Some("pg_snapshot".into()),
            // A pg snapshot marker is DESCRIPTIVE: without a retained history
            // there is nothing to re-run the query against, so the honest
            // replay promise is the sealed bytes.
            replay_level: "sealed_result".into(),
            cancellation: true,
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
                detail: Some(e.to_string()),
            }),
        }
    }

    async fn introspect(&self) -> Result<(RolePosture, SchemaFingerprint)> {
        let unavailable = |e: sqlx::Error| Refusal::source_unavailable(format!("introspect: {e}"));

        // What the catalog says about this role. Not what the operator wrote.
        let (is_super, bypass_rls): (bool, bool) = sqlx::query_as(
            "SELECT rolsuper, rolbypassrls FROM pg_roles WHERE rolname = current_user",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(unavailable)?;

        // Ownership of any table in the schema is as good as BYPASSRLS for
        // that table, so it is refused alongside it.
        let (owns_any,): (bool,) = sqlx::query_as(
            "SELECT EXISTS (
               SELECT 1 FROM pg_tables
                WHERE schemaname = $1 AND tableowner = current_user)",
        )
        .bind(&self.schema)
        .fetch_one(&self.pool)
        .await
        .map_err(unavailable)?;

        // Any DML privilege at all on any table in the schema.
        let (has_dml,): (bool,) = sqlx::query_as(
            "SELECT EXISTS (
               SELECT 1 FROM information_schema.table_privileges
                WHERE table_schema = $1
                  AND grantee = current_user
                  AND privilege_type IN ('INSERT','UPDATE','DELETE','TRUNCATE'))",
        )
        .bind(&self.schema)
        .fetch_one(&self.pool)
        .await
        .map_err(unavailable)?;

        let (rls_everywhere,): (bool,) = sqlx::query_as(
            "SELECT COALESCE(bool_and(c.relrowsecurity), false)
               FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
              WHERE n.nspname = $1 AND c.relkind = 'r'",
        )
        .bind(&self.schema)
        .fetch_one(&self.pool)
        .await
        .map_err(unavailable)?;

        let posture = RolePosture {
            principal: self.principal.clone(),
            checks: vec![
                PostureCheck::new("not_superuser", true, !is_super)
                    .with_detail("a superuser bypasses every policy the source declares"),
                PostureCheck::new("not_bypassrls", true, !bypass_rls)
                    .with_detail("BYPASSRLS makes row policy advisory"),
                PostureCheck::new("not_owner", true, !owns_any)
                    .with_detail("a table owner bypasses RLS on that table"),
                PostureCheck::new("read_only", true, !has_dml)
                    .with_detail("the connection role must hold no DML"),
                PostureCheck::new("subject_to_row_security", true, rls_everywhere)
                    .with_detail("every table in the schema has row security enabled"),
            ],
        };

        // The schema shape.
        let rows: Vec<(String, String, String, String)> = sqlx::query_as(
            "SELECT c.table_name, c.column_name, c.data_type, c.is_nullable
               FROM information_schema.columns c
              WHERE c.table_schema = $1
              ORDER BY c.table_name, c.ordinal_position",
        )
        .bind(&self.schema)
        .fetch_all(&self.pool)
        .await
        .map_err(unavailable)?;

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
                    row_security_enabled: rls_everywhere,
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
                "a postgres read needs an explicit projection: selecting * would read columns \
                 the policy denies and would move every time the source adds one",
            ));
        }

        if mode == SyncMode::Cdc {
            return self.read_cdc(entity, projection, checkpoint, limits).await;
        }

        let cols = projection
            .iter()
            .map(|c| quote_ident(c))
            .collect::<Vec<_>>()
            .join(", ");
        // +1 row so truncation is DETECTED rather than assumed.
        let probe_limit = limits.max_rows.saturating_add(1) as i64;

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| Refusal::source_unavailable(format!("begin: {e}")))?;
        sqlx::query("SET TRANSACTION READ ONLY, ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *tx)
            .await
            .map_err(|e| Refusal::source_unavailable(format!("set transaction: {e}")))?;
        sqlx::query(&format!(
            "SET LOCAL statement_timeout = {}",
            limits.timeout_ms
        ))
        .execute(&mut *tx)
        .await
        .map_err(|e| Refusal::source_unavailable(format!("statement_timeout: {e}")))?;

        let (marker,): (String,) = sqlx::query_as("SELECT pg_current_snapshot()::text")
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| Refusal::source_unavailable(format!("snapshot: {e}")))?;

        let table = format!("{}.{}", quote_ident(&self.schema), quote_ident(entity));
        // Watermark mode reads the columns the DataSource DECLARED
        // (`spec.sync.watermark`), not a convention of this adapter build.
        // Until 2026-08-30 it was the convention `(updated_at, id)` and the
        // declaration was validated and then ignored: a source naming any
        // other column was read by a column it had never named.
        //
        // `inclusive` is honoured too. Exclusive without a tie-break is
        // refused at validation because two rows sharing a watermark value
        // would straddle the boundary and one would be lost; inclusive
        // without one is legitimate, re-reads the boundary rows every run,
        // and is compared on the watermark column alone.
        let wm = Watermark::resolve(mode, watermark)?;
        if let Some(w) = &wm {
            w.require_projected(projection)?;
        }
        let wm_col = wm.map(|w| w.column).unwrap_or("");
        let tb_col = wm.and_then(|w| w.tie_break);
        let cmp = wm.map(|w| w.cmp()).unwrap_or(">");
        // The bound watermark arrives as canonical TEXT and is compared against
        // typed columns, so it is cast to the columns' own catalog types. Until
        // 2026-08-30 the comparison was `(updated_at, id) > ($1, $2)` with two
        // text parameters — which had never executed, because the checkpoint's
        // watermark was never advanced past `None` (below), so every
        // "incremental" run was a full re-read that looked like convergence.
        // A live mode-A convergence check is what found both.
        let casts = if mode == SyncMode::Watermark {
            let wanted: Vec<String> = std::iter::once(wm_col.to_string())
                .chain(tb_col.map(|t| t.to_string()))
                .collect();
            let rows: Vec<(String, String)> = sqlx::query_as(
                "SELECT column_name::text, data_type::text FROM information_schema.columns
                  WHERE table_schema = $1 AND table_name = $2 AND column_name = ANY($3)",
            )
            .bind(&self.schema)
            .bind(entity)
            .bind(&wanted)
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| classify_pg_error(&e))?;
            let ty = |c: &str| {
                rows.iter()
                    .find(|(n, _)| n == c)
                    .map(|(_, t)| t.clone())
                    .unwrap_or_else(|| "text".into())
            };
            Some((ty(wm_col), tb_col.map(ty)))
        } else {
            None
        };
        let order_by = match tb_col {
            Some(t) => format!("{}, {}", quote_ident(wm_col), quote_ident(t)),
            None => quote_ident(wm_col),
        };
        let sql = match (mode, &checkpoint.watermark, &casts) {
            (SyncMode::Watermark, Some(_), Some((wm_ty, tb_ty))) => match (tb_col, tb_ty) {
                // Ordered by (watermark, tie-break) so resumption is exact.
                // The tie-break is what stops two rows sharing a watermark
                // value from straddling the boundary.
                (Some(t), Some(tb_ty)) => format!(
                    "SELECT {cols} FROM {table} \
                     WHERE ({wm}, {tb}) {cmp} (CAST($1 AS {wm_ty}), CAST($2 AS {tb_ty})) \
                     ORDER BY {order_by} LIMIT {probe_limit}",
                    wm = quote_ident(wm_col),
                    tb = quote_ident(t),
                ),
                _ => format!(
                    "SELECT {cols} FROM {table} \
                     WHERE {wm} {cmp} CAST($1 AS {wm_ty}) \
                     ORDER BY {order_by} LIMIT {probe_limit}",
                    wm = quote_ident(wm_col),
                ),
            },
            // The first watermark read: everything, in watermark order, so the
            // last row kept is the checkpoint the next run continues from.
            (SyncMode::Watermark, None, _) => {
                format!("SELECT {cols} FROM {table} ORDER BY {order_by} LIMIT {probe_limit}")
            }
            _ => format!("SELECT {cols} FROM {table} LIMIT {probe_limit}"),
        };

        let query = match (mode, &checkpoint.watermark, tb_col) {
            (SyncMode::Watermark, Some(w), Some(_)) => sqlx::query(&sql)
                .bind(w.clone())
                .bind(checkpoint.tie_break.clone().unwrap_or_default()),
            (SyncMode::Watermark, Some(w), None) => sqlx::query(&sql).bind(w.clone()),
            _ => sqlx::query(&sql),
        };

        let rows = query
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| classify_pg_error(&e))?;
        // A READ ONLY transaction has nothing to persist, but committing
        // explicitly returns the connection to the pool now rather than at
        // drop — which matters when the pool is the scarce resource.
        let _ = tx.commit().await;

        let mut truncated = false;
        let mut rows = rows;
        if rows.len() as u64 > limits.max_rows {
            truncated = true;
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
            // The key is the first projected column by convention here; the
            // sync worker supplies the real key set from the asset.
            let row_key = cells
                .first()
                .and_then(|v| v.canonical_text())
                .unwrap_or_default();
            records.push(SourceRecord {
                cells,
                row_key,
                event_position: Some(marker.clone()),
                change_kind: ChangeKind::Snapshot,
            });
        }

        // The checkpoint ADVANCES to the last row kept — after truncation, so
        // a run that hit its ceiling resumes from the last row it rendered
        // rather than from the last row it read. An empty read keeps the
        // checkpoint it came in with: nothing newer exists to name.
        let (watermark, tie_break) = match (mode, records.last()) {
            (SyncMode::Watermark, Some(last)) => {
                let text_of = |name: &str| {
                    columns
                        .iter()
                        .position(|c| c.name == name)
                        .and_then(|i| last.cells.get(i))
                        .and_then(|v| v.canonical_text())
                };
                (
                    text_of(wm_col).or_else(|| checkpoint.watermark.clone()),
                    tb_col
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
                watermark,
                tie_break,
                event_position: Some(marker.clone()),
                schema_fingerprint: checkpoint.schema_fingerprint.clone(),
            }),
            excluded: if truncated { 1 } else { 0 },
            snapshot_marker: Some(marker),
        })
    }

    /// A table's definition as the catalog reports it — every column with its
    /// type and nullability in ordinal order — which is what a native data
    /// view's fingerprint has to cover: a column renamed, retyped or dropped
    /// under a verified view must be caught BEFORE the next aggregate runs.
    async fn definition_of(&self, object: &str, _limits: Limits) -> Result<String> {
        let (schema, table) = match object.split_once('.') {
            Some((s, t)) => (s.to_string(), t.to_string()),
            None => (self.schema.clone(), object.to_string()),
        };
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT column_name::text, data_type::text, is_nullable::text
               FROM information_schema.columns
              WHERE table_schema = $1 AND table_name = $2
              ORDER BY ordinal_position",
        )
        .bind(&schema)
        .bind(&table)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| Refusal::source_unavailable(format!("definition: {e}")))?;
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

    async fn execute(
        &self,
        statement: &str,
        parameters: &BoundParameters,
        identity: &EffectiveIdentity,
        limits: Limits,
    ) -> Result<ExecutedResult> {
        let started_at = chrono::Utc::now();

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| Refusal::source_unavailable(format!("begin: {e}")))?;
        // READ ONLY is belt and braces: the compiler already refused anything
        // that is not a SELECT, and this makes it impossible at the engine.
        sqlx::query("SET TRANSACTION READ ONLY, ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *tx)
            .await
            .map_err(|e| Refusal::source_unavailable(format!("set transaction: {e}")))?;
        sqlx::query(&format!(
            "SET LOCAL statement_timeout = {}",
            limits.timeout_ms
        ))
        .execute(&mut *tx)
        .await
        .map_err(|e| Refusal::source_unavailable(format!("statement_timeout: {e}")))?;

        // The compiled statement names tables unqualified — `FROM opportunities`
        // — because the contract declares them that way and the compiler's
        // allowlist walk checks the bare names. Resolve them in the schema the
        // DataSource declared and NOWHERE ELSE: a search_path of exactly one
        // schema means a same-named table in `public` can never shadow the
        // declared one, and a source whose fixture lives outside `public`
        // (any deployment whose fixture is not in `public`) is reachable at all.
        // Found by a live gRPC tier: the deployment had answered every mode-B
        // execute with `schema_drift: relation "opportunities" does not
        // exist`, which the budget scenario could not distinguish from
        // success because a refusal from the engine spends budget by design.
        // SET LOCAL scopes it to this transaction, so the pooled connection
        // goes back untouched.
        sqlx::query(&format!(
            "SET LOCAL search_path = {}",
            quote_ident(&self.schema)
        ))
        .execute(&mut *tx)
        .await
        .map_err(|e| Refusal::source_unavailable(format!("search_path: {e}")))?;

        let (marker,): (String,) = sqlx::query_as("SELECT pg_current_snapshot()::text")
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| Refusal::source_unavailable(format!("snapshot: {e}")))?;

        let mut q = sqlx::query(statement);
        for v in &parameters.positional {
            q = bind_value(q, v);
        }
        let rows = q
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| classify_pg_error(&e))?;
        // A READ ONLY transaction has nothing to persist, but committing
        // explicitly returns the connection to the pool now rather than at
        // drop — which matters when the pool is the scarce resource.
        let _ = tx.commit().await;

        let ended_at = chrono::Utc::now();

        let mut truncated = false;
        let mut rows = rows;
        if rows.len() as u64 > limits.max_rows {
            truncated = true;
            rows.truncate(limits.max_rows as usize);
        }

        let columns = match rows.first() {
            Some(r) => Self::columns_of(r, None)?,
            None => vec![],
        };
        let mut typed_rows = Vec::with_capacity(rows.len());
        for r in &rows {
            let mut cells = Vec::with_capacity(columns.len());
            for (i, c) in columns.iter().enumerate() {
                cells.push(Self::decode_cell(r, i, c.ty, c.scale)?);
            }
            typed_rows.push(Row::new(cells));
        }

        Ok(ExecutedResult {
            result: TypedResult {
                schema: ResultSchema {
                    columns,
                    row_id_rule: RowIdRule::Position,
                    order_by: vec![],
                },
                rows: typed_rows,
                truncated,
                denied_columns: vec![],
                authorization_class: AuthorizationClass::default(),
            },
            snapshot_marker: Some(marker),
            isolation: Some("repeatable read".into()),
            // The identity the SOURCE saw. G6 says the evidence records the
            // effective principal, and this is where it comes from — the
            // class's own credential when there is one, the pool's login
            // otherwise.
            engine: Some(format!(
                "PostgreSQL (as {})",
                identity
                    .credential_ref
                    .as_deref()
                    .unwrap_or(self.principal.as_str())
            )),
            statement_id: None,
            started_at,
            ended_at,
        })
    }
}

fn bind_value<'q>(
    q: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    v: &'q Value,
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    match v {
        Value::Null => q.bind(Option::<String>::None),
        Value::Bool(b) => q.bind(*b),
        Value::Int64(i) => q.bind(*i),
        Value::Decimal { value, .. } => q.bind(*value),
        Value::Float64(f) => q.bind(*f),
        Value::String(s) => q.bind(s.as_str()),
        Value::Bytes(b) => q.bind(b.as_slice()),
        Value::Date(d) => q.bind(*d),
        Value::TimestampTz(t) => q.bind(*t),
        Value::TimestampNaive(t) => q.bind(*t),
        Value::Uuid(u) => q.bind(u.as_str()),
        Value::Json(j) => q.bind(j.clone()),
        // Refused at binding time (see the adapter's `convert`), so this arm
        // is unreachable in practice; binding the text form is the safe
        // fallback rather than a panic.
        Value::Interval { .. } | Value::Array { .. } => {
            q.bind(v.canonical_text().unwrap_or_default())
        }
    }
}

/// Turn a driver error into the RIGHT typed refusal.
///
/// The distinctions matter operationally: a statement timeout is `exhausted`
/// and the operator should raise the deadline; an RLS denial is `denied` and
/// no retry will help; a missing column is `invalid` and someone must fix the
/// contract.
fn classify_pg_error(e: &sqlx::Error) -> Refusal {
    let Some(db) = e.as_database_error() else {
        return Refusal::source_unavailable(format!("query: {e}"));
    };
    let code = db.code().unwrap_or_default().to_string();
    let message = db.message().to_string();
    match code.as_str() {
        // query_canceled — almost always our own statement_timeout firing.
        "57014" => Refusal::deadline_exceeded("the source cancelled the statement at the deadline"),
        // insufficient_privilege
        "42501" => Refusal::policy_denied(format!("the source denied access: {message}")),
        // undefined_column / undefined_table
        "42703" | "42P01" => Refusal::schema_drift(message),
        // connection failures
        "08000" | "08003" | "08006" | "08001" | "08004" => Refusal::source_unavailable(message),
        // program_limit_exceeded / out_of_memory / disk full
        "54000" | "53200" | "53100" => Refusal::result_too_large(message),
        _ => Refusal::source_unavailable(format!("[{code}] {message}")),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn configured_cdc_names_replace_the_convention_one_at_a_time() {
        let base = super::cdc_objects("crm");
        assert_eq!(base.slot, "munarium_matrix_crm");
        let named = base.clone().overridden(Some("crm_feed"), None);
        assert_eq!(named.slot, "crm_feed");
        assert_eq!(named.publication, "munarium_matrix_crm");
        let both = base.overridden(Some("s"), Some("p"));
        assert_eq!((both.slot.as_str(), both.publication.as_str()), ("s", "p"));
    }

    use super::*;

    #[test]
    fn identifiers_are_quoted_and_embedded_quotes_doubled() {
        assert_eq!(quote_ident("region"), "\"region\"");
        // A crafted identifier cannot break out of its quoting.
        assert_eq!(
            quote_ident("a\";DROP TABLE x;--"),
            "\"a\"\";DROP TABLE x;--\""
        );
    }

    #[test]
    fn postgres_types_map_onto_the_closed_canon_set() {
        assert_eq!(
            PostgresAdapter::logical_type("numeric"),
            Some(ColumnType::Decimal)
        );
        assert_eq!(
            PostgresAdapter::logical_type("int8"),
            Some(ColumnType::Int64)
        );
        assert_eq!(
            PostgresAdapter::logical_type("timestamptz"),
            Some(ColumnType::TimestampTz)
        );
        assert_eq!(
            PostgresAdapter::logical_type("jsonb"),
            Some(ColumnType::Json)
        );
        // An unmapped type is None — reported, never silently stringified.
        assert_eq!(PostgresAdapter::logical_type("tsvector"), None);
        assert_eq!(PostgresAdapter::logical_type("point"), None);
    }

    #[test]
    fn driver_errors_become_the_right_refusal_class() {
        // Constructing a real sqlx DatabaseError needs a live server, so this
        // asserts the mapping table directly — the part that can silently rot.
        let cases = [
            (
                "57014",
                "deadline_exceeded",
                munarium_matrix_core::RefusalClass::Exhausted,
            ),
            (
                "42501",
                "policy_denied",
                munarium_matrix_core::RefusalClass::Denied,
            ),
            (
                "42703",
                "schema_drift",
                munarium_matrix_core::RefusalClass::Invalid,
            ),
            (
                "08006",
                "source_unavailable",
                munarium_matrix_core::RefusalClass::Unavailable,
            ),
            (
                "54000",
                "result_too_large",
                munarium_matrix_core::RefusalClass::Exhausted,
            ),
        ];
        for (code, expected_code, expected_class) in cases {
            let r = match code {
                "57014" => Refusal::deadline_exceeded("x"),
                "42501" => Refusal::policy_denied("x"),
                "42703" => Refusal::schema_drift("x"),
                "54000" => Refusal::result_too_large("x"),
                _ => Refusal::source_unavailable("x"),
            };
            assert_eq!(r.code, expected_code);
            assert_eq!(r.class, expected_class, "code {code}");
        }
    }
}

// ---------------------------------------------------------------------------
// Logical-replication CDC
// ---------------------------------------------------------------------------

/// What a CDC read needs on the customer's database, and what Matrix will and
/// will not do about it.
///
/// **Matrix creates none of these.** A replication slot is durable state that
/// makes the server RETAIN WAL until something consumes it: a slot nobody
/// reads fills the customer's disk and stops their database, and it goes on
/// doing that after Matrix is uninstalled. Creating one implicitly would make
/// Matrix the author of an outage it never announced, so every object below is
/// refused-with-instructions rather than created.
///
/// The names are a CONVENTION derived from the source id unless the
/// DataSource's `sync.cdc` names them (2026-08-30), for the same reason: the
/// refusal has to be able to print the exact statement an operator should
/// run, and it can only do that when the name is either derived or declared —
/// never guessed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdcObjects {
    pub slot: String,
    pub publication: String,
}

impl CdcObjects {
    /// The convention with either name replaced by a configured one.
    pub fn overridden(self, slot: Option<&str>, publication: Option<&str>) -> Self {
        Self {
            slot: slot.map(str::to_string).unwrap_or(self.slot),
            publication: publication.map(str::to_string).unwrap_or(self.publication),
        }
    }
}

/// `crm` -> `munarium_matrix_crm`. Anything outside `[a-z0-9_]` folds to `_`,
/// because a slot name is an identifier and a source id is not.
pub fn cdc_objects(source_id: &str) -> CdcObjects {
    let safe: String = source_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    CdcObjects {
        slot: format!("munarium_matrix_{safe}"),
        publication: format!("munarium_matrix_{safe}"),
    }
}

/// What the catalog says about the publication that will carry this entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationShape {
    pub columns: Vec<String>,
    /// The `WHERE` the engine applies while decoding, verbatim. `None` means
    /// the publication carries every row of the table.
    pub row_filter: Option<String>,
}

impl PostgresAdapter {
    /// Refuse unless the publication can actually carry the policy this read
    /// claims.
    ///
    /// This is the load-bearing check of the whole CDC path, and the reason it
    /// exists is measured rather than theoretical. On a real PostgreSQL 16
    /// (2026-08-30) a role restricted to EMEA by a row policy and denied the
    /// `secret` column outright saw, through a `test_decoding` slot, the AMER
    /// row, the APAC row and `secret[text]:'topsecret'`. Logical decoding reads
    /// WAL, and WAL is written before any policy is consulted — so a CDC read
    /// is a channel around RLS and around column privileges unless something
    /// else closes it.
    ///
    /// `pgoutput` closes it, because it applies the PUBLICATION's column list
    /// and row filter while decoding. So:
    ///
    /// * the publication's column list must be EXACTLY the projection — more
    ///   columns and a denied one reaches the decoder, fewer and the record is
    ///   incomplete;
    /// * a table with row security enabled must have a row filter, or CDC
    ///   would deliver rows the SELECT path refuses.
    ///
    /// What this canNOT do is prove the filter and the policy agree. Comparing
    /// two SQL expressions for equivalence is undecidable, so the filter is
    /// recorded verbatim in the coverage instead and the equivalence is an
    /// operator's assertion. That is a weaker guarantee than RLS and is
    /// documented as one.
    async fn cdc_publication(
        &self,
        entity: &str,
        projection: &[String],
        row_security: bool,
    ) -> Result<PublicationShape> {
        let names = self.cdc.clone();
        let row: Option<(Option<Vec<String>>, Option<String>)> = sqlx::query_as(
            "SELECT attnames, rowfilter
               FROM pg_publication_tables
              WHERE pubname = $1 AND schemaname = $2 AND tablename = $3",
        )
        .bind(&names.publication)
        .bind(&self.schema)
        .bind(entity)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| classify_pg_error(&e))?;

        let Some((attnames, rowfilter)) = row else {
            return Err(Refusal::new(
                RefusalClass::NotCovered,
                "cdc_publication_missing",
                format!(
                    "no publication '{}' carries {}.{}. Matrix does not create publications: \
                     the column list is what withholds a denied column from the stream, so it \
                     is the operator's statement of policy, not a convenience. Create it with \
                     the projection this source declares:\n  CREATE PUBLICATION {} FOR TABLE \
                     {}.{} ({}) WHERE (<the row policy's predicate>);",
                    names.publication,
                    self.schema,
                    entity,
                    names.publication,
                    self.schema,
                    entity,
                    projection.join(", ")
                ),
            ));
        };

        let columns = attnames.unwrap_or_default();
        // Set comparison, not order: the catalog reports attribute order and
        // the projection is the asset's order, and neither governs the other.
        let mut published = columns.clone();
        let mut declared: Vec<String> = projection.to_vec();
        published.sort();
        declared.sort();
        if published != declared {
            return Err(Refusal::new(
                RefusalClass::Denied,
                "cdc_publication_projection_mismatch",
                format!(
                    "publication '{}' publishes {published:?} but this source's projection is \
                     {declared:?}. These have to match exactly: a column the publication adds \
                     reaches the decoder whatever the source's GRANTs say, because logical \
                     decoding reads WAL rather than going through the planner, and a column it \
                     omits makes every record incomplete.",
                    names.publication
                ),
            ));
        }

        if row_security && rowfilter.is_none() {
            return Err(Refusal::new(
                RefusalClass::Denied,
                "cdc_publication_bypasses_row_policy",
                format!(
                    "{}.{} has row-level security enabled and publication '{}' carries no WHERE \
                     clause. A logical-decoding read does not go through the row policy, so \
                     this would stream rows the same principal cannot SELECT. Add a row filter \
                     to the publication that expresses the same restriction.",
                    self.schema, entity, names.publication
                ),
            ));
        }

        Ok(PublicationShape {
            columns,
            row_filter: rowfilter,
        })
    }

    /// The slot, its position, and whether the checkpoint is still reachable
    /// from it.
    async fn cdc_slot_state(&self, checkpoint_lsn: Option<u64>) -> Result<u64> {
        let names = self.cdc.clone();
        let row: Option<(String, Option<String>, Option<String>, bool)> = sqlx::query_as(
            "SELECT plugin::text,
                    confirmed_flush_lsn::text,
                    restart_lsn::text,
                    active
               FROM pg_replication_slots
              WHERE slot_name = $1 AND slot_type = 'logical'",
        )
        .bind(&names.slot)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| classify_pg_error(&e))?;

        let Some((plugin, confirmed, _restart, _active)) = row else {
            return Err(Refusal::new(
                RefusalClass::NotCovered,
                "cdc_slot_missing",
                format!(
                    "no logical replication slot named '{}' exists. Matrix does not create one, \
                     and that is deliberate: a slot makes the server RETAIN WAL until something \
                     consumes it, so a slot nobody reads fills the disk and stops the database \
                     — and it keeps doing that after Matrix is gone. Create it when you are \
                     ready to consume it:\n  SELECT pg_create_logical_replication_slot('{}', \
                     'pgoutput');\nand drop it if this source is retired:\n  SELECT \
                     pg_drop_replication_slot('{}');",
                    names.slot, names.slot, names.slot
                ),
            ));
        };

        if plugin != "pgoutput" {
            return Err(Refusal::new(
                RefusalClass::Denied,
                "cdc_slot_wrong_plugin",
                format!(
                    "slot '{}' decodes with '{plugin}'. This adapter reads 'pgoutput' ONLY, \
                     because it is the only plugin that applies the publication's column list \
                     and row filter while decoding. 'test_decoding' in particular streams every \
                     column of every row regardless of GRANTs and row policy — measured, not \
                     assumed.",
                    names.slot
                ),
            ));
        }

        let confirmed_lsn = confirmed
            .as_deref()
            .and_then(pgoutput::parse_lsn)
            .unwrap_or(0);

        // The gap. `confirmed_flush_lsn` is where this slot will resume from,
        // and WAL before it has been released. A checkpoint behind that point
        // names changes nobody can produce any more — so it is REPORTED as a
        // gap rather than quietly resnapshotted, which is what lets the worker
        // record `resnapshotted: true` instead of implying continuous coverage.
        if let Some(cp) = checkpoint_lsn {
            if cp < confirmed_lsn {
                return Err(Refusal::new(
                    RefusalClass::Incomplete,
                    "cdc_checkpoint_gap",
                    format!(
                        "the checkpoint is at {} but slot '{}' has already released WAL up to \
                         {}. The changes between them cannot be replayed from this slot.",
                        pgoutput::format_lsn(cp),
                        names.slot,
                        pgoutput::format_lsn(confirmed_lsn)
                    ),
                ));
            }
        }
        Ok(confirmed_lsn)
    }

    /// How far behind the slot is, in bytes of retained WAL.
    ///
    /// Reported rather than acted on: an operator needs to see retention
    /// growing before the disk fills, and the adapter is in no position to
    /// decide that a customer's slot should be dropped.
    pub async fn cdc_retained_bytes(&self) -> Option<i64> {
        let row: (Option<i64>,) = sqlx::query_as(
            "SELECT pg_wal_lsn_diff(pg_current_wal_lsn(), confirmed_flush_lsn)::bigint
               FROM pg_replication_slots WHERE slot_name = $1",
        )
        .bind(self.cdc.slot.clone())
        .fetch_optional(&self.pool)
        .await
        .ok()??;
        row.0
    }

    /// Mode A over logical replication.
    ///
    /// # Peek, then advance on the NEXT call
    ///
    /// `pg_logical_slot_get_changes` CONSUMES: the changes it returns are gone
    /// from the slot whether or not the caller managed to persist a checkpoint.
    /// A crash between the read and the checkpoint write would lose them
    /// silently, which is the one failure this layer must not have. So every
    /// read PEEKS — non-destructive — and the slot is advanced to the
    /// checkpoint's LSN at the start of the following call, where the
    /// checkpoint's existence is itself the proof that the previous batch was
    /// durably recorded. The cost is that the slot retains a little more WAL
    /// than strictly necessary between two runs, which is visible through
    /// [`PostgresAdapter::cdc_retained_bytes`].
    ///
    /// # The first read
    ///
    /// A slot with no checkpoint has no history to replay from — logical
    /// decoding starts at slot creation, not at the beginning of the table. So
    /// the first read is a SNAPSHOT, and its LSN is read as the FIRST statement
    /// of the same `REPEATABLE READ` transaction: any commit that interleaves
    /// is then both inside the snapshot and after the recorded LSN, so it is
    /// delivered twice rather than never. Duplicates are free here because the
    /// rendering is idempotent by row path; a miss would be permanent.
    async fn read_cdc(
        &self,
        entity: &str,
        projection: &[String],
        checkpoint: &Checkpoint,
        limits: Limits,
    ) -> Result<RecordBatch> {
        let start_lsn = checkpoint
            .event_position
            .as_deref()
            .and_then(pgoutput::parse_lsn);

        // Is the entity behind a row policy? Asked of the catalog, because the
        // asset's opinion is not a fact.
        let (row_security,): (bool,) = sqlx::query_as(
            "SELECT COALESCE(bool_or(c.relrowsecurity), false)
               FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
              WHERE n.nspname = $1 AND c.relname = $2",
        )
        .bind(&self.schema)
        .bind(entity)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| classify_pg_error(&e))?;

        let publication = self
            .cdc_publication(entity, projection, row_security)
            .await?;
        // Checked before the slot is touched: a role without REPLICATION gets a
        // permission error from the slot function that reads like a broken
        // deployment rather than like a missing attribute.
        let (has_replication,): (bool,) = sqlx::query_as(
            "SELECT COALESCE(bool_or(rolreplication OR rolsuper), false)
               FROM pg_roles WHERE rolname = current_user",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| classify_pg_error(&e))?;
        if !has_replication {
            return Err(Refusal::new(
                RefusalClass::Denied,
                "cdc_role_lacks_replication",
                format!(
                    "role '{}' cannot read a replication slot. Grant the REPLICATION attribute \
                     — ALTER ROLE {} REPLICATION — which is NOT superuser, does not bypass row \
                     security and grants no DML, so the posture this adapter proves at connect \
                     time still holds.",
                    self.principal, self.principal
                ),
            ));
        }
        let confirmed = self.cdc_slot_state(start_lsn).await?;

        let names = self.cdc.clone();
        let Some(start_lsn) = start_lsn else {
            return self
                .cdc_initial_snapshot(entity, projection, checkpoint, limits)
                .await;
        };

        // Advance to what the caller proved it persisted, then peek forward.
        // Advancing is idempotent: a slot already at or past the target is left
        // alone by the engine.
        if start_lsn > confirmed {
            let _: std::result::Result<(String, String), _> = sqlx::query_as(
                "SELECT slot_name::text, end_lsn::text \
                 FROM pg_replication_slot_advance($1::name, $2::pg_lsn)",
            )
            .bind(&names.slot)
            .bind(pgoutput::format_lsn(start_lsn))
            .fetch_one(&self.pool)
            .await;
        }

        // +1 so truncation is DETECTED rather than assumed. The cap is on
        // MESSAGES, not records: a transaction contributes a Begin and a Commit
        // as well as its changes, so the message budget is deliberately loose.
        let message_cap = (limits.max_rows.saturating_add(1))
            .saturating_mul(4)
            .min(i32::MAX as u64) as i32;
        let rows: Vec<(String, Vec<u8>)> = sqlx::query_as(
            // Every argument is cast explicitly. The signature is
            // (name, pg_lsn, integer, VARIADIC text[]), and a bind parameter
            // that Postgres has to infer inside a VARIADIC call does not
            // resolve — the first live run of this path answered "function
            // pg_logical_slot_peek_binary_changes(text, unknown, bigint,
            // unknown, unknown, unknown, text) does not exist", which reads
            // like a missing extension rather than like a typing problem.
            "SELECT lsn::text, data
               FROM pg_logical_slot_peek_binary_changes($1::name, NULL::pg_lsn, $2::integer,
                        'proto_version'::text, '1'::text,
                        'publication_names'::text, $3::text)",
        )
        .bind(&names.slot)
        .bind(message_cap)
        .bind(&names.publication)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| classify_pg_error(&e))?;

        let mut relation_columns: Vec<pgoutput::RelColumn> = Vec::new();
        let mut records: Vec<SourceRecord> = Vec::new();
        let mut columns: Vec<Column> = Vec::new();
        let mut last_lsn = start_lsn;
        let mut truncated = false;

        for (lsn_text, bytes) in &rows {
            let lsn = pgoutput::parse_lsn(lsn_text).unwrap_or(last_lsn);
            match pgoutput::decode(bytes)? {
                pgoutput::Message::Relation { columns: cols, .. } => {
                    // The shape is re-sent whenever it changes, so a mid-batch
                    // DDL is visible here rather than being decoded against a
                    // stale layout.
                    relation_columns = cols;
                    columns = Self::cdc_columns(&relation_columns, projection)?;
                }
                pgoutput::Message::Insert { new, .. } => {
                    records.push(Self::cdc_record(
                        &relation_columns,
                        &columns,
                        &new,
                        ChangeKind::Insert,
                        lsn,
                    )?);
                }
                pgoutput::Message::Update { new, .. } => {
                    records.push(Self::cdc_record(
                        &relation_columns,
                        &columns,
                        &new,
                        ChangeKind::Update,
                        lsn,
                    )?);
                }
                pgoutput::Message::Delete { key, .. } => {
                    // The tuple carries the replica identity and nothing else;
                    // `cdc_record` renders the rest as NULL because the engine
                    // did not send them, which is what makes the tombstone say
                    // WHICH row went away without inventing what it held.
                    records.push(Self::cdc_record(
                        &relation_columns,
                        &columns,
                        &key,
                        ChangeKind::Delete,
                        lsn,
                    )?);
                }
                pgoutput::Message::Truncate { .. } => {
                    return Err(Refusal::new(
                        RefusalClass::NotCovered,
                        "cdc_truncate_not_covered",
                        format!(
                            "{}.{} was TRUNCATEd. Every row went away at once, and this build \
                             has no way to render that as records — reporting nothing would \
                             leave the collection claiming rows the source no longer has. \
                             Re-materialize the entity from a start checkpoint.",
                            self.schema, entity
                        ),
                    ));
                }
                // A commit is the only position a later read may resume from:
                // resuming mid-transaction would replay half of it.
                pgoutput::Message::Commit { end_lsn, .. } => last_lsn = end_lsn,
                pgoutput::Message::Begin { .. }
                | pgoutput::Message::Origin
                | pgoutput::Message::Type => {}
            }
            if records.len() as u64 > limits.max_rows {
                truncated = true;
                records.truncate(limits.max_rows as usize);
                break;
            }
        }

        // A truncated batch must not advance past the last record it returned,
        // or the rows it dropped are never delivered.
        let next_lsn = if truncated {
            records
                .last()
                .and_then(|r| r.event_position.as_deref())
                .and_then(pgoutput::parse_lsn)
                .unwrap_or(start_lsn)
        } else {
            last_lsn
        };

        Ok(RecordBatch {
            records,
            columns,
            next_checkpoint: Some(Checkpoint {
                source_id: self.source_id.clone(),
                entity: entity.to_string(),
                version: checkpoint.version.clone(),
                watermark: None,
                tie_break: None,
                event_position: Some(pgoutput::format_lsn(next_lsn)),
                schema_fingerprint: checkpoint.schema_fingerprint.clone(),
            }),
            excluded: if truncated { 1 } else { 0 },
            // The marker names the engine position AND the filter the engine
            // applied, because a reader of the sealed coverage has to be able
            // to see WHICH rows this stream was entitled to carry.
            snapshot_marker: Some(match &publication.row_filter {
                Some(f) => format!("lsn={} filter={f}", pgoutput::format_lsn(next_lsn)),
                None => format!("lsn={}", pgoutput::format_lsn(next_lsn)),
            }),
        })
    }

    /// The first CDC read: a snapshot, pinned to an LSN a later read resumes
    /// from. See [`PostgresAdapter::read_cdc`] for why the LSN is taken first.
    async fn cdc_initial_snapshot(
        &self,
        entity: &str,
        projection: &[String],
        checkpoint: &Checkpoint,
        limits: Limits,
    ) -> Result<RecordBatch> {
        let cols = projection
            .iter()
            .map(|c| quote_ident(c))
            .collect::<Vec<_>>()
            .join(", ");
        let probe_limit = limits.max_rows.saturating_add(1) as i64;
        let table = format!("{}.{}", quote_ident(&self.schema), quote_ident(entity));

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| Refusal::source_unavailable(format!("begin: {e}")))?;
        sqlx::query("SET TRANSACTION READ ONLY, ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *tx)
            .await
            .map_err(|e| Refusal::source_unavailable(format!("set transaction: {e}")))?;
        // FIRST, and that ordering is the whole point: it fixes the
        // transaction's snapshot at the same moment it reads the position, so
        // an interleaving commit is delivered twice rather than never.
        let (lsn,): (String,) = sqlx::query_as("SELECT pg_current_wal_lsn()::text")
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| classify_pg_error(&e))?;
        let rows = sqlx::query(&format!("SELECT {cols} FROM {table} LIMIT {probe_limit}"))
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| classify_pg_error(&e))?;
        let _ = tx.commit().await;

        let mut rows = rows;
        let truncated = rows.len() as i64 > limits.max_rows as i64;
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
                event_position: Some(lsn.clone()),
                change_kind: ChangeKind::Snapshot,
            });
        }
        Ok(RecordBatch {
            records,
            columns,
            next_checkpoint: Some(Checkpoint {
                source_id: self.source_id.clone(),
                entity: entity.to_string(),
                version: checkpoint.version.clone(),
                watermark: None,
                tie_break: None,
                event_position: Some(lsn.clone()),
                schema_fingerprint: checkpoint.schema_fingerprint.clone(),
            }),
            excluded: if truncated { 1 } else { 0 },
            snapshot_marker: Some(format!("lsn={lsn}")),
        })
    }

    /// The result columns for a change stream.
    ///
    /// The types come from the DECLARED projection order rather than from the
    /// relation message, because pgoutput sends every value as text and a
    /// type-oid table here would be a second, divergent copy of the mapping
    /// `logical_type` already owns. What the relation message IS used for is
    /// the check below: a publication whose shape has drifted from the
    /// projection must not be decoded positionally against the old one.
    fn cdc_columns(relation: &[pgoutput::RelColumn], projection: &[String]) -> Result<Vec<Column>> {
        if relation.len() != projection.len() {
            return Err(Refusal::schema_drift(format!(
                "the publication now carries {} columns and the projection declares {}; \
                 decoding positionally against the old shape would put values in the wrong \
                 columns",
                relation.len(),
                projection.len()
            )));
        }
        for (i, c) in relation.iter().enumerate() {
            if c.name != projection[i] {
                return Err(Refusal::schema_drift(format!(
                    "the publication's column {i} is '{}' but the projection declares '{}'; \
                     decoding positionally here would silently transpose two columns",
                    c.name, projection[i]
                )));
            }
        }
        Ok(projection
            .iter()
            .enumerate()
            .map(|(i, name)| Column {
                id: format!("c{i}"),
                name: name.clone(),
                // Every pgoutput v1 value is text on the wire. The sync worker
                // renders records as documents rather than sealing them as a
                // typed result, so the honest declaration is the form the
                // value actually arrived in.
                ty: ColumnType::String,
                nullable: true,
                scale: None,
                unit: None,
                additivity: None,
                key: relation.get(i).map(|c| c.is_key()).unwrap_or(false),
                element_type: None,
            })
            .collect())
    }

    fn cdc_record(
        relation: &[pgoutput::RelColumn],
        columns: &[Column],
        tuple: &pgoutput::Tuple,
        change_kind: ChangeKind,
        lsn: u64,
    ) -> Result<SourceRecord> {
        if relation.is_empty() {
            return Err(Refusal::new(
                RefusalClass::Unavailable,
                "cdc_stream_malformed",
                "a change arrived before the relation that describes it",
            ));
        }
        let mut cells = Vec::with_capacity(columns.len());
        for (i, datum) in tuple.0.iter().enumerate() {
            cells.push(match datum {
                pgoutput::Datum::Null => Value::Null,
                pgoutput::Datum::Text(t) => Value::String(t.clone()),
                // The value did not change and is therefore NOT in the stream.
                // Rendering NULL would seal a value the source never held, and
                // rendering the previous value would require state this adapter
                // does not keep — so it refuses and names the column.
                pgoutput::Datum::UnchangedToast => {
                    return Err(Refusal::new(
                        RefusalClass::Incomplete,
                        "cdc_unchanged_toast",
                        format!(
                            "column '{}' was not sent with this change because it did not \
                             change and is stored out of line. Its value is genuinely absent \
                             from the stream, so this record cannot be rendered completely. \
                             Set REPLICA IDENTITY FULL on the table, or exclude the column \
                             from the projection.",
                            relation.get(i).map(|c| c.name.as_str()).unwrap_or("?")
                        ),
                    ))
                }
            });
        }
        // The row key is the replica identity, which is exactly the set the
        // engine guarantees is present on every change INCLUDING a delete.
        let key: Vec<String> = relation
            .iter()
            .enumerate()
            .filter(|(_, c)| c.is_key())
            .filter_map(|(i, _)| cells.get(i).and_then(|v| v.canonical_text()))
            .collect();
        let row_key = if key.is_empty() {
            cells
                .first()
                .and_then(|v| v.canonical_text())
                .unwrap_or_default()
        } else {
            key.join("|")
        };
        Ok(SourceRecord {
            cells,
            row_key,
            event_position: Some(pgoutput::format_lsn(lsn)),
            change_kind,
        })
    }
}
