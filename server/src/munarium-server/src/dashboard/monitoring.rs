// SPDX-License-Identifier: Apache-2.0
//! The monitoring pages (2026-08-17): overview, traffic, endpoints, usage,
//! health. The overview also carries the control-plane inventory tiles
//! (2026-08-27) so the first page an operator sees answers "what is
//! deployed here" beside "how is it doing".

use super::{
    admin_auth, error_panel, kv, pill, render, store_note, window_of, window_picker, Tone,
    WindowParam,
};
use crate::charts::{self, Series};
use crate::config::AuthMode;
use crate::reports_api;
use crate::state::AppState;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use std::sync::Arc;

/// One count shown in an overview tile (ClearView U5).
///
/// A failed read is shown as `unavailable` with its reason in a notice
/// below the tiles, never as 0. A successful read of nothing is still 0.
struct TileCount {
    value: String,
    sub: String,
    note: String,
}

fn tile_count<E: std::fmt::Display>(what: &str, read: Result<usize, E>, sub: &str) -> TileCount {
    match read {
        Ok(n) => TileCount {
            value: n.to_string(),
            sub: sub.to_string(),
            note: String::new(),
        },
        Err(e) => TileCount {
            value: "unavailable".to_string(),
            sub: "read failed; see the notice below".to_string(),
            note: format!(
                r#"<div class="notice">{} unavailable: {}</div>"#,
                charts::esc(what),
                charts::esc(&e.to_string())
            ),
        },
    }
}

/// One condition the overview's attention strip checks (ClearView U3):
/// what the count counts, where the detail lives, how serious a non-zero
/// count is, and the observed count — or why it could not be read.
pub(super) struct Check {
    pub what: &'static str,
    pub href: String,
    pub tone: Tone,
    pub observed: Result<i64, String>,
}

impl Check {
    fn new(what: &'static str, href: &str, tone: Tone, observed: Result<i64, String>) -> Self {
        Check {
            what,
            href: href.to_string(),
            tone,
            observed,
        }
    }
}

/// The attention strip: only the abnormal, each with its count and a link
/// to its page. A check whose read failed is listed as unavailable — never
/// as zero, and never folded into "nothing needs attention".
pub(super) fn attention_strip(checks: &[Check]) -> String {
    let mut needs = Vec::new();
    let mut unavailable = Vec::new();
    for c in checks {
        match &c.observed {
            Ok(n) if *n > 0 => needs.push(format!(
                r#"<li>{} <a href="{}">{}</a></li>"#,
                pill(&n.to_string(), c.tone),
                charts::esc(&c.href),
                charts::esc(c.what)
            )),
            Ok(_) => {}
            Err(reason) => unavailable.push(format!(
                r#"<li>{} {}: {}</li>"#,
                pill("unavailable", Tone::Neutral),
                charts::esc(c.what),
                charts::esc(reason)
            )),
        }
    }
    let read = checks.len() - unavailable.len();
    let summary = match (needs.is_empty(), unavailable.is_empty()) {
        (true, true) => format!(
            r#"<p>{} Nothing observed needs attention: {read} of {} checks read, each zero.</p>"#,
            pill("clear", Tone::Good),
            checks.len()
        ),
        (true, false) => format!(
            r#"<p>Nothing observed needs attention, but {} of {} checks could not be read, so this is not an all-clear.</p>"#,
            unavailable.len(),
            checks.len()
        ),
        (false, _) => String::new(),
    };
    let list = |items: &[String]| {
        if items.is_empty() {
            String::new()
        } else {
            format!("<ul>{}</ul>", items.concat())
        }
    };
    format!(
        r#"<section class="attention" aria-labelledby="attention-h"><h2 id="attention-h">needs attention</h2>{summary}{}{}</section>"#,
        list(&needs),
        list(&unavailable)
    )
}

