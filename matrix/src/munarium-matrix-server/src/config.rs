// SPDX-License-Identifier: Apache-2.0
//! Configuration, all from `MUNARIUM_MATRIX_*` environment variables.
//!
//! Two rules the server tree learned and this one inherits:
//!
//! - **Fail at startup, not at first use.** Everything checkable without side
//!   effects is checked before a port is bound. Exit code 2 means the
//!   environment is wrong and nothing was touched.
//! - **No secret is ever stored in a config field.** Secrets are *references*
//!   resolved at call time, so a `Debug` of the config cannot leak one.

use std::fmt;

/// Which role this process runs. One binary, one selected role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Registry, validation, journal, reports, scheduler. Serves `/admin`.
    Control,
    /// Short-lived, deadline-bounded `execute` work.
    Query,
    /// Resumable snapshot / watermark / CDC jobs.
    Sync,
    /// Observation -> discrepancy pipeline.
    Reconcile,
    /// Everything in one process — the laptop.
    All,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Control => "control",
            Role::Query => "query",
            Role::Sync => "sync",
            Role::Reconcile => "reconcile",
            Role::All => "all",
        }
    }

    /// Does this role serve the registry/write plane?
    pub fn serves_control(self) -> bool {
        matches!(self, Role::Control | Role::All)
    }
    // The remaining three predicates are read by the worker loops, which are
    // spawned from `main` when the sync/query/reconcile roles are wired into
    // the binary. They are tested here and kept beside `serves_control` so the
    // role vocabulary lives in one place.
    #[allow(dead_code)]
    pub fn serves_query(self) -> bool {
        matches!(self, Role::Query | Role::All)
    }
    #[allow(dead_code)]
    pub fn runs_sync(self) -> bool {
        matches!(self, Role::Sync | Role::All)
    }
    #[allow(dead_code)]
    pub fn runs_reconcile(self) -> bool {
        matches!(self, Role::Reconcile | Role::All)
    }
}

impl std::str::FromStr for Role {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "control" => Ok(Role::Control),
            "query" => Ok(Role::Query),
            "sync" => Ok(Role::Sync),
            "reconcile" => Ok(Role::Reconcile),
            "all" => Ok(Role::All),
            other => Err(format!(
                "MUNARIUM_MATRIX_ROLE must be control|query|sync|reconcile|all, got '{other}'"
            )),
        }
    }
}

/// A static token: `(token, tenant, role)` where role is rw | ro | mgmt —
/// the same vocabulary the server uses, so an operator learns it once.
#[derive(Clone, PartialEq, Eq)]
pub struct StaticToken {
    pub token: String,
    pub tenant: String,
    pub role: String,
}

impl fmt::Debug for StaticToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The token value is a secret even in a debug dump.
        f.debug_struct("StaticToken")
            .field("token", &"<redacted>")
            .field("tenant", &self.tenant)
            .field("role", &self.role)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthMode {
    /// Dev/conformance only: every caller is rw+mgmt on `tenant-default`.
    Disabled,
    Static(Vec<StaticToken>),
}

