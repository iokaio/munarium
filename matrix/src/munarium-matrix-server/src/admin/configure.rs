// SPDX-License-Identifier: Apache-2.0
//! The configure loop.
//!
//! **The repository is the source of truth.** This is the load-bearing
//! sentence. The server tree deleted its own `/admin/authoring` pages in
//! August because a form that ends in a download served no purpose beside the
//! CLI, and that judgement stands: this loop earns its place only by doing
//! three things a CLI cannot.
//!
//! 1. **Seed a draft from a live introspect.** `mxctl` can print a schema; it
//!    cannot put the schema in front of you while you write `spec.reads`
//!    against it. A draft here starts from the tables and columns the source
//!    actually exposes *to the effective principal* — so an author writes
//!    against what is there instead of guessing, and a column the role cannot
//!    see never reaches a contract.
//! 2. **Diff against the applied version, in one view.** Applied asset
//!    versions are immutable, so the question at author time is always "what
//!    would change", and answering it from two terminal windows is where
//!    mistakes live.
//! 3. **Flag the drift.** Applying in place is legitimate and sometimes
//!    necessary; pretending it did not happen is not. After an apply-in-place
//!    the deployment is marked *drifted from git* until the exported bundle
//!    lands, and the flag is on the page an operator already looks at.
//!
//! **Export is the default and apply is the exception**, in that order on the
//! page, because the ordinary path is a commit and a review.

use super::{action, admin_auth, chrome, csrf_field, error_page, render, rw_credential, writable};
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use std::sync::Arc;

const KINDS: &[(&str, &str)] = &[
    ("datasources", "DataSource"),
    ("contracts", "QueryContract"),
    ("metricviews", "MetricView"),
    ("dataviews", "DataView"),
    ("mappings", "ClaimMapping"),
];

fn kind_of(route: &str) -> Option<&'static str> {
    KINDS.iter().find(|(r, _)| *r == route).map(|(_, k)| *k)
}

// ---------------------------------------------------------------------------
// registry browser
// ---------------------------------------------------------------------------

