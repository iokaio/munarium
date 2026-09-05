// SPDX-License-Identifier: Apache-2.0
//! What this process can actually do, and why not.
//!
//! Configuration says what an operator asked for; infrastructure says what is
//! present. This module resolves the two into an honest answer, and the
//! distinction it draws is the important part:
//!
//! **Serving refuses. Mirror and shadow degrade.**
//!
//! A replica configured to serve from artifacts it cannot read must not start:
//! silently falling back to PostgreSQL for a scope the selector routes to the
//! datastore would hide the exact failure the no-fallback rule exists to
//! surface (§9.1). But mirror and shadow are additive — their whole contract is
//! that failing at them never affects PostgreSQL serving (§9.2) — so missing
//! infrastructure drops them to `postgres` loudly rather than taking a healthy
//! replica out of rotation over optional telemetry.

use std::time::Duration;

use crate::config::{ConfigError, DatastoreConfig};
use crate::RetrievalMode;

/// One capability, and the reason it is on or off.
///
/// The reason is not decoration. An operator seeing "artifact store: disabled"
/// needs to know whether that is a deliberate configuration, a missing
/// container, or a binary compiled without an engine — three different fixes,
/// and a bare boolean sends them looking in the wrong place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capability {
    pub name: &'static str,
    pub enabled: bool,
    pub detail: String,
}

impl Capability {
    fn on(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            enabled: true,
            detail: detail.into(),
        }
    }
    fn off(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            enabled: false,
            detail: detail.into(),
        }
    }
}

/// Which artifact store this process can reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactStoreKind {
    None,
    File,
    Azure,
    S3,
    Gcs,
    Postgres,
}

impl ArtifactStoreKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::File => "file",
            Self::Azure => "az",
            Self::S3 => "s3",
            Self::Gcs => "gcs",
            Self::Postgres => "pg",
        }
    }

    /// Unrecognised parses to `None` rather than erroring, and the capability
    /// then reports it as unconfigured. A typo must not select a backend.
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "file" => Self::File,
            "az" | "azure" => Self::Azure,
            "s3" => Self::S3,
            "gcs" => Self::Gcs,
            "pg" | "postgres" => Self::Postgres,
            _ => Self::None,
        }
    }
}

/// What the environment actually provides.
#[derive(Debug, Clone, Default)]
pub struct Infrastructure {
    pub has_postgres_catalog: bool,
    pub artifact_store: Option<ArtifactStoreKind>,
    /// A local root that exists and is WRITABLE. Probed rather than assumed: a
    /// configured path on a read-only mount looks configured, and that is the
    /// failure a bare "is it set?" check misses.
    pub local_root_writable: bool,
    pub local_root_configured: bool,
    /// Engines compiled into THIS binary, which is not the same question as
    /// what the deployment intends.
    pub compiled_engines: Vec<String>,
}

impl Infrastructure {
    /// Probe the filesystem for a configured local root.
    ///
    /// Writability is tested by writing, not by reading permissions: on a
    /// container filesystem the mode bits can say yes while the mount says no.
    pub fn probe_local_root(path: Option<&str>) -> (bool, bool) {
        let Some(p) = path else { return (false, false) };
        let dir = std::path::Path::new(p);
        if std::fs::create_dir_all(dir).is_err() {
            return (true, false);
        }
        let probe = dir.join(".munarium-write-probe");
        let writable = std::fs::write(&probe, b"").is_ok();
        let _ = std::fs::remove_file(&probe);
        (true, writable)
    }
}

/// What the process resolved it can do.
#[derive(Debug, Clone)]
pub struct DatastoreCapabilities {
    /// What the operator asked for.
    pub configured_mode: RetrievalMode,
    /// What this process will actually run. May be BELOW the configured mode.
    pub effective_mode: RetrievalMode,
    /// Set when configured and effective differ, or when serving is blocked.
    pub degraded_because: Option<String>,
    pub artifact_store: ArtifactStoreKind,
    pub capabilities: Vec<Capability>,
    /// Non-empty means the process must not start in the configured mode.
    pub blocking: Vec<ConfigError>,
    /// The resolved pin horizon, in seconds.
    ///
    /// Carried here because several things must agree on it — backfill's
    /// required-version window, retention, and the eviction leases — and two of
    /// them computing it separately is how a version becomes evictable while a
    /// session still holds it.
    pub pin_horizon_secs: u64,
}

impl DatastoreCapabilities {
    pub fn is_enabled(&self, name: &str) -> bool {
        self.capabilities
            .iter()
            .any(|c| c.name == name && c.enabled)
    }

    /// True when the configured mode cannot be honoured at all.
    pub fn must_refuse_startup(&self) -> bool {
        !self.blocking.is_empty()
            || (self.configured_mode == RetrievalMode::Datastore && self.degraded_because.is_some())
    }