/// The checks the overview runs, in the order the strip lists them.
async fn attention_checks(
    state: &AppState,
    tenant: &str,
    counts: &munarium_core::Result<reports_api::ControlPlaneCounts>,
    providers: &munarium_core::Result<Vec<munarium_api_types::ProviderModelsDto>>,
) -> Vec<Check> {
    let err = |e: &munarium_core::KernelError| e.to_string();
    let attention = reports_api::op_attention_counts(state, tenant).await;
    let table =
        |f: fn(&reports_api::AttentionCounts) -> i64| attention.as_ref().map(f).map_err(err);
    // Process state: whether THIS replica admits datastore traffic. Read
    // without I/O, so it is always observed.
    let readiness = state.datastore_readiness();
    let unserved = if readiness.admits() {
        0
    } else {
        readiness.blocking().len().max(1) as i64
    };
    vec![
        Check::new(
            "runs awaiting gate approval",
            "/admin/runbooks",
            Tone::Warn,
            counts
                .as_ref()
                .map(|c| c.runs_awaiting_approval)
                .map_err(err),
        ),
        Check::new(
            "blocking gate findings in the last 24 hours",
            "/admin/findings?severity=block",
            Tone::Bad,
            counts.as_ref().map(|c| c.findings_block_24h).map_err(err),
        ),
        Check::new(
            "datastore scopes this replica cannot serve yet (it is not admitting datastore traffic)",
            "/admin/storage",
            Tone::Bad,
            Ok(unserved),
        ),
        Check::new(
            "applied provider configs whose credential does not resolve",
            "/admin/providers",
            Tone::Warn,
            providers
                .as_ref()
                .map(|list| {
                    list.iter()
                        .filter(|p| p.source == "applied" && !p.credential_ok)
                        .count() as i64
                })
                .map_err(err),
        ),
        Check::new(
            "index build jobs failed in the last 24 hours",
            "/admin/build-jobs",
            Tone::Bad,
            table(|a| a.build_jobs_failed_24h),
        ),
        Check::new(
            "index build attempts still marked running after their lease expired",
            "/admin/storage",
            Tone::Warn,
            table(|a| a.build_attempts_lapsed),
        ),
        Check::new(
            "guarded commands with an unresolved outcome (in progress, or lost and to be inspected)",
            &format!("{}ops/command-recovery.md", super::DOCS_BASE),
            Tone::Warn,
            table(|a| a.commands_unresolved),
        ),
    ]
}

/// A stat tile that links to the page holding the detail.
fn tile_link(href: &str, label: &str, value: &str, sub: &str) -> String {
    format!(
        r#"<a class="tilelink" href="{}">{}</a>"#,
        charts::esc(href),
        charts::tile(label, value, sub)
    )
}

