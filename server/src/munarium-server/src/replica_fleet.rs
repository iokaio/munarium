// SPDX-License-Identifier: Apache-2.0
//! Replica heartbeats and the fleet-wide console login mode (ClearView
//! SCALE-3 and U10 / LOGIN1, maintainer decisions of 20 November 2026).
//!
//! - **Heartbeats.** Every Server process on the PostgreSQL store writes a row
//!   to `server_replica_heartbeats` (migration 0087) at startup and every
//!   [`HEARTBEAT_INTERVAL`], with an expiry [`HEARTBEAT_TTL_SECS`] ahead on the
//!   database clock. A row identifies the process by a random
//!   [`FleetIdentity::replica_id`] and records its build version, deployment
//!   plane, configured `MUNARIUM_REPLICA_COUNT` and capabilities. Expired rows
//!   are ignored by every reader and deleted by the next heartbeat. The memory
//!   store is single-replica by validation, so it reports itself.
//! - **Scale report.** `GET /v1/reports/scale` and the console Health page
//!   show the live replicas against the configured count and warn when more
//!   are live than configured. A failed read is `unavailable`, never 0.
//! - **Console fleet mode.** `console_fleet_mode` holds one row per
//!   deployment environment (`MUNARIUM_DEPLOYMENT_ENVIRONMENT_ID`): `legacy`
//!   (no row, the default) or `enabled`, and an epoch. Only the
//!   `munarium-server console-fleet` command changes it, behind the database
//!   credential, like `artifact-damage`. A replica serves console sessions
//!   only when its own `MUNARIUM_CONSOLE_SESSIONS=enabled` allows them AND the
//!   fleet mode is `enabled`; otherwise it serves the v1.4 login.
//! - **Cutover preflight.** `console-fleet enable` is refused unless every
//!   live replica reports the `console-sessions` capability, every one is
//!   configured with the expected replica count, and exactly that many are
//!   live. A v1.4 process writes no heartbeat, so it shows as a shortfall.
//! - **Epoch.** Switching back to `legacy` increments the epoch. Sessions
//!   record the epoch they were issued under and are honoured only at that
//!   epoch, so re-enabling refuses every handle issued before a rollback.
//!
//! The design is `docs/design/console-sessions.md`.

use crate::state::AppState;
use munarium_core::{KernelError, Result};
use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;
use utoipa::ToSchema;

/// How often a replica renews its heartbeat.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
/// How long a heartbeat stays live without renewal: three intervals, so one
/// slow or failed write does not drop a running replica.
pub const HEARTBEAT_TTL_SECS: i64 = 45;
/// Upper bound on one heartbeat or fleet read, including the pool wait.
pub const READ_TIMEOUT: Duration = Duration::from_secs(2);
/// The capability a replica reports when it will serve revocable console
/// sessions once the fleet mode is `enabled`: the build supports them and
/// its `MUNARIUM_CONSOLE_SESSIONS=enabled` allows them.
pub const CAP_CONSOLE_SESSIONS: &str = "console-sessions";

/// This process's place in the fleet.
#[derive(Debug)]
pub struct FleetIdentity {
    /// Random per process; never derived from the host.
    pub replica_id: String,
    /// `MUNARIUM_DEPLOYMENT_ENVIRONMENT_ID` (default `local`): the scope of
    /// heartbeats and of the console fleet mode.
    environment: std::sync::RwLock<String>,
}

impl FleetIdentity {
    pub fn new(environment: String) -> Self {
        Self {
            replica_id: format!("r-{}", uuid::Uuid::new_v4().simple()),
            environment: std::sync::RwLock::new(environment),
        }
    }

    pub fn from_env() -> Self {
        Self::new(
            std::env::var("MUNARIUM_DEPLOYMENT_ENVIRONMENT_ID")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "local".into()),
        )
    }

    pub fn environment(&self) -> String {
        self.environment
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Places this state in its own environment (tests: replicas of one test
    /// share a database with every other test).
    #[cfg(test)]
    pub fn set_environment(&self, environment: &str) {
        *self.environment.write().unwrap_or_else(|e| e.into_inner()) = environment.to_string();
    }
}

/// One live replica, as its heartbeat reports it.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct LiveReplica {
    /// Random per-process identity.
    pub replica_id: String,
    /// `MUNARIUM_INSTANCE_ID`, else the host name.
    pub instance_id: String,
    /// Server build version.
    pub version: String,
    /// `MUNARIUM_DEPLOYMENT_PLANE` (`rest` by default, `builder` for a
    /// separate builder group).
    pub plane: String,
    /// The `MUNARIUM_REPLICA_COUNT` this replica runs with.
    pub replica_count: u32,
    /// `console-sessions` when the replica will serve revocable console
    /// sessions under an enabled fleet mode.
    pub capabilities: Vec<String>,
    /// Seconds since its last heartbeat, by the database clock.
    pub seconds_since_heartbeat: f64,
}

impl LiveReplica {
    pub fn supports_console_sessions(&self) -> bool {
        self.capabilities.iter().any(|c| c == CAP_CONSOLE_SESSIONS)
    }
}

fn plane() -> String {
    std::env::var("MUNARIUM_DEPLOYMENT_PLANE")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "rest".into())
}

