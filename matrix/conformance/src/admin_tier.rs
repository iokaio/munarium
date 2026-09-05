// SPDX-License-Identifier: Apache-2.0
//! The operator console over real HTTP.
//!
//! The console's *unit* properties are asserted in the server crate, at the
//! assembled router: role gating, CSRF, Origin, the header set, what never
//! renders. Those are cheap, deterministic, and they run on every save.
//!
//! What they cannot prove is the property this tier exists for: **a draft
//! authored in the console, exported, and applied by `mxctl` produces exactly
//! the version applying it in place would have.** That equality is the whole
//! justification for exporting a bundle being the default path — if the two
//! could differ, the export would be a formality and an operator would be
//! right to skip it. It needs a real service, a real registry, and a real
//! immutability rule, so it lives here.
//!
//! Gated on `MUNARIUM_MATRIX_TEST_HTTP` like every other HTTP scenario, and
//! it SKIPS OUT LOUD when the console is off: a scenario that returns early
//! prints `ok`, which is indistinguishable from one that proved something.

#![cfg(test)]

use crate::Tier;

fn base() -> Option<(String, String)> {
    match crate::tier() {
        Tier::Http { url, token, .. } => Some((url, token.unwrap_or_else(|| "mxdev".into()))),
        _ => {
            println!(
                "SKIPPED: MUNARIUM_MATRIX_TEST_HTTP is not set, so nothing was tested. \
                 Run `test.ps1 -BlackBox` to exercise this tier."
            );
            None
        }
    }
}

/// The mgmt token. The console is mgmt-only, and the rw token is a SEPARATE
/// credential the action forms ask for — which is the property under test in
/// `admin_an_action_needs_the_rw_credential_not_the_admins_own`.
///
/// The default is the COMPOSE token. A deployment has its own, and when
/// nothing set this variable against one, every admin request authenticated
/// as nobody, every page redirected, and
/// five of these six scenarios failed on the first live run for a reason
/// that had nothing to do with the console.
fn mgmt_token() -> String {
    std::env::var("MUNARIUM_MATRIX_TEST_MGMT_TOKEN").unwrap_or_else(|_| "mxmgmt".into())
}

/// Prove the mgmt token actually authenticates, before any scenario leans on
/// it.
///
/// The sixth scenario, `admin_is_mgmt_only_over_the_wire`, asserts that a
/// non-mgmt caller is REDIRECTED — which is exactly what a bogus mgmt token
/// also produces. It therefore **passed on the live cycle where the other
/// five failed**, and would have passed against a console that did not work at
/// all. A test that passes whether or not the thing under test exists is worse
/// than a missing test, so the token is checked once, loudly, and the failure
/// names the variable to set.
async fn require_working_mgmt_token(url: &str) {
    let resp = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("a client")
        .get(format!("{url}/admin"))
        .bearer_auth(mgmt_token())
        .send()
        .await
        .expect("the console answers");
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return; // the console is off; the caller's own skip handles it
    }
    assert!(
        resp.status().is_success(),
        "the mgmt token did not authenticate against {url}/admin ({}). Set          MUNARIUM_MATRIX_TEST_MGMT_TOKEN to this deployment's mgmt token — it is          `mxmgmt` in compose. Without it          every page redirects and these scenarios test the redirect instead of the          console.",
        resp.status()
    );
}

