// SPDX-License-Identifier: Apache-2.0
//! The connector checkpoint contract (§3.7).
//!
//! "Incremental sync is idempotent" is an acceptance *result*, not an
//! assumption, and this module is where the result is made provable: every
//! record carries an idempotency key built from `(source, version, row key,
//! event position)`, and a replayed checkpoint therefore produces the same
//! keys and creates nothing new.
//!
//! The watermark semantics are spelled out because getting them wrong is
//! silent: an inclusive watermark re-reads the boundary row forever, and an
//! exclusive one without a tie-breaker **drops** rows that share the boundary
//! timestamp. Both are tested here.

use serde::{Deserialize, Serialize};

/// How a source's incremental read advances.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncMode {
    /// An immutable export described by a manifest. The preferred mode-A path:
    /// an export IS a snapshot, so there is nothing to reconcile.
    Manifest,
    /// Read everything, every time.
    Snapshot,
    /// Read rows after a watermark column value.
    Watermark,
    /// Delta Change Data Feed.
    Cdf,
    /// Logical replication.
    Cdc,
}

impl SyncMode {
    /// The wire spelling. Deliberately the SAME string serde emits, so a mode
    /// recorded in a run row and a mode in a serialized checkpoint cannot drift
    /// apart and quietly describe different things.
    pub fn as_str(self) -> &'static str {
        match self {
            SyncMode::Manifest => "manifest",
            SyncMode::Snapshot => "snapshot",
            SyncMode::Watermark => "watermark",
            SyncMode::Cdf => "cdf",
            SyncMode::Cdc => "cdc",
        }
    }
}

/// What a delete looks like in this source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum DeleteSemantics {
    /// Rows vanish. Only a full resnapshot can detect it.
    Hard,
    /// A column marks the row deleted.
    Soft { column: String },
    /// The source cannot express deletes at all; the only correct response to
    /// a suspected delete is a resnapshot.
    Unsupported,
}

/// Inclusive or exclusive, plus the tie-breaker that makes an exclusive
/// watermark safe.
///
/// `camelCase` + `deny_unknown_fields` because this type is embedded in the
/// `DataSource` asset, and an ignored `tieBreak` would turn a correct
/// declaration into a validation error with no explanation — which is exactly
/// what happened the first time this crate was compiled without them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WatermarkSpec {
    pub column: String,
    /// `false` (exclusive) is the usual choice and REQUIRES `tie_break`.
    pub inclusive: bool,
    /// A strictly-ordered secondary column. Without it, two rows sharing a
    /// watermark value straddle the boundary and one is lost.
    #[serde(default)]
    pub tie_break: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CheckpointError {
    #[error(
        "watermark on '{0}' is exclusive but declares no tieBreak: two rows sharing a watermark \
         value would straddle the boundary and one would never be read"
    )]
    ExclusiveWithoutTieBreak(String),
    #[error("watermark column and tieBreak are both '{0}'")]
    TieBreakSameAsWatermark(String),
    #[error("sync mode {0:?} needs a watermark specification")]
    ModeNeedsWatermark(SyncMode),
    #[error("sync mode {0:?} does not use a watermark, but one was declared")]
    ModeIgnoresWatermark(SyncMode),
}

impl WatermarkSpec {
    pub fn validate(&self) -> Result<(), CheckpointError> {
        match &self.tie_break {
            None if !self.inclusive => Err(CheckpointError::ExclusiveWithoutTieBreak(
                self.column.clone(),
            )),
            Some(t) if t == &self.column => {
                Err(CheckpointError::TieBreakSameAsWatermark(t.clone()))
            }
            _ => Ok(()),
        }
    }
}

