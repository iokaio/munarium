// SPDX-License-Identifier: Apache-2.0
//! The MCP page (ClearView §9.3): read-only agent activity and the tokens
//! that can enter the endpoint, each revocable through the existing U4
//! token-revocation review (`/admin/tokens/{jti}/revoke`, typed confirmation).
//! The page adds no action of its own; the console's management actions keep
//! their native roles. Every value from the store is escaped (U9).
//!
//! Every count shown is the signed-in tenant's. The replica's process-wide
//! MCP counters cover all tenants, so they are on the replica-scoped health
//! page instead, never here.

use super::{action, admin_auth, html_table, pill, render, store_note, Tone};
use crate::charts::esc;
use crate::state::AppState;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use std::sync::Arc;

const ACTIVE: &str = "mcp";

/// Tokens carrying `mcp`, active only, newest first.
const TOKEN_LIMIT: i64 = 100;

fn when(t: &chrono::DateTime<chrono::Utc>) -> String {
    esc(&t.format("%Y-%m-%d %H:%M:%S UTC").to_string())
}

fn outcome_pill(o: Option<&str>) -> String {
    match o {
        Some("completed") => pill("completed", Tone::Good),
        Some("receipt") => pill("receipt", Tone::Neutral),
        Some("refused") | Some("conflict") => pill(o.unwrap_or_default(), Tone::Warn),
        Some("withheld") => pill("not executed", Tone::Warn),
        Some(other) => pill(other, Tone::Bad),
        None => pill("unknown", Tone::Bad),
    }
}

