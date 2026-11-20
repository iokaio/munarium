// SPDX-License-Identifier: Apache-2.0
//! The MCP endpoint's durable records (migration 0070): A6 admission and
//! outcome linkage, and the §5.3 operation keys and receipts.
//!
//! Every write that must precede a tool's execution is one transaction whose
//! COMMIT acknowledgment the caller waits for under
//! `MUNARIUM_MCP_AUDIT_DEADLINE_MS`. Anything short of a confirmed commit —
//! store unavailable, deadline expired, an error after the commit was sent —
//! is an admission failure: the caller refuses the call and runs nothing.
//! The database clock is the only clock used for key validity and deadlines.

use munarium_core::{KernelError, Result};
use serde_json::{json, Value};
use sqlx::Row;

fn storage(e: sqlx::Error) -> KernelError {
    KernelError::Storage(e.to_string())
}

/// Test-only fault injection (A6-T1): fail before COMMIT, delay past the
/// deadline, or commit and then lose the acknowledgment.
#[cfg(test)]
#[derive(Default)]
pub struct Faults {
    pub fail_before_commit: std::sync::atomic::AtomicBool,
    pub lose_ack: std::sync::atomic::AtomicBool,
    pub delay_ms: std::sync::atomic::AtomicU64,
}

/// Why an admission was not confirmed. The detail is never shown to the
/// caller (it may carry driver text); it is logged and counted.
#[derive(Debug)]
pub enum AdmissionFailure {
    /// No durable store (memory deployment).
    NoStore,
    /// The store refused or failed before the commit was acknowledged.
    Store(String),
    /// The acknowledgment did not arrive by the deadline.
    Deadline,
    /// The commit may have happened but its acknowledgment was lost.
    LostAck,
}

impl AdmissionFailure {
    pub fn label(&self) -> &'static str {
        match self {
            AdmissionFailure::NoStore => "no-durable-store",
            AdmissionFailure::Store(_) => "store-error",
            AdmissionFailure::Deadline => "deadline",
            AdmissionFailure::LostAck => "lost-ack",
        }
    }
}

/// The admission record (A6). Content-free by construction: no argument
/// body, no credential, no retrieved text.
#[derive(Debug, Clone)]
pub struct Admission {
    pub invocation_id: String,
    pub tenant_id: String,
    pub uid: String,
    pub token_jti: String,
    pub request_id: String,
    pub tool: &'static str,
    pub protocol_version: &'static str,
    /// `read`, `execute` or `recover`.
    pub form: &'static str,
    pub instance_id: String,
}

/// A caller-known operation key (§5.3): an immutable UTC expiry and a
/// caller-generated random identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationKey {
    pub id: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

impl OperationKey {
    pub const MIN_ID: usize = 16;
    pub const MAX_ID: usize = 128;

    pub fn parse(v: &Value) -> std::result::Result<Self, String> {
        let obj = v
            .as_object()
            .ok_or("operation_key must be an object {id, expires_at}")?;
        if obj.keys().any(|k| k != "id" && k != "expires_at") {
            return Err("operation_key carries only id and expires_at".into());
        }
        let id = obj
            .get("id")
            .and_then(Value::as_str)
            .ok_or("operation_key.id is required")?;
        if id.len() < Self::MIN_ID
            || id.len() > Self::MAX_ID
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(format!(
                "operation_key.id must be {}-{} characters of A-Z a-z 0-9 - _ (a random value the client keeps)",
                Self::MIN_ID,
                Self::MAX_ID
            ));
        }
        let at = obj
            .get("expires_at")
            .and_then(Value::as_str)
            .ok_or("operation_key.expires_at is required")?;
        if !at.ends_with('Z') {
            return Err("operation_key.expires_at must be an RFC 3339 UTC time ending in Z".into());
        }
        let expires_at = chrono::DateTime::parse_from_rfc3339(at)
            .map_err(|_| "operation_key.expires_at must be an RFC 3339 UTC time")?
            .with_timezone(&chrono::Utc);
        Ok(Self {
            id: id.to_string(),
            expires_at,
        })
    }
}

