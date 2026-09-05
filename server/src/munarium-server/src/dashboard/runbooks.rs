// SPDX-License-Identifier: Apache-2.0
//! The runbooks hub and its viewers (2026-08-27): every hosted runbook,
//! published shape, and applied chronology-rules asset, each with a page
//! that shows the applied document the way `mmctl … info` would — plus the
//! run viewer, which is where the dashboard's one rw action lives
//! (approving a gate; see the module header in mod.rs for the credential
//! rule).

use super::{
    action, admin_auth, csrf_field, csrf_ok, error_panel, html_table, json_block, kv, link, notice,
    opt, pre, render, rw_credential, short, stale_form, state_badge, store_note, window_picker,
    WindowParam,
};
use crate::charts::{self, esc};
use crate::reports_api;
use crate::runbooks_api;
use crate::state::AppState;
use axum::extract::{Form, Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use munarium_runbooks::{parse_runbook, StepSpec};
use std::sync::Arc;

fn runbook_link(runbook_ref: &str) -> String {
    link(&format!("/admin/runbooks/{runbook_ref}"), runbook_ref)
}

fn shape_link(shape_ref: &str) -> String {
    link(&format!("/admin/shapes/{shape_ref}"), shape_ref)
}

fn run_link(run_id: &str) -> String {
    link(&format!("/admin/runs/{run_id}"), run_id)
}

fn collection_link(id: Option<&str>, name: &str) -> String {
    match id {
        Some(id) => link(&format!("/admin/collections/{id}"), name),
        None => format!(
            "{} <span class=\"badge\">not materialized</span>",
            esc(name)
        ),
    }
}

fn status_badge(status: &str) -> String {
    let class = if status == "active" {
        "badge"
    } else {
        "badge warn"
    };
    format!(r#"<span class="{class}">{}</span>"#, esc(status))
}

fn runs_table(rows: &[(String, String, String, String)]) -> String {
    html_table(
        &["run", "runbook", "state", "created"],
        &rows
            .iter()
            .map(|(id, rref, state_name, created)| {
                vec![
                    run_link(id),
                    runbook_link(rref),
                    state_badge(state_name),
                    esc(created),
                ]
            })
            .collect::<Vec<_>>(),
    )
}

/// /admin/runbooks — the hub. Each section degrades on its own: the
/// registries render on both stores, the tables say what they need.
pub(super) async fn hub(
    State(state): State<Arc<AppState>>,
    Query(q): Query<WindowParam>,
    headers: HeaderMap,
) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let tenant = admin.tenant.tenant_id.as_str();
    let window = q.window.as_deref().unwrap_or("7d");

    let runbooks_html = match runbooks_api::op_list_runbooks(&state, tenant, true).await {
        Ok(list) => html_table(
            &[
                "runbook",
                "name",
                "version",
                "status",
                "min level",
                "collections",
                "created",
            ],
            &list
                .iter()
                .map(|r| {
                    let cols: Vec<String> = r
                        .collections
                        .iter()
                        .map(|c| collection_link(c.collection_id.as_deref(), &c.name))
                        .collect();
                    vec![
                        runbook_link(&r.runbook_ref),
                        esc(&r.name),
                        r.version.to_string(),
                        status_badge(&r.status),
                        r.min_access_level.to_string(),
                        if cols.is_empty() {
                            "—".into()
                        } else {
                            cols.join(", ")
                        },
                        esc(&r.created_at),
                    ]
                })
                .collect::<Vec<_>>(),
        ),
        Err(e) => store_note(&e),
    };

    let shapes_html = match runbooks_api::op_list_shapes(&state, tenant).await {
        Ok(list) => html_table(
            &["shape", "yaml hash", "fact schema", "chunking", "created"],
            &list
                .iter()
                .map(|s| {
                    vec![
                        shape_link(&s.shape_ref),
                        format!("<code>{}</code>", esc(&short(&s.yaml_hash, 12))),
                        if s.has_fact_schema { "yes" } else { "none" }.into(),
                        s.chunking
                            .as_ref()
                            .map(|(strategy, max)| format!("{} · {max} chars", esc(strategy)))
                            .unwrap_or_else(|| "default".into()),
                        opt(&s.created_at),
                    ]
                })
                .collect::<Vec<_>>(),
        ),
        Err(e) => store_note(&e),
    };

    let chrono_html = match state.list_chronology_rules(tenant).await {
        Ok(list) => html_table(
            &["rules asset", "created"],
            &list
                .iter()
                .map(|(name, created)| {
                    vec![
                        link(&format!("/admin/chronology-rules/{name}"), name),
                        opt(created),
                    ]
                })
                .collect::<Vec<_>>(),
        ),
        Err(e) => store_note(&e),
    };

    let runs_html = match reports_api::op_runbook_report(&state, tenant, window).await {
        Ok(report) => {
            let tiles: String = report
                .runs
                .iter()
                .map(|r| {
                    charts::tile(
                        &r.state,
                        &r.runs.to_string(),
                        &r.avg_wall_ms
                            .map_or(String::new(), |w| format!("avg {:.1}s wall", w / 1000.0)),
                    )
                })
                .collect();
            let steps = charts::hbar_rows(
                &report
                    .steps
                    .iter()
                    .map(|s| (s.state.clone(), s.steps as f64, String::new()))
                    .collect::<Vec<_>>(),
            );
            let recent = reports_api::op_recent_runs(&state, tenant, 20)
                .await
                .unwrap_or_default();
            format!(
                "{}<div class=\"tiles\">{tiles}</div>\
                 <h2>step states</h2><div class=\"card\">{steps}</div>\
                 <h2>recent runs</h2><div class=\"card\">{}</div>",
                window_picker("/admin/runbooks", window),
                runs_table(&recent)
            )
        }
        Err(e) => store_note(&e),
    };

    let body = format!(
        r#"<h2>hosted runbooks</h2><div class="card">{runbooks_html}</div>
<h2>published shapes</h2><div class="card">{shapes_html}</div>
<h2>chronology rules</h2><div class="card">{chrono_html}</div>
<h2>runs</h2>{runs_html}
<div class="notice">Publishing stays on the API/CLI by design (<code>mmctl apply -f</code>, <code>mmctl bundle apply</code>): the applied bytes are the deploy artifact, and GitOps is their source of truth. This page reads them.</div>"#
    );
    render(&admin, "runbooks", "Runbooks", &body)
}

/// /admin/runbooks/{name} — one runbook (latest, or name@version) as
/// applied: lifecycle, collections, plan, knobs, and the yaml itself.
pub(super) async fn runbook(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let tenant = admin.tenant.tenant_id.as_str();
    let src = match runbooks_api::op_runbook_source(&state, tenant, &name).await {
        Ok(s) => s,
        Err(e) => return error_panel(&admin, "runbooks", "Runbook", &e),
    };
    let info = runbooks_api::op_runbook_info(&state, tenant, &src.runbook_ref).await;
    let doc = parse_runbook(&src.yaml);
    let runs = runbooks_api::op_runs_for_runbook(&state, tenant, &src.runbook_ref, 20)
        .await
        .unwrap_or_default();

    let mut lifecycle = vec![
        ("runbook", format!("<code>{}</code>", esc(&src.runbook_ref))),
        ("status", status_badge(&src.status)),
        ("created", esc(&src.created_at)),
        ("updated", esc(&src.updated_at)),
    ];
    if src.removal_requested_at.is_some() || src.removed_at.is_some() {
        lifecycle.push(("removal requested", opt(&src.removal_requested_at)));
        lifecycle.push(("removal requested by", opt(&src.removal_requested_by)));
        lifecycle.push(("removed", opt(&src.removed_at)));
    }
    if let Ok(doc) = &doc {
        lifecycle.push((
            "spec generation",
            if doc.spec.is_v2() {
                "v2 — collections".into()
            } else {
                format!(
                    "v1 — single shape {}",
                    doc.spec
                        .shape
                        .as_deref()
                        .map(shape_link)
                        .unwrap_or_default()
                )
            },
        ));
        lifecycle.push((
            "execution order",
            // The yaml vocabulary, not the Rust variant name.
            match doc.spec.execution_order() {
                munarium_runbooks::ExecutionOrder::StepMajor => "stepMajor (default)",
                munarium_runbooks::ExecutionOrder::CollectionMajor => "collectionMajor",
            }
            .to_string(),
        ));
        lifecycle.push((
            "completion",
            if doc.spec.completion.is_some() {
                "RAG completion on session turns".into()
            } else {
                "none (retrieval-only turns)".into()
            },
        ));
        if let Some(sources) = &doc.spec.sources {
            lifecycle.push((
                "sources",
                format!(
                    "container {} · prefix {}",
                    opt(&sources.container),
                    opt(&sources.prefix)
                ),
            ));
        }
    }

    let (versions_html, collections_html, retrieval_html, models_html) = match &info {
        Ok(info) => {
            let versions: Vec<String> = info
                .versions
                .iter()
                .map(|v| {
                    if *v == src.runbook_ref {
                        format!("<strong>{}</strong>", esc(v))
                    } else {
                        runbook_link(v)
                    }
                })
                .collect();
            let collections = html_table(
                &[
                    "collection",
                    "shape",
                    "level",
                    "compartments",
                    "active index",
                    "sources",
                ],
                &info
                    .collections
                    .iter()
                    .map(|c| {
                        vec![
                            collection_link(c.collection_id.as_deref(), &c.name),
                            shape_link(&c.shape_ref),
                            c.access_level.to_string(),
                            if c.compartments.is_empty() {
                                "—".into()
                            } else {
                                esc(&c.compartments.join(", "))
                            },
                            c.active_index
                                .as_deref()
                                .map(|i| format!("<code>{}</code>", esc(&short(i, 16))))
                                .unwrap_or_else(|| "none".into()),
                            c.source_count.to_string(),
                        ]
                    })
                    .collect::<Vec<_>>(),
            );
            (
                versions.join(" · "),
                collections,
                json_block(&info.retrieval),
                info.models
                    .as_ref()
                    .map(json_block)
                    .unwrap_or_else(|| "<div class=\"empty\">no models block — the tenant provider chain resolves every task</div>".into()),
            )
        }
        Err(e) => {
            let note = store_note(e);
            (note.clone(), note.clone(), note.clone(), note)
        }
    };

    let (plan_html, completion_html, bindings_html) = match &doc {
        Ok(doc) => {
            let plan = html_table(
                &["#", "step", "gate", "parameters"],
                &doc.spec
                    .steps
                    .iter()
                    .enumerate()
                    .map(|(i, step)| {
                        let (gate, params) = match step {
                            StepSpec::Cutover { approval } => (
                                if step.requires_approval() {
                                    "approval required".to_string()
                                } else {
                                    "—".into()
                                },
                                format!("approval: {}", opt(approval)),
                            ),
                            StepSpec::RetireOld { keep_versions } => {
                                ("—".into(), format!("keep_versions: {keep_versions}"))
                            }
                            _ => ("—".into(), "—".into()),
                        };
                        vec![i.to_string(), esc(step.name()), gate, params]
                    })
                    .collect::<Vec<_>>(),
            );
            let plan_note = if doc.spec.is_v2() {
                format!(
                    "<div class=\"legend\">v2: every step runs once per collection, {} — {} units per run</div>",
                    match doc.spec.execution_order() {
                        munarium_runbooks::ExecutionOrder::StepMajor => "step-major (all of step 1, then all of step 2, …)",
                        munarium_runbooks::ExecutionOrder::CollectionMajor => "collection-major (collection 1 through every step, then collection 2, …)",
                    },
                    doc.spec.steps.len() * doc.spec.collections.len()
                )
            } else {
                String::new()
            };
            let completion = match &doc.spec.completion {
                Some(c) => {
                    let verification = c
                        .verification
                        .as_ref()
                        .map(|v| {
                            format!(
                                "quotes: {} · citations: {} · max retries: {}",
                                v.quotes, v.citations, v.max_retries
                            )
                        })
                        .unwrap_or_else(|| "none".into());
                    format!(
                        "{}<h3>prompt template</h3>{}",
                        kv(&[
                            ("verification", verification),
                            (
                                "context char budget",
                                c.context_char_budget
                                    .map(|b| b.to_string())
                                    .unwrap_or_else(|| "engine default (16,000)".into())
                            ),
                        ]),
                        pre(&c.prompt_template)
                    )
                }
                None => "<div class=\"empty\">none</div>".into(),
            };
            let bindings = if doc.spec.is_v2() {
                html_table(
                    &[
                        "collection",
                        "filename prefix",
                        "media types",
                        "content hashes",
                    ],
                    &doc.spec
                        .collections
                        .iter()
                        .map(|c| {
                            let b = c.sources.clone().unwrap_or_default();
                            vec![
                                esc(&c.name),
                                opt(&b.filename_prefix),
                                if b.media_types.is_empty() {
                                    "—".into()
                                } else {
                                    esc(&b.media_types.join(", "))
                                },
                                if b.content_hashes.is_empty() {
                                    "—".into()
                                } else {
                                    format!("{} declared", b.content_hashes.len())
                                },
                            ]
                        })
                        .collect::<Vec<_>>(),
                )
            } else {
                "<div class=\"empty\">v1 runbooks bind by shape_ref on the source row</div>".into()
            };
            (format!("{plan_note}{plan}"), completion, bindings)
        }
        Err(e) => {
            let note = format!(
                r#"<div class="notice">stored yaml no longer parses: {}</div>"#,
                esc(e)
            );
            (note.clone(), note.clone(), note)
        }
    };

    let body = format!(
        r#"<div class="card">{}</div>
<h2>versions</h2><div class="card">{versions_html}</div>
<h2>collections</h2><div class="card">{collections_html}</div>
<h2>source bindings</h2><div class="card">{bindings_html}</div>
<h2>plan</h2><div class="card">{plan_html}</div>
<h2>retrieval</h2><div class="card">{retrieval_html}</div>
<h2>models</h2><div class="card">{models_html}</div>
<h2>completion</h2><div class="card">{completion_html}</div>
<h2>recent runs</h2><div class="card">{}</div>
<h2>applied yaml</h2><div class="card">{}</div>"#,
        kv(&lifecycle),
        runs_table(&runs),
        pre(&src.yaml),
    );
    render(
        &admin,
        "runbooks",
        &format!("Runbook {}", src.runbook_ref),
        &body,
    )
}

/// /admin/shapes/{shape_ref} — one published shape: metadata, the fact
/// schema, chunking/indexing, what depends on it, and the yaml.
pub(super) async fn shape(
    State(state): State<Arc<AppState>>,
    Path(shape_ref): Path<String>,
    headers: HeaderMap,
) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let tenant = admin.tenant.tenant_id.as_str();
    let src = match runbooks_api::op_shape_source(&state, tenant, &shape_ref).await {
        Ok(s) => s,
        Err(e) => return error_panel(&admin, "runbooks", "Shape", &e),
    };
    let Some(shape) = state.shapes.get(tenant, &shape_ref) else {
        return error_panel(
            &admin,
            "runbooks",
            "Shape",
            &munarium_core::KernelError::NotFound {
                kind: "shape",
                id: shape_ref,
            },
        );
    };
    let spec = &shape.doc.spec;
    let meta = kv(&[
        ("shape", format!("<code>{}</code>", esc(&shape.shape_ref()))),
        ("name", esc(&shape.doc.metadata.name)),
        ("version", shape.doc.metadata.version.to_string()),
        (
            "yaml hash",
            format!("<code>{}</code>", esc(&shape.yaml_hash)),
        ),
        ("created", opt(&src.created_at)),
        (
            "fact schema",
            if spec.fact.is_some() {
                "declared — claim bodies bearing this shape are validated against it".into()
            } else {
                "none — this shape constrains nothing at the claim gate".into()
            },
        ),
        (
            "supersession identity",
            spec.fact
                .as_ref()
                .and_then(|f| f.supersession.as_ref())
                .map(|s| esc(&s.identity.join(", ")))
                .unwrap_or_else(|| "—".into()),
        ),
        (
            "chunking",
            spec.chunking
                .as_ref()
                .map(|c| format!("{} · max {} chars", esc(&c.strategy), c.max_chars))
                .unwrap_or_else(|| "default (para@1 · 2000 chars)".into()),
        ),
        (
            "indexing",
            spec.indexing
                .as_ref()
                .map(|i| format!("rrf_k {} · candidate_n {}", i.rrf_k, i.candidate_n))
                .unwrap_or_else(|| "default (rrf_k 60 · candidate_n 50)".into()),
        ),
    ]);
    let schema_html = spec
        .fact
        .as_ref()
        .map(|f| json_block(&f.schema))
        .unwrap_or_else(|| "<div class=\"empty\">none</div>".into());

    // Dependents: collections bound to this shape and runbooks reaching
    // them (both pg-backed; the memory store gets the honest note).
    let dependents = match (
        crate::collections_api::op_list_collections(&state, tenant).await,
        runbooks_api::op_list_runbooks(&state, tenant, true).await,
    ) {
        (Ok(cols), Ok(runbooks)) => {
            let cols: Vec<String> = cols
                .iter()
                .filter(|c| c.shape_ref == shape_ref)
                .map(|c| collection_link(Some(&c.id), &c.name))
                .collect();
            let rbs: Vec<String> = runbooks
                .iter()
                .filter(|r| r.collections.iter().any(|c| c.shape_ref == shape_ref))
                .map(|r| runbook_link(&r.runbook_ref))
                .collect();
            kv(&[
                (
                    "collections",
                    if cols.is_empty() {
                        "none".into()
                    } else {
                        cols.join(", ")
                    },
                ),
                (
                    "runbooks",
                    if rbs.is_empty() {
                        "none".into()
                    } else {
                        rbs.join(", ")
                    },
                ),
            ])
        }
        (Err(e), _) | (_, Err(e)) => store_note(&e),
    };
    let yaml_note = if src.stored {
        String::new()
    } else {
        r#"<div class="legend">re-serialized from the registry — the memory store keeps no applied bytes</div>"#.into()
    };
    let body = format!(
        r#"<div class="card">{meta}</div>
<h2>fact schema</h2><div class="card">{schema_html}</div>
<h2>dependents</h2><div class="card">{dependents}</div>
<h2>applied yaml</h2><div class="card">{yaml_note}{}</div>
<div class="notice">Shapes are additive: this ref never changes content. A different schema is a new version, published through <code>POST /v1/shapes</code> / <code>mmctl apply -f</code>.</div>"#,
        pre(&src.yaml)
    );
    render(
        &admin,
        "runbooks",
        &format!("Shape {}", shape.shape_ref()),
        &body,
    )
}