pub(super) async fn overview(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let admin = match admin_auth(&state, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let tenant = admin.tenant.tenant_id.as_str();

    // Control-plane inventory: the registries render on both stores; the
    // table-backed counts degrade to a note on the memory store.
    let shapes = tile_count(
        "shapes",
        crate::runbooks_api::op_list_shapes(&state, tenant)
            .await
            .map(|s| s.len()),
        "published",
    );
    let provider_list = crate::providers_api::op_list_providers(&state, tenant).await;
    let providers = tile_count(
        "providers",
        provider_list.as_ref().map(|p| p.len()),
        "applied + defaults",
    );
    let read_notes = format!("{}{}", shapes.note, providers.note);
    let counts = reports_api::op_control_plane_counts(&state, tenant).await;
    let attention =
        attention_strip(&attention_checks(&state, tenant, &counts, &provider_list).await);
    let inventory = match counts {
        Ok(c) => format!(
            r#"<div class="tiles">{}{}{}{}{}{}{}{}</div>{read_notes}"#,
            tile_link(
                "/admin/runbooks",
                "runbooks",
                &c.runbooks_active.to_string(),
                &format!("active of {} hosted", c.runbooks_total)
            ),
            tile_link("/admin/runbooks", "shapes", &shapes.value, &shapes.sub),
            tile_link(
                "/admin/collections",
                "collections",
                &c.collections_active.to_string(),
                "active"
            ),
            tile_link(
                "/admin/runbooks",
                "runs awaiting approval",
                &c.runs_awaiting_approval.to_string(),
                &format!("{} running", c.runs_running)
            ),
            tile_link(
                "/admin/sessions?state=open",
                "sessions open",
                &c.sessions_open.to_string(),
                ""
            ),
            tile_link(
                "/admin/tokens",
                "tokens active",
                &c.tokens_active.to_string(),
                "unexpired, unrevoked"
            ),
            tile_link(
                "/admin/providers",
                "providers",
                &providers.value,
                &providers.sub
            ),
            tile_link(
                "/admin/findings?severity=block",
                "block findings · 24h",
                &c.findings_block_24h.to_string(),
                "gate rejections"
            ),
        ),
        Err(e) => format!(
            r#"<div class="tiles">{}{}</div>{read_notes}{}"#,
            tile_link("/admin/runbooks", "shapes", &shapes.value, &shapes.sub),
            tile_link(
                "/admin/providers",
                "providers",
                &providers.value,
                &providers.sub
            ),
            store_note(&e)
        ),
    };

    let ts = match reports_api::op_timeseries(&state, tenant, "24h", None).await {
        Ok(t) => t,
        Err(e) => {
            let body = format!(
                "{attention}<h2>control plane</h2>{inventory}<h2>traffic</h2>{}",
                store_note(&e)
            );
            return render(&admin, "overview", "Overview", &body);
        }
    };
    let requests: i64 = ts.buckets.iter().map(|b| b.requests).sum();
    let errors: i64 = ts.buckets.iter().map(|b| b.errors_4xx + b.errors_5xx).sum();
    let err_rate = if requests > 0 {
        format!("{:.2}%", errors as f64 * 100.0 / requests as f64)
    } else {
        "–".into()
    };
    let p95 = ts
        .buckets
        .iter()
        .filter_map(|b| b.p95_latency_ms)
        .fold(0.0_f64, f64::max);
    let sessions = tile_count(
        "sessions opened",
        reports_api::op_sessions_report(&state, tenant, "24h")
            .await
            .map(|s| {
                s.buckets
                    .iter()
                    .map(|b| b.sessions_opened.max(0) as usize)
                    .sum::<usize>()
            }),
        "",
    );
    let depth = state
        .interactions_tx
        .max_capacity()
        .saturating_sub(state.interactions_tx.capacity());

    let labels: Vec<String> = ts.buckets.iter().map(|b| b.bucket.clone()).collect();
    let spark = charts::line_chart(
        &labels,
        ts.bucket_seconds,
        &[Series {
            name: "requests",
            color: "var(--s1)",
            points: ts.buckets.iter().map(|b| Some(b.requests as f64)).collect(),
        }],
    );
    let lat = charts::line_chart(
        &labels,
        ts.bucket_seconds,
        &[
            Series {
                name: "p50 ms",
                color: "var(--s1)",
                points: ts.buckets.iter().map(|b| b.p50_latency_ms).collect(),
            },
            Series {
                name: "p95 ms",
                color: "var(--s2)",
                points: ts.buckets.iter().map(|b| b.p95_latency_ms).collect(),
            },
        ],
    );
    let body = format!(
        r#"{attention}<h2>control plane</h2>{inventory}
<h2>traffic · 24h</h2><div class="tiles">{}{}{}{}{}</div>{}
<h2>requests (24h)</h2><div class="card">{spark}</div>
<h2>latency (24h)</h2><div class="card">{lat}</div>"#,
        charts::tile("requests · 24h", &requests.to_string(), "all planes"),
        charts::tile("error rate · 24h", &err_rate, "status ≥ 400"),
        charts::tile("worst p95 · 24h", &format!("{p95:.0} ms"), "per bucket"),
        charts::tile("sessions opened · 24h", &sessions.value, &sessions.sub),
        charts::tile(
            "audit queue depth",
            &depth.to_string(),
            "this instance; drops on saturation"
        ),
        sessions.note,
    );
    render(&admin, "overview", "Overview", &body)
}

pub(super) async fn traffic(
    State(state): State<Arc<AppState>>,
    Query(q): Query<WindowParam>,
    headers: HeaderMap,
) -> Response {
    let admin = match admin_auth(&state, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let window = window_of(&q);
    let ts = match reports_api::op_timeseries(&state, &admin.tenant.tenant_id, window, None).await {
        Ok(t) => t,
        Err(e) => return error_panel(&admin, "traffic", "Traffic", &e),
    };
    let labels: Vec<String> = ts.buckets.iter().map(|b| b.bucket.clone()).collect();
    let req_chart = charts::line_chart(
        &labels,
        ts.bucket_seconds,
        &[Series {
            name: "requests",
            color: "var(--s1)",
            points: ts.buckets.iter().map(|b| Some(b.requests as f64)).collect(),
        }],
    );
    // Errors are STATES, not series: the reserved status colors, with the
    // legend carrying the labels.
    let err_chart = charts::stacked_bars(
        &labels,
        ts.bucket_seconds,
        &[
            Series {
                name: "4xx",
                color: "var(--serious)",
                points: ts
                    .buckets
                    .iter()
                    .map(|b| Some(b.errors_4xx as f64))
                    .collect(),
            },
            Series {
                name: "5xx",
                color: "var(--critical)",
                points: ts
                    .buckets
                    .iter()
                    .map(|b| Some(b.errors_5xx as f64))
                    .collect(),
            },
        ],
    );
    let lat_chart = charts::line_chart(
        &labels,
        ts.bucket_seconds,
        &[
            Series {
                name: "p50 ms",
                color: "var(--s1)",
                points: ts.buckets.iter().map(|b| b.p50_latency_ms).collect(),
            },
            Series {
                name: "p95 ms",
                color: "var(--s2)",
                points: ts.buckets.iter().map(|b| b.p95_latency_ms).collect(),
            },
        ],
    );
    let table = charts::data_table(
        &["bucket", "requests", "4xx", "5xx", "p50 ms", "p95 ms"],
        &ts.buckets
            .iter()
            .map(|b| {
                vec![
                    b.bucket.clone(),
                    b.requests.to_string(),
                    b.errors_4xx.to_string(),
                    b.errors_5xx.to_string(),
                    b.p50_latency_ms.map_or("–".into(), |v| format!("{v:.0}")),
                    b.p95_latency_ms.map_or("–".into(), |v| format!("{v:.0}")),
                ]
            })
            .collect::<Vec<_>>(),
    );
    let body = format!(
        "{}<h2>requests</h2><div class=\"card\">{req_chart}</div>\
         <h2>errors</h2><div class=\"card\">{err_chart}</div>\
         <h2>latency</h2><div class=\"card\">{lat_chart}{table}</div>",
        window_picker("/admin/traffic", window)
    );
    render(&admin, "traffic", "Traffic", &body)
}

pub(super) async fn endpoints(
    State(state): State<Arc<AppState>>,
    Query(q): Query<WindowParam>,
    headers: HeaderMap,
) -> Response {
    let admin = match admin_auth(&state, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let window = window_of(&q);
    let report = match reports_api::op_endpoints(&state, &admin.tenant.tenant_id, window, 20).await
    {
        Ok(t) => t,
        Err(e) => return error_panel(&admin, "endpoints", "Endpoints", &e),
    };
    let volume = charts::hbar_rows(
        &report
            .rows
            .iter()
            .map(|r| {
                (
                    r.method.clone(),
                    r.requests as f64,
                    format!("{:.1}% err", r.error_rate * 100.0),
                )
            })
            .collect::<Vec<_>>(),
    );
    let mut slow = report.rows.clone();
    slow.sort_by(|a, b| {
        b.p95_latency_ms
            .unwrap_or(0.0)
            .total_cmp(&a.p95_latency_ms.unwrap_or(0.0))
    });
    let slow_rows = charts::hbar_rows(
        &slow
            .iter()
            .take(10)
            .map(|r| {
                (
                    r.method.clone(),
                    r.p95_latency_ms.unwrap_or(0.0),
                    "ms p95".to_string(),
                )
            })
            .collect::<Vec<_>>(),
    );
    let table = charts::data_table(
        &["method", "requests", "error rate", "avg ms", "p95 ms"],
        &report
            .rows
            .iter()
            .map(|r| {
                vec![
                    r.method.clone(),
                    r.requests.to_string(),
                    format!("{:.2}%", r.error_rate * 100.0),
                    r.avg_latency_ms.map_or("–".into(), |v| format!("{v:.0}")),
                    r.p95_latency_ms.map_or("–".into(), |v| format!("{v:.0}")),
                ]
            })
            .collect::<Vec<_>>(),
    );
    let body = format!(
        "{}<h2>top endpoints by volume</h2><div class=\"card\">{volume}</div>\
         <h2>slowest by p95</h2><div class=\"card\">{slow_rows}{table}</div>",
        window_picker("/admin/endpoints", window)
    );
    render(&admin, "endpoints", "Endpoints", &body)
}

pub(super) async fn usage(
    State(state): State<Arc<AppState>>,
    Query(q): Query<WindowParam>,
    headers: HeaderMap,
) -> Response {
    let admin = match admin_auth(&state, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let group_by = q.group_by.as_deref().unwrap_or("uid");
    let rows =
        match reports_api::op_usage(&state, &admin.tenant.tenant_id, group_by, None, None).await {
            Ok(t) => t,
            Err(e) => return error_panel(&admin, "usage", "Usage", &e),
        };
    let picker: String = ["uid", "session", "runbook", "collection"]
        .iter()
        .map(|g| {
            if *g == group_by {
                format!("<strong>{g}</strong>")
            } else {
                format!(r#"<a href="/admin/usage?group_by={g}">{g}</a>"#)
            }
        })
        .collect::<Vec<_>>()
        .join(" · ");
    let bars = charts::hbar_rows(
        &rows
            .iter()
            .take(25)
            .map(|r| {
                (
                    r.key.clone(),
                    r.interactions as f64,
                    format!("{} turns", r.turns),
                )
            })
            .collect::<Vec<_>>(),
    );
    let table = charts::data_table(
        &[
            "key",
            "interactions",
            "turns",
            "in tokens",
            "out tokens",
            "avg ms",
        ],
        &rows
            .iter()
            .map(|r| {
                vec![
                    r.key.clone(),
                    r.interactions.to_string(),
                    r.turns.to_string(),
                    r.completion_input_tokens.to_string(),
                    r.completion_output_tokens.to_string(),
                    r.avg_latency_ms.map_or("–".into(), |v| format!("{v:.0}")),
                ]
            })
            .collect::<Vec<_>>(),
    );
    let body = format!(
        r#"<div class="legend">group by: {picker}</div><h2>interactions (all time)</h2><div class="card">{bars}{table}</div>"#
    );
    render(&admin, "usage", "Usage", &body)
}

pub(super) async fn health(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let admin = match admin_auth(&state, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let ready = state.store_ready().await;
    let (pool_size, pool_idle) = state
        .pg_pool()
        .map(|p| (p.size().to_string(), p.num_idle().to_string()))
        .unwrap_or_else(|| ("–".into(), "–".into()));
    let depth = state
        .interactions_tx
        .max_capacity()
        .saturating_sub(state.interactions_tx.capacity());
    let store = match state.pg_pool() {
        Some(_) => "postgres",
        None => "memory",
    };
    let cfg = &state.config;
    // Effective, NON-SECRET configuration: the database URL, static token
    // values, and the token secret never render — only whether they exist.
    let auth = match &cfg.auth {
        AuthMode::Disabled => "disabled (dev — every caller is rw + mgmt)".to_string(),
        AuthMode::Static(tokens) => {
            let count = |role: &str| tokens.iter().filter(|(_, _, r)| r == role).count();
            format!(
                "static — {} tokens ({} mgmt, {} rw, {} ro)",
                tokens.len(),
                count("mgmt"),
                count("rw"),
                count("ro")
            )
        }
    };
    let yes_no = |b: bool| if b { "yes" } else { "no" }.to_string();
    let config = kv(&[
        ("instance id", charts::esc(&cfg.instance_id)),
        (
            "tenant (this credential)",
            charts::esc(&admin.tenant.tenant_id),
        ),
        ("auth mode", charts::esc(&auth)),
        (
            "token secret",
            if cfg.token_secret.is_some() {
                "configured (capability JWTs can be issued)".into()
            } else {
                "not configured — POST /v1/access-tokens will refuse".into()
            },
        ),
        ("token ttl (default)", format!("{} s", cfg.token_ttl_secs)),
        ("token revocation check", yes_no(cfg.token_revocation_check)),
        (
            "console login",
            charts::esc(&state.console.settings.describe()),
        ),
        ("require uid", yes_no(cfg.require_uid)),
        ("store", store.into()),
        ("source bytes store", state.source_backend_id().into()),
        (
            "document intelligence",
            state
                .doc_intel_id()
                .unwrap_or("none (local extraction only)")
                .into(),
        ),
        ("rest listener", charts::esc(&cfg.http_addr)),
        (
            "grpc listener",
            charts::esc(cfg.grpc_addr.as_deref().unwrap_or("disabled")),
        ),
        ("ops listener", charts::esc(&cfg.ops_addr)),
        (
            "replica count (budget divisor)",
            cfg.replica_count.to_string(),
        ),
        ("registry ttl", format!("{} s", cfg.registry_ttl_secs)),
        ("db max connections", cfg.db_max_conns.to_string()),
        ("idempotency ttl", format!("{} s", cfg.idempotency_ttl_secs)),
        (
            "session idle ttl",
            if cfg.session_idle_ttl_secs == 0 {
                "0 (sweep disabled)".into()
            } else {
                format!("{} s", cfg.session_idle_ttl_secs)
            },
        ),
        (
            "interaction body cap",
            format!("{} bytes", cfg.interaction_body_max),
        ),
        ("shutdown grace", format!("{} s", cfg.shutdown_grace_secs)),
    ]);
    let scale = crate::scale_diagnostics::health_section(
        &crate::scale_diagnostics::op_scale_report(&state, &admin.tenant.tenant_id).await,
    );
    let body = format!(
        r#"<div class="tiles">{}{}{}{}{}{}</div>
{scale}
<h2>effective configuration</h2><div class="card">{config}</div>
<div class="notice">Process state only — never calls the paid <a href="/healthai">/healthai</a> probe.
Scrape target: <code>GET /metrics</code> on the ops plane (:9090). Secrets never render here: the database URL, static token values, and the token secret show only as present or absent.</div>"#,
        charts::tile("version", env!("CARGO_PKG_VERSION"), "munarium-server"),
        charts::tile("store", store, if ready { "ready" } else { "NOT READY" }),
        charts::tile("pool connections", &pool_size, "sqlx"),
        charts::tile("pool idle", &pool_idle, ""),
        charts::tile("audit queue depth", &depth.to_string(), "of 1024"),
        charts::tile(
            "concurrency ceiling",
            &state.config.max_concurrency.to_string(),
            "MUNARIUM_MAX_CONCURRENCY / plane"
        ),
    );
    render(&admin, "health", "Health", &body)
}

#[cfg(test)]
mod tests {
    //! ClearView U5 against the Phase 1 console fixture's failed-read cases.
    use super::*;
    use munarium_core::KernelError;

    const FIXTURE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/clearview-phase1/v14-console.json"
    ));

    #[test]
    fn a_failed_read_is_unavailable_and_an_empty_read_is_zero() {
        let fixture: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let cases = fixture["failed_reads"]["cases"].as_array().unwrap();
        let mut checked = 0;
        for c in cases {
            match c["v15"].as_str().unwrap() {
                "unavailable" => {
                    let got = tile_count(
                        c["tile"].as_str().unwrap(),
                        Err(KernelError::Storage("pool timed out <x>".into())),
                        "sub",
                    );
                    assert_eq!(got.value, "unavailable", "{}", c["case"]);
                    assert_ne!(got.value, c["expected_v14_value"].as_str().unwrap());
                    assert!(got.note.contains("unavailable"), "{}", got.note);
                    assert!(
                        got.note.contains("pool timed out &lt;x&gt;"),
                        "{}",
                        got.note
                    );
                    checked += 1;
                }
                v if v.starts_with("0 ") => {
                    let got = tile_count("shapes", Ok::<_, KernelError>(0), "published");
                    assert_eq!((got.value.as_str(), got.note.as_str()), ("0", ""));
                    assert_eq!(got.sub, "published");
                    checked += 1;
                }
                _ => {}
            }
        }
        assert_eq!(checked, 4, "fixture lost failed-read cases");
    }

    fn check(what: &'static str, observed: Result<i64, String>) -> Check {
        Check::new(what, "/admin/x?a=1&b=2", Tone::Warn, observed)
    }

    /// U3: zero is not listed, a count is listed with its label and link,
    /// and a failed read is unavailable: never 0, never an all-clear.
    #[test]
    fn the_attention_strip_lists_only_the_abnormal_and_never_hides_a_failed_read() {
        let clear = attention_strip(&[check("runs waiting", Ok(0)), check("findings", Ok(0))]);
        assert!(
            clear.contains("Nothing observed needs attention: 2 of 2 checks read"),
            "{clear}"
        );
        assert!(
            clear.contains(r#"<span class="badge good">clear</span>"#),
            "{clear}"
        );
        assert!(!clear.contains("<li>"), "{clear}");

        let needs = attention_strip(&[check("runs waiting", Ok(3)), check("findings", Ok(0))]);
        assert!(
            needs.contains(
                r#"<li><span class="badge warn">3</span> <a href="/admin/x?a=1&amp;b=2">runs waiting</a></li>"#
            ),
            "{needs}"
        );
        assert!(
            !needs.contains("findings"),
            "a zero is not abnormal: {needs}"
        );
        assert!(!needs.contains("Nothing observed"), "{needs}");

        let failed = attention_strip(&[
            check("runs waiting", Ok(0)),
            check("findings", Err("needs the postgres store <x>".into())),
        ]);
        assert!(
            failed.contains("unavailable</span> findings: needs the postgres store &lt;x&gt;"),
            "{failed}"
        );
        assert!(
            failed.contains("1 of 2 checks could not be read, so this is not an all-clear"),
            "{failed}"
        );
        assert!(!failed.contains("badge good"), "{failed}");
        assert!(
            !failed.contains(">0<"),
            "a failed read never reads as zero: {failed}"
        );
    }
}
