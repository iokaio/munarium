// SPDX-License-Identifier: Apache-2.0
#[path = "../../../tests/support/platform_actions.rs"]
mod support;
use munarium_core::{platform_actions::*, storage::StorageBackend};
use munarium_store_pg::PgStore;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires isolated MUNARIUM_TEST_DATABASE_URL; run with --ignored"]
async fn action_lifecycle_reconnect_and_historical_recovery() {
    let url = std::env::var("MUNARIUM_TEST_DATABASE_URL").expect("isolated database required");
    let tenant = format!("stage2-{}", uuid::Uuid::new_v4());
    let store = PgStore::connect(&url, &tenant).await.unwrap();
    let version = store.create_version(None, None).await.unwrap();
    let (event, ack) = support::lifecycle(&store, &version).await;
    drop(store);
    let reopened = PgStore::connect(&url, &tenant).await.unwrap();
    let policy = support::policy();
    let ledger = ActionLedger::new(&reopened, &version, &policy);
    let mut gate = support::admission("svc-gate");
    gate.now += 500;
    assert_eq!(
        ledger.append(&gate, &support::bytes(&event)).await.unwrap(),
        ack
    );
    assert_eq!(
        ledger.lookup(&gate, "publish-artifact").await.unwrap()["ledger_position"],
        11
    );
    let foreign = reopened
        .with_tenant(&format!("foreign-{}", uuid::Uuid::new_v4()))
        .await
        .unwrap();
    assert!(foreign.head(&version).await.is_err());
    let recovery_version = reopened.create_version(None, None).await.unwrap();
    support::historical_recovery(&reopened, &recovery_version).await;
}
