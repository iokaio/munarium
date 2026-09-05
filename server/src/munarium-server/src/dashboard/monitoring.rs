// SPDX-License-Identifier: Apache-2.0
//! The monitoring pages (2026-08-17): overview, traffic, endpoints, usage,
//! health. The overview also carries the control-plane inventory tiles
//! (2026-08-27) so the first page an operator sees answers "what is
//! deployed here" beside "how is it doing".

use super::{
    admin_auth, error_panel, kv, render, store_note, window_of, window_picker, WindowParam,
};
use crate::charts::{self, Series};
use crate::config::AuthMode;
use crate::reports_api;
use crate::state::AppState;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use std::sync::Arc;

/// A stat tile that links to the page holding the detail.
fn tile_link(href: &str, label: &str, value: &str, sub: &str) -> String {
    format!(
        r#"<a class="tilelink" href="{}">{}</a>"#,
        charts::esc(href),
        charts::tile(label, value, sub)
    )
}

pub(super) async fn overview(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let tenant = admin.tenant.tenant_id.as_str();

    // Control-plane inventory: the registries render on both stores; the
    // table-backed counts degrade to a note on the memory store.
    let shapes = crate::runbooks_api::op_list_shapes(&state, tenant)
        .await
        .map(|s| s.len())
        .unwrap_or(0);
    let providers = crate::providers_api::op_list_providers(&state, tenant)
        .await
        .map(|p| p.len())
        .unwrap_or(0);
    let inventory = match reports_api::op_control_plane_counts(&state, tenant).await {
        Ok(c) => format!(
            r#"<div class="tiles">{}{}{}{}{}{}{}{}</div>"#,
            tile_link(
                "/admin/runbooks",
                "runbooks",
                &c.runbooks_active.to_string(),
                &format!("active of {} hosted", c.runbooks_total)
            ),
            tile_link(
                "/admin/runbooks",
                "shapes",
                &shapes.to_string(),
                "published"
            ),
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
                &providers.to_string(),
                "applied + defaults"
            ),
            tile_link(
                "/admin/findings?severity=block",
                "block findings · 24h",
                &c.findings_block_24h.to_string(),
                "gate rejections"
            ),
        ),
        Err(e) => format!(
            r#"<div class="tiles">{}{}</div>{}"#,
            tile_link(
                "/admin/runbooks",
                "shapes",
                &shapes.to_string(),
                "published"
            ),
            tile_link(
                "/admin/providers",
                "providers",
                &providers.to_string(),
                "applied + defaults"
            ),
            store_note(&e)
        ),
    };

    let ts = match reports_api::op_timeseries(&state, tenant, "24h", None).await {
        Ok(t) => t,
        Err(e) => {
            let body = format!(
                "<h2>control plane</h2>{inventory}<h2>traffic</h2>{}",
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
    let sessions = reports_api::op_sessions_report(&state, tenant, "24h")
        .await
        .map(|s| s.buckets.iter().map(|b| b.sessions_opened).sum::<i64>())
        .unwrap_or(0);
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
        r#"<h2>control plane</h2>{inventory}
<h2>traffic · 24h</h2><div class="tiles">{}{}{}{}{}</div>
<h2>requests (24h)</h2><div class="card">{spark}</div>
<h2>latency (24h)</h2><div class="card">{lat}</div>"#,
        charts::tile("requests · 24h", &requests.to_string(), "all planes"),
        charts::tile("error rate · 24h", &err_rate, "status ≥ 400"),
        charts::tile("worst p95 · 24h", &format!("{p95:.0} ms"), "per bucket"),
        charts::tile("sessions opened · 24h", &sessions.to_string(), ""),
        charts::tile(
            "audit queue depth",
            &depth.to_string(),
            "this instance; drops on saturation"
        ),
    );
    render(&admin, "overview", "Overview", &body)
}

pub(super) async fn traffic(
    State(state): State<Arc<AppState>>,
    Query(q): Query<WindowParam>,
    headers: HeaderMap,
) -> Response {
    let admin = match admin_auth(&state, &headers) {
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
    let admin = match admin_auth(&state, &headers) {
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
    let admin = match admin_auth(&state, &headers) {
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
    let admin = match admin_auth(&state, &headers) {
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
    let body = format!(
        r#"<div class="tiles">{}{}{}{}{}{}</div>
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
