// SPDX-License-Identifier: Apache-2.0
//! P11 boundary regressions. Synthetic heads belong only in this test module.
use super::*;
use munarium_core::storage::FindingsQuery;
use std::time::Duration;

fn offline_store() -> PgStore {
    PgStore {
        pool: PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(1))
            .connect_lazy("postgres://test:test@127.0.0.1:1/test")
            .unwrap(),
        tenant_id: "integer-boundary".into(),
    }
}

fn assert_range_error<T: std::fmt::Debug>(result: Result<T>, field: &str) {
    assert!(
        matches!(result, Err(KernelError::Storage(ref message))
            if message.contains(field) && message.contains("PostgreSQL BIGINT")),
        "expected {field} range rejection before database I/O, got {result:?}"
    );
}

#[tokio::test]
async fn oversized_counter_count_is_rejected_before_database_io() {
    for count in [i64::MAX as u64 + 1, u64::MAX] {
        assert_range_error(
            offline_store()
                .record_counts("v", "k", "s", count, None)
                .await,
            "count",
        );
    }
}

#[tokio::test]
async fn oversized_counter_budget_is_rejected_before_database_io() {
    assert_range_error(
        offline_store()
            .record_counts("v", "k", "s", 0, Some(u64::MAX))
            .await,
        "budget",
    );
}

#[tokio::test]
async fn oversized_digest_seq_is_rejected_before_database_io() {
    assert_range_error(
        offline_store()
            .upsert_digest(&Digest {
                version_id: "v".into(),
                tier: 0,
                scope_path: "s".into(),
                content: "synthetic".into(),
                content_hash: "synthetic".into(),
                built_from_seq: u64::MAX,
            })
            .await,
        "built_from_seq",
    );
}

#[tokio::test]
async fn oversized_findings_seq_is_rejected_before_database_io() {
    assert_range_error(
        offline_store().record_findings("v", u64::MAX, &[]).await,
        "seq",
    );
}

#[tokio::test]
async fn oversized_query_limits_are_rejected_before_database_io() {
    use munarium_core::evidence::EvidenceStore;

    let Ok(limit) = usize::try_from(u64::MAX) else {
        return;
    };
    let store = offline_store();
    assert_range_error(
        store
            .findings(
                "v",
                &FindingsQuery {
                    limit: Some(limit),
                    ..Default::default()
                },
            )
            .await,
        "limit",
    );
    let evidence = PgEvidenceStore::new(store.pool.clone());
    assert_range_error(evidence.accesses("t", "e", limit).await, "limit");
    assert_range_error(
        evidence.purge_due("2026-09-24T00:00:00Z", limit).await,
        "limit",
    );
}

/// Seed one synthetic projection row; do not allocate trillions of writes or
/// expose a production sequence-setting API. The ordinary append path follows.
async fn seed_head(store: &PgStore, version: &str, head: u64) {
    sqlx::query("INSERT INTO anchors (tenant_id, id, version_id, detail_key, locked_value, seq) VALUES ($1, $2, $3, 'fixture.head', 'synthetic', $4)")
        .bind(&store.tenant_id)
        .bind(format!("anchor-{}", uuid::Uuid::new_v4().simple()))
        .bind(version)
        .bind(i64::try_from(head).unwrap())
        .execute(store.pool()).await.unwrap();
}