/// The outcome of claiming a key for new work.
#[derive(Debug)]
pub enum Claim {
    /// This admission owns the key; only it may dispatch.
    Claimed,
    /// The key already exists for this tenant, uid and tool.
    Existing(Box<OperationRow>),
    /// The key is expired, too long-lived, or too short for the work
    /// deadline, judged on the store clock after the lock wait.
    Invalid(&'static str),
}

/// A stored operation row (the receipt's source). Never holds a body.
#[derive(Debug, Clone)]
pub struct OperationRow {
    pub tool: String,
    pub key_id: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub args_digest: String,
    pub invocation_id: String,
    pub state: String,
    pub past_deadline: bool,
    pub resources: Value,
    pub http_status: Option<i32>,
    pub problem_type: Option<String>,
    pub session_id: Option<String>,
    pub turn_ordinal: Option<i32>,
    pub result_bytes: Option<i64>,
    pub result_too_large: bool,
}

async fn insert_admission(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    a: &Admission,
    original: Option<&str>,
) -> std::result::Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO mcp_invocations
            (invocation_id, tenant_id, uid, token_jti, request_id, tool, protocol_version,
             form, original_invocation_id, instance_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(&a.invocation_id)
    .bind(&a.tenant_id)
    .bind(&a.uid)
    .bind(&a.token_jti)
    .bind(&a.request_id)
    .bind(a.tool)
    .bind(a.protocol_version)
    .bind(a.form)
    .bind(original)
    .bind(&a.instance_id)
    .execute(&mut **tx)
    .await
    .map(|_| ())
}

fn row_to_operation(row: &sqlx::postgres::PgRow) -> OperationRow {
    OperationRow {
        tool: row.get("tool"),
        key_id: row.get("key_id"),
        expires_at: row.get("expires_at"),
        args_digest: row.get("args_digest"),
        invocation_id: row.get("invocation_id"),
        state: row.get("state"),
        past_deadline: row.get("past_deadline"),
        resources: row.get::<sqlx::types::Json<Value>, _>("resources").0,
        http_status: row.get("http_status"),
        problem_type: row.get("problem_type"),
        session_id: row.get("session_id"),
        turn_ordinal: row.get("turn_ordinal"),
        result_bytes: row.get("result_bytes"),
        result_too_large: row.get("result_too_large"),
    }
}

const OPERATION_COLUMNS: &str = "tool, key_id, expires_at, args_digest, invocation_id, state,
    (now() > deadline_at) AS past_deadline, resources, http_status, problem_type, session_id,
    turn_ordinal, result_bytes, result_too_large";

async fn lock_key(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant: &str,
    uid: &str,
    tool: &str,
    key: &str,
) -> std::result::Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('mcp-op/' || $1, 0))")
        .bind(format!("{tenant}\u{1f}{uid}\u{1f}{tool}\u{1f}{key}"))
        .execute(&mut **tx)
        .await
        .map(|_| ())
}

/// What one admission transaction did, besides committing the record.
#[derive(Debug)]
pub enum Admitted {
    /// A read tool (no key).
    Read,
    /// A keyed execution request.
    Execute(Claim),
    /// A recovery request: the stored row, if any (absent and inaccessible
    /// are told apart by the caller only internally).
    Recover(Option<Box<OperationRow>>),
}

/// The keyed half of an admission.
pub struct KeyedRequest<'a> {
    pub key: &'a OperationKey,
    /// `Some` for execution (canonical argument digest), `None` for recovery.
    pub execute: Option<(&'a str, &'a Value)>,
    pub max_validity_secs: u64,
    pub deadline_secs: u64,
}