/// The endpoint's state on THIS replica, stated first.
fn mode_note(state: &AppState) -> String {
    let s = &state.mcp.settings;
    let text = if s.enabled {
        format!(
            "This replica serves POST /mcp (MUNARIUM_MCP=enabled) with up to {} MCP requests in flight. Generating tools: {}. New keyed work: {}. Responses are bounded to {} bytes. Tool calls run only after a confirmed durable admission record{}.",
            state.mcp.slot_capacity,
            if s.paid_tools { "enabled under the generation policy" } else { "recovery form only (MUNARIUM_MCP_PAID_TOOLS=disabled)" },
            match s.execution {
                crate::mcp::Execution::Enabled => "enabled",
                crate::mcp::Execution::RecoveryOnly => "recovery only",
            },
            s.result_max_bytes,
            if state.pg_pool().is_some() { "" } else { "; this replica has no durable store, so every tool call is refused" },
        )
    } else {
        "This replica does not serve MCP (MUNARIUM_MCP is unset or disabled): /mcp does not exist here. Rows below were written by replicas that had it enabled.".to_string()
    };
    format!(r#"<div class="notice">{}</div>"#, esc(&text))
}

fn revocation_note(state: &AppState) -> String {
    let text = if state.config.token_revocation_check {
        "Revocation is enforced immediately: every replica checks the deny-list on each request (MUNARIUM_TOKEN_REVOCATION_CHECK=true), so a revoked token stops at its next MCP request."
    } else {
        "Revocation is recorded but NOT enforced on requests (MUNARIUM_TOKEN_REVOCATION_CHECK=false): a revoked token keeps working until it expires. Short token lifetimes bound that window."
    };
    format!(r#"<div class="notice">{}</div>"#, esc(text))
}

/// `GET /admin/mcp`.
pub(super) async fn page(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let admin = match admin_auth(&state, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let tenant = admin.tenant.tenant_id.clone();
    let activity = match crate::mcp_store::activity(&state, &tenant).await {
        Ok(a) => {
            let by_tool = html_table(
                &[
                    "tool",
                    "admitted (24 h)",
                    "completed",
                    "refused",
                    "unknown or pending",
                ],
                &a.by_tool
                    .iter()
                    .map(|(tool, n, done, refused, unknown)| {
                        vec![
                            format!("<code>{}</code>", esc(tool)),
                            esc(&n.to_string()),
                            esc(&done.to_string()),
                            esc(&refused.to_string()),
                            if *unknown > 0 {
                                pill(&unknown.to_string(), Tone::Warn)
                            } else {
                                esc("0")
                            },
                        ]
                    })
                    .collect::<Vec<_>>(),
            );
            let recent = html_table(
                &[
                    "invocation",
                    "tool",
                    "uid",
                    "form",
                    "admitted",
                    "outcome",
                    "status",
                    "recovers",
                ],
                &a.recent
                    .iter()
                    .map(|r| {
                        vec![
                            format!("<code>{}</code>", esc(&r.invocation_id)),
                            format!("<code>{}</code>", esc(&r.tool)),
                            esc(&r.uid),
                            esc(&r.form),
                            when(&r.admitted_at),
                            outcome_pill(r.outcome.as_deref()),
                            r.status
                                .map(|s| esc(&s.to_string()))
                                .unwrap_or_else(|| "—".into()),
                            r.original
                                .as_deref()
                                .map(|o| format!("<code>{}</code>", esc(o)))
                                .unwrap_or_else(|| "—".into()),
                        ]
                    })
                    .collect::<Vec<_>>(),
            );
            format!(
                r#"<div class="card"><h2>Tool calls by tool</h2>{by_tool}</div>
<div class="card"><h2>Keyed operations with an unknown outcome</h2><p>{}</p><p class="muted">An unknown outcome is never replayed. The caller recovers it with its own operation key; see the operator guide.</p></div>
<div class="card"><h2>Newest invocations</h2>{recent}<p class="muted">The newest {} admitted invocations. Arguments, results and evidence are never stored with them.</p></div>"#,
                if a.unknown > 0 {
                    pill(&a.unknown.to_string(), Tone::Warn)
                } else {
                    esc("0")
                },
                crate::mcp_store::ACTIVITY_LIMIT
            )
        }
        Err(e) => store_note(&e),
    };
    let tokens = match mcp_tokens(&state, &tenant).await {
        Ok(rows) => html_table(
            &["jti", "uid", "scopes", "runbooks", "expires", "action"],
            &rows
                .iter()
                .map(|(jti, uid, scopes, runbooks, expires)| {
                    vec![
                        format!("<code>{}</code>", esc(jti)),
                        esc(uid),
                        esc(&scopes.join(", ")),
                        runbooks
                            .as_ref()
                            .map(|r| esc(&r.join(", ")))
                            .unwrap_or_else(|| "any".into()),
                        when(expires),
                        action(
                            &admin,
                            format!(
                                r#"<form class="action" method="get" action="/admin/tokens/{}/revoke"><button type="submit" class="danger" aria-label="review revoking token {}">revoke…</button></form>"#,
                                esc(&super::path_segment(jti)),
                                esc(jti),
                            ),
                        ),
                    ]
                })
                .collect::<Vec<_>>(),
        ),
        Err(e) => store_note(&e),
    };
    let body = format!(
        r#"{}
<p class="muted">Every count on this page is this tenant's. This replica's MCP admission refusals, load shedding and dropped interaction records cover every tenant, so they are on the <a href="/admin/health">health</a> page.</p>
{activity}
<div class="card"><h2>Tokens that can enter MCP</h2>{}{tokens}<p class="muted">Unrevoked, unexpired tokens carrying the mcp scope (newest {TOKEN_LIMIT}). Revoking goes through the token review and its typed confirmation.</p></div>"#,
        mode_note(&state),
        revocation_note(&state),
    );
    render(&admin, ACTIVE, "MCP", &body)
}

#[allow(clippy::type_complexity)]
async fn mcp_tokens(
    state: &AppState,
    tenant: &str,
) -> munarium_core::Result<
    Vec<(
        String,
        String,
        Vec<String>,
        Option<Vec<String>>,
        chrono::DateTime<chrono::Utc>,
    )>,
> {
    let pool = crate::runbooks_api::pool(state)?;
    sqlx::query_as(
        "SELECT jti, uid, scopes, runbook_refs, expires_at FROM access_tokens
          WHERE tenant_id = $1 AND 'mcp' = ANY(scopes) AND revoked_at IS NULL
            AND expires_at > now()
          ORDER BY issued_at DESC LIMIT $2",
    )
    .bind(tenant)
    .bind(TOKEN_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(|e| munarium_core::KernelError::Storage(e.to_string()))
}
