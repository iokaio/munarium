// SPDX-License-Identifier: Apache-2.0
//! `/admin/storage` — the tiered strategy, made visible.
//!
//! stage 3 adds the MINIMUM view needed to understand the architecture before
//! artifacts have proven value (§13.4): a strategy banner, four tier cards, and
//! the plane-expectation and node tables. There are deliberately **no forms**.
//! Eviction, deletion, quarantine, prewarm, rebuild, binding, activation,
//! selector and mode changes are audited operations with their own safety
//! arguments; putting a button on any of them before the artifact data exists
//! to justify a design would be adding a control panel to a system nobody has
//! operated yet.
//!
//! Three rules this page keeps:
//!
//! - **No network in the render path.** No Blob listing, no HEAD, no hydration,
//!   no artifact open. L2 existence is shown as "as of last verification",
//!   because that is the only honest thing a page can say without a round trip
//!   it must not make while an operator is trying to see what is wrong.
//! - **Two clocks, shown separately.** The durable read has one timestamp and
//!   every node snapshot has its own age, so "fresh page, stale node" is
//!   legible. A missing node is `unknown` — never zero, never absent, never
//!   healthy.
//! - **Nothing sensitive.** No tenant ids, no source or query text, no
//!   credentials or SAS, no filesystem roots, no full internal URIs.

use std::sync::Arc;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;

use super::{admin_auth, html_table, kv, render, state_badge, store_note};
use crate::state::AppState;
use crate::storage_api::{self, SettingsView, StorageSnapshot, STALE_AFTER_SECS};