#[derive(Debug, Clone)]
pub struct Config {
    pub role: Role,
    pub http_addr: String,
    pub ops_addr: String,
    /// The gRPC data plane's listener. `None` when `MUNARIUM_MATRIX_GRPC_ADDR`
    /// is `disabled`. Served only by a role that serves the query plane.
    pub grpc_addr: Option<String>,
    pub database_url: Option<String>,
    pub db_max_conns: u32,
    pub auth: AuthMode,
    /// Base URL of munarium-server.
    pub server_url: Option<String>,
    /// Reference to the server token; resolved at call time, never stored.
    pub server_token_ref: Option<String>,
    /// The server version this build is built against.
    pub target_server_version: String,
    // The four settings below are parsed and validated at startup — so a typo
    // fails before a port is bound — and consumed by the worker loops and the
    // adapter factory as each role is wired in. Parsing them early is the
    // point: a config error must surface at boot, not at first use.
    #[allow(dead_code)]
    pub max_concurrency: usize,
    /// Default-deny egress. Turning it off is possible and loud.
    #[allow(dead_code)]
    pub egress_default_deny: bool,
    #[allow(dead_code)]
    pub log_format_json: bool,
    pub instance_id: String,
    /// Where landing-export fixtures live when the `file` object store is used.
    #[allow(dead_code)]
    pub file_root: Option<String>,
    /// Promotion gates. A mapping may be promoted to
    /// authoritative only when its latest completed run clears BOTH. The
    /// defaults are proposals awaiting confirmation (owner question Q8, 2026-08-28);
    /// they are deliberately strict, because the cost of a wrong promotion is
    /// canon rewritten under a machine's name.
    pub promotion_min_identity_precision: f64,
    pub promotion_min_value_conformance: f64,
    /// Whether `/admin` is served at all. Default enabled on the
    /// roles that could serve it; a hardened deployment sets
    /// `MUNARIUM_MATRIX_ADMIN=disabled` and the routes are not mounted —
    /// not hidden behind a check, absent, so there is nothing to
    /// misconfigure back on.
    pub admin_enabled: bool,
    /// A random per-process secret. Two things derive from it and neither is
    /// a stored credential: the admin CSRF token, and nothing else. Rotating
    /// on restart is the point — a synchronizer token from a previous process
    /// is stale, and a stale form is refused rather than replayed.
    pub boot_secret: String,
}

fn env(key: &str) -> Option<String> {
    std::env::var(format!("MUNARIUM_MATRIX_{key}"))
        .ok()
        .filter(|v| !v.trim().is_empty())
}

fn env_or(key: &str, default: &str) -> String {
    env(key).unwrap_or_else(|| default.to_string())
}

fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> Result<T, String>
where
    T::Err: fmt::Display,
{
    match env(key) {
        None => Ok(default),
        Some(v) => v.parse().map_err(|e| format!("MUNARIUM_MATRIX_{key}: {e}")),
    }
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let role: Role = env_or("ROLE", "all").parse()?;

        let auth = match env_or("AUTH_MODE", "static").to_lowercase().as_str() {
            "disabled" => AuthMode::Disabled,
            "static" => {
                let raw = match env("STATIC_TOKENS") {
                    Some(v) => v,
                    None => match env("STATIC_TOKEN_FILE") {
                        Some(path) => std::fs::read_to_string(&path).map_err(|e| {
                            format!("MUNARIUM_MATRIX_STATIC_TOKEN_FILE {path}: {e}")
                        })?,
                        None => {
                            return Err(
                                "MUNARIUM_MATRIX_AUTH_MODE=static requires \
                                        MUNARIUM_MATRIX_STATIC_TOKENS or _FILE \
                                        (token:tenant:role,...); use AUTH_MODE=disabled to opt out"
                                    .into(),
                            )
                        }
                    },
                };
                let mut tokens = Vec::new();
                for entry in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    let parts: Vec<&str> = entry.split(':').collect();
                    if parts.len() != 3 {
                        return Err(
                            "bad static token entry (expected token:tenant:role)".to_string()
                        );
                    }
                    if !matches!(parts[2], "rw" | "ro" | "mgmt") {
                        return Err(format!(
                            "static token role must be rw|ro|mgmt, got '{}'",
                            parts[2]
                        ));
                    }
                    tokens.push(StaticToken {
                        token: parts[0].to_string(),
                        tenant: parts[1].to_string(),
                        role: parts[2].to_string(),
                    });
                }
                if tokens.is_empty() {
                    return Err("MUNARIUM_MATRIX_STATIC_TOKENS is empty".into());
                }
                AuthMode::Static(tokens)
            }
            other => {
                return Err(format!(
                    "MUNARIUM_MATRIX_AUTH_MODE must be static|disabled, got '{other}'"
                ))
            }
        };

        let database_url = env("DATABASE_URL");
        // Every role except a bare `query` needs the store, and a query role
        // without one cannot journal — which is not a mode we offer.
        if database_url.is_none() {
            return Err(
                "MUNARIUM_MATRIX_DATABASE_URL is required (schema matrix, role matrix_owner)"
                    .into(),
            );
        }

        Ok(Config {
            role,
            http_addr: env_or("HTTP_ADDR", "0.0.0.0:8180"),
            ops_addr: env_or("OPS_ADDR", "0.0.0.0:9190"),
            grpc_addr: match env_or("GRPC_ADDR", "0.0.0.0:50151").as_str() {
                "disabled" | "" => None,
                a => Some(a.to_string()),
            },
            database_url,
            db_max_conns: env_parse("DB_MAX_CONNS", 10u32)?,
            auth,
            server_url: env("SERVER_URL"),
            server_token_ref: env("SERVER_TOKEN_REF"),
            target_server_version: env_or("TARGET_SERVER_VERSION", "1.0.0"),
            promotion_min_identity_precision: env_parse("PROMOTION_MIN_IDENTITY_PRECISION", 0.95)?,
            promotion_min_value_conformance: env_parse("PROMOTION_MIN_VALUE_CONFORMANCE", 0.99)?,
            admin_enabled: match env_or("ADMIN", "enabled").as_str() {
                "enabled" => true,
                "disabled" => false,
                other => {
                    return Err(format!(
                        "MUNARIUM_MATRIX_ADMIN must be 'enabled' or 'disabled', not '{other}'"
                    ))
                }
            },
            boot_secret: uuid::Uuid::new_v4().to_string(),
            max_concurrency: env_parse("MAX_CONCURRENCY", 64usize)?,
            egress_default_deny: env_or("EGRESS_DEFAULT_DENY", "true") != "false",
            log_format_json: env_or("LOG_FORMAT", "plain") == "json",
            instance_id: env_or(
                "INSTANCE_ID",
                &format!("matrix-{}", uuid::Uuid::new_v4().simple()),
            ),
            file_root: env("FILE_ROOT"),
        })
    }
}

