// SPDX-License-Identifier: Apache-2.0
//! `mxctl` — the Matrix operator CLI.
//!
//! Hand-rolled argument parsing, no `clap`: the command surface is small, the
//! dependency budget is real (this binary ships in the same distroless image),
//! and the server workspace made the same call for `mmctl`.
//!
//! `mxctl validate` runs **locally** when no server is configured and against
//! `/v1/assets/validate` when one is — the same validators either way, because
//! both call `munarium-matrix-types`. A CLI that validated differently from
//! the server it deploys to would be worse than no CLI.

use munarium_matrix_client::MatrixClient;
use munarium_matrix_types::{parse_asset, validate};

const USAGE: &str = r#"mxctl — Munarium Matrix operator CLI

USAGE
  mxctl <command> [args]

COMMANDS
  apply -f <file>            Apply an asset (DataSource | QueryContract | ClaimMapping)
  validate -f <file>         Validate without applying (local when no server is set)
  list <kind> [--all]        kind: datasources | contracts | mappings
  info <kind> <name>         Print the applied YAML, verbatim
  journal [--limit N]        Recent journal entries (mgmt token)
  health                     Per-source health
  version                    Server build, role and contract version

  verify <contract>          Run a contract's verified questions (exit 3 if any fail)
  verify-view <view>         Run a metric view's questions and record its fingerprint (exit 3 if any fail)
  sync <source>              Enqueue a sync run, one job per authorization class
  reconcile <mapping>        Enqueue a reconcile pass

  mappings status <name>                       Promotion state and gate numbers
  mappings history <name> [--limit <n>]        Gate values per run over time, vs the CURRENT thresholds
  mappings promote <name> --decision <id> [--reason <text>]
                                               Let the mapping write canon (gates checked server-side)
  mappings demote <name> --decision <id>       Stop the writes on the next poll
  mappings rollback <name> --decision <id>     Supersede every proposal with its prior value

ENVIRONMENT
  MUNARIUM_MATRIX_URL        Base URL (default http://localhost:8180)
  MUNARIUM_MATRIX_TOKEN      Bearer token

EXIT CODES
  0 ok · 1 command failed · 2 usage error · 3 validation findings or a failed
  verified question
"#;

fn main() {
    // Before any TLS: rustls 0.23 refuses to guess a provider when more than
    // one feature is enabled, and this workspace has two. See the function's
    // docs for how a live cycle found it and why compose could not.
    munarium_matrix_adapter::install_crypto_provider();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(run(&args));
    std::process::exit(code);
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).map(|s| s.as_str())
}

fn has(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn client() -> MatrixClient {
    let url = std::env::var("MUNARIUM_MATRIX_URL")
        .unwrap_or_else(|_| "http://localhost:8180".to_string());
    let token = std::env::var("MUNARIUM_MATRIX_TOKEN").ok();
    MatrixClient::new(&url, token.as_deref())
}

fn kind_path(kind: &str) -> Option<&'static str> {
    match kind {
        "datasources" | "datasource" | "ds" => Some("datasources"),
        "contracts" | "contract" => Some("contracts"),
        "mappings" | "mapping" => Some("mappings"),
        "metricviews" | "metricview" | "mv" => Some("metricviews"),
        "dataviews" | "dataview" | "dv" => Some("dataviews"),
        _ => None,
    }
}