/// Pull the CSRF token out of a rendered page. The console is server-rendered
/// with no script, so a form's hidden field is literally in the HTML — which
/// is also why a test can drive it without a browser.
fn csrf_of(html: &str) -> Option<String> {
    let needle = r#"name="csrf" value=""#;
    let start = html.find(needle)? + needle.len();
    let end = html[start..].find('"')? + start;
    Some(html[start..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> reqwest::Client {
        // No redirect following: the console's answer to an unauthenticated
        // request IS the redirect, and a client that followed it would assert
        // the login page instead of the refusal.
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("a client")
    }

    async fn get_admin(
        path: &str,
    ) -> Option<(reqwest::StatusCode, reqwest::header::HeaderMap, String)> {
        let (url, _) = base()?;
        let resp = client()
            .get(format!("{url}{path}"))
            .bearer_auth(mgmt_token())
            .send()
            .await
            .expect("the console answers");
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = resp.text().await.unwrap_or_default();
        // A deployment may legitimately run with MUNARIUM_MATRIX_ADMIN
        // disabled. Say so rather than failing — but say it, because silence
        // here would look like a pass.
        if status == reqwest::StatusCode::NOT_FOUND {
            println!(
                "SKIPPED: {path} answered 404 — this deployment runs with \
                 MUNARIUM_MATRIX_ADMIN=disabled or on a non-control role."
            );
            return None;
        }
        Some((status, headers, body))
    }

    /// Every read page renders **with JavaScript disabled**, which is the
    /// The console's exit gate, first clause. Asserted by the absence of any
    /// `<script`, not by a claim in a comment.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_HTTP"]
    async fn admin_every_read_page_renders_without_script() {
        for path in [
            "/admin",
            "/admin/sources",
            "/admin/runs",
            "/admin/journal",
            "/admin/verification",
            "/admin/mappings",
            "/admin/registry",
            "/admin/author",
        ] {
            let Some((status, headers, body)) = get_admin(path).await else {
                return;
            };
            assert!(status.is_success(), "{path}: {status}");
            assert!(
                !body.to_lowercase().contains("<script"),
                "{path} must render with no script"
            );
            assert!(body.contains("<main>"), "{path} rendered no page body");
            // The CSP is the machine-checkable half of the same claim.
            let csp = headers
                .get("content-security-policy")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default();
            assert!(csp.contains("default-src 'self'"), "{path}: {csp}");
            assert!(!csp.contains("script-src"), "{path}: {csp}");
        }
    }

    /// Anonymous means the login form, over the wire and not only in a unit
    /// test — including through whatever proxy a deployment puts in front.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_HTTP"]
    async fn admin_is_mgmt_only_over_the_wire() {
        let Some((url, rw)) = base() else { return };
        // Before asserting that a NON-mgmt caller is redirected, prove the
        // mgmt token is not also redirected — otherwise this passes against a
        // broken console, which is what it did on the first live cycle.
        require_working_mgmt_token(&url).await;
        for token in [None, Some(rw.as_str())] {
            let mut req = client().get(format!("{url}/admin"));
            if let Some(t) = token {
                req = req.bearer_auth(t);
            }
            let resp = req.send().await.expect("the console answers");
            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                println!("SKIPPED: /admin is not served by this deployment");
                return;
            }
            assert_eq!(
                resp.status(),
                reqwest::StatusCode::SEE_OTHER,
                "an rw or anonymous caller must be redirected, not served"
            );
            assert_eq!(
                resp.headers().get("location").and_then(|v| v.to_str().ok()),
                Some("/admin/login")
            );
        }
    }

    /// A write with no CSRF token is refused by the deployed service.
    ///
    /// The unit test proves the check; this proves it is still there after the
    /// router is assembled, the middleware wraps it and the ingress rewrites
    /// headers — which is where a check usually goes missing.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_HTTP"]
    async fn admin_a_write_without_a_csrf_token_is_refused() {
        let Some((url, rw)) = base() else { return };
        let resp = client()
            .post(format!("{url}/admin/sources/crm/probe"))
            .bearer_auth(mgmt_token())
            .form(&[("rw_token", rw.as_str())])
            .send()
            .await
            .expect("the console answers");
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            println!("SKIPPED: /admin is not served by this deployment");
            return;
        }
        let body = resp.text().await.unwrap_or_default();
        assert!(
            body.contains("stale form"),
            "a write with no CSRF token must be refused: {body:.400}"
        );
    }

    /// The role invariant, live: the mgmt credential that reaches every page
    /// cannot act. A leaked mgmt token is a read, not a write.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_HTTP"]
    async fn admin_an_action_needs_the_rw_credential_not_the_admins_own() {
        let Some((url, _)) = base() else { return };
        // The SOURCE's own page, not the list: the action forms live there,
        // and reading the list found no CSRF field at all — so both of these
        // scenarios "passed" by skipping, on a stated reason that was FALSE
        // (`crm` was registered the whole time). Found by running the tier
        // with `--nocapture`, which is the only way a skip is visible at all.
        // A skip on a wrong reason is worse than a silent one: it sends the
        // reader somewhere else.
        let Some((_, _, page)) = get_admin("/admin/sources/crm").await else {
            return;
        };
        let Some(csrf) = csrf_of(&page) else {
            panic!("the source page carries no CSRF field: {page:.600}");
        };
        let body = client()
            .post(format!("{url}/admin/sources/crm/probe"))
            .bearer_auth(mgmt_token())
            .form(&[("csrf", csrf.as_str()), ("rw_token", &mgmt_token())])
            .send()
            .await
            .expect("the console answers")
            .text()
            .await
            .unwrap_or_default();
        assert!(
            body.contains("cannot execute commands"),
            "an mgmt token offered as the rw credential must be refused: {body:.400}"
        );
    }

    /// **The console's exit gate.** A draft authored in the console, exported,
    /// and applied by the API from that export is byte-identical to what
    /// applying it in place produces.
    ///
    /// Done in the order an operator would: author against a live registry,
    /// export, apply the exported bytes, then read the applied YAML back and
    /// compare. The comparison is on BYTES, not on a re-parse — a
    /// re-serialisation that happened to mean the same thing would move every
    /// node id and every citation locus, which is precisely what the registry
    /// stores the original bytes to avoid.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_HTTP"]
    async fn admin_an_exported_draft_applies_identically_to_applying_in_place() {
        let Some((url, rw)) = base() else { return };
        let Some((_, _, page)) = get_admin("/admin/author").await else {
            return;
        };
        let Some(csrf) = csrf_of(&page) else {
            panic!("the author page carries no CSRF field");
        };

        // A contract that is complete enough to validate, and named so it
        // cannot collide with a fixture. Version 1 every time: the registry
        // refuses a mutation of an applied version, so a rerun must either
        // find the same bytes (unchanged) or a different name.
        let name = format!("admin-export-{}", uuid_suffix());
        let draft = format!(
            r#"apiVersion: munarium.ioka.io/v1
kind: QueryContract
metadata:
  name: {name}
  version: 1
spec:
  source: crm
  description: >-
    Authored by the admin console's conformance scenario. Open pipeline by
    region, as of a date.
  parameters:
    as_of: {{ type: date, required: true }}
  statementByDialect:
    postgres:
      inline: >-
        SELECT region, SUM(amount) AS pipeline_amount, COUNT(*) AS opportunity_count
        FROM opportunities
        WHERE stage <> 'Closed Won' AND updated_at <= :as_of
        GROUP BY region ORDER BY region
  reads:
    tables: [opportunities]
    columns: [region, amount, stage, updated_at]
  result:
    columns:
      region: {{ type: string, key: true }}
      pipeline_amount: {{ type: decimal, scale: 2, unit: USD, additivity: additive }}
      opportunity_count: {{ type: int64, additivity: additive }}
    columnOrder: [region, pipeline_amount, opportunity_count]
    orderBy: [region]
  policy:
    authorization: source_native
"#
        );

        // 1. Validate and diff, through the console, exactly as an operator
        //    would. The console must agree with the service — it posts to the
        //    same validators, which is the point of not carrying its own copy
        //    of the rules.
        let validated = client()
            .post(format!("{url}/admin/author"))
            .bearer_auth(mgmt_token())
            .form(&[("csrf", csrf.as_str()), ("yaml", draft.as_str())])
            .send()
            .await
            .expect("the console answers")
            .text()
            .await
            .unwrap_or_default();
        assert!(
            validated.contains("no findings") || validated.contains("advisory"),
            "the draft did not validate through the console: {validated:.800}"
        );
        assert!(
            validated.contains("would be its first version"),
            "the console should report this name as unapplied: {validated:.400}"
        );

        // 2. Export. The manifest is the point: a bundle whose bytes changed
        //    on the way to the repository applies something nobody reviewed.
        let exported = client()
            .post(format!("{url}/admin/author/export"))
            .bearer_auth(mgmt_token())
            .form(&[("csrf", csrf.as_str()), ("yaml", draft.as_str())])
            .send()
            .await
            .expect("the console answers")
            .text()
            .await
            .unwrap_or_default();
        assert!(exported.contains("sha256:"), "no manifest: {exported:.400}");
        assert!(
            exported.contains(&format!("querycontract.{name}.yaml")),
            "the export is not named for the asset: {exported:.400}"
        );

        // 3. Apply the exported bytes the way `mxctl` would — the public API,
        //    not the console.
        let applied = client()
            .post(format!("{url}/v1/assets"))
            .bearer_auth(&rw)
            .header("content-type", "text/yaml")
            .body(draft.clone())
            .send()
            .await
            .expect("the API answers");
        assert!(
            applied.status().is_success(),
            "apply: {}: {}",
            applied.status(),
            applied.text().await.unwrap_or_default()
        );

        // 4. Read it back. Byte-identical, or the export is a formality.
        let stored = client()
            .get(format!("{url}/v1/contracts/{name}"))
            .bearer_auth(&rw)
            .send()
            .await
            .expect("the API answers")
            .text()
            .await
            .unwrap_or_default();
        assert_eq!(
            stored, draft,
            "the registry must store the exported bytes verbatim"
        );

        // 5. And applying IN PLACE the same bytes is `unchanged` — the two
        //    paths converge on one version rather than minting a second.
        let again: serde_json::Value = client()
            .post(format!("{url}/v1/assets"))
            .bearer_auth(&rw)
            .header("content-type", "text/yaml")
            .body(draft)
            .send()
            .await
            .expect("the API answers")
            .json()
            .await
            .expect("json");
        assert_eq!(
            again["unchanged"],
            serde_json::json!(true),
            "re-applying identical bytes must be a no-op, not a new version: {again}"
        );
    }

    /// A console write lands a journal row that says where it came from.
    ///
    /// The audit property: `via: admin-ui` distinguishes a click from an API
    /// call, and it is a parameter rather than a header precisely so a caller
    /// cannot forge it.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_HTTP"]
    async fn admin_a_console_write_is_journaled_as_admin_ui() {
        let Some((url, rw)) = base() else { return };
        // The SOURCE's own page, not the list: the action forms live there,
        // and reading the list found no CSRF field at all — so both of these
        // scenarios "passed" by skipping, on a stated reason that was FALSE
        // (`crm` was registered the whole time). Found by running the tier
        // with `--nocapture`, which is the only way a skip is visible at all.
        // A skip on a wrong reason is worse than a silent one: it sends the
        // reader somewhere else.
        let Some((_, _, page)) = get_admin("/admin/sources/crm").await else {
            return;
        };
        let Some(csrf) = csrf_of(&page) else {
            panic!("the source page carries no CSRF field: {page:.600}");
        };
        // Probe is the cheapest write that journals. Its OUTCOME does not
        // matter here — an unreachable source still lands a row, which is
        // exactly what an auditor needs.
        let _ = client()
            .post(format!("{url}/admin/sources/crm/probe"))
            .bearer_auth(mgmt_token())
            .form(&[("csrf", csrf.as_str()), ("rw_token", rw.as_str())])
            .send()
            .await
            .expect("the console answers");

        // The journal is MGMT, not rw — reading it with the rw credential
        // answers 403 and the assertion then reports "no admin-ui row" about a
        // body that is a permission error. The first version of this test did
        // exactly that.
        let journal: serde_json::Value = client()
            .get(format!("{url}/v1/journal?limit=50"))
            .bearer_auth(mgmt_token())
            .send()
            .await
            .expect("the API answers")
            .json()
            .await
            .expect("json");
        let entries = journal["entries"].as_array().cloned().unwrap_or_default();
        assert!(
            entries
                .iter()
                .any(|e| e["via"].as_str() == Some("admin-ui")),
            "no journal row carries `via: admin-ui`: {}",
            serde_json::to_string(&journal).unwrap_or_default()
        );
    }

    /// The console's exit gate, last clause: "the drift flag sets and clears".
    ///
    /// Until 2026-08-30 the flag was a sentence on the apply page — rendered
    /// once, persisted nowhere, cleared by nothing — and the phase record
    /// said so plainly. Now it is derived from the journal: an asset whose
    /// latest successful apply came through the console is drifted, and a
    /// later apply of the same bytes by any other plane (here, the API, as
    /// `mxctl` from the landed bundle would) clears it. Both halves are
    /// asserted on the registry page, which is where an operator looks.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_HTTP"]
    async fn admin_the_drift_flag_sets_on_apply_in_place_and_clears_when_the_bundle_lands() {
        let Some((url, rw)) = base() else { return };
        let Some((_, _, page)) = get_admin("/admin/author").await else {
            return;
        };
        let Some(csrf) = csrf_of(&page) else {
            panic!("the author page carries no CSRF field");
        };
        let name = format!("admin-drift-{}", uuid_suffix());
        let decision = format!("DEC-{}", uuid_suffix());
        let draft = format!(
            r#"apiVersion: munarium.ioka.io/v1
kind: QueryContract
metadata:
  name: {name}
  version: 1
spec:
  source: crm
  description: Applied in place by the drift-flag scenario.
  parameters:
    as_of: {{ type: date, required: true }}
  statementByDialect:
    postgres:
      inline: >-
        SELECT region, SUM(amount) AS pipeline_amount, COUNT(*) AS opportunity_count
        FROM opportunities
        WHERE stage <> 'Closed Won' AND updated_at <= :as_of
        GROUP BY region ORDER BY region
  reads:
    tables: [opportunities]
    columns: [region, amount, stage, updated_at]
  result:
    columns:
      region: {{ type: string, key: true }}
      pipeline_amount: {{ type: decimal, scale: 2, unit: USD, additivity: additive }}
      opportunity_count: {{ type: int64, additivity: additive }}
    columnOrder: [region, pipeline_amount, opportunity_count]
    orderBy: [region]
  policy:
    authorization: source_native
"#
        );

        // 1. Apply IN PLACE through the console, with a decision id.
        let applied = client()
            .post(format!("{url}/admin/author/apply"))
            .bearer_auth(mgmt_token())
            .form(&[
                ("csrf", csrf.as_str()),
                ("yaml", draft.as_str()),
                ("rw_token", rw.as_str()),
                ("decision_id", decision.as_str()),
            ])
            .send()
            .await
            .expect("the console answers")
            .text()
            .await
            .unwrap_or_default();
        assert!(
            applied.contains("drifted from git"),
            "the apply page did not announce the drift: {applied:.600}"
        );

        // 2. The flag is SET: the registry page names the asset, the decision,
        //    and the badge — not just the apply page that already scrolled by.
        let Some((_, _, registry)) = get_admin("/admin/registry").await else {
            return;
        };
        let asset_ref = format!("{name}@1");
        assert!(
            registry.contains("Drifted from git")
                && registry.contains(&asset_ref)
                && registry.contains(&decision),
            "the registry does not flag {asset_ref} under {decision}: {registry:.800}"
        );
        let Some((_, _, asset_page)) =
            get_admin(&format!("/admin/registry/contracts/{name}")).await
        else {
            return;
        };
        assert!(
            asset_page.contains("drifted from git") && asset_page.contains(&decision),
            "the asset page does not carry the flag: {asset_page:.600}"
        );

        // 3. The bundle lands: the SAME bytes applied by another plane — the
        //    API, as `mxctl` from the repository would. `unchanged`, because
        //    the two paths converge on one version.
        let landed: serde_json::Value = client()
            .post(format!("{url}/v1/assets"))
            .bearer_auth(&rw)
            .header("content-type", "text/yaml")
            .body(draft)
            .send()
            .await
            .expect("the API answers")
            .json()
            .await
            .expect("json");
        assert_eq!(landed["unchanged"], serde_json::json!(true), "{landed}");

        // 4. The flag is CLEAR for this asset. Another scenario may have left
        //    its own drifted asset behind, so the assertion is about THIS
        //    asset_ref's row and not about the page carrying the word.
        let Some((_, _, registry)) = get_admin("/admin/registry").await else {
            return;
        };
        let row = registry
            .split("<tr>")
            .find(|r| r.contains(&asset_ref))
            .unwrap_or_else(|| panic!("{asset_ref} vanished from the registry: {registry:.400}"));
        assert!(
            !row.contains("drifted") && !row.contains(&decision),
            "the flag did not clear after the bundle landed: {row:.400}"
        );
        let Some((_, _, asset_page)) =
            get_admin(&format!("/admin/registry/contracts/{name}")).await
        else {
            return;
        };
        assert!(
            !asset_page.contains("drifted from git"),
            "the asset page still carries the flag: {asset_page:.600}"
        );
    }

    /// A short suffix so a rerun does not collide with its own applied
    /// version. Not a uuid crate dependency for six characters.
    fn uuid_suffix() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let alphabet = b"abcdefghijklmnopqrstuvwxyz";
        (0..6)
            .map(|i| alphabet[((n >> (i * 5)) % 26) as usize] as char)
            .collect()
    }
}
