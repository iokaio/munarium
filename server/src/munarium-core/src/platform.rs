// SPDX-License-Identifier: Apache-2.0
//! Experimental Stage 1 records over the existing append-only storage seam.
//! Authentication, tenant/store selection and reserved-ledger custody belong to Server.
//! No platform event changes a governing policy or grants execution authority.
use crate::{
    ledger::FactQuery,
    storage::{NewClaim, StorageBackend},
    KernelError, Result,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

fn invalid() -> KernelError {
    KernelError::InvalidInput("invalid platform decision record".into())
}
fn denied() -> KernelError {
    KernelError::Forbidden("platform record authority refused".into())
}

/// Exact canonical JSON with bounded decision-json-v1 values; no repair of signed bytes.
pub fn canonical_record(raw: &[u8]) -> Result<Value> {
    if raw.len() > 65536 {
        return Err(invalid());
    }
    let v: Value = serde_json::from_slice(raw).map_err(|_| invalid())?;
    fn check(v: &Value, n: usize) -> bool {
        match v {
            Value::Object(o) => {
                n < 16 && o.keys().all(|k| k.is_ascii()) && o.values().all(|v| check(v, n + 1))
            }
            Value::Array(a) => n < 16 && a.iter().all(|v| check(v, n + 1)),
            Value::Number(n) => n
                .as_i64()
                .is_some_and(|n| (-9007199254740991..=9007199254740991).contains(&n)),
            _ => true,
        }
    }
    if !v.is_object() || !check(&v, 0) || serde_json::to_vec(&v).map_err(|_| invalid())? != raw {
        return Err(invalid());
    }
    Ok(v)
}
/// Domain-separated candidate record digest.
pub fn record_digest(domain: &str, value: &Value) -> Result<String> {
    let raw = serde_json::to_vec(value).map_err(|_| invalid())?;
    canonical_record(&raw)?;
    let mut h = Sha256::new();
    h.update(domain.as_bytes());
    h.update([0]);
    h.update(raw);
    Ok(format!("sha256:{:x}", h.finalize()))
}
/// Validate the pinned foundation candidate and its cross-field semantics.
pub fn validate_record(kind: &str, v: &Value) -> Result<()> {
    canonical_record(&serde_json::to_vec(v).map_err(|_| invalid())?)?;
    let root: Value = serde_json::from_str(include_str!(
        "../../../contract/platform-stage1/foundation.schema.json"
    ))
    .map_err(|_| invalid())?;
    if root["$defs"].get(kind).is_none() {
        return Err(invalid());
    }
    let schema = json!({"$schema":"https://json-schema.org/draft/2020-12/schema","$defs":root["$defs"],"$ref":format!("#/$defs/{kind}")});
    if !jsonschema::validator_for(&schema)
        .map_err(|_| invalid())?
        .is_valid(v)
    {
        return Err(invalid());
    }
    match kind {
        "request" => {
            let total = v["attachments"]
                .as_array()
                .ok_or_else(invalid)?
                .iter()
                .try_fold(0u64, |sum, a| {
                    sum.checked_add(a["bytes"].as_u64().unwrap_or(u64::MAX))
                });
            if total.is_none_or(|n| n > 1048576) {
                return Err(invalid());
            }
        }
        "lineage" => {
            if v["trust"] == "verified"
                && v.as_object().is_none_or(|o| o.values().any(Value::is_null))
            {
                return Err(invalid());
            }
        }
        "decision" => {
            let obligations = v["obligations"].as_array().ok_or_else(invalid)?;
            if (v["outcome"] == "approval-required") != !obligations.is_empty() {
                return Err(invalid());
            }
            for o in obligations {
                if o["request_digest"] != v["request_digest"]
                    || o["policy_digest"] != v["policy_digest"]
                {
                    return Err(invalid());
                }
            }
            for lineage in v["lineage"].as_array().ok_or_else(invalid)? {
                validate_record("lineage", lineage)?;
                if lineage["tenant"] != v["tenant"]
                    || (v["outcome"] != "denied" && lineage["trust"] != "verified")
                {
                    return Err(invalid());
                }
            }
        }
        "event" => {
            let payload = &v["payload"];
            let kind = match v["kind"].as_str() {
                Some("proposal") => "request",
                Some("decision") => "decision",
                Some("refusal") => "refusal",
                _ => return Err(invalid()),
            };
            validate_record(kind, payload)?;
            let request = if kind == "request" {
                record_digest("munarium:decision-request:v1", payload)?
            } else {
                payload["request_digest"]
                    .as_str()
                    .ok_or_else(invalid)?
                    .to_owned()
            };
            if v["tenant"] != payload["tenant"]
                || v["operation_id"] != payload["operation_id"]
                || v["request_digest"] != request
                || v["payload_digest"]
                    != record_digest("munarium:decision-event-payload:v1", payload)?
                || (v["sequence"] == 1) != v["prior_event_digest"].is_null()
            {
                return Err(invalid());
            }
        }
        _ => {}
    }
    Ok(())
}

/// A receiving adapter's freshly authenticated service and exact allowed operations.
/// Never deserialize this object from a request; it is not an identity verifier.
#[derive(serde::Serialize)]
pub struct RecorderIdentity {
    /// Verified original actor, without manufacturing a human origin.
    pub origin: String,
    /// Verified current actor.
    pub actor: String,
    /// Verified nonhuman origin kind.
    pub origin_kind: String,
    /// Digest of the complete currently verified service assertion.
    pub principal_digest: String,
}
pub struct RecorderContext {
    /// Per-operation verified identity retained in the protected audit evidence.
    pub identity: RecorderIdentity,
    /// Tenant selected from current verified identity.
    pub tenant: String,
    /// Actual authenticated transport peer, matching the verified service assertion.
    pub service: String,
    /// Independently admitted event source this service may record.
    pub source: String,
    /// Exact epoch admitted by operator configuration.
    pub epoch: u64,
    /// Current independently admitted record permission.
    pub can_record: bool,
    /// Current independently admitted read permission.
    pub can_read: bool,
}

/// Tenant-scoped reserved ledger. Ordinary writers must not hold this version handle.
pub struct EventLedger<'a> {
    store: &'a dyn StorageBackend,
    tenant: &'a str,
    version: &'a str,
}
impl<'a> EventLedger<'a> {
    /// Bind an independently selected tenant store and reserved version.
    pub fn new(store: &'a dyn StorageBackend, tenant: &'a str, version: &'a str) -> Self {
        Self {
            store,
            tenant,
            version,
        }
    }
    fn authorize(&self, c: &RecorderContext, write: bool) -> Result<()> {
        if c.tenant != self.tenant
            || c.service != c.source
            || !matches!(c.identity.origin_kind.as_str(), "agent" | "service")
            || c.identity.actor.is_empty()
            || c.identity.origin.is_empty()
            || c.identity.principal_digest.len() != 71
            || !c.identity.principal_digest.starts_with("sha256:")
            || !c.identity.principal_digest.as_bytes()[7..]
                .iter()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
            || if write { !c.can_record } else { !c.can_read }
        {
            return Err(denied());
        }
        Ok(())
    }
    /// Persist an exact event with atomic expected-head retry, immutable idempotency and source sequence checks.
    pub async fn append(&self, c: &RecorderContext, raw: &[u8]) -> Result<Value> {
        self.authorize(c, true)?;
        let event = canonical_record(raw)?;
        validate_record("event", &event)?;
        if event["tenant"] != self.tenant
            || event["source_service"] != c.source
            || event["source_epoch"] != c.epoch
        {
            return Err(denied());
        }
        let event_digest = record_digest("munarium:decision-event:v1", &event)?;
        for _ in 0..16 {
            let head = self.store.head(self.version).await?;
            let prior = self
                .store
                .slice_facts(
                    self.version,
                    &FactQuery {
                        as_of_seq: Some(head),
                        ..Default::default()
                    },
                )
                .await?;
            let mut last: Option<Value> = None;
            for claim in prior
                .iter()
                .filter(|p| p.subject == "platform.decision-events")
            {
                let old: Value = serde_json::from_str(&claim.value).map_err(|_| invalid())?;
                if old["event_id"] == event["event_id"] {
                    if old != event {
                        return Err(KernelError::IdempotencyMismatch);
                    }
                    return Ok(ack(&event, &event_digest, self.version, claim.seq));
                }
                if old["operation_id"] == event["operation_id"]
                    && old["request_digest"] != event["request_digest"]
                {
                    return Err(KernelError::IdempotencyMismatch);
                }
                if old["operation_id"] == event["operation_id"]
                    && old["kind"] != "proposal"
                    && event["kind"] != "proposal"
                    && old["payload"] != event["payload"]
                {
                    return Err(KernelError::IdempotencyMismatch);
                }
                if old["source_service"] == event["source_service"]
                    && old["source_epoch"] == event["source_epoch"]
                    && last
                        .as_ref()
                        .is_none_or(|p| old["sequence"].as_u64() > p["sequence"].as_u64())
                {
                    last = Some(old);
                }
            }
            match last {
                None => {
                    if event["sequence"] != 1 {
                        return Err(invalid());
                    }
                }
                Some(last) => {
                    if event["sequence"].as_u64()
                        != last["sequence"].as_u64().and_then(|n| n.checked_add(1))
                        || event["prior_event_digest"]
                            != record_digest("munarium:decision-event:v1", &last)?
                    {
                        return Err(invalid());
                    }
                }
            }
            let id = event["event_id"].as_str().ok_or_else(invalid)?;
            let mut claim = NewClaim::fact(
                "platform.decision-events",
                id,
                std::str::from_utf8(raw).map_err(|_| invalid())?,
            );
            claim.evidence = Some(
                json!({"event_digest":event_digest,"source_service":c.source,"recorder":c.identity}),
            );
            match self
                .store
                .append_claim(self.version, claim, Some(head))
                .await
            {
                Ok(stored) => return Ok(ack(&event, &event_digest, self.version, stored.seq)),
                Err(KernelError::HeadConflict { .. }) => continue,
                Err(e) => return Err(e),
            }
        }
        Err(KernelError::Storage("platform event contention".into()))
    }
    /// Read one tenant's operation at a stable ledger position. This never submits work.
    pub async fn source_head(&self, c: &RecorderContext) -> Result<Value> {
        self.authorize(c, false)?;
        let pin = self.store.head(self.version).await?;
        let claims = self
            .store
            .slice_facts(
                self.version,
                &FactQuery {
                    as_of_seq: Some(pin),
                    ..Default::default()
                },
            )
            .await?;
        let mut last: Option<Value> = None;
        for claim in claims
            .iter()
            .filter(|p| p.subject == "platform.decision-events")
        {
            let event: Value = serde_json::from_str(&claim.value).map_err(|_| invalid())?;
            if event["tenant"] != self.tenant {
                return Err(denied());
            }
            if event["source_service"] == c.source
                && event["source_epoch"] == c.epoch
                && last
                    .as_ref()
                    .is_none_or(|prior| event["sequence"].as_u64() > prior["sequence"].as_u64())
            {
                last = Some(event);
            }
        }
        match last {
            Some(event) => Ok(
                json!({"sequence":event["sequence"],"event_digest":record_digest("munarium:decision-event:v1",&event)?}),
            ),
            None => Ok(json!({"sequence":0,"event_digest":null})),
        }
    }
    /// Read one tenant's operation at a stable ledger position. This never submits work.
    pub async fn lookup(&self, c: &RecorderContext, operation: &str) -> Result<Vec<Value>> {
        self.authorize(c, false)?;
        let pin = self.store.head(self.version).await?;
        let claims = self
            .store
            .slice_facts(
                self.version,
                &FactQuery {
                    as_of_seq: Some(pin),
                    ..Default::default()
                },
            )
            .await?;
        let mut events = Vec::new();
        for claim in claims
            .iter()
            .filter(|p| p.subject == "platform.decision-events")
        {
            let v: Value = serde_json::from_str(&claim.value).map_err(|_| invalid())?;
            if v["tenant"] != self.tenant {
                return Err(denied());
            }
            if v["operation_id"] == operation {
                events.push(v);
            }
        }
        Ok(events)
    }

