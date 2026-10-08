// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{
    ledger::FactQuery,
    storage::{NewClaim, StorageBackend},
};
mod activation;
pub use activation::{ActivationEnrollment, ActivationEvidence};

const ARTIFACTS: &str = "platform.action-artifacts-v1";
const EVENTS: &str = "platform.action-events-v1";

struct Stored {
    value: Value,
    seq: u64,
    received_at: u64,
}
pub struct ActionLedger<'a> {
    store: &'a dyn StorageBackend,
    version: &'a str,
    policy: &'a ActionPolicy,
}
impl<'a> ActionLedger<'a> {
    pub fn new(store: &'a dyn StorageBackend, version: &'a str, policy: &'a ActionPolicy) -> Self {
        Self {
            store,
            version,
            policy,
        }
    }
    fn authorize(&self, admission: &ActionAdmission, write: bool) -> Result<()> {
        self.policy.validate()?;
        if admission.tenant != self.policy.scope.tenant
            || admission.deployment != self.policy.scope.deployment
            || admission.identity.origin_kind != "service"
            || !identifier(&admission.identity.actor)
            || !identifier(&admission.identity.origin)
            || !hash(&admission.identity.principal_digest)
            || admission.now > MAX
            || if write {
                !admission.can_record
            } else {
                !admission.can_read || !self.policy.readers.contains(&admission.service)
            }
        {
            return Err(denied());
        }
        Ok(())
    }
    async fn snapshot(&self) -> Result<(u64, Vec<Stored>, Vec<Stored>)> {
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
        let mut artifacts = Vec::new();
        let mut events = Vec::new();
        for fact in facts {
            if fact.subject != ARTIFACTS && fact.subject != EVENTS {
                continue;
            }
            let value = canonical_record(fact.value.as_bytes())?;
            check_scope(&value, &self.policy.scope.value())?;
            let stored = Stored {
                value,
                seq: fact.seq,
                received_at: number(&fact.evidence.as_ref().ok_or_else(invalid)?["received_at"])?,
            };
            if fact.subject == ARTIFACTS {
                artifacts.push(stored);
            } else {
                events.push(stored);
            }
        }
        Ok((head, artifacts, events))
    }
    fn archive_owner(&self, service: &str, kind: &str) -> bool {
        let owner = match kind {
            "action-request" | "action-decision" => "gate",
            "action-approval" | "activation" => "council",
            _ => return false,
        };
        // A registered logical producer is bound to the actual enrolled transport service.
        self.policy
            .streams
            .iter()
            .any(|s| s.producer == owner && s.service == service)
    }
    /// Archive an owning service's assertion. This is NOT an event acknowledgement.
    pub async fn archive(&self, admission: &ActionAdmission, raw: &[u8]) -> Result<Value> {
        self.authorize(admission, true)?;
        let value = canonical_record(raw)?;
        validate_action_record(&value)?;
        check_scope(&value, &self.policy.scope.value())?;
        let kind = text(&value["type"])?;
        if !self.archive_owner(&admission.service, kind) {
            return Err(denied());
        }
        let digest = action_digest(kind, &value)?;
        for _ in 0..32 {
            let (head, artifacts, _) = self.snapshot().await?;
            for old in &artifacts {
                if kind == "action-approval"
                    && old.value["type"] == kind
                    && old.value["approval"] == value["approval"]
                    && old.value != value
                {
                    return Err(KernelError::IdempotencyMismatch);
                }
                if kind == "action-request"
                    && old.value["type"] == kind
                    && old.value["operation"] == value["operation"]
                    && old.value["intent_digest"] != value["intent_digest"]
                {
                    return Err(KernelError::IdempotencyMismatch);
                }
                if artifact_key(&old.value)? == artifact_key(&value)? {
                    if old.value != value {
                        return Err(KernelError::IdempotencyMismatch);
                    }
                    return Ok(
                        json!({"digest":digest,"ledger_position":old.seq,"record_type":kind}),
                    );
                }
            }
            validate_archive(&value, &artifacts)?;
            let mut claim = NewClaim::fact(
                ARTIFACTS,
                &digest,
                std::str::from_utf8(raw).map_err(|_| invalid())?,
            );
            claim.evidence = Some(
                json!({"artifact_digest":digest,"received_at":admission.now,"recorder":admission.identity,"service":admission.service}),
            );
            match self
                .store
                .append_claim(self.version, claim, Some(head))
                .await
            {
                Ok(stored) => {
                    return Ok(
                        json!({"digest":digest,"ledger_position":stored.seq,"record_type":kind}),
                    )
                }
                Err(KernelError::HeadConflict { .. }) => continue,
                Err(error) => return Err(error),
            }
        }
        Err(KernelError::Storage(
            "platform action archive contention".into(),
        ))
    }
    fn event_admission<'b>(
        &'b self,
        admission: &ActionAdmission,
        event: &Value,
        digest: &str,
    ) -> Result<Option<&'b RecoveryPermit>> {
        let stream = self
            .policy
            .stream(text(&event["stream"]["id"])?)
            .ok_or_else(denied)?;
        let generation = number(&event["source_generation"])?;
        if event["producer"] != stream.producer || !stream.kinds.contains(text(&event["kind"])?) {
            return Err(denied());
        }
        if generation == stream.generation {
            if admission.service != stream.service {
                return Err(denied());
            }
            return Ok(None);
        }
        let permit = self
            .policy
            .recovery
            .iter()
            .find(|p| {
                p.recorder == admission.service
                    && p.event_digest == digest
                    && p.stream_id == stream.stream_id
                    && p.generation == generation
                    && p.producer == stream.producer
                    && event["kind"] == p.kind
                    && number(&event["occurred_at"]).is_ok_and(|n| n <= p.cutoff)
                    && event["payload"]["claim"]["id"] == p.claim_id
            })
            .ok_or_else(denied)?;
        Ok(Some(permit))
    }
    fn acknowledgement(&self, event: &Value, position: u64, received_at: u64) -> Result<Value> {
        Ok(
            json!({"schema_version":1,"type":"event-ack","profile":PROFILE,
            "scope":self.policy.scope.value(),"event_id":event["event_id"],
            "event_digest":action_digest("accountability-event",event)?,"payload_digest":event["payload_digest"],
            "ledger":{"scope":self.policy.scope.value(),"kind":"ledger","id":self.version},
            "position":position,"received_at":received_at}),
        )
    }
    /// Commit a validated event through the protected store's expected-head transaction.
    pub async fn append(&self, admission: &ActionAdmission, raw: &[u8]) -> Result<Value> {
        self.authorize(admission, true)?;
        let event = canonical_record(raw)?;
        validate_action_record(&event)?;
        if event["type"] != "accountability-event" {
            return Err(invalid());
        }
        check_scope(&event, &self.policy.scope.value())?;
        let digest = action_digest("accountability-event", &event)?;
        let recovery = self.event_admission(admission, &event, &digest)?;
        for _ in 0..32 {
            let (head, artifacts, events) = self.snapshot().await?;
            let mut last: Option<&Value> = None;
            for old in &events {
                if old.value["event_id"] == event["event_id"] {
                    if old.value != event {
                        return Err(KernelError::IdempotencyMismatch);
                    }
                    return self.acknowledgement(&event, old.seq, old.received_at);
                }
                if old.value["stream"] == event["stream"]
                    && old.value["source_generation"] == event["source_generation"]
                    && last.is_none_or(|v| old.value["sequence"].as_u64() > v["sequence"].as_u64())
                {
                    last = Some(&old.value);
                }
                if event["family"] == "action" && old.value["family"] == "action" {
                    let (a, b) = (&old.value["payload"], &event["payload"]);
                    if a["attempt"] == b["attempt"]
                        && (a["request_digest"] != b["request_digest"]
                            || a["context_digest"] != b["context_digest"]
                            || a["operation"] != b["operation"])
                    {
                        return Err(KernelError::IdempotencyMismatch);
                    }
                    if a.get("claim").is_some()
                        && a["claim"] == b["claim"]
                        && (a["attempt"] != b["attempt"] || a["operation"] != b["operation"])
                    {
                        return Err(KernelError::IdempotencyMismatch);
                    }
                    if a["attempt"] == b["attempt"]
                        && a["kind"] == b["kind"]
                        && a != b
                        && matches!(
                            text(&b["kind"])?,
                            "claim-created"
                                | "grant-issued"
                                | "consumption-reserved"
                                | "predispatch"
                                | "send-intent"
                                | "outcome"
                        )
                    {
                        return Err(KernelError::IdempotencyMismatch);
                    }
                }
            }
            match last {
                None if event["sequence"] != 1 || !event["predecessor"].is_null() => {
                    return Err(invalid())
                }
                Some(last)
                    if number(&event["sequence"])? != number(&last["sequence"])? + 1
                        || event["predecessor"] != action_digest("accountability-event", last)? =>
                {
                    return Err(invalid())
                }
                _ => {}
            }
            self.prerequisites(&event, &artifacts, &events)?;
            if let Some(permit) = recovery {
                if event["kind"] == "claim-created" {
                    if permit.claim_event_digest != digest {
                        return Err(denied());
                    }
                } else if !events.iter().any(|e| {
                    e.value["kind"] == "claim-created"
                        && action_digest("accountability-event", &e.value)
                            .is_ok_and(|d| d == permit.claim_event_digest)
                        && same_attempt(&e.value["payload"], &event["payload"])
                }) {
                    return Err(denied());
                }
            }
            let mut claim = NewClaim::fact(
                EVENTS,
                text(&event["event_id"])?,
                std::str::from_utf8(raw).map_err(|_| invalid())?,
            );
            claim.evidence = Some(
                json!({"event_digest":digest,"received_at":admission.now,"recorder":admission.identity,
                "recorder_service":admission.service,"original_producer":event["producer"],"historical":recovery.is_some()}),
            );
            match self
                .store
                .append_claim(self.version, claim, Some(head))
                .await
            {
                Ok(stored) => return self.acknowledgement(&event, stored.seq, admission.now),
                Err(KernelError::HeadConflict { .. }) => continue,
                Err(error) => return Err(error),
            }
        }
        Err(KernelError::Storage(
            "platform action event contention".into(),
        ))
    }
    fn prerequisites(&self, event: &Value, artifacts: &[Stored], events: &[Stored]) -> Result<()> {
        let payload = &event["payload"];
        if event["family"] == "activation" {
            let receipt = &payload["receipt"];
            let transition = artifacts
                .iter()
                .find(|a| {
                    a.value["type"] == "activation"
                        && a.value["transition"] == receipt["transition"]
                })
                .ok_or_else(invalid)?;
            if receipt["transition_digest"] != action_digest("activation", &transition.value)?
                || [
                    "prior_epoch",
                    "successor_epoch",
                    "artifact_set_digest",
                    "participant_set_digest",
                ]
                .iter()
                .any(|k| receipt[k] != transition.value[k])
            {
                return Err(invalid());
            }
            return Ok(());
        }
        let request = request_for(payload, artifacts)?;
        if payload["activation_epoch"] != request["context"]["activation"]["revision"]
            || payload["recovery_epoch"] != request["context"]["recovery"]["revision"]
        {
            return Err(invalid());
        }
        let find = |kind: &str| {
            events
                .iter()
                .find(|e| e.value["kind"] == kind && same_attempt(&e.value["payload"], payload))
        };
        let prior = |kind: &str| find(kind).ok_or_else(invalid);
        let kind = text(&event["kind"])?;
        if kind == "approval-recorded" {
            let approval = artifact_for("action-approval", payload, artifacts)?;
            if approval["approval"] != payload["approval"]
                || payload["approval_digest"] != action_digest("action-approval", approval)?
            {
                return Err(invalid());
            }
            return Ok(());
        }
        if kind == "claim-created" {
            prior("approval-recorded")?;
            if payload["grant_binding_digest"] != payload["request_digest"] {
                return Err(invalid());
            }
            return Ok(());
        }
        let claim = &prior("claim-created")?.value["payload"];
        if payload["claim"] != claim["claim"] {
            return Err(invalid());
        }
        if kind == "grant-issued" {
            return Ok(());
        }
        if payload.get("grant").is_some()
            && payload["grant"] != prior("grant-issued")?.value["payload"]["grant"]
        {
            return Err(invalid());
        }
        match kind {
            "consumption-reserved" => {
                let reservations = payload["reservations"].as_array().ok_or_else(invalid)?;
                let mut ids = BTreeSet::new();
                for reservation in reservations {
                    if !ids.insert(text(&reservation["reservation"]["id"])?)
                        || reservation["target"] != request["intent"]["target"]
                        || reservation["root"] != request["intent"]["task"]["root"]
                        || number(&reservation["window_start"])? % 3600 != 0
                    {
                        return Err(invalid());
                    }
                }
            }
            "predispatch" => {
                if payload["consumption_event_digest"]
                    != action_digest(
                        "accountability-event",
                        &prior("consumption-reserved")?.value,
                    )?
                {
                    return Err(invalid());
                }
            }
            "send-intent" => {
                let predispatch = prior("predispatch")?;
                let ack = self.acknowledgement(
                    &predispatch.value,
                    predispatch.seq,
                    predispatch.received_at,
                )?;
                let consumed = &prior("consumption-reserved")?.value["payload"];
                if payload["predispatch_ack_digest"] != action_digest("event-ack", &ack)?
                    || payload["worker"] != consumed["worker"]
                    || payload["worker_fence"] != consumed["worker_fence"]
                    || payload["target"] != request["intent"]["target"]
                    || payload["effect_key"] != request["operation"]["id"]
                {
                    return Err(invalid());
                }
            }
            "outcome" => {
                let sent = &prior("send-intent")?.value["payload"];
                if payload["invocation"] != sent["invocation"]
                    || payload["effect_key"] != sent["effect_key"]
                {
                    return Err(invalid());
                }
            }
            "reconciliation" => {
                let prior = events
                    .iter()
                    .find(|e| {
                        matches!(e.value["kind"].as_str(), Some("outcome" | "reconciliation"))
                            && same_attempt(&e.value["payload"], payload)
                            && action_digest("accountability-event", &e.value)
                                .is_ok_and(|d| payload["prior_outcome_digest"] == d)
                    })
                    .ok_or_else(invalid)?;
                if payload["effect_key"] != prior.value["payload"]["effect_key"] {
                    return Err(invalid());
                }
            }
            _ => return Err(invalid()),
        }
        Ok(())
    }
    pub async fn lookup(&self, admission: &ActionAdmission, operation: &str) -> Result<Value> {
        self.authorize(admission, false)?;
        if !identifier(operation) {
            return Err(invalid());
        }
        let (pin, artifacts, events) = self.snapshot().await?;
        Ok(json!({"ledger_position":pin,
            "artifacts":artifacts.into_iter().filter(|a| a.value["operation"]["id"] == operation).map(|a|a.value).collect::<Vec<_>>(),
            "events":events.into_iter().filter(|e|e.value["family"] == "action" && e.value["payload"]["operation"]["id"] == operation).map(|e|e.value).collect::<Vec<_>>()}))
    }
    pub async fn transition(&self, admission: &ActionAdmission, transition: &str) -> Result<Value> {
        self.authorize(admission, false)?;
        if !identifier(transition) {
            return Err(invalid());
        }
        let (pin, artifacts, events) = self.snapshot().await?;
        Ok(json!({"ledger_position":pin,
            "artifacts":artifacts.into_iter().filter(|a| a.value["type"] == "activation"
                && a.value["transition"]["id"] == transition).map(|a|a.value).collect::<Vec<_>>(),
            "events":events.into_iter().filter(|e|e.value["family"] == "activation"
                && e.value["payload"]["receipt"]["transition"]["id"] == transition).map(|e|e.value).collect::<Vec<_>>()}))
    }
    pub async fn source_head(
        &self,
        admission: &ActionAdmission,
        stream: &str,
        generation: u64,
    ) -> Result<Value> {
        self.authorize(admission, false)?;
        let registration = self.policy.stream(stream).ok_or_else(denied)?;
        if generation == 0 || generation > registration.generation {
            return Err(denied());
        }
        let (_, _, events) = self.snapshot().await?;
        let last = events
            .iter()
            .filter(|e| {
                e.value["stream"]["id"] == stream && e.value["source_generation"] == generation
            })
            .max_by_key(|e| e.value["sequence"].as_u64());
        match last {
            Some(event) => Ok(
                json!({"sequence":event.value["sequence"],"event_digest":action_digest("accountability-event",&event.value)?}),
            ),
            None => Ok(json!({"sequence":0,"event_digest":null})),
        }
    }
}

