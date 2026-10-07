// SPDX-License-Identifier: Apache-2.0
//! Experimental Stage 2 accountability, independent of transport and provider layers.
//! A recorded assertion or acknowledgement never grants execution authority.
mod ledger;
pub use ledger::ActionLedger;

use crate::{platform::canonical_record, platform::record_digest, KernelError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::sync::LazyLock;

pub const PROFILE: &str = "stage2-single-cell-v1";
pub const BUNDLE: &str = "8aca66588c87107a0c7a7720c68f921dfd2bdcaecf6433728afa4b1c51420aa6";
const MAX: u64 = 9007199254740991;

fn invalid() -> KernelError {
    KernelError::InvalidInput("invalid platform action record".into())
}
fn denied() -> KernelError {
    KernelError::Forbidden("platform action record authority refused".into())
}
fn text(value: &Value) -> Result<&str> {
    value.as_str().ok_or_else(invalid)
}
fn number(value: &Value) -> Result<u64> {
    value.as_u64().filter(|n| *n <= MAX).ok_or_else(invalid)
}
fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b":._/-".contains(&b))
}
fn hash(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value.as_bytes()[7..]
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub domain: String,
    pub tenant: String,
    pub deployment: String,
    pub cell: String,
}
impl Scope {
    pub fn value(&self) -> Value {
        json!({"domain":self.domain,"tenant":self.tenant,"deployment":self.deployment,"cell":self.cell})
    }
    fn valid(&self) -> bool {
        [&self.domain, &self.tenant, &self.deployment, &self.cell]
            .into_iter()
            .all(|v| identifier(v))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamRegistration {
    pub stream_id: String,
    pub producer: String,
    pub service: String,
    pub generation: u64,
    pub kinds: BTreeSet<String>,
}

/// A separately ratified assertion about exact retained committed producer evidence.
/// It is never constructed from an append request or a caller-supplied identity.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryPermit {
    pub recorder: String,
    pub producer: String,
    pub stream_id: String,
    pub generation: u64,
    pub kind: String,
    pub event_digest: String,
    pub claim_id: String,
    pub claim_event_digest: String,
    pub cutoff: u64,
}

/// Owned by Server in the currently fenced, independently ratified authority artifact.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionPolicy {
    pub schema_version: u8,
    pub profile: String,
    pub scope: Scope,
    pub streams: Vec<StreamRegistration>,
    pub readers: BTreeSet<String>,
    pub recovery: Vec<RecoveryPermit>,
}
impl ActionPolicy {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1
            || self.profile != PROFILE
            || !self.scope.valid()
            || self.streams.is_empty()
            || self.streams.len() > 128
            || self.readers.len() > 32
            || self.readers.iter().any(|r| !identifier(r))
            || self.recovery.len() > 256
        {
            return Err(denied());
        }
        let mut ids = BTreeSet::new();
        for stream in &self.streams {
            if !identifier(&stream.stream_id)
                || !identifier(&stream.service)
                || !ids.insert(&stream.stream_id)
                || stream.generation == 0
                || stream.generation > MAX
                || stream.kinds.is_empty()
                || stream
                    .kinds
                    .iter()
                    .any(|kind| !producer_allowed(kind, &stream.producer))
            {
                return Err(denied());
            }
        }
        let mut permits = BTreeSet::new();
        for permit in &self.recovery {
            let stream = self.stream(&permit.stream_id).ok_or_else(denied)?;
            if !identifier(&permit.recorder)
                || !identifier(&permit.claim_id)
                || permit.producer != stream.producer
                || permit.generation == 0
                || permit.generation >= stream.generation
                || !stream.kinds.contains(&permit.kind)
                || !recoverable(&permit.kind)
                || !hash(&permit.event_digest)
                || !hash(&permit.claim_event_digest)
                || permit.cutoff > MAX
                || !permits.insert((&permit.recorder, &permit.event_digest))
            {
                return Err(denied());
            }
        }
        Ok(())
    }
    fn stream(&self, id: &str) -> Option<&StreamRegistration> {
        self.streams.iter().find(|s| s.stream_id == id)
    }
}

/// Receiving adapter's current verified principal, transport peer and permissions.
/// Not deserializable; a request cannot supply its own authority.
pub struct ActionAdmission {
    pub identity: crate::platform::RecorderIdentity,
    pub service: String,
    pub tenant: String,
    pub deployment: String,
    pub now: u64,
    pub can_record: bool,
    pub can_read: bool,
}

pub fn action_digest(domain: &str, value: &Value) -> Result<String> {
    record_digest(&format!("munarium:stage2:{domain}:v1"), value)
}

static SCHEMA: LazyLock<Option<jsonschema::Validator>> = LazyLock::new(|| {
    let root: Value = serde_json::from_str(include_str!(
        "../../../../contract/platform-stage2-v1/schema.json"
    ))
    .ok()?;
    jsonschema::validator_for(&root).ok()
});

