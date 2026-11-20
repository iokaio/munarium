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
//! The same boundary enrolls and revokes verifier public keys. Only an
//! observation signed by an enrolled, unrevoked key counts toward a block or
//! a clearance. `generate-verifier-key` and `verifier-public-key` run without
//! a database and never print the private key.
//!
//! ```text
//! munarium-server artifact-damage list --tenant T
//! munarium-server artifact-damage authorize-repair --tenant T --incident I \
//!     --workflow W --actor A [--reason R]
//! munarium-server artifact-damage generate-verifier-key --out PATH
//! munarium-server artifact-damage verifier-public-key --key-file PATH
//! munarium-server artifact-damage enroll-verifier --public-key K --label L --actor A
//! munarium-server artifact-damage revoke-verifier --fingerprint F --actor A [--reason R]
//! munarium-server artifact-damage list-verifiers
//! ```

use munarium_core::artifact_verifier::VerifierSigningKey;
use munarium_store_pg::artifact_damage::{
    enroll_verifier, revoke_verifier, verifier_keys, ArtifactDamageLedger, Incident, PinState,
    VerifierKeyRecord,
};

use crate::config::{Config, StoreKind};

const USAGE: &str = "usage: munarium-server artifact-damage list --tenant T\n       \
munarium-server artifact-damage authorize-repair --tenant T --incident I --workflow W --actor A [--reason R]\n       \
munarium-server artifact-damage generate-verifier-key --out PATH\n       \
munarium-server artifact-damage verifier-public-key --key-file PATH\n       \
munarium-server artifact-damage enroll-verifier --public-key K --label L --actor A\n       \
munarium-server artifact-damage revoke-verifier --fingerprint F --actor A [--reason R]\n       \
munarium-server artifact-damage list-verifiers";

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

fn key_json(k: &VerifierKeyRecord) -> serde_json::Value {
    serde_json::json!({
        "fingerprint": k.fingerprint,
        "public_key": k.public_key,
        "label": k.label,
        "enrolled_by": k.enrolled_by,
        "enrolled_at": k.enrolled_at,
        "revoked_at": k.revoked_at,
        "revoked_by": k.revoked_by,
        "revoke_reason": k.revoke_reason,
    })
}

/// Write a fresh verifier key to `path`, which must not exist yet. The file
/// holds only the encoded seed; on Unix it is created owner-read/write only.
fn generate_key_file(path: &str) -> Result<serde_json::Value, String> {
    use std::io::Write as _;
    let key = VerifierSigningKey::generate();
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|e| format!("cannot create key file {path:?}: {}", e.kind()))?;
    file.write_all(format!("{}\n", key.encode_secret()).as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|e| format!("cannot write key file {path:?}: {}", e.kind()))?;
    Ok(serde_json::json!({
        "key_file": path,
        "fingerprint": key.fingerprint(),
        "public_key": key.public_key(),
    }))
}

/// The commands that need no database: key generation and public-key
/// derivation. `None` for every other command.
fn offline(command: &str, rest: &[String]) -> Option<Result<serde_json::Value, String>> {
    let run = || -> Result<serde_json::Value, String> {
        let flags = flags(rest)?;
        match command {
            "generate-verifier-key" => generate_key_file(need(&flags, "out")?),
            _ => {
                let key = munarium_retrieval::damage::load_verifier_key(need(&flags, "key-file")?)
                    .map_err(|e| e.to_string())?;
                Ok(serde_json::json!({
                    "fingerprint": key.fingerprint(),
                    "public_key": key.public_key(),
                }))
            }
        }
    };
    matches!(command, "generate-verifier-key" | "verifier-public-key").then(run)
}

/// Run one command against `pool`. Split from [`run`] so tests drive it
/// without process configuration.
pub async fn execute(pool: &sqlx::PgPool, args: &[String]) -> Result<serde_json::Value, String> {
    let (command, rest) = args.split_first().ok_or_else(|| USAGE.to_string())?;
    if let Some(out) = offline(command, rest) {
        return out;
    }
    let flags = flags(rest)?;
    match command.as_str() {
        "enroll-verifier" => {
            return enroll_verifier(
                pool,
                need(&flags, "public-key")?,
                need(&flags, "label")?,
                need(&flags, "actor")?,
            )
            .await
            .map(|k| key_json(&k))
            .map_err(|e| e.to_string());
        }
        "revoke-verifier" => {
            return revoke_verifier(
                pool,
                need(&flags, "fingerprint")?,
                need(&flags, "actor")?,
                flags.get("reason").map(String::as_str),
            )
            .await
            .map(|k| key_json(&k))
            .map_err(|e| e.to_string());
        }
        "list-verifiers" => {
            let keys = verifier_keys(pool).await.map_err(|e| e.to_string())?;
            return Ok(serde_json::json!({
                "verifiers": keys.iter().map(key_json).collect::<Vec<_>>(),
            }));
        }
        _ => {}
    }
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
    if let Some((command, rest)) = args.split_first() {
        if let Some(out) = offline(command, rest) {
            return match out {
                Ok(out) => {
                    println!("{out}");
                    0
                }
                Err(e) => {
                    eprintln!("artifact-damage: {e}");
                    2
                }
            };
        }
    }
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
