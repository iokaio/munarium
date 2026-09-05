// SPDX-License-Identifier: Apache-2.0
//! management-plane reporting — the monitoring/governance surface.
//! All routes require the mgmt role; everything is tenant-scoped and reads
//! the tables the earlier milestones populate (interactions, sessions,
//! session_turns, access_tokens).

use crate::error::ApiError;
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use munarium_api_types as dto;
use munarium_core::{KernelError, Result};
use std::sync::Arc;

type ApiResult<T> = std::result::Result<T, ApiError>;

use crate::runbooks_api::pool;

fn parse_ts(s: &Option<String>, field: &str) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
    match s {
        None => Ok(None),
        Some(raw) => chrono::DateTime::parse_from_rfc3339(raw)
            .map(|t| Some(t.with_timezone(&chrono::Utc)))
            .map_err(|e| KernelError::InvalidInput(format!("{field}: {e} (RFC 3339 expected)"))),
    }
}

/// The audit keyset cursor is the previous page's `created_at` text — which
/// Postgres renders as "2026-08-17 21:05:33.123+00", not RFC 3339. Accept
/// both so a caller can round-trip `next_before` verbatim OR hand-write an
/// RFC 3339 bound; reject anything else as caller error (422, never a
/// storage 500).
fn parse_cursor(s: &Option<String>) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
    match s {
        None => Ok(None),
        Some(raw) => chrono::DateTime::parse_from_rfc3339(raw)
            .or_else(|_| chrono::DateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f%#z"))
            .map(|t| Some(t.with_timezone(&chrono::Utc)))
            .map_err(|e| {
                KernelError::InvalidInput(format!(
                    "before: {e} (pass a previous page's next_before, or RFC 3339)"
                ))
            }),
    }
}

// ---------------------------------------------------------------------------
// usage
// ---------------------------------------------------------------------------

#[derive(Debug, Default, serde::Deserialize)]
pub struct UsageQuery {
    #[serde(default)]
    group_by: Option<String>,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
}

pub async fn op_usage(
    state: &AppState,
    tenant: &str,
    group_by: &str,
    from: Option<chrono::DateTime<chrono::Utc>>,
    to: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<Vec<dto::UsageRow>> {
    // Two aggregates joined by the grouping key: interaction counts/latency
    // and turn/token spend. Each group_by has its own key expressions.
    let (ikey, tkey_join, tkey) = match group_by {
        "uid" => ("i.uid", "", "t.uid"),
        "session" => ("i.session_id", "", "t.session_id"),
        "runbook" => (
            "i.runbook_ref",
            "JOIN sessions s ON s.tenant_id = t.tenant_id AND s.id = t.session_id",
            "s.runbook_ref",
        ),
        "collection" => ("unnest_key", "", "unnest_key"),
        other => {
            return Err(KernelError::InvalidInput(format!(
                "group_by must be uid|session|runbook|collection, got '{other}'"
            )))
        }
    };

    let sql = if group_by == "collection" {
        // Collections: unnest the interaction's collection list; turns join
        // through the same unnest on session_turns.collections_searched.
        "WITH i AS (
            SELECT unnest(collection_ids) AS key, latency_ms
              FROM interactions
             WHERE tenant_id = $1 AND collection_ids IS NOT NULL
               AND ($2::timestamptz IS NULL OR created_at >= $2)
               AND ($3::timestamptz IS NULL OR created_at <  $3)
         ), t AS (
            SELECT unnest(collections_searched) AS key,
                   COALESCE((completion->>'input_tokens')::bigint, 0)  AS itok,
                   COALESCE((completion->>'output_tokens')::bigint, 0) AS otok
              FROM session_turns
             WHERE tenant_id = $1
               AND ($2::timestamptz IS NULL OR created_at >= $2)
               AND ($3::timestamptz IS NULL OR created_at <  $3)
         )
         SELECT COALESCE(i.key, t.key) AS key,
                COALESCE(i.interactions, 0) AS interactions,
                COALESCE(t.turns, 0) AS turns,
                COALESCE(t.itok, 0) AS itok, COALESCE(t.otok, 0) AS otok,
                i.avg_latency
           FROM (SELECT key, count(*) AS interactions, avg(latency_ms)::float8 AS avg_latency
                   FROM i GROUP BY key) i
           FULL OUTER JOIN
                (SELECT key, count(*) AS turns, sum(itok)::bigint AS itok, sum(otok)::bigint AS otok
                   FROM t GROUP BY key) t USING (key)
          ORDER BY interactions DESC NULLS LAST, key LIMIT 500"
            .to_string()
    } else {
        format!(
            "SELECT COALESCE(i.key, t.key) AS key,
                    COALESCE(i.interactions, 0) AS interactions,
                    COALESCE(t.turns, 0) AS turns,
                    COALESCE(t.itok, 0) AS itok, COALESCE(t.otok, 0) AS otok,
                    i.avg_latency
               FROM (SELECT {ikey} AS key, count(*) AS interactions,
                            avg(i.latency_ms)::float8 AS avg_latency
                       FROM interactions i
                      WHERE i.tenant_id = $1 AND {ikey} IS NOT NULL
                        AND ($2::timestamptz IS NULL OR i.created_at >= $2)
                        AND ($3::timestamptz IS NULL OR i.created_at <  $3)
                      GROUP BY {ikey}) i
               FULL OUTER JOIN
                    (SELECT {tkey} AS key, count(*) AS turns,
                            sum(COALESCE((t.completion->>'input_tokens')::bigint, 0))::bigint  AS itok,
                            sum(COALESCE((t.completion->>'output_tokens')::bigint, 0))::bigint AS otok
                       FROM session_turns t {tkey_join}
                      WHERE t.tenant_id = $1
                        AND ($2::timestamptz IS NULL OR t.created_at >= $2)
                        AND ($3::timestamptz IS NULL OR t.created_at <  $3)
                      GROUP BY {tkey}) t USING (key)
              ORDER BY interactions DESC NULLS LAST, key LIMIT 500"
        )
    };

    let rows: Vec<(String, i64, i64, i64, i64, Option<f64>)> = sqlx::query_as(&sql)
        .bind(tenant)
        .bind(from)
        .bind(to)
        .fetch_all(pool(state)?)
        .await
        .map_err(|e| KernelError::Storage(e.to_string()))?;
    Ok(rows
        .into_iter()
        .map(
            |(key, interactions, turns, itok, otok, avg_latency)| dto::UsageRow {
                key,
                interactions,
                turns,
                completion_input_tokens: itok,
                completion_output_tokens: otok,
                avg_latency_ms: avg_latency,
            },
        )
        .collect())
}