/// One admission transaction: for an execution, atomically lock and claim the
/// key (or find it), then insert the A6 record linked to the original
/// invocation when one exists; COMMIT; wait for the acknowledgment under the
/// deadline. Only `Ok` authorizes anything.
pub async fn admit(
    state: &crate::state::AppState,
    a: &Admission,
    keyed: Option<KeyedRequest<'_>>,
) -> std::result::Result<Admitted, AdmissionFailure> {
    let Some(pool) = state.pg_pool() else {
        return Err(AdmissionFailure::NoStore);
    };
    let deadline = std::time::Duration::from_millis(state.mcp.settings.audit_deadline_ms);
    let work = async {
        #[cfg(test)]
        {
            let delay = state
                .mcp
                .faults
                .delay_ms
                .load(std::sync::atomic::Ordering::SeqCst);
            if delay > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
        }
        let mut tx = pool
            .begin()
            .await
            .map_err(|e| AdmissionFailure::Store(e.to_string()))?;
        let st = |e: sqlx::Error| AdmissionFailure::Store(e.to_string());
        let outcome = match &keyed {
            None => {
                insert_admission(&mut tx, a, None).await.map_err(st)?;
                Admitted::Read
            }
            Some(k) => {
                lock_key(&mut tx, &a.tenant_id, &a.uid, a.tool, &k.key.id)
                    .await
                    .map_err(st)?;
                let existing = sqlx::query(&format!(
                    "SELECT {OPERATION_COLUMNS} FROM mcp_operations
                      WHERE tenant_id = $1 AND uid = $2 AND tool = $3 AND key_id = $4"
                ))
                .bind(&a.tenant_id)
                .bind(&a.uid)
                .bind(a.tool)
                .bind(&k.key.id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(st)?
                .map(|r| Box::new(row_to_operation(&r)));
                match (&k.execute, existing) {
                    (Some((digest, resources)), None) => {
                        // Judge validity on the store clock, after the lock.
                        let (live, within, outlives): (bool, bool, bool) = sqlx::query_as(
                            "SELECT now() < $1,
                                    $1 <= now() + make_interval(secs => $2),
                                    $1 >= now() + make_interval(secs => $3)",
                        )
                        .bind(k.key.expires_at)
                        .bind(k.max_validity_secs as f64)
                        .bind(k.deadline_secs as f64)
                        .fetch_one(&mut *tx)
                        .await
                        .map_err(st)?;
                        let invalid = if !live {
                            Some("the operation key has expired; a client never extends a key or mints a replacement automatically")
                        } else if !within {
                            Some("the operation key's expiry is beyond the maximum validity window (MUNARIUM_MCP_KEY_MAX_VALIDITY_SECS)")
                        } else if !outlives {
                            Some("the operation key expires before the admitted work deadline (MUNARIUM_MCP_CALL_DEADLINE_SECS)")
                        } else {
                            None
                        };
                        match invalid {
                            Some(why) => {
                                insert_admission(&mut tx, a, None).await.map_err(st)?;
                                Admitted::Execute(Claim::Invalid(why))
                            }
                            None => {
                                sqlx::query(
                                    "INSERT INTO mcp_operations
                                        (tenant_id, uid, tool, key_id, expires_at, args_digest,
                                         invocation_id, state, deadline_at, resources)
                                     VALUES ($1, $2, $3, $4, $5, $6, $7, 'pending',
                                             now() + make_interval(secs => $8), $9)",
                                )
                                .bind(&a.tenant_id)
                                .bind(&a.uid)
                                .bind(a.tool)
                                .bind(&k.key.id)
                                .bind(k.key.expires_at)
                                .bind(digest)
                                .bind(&a.invocation_id)
                                .bind(k.deadline_secs as f64)
                                .bind(sqlx::types::Json(resources))
                                .execute(&mut *tx)
                                .await
                                .map_err(st)?;
                                insert_admission(&mut tx, a, None).await.map_err(st)?;
                                Admitted::Execute(Claim::Claimed)
                            }
                        }
                    }
                    (Some(_), Some(row)) => {
                        insert_admission(&mut tx, a, Some(&row.invocation_id))
                            .await
                            .map_err(st)?;
                        Admitted::Execute(Claim::Existing(row))
                    }
                    (None, row) => {
                        insert_admission(
                            &mut tx,
                            a,
                            row.as_ref().map(|r| r.invocation_id.as_str()),
                        )
                        .await
                        .map_err(st)?;
                        Admitted::Recover(row)
                    }
                }
            }
        };
        #[cfg(test)]
        if state
            .mcp
            .faults
            .fail_before_commit
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            drop(tx);
            return Err(AdmissionFailure::Store(
                "injected failure before commit".into(),
            ));
        }
        // An error from COMMIT itself leaves the outcome uncertain: the
        // commit may have happened with its acknowledgment lost.
        tx.commit().await.map_err(|_| AdmissionFailure::LostAck)?;
        #[cfg(test)]
        if state
            .mcp
            .faults
            .lose_ack
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(AdmissionFailure::LostAck);
        }
        Ok(outcome)
    };
    match tokio::time::timeout(deadline, work).await {
        Ok(r) => r,
        Err(_) => Err(AdmissionFailure::Deadline),
    }
}

