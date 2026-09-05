// SPDX-License-Identifier: Apache-2.0
//! The control-plane inventory pages (2026-08-27): providers, collections
//! (+ bulk uploads), sessions (+ turn-by-turn detail), capability tokens
//! (list / issue / revoke — the management-plane actions), the audit trail,
//! and the persisted gate findings. Reads come from the api modules' op_*
//! functions; the two token actions call the same op functions the /v1
//! twins do and are mgmt-role by construction.

use super::{
    action, admin_auth, csrf_field, csrf_ok, error_panel, html_table, json_block, kv, link, notice,
    opt, pre, render, severity_badge, short, stale_form, store_note, window_of, CsrfOnlyForm,
};
use crate::charts::{self, esc};
use crate::reports_api;
use crate::state::AppState;
use axum::extract::{Form, Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use munarium_api_types as dto;
use std::sync::Arc;

fn list_or_dash(items: &[String]) -> String {
    if items.is_empty() {
        "—".into()
    } else {
        esc(&items.join(", "))
    }
}

// ---------------------------------------------------------------------------
// providers
// ---------------------------------------------------------------------------

pub(super) async fn providers(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let tenant = admin.tenant.tenant_id.as_str();
    // Applied configs + env-backed defaults: free introspection, zero
    // provider calls, credentialRef never echoed — only whether it resolves.
    let configs = match crate::providers_api::op_list_providers(&state, tenant).await {
        Ok(list) => html_table(
            &[
                "config",
                "family",
                "source",
                "credential",
                "fast tier",
                "capable tier",
                "frontier tier",
            ],
            &list
                .iter()
                .map(|p| {
                    vec![
                        format!("<code>{}</code>", esc(&p.name)),
                        esc(&p.provider),
                        esc(&p.source),
                        if p.credential_ok {
                            r#"<span class="swatch" style="background:var(--good)"></span> resolves"#.into()
                        } else {
                            r#"<span class="swatch" style="background:var(--critical)"></span> missing"#.into()
                        },
                        opt(&p.fast),
                        opt(&p.capable),
                        opt(&p.frontier),
                    ]
                })
                .collect::<Vec<_>>(),
        ),
        Err(e) => store_note(&e),
    };
    let spend = match reports_api::op_cost(&state, tenant, None, None).await {
        Ok(rows) => {
            let bars = charts::hbar_rows(
                &rows
                    .iter()
                    .map(|r| {
                        (
                            format!("{}/{}", r.provider, r.model),
                            (r.input_tokens + r.output_tokens) as f64,
                            format!("in {} / out {}", r.input_tokens, r.output_tokens),
                        )
                    })
                    .collect::<Vec<_>>(),
            );
            let table = charts::data_table(
                &[
                    "provider",
                    "model",
                    "turns",
                    "overridden",
                    "in tokens",
                    "out tokens",
                ],
                &rows
                    .iter()
                    .map(|r| {
                        vec![
                            r.provider.clone(),
                            r.model.clone(),
                            r.turns.to_string(),
                            r.overridden_turns.to_string(),
                            r.input_tokens.to_string(),
                            r.output_tokens.to_string(),
                        ]
                    })
                    .collect::<Vec<_>>(),
            );
            format!("{bars}{table}")
        }
        Err(e) => store_note(&e),
    };
    let body = format!(
        r#"<h2>configured providers</h2><div class="card">{configs}</div>
<div class="legend">source: applied = a tenant ProviderConfig (POST /v1/providers); default = synthesized from the family's MUNARIUM_SECRET_* env var. The reserved config name <code>default</code> resolves anthropic → openai → openrouter, first usable credential wins.</div>
<h2>completion token spend by provider/model (all time)</h2><div class="card">{spend}</div>
<div class="notice">Token facts only — dollar pricing lives upstream. Live provider probes are <a href="/healthai">/healthai</a> (six paid completions per call; a diagnostic, not a monitor).</div>"#
    );
    render(&admin, "providers", "Providers", &body)
}

// ---------------------------------------------------------------------------
// collections
// ---------------------------------------------------------------------------

fn collection_row(c: &dto::CollectionDto) -> Vec<String> {
    vec![
        link(&format!("/admin/collections/{}", c.id), &c.name),
        format!("<code>{}</code>", esc(&c.id)),
        link(&format!("/admin/shapes/{}", c.shape_ref), &c.shape_ref),
        c.access_level.to_string(),
        list_or_dash(&c.compartments),
        esc(&c.status),
        c.source_count.to_string(),
        c.active_index
            .as_deref()
            .map(|i| format!("<code>{}</code>", esc(&short(i, 16))))
            .unwrap_or_else(|| "none".into()),
        esc(&c.created_at),
    ]
}

const COLLECTION_HEADERS: &[&str] = &[
    "collection",
    "id",
    "shape",
    "level",
    "compartments",
    "status",
    "sources",
    "active index",
    "created",
];

pub(super) async fn collections(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let tenant = admin.tenant.tenant_id.as_str();
    let table = match crate::collections_api::op_list_collections(&state, tenant).await {
        Ok(list) => html_table(
            COLLECTION_HEADERS,
            &list.iter().map(collection_row).collect::<Vec<_>>(),
        ),
        Err(e) => store_note(&e),
    };
    let bulk = match crate::ingest_api::op_recent_bulk_uploads(&state, tenant, 20).await {
        Ok(rows) => html_table(
            &[
                "bulk session",
                "label",
                "status",
                "declared",
                "stored",
                "failed",
                "pending",
                "opened by",
                "opened",
                "expires",
                "completed",
            ],
            &rows
                .iter()
                .map(|b| {
                    vec![
                        format!("<code>{}</code>", esc(&b.bulk_id)),
                        opt(&b.label),
                        esc(&b.status),
                        b.total.to_string(),
                        b.stored.to_string(),
                        b.failed.to_string(),
                        b.pending.to_string(),
                        esc(&b.created_by),
                        esc(&b.created_at),
                        esc(&b.expires_at),
                        opt(&b.completed_at),
                    ]
                })
                .collect::<Vec<_>>(),
        ),
        Err(e) => store_note(&e),
    };
    let body = format!(
        r#"<h2>collections</h2><div class="card">{table}</div>
<div class="legend">level = the access level a capability token must dominate; compartments = the need-to-know tags it must carry (all of them). Physical index data is never deleted by any API — retirement is soft.</div>
<h2>recent bulk upload sessions</h2><div class="card">{bulk}</div>"#
    );
    render(&admin, "collections", "Collections", &body)
}

pub(super) async fn collection(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let tenant = admin.tenant.tenant_id.as_str();
    let c = match crate::collections_api::op_get_collection(&state, tenant, &id).await {
        Ok(c) => c,
        Err(e) => return error_panel(&admin, "collections", "Collection", &e),
    };
    let indexes = match crate::collections_api::op_collection_indexes(&state, tenant, &c.id).await {
        Ok(rows) => html_table(
            &[
                "index version",
                "active",
                "watermark seq",
                "built",
                "manifest",
            ],
            &rows
                .iter()
                .map(|r| {
                    vec![
                        format!("<code>{}</code>", esc(&r.id)),
                        if r.active {
                            r#"<span class="swatch" style="background:var(--good)"></span> active"#
                                .into()
                        } else {
                            "—".into()
                        },
                        r.watermark_seq.to_string(),
                        esc(&r.built_at),
                        format!(
                            "<details><summary>manifest</summary>{}</details>",
                            json_block(&r.manifest)
                        ),
                    ]
                })
                .collect::<Vec<_>>(),
        ),
        Err(e) => store_note(&e),
    };
    let runbooks = match crate::runbooks_api::op_list_runbooks(&state, tenant, true).await {
        Ok(list) => {
            let refs: Vec<String> = list
                .iter()
                .filter(|r| r.collections.iter().any(|rc| rc.name == c.name))
                .map(|r| {
                    link(
                        &format!("/admin/runbooks/{}", r.runbook_ref),
                        &r.runbook_ref,
                    )
                })
                .collect();
            if refs.is_empty() {
                "<div class=\"empty\">no runbook declares this collection</div>".into()
            } else {
                refs.join(" · ")
            }
        }
        Err(e) => store_note(&e),
    };
    let body = format!(
        r#"<div class="card">{}</div>
<h2>index versions</h2><div class="card">{indexes}</div>
<h2>runbooks reaching this collection</h2><div class="card">{runbooks}</div>"#,
        kv(&[
            ("collection", format!("<code>{}</code>", esc(&c.id))),
            ("name", esc(&c.name)),
            (
                "shape",
                link(&format!("/admin/shapes/{}", c.shape_ref), &c.shape_ref)
            ),
            ("access level", c.access_level.to_string()),
            ("compartments", list_or_dash(&c.compartments)),
            ("status", esc(&c.status)),
            ("description", opt(&c.description)),
            ("bound sources", c.source_count.to_string()),
            (
                "active index",
                c.active_index
                    .as_deref()
                    .map(|i| format!("<code>{}</code>", esc(i)))
                    .unwrap_or_else(|| "none — searches find nothing until a cutover".into()),
            ),
            ("created", esc(&c.created_at)),
        ])
    );
    render(
        &admin,
        "collections",
        &format!("Collection {}", c.name),
        &body,
    )
}

// ---------------------------------------------------------------------------
// sessions
// ---------------------------------------------------------------------------

#[derive(Debug, Default, serde::Deserialize)]
pub struct SessionsParams {
    #[serde(default)]
    window: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

pub(super) async fn sessions(
    State(state): State<Arc<AppState>>,
    Query(q): Query<SessionsParams>,
    headers: HeaderMap,
) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let tenant = admin.tenant.tenant_id.as_str();
    let window_param = super::WindowParam {
        window: q.window.clone(),
        group_by: None,
    };
    let window = window_of(&window_param);
    let state_filter = q.state.as_deref().filter(|s| !s.is_empty() && *s != "all");
    let chart = match reports_api::op_sessions_report(&state, tenant, window).await {
        Ok(report) => {
            let labels: Vec<String> = report.buckets.iter().map(|b| b.bucket.clone()).collect();
            let chart = charts::line_chart(
                &labels,
                report.bucket_seconds,
                &[
                    charts::Series {
                        name: "turns",
                        color: "var(--s1)",
                        points: report
                            .buckets
                            .iter()
                            .map(|b| Some(b.turns as f64))
                            .collect(),
                    },
                    charts::Series {
                        name: "sessions opened",
                        color: "var(--s2)",
                        points: report
                            .buckets
                            .iter()
                            .map(|b| Some(b.sessions_opened as f64))
                            .collect(),
                    },
                    charts::Series {
                        name: "active uids",
                        color: "var(--s3)",
                        points: report
                            .buckets
                            .iter()
                            .map(|b| Some(b.active_uids as f64))
                            .collect(),
                    },
                ],
            );
            let table = charts::data_table(
                &["bucket", "sessions opened", "turns", "active uids"],
                &report
                    .buckets
                    .iter()
                    .map(|b| {
                        vec![
                            b.bucket.clone(),
                            b.sessions_opened.to_string(),
                            b.turns.to_string(),
                            b.active_uids.to_string(),
                        ]
                    })
                    .collect::<Vec<_>>(),
            );
            format!(
                "{}<div class=\"card\">{chart}{table}</div>",
                super::window_picker("/admin/sessions", window)
            )
        }
        Err(e) => store_note(&e),
    };
    // `window` is caller input: it reaches the report as a validated enum
    // but reaches THIS markup verbatim, so it is escaped like any other
    // user-derived string.
    let window_attr = esc(window);
    let picker: String = ["all", "open", "closed", "expired"]
        .iter()
        .map(|s| {
            if *s == state_filter.unwrap_or("all") {
                format!("<strong>{s}</strong>")
            } else {
                format!(r#"<a href="/admin/sessions?state={s}&amp;window={window_attr}">{s}</a>"#)
            }
        })
        .collect::<Vec<_>>()
        .join(" · ");
    let table =
        match crate::sessions_api::op_recent_sessions(&state, tenant, state_filter, 50).await {
            Ok(rows) => html_table(
                &[
                    "session",
                    "uid",
                    "runbook",
                    "state",
                    "level",
                    "compartments",
                    "turns",
                    "created",
                    "last turn",
                ],
                &rows
                    .iter()
                    .map(|s| {
                        vec![
                            link(&format!("/admin/sessions/{}", s.id), &s.id),
                            esc(&s.uid),
                            link(
                                &format!("/admin/runbooks/{}", s.runbook_ref),
                                &s.runbook_ref,
                            ),
                            esc(&s.state),
                            s.access_level.to_string(),
                            list_or_dash(&s.compartments),
                            s.turns.to_string(),
                            esc(&s.created_at),
                            opt(&s.last_turn_at),
                        ]
                    })
                    .collect::<Vec<_>>(),
            ),
            Err(e) => store_note(&e),
        };
    let body = format!(
        r#"<h2>session activity</h2>{chart}
<h2>recent sessions</h2><div class="legend">state: {picker}</div><div class="card">{table}</div>"#
    );
    render(&admin, "sessions", "Sessions", &body)
}

pub(super) async fn session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let s = match crate::sessions_api::op_get_session(&state, &admin.tenant.tenant_id, &id).await {
        Ok(s) => s,
        Err(e) => return error_panel(&admin, "sessions", "Session", &e),
    };
    let mut turns = String::new();
    for t in &s.turns {
        let hits = t.hits.as_array().map(|a| a.len()).unwrap_or(0);
        let completion = match &t.completion {
            Some(c) => {
                let mut rows: Vec<(&str, String)> = Vec::new();
                let mut answer = String::new();
                if let Some(obj) = c.as_object() {
                    for (k, v) in obj {
                        if k == "text" {
                            answer = v
                                .as_str()
                                .map(String::from)
                                .unwrap_or_else(|| v.to_string());
                            continue;
                        }
                        let shown = match v {
                            serde_json::Value::String(s) => esc(s),
                            other => esc(&other.to_string()),
                        };
                        rows.push((k.as_str(), shown));
                    }
                } else {
                    rows.push(("completion", esc(&c.to_string())));
                }
                let answer_html = if answer.is_empty() {
                    String::new()
                } else {
                    format!(
                        "<details open><summary>answer</summary>{}</details>",
                        pre(&answer)
                    )
                };
                format!("{}{answer_html}", kv(&rows))
            }
            None => "<div class=\"empty\">retrieval-only turn (no completion)</div>".into(),
        };
        turns.push_str(&format!(
            r#"<h3>turn {} <span class="legend">({})</span></h3><div class="card">{}<details><summary>{hits} hits</summary>{}</details><h4>completion</h4>{completion}</div>"#,
            t.ordinal,
            esc(&t.created_at),
            kv(&[
                ("query", esc(&t.query)),
                ("collections searched", list_or_dash(&t.collections_searched)),
            ]),
            json_block(&t.hits),
        ));
    }
    if turns.is_empty() {
        turns = "<div class=\"empty\">no turns yet</div>".into();
    }
    let body = format!(
        r#"<div class="card">{}</div><h2>turns</h2>{turns}"#,
        kv(&[
            ("session", format!("<code>{}</code>", esc(&s.session_id))),
            ("uid", esc(&s.uid)),
            (
                "runbook (pinned)",
                link(
                    &format!("/admin/runbooks/{}", s.runbook_ref),
                    &s.runbook_ref
                )
            ),
            ("state", esc(&s.state)),
            ("access level (snapshot)", s.access_level.to_string()),
            ("compartments (snapshot)", list_or_dash(&s.compartments)),
            ("created", esc(&s.created_at)),
            ("turns", s.turns.len().to_string()),
        ])
    );
    render(
        &admin,
        "sessions",
        &format!("Session {}", s.session_id),
        &body,
    )
}

// ---------------------------------------------------------------------------
// tokens — the management-plane actions
// ---------------------------------------------------------------------------

#[derive(Debug, Default, serde::Deserialize)]
pub struct TokensParams {
    #[serde(default)]
    uid: Option<String>,
    /// Present = include expired and revoked tokens.
    #[serde(default)]
    all: Option<String>,
}

fn issue_form(admin: &super::AdminCtx) -> String {
    action(
        admin,
        format!(
            r#"<form class="action" method="post" action="/admin/tokens/issue">{}
<label>uid <input type="text" name="uid" required placeholder="alice"></label>
<label>access level <input type="number" name="access_level" value="0" min="0" style="width:70px"></label>
<label>compartments <input type="text" name="compartments" placeholder="finance,legal"></label>
<label><input type="checkbox" name="scope_query" value="1" checked> query</label>
<label><input type="checkbox" name="scope_ingest" value="1"> ingest</label>
<label>runbooks <input type="text" name="runbook_refs" placeholder="name allowlist, comma-separated"></label>
<label>ttl s <input type="number" name="ttl_secs" min="1" style="width:90px" placeholder="default"></label>
<button type="submit">issue token</button></form>"#,
            csrf_field(admin)
        ),
    )
}

pub(super) async fn tokens(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TokensParams>,
    headers: HeaderMap,
) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let tenant = admin.tenant.tenant_id.as_str();
    let uid = q.uid.as_deref().filter(|u| !u.is_empty());
    let active_only = q.all.is_none();
    let list = match reports_api::op_list_tokens(&state, tenant, uid, active_only).await {
        Ok(rows) => html_table(
            &[
                "jti",
                "uid",
                "level",
                "compartments",
                "scopes",
                "runbooks",
                "issued by",
                "issued",
                "expires",
                "revoked",
                "action",
            ],
            &rows
                .iter()
                .map(|t| {
                    let revoke = if t.revoked_at.is_none() {
                        action(
                            &admin,
                            format!(
                                r#"<form class="action" method="post" action="/admin/tokens/{}/revoke">{}<button type="submit" class="danger">revoke</button></form>"#,
                                esc(&t.jti),
                                csrf_field(&admin)
                            ),
                        )
                    } else {
                        "—".into()
                    };
                    vec![
                        format!("<code>{}</code>", esc(&t.jti)),
                        esc(&t.uid),
                        t.access_level.to_string(),
                        list_or_dash(&t.compartments),
                        list_or_dash(&t.scopes),
                        t.runbook_refs
                            .as_ref()
                            .map(|r| list_or_dash(r))
                            .unwrap_or_else(|| "any".into()),
                        esc(&t.issued_by),
                        esc(&t.issued_at),
                        esc(&t.expires_at),
                        opt(&t.revoked_at),
                        revoke,
                    ]
                })
                .collect::<Vec<_>>(),
        ),
        Err(e) => store_note(&e),
    };
    let filter = format!(
        r#"<form class="action" method="get" action="/admin/tokens"><label>uid <input type="text" name="uid" value="{}"></label><label><input type="checkbox" name="all" value="1"{}> include expired + revoked</label><button type="submit">filter</button></form>"#,
        esc(uid.unwrap_or_default()),
        if active_only { "" } else { " checked" }
    );
    let enforcement = if state.config.token_revocation_check {
        "Revocation is enforced at verify time (MUNARIUM_TOKEN_REVOCATION_CHECK=true)."
    } else {
        "Revocation is recorded but NOT enforced at verify time (MUNARIUM_TOKEN_REVOCATION_CHECK is off): a revoked token keeps working until it expires."
    };
    let body = format!(
        r#"<h2>issue a capability token</h2><div class="card">{}</div>
<div class="legend">Minted for the API-management layer's end user: least-privilege claims, short ttl, audited by jti — the token material shows once and is never stored.</div>
<h2>issued tokens</h2><div class="legend">{filter}</div><div class="card">{list}</div>
<div class="notice">{enforcement}</div>"#,
        issue_form(&admin)
    );
    render(&admin, "tokens", "Tokens", &body)
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct IssueForm {
    #[serde(default)]
    _csrf: String,
    #[serde(default)]
    uid: String,
    /// Numeric inputs arrive as TEXT on purpose: a cleared `<input
    /// type=number>` posts `ttl_secs=` (empty), which `Option<u64>` rejects
    /// as a 422 before the handler runs. Parsed leniently below.
    #[serde(default)]
    access_level: String,
    #[serde(default)]
    compartments: String,
    #[serde(default)]
    scope_query: Option<String>,
    #[serde(default)]
    scope_ingest: Option<String>,
    #[serde(default)]
    runbook_refs: String,
    #[serde(default)]
    ttl_secs: String,
}

/// A blank or unparsable numeric form field is "not given".
fn parse_num<T: std::str::FromStr>(s: &str) -> Option<T> {
    s.trim().parse().ok()
}

fn csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(String::from)
        .collect()
}