/// GET /v1/reports/usage
#[utoipa::path(get, path = "/v1/reports/usage",
    params(("group_by" = Option<String>, Query, description = "uid | session | runbook | collection (default uid)"),
           ("from" = Option<String>, Query, description = "RFC 3339 inclusive lower bound"),
           ("to" = Option<String>, Query, description = "RFC 3339 exclusive upper bound")),
    responses((status = 200, body = dto::UsageResponse)), tag = "reports")]
pub async fn usage(
    State(state): State<Arc<AppState>>,
    Query(q): Query<UsageQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::UsageResponse>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    let group_by = q.group_by.as_deref().unwrap_or("uid").to_string();
    let from = parse_ts(&q.from, "from")?;
    let to = parse_ts(&q.to, "to")?;
    let rows = op_usage(&state, &ctx.tenant_id, &group_by, from, to).await?;
    Ok(Json(dto::UsageResponse {
        group_by,
        from: q.from,
        to: q.to,
        rows,
    }))
}

// ---------------------------------------------------------------------------
// audit
// ---------------------------------------------------------------------------

#[derive(Debug, Default, serde::Deserialize)]
pub struct AuditQuery {
    #[serde(default)]
    uid: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    runbook: Option<String>,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
    /// Include the captured request/response bodies (off by default —
    /// envelope-only pages are much lighter).
    #[serde(default)]
    bodies: bool,
    /// Keyset cursor: only entries strictly older than this timestamp.
    /// Pass the previous page's `next_before` verbatim.
    #[serde(default)]
    before: Option<String>,
}

/// The audit read's parameters — the REST query and the /admin audit page
/// (2026-08-27) both build one of these, so the trail has ONE query.
#[derive(Debug, Default)]
pub struct AuditFilter {
    pub uid: Option<String>,
    pub session_id: Option<String>,
    pub runbook: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub limit: Option<i64>,
    pub bodies: bool,
    pub before: Option<String>,
}

impl From<AuditQuery> for AuditFilter {
    fn from(q: AuditQuery) -> Self {
        Self {
            uid: q.uid,
            session_id: q.session_id,
            runbook: q.runbook,
            from: q.from,
            to: q.to,
            limit: q.limit,
            bodies: q.bodies,
            before: q.before,
        }
    }
}

/// GET /v1/reports/audit — the interaction trail, newest first.
#[utoipa::path(get, path = "/v1/reports/audit",
    params(("uid" = Option<String>, Query), ("session_id" = Option<String>, Query),
           ("runbook" = Option<String>, Query), ("from" = Option<String>, Query),
           ("to" = Option<String>, Query), ("limit" = Option<i64>, Query, description = "default 100, max 1000"),
           ("bodies" = Option<bool>, Query, description = "include captured request/response bodies"),
           ("before" = Option<String>, Query, description = "keyset cursor: entries strictly older than this timestamp (pass the previous page's next_before)")),
    responses((status = 200, body = dto::AuditResponse)), tag = "reports")]
pub async fn audit(
    State(state): State<Arc<AppState>>,
    Query(q): Query<AuditQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::AuditResponse>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    Ok(Json(
        op_audit(&state, &ctx.tenant_id, &AuditFilter::from(q)).await?,
    ))
}