    /// Retain an immutable replay artifact after its proposal/result have been recorded.
    /// Only a privileged recorder may supply resolved evidence, policy and evaluator inputs.
    /// The reserved store must be inaccessible to ordinary writers and other tenants.
    pub async fn archive_replay(&self, c: &RecorderContext, raw: &[u8]) -> Result<String> {
        self.authorize(c, true)?;
        if raw.len() > 8 * 1024 * 1024 {
            return Err(invalid());
        }
        let bundle: Value = serde_json::from_slice(raw).map_err(|_| invalid())?;
        if serde_json::to_vec(&bundle).map_err(|_| invalid())? != raw {
            return Err(invalid());
        }
        let request = &bundle["request"];
        validate_record("request", request)?;
        let result = &bundle["decision"];
        validate_record(
            if result.get("outcome").is_some() {
                "decision"
            } else {
                "refusal"
            },
            result,
        )?;
        let operation = request["operation_id"].as_str().ok_or_else(invalid)?;
        let request_digest = record_digest("munarium:decision-request:v1", request)?;
        if request["tenant"] != self.tenant
            || result["tenant"] != self.tenant
            || result["operation_id"] != operation
            || result["request_digest"] != request_digest
        {
            return Err(denied());
        }
        let mut h = Sha256::new();
        h.update(b"munarium:decision-replay:v1\0");
        h.update(raw);
        let digest = format!("sha256:{:x}", h.finalize());
        for _ in 0..16 {
            let head = self.store.head(self.version).await?;
            let facts = self
                .store
                .slice_facts(
                    self.version,
                    &FactQuery {
                        as_of_seq: Some(head),
                        ..Default::default()
                    },
                )
                .await?;
            let mut proposal = false;
            let mut decision = false;
            for fact in facts {
                if fact.subject == "platform.decision-replays" && fact.key == operation {
                    if fact.value.as_bytes() != raw {
                        return Err(KernelError::IdempotencyMismatch);
                    }
                    return Ok(digest);
                }
                if fact.subject == "platform.decision-events" {
                    let event: Value = serde_json::from_str(&fact.value).map_err(|_| invalid())?;
                    if event["operation_id"] == operation {
                        if event["request_digest"] != request_digest {
                            return Err(KernelError::IdempotencyMismatch);
                        }
                        proposal |= event["kind"] == "proposal" && event["payload"] == *request;
                        decision |= event["payload"] == *result;
                    }
                }
            }
            if !proposal || !decision {
                return Err(invalid());
            }
            let mut claim = NewClaim::fact(
                "platform.decision-replays",
                operation,
                std::str::from_utf8(raw).map_err(|_| invalid())?,
            );
            claim.evidence = Some(
                json!({"replay_digest":digest,"request_digest":request_digest,"recorder":c.identity}),
            );
            match self
                .store
                .append_claim(self.version, claim, Some(head))
                .await
            {
                Ok(_) => return Ok(digest),
                Err(KernelError::HeadConflict { .. }) => continue,
                Err(e) => return Err(e),
            }
        }
        Err(KernelError::Storage("platform replay contention".into()))
    }

