// SPDX-License-Identifier: Apache-2.0
//! Non-gating, deterministic governance measurements. Run explicitly in release
//! mode with `cargo test -p munarium-server --release governance_baseline -- --ignored --nocapture`.
use munarium_core::{
    gates::run_gates,
    storage::{load_snapshot, NewClaim, StorageBackend},
    types::*,
};
use munarium_store_mem::MemStore;
use sha2::{Digest, Sha256};
use std::{
    hint::black_box,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};

fn store() -> MemStore {
    let next = AtomicU64::new(0);
    MemStore::with_id_generator(Arc::new(move || {
        format!("{:032x}", next.fetch_add(1, Ordering::SeqCst))
    }))
}

// Four versions; approximately 10% corrections, 10% disputed claims and 10%
// anchors. Every correction follows its accepted original in the same lineage.
async fn corpus(size: usize) -> (MemStore, String) {
    let store = store();
    let mut version = store.create_version(None, None).await.unwrap();
    let mut previous = String::new();
    for i in 0..size {
        if i > 0 && i % (size / 4) == 0 {
            version = store.create_version(Some(&version), None).await.unwrap();
        }
        let subject = format!("subject-{}", if i % 10 == 1 { i - 1 } else { i });
        let mut claim = NewClaim::fact(&subject, "value", &format!("value-{i}"));
        claim.scope_path = Some(format!("fixture/{}", i % 8));
        if i % 10 == 1 {
            claim.claim_type = ClaimType::Correction;
            claim.supersedes_id = Some(previous.clone());
        }
        if i % 10 == 2 {
            claim.status = ClaimStatus::Disputed;
        }
        previous = store.append_claim(&version, claim, None).await.unwrap().id;
        if i % 10 == 3 {
            store
                .lock_anchor(
                    &version,
                    &subject,
                    "value",
                    &format!("value-{i}"),
                    None,
                    None,
                )
                .await
                .unwrap();
        }
    }
    (store, version)
}

fn snapshot_bytes(snapshot: &MeshSnapshot) -> Vec<u8> {
    serde_json::to_vec(&(
        &snapshot.version_id,
        &snapshot.facts,
        &snapshot.anchors,
        &snapshot.digests,
        &snapshot.promises,
        &snapshot.counters,
        &snapshot.entities,
        snapshot.as_of_seq,
    ))
    .unwrap()
}

fn candidate() -> Candidate {
    Candidate {
        claims: vec![ProposedClaim {
            claim_type: ClaimType::Fact,
            subject: "subject-3".into(),
            key: "value".into(),
            value: "conflict".into(),
            supersedes_id: None,
        }],
        ..Default::default()
    }
}

