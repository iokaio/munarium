// SPDX-License-Identifier: Apache-2.0
//! Explicit live persistence test; never passes vacuously without PostgreSQL.
use munarium_core::{platform::*, storage::StorageBackend};
use munarium_store_pg::PgStore;
use serde_json::{json, Value};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires isolated MUNARIUM_TEST_DATABASE_URL; run with --ignored"]
async fn platform_events_survive_reconnect_and_serialize_duplicate_writers() {
    let url = std::env::var("MUNARIUM_TEST_DATABASE_URL").expect("isolated test database required");
    let tenant = format!("stage1-{}", uuid::Uuid::new_v4());
    let store = PgStore::connect(&url, &tenant).await.unwrap();
    let version = store.create_version(None, None).await.unwrap();
    let all: Value = serde_json::from_str(include_str!(
        "../../../contract/platform-stage1/record-vectors.json"
    ))
    .unwrap();
    let mut event = all["examples"]["event"].clone();
    event["kind"] = json!("proposal");
    event["payload"] = all["examples"]["request"].clone();
    event["tenant"] = json!(tenant);
    event["payload"]["tenant"] = json!(tenant);
    event["payload_digest"] =
        json!(record_digest("munarium:decision-event-payload:v1", &event["payload"]).unwrap());
    event["request_digest"] =
        json!(record_digest("munarium:decision-request:v1", &event["payload"]).unwrap());
    let context = RecorderContext {
        identity: RecorderIdentity {
            origin: "fixture-recorder".into(),
            actor: "fixture-recorder".into(),
            origin_kind: "service".into(),
            principal_digest: format!("sha256:{}", "a".repeat(64)),
        },
        tenant: tenant.clone(),
        service: event["source_service"].as_str().unwrap().into(),
        source: event["source_service"].as_str().unwrap().into(),
        epoch: 1,
        can_record: true,
        can_read: true,
    };
    let raw = serde_json::to_vec(&event).unwrap();
    let ledger = EventLedger::new(&store, &tenant, &version);
    let (a, b) = tokio::join!(ledger.append(&context, &raw), ledger.append(&context, &raw));
    let ack = a.unwrap();
    assert_eq!(ack, b.unwrap());
    verify_ack(&event, &ack).unwrap();
    assert_eq!(store.head(&version).await.unwrap(), 1);
    drop(store);
    let reopened = PgStore::connect(&url, &tenant).await.unwrap();
    let ledger = EventLedger::new(&reopened, &tenant, &version);
    assert_eq!(ledger.append(&context, &raw).await.unwrap(), ack);
    assert_eq!(
        ledger
            .lookup(&context, event["operation_id"].as_str().unwrap())
            .await
            .unwrap(),
        vec![event.clone()]
    );
    let mut changed = event.clone();
    changed["recorded_at"] = json!(1234);
    assert!(ledger
        .append(&context, &serde_json::to_vec(&changed).unwrap())
        .await
        .is_err());
    let result = json!({"schema_version":1,"tenant":tenant,"operation_id":event["operation_id"],
        "request_digest":event["request_digest"],"reasons":["fixture-refusal"]});
    let mut second = event.clone();
    second["kind"] = json!("refusal");
    second["event_id"] = json!("recorded-refusal");
    second["sequence"] = json!(2);
    second["prior_event_digest"] =
        json!(record_digest("munarium:decision-event:v1", &event).unwrap());
    second["payload"] = result.clone();
    second["payload_digest"] =
        json!(record_digest("munarium:decision-event-payload:v1", &result).unwrap());
    ledger
        .append(&context, &serde_json::to_vec(&second).unwrap())
        .await
        .unwrap();
    let replay = serde_json::to_vec(
        &json!({"request":event["payload"],"decision":result,"snapshot":null,"input":{}}),
    )
    .unwrap();
    let replay_digest = ledger.archive_replay(&context, &replay).await.unwrap();
    assert_eq!(
        ledger.archive_replay(&context, &replay).await.unwrap(),
        replay_digest
    );
    drop(reopened);
    let reopened = PgStore::connect(&url, &tenant).await.unwrap();
    let ledger = EventLedger::new(&reopened, &tenant, &version);
    assert_eq!(
        ledger
            .replay(&context, event["operation_id"].as_str().unwrap())
            .await
            .unwrap(),
        Some((replay, replay_digest))
    );
    let other = PgStore::connect(&url, "other-stage1-tenant").await.unwrap();
    assert!(other.head(&version).await.is_err());
    let mut foreign = context;
    foreign.tenant = "other-stage1-tenant".into();
    assert!(ledger
        .lookup(&foreign, event["operation_id"].as_str().unwrap())
        .await
        .is_err());
    assert!(ledger
        .replay(&foreign, event["operation_id"].as_str().unwrap())
        .await
        .is_err());
    assert_eq!(reopened.head(&version).await.unwrap(), 3);
}
