// SPDX-License-Identifier: Apache-2.0
//! The SQL Server adapter.
//!
//! The third SQL engine behind the same seam. Postgres and MySQL between them
//! settled quoting and placeholders; what this one had to settle is harder,
//! and worth reading before changing anything here:
//!
//! 1. **T-SQL has no read-only transaction.** Postgres has
//!    `SET TRANSACTION READ ONLY`; MySQL has `START TRANSACTION READ ONLY`;
//!    SQL Server has neither. Read-only on this engine is a property of the
//!    PRINCIPAL (its permissions) and of the TOPOLOGY (`ApplicationIntent=
//!    ReadOnly` routes to a readable secondary, where a write is impossible
//!    because the replica is not writable), never of the transaction. So the
//!    posture is asked of the engine in [`SqlServerAdapter::introspect`] —
//!    server role, database roles, and DML permission on the schema — and the
//!    connection declares read-only intent. Writing "the transaction is read
//!    only" here would be a comfortable sentence about a flag that does not
//!    exist.
//!
//! 2. **Transaction characteristics come BEFORE the transaction, on the same
//!    session** — the same rule MySQL taught, for a different reason. SQL
//!    Server accepts most isolation changes inside a transaction but refuses
//!    SNAPSHOT: a statement run under snapshot isolation in a transaction that
//!    did not START in snapshot isolation fails with 3951. And this adapter
//!    opens a FRESH session per operation rather than pooling, because
//!    `SET LOCK_TIMEOUT`, `SET ROWCOUNT` and the isolation level are all
//!    session state: a pooled session handed back with any of them still set
//!    is the next caller's silent problem, and the MySQL adapter had to pin a
//!    connection out of its pool to avoid exactly that. One session per
//!    operation costs a TDS handshake and buys the property outright.
//!
//! 3. **Transaction control cannot go through the parameterised path.**
//!    tiberius's `query()` is an `sp_executesql` RPC, and changing `@@TRANCOUNT`
//!    inside a procedure is error 266 on return. `simple_query()` sends a plain
//!    batch. This is the same shape as MySQL's 1295 ("not supported in the
//!    prepared statement protocol") arriving for a different reason, which is
//!    why it is worth naming twice.
//!
//! 4. **A snapshot marker only when it is not a race.** Change tracking gives
//!    the database a monotonic version, and `CHANGE_TRACKING_CURRENT_VERSION()`
//!    read INSIDE a snapshot-isolated transaction names the same consistent
//!    view the rows came from — which is the arrangement Microsoft's own change
//!    tracking guidance prescribes. Read outside one it would be a number that
//!    raced the read, so [`snapshot_marker_for`] returns `None` unless both
//!    conditions hold, and both are observed rather than assumed.
//!
//! 5. **MONEY is refused.** It is an exact 4-decimal currency type on the
//!    server and an IEEE-754 double in this driver (tiberius decodes it as
//!    `read_i32_le() as f64 / 1e4`). A currency silently becoming a float is
//!    the precise failure canon@1 exists to prevent, so `money` and
//!    `smallmoney` are unmodelled types and a read of one is refused by name.
//!    A deployment that needs the column casts it in the contract's statement.
//!
//! 6. **The result shape is asked of the ENGINE before the statement runs.**
//!    This is not tidiness. `sys.dm_exec_describe_first_result_set` is SQL
//!    Server's own answer to "what will this return": it names every output
//!    column and its type as TEXT, costs a plan compilation and no execution,
//!    and lets the adapter refuse `geography` and `money` BY NAME before the
//!    driver ever decodes them. A statement the engine cannot describe is
//!    refused rather than attempted.
//!
//!    It was BUILT because the driver of the day, `tiberius` 0.12.3,
//!    `todo!()`d — panicked, not errored — parsing the column metadata token
//!    for a `Udt`, which is how every spatial column and `hierarchyid`
//!    arrives: a `SELECT` naming a `geography` column brought the process
//!    down before a single row existed, so a refusal built on the driver's
//!    own metadata was unreachable for exactly the types that most needed
//!    refusing. `tiberius-ng` 0.13, which this adapter moved to on
//!    2026-08-30, handles that token, so the panic is gone.
//!
//!    The pre-flight STAYS, and not out of inertia. Asking the engine is the
//!    fail-closed design independent of any driver bug: it refuses an
//!    unmodelled type by NAME rather than after a decode, it is the only
//!    thing that would catch the next such token, and a refusal that depends
//!    on the driver being correct is a refusal that moves when the driver
//!    does.
//!
//! Mode A is snapshot and watermark. Change tracking could serve a change
//! feed, and reading a version is not reading one, so `sync_modes` says what
//! is built rather than what the engine could support.

// A `Refusal` is a typed answer, not an exception; every adapter returns one
// by value.
#![allow(clippy::result_large_err)]
#![forbid(unsafe_code)]

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
use std::time::Duration;
use tiberius::{Client, Config, Row as TdsRow, ToSql};
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

pub const ADAPTER_VERSION: &str = "sqlserver@0.1.0";

/// One TDS session. Not a pool: see the module note on session state.
type Session = Client<Compat<tokio::net::TcpStream>>;

pub struct SqlServerAdapter {
    source_id: String,
    /// Holds the resolved credential inside `AuthMethod`. NEVER `{:?}` this —
    /// tiberius's `Debug` for `Config` prints the password.
    config: Config,
    /// The schema this source's tables live in. SQL Server's three-part name
    /// is database.schema.table and the database is in the connection string,
    /// so this is the middle part — `dbo` unless the asset says otherwise.
    schema: String,
    principal: String,
}

/// SQL Server quotes with brackets and escapes an embedded `]` by doubling.
///
/// `"ident"` also works when QUOTED_IDENTIFIER is ON (which TDS clients get by
/// default), and the semantic compiler's native backend emits exactly that.
/// Brackets are used here anyway: they are unambiguous regardless of session
/// settings, and a read that depends on a session flag is a read that breaks
/// when someone changes a default.
pub fn quote_ident(ident: &str) -> String {
    format!("[{}]", ident.replace(']', "]]"))
}

/// The isolation a read actually ran under. Observed, not requested: a
/// database that does not allow snapshot isolation gets read committed, and
/// the difference decides whether a snapshot marker means anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Isolation {
    Snapshot,
    ReadCommitted,
}

impl Isolation {
    fn set_statement(self) -> &'static str {
        match self {
            Isolation::Snapshot => "SET TRANSACTION ISOLATION LEVEL SNAPSHOT",
            Isolation::ReadCommitted => "SET TRANSACTION ISOLATION LEVEL READ COMMITTED",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Isolation::Snapshot => "snapshot",
            Isolation::ReadCommitted => "read committed",
        }
    }
}