/// What this replica reports about itself.
pub fn capabilities(state: &AppState) -> Vec<String> {
    let mut caps = Vec::new();
    if state.console.settings.sessions() {
        caps.push(CAP_CONSOLE_SESSIONS.to_string());
    }
    caps
}

fn this_replica(state: &AppState) -> LiveReplica {
    LiveReplica {
        replica_id: state.fleet.replica_id.clone(),
        instance_id: state.config.instance_id.clone(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        plane: plane(),
        replica_count: state.config.replica_count,
        capabilities: capabilities(state),
        seconds_since_heartbeat: 0.0,
    }
}

async fn bounded<T>(
    what: &'static str,
    fut: impl std::future::Future<Output = std::result::Result<T, sqlx::Error>>,
) -> Result<T> {
    tokio::time::timeout(READ_TIMEOUT, fut)
        .await
        .map_err(|_| KernelError::Storage(format!("{what} timed out")))?
        .map_err(|e| KernelError::Storage(e.to_string()))
}

/// Writes this replica's heartbeat and deletes the environment's expired
/// rows. A no-op on the memory store.
pub async fn beat(state: &AppState) -> Result<()> {
    let Some(pool) = state.pg_pool() else {
        return Ok(());
    };
    let me = this_replica(state);
    let env = state.fleet.environment();
    bounded(
        "replica heartbeat",
        sqlx::query(
            "INSERT INTO server_replica_heartbeats
                 (environment_id, replica_id, instance_id, version, plane, replica_count,
                  capabilities, started_at, last_seen_at, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, now(), now(),
                     now() + make_interval(secs => $8::double precision))
             ON CONFLICT (environment_id, replica_id) DO UPDATE SET
                 instance_id = EXCLUDED.instance_id,
                 version = EXCLUDED.version,
                 plane = EXCLUDED.plane,
                 replica_count = EXCLUDED.replica_count,
                 capabilities = EXCLUDED.capabilities,
                 last_seen_at = now(),
                 expires_at = EXCLUDED.expires_at",
        )
        .bind(&env)
        .bind(&me.replica_id)
        .bind(&me.instance_id)
        .bind(&me.version)
        .bind(&me.plane)
        .bind(i32::try_from(me.replica_count).unwrap_or(i32::MAX))
        .bind(&me.capabilities)
        .bind(HEARTBEAT_TTL_SECS as f64)
        .execute(pool),
    )
    .await?;
    bounded(
        "replica heartbeat prune",
        sqlx::query(
            "DELETE FROM server_replica_heartbeats
              WHERE environment_id = $1 AND expires_at <= now()",
        )
        .bind(&env)
        .execute(pool),
    )
    .await?;
    Ok(())
}

/// Renews the heartbeat every [`HEARTBEAT_INTERVAL`] until the state is
/// dropped. A failed write is logged, never fatal: the row then expires and
/// the scale report and cutover preflight see one replica fewer, which is
/// the conservative reading.
pub fn spawn_heartbeat(state: &Arc<AppState>) {
    if state.pg_pool().is_none() {
        return;
    }
    let weak = Arc::downgrade(state);
    tokio::spawn(async move {
        loop {
            let Some(state) = weak.upgrade() else { return };
            if let Err(e) = beat(&state).await {
                tracing::warn!(error = %e, "replica heartbeat failed");
            }
            drop(state);
            tokio::time::sleep(HEARTBEAT_INTERVAL).await;
        }
    });
}

type HeartbeatRow = (String, String, String, String, i32, Vec<String>, f64);

/// The live replicas of `environment`: rows whose expiry is still ahead by
/// the database clock.
pub async fn read_live<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    environment: &str,
) -> Result<Vec<LiveReplica>> {
    let rows: Vec<HeartbeatRow> = bounded(
        "replica heartbeat read",
        sqlx::query_as(
            "SELECT replica_id, instance_id, version, plane, replica_count, capabilities,
                    GREATEST(EXTRACT(EPOCH FROM now() - last_seen_at), 0)::float8
               FROM server_replica_heartbeats
              WHERE environment_id = $1 AND expires_at > now()
              ORDER BY instance_id, replica_id",
        )
        .bind(environment)
        .fetch_all(exec),
    )
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| LiveReplica {
            replica_id: r.0,
            instance_id: r.1,
            version: r.2,
            plane: r.3,
            replica_count: u32::try_from(r.4).unwrap_or(0),
            capabilities: r.5,
            seconds_since_heartbeat: r.6,
        })
        .collect())
}

/// The live replicas sharing this replica's database and environment. The
/// memory store is a single replica by validation and reports itself.
pub async fn live_replicas(state: &AppState) -> Result<Vec<LiveReplica>> {
    match state.pg_pool() {
        Some(pool) => read_live(pool, &state.fleet.environment()).await,
        None => Ok(vec![this_replica(state)]),
    }
}

// ---------------------------------------------------------------------------
// console fleet mode
// ---------------------------------------------------------------------------

/// The console login mode of a whole environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FleetMode {
    /// The v1.4 login on every replica. The default (no row).
    Legacy,
    /// Revocable sessions on every replica whose environment allows them.
    Enabled,
}

