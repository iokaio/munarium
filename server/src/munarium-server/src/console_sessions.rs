// SPDX-License-Identifier: Apache-2.0
//! Revocable console sessions (ClearView U10 / LOGIN1).
//!
//! The design is `docs/design/console-sessions.md`. In short:
//!
//! - **Opt-in.** `MUNARIUM_CONSOLE_SESSIONS` defaults to `legacy`: the v1.4
//!   login, whose `__munarium_admin` cookie holds the static mgmt token. An
//!   operator switches a fleet to `enabled` only after every replica serving
//!   console traffic runs a build with this module (LOGIN1).
//! - **Opaque handle.** Under `enabled`, a successful console login creates a
//!   random handle. The browser cookie carries only the handle; the store
//!   keeps a SHA-256 digest of it, never the handle itself.
//! - **Shared store.** On the PostgreSQL store the session is a row in
//!   `console_sessions` (migration 0050), so every replica sharing the
//!   database honours sign-out, revocation and expiry from its next request.
//!   The memory store keeps sessions in process; it is single-replica by
//!   validation, so that is the whole fleet.
//! - **Bound to tenant and authority.** The row records the tenant, the role
//!   and a salted digest of the static credential that signed in. Every
//!   request re-resolves that credential against the replica's current
//!   configuration: a rotated or removed token, a changed tenant or a lost
//!   mgmt role ends the session.
//! - **Expiry.** Idle and absolute expiry are enforced on every request by
//!   the database clock (the process clock on the memory store), using the
//!   stricter of the values recorded at sign-in and the replica's settings.
//! - **CSRF and review plans.** Each session has its own random binding
//!   secret, stored server-side and never sent, from which the CSRF token and
//!   the U4 review plans are derived. They therefore validate on every
//!   replica and die with the session.
//!
//! Legacy mode never reads the session cookie, so a rolled-back replica
//! honours no handle; rows remain until their expiry and pruning.

use crate::state::{AppState, TenantCtx};
use munarium_core::KernelError;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

type Result<T> = std::result::Result<T, KernelError>;

/// The v1.4 console cookie: the static mgmt token itself.
pub const LEGACY_COOKIE: &str = "__munarium_admin";
/// The session cookie on plain HTTP (`Path=/admin`).
pub const SESSION_COOKIE: &str = "__munarium_console";
/// The session cookie with `MUNARIUM_CONSOLE_COOKIE_SECURE=true`: the
/// `__Host-` prefix requires `Secure`, `Path=/` and no `Domain`.
pub const SECURE_SESSION_COOKIE: &str = "__Host-munarium_console";

pub const DEFAULT_IDLE_SECS: u64 = 1_800;
pub const DEFAULT_MAX_SECS: u64 = 28_800;
const IDLE_RANGE: (u64, u64) = (60, 86_400);
const MAX_RANGE: (u64, u64) = (300, 604_800);
/// How long a row outlives its absolute expiry before pruning, so a
/// revocation or sign-out stays visible on the sessions page for a week.
pub const PRUNE_AFTER_DAYS: i64 = 7;
/// The sessions page shows at most this many rows, newest first.
pub const LIST_LIMIT: i64 = 100;

const HANDLE_PREFIX: &str = "mcs1_";

/// Which console login a replica serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleLogin {
    /// v1.4: the cookie holds the static mgmt token. The default.
    Legacy,
    /// The cookie holds an opaque, revocable session handle.
    Sessions,
}

/// The validated console-login settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsoleSessionSettings {
    pub login: ConsoleLogin,
    /// `MUNARIUM_CONSOLE_SESSION_IDLE_SECS`.
    pub idle_secs: u64,
    /// `MUNARIUM_CONSOLE_SESSION_MAX_SECS`: absolute lifetime from sign-in.
    pub max_secs: u64,
    /// `MUNARIUM_CONSOLE_COOKIE_SECURE`: `Secure` plus the `__Host-` name.
    pub secure_cookie: bool,
}

impl Default for ConsoleSessionSettings {
    fn default() -> Self {
        Self {
            login: ConsoleLogin::Legacy,
            idle_secs: DEFAULT_IDLE_SECS,
            max_secs: DEFAULT_MAX_SECS,
            secure_cookie: false,
        }
    }
}