/// The marker a read may report, and `None` whenever it would be a guess.
///
/// Kept separate from the I/O so both branches are testable without a server:
/// the presence half is proven live by the conformance tier, the absence half
/// by the unit tests below. An adapter that reported the version regardless of
/// isolation would hand a manifest a position taken at a different instant
/// from the rows — the shape of claim this whole layer exists to refuse.
pub fn snapshot_marker_for(
    change_tracking_version: Option<i64>,
    isolation: Isolation,
) -> Option<String> {
    match (change_tracking_version, isolation) {
        (Some(v), Isolation::Snapshot) => Some(format!("ct:{v}")),
        _ => None,
    }
}

impl SqlServerAdapter {
    /// Connect with a resolved credential.
    ///
    /// The credential is an ADO.NET (or JDBC) connection string, which is SQL
    /// Server's own vocabulary and what tiberius parses. There is deliberately
    /// no `sqlserver://` URL form: inventing a third dialect of connection
    /// string would be a third thing to get subtly wrong about ports, instance
    /// names and certificate trust.
    ///
    /// `max_conns` is accepted and ignored so the call site matches the other
    /// SQL adapters; this one holds no pool.
    pub async fn connect(
        source_id: &str,
        ado: &str,
        schema: &str,
        _max_conns: u32,
    ) -> Result<Self> {
        let mut config = if ado.trim_start().to_ascii_lowercase().starts_with("jdbc:") {
            Config::from_jdbc_string(ado)
        } else {
            Config::from_ado_string(ado)
        }
        .map_err(|e| {
            // The parser's message can echo the string it failed on, and that
            // string holds the password.
            Refusal::invalid(
                "not_covered",
                format!(
                    "the connection string for source '{source_id}' did not parse as an \
                     ADO.NET or JDBC connection string ({})",
                    e_kind(&e)
                ),
            )
        })?;
        // Declares read-only intent on the wire. On a standalone server this is
        // accepted and does nothing; against an availability-group listener it
        // routes to a readable secondary, where a write is refused by the
        // engine rather than by our good intentions. Setting it costs nothing
        // and is the only engine-enforced read-only posture this dialect has.
        config.readonly(true);

        let principal = principal_of(ado).unwrap_or_else(|| "sqlserver".to_string());
        let adapter = Self {
            source_id: source_id.to_string(),
            config,
            schema: schema.to_string(),
            principal,
        };
        // Fail at connect rather than at the first read: a source that cannot
        // be reached should say so while an operator is still looking.
        let _ = adapter.session().await?;
        Ok(adapter)
    }

    /// A fresh session. See the module note: session state is why.
    async fn session(&self) -> Result<Session> {
        let tcp = tokio::net::TcpStream::connect(self.config.get_addr())
            .await
            .map_err(|e| Refusal::source_unavailable(format!("connect: {e}")))?;
        // Nagle would add up to 40ms to every small round trip, and this
        // adapter makes several per operation.
        let _ = tcp.set_nodelay(true);
        Client::connect(self.config.clone(), tcp.compat_write())
            .await
            .map_err(|e| classify_sqlserver_error(&e))
    }

    /// The closed canon@1 set, from SQL Server's own type names.
    ///
    /// `None` is a refusal, not a fallback. Four families are deliberately
    /// absent: `money`/`smallmoney` (exact on the server, a double in this
    /// driver — see the module note), `time` (canon@1 has no time-of-day
    /// type), the spatial and `hierarchyid` UDTs, and `xml`/`sql_variant`/
    /// `rowversion`, none of which has one logical shape.
    pub fn logical_type(name: &str) -> Option<ColumnType> {
        Some(match name.to_ascii_lowercase().as_str() {
            "bit" => ColumnType::Bool,
            "tinyint" | "smallint" | "int" | "bigint" => ColumnType::Int64,
            "decimal" | "numeric" => ColumnType::Decimal,
            "float" | "real" => ColumnType::Float64,
            // `text` and `ntext` are deprecated and still readable; refusing
            // them would refuse legacy tables for a reason that helps nobody.
            "char" | "varchar" | "text" | "nchar" | "nvarchar" | "ntext" | "sysname" => {
                ColumnType::String
            }
            "binary" | "varbinary" | "image" => ColumnType::Bytes,
            "date" => ColumnType::Date,
            // `datetimeoffset` carries a zone; `datetime`, `datetime2` and
            // `smalldatetime` carry none. Different canon@1 types, exactly as
            // MySQL's TIMESTAMP and DATETIME are.
            "datetimeoffset" => ColumnType::TimestampTz,
            "datetime" | "datetime2" | "smalldatetime" => ColumnType::TimestampNaive,
            "uniqueidentifier" => ColumnType::Uuid,
            _ => return None,
        })
    }

    /// Build the result columns from the shape the ENGINE described.
    ///
    /// The order matters and is the whole point: the ENGINE's description is
    /// what an unmodelled type is refused from, so a column this adapter
    /// cannot model is named before the driver decodes anything. See the
    /// module note — this outlived the driver panic that first forced it.
    fn columns_of(meta: &[DescribedColumn], declared: Option<&[Column]>) -> Result<Vec<Column>> {
        meta.iter()
            .enumerate()
            .map(|(i, described)| {
                let name = &described.name;
                let ty = Self::logical_type(&described.source_type).ok_or_else(|| {
                    Refusal::schema_drift(format!(
                        "column '{name}' has SQL Server type {} which canon@1 does not model; \
                         cast it in the contract's statement or exclude it",
                        described.source_type
                    ))
                })?;
                let declared_col = declared.and_then(|d| d.iter().find(|x| &x.name == name));
                Ok(Column {
                    id: format!("c{i}"),
                    name: name.clone(),
                    ty: declared_col.map(|d| d.ty).unwrap_or(ty),
                    nullable: declared_col
                        .map(|d| d.nullable)
                        .unwrap_or(described.nullable),
                    scale: declared_col.and_then(|d| d.scale),
                    unit: declared_col.and_then(|d| d.unit.clone()),
                    additivity: declared_col.and_then(|d| d.additivity),
                    key: declared_col.map(|d| d.key).unwrap_or(false),
                    element_type: None,
                })
            })
            .collect()
    }

