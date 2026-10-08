// SPDX-License-Identifier: Apache-2.0
#[path = "../../../tests/support/platform_actions.rs"]
mod support;
use munarium_core::{platform_actions::*, storage::StorageBackend};
use munarium_store_mem::MemStore;
use serde_json::json;

#[tokio::test]
async fn server_activation_is_atomic_and_idempotent() {
    let store = MemStore::new();
    let version = store.create_version(None, None).await.unwrap();
    support::activation_participant(&store, &version).await;
    let version = store.create_version(None, None).await.unwrap();
    support::activation_race(&store, &version).await;
}

#[tokio::test]
async fn action_lifecycle_and_denials() {
    let store = MemStore::new();
    let version = store.create_version(None, None).await.unwrap();
    support::lifecycle(&store, &version).await;
}
#[tokio::test]
async fn bounded_historical_recovery() {
    let store = MemStore::new();
    let version = store.create_version(None, None).await.unwrap();
    support::historical_recovery(&store, &version).await;
}
#[tokio::test]
async fn activation_binding_and_conflicting_writers() {
    let store = MemStore::new();
    let version = store.create_version(None, None).await.unwrap();
    let policy = support::policy();
    let records = support::records();
    let ledger = ActionLedger::new(&store, &version, &policy);
    let gate = support::admission("svc-registry");
    let event = &records["activation-event"];
    assert!(ledger.append(&gate, &support::bytes(event)).await.is_err());
    ledger
        .archive(
            &support::admission("svc-council"),
            &support::bytes(&records["activation"]),
        )
        .await
        .unwrap();
    let mut wrong = event.clone();
    wrong["payload"]["receipt"]["successor_epoch"] = json!(3);
    support::rehash(&mut wrong);
    assert!(ledger.append(&gate, &support::bytes(&wrong)).await.is_err());
    wrong = event.clone();
    wrong["occurred_at"] = json!(999);
    let a = support::bytes(event);
    let b = support::bytes(&wrong);
    let (one, two) = tokio::join!(ledger.append(&gate, &a), ledger.append(&gate, &b));
    assert_ne!(one.is_ok(), two.is_ok());
    assert_eq!(store.head(&version).await.unwrap(), 2);
    let accepted = if one.is_ok() { event } else { &wrong };
    let mut next = accepted.clone();
    next["event_id"] = json!("activation-second");
    next["sequence"] = json!(2);
    next["predecessor"] = json!(action_digest("accountability-event", accepted).unwrap());
    let next_ack = ledger.append(&gate, &support::bytes(&next)).await.unwrap();
    verify_action_ack(&next, &next_ack).unwrap();
    let read = support::admission("svc-gate");
    let result = ledger
        .transition(
            &read,
            records["activation"]["transition"]["id"].as_str().unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(result["events"].as_array().unwrap().len(), 2);
    assert_eq!(result["artifacts"].as_array().unwrap().len(), 1);
}