#[tokio::test]
async fn deterministic_governance_trace() {
    let mut traces = Vec::new();
    for _ in 0..2 {
        let (store, version) = corpus(40).await;
        let pin = store.head(&version).await.unwrap();
        let snapshot = load_snapshot(&store, &version, None, None, Some(pin))
            .await
            .unwrap();
        let findings = run_gates(&snapshot, &candidate());
        assert!(!findings.is_empty());
        let request = serde_json::from_value(serde_json::json!({"claim_type":"fact", "subject":"subject-3", "key":"value", "value":"conflict"})).unwrap();
        let result = crate::service::append_events(
            &store,
            &munarium_shapes::ShapeRegistry::default(),
            "fixture",
            &version,
            &[request],
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(result.claims[0].status, ClaimStatus::Disputed);
        traces.push((
            snapshot_bytes(&snapshot),
            serde_json::to_vec(&result.claims).unwrap(),
            serde_json::to_vec(&result.findings).unwrap(),
        ));
    }
    assert_eq!(traces[0], traces[1]);
}

#[tokio::test]
#[ignore = "measurement, not a timing gate"]
async fn governance_baseline() {
    assert!(!black_box(cfg!(debug_assertions)), "measure with --release");
    println!(
        "metadata={}",
        serde_json::json!({
            "schema":1, "fixture":"governance-v1", "profile":"release", "concurrency":1,
            "repetitions":3, "gate_samples":100, "snapshot_samples":10,
            "serialization_samples":10, "allocations":"not_measured", "memory":"not_measured",
            "os":std::env::consts::OS, "arch":std::env::consts::ARCH,
            "available_parallelism":std::thread::available_parallelism().unwrap().get(),
            "cargo_lock_sha256":hex::encode(Sha256::digest(include_bytes!("../../../Cargo.lock"))),
            "fixture_source_sha256":hex::encode(Sha256::digest(include_bytes!("governance_baseline.rs"))),
        })
    );
    for size in [64usize, 256, 1024] {
        for repetition in 0..3 {
            let (fixed, version) = corpus(size).await;
            let pin = fixed.head(&version).await.unwrap();
            let snapshot = load_snapshot(&fixed, &version, None, None, Some(pin))
                .await
                .unwrap();
            let corpus_hash = hex::encode(Sha256::digest(snapshot_bytes(&snapshot)));
            let candidate = candidate();
            // Warm each read stage once; cold-start construction is not timed.
            black_box(run_gates(&snapshot, &candidate));
            black_box(snapshot_bytes(&snapshot));
            let start = Instant::now();
            for _ in 0..100 {
                black_box(run_gates(black_box(&snapshot), black_box(&candidate)));
            }
            let gate_ns = start.elapsed().as_nanos() / 100;
            let start = Instant::now();
            for _ in 0..10 {
                black_box(
                    load_snapshot(&fixed, &version, None, None, Some(pin))
                        .await
                        .unwrap(),
                );
            }
            let snapshot_ns = start.elapsed().as_nanos() / 10;
            let start = Instant::now();
            for _ in 0..10 {
                black_box(snapshot_bytes(black_box(&snapshot)));
            }
            let serialization_ns = start.elapsed().as_nanos() / 10;
            // The second workload grows from empty, unique accepted facts,
            // one lineage. Separate raw adapter cost from the actual service's
            // head/snapshot/gate/append/findings path. Prepare input before timing.
            let raw: Vec<_> = (0..size)
                .map(|i| NewClaim::fact(&format!("growing-{i}"), "value", "fixture"))
                .collect();
            let requests: Vec<munarium_api_types::ProposeClaimRequest> = (0..size).map(|i|
                serde_json::from_value(serde_json::json!({"claim_type":"fact", "subject":format!("growing-{i}"), "key":"value", "value":"fixture"})).unwrap()).collect();
            let raw_store = store();
            let raw_version = raw_store.create_version(None, None).await.unwrap();
            let start = Instant::now();
            for claim in raw {
                black_box(
                    raw_store
                        .append_claim(&raw_version, claim, None)
                        .await
                        .unwrap(),
                );
            }
            let adapter_growing_ns = start.elapsed().as_nanos();
            let governed = store();
            let governed_version = governed.create_version(None, None).await.unwrap();
            let shapes = munarium_shapes::ShapeRegistry::default();
            let start = Instant::now();
            for request in requests {
                black_box(
                    crate::service::append_events(
                        &governed,
                        &shapes,
                        "fixture",
                        &governed_version,
                        &[request],
                        None,
                        None,
                        None,
                    )
                    .await
                    .unwrap(),
                );
            }
            let governed_growing_ns = start.elapsed().as_nanos();
            assert_eq!(governed.head(&governed_version).await.unwrap(), size as u64);
            println!(
                "sample={}",
                serde_json::json!({"size":size, "repetition":repetition,
                "corpus_sha256":corpus_hash, "gate_ns":gate_ns, "snapshot_ns":snapshot_ns,
                "serialization_ns":serialization_ns, "adapter_growing_ns":adapter_growing_ns,
                "governed_growing_ns":governed_growing_ns})
            );
        }
    }
}