impl FleetMode {
    pub fn as_str(self) -> &'static str {
        match self {
            FleetMode::Legacy => "legacy",
            FleetMode::Enabled => "enabled",
        }
    }
}

/// The stored fleet mode of one environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleFleet {
    pub mode: FleetMode,
    /// Incremented on every switch from `enabled` back to `legacy`.
    pub epoch: i64,
    pub changed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub changed_by: Option<String>,
}

impl ConsoleFleet {
    /// No row: the v1.4 login at epoch 0.
    pub fn default_legacy() -> Self {
        Self {
            mode: FleetMode::Legacy,
            epoch: 0,
            changed_at: None,
            changed_by: None,
        }
    }
}

type FleetRow = (String, i64, chrono::DateTime<chrono::Utc>, String);

fn fleet_from(row: Option<FleetRow>) -> ConsoleFleet {
    match row {
        None => ConsoleFleet::default_legacy(),
        Some((mode, epoch, at, by)) => ConsoleFleet {
            // The column's CHECK admits only these two values.
            mode: if mode == "enabled" {
                FleetMode::Enabled
            } else {
                FleetMode::Legacy
            },
            epoch,
            changed_at: Some(at),
            changed_by: Some(by),
        },
    }
}

/// Reads an environment's fleet mode.
pub async fn read_console_fleet<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    environment: &str,
) -> Result<ConsoleFleet> {
    let row: Option<FleetRow> = bounded(
        "console fleet mode read",
        sqlx::query_as(
            "SELECT mode, epoch, changed_at, changed_by
               FROM console_fleet_mode WHERE environment_id = $1",
        )
        .bind(environment)
        .fetch_optional(exec),
    )
    .await?;
    Ok(fleet_from(row))
}

/// The fleet mode of this replica's environment, or `None` on the memory
/// store, which has no shared mode: its own setting is the whole fleet's.
pub async fn console_fleet(state: &AppState) -> Option<Result<ConsoleFleet>> {
    match state.pg_pool() {
        Some(pool) => Some(read_console_fleet(pool, &state.fleet.environment()).await),
        None => None,
    }
}

/// `Some(epoch)` when this replica serves console sessions now: its
/// `MUNARIUM_CONSOLE_SESSIONS=enabled` allows them and, on PostgreSQL, the
/// environment's fleet mode is `enabled` (the memory store's setting is the
/// fleet's, at epoch 0). `None` means the v1.4 login. Under the default
/// `legacy` setting nothing is read, so a v1.4 configuration behaves exactly
/// as before. An error means the mode is unknown; callers fail closed.
pub async fn session_epoch(state: &AppState) -> Result<Option<i64>> {
    if !state.console.settings.sessions() {
        return Ok(None);
    }
    match console_fleet(state).await {
        None => Ok(Some(0)),
        Some(Ok(f)) if f.mode == FleetMode::Enabled => Ok(Some(f.epoch)),
        Some(Ok(_)) => Ok(None),
        Some(Err(e)) => Err(e),
    }
}

/// The Health page's "console login" line: this replica's setting, the
/// fleet mode and what this replica serves now.
pub async fn describe_console_login(state: &AppState) -> String {
    let setting = state.console.settings.describe();
    match console_fleet(state).await {
        None => format!("{setting}; memory store: this setting is the fleet mode"),
        Some(Err(_)) => format!("{setting}; fleet mode unavailable (read failed)"),
        Some(Ok(f)) => {
            let serving = if state.console.settings.sessions() && f.mode == FleetMode::Enabled {
                "serves sessions"
            } else {
                "serves the v1.4 login"
            };
            format!(
                "{setting}; fleet mode {} at epoch {} (environment {}); this replica {serving}",
                f.mode.as_str(),
                f.epoch,
                state.fleet.environment()
            )
        }
    }
}

// ---------------------------------------------------------------------------
// cutover preflight
// ---------------------------------------------------------------------------

/// Why `console-fleet enable` was refused. The codes are stable and carry no
/// replica content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The heartbeats could not be read.
    MembershipUnavailable,
    /// A live replica does not report the `console-sessions` capability.
    ReplicaLacksConsoleSessions,
    /// A live replica is configured with a different `MUNARIUM_REPLICA_COUNT`.
    ReplicaCountMismatch,
    /// Fewer replicas are live than expected: a v1.4 process, a replica of
    /// unknown version or one that cannot write its heartbeat may be serving.
    ReplicaShortfall,
    /// More replicas are live than expected.
    ReplicaExcess,
}

impl Refusal {
    pub fn code(self) -> &'static str {
        match self {
            Refusal::MembershipUnavailable => "membership_unavailable",
            Refusal::ReplicaLacksConsoleSessions => "replica_lacks_console_sessions",
            Refusal::ReplicaCountMismatch => "replica_count_mismatch",
            Refusal::ReplicaShortfall => "replica_shortfall",
            Refusal::ReplicaExcess => "replica_excess",
        }
    }
}

