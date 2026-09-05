// SPDX-License-Identifier: Apache-2.0
//! Datastore configuration, and the validation that refuses a bad one.
//!
//! §9.3 lists what configuration must reject. The list matters more than it
//! looks: each entry is a setting that, left wrong, produces a system that
//! appears to work. A pin horizon shorter than the session TTL does not fail —
//! it deletes an artifact a live session is still allowed to ask for, and the
//! failure surfaces later as a missing version nobody can explain.
//!
//! So validation happens once, at startup, and refuses rather than warns. A
//! warning about a destructive retention policy is a warning nobody reads until
//! after the data is gone.

use std::time::Duration;

use crate::RetrievalMode;

/// How long after a version stops being active a session or runbook may still
/// legitimately resolve it (§5.3).
///
/// This is the value every retention decision hangs off: L1 residency may be
/// evicted whenever it is unpinned, because L2 can rehydrate it, but **L2 bytes
/// cannot be reclaimed while any supported pin can still resolve them**.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinHorizon(Duration);

impl PinHorizon {
    pub fn as_duration(self) -> Duration {
        self.0
    }

    /// An explicitly configured horizon. Validation still compares it against
    /// the derived minimum, so setting one does not bypass the check.
    pub fn from_secs(secs: u64) -> Self {
        Self(Duration::from_secs(secs))
    }

    /// The shortest horizon that is safe given the TTLs already in force.
    ///
    /// A pin can be created at the last instant before a session expires, so
    /// the horizon must cover the longest TTL PLUS a margin for recovery and
    /// audit — an operator investigating an incident an hour later is still a
    /// legitimate reader of the version that answered.
    pub fn derive(
        session_idle_ttl: Duration,
        runbook_ttl: Duration,
        recovery_margin: Duration,
    ) -> Self {
        Self(session_idle_ttl.max(runbook_ttl) + recovery_margin)
    }
}

/// The recovery/audit margin added to the longest TTL when deriving a horizon.
///
/// Six hours rather than a round day: long enough that an incident spanning a
/// shift change still has its evidence, short enough that it does not quietly
/// become the dominant term and hide a misconfigured TTL.
pub const DEFAULT_RECOVERY_MARGIN: Duration = Duration::from_secs(6 * 60 * 60);

/// Everything the datastore tier reads from the environment.
#[derive(Debug, Clone)]
pub struct DatastoreConfig {
    pub mode: RetrievalMode,
    pub local_root: Option<String>,
    pub l1_high_watermark_bytes: u64,
    pub l1_low_watermark_bytes: u64,
    pub pin_horizon: PinHorizon,
    pub retired_retention: Duration,
    /// Set only by an operator who has read what it means.
    pub allow_short_pin_horizon: bool,
    pub supported_formats: (u32, u32),
    /// Compiled-in engines, so serving mode can refuse a binary that cannot
    /// open what it is being asked to serve.
    pub compiled_engines: Vec<String>,
}

/// Why a configuration was refused.
///
/// Each variant names the consequence, not just the rule: an operator reading
/// this at 3am needs to know what would have happened, not which line failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    NoCatalog(RetrievalMode),
    InvertedWatermarks {
        high: u64,
        low: u64,
    },
    ShortPinHorizon {
        configured: Duration,
        derived: Duration,
    },
    RetentionBelowPinHorizon {
        retention: Duration,
        horizon: Duration,
    },
    NoLocalRoot(RetrievalMode),
    NoCompatibleEngine,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCatalog(m) => write!(
                f,
                "MUNARIUM_RETRIEVAL_MODE={} needs the PostgreSQL store: the artifact catalog, \
                 bindings and rollout selector are all durable truth, and without them the \
                 datastore tier has nothing to resolve a version against",
                m.as_str()
            ),
            Self::InvertedWatermarks { high, low } => write!(
                f,
                "L1 low watermark {low} is not below high watermark {high}; equal or inverted \
                 watermarks make eviction either continuous or impossible"
            ),
            Self::ShortPinHorizon {
                configured,
                derived,
            } => write!(
                f,
                "MUNARIUM_DATASTORE_PIN_HORIZON is {}s but the session and runbook TTLs in force \
                 imply at least {}s. A shorter horizon does not fail loudly -- it collects an \
                 artifact a live session may still ask for, and surfaces later as a version \
                 nobody can explain. Set MUNARIUM_DATASTORE_ALLOW_SHORT_PIN_HORIZON=true only \
                 with that consequence accepted.",
                configured.as_secs(),
                derived.as_secs()
            ),
            Self::RetentionBelowPinHorizon { retention, horizon } => write!(
                f,
                "MUNARIUM_DATASTORE_RETIRED_RETENTION is {}s, below the pin horizon of {}s. L1 \
                 residency may be evicted freely because L2 can rehydrate it, but L2 bytes \
                 cannot be reclaimed while any supported pin can still resolve them.",
                retention.as_secs(),
                horizon.as_secs()
            ),
            Self::NoLocalRoot(m) => write!(
                f,
                "MUNARIUM_RETRIEVAL_MODE={} needs MUNARIUM_DATASTORE_LOCAL_ROOT: artifacts are \
                 served from local files, and a replica with nowhere to put them cannot serve",
                m.as_str()
            ),
            Self::NoCompatibleEngine => write!(
                f,
                "serving mode with no search engine compiled into this binary; it could accept \
                 traffic it has no way to answer"
            ),
        }
    }
}

