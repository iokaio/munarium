// SPDX-License-Identifier: Apache-2.0
//! Shared application state and authentication.

use crate::config::{AuthMode, Config, Role};
use munarium_matrix_store::MatrixStore;
use munarium_matrix_types::dto::Problem;
use std::sync::Arc;

/// The authenticated caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub tenant: String,
    /// rw | ro | mgmt
    pub role: String,
    pub disabled_mode: bool,
}

impl Caller {
    /// Commands need rw. `mgmt` deliberately does NOT pass: the management
    /// role reads and administers; it does not apply assets. Same split the
    /// server draws, for the same reason — a leaked mgmt token must not be
    /// able to change what the system does.
    pub fn require_rw(&self) -> Result<(), Problem> {
        if self.role == "rw" || self.disabled_mode {
            Ok(())
        } else {
            Err(Problem::new(
                "forbidden",
                403,
                "forbidden",
                format!("role '{}' cannot execute commands (rw required)", self.role),
            ))
        }
    }

    pub fn require_mgmt(&self) -> Result<(), Problem> {
        if self.role == "mgmt" || self.disabled_mode {
            Ok(())
        } else {
            Err(Problem::new(
                "forbidden",
                403,
                "forbidden",
                format!(
                    "role '{}' cannot use the management plane (mgmt required)",
                    self.role
                ),
            ))
        }
    }
}

pub struct AppState {
    pub config: Config,
    pub store: MatrixStore,
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// The server version observed at startup, and what it means. Reported on
    /// `/version` so an operator can see the lockstep answer without reading
    /// logs.
    pub server_version: Option<String>,
    pub server_compatibility: crate::compat::Compatibility,
    /// Set the moment a stop signal arrives, so `/readyz` reports draining and
    /// load balancers stop routing before in-flight work finishes.
    pub draining: std::sync::atomic::AtomicBool,
    pub metrics: Arc<crate::metrics::Metrics>,
    /// Adapters this build carries beyond the ones core links directly.
    ///
    /// Empty in a stock core build. Munarium Matrix Enterprise, and any
    /// out-of-tree host implementing the public `SourceAdapter` interface,
    /// fills it at start-up through [`AppState::with_adapters`]; a kind nobody
    /// registered is refused by name rather than mis-served.
    pub adapters: Arc<crate::adapters::AdapterRegistry>,
}

impl AppState {
    /// A state with no server contacted. Used by tests and by any role that
    /// runs without a configured server URL.
    #[allow(dead_code)]
    pub fn new(config: Config, store: MatrixStore) -> Arc<Self> {
        Self::with_server_version(config, store, None)
    }

    pub fn with_server_version(
        config: Config,
        store: MatrixStore,
        server_version: Option<String>,
    ) -> Arc<Self> {
        Self::with_adapters(
            config,
            store,
            server_version,
            Arc::new(crate::adapters::AdapterRegistry::new()),
        )
    }

    /// The full constructor, and the one an out-of-tree binary uses.
    ///
    /// Munarium Matrix Enterprise builds its registry, registers the
    /// analytics-platform adapters, and calls this. Nothing else about the
    /// runtime differs between that build and a stock one, which is what keeps
    /// the two honest about each other.
    pub fn with_adapters(
        config: Config,
        store: MatrixStore,
        server_version: Option<String>,
        adapters: Arc<crate::adapters::AdapterRegistry>,
    ) -> Arc<Self> {
        let server_compatibility =
            crate::compat::compare(&config.target_server_version, server_version.as_deref());
        Arc::new(Self {
            config,
            store,
            started_at: chrono::Utc::now(),
            server_version,
            server_compatibility,
            draining: std::sync::atomic::AtomicBool::new(false),
            metrics: Arc::new(crate::metrics::Metrics::default()),
            adapters,
        })
    }

    /// Seconds this process has been up — the one thing `/version` can report
    /// that a restart makes obvious.
    pub fn uptime_seconds(&self) -> i64 {
        (chrono::Utc::now() - self.started_at).num_seconds()
    }

    pub fn role(&self) -> Role {
        self.config.role
    }

