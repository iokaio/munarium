// SPDX-License-Identifier: Apache-2.0
//! `mmctl matrix …` — one CLI for GitOps across both trees.
//!
//! Every subcommand is one REST call to Matrix's own API, forwarded verbatim.
//! Nothing here validates a Matrix asset locally: ground rule 1 forbids a
//! `server/` crate from depending on a `matrix/` crate, and the plan's "reuse
//! mxctl's validators via the Rust client" would have been exactly that edge.
//! Matrix validates on apply and answers with the same findings `mxctl` would
//! print, so the honest passthrough loses nothing — it just does not pretend
//! to know the grammar.
//!
//!   mmctl matrix version
//!   mmctl matrix apply -f <asset.yaml>          POST /v1/assets (text/yaml)
//!   mmctl matrix validate -f <asset.yaml>       POST /v1/assets/validate
//!   mmctl matrix introspect <source>            POST /v1/datasources/{name}/introspect
//!   mmctl matrix probe <source>                 POST /v1/datasources/{name}/probe
//!   mmctl matrix verify <contract>              POST /v1/contracts/{name}/verify  (exit 3 on a failed question)
//!   mmctl matrix verify-view <view>             POST /v1/metricviews/{name}/verify, else /v1/dataviews/{name}/verify (exit 3 on a failed question)
//!   mmctl matrix sync <source>                  POST /v1/datasources/{name}/sync
//!   mmctl matrix reconcile <mapping>            POST /v1/mappings/{name}/run
//!   mmctl matrix journal [--limit N]            GET  /v1/journal
//!
//! Env: MUNARIUMCTL_MATRIX_URL (default http://localhost:8180),
//!      MUNARIUMCTL_MATRIX_TOKEN, MUNARIUMCTL_UID (shared with the server side).

use std::time::Duration;

fn base() -> String {
    std::env::var("MUNARIUMCTL_MATRIX_URL").unwrap_or_else(|_| "http://localhost:8180".into())
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(600))
        .build()
        .expect("client")
}

fn auth(rb: reqwest::blocking::RequestBuilder) -> reqwest::blocking::RequestBuilder {
    let rb = match std::env::var("MUNARIUMCTL_MATRIX_TOKEN") {
        Ok(t) => rb.bearer_auth(t),
        Err(_) => rb,
    };
    let uid = std::env::var("MUNARIUMCTL_UID").unwrap_or_else(|_| "mmctl".into());
    rb.header("x-munarium-request-id", uuid::Uuid::new_v4().to_string())
        .header("x-munarium-uid", uid)
}

fn die(msg: &str) -> ! {
    eprintln!("mmctl matrix: {msg}");
    std::process::exit(1);
}

/// Matrix answers problem+json with a `refusal` array on a 422; print the
/// findings one per line rather than the JSON, because an operator fixing an
/// asset wants the path and the message, not the envelope.
fn check(resp: reqwest::blocking::Response) -> serde_json::Value {
    let status = resp.status();
    let body: serde_json::Value = resp.json().unwrap_or_default();
    if !status.is_success() {
        if let Some(findings) = body["refusal"].as_array() {
            for f in findings {
                eprintln!(
                    "  {}  {}  {}",
                    f["code"].as_str().unwrap_or("?"),
                    f["path"].as_str().unwrap_or(""),
                    f["message"].as_str().unwrap_or("")
                );
            }
        }
        die(&format!(
            "{status}: {}",
            body["detail"].as_str().unwrap_or(&body.to_string())
        ));
    }
    body
}

fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
}

fn read_asset(args: &[String]) -> String {
    let path = flag_value(args, "-f").unwrap_or_else(|| die("usage: -f <asset.yaml>"));
    std::fs::read_to_string(path).unwrap_or_else(|e| die(&format!("{path}: {e}")))
}