impl DatastoreConfig {
    /// Validate against the deployment's own TTLs.
    ///
    /// Returns EVERY problem rather than the first: an operator fixing a
    /// configuration wants the whole list, not three restarts.
    pub fn validate(
        &self,
        has_postgres_catalog: bool,
        session_idle_ttl: Duration,
        runbook_ttl: Duration,
    ) -> Vec<ConfigError> {
        let mut errors = Vec::new();
        let serving = matches!(self.mode, RetrievalMode::Datastore);
        let needs_catalog = !matches!(self.mode, RetrievalMode::Postgres);

        if needs_catalog && !has_postgres_catalog {
            errors.push(ConfigError::NoCatalog(self.mode));
        }
        // The cache, pin-horizon and retention checks apply only where a
        // datastore is in play. `postgres` mode is the rollback path, and a
        // rollback that can refuse to start is not a rollback: a stray
        // `MUNARIUM_DATASTORE_RETIRED_RETENTION` left behind from a datastore
        // deployment must not keep a PostgreSQL-mode replica down.
        if needs_catalog {
            if self.l1_low_watermark_bytes >= self.l1_high_watermark_bytes {
                errors.push(ConfigError::InvertedWatermarks {
                    high: self.l1_high_watermark_bytes,
                    low: self.l1_low_watermark_bytes,
                });
            }

            let derived =
                PinHorizon::derive(session_idle_ttl, runbook_ttl, DEFAULT_RECOVERY_MARGIN);
            if self.pin_horizon.as_duration() < derived.as_duration()
                && !self.allow_short_pin_horizon
            {
                errors.push(ConfigError::ShortPinHorizon {
                    configured: self.pin_horizon.as_duration(),
                    derived: derived.as_duration(),
                });
            }
            // Checked against the CONFIGURED horizon, not the derived one: if
            // an operator deliberately shortened the horizon, retention
            // follows that decision rather than a value nobody chose.
            if self.retired_retention < self.pin_horizon.as_duration() {
                errors.push(ConfigError::RetentionBelowPinHorizon {
                    retention: self.retired_retention,
                    horizon: self.pin_horizon.as_duration(),
                });
            }
        }
        if serving {
            if self.local_root.is_none() {
                errors.push(ConfigError::NoLocalRoot(self.mode));
            }
            if self.compiled_engines.is_empty() {
                errors.push(ConfigError::NoCompatibleEngine);
            }
        }
        errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: Duration = Duration::from_secs(3600);

    fn ok_config() -> DatastoreConfig {
        DatastoreConfig {
            mode: RetrievalMode::Datastore,
            local_root: Some("/var/lib/munarium/indexes".into()),
            l1_high_watermark_bytes: 8 * 1024 * 1024 * 1024,
            l1_low_watermark_bytes: 6 * 1024 * 1024 * 1024,
            pin_horizon: PinHorizon::derive(HOUR, HOUR * 2, DEFAULT_RECOVERY_MARGIN),
            retired_retention: HOUR * 24,
            allow_short_pin_horizon: false,
            supported_formats: (1, 1),
            compiled_engines: vec!["tantivy".into()],
        }
    }

    #[test]
    fn a_sound_configuration_passes() {
        assert!(ok_config().validate(true, HOUR, HOUR * 2).is_empty());
    }

    /// The derivation is the longest TTL plus the margin, not the sum of the
    /// TTLs: a pin comes from a session OR a runbook, never both at once.
    #[test]
    fn the_horizon_derives_from_the_longest_ttl_plus_a_margin() {
        let h = PinHorizon::derive(HOUR, HOUR * 3, DEFAULT_RECOVERY_MARGIN);
        assert_eq!(h.as_duration(), HOUR * 3 + DEFAULT_RECOVERY_MARGIN);
        // Order does not matter.
        assert_eq!(
            PinHorizon::derive(HOUR * 3, HOUR, DEFAULT_RECOVERY_MARGIN),
            h
        );
    }

    #[test]
    fn a_short_pin_horizon_is_refused_and_the_message_says_why() {
        let mut c = ok_config();
        c.pin_horizon = PinHorizon(Duration::from_secs(60));
        c.retired_retention = Duration::from_secs(60);
        let errs = c.validate(true, HOUR, HOUR * 2);
        let short = errs
            .iter()
            .find(|e| matches!(e, ConfigError::ShortPinHorizon { .. }))
            .expect("a short horizon must be refused");
        let msg = short.to_string();
        // The message must name the CONSEQUENCE, not just the rule -- this is
        // read by someone deciding whether to set the override.
        assert!(msg.contains("live session"), "{msg}");
        assert!(msg.contains("ALLOW_SHORT_PIN_HORIZON"), "{msg}");
    }

    /// The override exists so a deliberate choice is possible; it must actually
    /// work, or an operator will find a worse way around the check.
    #[test]
    fn the_override_permits_a_short_horizon() {
        let mut c = ok_config();
        c.pin_horizon = PinHorizon(Duration::from_secs(60));
        c.retired_retention = Duration::from_secs(60);
        c.allow_short_pin_horizon = true;
        let errs = c.validate(true, HOUR, HOUR * 2);
        assert!(
            !errs
                .iter()
                .any(|e| matches!(e, ConfigError::ShortPinHorizon { .. })),
            "{errs:?}"
        );
    }

    /// Retention is checked against the CONFIGURED horizon: an operator who
    /// deliberately shortened it should not then be told off by a value nobody
    /// chose.
    #[test]
    fn retention_below_the_pin_horizon_is_refused() {
        let mut c = ok_config();
        c.retired_retention = Duration::from_secs(60);
        let errs = c.validate(true, HOUR, HOUR * 2);
        let e = errs
            .iter()
            .find(|e| matches!(e, ConfigError::RetentionBelowPinHorizon { .. }))
            .expect("refused");
        assert!(e.to_string().contains("rehydrate"), "{e}");
    }

    #[test]
    fn every_non_postgres_mode_needs_the_catalog() {
        for mode in [
            RetrievalMode::Mirror,
            RetrievalMode::Shadow,
            RetrievalMode::Datastore,
        ] {
            let mut c = ok_config();
            c.mode = mode;
            let errs = c.validate(false, HOUR, HOUR * 2);
            assert!(
                errs.iter().any(|e| matches!(e, ConfigError::NoCatalog(_))),
                "{mode:?} should need the catalog"
            );
        }
        // postgres mode does not.
        let mut c = ok_config();
        c.mode = RetrievalMode::Postgres;
        c.local_root = None;
        c.compiled_engines.clear();
        let errs = c.validate(false, HOUR, HOUR * 2);
        assert!(!errs.iter().any(|e| matches!(e, ConfigError::NoCatalog(_))));
    }

    /// postgres mode is the rollback path and must never be blocked by
    /// datastore-only settings left behind from a datastore deployment:
    /// inverted watermarks, a short pin horizon or a retention below it are
    /// all about artifacts that postgres mode never touches.
    #[test]
    fn postgres_mode_ignores_the_datastore_only_checks() {
        let mut c = ok_config();
        c.mode = RetrievalMode::Postgres;
        c.l1_low_watermark_bytes = c.l1_high_watermark_bytes;
        c.pin_horizon = PinHorizon(Duration::from_secs(60));
        c.retired_retention = Duration::from_secs(1);
        let errs = c.validate(false, HOUR, HOUR * 2);
        assert!(errs.is_empty(), "{errs:?}");
    }

    /// Only SERVING mode needs somewhere to put artifacts and an engine to read
    /// them. Mirror and shadow build and compare; they do not answer from an
    /// artifact, so requiring these of them would block a perfectly good
    /// mirror deployment.
    #[test]
    fn only_serving_mode_needs_a_local_root_and_an_engine() {
        let mut c = ok_config();
        c.local_root = None;
        c.compiled_engines.clear();

        c.mode = RetrievalMode::Datastore;
        let errs = c.validate(true, HOUR, HOUR * 2);
        assert!(errs
            .iter()
            .any(|e| matches!(e, ConfigError::NoLocalRoot(_))));
        assert!(errs.contains(&ConfigError::NoCompatibleEngine));

        c.mode = RetrievalMode::Mirror;
        let errs = c.validate(true, HOUR, HOUR * 2);
        assert!(!errs
            .iter()
            .any(|e| matches!(e, ConfigError::NoLocalRoot(_))));
        assert!(!errs.contains(&ConfigError::NoCompatibleEngine));
    }

    #[test]
    fn inverted_watermarks_are_refused() {
        let mut c = ok_config();
        c.l1_low_watermark_bytes = c.l1_high_watermark_bytes;
        assert!(c
            .validate(true, HOUR, HOUR * 2)
            .iter()
            .any(|e| matches!(e, ConfigError::InvertedWatermarks { .. })));
    }

    /// Validation reports EVERY problem. An operator fixing a configuration
    /// wants the whole list, not three restarts to discover three faults.
    #[test]
    fn every_problem_is_reported_not_just_the_first() {
        let mut c = ok_config();
        c.mode = RetrievalMode::Datastore;
        c.local_root = None;
        c.compiled_engines.clear();
        c.l1_low_watermark_bytes = c.l1_high_watermark_bytes + 1;
        c.pin_horizon = PinHorizon(Duration::from_secs(1));
        c.retired_retention = Duration::from_secs(0);
        let errs = c.validate(false, HOUR, HOUR * 2);
        assert!(errs.len() >= 5, "expected several, got {errs:?}");
    }
}