    /// Authenticated lookup of exact archived bytes and their checked digest.
    pub async fn replay(
        &self,
        c: &RecorderContext,
        operation: &str,
    ) -> Result<Option<(Vec<u8>, String)>> {
        self.authorize(c, false)?;
        let head = self.store.head(self.version).await?;
        let facts = self
            .store
            .slice_facts(
                self.version,
                &FactQuery {
                    as_of_seq: Some(head),
                    ..Default::default()
                },
            )
            .await?;
        let mut found = None;
        for fact in facts {
            if fact.subject == "platform.decision-replays" && fact.key == operation {
                if found.is_some() {
                    return Err(invalid());
                }
                let v: Value = serde_json::from_str(&fact.value).map_err(|_| invalid())?;
                if v["request"]["tenant"] != self.tenant {
                    return Err(denied());
                }
                let raw = fact.value.into_bytes();
                let mut h = Sha256::new();
                h.update(b"munarium:decision-replay:v1\0");
                h.update(&raw);
                let digest = format!("sha256:{:x}", h.finalize());
                if fact
                    .evidence
                    .as_ref()
                    .is_none_or(|e| e["replay_digest"] != digest)
                {
                    return Err(invalid());
                }
                found = Some((raw, digest));
            }
        }
        Ok(found)
    }
}
fn ack(event: &Value, digest: &str, ledger: &str, position: u64) -> Value {
    json!({"schema_version":1,"tenant":event["tenant"],"event_id":event["event_id"],"payload_digest":event["payload_digest"],
        "event_digest":digest,"ledger_id":ledger,"ledger_position":position})
}

/// Check an acknowledgement rather than treating transport success as recording evidence.
pub fn verify_ack(event: &Value, ack: &Value) -> Result<()> {
    validate_record("event", event)?;
    validate_record("ack", ack)?;
    if ["tenant", "event_id", "payload_digest"]
        .iter()
        .any(|f| event[*f] != ack[*f])
        || ack["event_digest"] != record_digest("munarium:decision-event:v1", event)?
    {
        return Err(invalid());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_vendor_files_match_the_exported_pins() {
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contract/platform-stage1");
        let lock: Value =
            serde_json::from_slice(&std::fs::read(root.join("vendor-lock.json")).unwrap()).unwrap();
        for (name, pin) in lock["files"].as_object().unwrap() {
            let raw = std::fs::read_to_string(root.join(name))
                .unwrap()
                .replace("\r\n", "\n");
            assert_eq!(
                format!("{:x}", Sha256::digest(raw.as_bytes())),
                pin["sha256"]
            );
        }
    }
}