    fn decode_cell(row: &TdsRow, idx: usize, ty: ColumnType, scale: Option<u32>) -> Result<Value> {
        let bad = |e: tiberius::error::Error| {
            Refusal::schema_drift(format!(
                "column {idx} did not decode as {ty}: {}",
                e_kind(&e)
            ))
        };
        Ok(match ty {
            ColumnType::Bool => match row.try_get::<bool, _>(idx).map_err(bad)? {
                Some(v) => Value::Bool(v),
                None => Value::Null,
            },
            // TDS reports an integer at its declared width, and tiberius will
            // only convert a column to the exact Rust type that matches it. A
            // single `try_get::<i64>` would fail on every `int` column in every
            // customer schema.
            ColumnType::Int64 => {
                if let Ok(v) = row.try_get::<i64, _>(idx) {
                    v.map(Value::Int64).unwrap_or(Value::Null)
                } else if let Ok(v) = row.try_get::<i32, _>(idx) {
                    v.map(|v| Value::Int64(v as i64)).unwrap_or(Value::Null)
                } else if let Ok(v) = row.try_get::<i16, _>(idx) {
                    v.map(|v| Value::Int64(v as i64)).unwrap_or(Value::Null)
                } else {
                    match row.try_get::<u8, _>(idx).map_err(bad)? {
                        Some(v) => Value::Int64(v as i64),
                        None => Value::Null,
                    }
                }
            }
            ColumnType::Decimal => match row.try_get::<Decimal, _>(idx).map_err(bad)? {
                Some(v) => Value::Decimal {
                    value: v,
                    // The column's declared scale wins when the contract states
                    // one; otherwise the value's own, which TDS carries
                    // faithfully (`Decimal::from_i128_with_scale`). This is why
                    // `900000.50` does not arrive as `900000.5`.
                    scale: scale.unwrap_or_else(|| v.scale()),
                },
                None => Value::Null,
            },
            ColumnType::Float64 => {
                if let Ok(v) = row.try_get::<f64, _>(idx) {
                    v.map(Value::Float64).unwrap_or(Value::Null)
                } else {
                    match row.try_get::<f32, _>(idx).map_err(bad)? {
                        Some(v) => Value::Float64(v as f64),
                        None => Value::Null,
                    }
                }
            }
            ColumnType::String => match row.try_get::<&str, _>(idx).map_err(bad)? {
                Some(v) => Value::String(v.to_string()),
                None => Value::Null,
            },
            ColumnType::Bytes => match row.try_get::<&[u8], _>(idx).map_err(bad)? {
                Some(v) => Value::Bytes(v.to_vec()),
                None => Value::Null,
            },
            ColumnType::Date => match row.try_get::<chrono::NaiveDate, _>(idx).map_err(bad)? {
                Some(v) => Value::Date(v),
                None => Value::Null,
            },
            ColumnType::TimestampTz => match row
                .try_get::<chrono::DateTime<chrono::Utc>, _>(idx)
                .map_err(bad)?
            {
                Some(v) => Value::TimestampTz(v),
                None => Value::Null,
            },
            ColumnType::TimestampNaive => {
                match row.try_get::<chrono::NaiveDateTime, _>(idx).map_err(bad)? {
                    Some(v) => Value::TimestampNaive(v),
                    None => Value::Null,
                }
            }
            ColumnType::Uuid => match row.try_get::<tiberius::Uuid, _>(idx).map_err(bad)? {
                Some(v) => Value::Uuid(v.to_string()),
                None => Value::Null,
            },
            other => {
                return Err(Refusal::schema_drift(format!(
                    "canon@1 type {other} has no SQL Server decoding in this adapter"
                )))
            }
        })
    }