/// Shape and intrinsic digest checks; current authority and stored prerequisites are separate.
pub fn validate_action_record(value: &Value) -> Result<()> {
    canonical_record(&serde_json::to_vec(value).map_err(|_| invalid())?)?;
    if !SCHEMA.as_ref().ok_or_else(invalid)?.is_valid(value) {
        return Err(invalid());
    }
    match text(&value["type"])? {
        "action-request" => {
            if value["intent_digest"] != action_digest("intent", &value["intent"])?
                || value["context_digest"] != action_digest("context", &value["context"])?
            {
                return Err(invalid());
            }
            let attachments = value["intent"]["attachments"]
                .as_array()
                .ok_or_else(invalid)?;
            let mut bytes = 0u64;
            for attachment in attachments {
                bytes = bytes
                    .checked_add(number(&attachment["bytes"])?)
                    .ok_or_else(invalid)?;
            }
            if bytes > 1048576 {
                return Err(invalid());
            }
        }
        "action-approval" => {
            let lifetime = number(&value["expires_at"])?
                .checked_sub(number(&value["issued_at"])?)
                .ok_or_else(invalid)?;
            if lifetime == 0 || lifetime > 300 || value["approver"]["kind"] != "human" {
                return Err(invalid());
            }
        }
        "activation" => {
            let profile: Value = serde_json::from_str(include_str!(
                "../../../../contract/platform-stage2-v1/profile.json"
            ))
            .map_err(|_| invalid())?;
            let artifacts = value["artifacts"].as_array().ok_or_else(invalid)?;
            let kinds: BTreeSet<_> = artifacts.iter().map(|a| a["kind"].as_str()).collect();
            if number(&value["successor_epoch"])? <= number(&value["prior_epoch"])?
                || value["profile_digest"] != action_digest("profile", &profile)?
                || kinds.len() != artifacts.len()
                || number(&value["expires_at"])? <= number(&value["not_before"])?
                || value["participants"] != json!(["gate", "registry", "server", "warden"])
                || value["artifact_set_digest"]
                    != action_digest("artifact-set", &json!({"artifacts":value["artifacts"]}))?
                || value["participant_set_digest"]
                    != action_digest(
                        "participants",
                        &json!({"participants":value["participants"]}),
                    )?
            {
                return Err(invalid());
            }
        }
        "accountability-event" => {
            let kind = text(&value["kind"])?;
            let activation = kind == "activation-applied";
            if value["payload"]["kind"] != kind
                || value["family"] != if activation { "activation" } else { "action" }
                || value["payload_digest"] != action_digest("event-payload", &value["payload"])?
                || (value["sequence"] == 1) != value["predecessor"].is_null()
                || !producer_allowed(kind, text(&value["producer"])?)
            {
                return Err(invalid());
            }
            if activation
                && (value["payload"]["receipt"]["participant"] != value["producer"]
                    || value["payload"]["receipt"]["phase"] != "applied")
            {
                return Err(invalid());
            }
            if matches!(kind, "outcome" | "reconciliation")
                && value["payload"]["effect_status"] != "unresolved"
                && value["payload"]["evidence"]
                    .as_array()
                    .is_none_or(Vec::is_empty)
            {
                return Err(invalid());
            }
        }
        _ => {}
    }
    Ok(())
}

fn producer_allowed(kind: &str, producer: &str) -> bool {
    match kind {
        "approval-recorded" => producer == "council",
        "grant-issued" => producer == "warden",
        "claim-created"
        | "consumption-reserved"
        | "predispatch"
        | "send-intent"
        | "outcome"
        | "reconciliation" => producer == "gate",
        "activation-applied" => matches!(producer, "gate" | "registry" | "server" | "warden"),
        _ => false,
    }
}
fn recoverable(kind: &str) -> bool {
    matches!(
        kind,
        "claim-created"
            | "grant-issued"
            | "consumption-reserved"
            | "predispatch"
            | "send-intent"
            | "outcome"
            | "reconciliation"
    )
}
fn check_scope(value: &Value, scope: &Value) -> Result<()> {
    match value {
        Value::Object(object) => {
            if object.get("scope").is_some_and(|s| s != scope) {
                return Err(denied());
            }
            for value in object.values() {
                check_scope(value, scope)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                check_scope(value, scope)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Check a Server acknowledgement against exact immutable event bytes and ledger scope.
pub fn verify_action_ack(event: &Value, acknowledgement: &Value) -> Result<()> {
    validate_action_record(event)?;
    validate_action_record(acknowledgement)?;
    if event["type"] != "accountability-event"
        || acknowledgement["type"] != "event-ack"
        || acknowledgement["scope"] != event["scope"]
        || acknowledgement["event_id"] != event["event_id"]
        || acknowledgement["event_digest"] != action_digest("accountability-event", event)?
        || acknowledgement["payload_digest"] != event["payload_digest"]
        || acknowledgement["ledger"]["scope"] != event["scope"]
    {
        return Err(invalid());
    }
    Ok(())
}
