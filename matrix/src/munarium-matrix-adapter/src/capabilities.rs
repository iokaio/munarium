// SPDX-License-Identifier: Apache-2.0
//! Declared capabilities.
//!
//! "Supports Postgres" is not a claim this system accepts. An adapter declares
//! which sync modes it implements, which policy strategies it can honour, what
//! kind of snapshot marker it produces and what replay level that marker
//! actually buys — and the layers above **refuse** when a request needs
//! something undeclared. The support matrix in the docs is generated from
//! these values, so a documentation claim cannot outrun the code.

use munarium_matrix_core::checkpoint::SyncMode;
use munarium_matrix_core::{Refusal, RefusalClass};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyStrategy {
    /// The source enforces per-principal policy itself (RLS, Unity Catalog).
    SourceNative,
    /// One least-privilege principal per authorization equivalence class.
    PerClassPrincipals,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Capabilities {
    pub sync_modes: Vec<SyncMode>,
    pub policy_strategies: Vec<PolicyStrategy>,
    /// Can this adapter execute a mode-B query contract at all?
    pub query_contracts: bool,
    /// Executes bounded semantic intents over metric views the source owns
    ///. Only an adapter whose engine has a semantic layer
    /// with `MEASURE()` semantics declares it.
    #[serde(default)]
    pub metric_views: bool,
    /// Executes bounded semantic intents over a native data view — one fact
    /// table, declared aggregates. Any SQL engine with a schema
    /// definition to fingerprint declares it.
    #[serde(default)]
    pub data_views: bool,
    /// This adapter answers bounded semantic intents natively, over its own
    /// semantic layer's API, rather than by compiling SQL. The name
    /// is the provider family — `dbt`, `cube` — and rides in the sealed
    /// manifest so a reader knows whose metric definitions produced a number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_provider: Option<String>,
    /// The SQL dialect its statements are parsed as, if any.
    pub dialect: Option<String>,
    /// What its snapshot marker is called, e.g. `pg_snapshot`, `delta_version`,
    /// `manifest`.
    pub snapshot_marker: Option<String>,
    /// The best replay level this source can honestly offer. `sealed_result`
    /// means "we can give you the bytes back"; `source_time_travel` means the
    /// query can be re-run against the same state.
    pub replay_level: String,
    /// Whether the engine can cancel a running statement. When false, a
    /// deadline can only abandon the call, which is worth knowing.
    pub cancellation: bool,
    /// Whether the engine enforces a row/byte limit itself rather than the
    /// adapter truncating after the fact.
    pub source_side_limits: bool,
}

impl Capabilities {
    /// A conservative baseline: read-only snapshots, nothing else.
    pub fn minimal(replay_level: &str) -> Self {
        Self {
            sync_modes: vec![SyncMode::Snapshot],
            policy_strategies: vec![PolicyStrategy::PerClassPrincipals],
            query_contracts: false,
            metric_views: false,
            data_views: false,
            semantic_provider: None,
            dialect: None,
            snapshot_marker: None,
            replay_level: replay_level.to_string(),
            cancellation: false,
            source_side_limits: false,
        }
    }

    pub fn supports_sync(&self, mode: SyncMode) -> bool {
        self.sync_modes.contains(&mode)
    }

    pub fn supports_policy(&self, strategy: PolicyStrategy) -> bool {
        self.policy_strategies.contains(&strategy)
    }

    /// Refuse — with the reason — when a sync mode is not implemented.
    pub fn require_sync(&self, mode: SyncMode) -> Result<(), Refusal> {
        if self.supports_sync(mode) {
            return Ok(());
        }
        Err(Refusal::new(
            RefusalClass::NotCovered,
            "not_covered",
            format!(
                "this adapter does not implement sync mode {mode:?}; it declares {:?}",
                self.sync_modes
            ),
        ))
    }

    pub fn require_policy(&self, strategy: PolicyStrategy) -> Result<(), Refusal> {
        if self.supports_policy(strategy) {
            return Ok(());
        }
        Err(Refusal::policy_delegation_unavailable(format!(
            "this adapter cannot honour the {strategy:?} strategy; it declares {:?}",
            self.policy_strategies
        )))
    }

    pub fn require_metric_views(&self) -> Result<(), Refusal> {
        if self.metric_views {
            return Ok(());
        }
        Err(Refusal::metric_not_covered(
            "this adapter does not execute semantic intents over metric views",
        ))
    }

    pub fn require_data_views(&self) -> Result<(), Refusal> {
        if self.data_views {
            return Ok(());
        }
        Err(Refusal::metric_not_covered(
            "this adapter does not execute semantic intents over native data views",
        ))
    }

    pub fn require_query_contracts(&self) -> Result<(), Refusal> {
        if self.query_contracts {
            return Ok(());
        }
        Err(Refusal::not_covered(
            "this adapter does not execute query contracts",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps() -> Capabilities {
        Capabilities {
            sync_modes: vec![SyncMode::Snapshot, SyncMode::Watermark],
            policy_strategies: vec![PolicyStrategy::SourceNative],
            query_contracts: true,
            metric_views: false,
            data_views: false,
            semantic_provider: None,
            dialect: Some("postgres".into()),
            snapshot_marker: Some("pg_snapshot".into()),
            replay_level: "sealed_result".into(),
            cancellation: true,
            source_side_limits: true,
        }
    }

    #[test]
    fn an_undeclared_sync_mode_is_refused_with_what_is_declared() {
        let r = caps().require_sync(SyncMode::Cdf).unwrap_err();
        assert_eq!(r.class, RefusalClass::NotCovered);
        assert!(r.message.contains("Snapshot"), "{}", r.message);
        assert!(caps().require_sync(SyncMode::Watermark).is_ok());
    }

    #[test]
    fn an_undeclared_policy_strategy_refuses_rather_than_downgrading() {
        // The dangerous alternative would be to fall back to a shared
        // principal, which is precisely the row-policy bypass G6 forbids.
        let r = caps()
            .require_policy(PolicyStrategy::PerClassPrincipals)
            .unwrap_err();
        assert_eq!(r.code, "policy_delegation_unavailable");
        assert_eq!(r.class, RefusalClass::Denied);
    }

    #[test]
    fn minimal_capabilities_promise_almost_nothing() {
        let m = Capabilities::minimal("sealed_result");
        assert!(m.require_query_contracts().is_err());
        assert!(m.require_sync(SyncMode::Watermark).is_err());
        assert!(m.require_sync(SyncMode::Snapshot).is_ok());
    }
}
