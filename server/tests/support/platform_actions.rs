// SPDX-License-Identifier: Apache-2.0
//! Shared behavioral assertions exercised against both real storage implementations.
use munarium_core::{platform::RecorderIdentity, platform_actions::*, storage::StorageBackend};
use serde_json::{json, Value};

pub fn records() -> Value {
    serde_json::from_str::<Value>(include_str!(
        "../../contract/platform-stage2-v1/vectors.json"
    ))
    .unwrap()["records"]
        .clone()
}
pub fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}
pub fn policy() -> ActionPolicy {
    let records = records();
    let streams = records
        .as_object()
        .unwrap()
        .values()
        .filter(|v| v["type"] == "accountability-event")
        .map(|v| StreamRegistration {
            stream_id: v["stream"]["id"].as_str().unwrap().into(),
            producer: v["producer"].as_str().unwrap().into(),
            service: format!("svc-{}", v["producer"].as_str().unwrap()),
            generation: 1,
            kinds: [v["kind"].as_str().unwrap().into()].into_iter().collect(),
        })
        .collect();
    ActionPolicy {
        schema_version: 1,
        profile: PROFILE.into(),
        scope: serde_json::from_value(records["request"]["operation"]["scope"].clone()).unwrap(),
        streams,
        readers: ["svc-gate".into()].into_iter().collect(),
        recovery: vec![],
    }
}
pub fn admission(service: &str) -> ActionAdmission {
    let scope = policy().scope;
    ActionAdmission {
        identity: RecorderIdentity {
            origin: service.into(),
            actor: service.into(),
            origin_kind: "service".into(),
            principal_digest: format!("sha256:{}", "a".repeat(64)),
        },
        service: service.into(),
        tenant: scope.tenant,
        deployment: scope.deployment,
        now: 1001,
        can_record: true,
        can_read: true,
    }
}
pub fn rehash(event: &mut Value) {
    event["payload_digest"] = json!(action_digest("event-payload", &event["payload"]).unwrap());
}
pub async fn archives(ledger: &ActionLedger<'_>) {
    let records = records();
    for (name, service) in [
        ("request", "svc-gate"),
        ("decision", "svc-gate"),
        ("approval", "svc-council"),
    ] {
        let raw = bytes(&records[name]);
        let ack = ledger.archive(&admission(service), &raw).await.unwrap();
        assert_eq!(
            ledger.archive(&admission(service), &raw).await.unwrap(),
            ack
        );
        assert!(
            ack.get("event_digest").is_none(),
            "artifact receipt is not an event ack"
        );
    }
}

/// Complete lifecycle, durable exact acknowledgements, deny edges and CAS races.
/// Returns the final event/ack so PostgreSQL can reconnect and retry independently.
pub async fn lifecycle(store: &dyn StorageBackend, version: &str) -> (Value, Value) {
    let records = records();
    let policy = policy();
    let ledger = ActionLedger::new(store, version, &policy);
    let gate = admission("svc-gate");
    assert!(ledger
        .append(&gate, &bytes(&records["claim-created"]))
        .await
        .is_err());
    assert!(ledger
        .archive(&admission("svc-council"), &bytes(&records["request"]))
        .await
        .is_err());
    assert!(ledger
        .archive(&gate, &bytes(&records["decision"]))
        .await
        .is_err());
    assert_eq!(store.head(version).await.unwrap(), 0);
    archives(&ledger).await;
    assert_eq!(store.head(version).await.unwrap(), 3);
    for name in [
        "claim-created",
        "grant-issued",
        "consumption-reserved",
        "predispatch",
        "send-intent",
        "outcome",
        "reconciliation",
    ] {
        let event = &records[name];
        let owner = admission(&format!("svc-{}", event["producer"].as_str().unwrap()));
        assert!(
            ledger.append(&owner, &bytes(event)).await.is_err(),
            "{name} lacks its prior lifecycle fact"
        );
    }
    assert_eq!(store.head(version).await.unwrap(), 3);
    let mut changed = records["request"].clone();
    changed["context"]["expires_at"] = json!(1299);
    changed["context_digest"] = json!(action_digest("context", &changed["context"]).unwrap());
    assert!(ledger.archive(&gate, &bytes(&changed)).await.is_err());
    changed = records["request"].clone();
    changed["attempt"]["id"] = json!("second-attempt");
    changed["intent"]["parameters"]["destination"] = json!("changed-intent");
    changed["intent_digest"] = json!(action_digest("intent", &changed["intent"]).unwrap());
    assert!(ledger.archive(&gate, &bytes(&changed)).await.is_err());
    let mut predispatch_ack = Value::Null;
    let mut final_pair = (Value::Null, Value::Null);
    for name in [
        "approval-recorded",
        "claim-created",
        "grant-issued",
        "consumption-reserved",
        "predispatch",
        "send-intent",
        "outcome",
        "reconciliation",
    ] {
        let mut event = records[name].clone();
        if name == "send-intent" {
            // The fixture ack is deliberately for another ledger/position.
            assert!(ledger.append(&gate, &bytes(&event)).await.is_err());
            event["payload"]["predispatch_ack_digest"] =
                json!(action_digest("event-ack", &predispatch_ack).unwrap());
            rehash(&mut event);
        }
        let service = format!("svc-{}", event["producer"].as_str().unwrap());
        let mut current = admission(&service);
        let raw = bytes(&event);
        let before = store.head(version).await.unwrap();
        current.identity.origin_kind = "agent".into();
        assert!(ledger.append(&current, &raw).await.is_err());
        current = admission("spoofed-peer");
        assert!(ledger.append(&current, &raw).await.is_err());
        current = admission(&service);
        current.tenant = "foreign".into();
        assert!(ledger.append(&current, &raw).await.is_err());
        current = admission(&service);
        let mut invalid = event.clone();
        invalid["source_generation"] = json!(2);
        assert!(ledger.append(&current, &bytes(&invalid)).await.is_err());
        invalid = event.clone();
        invalid["sequence"] = json!(2);
        invalid["predecessor"] = json!(format!("sha256:{}", "0".repeat(64)));
        assert!(ledger.append(&current, &bytes(&invalid)).await.is_err());
        assert_eq!(store.head(version).await.unwrap(), before);
        let (a, b) = tokio::join!(ledger.append(&current, &raw), ledger.append(&current, &raw));
        let ack = a.unwrap();
        assert_eq!(b.unwrap(), ack);
        verify_action_ack(&event, &ack).unwrap();
        assert_eq!(store.head(version).await.unwrap(), before + 1);
        current.now += 100;
        assert_eq!(ledger.append(&current, &raw).await.unwrap(), ack);
        invalid = event.clone();
        invalid["occurred_at"] = json!(999);
        assert!(ledger.append(&current, &bytes(&invalid)).await.is_err());
        let mut wrong_ack = ack.clone();
        wrong_ack["event_digest"] = json!(format!("sha256:{}", "0".repeat(64)));
        assert!(verify_action_ack(&event, &wrong_ack).is_err());
        let head = ledger
            .source_head(&gate, event["stream"]["id"].as_str().unwrap(), 1)
            .await
            .unwrap();
        assert_eq!(head["sequence"], 1);
        assert_eq!(head["event_digest"], ack["event_digest"]);
        if name == "predispatch" {
            predispatch_ack = ack.clone();
        }
        final_pair = (event, ack);
    }
    let result = ledger.lookup(&gate, "publish-artifact").await.unwrap();
    assert_eq!(result["events"].as_array().unwrap().len(), 8);
    assert_eq!(result["artifacts"].as_array().unwrap().len(), 3);
    assert_eq!(result["ledger_position"], 11);
    assert!(ledger
        .lookup(&admission("svc-council"), "publish-artifact")
        .await
        .is_err());
    final_pair
}