#[tokio::test]
async fn high_sequences_round_trip_and_preserve_inclusive_pins() {
    let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
        eprintln!("UNAVAILABLE: P11 high sequences require MUNARIUM_TEST_DATABASE_URL");
        return;
    };
    let store = PgStore::connect(&url, &format!("p11-seq-{}", uuid::Uuid::new_v4().simple()))
        .await
        .unwrap();
    for seq in [
        (1_u64 << 53) - 1,
        1_u64 << 53,
        (1_u64 << 53) + 1,
        i64::MAX as u64,
    ] {
        let version = store.create_version(None, None).await.unwrap();
        seed_head(&store, &version, seq - 1).await;
        let claim = store
            .append_claim(&version, NewClaim::fact("s", "k", "v"), Some(seq - 1))
            .await
            .unwrap();
        assert_eq!(claim.seq, seq);
        assert_eq!(store.head(&version).await.unwrap(), seq);
        assert_eq!(store.get_claim(&claim.id).await.unwrap().unwrap().seq, seq);
        assert!(store
            .slice_facts(
                &version,
                &FactQuery {
                    as_of_seq: Some(seq - 1),
                    ..Default::default()
                }
            )
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .slice_facts(
                    &version,
                    &FactQuery {
                        as_of_seq: Some(seq),
                        ..Default::default()
                    }
                )
                .await
                .unwrap()[0]
                .seq,
            seq
        );
        // Compare-and-append replay rejects the stale head without rounding
        // either field in its conflict metadata.
        assert!(
            matches!(store.append_claim(&version, NewClaim::fact("s", "k", "v"), Some(seq - 1)).await,
            Err(KernelError::HeadConflict { expected, actual }) if expected == seq - 1 && actual == seq)
        );
        let event_seq: i64 = sqlx::query_scalar("SELECT seq FROM ledger_events WHERE tenant_id=$1 AND version_id=$2 AND body->>'claim_id'=$3")
            .bind(&store.tenant_id).bind(&version).bind(&claim.id).fetch_one(store.pool()).await.unwrap();
        assert_eq!(u64::try_from(event_seq).unwrap(), seq);
        // Pins span u64 on the wire. A pin beyond the largest stored seq is
        // still an inclusive upper bound, never a wrapped negative SQL value.
        assert_eq!(
            store.anchors(&version, Some(u64::MAX)).await.unwrap().len(),
            1
        );
        let finding = GateFinding {
            rule_id: "p11.boundary".into(),
            severity: Severity::Info,
            message: "synthetic".into(),
            scope_path: None,
            detail: None,
        };
        store
            .record_findings(&version, seq, &[finding])
            .await
            .unwrap();
        let rows = store
            .findings(
                &version,
                &FindingsQuery {
                    as_of_seq: Some(u64::MAX),
                    limit: Some(1),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].seq, seq);
        if seq == i64::MAX as u64 {
            assert_range_error(
                store
                    .append_claim(&version, NewClaim::fact("s", "overflow", "v"), None)
                    .await,
                "seq",
            );
            assert_range_error(
                store.lock_anchor(&version, "s", "k", "v", None, None).await,
                "seq",
            );
            assert_range_error(
                store
                    .register_promise(&version, "p", "setup", "v", None, None)
                    .await,
                "seq",
            );
            assert_range_error(
                store.record_counts(&version, "k", "s", 1, None).await,
                "seq",
            );
            assert_eq!(store.head(&version).await.unwrap(), seq);
        }
    }
}

#[tokio::test]
async fn counter_values_round_trip_at_integer_boundaries() {
    let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
        eprintln!("UNAVAILABLE: P11 counter boundaries require MUNARIUM_TEST_DATABASE_URL");
        return;
    };
    let store = PgStore::connect(
        &url,
        &format!("p11-count-{}", uuid::Uuid::new_v4().simple()),
    )
    .await
    .unwrap();
    let version = store.create_version(None, None).await.unwrap();
    for value in [
        (1_u64 << 53) - 1,
        1_u64 << 53,
        (1_u64 << 53) + 1,
        i64::MAX as u64,
    ] {
        store
            .record_counts(&version, "tokens", "s", value, Some(value))
            .await
            .unwrap();
        let totals = store
            .counter_totals(&version, Some(u64::MAX))
            .await
            .unwrap();
        assert_eq!(totals.len(), 1);
        assert_eq!(totals[0].total, value);
        assert_eq!(totals[0].budget, Some(value));
    }
    let head = store.head(&version).await.unwrap();
    assert_range_error(
        store
            .record_counts(&version, "tokens", "s", u64::MAX, None)
            .await,
        "count",
    );
    assert_eq!(store.head(&version).await.unwrap(), head);
    assert_eq!(
        store.counter_totals(&version, None).await.unwrap()[0].total,
        i64::MAX as u64
    );
}

#[tokio::test]
async fn batch_sequence_exhaustion_is_an_error_and_rolls_back() {
    let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
        eprintln!("UNAVAILABLE: P11 sequence exhaustion requires MUNARIUM_TEST_DATABASE_URL");
        return;
    };
    let store = PgStore::connect(&url, &format!("p11-max-{}", uuid::Uuid::new_v4().simple()))
        .await
        .unwrap();
    let version = store.create_version(None, None).await.unwrap();
    let head = i64::MAX as u64 - 1;
    seed_head(&store, &version, head).await;
    assert_range_error(
        store
            .append_claims(
                &version,
                vec![NewClaim::fact("s", "a", "1"), NewClaim::fact("s", "b", "2")],
                None,
            )
            .await,
        "seq",
    );
    assert!(store
        .slice_facts(&version, &FactQuery::default())
        .await
        .unwrap()
        .is_empty());
    assert_eq!(store.head(&version).await.unwrap(), head);
    let events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ledger_events WHERE tenant_id=$1 AND version_id=$2",
    )
    .bind(&store.tenant_id)
    .bind(&version)
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(events, 0);
}