pub(super) async fn storage(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let admin = match admin_auth(&state, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let tenant = admin.tenant.tenant_id.as_str();

    let body = match storage_api::op_storage_snapshot(&state, tenant).await {
        Ok(snap) => render_body(&snap),
        // Independent-section degradation: a failed read renders as a note, not
        // as a blank page. An operator loading this during an incident needs to
        // know the read failed, not to see nothing and guess.
        Err(e) => store_note(&e),
    };
    render(&admin, "storage", "Tiered storage", &body)
}

fn render_body(s: &StorageSnapshot) -> String {
    let mut out = String::new();
    out.push_str(&strategy_banner(s));
    out.push_str(&shadow_card(s));
    out.push_str(&settings_card(&s.settings));
    out.push_str(&tier_cards(s));
    out.push_str(&expectations_table(s));
    out.push_str(&nodes_table(s));
    out.push_str(&rollout_table(s));
    out.push_str(&legend());
    out
}

fn strategy_banner(s: &StorageSnapshot) -> String {
    // PostgreSQL mode is rendered as disabled/reference-only rather than
    // disappearing: a page that vanishes when a feature is off cannot tell an
    // operator that the feature is off.
    let posture = match s.this_replica_mode.as_str() {
        "postgres" => "PostgreSQL serves everything. The datastore tier is present but not in use.",
        "mirror" => "PostgreSQL serves; datastore artifacts are also built and verified.",
        "shadow" => "PostgreSQL serves and controls every response; datastore queries are sampled for comparison only.",
        "datastore" => "The datastore serves the scopes the rollout selector routes to it. PostgreSQL serves the rest.",
        _ => "Unrecognised mode; treating as PostgreSQL.",
    };
    format!(
        r#"<h2>Strategy</h2>
<div class="card">
  {}
  <p>{}</p>
  <p class="legend">Rollback to PostgreSQL is always available: it is a selector or configuration change, never a data migration.</p>
</div>"#,
        kv(&[
            ("mode (this replica)", s.this_replica_mode.clone()),
            (
                "scopes on datastore",
                s.rollout_datastore_scopes.to_string()
            ),
            (
                "this replica admits",
                if s.readiness.admits {
                    "yes".to_string()
                } else {
                    // Escaped: `kv` values are trusted HTML, and these
                    // strings are built from error Display output (I/O and
                    // store errors carry paths and URIs) — the one sink on
                    // the dashboard that took untrusted text unescaped.
                    format!(
                        "NO — warming; blocking: {}",
                        crate::charts::esc(&s.readiness.blocking.join(", "))
                    )
                },
            ),
            ("durable read at", s.read_at.clone()),
        ]),
        html_escape(posture)
    )
}

/// The shadow plane's counters (§13.4): sampling, comparisons and the drops.
///
/// Rendered only when a plane exists, and in `shadow` mode a MISSING plane is
/// itself rendered — an operator who enabled the mode and got no plane needs
/// the page to say so, not to look like `postgres` with extra steps. No query
/// text appears anywhere in these numbers; a comparison has nowhere to hold
/// one.
fn shadow_card(s: &StorageSnapshot) -> String {
    let Some(v) = &s.shadow else {
        if s.this_replica_mode == "shadow" {
            return "<h2>Shadow comparisons</h2>
<div class=\"card\"><p class=\"bad\">                    <strong>Shadow mode is configured but this replica has no shadow                     plane.</strong> A prerequisite was missing at startup — the local                     root, the artifact store, or valid watermarks; the startup log                     names which. PostgreSQL serves regardless.</p></div>
"
                .to_string();
        }
        return String::new();
    };
    let sampling = if v.sample_one_in == 0 {
        "configured OFF (MUNARIUM_DATASTORE_SHADOW_SAMPLE_RATE unset or 0)".to_string()
    } else {
        format!("one request in {}", v.sample_one_in)
    };
    let overlap = match v.mean_fused_overlap {
        // No measurement is not perfect agreement and not disaster; it is
        // nothing yet, and the page says so in words.
        None => "no completed comparisons yet".to_string(),
        Some(m) => format!("{:.3}", m),
    };
    let corrupting = if v.corrupting > 0 {
        format!(
            "<p class=\"bad\"><strong>{} corrupting comparison(s)</strong> — a text-hash or              provenance mismatch. No tolerance band absorbs this; the log carries the              fingerprints.</p>",
            v.corrupting
        )
    } else {
        String::new()
    };
    format!(
        "<h2>Shadow comparisons</h2>
<div class=\"card\">
{}{}
</div>
",
        kv(&[
            ("sampling", sampling),
            ("completed", v.completed.to_string()),
            ("mean fused overlap", overlap),
            ("dropped (shed)", v.dropped.to_string()),
            ("timeout", v.timeout.to_string()),
            (
                "rejected (no binding / unsupported)",
                v.rejected.to_string()
            ),
            ("errors", v.error.to_string()),
            ("not sampled", v.not_sampled.to_string()),
        ]),
        corrupting
    )
}

/// The read-only settings card.
///
/// Every feature with its state and its REASON, so an operator can see at a
/// glance which parts of the datastore tier this process can actually use and
/// what would have to change to enable one. There are no controls: every
/// setting here is a deployment variable, and a button that appeared to change
/// one would be lying about where the value lives.
fn settings_card(v: &SettingsView) -> String {
    let mut out = String::from("<h2>Datastore settings</h2>\n<div class=\"card\">\n");

    // A mismatch between configured and effective is the single most important
    // thing on this page, so it leads rather than sitting in a table.
    if v.must_refuse_startup {
        out.push_str(&format!(
            "<p class=\"bad\"><strong>Serving cannot start.</strong> {}</p>",
            html_escape(
                v.degraded_because
                    .as_deref()
                    .unwrap_or("configuration refused")
            )
        ));
    } else if let Some(why) = &v.degraded_because {
        out.push_str(&format!(
            "<p class=\"warn\"><strong>Degraded: configured {}, running {}.</strong> {}</p>",
            html_escape(&v.configured_mode),
            html_escape(&v.effective_mode),
            html_escape(why)
        ));
    }

    out.push_str(&kv(&[
        ("configured mode", v.configured_mode.clone()),
        ("effective mode", v.effective_mode.clone()),
        ("artifact store", v.artifact_store.clone()),
    ]));

    let rows: Vec<Vec<String>> = v
        .capabilities
        .iter()
        .map(|c| {
            vec![
                html_escape(&c.name),
                if c.enabled {
                    "<span class=\"ok\">enabled</span>".to_string()
                } else {
                    "<span class=\"warn\">disabled</span>".to_string()
                },
                html_escape(&c.detail),
            ]
        })
        .collect();
    out.push_str(&html_table(&["feature", "state", "why"], &rows));

    if !v.blocking.is_empty() {
        out.push_str("<p class=\"bad\"><strong>Configuration refused:</strong></p><ul>");
        for b in &v.blocking {
            out.push_str(&format!("<li>{}</li>", html_escape(b)));
        }
        out.push_str("</ul>");
    }

    out.push_str(
        "<p class=\"legend\">Read-only. Every value here comes from a deployment variable \
         resolved once at startup; changing one is a deployment event, not a control on this \
         page.</p></div>",
    );
    out
}

fn tier_cards(s: &StorageSnapshot) -> String {
    let t = &s.truth;
    format!(
        r#"<h2>Tiers</h2>
<div class="card">
  <h3>Truth — PostgreSQL</h3>
  {}
</div>
<div class="card">
  <h3>L2 — shared object store</h3>
  {}
  <p class="legend">Counts come from the catalog. Object existence is <em>as of last verification</em>; this page never contacts the store.</p>
</div>
<div class="card">
  <h3>L1 — per-node local cache</h3>
  <p class="legend">Process-local and disposable. Per-node figures are in the node table below; there is no cluster-wide L1 total, because there is no cluster-wide L1.</p>
</div>
<div class="card">
  <h3>L0 — per-node open handles</h3>
  <p class="legend">Process-local. Mapped bytes and resident bytes are different things and are reported separately by each node.</p>
</div>"#,
        kv(&[
            ("artifacts", t.artifacts.to_string()),
            ("verified", t.verified.to_string()),
            ("sealed (not yet usable)", t.sealed.to_string()),
            ("failed", t.failed.to_string()),
            ("retired", t.retired.to_string()),
            ("bindings", s.bindings.to_string()),
            ("build attempts running", s.attempts_running.to_string()),
            (
                "build attempts lease-expired",
                s.attempts_expired.to_string()
            ),
        ]),
        kv(&[
            ("catalogued bytes", human_bytes(t.bytes)),
            ("artifacts verified", t.verified.to_string()),
        ]),
    )
}

fn expectations_table(s: &StorageSnapshot) -> String {
    if s.expectations.is_empty() {
        return r#"<h2>Deployment expectations</h2>
<div class="card">
  <p>None recorded.</p>
  <p class="legend">Without an expectation row a cutover has no gate: heartbeats can prove a process exists, but their absence cannot prove how many ought to. Serving mode requires one positive, exact-revision row per traffic plane.</p>
</div>"#
            .to_string();
    }
    let headers = [
        "plane",
        "revision",
        "min fresh",
        "min open",
        "fraction",
        "mode",
        "observed fresh",
        "wrong revision",
        "warming",
        "stale",
        "gen",
        "verified",
    ];
    let rows: Vec<Vec<String>> = s
        .expectations
        .iter()
        .map(|e| {
            vec![
                html_escape(&e.plane),
                short(&e.deployment_revision),
                e.minimum_fresh_nodes.to_string(),
                e.minimum_open_nodes.to_string(),
                e.minimum_open_fraction
                    .map(|f| format!("{:.0}%", f * 100.0))
                    .unwrap_or_else(|| "—".into()),
                html_escape(&e.required_mode),
                // The comparison an operator actually needs: expected beside
                // observed, with a deficit called out rather than left to
                // arithmetic.
                deficit_cell(e.observed_fresh, e.minimum_fresh_nodes as i64),
                e.observed_wrong_revision.to_string(),
                e.observed_warming.to_string(),
                e.observed_stale.to_string(),
                e.generation.to_string(),
                e.verified_age_secs
                    .map(|a| format!("{}s ago", a))
                    .unwrap_or_else(|| "never".into()),
            ]
        })
        .collect();
    format!(
        "<h2>Deployment expectations</h2><div class=\"card\">{}</div>",
        html_table(&headers, &rows)
    )
}

fn nodes_table(s: &StorageSnapshot) -> String {
    if s.nodes.is_empty() {
        return r#"<h2>Nodes</h2>
<div class="card">
  <p>No node has reported.</p>
  <p class="legend">This is <em>unknown</em>, not <em>empty</em>: a fleet cannot be judged by who happens to answer. Compare against the expectations above.</p>
</div>"#
            .to_string();
    }
    let headers = [
        "node",
        "plane",
        "revision",
        "mode",
        "admission",
        "seen",
        "blocking scopes",
        "L1 used/budget",
        "L0 used/budget",
        "local root",
    ];
    let rows: Vec<Vec<String>> = s
        .nodes
        .iter()
        .map(|n| {
            vec![
                short(&n.node_id),
                html_escape(&n.plane),
                short(&n.deployment_revision),
                html_escape(&n.retrieval_mode),
                state_badge(&n.admission_state),
                freshness(n.seen_age_secs),
                n.blocking_scopes.to_string(),
                ratio(n.l1_used_bytes, n.l1_budget_bytes),
                ratio(n.l0_used_bytes, n.l0_budget_bytes),
                n.local_root_health
                    .as_deref()
                    .map(html_escape)
                    .unwrap_or_else(|| "—".into()),
            ]
        })
        .collect();
    format!(
        "<h2>Nodes</h2><div class=\"card\">{}</div>",
        html_table(&headers, &rows)
    )
}

fn rollout_table(s: &StorageSnapshot) -> String {
    if s.rollout.is_empty() {
        return r#"<h2>Rollout selector</h2><div class="card"><p>No scope is routed to the datastore. Everything serves from PostgreSQL, which is the default for a scope with no row at all.</p></div>"#.to_string();
    }
    let headers = [
        "scope kind",
        "scope",
        "serving",
        "prewarm staged",
        "required versions",
        "gen",
    ];
    let rows: Vec<Vec<String>> = s
        .rollout
        .iter()
        .map(|r| {
            vec![
                html_escape(&r.scope_kind),
                html_escape(&r.scope_id),
                state_badge(&r.serving),
                if r.prewarm_staged {
                    "yes".into()
                } else {
                    "no".into()
                },
                html_escape(&r.required_versions_policy),
                r.generation.to_string(),
            ]
        })
        .collect();
    format!(
        "<h2>Rollout selector</h2><div class=\"card\">{}</div>",
        html_table(&headers, &rows)
    )
}

fn legend() -> String {
    r#"<div class="legend">
  <p><strong>This page performs no network I/O.</strong> Every figure comes from PostgreSQL. L2 object existence is as of last verification, and L1/L0 figures are each node's own last report.</p>
  <p><strong>Node state is soft state.</strong> Correctness never depends on it — exact-version resolution, manifest verification and per-request open enforce that independently on every node. These rows exist so a cutover can be judged and a fleet can be seen.</p>
  <p><strong>There are no actions here by design.</strong> Binding, activation, eviction, quarantine and mode changes are audited operations; a follow-on design comes after mirror builds produce real data to justify one.</p>
</div>"#
        .to_string()
}

// --- small helpers ----------------------------------------------------------

fn html_escape(s: &str) -> String {
    crate::charts::esc(s)
}

/// Identifiers are truncated for display. A revision or an opaque node id is
/// long, and the first characters are what an operator compares.
fn short(s: &str) -> String {
    let t: String = s.chars().take(12).collect();
    if s.chars().count() > 12 {
        html_escape(&format!("{t}…"))
    } else {
        html_escape(&t)
    }
}

fn freshness(age: i64) -> String {
    if age <= STALE_AFTER_SECS {
        format!("{age}s")
    } else if age <= STALE_AFTER_SECS * 2 {
        format!("<span class=\"warn\">stale ({age}s)</span>")
    } else {
        format!("<span class=\"bad\">unknown ({age}s)</span>")
    }
}

fn deficit_cell(observed: i64, required: i64) -> String {
    if observed >= required {
        format!("{observed}")
    } else {
        format!("<span class=\"bad\">{observed} of {required}</span>")
    }
}

fn ratio(used: Option<i64>, budget: Option<i64>) -> String {
    match (used, budget) {
        (Some(u), Some(b)) => format!("{} / {}", human_bytes(u), human_bytes(b)),
        (Some(u), None) => human_bytes(u),
        _ => "—".into(),
    }
}

fn human_bytes(n: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage_api::{CapabilityRow, NodeRow, PlaneExpectationRow, RolloutRow, TierCounts};

    fn settings(configured: &str, effective: &str) -> SettingsView {
        SettingsView {
            configured_mode: configured.into(),
            effective_mode: effective.into(),
            degraded_because: None,
            must_refuse_startup: false,
            artifact_store: "az".into(),
            capabilities: vec![
                CapabilityRow {
                    name: "catalog".into(),
                    enabled: true,
                    detail: "PostgreSQL reachable".into(),
                },
                CapabilityRow {
                    name: "artifact-store".into(),
                    enabled: true,
                    detail: "az backend configured".into(),
                },
                CapabilityRow {
                    name: "vector-engine".into(),
                    enabled: false,
                    detail: "no vector engine compiled in; lexical-only artifacts still work"
                        .into(),
                },
            ],
            blocking: Vec::new(),
        }
    }

    fn snapshot(mode: &str) -> StorageSnapshot {
        StorageSnapshot {
            settings: settings(mode, mode),
            this_replica_mode: mode.into(),
            truth: TierCounts {
                artifacts: 3,
                bytes: 5_242_880,
                verified: 2,
                sealed: 1,
                failed: 0,
                retired: 0,
            },
            bindings: 2,
            attempts_running: 1,
            attempts_expired: 0,
            rollout_datastore_scopes: 1,
            rollout: vec![RolloutRow {
                scope_kind: "collection".into(),
                scope_id: "col-1".into(),
                serving: "datastore".into(),
                prewarm_staged: true,
                required_versions_policy: "active_pinned_and_horizon".into(),
                generation: 4,
            }],
            expectations: vec![PlaneExpectationRow {
                plane: "rest".into(),
                deployment_revision: "sha-abcdef123456789".into(),
                minimum_fresh_nodes: 2,
                minimum_open_nodes: 2,
                minimum_open_fraction: Some(0.5),
                required_mode: "datastore".into(),
                generation: 1,
                verified_age_secs: Some(30),
                observed_fresh: 1,
                observed_wrong_revision: 1,
                observed_warming: 1,
                observed_stale: 1,
            }],
            nodes: vec![NodeRow {
                node_id: "node-aaaaaaaaaaaaaaaa".into(),
                plane: "rest".into(),
                deployment_revision: "sha-abcdef123456789".into(),
                retrieval_mode: "datastore".into(),
                admission_state: "warming".into(),
                seen_age_secs: 5,
                blocking_scopes: 2,
                l1_used_bytes: Some(1_073_741_824),
                l1_budget_bytes: Some(2_147_483_648),
                l0_used_bytes: Some(1024),
                l0_budget_bytes: None,
                local_root_health: Some("healthy".into()),
            }],
            read_at: "2026-08-30T00:00:00Z".into(),
            readiness: storage_api::ReadinessView {
                admits: true,
                selected_scopes: 0,
                blocking: Vec::new(),
            },
            shadow: None,
        }
    }

    #[test]
    fn postgres_mode_renders_the_strategy_as_present_but_unused() {
        let html = render_body(&snapshot("postgres"));
        assert!(html.contains("not in use"), "{html}");
        // The page must not disappear in postgres mode: an operator has to be
        // able to see that the feature is OFF.
        assert!(html.contains("Tiers"));
        assert!(html.contains("Rollback to PostgreSQL"));
    }

    #[test]
    fn a_deficit_against_the_expectation_is_called_out() {
        let html = render_body(&snapshot("datastore"));
        // 1 observed against 2 required must not render as a bare "1".
        assert!(
            html.contains("1 of 2"),
            "the deficit must be explicit: {html}"
        );
        assert!(
            html.contains("wrong revision") || html.contains("wrong"),
            "{html}"
        );
    }

    #[test]
    fn an_empty_fleet_reads_as_unknown_not_as_empty() {
        let mut s = snapshot("datastore");
        s.nodes.clear();
        let html = render_body(&s);
        assert!(html.contains("unknown"), "{html}");
        assert!(
            !html.contains("No node has reported.</p>\n  <p>0 nodes"),
            "{html}"
        );
    }

    #[test]
    fn a_missing_expectation_says_a_cutover_has_no_gate() {
        let mut s = snapshot("datastore");
        s.expectations.clear();
        let html = render_body(&s);
        assert!(html.contains("no gate"), "{html}");
    }

    #[test]
    fn stale_and_unknown_nodes_are_distinguished() {
        assert!(freshness(5).contains("5s"));
        assert!(freshness(STALE_AFTER_SECS + 1).contains("stale"));
        assert!(freshness(STALE_AFTER_SECS * 3).contains("unknown"));
    }

    /// The page renders in a shared admin console. Nothing it prints may be a
    /// credential, a filesystem root, or an internal URI.
    #[test]
    fn nothing_sensitive_reaches_the_page() {
        let html = render_body(&snapshot("datastore"));
        for forbidden in [
            "sig=",
            "sv=",
            "AccountKey",
            "/var/lib/",
            "C:\\",
            "postgres://",
        ] {
            assert!(
                !html.contains(forbidden),
                "{forbidden:?} leaked into the page"
            );
        }
    }

    #[test]
    fn identifiers_are_truncated_rather_than_printed_whole() {
        let html = render_body(&snapshot("datastore"));
        assert!(
            !html.contains("node-aaaaaaaaaaaaaaaa"),
            "node id printed in full"
        );
        assert!(
            html.contains("node-aaaaaaa"),
            "a prefix should still be shown: {html}"
        );
    }

    /// Every feature shows its state AND its reason. A bare "disabled" sends an
    /// operator looking in three places.
    #[test]
    fn the_settings_card_shows_each_feature_with_its_reason() {
        let html = render_body(&snapshot("mirror"));
        assert!(html.contains("Datastore settings"), "{html}");
        assert!(html.contains("artifact-store"));
        assert!(html.contains("az backend configured"));
        assert!(html.contains("vector-engine"));
        assert!(
            html.contains("lexical-only artifacts still work"),
            "a disabled feature must say why: {html}"
        );
        assert!(html.contains("enabled") && html.contains("disabled"));
    }

    /// A degraded process that looks healthy is what this page exists to
    /// prevent, so the mismatch leads rather than hiding in a table.
    #[test]
    fn a_degraded_mode_is_stated_prominently_with_both_modes() {
        let mut s = snapshot("mirror");
        s.settings.configured_mode = "mirror".into();
        s.settings.effective_mode = "postgres".into();
        s.settings.degraded_because = Some("no artifact store; serving is unaffected".into());
        let html = render_body(&s);
        assert!(html.contains("Degraded"), "{html}");
        assert!(
            html.contains("configured mirror, running postgres"),
            "{html}"
        );
        assert!(html.contains("serving is unaffected"), "{html}");
    }

    /// A refusal reads differently from a degradation: one means the process
    /// will not start, the other that it started with less.
    #[test]
    fn a_startup_refusal_is_distinguished_from_a_degradation() {
        let mut s = snapshot("datastore");
        s.settings.must_refuse_startup = true;
        s.settings.degraded_because = Some("local root is not writable".into());
        s.settings.blocking = vec!["retired retention is below the pin horizon".into()];
        let html = render_body(&s);
        assert!(html.contains("Serving cannot start"), "{html}");
        assert!(html.contains("Configuration refused"), "{html}");
        assert!(html.contains("below the pin horizon"), "{html}");
        assert!(
            !html.contains("Degraded:"),
            "a refusal is not a degradation: {html}"
        );
    }

    /// The card is read-only. A button would claim a value lives somewhere it
    /// does not.
    #[test]
    fn the_settings_card_offers_no_controls() {
        let html = render_body(&snapshot("datastore"));
        for control in ["<form", "<button", "<input", "<select"] {
            assert!(
                !html.contains(control),
                "{control} found: the page is read-only"
            );
        }
        assert!(html.contains("Read-only"), "{html}");
    }

    #[test]
    fn byte_counts_are_human_readable() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(5_242_880), "5.0 MiB");
        assert_eq!(human_bytes(2_147_483_648), "2.0 GiB");
    }
}