pub async fn historical_recovery(store: &dyn StorageBackend, version: &str) {
    let records = records();
    let mut policy = policy();
    let gate = admission("svc-gate");
    let ledger = ActionLedger::new(store, version, &policy);
    archives(&ledger).await;
    ledger
        .append(
            &admission("svc-council"),
            &bytes(&records["approval-recorded"]),
        )
        .await
        .unwrap();
    let claim = &records["claim-created"];
    let claim_digest = action_digest("accountability-event", claim).unwrap();
    let grant = &records["grant-issued"];
    for stream in &mut policy.streams {
        stream.generation = 2;
    }
    let ledger = ActionLedger::new(store, version, &policy);
    assert!(ledger.append(&gate, &bytes(claim)).await.is_err());
    for event in [claim, grant] {
        policy.recovery.push(RecoveryPermit {
            recorder: "svc-recovery".into(),
            producer: event["producer"].as_str().unwrap().into(),
            stream_id: event["stream"]["id"].as_str().unwrap().into(),
            generation: 1,
            kind: event["kind"].as_str().unwrap().into(),
            event_digest: action_digest("accountability-event", event).unwrap(),
            claim_id: claim["payload"]["claim"]["id"].as_str().unwrap().into(),
            claim_event_digest: claim_digest.clone(),
            cutoff: 1000,
        });
    }
    let recovery = admission("svc-recovery");
    let ledger = ActionLedger::new(store, version, &policy);
    assert!(
        ledger.append(&recovery, &bytes(grant)).await.is_err(),
        "original claim is missing"
    );
    assert!(
        ledger.append(&gate, &bytes(claim)).await.is_err(),
        "old producer is not recorder"
    );
    let mut invalid_policy = policy.clone();
    invalid_policy.recovery[0].claim_event_digest = format!("sha256:{}", "0".repeat(64));
    assert!(ActionLedger::new(store, version, &invalid_policy)
        .append(&recovery, &bytes(claim))
        .await
        .is_err());
    invalid_policy = policy.clone();
    invalid_policy.recovery[0].cutoff = 999;
    assert!(ActionLedger::new(store, version, &invalid_policy)
        .append(&recovery, &bytes(claim))
        .await
        .is_err());
    assert_eq!(store.head(version).await.unwrap(), 4);
    let ack = ledger.append(&recovery, &bytes(claim)).await.unwrap();
    ledger.append(&recovery, &bytes(grant)).await.unwrap();
    assert_eq!(ledger.append(&recovery, &bytes(claim)).await.unwrap(), ack);
    assert_eq!(
        ledger
            .source_head(&gate, claim["stream"]["id"].as_str().unwrap(), 2)
            .await
            .unwrap()["sequence"],
        0
    );
    let facts = store
        .slice_facts(version, &Default::default())
        .await
        .unwrap();
    let audit = facts
        .iter()
        .find(|f| f.seq == ack["position"].as_u64().unwrap())
        .unwrap()
        .evidence
        .as_ref()
        .unwrap();
    assert_eq!(audit["historical"], true);
    assert_eq!(audit["original_producer"], "gate");
    assert_eq!(audit["recorder_service"], "svc-recovery");
    // Current permission still applies to exact historical retries.
    policy.recovery.clear();
    assert!(ActionLedger::new(store, version, &policy)
        .append(&recovery, &bytes(claim))
        .await
        .is_err());
}