pub(super) async fn token_issue(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<IssueForm>,
) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    if admin.view_only {
        return notice(
            &admin,
            "tokens",
            "Tokens",
            "view-only passthrough — issue tokens on the server's own /admin",
        );
    }
    if !csrf_ok(&admin, &form._csrf) {
        return stale_form(&admin, "tokens", "Tokens");
    }
    let mut scopes = Vec::new();
    if form.scope_query.is_some() {
        scopes.push("query".to_string());
    }
    if form.scope_ingest.is_some() {
        scopes.push("ingest".to_string());
    }
    let runbook_refs = csv(&form.runbook_refs);
    let req = dto::IssueTokenRequest {
        uid: form.uid.trim().to_string(),
        access_level: parse_num(&form.access_level).unwrap_or(0),
        compartments: csv(&form.compartments),
        scopes,
        runbook_refs: (!runbook_refs.is_empty()).then_some(runbook_refs),
        ttl_secs: parse_num(&form.ttl_secs),
    };
    // `issued_by` is the audit column the REST path fills with the caller's
    // asserted uid; the console has no uid, so it names itself.
    match crate::tokens_api::op_issue_token(&state, "admin-console", &admin.tenant.tenant_id, req)
        .await
    {
        Ok(resp) => {
            let body = format!(
                r#"<div class="notice">token minted — copy it now; it is shown once and never stored (the audit keeps only the jti and claims).</div>
<div class="card">{}</div>
<p><a href="/admin/tokens">← back to tokens</a></p>"#,
                kv(&[
                    ("jti", format!("<code>{}</code>", esc(&resp.jti))),
                    ("expires", esc(&resp.expires_at)),
                    (
                        "token",
                        format!(r#"<div class="secret">{}</div>"#, esc(&resp.token))
                    ),
                ])
            );
            render(&admin, "tokens", "Token minted", &body)
        }
        Err(e) => notice(&admin, "tokens", "Tokens", &e.to_string()),
    }
}

