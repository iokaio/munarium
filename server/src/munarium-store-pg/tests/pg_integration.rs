// SPDX-License-Identifier: Apache-2.0
//! Postgres integration tests. Skip (pass vacuously) when
//! MUNARIUM_TEST_DATABASE_URL is unset, so plain `cargo test --workspace` stays
//! green without a database; CI and local runs set it to the compose
//! postgres (postgres://munarium:munarium-dev@localhost:5433/munarium).

use munarium_core::storage::{NewClaim, StorageBackend};
use munarium_core::KernelError;
use munarium_store_pg::PgStore;
use std::sync::Arc;

fn test_url() -> Option<String> {
    std::env::var("MUNARIUM_TEST_DATABASE_URL").ok()
}

fn fresh_tenant(prefix: &str) -> String {
    format!(
        "{prefix}-{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

/// The lineage_heads FOR UPDATE design validated under real concurrency:
/// two writers race the same expected_head; exactly one wins, the loser gets
/// a clean retryable HeadConflict, and the ledger stays gap-free.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_appends_serialize_on_lineage_head() {
    let Some(url) = test_url() else { return };
    let store = Arc::new(
        PgStore::connect(&url, &fresh_tenant("conc"))
            .await
            .expect("connect"),
    );
    let v = store.create_version(None, None).await.expect("version");

    for _round in 0..5 {
        let head = store.head(&v).await.expect("head");
        let (a, b) = tokio::join!(
            {
                let s = store.clone();
                let v = v.clone();
                async move {
                    s.append_claim(
                        &v,
                        NewClaim::fact("s", "k", &format!("a{head}")),
                        Some(head),
                    )
                    .await
                }
            },
            {
                let s = store.clone();
                let v = v.clone();
                async move {
                    s.append_claim(
                        &v,
                        NewClaim::fact("s", "k2", &format!("b{head}")),
                        Some(head),
                    )
                    .await
                }
            }
        );
        let winners = [&a, &b].iter().filter(|r| r.is_ok()).count();
        let conflicts = [&a, &b]
            .iter()
            .filter(|r| matches!(r, Err(KernelError::HeadConflict { .. })))
            .count();
        assert_eq!(
            (winners, conflicts),
            (1, 1),
            "exactly one writer must win per race: {a:?} / {b:?}"
        );
    }

    // seq is dense and monotonic: 5 winners => head 5, seqs 1..=5
    let head = store.head(&v).await.expect("head");
    assert_eq!(head, 5);
    let facts = store
        .slice_facts(&v, &munarium_core::ledger::FactQuery::default())
        .await
        .expect("slice");
    let mut seqs: Vec<u64> = facts.iter().map(|f| f.seq).collect();
    seqs.sort();
    assert_eq!(seqs, vec![1, 2, 3, 4, 5]);
}

/// Cross-backend agreement: an identical command script produces identical
/// semantic views (facts, anchors, promises — head AND pinned) on MemStore
/// and PgStore. Ids differ; the comparison is over normalized content + seq.
#[tokio::test]
async fn pg_agrees_with_mem_reference() {
    let Some(url) = test_url() else { return };
    let pg = PgStore::connect(&url, &fresh_tenant("agree"))
        .await
        .expect("connect");
    let mem = munarium_store_mem::MemStore::new();

    async fn run_script(store: &dyn StorageBackend) -> String {
        let v1 = store.create_version(None, None).await.unwrap();
        let c1 = store
            .append_claim(&v1, NewClaim::fact("hero", "eyes", "green"), None)
            .await
            .unwrap();
        store
            .register_promise(&v1, "reveal", "setup", "open the letter", None, Some("ch3"))
            .await
            .unwrap();
        store
            .lock_anchor(&v1, "hero", "name", "Ansel", None, None)
            .await
            .unwrap();
        let v2 = store.create_version(Some(&v1), None).await.unwrap();
        let mut fix = NewClaim::fact("hero", "eyes", "blue");
        fix.claim_type = munarium_core::types::ClaimType::Correction;
        fix.supersedes_id = Some(c1.id);
        store.append_claim(&v2, fix, None).await.unwrap();
        store.fulfill_promise(&v2, "reveal").await.unwrap();
        v2
    }

    async fn view(store: &dyn StorageBackend, version: &str, pin: Option<u64>) -> Vec<String> {
        let facts = store
            .slice_facts(
                version,
                &munarium_core::ledger::FactQuery {
                    as_of_seq: pin,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let mut out: Vec<String> = facts
            .iter()
            .map(|f| format!("F:{}@{}", f.normalized_text(), f.seq))
            .collect();
        for (k, a) in store.anchors(version, pin).await.unwrap() {
            out.push(format!("A:{k}={}@{}", a.locked_value, a.seq));
        }
        for p in store.promises(version, pin).await.unwrap() {
            out.push(format!("P:{}:{:?}@{}", p.key, p.status, p.seq));
        }
        out.sort();
        out
    }

    let pg_leaf = run_script(&pg).await;
    let mem_leaf = run_script(&mem).await;

    for pin in [None, Some(1), Some(2), Some(3)] {
        let pg_view = view(&pg, &pg_leaf, pin).await;
        let mem_view = view(&mem, &mem_leaf, pin).await;
        assert_eq!(pg_view, mem_view, "views must agree at pin {pin:?}");
    }
}

/// Findings persistence (2026-08-17): the pg store round-trips
/// record_findings/findings identically to the mem reference — same rows,
/// same pin behavior — so the two backends cannot drift on the new store.
#[tokio::test]
async fn findings_round_trip_agrees_with_the_mem_reference() {
    let Some(url) = test_url() else { return };
    let pg = PgStore::connect(&url, &fresh_tenant("find"))
        .await
        .expect("connect");
    let mem = munarium_store_mem::MemStore::new();

    let finding = munarium_core::types::GateFinding {
        rule_id: "gate.ledger-conflict".into(),
        severity: munarium_core::types::Severity::Block,
        message: "hero.eyes: 'blue' conflicts with established 'green'".into(),
        scope_path: Some("ch01".into()),
        detail: Some(serde_json::json!({ "existing": "green", "candidate": "blue" })),
    };
    let mut results = Vec::new();
    for store in [&pg as &dyn StorageBackend, &mem as &dyn StorageBackend] {
        let v = store.create_version(None, None).await.expect("version");
        store
            .record_findings(&v, 2, std::slice::from_ref(&finding))
            .await
            .expect("record");
        let all = store
            .findings(&v, &munarium_core::storage::FindingsQuery::default())
            .await
            .expect("read");
        let pinned_out = store
            .findings(
                &v,
                &munarium_core::storage::FindingsQuery {
                    as_of_seq: Some(1),
                    ..Default::default()
                },
            )
            .await
            .expect("pinned read");
        let by_rule = store
            .findings(
                &v,
                &munarium_core::storage::FindingsQuery {
                    rule_id: Some("gate.ledger-conflict".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("rule read");
        results.push((all, pinned_out.len(), by_rule.len()));
    }
    let (pg_all, pg_pinned, pg_rule) = &results[0];
    let (mem_all, mem_pinned, mem_rule) = &results[1];
    assert_eq!(pg_all.len(), 1);
    assert_eq!(pg_all[0].seq, 2);
    assert_eq!(pg_all[0].finding, finding);
    assert_eq!(pg_all, mem_all, "backends must agree byte-for-byte");
    assert_eq!((pg_pinned, pg_rule), (mem_pinned, mem_rule));
    assert_eq!(*pg_pinned, 0, "a pin before the write bounds the store");
}

// ---------------------------------------------------------------------------
// The evidence plane, run against BOTH stores
// ---------------------------------------------------------------------------
//
// This tree uses runtime-checked SQL rather than `query!` macros, so the drift
// net is that the memory and Postgres stores answer the same scenarios
// identically. Each test below builds both backends and asserts the same
// property against each, which is why the assertions carry the backend name:
// a failure has to say WHICH store disagreed.
//
// The Postgres half skips when MUNARIUM_TEST_DATABASE_URL is unset, exactly
// like the rest of this file. The memory half always runs — so a semantic
// mistake in the shared logic fails on every developer's machine, and only a
// genuinely SQL-shaped mistake waits for CI.

mod evidence {
    use super::{fresh_tenant, test_url};
    use munarium_core::evidence::*;
    use munarium_store_mem::MemEvidenceStore;
    use munarium_store_pg::{PgEvidenceStore, PgStore};

    fn manifest(
        tenant: &str,
        logical: &str,
        level: i32,
        compartments: &[&str],
    ) -> EvidenceManifest {
        EvidenceManifest {
            contract_version: CONTRACT_VERSION.trim().to_string(),
            canon: "canon@1".into(),
            evidence_id: None,
            tenant: tenant.to_string(),
            kind: EvidenceKind::Table,
            logical_result_hash: format!("sha256:{logical}"),
            artifact_hash: format!("sha256:{}", "b".repeat(64)),
            bytes_len: 12,
            media_type: MEDIA_TYPE_CSV.into(),
            source: SourceRef {
                source_id: "crm".into(),
                source_version: 1,
                adapter: "postgres".into(),
                adapter_version: None,
                engine: None,
                driver: None,
            },
            versions: Versions {
                policy: Some("policy@3".into()),
                ..Default::default()
            },
            plan: None,
            schema: EvidenceSchema {
                columns: vec![EvidenceColumn {
                    id: "c1".into(),
                    name: "region".into(),
                    ty: ColumnType::String,
                    nullable: false,
                    scale: None,
                    unit: None,
                    additivity: None,
                    key: true,
                    element_type: None,
                }],
            },
            identity: EvidenceIdentity {
                row_id_rule: RowIdRule::Keys,
                order_by: vec![],
                rows: Some(1),
            },
            completeness: Completeness {
                truncated: false,
                declared_max_rows: None,
                rows_covered: None,
                rows_excluded: None,
                exclusion_reason: None,
            },
            redaction: None,
            snapshot_vector: vec![SnapshotMarker {
                source_id: "crm".into(),
                marker: None,
                isolation: None,
                started_at: None,
                ended_at: None,
                replay_level: "sealed_result".into(),
                replay_expires_at: None,
            }],
            freshness: None,
            execution: Execution {
                started_at: "2026-08-28T10:00:00Z".into(),
                ended_at: "2026-08-28T10:00:01Z".into(),
                effective_principal: None,
                statement_id: None,
            },
            authorization_class: AuthorizationClass {
                name: None,
                access_level: level,
                compartments: compartments.iter().map(|c| c.to_string()).collect(),
            },
            retention: None,
        }
    }

    fn artifact(m: EvidenceManifest, id: &str, state: EvidenceState) -> EvidenceArtifact {
        EvidenceArtifact {
            evidence_id: id.to_string(),
            tenant: m.tenant.clone(),
            state,
            manifest: m,
            blob_path: format!("evidence/{id}"),
            created_at: "2026-08-28T10:00:00.000Z".into(),
            committed_at: None,
        }
    }

    /// Both backends, as trait objects, so one body tests both. The Postgres
    /// entry is absent when no database is configured.
    ///
    /// **The skip is loud, and deliberately so.** Munarium Matrix's own
    /// Postgres conformance tier was vacuously green for a while because its
    /// setup turned any connection failure into a "no database" skip that
    /// reported as a pass — the tests said `ok` and had tested nothing. Two
    /// defenses against repeating that here:
    ///
    /// 1. The memory backend ALWAYS runs, so every one of these tests asserts
    ///    something real on every machine.
    /// 2. When `MUNARIUM_TEST_DATABASE_URL` **is** set, a connection failure
    ///    panics rather than skipping. Configuring a database and silently
    ///    getting the memory-only run is the exact failure mode that made the
    ///    Matrix tier lie.
    async fn backends() -> Vec<(&'static str, Box<dyn EvidenceStore>)> {
        let mut out: Vec<(&'static str, Box<dyn EvidenceStore>)> =
            vec![("mem", Box::new(MemEvidenceStore::new()))];
        match test_url() {
            Some(url) => {
                let base = PgStore::connect(&url, &fresh_tenant("ev")).await.expect(
                    "MUNARIUM_TEST_DATABASE_URL is set but the connection failed. This is a                      FAILURE, not a skip: a configured database that quietly falls back to                      memory is how a store tier reports green while testing nothing.",
                );
                out.push(("pg", Box::new(PgEvidenceStore::new(base.pool().clone()))));
            }
            None => eprintln!(
                "NOTE: MUNARIUM_TEST_DATABASE_URL is unset — this assertion ran against the                  MEMORY store only. The Postgres half is UNTESTED in this run."
            ),
        }
        out
    }

    #[tokio::test]
    async fn a_replayed_seal_is_one_artifact_on_every_backend() {
        for (name, store) in backends().await {
            let tenant = fresh_tenant("t");
            let m = manifest(&tenant, &"1".repeat(64), 2, &["fin"]);

            let first = store
                .register(&artifact(m.clone(), "ev-a", EvidenceState::Committed), None)
                .await
                .expect("register");
            assert!(first.created, "[{name}] the first seal creates");

            // A DIFFERENT id, the SAME logical result: the domain key decides.
            let second = store
                .register(&artifact(m, "ev-b", EvidenceState::Committed), None)
                .await
                .expect("register");
            assert!(!second.created, "[{name}] a replay must not create");
            assert_eq!(
                first.evidence_id, second.evidence_id,
                "[{name}] a replay must resolve to the FIRST artifact"
            );
        }
    }

    #[tokio::test]
    async fn re_serializing_one_result_does_not_mint_a_second_artifact() {
        // The reason the two hashes are distinct: a CSV and a Parquet encoding
        // of one logical result are one artifact, not two.
        for (name, store) in backends().await {
            let tenant = fresh_tenant("t");
            let a = manifest(&tenant, &"1".repeat(64), 2, &["fin"]);
            let mut b = a.clone();
            b.artifact_hash = format!("sha256:{}", "c".repeat(64));
            b.media_type = MEDIA_TYPE_PARQUET.into();

            let first = store
                .register(&artifact(a, "ev-a", EvidenceState::Committed), None)
                .await
                .expect("register");
            let second = store
                .register(&artifact(b, "ev-b", EvidenceState::Committed), None)
                .await
                .expect("register");
            assert_eq!(
                first.evidence_id, second.evidence_id,
                "[{name}] two serializations of one result are one artifact"
            );
        }
    }

    #[tokio::test]
    async fn a_different_authorization_class_is_a_different_artifact() {
        for (name, store) in backends().await {
            let tenant = fresh_tenant("t");
            let low = manifest(&tenant, &"1".repeat(64), 2, &["fin"]);
            let high = manifest(&tenant, &"1".repeat(64), 9, &["fin"]);

            let a = store
                .register(&artifact(low, "ev-a", EvidenceState::Committed), None)
                .await
                .expect("register");
            let b = store
                .register(&artifact(high, "ev-b", EvidenceState::Committed), None)
                .await
                .expect("register");
            assert_ne!(
                a.evidence_id, b.evidence_id,
                "[{name}] the same rows under a different clearance are a different artifact"
            );
        }
    }

    #[tokio::test]
    async fn tenants_never_see_each_others_artifacts() {
        for (name, store) in backends().await {
            let mine = fresh_tenant("mine");
            let theirs = fresh_tenant("theirs");
            let m = manifest(&mine, &"1".repeat(64), 2, &["fin"]);
            store
                .register(&artifact(m, "ev-x", EvidenceState::Committed), None)
                .await
                .expect("register");

            assert!(
                store.get(&mine, "ev-x").await.expect("get").is_some(),
                "[{name}] the owning tenant sees it"
            );
            assert!(
                store.get(&theirs, "ev-x").await.expect("get").is_none(),
                "[{name}] another tenant must not, even with the exact id"
            );
        }
    }

    #[tokio::test]
    async fn a_grant_is_single_use_on_every_backend() {
        for (name, store) in backends().await {
            let tenant = fresh_tenant("t");
            let m = manifest(&tenant, &"1".repeat(64), 2, &["fin"]);
            let grant = EvidenceGrant {
                grant_id: "gr-1".into(),
                evidence_id: "ev-a".into(),
                tenant: tenant.clone(),
                expires_at: "2099-01-01T00:00:00.000Z".into(),
                used_at: None,
            };
            store
                .register(&artifact(m, "ev-a", EvidenceState::Pending), Some(&grant))
                .await
                .expect("register");

            let now = "2026-08-28T10:00:00.000Z";
            assert!(
                store
                    .consume_grant(&tenant, "ev-a", "gr-1", now)
                    .await
                    .expect("consume")
                    .is_some(),
                "[{name}] the first spend succeeds"
            );
            assert!(
                store
                    .consume_grant(&tenant, "ev-a", "gr-1", now)
                    .await
                    .expect("consume")
                    .is_none(),
                "[{name}] the second spend must fail — a grant is SINGLE use"
            );
        }
    }

    #[tokio::test]
    async fn an_expired_grant_is_refused() {
        for (name, store) in backends().await {
            let tenant = fresh_tenant("t");
            let m = manifest(&tenant, &"1".repeat(64), 2, &["fin"]);
            let grant = EvidenceGrant {
                grant_id: "gr-old".into(),
                evidence_id: "ev-a".into(),
                tenant: tenant.clone(),
                expires_at: "2020-01-01T00:00:00.000Z".into(),
                used_at: None,
            };
            store
                .register(&artifact(m, "ev-a", EvidenceState::Pending), Some(&grant))
                .await
                .expect("register");
            assert!(
                store
                    .consume_grant(&tenant, "ev-a", "gr-old", "2026-08-28T10:00:00.000Z")
                    .await
                    .expect("consume")
                    .is_none(),
                "[{name}] an expired grant must not spend"
            );
        }
    }

    #[tokio::test]
    async fn a_grant_bound_to_another_artifact_is_refused() {
        for (name, store) in backends().await {
            let tenant = fresh_tenant("t");
            let m = manifest(&tenant, &"1".repeat(64), 2, &["fin"]);
            let grant = EvidenceGrant {
                grant_id: "gr-1".into(),
                evidence_id: "ev-a".into(),
                tenant: tenant.clone(),
                expires_at: "2099-01-01T00:00:00.000Z".into(),
                used_at: None,
            };
            store
                .register(&artifact(m, "ev-a", EvidenceState::Pending), Some(&grant))
                .await
                .expect("register");
            assert!(
                store
                    .consume_grant(&tenant, "ev-OTHER", "gr-1", "2026-08-28T10:00:00.000Z")
                    .await
                    .expect("consume")
                    .is_none(),
                "[{name}] a grant is bound to ONE artifact"
            );
        }
    }

    #[tokio::test]
    async fn a_replayed_commit_reports_false() {
        for (name, store) in backends().await {
            let tenant = fresh_tenant("t");
            let m = manifest(&tenant, &"1".repeat(64), 2, &["fin"]);
            store
                .register(&artifact(m, "ev-a", EvidenceState::Pending), None)
                .await
                .expect("register");

            let at = "2026-08-28T11:00:00.000Z";
            assert!(
                store.commit(&tenant, "ev-a", at).await.expect("commit"),
                "[{name}] the first commit changes state"
            );
            assert!(
                !store.commit(&tenant, "ev-a", at).await.expect("commit"),
                "[{name}] a replayed commit must report false, not restamp the \
                 retention clock"
            );
            let stored = store.get(&tenant, "ev-a").await.expect("get").expect("row");
            assert_eq!(stored.state, EvidenceState::Committed, "[{name}]");
        }
    }

    #[tokio::test]
    async fn committing_an_unknown_artifact_is_not_found() {
        for (name, store) in backends().await {
            let tenant = fresh_tenant("t");
            let err = store
                .commit(&tenant, "ev-nope", "2026-08-28T11:00:00.000Z")
                .await
                .expect_err("must fail");
            assert!(
                matches!(err, munarium_core::KernelError::NotFound { .. }),
                "[{name}] expected NotFound, got {err}"
            );
        }
    }

    #[tokio::test]
    async fn the_manifest_survives_the_round_trip_intact() {
        // The manifest IS the record; a store that silently reshaped it would
        // make every later citation argue with the one that sealed it.
        for (name, store) in backends().await {
            let tenant = fresh_tenant("t");
            let m = manifest(&tenant, &"1".repeat(64), 2, &["fin", "hr"]);
            store
                .register(&artifact(m.clone(), "ev-a", EvidenceState::Committed), None)
                .await
                .expect("register");
            let back = store.get(&tenant, "ev-a").await.expect("get").expect("row");
            assert_eq!(back.manifest, m, "[{name}] the manifest must round-trip");
            assert_eq!(back.blob_path, "evidence/ev-a", "[{name}]");
        }
    }

    // -- retention (package 2) --------------------------------------------

    fn with_retention(
        mut m: EvidenceManifest,
        expires_at: Option<&str>,
        legal_hold: bool,
    ) -> EvidenceManifest {
        m.retention = Some(Retention {
            expires_at: expires_at.map(str::to_string),
            legal_hold,
            purged_at: None,
        });
        m
    }

    #[tokio::test]
    async fn only_expired_unheld_artifacts_are_due() {
        for (name, store) in backends().await {
            let tenant = fresh_tenant("t");
            // Expired and unheld — due.
            store
                .register(
                    &artifact(
                        with_retention(
                            manifest(&tenant, &"1".repeat(64), 2, &["fin"]),
                            Some("2020-01-01T00:00:00Z"),
                            false,
                        ),
                        "ev-due",
                        EvidenceState::Committed,
                    ),
                    None,
                )
                .await
                .expect("register");
            // Expired but HELD — not due, which is the point of a hold.
            store
                .register(
                    &artifact(
                        with_retention(
                            manifest(&tenant, &"2".repeat(64), 2, &["fin"]),
                            Some("2020-01-01T00:00:00Z"),
                            true,
                        ),
                        "ev-held",
                        EvidenceState::Committed,
                    ),
                    None,
                )
                .await
                .expect("register");
            // Not yet expired — not due.
            store
                .register(
                    &artifact(
                        with_retention(
                            manifest(&tenant, &"3".repeat(64), 2, &["fin"]),
                            Some("2099-01-01T00:00:00Z"),
                            false,
                        ),
                        "ev-fresh",
                        EvidenceState::Committed,
                    ),
                    None,
                )
                .await
                .expect("register");
            // NO retention block at all — never due. An artifact nobody gave a
            // lifetime to is kept, not guessed at.
            store
                .register(
                    &artifact(
                        manifest(&tenant, &"4".repeat(64), 2, &["fin"]),
                        "ev-forever",
                        EvidenceState::Committed,
                    ),
                    None,
                )
                .await
                .expect("register");

            let due = store
                .purge_due("2026-08-28T12:00:00Z", 100)
                .await
                .expect("purge_due");
            let ids: Vec<&str> = due
                .iter()
                .filter(|a| a.tenant == tenant)
                .map(|a| a.evidence_id.as_str())
                .collect();
            assert_eq!(
                ids,
                vec!["ev-due"],
                "[{name}] only the expired, unheld artifact is due"
            );
        }
    }

    #[tokio::test]
    async fn a_purge_is_claimed_once() {
        // N-replica safety: the mark is conditional, so two sweepers cannot
        // both claim the same row.
        for (name, store) in backends().await {
            let tenant = fresh_tenant("t");
            store
                .register(
                    &artifact(
                        with_retention(
                            manifest(&tenant, &"1".repeat(64), 2, &["fin"]),
                            Some("2020-01-01T00:00:00Z"),
                            false,
                        ),
                        "ev-a",
                        EvidenceState::Committed,
                    ),
                    None,
                )
                .await
                .expect("register");
            let at = "2026-08-28T12:00:00.000Z";
            assert!(
                store.mark_purged(&tenant, "ev-a", at).await.expect("mark"),
                "[{name}] the first claim wins"
            );
            assert!(
                !store.mark_purged(&tenant, "ev-a", at).await.expect("mark"),
                "[{name}] the second claim must lose"
            );
            let back = store.get(&tenant, "ev-a").await.expect("get").expect("row");
            assert_eq!(back.state, EvidenceState::Purged, "[{name}]");
            assert!(
                back.manifest.retention.as_ref().unwrap().purged_at.is_some(),
                "[{name}] the row survives a purge WITH purged_at, so citations                  resolve evidence-expired rather than not-found"
            );
        }
    }

    #[tokio::test]
    async fn a_hold_can_be_placed_and_lifted_after_sealing() {
        for (name, store) in backends().await {
            let tenant = fresh_tenant("t");
            store
                .register(
                    &artifact(
                        with_retention(
                            manifest(&tenant, &"1".repeat(64), 2, &["fin"]),
                            Some("2020-01-01T00:00:00Z"),
                            false,
                        ),
                        "ev-a",
                        EvidenceState::Committed,
                    ),
                    None,
                )
                .await
                .expect("register");

            assert!(store
                .set_legal_hold(&tenant, "ev-a", true)
                .await
                .expect("hold"));
            let due = store
                .purge_due("2026-08-28T12:00:00Z", 100)
                .await
                .expect("due");
            assert!(
                !due.iter().any(|a| a.tenant == tenant),
                "[{name}] a hold placed after sealing must stop the janitor"
            );
            let back = store.get(&tenant, "ev-a").await.expect("get").expect("row");
            assert!(
                back.manifest.retention.as_ref().unwrap().legal_hold,
                "[{name}] the read must reflect the hold"
            );

            assert!(store
                .set_legal_hold(&tenant, "ev-a", false)
                .await
                .expect("hold"));
            let due = store
                .purge_due("2026-08-28T12:00:00Z", 100)
                .await
                .expect("due");
            assert!(
                due.iter().any(|a| a.tenant == tenant),
                "[{name}] lifting the hold makes it due again"
            );

            assert!(
                !store
                    .set_legal_hold(&tenant, "ev-nope", true)
                    .await
                    .expect("hold"),
                "[{name}] an unknown artifact reports false"
            );
        }
    }

    #[tokio::test]
    async fn accesses_come_back_newest_first_and_capped() {
        for (name, store) in backends().await {
            let tenant = fresh_tenant("t");
            let m = manifest(&tenant, &"1".repeat(64), 2, &["fin"]);
            store
                .register(&artifact(m, "ev-a", EvidenceState::Committed), None)
                .await
                .expect("register");

            for (i, outcome) in ["ok", "denied", "ok"].iter().enumerate() {
                store
                    .record_access(&EvidenceAccess {
                        evidence_id: "ev-a".into(),
                        tenant: tenant.clone(),
                        uid: format!("u{i}"),
                        kind: "manifest".into(),
                        row_from: None,
                        row_limit: None,
                        outcome: (*outcome).to_string(),
                        at: format!("2026-08-28T1{i}:00:00.000Z"),
                    })
                    .await
                    .expect("record");
            }
            let rows = store.accesses(&tenant, "ev-a", 2).await.expect("accesses");
            assert_eq!(rows.len(), 2, "[{name}] the limit is honored");
            assert_eq!(rows[0].uid, "u2", "[{name}] newest first");
        }
    }
}

// ---------------------------------------------------------------------------
// Daily token budget parity (spending caps, migration 0029)
//
// Same discipline as the evidence parity block above: the memory backend
// always runs, the Postgres half panics rather than skips when a database is
// configured and unreachable, and every scenario runs against both so a
// failure says which store disagreed.
// ---------------------------------------------------------------------------

mod budget {
    use super::{fresh_tenant, test_url};
    use munarium_core::budget::{BudgetOutcome, BudgetStore};
    use munarium_store_mem::MemBudgetStore;
    use munarium_store_pg::{PgBudgetStore, PgStore};

    async fn backends() -> Vec<(&'static str, Box<dyn BudgetStore>)> {
        let mut out: Vec<(&'static str, Box<dyn BudgetStore>)> =
            vec![("mem", Box::new(MemBudgetStore::new()))];
        match test_url() {
            Some(url) => {
                let base = PgStore::connect(&url, &fresh_tenant("bud")).await.expect(
                    "MUNARIUM_TEST_DATABASE_URL is set but the connection failed. This is a \
                     FAILURE, not a skip: a configured database that quietly falls back to \
                     memory is how a store tier reports green while testing nothing.",
                );
                out.push(("pg", Box::new(PgBudgetStore::new(base.pool().clone()))));
            }
            None => eprintln!(
                "NOTE: MUNARIUM_TEST_DATABASE_URL is unset — this assertion ran against the \
                 MEMORY store only. The Postgres half is UNTESTED in this run."
            ),
        }
        out
    }

    #[tokio::test]
    async fn grants_until_the_ceiling_then_refuses_with_remaining() {
        for (name, store) in backends().await {
            let tenant = fresh_tenant("bud-ceiling");
            let first = store
                .reserve(&tenant, "demo-anthropic", "frontier", 600, Some(1000))
                .await
                .unwrap();
            assert!(matches!(first, BudgetOutcome::Granted(_)), "[{name}]");
            match store
                .reserve(&tenant, "demo-anthropic", "frontier", 600, Some(1000))
                .await
                .unwrap()
            {
                BudgetOutcome::Exhausted {
                    requested,
                    remaining,
                    limit,
                } => {
                    assert_eq!(requested, 600, "[{name}]");
                    assert_eq!(remaining, 400, "[{name}]");
                    assert_eq!(limit, 1000, "[{name}]");
                }
                other => panic!("[{name}] expected Exhausted, got {other:?}"),
            }
            // A different tier of the same config is its own scope.
            assert!(
                matches!(
                    store
                        .reserve(&tenant, "demo-anthropic", "fast", 600, Some(1000))
                        .await
                        .unwrap(),
                    BudgetOutcome::Granted(_)
                ),
                "[{name}]"
            );
        }
    }

    #[tokio::test]
    async fn settle_corrects_to_actuals_and_release_refunds() {
        for (name, store) in backends().await {
            let tenant = fresh_tenant("bud-settle");
            let BudgetOutcome::Granted(r) = store
                .reserve(&tenant, "cfg", "capable", 900, Some(1000))
                .await
                .unwrap()
            else {
                panic!("[{name}] expected grant");
            };
            store.settle(&r, Some(100)).await.unwrap();
            assert!(
                matches!(
                    store
                        .reserve(&tenant, "cfg", "capable", 800, Some(1000))
                        .await
                        .unwrap(),
                    BudgetOutcome::Granted(_)
                ),
                "[{name}] settling to actuals must free the estimate's headroom"
            );
            let BudgetOutcome::Granted(r2) = store
                .reserve(&tenant, "cfg", "fast", 1000, Some(1000))
                .await
                .unwrap()
            else {
                panic!("[{name}] expected grant");
            };
            store.release(&r2).await.unwrap();
            assert!(
                matches!(
                    store
                        .reserve(&tenant, "cfg", "fast", 1000, Some(1000))
                        .await
                        .unwrap(),
                    BudgetOutcome::Granted(_)
                ),
                "[{name}] release must refund the whole reservation"
            );
        }
    }

    #[tokio::test]
    async fn unlimited_writes_nothing_and_ledger_groups() {
        for (name, store) in backends().await {
            let tenant = fresh_tenant("bud-ledger");
            assert_eq!(
                store
                    .reserve(&tenant, "cfg", "frontier", 100, None)
                    .await
                    .unwrap(),
                BudgetOutcome::Unlimited,
                "[{name}]"
            );
            assert!(store.ledger(&tenant).await.unwrap().is_empty(), "[{name}]");
            let BudgetOutcome::Granted(r) = store
                .reserve(&tenant, "cfg", "frontier", 70, Some(1000))
                .await
                .unwrap()
            else {
                panic!("[{name}] expected grant");
            };
            store.settle(&r, Some(50)).await.unwrap();
            let BudgetOutcome::Granted(_) = store
                .reserve(&tenant, "cfg", "frontier", 30, Some(1000))
                .await
                .unwrap()
            else {
                panic!("[{name}] expected grant");
            };
            let rows = store.ledger(&tenant).await.unwrap();
            assert_eq!(rows.len(), 1, "[{name}]");
            assert_eq!(rows[0].settled_units, 50, "[{name}]");
            assert_eq!(rows[0].held_units, 30, "[{name}]");
            assert_eq!(rows[0].reservations, 2, "[{name}]");
        }
    }

    /// The Matrix race, re-run against OUR ledger: ten concurrent requests
    /// for 2 units against a ceiling of 10 must grant exactly five. Matrix
    /// measured SIX grants (12 units) before its advisory lock landed; the
    /// memory store passes by construction (one mutex), the Postgres store
    /// passes because of `pg_advisory_xact_lock` — remove the lock and this
    /// test is the one that fails.
    #[tokio::test]
    async fn concurrent_reserves_never_oversubscribe_the_ceiling() {
        for (name, store) in backends().await {
            let store: std::sync::Arc<dyn BudgetStore> = std::sync::Arc::from(store);
            let tenant = fresh_tenant("bud-race");
            let mut handles = Vec::new();
            for _ in 0..10 {
                let store = store.clone();
                let tenant = tenant.clone();
                handles.push(tokio::spawn(async move {
                    store
                        .reserve(&tenant, "cfg", "frontier", 2, Some(10))
                        .await
                        .unwrap()
                }));
            }
            let mut granted = 0;
            for h in handles {
                if matches!(h.await.unwrap(), BudgetOutcome::Granted(_)) {
                    granted += 1;
                }
            }
            assert_eq!(
                granted, 5,
                "[{name}] ceiling 10 / requests of 2: exactly five grants"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Per-call output-token budgets (max-tokens, migration 0031) — Postgres only:
// the memory store keeps its replacements in the server's registry, so there
// is no second backend to hold to parity here. Skips (silently, like the
// other pg-only tests) when no database is configured.
// ---------------------------------------------------------------------------

mod max_tokens {
    use super::{fresh_tenant, test_url};
    use munarium_store_pg::{PgMaxTokensStore, PgStore};

    #[tokio::test]
    async fn replace_is_a_whole_object_round_trip_never_a_merge() {
        let Some(url) = test_url() else { return };
        let base = PgStore::connect(&url, &fresh_tenant("mxt"))
            .await
            .expect("MUNARIUM_TEST_DATABASE_URL is set but the connection failed");
        let store = PgMaxTokensStore::new(base.pool().clone());
        let tenant = fresh_tenant("mxt-row");
        assert!(store.get(&tenant).await.unwrap().is_none());

        let first = serde_json::json!({
            "turn_completion": 4096, "query_expansion": 256, "complete_default": 1024,
            "healthai_probe": 512, "hierarchy_classifier": 32, "hierarchy_intent": 480,
            "runbook_advisory": 2048, "authoring_assist": 8192
        });
        let at1 = store.replace(&tenant, &first).await.unwrap();
        let (got, at) = store
            .get(&tenant)
            .await
            .unwrap()
            .expect("row after replace");
        assert_eq!(got, first);
        assert_eq!(at, at1);
        assert!(at1.ends_with('Z') && at1.contains('T'), "rfc3339: {at1}");

        // A second replace overwrites the WHOLE object: a field absent from
        // the new value is gone, not carried over.
        let second = serde_json::json!({ "turn_completion": 512 });
        let at2 = store.replace(&tenant, &second).await.unwrap();
        let (got, at) = store
            .get(&tenant)
            .await
            .unwrap()
            .expect("row after overwrite");
        assert_eq!(got, second);
        assert_eq!(at, at2);
        assert!(at2 >= at1, "updated_at moves forward: {at1} -> {at2}");

        // Another tenant is untouched.
        assert!(store
            .get(&fresh_tenant("mxt-other"))
            .await
            .unwrap()
            .is_none());
    }
}