    /// Ask the engine what a statement will return, before running it.
    ///
    /// This exists because tiberius PANICS on a `Udt` column's metadata token,
    /// so the driver can never be the thing that refuses a `geography` column
    /// (module note 6). It buys three things beyond survival: the refusal names
    /// the column and the type an operator would recognise from their own DDL;
    /// nothing is executed when the shape is wrong, so a bad contract costs a
    /// plan compilation rather than a scan; and the check is the ENGINE's
    /// opinion of the statement rather than a parse of it here.
    ///
    /// A statement the engine cannot describe is refused. That is deliberately
    /// fail-closed: the alternative is to run it and hope, and "hope" here
    /// means a process-level panic.
    async fn describe_result_shape(
        session: &mut Session,
        statement: &str,
        params: &[Value],
    ) -> Result<Vec<DescribedColumn>> {
        let declaration = param_declaration(params);
        // Bound, not interpolated — the statement text is a VALUE to this
        // query, and building it into the SQL would be a hole in the one place
        // the adapter is meant to be closing one.
        let owned: Vec<Box<dyn ToSql>> = vec![
            Box::new(statement.to_string()),
            match &declaration {
                Some(d) => Box::new(d.clone()),
                None => Box::new(Option::<String>::None),
            },
        ];
        let bound: Vec<&dyn ToSql> = owned.iter().map(|b| b.as_ref()).collect();
        let rows = session
            .query(
                "SELECT column_ordinal, name, system_type_name, is_nullable, \
                 error_number, error_message \
                 FROM sys.dm_exec_describe_first_result_set(@P1, @P2, 0) \
                 ORDER BY column_ordinal",
                &bound,
            )
            .await
            .map_err(|e| classify_sqlserver_error(&e))?
            .into_first_result()
            .await
            .map_err(|e| classify_sqlserver_error(&e))?;

        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            // The function reports a statement it cannot describe as a single
            // row carrying the error rather than as a failure, so the error
            // column has to be read or the refusal is silently skipped.
            if let Some(number) = r.get::<i32, _>("error_number") {
                let message = r.get::<&str, _>("error_message").unwrap_or_default();
                return Err(Refusal::not_covered(format!(
                    "SQL Server cannot describe this statement's result shape \
                     (error {number}: {message}); this adapter will not execute a statement \
                     whose column types it cannot check first"
                )));
            }
            out.push(DescribedColumn {
                name: r.get::<&str, _>("name").unwrap_or_default().to_string(),
                source_type: base_type_name(r.get::<&str, _>("system_type_name").unwrap_or("")),
                nullable: r.get::<bool, _>("is_nullable").unwrap_or(true),
            });
        }
        Ok(out)
    }

    /// Run a batch that returns nothing, and DRIVE it to completion.
    ///
    /// tiberius reports a failed batch when the stream is consumed, not when
    /// the future that sent it resolves — so dropping the stream (which the
    /// next call would silently flush) turns a failed `BEGIN TRANSACTION` into
    /// a read that quietly ran outside one.
    async fn run_batch(session: &mut Session, sql: &str) -> Result<()> {
        let stream = session
            .simple_query(sql.to_string())
            .await
            .map_err(|e| classify_sqlserver_error(&e))?;
        stream
            .into_results()
            .await
            .map_err(|e| classify_sqlserver_error(&e))?;
        Ok(())
    }

    async fn scalar_i64(session: &mut Session, sql: &str) -> Result<Option<i64>> {
        let stream = session
            .simple_query(sql.to_string())
            .await
            .map_err(|e| classify_sqlserver_error(&e))?;
        let row = stream
            .into_row()
            .await
            .map_err(|e| classify_sqlserver_error(&e))?;
        Ok(row.and_then(|r| r.get::<i64, _>(0)))
    }

    /// Whether the DATABASE allows snapshot isolation.
    ///
    /// Asked rather than attempted: a session may `SET TRANSACTION ISOLATION
    /// LEVEL SNAPSHOT` on a database that forbids it and the SET succeeds —
    /// the failure arrives as 3952 on the first READ, by which time recovering
    /// means re-running the caller's statement. `sys.databases` rather than
    /// `DATABASEPROPERTYEX`, because the latter returned NULL for the fixture's
    /// least-privileged reader (it needs a metadata permission a data reader
    /// has no reason to hold) and a NULL there would silently mean "no".
    async fn snapshot_allowed(session: &mut Session) -> bool {
        let sql = "SELECT CAST(snapshot_isolation_state AS BIGINT) \
                     FROM sys.databases WHERE database_id = DB_ID()";
        matches!(Self::scalar_i64(session, sql).await, Ok(Some(1)))
    }

    /// Open a session, bound it, and begin the read transaction.
    ///
    /// Returns the isolation the transaction ACTUALLY started in, because that
    /// is what decides whether a change-tracking version is a marker or a race.
    async fn begin_read(&self, row_cap: u64, timeout_ms: u64) -> Result<(Session, Isolation)> {
        let mut session = self.session().await?;
        let isolation = if Self::snapshot_allowed(&mut session).await {
            Isolation::Snapshot
        } else {
            Isolation::ReadCommitted
        };
        // Three engine-side bounds, all session-scoped, all applied BEFORE the
        // transaction opens:
        //   * LOCK_TIMEOUT bounds waiting on a lock — the one thing SQL Server
        //     will abort for us. There is no server-side statement timeout on
        //     this engine, so the wall clock is enforced by the caller's
        //     `tokio::time::timeout` and the session is dropped, which the
        //     engine treats as an aborted request and rolls back.
        //   * ROWCAP is a real source-side row limit, so a runaway result is
        //     stopped at the engine rather than after it crossed the wire.
        //   * The isolation level, which SNAPSHOT refuses to accept once a
        //     transaction is open (3951).
        let preamble = format!(
            "SET LOCK_TIMEOUT {timeout_ms};\nSET ROWCOUNT {row_cap};\n{};",
            isolation.set_statement()
        );
        Self::run_batch(&mut session, &preamble).await?;
        Self::run_batch(&mut session, "BEGIN TRANSACTION").await?;
        Ok((session, isolation))
    }
}

/// A tiberius error's own words, with nothing of ours added.
///
/// `Display` for a config-parse error can quote the string it failed on, which
/// holds a password; every caller that formats an error goes through here so
/// that decision is made once. Server errors are safe to quote — they are the
/// engine talking about the schema.
fn e_kind(e: &tiberius::error::Error) -> String {
    match e {
        tiberius::error::Error::Server(t) => format!("{} (error {})", t.message(), t.code()),
        tiberius::error::Error::Io { kind, .. } => format!("io: {kind:?}"),
        tiberius::error::Error::Tls(_) => "tls handshake failed".into(),
        tiberius::error::Error::Protocol(_) => "protocol error".into(),
        tiberius::error::Error::Conversion(m) => format!("conversion: {m}"),
        _ => "the driver rejected the request".into(),
    }
}

/// The login name in an ADO/JDBC connection string, for the evidence record.
///
/// Only the user key is read; the password is never touched, so this cannot
/// accidentally return one.
fn principal_of(connection_string: &str) -> Option<String> {
    for part in connection_string.split(';') {
        let (k, v) = part.split_once('=')?;
        let key = k.trim().to_ascii_lowercase().replace(' ', "");
        if matches!(key.as_str(), "uid" | "user" | "userid" | "username") {
            return Some(v.trim().to_string());
        }
    }
    None
}

/// One output column, as SQL Server itself describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DescribedColumn {
    pub name: String,
    /// The base type name, with any `(precision, scale)` or `(max)` stripped —
    /// `decimal(18,2)` is a `decimal`, and the scale rides in the value.
    pub source_type: String,
    pub nullable: bool,
}

/// `decimal(18,2)` -> `decimal`, `nvarchar(max)` -> `nvarchar`.
fn base_type_name(system_type_name: &str) -> String {
    system_type_name
        .split_once('(')
        .map(|(head, _)| head)
        .unwrap_or(system_type_name)
        .trim()
        .to_ascii_lowercase()
}

/// A `@params` declaration for the values about to be bound.
///
/// `sys.dm_exec_describe_first_result_set` compiles the statement, so every
/// placeholder in it has to be declared or the describe fails. The declared
/// types only have to let the statement COMPILE — the real execution binds the
/// real values through the driver — so each canon@1 type maps to the widest
/// SQL Server type that accepts it.
fn param_declaration(values: &[Value]) -> Option<String> {
    if values.is_empty() {
        return None;
    }
    let parts: Vec<String> = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let ty = match v {
                Value::Bool(_) => "bit",
                Value::Int64(_) => "bigint",
                Value::Decimal { .. } => "decimal(38,10)",
                Value::Float64(_) => "float",
                Value::Bytes(_) => "varbinary(8000)",
                Value::Date(_) => "date",
                Value::TimestampTz(_) => "datetimeoffset",
                Value::TimestampNaive(_) => "datetime2",
                // NULL and everything canon@1 carries as text.
                _ => "nvarchar(4000)",
            };
            format!("@P{} {ty}", i + 1)
        })
        .collect();
    Some(parts.join(", "))
}