pub(super) async fn token_revoke(
    State(state): State<Arc<AppState>>,
    Path(jti): Path<String>,
    headers: HeaderMap,
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    if admin.view_only {
        return notice(
            &admin,
            "tokens",
            "Tokens",
            "view-only passthrough — revoke on the server's own /admin",
        );
    }
    if !csrf_ok(&admin, &form._csrf) {
        return stale_form(&admin, "tokens", "Tokens");
    }
    match reports_api::op_revoke_token(&state, &admin.tenant.tenant_id, jti).await {
        Ok(_) => Redirect::to("/admin/tokens").into_response(),
        Err(e) => notice(&admin, "tokens", "Tokens", &e.to_string()),
    }
}

// ---------------------------------------------------------------------------
// audit
// ---------------------------------------------------------------------------

#[derive(Debug, Default, serde::Deserialize)]
pub struct AuditParams {
    #[serde(default)]
    uid: Option<String>,
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    runbook: Option<String>,
    #[serde(default)]
    before: Option<String>,
    /// Text for the same reason as the token form: `limit=` must not 422.
    #[serde(default)]
    limit: Option<String>,
}

fn non_empty(s: &Option<String>) -> Option<String> {
    s.as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(String::from)
}

pub(super) async fn audit(
    State(state): State<Arc<AppState>>,
    Query(q): Query<AuditParams>,
    headers: HeaderMap,
) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let limit = q
        .limit
        .as_deref()
        .and_then(parse_num::<i64>)
        .unwrap_or(100)
        .clamp(1, 500);
    let filter = reports_api::AuditFilter {
        uid: non_empty(&q.uid),
        session_id: non_empty(&q.session),
        runbook: non_empty(&q.runbook),
        from: None,
        to: None,
        limit: Some(limit),
        bodies: false,
        before: non_empty(&q.before),
    };
    let form = format!(
        r#"<form class="action" method="get" action="/admin/audit"><label>uid <input type="text" name="uid" value="{}"></label><label>session <input type="text" name="session" value="{}"></label><label>runbook <input type="text" name="runbook" value="{}"></label><button type="submit">filter</button></form>"#,
        esc(filter.uid.as_deref().unwrap_or_default()),
        esc(filter.session_id.as_deref().unwrap_or_default()),
        esc(filter.runbook.as_deref().unwrap_or_default()),
    );
    let (table, older) = match reports_api::op_audit(&state, &admin.tenant.tenant_id, &filter).await
    {
        Ok(page) => {
            let table = html_table(
                &[
                    "created",
                    "plane",
                    "method",
                    "status",
                    "ms",
                    "uid",
                    "session",
                    "runbook",
                    "request id",
                    "token",
                ],
                &page
                    .entries
                    .iter()
                    .map(|e| {
                        let status_html = match e.status {
                            Some(s) if s >= 500 => format!(
                                r#"<span class="swatch" style="background:var(--critical)"></span> {s}"#
                            ),
                            Some(s) if s >= 400 => format!(
                                r#"<span class="swatch" style="background:var(--serious)"></span> {s}"#
                            ),
                            Some(s) => s.to_string(),
                            None => "—".into(),
                        };
                        vec![
                            esc(&e.created_at),
                            esc(&e.plane),
                            esc(&e.method),
                            status_html,
                            e.latency_ms.map(|v| v.to_string()).unwrap_or_else(|| "—".into()),
                            esc(&e.uid),
                            e.session_id
                                .as_deref()
                                .map(|s| link(&format!("/admin/sessions/{s}"), s))
                                .unwrap_or_else(|| "—".into()),
                            e.runbook_ref
                                .as_deref()
                                .map(|r| link(&format!("/admin/runbooks/{r}"), r))
                                .unwrap_or_else(|| "—".into()),
                            e.request_id
                                .as_deref()
                                .map(|r| format!("<code>{}</code>", esc(r)))
                                .unwrap_or_else(|| "—".into()),
                            e.token_jti
                                .as_deref()
                                .map(|j| format!("<code>{}</code>", esc(&short(j, 14))))
                                .unwrap_or_else(|| "—".into()),
                        ]
                    })
                    .collect::<Vec<_>>(),
            );
            let older = page
                .next_before
                .as_deref()
                .map(|nb| {
                    let mut href = format!("/admin/audit?before={}&limit={limit}", urlencode(nb));
                    for (k, v) in [
                        ("uid", &filter.uid),
                        ("session", &filter.session_id),
                        ("runbook", &filter.runbook),
                    ] {
                        if let Some(v) = v {
                            href.push_str(&format!("&{k}={}", urlencode(v)));
                        }
                    }
                    format!(r#"<p><a href="{}">older →</a></p>"#, esc(&href))
                })
                .unwrap_or_default();
            (table, older)
        }
        Err(e) => (store_note(&e), String::new()),
    };
    let body = format!(
        r#"<div class="legend">{form}</div><div class="card">{table}{older}</div>
<div class="notice">Every /v1 request is one row (uid-attributed; bodies capped and never shown here — <code>GET /v1/reports/audit?bodies=true</code> has them). Dashboard reads are not captured. One <code>x-munarium-request-id</code> ties a failing response, its log span, and its row together.</div>"#
    );
    render(&admin, "audit", "Audit trail", &body)
}