fn post_yaml(path: &str, yaml: String) -> serde_json::Value {
    check(
        auth(client().post(format!("{}{path}", base())))
            .header("content-type", "text/yaml")
            .body(yaml)
            .send()
            .unwrap_or_else(|e| die(&e.to_string())),
    )
}

/// POST to `first`; on a 404 there, POST to `second` instead.
fn post_empty_or(first: &str, second: &str) -> serde_json::Value {
    let resp = auth(client().post(format!("{}{first}", base())))
        .send()
        .unwrap_or_else(|e| die(&e.to_string()));
    if resp.status().as_u16() == 404 {
        return post_empty(second);
    }
    check(resp)
}

fn post_empty(path: &str) -> serde_json::Value {
    check(
        auth(client().post(format!("{}{path}", base())))
            .send()
            .unwrap_or_else(|e| die(&e.to_string())),
    )
}

fn get(path: &str) -> serde_json::Value {
    check(
        auth(client().get(format!("{}{path}", base())))
            .send()
            .unwrap_or_else(|e| die(&e.to_string())),
    )
}

fn print(v: &serde_json::Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

fn name_arg(args: &[String], what: &str) -> String {
    args.get(2)
        .cloned()
        .unwrap_or_else(|| die(&format!("usage: mmctl matrix {} <{what}>", args[1])))
}

/// `args` is the full argv; `args[1] == "matrix"`.
pub fn dispatch(args: &[String]) {
    // `args` arrives with the binary name already stripped (main.rs
    // `skip(1)`), so `matrix` is index 0 and the subcommand index 1 — the
    // first live call printed usage for a valid command because this read
    // one slot too far.
    match args.get(1).map(String::as_str) {
        Some("version") => print(&get("/version")),
        Some("apply") => print(&post_yaml("/v1/assets", read_asset(args))),
        Some("validate") => print(&post_yaml("/v1/assets/validate", read_asset(args))),
        Some("introspect") => {
            let n = name_arg(args, "source");
            print(&post_empty(&format!("/v1/datasources/{n}/introspect")))
        }
        Some("probe") => {
            let n = name_arg(args, "source");
            print(&post_empty(&format!("/v1/datasources/{n}/probe")))
        }
        Some("verify-view") => {
            let n = name_arg(args, "view");
            // A metric view first; a native data view when there is none by
            // that name. Any other failure is reported as it came.
            let out = post_empty_or(
                &format!("/v1/metricviews/{n}/verify"),
                &format!("/v1/dataviews/{n}/verify"),
            );
            print(&out);
            let failed = out["failed"].as_u64().unwrap_or(0);
            if failed > 0 {
                std::process::exit(3);
            }
        }
        Some("sync") => {
            let n = name_arg(args, "source");
            print(&post_empty(&format!("/v1/datasources/{n}/sync")))
        }
        Some("reconcile") => {
            let n = name_arg(args, "mapping");
            print(&post_empty(&format!("/v1/mappings/{n}/run")))
        }
        Some("verify") => {
            let n = name_arg(args, "contract");
            let out = post_empty(&format!("/v1/contracts/{n}/verify"));
            print(&out);
            // The same exit discipline as mxctl: a failed verified question is
            // a broken contract, and CI must be able to tell that from a
            // broken command.
            let failed = out["failed"].as_u64().unwrap_or(0)
                + out["questions"]
                    .as_array()
                    .map(|q| q.iter().filter(|x| x["ok"] == false).count() as u64)
                    .unwrap_or(0);
            if failed > 0 {
                std::process::exit(3);
            }
        }
        Some("journal") => {
            let limit = flag_value(args, "--limit").cloned().unwrap_or_else(|| "50".into());
            print(&get(&format!("/v1/journal?limit={limit}")))
        }
        _ => die(
            "usage: mmctl matrix version | apply -f <yaml> | validate -f <yaml> | introspect <source> | \
             probe <source> | verify <contract> | verify-view <view> | sync <source> | reconcile <mapping> | \
             journal [--limit N]",
        ),
    }
}
