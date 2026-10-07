// SPDX-License-Identifier: Apache-2.0
use munarium_core::{platform::*, storage::StorageBackend};
use munarium_store_mem::MemStore;
use serde_json::{json, Value};

#[test]
fn server_independently_consumes_canonical_vectors() {
    let vectors: Value = serde_json::from_str(include_str!(
        "../../../contract/platform-stage1/decision-json-v1-vectors.json"
    ))
    .unwrap();
    for case in vectors["cases"].as_array().unwrap() {
        if case["result"] == "reject" {
            assert!(
                canonical_record(case["input"].as_str().unwrap().as_bytes()).is_err(),
                "{}",
                case["id"]
            );
        } else {
            let value = canonical_record(case["canonical"].as_str().unwrap().as_bytes()).unwrap();
            assert_eq!(
                record_digest("munarium:decision-request:v1", &value).unwrap(),
                case["digest"]
            );
        }
    }
}

fn fixture() -> (Value, RecorderContext) {
    let all: Value = serde_json::from_str(include_str!(
        "../../../contract/platform-stage1/record-vectors.json"
    ))
    .unwrap();
    let event = all["examples"]["event"].clone();
    let c = RecorderContext {
        identity: RecorderIdentity {
            origin: "fixture-recorder".into(),
            actor: "fixture-recorder".into(),
            origin_kind: "service".into(),
            principal_digest: format!("sha256:{}", "a".repeat(64)),
        },
        tenant: event["tenant"].as_str().unwrap().into(),
        service: event["source_service"].as_str().unwrap().into(),
        source: event["source_service"].as_str().unwrap().into(),
        epoch: event["source_epoch"].as_u64().unwrap(),
        can_record: true,
        can_read: true,
    };
    (event, c)
}
#[tokio::test]
async fn platform_records_preserve_exact_retries_conflicts_sequences_and_tenants() {
    let store = MemStore::new();
    let version = store.create_version(None, None).await.unwrap();
    let (event, mut c) = fixture();
    let tenant = c.tenant.clone();
    let ledger = EventLedger::new(&store, &tenant, &version);
    let raw = serde_json::to_vec(&event).unwrap();
    let ack = ledger.append(&c, &raw).await.unwrap();
    verify_ack(&event, &ack).unwrap();
    assert_eq!(ledger.append(&c, &raw).await.unwrap(), ack);
    assert_eq!(store.head(&version).await.unwrap(), 1);
    let facts = store
        .slice_facts(&version, &Default::default())
        .await
        .unwrap();
    let audit = facts[0].evidence.as_ref().unwrap();
    assert_eq!(audit["recorder"]["origin"], c.identity.origin);
    assert_eq!(audit["recorder"]["origin_kind"], "service");
    assert_eq!(
        audit["recorder"]["principal_digest"],
        c.identity.principal_digest
    );
    let mut changed = event.clone();
    changed["recorded_at"] = json!(1234);
    assert!(ledger
        .append(&c, &serde_json::to_vec(&changed).unwrap())
        .await
        .is_err());
    changed = event.clone();
    changed["event_id"] = json!("second");
    changed["sequence"] = json!(3);
    changed["prior_event_digest"] =
        json!(record_digest("munarium:decision-event:v1", &event).unwrap());
    assert!(ledger
        .append(&c, &serde_json::to_vec(&changed).unwrap())
        .await
        .is_err());
    changed["sequence"] = json!(2);
    ledger
        .append(&c, &serde_json::to_vec(&changed).unwrap())
        .await
        .unwrap();
    let operation = event["operation_id"].as_str().unwrap();
    assert_eq!(ledger.lookup(&c, operation).await.unwrap().len(), 2);
    c.tenant = "other".into();
    assert!(ledger.lookup(&c, operation).await.is_err());
    assert!(ledger.append(&c, &raw).await.is_err());
    c.tenant = tenant.clone();
    c.can_record = false;
    assert!(ledger.append(&c, &raw).await.is_err());
}
#[tokio::test]
async fn concurrent_event_retry_has_one_ledger_position() {
    let store = MemStore::new();
    let version = store.create_version(None, None).await.unwrap();
    let (event, c) = fixture();
    let ledger = EventLedger::new(&store, &c.tenant, &version);
    let raw = serde_json::to_vec(&event).unwrap();
    let (a, b) = tokio::join!(ledger.append(&c, &raw), ledger.append(&c, &raw));
    assert_eq!(a.unwrap(), b.unwrap());
    assert_eq!(store.head(&version).await.unwrap(), 1);
}