impl ConsoleSessionSettings {
    /// The enabled settings with the defaults (tests).
    #[cfg(test)]
    pub fn enabled() -> Self {
        Self {
            login: ConsoleLogin::Sessions,
            ..Self::default()
        }
    }

    /// Reads the settings through `var` (the process environment in
    /// production). Under `legacy` (the default) the other settings are not
    /// read at all, so a v1.4 deployment's environment means what it meant.
    pub fn from_lookup(var: impl Fn(&str) -> Option<String>) -> std::result::Result<Self, String> {
        let get = |k: &str| {
            var(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let login = match get("MUNARIUM_CONSOLE_SESSIONS").as_deref() {
            None | Some("legacy") => ConsoleLogin::Legacy,
            Some("enabled") => ConsoleLogin::Sessions,
            Some(other) => {
                return Err(format!(
                    "MUNARIUM_CONSOLE_SESSIONS must be legacy or enabled, not {other:?}"
                ))
            }
        };
        if login == ConsoleLogin::Legacy {
            return Ok(Self::default());
        }
        let secs = |key: &str, default: u64, (lo, hi): (u64, u64)| match get(key) {
            None => Ok(default),
            Some(v) => {
                let n: u64 = v.parse().map_err(|e| format!("{key}: {e}"))?;
                if n < lo || n > hi {
                    Err(format!(
                        "{key} must be between {lo} and {hi} seconds, not {n}"
                    ))
                } else {
                    Ok(n)
                }
            }
        };
        let idle_secs = secs(
            "MUNARIUM_CONSOLE_SESSION_IDLE_SECS",
            DEFAULT_IDLE_SECS,
            IDLE_RANGE,
        )?;
        let max_secs = secs(
            "MUNARIUM_CONSOLE_SESSION_MAX_SECS",
            DEFAULT_MAX_SECS,
            MAX_RANGE,
        )?;
        if idle_secs > max_secs {
            return Err(format!(
                "MUNARIUM_CONSOLE_SESSION_IDLE_SECS ({idle_secs}) must not exceed \
                 MUNARIUM_CONSOLE_SESSION_MAX_SECS ({max_secs})"
            ));
        }
        let secure_cookie = match get("MUNARIUM_CONSOLE_COOKIE_SECURE")
            .map(|v| v.to_ascii_lowercase())
            .as_deref()
        {
            None | Some("false") | Some("0") | Some("no") => false,
            Some("true") | Some("1") | Some("yes") => true,
            Some(other) => {
                return Err(format!(
                    "MUNARIUM_CONSOLE_COOKIE_SECURE must be true or false, not {other:?}"
                ))
            }
        };
        Ok(Self {
            login,
            idle_secs,
            max_secs,
            secure_cookie,
        })
    }

    pub fn from_env() -> std::result::Result<Self, String> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn sessions(&self) -> bool {
        self.login == ConsoleLogin::Sessions
    }

    pub fn cookie_name(&self) -> &'static str {
        if self.secure_cookie {
            SECURE_SESSION_COOKIE
        } else {
            SESSION_COOKIE
        }
    }

    fn cookie_attrs(&self) -> &'static str {
        if self.secure_cookie {
            "HttpOnly; SameSite=Strict; Secure; Path=/"
        } else {
            "HttpOnly; SameSite=Strict; Path=/admin"
        }
    }

    /// `Set-Cookie` for a new handle: a browser-session cookie (no
    /// `Max-Age`); the server enforces expiry.
    pub fn set_cookie(&self, handle: &str) -> String {
        format!("{}={handle}; {}", self.cookie_name(), self.cookie_attrs())
    }

    /// `Set-Cookie` that removes the session cookie.
    pub fn clear_cookie(&self) -> String {
        format!(
            "{}=; Max-Age=0; {}",
            self.cookie_name(),
            self.cookie_attrs()
        )
    }

    /// A one-line description for the health page.
    pub fn describe(&self) -> String {
        match self.login {
            ConsoleLogin::Legacy => {
                "legacy (v1.4 cookie holds the mgmt token; MUNARIUM_CONSOLE_SESSIONS=legacy)".into()
            }
            ConsoleLogin::Sessions => format!(
                "sessions (opaque handle; idle {} s, absolute {} s, cookie {}{})",
                self.idle_secs,
                self.max_secs,
                self.cookie_name(),
                if self.secure_cookie { ", Secure" } else { "" }
            ),
        }
    }
}