/// /admin/chronology-rules/{name} — an applied rules asset, verbatim.
pub(super) async fn chronology(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let yaml = match state
        .load_chronology_rules_yaml(&admin.tenant.tenant_id, &name)
        .await
    {
        Ok(Some(y)) => y,
        Ok(None) => {
            return error_panel(
                &admin,
                "runbooks",
                "Chronology rules",
                &munarium_core::KernelError::NotFound {
                    kind: "chronology-rules",
                    id: name,
                },
            )
        }
        Err(e) => return error_panel(&admin, "runbooks", "Chronology rules", &e),
    };
    let rule_count = crate::chronology_api::parse_rules_doc(&yaml)
        .map(|d| d.spec.all_targets().len().to_string())
        .unwrap_or_else(|e| format!("no longer parses: {e}"));
    let body = format!(
        r#"<div class="card">{}</div>
<h2>applied yaml</h2><div class="card">{}</div>
<div class="notice">A memory version arms these rules by naming the asset in its creation metadata — <code>{{"chronology_rules": "{}"}}</code> — and the sixth gate then runs on every gated write of that lineage.</div>"#,
        kv(&[
            ("asset", format!("<code>{}</code>", esc(&name))),
            ("rule targets", esc(&rule_count)),
        ]),
        pre(&yaml),
        esc(&name)
    );
    render(
        &admin,
        "runbooks",
        &format!("Chronology rules {name}"),
        &body,
    )
}