    /// Resolve a bearer token. Constant-time comparison so a token cannot be
    /// recovered a byte at a time from response timing.
    pub fn authenticate(&self, bearer: Option<&str>) -> Result<Caller, Problem> {
        match &self.config.auth {
            AuthMode::Disabled => Ok(Caller {
                tenant: "tenant-default".into(),
                role: "rw".into(),
                disabled_mode: true,
            }),
            AuthMode::Static(tokens) => {
                let token = bearer.ok_or_else(|| {
                    Problem::new(
                        "unauthenticated",
                        401,
                        "unauthenticated",
                        "missing bearer token",
                    )
                })?;
                tokens
                    .iter()
                    .find(|t| constant_time_eq(&t.token, token))
                    .map(|t| Caller {
                        tenant: t.tenant.clone(),
                        role: t.role.clone(),
                        disabled_mode: false,
                    })
                    .ok_or_else(|| {
                        Problem::new("unauthenticated", 401, "unauthenticated", "invalid token")
                    })
            }
        }
    }
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StaticToken;

    fn config(auth: AuthMode) -> Config {
        Config {
            role: Role::All,
            http_addr: "127.0.0.1:0".into(),
            ops_addr: "127.0.0.1:0".into(),
            grpc_addr: None,
            database_url: Some("postgres://x".into()),
            db_max_conns: 2,
            auth,
            server_url: None,
            server_token_ref: None,
            target_server_version: "0.3.0".into(),
            max_concurrency: 8,
            egress_default_deny: true,
            log_format_json: false,
            instance_id: "test".into(),
            file_root: None,
            promotion_min_identity_precision: 0.95,
            promotion_min_value_conformance: 0.99,
            admin_enabled: true,
            boot_secret: "test-boot-secret".into(),
        }
    }

    fn state(auth: AuthMode) -> AppState {
        AppState {
            config: config(auth),
            // Never touched by these tests; auth is pure.
            store: MatrixStore::disconnected_for_tests(),
            started_at: chrono::Utc::now(),
            server_version: None,
            server_compatibility: crate::compat::Compatibility::Unknown,
            draining: std::sync::atomic::AtomicBool::new(false),
            metrics: Arc::new(crate::metrics::Metrics::default()),
            adapters: Arc::new(crate::adapters::AdapterRegistry::new()),
        }
    }

    // A runtime is needed even for a LAZY pool: sqlx spawns its pool
    // maintenance task at construction.
    #[tokio::test]
    async fn a_wrong_token_is_unauthenticated_and_a_missing_one_too() {
        let s = state(AuthMode::Static(vec![StaticToken {
            token: "good".into(),
            tenant: "acme".into(),
            role: "rw".into(),
        }]));
        assert_eq!(s.authenticate(Some("good")).unwrap().tenant, "acme");
        assert_eq!(s.authenticate(Some("bad")).unwrap_err().status, 401);
        assert_eq!(s.authenticate(None).unwrap_err().status, 401);
    }

    #[test]
    fn mgmt_cannot_write_and_rw_cannot_administer() {
        let rw = Caller {
            tenant: "a".into(),
            role: "rw".into(),
            disabled_mode: false,
        };
        let mgmt = Caller {
            tenant: "a".into(),
            role: "mgmt".into(),
            disabled_mode: false,
        };
        let ro = Caller {
            tenant: "a".into(),
            role: "ro".into(),
            disabled_mode: false,
        };

        assert!(rw.require_rw().is_ok());
        assert!(
            mgmt.require_rw().is_err(),
            "a leaked mgmt token must not apply assets"
        );
        assert!(ro.require_rw().is_err());

        assert!(mgmt.require_mgmt().is_ok());
        assert!(rw.require_mgmt().is_err());
    }

    #[tokio::test]
    async fn disabled_auth_passes_every_gate_and_says_so() {
        let s = state(AuthMode::Disabled);
        let c = s.authenticate(None).unwrap();
        assert!(c.disabled_mode);
        assert!(c.require_rw().is_ok() && c.require_mgmt().is_ok());
    }
}
