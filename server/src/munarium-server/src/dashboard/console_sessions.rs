// SPDX-License-Identifier: Apache-2.0
//! The console sessions page (ClearView U10): the tenant's signed-in console
//! sessions and their revocation, behind the U4 review and typed
//! confirmation. The store is `crate::console_sessions`; the design is
//! `docs/design/console-sessions.md`.
//!
//! The page also renders in the default `legacy` mode: sessions left from an
//! earlier switch to `enabled` are listed there so a rollback can revoke them.

use super::{
    action, admin_auth, csrf_field, csrf_ok, error_panel, html_table, inventory::urlencode, kv,
    notice, pill, render, short, stale_form, Tone,
};
use crate::charts::esc;
use crate::console_sessions::{self as store, SessionRecord};
use crate::state::AppState;
use axum::extract::{Form, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use std::sync::Arc;

const ACTIVE: &str = "console sessions";
const LIST: &str = "/admin/console-sessions";
/// The action name the revocation plan is bound to.
const REVOKE_ACTION: &str = "console-session.revoke";

fn session_path(session_id: &str) -> String {
    format!("{LIST}/{}", urlencode(session_id))
}

fn when(t: &chrono::DateTime<chrono::Utc>) -> String {
    esc(&t.format("%Y-%m-%d %H:%M:%S UTC").to_string())
}

fn state_pill(r: &SessionRecord) -> String {
    match (&r.revoked_at, &r.revoked_reason) {
        (Some(_), Some(reason)) if reason == "sign-out" => pill("signed out", Tone::Neutral),
        (Some(_), _) => pill("revoked", Tone::Bad),
        (None, _) if r.live => pill("live", Tone::Good),
        (None, _) if r.superseded => pill("ended by fleet rollback", Tone::Neutral),
        (None, _) => pill("expired", Tone::Warn),
    }
}

/// What this replica does with console sessions, stated on every page of
/// this group: its own setting, and on PostgreSQL the fleet mode that
/// decides whether that setting is in effect (LOGIN1).
async fn mode_note(state: &AppState) -> String {
    let s = &state.console.settings;
    let fleet = crate::replica_fleet::console_fleet(state).await;
    let shared = if fleet.is_some() {
        "on every replica sharing this database"
    } else {
        "in this process (memory store: a single replica)"
    };
    let fleet_line = match &fleet {
        None => String::new(),
        Some(Ok(f)) => format!(
            " The console fleet mode is {} at epoch {}; it is changed only with munarium-server console-fleet. Sessions issued before the fleet was last switched back to legacy are never honoured again.",
            f.mode.as_str(),
            f.epoch
        ),
        Some(Err(_)) => " The console fleet mode could not be read (unavailable).".into(),
    };
    let fleet_enabled = match &fleet {
        None => true,
        Some(Ok(f)) => f.mode == crate::replica_fleet::FleetMode::Enabled,
        Some(Err(_)) => false,
    };
    let text = if s.sessions() && fleet_enabled {
        format!(
            "This replica signs operators in with revocable sessions (MUNARIUM_CONSOLE_SESSIONS=enabled and the fleet mode enabled): the browser cookie carries only an opaque handle. A session ends after {} s without a request or {} s after sign-in, whichever comes first. Signing out or revoking a session ends it {shared} from its next request. Requests with an Authorization header are unaffected.{fleet_line}",
            s.idle_secs, s.max_secs
        )
    } else if s.sessions() {
        format!(
            "This replica allows revocable sessions (MUNARIUM_CONSOLE_SESSIONS=enabled) but serves the v1.4 console login until the fleet mode is enabled: the browser cookie holds the mgmt token.{fleet_line}"
        )
    } else {
        format!(
            "This replica uses the v1.4 console login (MUNARIUM_CONSOLE_SESSIONS=legacy): the browser cookie holds the mgmt token, and no session is created or honoured here. Sessions left from an earlier switch to enabled are listed so they can be revoked; each is shown as live while its own recorded idle and absolute expiry would still let an enabled replica at the current fleet epoch honour it.{fleet_line}"
        )
    };
    format!(r#"<div class="notice">{}</div>"#, esc(&text))
}

/// `GET /admin/console-sessions` — the tenant's sessions, newest first.
pub(super) async fn list(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let admin = match admin_auth(&state, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let rows = match store::list(&state, &admin.tenant.tenant_id).await {
        Ok(rows) => rows,
        Err(e) => return error_panel(&admin, ACTIVE, "Console sessions", &e),
    };
    let shown = rows.len();
    let table = html_table(
        &[
            "session",
            "state",
            "signed in",
            "last request",
            "expires",
            "replica",
            "action",
        ],
        &rows
            .iter()
            .map(|r| {
                let mine = admin.session.as_deref() == Some(r.session_id.as_str());
                let id = if mine {
                    format!(
                        "<code>{}</code> {}",
                        esc(&r.session_id),
                        pill("this session", Tone::Neutral)
                    )
                } else {
                    format!("<code>{}</code>", esc(&r.session_id))
                };
                let act = if r.revoked_at.is_none() {
                    action(
                        &admin,
                        format!(
                            r#"<form class="action" method="get" action="{}/revoke"><button type="submit" class="danger" aria-label="review revoking console session {}">revoke…</button></form>"#,
                            esc(&session_path(&r.session_id)),
                            esc(&r.session_id),
                        ),
                    )
                } else {
                    "—".into()
                };
                vec![
                    id,
                    state_pill(r),
                    when(&r.created_at),
                    when(&r.last_seen_at),
                    when(&r.expires_at),
                    esc(&r.created_by),
                    act,
                ]
            })
            .collect::<Vec<_>>(),
    );
    let limit = if shown as i64 >= store::LIST_LIMIT {
        format!(
            r#"<p class="muted">The newest {} sessions are shown.</p>"#,
            store::LIST_LIMIT
        )
    } else {
        String::new()
    };
    let body = format!(
        r#"{}<div class="card">{table}</div>{limit}
<p class="muted">Rows are deleted seven days after their absolute expiry. Revoked and signed-out sessions stay listed until then.</p>"#,
        mode_note(&state).await
    );
    render(&admin, ACTIVE, "Console sessions", &body)
}

/// A digest of the session row the review showed. `last_seen_at` is left
/// out on purpose: every console request moves it, including the review
/// itself.
fn session_revision(r: &SessionRecord) -> String {
    use sha2::Digest as _;
    let v = serde_json::json!([
        r.session_id,
        r.tenant_id,
        r.role,
        r.created_by,
        r.created_at.to_rfc3339(),
        r.idle_secs,
        r.expires_at.to_rfc3339(),
        r.revoked_at.map(|t| t.to_rfc3339()),
    ]);
    hex::encode(sha2::Sha256::digest(v.to_string().as_bytes()))
}

fn review_title(session_id: &str) -> String {
    format!("Revoke console session {}", short(session_id, 24))
}

fn revoke_review(admin: &super::AdminCtx, r: &SessionRecord, problem: Option<&str>) -> String {
    let problem = problem
        .map(|p| format!(r#"<div class="notice">{}</div>"#, esc(p)))
        .unwrap_or_default();
    let mine = admin.session.as_deref() == Some(r.session_id.as_str());
    let details = kv(&[
        ("session", format!("<code>{}</code>", esc(&r.session_id))),
        ("state", state_pill(r)),
        ("role", esc(&r.role)),
        ("signed in", when(&r.created_at)),
        ("last request", when(&r.last_seen_at)),
        ("expires", when(&r.expires_at)),
        ("signed in on replica", esc(&r.created_by)),
        (
            "revoked",
            r.revoked_at
                .as_ref()
                .map(when)
                .unwrap_or_else(|| "—".into()),
        ),
    ]);
    let confirm = match &r.revoked_at {
        Some(at) => format!(
            r#"<div class="notice">This session was already ended at {}. There is nothing to confirm.</div>"#,
            when(at)
        ),
        None => action(
            admin,
            format!(
                r#"<form class="action confirm" method="post" action="{}/revoke/confirm">{}<input type="hidden" name="plan" value="{}">
<label>type the session id to confirm <input type="text" name="confirm" autocomplete="off" spellcheck="false" required></label>
<button type="submit" class="danger">revoke this session</button></form>"#,
                esc(&session_path(&r.session_id)),
                csrf_field(admin),
                esc(&super::action_plan(
                    admin,
                    REVOKE_ACTION,
                    &r.session_id,
                    &session_revision(r),
                )),
            ),
        ),
    };
    let yours = if mine {
        " This is the session you are using: revoking it signs you out."
    } else {
        ""
    };
    format!(
        r#"{problem}<div class="card">{details}</div>
<div class="notice">Revoking ends this session on every replica that shares the session store, from its next request; its browser must sign in again.{yours}</div>
{confirm}
<p><a href="{LIST}">← back to console sessions (nothing changes)</a></p>"#
    )
}

async fn render_review(
    state: &AppState,
    admin: &super::AdminCtx,
    session_id: &str,
    status: StatusCode,
    problem: Option<&str>,
) -> Response {
    let title = review_title(session_id);
    match store::get(state, &admin.tenant.tenant_id, session_id).await {
        Ok(r) => (
            status,
            render(admin, ACTIVE, &title, &revoke_review(admin, &r, problem)),
        )
            .into_response(),
        Err(e) => error_panel(admin, ACTIVE, &title, &e),
    }
}

/// `GET /admin/console-sessions/{session_id}/revoke` — the review. Reads
/// only (it does record the reviewing session's own use).
pub(super) async fn revoke_review_page(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let admin = match admin_auth(&state, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    render_review(&state, &admin, &session_id, StatusCode::OK, None).await
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct RevokeConfirmForm {
    #[serde(default)]
    _csrf: String,
    #[serde(default)]
    plan: String,
    #[serde(default)]
    confirm: String,
}

/// `POST /admin/console-sessions/{session_id}/revoke/confirm` — revokes,
/// after rechecking the mgmt role, view-only refusal, the CSRF token, the
/// typed session id, the target (tenant-scoped) and the plan. The write is
/// conditional on the session still being unrevoked.
pub(super) async fn revoke_confirm(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
    Form(form): Form<RevokeConfirmForm>,
) -> Response {
    let admin = match admin_auth(&state, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    if admin.view_only {
        return notice(
            &admin,
            ACTIVE,
            "Console sessions",
            "view-only passthrough — revoke on the server's own /admin",
        );
    }
    if !csrf_ok(&admin, &form._csrf) {
        return stale_form(&admin, ACTIVE, "Console sessions");
    }
    if form.confirm.trim() != session_id {
        return render_review(
            &state,
            &admin,
            &session_id,
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("The identifier you typed does not match this session's id. Nothing was revoked."),
        )
        .await;
    }
    let title = review_title(&session_id);
    let r = match store::get(&state, &admin.tenant.tenant_id, &session_id).await {
        Ok(r) => r,
        Err(e) => return error_panel(&admin, ACTIVE, &title, &e),
    };
    let conflict = |msg: &str| {
        (
            StatusCode::CONFLICT,
            render(
                &admin,
                ACTIVE,
                &title,
                &format!(
                    r#"<div class="notice">{}</div><p><a href="{}/revoke">review the session again</a> · <a href="{LIST}">back to console sessions</a></p>"#,
                    esc(msg),
                    esc(&session_path(&session_id)),
                ),
            ),
        )
            .into_response()
    };
    if r.revoked_at.is_some() {
        return conflict("This session was already ended. Nothing changed.");
    }
    if !super::plan_ok(
        &admin,
        REVOKE_ACTION,
        &session_id,
        &session_revision(&r),
        &form.plan,
    ) {
        return conflict(
            "This confirmation no longer matches the session or operator it was reviewed against (the session changed, another credential is submitting it, or the server restarted). Nothing was revoked; review it again.",
        );
    }
    match store::revoke_if_unrevoked(&state, &admin.tenant.tenant_id, &session_id).await {
        Ok(true) => Redirect::to(LIST).into_response(),
        Ok(false) => conflict("This session was ended by another request first. Nothing changed."),
        Err(e) => notice(&admin, ACTIVE, "Console sessions", &e.to_string()),
    }
}
