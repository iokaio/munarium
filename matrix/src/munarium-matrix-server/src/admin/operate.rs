// SPDX-License-Identifier: Apache-2.0
//! The actions.
//!
//! Every handler here is a thin shell around the SAME `op_*` function `/v1`
//! calls. Not a copy of it, and not a privileged in-process shortcut past it:
//! the identical function, with a `Caller` built from the rw token the
//! operator typed into the form, and `via: admin-ui` so the journal row says
//! which plane it came from. That is what makes "there is no second policy"
//! true by construction rather than by review — a gate added to `/v1`
//! tomorrow applies here without anyone remembering to add it.
//!
//! Each action follows the same four beats: confirm the write is allowed
//! (view-only, Origin, CSRF), resolve the rw credential, run the operation,
//! then show what happened **including the journal id**, so an operator can
//! find the row their click produced.

use super::{admin_auth, chrome, render, rw_credential, writable};
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use std::sync::Arc;

#[derive(Debug, serde::Deserialize)]
pub struct ActionForm {
    #[serde(default)]
    pub csrf: String,
    #[serde(default)]
    pub rw_token: String,
    #[serde(default)]
    pub decision_id: String,
    #[serde(default)]
    pub reason: String,
}

/// The shared preamble. Returns either an authorized rw caller or the page to
/// render instead.
macro_rules! prepare {
    ($state:expr, $headers:expr, $form:expr, $active:expr, $title:expr) => {{
        let admin = match admin_auth(&$state, &$headers) {
            Ok(a) => a,
            Err(r) => return r,
        };
        if let Err(msg) = writable(&admin, &$headers, &$form.csrf) {
            return super::notice(&admin, $active, $title, &msg);
        }
        match rw_credential(&$state, &admin, &$form.rw_token) {
            Ok(rw) => (admin, rw),
            Err(e) => return super::notice(&admin, $active, $title, &e),
        }
    }};
}

fn outcome_page(
    admin: &super::AdminCtx,
    active: &str,
    title: &str,
    back: &str,
    panel: String,
) -> Response {
    let body = format!(
        r#"{panel}<div class="legend">{} · the journal row for this action carries <code>via: admin-ui</code> and your rw principal as its actor.</div>"#,
        chrome::link(back, "back")
    );
    render(admin, active, title, &body)
}