/// The interaction-trail read shared by the REST view and the dashboard.
#[allow(clippy::type_complexity)]
pub async fn op_audit(
    state: &AppState,
    tenant: &str,
    q: &AuditFilter,
) -> Result<dto::AuditResponse> {
    let from = parse_ts(&q.from, "from")?;
    let to = parse_ts(&q.to, "to")?;
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);
    let rows: Vec<(
        String,
        String,
        Option<String>,
        Option<String>,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<i32>,
        Option<i32>,
        Option<serde_json::Value>,
        Option<serde_json::Value>,
        String,
    )> = sqlx::query_as(
        "SELECT id, uid, session_id, request_id, plane, method, runbook_ref,
                token_jti, status, latency_ms, request, response, created_at::text
           FROM interactions
          WHERE tenant_id = $1
            AND ($2::text IS NULL OR uid = $2)
            AND ($3::text IS NULL OR session_id = $3)
            AND ($4::text IS NULL OR runbook_ref = $4)
            AND ($5::timestamptz IS NULL OR created_at >= $5)
            AND ($6::timestamptz IS NULL OR created_at <  $6)
            AND ($8::timestamptz IS NULL OR created_at <  $8)
          ORDER BY created_at DESC LIMIT $7",
    )
    .bind(tenant)
    .bind(&q.uid)
    .bind(&q.session_id)
    .bind(&q.runbook)
    .bind(from)
    .bind(to)
    .bind(limit)
    .bind(parse_cursor(&q.before)?)
    .fetch_all(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    let entries: Vec<dto::AuditEntryDto> = rows
        .into_iter()
        .map(
            |(
                id,
                uid,
                session_id,
                request_id,
                plane,
                method,
                runbook_ref,
                token_jti,
                status,
                latency_ms,
                request,
                response,
                created_at,
            )| dto::AuditEntryDto {
                id,
                uid,
                session_id,
                request_id,
                plane,
                method,
                runbook_ref,
                token_jti,
                status,
                latency_ms,
                request: q.bodies.then_some(request).flatten(),
                response: q.bodies.then_some(response).flatten(),
                created_at,
            },
        )
        .collect();
    // A full page implies more may exist; the last row's created_at is the
    // cursor for the next (older) page.
    let next_before = (entries.len() as i64 == limit)
        .then(|| entries.last().map(|e| e.created_at.clone()))
        .flatten();
    Ok(dto::AuditResponse {
        entries,
        next_before,
    })
}

// ---------------------------------------------------------------------------
// cost (model-spend rollup)
// ---------------------------------------------------------------------------

#[derive(Debug, Default, serde::Deserialize)]
pub struct CostQuery {
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
}

pub async fn op_cost(
    state: &AppState,
    tenant: &str,
    from: Option<chrono::DateTime<chrono::Utc>>,
    to: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<Vec<dto::CostRow>> {
    let rows: Vec<(String, String, i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT completion->>'provider' AS provider,
                completion->>'model' AS model,
                count(*) AS turns,
                count(*) FILTER (WHERE (completion->'resolved'->>'was_override')::boolean) AS overridden,
                sum(COALESCE((completion->>'input_tokens')::bigint, 0))::bigint  AS itok,
                sum(COALESCE((completion->>'output_tokens')::bigint, 0))::bigint AS otok
           FROM session_turns
          WHERE tenant_id = $1 AND completion IS NOT NULL
            AND ($2::timestamptz IS NULL OR created_at >= $2)
            AND ($3::timestamptz IS NULL OR created_at <  $3)
          GROUP BY 1, 2
          -- ORDER BY cannot use `itok + otok`: Postgres resolves output
          -- aliases in ORDER BY only as bare column references, never inside
          -- expressions (found live 2026-08-12 — the alias form 500s).
          ORDER BY sum(COALESCE((completion->>'input_tokens')::bigint, 0))
                 + sum(COALESCE((completion->>'output_tokens')::bigint, 0)) DESC",
    )
    .bind(tenant)
    .bind(from)
    .bind(to)
    .fetch_all(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    Ok(rows
        .into_iter()
        .map(
            |(provider, model, turns, overridden, itok, otok)| dto::CostRow {
                provider,
                model,
                turns,
                overridden_turns: overridden,
                input_tokens: itok,
                output_tokens: otok,
            },
        )
        .collect())
}

/// GET /v1/reports/cost — completion token spend per resolved provider/model,
/// native vs overridden. Dollar pricing is the platform's concern; the server
/// reports the token facts.
#[utoipa::path(get, path = "/v1/reports/cost",
    params(("from" = Option<String>, Query), ("to" = Option<String>, Query)),
    responses((status = 200, body = dto::CostResponse)), tag = "reports")]
pub async fn cost(
    State(state): State<Arc<AppState>>,
    Query(q): Query<CostQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::CostResponse>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    let from = parse_ts(&q.from, "from")?;
    let to = parse_ts(&q.to, "to")?;
    let rows = op_cost(&state, &ctx.tenant_id, from, to).await?;
    Ok(Json(dto::CostResponse {
        from: q.from,
        to: q.to,
        rows,
    }))
}

// ---------------------------------------------------------------------------
// budgets (spending caps)
// ---------------------------------------------------------------------------

/// Today's spending-cap ledger joined with the configured ceilings. Usage is
/// read through the store's own window expression — the report and the
/// enforcer must never disagree about which day it is — and a configured cap
/// with no traffic yet still gets a zero row, because "0 of 1,000,000" is
/// the row an operator looks for.
pub async fn op_budgets(state: &AppState, tenant: &str) -> Result<Vec<dto::BudgetRow>> {
    let ledger = state.budgets().ledger(tenant).await?;
    let mut limits: std::collections::HashMap<(String, String), u64> =
        std::collections::HashMap::new();
    for entry in state.providers.list(state, tenant).await? {
        let caps = &entry.doc.spec.budgets.daily_tokens;
        for tier in munarium_providers::ModelTier::ALL {
            if let Some(limit) = caps.for_tier(tier) {
                limits.insert(
                    (entry.doc.metadata.name.clone(), tier.as_str().to_string()),
                    limit,
                );
            }
        }
    }
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let mut rows: Vec<dto::BudgetRow> = Vec::new();
    for row in ledger {
        let key = (row.config.clone(), row.tier.clone());
        let limit = limits.get(&key).copied();
        seen.insert(key);
        let used = row.held_units + row.settled_units;
        rows.push(dto::BudgetRow {
            config: row.config,
            tier: row.tier,
            day: row.day,
            held_tokens: row.held_units as i64,
            settled_tokens: row.settled_units as i64,
            reservations: row.reservations as i64,
            limit: limit.map(|l| l as i64),
            remaining: limit.map(|l| l.saturating_sub(used) as i64),
        });
    }
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    for ((config, tier), limit) in &limits {
        if !seen.contains(&(config.clone(), tier.clone())) {
            rows.push(dto::BudgetRow {
                config: config.clone(),
                tier: tier.clone(),
                day: today.clone(),
                held_tokens: 0,
                settled_tokens: 0,
                reservations: 0,
                limit: Some(*limit as i64),
                remaining: Some(*limit as i64),
            });
        }
    }
    rows.sort_by(|a, b| (&a.config, &a.tier).cmp(&(&b.config, &b.tier)));
    Ok(rows)
}