/// SQL Server's own error numbers, mapped to the closed refusal set.
///
/// The numbers are the engine's, not a family of strings: 229 is "permission
/// denied on object" and will still be 229 in ten years, which is the
/// difference between a classifier and a heuristic.
pub fn classify_sqlserver_error(e: &tiberius::error::Error) -> Refusal {
    if let tiberius::error::Error::Server(token) = e {
        let message = token.message().to_string();
        return match token.code() {
            // 229/230 object- and column-level permission; 262 permission
            // denied in database; 297 no permission to perform this action;
            // 916 no access to the database under the current security
            // context; 18456 login failed.
            229 | 230 | 262 | 297 | 916 | 18456 => Refusal::policy_denied(message),
            // 208 invalid object name; 207 invalid column name; 4104 the
            // multi-part identifier could not be bound. The contract names
            // something the source does not have: drift, not unavailability.
            207 | 208 | 4104 => Refusal::schema_drift(message),
            // 1205 deadlock victim; 1222 lock request timeout (which is what
            // SET LOCK_TIMEOUT produces); 8645 memory timeout.
            1205 | 1222 | 8645 => {
                Refusal::new(RefusalClass::Exhausted, "deadline_exceeded", message)
            }
            // 3952 the statement ran under snapshot isolation on a database
            // that does not allow it, and 3951 the transaction did not start
            // in it. Both mean this adapter asked for a guarantee the source
            // cannot give — invalid rather than transient, because retrying
            // changes nothing.
            3951 | 3952 => Refusal::new(
                RefusalClass::Invalid,
                "snapshot_isolation_unavailable",
                message,
            ),
            // 4060 cannot open database; 40613 database unavailable (Azure
            // SQL); 40197/40501 service busy.
            4060 | 40197 | 40501 | 40613 => Refusal::source_unavailable(message),
            _ => Refusal::source_unavailable(message),
        };
    }
    Refusal::source_unavailable(e_kind(e))
}

/// `$1`, `$2`, … → `@P1`, `@P2`, ….
///
/// The compiler renumbers a contract's named parameters positionally and
/// renders them Postgres-style; SQL Server's placeholder is `@Pn` bound by
/// position in the parameter slice. Rewriting here rather than in the compiler
/// keeps ONE plan — and one plan hash, which is over the parsed AST — running
/// on every engine. A `$` inside a string literal is left alone, and so is a
/// number beyond the bound count: if the compiler and the binder ever
/// disagree, the engine must see the discrepancy and refuse rather than
/// receive a placeholder with nothing bound.
pub fn positional_to_at_p(sql: &str, count: usize) -> String {
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
                    out.push_str(&format!("@P{k}"));
                    continue;
                }
            }
        }
        out.push(c);
    }
    out
}

/// A bound value in a form tiberius can send.
///
/// Owned rather than borrowed because `Client::query` wants a slice of trait
/// objects that outlive the call; boxing once per parameter is cheaper than
/// the lifetime gymnastics and impossible to get wrong.
fn to_tds(v: &Value) -> Box<dyn ToSql> {
    match v {
        // Typed as NVARCHAR NULL. SQL Server converts a NULL of any type to
        // the column's type, so the declared type of a null is not observable.
        Value::Null => Box::new(Option::<String>::None),
        Value::Bool(b) => Box::new(*b),
        Value::Int64(i) => Box::new(*i),
        Value::Decimal { value, .. } => Box::new(*value),
        Value::Float64(f) => Box::new(*f),
        Value::String(s) => Box::new(s.clone()),
        Value::Bytes(b) => Box::new(b.clone()),
        Value::Date(d) => Box::new(*d),
        Value::TimestampTz(t) => Box::new(*t),
        Value::TimestampNaive(t) => Box::new(*t),
        // Everything else reaches the engine as its canonical text, which is
        // what the contract declared it as anyway.
        other => Box::new(other.canonical_text().unwrap_or_default()),
    }
}