/// /admin/runs/{run_id} — the checkpointed step machine, step by step,
/// with the gate-approval action where a step is awaiting one.
pub(super) async fn run(
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let run = match runbooks_api::op_get_run(&state, &admin.tenant.tenant_id, &run_id).await {
        Ok(r) => r,
        Err(e) => return error_panel(&admin, "runbooks", "Run", &e),
    };
    let steps = html_table(
        &["#", "step", "state", "detail", "action"],
        &run.steps
            .iter()
            .map(|s| {
                let detail = s
                    .detail
                    .as_ref()
                    .map(|d| {
                        format!(
                            "<details><summary>detail</summary>{}</details>",
                            json_block(d)
                        )
                    })
                    .unwrap_or_else(|| "—".into());
                let act = if s.state == "awaiting_approval" {
                    action(
                        &admin,
                        format!(
                            r#"<form class="action" method="post" action="/admin/runs/{}/steps/{}/approve">{}<label>rw token <input type="password" name="rw_token" autocomplete="off" placeholder="rw credential"></label><button type="submit">approve gate</button></form>"#,
                            esc(&run.run_id),
                            s.ordinal,
                            csrf_field(&admin)
                        ),
                    )
                } else {
                    "—".into()
                };
                vec![
                    s.ordinal.to_string(),
                    esc(&s.name),
                    state_badge(&s.state),
                    detail,
                    act,
                ]
            })
            .collect::<Vec<_>>(),
    );
    let body = format!(
        r#"<div class="card">{}</div>
<h2>steps</h2><div class="card">{steps}</div>
<div class="notice">Approving a gate is an <strong>rw</strong> operation (it can append ledger events when the run names a lineage), so the form asks for the rw credential each time and never stores it — the mgmt session alone cannot approve. It is the same call as <code>POST /v1/runs/{}/steps/&lt;ordinal&gt;/approve</code>.</div>"#,
        kv(&[
            ("run", format!("<code>{}</code>", esc(&run.run_id))),
            ("runbook", runbook_link(&run.runbook_ref)),
            ("state", state_badge(&run.state)),
            (
                "lineage",
                run.version_id
                    .as_deref()
                    .map(|v| format!(
                        "<code>{}</code> — every transition is a ledger event",
                        esc(v)
                    ))
                    .unwrap_or_else(|| "none (transitions persist in runbook_steps only)".into()),
            ),
        ]),
        esc(&run.run_id)
    );
    render(&admin, "runbooks", &format!("Run {}", run.run_id), &body)
}

#[derive(Debug, serde::Deserialize)]
pub struct ApproveForm {
    #[serde(default)]
    _csrf: String,
    #[serde(default)]
    rw_token: String,
}

pub(super) async fn approve(
    State(state): State<Arc<AppState>>,
    Path((run_id, ordinal)): Path<(String, usize)>,
    headers: HeaderMap,
    Form(form): Form<ApproveForm>,
) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    if admin.view_only {
        return notice(
            &admin,
            "runbooks",
            "Run",
            "view-only passthrough — approve on the server's own /admin",
        );
    }
    if !csrf_ok(&admin, &form._csrf) {
        return stale_form(&admin, "runbooks", "Run");
    }
    let rw = match rw_credential(&state, &admin, &form.rw_token) {
        Ok(ctx) => ctx,
        Err(msg) => return notice(&admin, "runbooks", "Run", &msg),
    };
    match runbooks_api::op_approve_step(&state, &rw.tenant_id, &run_id, ordinal).await {
        Ok(_) => Redirect::to(&format!("/admin/runs/{run_id}")).into_response(),
        Err(e) => notice(&admin, "runbooks", "Run", &e.to_string()),
    }
}