/// After a refused admission whose commit may have happened (deadline or a
/// lost acknowledgment), mark a claimed key `withheld` so its receipt says the
/// work never ran. Best effort and bounded: if the store is still out the
/// key stays `pending` and reads `unknown` after its deadline — never a cue
/// to replay.
pub async fn mark_withheld(state: &crate::state::AppState, invocation_id: &str) {
    let Some(pool) = state.pg_pool() else { return };
    let _ = tokio::time::timeout(
        std::time::Duration::from_millis(state.mcp.settings.audit_deadline_ms),
        async {
            let _ = sqlx::query(
                "UPDATE mcp_operations SET state = 'withheld', outcome_at = now()
                  WHERE invocation_id = $1 AND state = 'pending'",
            )
            .bind(invocation_id)
            .execute(pool)
            .await;
            let _ = sqlx::query(
                "UPDATE mcp_invocations SET outcome = 'withheld', outcome_at = now()
                  WHERE invocation_id = $1 AND outcome IS NULL",
            )
            .bind(invocation_id)
            .execute(pool)
            .await;
        },
    )
    .await;
}

/// The instant a dispatch starts: recheck the key's expiry on the store clock
/// (the claim checked it too, but time passes between commit and dispatch),
/// and stamp `dispatched_at` on the invocation. False refuses the dispatch.
pub async fn begin_dispatch(
    state: &crate::state::AppState,
    invocation_id: &str,
    key: Option<&OperationKey>,
) -> Result<bool> {
    let Some(pool) = state.pg_pool() else {
        return Ok(false);
    };
    if let Some(k) = key {
        let live: bool = sqlx::query_scalar("SELECT now() < $1")
            .bind(k.expires_at)
            .fetch_one(pool)
            .await
            .map_err(storage)?;
        if !live {
            return Ok(false);
        }
    }
    sqlx::query("UPDATE mcp_invocations SET dispatched_at = now() WHERE invocation_id = $1")
        .bind(invocation_id)
        .execute(pool)
        .await
        .map_err(storage)?;
    Ok(true)
}

/// The known outcome of one invocation.
#[derive(Debug, Default, Clone)]
pub struct Outcome {
    /// `completed`, `refused`, `unknown`, `receipt`, `conflict` or `withheld`.
    pub outcome: &'static str,
    pub http_status: Option<i32>,
    pub problem_type: Option<String>,
    pub session_id: Option<String>,
    pub turn_ordinal: Option<i32>,
    pub result_bytes: Option<i64>,
    pub result_too_large: bool,
}

/// Record the observed outcome on the invocation and, for a keyed execution,
/// on its operation row. Best effort: a failure leaves the outcome unknown
/// (reported as such), it never re-runs anything.
pub async fn record_outcome(
    state: &crate::state::AppState,
    invocation_id: &str,
    keyed: bool,
    o: &Outcome,
) {
    let Some(pool) = state.pg_pool() else { return };
    if let Err(e) = sqlx::query(
        "UPDATE mcp_invocations SET outcome = $2, outcome_status = $3, outcome_at = now()
          WHERE invocation_id = $1",
    )
    .bind(invocation_id)
    .bind(o.outcome)
    .bind(o.http_status)
    .execute(pool)
    .await
    {
        tracing::warn!(error = %e, "mcp outcome not recorded; the invocation reads as unknown");
    }
    if keyed {
        let operation_state = match o.outcome {
            "completed" => "completed",
            "refused" => "refused",
            "withheld" => "withheld",
            _ => "unknown",
        };
        if let Err(e) = sqlx::query(
            "UPDATE mcp_operations
                SET state = $2, http_status = $3, problem_type = $4, session_id = $5,
                    turn_ordinal = $6, result_bytes = $7, result_too_large = $8,
                    outcome_at = now()
              WHERE invocation_id = $1 AND state = 'pending'",
        )
        .bind(invocation_id)
        .bind(operation_state)
        .bind(o.http_status)
        .bind(&o.problem_type)
        .bind(&o.session_id)
        .bind(o.turn_ordinal)
        .bind(o.result_bytes)
        .bind(o.result_too_large)
        .execute(pool)
        .await
        {
            tracing::warn!(error = %e, "mcp receipt not recorded; it reads as unknown after the deadline");
        }
    }
}