/// GET /v1/reports/budgets — today's spending-cap ledger per provider config
/// × tier beside each scope's configured daily ceiling. Token facts only,
/// like /v1/reports/cost: dollar pricing stays the platform's concern.
#[utoipa::path(get, path = "/v1/reports/budgets",
    responses((status = 200, body = dto::BudgetReportResponse)), tag = "reports")]
pub async fn budgets(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::BudgetReportResponse>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    let rows = op_budgets(&state, &ctx.tenant_id).await?;
    Ok(Json(dto::BudgetReportResponse { rows }))
}

/// Recent runbook runs for the dashboard timeline (SQL stays in this module;
/// dashboard/runbooks.rs renders only).
pub async fn op_recent_runs(
    state: &AppState,
    tenant: &str,
    limit: i64,
) -> Result<Vec<(String, String, String, String)>> {
    sqlx::query_as(
        "SELECT id, runbook_ref, state, created_at::text
           FROM runbook_runs
          WHERE tenant_id = $1
          ORDER BY created_at DESC LIMIT $2",
    )
    .bind(tenant)
    .bind(limit)
    .fetch_all(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))
}

/// One persisted gate finding for the /admin findings page (2026-08-27).
pub struct FindingRow {
    pub version_id: String,
    pub seq: i64,
    pub rule_id: String,
    pub severity: String,
    pub message: String,
    pub scope_path: Option<String>,
    pub recorded_at: String,
}

