// SPDX-License-Identifier: Apache-2.0
//! Operator commands for durable artifact damage (ClearView ART2).
//!
//! `munarium-server artifact-damage <command>` runs against the configured
//! PostgreSQL and exits. It is the only way to preauthorize a repair
//! workflow, deliberately outside the `/v1` API: preauthorization grants the
//! authority to clear a fleet block, and in this release that authority is
//! held by whoever operates the deployment's database credential, the same
//! boundary as migrations and `platform-enroll`. The command never clears a
//! block itself. Clearance happens only when two independent serving replicas
//! verify the repaired bytes (`docs/ops/artifact-damage.md`).
//!
//! ```text
//! munarium-server artifact-damage list --tenant T
//! munarium-server artifact-damage authorize-repair --tenant T --incident I \
//!     --workflow W --actor A [--reason R]
//! ```

use munarium_store_pg::artifact_damage::{ArtifactDamageLedger, Incident, PinState};

use crate::config::{Config, StoreKind};

const USAGE: &str = "usage: munarium-server artifact-damage list --tenant T\n       \
munarium-server artifact-damage authorize-repair --tenant T --incident I --workflow W --actor A [--reason R]";

/// Parse `--name value` pairs, refusing anything else.
fn flags(args: &[String]) -> Result<std::collections::BTreeMap<String, String>, String> {
    let mut out = std::collections::BTreeMap::new();
    let mut it = args.iter();
    while let Some(name) = it.next() {
        let Some(key) = name.strip_prefix("--") else {
            return Err(format!("unexpected argument {name:?}"));
        };
        let Some(value) = it.next() else {
            return Err(format!("--{key} needs a value"));
        };
        if out.insert(key.to_string(), value.clone()).is_some() {
            return Err(format!("--{key} given twice"));
        }
    }
    Ok(out)
}

fn need<'a>(
    flags: &'a std::collections::BTreeMap<String, String>,
    name: &str,
) -> Result<&'a str, String> {
    flags
        .get(name)
        .map(String::as_str)
        .ok_or_else(|| format!("--{name} is required"))
}

fn pin_json(p: &PinState) -> serde_json::Value {
    serde_json::json!({
        "index_version_id": p.index_version_id,
        "artifact_id": p.artifact_id,
        "generation": p.generation,
        "state": p.state,
        "active_incident_id": p.active_incident_id,
    })
}

fn incident_json(i: &Incident) -> serde_json::Value {
    serde_json::json!({
        "incident_id": i.incident_id,
        "index_version_id": i.index_version_id,
        "artifact_id": i.artifact_id,
        "object_key": i.object_key,
        "expected_sha256": i.expected_sha256,
        "state": i.state,
        "blocked_generation": i.blocked_generation,
        "confirming_verifiers": i.confirming_verifiers,
        "block_assurance": i.block_assurance,
        "cleared_generation": i.cleared_generation,
        "clearing_attempt_id": i.clearing_attempt_id,
        "clearing_workflow_id": i.clearing_workflow_id,
        "clearing_verifiers": i.clearing_verifiers,
        "clear_assurance": i.clear_assurance,
    })
}

/// Run one command against `pool`. Split from [`run`] so tests drive it
/// without process configuration.
pub async fn execute(pool: &sqlx::PgPool, args: &[String]) -> Result<serde_json::Value, String> {
    let (command, rest) = args.split_first().ok_or_else(|| USAGE.to_string())?;
    let flags = flags(rest)?;
    let ledger = ArtifactDamageLedger::new(pool.clone(), need(&flags, "tenant")?);
    match command.as_str() {
        "list" => {
            let pins = ledger.pins().await.map_err(|e| e.to_string())?;
            let incidents = ledger.incidents().await.map_err(|e| e.to_string())?;
            Ok(serde_json::json!({
                "pins": pins.iter().map(pin_json).collect::<Vec<_>>(),
                "incidents": incidents.iter().map(incident_json).collect::<Vec<_>>(),
                "history_entries": ledger.history_len().await.map_err(|e| e.to_string())?,
            }))
        }
        "authorize-repair" => {
            let attempt = ledger
                .authorize_repair(
                    need(&flags, "incident")?,
                    need(&flags, "workflow")?,
                    need(&flags, "actor")?,
                    flags.get("reason").map(String::as_str),
                )
                .await
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({
                "attempt_id": attempt.attempt_id,
                "incident_id": attempt.incident_id,
                "index_version_id": attempt.index_version_id,
                "artifact_id": attempt.artifact_id,
                "workflow_id": attempt.workflow_id,
                "authorized_by": attempt.authorized_by,
                "state": attempt.state,
            }))
        }
        other => Err(format!(
            "unknown artifact-damage command {other:?}\n{USAGE}"
        )),
    }
}

/// The subcommand's entry point: returns the process exit code.
pub async fn run(config: &Config, args: &[String]) -> i32 {
    if config.store != StoreKind::Postgres {
        eprintln!("artifact-damage requires MUNARIUM_STORE=postgres");
        return 2;
    }
    let Some(url) = config.database_url.as_deref() else {
        eprintln!("artifact-damage requires a database URL");
        return 2;
    };
    let store =
        match munarium_store_pg::PgStore::connect(url, munarium_store_pg::DEFAULT_TENANT).await {
            Ok(s) => s,
            Err(_) => {
                eprintln!("artifact-damage: database unavailable");
                return 1;
            }
        };
    match execute(store.pool(), args).await {
        Ok(out) => {
            println!("{out}");
            0
        }
        Err(e) => {
            eprintln!("artifact-damage: {e}");
            2
        }
    }
}