async fn run(args: &[String]) -> i32 {
    let Some(cmd) = args.first().map(|s| s.as_str()) else {
        print!("{USAGE}");
        return 2;
    };

    match cmd {
        "help" | "--help" | "-h" => {
            print!("{USAGE}");
            0
        }

        "validate" => {
            let Some(path) = flag(args, "-f").or_else(|| flag(args, "--file")) else {
                eprintln!("validate needs -f <file>");
                return 2;
            };
            let yaml = match std::fs::read_to_string(path) {
                Ok(y) => y,
                Err(e) => {
                    eprintln!("{path}: {e}");
                    return 1;
                }
            };
            // Local validation by default: an operator editing a file should
            // not need a running server to find a typo.
            let findings = match parse_asset(&yaml) {
                Ok(a) => {
                    println!("{} {}", a.kind(), a.asset_ref());
                    a.validate()
                }
                Err(e) => {
                    eprintln!("parse: {e}");
                    return 3;
                }
            };
            print_findings(&findings);
            if validate::is_valid(&findings) {
                println!("valid");
                0
            } else {
                3
            }
        }

        "apply" => {
            let Some(path) = flag(args, "-f").or_else(|| flag(args, "--file")) else {
                eprintln!("apply needs -f <file>");
                return 2;
            };
            let yaml = match std::fs::read_to_string(path) {
                Ok(y) => y,
                Err(e) => {
                    eprintln!("{path}: {e}");
                    return 1;
                }
            };
            // Validate locally first so an obviously-bad file never becomes a
            // round trip, then let the server be the authority.
            match parse_asset(&yaml) {
                Ok(a) => {
                    let findings = a.validate();
                    if !validate::is_valid(&findings) {
                        print_findings(&findings);
                        eprintln!("not applied");
                        return 3;
                    }
                }
                Err(e) => {
                    eprintln!("parse: {e}");
                    return 3;
                }
            }
            match client().apply(&yaml).await {
                Ok(r) => {
                    println!(
                        "{} {} {}",
                        r.kind,
                        r.asset_ref,
                        if r.unchanged {
                            "(unchanged)"
                        } else {
                            "applied"
                        }
                    );
                    print_findings(&r.findings);
                    0
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }

        "verify" => {
            let Some(name) = args.get(1) else {
                eprintln!("verify needs a contract name");
                return 2;
            };
            match client().verify(name).await {
                Ok(r) => {
                    for q in &r.questions {
                        println!("{:<6} {}", if q.ok { "ok" } else { "FAIL" }, q.question);
                        for f in &q.failures {
                            println!("       {f}");
                        }
                    }
                    println!("{} passed, {} failed", r.passed, r.failed);
                    // Exit 3, not 1: the command worked and the CONTRACT did
                    // not. A CI step needs to tell those apart.
                    if r.failed == 0 {
                        0
                    } else {
                        3
                    }
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }

        "verify-view" => {
            let Some(name) = args.get(1) else {
                eprintln!("verify-view needs a metric view name");
                return 2;
            };
            match client().verify_view(name).await {
                Ok(r) => {
                    for q in &r.questions {
                        println!("{:<6} {}", if q.ok { "ok" } else { "FAIL" }, q.question);
                        for f in &q.failures {
                            println!("       {f}");
                        }
                    }
                    if let Some(fp) = &r.fingerprint {
                        println!("definition {fp}");
                    }
                    println!("{} passed, {} failed", r.passed, r.failed);
                    // Same exit discipline as `verify`: 3 means the VIEW did
                    // not pass, not that the command broke.
                    if r.failed == 0 {
                        0
                    } else {
                        3
                    }
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }

        "sync" => {
            let Some(name) = args.get(1) else {
                eprintln!("sync needs a data source name");
                return 2;
            };
            match client().sync(name).await {
                Ok(r) => {
                    println!("{}", r.detail);
                    for j in &r.jobs {
                        println!("  {j}");
                    }
                    0
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }

        "mappings" => {
            let (Some(sub), Some(name)) = (args.get(1).map(String::as_str), args.get(2)) else {
                eprintln!("mappings needs: status|history|promote|demote|rollback <name>");
                return 2;
            };
            let decision = flag(args, "--decision");
            let need_decision = |what: &str| -> Option<i32> {
                if decision.is_none() {
                    eprintln!(
                        "mappings {what} needs --decision <id>: the operator's record of why"
                    );
                    return Some(2);
                }
                None
            };
            let print_status = |s: &munarium_matrix_types::dto::PromotionStatus| {
                println!(
                    "{}  mode={}  promoted={}{}",
                    s.mapping,
                    s.mode,
                    s.promoted,
                    s.decision_id
                        .as_deref()
                        .map(|d| format!("  decision={d}"))
                        .unwrap_or_default()
                );
                println!("  authority scopes: {}", s.authority_scopes);
                match &s.gates {
                    Some(g) => println!(
                        "  gates (run {}): identity {:.4} (min {:.2})  conformance {:.4} (min {:.2})  observations {}",
                        g.run_id.as_deref().unwrap_or("?"),
                        g.identity_precision,
                        g.min_identity_precision,
                        g.value_conformance,
                        g.min_value_conformance,
                        g.observations
                    ),
                    None => println!("  gates: no completed run yet — run it in shadow first"),
                }
            };
            let result = match sub {
                "status" => client().promotion_status(name).await.map(|s| {
                    print_status(&s);
                    0
                }),
                "history" => {
                    let limit = flag(args, "--limit").and_then(|s| s.parse::<i64>().ok());
                    client().gate_history(name, limit).await.map(|h| {
                        println!("mapping   {}", h.mapping);
                        println!(
                            "gates     identity >= {:.4}   value >= {:.4}",
                            h.min_identity_precision, h.min_value_conformance
                        );
                        println!(
                            "runs      {} completed, {} would pass the CURRENT thresholds",
                            h.runs.len(),
                            h.passing
                        );
                        if h.runs.is_empty() {
                            println!("
(no completed runs yet — a mapping that has never run measures nothing)");
                            return 0;
                        }
                        println!();
                        println!(
                            "{:<22} {:>7} {:>9} {:>9} {:>9} {:>9}  verdict",
                            "ended", "obs", "identity", "margin", "value", "margin"
                        );
                        for r in &h.runs {
                            println!(
                                "{:<22} {:>7} {:>9.4} {:>+9.4} {:>9.4} {:>+9.4}  {}",
                                r.ended_at.as_deref().unwrap_or("-"),
                                r.observations,
                                r.identity_precision,
                                r.identity_margin,
                                r.value_conformance,
                                r.value_margin,
                                if r.would_pass { "pass" } else { "BLOCKED" }
                            );
                        }
                        // The margin columns are the point. A threshold every
                        // run clears by 0.0004 is doing something very
                        // different from one every run clears by 0.05, and a
                        // pass/fail column cannot tell you which you have.
                        println!(
                            "
Margins are signed distance from the threshold. Small positives are                              near-misses;
negatives are what the threshold actually blocked. Change                              the threshold with
MUNARIUM_MATRIX_PROMOTION_MIN_IDENTITY_PRECISION /                              _MIN_VALUE_CONFORMANCE and re-run
this command to see which past runs                              the new number would admit."
                        );
                        0
                    })
                }
                "promote" => {
                    if let Some(code) = need_decision("promote") {
                        return code;
                    }
                    client()
                        .promote(name, decision.unwrap(), flag(args, "--reason"))
                        .await
                        .map(|s| {
                            print_status(&s);
                            0
                        })
                }
                "demote" => {
                    if let Some(code) = need_decision("demote") {
                        return code;
                    }
                    client().demote(name, decision.unwrap()).await.map(|s| {
                        print_status(&s);
                        0
                    })
                }
                "rollback" => {
                    if let Some(code) = need_decision("rollback") {
                        return code;
                    }
                    client().rollback(name, decision.unwrap()).await.map(|r| {
                        println!(
                            "{}: superseded {}  skipped (no prior value) {}  already rolled back {}  disputed {}",
                            r.mapping, r.superseded, r.skipped_no_prior, r.already_rolled_back, r.disputed
                        );
                        0
                    })
                }
                other => {
                    eprintln!("unknown mappings subcommand '{other}' (status|history|promote|demote|rollback)");
                    return 2;
                }
            };
            match result {
                Ok(code) => code,
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }

        "reconcile" => {
            let Some(name) = args.get(1) else {
                eprintln!("reconcile needs a mapping name");
                return 2;
            };
            match client().reconcile(name).await {
                Ok(r) => {
                    println!("{}", r.detail);
                    for j in &r.jobs {
                        println!("  {j}");
                    }
                    0
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }

        "list" => {
            let Some(kind) = args.get(1).and_then(|k| kind_path(k)) else {
                eprintln!("list needs a kind: datasources | contracts | mappings");
                return 2;
            };
            match client().list(kind, has(args, "--all")).await {
                Ok(r) => {
                    if r.assets.is_empty() {
                        println!("(none)");
                    }
                    for a in r.assets {
                        println!(
                            "{:<28} {:<16} {}",
                            a.asset_ref,
                            a.kind,
                            a.source.unwrap_or_default()
                        );
                    }
                    0
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }

        "info" => {
            let (Some(kind), Some(name)) = (args.get(1).and_then(|k| kind_path(k)), args.get(2))
            else {
                eprintln!("info needs a kind and a name");
                return 2;
            };
            match client().get_yaml(kind, name).await {
                Ok(y) => {
                    print!("{y}");
                    0
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }

        "journal" => {
            let limit = flag(args, "--limit")
                .and_then(|l| l.parse().ok())
                .unwrap_or(20);
            match client().journal(limit).await {
                Ok(r) => {
                    for e in r.entries {
                        println!(
                            "{}  {:<10} {:<9} {}{}",
                            e.created_at,
                            e.kind,
                            e.outcome,
                            e.asset_ref.or(e.source).unwrap_or_default(),
                            e.refusal_code
                                .map(|c| format!("  [{c}]"))
                                .unwrap_or_default()
                        );
                    }
                    0
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }

        "health" => match client().healthdata().await {
            Ok(r) => {
                for s in r.sources {
                    println!(
                        "{:<24} {:<10} {}",
                        s.source,
                        if s.reachable { "ok" } else { "unreachable" },
                        s.detail.unwrap_or_default()
                    );
                }
                if r.healthy {
                    0
                } else {
                    1
                }
            }
            Err(e) => {
                eprintln!("{e}");
                1
            }
        },

        "version" => match client().version().await {
            Ok(v) => {
                println!(
                    "matrix {} (role {}, contract {}, target server {})",
                    v.version, v.role, v.contract_version, v.target_server_version
                );
                0
            }
            Err(e) => {
                eprintln!("{e}");
                1
            }
        },

        other => {
            eprintln!("unknown command '{other}'\n");
            print!("{USAGE}");
            2
        }
    }
}

fn print_findings(findings: &[validate::Finding]) {
    for f in findings {
        let severity = if validate::is_error(f) {
            "error"
        } else {
            "note "
        };
        println!("  {severity} {:<38} {}", f.code, f.message);
        if !f.path.is_empty() {
            println!("         at {}", f.path);
        }
    }
}