/// The cutover rule: every live replica can serve sessions, every one is
/// configured with `expected` replicas, and exactly `expected` are live.
pub fn preflight(live: &[LiveReplica], expected: u32) -> std::result::Result<(), Refusal> {
    if live.iter().any(|r| !r.supports_console_sessions()) {
        return Err(Refusal::ReplicaLacksConsoleSessions);
    }
    if live.iter().any(|r| r.replica_count != expected) {
        return Err(Refusal::ReplicaCountMismatch);
    }
    let n = live.len() as u64;
    match n.cmp(&u64::from(expected)) {
        std::cmp::Ordering::Less => Err(Refusal::ReplicaShortfall),
        std::cmp::Ordering::Greater => Err(Refusal::ReplicaExcess),
        std::cmp::Ordering::Equal => Ok(()),
    }
}

/// The outcome of a `console-fleet` change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// The mode changed; the new state.
    Changed(ConsoleFleet),
    /// The mode was already the requested one.
    Unchanged(ConsoleFleet),
    /// `enable` was refused; nothing was written.
    Refused {
        refusal: Refusal,
        live: Option<usize>,
    },
}

/// Serializes changes to one environment's mode for the transaction.
async fn lock_environment(tx: &mut sqlx::PgConnection, environment: &str) -> Result<()> {
    bounded(
        "console fleet lock",
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext('munarium-console-fleet:' || $1))")
            .bind(environment)
            .execute(tx),
    )
    .await?;
    Ok(())
}

/// `console-fleet enable`: runs the preflight and, if it passes, records
/// `enabled` at the current epoch. One transaction under an environment
/// lock, so two concurrent changes cannot interleave.
pub async fn enable(
    pool: &sqlx::PgPool,
    environment: &str,
    expected: u32,
    actor: &str,
) -> Result<Change> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| KernelError::Storage(e.to_string()))?;
    lock_environment(&mut tx, environment).await?;
    let current = read_console_fleet(&mut *tx, environment).await?;
    if current.mode == FleetMode::Enabled {
        return Ok(Change::Unchanged(current));
    }
    let live = match read_live(&mut *tx, environment).await {
        Ok(live) => live,
        Err(e) => {
            tracing::warn!(error = %e, "console fleet preflight could not read heartbeats");
            return Ok(Change::Refused {
                refusal: Refusal::MembershipUnavailable,
                live: None,
            });
        }
    };
    if let Err(refusal) = preflight(&live, expected) {
        return Ok(Change::Refused {
            refusal,
            live: Some(live.len()),
        });
    }
    bounded(
        "console fleet write",
        sqlx::query(
            "INSERT INTO console_fleet_mode (environment_id, mode, epoch, changed_at, changed_by)
             VALUES ($1, 'enabled', $2, now(), $3)
             ON CONFLICT (environment_id) DO UPDATE SET
                 mode = 'enabled', changed_at = now(), changed_by = EXCLUDED.changed_by",
        )
        .bind(environment)
        .bind(current.epoch)
        .bind(actor)
        .execute(&mut *tx),
    )
    .await?;
    let after = read_console_fleet(&mut *tx, environment).await?;
    tx.commit()
        .await
        .map_err(|e| KernelError::Storage(e.to_string()))?;
    Ok(Change::Changed(after))
}

/// `console-fleet disable`: back to the v1.4 login, incrementing the epoch so
/// that no handle issued so far is ever honoured again. Never refused.
pub async fn disable(pool: &sqlx::PgPool, environment: &str, actor: &str) -> Result<Change> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| KernelError::Storage(e.to_string()))?;
    lock_environment(&mut tx, environment).await?;
    let current = read_console_fleet(&mut *tx, environment).await?;
    if current.mode == FleetMode::Legacy {
        return Ok(Change::Unchanged(current));
    }
    bounded(
        "console fleet write",
        sqlx::query(
            "UPDATE console_fleet_mode
                SET mode = 'legacy', epoch = epoch + 1, changed_at = now(), changed_by = $2
              WHERE environment_id = $1",
        )
        .bind(environment)
        .bind(actor)
        .execute(&mut *tx),
    )
    .await?;
    let after = read_console_fleet(&mut *tx, environment).await?;
    tx.commit()
        .await
        .map_err(|e| KernelError::Storage(e.to_string()))?;
    Ok(Change::Changed(after))
}

// ---------------------------------------------------------------------------
// `munarium-server console-fleet`
// ---------------------------------------------------------------------------

const USAGE: &str = "usage: munarium-server console-fleet status [--environment E]\n       \
munarium-server console-fleet enable --expected-replicas N --actor A [--environment E]\n       \
munarium-server console-fleet disable --actor A [--environment E]";

/// Exit status when `enable` is refused by the preflight.
pub const EXIT_REFUSED: i32 = 3;