    pub fn resolve(
        config: &DatastoreConfig,
        infra: &Infrastructure,
        session_idle_ttl: Duration,
        runbook_ttl: Duration,
    ) -> Self {
        let mut caps = Vec::new();
        let store = infra.artifact_store.unwrap_or(ArtifactStoreKind::None);

        caps.push(if infra.has_postgres_catalog {
            Capability::on(
                "catalog",
                "PostgreSQL reachable; artifacts, bindings and the rollout selector resolve",
            )
        } else {
            Capability::off(
                "catalog",
                "no PostgreSQL store; the datastore tier has nothing to resolve a version against",
            )
        });

        caps.push(match store {
            ArtifactStoreKind::None => Capability::off(
                "artifact-store",
                "MUNARIUM_DATASTORE_ARTIFACT_STORE unset or unrecognised; artifacts have nowhere to live",
            ),
            k => Capability::on("artifact-store", format!("{} backend configured", k.as_str())),
        });

        caps.push(
            match (infra.local_root_configured, infra.local_root_writable) {
                (false, _) => Capability::off(
                    "l1-cache",
                    "MUNARIUM_DATASTORE_LOCAL_ROOT unset; nowhere to hydrate to",
                ),
                (true, false) => Capability::off(
                    "l1-cache",
                    "the configured local root is missing or not writable",
                ),
                (true, true) => Capability::on("l1-cache", "local root present and writable"),
            },
        );

        let lexical = infra.compiled_engines.iter().any(|e| e == "tantivy");
        caps.push(if lexical {
            Capability::on("lexical-engine", "tantivy compiled in")
        } else {
            Capability::off(
                "lexical-engine",
                "no lexical engine compiled into this binary",
            )
        });

        let vector = infra
            .compiled_engines
            .iter()
            .any(|e| e.starts_with("munarium-flat") || e == "diskann");
        caps.push(if vector {
            Capability::on("vector-engine", "exact vector index available")
        } else {
            // Not fatal: a lexical-only corpus is a first-class shape, not a
            // degraded one.
            Capability::off(
                "vector-engine",
                "no vector engine compiled in; lexical-only artifacts still work",
            )
        });

        // Mirror and shadow BUILD, so they need everything a build needs. Only
        // serving additionally reads artifacts back on the request path, which
        // needs the same set.
        let can_build = infra.has_postgres_catalog
            && store != ArtifactStoreKind::None
            && infra.local_root_writable
            && lexical;

        let blocking = config.validate(infra.has_postgres_catalog, session_idle_ttl, runbook_ttl);

        let (effective_mode, degraded_because) = match config.mode {
            RetrievalMode::Datastore if !can_build => (
                RetrievalMode::Datastore,
                Some(
                    "configured to serve from artifacts, but the infrastructure to read them is \
                     incomplete. This process must not start rather than accept traffic it \
                     cannot answer -- a silent fallback to PostgreSQL would hide exactly the \
                     failure the no-fallback rule exists to surface."
                        .to_string(),
                ),
            ),
            RetrievalMode::Mirror | RetrievalMode::Shadow if !can_build => (
                RetrievalMode::Postgres,
                Some(
                    "mirror/shadow needs a catalog, an artifact store, a writable local root and \
                     a lexical engine; at least one is missing, so this replica serves from \
                     PostgreSQL and builds nothing. Serving is unaffected -- that is the whole \
                     contract of these modes."
                        .to_string(),
                ),
            ),
            m => (m, None),
        };

        Self {
            configured_mode: config.mode,
            effective_mode,
            degraded_because,
            artifact_store: store,
            capabilities: caps,
            blocking,
            pin_horizon_secs: config.pin_horizon.as_duration().as_secs(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PinHorizon, DEFAULT_RECOVERY_MARGIN};

    const HOUR: Duration = Duration::from_secs(3600);

    fn full() -> Infrastructure {
        Infrastructure {
            has_postgres_catalog: true,
            artifact_store: Some(ArtifactStoreKind::Azure),
            local_root_writable: true,
            local_root_configured: true,
            compiled_engines: vec!["tantivy".into(), "munarium-flat".into()],
        }
    }

    fn cfg(mode: RetrievalMode) -> DatastoreConfig {
        DatastoreConfig {
            mode,
            local_root: Some("/var/lib/munarium/indexes".into()),
            l1_high_watermark_bytes: 1 << 30,
            l1_low_watermark_bytes: 1 << 29,
            pin_horizon: PinHorizon::derive(HOUR, HOUR, DEFAULT_RECOVERY_MARGIN),
            retired_retention: HOUR * 48,
            allow_short_pin_horizon: false,
            supported_formats: (1, 1),
            compiled_engines: vec!["tantivy".into()],
        }
    }

    fn resolve(mode: RetrievalMode, infra: &Infrastructure) -> DatastoreCapabilities {
        DatastoreCapabilities::resolve(&cfg(mode), infra, HOUR, HOUR)
    }

    #[test]
    fn full_infrastructure_enables_everything() {
        let c = resolve(RetrievalMode::Datastore, &full());
        assert!(c.blocking.is_empty(), "{:?}", c.blocking);
        assert_eq!(c.effective_mode, RetrievalMode::Datastore);
        assert!(c.degraded_because.is_none());
        assert!(!c.must_refuse_startup());
        for name in [
            "catalog",
            "artifact-store",
            "l1-cache",
            "lexical-engine",
            "vector-engine",
        ] {
            assert!(c.is_enabled(name), "{name} should be enabled");
        }
    }

    /// The distinction the design rests on: mirror is additive, so it degrades
    /// rather than taking a healthy replica out of rotation.
    #[test]
    fn mirror_degrades_to_postgres_when_infrastructure_is_missing() {
        let mut infra = full();
        infra.artifact_store = None;
        let c = resolve(RetrievalMode::Mirror, &infra);
        assert_eq!(c.effective_mode, RetrievalMode::Postgres);
        assert!(!c.must_refuse_startup(), "a degraded mirror still starts");
        let why = c.degraded_because.expect("a degradation must say why");
        assert!(why.contains("Serving is unaffected"), "{why}");
    }

    /// Serving does NOT degrade. A silent fallback would hide the failure the
    /// no-fallback rule exists to surface.
    #[test]
    fn serving_refuses_rather_than_degrading() {
        let mut infra = full();
        infra.local_root_writable = false;
        let c = resolve(RetrievalMode::Datastore, &infra);
        assert_eq!(
            c.effective_mode,
            RetrievalMode::Datastore,
            "serving must not silently become postgres"
        );
        assert!(c.must_refuse_startup());
        assert!(c.degraded_because.unwrap().contains("must not start"));
    }

    /// The failure a bare "is it set?" check misses: a path on a read-only
    /// mount looks configured.
    #[test]
    fn configured_but_unwritable_is_a_distinct_reason_from_unset() {
        let mut infra = full();
        infra.local_root_writable = false;
        let c = resolve(RetrievalMode::Mirror, &infra);
        let cap = c
            .capabilities
            .iter()
            .find(|x| x.name == "l1-cache")
            .unwrap();
        assert!(!cap.enabled);
        assert!(cap.detail.contains("not writable"), "{}", cap.detail);

        infra.local_root_configured = false;
        let c = resolve(RetrievalMode::Mirror, &infra);
        let cap = c
            .capabilities
            .iter()
            .find(|x| x.name == "l1-cache")
            .unwrap();
        assert!(cap.detail.contains("unset"), "{}", cap.detail);
    }

    /// postgres mode needs none of it, and must never be blocked: it is the
    /// rollback path, and a rollback that can fail to start is not a rollback.
    #[test]
    fn postgres_mode_needs_no_datastore_infrastructure() {
        let c = resolve(RetrievalMode::Postgres, &Infrastructure::default());
        assert_eq!(c.effective_mode, RetrievalMode::Postgres);
        assert!(c.degraded_because.is_none());
        assert!(c.blocking.is_empty(), "{:?}", c.blocking);
        assert!(!c.must_refuse_startup());
    }

    #[test]
    fn a_missing_vector_engine_does_not_block_anything() {
        let mut infra = full();
        infra.compiled_engines = vec!["tantivy".into()];
        let c = resolve(RetrievalMode::Mirror, &infra);
        assert!(!c.is_enabled("vector-engine"));
        assert_eq!(c.effective_mode, RetrievalMode::Mirror);
    }

    #[test]
    fn every_capability_carries_a_reason() {
        let c = resolve(RetrievalMode::Mirror, &Infrastructure::default());
        for cap in &c.capabilities {
            assert!(!cap.detail.is_empty(), "{} has no reason", cap.name);
        }
    }

    #[test]
    fn an_unrecognised_store_name_does_not_select_a_backend() {
        assert_eq!(ArtifactStoreKind::parse("azure"), ArtifactStoreKind::Azure);
        assert_eq!(ArtifactStoreKind::parse("aZ"), ArtifactStoreKind::Azure);
        assert_eq!(ArtifactStoreKind::parse("azzure"), ArtifactStoreKind::None);
        assert_eq!(ArtifactStoreKind::parse(""), ArtifactStoreKind::None);
    }

    #[test]
    fn probing_an_unset_root_reports_unconfigured_not_unwritable() {
        assert_eq!(Infrastructure::probe_local_root(None), (false, false));
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("l1");
        let (configured, writable) = Infrastructure::probe_local_root(Some(p.to_str().unwrap()));
        assert!(configured && writable);
    }
}
