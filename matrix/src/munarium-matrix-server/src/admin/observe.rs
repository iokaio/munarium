// SPDX-License-Identifier: Apache-2.0
//! The read pages.
//!
//! Everything here is a SELECT through `munarium_matrix_store::reports` or a
//! registry read. Nothing on these pages calls a source: an operator opening
//! the console must not cause outbound traffic, which is why the sources page
//! reports *registration* and posture and leaves reachability to the explicit
//! probe action.
//!
//! Two rules the pages keep:
//!
//! - **Secrets never render.** A `credentialRef` is a name; a probe result is
//!   ok / denied / unreachable; a connection block shows host and database and
//!   never a password, because the applied YAML is displayed verbatim and the
//!   validator already refuses an inline secret.
//! - **Evidence never renders.** An evidence id is a link out to
//!   munarium-server, whose resolver is access-checked per session. This
//!   console shows ids, counts and hashes.

use super::{action, admin_auth, chrome, error_page, render, AdminCtx};
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use std::sync::Arc;

#[derive(Debug, Default, serde::Deserialize)]
pub struct WindowQuery {
    #[serde(default)]
    pub hours: Option<i64>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub refusals: Option<String>,
}

fn hours_of(q: &WindowQuery) -> i64 {
    q.hours.unwrap_or(24).clamp(1, 720)
}