/// `Set-Cookie` that removes the v1.4 cookie, sent when a session login
/// replaces it so the static token no longer sits in the browser.
pub fn clear_legacy_cookie() -> String {
    format!("{LEGACY_COOKIE}=; Max-Age=0; HttpOnly; SameSite=Strict; Path=/admin")
}

/// One stored session, as the sessions page shows it. Never the handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub session_id: String,
    pub tenant_id: String,
    pub role: String,
    pub created_by: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_seen_at: chrono::DateTime<chrono::Utc>,
    pub idle_secs: i64,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub revoked_at: Option<chrono::DateTime<chrono::Utc>>,
    pub revoked_reason: Option<String>,
    /// Whether the session would be honoured now, by this replica's settings.
    pub live: bool,
}

/// A resolved, live session.
#[derive(Debug, Clone)]
pub struct LiveSession {
    pub session_id: String,
    pub tenant: TenantCtx,
    /// The per-session secret the CSRF token and review plans derive from.
    pub binding_secret: String,
}

#[derive(Debug, Clone)]
struct MemSession {
    record: SessionRecord,
    credential_digest: String,
    binding_secret: String,
}

/// The session store of one replica: the settings, and — on the memory store
/// only — the sessions themselves.
#[derive(Debug, Default)]
pub struct ConsoleSessions {
    pub settings: ConsoleSessionSettings,
    mem: Mutex<HashMap<String, MemSession>>,
    /// Seconds added to the process clock (memory store only). Always zero
    /// outside tests; PostgreSQL uses the database clock.
    skew_secs: AtomicI64,
}

fn sha256_hex(s: &str) -> String {
    use sha2::Digest as _;
    hex::encode(sha2::Sha256::digest(s.as_bytes()))
}

