// SPDX-License-Identifier: Apache-2.0
//! Core domain types. These are the semantic truth; wire DTOs live in
//! munarium-api-types and convert explicitly.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type Seq = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimType {
    Fact,
    /// A value legitimately changed (status transition). Supersedes.
    Update,
    /// The earlier value was wrong. Supersedes; exempt from ledger-conflict.
    Correction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStatus {
    Accepted,
    /// Blocked by a gate — recorded, never dropped.
    Disputed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    Witnessed,
    Backfilled,
    Repaired,
    Emergent,
    CoverageRepair,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warn,
    Block,
}

/// Where a connector-originated claim came from.
///
/// A claim proposed by Munarium Matrix carries the exact row, source and
/// mapping it was derived from, so a reviewer can walk from a ledger fact back
/// to the sealed evidence without trusting the claim's text. Optional and
/// additive: model-extracted claims never carry one, and nothing in the gates
/// reads it — origin is provenance for humans and for the reconciliation
/// pipeline, never an input to acceptance.
///
/// `observed_at` is an RFC-3339 string rather than a timestamp type so the
/// kernel stays free of a clock dependency and the value survives every wire
/// (JSON, proto, JSONB) byte-for-byte.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimOrigin {
    /// `connector` today; `rollback` when a mapping supersedes its own claim.
    pub kind: String,
    pub source_id: String,
    /// `name@version` of the ClaimMapping that produced the claim.
    pub mapping_version: String,
    /// The source row's stable key — the observation's idempotency identity.
    pub row_key: String,
    /// Engine position (LSN, delta version, manifest offset), when the source
    /// exposes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_position: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<String>,
    /// The sealed evidence artifact the observation batch was sealed as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Claim {
    pub id: String,
    pub version_id: String,
    pub seq: Seq,
    pub claim_type: ClaimType,
    pub subject: String,
    pub key: String,
    pub value: String,
    pub scope_path: Option<String>,
    pub status: ClaimStatus,
    pub provenance: Provenance,
    pub supersedes_id: Option<String>,
    pub entity_id: Option<String>,
    pub evidence: Option<serde_json::Value>,
    pub confidence: Option<f64>,
    pub shape_ref: Option<String>,
    /// Connector origin. `None` for every model-extracted claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<ClaimOrigin>,
}

impl Claim {
    /// The `claim_key`: the supersession-lineage identity.
    pub fn claim_key(&self) -> String {
        format!("{}.{}", self.subject, self.key)
    }

    /// Canonical `subject.key=value` text — the shape every gate parses.
    pub fn normalized_text(&self) -> String {
        crate::ledger::normalize_claim(&self.subject, &self.key, &self.value)
    }
}

/// A locked detail: `subject.key` may never drift from `locked_value` while locked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Anchor {
    pub id: String,
    pub version_id: String,
    pub detail_key: String, // "subject.key"
    pub locked_value: String,
    pub locked_at_scope: Option<String>,
    pub status: AnchorStatus,
    pub seq: Seq,
    pub evidence: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorStatus {
    Locked,
    Released,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Promise {
    pub id: String,
    pub version_id: String,
    pub key: String, // stable coordination key
    pub kind: String,
    pub description: String,
    pub origin_scope: Option<String>,
    pub due_scope: Option<String>,
    pub status: PromiseStatus,
    pub seq: Seq,
    /// None = not fulfilled. Under a pin, `fulfilled_seq > as_of` reads back OPEN.
    pub fulfilled_seq: Option<Seq>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromiseStatus {
    Open,
    Fulfilled,
    Expired,
    Violated,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CounterTotal {
    pub key: String,
    pub total: u64,
    pub budget: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WitnessState {
    pub scope_path: String,
    pub epoch: u64,
    pub witnessed: bool,
}

/// Digest ladder rung. tier 0 = per scope, 1 = per scope-prefix group,
/// 2 = whole-lineage rollup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Digest {
    pub version_id: String,
    pub tier: u8,
    pub scope_path: String, // "" for the rollup
    pub content: String,
    pub content_hash: String,
    pub built_from_seq: Seq,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entity {
    pub id: String,
    pub version_id: String,
    pub canonical_name: String,
    pub entity_type: Option<String>,
    pub aliases: Vec<String>,
    pub seq: Seq,
    pub merged_into: Option<String>,
}

/// A deterministic-gate (or policy) finding. `rule_id` keeps the dotted
/// vocabulary (`gate.ledger-conflict`, ...).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateFinding {
    pub rule_id: String,
    pub severity: Severity,
    pub message: String,
    pub scope_path: Option<String>,
    pub detail: Option<serde_json::Value>,
}

/// One proposed claim inside a candidate unit (pre-acceptance).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposedClaim {
    pub claim_type: ClaimType,
    pub subject: String,
    pub key: String,
    pub value: String,
    pub supersedes_id: Option<String>,
}

impl ProposedClaim {
    pub fn claim_key(&self) -> String {
        format!("{}.{}", self.subject, self.key)
    }
}

/// The unit of gate evaluation: everything a model proposed for one scope,
/// plus the raw output text for the text gates.
#[derive(Debug, Clone, Default)]
pub struct Candidate {
    pub scope_path: Option<String>,
    pub text: String,
    pub claims: Vec<ProposedClaim>,      // claim_type Fact/Update
    pub corrections: Vec<ProposedClaim>, // claim_type Correction
    pub previous_texts: Vec<String>,
}

/// Point-in-time view the composer and gates read. One pin bounds everything.
#[derive(Debug, Clone, Default)]
pub struct MeshSnapshot {
    pub version_id: String,
    pub facts: Vec<Claim>,
    /// detail_key -> Anchor, later-version-wins, locked only.
    pub anchors: BTreeMap<String, Anchor>,
    pub digests: Vec<Digest>,
    pub promises: Vec<Promise>,
    pub counters: Vec<CounterTotal>,
    pub entities: Vec<Entity>,
    pub as_of_seq: Option<Seq>,
    pub as_of_date: Option<String>,
}

impl MeshSnapshot {
    pub fn max_seq(&self) -> Seq {
        self.facts.iter().map(|f| f.seq).max().unwrap_or(0)
    }
}