/// Validate the mode/watermark pairing at apply time.
pub fn validate_sync(
    mode: SyncMode,
    watermark: Option<&WatermarkSpec>,
) -> Result<(), CheckpointError> {
    match (mode, watermark) {
        (SyncMode::Watermark, None) => Err(CheckpointError::ModeNeedsWatermark(mode)),
        (SyncMode::Watermark, Some(w)) => w.validate(),
        (m, Some(_)) if m != SyncMode::Watermark => Err(CheckpointError::ModeIgnoresWatermark(m)),
        _ => Ok(()),
    }
}

/// Where a sync run resumes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub source_id: String,
    pub entity: String,
    /// The render or mapping version this checkpoint belongs to. A version bump
    /// invalidates the checkpoint on purpose: the same rows must be re-rendered.
    pub version: String,
    /// Watermark value reached, canon@1 text.
    pub watermark: Option<String>,
    /// Tie-break value at that watermark.
    pub tie_break: Option<String>,
    /// Engine-native position: LSN, delta version, manifest id.
    pub event_position: Option<String>,
    /// The source schema fingerprint this checkpoint was taken under. A change
    /// means drift, and drift is fail-closed.
    pub schema_fingerprint: Option<String>,
}

impl Checkpoint {
    pub fn start(source_id: &str, entity: &str, version: &str) -> Self {
        Self {
            source_id: source_id.into(),
            entity: entity.into(),
            version: version.into(),
            watermark: None,
            tie_break: None,
            event_position: None,
            schema_fingerprint: None,
        }
    }

    /// True when this checkpoint has never advanced — the resnapshot case.
    pub fn is_start(&self) -> bool {
        self.watermark.is_none() && self.event_position.is_none()
    }
}

/// The per-record idempotency key. Replaying a checkpoint reproduces these
/// exactly, which is what makes "zero new documents on replay" a test rather
/// than a hope.
pub fn idempotency_key(
    source_id: &str,
    version: &str,
    row_key: &str,
    event_position: Option<&str>,
) -> String {
    match event_position {
        Some(p) => format!("{source_id}|{version}|{row_key}|{p}"),
        None => format!("{source_id}|{version}|{row_key}|"),
    }
}

/// What to do when the source schema fingerprint moves.
///
/// The wire form is a **string**, exactly as the asset grammar spells it:
/// `refuse`, or `compat:<decision-id>`. Requiring the id inside the value is
/// what makes "a human reviewed this drift" a recorded fact rather than a
/// boolean someone flipped.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DriftPolicy {
    /// Default and the only safe default: stop and refuse.
    #[default]
    Refuse,
    /// Accept, because a human reviewed exactly this change and recorded the
    /// decision id. The id is journaled with the run.
    Compat { decision_id: String },
}

impl std::fmt::Display for DriftPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DriftPolicy::Refuse => f.write_str("refuse"),
            DriftPolicy::Compat { decision_id } => write!(f, "compat:{decision_id}"),
        }
    }
}

impl std::str::FromStr for DriftPolicy {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let s = s.trim();
        if s == "refuse" {
            return Ok(DriftPolicy::Refuse);
        }
        match s.strip_prefix("compat:") {
            Some(id) if !id.trim().is_empty() => Ok(DriftPolicy::Compat {
                decision_id: id.trim().to_string(),
            }),
            Some(_) => Err(
                "onDrift 'compat:' needs a decision id — accepting drift anonymously is how a \
                 reviewed exception becomes an unreviewed one"
                    .to_string(),
            ),
            None => Err(format!(
                "onDrift must be 'refuse' or 'compat:<decision-id>', got '{s}'"
            )),
        }
    }
}

impl Serialize for DriftPolicy {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for DriftPolicy {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exclusive_watermark_without_a_tiebreak_is_refused() {
        let w = WatermarkSpec {
            column: "updated_at".into(),
            inclusive: false,
            tie_break: None,
        };
        assert_eq!(
            w.validate(),
            Err(CheckpointError::ExclusiveWithoutTieBreak(
                "updated_at".into()
            ))
        );
    }