fn flags(
    args: &[String],
) -> std::result::Result<std::collections::BTreeMap<String, String>, String> {
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

fn fleet_json(environment: &str, f: &ConsoleFleet) -> serde_json::Value {
    serde_json::json!({
        "environment": environment,
        "mode": f.mode.as_str(),
        "epoch": f.epoch,
        "changed_at": f.changed_at.map(|t| t.to_rfc3339()),
        "changed_by": f.changed_by,
    })
}

fn change_json(environment: &str, change: &Change) -> (i32, serde_json::Value) {
    match change {
        Change::Changed(f) => {
            let mut v = fleet_json(environment, f);
            v["result"] = "changed".into();
            (0, v)
        }
        Change::Unchanged(f) => {
            let mut v = fleet_json(environment, f);
            v["result"] = "unchanged".into();
            (0, v)
        }
        Change::Refused { refusal, live } => (
            EXIT_REFUSED,
            serde_json::json!({
                "environment": environment,
                "result": "refused",
                "reason": refusal.code(),
                "live_replicas": live,
            }),
        ),
    }
}

/// Runs one `console-fleet` command against `pool`. `environment` is the
/// default scope (`MUNARIUM_DEPLOYMENT_ENVIRONMENT_ID`). Returns the exit
/// status and the JSON to print.
pub async fn execute(
    pool: &sqlx::PgPool,
    default_environment: &str,
    args: &[String],
) -> std::result::Result<(i32, String), String> {
    let Some((command, rest)) = args.split_first() else {
        return Err(USAGE.into());
    };
    let flags = flags(rest)?;
    let allowed: &[&str] = match command.as_str() {
        "status" => &["environment"],
        "enable" => &["environment", "expected-replicas", "actor"],
        "disable" => &["environment", "actor"],
        _ => return Err(USAGE.into()),
    };
    if let Some(unknown) = flags.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(format!("--{unknown} is not an option of {command}"));
    }
    let environment = flags
        .get("environment")
        .map(String::as_str)
        .unwrap_or(default_environment);
    let actor = || {
        flags
            .get("actor")
            .map(|a| a.trim())
            .filter(|a| !a.is_empty())
            .ok_or_else(|| "--actor is required".to_string())
    };
    let render = |v: serde_json::Value| serde_json::to_string_pretty(&v).unwrap_or_default();
    match command.as_str() {
        "status" => {
            let fleet = read_console_fleet(pool, environment)
                .await
                .map_err(|e| e.to_string())?;
            let mut v = fleet_json(environment, &fleet);
            v["live_replicas"] = match read_live(pool, environment).await {
                Ok(live) => serde_json::to_value(&live).unwrap_or_default(),
                Err(_) => "unavailable".into(),
            };
            Ok((0, render(v)))
        }
        "enable" => {
            let actor = actor()?;
            let expected: u32 = flags
                .get("expected-replicas")
                .ok_or("--expected-replicas is required")?
                .parse()
                .map_err(|_| "--expected-replicas must be a positive integer".to_string())?;
            if expected == 0 {
                return Err("--expected-replicas must be a positive integer".into());
            }
            let change = enable(pool, environment, expected, actor)
                .await
                .map_err(|e| e.to_string())?;
            let (code, v) = change_json(environment, &change);
            Ok((code, render(v)))
        }
        "disable" => {
            let actor = actor()?;
            let change = disable(pool, environment, actor)
                .await
                .map_err(|e| e.to_string())?;
            let (code, v) = change_json(environment, &change);
            Ok((code, render(v)))
        }
        _ => Err(USAGE.into()),
    }
}