fn window_picker(path: &str, current: i64) -> String {
    let links: String = [(1, "1h"), (24, "24h"), (168, "7d"), (720, "30d")]
        .iter()
        .map(|(h, label)| {
            if *h == current {
                format!("<strong>{label}</strong>")
            } else {
                format!(r#"<a href="{path}?hours={h}">{label}</a>"#)
            }
        })
        .collect::<Vec<_>>()
        .join(" · ");
    format!(r#"<div class="legend">window: {links}</div>"#)
}

/// The store failed. Rendered as a notice on the page rather than a 500,
/// because a console page that 500s hides the diagnosis it exists to show.
fn store_note(e: &munarium_matrix_store::StoreError) -> String {
    format!(
        r#"<div class="notice">{}</div>"#,
        chrome::esc(&e.to_string())
    )
}

// ---------------------------------------------------------------------------
// overview
// ---------------------------------------------------------------------------

pub async fn overview(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<WindowQuery>,
) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let tenant = &admin.caller.tenant;
    let hours = hours_of(&q);
    let mut body = window_picker("/admin", hours);

    // Deployment facts first: what this process is, and whether it agrees with
    // the server it seals into. A lockstep that is not `exact` is the single
    // most consequential thing on this page — an evidence id minted here may
    // not resolve there — so it is above everything else and badged.
    let compat = state.server_compatibility.as_str();
    body.push_str("<h2>deployment</h2>");
    body.push_str(&chrome::kv(&[
        ("role", chrome::esc(state.role().as_str())),
        ("version", chrome::esc(env!("CARGO_PKG_VERSION"))),
        (
            "contract",
            chrome::esc(munarium_matrix_core::CONTRACT_VERSION),
        ),
        (
            "server",
            format!(
                "{} (targets {}) {}",
                chrome::opt(state.server_version.as_deref()),
                chrome::esc(&state.config.target_server_version),
                chrome::state_badge(if compat == "exact" { "ok" } else { "drifted" }),
            ),
        ),
        ("lockstep", chrome::esc(compat)),
        ("uptime", format!("{} s", state.uptime_seconds())),
    ]));

    body.push_str("<h2>queues</h2>");
    match state.store.queue_depth(tenant).await {
        Ok(rows) if rows.is_empty() => body.push_str(r#"<div class="empty">nothing queued</div>"#),
        Ok(rows) => {
            let table: Vec<Vec<String>> = rows
                .iter()
                .map(|r| {
                    vec![
                        chrome::esc(&r.queue),
                        chrome::state_badge(&r.state),
                        r.count.to_string(),
                        // The age is the number that says whether a queue is
                        // moving. A depth with no age is a depth an operator
                        // cannot act on.
                        r.oldest_age_seconds
                            .map(|s| format!("{s} s"))
                            .unwrap_or_else(|| "—".into()),
                    ]
                })
                .collect();
            body.push_str(&chrome::table(
                &["queue", "state", "jobs", "oldest"],
                &table,
            ));
        }
        Err(e) => body.push_str(&store_note(&e)),
    }

    body.push_str("<h2>activity</h2>");
    match state.store.activity(tenant, hours).await {
        Ok(rows) if rows.is_empty() => {
            body.push_str(r#"<div class="empty">no journal rows in this window</div>"#)
        }
        Ok(rows) => {
            let table: Vec<Vec<String>> = rows
                .iter()
                .map(|(kind, total, refused)| {
                    vec![chrome::esc(kind), total.to_string(), refused.to_string()]
                })
                .collect();
            body.push_str(&chrome::table(&["kind", "total", "refused"], &table));
        }
        Err(e) => body.push_str(&store_note(&e)),
    }

    body.push_str("<h2>refusals</h2>");
    match state.store.refusal_counts(tenant, hours).await {
        Ok(rows) if rows.is_empty() => {
            body.push_str(r#"<div class="empty">none in this window</div>"#)
        }
        Ok(rows) => {
            let bars: Vec<(String, f64)> = rows
                .iter()
                .map(|r| (format!("{} ({})", r.code, r.kind), r.count as f64))
                .collect();
            body.push_str(&chrome::bars(&bars, ""));
        }
        Err(e) => body.push_str(&store_note(&e)),
    }

    body.push_str("<h2>budget, this hour</h2>");
    match state.store.budget_ledger(tenant).await {
        Ok(rows) if rows.is_empty() => body.push_str(r#"<div class="empty">nothing spent</div>"#),
        Ok(rows) => {
            let table: Vec<Vec<String>> = rows
                .iter()
                .map(|r| {
                    vec![
                        chrome::link(&format!("/admin/sources/{}", r.source_name), &r.source_name),
                        r.settled.to_string(),
                        r.held.to_string(),
                        r.released.to_string(),
                    ]
                })
                .collect();
            body.push_str(&chrome::table(
                &["source", "settled", "held", "released"],
                &table,
            ));
            body.push_str(
                r#"<div class="legend">held units are in flight; released ones were refunded because the source was never reached.</div>"#,
            );
        }
        Err(e) => body.push_str(&store_note(&e)),
    }

    // The two consoles show different halves of the same system and
    // link to each other rather than duplicating. Server-side facts —
    // evidence counts, hierarchy decisions, refusal rates as the SERVER saw
    // them — live there.
    body.push_str(
        r#"<h2>elsewhere</h2><div class="legend">Server-side facts (sealed evidence, hierarchy decisions, the turns that used them) live on munarium-server's <code>/admin/matrix</code>. Nothing is duplicated between the two.</div>"#,
    );

    render(&admin, "overview", "overview", &body)
}

// ---------------------------------------------------------------------------
// sources
// ---------------------------------------------------------------------------

pub async fn sources(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let assets = match state
        .store
        .list_assets(&admin.caller.tenant, Some("DataSource"), true)
        .await
    {
        Ok(a) => a,
        Err(e) => return error_page(&admin, "sources", "sources", &e.to_string()),
    };
    if assets.is_empty() {
        return render(
            &admin,
            "sources",
            "sources",
            r#"<div class="empty">no DataSource is registered. Apply one from <a href="/admin/author">author</a>, or with <code>mxctl apply</code>.</div>"#,
        );
    }
    let mut rows = Vec::new();
    for a in &assets {
        // The applied YAML is the truth; parse it for the adapter and host
        // rather than trusting a denormalized column.
        let (adapter, host) = match a.parse() {
            Ok(munarium_matrix_types::Asset::DataSource(d)) => (
                format!("{:?}", d.spec.adapter).to_lowercase(),
                // `connection` is an open map by design (each adapter owns
                // its own keys), so `host` is read rather than typed. A
                // source with no host is a landing export, and an empty cell
                // is the honest rendering of that.
                d.spec
                    .connection
                    .get("host")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ),
            _ => ("—".into(), String::new()),
        };
        rows.push(vec![
            chrome::link(&format!("/admin/sources/{}", a.name), &a.name),
            format!("v{}", a.version),
            chrome::esc(&adapter),
            chrome::esc(&host),
        ]);
    }
    let body = format!(
        "{}{}",
        chrome::table(&["source", "version", "adapter", "host"], &rows),
        r#"<div class="legend">Registration, not reachability — opening this page must not cause outbound traffic. Probe a source from its own page.</div>"#
    );
    render(&admin, "sources", "sources", &body)
}

pub async fn source(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let tenant = &admin.caller.tenant;
    let asset = match state.store.get_asset(tenant, "DataSource", &name).await {
        Ok(a) => a,
        Err(e) => return error_page(&admin, "sources", &name, &e.to_string()),
    };

    let mut body = String::new();
    body.push_str("<h2>actions</h2>");
    body.push_str(&rw_form(
        &admin,
        &format!("/admin/sources/{name}/probe"),
        "probe",
    ));
    body.push_str(&rw_form(
        &admin,
        &format!("/admin/sources/{name}/introspect"),
        "introspect",
    ));
    body.push_str(&rw_form(
        &admin,
        &format!("/admin/sources/{name}/sync"),
        "sync now",
    ));
    body.push_str(
        r#"<div class="legend">Each action runs the same <code>/v1</code> operation <code>mxctl</code> would, under the rw credential you supply per submission — the console never stores one, and a leaked mgmt token cannot act.</div>"#,
    );

    body.push_str("<h2>applied yaml</h2>");
    if let Some(d) = state
        .store
        .drifted_assets(tenant)
        .await
        .unwrap_or_default()
        .into_iter()
        .find(|d| d.asset_ref == asset.asset_ref())
    {
        body.push_str(&format!(
            r#"<div class="notice">{} applied in place from the console under decision <code>{}</code> — <strong>drifted from git</strong> until the exported bundle is applied from the repository.</div>"#,
            chrome::state_badge("drifted"),
            chrome::esc(d.decision_id.as_deref().unwrap_or("—"))
        ));
    }
    body.push_str(&chrome::pre(&asset.yaml));
    body.push_str(&format!(
        r#"<div class="legend">{} · applied {}</div>"#,
        chrome::esc(&asset.yaml_hash),
        chrome::esc(&asset.created_at)
    ));

    body.push_str("<h2>checkpoints</h2>");
    match state.store.list_checkpoints(tenant, Some(&name)).await {
        Ok(rows) if rows.is_empty() => body
            .push_str(r#"<div class="empty">none — this source has never completed a sync</div>"#),
        Ok(rows) => {
            let t: Vec<Vec<String>> = rows
                .iter()
                .map(|r| {
                    vec![
                        chrome::esc(&r.entity),
                        chrome::esc(&r.version),
                        chrome::opt(r.watermark.as_deref()),
                        chrome::opt(r.event_position.as_deref()),
                        chrome::esc(&chrome::short(
                            r.schema_fingerprint.as_deref().unwrap_or("—"),
                            20,
                        )),
                        chrome::esc(&r.updated_at),
                    ]
                })
                .collect();
            body.push_str(&chrome::table(
                &[
                    "entity",
                    "version",
                    "watermark",
                    "event position",
                    "fingerprint",
                    "updated",
                ],
                &t,
            ));
        }
        Err(e) => body.push_str(&store_note(&e)),
    }

    body.push_str("<h2>recent syncs</h2>");
    body.push_str(&sync_run_table(&state, tenant, Some(&name)).await);

    render(&admin, "sources", &name, &body)
}

/// An action form that also asks for the rw credential.
fn rw_form(admin: &AdminCtx, path: &str, label: &str) -> String {
    action(
        admin,
        path,
        label,
        r#"<input name="rw_token" type="password" placeholder="rw token" autocomplete="off" style="width:14rem">"#,
        false,
    )
}

async fn sync_run_table(state: &AppState, tenant: &str, source: Option<&str>) -> String {
    match state.store.list_sync_runs(tenant, source, 50).await {
        Ok(rows) if rows.is_empty() => r#"<div class="empty">none</div>"#.into(),
        Ok(rows) => {
            let t: Vec<Vec<String>> = rows
                .iter()
                .map(|r| {
                    vec![
                        chrome::esc(&r.started_at),
                        chrome::link(&format!("/admin/sources/{}", r.source_name), &r.source_name),
                        chrome::esc(&r.entity),
                        chrome::esc(&r.mode),
                        chrome::state_badge(&r.state),
                        r.records_read.to_string(),
                        r.records_rendered.to_string(),
                        // Excluded is its own column and never folded into a
                        // total: G4 says a collection states the rows it
                        // covers AND the rows it excludes.
                        r.records_excluded.to_string(),
                        r.documents_uploaded.to_string(),
                        chrome::esc(r.refusal.as_deref().unwrap_or("")),
                    ]
                })
                .collect();
            chrome::table(
                &[
                    "started", "source", "entity", "mode", "state", "read", "rendered", "excluded",
                    "uploaded", "refusal",
                ],
                &t,
            )
        }
        Err(e) => store_note(&e),
    }
}

pub async fn runs(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let tenant = &admin.caller.tenant;
    let mut body = String::from("<h2>sync runs</h2>");
    body.push_str(&sync_run_table(&state, tenant, None).await);
    body.push_str("<h2>checkpoints</h2>");
    match state.store.list_checkpoints(tenant, None).await {
        Ok(rows) if rows.is_empty() => body.push_str(r#"<div class="empty">none</div>"#),
        Ok(rows) => {
            let t: Vec<Vec<String>> = rows
                .iter()
                .map(|r| {
                    vec![
                        chrome::link(&format!("/admin/sources/{}", r.source_name), &r.source_name),
                        chrome::esc(&r.entity),
                        chrome::esc(&r.version),
                        chrome::opt(r.watermark.as_deref()),
                        chrome::opt(r.event_position.as_deref()),
                        chrome::esc(&r.updated_at),
                    ]
                })
                .collect();
            body.push_str(&chrome::table(
                &[
                    "source",
                    "entity",
                    "version",
                    "watermark",
                    "event position",
                    "updated",
                ],
                &t,
            ));
        }
        Err(e) => body.push_str(&store_note(&e)),
    }
    render(&admin, "runs", "runs and checkpoints", &body)
}

// ---------------------------------------------------------------------------
// journal
// ---------------------------------------------------------------------------

pub async fn journal(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<WindowQuery>,
) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let refusals_only = q.refusals.as_deref() == Some("1");
    let query = munarium_matrix_store::journal::JournalQuery {
        kind: q.kind.clone(),
        source_name: q.source.clone(),
        refusals_only,
        before: None,
        limit: 200,
    };
    let entries = match state.store.list_journal(&admin.caller.tenant, &query).await {
        Ok(e) => e,
        Err(e) => return error_page(&admin, "journal", "journal", &e.to_string()),
    };
    let filter = format!(
        r#"<div class="legend">{} · {}</div>"#,
        if refusals_only {
            r#"<a href="/admin/journal">all</a> · <strong>refusals</strong>"#
        } else {
            r#"<strong>all</strong> · <a href="/admin/journal?refusals=1">refusals</a>"#
        },
        "payloads are redacted at write time and this console has no reveal — a parameter value is customer data, and an operator console is the wrong place to make it readable"
    );
    let rows: Vec<Vec<String>> = entries
        .iter()
        .map(|e| {
            vec![
                chrome::esc(&e.created_at),
                chrome::esc(&e.kind),
                chrome::opt(e.source.as_deref()),
                chrome::opt(e.asset_ref.as_deref()),
                chrome::state_badge(&e.outcome),
                chrome::opt(e.refusal_code.as_deref()),
                chrome::opt(e.via.as_deref()),
                chrome::opt(e.actor.as_deref()),
                // An execute row says where its time went: the source's own
                // statement, the seal, and — what is left — Matrix itself.
                e.duration_ms
                    .map(|d| match (e.source_ms, e.seal_ms) {
                        (Some(s), Some(k)) => format!(
                            "{d} ms <small>(source {s} · seal {k} · matrix {})</small>",
                            d.saturating_sub(s + k)
                        ),
                        _ => format!("{d} ms"),
                    })
                    .unwrap_or_default(),
                // An evidence id is shown and never resolved here: the
                // resolver is the server's and is access-checked per session.
                chrome::esc(&chrome::short(e.evidence_id.as_deref().unwrap_or("—"), 22)),
            ]
        })
        .collect();
    let body = format!(
        "{filter}{}",
        chrome::table(
            &[
                "when", "kind", "source", "asset", "outcome", "refusal", "via", "actor", "took",
                "evidence"
            ],
            &rows
        )
    );
    render(&admin, "journal", "journal", &body)
}

// ---------------------------------------------------------------------------
// verification
// ---------------------------------------------------------------------------

pub async fn verification(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let tenant = &admin.caller.tenant;
    let mut body = String::new();

    body.push_str("<h2>contracts</h2>");
    match state
        .store
        .list_assets(tenant, Some("QueryContract"), true)
        .await
    {
        Ok(assets) => {
            let can_verify = state.role().serves_query();
            let rows: Vec<Vec<String>> = assets
                .iter()
                .map(|a| {
                    let act = if can_verify {
                        rw_form(
                            &admin,
                            &format!("/admin/contracts/{}/verify", a.name),
                            "verify now",
                        )
                    } else {
                        // Verification is a QUERY-plane act. On a control-only
                        // container the button would 404, so it is a note that
                        // says which role serves it.
                        r#"<span class="note">served by the query role</span>"#.into()
                    };
                    vec![
                        chrome::link(&format!("/admin/registry/contracts/{}", a.name), &a.name),
                        format!("v{}", a.version),
                        act,
                    ]
                })
                .collect();
            body.push_str(&chrome::table(&["contract", "version", ""], &rows));
        }
        Err(e) => body.push_str(&store_note(&e)),
    }

    body.push_str("<h2>views: last verification on record</h2>");
    match state.store.latest_verifications(tenant).await {
        Ok(rows) if rows.is_empty() => body.push_str(
            r#"<div class="empty">none. A semantic view executes only after a passing verification — that record is the definition fingerprint an execute compares against.</div>"#,
        ),
        Ok(rows) => {
            let t: Vec<Vec<String>> = rows
                .iter()
                .map(|(kind, name, fingerprint, passed, failed, at)| {
                    vec![
                        chrome::esc(kind),
                        chrome::esc(name),
                        chrome::state_badge(if *failed == 0 { "ok" } else { "failed" }),
                        passed.to_string(),
                        failed.to_string(),
                        chrome::esc(&chrome::short(fingerprint, 24)),
                        chrome::esc(at),
                    ]
                })
                .collect();
            body.push_str(&chrome::table(
                &["kind", "view", "state", "passed", "failed", "fingerprint", "at"],
                &t,
            ));
        }
        Err(e) => body.push_str(&store_note(&e)),
    }
    render(&admin, "verification", "verification", &body)
}

// ---------------------------------------------------------------------------
// mappings
// ---------------------------------------------------------------------------

pub async fn mappings(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let assets = match state
        .store
        .list_assets(&admin.caller.tenant, Some("ClaimMapping"), true)
        .await
    {
        Ok(a) => a,
        Err(e) => return error_page(&admin, "mappings", "mappings", &e.to_string()),
    };
    let mut rows = Vec::new();
    for a in &assets {
        let status = crate::rest::op_promotion_status(&state, &admin.caller, &a.name).await;
        let (mode, promoted) = match &status {
            Ok(s) => (
                s.mode.clone(),
                if s.promoted { "promoted" } else { "shadow" }.to_string(),
            ),
            Err(_) => ("—".into(), "—".into()),
        };
        rows.push(vec![
            chrome::link(&format!("/admin/mappings/{}", a.name), &a.name),
            format!("v{}", a.version),
            chrome::esc(&mode),
            chrome::state_badge(&promoted),
        ]);
    }
    let body = chrome::table(&["mapping", "version", "declared mode", "state"], &rows);
    render(&admin, "mappings", "mappings", &body)
}

pub async fn mapping(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let status = match crate::rest::op_promotion_status(&state, &admin.caller, &name).await {
        Ok(s) => s,
        Err(e) => return error_page(&admin, "mappings", &name, &e.0.detail),
    };

    let mut body = String::new();
    body.push_str("<h2>promotion</h2>");
    let gate_row = |label: &str, actual: f64, min: f64| -> String {
        format!(
            "{} {:.4} (minimum {:.4}) {}",
            label,
            actual,
            min,
            chrome::state_badge(if actual >= min { "ok" } else { "failed" })
        )
    };
    let gates = status
        .gates
        .as_ref()
        .map(|g| {
            format!(
                "{}<br>{}",
                gate_row(
                    "identity precision",
                    g.identity_precision,
                    g.min_identity_precision
                ),
                gate_row(
                    "value conformance",
                    g.value_conformance,
                    g.min_value_conformance
                ),
            )
        })
        .unwrap_or_else(|| "no completed run to measure — run it in shadow first".to_string());
    body.push_str(&chrome::kv(&[
        ("declared mode", chrome::esc(&status.mode)),
        (
            "state",
            chrome::state_badge(if status.promoted {
                "promoted"
            } else {
                "shadow"
            }),
        ),
        ("authority scopes", status.authority_scopes.to_string()),
        ("decision", chrome::opt(status.decision_id.as_deref())),
        ("promoted at", chrome::opt(status.promoted_at.as_deref())),
        ("gates", gates),
    ]));

    if let Some(run) = &status.latest_run {
        body.push_str("<h2>latest run</h2>");
        body.push_str(&chrome::kv(&[
            ("run", chrome::esc(&run.run_id)),
            ("state", chrome::state_badge(&run.state)),
            ("observations", run.observations.to_string()),
            ("discrepancies", run.discrepancies.to_string()),
            ("ambiguous", run.ambiguous.to_string()),
            ("findings filed", run.findings_filed.to_string()),
            ("proposals", run.proposals.to_string()),
            ("ended", chrome::opt(run.ended_at.as_deref())),
        ]));
    }

    body.push_str("<h2>actions</h2>");
    body.push_str(&rw_form(
        &admin,
        &format!("/admin/mappings/{name}/run"),
        "run a pass",
    ));
    // The decision id is required by the API, not by this form: a promotion
    // without the operator's record of why is refused at `/v1`, and asking for
    // it here only means the refusal arrives before the round trip.
    let decision = r#"<input name="rw_token" type="password" placeholder="rw token" autocomplete="off" style="width:11rem"><input name="decision_id" placeholder="decision id" style="width:11rem"><input name="reason" placeholder="reason" style="width:14rem">"#;
    if status.promoted {
        body.push_str(&action(
            &admin,
            &format!("/admin/mappings/{name}/demote"),
            "demote",
            decision,
            true,
        ));
    } else {
        body.push_str(&action(
            &admin,
            &format!("/admin/mappings/{name}/promote"),
            "promote",
            decision,
            false,
        ));
    }
    body.push_str(
        r#"<div class="legend">The gates above are enforced by the API at the moment of the decision, against the latest completed run. This page presents them; it cannot promote past one.</div>"#,
    );
    render(&admin, "mappings", &name, &body)
}