/// Minimal percent-encoding for query values we build ourselves.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// findings
// ---------------------------------------------------------------------------

#[derive(Debug, Default, serde::Deserialize)]
pub struct FindingsParams {
    #[serde(default)]
    severity: Option<String>,
    /// Rule-id prefix: `gate.` (kernel gates) or `matrix.` (connector
    /// findings). Anything else is ignored, not queried.
    #[serde(default)]
    rule: Option<String>,
}

pub(super) async fn findings(
    State(state): State<Arc<AppState>>,
    Query(q): Query<FindingsParams>,
    headers: HeaderMap,
) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let severity = q
        .severity
        .as_deref()
        .filter(|s| matches!(*s, "info" | "warn" | "block"));
    // Closed vocabulary on purpose: the picker links are the only values this
    // page ever queries with, so a crafted `?rule=` cannot reach the SQL.
    let rule = q
        .rule
        .as_deref()
        .filter(|r| matches!(*r, "gate." | "matrix."));
    let rule_qs = rule.map(|r| format!("&rule={r}")).unwrap_or_default();
    let picker: String = ["all", "block", "warn", "info"]
        .iter()
        .map(|s| {
            if *s == severity.unwrap_or("all") {
                format!("<strong>{s}</strong>")
            } else {
                format!(r#"<a href="/admin/findings?severity={s}{rule_qs}">{s}</a>"#)
            }
        })
        .collect::<Vec<_>>()
        .join(" · ");
    let sev_qs = severity
        .map(|s| format!("severity={s}&"))
        .unwrap_or_default();
    let rule_picker: String = [("all", ""), ("gate.", "gate."), ("matrix.", "matrix.")]
        .iter()
        .map(|(label, value)| {
            if rule.unwrap_or("") == *value {
                format!("<strong>{label}</strong>")
            } else if value.is_empty() {
                format!(r#"<a href="/admin/findings?{sev_qs}">{label}</a>"#)
            } else {
                format!(r#"<a href="/admin/findings?{sev_qs}rule={value}">{label}</a>"#)
            }
        })
        .collect::<Vec<_>>()
        .join(" · ");
    let table =
        match reports_api::op_recent_findings(&state, &admin.tenant.tenant_id, severity, rule, 200)
            .await
        {
            Ok(rows) => html_table(
                &[
                    "recorded", "severity", "rule", "version", "seq", "scope", "message",
                ],
                &rows
                    .iter()
                    .map(|f| {
                        vec![
                            esc(&f.recorded_at),
                            severity_badge(&f.severity),
                            format!("<code>{}</code>", esc(&f.rule_id)),
                            format!("<code>{}</code>", esc(&f.version_id)),
                            f.seq.to_string(),
                            opt(&f.scope_path),
                            esc(&f.message),
                        ]
                    })
                    .collect::<Vec<_>>(),
            ),
            Err(e) => store_note(&e),
        };
    let body = format!(
        r#"<div class="legend">severity: {picker} &nbsp;·&nbsp; rule: {rule_picker}</div><div class="card">{table}</div>
<div class="notice">Persisted findings across every lineage, newest first (200 max). A <strong>block</strong> is a refused write; a <strong>warn</strong> disputed a claim without refusing it. <code>gate.</code> findings come from the kernel gates at write time; <code>matrix.</code> findings are filed by Munarium Matrix's reconciliation (warn/info only — they never block). Per-version detail: <code>GET /v1/versions/{{id}}/findings?rule_prefix=</code>.</div>"#
    );
    render(&admin, "findings", "Gate findings", &body)
}

/// Where `/admin/matrix` sends an operator for Matrix-side facts, or `None`
/// when nothing is configured. `MUNARIUM_MATRIX_ADMIN_URL` is taken verbatim
/// (it names the console itself, so no `/admin` is appended); the base URL
/// fallback appends the console's path.
pub(super) fn matrix_admin_href(config: &crate::config::Config) -> Option<String> {
    if let Some(admin) = config.matrix_admin_url.as_deref() {
        return Some(admin.trim_end_matches('/').to_string());
    }
    config
        .matrix_base_url
        .as_deref()
        .map(|base| format!("{}/admin", base.trim_end_matches('/')))
}

/// `/admin/matrix` — the structured-evidence plane and the hierarchy that
/// reads it.
///
/// Two questions an operator actually has, on one page. **Is Matrix
/// reachable?** and **which layer is quietly refusing?** The second is the one
/// that does not show up anywhere else: a layer that refuses on most turns
/// still returns 200, so the answers get thinner while every dashboard stays
/// green.
pub(super) async fn matrix(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };

    // The two consoles show different halves of one system and link to each
    // other rather than duplicating. Everything on
    // THIS page is a server-side fact — how the plane behaved from here.
    // Sources, queues, syncs, checkpoints, budgets and the registry are
    // Matrix's own, and they live on Matrix's console. The link appears only
    // when a URL is configured, because an <a> to nowhere is worse than no
    // link: it reads as a deployment that has one.
    //
    // `MUNARIUM_MATRIX_ADMIN_URL` names where a BROWSER
    // reaches that console; the base URL is the service-to-service address,
    // which on an internal ingress a person cannot open. The explicit setting
    // wins; the base URL is the fallback for deployments where the two are
    // one host, as a small deployment often is. Computed before the report is read,
    // and rendered on the store-unavailable path too — the other console is
    // most useful exactly when this one cannot read its own store.
    let elsewhere = match matrix_admin_href(&state.config) {
        Some(href) => format!(
            r#"<div class="notice">Matrix-side facts — sources and their posture, sync runs and checkpoints, the budget ledger, the registry with its diffs, promotion gates and the journal — live on <a href="{}">Matrix's own operator console</a>. Nothing is duplicated between the two, and no crate crosses the tree boundary in either direction.</div>"#,
            esc(&href)
        ),
        None => String::new(),
    };

    let plane = match reports_api::op_matrix_report(&state, &admin.tenant.tenant_id).await {
        Ok(r) => r,
        Err(e) => {
            return render(
                &admin,
                "matrix",
                "Matrix",
                &format!("{}{elsewhere}", store_note(&e)),
            )
        }
    };

    // Not configured and configured-but-failing must never read the same. The
    // first is a deployment that does not use Matrix; the second is an outage.
    let status = if !plane.configured {
        r#"<span class="badge">not configured</span> — <code>MUNARIUM_MATRIX_BASE_URL</code> is unset, so no runbook can serve a data view"#.to_string()
    } else if plane.circuit_open {
        format!(
            r#"<span class="badge bad">circuit open</span> — {} consecutive failures; calls are being refused without being attempted until the cool-off elapses"#,
            plane.consecutive_failures
        )
    } else if plane.consecutive_failures > 0 {
        format!(
            r#"<span class="badge warn">degraded</span> — {} consecutive failures, circuit still closed"#,
            plane.consecutive_failures
        )
    } else {
        r#"<span class="badge good">healthy</span>"#.to_string()
    };

    let views = if plane.data_views.is_empty() {
        "<p>No runbook declares a data view.</p>".to_string()
    } else {
        html_table(
            &["runbook", "data view", "contract", "access level"],
            &plane
                .data_views
                .iter()
                .map(|v| {
                    vec![
                        format!("<code>{}</code>", esc(&v.runbook_ref)),
                        esc(&v.name),
                        format!("<code>{}</code>", esc(&v.contract)),
                        v.access_level.to_string(),
                    ]
                })
                .collect::<Vec<_>>(),
        )
    };

    let hierarchy = match reports_api::op_evidence_report(&state, &admin.tenant.tenant_id, "24h")
        .await
    {
        Ok(r) => {
            let head = format!(
                r#"<p>{} hierarchy turns · {} legacy turns · {} could support a completeness claim (last 24h)</p>"#,
                r.hierarchy_turns, r.legacy_turns, r.completeness_available
            );
            if r.layers.is_empty() {
                format!("{head}<p>No profile has run in this window.</p>")
            } else {
                let table = html_table(
                    &[
                        "profile",
                        "layer",
                        "turns",
                        "refused",
                        "complete",
                        "refusal codes",
                        "p50 ms",
                        "p95 ms",
                    ],
                    &r.layers
                        .iter()
                        .map(|l| {
                            // A layer refusing on most turns is the finding
                            // this page exists for, so it is flagged rather
                            // than left to be spotted in a column of numbers.
                            let ratio = if l.turns > 0 {
                                l.refusals as f64 / l.turns as f64
                            } else {
                                0.0
                            };
                            let refused = if ratio >= 0.5 {
                                format!(
                                    r#"<span class="badge bad">{} ({:.0}%)</span>"#,
                                    l.refusals,
                                    ratio * 100.0
                                )
                            } else {
                                l.refusals.to_string()
                            };
                            vec![
                                esc(&l.profile),
                                esc(&l.layer),
                                l.turns.to_string(),
                                refused,
                                l.complete.to_string(),
                                esc(&l.refusal_codes.join(", ")),
                                l.p50_ms.to_string(),
                                l.p95_ms.to_string(),
                            ]
                        })
                        .collect::<Vec<_>>(),
                );
                format!("{head}{table}")
            }
        }
        Err(e) => store_note(&e),
    };

    let body = format!(
        r#"<div class="card"><h2>Structured-evidence plane</h2><p>{status}</p>{views}</div>
<div class="card"><h2>Evidence hierarchy</h2>{hierarchy}</div>
{elsewhere}
<div class="notice">The circuit breaker is <strong>per instance</strong>, not per tenant, so this reading is about this replica and carries no tenant label anywhere — a shared breaker reported per tenant would let one tenant's scrape reveal another's traffic. A layer flagged red refuses on at least half its turns: those turns still return 200, so the answers are thinner than the runbook claims while nothing else goes red. Machine-readable: <code>GET /v1/reports/matrix</code> and <code>GET /v1/reports/evidence</code>.</div>"#
    );
    render(&admin, "matrix", "Matrix", &body)
}