pub async fn registry(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let tenant = &admin.caller.tenant;
    let mut body = String::new();
    // The drift flag (the console's exit gate: "the drift flag sets and clears").
    // Read once for the page; an asset whose latest apply came through this
    // console is marked until a later apply arrives by another plane.
    let drifted = state.store.drifted_assets(tenant).await.unwrap_or_default();
    if !drifted.is_empty() {
        let items: Vec<String> = drifted
            .iter()
            .map(|d| {
                format!(
                    "<code>{}</code> (decision <code>{}</code>, {})",
                    chrome::esc(&d.asset_ref),
                    chrome::esc(d.decision_id.as_deref().unwrap_or("—")),
                    chrome::esc(&d.applied_at)
                )
            })
            .collect();
        body.push_str(&format!(
            r#"<div class="notice"><strong>Drifted from git:</strong> {} — applied in place from this console; the flag clears when the exported bundle is applied from the repository (by <code>mxctl</code>, CI or the API).</div>"#,
            items.join("; ")
        ));
    }
    for (route, kind) in KINDS {
        body.push_str(&format!("<h2>{}</h2>", chrome::esc(route)));
        // `latest_only: false` — the history IS the point of this page. A
        // listing that silently showed only the latest would be
        // indistinguishable from a registry with no history at all, which is
        // exactly the defect the Python client shipped with on the same day
        // this was written.
        match state.store.list_assets(tenant, Some(kind), false).await {
            Ok(assets) if assets.is_empty() => {
                body.push_str(r#"<div class="empty">none registered</div>"#)
            }
            Ok(assets) => {
                let rows: Vec<Vec<String>> = assets
                    .iter()
                    .map(|a| {
                        let git = match drifted.iter().find(|d| d.asset_ref == a.asset_ref()) {
                            Some(d) => format!(
                                r#"{} decision <code>{}</code>"#,
                                chrome::state_badge("drifted"),
                                chrome::esc(d.decision_id.as_deref().unwrap_or("—"))
                            ),
                            None => "—".to_string(),
                        };
                        vec![
                            chrome::link(
                                &format!("/admin/registry/{route}/{}", a.name),
                                &a.asset_ref(),
                            ),
                            chrome::esc(&chrome::short(&a.yaml_hash, 20)),
                            chrome::esc(&a.created_at),
                            git,
                        ]
                    })
                    .collect();
                body.push_str(&chrome::table(&["asset", "bytes", "applied", "git"], &rows));
            }
            Err(e) => body.push_str(&format!(
                r#"<div class="notice">{}</div>"#,
                chrome::esc(&e.to_string())
            )),
        }
    }
    body.push_str(
        r#"<div class="legend">Every version ever applied, newest first. An applied version is immutable — a correction is a new version, which is why nothing here is editable.</div>"#,
    );
    render(&admin, "registry", "registry", &body)
}

pub async fn asset(
    State(state): State<Arc<AppState>>,
    Path((route, name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let Some(kind) = kind_of(&route) else {
        return error_page(&admin, "registry", &route, "unknown asset kind");
    };
    let tenant = &admin.caller.tenant;
    let versions = match state.store.list_assets(tenant, Some(kind), false).await {
        Ok(a) => a.into_iter().filter(|a| a.name == name).collect::<Vec<_>>(),
        Err(e) => return error_page(&admin, "registry", &name, &e.to_string()),
    };
    if versions.is_empty() {
        return error_page(&admin, "registry", &name, "not found");
    }
    let current = match state.store.get_asset(tenant, kind, &name).await {
        Ok(a) => a,
        Err(e) => return error_page(&admin, "registry", &name, &e.to_string()),
    };

    let mut body = format!("<h2>{}</h2>", chrome::esc(&current.asset_ref()));
    if let Some(d) = state
        .store
        .drifted_assets(tenant)
        .await
        .unwrap_or_default()
        .into_iter()
        .find(|d| d.asset_ref == current.asset_ref())
    {
        body.push_str(&format!(
            r#"<div class="notice">{} This version was applied in place from the console under decision <code>{}</code> ({}) and is <strong>drifted from git</strong> until the exported bundle is applied from the repository.</div>"#,
            chrome::state_badge("drifted"),
            chrome::esc(d.decision_id.as_deref().unwrap_or("—")),
            chrome::esc(&d.applied_at)
        ));
    }
    body.push_str(&chrome::pre(&current.yaml));

    // The diff against the version before, when there is one. This is the
    // question an operator asks about a registry: not "what does v7 say" but
    // "what changed in v7".
    if versions.len() > 1 {
        let previous = versions
            .iter()
            .filter(|v| v.version < current.version)
            .max_by_key(|v| v.version);
        if let Some(prev) = previous {
            body.push_str(&format!(
                "<h2>v{} → v{}</h2>",
                prev.version, current.version
            ));
            body.push_str(&chrome::diff(&prev.yaml, &current.yaml));
        }
    }

    body.push_str("<h2>versions</h2>");
    let rows: Vec<Vec<String>> = versions
        .iter()
        .map(|v| {
            vec![
                format!("v{}", v.version),
                chrome::esc(&v.yaml_hash),
                chrome::esc(&v.created_at),
            ]
        })
        .collect();
    body.push_str(&chrome::table(&["version", "bytes", "applied"], &rows));
    render(&admin, "registry", &name, &body)
}

// ---------------------------------------------------------------------------
// author
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
pub struct DraftForm {
    #[serde(default)]
    pub csrf: String,
    #[serde(default)]
    pub yaml: String,
    /// Set when the operator pressed "seed from source" rather than
    /// "validate": the draft is regenerated from a live introspect.
    #[serde(default)]
    pub seed_source: String,
    #[serde(default)]
    pub rw_token: String,
}

pub async fn author(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let sources = state
        .store
        .list_assets(&admin.caller.tenant, Some("DataSource"), true)
        .await
        .unwrap_or_default();
    let body = author_body(&admin, "", &sources, "");
    render(&admin, "author", "author", &body)
}

fn author_body(
    admin: &super::AdminCtx,
    yaml: &str,
    sources: &[munarium_matrix_store::registry::StoredAsset],
    result: &str,
) -> String {
    let options: String = sources
        .iter()
        .map(|s| format!("<option>{}</option>", chrome::esc(&s.name)))
        .collect();
    let seed = if sources.is_empty() {
        r#"<div class="legend">No DataSource is registered, so there is nothing to introspect. Paste one below and apply it first.</div>"#.to_string()
    } else {
        format!(
            r#"<form method="post" action="/admin/author">{}
<label for="seed_source">seed a contract draft by introspecting</label>
<div style="display:flex;gap:.4rem"><select id="seed_source" name="seed_source">{options}</select>
<input name="rw_token" type="password" placeholder="rw token" autocomplete="off" style="width:12rem">
<button type="submit">introspect</button></div>
<div class="legend">Introspection reads the schema as the source's effective principal, so a column that principal cannot see never reaches the draft.</div>
</form>"#,
            csrf_field(admin)
        )
    };
    format!(
        r#"{seed}
<h2>draft</h2>
<form method="post" action="/admin/author">{csrf}
<textarea name="yaml" rows="22" spellcheck="false">{yaml}</textarea>
<div style="margin-top:.5rem"><button type="submit">validate and diff</button></div>
</form>
{result}"#,
        csrf = csrf_field(admin),
        yaml = chrome::esc(yaml),
    )
}

/// `POST /admin/author` — validate a draft, or seed one from an introspect.
///
/// Validation is Matrix's own: the draft is posted to the same validators
/// `mxctl validate` and `POST /v1/assets/validate` run. A console carrying its
/// own copy of the rules would drift from the service that enforces them, and
/// the drift would show up as a draft that is green here and refused there.
pub async fn draft(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Form(form): axum::extract::Form<DraftForm>,
) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(a) => a,
        Err(r) => return r,
    };
    if let Err(msg) = writable(&admin, &headers, &form.csrf) {
        return super::notice(&admin, "author", "author", &msg);
    }
    let tenant = &admin.caller.tenant;
    let sources = state
        .store
        .list_assets(tenant, Some("DataSource"), true)
        .await
        .unwrap_or_default();

    // --- seed ---------------------------------------------------------------
    if !form.seed_source.trim().is_empty() {
        let rw = match rw_credential(&state, &admin, &form.rw_token) {
            Ok(c) => c,
            Err(e) => {
                return render(
                    &admin,
                    "author",
                    "author",
                    &author_body(
                        &admin,
                        &form.yaml,
                        &sources,
                        &format!(r#"<div class="notice">{}</div>"#, chrome::esc(&e)),
                    ),
                )
            }
        };
        let report = crate::rest::op_introspect(
            &state,
            &rw,
            form.seed_source.trim(),
            None,
            crate::rest::VIA_ADMIN_UI,
        )
        .await;
        return match report {
            Ok(r) => {
                let yaml = seed_contract(&r);
                render(
                    &admin,
                    "author",
                    "author",
                    &author_body(&admin, &yaml, &sources, &posture_panel(&r)),
                )
            }
            Err(e) => render(
                &admin,
                "author",
                "author",
                &author_body(
                    &admin,
                    &form.yaml,
                    &sources,
                    &format!(r#"<div class="notice">{}</div>"#, chrome::esc(&e.0.detail)),
                ),
            ),
        };
    }

    // --- validate + diff ----------------------------------------------------
    let parsed = munarium_matrix_types::parse_asset(&form.yaml);
    let mut result = String::from("<h2>validation</h2>");
    let findings = match &parsed {
        Ok(a) => a.validate(),
        Err(e) => {
            // A draft that does not parse has no findings to show, and saying
            // "0 findings" about it would read as a pass.
            result.push_str(&format!(
                r#"<div class="notice">{}</div>"#,
                chrome::esc(&e.to_string())
            ));
            return render(
                &admin,
                "author",
                "author",
                &author_body(&admin, &form.yaml, &sources, &result),
            );
        }
    };
    if findings.is_empty() {
        result.push_str(&format!(
            r#"<div class="legend">{} no findings</div>"#,
            chrome::state_badge("ok")
        ));
    } else {
        let rows: Vec<Vec<String>> = findings
            .iter()
            .map(|f| {
                vec![
                    chrome::state_badge(if munarium_matrix_types::validate::is_error(f) {
                        "failed"
                    } else {
                        "running"
                    }),
                    chrome::esc(&f.code),
                    chrome::esc(&f.message),
                ]
            })
            .collect();
        result.push_str(&chrome::table(&["", "code", "message"], &rows));
        // Not every finding blocks. Three codes are advisory, and a console
        // that treated "findings is non-empty" as invalid would refuse three
        // healthy assets — the exact disagreement-with-the-service this loop
        // is meant to avoid.
        result.push_str(
            r#"<div class="legend">An advisory finding does not block an apply. The service decides that, not this page.</div>"#,
        );
    }

    if munarium_matrix_types::validate::is_valid(&findings) {
        {
            let parsed = parsed.expect("checked above");
            let (kind, name) = (parsed.kind(), parsed.metadata().name.clone());
            result.push_str("<h2>against what is applied</h2>");
            match state.store.get_asset(tenant, kind, &name).await {
                Ok(applied) if applied.yaml == form.yaml => result.push_str(
                    r#"<div class="legend">byte-identical to the applied version — applying would change nothing</div>"#,
                ),
                Ok(applied) => {
                    result.push_str(&format!(
                        r#"<div class="legend">applied: {}</div>"#,
                        chrome::esc(&applied.asset_ref())
                    ));
                    result.push_str(&chrome::diff(&applied.yaml, &form.yaml));
                }
                Err(_) => result.push_str(&format!(
                    r#"<div class="legend">nothing named {} is applied yet — this would be its first version</div>"#,
                    chrome::esc(&name)
                )),
            }

            // Export first, apply second, deliberately: the ordinary path is a
            // commit and a review.
            result.push_str("<h2>then</h2>");
            let hidden = format!(
                r#"<input type="hidden" name="yaml" value="{}">"#,
                chrome::esc(&form.yaml)
            );
            result.push_str(&action(
                &admin,
                "/admin/author/export",
                "export a bundle to commit",
                &hidden,
                false,
            ));
            result.push_str(&action(
                &admin,
                "/admin/author/apply",
                "apply in place",
                &format!(
                    r#"{hidden}<input name="rw_token" type="password" placeholder="rw token" autocomplete="off" style="width:12rem"><input name="decision_id" placeholder="decision id" style="width:11rem">"#
                ),
                true,
            ));
            result.push_str(
                r#"<div class="legend">Applying in place is legitimate and sometimes necessary. It flags the deployment as drifted from git until the exported bundle lands.</div>"#,
            );
        }
    }
    render(
        &admin,
        "author",
        "author",
        &author_body(&admin, &form.yaml, &sources, &result),
    )
}

/// A contract draft seeded from a live introspect.
///
/// Deliberately incomplete: the statement, the parameters and the verified
/// questions are the author's, and generating a plausible-looking statement
/// would be the console asserting it knows what the contract means. What it
/// CAN supply without guessing is exactly what the source reported — the
/// tables and the columns, as the effective principal sees them.
fn seed_contract(r: &munarium_matrix_types::dto::IntrospectResponse) -> String {
    let mut out = String::from(
        "apiVersion: munarium.ioka.io/v1\nkind: QueryContract\nmetadata:\n  name: CHANGE-ME\n  version: 1\nspec:\n",
    );
    out.push_str(&format!("  source: {}\n", r.source));
    out.push_str("  description: >-\n    CHANGE-ME: what this answers, and what its words mean.\n");
    out.push_str("  parameters: {}\n");
    out.push_str(
        "  statementByDialect:\n    # CHANGE-ME: one SELECT per dialect, :named parameters only.\n",
    );
    out.push_str("  reads:\n    tables: [");
    out.push_str(
        &r.tables
            .iter()
            .map(|t| t.name.clone())
            .collect::<Vec<_>>()
            .join(", "),
    );
    out.push_str("]\n    columns:\n");
    for t in &r.tables {
        for c in &t.columns {
            match &c.logical_type {
                // A column canon@1 does not model is listed and commented out
                // rather than omitted: silence would look like the column does
                // not exist, and an author would go looking for it in the
                // source instead of learning it cannot be used.
                None => out.push_str(&format!(
                    "      # {}.{}  — source type {} maps to no canon@1 type; it cannot be read\n",
                    t.name, c.name, c.source_type
                )),
                Some(lt) => out.push_str(&format!(
                    "      - {}  # {}.{} {}\n",
                    c.name, t.name, c.name, lt
                )),
            }
        }
    }
    out.push_str("  result:\n    columns: {}\n    columnOrder: []\n    orderBy: []\n");
    out.push_str("  policy:\n    authorization: source_native\n");
    out
}

fn posture_panel(r: &munarium_matrix_types::dto::IntrospectResponse) -> String {
    let rows: Vec<Vec<String>> = r
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
    format!(
        "<h2>role posture</h2>{}{}",
        chrome::kv(&[
            ("principal", chrome::esc(&r.posture.principal)),
            (
                "verdict",
                chrome::state_badge(if r.posture.ok { "ok" } else { "failed" })
            ),
            (
                "schema fingerprint",
                chrome::opt(r.schema_fingerprint.as_deref())
            ),
        ]),
        chrome::table(&["", "check", "", "detail"], &rows)
    )
}

// ---------------------------------------------------------------------------
// export / apply
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
pub struct ApplyForm {
    #[serde(default)]
    pub csrf: String,
    #[serde(default)]
    pub yaml: String,
    #[serde(default)]
    pub rw_token: String,
    #[serde(default)]
    pub decision_id: String,
}

/// `POST /admin/author/export` — the bundle to commit.
///
/// A hash manifest beside the bytes, in the shape `mxctl apply` reads. The
/// manifest is the point: a bundle whose bytes changed on the way to the
/// repository is a bundle that applies something nobody reviewed.
pub async fn export(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Form(form): axum::extract::Form<ApplyForm>,
) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(a) => a,
        Err(r) => return r,
    };
    if let Err(msg) = writable(&admin, &headers, &form.csrf) {
        return super::notice(&admin, "author", "export", &msg);
    }
    let Ok(parsed) = munarium_matrix_types::parse_asset(&form.yaml) else {
        return super::notice(&admin, "author", "export", "that draft does not parse");
    };
    use sha2::Digest as _;
    let hash = hex::encode(sha2::Sha256::digest(form.yaml.as_bytes()));
    let filename = format!(
        "{}.{}.yaml",
        parsed.kind().to_lowercase(),
        parsed.metadata().name
    );
    let manifest = format!("sha256:{hash}  {filename}\n");
    let body = format!(
        r#"<h2>{filename}</h2>{}<h2>MANIFEST</h2>{}<div class="legend">Commit both. <code>mxctl apply {filename}</code> against a Matrix that has verified the manifest produces exactly the version this page diffed — that equality is what makes exporting the default path rather than a formality.</div>"#,
        chrome::pre(&form.yaml),
        chrome::pre(&manifest),
    );
    render(&admin, "author", "export", &body)
}

/// `POST /admin/author/apply` — apply in place, and say so afterwards.
pub async fn apply(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Form(form): axum::extract::Form<ApplyForm>,
) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(a) => a,
        Err(r) => return r,
    };
    if let Err(msg) = writable(&admin, &headers, &form.csrf) {
        return super::notice(&admin, "author", "apply", &msg);
    }
    if form.decision_id.trim().is_empty() {
        return super::notice(
            &admin,
            "author",
            "apply",
            "an apply in place needs a decision id — the operator's record of why the \
             repository is not the thing that changed",
        );
    }
    let rw = match rw_credential(&state, &admin, &form.rw_token) {
        Ok(c) => c,
        Err(e) => return super::notice(&admin, "author", "apply", &e),
    };
    match crate::rest::op_apply_asset(
        &state,
        &rw,
        &form.yaml,
        Some(form.decision_id.trim().to_string()),
        crate::rest::VIA_ADMIN_UI,
    )
    .await
    {
        Ok(outcome) => {
            let body = format!(
                r#"<div class="legend">{} applied {}{}</div>
<div class="notice">This deployment is now <strong>drifted from git</strong> for {}: the applied bytes exist only here until the exported bundle lands in the repository. The drift is recorded in the journal under decision <code>{}</code>.</div>"#,
                chrome::state_badge("ok"),
                chrome::esc(&outcome.asset_ref),
                if outcome.unchanged {
                    " (unchanged — the same bytes were already applied)"
                } else {
                    ""
                },
                chrome::esc(&outcome.asset_ref),
                chrome::esc(form.decision_id.trim()),
            );
            render(&admin, "author", "applied", &body)
        }
        Err(e) => super::notice(&admin, "author", "apply", &e.0.detail),
    }
}