#[tokio::test]
async fn negative_stored_sequences_do_not_decode_as_unsigned_maxima() {
    let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
        eprintln!("UNAVAILABLE: P11 corrupt sequence decoding requires MUNARIUM_TEST_DATABASE_URL");
        return;
    };
    let store = PgStore::connect(
        &url,
        &format!("p11-negative-{}", uuid::Uuid::new_v4().simple()),
    )
    .await
    .unwrap();
    let version = store.create_version(None, None).await.unwrap();
    sqlx::query("INSERT INTO anchors (tenant_id, id, version_id, detail_key, locked_value, seq) VALUES ($1, 'negative', $2, 'fixture.bad', 'synthetic', -1)")
        .bind(&store.tenant_id).bind(&version).execute(store.pool()).await.unwrap();
    assert!(matches!(
        store.anchors(&version, None).await,
        Err(KernelError::Storage(_))
    ));
}

#[tokio::test]
async fn reservation_amounts_and_reporting_totals_preserve_boundaries() {
    use munarium_core::budget::{BudgetOutcome, BudgetStore};

    let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
        eprintln!("UNAVAILABLE: P11 reservation boundaries require MUNARIUM_TEST_DATABASE_URL");
        return;
    };
    let tenant = format!("p11-budget-{}", uuid::Uuid::new_v4().simple());
    let store = PgStore::connect(&url, &tenant).await.unwrap();
    let budget = PgBudgetStore::new(store.pool.clone());
    for value in [
        (1_u64 << 53) - 1,
        1_u64 << 53,
        (1_u64 << 53) + 1,
        i64::MAX as u64,
    ] {
        let config = value.to_string();
        let BudgetOutcome::Granted(reservation) = budget
            .reserve(&tenant, &config, "answer", value, Some(value))
            .await
            .unwrap()
        else {
            panic!("boundary reservation must fit")
        };
        assert_eq!(reservation.units, value);
        let evidence = budget
            .evidence(&tenant, &reservation.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(evidence.original_units, Some(value));
        assert_eq!(evidence.accounted_units, value);
        let rows = budget.ledger(&tenant).await.unwrap();
        let row = rows.iter().find(|row| row.config == config).unwrap();
        assert_eq!(row.held_units, value);
        assert!(
            matches!(budget.reserve(&tenant, &config, "answer", 1, Some(value)).await.unwrap(),
            BudgetOutcome::Exhausted { requested: 1, remaining: 0, limit } if limit == value)
        );
        budget.settle(&reservation, Some(value)).await.unwrap();
        // Replayed settlement is idempotent and preserves the original result.
        budget.settle(&reservation, Some(0)).await.unwrap();
        let rows = budget.ledger(&tenant).await.unwrap();
        let row = rows.iter().find(|row| row.config == config).unwrap();
        assert_eq!(row.held_units, 0);
        assert_eq!(row.settled_units, value);
        assert_range_error(
            budget.settle(&reservation, Some(u64::MAX)).await,
            "budget units",
        );
    }
    assert_range_error(
        budget
            .reserve(&tenant, "overflow", "answer", u64::MAX, Some(u64::MAX))
            .await,
        "budget units",
    );
}

#[tokio::test]
async fn counter_aggregate_overflow_is_a_storage_error_not_a_wrapped_total() {
    let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
        eprintln!("UNAVAILABLE: P11 aggregate boundaries require MUNARIUM_TEST_DATABASE_URL");
        return;
    };
    let store = PgStore::connect(&url, &format!("p11-sum-{}", uuid::Uuid::new_v4().simple()))
        .await
        .unwrap();
    let version = store.create_version(None, None).await.unwrap();
    store
        .record_counts(&version, "tokens", "a", i64::MAX as u64, None)
        .await
        .unwrap();
    store
        .record_counts(&version, "tokens", "b", 1, None)
        .await
        .unwrap();
    assert!(matches!(
        store.counter_totals(&version, None).await,
        Err(KernelError::Storage(_))
    ));
    // The older pin still reads the supported total exactly.
    assert_eq!(
        store.counter_totals(&version, Some(1)).await.unwrap()[0].total,
        i64::MAX as u64
    );
}