    #[test]
    fn an_inclusive_watermark_needs_no_tiebreak_but_may_have_one() {
        let w = WatermarkSpec {
            column: "updated_at".into(),
            inclusive: true,
            tie_break: None,
        };
        assert_eq!(w.validate(), Ok(()));
        let w = WatermarkSpec {
            column: "updated_at".into(),
            inclusive: false,
            tie_break: Some("id".into()),
        };
        assert_eq!(w.validate(), Ok(()));
    }

    #[test]
    fn a_tiebreak_equal_to_the_watermark_breaks_nothing() {
        let w = WatermarkSpec {
            column: "updated_at".into(),
            inclusive: false,
            tie_break: Some("updated_at".into()),
        };
        assert!(matches!(
            w.validate(),
            Err(CheckpointError::TieBreakSameAsWatermark(_))
        ));
    }

    #[test]
    fn mode_and_watermark_must_agree() {
        assert!(matches!(
            validate_sync(SyncMode::Watermark, None),
            Err(CheckpointError::ModeNeedsWatermark(_))
        ));
        let w = WatermarkSpec {
            column: "u".into(),
            inclusive: true,
            tie_break: None,
        };
        assert!(matches!(
            validate_sync(SyncMode::Snapshot, Some(&w)),
            Err(CheckpointError::ModeIgnoresWatermark(_))
        ));
        assert_eq!(validate_sync(SyncMode::Snapshot, None), Ok(()));
        assert_eq!(validate_sync(SyncMode::Manifest, None), Ok(()));
    }

    #[test]
    fn idempotency_keys_are_stable_and_version_scoped() {
        let a = idempotency_key("crm", "record-documents@1", "42", Some("0/1A2B"));
        let b = idempotency_key("crm", "record-documents@1", "42", Some("0/1A2B"));
        assert_eq!(a, b, "a replayed checkpoint must reproduce the key exactly");
        // A render version bump deliberately invalidates: the same row must be
        // re-rendered rather than skipped as already-present.
        let c = idempotency_key("crm", "record-documents@2", "42", Some("0/1A2B"));
        assert_ne!(a, c);
        // A different event position is a different event for the same row.
        let d = idempotency_key("crm", "record-documents@1", "42", Some("0/1A2C"));
        assert_ne!(a, d);
    }

    #[test]
    fn drift_refuses_by_default() {
        assert_eq!(DriftPolicy::default(), DriftPolicy::Refuse);
    }

    #[test]
    fn drift_policy_round_trips_through_its_string_form() {
        use std::str::FromStr;
        assert_eq!(DriftPolicy::from_str("refuse"), Ok(DriftPolicy::Refuse));
        assert_eq!(
            DriftPolicy::from_str("compat:DEC-2026-08-28-01"),
            Ok(DriftPolicy::Compat {
                decision_id: "DEC-2026-08-28-01".into()
            })
        );
        assert_eq!(
            DriftPolicy::Compat {
                decision_id: "x".into()
            }
            .to_string(),
            "compat:x"
        );
        // An anonymous exception is refused: a reviewed drift has a reviewer.
        assert!(DriftPolicy::from_str("compat:").is_err());
        assert!(DriftPolicy::from_str("ignore").is_err());
    }

    #[test]
    fn a_fresh_checkpoint_is_a_resnapshot() {
        let c = Checkpoint::start("crm", "opportunities", "record-documents@1");
        assert!(c.is_start());
    }
}

#[cfg(test)]
mod sync_mode_tests {
    use super::SyncMode;

    #[test]
    fn as_str_matches_what_serde_emits() {
        for mode in [
            SyncMode::Manifest,
            SyncMode::Snapshot,
            SyncMode::Watermark,
            SyncMode::Cdf,
            SyncMode::Cdc,
        ] {
            let json = serde_json::to_string(&mode).unwrap();
            assert_eq!(
                json.trim_matches('"'),
                mode.as_str(),
                "as_str and serde must not drift: a run row and a checkpoint                  would then describe different modes with the same name"
            );
        }
    }
}