pub async fn probe(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    axum::extract::Form(form): axum::extract::Form<ActionForm>,
) -> Response {
    let (admin, rw) = prepare!(state, headers, form, "sources", "probe");
    let panel =
        match crate::rest::op_probe(&state, &rw, &name, None, crate::rest::VIA_ADMIN_UI).await {
            // A refusal is an ANSWER to a probe, not an error — "unreachable, and
            // here is the typed reason" is what was asked. The detail is the
            // adapter's own message and never a connection string: the validator
            // refuses an inline secret in the asset, and the credential is
            // resolved by reference at call time.
            Ok(p) => chrome::kv(&[
                (
                    "reachable",
                    chrome::state_badge(if p.reachable { "ok" } else { "unreachable" }),
                ),
                (
                    "latency",
                    p.latency_ms
                        .map(|m| format!("{m} ms"))
                        .unwrap_or_else(|| "—".into()),
                ),
                ("breaker", chrome::esc(&p.breaker)),
                ("detail", chrome::opt(p.detail.as_deref())),
            ]),
            Err(e) => format!(r#"<div class="notice">{}</div>"#, chrome::esc(&e.0.detail)),
        };
    outcome_page(
        &admin,
        "sources",
        &format!("probe {name}"),
        &format!("/admin/sources/{name}"),
        panel,
    )
}

pub async fn introspect(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    axum::extract::Form(form): axum::extract::Form<ActionForm>,
) -> Response {
    let (admin, rw) = prepare!(state, headers, form, "sources", "introspect");
    let panel =
        match crate::rest::op_introspect(&state, &rw, &name, None, crate::rest::VIA_ADMIN_UI).await
        {
            Ok(r) => {
                let posture: Vec<Vec<String>> = r
                    .posture
                    .checks
                    .iter()
                    .map(|c| {
                        vec![
                            chrome::state_badge(if c.ok { "ok" } else { "failed" }),
                            chrome::esc(&c.name),
                            format!("required {} / observed {}", c.required, c.observed),
                            chrome::opt(c.detail.as_deref()),
                        ]
                    })
                    .collect();
                let mut schema = Vec::new();
                for t in &r.tables {
                    for c in &t.columns {
                        schema.push(vec![
                            chrome::esc(&t.name),
                            // Row security is reported as present or ABSENT, never
                            // omitted: a missing field reads as "not checked", and
                            // "this table has no row security" is a fact an
                            // operator needs stated.
                            chrome::state_badge(if t.row_security_enabled { "ok" } else { "idle" }),
                            chrome::esc(&c.name),
                            chrome::esc(&c.source_type),
                            c.logical_type
                                .as_deref()
                                .map(chrome::esc)
                                .unwrap_or_else(|| {
                                    r#"<span class="note">no canon@1 type — unusable</span>"#.into()
                                }),
                            if c.nullable {
                                "null".into()
                            } else {
                                String::new()
                            },
                        ]);
                    }
                }
                format!(
                    "{}{}<h2>schema</h2>{}",
                    chrome::kv(&[
                        ("principal", chrome::esc(&r.posture.principal)),
                        (
                            "posture",
                            chrome::state_badge(if r.posture.ok { "ok" } else { "failed" })
                        ),
                        ("fingerprint", chrome::opt(r.schema_fingerprint.as_deref())),
                    ]),
                    chrome::table(&["", "check", "", "detail"], &posture),
                    chrome::table(
                        &["table", "rls", "column", "source type", "canon@1", ""],
                        &schema
                    )
                )
            }
            Err(e) => format!(r#"<div class="notice">{}</div>"#, chrome::esc(&e.0.detail)),
        };
    outcome_page(
        &admin,
        "sources",
        &format!("introspect {name}"),
        &format!("/admin/sources/{name}"),
        panel,
    )
}

pub async fn sync(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    axum::extract::Form(form): axum::extract::Form<ActionForm>,
) -> Response {
    let (admin, rw) = prepare!(state, headers, form, "sources", "sync");
    let panel =
        match crate::rest::op_enqueue_sync(&state, &rw, &name, None, crate::rest::VIA_ADMIN_UI)
            .await
        {
            // Enqueued, not run. A sync takes minutes and belongs to the sync
            // role's queue; the console hands back the job ids and points at the
            // page that watches them, rather than pretending the click finished
            // the work.
            Ok(j) => format!(
                "{}<div class=\"legend\">{} — watch them on {}</div>",
                chrome::kv(&[
                    ("accepted", j.accepted.to_string()),
                    ("jobs", chrome::esc(&j.jobs.join(", "))),
                ]),
                chrome::esc(&j.detail),
                chrome::link("/admin/runs", "runs")
            ),
            Err(e) => format!(r#"<div class="notice">{}</div>"#, chrome::esc(&e.0.detail)),
        };
    outcome_page(
        &admin,
        "sources",
        &format!("sync {name}"),
        &format!("/admin/sources/{name}"),
        panel,
    )
}

pub async fn verify(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    axum::extract::Form(form): axum::extract::Form<ActionForm>,
) -> Response {
    let (admin, rw) = prepare!(state, headers, form, "verification", "verify");
    // Verification is a query-plane act. On a control-only container the
    // console renders this as a note rather than a button, but a hand-crafted
    // POST would still land here — so the refusal names the role rather than
    // running work this container is not supposed to do.
    if !state.role().serves_query() {
        return super::notice(
            &admin,
            "verification",
            "verify",
            "verification runs on the query role; this container serves the control plane",
        );
    }
    let panel =
        match crate::rest::op_verify_contract(&state, &rw, &name, None, crate::rest::VIA_ADMIN_UI)
            .await
        {
            Ok(v) => {
                let rows: Vec<Vec<String>> = v
                    .questions
                    .iter()
                    .map(|q| {
                        vec![
                            chrome::state_badge(if q.ok { "ok" } else { "failed" }),
                            chrome::esc(&q.question),
                            q.rows.map(|r| r.to_string()).unwrap_or_default(),
                            chrome::esc(&q.failures.join("; ")),
                        ]
                    })
                    .collect();
                format!(
                    "{}{}",
                    chrome::kv(&[
                        ("contract", chrome::esc(&v.contract)),
                        ("passed", v.passed.to_string()),
                        (
                            "failed",
                            format!(
                                "{} {}",
                                v.failed,
                                chrome::state_badge(if v.failed == 0 { "ok" } else { "failed" })
                            )
                        ),
                    ]),
                    chrome::table(&["", "question", "rows", "failures"], &rows)
                )
            }
            Err(e) => format!(r#"<div class="notice">{}</div>"#, chrome::esc(&e.0.detail)),
        };
    outcome_page(
        &admin,
        "verification",
        &format!("verify {name}"),
        "/admin/verification",
        panel,
    )
}

pub async fn run_mapping(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    axum::extract::Form(form): axum::extract::Form<ActionForm>,
) -> Response {
    let (admin, rw) = prepare!(state, headers, form, "mappings", "run");
    let panel =
        match crate::rest::op_enqueue_mapping(&state, &rw, &name, None, crate::rest::VIA_ADMIN_UI)
            .await
        {
            Ok(j) => chrome::kv(&[
                ("accepted", j.accepted.to_string()),
                ("jobs", chrome::esc(&j.jobs.join(", "))),
                ("detail", chrome::esc(&j.detail)),
            ]),
            Err(e) => format!(r#"<div class="notice">{}</div>"#, chrome::esc(&e.0.detail)),
        };
    outcome_page(
        &admin,
        "mappings",
        &format!("run {name}"),
        &format!("/admin/mappings/{name}"),
        panel,
    )
}

pub async fn promote(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    axum::extract::Form(form): axum::extract::Form<ActionForm>,
) -> Response {
    let (admin, rw) = prepare!(state, headers, form, "mappings", "promote");
    let req = munarium_matrix_types::dto::PromoteRequest {
        decision_id: form.decision_id.trim().to_string(),
        reason: (!form.reason.trim().is_empty()).then(|| form.reason.trim().to_string()),
        actor: None,
    };
    // Every gate lives inside `op_promote`, checked at the moment of the
    // decision against the latest completed run. This page cannot promote past
    // one, and a refusal arrives naming the gate and the numbers.
    let panel =
        match crate::rest::op_promote(&state, &rw, &name, &req, None, crate::rest::VIA_ADMIN_UI)
            .await
        {
            Ok(s) => chrome::kv(&[
                (
                    "state",
                    chrome::state_badge(if s.promoted { "promoted" } else { "shadow" }),
                ),
                ("decision", chrome::opt(s.decision_id.as_deref())),
                ("since", chrome::opt(s.promoted_at.as_deref())),
            ]),
            Err(e) => format!(r#"<div class="notice">{}</div>"#, chrome::esc(&e.0.detail)),
        };
    outcome_page(
        &admin,
        "mappings",
        &format!("promote {name}"),
        &format!("/admin/mappings/{name}"),
        panel,
    )
}

pub async fn demote(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    axum::extract::Form(form): axum::extract::Form<ActionForm>,
) -> Response {
    let (admin, rw) = prepare!(state, headers, form, "mappings", "demote");
    // A demotion takes a decision id and nothing else: the API has no reason
    // field here, and inventing one on the form would promise a record the
    // system does not keep.
    let req = munarium_matrix_types::dto::DecisionRequest {
        decision_id: form.decision_id.trim().to_string(),
    };
    let panel = match crate::rest::op_demote(
        &state,
        &rw,
        &name,
        &req,
        None,
        crate::rest::VIA_ADMIN_UI,
    )
    .await
    {
        // Demotion stops future writes and touches nothing already proposed —
        // that is what rollback is for, and saying so here keeps an operator
        // from believing the ledger was reverted by this click.
        Ok(s) => format!(
            "{}<div class=\"legend\">Writes stop at the next reconcile poll. Claims this mapping has already proposed are untouched; superseding them is <code>mxctl mappings rollback</code>.</div>",
            chrome::kv(&[(
                "state",
                chrome::state_badge(if s.promoted { "promoted" } else { "shadow" })
            )])
        ),
        Err(e) => format!(r#"<div class="notice">{}</div>"#, chrome::esc(&e.0.detail)),
    };
    outcome_page(
        &admin,
        "mappings",
        &format!("demote {name}"),
        &format!("/admin/mappings/{name}"),
        panel,
    )
}