fn artifact_key(value: &Value) -> Result<(&str, &str)> {
    let kind = text(&value["type"])?;
    let id = if kind == "activation" {
        &value["transition"]["id"]
    } else {
        &value["attempt"]["id"]
    };
    Ok((kind, text(id)?))
}
fn same_attempt(a: &Value, b: &Value) -> bool {
    a["operation"] == b["operation"]
        && a["attempt"] == b["attempt"]
        && a["request_digest"] == b["request_digest"]
        && a["context_digest"] == b["context_digest"]
}
fn artifact_for<'a>(kind: &str, record: &Value, artifacts: &'a [Stored]) -> Result<&'a Value> {
    artifacts
        .iter()
        .find(|a| {
            a.value["type"] == kind
                && a.value["attempt"] == record["attempt"]
                && a.value["operation"] == record["operation"]
        })
        .map(|a| &a.value)
        .ok_or_else(invalid)
}
fn request_for<'a>(record: &Value, artifacts: &'a [Stored]) -> Result<&'a Value> {
    let request = artifact_for("action-request", record, artifacts)?;
    if record["request_digest"] != action_digest("action-request", request)?
        || record["context_digest"] != request["context_digest"]
    {
        return Err(invalid());
    }
    Ok(request)
}
fn validate_archive(value: &Value, artifacts: &[Stored]) -> Result<()> {
    match text(&value["type"])? {
        "action-request" | "activation" => {}
        "action-decision" => {
            let request = request_for(value, artifacts)?;
            let expected = json!([
                {"kind":"distinct-approval","owner":"council","policy_digest":request["context"]["policy_digest"]},
                {"kind":"mandatory-recording","owner":"server","phase":"before-send"},
                {"kind":"action-capacity","owner":"gate","target":request["intent"]["target"],"limit":2,"window_seconds":3600},
                {"kind":"credential-custody","owner":"warden","audience":request["intent"]["target"]}]);
            if value["obligations"]
                != if value["outcome"] == "denied" {
                    json!([])
                } else {
                    expected
                }
            {
                return Err(invalid());
            }
        }
        "action-approval" => {
            let request = request_for(value, artifacts)?;
            let decision = artifact_for("action-decision", value, artifacts)?;
            if decision["outcome"] != "approval-required"
                || value["decision_digest"] != action_digest("action-decision", decision)?
                || value["approver"] == request["intent"]["actor"]
                || value["approver"] == request["intent"]["origin"]
            {
                return Err(invalid());
            }
        }
        _ => return Err(invalid()),
    }
    Ok(())
}
