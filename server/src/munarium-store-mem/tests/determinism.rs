// SPDX-License-Identifier: Apache-2.0
use munarium_core::{
    budget::{BudgetOutcome, BudgetStore},
    storage::{load_snapshot, NewClaim, StorageBackend},
    types::*,
};
use munarium_store_mem::{
    determinism::{Clock, IdGenerator},
    MemBudgetStore, MemStore,
};
use std::sync::{
    atomic::{AtomicI64, AtomicU64, Ordering},
    Arc,
};

fn ids() -> IdGenerator {
    let next = AtomicU64::new(0);
    Arc::new(move || format!("{:032x}", next.fetch_add(1, Ordering::SeqCst)))
}

async fn trace() -> Vec<u8> {
    let store = MemStore::with_id_generator(ids());
    let version = store.create_version(None, None).await.unwrap();
    assert_eq!(version, format!("memv-{:032x}", 0));
    let claim = store
        .append_claim(&version, NewClaim::fact("fixture", "color", "blue"), None)
        .await
        .unwrap();
    assert_eq!(claim.id, format!("claim-{:032x}", 1));
    let anchor = store
        .lock_anchor(&version, "fixture", "color", "blue", None, None)
        .await
        .unwrap();
    assert_eq!(anchor.id, format!("anchor-{:032x}", 2));
    let promise = store
        .register_promise(&version, "review", "task", "review fixture", None, None)
        .await
        .unwrap();
    assert_eq!(promise.id, format!("prom-{:032x}", 3));
    let child = store.create_version(Some(&version), None).await.unwrap();
    let mut correction = NewClaim::fact("fixture", "color", "green");
    correction.claim_type = ClaimType::Correction;
    correction.supersedes_id = Some(claim.id);
    let batch = store
        .append_claims(&child, vec![correction], None)
        .await
        .unwrap();
    assert_eq!(batch[0].id, format!("claim-{:032x}", 5));
    let pin = store.head(&child).await.unwrap();
    let snapshot = load_snapshot(&store, &child, None, None, Some(pin))
        .await
        .unwrap();
    serde_json::to_vec(&(
        snapshot.facts,
        snapshot.anchors,
        snapshot.promises,
        snapshot.digests,
    ))
    .unwrap()
}

#[tokio::test]
async fn ids_and_pinned_digests_replay_exactly() {
    assert_eq!(trace().await, trace().await);
    // Existing constructors retain UUID identities and prefixes.
    for store in [MemStore::new(), MemStore::default()] {
        let a = store.create_version(None, None).await.unwrap();
        let b = store.create_version(None, None).await.unwrap();
        assert_ne!(a, b);
        assert_eq!(a.len(), 37);
        assert!(uuid::Uuid::parse_str(a.strip_prefix("memv-").unwrap()).is_ok());
    }
}

#[tokio::test]
async fn midnight_backward_clock_and_exact_stale_boundary() {
    let midnight = chrono::DateTime::parse_from_rfc3339("2026-01-02T00:00:00Z")
        .unwrap()
        .timestamp();
    let now = Arc::new(AtomicI64::new(midnight - 1));
    let clock: Clock = {
        let now = now.clone();
        Arc::new(move || chrono::DateTime::from_timestamp(now.load(Ordering::SeqCst), 0).unwrap())
    };
    let store = MemBudgetStore::with_dependencies(clock, ids());
    let BudgetOutcome::Granted(old) = store
        .reserve("tenant", "cfg", "fast", 10, Some(10))
        .await
        .unwrap()
    else {
        panic!("grant")
    };
    assert_eq!(old.day, "2026-01-01");
    now.store(midnight, Ordering::SeqCst);
    assert!(store.ledger("tenant").await.unwrap().is_empty());
    let BudgetOutcome::Granted(new) = store
        .reserve("tenant", "cfg", "fast", 7, Some(10))
        .await
        .unwrap()
    else {
        panic!("new day has capacity")
    };
    assert_eq!(new.day, "2026-01-02");
    store.settle(&old, Some(20)).await.unwrap();
    assert_eq!(store.ledger("tenant").await.unwrap()[0].held_units, 7);
    // Moving backwards neither refunds yesterday's debt nor expires future rows.
    now.store(midnight - 2, Ordering::SeqCst);
    assert_eq!(store.sweep_stale(0).await.unwrap(), 0);
    assert_eq!(store.ledger("tenant").await.unwrap()[0].settled_units, 20);
    assert!(matches!(
        store
            .reserve("tenant", "cfg", "fast", 1, Some(10))
            .await
            .unwrap(),
        BudgetOutcome::Exhausted { .. }
    ));
    now.store(midnight + 9, Ordering::SeqCst);
    assert_eq!(store.sweep_stale(10).await.unwrap(), 0);
    now.store(midnight + 10, Ordering::SeqCst);
    assert_eq!(store.sweep_stale(10).await.unwrap(), 1);
    assert_eq!(store.sweep_stale(10).await.unwrap(), 0);
    let evidence = store.evidence("tenant", &new.id).await.unwrap().unwrap();
    assert_eq!(evidence.original_units, Some(7));
    assert_eq!(evidence.accounted_units, 7);
    assert_eq!(evidence.usage, None);
    assert_eq!(store.ledger("tenant").await.unwrap()[0].settled_units, 7);
}