#[async_trait::async_trait]
impl SourceAdapter for SqlServerAdapter {
    fn kind(&self) -> &'static str {
        "sqlserver"
    }

    fn adapter_version(&self) -> &'static str {
        ADAPTER_VERSION
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            sync_modes: vec![SyncMode::Snapshot, SyncMode::Watermark],
            // SQL Server has a real row-level policy engine, so source-native
            // is honest here in a way it is not on MySQL; per-class principals
            // also work for a deployment that prefers separate logins.
            policy_strategies: vec![
                PolicyStrategy::SourceNative,
                PolicyStrategy::PerClassPrincipals,
            ],
            query_contracts: true,
            metric_views: false,
            // One fact table and declared aggregates: any SQL engine with a
            // catalog definition to fingerprint serves that.
            data_views: true,
            semantic_provider: None,
            dialect: Some("sqlserver".to_string()),
            // What the ENGINE can offer. Whether a given read got one is the
            // manifest's business, and depends on change tracking being on and
            // the transaction being snapshot-isolated.
            snapshot_marker: Some("change_tracking_version".to_string()),
            // A change-tracking version is a POSITION, not a retained history:
            // there is nothing to re-run the query against, so the honest
            // promise is the sealed bytes. (Temporal tables would support more,
            // for the tables that are system-versioned — which is a per-table
            // property and cannot be a per-source capability.)
            replay_level: "sealed_result".into(),
            // Not a TDS attention packet, which tiberius does not expose:
            // dropping the session closes the socket, and SQL Server aborts the
            // request and rolls the transaction back. Because this adapter owns
            // its session exclusively, that is a real cancellation rather than
            // a hope.
            cancellation: true,
            // Rows, yes — `SET ROWCOUNT` stops the engine. Time, no: SQL Server
            // has no server-side statement timeout, and the deadline is the
            // caller's clock plus the cancellation above.
            source_side_limits: true,
        }
    }

    async fn probe(&self) -> Result<ProbeResult> {
        let started = std::time::Instant::now();
        let mut session = match self.session().await {
            Ok(s) => s,
            Err(e) => {
                return Ok(ProbeResult {
                    reachable: false,
                    latency_ms: None,
                    detail: Some(e.message),
                })
            }
        };
        match Self::run_batch(&mut session, "SELECT 1").await {
            Ok(()) => Ok(ProbeResult {
                reachable: true,
                latency_ms: Some(started.elapsed().as_millis() as u64),
                detail: None,
            }),
            Err(e) => Ok(ProbeResult {
                reachable: false,
                latency_ms: None,
                detail: Some(e.message),
            }),
        }
    }

    /// What this login can actually do, proved by asking the engine.
    ///
    /// One operational note that cost a measurement to learn: SQL Server
    /// filters catalog metadata by permission, so a login with SELECT on a
    /// table still sees ZERO rows in `sys.security_predicates` unless it holds
    /// VIEW DEFINITION. Without that grant this function reports "no row
    /// security" for a table that has it — an absence of evidence read as
    /// evidence of absence, on the one check where that is most dangerous.
    /// Postgres has no equivalent trap because `pg_catalog` is world-readable.
    async fn introspect(&self) -> Result<(RolePosture, SchemaFingerprint)> {
        let mut session = self.session().await?;

        // Five facts, one round trip. `IS_SRVROLEMEMBER` returns NULL for a
        // role the login cannot see, and a NULL here must not read as "not a
        // sysadmin", so it is coalesced to the UNSAFE answer.
        let sql = format!(
            "SELECT CAST(COALESCE(IS_SRVROLEMEMBER('sysadmin'), 1) AS BIGINT) AS is_sysadmin,
                    CAST(COALESCE(IS_ROLEMEMBER('db_owner'), 1) AS BIGINT) AS is_db_owner,
                    CAST(
                      COALESCE(IS_ROLEMEMBER('db_datawriter'), 1)
                      + COALESCE(IS_ROLEMEMBER('db_ddladmin'), 1)
                      + HAS_PERMS_BY_NAME('{schema}', 'SCHEMA', 'INSERT')
                      + HAS_PERMS_BY_NAME('{schema}', 'SCHEMA', 'UPDATE')
                      + HAS_PERMS_BY_NAME('{schema}', 'SCHEMA', 'DELETE')
                      + HAS_PERMS_BY_NAME('{schema}', 'SCHEMA', 'ALTER')
                    AS BIGINT) AS write_grants,
                    CAST((SELECT COUNT(*) FROM sys.tables t
                            JOIN sys.schemas s ON s.schema_id = t.schema_id
                           WHERE s.name = '{schema}'
                             AND COALESCE(t.principal_id, s.principal_id)
                                 = DATABASE_PRINCIPAL_ID()) AS BIGINT) AS owned_tables",
            schema = self.schema.replace('\'', "''")
        );
        let stream = session
            .simple_query(sql)
            .await
            .map_err(|e| classify_sqlserver_error(&e))?;
        let row = stream
            .into_row()
            .await
            .map_err(|e| classify_sqlserver_error(&e))?
            .ok_or_else(|| Refusal::source_unavailable("the posture query returned no row"))?;
        let field = |name: &str| row.get::<i64, _>(name).unwrap_or(1);
        let is_sysadmin = field("is_sysadmin") != 0 || field("is_db_owner") != 0;
        let has_dml = field("write_grants") != 0;
        let owns_any = field("owned_tables") != 0;

        // Row security, per table. An enabled FILTER predicate on the table is
        // the fact; a disabled policy is not protection.
        let rls_sql = format!(
            "SELECT t.name AS table_name,
                    CAST(CASE WHEN EXISTS (
                        SELECT 1 FROM sys.security_predicates sp
                          JOIN sys.security_policies pol ON pol.object_id = sp.object_id
                         WHERE sp.target_object_id = t.object_id AND pol.is_enabled = 1)
                    THEN 1 ELSE 0 END AS BIGINT) AS rls
               FROM sys.tables t JOIN sys.schemas s ON s.schema_id = t.schema_id
              WHERE s.name = '{schema}'",
            schema = self.schema.replace('\'', "''")
        );
        let rls_rows = session
            .simple_query(rls_sql)
            .await
            .map_err(|e| classify_sqlserver_error(&e))?
            .into_first_result()
            .await
            .map_err(|e| classify_sqlserver_error(&e))?;
        let mut rls_by_table: Vec<(String, bool)> = Vec::new();
        for r in &rls_rows {
            let name = r
                .get::<&str, _>("table_name")
                .unwrap_or_default()
                .to_string();
            rls_by_table.push((name, r.get::<i64, _>("rls").unwrap_or(0) != 0));
        }
        // "Some tables are protected" is not a posture. The check is TRUE only
        // when every table this source exposes is behind an enabled policy —
        // the same bar the Postgres adapter sets with `bool_and`.
        let rls_everywhere =
            !rls_by_table.is_empty() && rls_by_table.iter().all(|(_, protected)| *protected);

        let posture = RolePosture {
            principal: self.principal.clone(),
            checks: vec![
                PostureCheck::new("not_superuser", true, !is_sysadmin).with_detail(
                    "sysadmin, and db_owner within the database, bypass every policy the \
                     source declares",
                ),
                PostureCheck::new("not_owner", true, !owns_any)
                    .with_detail("a table's owner is not subject to its security policy"),
                PostureCheck::new("read_only", true, !has_dml).with_detail(
                    "the login must hold no write role and no INSERT/UPDATE/DELETE/ALTER \
                     permission on the schema; T-SQL has no read-only transaction, so this \
                     is where read-only is actually established",
                ),
                // Reported whether present or absent, and here it can genuinely
                // be either — which is the contrast that makes the MySQL
                // adapter's permanent `false` legible rather than looking like
                // a stub.
                PostureCheck::new("subject_to_row_security", true, rls_everywhere).with_detail(
                    "every table in the schema is behind an enabled row-level security policy \
                     (requires VIEW DEFINITION to observe: without it a protected table looks \
                     unprotected)",
                ),
            ],
        };

        let shape_sql = format!(
            "SELECT TABLE_NAME AS t, COLUMN_NAME AS c, DATA_TYPE AS d, IS_NULLABLE AS n
               FROM INFORMATION_SCHEMA.COLUMNS
              WHERE TABLE_SCHEMA = '{schema}'
              ORDER BY TABLE_NAME, ORDINAL_POSITION",
            schema = self.schema.replace('\'', "''")
        );
        let shape_rows = session
            .simple_query(shape_sql)
            .await
            .map_err(|e| classify_sqlserver_error(&e))?
            .into_first_result()
            .await
            .map_err(|e| classify_sqlserver_error(&e))?;

        let mut tables: Vec<TableShape> = Vec::new();
        for r in &shape_rows {
            let table = r.get::<&str, _>("t").unwrap_or_default().to_string();
            let column = r.get::<&str, _>("c").unwrap_or_default().to_string();
            let data_type = r.get::<&str, _>("d").unwrap_or_default().to_string();
            let nullable = r.get::<&str, _>("n").unwrap_or("YES") == "YES";
            let shape = ColumnShape {
                name: column,
                source_type: data_type.clone(),
                logical_type: Self::logical_type(&data_type),
                nullable,
            };
            let protected = rls_by_table
                .iter()
                .find(|(n, _)| n == &table)
                .map(|(_, p)| *p)
                .unwrap_or(false);
            match tables.iter_mut().find(|t| t.name == table) {
                Some(t) => t.columns.push(shape),
                None => tables.push(TableShape {
                    name: table,
                    columns: vec![shape],
                    row_security_enabled: protected,
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
                "a sqlserver read needs an explicit projection: selecting * would read \
                 columns the policy denies and would move every time the source adds one",
            ));
        }
        let cols = projection
            .iter()
            .map(|c| quote_ident(c))
            .collect::<Vec<_>>()
            .join(", ");
        let table = format!("{}.{}", quote_ident(&self.schema), quote_ident(entity));
        // +1 row so truncation is DETECTED rather than assumed.
        let probe_limit = limits.max_rows.saturating_add(1);

        // SQL Server has no LIMIT and no row-value comparison. `TOP (n)` is the
        // former; the latter has to be spelled out, because `(a, b) > (x, y)`
        // is a syntax error here and writing `a > x AND b > y` instead would
        // silently drop every row that shares a watermark with the boundary —
        // which is exactly the tie the tie-break exists for.
        // The watermark columns come from the DataSource's declaration
        // (`Watermark::resolve`), not from this adapter: `[updated_at]` and
        // `[id]` were hard-coded here until 2026-08-30, so a source that
        // declared any other column validated and was then read by one it had
        // never named.
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
        let (sql, bound) = match (wm, &checkpoint.watermark) {
            (Some(w), Some(_)) => {
                let cmp = w.cmp();
                let wmc = quote_ident(w.column);
                match w.tie_break {
                    // No row-value comparison in T-SQL: `(a, b) > (x, y)` is a
                    // syntax error, and `a > x AND b > y` would silently drop
                    // every row that shares a watermark with the boundary --
                    // exactly the tie the tie-break exists for.
                    Some(t) => (
                        format!(
                            "SELECT TOP ({probe_limit}) {cols} FROM {table} \
                             WHERE {wmc} > @P1 \
                             OR ({wmc} = @P1 AND {tb} {cmp} @P2) \
                             ORDER BY {order_by}",
                            tb = quote_ident(t),
                        ),
                        2,
                    ),
                    None => (
                        format!(
                            "SELECT TOP ({probe_limit}) {cols} FROM {table} \
                             WHERE {wmc} {cmp} @P1 ORDER BY {order_by}"
                        ),
                        1,
                    ),
                }
            }
            // The first watermark read: everything, in watermark order, so the
            // last row kept is the checkpoint the next run continues from.
            (Some(_), None) => (
                format!("SELECT TOP ({probe_limit}) {cols} FROM {table} ORDER BY {order_by}"),
                0,
            ),
            _ => (format!("SELECT TOP ({probe_limit}) {cols} FROM {table}"), 0),
        };

        let (mut session, isolation) = self.begin_read(probe_limit, limits.timeout_ms).await?;
        let ct = Self::scalar_i64(
            &mut session,
            "SELECT CAST(CHANGE_TRACKING_CURRENT_VERSION() AS BIGINT)",
        )
        .await
        .unwrap_or(None);
        let marker = snapshot_marker_for(ct, isolation);

        let probe_values: Vec<Value> = match bound {
            2 => vec![
                Value::String(checkpoint.watermark.clone().unwrap_or_default()),
                Value::String(checkpoint.tie_break.clone().unwrap_or_default()),
            ],
            1 => vec![Value::String(
                checkpoint.watermark.clone().unwrap_or_default(),
            )],
            _ => vec![],
        };
        let owned: Vec<Box<dyn ToSql>> = probe_values.iter().map(to_tds).collect();
        let params: Vec<&dyn ToSql> = owned.iter().map(|b| b.as_ref()).collect();

        // The shape first, and the refusal with it: a projection naming a
        // spatial column must not reach the driver.
        let described = Self::describe_result_shape(&mut session, &sql, &probe_values).await?;
        let columns = Self::columns_of(&described, None)?;
        let mut rows = session
            .query(sql, &params)
            .await
            .map_err(|e| classify_sqlserver_error(&e))?
            .into_first_result()
            .await
            .map_err(|e| classify_sqlserver_error(&e))?;

        let truncated = rows.len() as u64 > limits.max_rows;
        if truncated {
            rows.truncate(limits.max_rows as usize);
        }
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
        let _ = Self::run_batch(&mut session, "COMMIT").await;

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
        let sql = positional_to_at_p(statement, parameters.positional.len());
        let probe_limit = limits.max_rows.saturating_add(1);

        let (mut session, isolation) = self.begin_read(probe_limit, limits.timeout_ms).await?;
        // Read inside the transaction, so under SNAPSHOT it names the same
        // consistent view the rows come from.
        let ct = Self::scalar_i64(
            &mut session,
            "SELECT CAST(CHANGE_TRACKING_CURRENT_VERSION() AS BIGINT)",
        )
        .await
        .unwrap_or(None);

        let owned: Vec<Box<dyn ToSql>> = parameters.positional.iter().map(to_tds).collect();
        let params: Vec<&dyn ToSql> = owned.iter().map(|b| b.as_ref()).collect();

        // The wall clock. SQL Server has no server-side statement timeout, so
        // this is the deadline — and dropping the session on the way out of a
        // timeout closes the socket, which the engine treats as an aborted
        // request and rolls back.
        let deadline = Duration::from_millis(limits.timeout_ms.max(1));
        let (columns, rows) = match tokio::time::timeout(deadline, async {
            // Refused on the ENGINE'S description, before the statement runs at
            // all: a type canon@1 does not model must be named, and on this
            // driver it cannot be named any later than here without a panic.
            let described =
                SqlServerAdapter::describe_result_shape(&mut session, &sql, &parameters.positional)
                    .await?;
            let columns = SqlServerAdapter::columns_of(&described, None)?;
            let rows = session
                .query(sql, &params)
                .await
                .map_err(|e| classify_sqlserver_error(&e))?
                .into_first_result()
                .await
                .map_err(|e| classify_sqlserver_error(&e))?;
            Ok::<_, Refusal>((columns, rows))
        })
        .await
        {
            Ok(inner) => inner?,
            Err(_) => {
                return Err(Refusal::deadline_exceeded(format!(
                    "the statement did not finish within {}ms; the session was closed, which \
                     aborts the request at the engine",
                    limits.timeout_ms
                )))
            }
        };
        let ended_at = chrono::Utc::now();

        let mut rows = rows;
        let truncated = rows.len() as u64 > limits.max_rows;
        if truncated {
            rows.truncate(limits.max_rows as usize);
        }
        let mut out_rows = Vec::with_capacity(rows.len());
        for r in &rows {
            let mut cells = Vec::with_capacity(columns.len());
            for (i, c) in columns.iter().enumerate() {
                cells.push(SqlServerAdapter::decode_cell(r, i, c.ty, c.scale)?);
            }
            out_rows.push(Row { cells });
        }
        let _ = Self::run_batch(&mut session, "COMMIT").await;

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
            snapshot_marker: snapshot_marker_for(ct, isolation),
            isolation: Some(isolation.as_str().into()),
            engine: Some("sqlserver".into()),
            statement_id: None,
            started_at,
            ended_at,
        })
    }

    /// A table's definition as `INFORMATION_SCHEMA` reports it — the same shape
    /// the Postgres and MySQL adapters return, so a native data view's
    /// fingerprint means the same thing on all three engines.
    async fn definition_of(&self, object: &str, _limits: Limits) -> Result<String> {
        let (schema, table) = match object.split_once('.') {
            Some((s, t)) => (s.to_string(), t.to_string()),
            None => (self.schema.clone(), object.to_string()),
        };
        let mut session = self.session().await?;
        let sql = format!(
            "SELECT COLUMN_NAME AS c, DATA_TYPE AS d, IS_NULLABLE AS n
               FROM INFORMATION_SCHEMA.COLUMNS
              WHERE TABLE_SCHEMA = '{}' AND TABLE_NAME = '{}'
              ORDER BY ORDINAL_POSITION",
            schema.replace('\'', "''"),
            table.replace('\'', "''"),
        );
        let rows = session
            .simple_query(sql)
            .await
            .map_err(|e| classify_sqlserver_error(&e))?
            .into_first_result()
            .await
            .map_err(|e| classify_sqlserver_error(&e))?;
        if rows.is_empty() {
            return Err(Refusal::not_covered(format!(
                "{object} has no columns visible to this principal, or does not exist"
            )));
        }
        Ok(rows
            .iter()
            .map(|r| {
                format!(
                    "{}:{}:{}",
                    r.get::<&str, _>("c").unwrap_or_default(),
                    r.get::<&str, _>("d").unwrap_or_default(),
                    r.get::<&str, _>("n").unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_bracketed_and_an_embedded_bracket_is_doubled() {
        assert_eq!(quote_ident("orders"), "[orders]");
        assert_eq!(quote_ident("we]ird"), "[we]]ird]");
    }

    #[test]
    fn positional_placeholders_become_at_p_in_order() {
        assert_eq!(
            positional_to_at_p("SELECT a FROM t WHERE b > $1 AND c = $2 AND d = 'x$3y'", 2),
            "SELECT a FROM t WHERE b > @P1 AND c = @P2 AND d = 'x$3y'"
        );
    }

    #[test]
    fn a_placeholder_beyond_the_bound_count_is_left_alone_rather_than_invented() {
        // If the compiler and the binder ever disagree, the engine must see the
        // discrepancy and refuse — not receive an `@P2` with nothing bound.
        assert_eq!(positional_to_at_p("SELECT $1, $2", 1), "SELECT @P1, $2");
    }

    #[test]
    fn sqlserver_types_map_onto_the_closed_set_and_an_unknown_one_is_not_guessed() {
        assert_eq!(
            SqlServerAdapter::logical_type("decimal"),
            Some(ColumnType::Decimal)
        );
        assert_eq!(
            SqlServerAdapter::logical_type("bigint"),
            Some(ColumnType::Int64)
        );
        // `datetimeoffset` is zoned and `datetime2` is not: different canon@1
        // types, the same distinction MySQL's TIMESTAMP/DATETIME draws.
        assert_eq!(
            SqlServerAdapter::logical_type("datetimeoffset"),
            Some(ColumnType::TimestampTz)
        );
        assert_eq!(
            SqlServerAdapter::logical_type("datetime2"),
            Some(ColumnType::TimestampNaive)
        );
        // The refusals that matter. `money` is EXACT on the server and a double
        // in this driver, so accepting it would put a currency in sealed
        // evidence as a float.
        assert_eq!(SqlServerAdapter::logical_type("money"), None);
        assert_eq!(SqlServerAdapter::logical_type("smallmoney"), None);
        assert_eq!(SqlServerAdapter::logical_type("geography"), None);
        assert_eq!(SqlServerAdapter::logical_type("hierarchyid"), None);
        assert_eq!(SqlServerAdapter::logical_type("sql_variant"), None);
        assert_eq!(SqlServerAdapter::logical_type("time"), None);
    }

    #[test]
    fn a_marker_is_reported_only_when_it_is_not_a_race() {
        // Both conditions hold: change tracking gave a version, and the
        // transaction is snapshot-isolated, so the version names the same view
        // the rows came from.
        assert_eq!(
            snapshot_marker_for(Some(7), Isolation::Snapshot),
            Some("ct:7".to_string())
        );
        // Version 0 is a real version, not "no version" — a fresh database
        // reports it and a marker of `ct:0` is correct.
        assert_eq!(
            snapshot_marker_for(Some(0), Isolation::Snapshot),
            Some("ct:0".to_string())
        );
        // Change tracking is off: there is no position to report.
        assert_eq!(snapshot_marker_for(None, Isolation::Snapshot), None);
        // Change tracking is on but the read was not snapshot-isolated, so the
        // version was taken at a different instant from the rows. Reporting it
        // would put a racing position into a sealed manifest.
        assert_eq!(snapshot_marker_for(Some(7), Isolation::ReadCommitted), None);
        assert_eq!(snapshot_marker_for(None, Isolation::ReadCommitted), None);
    }

    #[test]
    fn the_principal_is_the_login_and_never_the_password() {
        let ado = "Server=tcp:db,1433;User Id=matrix_reader;Password=hunter2;Database=crm";
        assert_eq!(principal_of(ado).as_deref(), Some("matrix_reader"));
        assert_eq!(
            principal_of("Server=tcp:db;uid=svc;pwd=secret").as_deref(),
            Some("svc")
        );
        // No user key at all (integrated security) is not an error; it is an
        // unknown principal, and the caller substitutes a default.
        assert_eq!(principal_of("Server=tcp:db;IntegratedSecurity=true"), None);
    }

    #[test]
    fn the_isolation_statement_names_the_level_the_result_will_report() {
        assert!(Isolation::Snapshot.set_statement().ends_with("SNAPSHOT"));
        assert_eq!(Isolation::Snapshot.as_str(), "snapshot");
        assert_eq!(Isolation::ReadCommitted.as_str(), "read committed");
    }
}