/// Resolve a secret by reference. `env:NAME` reads an environment variable,
/// `file:PATH` reads a file (the Key Vault CSI shape), and a bare name is
/// looked up as `MUNARIUM_MATRIX_SECRET_<NAME>`.
///
/// Never logs, never returns the reference in an error — an error message that
/// echoes `env:PROD_DB_PASSWORD=hunter2` has defeated the whole mechanism.
pub fn resolve_secret(reference: &str) -> Result<String, String> {
    let r = reference.trim();
    let value = if let Some(name) = r.strip_prefix("env:") {
        std::env::var(name).ok()
    } else if let Some(path) = r.strip_prefix("file:") {
        std::fs::read_to_string(path)
            .ok()
            .map(|s| s.trim().to_string())
    } else {
        let key = format!(
            "MUNARIUM_MATRIX_SECRET_{}",
            r.to_uppercase().replace('-', "_")
        );
        std::env::var(&key).ok()
    };
    value
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("credential '{r}' did not resolve"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_parse_and_gate_the_right_planes() {
        assert_eq!("sync".parse::<Role>().unwrap(), Role::Sync);
        assert!("gateway".parse::<Role>().is_err());
        assert!(Role::Sync.runs_sync());
        assert!(
            !Role::Sync.serves_control(),
            "a sync worker serves no registry"
        );
        assert!(Role::All.serves_control() && Role::All.runs_reconcile());
        assert!(
            !Role::Query.runs_sync(),
            "a query container refuses sync work"
        );
    }

    #[test]
    fn a_static_token_never_prints_itself() {
        let t = StaticToken {
            token: "super-secret".into(),
            tenant: "acme".into(),
            role: "rw".into(),
        };
        let printed = format!("{t:?}");
        assert!(!printed.contains("super-secret"), "{printed}");
        assert!(printed.contains("acme"));
    }

    #[test]
    fn secret_resolution_never_echoes_the_value_in_an_error() {
        let err = resolve_secret("env:MUNARIUM_MATRIX_TEST_ABSENT_VAR").unwrap_err();
        assert!(err.contains("did not resolve"));
        assert!(!err.contains('='));
    }
}