/// Spend observed for an original invocation: settled usage, outstanding
/// (unsettled or uncertain) reservations and the attempt count. Read-only.
pub async fn spend(
    state: &crate::state::AppState,
    tenant: &str,
    invocation_id: &str,
) -> Result<Value> {
    let Some(pool) = state.pg_pool() else {
        return Ok(json!({"attempts": 0, "settled_tokens": 0, "outstanding_reserved_tokens": 0}));
    };
    let (attempts, settled, outstanding): (i64, i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE state <> 'released'),
                COALESCE(sum(settled_units) FILTER (WHERE state = 'settled'), 0)::bigint,
                COALESCE(sum(bound_units) FILTER (WHERE state IN ('reserved', 'uncertain')), 0)::bigint
           FROM mcp_token_attempts WHERE tenant_id = $1 AND invocation_id = $2",
    )
    .bind(tenant)
    .bind(invocation_id)
    .fetch_one(pool)
    .await
    .map_err(storage)?;
    Ok(json!({
        "attempts": attempts,
        "settled_tokens": settled,
        "outstanding_reserved_tokens": outstanding,
    }))
}

/// Delete operation keys past their expiry by more than `grace_secs`, on the
/// store clock. Expiry is immutable and a late request on an expired key is
/// refused whether or not its row still exists, so pruning cannot reopen a
/// key. Audit rows and generation reservations are never pruned here.
pub async fn prune_keys(pool: &sqlx::PgPool, grace_secs: u64) -> Result<u64> {
    Ok(sqlx::query(
        "DELETE FROM mcp_operations WHERE expires_at < now() - make_interval(secs => $1)",
    )
    .bind(grace_secs as f64)
    .execute(pool)
    .await
    .map_err(storage)?
    .rows_affected())
}

/// The console's MCP activity summary (§9.3), tenant-scoped.
pub struct Activity {
    /// (tool, admitted, completed, refused, unknown-or-pending).
    pub by_tool: Vec<(String, i64, i64, i64, i64)>,
    /// The newest invocations: (invocation, tool, uid, form, admitted, outcome, status).
    pub recent: Vec<ActivityRow>,
    /// Keyed operations whose outcome is unknown (past their deadline).
    pub unknown: i64,
}

pub struct ActivityRow {
    pub invocation_id: String,
    pub tool: String,
    pub uid: String,
    pub form: String,
    pub original: Option<String>,
    pub admitted_at: chrono::DateTime<chrono::Utc>,
    pub outcome: Option<String>,
    pub status: Option<i32>,
}

pub const ACTIVITY_LIMIT: i64 = 50;

pub async fn activity(state: &crate::state::AppState, tenant: &str) -> Result<Activity> {
    let pool = crate::runbooks_api::pool(state)?;
    let by_tool: Vec<(String, i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT tool, count(*),
                count(*) FILTER (WHERE outcome = 'completed'),
                count(*) FILTER (WHERE outcome IN ('refused', 'conflict', 'withheld')),
                count(*) FILTER (WHERE outcome IS NULL OR outcome = 'unknown')
           FROM mcp_invocations
          WHERE tenant_id = $1 AND admitted_at > now() - interval '1 day'
          GROUP BY tool ORDER BY tool",
    )
    .bind(tenant)
    .fetch_all(pool)
    .await
    .map_err(storage)?;
    let recent = sqlx::query(
        "SELECT invocation_id, tool, uid, form, original_invocation_id, admitted_at, outcome,
                outcome_status
           FROM mcp_invocations WHERE tenant_id = $1
          ORDER BY admitted_at DESC, invocation_id DESC LIMIT $2",
    )
    .bind(tenant)
    .bind(ACTIVITY_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(storage)?
    .iter()
    .map(|r| ActivityRow {
        invocation_id: r.get("invocation_id"),
        tool: r.get("tool"),
        uid: r.get("uid"),
        form: r.get("form"),
        original: r.get("original_invocation_id"),
        admitted_at: r.get("admitted_at"),
        outcome: r.get("outcome"),
        status: r.get("outcome_status"),
    })
    .collect();
    let unknown: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM mcp_operations
          WHERE tenant_id = $1
            AND (state = 'unknown' OR (state = 'pending' AND now() > deadline_at))",
    )
    .bind(tenant)
    .fetch_one(pool)
    .await
    .map_err(storage)?;
    Ok(Activity {
        by_tool,
        recent,
        unknown,
    })
}