/// The tenant's most recent gate findings across every lineage — the
/// cross-version view the per-version `GET /v1/versions/{id}/findings` read
/// cannot give. Newest first; `severity` filters to one of info|warn|block.
#[allow(clippy::type_complexity)]
pub async fn op_recent_findings(
    state: &AppState,
    tenant: &str,
    severity: Option<&str>,
    rule_prefix: Option<&str>,
    limit: i64,
) -> Result<Vec<FindingRow>> {
    // The prefix is a plain string prefix, escaped for LIKE so an underscore
    // in a rule id matches itself. Kept as a literal `||` concatenation so
    // the planner can use the index on rule_id when one exists.
    let prefix = rule_prefix.map(|p| {
        p.replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    });
    let rows: Vec<(String, i64, String, String, String, Option<String>, String)> = sqlx::query_as(
        "SELECT version_id, seq, rule_id, severity, message, scope_path, recorded_at::text
           FROM gate_findings
          WHERE tenant_id = $1 AND ($2::text IS NULL OR severity = $2)
            AND ($4::text IS NULL OR rule_id LIKE $4 || '%')
          ORDER BY recorded_at DESC, seq DESC LIMIT $3",
    )
    .bind(tenant)
    .bind(severity)
    .bind(limit)
    .bind(prefix)
    .fetch_all(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    Ok(rows
        .into_iter()
        .map(
            |(version_id, seq, rule_id, severity, message, scope_path, recorded_at)| FindingRow {
                version_id,
                seq,
                rule_id,
                severity,
                message,
                scope_path,
                recorded_at,
            },
        )
        .collect())
}

/// The control-plane inventory on the /admin overview (2026-08-27): one
/// round trip of scalar subqueries over the tenant's tables. Shapes are
/// counted from the in-process registry by the caller (they live there on
/// both stores).
#[derive(Debug, Default, Clone, Copy)]
pub struct ControlPlaneCounts {
    pub runbooks_active: i64,
    pub runbooks_total: i64,
    pub collections_active: i64,
    pub sessions_open: i64,
    pub tokens_active: i64,
    pub runs_awaiting_approval: i64,
    pub runs_running: i64,
    pub findings_block_24h: i64,
}

pub async fn op_control_plane_counts(state: &AppState, tenant: &str) -> Result<ControlPlaneCounts> {
    let row: (i64, i64, i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM runbooks WHERE tenant_id = $1 AND status = 'active'),
                (SELECT count(*) FROM runbooks WHERE tenant_id = $1),
                (SELECT count(*) FROM collections WHERE tenant_id = $1 AND status = 'active'),
                (SELECT count(*) FROM sessions WHERE tenant_id = $1 AND state = 'open'),
                (SELECT count(*) FROM access_tokens
                  WHERE tenant_id = $1 AND revoked_at IS NULL AND expires_at > now()),
                (SELECT count(*) FROM runbook_runs WHERE tenant_id = $1 AND state = 'awaiting_approval'),
                (SELECT count(*) FROM runbook_runs WHERE tenant_id = $1 AND state = 'running'),
                (SELECT count(*) FROM gate_findings
                  WHERE tenant_id = $1 AND severity = 'block'
                    AND recorded_at >= now() - interval '24 hours')",
    )
    .bind(tenant)
    .fetch_one(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    Ok(ControlPlaneCounts {
        runbooks_active: row.0,
        runbooks_total: row.1,
        collections_active: row.2,
        sessions_open: row.3,
        tokens_active: row.4,
        runs_awaiting_approval: row.5,
        runs_running: row.6,
        findings_block_24h: row.7,
    })
}

// ---------------------------------------------------------------------------
// dashboard views (2026-08-17): timeseries / endpoints / runbooks / sessions.
// Ad-hoc aggregates over the audit tables at request time — no rollup
// tables. Fine at demo scale; the rollup threshold to watch is ~5M
// interaction rows or a dashboard p95 over ~500ms, at which point a
// usage-rollup migration (bucketed pre-aggregation) earns its keep.
// ---------------------------------------------------------------------------

/// window=1h|24h|7d|30d → (seconds, bucket_seconds). The bucket widths keep
/// every window at 60–120 points — enough for a chart, bounded for a query.
fn parse_window(window: &str) -> Result<(i64, i64)> {
    match window {
        "1h" => Ok((3_600, 60)),
        "24h" => Ok((86_400, 900)),
        "7d" => Ok((604_800, 7_200)),
        "30d" => Ok((2_592_000, 21_600)),
        other => Err(KernelError::InvalidInput(format!(
            "window must be 1h|24h|7d|30d, got '{other}'"
        ))),
    }
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct TimeseriesQuery {
    #[serde(default)]
    window: Option<String>,
    #[serde(default)]
    plane: Option<String>,
}

#[allow(clippy::type_complexity)]
pub async fn op_timeseries(
    state: &AppState,
    tenant: &str,
    window: &str,
    plane: Option<&str>,
) -> Result<dto::TimeseriesResponse> {
    let (window_secs, bucket_secs) = parse_window(window)?;
    if let Some(p) = plane {
        if p != "rest" && p != "grpc" {
            return Err(KernelError::InvalidInput(format!(
                "plane must be rest|grpc, got '{p}'"
            )));
        }
    }
    let rows: Vec<(String, i64, i64, i64, Option<f64>, Option<f64>)> = sqlx::query_as(
        "SELECT to_char(date_bin(make_interval(secs => $2), created_at, 'epoch'),
                        'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS bucket,
                count(*) AS requests,
                count(*) FILTER (WHERE status >= 400 AND status < 500) AS e4,
                count(*) FILTER (WHERE status >= 500) AS e5,
                percentile_cont(0.5)  WITHIN GROUP (ORDER BY latency_ms)::float8 AS p50,
                percentile_cont(0.95) WITHIN GROUP (ORDER BY latency_ms)::float8 AS p95
           FROM interactions
          WHERE tenant_id = $1
            AND created_at >= now() - make_interval(secs => $3)
            AND ($4::text IS NULL OR plane = $4)
          GROUP BY 1 ORDER BY 1",
    )
    .bind(tenant)
    .bind(bucket_secs as f64)
    .bind(window_secs as f64)
    .bind(plane)
    .fetch_all(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    Ok(dto::TimeseriesResponse {
        window: window.to_string(),
        bucket_seconds: bucket_secs,
        plane: plane.map(String::from),
        buckets: rows
            .into_iter()
            .map(
                |(bucket, requests, e4, e5, p50, p95)| dto::TimeseriesBucket {
                    bucket,
                    requests,
                    errors_4xx: e4,
                    errors_5xx: e5,
                    p50_latency_ms: p50,
                    p95_latency_ms: p95,
                },
            )
            .collect(),
    })
}

/// GET /v1/reports/timeseries — bucketed traffic/error/latency series.
#[utoipa::path(get, path = "/v1/reports/timeseries",
    params(("window" = Option<String>, Query, description = "1h | 24h | 7d | 30d (default 24h)"),
           ("plane" = Option<String>, Query, description = "rest | grpc (default both)")),
    responses((status = 200, body = dto::TimeseriesResponse)), tag = "reports")]
pub async fn timeseries(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TimeseriesQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::TimeseriesResponse>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    let window = q.window.as_deref().unwrap_or("24h");
    Ok(Json(
        op_timeseries(&state, &ctx.tenant_id, window, q.plane.as_deref()).await?,
    ))
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct EndpointsQuery {
    #[serde(default)]
    window: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

#[allow(clippy::type_complexity)]
pub async fn op_endpoints(
    state: &AppState,
    tenant: &str,
    window: &str,
    limit: i64,
) -> Result<dto::EndpointsResponse> {
    let (window_secs, _) = parse_window(window)?;
    let rows: Vec<(String, i64, f64, Option<f64>, Option<f64>)> = sqlx::query_as(
        "SELECT method, count(*) AS requests,
                (count(*) FILTER (WHERE status >= 400))::float8 / count(*)::float8 AS error_rate,
                avg(latency_ms)::float8 AS avg_ms,
                percentile_cont(0.95) WITHIN GROUP (ORDER BY latency_ms)::float8 AS p95
           FROM interactions
          WHERE tenant_id = $1
            AND created_at >= now() - make_interval(secs => $2)
          GROUP BY method ORDER BY requests DESC LIMIT $3",
    )
    .bind(tenant)
    .bind(window_secs as f64)
    .bind(limit)
    .fetch_all(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    Ok(dto::EndpointsResponse {
        window: window.to_string(),
        rows: rows
            .into_iter()
            .map(
                |(method, requests, error_rate, avg, p95)| dto::EndpointRow {
                    method,
                    requests,
                    error_rate,
                    avg_latency_ms: avg,
                    p95_latency_ms: p95,
                },
            )
            .collect(),
    })
}

/// GET /v1/reports/endpoints — top endpoints by volume, with error rate and
/// latency (the slow-endpoint view is the same rows sorted client-side).
#[utoipa::path(get, path = "/v1/reports/endpoints",
    params(("window" = Option<String>, Query, description = "1h | 24h | 7d | 30d (default 24h)"),
           ("limit" = Option<i64>, Query, description = "default 20, max 200")),
    responses((status = 200, body = dto::EndpointsResponse)), tag = "reports")]
pub async fn endpoints(
    State(state): State<Arc<AppState>>,
    Query(q): Query<EndpointsQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::EndpointsResponse>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    let window = q.window.as_deref().unwrap_or("24h");
    let limit = q.limit.unwrap_or(20).clamp(1, 200);
    Ok(Json(
        op_endpoints(&state, &ctx.tenant_id, window, limit).await?,
    ))
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct WindowQuery {
    #[serde(default)]
    window: Option<String>,
}

pub async fn op_runbook_report(
    state: &AppState,
    tenant: &str,
    window: &str,
) -> Result<dto::RunbookReportResponse> {
    let (window_secs, _) = parse_window(window)?;
    let runs: Vec<(String, i64, Option<f64>)> = sqlx::query_as(
        "SELECT r.state, count(*) AS runs,
                avg(extract(epoch FROM (s.last_update - r.created_at)) * 1000)::float8 AS wall_ms
           FROM runbook_runs r
           LEFT JOIN (SELECT tenant_id, run_id, max(updated_at) AS last_update
                        FROM runbook_steps GROUP BY 1, 2) s
             ON s.tenant_id = r.tenant_id AND s.run_id = r.id
          WHERE r.tenant_id = $1
            AND r.created_at >= now() - make_interval(secs => $2)
          GROUP BY r.state ORDER BY r.state",
    )
    .bind(tenant)
    .bind(window_secs as f64)
    .fetch_all(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    let steps: Vec<(String, i64)> = sqlx::query_as(
        "SELECT st.state, count(*) AS steps
           FROM runbook_steps st
           JOIN runbook_runs r ON r.tenant_id = st.tenant_id AND r.id = st.run_id
          WHERE st.tenant_id = $1
            AND r.created_at >= now() - make_interval(secs => $2)
          GROUP BY st.state ORDER BY st.state",
    )
    .bind(tenant)
    .bind(window_secs as f64)
    .fetch_all(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    Ok(dto::RunbookReportResponse {
        window: window.to_string(),
        runs: runs
            .into_iter()
            .map(|(state, runs, wall)| dto::RunbookRunsRow {
                state,
                runs,
                avg_wall_ms: wall,
            })
            .collect(),
        steps: steps
            .into_iter()
            .map(|(state, steps)| dto::RunbookStepsRow { state, steps })
            .collect(),
    })
}

/// GET /v1/reports/runbooks — run/step state breakdown with wall time.
#[utoipa::path(get, path = "/v1/reports/runbooks",
    params(("window" = Option<String>, Query, description = "1h | 24h | 7d | 30d (default 7d)")),
    responses((status = 200, body = dto::RunbookReportResponse)), tag = "reports")]
pub async fn runbook_report(
    State(state): State<Arc<AppState>>,
    Query(q): Query<WindowQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::RunbookReportResponse>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    let window = q.window.as_deref().unwrap_or("7d");
    Ok(Json(
        op_runbook_report(&state, &ctx.tenant_id, window).await?,
    ))
}

pub async fn op_sessions_report(
    state: &AppState,
    tenant: &str,
    window: &str,
) -> Result<dto::SessionsReportResponse> {
    let (window_secs, bucket_secs) = parse_window(window)?;
    let rows: Vec<(String, i64, i64, i64)> = sqlx::query_as(
        "SELECT COALESCE(s.bucket, t.bucket) AS bucket,
                COALESCE(s.opened, 0) AS opened,
                COALESCE(t.turns, 0) AS turns,
                COALESCE(t.uids, 0) AS uids
           FROM (SELECT to_char(date_bin(make_interval(secs => $2), created_at, 'epoch'),
                                'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS bucket,
                        count(*) AS opened
                   FROM sessions
                  WHERE tenant_id = $1
                    AND created_at >= now() - make_interval(secs => $3)
                  GROUP BY 1) s
           FULL OUTER JOIN
                (SELECT to_char(date_bin(make_interval(secs => $2), created_at, 'epoch'),
                                'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS bucket,
                        count(*) AS turns, count(DISTINCT uid) AS uids
                   FROM session_turns
                  WHERE tenant_id = $1
                    AND created_at >= now() - make_interval(secs => $3)
                  GROUP BY 1) t USING (bucket)
          ORDER BY bucket",
    )
    .bind(tenant)
    .bind(bucket_secs as f64)
    .bind(window_secs as f64)
    .fetch_all(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    Ok(dto::SessionsReportResponse {
        window: window.to_string(),
        bucket_seconds: bucket_secs,
        buckets: rows
            .into_iter()
            .map(|(bucket, opened, turns, uids)| dto::SessionsBucket {
                bucket,
                sessions_opened: opened,
                turns,
                active_uids: uids,
            })
            .collect(),
    })
}

/// GET /v1/reports/sessions — bucketed session/turn activity.
#[utoipa::path(get, path = "/v1/reports/sessions",
    params(("window" = Option<String>, Query, description = "1h | 24h | 7d | 30d (default 24h)")),
    responses((status = 200, body = dto::SessionsReportResponse)), tag = "reports")]
pub async fn sessions_report(
    State(state): State<Arc<AppState>>,
    Query(q): Query<WindowQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::SessionsReportResponse>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    let window = q.window.as_deref().unwrap_or("24h");
    Ok(Json(
        op_sessions_report(&state, &ctx.tenant_id, window).await?,
    ))
}

// ---------------------------------------------------------------------------
// token audit + revoke
// ---------------------------------------------------------------------------

#[derive(Debug, Default, serde::Deserialize)]
pub struct TokensQuery {
    #[serde(default)]
    uid: Option<String>,
    #[serde(default)]
    active: Option<bool>,
}

/// GET /v1/access-tokens — the issuance audit (never token material).
#[utoipa::path(get, path = "/v1/access-tokens",
    params(("uid" = Option<String>, Query),
           ("active" = Option<bool>, Query, description = "true = unexpired + unrevoked only")),
    responses((status = 200, body = dto::TokensResponse)), tag = "access-tokens")]
#[allow(clippy::type_complexity)]
pub async fn list_tokens(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TokensQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::TokensResponse>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    Ok(Json(dto::TokensResponse {
        tokens: op_list_tokens(
            &state,
            &ctx.tenant_id,
            q.uid.as_deref(),
            q.active.unwrap_or(false),
        )
        .await?,
    }))
}

/// The issuance-audit read, shared by both planes (never token material).
#[allow(clippy::type_complexity)]
pub async fn op_list_tokens(
    state: &AppState,
    tenant: &str,
    uid: Option<&str>,
    active: bool,
) -> std::result::Result<Vec<dto::TokenInfoDto>, KernelError> {
    let rows: Vec<(
        String,
        String,
        i32,
        Vec<String>,
        Vec<String>,
        Option<Vec<String>>,
        String,
        String,
        String,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT jti, uid, access_level, compartments, scopes, runbook_refs,
                issued_by, issued_at::text, expires_at::text, revoked_at::text
           FROM access_tokens
          WHERE tenant_id = $1
            AND ($2::text IS NULL OR uid = $2)
            AND (NOT $3 OR (revoked_at IS NULL AND expires_at > now()))
          ORDER BY issued_at DESC LIMIT 500",
    )
    .bind(tenant)
    .bind(uid)
    .bind(active)
    .fetch_all(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    Ok(rows
        .into_iter()
        .map(
            |(jti, uid, lvl, cmp, scopes, rb, issued_by, issued_at, expires_at, revoked_at)| {
                dto::TokenInfoDto {
                    jti,
                    uid,
                    access_level: lvl,
                    compartments: cmp,
                    scopes,
                    runbook_refs: rb,
                    issued_by,
                    issued_at,
                    expires_at,
                    revoked_at,
                }
            },
        )
        .collect())
}

/// POST /v1/access-tokens/{jti}/revoke — deny-list entry; enforced at verify
/// time only when MUNARIUM_TOKEN_REVOCATION_CHECK=true (the response says which).
#[utoipa::path(post, path = "/v1/access-tokens/{jti}/revoke",
    params(("jti" = String, Path)),
    responses((status = 200, body = dto::RevokeTokenResponse),
              (status = 404, description = "unknown jti")), tag = "access-tokens")]
pub async fn revoke_token(
    State(state): State<Arc<AppState>>,
    Path(jti): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::RevokeTokenResponse>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    Ok(Json(op_revoke_token(&state, &ctx.tenant_id, jti).await?))
}

/// The deny-list write, shared by both planes.
pub async fn op_revoke_token(
    state: &AppState,
    tenant: &str,
    jti: String,
) -> std::result::Result<dto::RevokeTokenResponse, KernelError> {
    let updated = sqlx::query(
        "UPDATE access_tokens SET revoked_at = COALESCE(revoked_at, now())
          WHERE tenant_id = $1 AND jti = $2",
    )
    .bind(tenant)
    .bind(&jti)
    .execute(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    if updated.rows_affected() == 0 {
        return Err(KernelError::NotFound {
            kind: "token",
            id: jti,
        });
    }
    Ok(dto::RevokeTokenResponse {
        jti,
        revoked: true,
        revocation_check_enabled: state.config.token_revocation_check,
    })
}

// ---------------------------------------------------------------------------
// The evidence hierarchy and the Matrix plane
// ---------------------------------------------------------------------------

/// How the evidence hierarchy behaved over the window.
///
/// Reads `session_turns.hierarchy`, which the turn loop persists per turn. The
/// operational question is "which layer is quietly refusing?" — a layer that
/// refuses on most turns is misconfigured or pointed at something down, and
/// either way the answers being served are thinner than the runbook claims
/// while every one of those turns still returns 200.
pub async fn op_evidence_report(
    state: &AppState,
    tenant: &str,
    window: &str,
) -> Result<dto::EvidenceReportResponse> {
    let (window_secs, _) = parse_window(window)?;
    let rows: Vec<(Option<serde_json::Value>,)> = sqlx::query_as(
        "SELECT hierarchy FROM session_turns
          WHERE tenant_id = $1 AND created_at >= now() - make_interval(secs => $2)",
    )
    .bind(tenant)
    .bind(window_secs as f64)
    .fetch_all(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;

    let mut hierarchy_turns = 0i64;
    let mut legacy_turns = 0i64;
    let mut completeness_available = 0i64;
    // (profile, layer) -> (turns, refusals, complete, codes, durations)
    type Acc = (i64, i64, i64, Vec<String>, Vec<i64>);
    let mut by_layer: std::collections::BTreeMap<(String, String), Acc> = Default::default();

    for (raw,) in rows {
        let Some(value) = raw else {
            legacy_turns += 1;
            continue;
        };
        let Ok(d) = serde_json::from_value::<dto::EvidenceHierarchyDecisionDto>(value) else {
            // A row we cannot parse is counted as a hierarchy turn but
            // contributes no layer stats. Silently dropping it would make the
            // totals disagree with the table.
            hierarchy_turns += 1;
            continue;
        };
        hierarchy_turns += 1;
        if d.completeness_available {
            completeness_available += 1;
        }
        for l in d.layers {
            let e = by_layer
                .entry((d.profile.clone(), l.layer.clone()))
                .or_default();
            e.0 += 1;
            if l.block == "refusal" {
                e.1 += 1;
                if let Some(code) = l.refusal_code {
                    e.3.push(code);
                }
            }
            if l.supports_completeness {
                e.2 += 1;
            }
            e.4.push(l.elapsed_ms as i64);
        }
    }

    let layers = by_layer
        .into_iter()
        .map(
            |((profile, layer), (turns, refusals, complete, codes, mut ms))| {
                ms.sort_unstable();
                // Most frequent first, so the operator reads the dominant cause
                // rather than whichever code happened to land first.
                let mut freq: std::collections::BTreeMap<String, usize> = Default::default();
                for c in codes {
                    *freq.entry(c).or_default() += 1;
                }
                let mut refusal_codes: Vec<(String, usize)> = freq.into_iter().collect();
                refusal_codes.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                dto::EvidenceLayerStatsDto {
                    profile,
                    layer,
                    turns,
                    refusals,
                    complete,
                    refusal_codes: refusal_codes.into_iter().map(|(c, _)| c).collect(),
                    p50_ms: percentile(&ms, 0.50),
                    p95_ms: percentile(&ms, 0.95),
                }
            },
        )
        .collect();

    Ok(dto::EvidenceReportResponse {
        window: window.to_string(),
        hierarchy_turns,
        legacy_turns,
        completeness_available,
        layers,
    })
}

/// Nearest-rank percentile. Empty is 0 — no observations is not a latency.
fn percentile(sorted: &[i64], p: f64) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = ((p * sorted.len() as f64).ceil() as usize).max(1);
    sorted[rank.min(sorted.len()) - 1]
}

#[utoipa::path(get, path = "/v1/reports/evidence",
    params(("window" = Option<String>, Query, description = "24h (default), 7d, 30d")),
    responses((status = 200, body = dto::EvidenceReportResponse)), tag = "reports")]
