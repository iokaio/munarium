// SPDX-License-Identifier: Apache-2.0
//! Server-owned activation state and audit custody in one protected ledger transaction.
use super::*;

const ENROLLMENT: &str = "platform.server-activation-enrollment-v1";
const APPLIED: &str = "platform.server-activation-applied-v1";

/// Current signed configuration, supplied by the authenticated adapter.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivationEnrollment {
    pub initial_epoch: u64,
    pub initial_artifact_set_digest: String,
    pub service: String,
    pub coordinator: String,
    pub stream_id: String,
}

/// Independently obtained evidence, never deserialized from an apply request.
pub struct ActivationEvidence {
    pub ratified: Value,
    pub pause: Value,
    pub gate_head: Value,
    pub registry_receipt: Value,
    pub registry_head: Value,
    pub authority_revision: String,
    pub now: u64,
}

fn encoded(value: &Value) -> Result<String> {
    let raw = serde_json::to_string(value).map_err(|_| invalid())?;
    canonical_record(raw.as_bytes())?;
    Ok(raw)
}
fn receipt(t: &Value) -> Result<Value> {
    let mut r = json!({"schema_version":1,"profile":PROFILE,"type":"activation-receipt",
        "participant":"server","phase":"applied","transition_digest":action_digest("activation",t)?});
    for k in [
        "transition",
        "prior_epoch",
        "successor_epoch",
        "artifact_set_digest",
        "participant_set_digest",
    ] {
        r[k] = t[k].clone();
    }
    validate_action_record(&r)?;
    Ok(r)
}
fn check_receipt(t: &Value, r: &Value, owner: &str, phase: &str) -> Result<()> {
    validate_action_record(r)?;
    let mut expected = receipt(t)?;
    expected["participant"] = json!(owner);
    expected["phase"] = json!(phase);
    if r != &expected {
        return Err(denied());
    }
    Ok(())
}
impl ActionLedger<'_> {
    fn activation_enrollment(&self, e: &ActivationEnrollment) -> Result<Value> {
        self.policy.validate()?;
        let stream = self.policy.stream(&e.stream_id).ok_or_else(denied)?;
        if e.initial_epoch == 0
            || e.initial_epoch > MAX
            || !hash(&e.initial_artifact_set_digest)
            || stream.producer != "server"
            || stream.service != e.service
            || !stream.kinds.contains("activation-applied")
            || !self.archive_owner(&e.coordinator, "activation")
        {
            return Err(denied());
        }
        Ok(
            json!({"scope":self.policy.scope.value(),"initial_epoch":e.initial_epoch,
            "initial_artifact_set_digest":e.initial_artifact_set_digest}),
        )
    }
    async fn activation_facts(&self, pin: u64) -> Result<Vec<crate::types::Claim>> {
        self.store
            .slice_facts(
                self.version,
                &FactQuery {
                    as_of_seq: Some(pin),
                    ..Default::default()
                },
            )
            .await
    }
    /// Pin the initial head once. Later governing configuration cannot reset history.
    pub async fn initialize_activation(&self, e: &ActivationEnrollment) -> Result<()> {
        let value = self.activation_enrollment(e)?;
        for _ in 0..32 {
            let pin = self.store.head(self.version).await?;
            for fact in self.activation_facts(pin).await? {
                if fact.subject == ENROLLMENT {
                    if canonical_record(fact.value.as_bytes())? != value {
                        return Err(KernelError::IdempotencyMismatch);
                    }
                    return Ok(());
                }
            }
            match self
                .store
                .append_claim(
                    self.version,
                    NewClaim::fact(ENROLLMENT, "initial", &encoded(&value)?),
                    Some(pin),
                )
                .await
            {
                Ok(_) => return Ok(()),
                Err(KernelError::HeadConflict { .. }) => continue,
                Err(e) => return Err(e),
            }
        }
        Err(KernelError::Storage(
            "activation enrollment contention".into(),
        ))
    }
    /// Current local participant state; historical audit appends cannot change it.
    pub async fn activation_head(&self, e: &ActivationEnrollment) -> Result<Value> {
        self.initialize_activation(e).await?;
        let pin = self.store.head(self.version).await?;
        let facts = self.activation_facts(pin).await?;
        let current = facts
            .iter()
            .filter(|f| f.subject == APPLIED)
            .max_by_key(|f| f.seq)
            .map(|f| canonical_record(f.value.as_bytes()))
            .transpose()?;
        Ok(
            json!({"scope":self.policy.scope.value(),"participant":"server",
            "epoch":current.as_ref().map(|v|v["receipt"]["successor_epoch"].clone()).unwrap_or(json!(e.initial_epoch)),
            "artifact_set_digest":current.as_ref().map(|v|v["receipt"]["artifact_set_digest"].clone()).unwrap_or(json!(e.initial_artifact_set_digest)),
            "cell_resumed":false,"execution_enabled":false}),
        )
    }
    /// Return the immutable receipt, without granting authority.
    pub async fn activation_lookup(&self, e: &ActivationEnrollment, id: &str) -> Result<Value> {
        self.initialize_activation(e).await?;
        let pin = self.store.head(self.version).await?;
        let facts = self.activation_facts(pin).await?;
        let state = facts
            .iter()
            .filter(|f| f.subject == APPLIED)
            .map(|f| canonical_record(f.value.as_bytes()))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .find(|v| v["receipt"]["transition"]["id"] == id)
            .ok_or_else(denied)?;
        Ok(state["receipt"].clone())
    }
    /// Commit only adapter-verified, currently ratified and independently paused inputs.
    /// The adapter holds the root authority fence through this operation.
    pub async fn apply_activation(
        &self,
        e: &ActivationEnrollment,
        t: &Value,
        proof: &ActivationEvidence,
    ) -> Result<Value> {
        self.initialize_activation(e).await?;
        validate_action_record(t)?;
        check_scope(t, &self.policy.scope.value())?;
        if t["type"] != "activation"
            || proof.now > MAX
            || proof.ratified["ratified"] != true
            || proof.ratified["transition"] != *t
            || proof.ratified["transition_digest"] != action_digest("activation", t)?
            || proof.now < number(&t["not_before"])?.saturating_add(2)
            || proof.now.saturating_add(2) >= number(&t["expires_at"])?
        {
            return Err(denied());
        }
        check_receipt(t, &proof.pause, "gate", "paused")?;
        check_receipt(t, &proof.registry_receipt, "registry", "applied")?;
        let h = &proof.gate_head;
        if h["scope"] != self.policy.scope.value()
            || h["participant"] != "gate"
            || h["paused"] != true
            || h["transition_id"] != t["transition"]["id"]
            || !((h["epoch"] == t["prior_epoch"]
                && h["artifact_set_digest"] == t["prior_artifact_set_digest"])
                || (h["epoch"] == t["successor_epoch"]
                    && h["artifact_set_digest"] == t["artifact_set_digest"]))
        {
            return Err(denied());
        }
        let h = &proof.registry_head;
        if h["scope"] != self.policy.scope.value()
            || h["participant"] != "registry"
            || h["epoch"] != t["successor_epoch"]
            || h["artifact_set_digest"] != t["artifact_set_digest"]
        {
            return Err(denied());
        }
        let receipt = receipt(t)?;
        let transition_digest = action_digest("activation", t)?;
        let stream = self.policy.stream(&e.stream_id).ok_or_else(denied)?;
        for _ in 0..32 {
            let (pin, artifacts, events) = self.snapshot().await?;
            let facts = self.activation_facts(pin).await?;
            let mut current = None;
            for fact in facts.iter().filter(|f| f.subject == APPLIED) {
                let old = canonical_record(fact.value.as_bytes())?;
                if old["receipt"]["transition"] == t["transition"] {
                    if old["receipt"] != receipt {
                        return Err(KernelError::IdempotencyMismatch);
                    }
                    return Ok(receipt);
                }
                if current.as_ref().is_none_or(|(seq, _)| *seq < fact.seq) {
                    current = Some((fact.seq, old));
                }
            }
            let (epoch, digest) = match current {
                Some((_, v)) => (
                    number(&v["receipt"]["successor_epoch"])?,
                    text(&v["receipt"]["artifact_set_digest"])?.to_owned(),
                ),
                None => (e.initial_epoch, e.initial_artifact_set_digest.clone()),
            };
            if t["prior_epoch"] != epoch || t["prior_artifact_set_digest"] != digest {
                return Err(KernelError::IdempotencyMismatch);
            }
            let last = events
                .iter()
                .filter(|v| {
                    v.value["stream"]["id"] == e.stream_id
                        && v.value["source_generation"] == stream.generation
                })
                .max_by_key(|v| v.value["sequence"].as_u64());
            let payload = json!({"kind":"activation-applied","receipt":receipt});
            let event = json!({"schema_version":1,"type":"accountability-event","profile":PROFILE,
                "scope":self.policy.scope.value(),"event_id":format!("server-{}",&transition_digest[7..]),
                "stream":{"scope":self.policy.scope.value(),"kind":"stream","id":e.stream_id},
                "source_generation":stream.generation,"sequence":last.map(|l|number(&l.value["sequence"])).transpose()?.unwrap_or(0)+1,
                "predecessor":last.map(|l|action_digest("accountability-event",&l.value)).transpose()?,
                "producer":"server","family":"activation","kind":"activation-applied","occurred_at":proof.now,
                "clock":"bounded-utc-2s","causal_parents":[],
                "payload":payload,"payload_digest":action_digest("event-payload",&payload)?});
            validate_action_record(&event)?;
            if events
                .iter()
                .any(|v| v.value["event_id"] == event["event_id"])
            {
                return Err(KernelError::IdempotencyMismatch);
            }
            let old = artifacts.iter().find(|a| {
                a.value["type"] == "activation" && a.value["transition"] == t["transition"]
            });
            if old.is_some_and(|a| a.value != *t) {
                return Err(KernelError::IdempotencyMismatch);
            }
            let provenance = json!({"received_at":proof.now,"authority_revision":proof.authority_revision,
                "coordinator":e.coordinator,"recorder_service":e.service,"source":"server-activation"});
            let mut claims = Vec::new();
            if old.is_none() {
                let mut archive = NewClaim::fact(ARTIFACTS, &transition_digest, &encoded(t)?);
                archive.evidence = Some(provenance.clone());
                claims.push(archive);
            }
            let mut audit = NewClaim::fact(EVENTS, text(&event["event_id"])?, &encoded(&event)?);
            audit.evidence = Some(provenance.clone());
            claims.push(audit);
            let mut applied = NewClaim::fact(
                APPLIED,
                text(&t["transition"]["id"])?,
                &encoded(&json!({"receipt":receipt}))?,
            );
            applied.evidence = Some(provenance);
            claims.push(applied);
            match self
                .store
                .append_claims(self.version, claims, Some(pin))
                .await
            {
                Ok(_) => return Ok(receipt),
                Err(KernelError::HeadConflict { .. }) => continue,
                Err(e) => return Err(e),
            }
        }
        Err(KernelError::Storage("activation commit contention".into()))
    }
}