fn random_hex64() -> String {
    // Two v4 UUIDs: 244 bits from the OS CSPRNG, like the boot secret.
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// A new, random handle.
fn new_handle() -> String {
    format!("{HANDLE_PREFIX}{}", random_hex64())
}

/// Whether `handle` has the shape this build issues. Anything else is
/// refused without a store lookup.
pub fn well_formed(handle: &str) -> bool {
    handle.strip_prefix(HANDLE_PREFIX).is_some_and(|h| {
        h.len() == 64
            && h.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// What the store keeps instead of the handle.
pub fn handle_digest(handle: &str) -> String {
    sha256_hex(&format!("munarium-console-handle:{handle}"))
}

/// The marker a disabled-auth login is bound to (there is no credential).
const DISABLED_CREDENTIAL: &str = "\u{0}auth-mode:disabled";

/// The credential a session is bound to, salted with its session id so the
/// stored value is useless for any other session.
fn credential_digest(session_id: &str, credential: &str) -> String {
    sha256_hex(&format!(
        "munarium-console-credential:{}:{session_id}:{credential}",
        session_id.len()
    ))
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// The replica's own idle and absolute bounds that liveness applies on top
/// of each row's recorded values, in seconds.
///
/// Under `enabled` they are this replica's settings, so the stricter of the
/// recorded and configured values wins. Under `legacy` the replica serves no
/// session and has no settings of its own (they are not even read), so it
/// judges each row by its recorded values alone: the widest values any
/// replica can record ([`IDLE_RANGE`] and [`MAX_RANGE`] maxima, which every
/// recorded value is within) impose no further bound. A rolled-back replica
/// therefore lists as live every session a re-enabled fleet could still
/// honour, and so prompts its revocation, rather than labelling it by the
/// defaults.
fn liveness_bounds(s: &ConsoleSessionSettings) -> (i64, i64) {
    if s.sessions() {
        (s.idle_secs as i64, s.max_secs as i64)
    } else {
        (IDLE_RANGE.1 as i64, MAX_RANGE.1 as i64)
    }
}

/// The memory store's liveness rule — the same predicate the PostgreSQL
/// query applies: not revoked, before the recorded absolute expiry, within
/// this replica's absolute lifetime, and used within the stricter idle bound
/// ([`liveness_bounds`]).
fn live_at(
    r: &SessionRecord,
    now: chrono::DateTime<chrono::Utc>,
    s: &ConsoleSessionSettings,
) -> bool {
    let secs = chrono::Duration::seconds;
    let (idle_bound, max_bound) = liveness_bounds(s);
    let idle = r.idle_secs.min(idle_bound);
    r.revoked_at.is_none()
        && now < r.expires_at
        && now < r.created_at + secs(max_bound)
        && now < r.last_seen_at + secs(idle)
}

const PG_RECORD_COLUMNS: &str = "session_id, tenant_id, role, created_by, created_at, \
     last_seen_at, idle_secs, expires_at, revoked_at, revoked_reason";

/// The PostgreSQL liveness predicate (`$2` idle seconds, `$3` absolute
/// seconds of this replica). Kept identical to [`live_at`].
const PG_LIVE: &str = "revoked_at IS NULL \
     AND now() < expires_at \
     AND now() < created_at + make_interval(secs => $3::double precision) \
     AND now() < last_seen_at + make_interval(secs => LEAST(idle_secs, $2)::double precision)";

type PgRecord = (
    String,
    String,
    String,
    String,
    chrono::DateTime<chrono::Utc>,
    chrono::DateTime<chrono::Utc>,
    i64,
    chrono::DateTime<chrono::Utc>,
    Option<chrono::DateTime<chrono::Utc>>,
    Option<String>,
    bool,
);

fn from_pg(r: PgRecord) -> SessionRecord {
    SessionRecord {
        session_id: r.0,
        tenant_id: r.1,
        role: r.2,
        created_by: r.3,
        created_at: r.4,
        last_seen_at: r.5,
        idle_secs: r.6,
        expires_at: r.7,
        revoked_at: r.8,
        revoked_reason: r.9,
        live: r.10,
    }
}

fn storage(e: sqlx::Error) -> KernelError {
    KernelError::Storage(e.to_string())
}

impl ConsoleSessions {
    pub fn new(settings: ConsoleSessionSettings) -> Self {
        Self {
            settings,
            ..Self::default()
        }
    }

    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now() + chrono::Duration::seconds(self.skew_secs.load(Ordering::Relaxed))
    }

    /// Moves the memory store's clock (tests only).
    #[cfg(test)]
    pub fn advance_clock(&self, secs: i64) {
        self.skew_secs.fetch_add(secs, Ordering::Relaxed);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, MemSession>> {
        self.mem.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn idle(&self) -> i64 {
        self.settings.idle_secs as i64
    }

    fn max(&self) -> i64 {
        self.settings.max_secs as i64
    }

    /// The `$2` and `$3` bounds of [`PG_LIVE`] ([`liveness_bounds`]).
    fn bounds(&self) -> (i64, i64) {
        liveness_bounds(&self.settings)
    }
}

/// Starts a session for a credential that has just authenticated as `ctx`
/// (mgmt, or the disabled-auth pseudo-principal). Returns the handle for the
/// cookie. Also prunes rows a week past their absolute expiry.
pub async fn create(state: &AppState, ctx: &TenantCtx, credential: &str) -> Result<String> {
    let sessions = &state.console;
    let handle = new_handle();
    let session_id = format!("cs-{}", uuid::Uuid::new_v4().simple());
    let bound = if ctx.disabled_mode {
        DISABLED_CREDENTIAL
    } else {
        credential
    };
    let credential_digest = credential_digest(&session_id, bound);
    let binding_secret = random_hex64();
    let created_by = state.config.instance_id.clone();
    match state.pg_pool() {
        Some(pool) => {
            sqlx::query(
                "DELETE FROM console_sessions
                  WHERE expires_at < now() - make_interval(days => $1::int)",
            )
            .bind(PRUNE_AFTER_DAYS as i32)
            .execute(pool)
            .await
            .map_err(storage)?;
            sqlx::query(
                "INSERT INTO console_sessions
                   (handle_digest, session_id, tenant_id, role, credential_digest,
                    binding_secret, created_by, idle_secs, expires_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8,
                         now() + make_interval(secs => $9::double precision))",
            )
            .bind(handle_digest(&handle))
            .bind(&session_id)
            .bind(&ctx.tenant_id)
            .bind(&ctx.role)
            .bind(&credential_digest)
            .bind(&binding_secret)
            .bind(&created_by)
            .bind(sessions.idle())
            .bind(sessions.max())
            .execute(pool)
            .await
            .map_err(storage)?;
        }
        None => {
            let now = sessions.now();
            let mut mem = sessions.lock();
            mem.retain(|_, s| {
                s.record.expires_at + chrono::Duration::days(PRUNE_AFTER_DAYS) >= now
            });
            mem.insert(
                handle_digest(&handle),
                MemSession {
                    record: SessionRecord {
                        session_id,
                        tenant_id: ctx.tenant_id.clone(),
                        role: ctx.role.clone(),
                        created_by,
                        created_at: now,
                        last_seen_at: now,
                        idle_secs: sessions.idle(),
                        expires_at: now + chrono::Duration::seconds(sessions.max()),
                        revoked_at: None,
                        revoked_reason: None,
                        live: true,
                    },
                    credential_digest,
                    binding_secret,
                },
            );
        }
    }
    Ok(handle)
}

/// The authority a session's credential has on this replica NOW: the static
/// token whose salted digest matches, resolved through the same
/// `authenticate` every request uses.
fn current_authority(state: &AppState, session_id: &str, digest: &str) -> Option<TenantCtx> {
    match &state.config.auth {
        crate::config::AuthMode::Disabled => {
            constant_time_eq(&credential_digest(session_id, DISABLED_CREDENTIAL), digest)
                .then(|| state.authenticate(None).ok())
                .flatten()
        }
        crate::config::AuthMode::Static(tokens) => tokens
            .iter()
            .find(|(t, _, _)| constant_time_eq(&credential_digest(session_id, t), digest))
            .and_then(|(t, _, _)| state.authenticate(Some(t)).ok()),
    }
}

/// Whether a request counts as the operator's use of the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Use {
    /// An operator's request: it restarts the idle clock.
    Active,
    /// A page reloading itself (the build-job page's meta refresh): liveness
    /// is checked exactly as for [`Use::Active`], but the idle clock is not
    /// restarted, so an unattended page cannot keep a session alive.
    Passive,
}

/// Resolves a cookie handle to a live session, recording an
/// [`Use::Active`] use (idle expiry counts from the last such request).
/// `None` for anything that is not a live session this replica's
/// configuration still authorizes: a malformed, unknown, tampered,
/// signed-out, revoked or expired handle, or a credential that no longer
/// resolves to the same tenant and role with mgmt authority.
pub async fn resolve(state: &AppState, handle: &str, using: Use) -> Result<Option<LiveSession>> {
    if !well_formed(handle) {
        return Ok(None);
    }
    let sessions = &state.console;
    let digest = handle_digest(handle);
    let touch = using == Use::Active;
    let found: Option<(String, String, String, String, String)> = match state.pg_pool() {
        Some(pool) => sqlx::query_as(&if touch {
            format!(
                "UPDATE console_sessions SET last_seen_at = now()
                  WHERE handle_digest = $1 AND {PG_LIVE}
              RETURNING session_id, tenant_id, role, credential_digest, binding_secret"
            )
        } else {
            format!(
                "SELECT session_id, tenant_id, role, credential_digest, binding_secret
                   FROM console_sessions WHERE handle_digest = $1 AND {PG_LIVE}"
            )
        })
        .bind(&digest)
        .bind(sessions.idle())
        .bind(sessions.max())
        .fetch_optional(pool)
        .await
        .map_err(storage)?,
        None => {
            let now = sessions.now();
            let mut mem = sessions.lock();
            match mem.get_mut(&digest) {
                Some(s) if live_at(&s.record, now, &sessions.settings) => {
                    if touch {
                        s.record.last_seen_at = now;
                    }
                    Some((
                        s.record.session_id.clone(),
                        s.record.tenant_id.clone(),
                        s.record.role.clone(),
                        s.credential_digest.clone(),
                        s.binding_secret.clone(),
                    ))
                }
                _ => None,
            }
        }
    };
    let Some((session_id, tenant_id, role, credential, binding_secret)) = found else {
        return Ok(None);
    };
    let Some(tenant) = current_authority(state, &session_id, &credential) else {
        return Ok(None);
    };
    let authorized = tenant.tenant_id == tenant_id
        && tenant.role == role
        && (tenant.role == "mgmt" || tenant.disabled_mode);
    Ok(authorized.then_some(LiveSession {
        session_id,
        tenant,
        binding_secret,
    }))
}

/// Sign-out: revokes the session behind `handle` on every replica. Returns
/// whether a live row changed.
pub async fn sign_out(state: &AppState, handle: &str) -> Result<bool> {
    if !well_formed(handle) {
        return Ok(false);
    }
    let digest = handle_digest(handle);
    match state.pg_pool() {
        Some(pool) => Ok(sqlx::query(
            "UPDATE console_sessions SET revoked_at = now(), revoked_reason = 'sign-out'
              WHERE handle_digest = $1 AND revoked_at IS NULL",
        )
        .bind(&digest)
        .execute(pool)
        .await
        .map_err(storage)?
        .rows_affected()
            == 1),
        None => {
            let now = state.console.now();
            let mut mem = state.console.lock();
            Ok(match mem.get_mut(&digest) {
                Some(s) if s.record.revoked_at.is_none() => {
                    s.record.revoked_at = Some(now);
                    s.record.revoked_reason = Some("sign-out".into());
                    true
                }
                _ => false,
            })
        }
    }
}

/// The tenant's sessions, newest first, at most [`LIST_LIMIT`].
pub async fn list(state: &AppState, tenant: &str) -> Result<Vec<SessionRecord>> {
    let sessions = &state.console;
    match state.pg_pool() {
        Some(pool) => {
            let rows: Vec<PgRecord> = sqlx::query_as(&format!(
                "SELECT {PG_RECORD_COLUMNS}, ({PG_LIVE}) AS live
                   FROM console_sessions WHERE tenant_id = $1
                  ORDER BY created_at DESC, session_id DESC LIMIT $4"
            ))
            .bind(tenant)
            .bind(sessions.bounds().0)
            .bind(sessions.bounds().1)
            .bind(LIST_LIMIT)
            .fetch_all(pool)
            .await
            .map_err(storage)?;
            Ok(rows.into_iter().map(from_pg).collect())
        }
        None => {
            let now = sessions.now();
            let mut out: Vec<SessionRecord> = sessions
                .lock()
                .values()
                .filter(|s| s.record.tenant_id == tenant)
                .map(|s| SessionRecord {
                    live: live_at(&s.record, now, &sessions.settings),
                    ..s.record.clone()
                })
                .collect();
            out.sort_by(|a, b| (b.created_at, &b.session_id).cmp(&(a.created_at, &a.session_id)));
            out.truncate(LIST_LIMIT as usize);
            Ok(out)
        }
    }
}

/// One of the tenant's sessions by its public id.
pub async fn get(state: &AppState, tenant: &str, session_id: &str) -> Result<SessionRecord> {
    let sessions = &state.console;
    let not_found = || KernelError::NotFound {
        kind: "console session",
        id: session_id.to_string(),
    };
    match state.pg_pool() {
        Some(pool) => {
            let row: Option<PgRecord> = sqlx::query_as(&format!(
                "SELECT {PG_RECORD_COLUMNS}, ({PG_LIVE}) AS live
                   FROM console_sessions WHERE tenant_id = $1 AND session_id = $4"
            ))
            .bind(tenant)
            .bind(sessions.bounds().0)
            .bind(sessions.bounds().1)
            .bind(session_id)
            .fetch_optional(pool)
            .await
            .map_err(storage)?;
            row.map(from_pg).ok_or_else(not_found)
        }
        None => {
            let now = sessions.now();
            sessions
                .lock()
                .values()
                .find(|s| s.record.tenant_id == tenant && s.record.session_id == session_id)
                .map(|s| SessionRecord {
                    live: live_at(&s.record, now, &sessions.settings),
                    ..s.record.clone()
                })
                .ok_or_else(not_found)
        }
    }
}

/// Revokes one of the tenant's sessions if it is not revoked yet. The write
/// is conditional, so a duplicate or racing revocation acts at most once.
pub async fn revoke_if_unrevoked(state: &AppState, tenant: &str, session_id: &str) -> Result<bool> {
    match state.pg_pool() {
        Some(pool) => Ok(sqlx::query(
            "UPDATE console_sessions SET revoked_at = now(), revoked_reason = 'revoked'
              WHERE tenant_id = $1 AND session_id = $2 AND revoked_at IS NULL",
        )
        .bind(tenant)
        .bind(session_id)
        .execute(pool)
        .await
        .map_err(storage)?
        .rows_affected()
            == 1),
        None => {
            let now = state.console.now();
            let mut mem = state.console.lock();
            Ok(
                match mem
                    .values_mut()
                    .find(|s| s.record.tenant_id == tenant && s.record.session_id == session_id)
                {
                    Some(s) if s.record.revoked_at.is_none() => {
                        s.record.revoked_at = Some(now);
                        s.record.revoked_reason = Some("revoked".into());
                        true
                    }
                    _ => false,
                },
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn the_default_is_the_v1_4_login_and_ignores_the_other_settings() {
        assert_eq!(
            ConsoleSessionSettings::from_lookup(lookup(&[])).unwrap(),
            ConsoleSessionSettings::default()
        );
        assert_eq!(
            ConsoleSessionSettings::default().login,
            ConsoleLogin::Legacy
        );
        // Under legacy nothing else is read, so nothing else can fail.
        let s = ConsoleSessionSettings::from_lookup(lookup(&[
            ("MUNARIUM_CONSOLE_SESSIONS", "legacy"),
            ("MUNARIUM_CONSOLE_SESSION_IDLE_SECS", "nonsense"),
        ]))
        .unwrap();
        assert!(!s.sessions());
    }

    #[test]
    fn enabled_settings_are_validated() {
        let s = ConsoleSessionSettings::from_lookup(lookup(&[(
            "MUNARIUM_CONSOLE_SESSIONS",
            "enabled",
        )]))
        .unwrap();
        assert_eq!(s, ConsoleSessionSettings::enabled());
        assert_eq!(
            (s.idle_secs, s.max_secs, s.secure_cookie),
            (1_800, 28_800, false)
        );
        let s = ConsoleSessionSettings::from_lookup(lookup(&[
            ("MUNARIUM_CONSOLE_SESSIONS", "enabled"),
            ("MUNARIUM_CONSOLE_SESSION_IDLE_SECS", "600"),
            ("MUNARIUM_CONSOLE_SESSION_MAX_SECS", "3600"),
            ("MUNARIUM_CONSOLE_COOKIE_SECURE", "true"),
        ]))
        .unwrap();
        assert_eq!(
            (s.idle_secs, s.max_secs, s.secure_cookie),
            (600, 3600, true)
        );
        for bad in [
            vec![("MUNARIUM_CONSOLE_SESSIONS", "on")],
            vec![
                ("MUNARIUM_CONSOLE_SESSIONS", "enabled"),
                ("MUNARIUM_CONSOLE_SESSION_IDLE_SECS", "59"),
            ],
            vec![
                ("MUNARIUM_CONSOLE_SESSIONS", "enabled"),
                ("MUNARIUM_CONSOLE_SESSION_MAX_SECS", "604801"),
            ],
            vec![
                ("MUNARIUM_CONSOLE_SESSIONS", "enabled"),
                ("MUNARIUM_CONSOLE_SESSION_IDLE_SECS", "7200"),
                ("MUNARIUM_CONSOLE_SESSION_MAX_SECS", "3600"),
            ],
            vec![
                ("MUNARIUM_CONSOLE_SESSIONS", "enabled"),
                ("MUNARIUM_CONSOLE_COOKIE_SECURE", "maybe"),
            ],
        ] {
            assert!(
                ConsoleSessionSettings::from_lookup(lookup(&bad)).is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn cookies_carry_the_required_attributes() {
        let plain = ConsoleSessionSettings::enabled();
        assert_eq!(
            plain.set_cookie("h"),
            "__munarium_console=h; HttpOnly; SameSite=Strict; Path=/admin"
        );
        let secure = ConsoleSessionSettings {
            secure_cookie: true,
            ..plain
        };
        // `__Host-` requires Secure, Path=/ and no Domain.
        let c = secure.set_cookie("h");
        assert!(c.starts_with("__Host-munarium_console=h;"), "{c}");
        assert!(c.contains("; Secure;") && c.ends_with("Path=/") && !c.contains("Domain"));
        assert!(secure.clear_cookie().contains("Max-Age=0"));
        assert!(clear_legacy_cookie().starts_with("__munarium_admin=; Max-Age=0;"));
    }

    #[test]
    fn handles_are_random_well_formed_and_stored_only_as_digests() {
        let a = new_handle();
        let b = new_handle();
        assert_ne!(a, b);
        assert!(well_formed(&a) && well_formed(&b));
        assert_ne!(handle_digest(&a), handle_digest(&b));
        assert!(!handle_digest(&a).contains(&a[HANDLE_PREFIX.len()..]));
        for bad in [
            "",
            "mcs1_",
            &a[..a.len() - 1],
            &format!("{a}0"),
            &a.to_uppercase(),
            &a.replace(HANDLE_PREFIX, "mcs2_"),
            "the-mgmt-token",
        ] {
            assert!(!well_formed(bad), "{bad}");
        }
        // The credential digest is salted per session.
        assert_ne!(
            credential_digest("cs-1", "m"),
            credential_digest("cs-2", "m")
        );
        assert_ne!(
            credential_digest("cs-1", "m"),
            credential_digest("cs-1", "m2")
        );
    }

    fn record(now: chrono::DateTime<chrono::Utc>) -> SessionRecord {
        SessionRecord {
            session_id: "cs-x".into(),
            tenant_id: "t".into(),
            role: "mgmt".into(),
            created_by: "i".into(),
            created_at: now,
            last_seen_at: now,
            idle_secs: 600,
            expires_at: now + chrono::Duration::seconds(3600),
            revoked_at: None,
            revoked_reason: None,
            live: true,
        }
    }

    /// Each clause of the liveness rule, alone, ends a session.
    #[test]
    fn liveness_enforces_revocation_idle_and_absolute_expiry() {
        let t0 = chrono::Utc::now();
        let s = ConsoleSessionSettings {
            idle_secs: 600,
            max_secs: 3600,
            ..ConsoleSessionSettings::enabled()
        };
        let secs = chrono::Duration::seconds;
        let r = record(t0);
        assert!(live_at(&r, t0 + secs(599), &s));
        // Idle.
        assert!(!live_at(&r, t0 + secs(600), &s));
        // A replica with a stricter idle setting applies it.
        let strict = ConsoleSessionSettings { idle_secs: 60, ..s };
        assert!(!live_at(&r, t0 + secs(60), &strict));
        // Absolute, even while in use.
        let mut busy = r.clone();
        busy.last_seen_at = t0 + secs(3590);
        assert!(live_at(&busy, t0 + secs(3599), &s));
        assert!(!live_at(&busy, t0 + secs(3600), &s));
        // A replica with a shorter absolute lifetime applies it.
        let short = ConsoleSessionSettings {
            max_secs: 1800,
            ..s
        };
        assert!(!live_at(&busy, t0 + secs(1800), &short));
        // Revoked.
        let mut revoked = r.clone();
        revoked.revoked_at = Some(t0);
        assert!(!live_at(&revoked, t0 + secs(1), &s));
    }

    /// A legacy (rolled-back) replica has no session settings of its own,
    /// so it judges each row by the values recorded at sign-in alone, never
    /// by the defaults: a session a re-enabled fleet could still honour is
    /// listed as live, which prompts its revocation.
    #[test]
    fn a_legacy_replica_judges_liveness_by_the_recorded_values() {
        let t0 = chrono::Utc::now();
        let secs = chrono::Duration::seconds;
        let legacy = ConsoleSessionSettings::default();
        assert!(!legacy.sessions());
        // Recorded under idle 7200 s and absolute 86400 s, both looser than
        // the defaults (1800 s and 28800 s).
        let mut r = record(t0);
        r.idle_secs = 7_200;
        r.expires_at = t0 + secs(86_400);
        assert!(
            live_at(&r, t0 + secs(3_600), &legacy),
            "idle by default only"
        );
        assert!(live_at(&r, t0 + secs(7_199), &legacy));
        assert!(!live_at(&r, t0 + secs(7_200), &legacy), "recorded idle");
        let mut busy = r.clone();
        busy.last_seen_at = t0 + secs(80_000);
        assert!(
            live_at(&busy, t0 + secs(80_001), &legacy),
            "past default max"
        );
        assert!(
            !live_at(&busy, t0 + secs(86_400), &legacy),
            "recorded expiry"
        );
        let mut revoked = r.clone();
        revoked.revoked_at = Some(t0);
        assert!(!live_at(&revoked, t0 + secs(1), &legacy));
        // An enabled replica still applies the stricter of the two.
        assert!(!live_at(
            &r,
            t0 + secs(3_600),
            &ConsoleSessionSettings::enabled()
        ));
        assert_eq!(liveness_bounds(&legacy), (86_400, 604_800));
    }
}