pub async fn evidence_report(
    State(state): State<Arc<AppState>>,
    Query(q): Query<WindowQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::EvidenceReportResponse>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    let window = q.window.as_deref().unwrap_or("24h");
    Ok(Json(
        op_evidence_report(&state, &ctx.tenant_id, window).await?,
    ))
}

/// The Matrix plane as this server sees it.
pub async fn op_matrix_report(state: &AppState, tenant: &str) -> Result<dto::MatrixReportResponse> {
    let mut data_views = Vec::new();
    // `runbook_ref` is already `name@version` and the column is `yaml`, not
    // `body`. Guessed wrong the first time; the live conformance run is what
    // said so (2026-08-29) — a report nobody executes is a 500 waiting for an
    // operator to find during an incident.
    //
    // Removed runbooks are excluded: a data view on a runbook nobody can use
    // is not part of this deployment's surface.
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT runbook_ref, yaml
           FROM runbooks
          WHERE tenant_id = $1 AND status = 'active'
          ORDER BY runbook_ref",
    )
    .bind(tenant)
    .fetch_all(pool(state)?)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    for (runbook_ref, body) in rows {
        let doc = match munarium_runbooks::parse_runbook(&body) {
            Ok(doc) => doc,
            // A stored runbook that no longer parses (a grammar change since
            // it was applied) must not simply vanish from the report.
            Err(e) => {
                tracing::warn!(runbook_ref = %runbook_ref, error = %e, "stored runbook does not parse; omitted from the matrix report");
                continue;
            }
        };
        for v in doc.spec.data_views {
            data_views.push(dto::MatrixDataViewDto {
                runbook_ref: runbook_ref.clone(),
                name: v.name,
                contract: v.contract,
                access_level: v.access_level,
            });
        }
    }
    Ok(dto::MatrixReportResponse {
        // Not configured and configured-but-failing must not read the same.
        configured: state.config.matrix_base_url.is_some(),
        circuit_open: state.matrix_breaker.is_open(),
        consecutive_failures: state.matrix_breaker.consecutive_failures(),
        data_views,
    })
}

#[utoipa::path(get, path = "/v1/reports/matrix",
    responses((status = 200, body = dto::MatrixReportResponse)), tag = "reports")]
pub async fn matrix_report(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::MatrixReportResponse>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    Ok(Json(op_matrix_report(&state, &ctx.tenant_id).await?))
}

#[cfg(test)]
mod s35_tests {
    use super::*;

    #[test]
    fn nearest_rank_percentiles_are_exact_at_the_edges() {
        let v = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        assert_eq!(percentile(&v, 0.50), 5);
        assert_eq!(percentile(&v, 0.95), 10);
        assert_eq!(percentile(&[42], 0.95), 42);
        // No observations is not a latency of anything.
        assert_eq!(percentile(&[], 0.50), 0);
    }
}
