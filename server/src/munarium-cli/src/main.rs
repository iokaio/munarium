// SPDX-License-Identifier: Apache-2.0
//! mmctl — the operator CLI. Deliberately std-arg-parsed and thin:
//! every operation is one REST call, so anything mmctl does, CI can do
//! with curl.
//!
//!   mmctl apply -f <file.yaml>       # kind-routed (parsed): Shape | ProviderConfig | Runbook | ChronologyRules
//!   mmctl run <runbook> [--version-id V] [--watch]
//!   mmctl approve <run-id> <step-ordinal>
//!   mmctl get run <run-id>
//!   mmctl runbook list|info <name>|validate -f <file.yaml> [--suggest]
//!   mmctl author patterns|pattern <id>|new|list|show|answer|validate|assist|export
//!   mmctl bundle apply -f <bundle.json> [--dir <dir>]   (hash-verified prod deploy)
//!   mmctl bulk upload --dir <dir> [--prefix <p/>] [--label L] [--resume <bulk-id>]
//!   mmctl bulk status <bulk-id> [--needed] | bulk complete <bulk-id>
//!   mmctl token issue <uid> <level> <scopes,csv> [compartments,csv]   (mgmt token)
//!   mmctl datastore status|verify|rebuild <index-version-id> | backfill <collection-id> | bind <v> <slot> <artifact>
//!                                    (derived-index artifacts; exit 3 = failed verify / incomplete scope)
//!   mmctl matrix version|apply|validate|introspect|probe|verify|sync|reconcile|journal
//!                                    (forwarded to Munarium Matrix; see matrix.rs)
//!
//! Env: MUNARIUMCTL_URL (default http://localhost:8080), MUNARIUMCTL_TOKEN,
//!      MUNARIUMCTL_UID (the uid contract; default "mmctl").

mod matrix;

use std::time::Duration;

fn base() -> String {
    std::env::var("MUNARIUMCTL_URL").unwrap_or_else(|_| "http://localhost:8080".into())
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(600))
        .build()
        .expect("client")
}

fn auth(rb: reqwest::blocking::RequestBuilder) -> reqwest::blocking::RequestBuilder {
    let rb = match std::env::var("MUNARIUMCTL_TOKEN") {
        Ok(t) => rb.bearer_auth(t),
        Err(_) => rb,
    };
    let uid = std::env::var("MUNARIUMCTL_UID").unwrap_or_else(|_| "mmctl".into());
    rb.header("idempotency-key", uuid::Uuid::new_v4().to_string())
        .header("x-munarium-uid", uid)
}

fn die(msg: &str) -> ! {
    eprintln!("mmctl: {msg}");
    std::process::exit(1);
}