/// `munarium-server console-fleet ...`: connects to the configured database
/// and exits.
pub async fn run(config: &crate::config::Config, args: &[String]) -> i32 {
    if config.store != crate::config::StoreKind::Postgres {
        eprintln!(
            "console-fleet requires MUNARIUM_STORE=postgres (the memory store is one replica; \
             its MUNARIUM_CONSOLE_SESSIONS setting is the fleet mode)"
        );
        return 2;
    }
    let Some(url) = config.database_url.as_deref() else {
        eprintln!("console-fleet requires a database URL");
        return 2;
    };
    let store =
        match munarium_store_pg::PgStore::connect(url, munarium_store_pg::DEFAULT_TENANT).await {
            Ok(s) => s,
            Err(_) => {
                eprintln!("console-fleet: database unavailable");
                return 1;
            }
        };
    let environment = FleetIdentity::from_env().environment();
    match execute(store.pool(), &environment, args).await {
        Ok((code, out)) => {
            println!("{out}");
            code
        }
        Err(e) => {
            eprintln!("console-fleet: {e}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn replica(count: u32, capable: bool) -> LiveReplica {
        LiveReplica {
            replica_id: format!("r-{}", uuid::Uuid::new_v4().simple()),
            instance_id: "pod".into(),
            version: "1.5.0".into(),
            plane: "rest".into(),
            replica_count: count,
            capabilities: if capable {
                vec![CAP_CONSOLE_SESSIONS.into()]
            } else {
                Vec::new()
            },
            seconds_since_heartbeat: 1.0,
        }
    }

    #[test]
    fn the_preflight_requires_capable_agreeing_replicas_in_the_expected_number() {
        let two = [replica(2, true), replica(2, true)];
        assert_eq!(preflight(&two, 2), Ok(()));
        // A v1.4 or silent replica: one heartbeat short.
        assert_eq!(preflight(&two[..1], 2), Err(Refusal::ReplicaShortfall));
        assert_eq!(preflight(&[], 1), Err(Refusal::ReplicaShortfall));
        // More live than expected.
        let three = [replica(2, true), replica(2, true), replica(2, true)];
        assert_eq!(preflight(&three, 2), Err(Refusal::ReplicaExcess));
        // A replica whose environment does not allow sessions.
        let mixed = [replica(2, true), replica(2, false)];
        assert_eq!(
            preflight(&mixed, 2),
            Err(Refusal::ReplicaLacksConsoleSessions)
        );
        // A replica configured with another count.
        let disagree = [replica(2, true), replica(3, true)];
        assert_eq!(preflight(&disagree, 2), Err(Refusal::ReplicaCountMismatch));
        // The expected count the operator states must also match the
        // replicas' configured count.
        assert_eq!(preflight(&two, 3), Err(Refusal::ReplicaCountMismatch));
    }

    #[test]
    fn refusal_codes_are_stable() {
        for (r, code) in [
            (Refusal::MembershipUnavailable, "membership_unavailable"),
            (
                Refusal::ReplicaLacksConsoleSessions,
                "replica_lacks_console_sessions",
            ),
            (Refusal::ReplicaCountMismatch, "replica_count_mismatch"),
            (Refusal::ReplicaShortfall, "replica_shortfall"),
            (Refusal::ReplicaExcess, "replica_excess"),
        ] {
            assert_eq!(r.code(), code);
        }
    }

    #[test]
    fn no_row_is_the_v1_4_login_at_epoch_zero() {
        assert_eq!(fleet_from(None), ConsoleFleet::default_legacy());
        assert_eq!(fleet_from(None).mode, FleetMode::Legacy);
        assert_eq!(fleet_from(None).epoch, 0);
    }
}

#[cfg(test)]
mod pg_tests {
    //! Heartbeats, the scale report's membership and the cutover against a
    //! real PostgreSQL: several `AppState`s (replicas) over one database, each
    //! test in its own deployment environment. Skipped without
    //! `MUNARIUM_TEST_DATABASE_URL`.
    use super::*;
    use crate::config::AuthMode;
    use crate::console_sessions::ConsoleSessionSettings;
    use tower::ServiceExt;

    fn url() -> Option<String> {
        let url = std::env::var("MUNARIUM_TEST_DATABASE_URL").ok();
        if url.is_none() {
            eprintln!("unavailable: isolated PostgreSQL required");
        }
        url
    }

    fn environment() -> String {
        format!("fleet-test-{}", uuid::Uuid::new_v4().simple())
    }

    /// A replica in `env` configured with `count` and, when `sessions`,
    /// `MUNARIUM_CONSOLE_SESSIONS=enabled`.
    async fn replica(url: &str, env: &str, count: u32, sessions: bool) -> Arc<AppState> {
        let base = crate::providers_api::usage_tests::test_state_with_auth(
            Some(url.to_string()),
            AuthMode::Static(vec![("admin".into(), "fleet".into(), "mgmt".into())]),
        )
        .await;
        let mut config = base.config.clone();
        config.replica_count = count;
        config.source_store = crate::config::SourceStoreConfig::Pg;
        let settings = if sessions {
            ConsoleSessionSettings::enabled()
        } else {
            ConsoleSessionSettings::default()
        };
        let state = AppState::new_with_console(config, settings)
            .await
            .expect("state");
        state.fleet.set_environment(env);
        state
    }

    async fn expire(pool: &sqlx::PgPool, env: &str, replica: &AppState) {
        sqlx::query(
            "UPDATE server_replica_heartbeats SET expires_at = now() - interval '1 second'
              WHERE environment_id = $1 AND replica_id = $2",
        )
        .bind(env)
        .bind(&replica.fleet.replica_id)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn rows(pool: &sqlx::PgPool, env: &str) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM server_replica_heartbeats WHERE environment_id = $1",
        )
        .bind(env)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    /// A heartbeat is live until its expiry by the database clock; an
    /// expired row is ignored at once and deleted by the next heartbeat.
    /// Rows record the replica's version, plane, count and capabilities,
    /// and another environment's rows are never counted.
    #[tokio::test]
    async fn heartbeats_expire_are_pruned_and_stay_in_their_environment() {
        let Some(url) = url() else { return };
        let env = environment();
        let a = replica(&url, &env, 2, true).await;
        let b = replica(&url, &env, 2, false).await;
        let elsewhere = replica(&url, &environment(), 2, true).await;
        let pool = a.pg_pool().unwrap().clone();
        for r in [&a, &b, &elsewhere] {
            beat(r).await.unwrap();
        }
        // A second beat renews, never duplicates.
        beat(&a).await.unwrap();
        let live = live_replicas(&a).await.unwrap();
        assert_eq!(live.len(), 2, "{live:?}");
        let mine = live
            .iter()
            .find(|r| r.replica_id == a.fleet.replica_id)
            .unwrap();
        assert_eq!(
            (
                mine.version.as_str(),
                mine.plane.as_str(),
                mine.replica_count
            ),
            (env!("CARGO_PKG_VERSION"), "rest", 2)
        );
        assert!(mine.supports_console_sessions());
        let theirs = live
            .iter()
            .find(|r| r.replica_id == b.fleet.replica_id)
            .unwrap();
        assert!(
            theirs.capabilities.is_empty(),
            "legacy setting: no capability"
        );
        assert!(mine.seconds_since_heartbeat < HEARTBEAT_TTL_SECS as f64);

        // Expired: ignored before any prune...
        expire(&pool, &env, &b).await;
        assert_eq!(rows(&pool, &env).await, 2);
        let live = live_replicas(&a).await.unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].replica_id, a.fleet.replica_id);
        // ...and deleted by the next heartbeat in the environment.
        beat(&a).await.unwrap();
        assert_eq!(rows(&pool, &env).await, 1);
        // The expiry is HEARTBEAT_TTL_SECS ahead on the database clock.
        let ahead: f64 = sqlx::query_scalar(
            "SELECT EXTRACT(EPOCH FROM expires_at - now())::float8
               FROM server_replica_heartbeats WHERE environment_id = $1",
        )
        .bind(&env)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            ahead > (HEARTBEAT_TTL_SECS - 10) as f64 && ahead <= HEARTBEAT_TTL_SECS as f64,
            "{ahead}"
        );
        // A replica that beats again is live again.
        beat(&b).await.unwrap();
        assert_eq!(live_replicas(&a).await.unwrap().len(), 2);
    }

    /// SCALE-3: the report counts live replicas against the configured count
    /// and warns only when more are live; the route carries the same.
    #[tokio::test]
    async fn the_scale_report_compares_live_with_configured_replicas() {
        let Some(url) = url() else { return };
        let env = environment();
        let a = replica(&url, &env, 2, true).await;
        let b = replica(&url, &env, 2, true).await;
        let c = replica(&url, &env, 2, false).await;
        let pool = a.pg_pool().unwrap().clone();

        // Nothing has beaten yet: observed, zero live (not unavailable).
        let r = crate::scale_diagnostics::op_scale_report(&a, "fleet").await;
        assert_eq!(r.observed_replicas, Some(0));
        assert_eq!(r.replica_membership.status, "observed");

        beat(&a).await.unwrap();
        let r = crate::scale_diagnostics::op_scale_report(&a, "fleet").await;
        assert_eq!((r.observed_replicas, r.replica_count), (Some(1), 2));
        assert_eq!(r.replica_membership.exceeds_configured, Some(false));
        assert_eq!(r.replica_membership.environment_id, env);

        beat(&b).await.unwrap();
        beat(&c).await.unwrap();
        let resp = crate::rest::router(a.clone())
            .oneshot(
                axum::http::Request::get("/v1/reports/scale")
                    .header("authorization", "Bearer admin")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), 1_000_000)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["observed_replicas"], 3, "{body}");
        assert_eq!(body["replica_count"], 2);
        assert_eq!(body["replica_membership"]["exceeds_configured"], true);
        assert_eq!(
            body["replica_membership"]["replicas"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        let html = crate::scale_diagnostics::health_section(
            &crate::scale_diagnostics::op_scale_report(&a, "fleet").await,
        );
        assert!(html.contains("warning: 3 replicas are live"), "{html}");

        // One stops: back within the configured count, no warning.
        expire(&pool, &env, &c).await;
        let r = crate::scale_diagnostics::op_scale_report(&b, "fleet").await;
        assert_eq!(r.observed_replicas, Some(2));
        assert_eq!(r.replica_membership.exceeds_configured, Some(false));
    }

    /// A heartbeat read that fails is unavailable with a null count, never 0.
    #[tokio::test]
    async fn an_unreachable_database_makes_membership_unavailable() {
        // Port 1 on loopback refuses at once; no database is needed.
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://nobody:nothing@127.0.0.1:1/none")
            .unwrap();
        let read = read_live(&pool, "env").await;
        assert!(read.is_err());
        let m = crate::scale_diagnostics::membership_report("env".into(), 2, read);
        assert_eq!(m.status, "unavailable");
        assert_eq!(m.exceeds_configured, None);
        assert!(m.replicas.is_empty());
        // The cutover cannot proceed either: nothing is written.
        assert!(enable(&pool, "env", 1, "op").await.is_err());
        assert!(read_console_fleet(&pool, "env").await.is_err());
    }

    fn refused(change: Change) -> Refusal {
        match change {
            Change::Refused { refusal, .. } => refusal,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// LOGIN1 preflight: enabling is refused while a live replica lacks the
    /// session capability, while fewer replicas are live than expected (a
    /// v1.4, stale or unknown replica), while more are live, or while a
    /// replica's configured count disagrees; it succeeds once every live
    /// replica is capable and the count matches. A refusal writes nothing.
    #[tokio::test]
    async fn the_cutover_is_refused_until_every_expected_replica_is_capable() {
        let Some(url) = url() else { return };
        let env = environment();
        let a = replica(&url, &env, 2, true).await;
        let legacy = replica(&url, &env, 2, false).await;
        let pool = a.pg_pool().unwrap().clone();

        // Default: no row is the v1.4 login at epoch 0.
        assert_eq!(
            read_console_fleet(&pool, &env).await.unwrap(),
            ConsoleFleet::default_legacy()
        );
        // No heartbeat at all: every expected replica is missing.
        assert_eq!(
            refused(enable(&pool, &env, 2, "op").await.unwrap()),
            Refusal::ReplicaShortfall
        );
        beat(&a).await.unwrap();
        // One live, two expected: the other may be a v1.4 replica.
        assert_eq!(
            refused(enable(&pool, &env, 2, "op").await.unwrap()),
            Refusal::ReplicaShortfall
        );
        // A live replica whose setting does not allow sessions.
        beat(&legacy).await.unwrap();
        assert_eq!(
            refused(enable(&pool, &env, 2, "op").await.unwrap()),
            Refusal::ReplicaLacksConsoleSessions
        );
        expire(&pool, &env, &legacy).await;
        let b = replica(&url, &env, 2, true).await;
        beat(&b).await.unwrap();
        // The operator's expected count disagrees with the configured one.
        assert_eq!(
            refused(enable(&pool, &env, 3, "op").await.unwrap()),
            Refusal::ReplicaCountMismatch
        );
        // A third capable replica: more live than expected.
        let c = replica(&url, &env, 2, true).await;
        beat(&c).await.unwrap();
        assert_eq!(
            refused(enable(&pool, &env, 2, "op").await.unwrap()),
            Refusal::ReplicaExcess
        );
        // A stale heartbeat counts as missing.
        expire(&pool, &env, &c).await;
        expire(&pool, &env, &b).await;
        assert_eq!(
            refused(enable(&pool, &env, 2, "op").await.unwrap()),
            Refusal::ReplicaShortfall
        );
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM console_fleet_mode WHERE environment_id = $1")
                .bind(&env)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 0, "refusals write nothing");

        // Every live replica capable and the count matches: enabled.
        beat(&b).await.unwrap();
        match enable(&pool, &env, 2, "op").await.unwrap() {
            Change::Changed(f) => {
                assert_eq!((f.mode, f.epoch), (FleetMode::Enabled, 0));
                assert_eq!(f.changed_by.as_deref(), Some("op"));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            enable(&pool, &env, 2, "op").await.unwrap(),
            Change::Unchanged(_)
        ));
        assert_eq!(session_epoch(&a).await.unwrap(), Some(0));
        // The per-replica upper bound: a replica whose setting is legacy
        // never serves sessions, whatever the fleet mode.
        assert_eq!(session_epoch(&legacy).await.unwrap(), None);

        // Rollback bumps the epoch; a second disable changes nothing.
        match disable(&pool, &env, "op").await.unwrap() {
            Change::Changed(f) => assert_eq!((f.mode, f.epoch), (FleetMode::Legacy, 1)),
            other => panic!("{other:?}"),
        }
        assert_eq!(session_epoch(&a).await.unwrap(), None);
        match disable(&pool, &env, "op").await.unwrap() {
            Change::Unchanged(f) => assert_eq!(f.epoch, 1),
            other => panic!("{other:?}"),
        }
        // Re-enabled at the new epoch.
        match enable(&pool, &env, 2, "op").await.unwrap() {
            Change::Changed(f) => assert_eq!((f.mode, f.epoch), (FleetMode::Enabled, 1)),
            other => panic!("{other:?}"),
        }
        assert_eq!(session_epoch(&b).await.unwrap(), Some(1));
        // Another environment in the same database is untouched.
        let other = environment();
        assert_eq!(
            read_console_fleet(&pool, &other).await.unwrap(),
            ConsoleFleet::default_legacy()
        );
    }

    /// The `console-fleet` command: its JSON, the refusal exit status and
    /// code, and argument validation.
    #[tokio::test]
    async fn the_console_fleet_command_reports_changes_and_refusals() {
        let Some(url) = url() else { return };
        let env = environment();
        let a = replica(&url, &env, 1, true).await;
        let pool = a.pg_pool().unwrap().clone();
        let args = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();

        let (code, out) = execute(&pool, &env, &args("status")).await.unwrap();
        assert_eq!(code, 0);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            (v["mode"].as_str(), v["epoch"].as_i64()),
            (Some("legacy"), Some(0))
        );
        assert_eq!(v["live_replicas"], serde_json::json!([]));

        let (code, out) = execute(
            &pool,
            &env,
            &args("enable --expected-replicas 1 --actor op"),
        )
        .await
        .unwrap();
        assert_eq!(code, EXIT_REFUSED);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["result"], "refused");
        assert_eq!(v["reason"], "replica_shortfall");
        assert_eq!(v["live_replicas"], 0);

        beat(&a).await.unwrap();
        let (code, out) = execute(
            &pool,
            &env,
            &args("enable --expected-replicas 1 --actor op"),
        )
        .await
        .unwrap();
        assert_eq!(code, 0, "{out}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            (v["result"].as_str(), v["mode"].as_str()),
            (Some("changed"), Some("enabled"))
        );
        let (_, out) = execute(&pool, &env, &args("status")).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["live_replicas"][0]["replica_id"],
            a.fleet.replica_id.as_str()
        );

        let (code, out) = execute(
            &pool,
            "unused",
            &args(&format!("disable --actor op --environment {env}")),
        )
        .await
        .unwrap();
        assert_eq!(code, 0);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            (v["mode"].as_str(), v["epoch"].as_i64()),
            (Some("legacy"), Some(1))
        );

        for bad in [
            "",
            "flip",
            "enable --actor op",
            "enable --expected-replicas 0 --actor op",
            "enable --expected-replicas two --actor op",
            "enable --expected-replicas 1",
            "disable",
            "disable --actor op --force yes",
            "status extra",
        ] {
            assert!(execute(&pool, &env, &args(bad)).await.is_err(), "{bad:?}");
        }
    }
}