fn check(resp: reqwest::blocking::Response) -> serde_json::Value {
    let status = resp.status();
    let body: serde_json::Value = resp.json().unwrap_or_default();
    if !status.is_success() {
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

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    hex::encode(sha2::Sha256::digest(bytes))
}

/// Media type by extension for `bulk upload`. Unknown extensions upload as
/// octet-stream — collections binding on mediaTypes simply won't match them.
fn media_type_for(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or_default() {
        "md" => "text/markdown",
        "txt" => "text/plain",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "yaml" | "yml" => "text/yaml",
        "csv" => "text/csv",
        "pdf" => "application/pdf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        _ => "application/octet-stream",
    }
}

/// The bundle's manifest contract: sha256 over the byte-sorted
/// "path\0hash\n" lines (munarium-authoring/src/bundle.rs).
fn manifest_hash(hashes: &std::collections::BTreeMap<String, String>) -> String {
    let mut buf = String::new();
    for (path, hash) in hashes {
        buf.push_str(path);
        buf.push('\0');
        buf.push_str(hash);
        buf.push('\n');
    }
    sha256_hex(buf.as_bytes())
}

/// Client-side bundle verification: per-file hashes + the manifest hash.
/// `files` maps path -> yaml (from the bundle or re-read from disk).
fn verify_bundle_files(
    bundle: &serde_json::Value,
    files: &std::collections::BTreeMap<String, String>,
) -> Result<(), String> {
    let declared: std::collections::BTreeMap<String, String> = bundle["hashes"]
        .as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    if declared.len() != files.len() {
        return Err("bundle hashes and files disagree on the file set".into());
    }
    for (path, yaml) in files {
        let actual = sha256_hex(yaml.as_bytes());
        match declared.get(path) {
            Some(d) if *d == actual => {}
            _ => {
                return Err(format!(
                "'{path}' does not match its declared hash — bundle content drifted since export"
            ))
            }
        }
    }
    if manifest_hash(&declared) != bundle["manifest_hash"].as_str().unwrap_or_default() {
        return Err("manifest_hash does not match the declared hashes".into());
    }
    Ok(())
}

/// LF-normalize file content read from disk before hashing or POSTing.
/// The server emits LF and bundle hashes are defined over those bytes; on
/// Windows, git autocrlf rewrites a checked-out export to CRLF, and a
/// deploy refused over line endings would be a false alarm, not integrity.
fn read_normalized(path: &std::path::Path) -> String {
    std::fs::read_to_string(path)
        .map(|s| s.replace("\r\n", "\n"))
        .unwrap_or_else(|e| die(&format!("read {}: {e}", path.display())))
}

/// Bundle file paths are joined under a target directory, so they must be
/// clean relative paths — no traversal, no absolute roots, no drive letters.
fn safe_rel_path(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with('/')
        && !p.contains('\\')
        && !p.contains(':')
        && !p.split('/').any(|seg| seg == ".." || seg.is_empty())
}

/// Parsed `kind:` for routing — never substring-sniffed: a runbook whose
/// completion template mentions "kind: Shape" must not be posted to
/// /v1/shapes.
fn yaml_kind(yaml: &str) -> String {
    serde_yaml::from_str::<serde_json::Value>(yaml)
        .ok()
        .and_then(|v| v.get("kind").and_then(|k| k.as_str()).map(String::from))
        .unwrap_or_default()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("apply") => {
            let path = args
                .iter()
                .position(|a| a == "-f")
                .and_then(|i| args.get(i + 1))
                .unwrap_or_else(|| die("usage: mmctl apply -f <file.yaml>"));
            let yaml =
                std::fs::read_to_string(path).unwrap_or_else(|e| die(&format!("read {path}: {e}")));
            let route = match yaml_kind(&yaml).as_str() {
                "ChronologyRules" => "/v1/chronology-rules",
                "Shape" => "/v1/shapes",
                "ProviderConfig" => "/v1/providers",
                "Runbook" => "/v1/runbooks",
                _ => die(
                    "file must declare kind: Shape | ProviderConfig | Runbook | ChronologyRules",
                ),
            };
            let body = check(
                auth(client().post(format!("{}{route}", base())))
                    .header("content-type", "text/yaml")
                    .body(yaml)
                    .send()
                    .unwrap_or_else(|e| die(&e.to_string())),
            );
            println!("{}", serde_json::to_string_pretty(&body).unwrap());
        }
        Some("run") => {
            let name = args
                .get(1)
                .unwrap_or_else(|| die("usage: mmctl run <runbook> [--version-id V] [--watch]"));
            let version = args
                .iter()
                .position(|a| a == "--version-id")
                .and_then(|i| args.get(i + 1))
                .map(|v| format!("?version_id={v}"))
                .unwrap_or_default();
            let body = check(
                auth(client().post(format!("{}/v1/runbooks/{name}/runs{version}", base())))
                    .send()
                    .unwrap_or_else(|e| die(&e.to_string())),
            );
            println!("{}", serde_json::to_string_pretty(&body).unwrap());
            if args.iter().any(|a| a == "--watch") {
                let run_id = body["run_id"].as_str().unwrap_or_default().to_string();
                loop {
                    std::thread::sleep(Duration::from_secs(2));
                    let run = check(
                        auth(client().get(format!("{}/v1/runs/{run_id}", base())))
                            .send()
                            .unwrap_or_else(|e| die(&e.to_string())),
                    );
                    let state = run["state"].as_str().unwrap_or_default().to_string();
                    println!("{}", serde_json::to_string_pretty(&run).unwrap());
                    if state != "running" {
                        if state == "awaiting_approval" {
                            println!("approve with: mmctl approve {run_id} <step-ordinal>");
                        }
                        break;
                    }
                }
            }
        }
        Some("approve") => {
            let run_id = args
                .get(1)
                .unwrap_or_else(|| die("usage: mmctl approve <run-id> <step-ordinal>"));
            let ordinal = args
                .get(2)
                .unwrap_or_else(|| die("usage: mmctl approve <run-id> <step-ordinal>"));
            let body = check(
                auth(client().post(format!(
                    "{}/v1/runs/{run_id}/steps/{ordinal}/approve",
                    base()
                )))
                .send()
                .unwrap_or_else(|e| die(&e.to_string())),
            );
            println!("{}", serde_json::to_string_pretty(&body).unwrap());
        }
        Some("runbook") => match args.get(1).map(String::as_str) {
            Some("list") => {
                let body = check(
                    auth(client().get(format!("{}/v1/runbooks", base())))
                        .send()
                        .unwrap_or_else(|e| die(&e.to_string())),
                );
                println!("{}", serde_json::to_string_pretty(&body).unwrap());
            }
            Some("info") => {
                let name = args
                    .get(2)
                    .unwrap_or_else(|| die("usage: mmctl runbook info <name[@version]>"));
                let body = check(
                    auth(client().get(format!("{}/v1/runbooks/{name}", base())))
                        .send()
                        .unwrap_or_else(|e| die(&e.to_string())),
                );
                println!("{}", serde_json::to_string_pretty(&body).unwrap());
            }
            Some("validate") => {
                let path = args
                    .iter()
                    .position(|a| a == "-f")
                    .and_then(|i| args.get(i + 1))
                    .unwrap_or_else(|| {
                        die("usage: mmctl runbook validate -f <file.yaml> [--suggest]")
                    });
                let yaml = std::fs::read_to_string(path)
                    .unwrap_or_else(|e| die(&format!("read {path}: {e}")));
                let suggest = args.iter().any(|a| a == "--suggest");
                let body = check(
                    auth(
                        client().post(format!("{}/v1/runbooks/validate?suggest={suggest}", base())),
                    )
                    .header("content-type", "text/yaml")
                    .body(yaml)
                    .send()
                    .unwrap_or_else(|e| die(&e.to_string())),
                );
                println!("{}", serde_json::to_string_pretty(&body).unwrap());
            }
            _ => die("usage: mmctl runbook list|info <name>|validate -f <file.yaml>"),
        },
        Some("token") if args.get(1).map(String::as_str) == Some("issue") => {
            let uid = args.get(2).unwrap_or_else(|| {
                die("usage: mmctl token issue <uid> <level> <scopes,csv> [compartments,csv]")
            });
            let level: i32 = args
                .get(3)
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| die("level must be an integer"));
            let scopes: Vec<&str> = args
                .get(4)
                .map(|s| s.split(',').collect())
                .unwrap_or_else(|| die("scopes required (query|ingest, comma-separated)"));
            let compartments: Vec<&str> = args
                .get(5)
                .map(|s| s.split(',').filter(|x| !x.is_empty()).collect())
                .unwrap_or_default();
            let body = check(
                auth(client().post(format!("{}/v1/access-tokens", base())))
                    .json(&serde_json::json!({
                        "uid": uid, "access_level": level,
                        "scopes": scopes, "compartments": compartments,
                    }))
                    .send()
                    .unwrap_or_else(|e| die(&e.to_string())),
            );
            println!("{}", serde_json::to_string_pretty(&body).unwrap());
        }
        Some("author") => match args.get(1).map(String::as_str) {
            Some("patterns") => {
                let body = check(
                    auth(client().get(format!("{}/v1/authoring/patterns", base())))
                        .send()
                        .unwrap_or_else(|e| die(&e.to_string())),
                );
                println!("{}", serde_json::to_string_pretty(&body).unwrap());
            }
            Some("pattern") => {
                let id = args
                    .get(2)
                    .unwrap_or_else(|| die("usage: mmctl author pattern <id>"));
                let body = check(
                    auth(client().get(format!("{}/v1/authoring/patterns/{id}", base())))
                        .send()
                        .unwrap_or_else(|e| die(&e.to_string())),
                );
                println!("{}", serde_json::to_string_pretty(&body).unwrap());
            }
            Some("new") => {
                let name = args.get(2).unwrap_or_else(|| {
                    die("usage: mmctl author new <name> [--pattern <id>] [--seed]")
                });
                let mut req = serde_json::json!({ "name": name });
                if let Some(p) = flag_value(&args, "--pattern") {
                    req["pattern_id"] = serde_json::json!(p);
                }
                if args.iter().any(|a| a == "--seed") {
                    req["seed_from_exemplar"] = serde_json::json!(true);
                }
                let body = check(
                    auth(client().post(format!("{}/v1/authoring/drafts", base())))
                        .json(&req)
                        .send()
                        .unwrap_or_else(|e| die(&e.to_string())),
                );
                println!("{}", serde_json::to_string_pretty(&body).unwrap());
            }
            Some("list") => {
                let body = check(
                    auth(client().get(format!("{}/v1/authoring/drafts", base())))
                        .send()
                        .unwrap_or_else(|e| die(&e.to_string())),
                );
                println!("{}", serde_json::to_string_pretty(&body).unwrap());
            }
            Some("show") => {
                let id = args
                    .get(2)
                    .unwrap_or_else(|| die("usage: mmctl author show <draft-id>"));
                let body = check(
                    auth(client().get(format!("{}/v1/authoring/drafts/{id}", base())))
                        .send()
                        .unwrap_or_else(|e| die(&e.to_string())),
                );
                println!("{}", serde_json::to_string_pretty(&body).unwrap());
            }
            Some("answer") => {
                let id = args.get(2).unwrap_or_else(|| {
                    die("usage: mmctl author answer <draft-id> -f <answers.yaml>")
                });
                let path = flag_value(&args, "-f").unwrap_or_else(|| {
                    die("usage: mmctl author answer <draft-id> -f <answers.yaml>")
                });
                let yaml = std::fs::read_to_string(path)
                    .unwrap_or_else(|e| die(&format!("read {path}: {e}")));
                let answers: serde_json::Value = serde_yaml::from_str(&yaml)
                    .unwrap_or_else(|e| die(&format!("answers yaml: {e}")));
                // --no-materialize stores the answers WITHOUT regenerating
                // documents — the flag for seeded or assist-edited drafts,
                // where re-materialization would replace those documents.
                let materialize = !args.iter().any(|a| a == "--no-materialize");
                let body = check(
                    auth(client().put(format!("{}/v1/authoring/drafts/{id}/answers", base())))
                        .json(&serde_json::json!({ "answers": answers, "materialize": materialize }))
                        .send()
                        .unwrap_or_else(|e| die(&e.to_string())),
                );
                println!("{}", serde_json::to_string_pretty(&body).unwrap());
            }
            Some("validate") => {
                let id = args
                    .get(2)
                    .unwrap_or_else(|| die("usage: mmctl author validate <draft-id>"));
                let body = check(
                    auth(client().post(format!(
                        "{}/v1/authoring/drafts/{id}/validate",
                        base()
                    )))
                    .send()
                    .unwrap_or_else(|e| die(&e.to_string())),
                );
                println!("{}", serde_json::to_string_pretty(&body).unwrap());
            }
            Some("assist") => {
                let id = args
                    .get(2)
                    .unwrap_or_else(|| die("usage: mmctl author assist <draft-id> [--description D] [--instructions I] [--provider P] [--model M] [--tier T]"));
                let mut req = serde_json::json!({});
                for (flag, key) in [
                    ("--description", "description"),
                    ("--instructions", "instructions"),
                    ("--provider", "provider"),
                    ("--model", "model"),
                    ("--tier", "tier"),
                ] {
                    if let Some(v) = flag_value(&args, flag) {
                        req[key] = serde_json::json!(v);
                    }
                }
                let body = check(
                    auth(client().post(format!("{}/v1/authoring/drafts/{id}/assist", base())))
                        .json(&req)
                        .send()
                        .unwrap_or_else(|e| die(&e.to_string())),
                );
                println!("{}", serde_json::to_string_pretty(&body).unwrap());
            }
            Some("export") => {
                let id = args
                    .get(2)
                    .unwrap_or_else(|| die("usage: mmctl author export <draft-id> --out <dir>"));
                let out = flag_value(&args, "--out")
                    .unwrap_or_else(|| die("usage: mmctl author export <draft-id> --out <dir>"));
                let body = check(
                    auth(client().post(format!("{}/v1/authoring/drafts/{id}/export", base())))
                        .send()
                        .unwrap_or_else(|e| die(&e.to_string())),
                );
                let out_dir = std::path::Path::new(out);
                let files = body["files"].as_object().cloned().unwrap_or_default();
                let mut written: std::collections::BTreeMap<String, String> = Default::default();
                for (path, yaml) in &files {
                    if !safe_rel_path(path) {
                        die(&format!("bundle names an unsafe path '{path}' — refusing"));
                    }
                    let yaml = yaml.as_str().unwrap_or_default();
                    let target = out_dir.join(path);
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent)
                            .unwrap_or_else(|e| die(&format!("mkdir {}: {e}", parent.display())));
                    }
                    std::fs::write(&target, yaml)
                        .unwrap_or_else(|e| die(&format!("write {}: {e}", target.display())));
                    // Verify what actually landed on disk, not what we held
                    // in memory.
                    written.insert(path.clone(), read_normalized(&target));
                }
                verify_bundle_files(&body, &written).unwrap_or_else(|e| die(&e));
                let bundle_path = out_dir.join("bundle.json");
                std::fs::write(
                    &bundle_path,
                    serde_json::to_string_pretty(&body).unwrap(),
                )
                .unwrap_or_else(|e| die(&format!("write {}: {e}", bundle_path.display())));
                eprintln!(
                    "exported + verified {} files to {} (manifest {})",
                    written.len(),
                    out_dir.display(),
                    body["manifest_hash"].as_str().unwrap_or_default()
                );
                println!("{}", serde_json::to_string_pretty(&body["validation"]).unwrap());
            }
            _ => die("usage: mmctl author patterns | pattern <id> | new <name> [--pattern <id>] [--seed] | list | show <draft-id> | answer <draft-id> -f <answers.yaml> [--no-materialize] | validate <draft-id> | assist <draft-id> [...] | export <draft-id> --out <dir>"),
        },
        Some("bundle") if args.get(1).map(String::as_str) == Some("apply") => {
            // The prod-deploy path: verify hashes, then POST each file in
            // apply_order through the SAME kind-sniffed routes `apply` uses.
            let path = flag_value(&args, "-f")
                .unwrap_or_else(|| die("usage: mmctl bundle apply -f <bundle.json> [--dir <dir>]"));
            let raw = std::fs::read_to_string(path)
                .unwrap_or_else(|e| die(&format!("read {path}: {e}")));
            let bundle: serde_json::Value =
                serde_json::from_str(&raw).unwrap_or_else(|e| die(&format!("bundle json: {e}")));
            if bundle["kind"].as_str() != Some("MunariumAuthoringBundle") {
                die("file is not an munarium authoring bundle (kind: MunariumAuthoringBundle)");
            }
            // --dir: the git-reviewed files on disk are the source; each must
            // still match the bundle's declared hash (LF-normalized, so an
            // autocrlf checkout is not a false integrity alarm).
            let files: std::collections::BTreeMap<String, String> =
                if let Some(dir) = flag_value(&args, "--dir") {
                    bundle["hashes"]
                        .as_object()
                        .map(|m| m.keys())
                        .into_iter()
                        .flatten()
                        .map(|p| {
                            if !safe_rel_path(p) {
                                die(&format!("bundle names an unsafe path '{p}' — refusing"));
                            }
                            let f = std::path::Path::new(dir).join(p);
                            (p.clone(), read_normalized(&f))
                        })
                        .collect()
                } else {
                    bundle["files"]
                        .as_object()
                        .map(|m| {
                            m.iter()
                                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                                .collect()
                        })
                        .unwrap_or_default()
                };
            verify_bundle_files(&bundle, &files).unwrap_or_else(|e| die(&e));
            let order: Vec<String> = bundle["apply_order"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            for p in &order {
                let yaml = files
                    .get(p)
                    .unwrap_or_else(|| die(&format!("apply_order names unknown file '{p}'")));
                let route = match yaml_kind(yaml).as_str() {
                    "Shape" => "/v1/shapes",
                    "Runbook" => "/v1/runbooks",
                    other => die(&format!(
                        "'{p}' declares kind '{other}' — a bundle holds only Shape and Runbook"
                    )),
                };
                let body = check(
                    auth(client().post(format!("{}{route}", base())))
                        .header("content-type", "text/yaml")
                        .body(yaml.clone())
                        .send()
                        .unwrap_or_else(|e| die(&e.to_string())),
                );
                eprintln!("applied {p}");
                println!("{}", serde_json::to_string_pretty(&body).unwrap());
            }
        }
        Some("bulk") => match args.get(1).map(String::as_str) {
            // Chunked, resumable corpus upload over the bulk-session plane.
            // Idempotent end to end: re-running the same directory re-sends
            // only what the server still reports as needed.
            Some("upload") => {
                let dir = flag_value(&args, "--dir").unwrap_or_else(|| {
                    die("usage: mmctl bulk upload --dir <dir> [--prefix <p/>] [--label L] [--resume <bulk-id>] [--chunk-files N] [--chunk-bytes N]")
                });
                let prefix = flag_value(&args, "--prefix").cloned().unwrap_or_default();
                if !prefix.is_empty() && !prefix.ends_with('/') {
                    die("--prefix must end with '/'");
                }
                let chunk_files: usize = flag_value(&args, "--chunk-files")
                    .map(|v| v.parse().unwrap_or_else(|_| die("--chunk-files must be an integer")))
                    .unwrap_or(400)
                    .min(500);
                let chunk_bytes: u64 = flag_value(&args, "--chunk-bytes")
                    .map(|v| v.parse().unwrap_or_else(|_| die("--chunk-bytes must be an integer")))
                    .unwrap_or(140_000_000);

                // Walk the directory: logical path = prefix + rel path ('/').
                let root = std::path::Path::new(dir);
                if !root.is_dir() {
                    die(&format!("--dir {dir} is not a directory"));
                }
                let mut paths: Vec<(String, std::path::PathBuf)> = Vec::new();
                let mut stack = vec![root.to_path_buf()];
                while let Some(d) = stack.pop() {
                    let entries = std::fs::read_dir(&d)
                        .unwrap_or_else(|e| die(&format!("read_dir {}: {e}", d.display())));
                    for entry in entries {
                        let entry = entry.unwrap_or_else(|e| die(&e.to_string()));
                        let p = entry.path();
                        if p.is_dir() {
                            stack.push(p);
                        } else if p.is_file() {
                            let rel = p
                                .strip_prefix(root)
                                .unwrap_or_else(|e| die(&e.to_string()))
                                .to_string_lossy()
                                .replace('\\', "/");
                            paths.push((format!("{prefix}{rel}"), p));
                        }
                    }
                }
                paths.sort();
                if paths.is_empty() {
                    die("no files found");
                }
                eprintln!("hashing {} files...", paths.len());
                let mut manifest = Vec::with_capacity(paths.len());
                let mut by_name: std::collections::HashMap<String, (std::path::PathBuf, String, u64)> =
                    Default::default();
                for (logical, p) in &paths {
                    let bytes = std::fs::read(p)
                        .unwrap_or_else(|e| die(&format!("read {}: {e}", p.display())));
                    let sha = sha256_hex(&bytes);
                    let media = media_type_for(logical);
                    manifest.push(serde_json::json!({
                        "filename": logical, "sha256": sha,
                        "bytes_len": bytes.len(), "media_type": media,
                    }));
                    by_name.insert(logical.clone(), (p.clone(), media.to_string(), bytes.len() as u64));
                }

                // Open (or resume) the session; the server's diff is the
                // work list — no client-side ledger required.
                let (bulk_id, needed): (String, Vec<String>) =
                    if let Some(resume) = flag_value(&args, "--resume") {
                        let body = check(
                            auth(client().get(format!(
                                "{}/v1/ingest/bulk/{resume}?include_needed=true",
                                base()
                            )))
                            .send()
                            .unwrap_or_else(|e| die(&e.to_string())),
                        );
                        if body["status"].as_str() != Some("open") {
                            die(&format!(
                                "session {resume} is {}; only open sessions accept chunks",
                                body["status"].as_str().unwrap_or("unknown")
                            ));
                        }
                        let needed = body["needed"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|v| v.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default();
                        (resume.clone(), needed)
                    } else {
                        let mut req = serde_json::json!({ "files": manifest });
                        if let Some(label) = flag_value(&args, "--label") {
                            req["label"] = serde_json::json!(label);
                        }
                        let body = check(
                            auth(client().post(format!("{}/v1/ingest/bulk", base())))
                                .json(&req)
                                .send()
                                .unwrap_or_else(|e| die(&e.to_string())),
                        );
                        let bulk_id = body["bulk_id"].as_str().unwrap_or_default().to_string();
                        eprintln!(
                            "session {bulk_id}: {} total, {} already present, {} needed",
                            body["total"], body["already_present"],
                            body["needed"].as_array().map(|a| a.len()).unwrap_or(0)
                        );
                        let needed = body["needed"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|v| v.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default();
                        (bulk_id, needed)
                    };

                // Chunk by count AND raw bytes; retry each chunk (idempotent).
                use base64::Engine as _;
                let mut sent = 0usize;
                let mut chunk: Vec<serde_json::Value> = Vec::new();
                let mut chunk_size = 0u64;
                let total_needed = needed.len();
                let flush = |chunk: &mut Vec<serde_json::Value>, sent: &mut usize| {
                    if chunk.is_empty() {
                        return;
                    }
                    let files: Vec<serde_json::Value> = std::mem::take(chunk);
                    let n = files.len();
                    let mut attempt = 0;
                    loop {
                        attempt += 1;
                        let resp = auth(client().post(format!(
                            "{}/v1/ingest/bulk/{bulk_id}/chunk",
                            base()
                        )))
                        .json(&serde_json::json!({ "files": files }))
                        .send();
                        match resp {
                            Ok(r) if r.status().is_success() => {
                                let body: serde_json::Value = r.json().unwrap_or_default();
                                *sent += n;
                                eprintln!(
                                    "chunk ok ({n} files; {sent}/{total_needed}): stored {} skipped {} pending {} failed {}",
                                    body["stored"], body["skipped_existing"],
                                    body["pending"], body["failed"]
                                );
                                let errs: Vec<&serde_json::Value> = body["results"]
                                    .as_array()
                                    .map(|a| a.iter().filter(|x| !x["error"].is_null()).collect())
                                    .unwrap_or_default();
                                for e in errs.iter().take(5) {
                                    eprintln!("  FAILED {}: {}", e["filename"], e["error"]);
                                }
                                if errs.len() > 5 {
                                    eprintln!("  ... {} more per-file failures", errs.len() - 5);
                                }
                                break;
                            }
                            Ok(r) if attempt < 4 && r.status().is_server_error() => {
                                eprintln!("chunk attempt {attempt} got {}; retrying...", r.status());
                                std::thread::sleep(Duration::from_secs(5 * attempt));
                            }
                            Ok(r) => {
                                let body: serde_json::Value = r.json().unwrap_or_default();
                                die(&format!("chunk failed: {body}"));
                            }
                            Err(e) if attempt < 4 => {
                                eprintln!("chunk attempt {attempt} error {e}; retrying...");
                                std::thread::sleep(Duration::from_secs(5 * attempt));
                            }
                            Err(e) => die(&format!("chunk failed: {e}")),
                        }
                    }
                };
                for name in &needed {
                    let Some((path, media, len)) = by_name.get(name) else {
                        eprintln!("WARNING: server needs '{name}' but it is not under --dir; skipping");
                        continue;
                    };
                    if chunk.len() >= chunk_files || (chunk_size + len > chunk_bytes && !chunk.is_empty())
                    {
                        flush(&mut chunk, &mut sent);
                        chunk_size = 0;
                    }
                    let bytes = std::fs::read(path)
                        .unwrap_or_else(|e| die(&format!("read {}: {e}", path.display())));
                    chunk.push(serde_json::json!({
                        "filename": name, "media_type": media,
                        "content_base64": base64::engine::general_purpose::STANDARD.encode(&bytes),
                    }));
                    chunk_size += len;
                }
                flush(&mut chunk, &mut sent);

                // Finalize: the server re-verifies every manifest entry.
                let body = check(
                    auth(client().post(format!(
                        "{}/v1/ingest/bulk/{bulk_id}/complete",
                        base()
                    )))
                    .send()
                    .unwrap_or_else(|e| die(&e.to_string())),
                );
                println!("{}", serde_json::to_string_pretty(&body).unwrap());
                if body["status"].as_str() != Some("completed") {
                    die(&format!(
                        "session {bulk_id} incomplete — resume with: mmctl bulk upload --dir {dir} --prefix '{prefix}' --resume {bulk_id}"
                    ));
                }
            }
            Some("status") => {
                let id = args
                    .get(2)
                    .unwrap_or_else(|| die("usage: mmctl bulk status <bulk-id> [--needed]"));
                let needed = args.iter().any(|a| a == "--needed");
                let body = check(
                    auth(client().get(format!(
                        "{}/v1/ingest/bulk/{id}?include_needed={needed}",
                        base()
                    )))
                    .send()
                    .unwrap_or_else(|e| die(&e.to_string())),
                );
                println!("{}", serde_json::to_string_pretty(&body).unwrap());
            }
            Some("complete") => {
                let id = args
                    .get(2)
                    .unwrap_or_else(|| die("usage: mmctl bulk complete <bulk-id>"));
                let body = check(
                    auth(client().post(format!("{}/v1/ingest/bulk/{id}/complete", base())))
                        .send()
                        .unwrap_or_else(|e| die(&e.to_string())),
                );
                println!("{}", serde_json::to_string_pretty(&body).unwrap());
            }
            _ => die("usage: mmctl bulk upload --dir <dir> [--prefix <p/>] [--label L] [--resume <bulk-id>] | bulk status <bulk-id> [--needed] | bulk complete <bulk-id>"),
        },
        Some("get") if args.get(1).map(String::as_str) == Some("run") => {
            let run_id = args
                .get(2)
                .unwrap_or_else(|| die("usage: mmctl get run <run-id>"));
            let body = check(
                auth(client().get(format!("{}/v1/runs/{run_id}", base())))
                    .send()
                    .unwrap_or_else(|e| die(&e.to_string())),
            );
            println!("{}", serde_json::to_string_pretty(&body).unwrap());
        }
        // The derived-index tier. Reporting and operating
        // on artifacts; nothing here changes which index version is ACTIVE,
        // and a mirror build never writes the `serving` binding.
        Some("datastore") => match args.get(1).map(String::as_str) {
            Some("status") => {
                let v = args
                    .get(2)
                    .unwrap_or_else(|| die("usage: mmctl datastore status <index-version-id>"));
                let body = check(
                    auth(client().get(format!("{}/v1/index-artifacts/{v}", base())))
                        .send()
                        .unwrap_or_else(|e| die(&e.to_string())),
                );
                println!("{}", serde_json::to_string_pretty(&body).unwrap());
            }
            Some("verify") => {
                let v = args
                    .get(2)
                    .unwrap_or_else(|| die("usage: mmctl datastore verify <index-version-id>"));
                let body = check(
                    auth(client().post(format!("{}/v1/index-artifacts/{v}/verify", base())))
                        .send()
                        .unwrap_or_else(|e| die(&e.to_string())),
                );
                println!("{}", serde_json::to_string_pretty(&body).unwrap());
                // Exit 3 on an artifact that failed verification, so CI can
                // tell a broken artifact from a broken command -- the same
                // convention `mmctl matrix verify` uses.
                let failed = body["results"]
                    .as_array()
                    .map(|r| r.iter().any(|x| x["verified"] == serde_json::Value::Bool(false)))
                    .unwrap_or(false);
                if failed {
                    std::process::exit(3);
                }
            }
            Some("rebuild") => {
                let v = args
                    .get(2)
                    .unwrap_or_else(|| die("usage: mmctl datastore rebuild <index-version-id>"));
                let body = check(
                    auth(client().post(format!("{}/v1/index-artifacts/{v}/rebuild", base())))
                        .send()
                        .unwrap_or_else(|e| die(&e.to_string())),
                );
                println!("{}", serde_json::to_string_pretty(&body).unwrap());
            }
            Some("backfill") => {
                let c = args
                    .get(2)
                    .unwrap_or_else(|| die("usage: mmctl datastore backfill <collection-id>"));
                let body = check(
                    auth(client().post(format!("{}/v1/index-artifacts/backfill", base())))
                        .json(&serde_json::json!({ "collection_id": c }))
                        .send()
                        .unwrap_or_else(|e| die(&e.to_string())),
                );
                println!("{}", serde_json::to_string_pretty(&body).unwrap());
                // Incomplete is not an error -- a version another node is
                // building will finish -- but it is not success either, and a
                // rollout gate must be able to tell the difference.
                if body["complete"] != serde_json::Value::Bool(true) {
                    std::process::exit(3);
                }
            }
            Some("bind") => {
                // mmctl datastore bind <index-version-id> <staged|shadow> <artifact-id>
                //     [--expect <generation>] [--reason <text>]
                let v = args
                    .get(2)
                    .unwrap_or_else(|| die("usage: mmctl datastore bind <index-version-id> <staged|shadow> <artifact-id> [--expect <generation>] [--reason <text>]"));
                let slot = args
                    .get(3)
                    .unwrap_or_else(|| die("bind needs a slot: staged or shadow"));
                let artifact = args
                    .get(4)
                    .unwrap_or_else(|| die("bind needs an artifact id"));
                let mut body = serde_json::json!({ "slot": slot, "artifact_id": artifact });
                let mut i = 5;
                while i < args.len() {
                    match args[i].as_str() {
                        "--expect" => {
                            let g: i64 = args
                                .get(i + 1)
                                .and_then(|x| x.parse().ok())
                                .unwrap_or_else(|| die("--expect needs a generation number"));
                            body["expected_generation"] = serde_json::json!(g);
                            i += 2;
                        }
                        "--reason" => {
                            let r = args.get(i + 1).unwrap_or_else(|| die("--reason needs text"));
                            body["reason"] = serde_json::json!(r);
                            i += 2;
                        }
                        other => die(&format!("unknown flag {other:?}")),
                    }
                }
                let body = check(
                    auth(client().post(format!("{}/v1/index-artifacts/{v}/bind", base())))
                        .json(&body)
                        .send()
                        .unwrap_or_else(|e| die(&e.to_string())),
                );
                println!("{}", serde_json::to_string_pretty(&body).unwrap());
            }
            Some("promote") => {
                // mmctl datastore promote <index-version-id> --staged <g> [--serving <g>] [--reason <text>]
                let v = args.get(2).unwrap_or_else(|| {
                    die("usage: mmctl datastore promote <index-version-id> --staged <generation> [--serving <generation>] [--reason <text>]")
                });
                let mut staged: Option<i64> = None;
                let mut serving: i64 = 0;
                let mut reason: Option<String> = None;
                let mut i = 3;
                while i < args.len() {
                    match args[i].as_str() {
                        "--staged" => {
                            staged = args.get(i + 1).and_then(|x| x.parse().ok());
                            i += 2;
                        }
                        "--serving" => {
                            serving = args
                                .get(i + 1)
                                .and_then(|x| x.parse().ok())
                                .unwrap_or_else(|| die("--serving needs a generation"));
                            i += 2;
                        }
                        "--reason" => {
                            reason = args.get(i + 1).cloned();
                            i += 2;
                        }
                        other => die(&format!("unknown flag {other:?}")),
                    }
                }
                let staged =
                    staged.unwrap_or_else(|| die("promote needs --staged <generation>, read from datastore status"));
                let mut body = serde_json::json!({
                    "expected_staged_generation": staged,
                    "expected_serving_generation": serving,
                });
                if let Some(r) = reason {
                    body["reason"] = serde_json::json!(r);
                }
                let body = check(
                    auth(client().post(format!("{}/v1/index-artifacts/{v}/promote", base())))
                        .json(&body)
                        .send()
                        .unwrap_or_else(|e| die(&e.to_string())),
                );
                println!("{}", serde_json::to_string_pretty(&body).unwrap());
            }
            Some("rollout") => {
                // mmctl datastore rollout get <kind> <id>
                // mmctl datastore rollout set <kind> <id> <postgres|datastore>
                //     [--prewarm] [--expect <g>] [--reason <text>]
                match args.get(2).map(String::as_str) {
                    Some("get") => {
                        let kind = args.get(3).unwrap_or_else(|| die("rollout get <kind> <id>"));
                        let id = args.get(4).unwrap_or_else(|| die("rollout get <kind> <id>"));
                        let body = check(
                            auth(client().get(format!(
                                "{}/v1/retrieval-rollout/{kind}/{id}",
                                base()
                            )))
                            .send()
                            .unwrap_or_else(|e| die(&e.to_string())),
                        );
                        println!("{}", serde_json::to_string_pretty(&body).unwrap());
                    }
                    Some("set") => {
                        let kind = args
                            .get(3)
                            .unwrap_or_else(|| die("rollout set <kind> <id> <serving>"));
                        let id = args
                            .get(4)
                            .unwrap_or_else(|| die("rollout set <kind> <id> <serving>"));
                        let serving = args
                            .get(5)
                            .unwrap_or_else(|| die("rollout set <kind> <id> <postgres|datastore>"));
                        let mut body = serde_json::json!({
                            "scope_kind": kind, "scope_id": id, "serving": serving,
                            "prewarm_staged": false,
                        });
                        let mut i = 6;
                        while i < args.len() {
                            match args[i].as_str() {
                                "--prewarm" => {
                                    body["prewarm_staged"] = serde_json::json!(true);
                                    i += 1;
                                }
                                "--expect" => {
                                    let g: i64 = args
                                        .get(i + 1)
                                        .and_then(|x| x.parse().ok())
                                        .unwrap_or_else(|| die("--expect needs a generation"));
                                    body["expected_generation"] = serde_json::json!(g);
                                    i += 2;
                                }
                                "--reason" => {
                                    let r =
                                        args.get(i + 1).unwrap_or_else(|| die("--reason needs text"));
                                    body["reason"] = serde_json::json!(r);
                                    i += 2;
                                }
                                other => die(&format!("unknown flag {other:?}")),
                            }
                        }
                        let body = check(
                            auth(client().put(format!("{}/v1/retrieval-rollout", base())))
                                .json(&body)
                                .send()
                                .unwrap_or_else(|e| die(&e.to_string())),
                        );
                        println!("{}", serde_json::to_string_pretty(&body).unwrap());
                    }
                    _ => die("usage: mmctl datastore rollout get <kind> <id> | set <kind> <id> <postgres|datastore> [--prewarm] [--expect <g>] [--reason <text>]"),
                }
            }
            Some("jobs") => {
                // mmctl datastore jobs enqueue <backfill|rebuild|direct> <target> [--max-chars N] [--watermark N]
                // mmctl datastore jobs get <job-id> | list | cancel <job-id>
                match args.get(2).map(String::as_str) {
                    Some("enqueue") => {
                        let kind = args
                            .get(3)
                            .unwrap_or_else(|| die("jobs enqueue <backfill|rebuild|direct> <target>"));
                        let target = args
                            .get(4)
                            .unwrap_or_else(|| die("jobs enqueue needs a target id"));
                        let mut body = if kind == "rebuild" {
                            serde_json::json!({ "kind": kind, "index_version_id": target })
                        } else {
                            serde_json::json!({ "kind": kind, "collection_id": target })
                        };
                        let mut i = 5;
                        while i < args.len() {
                            match args[i].as_str() {
                                "--max-chars" => {
                                    body["max_chars"] = serde_json::json!(args
                                        .get(i + 1)
                                        .and_then(|x| x.parse::<u64>().ok())
                                        .unwrap_or_else(|| die("--max-chars needs a number")));
                                    i += 2;
                                }
                                "--watermark" => {
                                    body["watermark_seq"] = serde_json::json!(args
                                        .get(i + 1)
                                        .and_then(|x| x.parse::<u64>().ok())
                                        .unwrap_or_else(|| die("--watermark needs a number")));
                                    i += 2;
                                }
                                other => die(&format!("unknown flag {other:?}")),
                            }
                        }
                        let body = check(
                            auth(client().post(format!("{}/v1/index-build-jobs", base())))
                                .json(&body)
                                .send()
                                .unwrap_or_else(|e| die(&e.to_string())),
                        );
                        println!("{}", serde_json::to_string_pretty(&body).unwrap());
                    }
                    Some("get") => {
                        let id = args.get(3).unwrap_or_else(|| die("jobs get <job-id>"));
                        let body = check(
                            auth(client().get(format!("{}/v1/index-build-jobs/{id}", base())))
                                .send()
                                .unwrap_or_else(|e| die(&e.to_string())),
                        );
                        println!("{}", serde_json::to_string_pretty(&body).unwrap());
                        // A poller in a script needs the state as an exit code:
                        // 0 done, 3 not yet, 4 failed.
                        match body["state"].as_str() {
                            Some("succeeded") => {}
                            Some("failed") | Some("cancelled") => std::process::exit(4),
                            _ => std::process::exit(3),
                        }
                    }
                    Some("list") => {
                        let body = check(
                            auth(client().get(format!("{}/v1/index-build-jobs", base())))
                                .send()
                                .unwrap_or_else(|e| die(&e.to_string())),
                        );
                        println!("{}", serde_json::to_string_pretty(&body).unwrap());
                    }
                    Some("cancel") => {
                        let id = args.get(3).unwrap_or_else(|| die("jobs cancel <job-id>"));
                        let body = check(
                            auth(client()
                                .post(format!("{}/v1/index-build-jobs/{id}/cancel", base())))
                            .send()
                            .unwrap_or_else(|e| die(&e.to_string())),
                        );
                        println!("{}", serde_json::to_string_pretty(&body).unwrap());
                    }
                    _ => die("usage: mmctl datastore jobs enqueue <kind> <target> | get <job-id> | list | cancel <job-id>"),
                }
            }
            _ => die(
                "usage: mmctl datastore status <index-version-id> | verify <index-version-id> |                  rebuild <index-version-id> | backfill <collection-id> |                  bind <index-version-id> <staged|shadow> <artifact-id> [--expect <g>] [--reason <text>] |                  promote <index-version-id> --staged <g> [--serving <g>] |                  rollout get|set ... | jobs enqueue|get|list|cancel ...",
            ),
        },
        // One CLI for GitOps across both trees: every
        // `mmctl matrix` call is a REST call to Matrix's own API, forwarded
        // verbatim. No matrix/ crate is linked — ground rule 1.
        Some("matrix") => matrix::dispatch(&args),
        _ => {
            eprintln!("mmctl {} — usage:", env!("CARGO_PKG_VERSION"));
            eprintln!(
                "  apply -f <file.yaml>                    (Shape | ProviderConfig | Runbook)"
            );
            eprintln!("  run <runbook> [--version-id V] [--watch]");
            eprintln!("  approve <run-id> <step-ordinal>");
            eprintln!("  get run <run-id>");
            eprintln!("  runbook list | runbook info <name> | runbook validate -f <file.yaml> [--suggest]");
            eprintln!("  author patterns | pattern <id> | new <name> [--pattern <id>] [--seed]");
            eprintln!("  author list | show <d> | answer <d> -f <answers.yaml> [--no-materialize] | validate <d> | assist <d> | export <d> --out <dir>");
            eprintln!("  bundle apply -f <bundle.json> [--dir <dir>]      (prod deploy; verifies hashes first)");
            eprintln!("  bulk upload --dir <dir> [--prefix <p/>] [--label L] [--resume <bulk-id>]   (chunked corpus load)");
            eprintln!("  bulk status <bulk-id> [--needed] | bulk complete <bulk-id>");
            eprintln!("  token issue <uid> <level> <scopes,csv> [compartments,csv]   (mgmt token)");
            eprintln!("  datastore status <index-version-id> | verify <v> | rebuild <v> | backfill <collection-id> | bind <v> <staged|shadow> <artifact-id>");
            eprintln!("        (derived-index artifacts; verify/backfill exit 3 on a failure or an incomplete scope)");
            eprintln!(
                "env: MUNARIUMCTL_URL (default http://localhost:8080), MUNARIUMCTL_TOKEN, MUNARIUMCTL_UID"
            );
            std::process::exit(2);
        }
    }
}
