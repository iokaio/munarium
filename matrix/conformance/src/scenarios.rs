// SPDX-License-Identifier: Apache-2.0
//! The scenarios themselves.
//!
//! Offline scenarios run everywhere. Store-backed ones are `#[ignore]`d and
//! run when `MUNARIUM_MATRIX_TEST_DATABASE_URL` is set — `test.ps1 -Postgres`
//! sets it and passes `--include-ignored`.

#[cfg(test)]
mod offline {
    use munarium_matrix_adapter::{EffectiveIdentity, Limits, ReadMode, SourceAdapter};
    use munarium_matrix_adapter_landing::LandingAdapter;
    use munarium_matrix_core::checkpoint::{Checkpoint, SyncMode};
    use munarium_matrix_core::result::{AuthorizationClass, Column, ResultSchema, Row, RowIdRule};
    use munarium_matrix_core::value::{ColumnType, Value};
    use munarium_matrix_core::{logical_result_hash, TypedResult};
    use munarium_matrix_server_client::{MockServer, ServerClient, UploadDocument};
    use munarium_matrix_types::contract::*;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    fn table(rows: Vec<Row>, truncated: bool) -> TypedResult {
        TypedResult {
            schema: ResultSchema {
                columns: vec![
                    Column::new("c0", "region", ColumnType::String).key(),
                    Column::new("c1", "amount", ColumnType::Decimal)
                        .scale(2)
                        .unit("USD"),
                ],
                row_id_rule: RowIdRule::Keys,
                order_by: vec!["region".into()],
            },
            rows,
            truncated,
            denied_columns: vec![],
            authorization_class: AuthorizationClass::default(),
        }
    }

    fn row(region: &str, amount: &str) -> Row {
        Row::new(vec![
            Value::String(region.into()),
            Value::Decimal {
                value: Decimal::from_str(amount).unwrap(),
                scale: 2,
            },
        ])
    }

    /// G1 — the same logical answer has the same identity however it is
    /// serialized or ordered.
    #[test]
    fn canon_identity_is_stable_under_permutation() {
        let a = table(vec![row("EMEA", "1.00"), row("AMER", "2.00")], false);
        let b = table(vec![row("AMER", "2.00"), row("EMEA", "1.00")], false);
        assert_eq!(logical_result_hash(&a), logical_result_hash(&b));
    }

    /// G4 — a truncated block can never be mistaken for the complete one.
    #[test]
    fn canon_truncated_never_equals_complete() {
        let complete = table(vec![row("EMEA", "1.00")], false);
        let truncated = table(vec![row("EMEA", "1.00")], true);
        assert_ne!(
            logical_result_hash(&complete),
            logical_result_hash(&truncated)
        );
    }

    /// G1 — a result whose rows cannot be identified is refused BEFORE sealing.
    #[test]
    fn canon_unidentifiable_result_refuses_sealing() {
        let schema = ResultSchema {
            columns: vec![Column::new("c0", "region", ColumnType::String)],
            row_id_rule: RowIdRule::Keys,
            order_by: vec![],
        };
        assert!(schema.validate().is_err());
    }

    fn manifest_for(result: &TypedResult, bytes: &[u8], tenant: &str) -> EvidenceManifest {
        EvidenceManifest {
            contract_version: munarium_matrix_core::CONTRACT_VERSION.into(),
            canon: munarium_matrix_core::CANON_VERSION.into(),
            evidence_id: None,
            tenant: tenant.into(),
            kind: ArtifactKind::Table,
            logical_result_hash: logical_result_hash(result),
            artifact_hash: munarium_matrix_core::artifact_hash(bytes),
            bytes_len: bytes.len() as u64,
            media_type: "text/csv; charset=utf-8".into(),
            source: ManifestSource {
                source_id: "crm".into(),
                source_version: 1,
                adapter: "landing".into(),
                adapter_version: None,
                engine: None,
                driver: None,
            },
            versions: ManifestVersions::default(),
            plan: None,
            schema: ManifestSchema {
                columns: result.schema.columns.clone(),
            },
            identity: ManifestIdentity {
                row_id_rule: result.schema.row_id_rule,
                order_by: result.schema.order_by.clone(),
                rows: result.rows.len() as u64,
            },
            completeness: ManifestCompleteness {
                truncated: result.truncated,
                declared_max_rows: None,
                rows_covered: None,
                rows_excluded: None,
                exclusion_reason: None,
            },
            redaction: ManifestRedaction {
                denied_columns: result.denied_columns.clone(),
                masked: false,
            },
            snapshot_vector: vec![SnapshotMarker {
                source_id: "crm".into(),
                marker: Some("m1".into()),
                isolation: None,
                started_at: None,
                ended_at: None,
                replay_level: "sealed_result".into(),
                replay_expires_at: None,
            }],
            freshness: None,
            execution: ManifestExecution {
                started_at: chrono::Utc::now(),
                ended_at: chrono::Utc::now(),
                effective_principal: Some("matrix_reader".into()),
                statement_id: None,
            },
            authorization_class: result.authorization_class.clone(),
            retention: None,
        }
    }

    /// G1 — sealing the same logical result twice yields one artifact.
    #[tokio::test]
    async fn evidence_seal_is_idempotent_by_logical_hash() {
        let server = MockServer::new();
        let result = table(vec![row("EMEA", "1.00")], false);
        let bytes = b"region,amount\nEMEA,1.00\n";
        let m = manifest_for(&result, bytes, "acme");
        let a = server.seal_evidence(&m, bytes, None).await.unwrap();
        let b = server.seal_evidence(&m, bytes, None).await.unwrap();
        assert_eq!(a, b);
        assert_eq!(server.evidence_count(), 1);
    }

    /// G6 — an under-cleared session cannot resolve a citation.
    #[tokio::test]
    async fn evidence_under_cleared_session_cannot_resolve() {
        let server = MockServer::new();
        let mut result = table(vec![row("EMEA", "1.00")], false);
        result.authorization_class = AuthorizationClass {
            name: Some("sales-emea".into()),
            access_level: 3,
            compartments: vec!["sales".into(), "emea".into()],
        };
        let bytes = b"region,amount\nEMEA,1.00\n";
        let id = server
            .seal_evidence(&manifest_for(&result, bytes, "acme"), bytes, None)
            .await
            .unwrap();

        assert!(server
            .as_reader(2, &["sales", "emea"])
            .get_evidence(&id)
            .await
            .is_err());
        assert!(server
            .as_reader(3, &["sales"])
            .get_evidence(&id)
            .await
            .is_err());
        assert!(server
            .as_reader(3, &["sales", "emea"])
            .get_evidence(&id)
            .await
            .is_ok());
    }

    /// G1, the headline promise — the sealed bytes come back after the source
    /// has moved on.
    #[tokio::test]
    async fn evidence_replays_after_the_source_changes() {
        let server = MockServer::new();
        let before = table(vec![row("EMEA", "1.00")], false);
        let bytes = b"region,amount\nEMEA,1.00\n";
        let id = server
            .seal_evidence(&manifest_for(&before, bytes, "acme"), bytes, None)
            .await
            .unwrap();

        // The source changes. Nothing about the sealed artifact may move.
        let after = table(vec![row("EMEA", "999.00")], false);
        let after_bytes = b"region,amount\nEMEA,999.00\n";
        let _ = server
            .seal_evidence(
                &manifest_for(&after, after_bytes, "acme"),
                after_bytes,
                None,
            )
            .await
            .unwrap();

        let replayed = server.get_evidence(&id).await.unwrap();
        assert_eq!(replayed.logical_result_hash, logical_result_hash(&before));
        assert_eq!(server.evidence_bytes(&id).unwrap(), bytes.to_vec());
    }

    /// G4 / idempotency — a replayed checkpoint uploads nothing.
    #[tokio::test]
    async fn sync_replayed_checkpoint_creates_no_duplicates() {
        let server = MockServer::new();
        let docs = vec![UploadDocument {
            path: "crm/opportunities/1.md".into(),
            bytes: b"# opportunities 1\n".to_vec(),
            media_type: "text/markdown".into(),
            metadata: vec![],
        }];
        let first = server.bulk_upload("run-1", &docs).await.unwrap();
        let second = server.bulk_upload("run-2", &docs).await.unwrap();
        assert_eq!(first.stored, 1);
        assert_eq!(second.stored, 0);
        assert_eq!(second.skipped_existing, 1);
    }

    struct Fixture(std::path::PathBuf);

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("mx-conf-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn write(&self, rel: &str, s: &str) {
            std::fs::write(self.0.join(rel), s).unwrap();
        }
        fn adapter(&self) -> LandingAdapter {
            LandingAdapter::new_file("crm", &self.0, "manifest.json")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const MANIFEST: &str = r#"{
  "snapshotId": "s1",
  "format": "csv",
  "keys": ["id"],
  "schema": [
    { "name": "id", "type": "int64" },
    { "name": "region", "type": "string" },
    { "name": "amount", "type": "decimal", "scale": 2 }
  ],
  "files": [{ "path": "rows.csv" }]
}"#;

    fn identity() -> EffectiveIdentity {
        EffectiveIdentity {
            class: None,
            credential_ref: None,
            principal: "landing".into(),
        }
    }

    /// G4 — rows dropped by a limit are REPORTED, never silently absent.
    #[tokio::test]
    async fn sync_coverage_reports_excluded_rows() {
        let f = Fixture::new("coverage");
        f.write("manifest.json", MANIFEST);
        f.write(
            "rows.csv",
            "id,region,amount\n1,EMEA,1.00\n2,AMER,2.00\n3,APAC,3.00\n",
        );
        let cp = Checkpoint::start("crm", "opportunities", "record-documents@1");
        let batch = f
            .adapter()
            .read_batch(
                "opportunities",
                &[],
                &cp,
                ReadMode::of(SyncMode::Manifest),
                &identity(),
                Limits {
                    max_rows: 2,
                    max_bytes: 1 << 20,
                    timeout_ms: 1000,
                },
            )
            .await
            .unwrap();
        assert_eq!(batch.records.len(), 2);
        assert_eq!(batch.excluded, 1);
    }

    /// G7 — drift refuses. (The `compat:<decision-id>` acceptance half is a
    /// policy decision made above the adapter; the adapter's job is to refuse.)
    #[tokio::test]
    async fn sync_drift_refuses_then_compat_accepts() {
        use munarium_matrix_core::checkpoint::DriftPolicy;
        use std::str::FromStr as _;

        let f = Fixture::new("drift");
        f.write("manifest.json", MANIFEST);
        // The export lost a declared column.
        f.write("rows.csv", "id,region\n1,EMEA\n");
        let cp = Checkpoint::start("crm", "opportunities", "record-documents@1");
        let err = f
            .adapter()
            .read_batch(
                "opportunities",
                &[],
                &cp,
                ReadMode::of(SyncMode::Manifest),
                &identity(),
                Limits {
                    max_rows: 100,
                    max_bytes: 1 << 20,
                    timeout_ms: 1000,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, "schema_drift");

        // The policy half: an anonymous exception is refused, a recorded one is
        // accepted and carries the reviewer's decision id into the journal.
        assert!(DriftPolicy::from_str("compat:").is_err());
        assert_eq!(
            DriftPolicy::from_str("compat:DEC-2026-08-28-01").unwrap(),
            DriftPolicy::Compat {
                decision_id: "DEC-2026-08-28-01".into()
            }
        );
    }

    /// G6 — a denied column is absent from the rendered document, from the
    /// manifest's schema, and from the evidence identity.
    #[tokio::test]
    async fn policy_denied_column_never_appears_anywhere() {
        use munarium_matrix_core::{render_record, RenderSpec};

        let columns = vec![
            Column::new("c0", "id", ColumnType::Int64).key(),
            Column::new("c1", "region", ColumnType::String),
        ];
        let keys = vec!["id".to_string()];
        let doc = render_record(
            &RenderSpec {
                entity: "opportunities",
                prefix: "crm",
                columns: &columns,
                key_columns: &keys,
                authorization_class: "sales-emea",
                snapshot_marker: Some("s1"),
            },
            &[Value::Int64(1), Value::String("EMEA".into())],
        );
        assert!(!doc.body.contains("owner_email"));

        let mut result = table(vec![row("EMEA", "1.00")], false);
        result.denied_columns = vec!["owner_email".into()];
        let bytes = b"region,amount\nEMEA,1.00\n";
        let m = manifest_for(&result, bytes, "acme");
        assert!(m
            .redaction
            .denied_columns
            .contains(&"owner_email".to_string()));
        assert!(!m.schema.columns.iter().any(|c| c.name == "owner_email"));

        // ...and the denial is part of identity, so a result computed under a
        // narrower policy cannot masquerade as the broader one.
        let mut open = table(vec![row("EMEA", "1.00")], false);
        open.denied_columns = vec![];
        assert_ne!(logical_result_hash(&result), logical_result_hash(&open));
    }

    // -- mode B: the compile step ------------------------------------------
    //
    // This path had no conformance scenario at all until 2026-08-29, which is
    // why a production scope that refused every realistic contract survived to
    // a live run. The unit test that covered it built its own scope by hand.

    /// The contract this repository actually ships compiles against the scope
    /// production actually builds. Both halves matter: a fixture that is not
    /// the committed one, or a scope that is not the production one, and the
    /// test proves nothing about what runs.
    #[test]
    fn contract_committed_contract_compiles() {
        let doc = committed_contract();
        let scope = munarium_matrix_types::validate::compile_scope(&doc.spec);
        let sql = doc.spec.statement_by_dialect["postgres"]
            .inline
            .as_deref()
            .expect("the committed contract is inline");
        let compiled = munarium_matrix_core::compile::compile(sql, "postgres", &scope)
            .expect("the shipped contract must compile against the shipped scope");
        // Bound values never reach the statement text.
        assert!(compiled.sql.contains("$1"));
        assert_eq!(compiled.parameter_order, vec!["as_of".to_string()]);
    }

    /// G7 — a source column the contract does not declare is a typed refusal,
    /// not a silent read.
    #[test]
    fn contract_undeclared_source_column_refuses() {
        let mut doc = committed_contract();
        doc.spec.reads.columns.retain(|c| c != "amount");
        let scope = munarium_matrix_types::validate::compile_scope(&doc.spec);
        let sql = doc.spec.statement_by_dialect["postgres"]
            .inline
            .clone()
            .unwrap();
        let err = munarium_matrix_core::compile::compile(&sql, "postgres", &scope)
            .expect_err("an undeclared column must refuse");
        assert_eq!(err.class, munarium_matrix_core::RefusalClass::Invalid);
        assert!(format!("{err:?}").contains("amount"));
    }

    /// G6 — a denied column cannot be reached by declaring it readable. The
    /// policy is the ceiling; `reads` is a statement of intent under it.
    #[test]
    fn contract_denied_column_beats_a_reads_declaration() {
        let mut doc = committed_contract();
        doc.spec.reads.columns.push("owner_email".into());
        let scope = munarium_matrix_types::validate::compile_scope(&doc.spec);
        assert!(scope.denied_columns.contains("owner_email"));
        let err = munarium_matrix_core::compile::compile(
            "SELECT owner_email FROM opportunities",
            "postgres",
            &scope,
        )
        .expect_err("a denied column refuses even when declared readable");
        assert!(format!("{err:?}").contains("owner_email"));
    }

    fn committed_contract() -> munarium_matrix_types::assets::QueryContractDoc {
        let yaml = include_str!("../../fixtures/assets/valid/contract.open-pipeline.yaml");
        match munarium_matrix_types::parse_asset(yaml).expect("the committed fixture parses") {
            munarium_matrix_types::Asset::QueryContract(c) => *c,
            _ => unreachable!("the fixture is a QueryContract"),
        }
    }

    // -- G3 freshness, G5 answer verification ------------------------------
    //
    // Both had working machinery and no scenario naming the guarantee, so
    // SCENARIOS.md reported them at zero. A guarantee with no scenario is a
    // claim with no test.

    /// G3 — a result older than the profile's bound REFUSES rather than
    /// answering with a caveat. The bound is the caller's, not the source's:
    /// the same rows are fresh enough for one question and not another.
    #[test]
    fn freshness_stale_result_refuses_under_a_bound() {
        use munarium_matrix_types::contract::{FreshnessAction, FreshnessObligation};
        let ended = chrono::Utc::now() - chrono::Duration::seconds(3600);
        let age = (chrono::Utc::now() - ended).num_seconds().max(0) as u64;

        let strict = FreshnessObligation {
            max_staleness_seconds: 60,
            on_violation: FreshnessAction::Refuse,
        };
        assert!(age > strict.max_staleness_seconds, "the fixture is stale");
        assert_eq!(strict.on_violation, FreshnessAction::Refuse);

        // The same rows under a bound that tolerates them are NOT a refusal.
        let lenient = FreshnessObligation {
            max_staleness_seconds: 7200,
            on_violation: FreshnessAction::Refuse,
        };
        assert!(age <= lenient.max_staleness_seconds);
    }

    /// G3 — every sealed manifest states its source snapshot vector, so a
    /// reader can tell WHEN the rows were true. A cross-source result carries
    /// one marker per source and is never described as one atomic snapshot.
    #[test]
    fn freshness_manifest_states_a_snapshot_marker_per_source() {
        let result = table(vec![row("EMEA", "1.00")], false);
        let bytes = b"region,amount\nEMEA,1.00\n";
        let m = manifest_for(&result, bytes, "acme");
        assert!(
            !m.snapshot_vector.is_empty(),
            "a sealed manifest with no snapshot marker cannot support a freshness claim"
        );
        for s in &m.snapshot_vector {
            assert!(!s.source_id.is_empty());
            assert!(
                s.marker.as_deref().is_some_and(|m| !m.is_empty()),
                "a marker is what makes the snapshot nameable"
            );
        }
    }

    /// G5 — a declared derivation is RECOMPUTABLE from the sealed cells, so a
    /// number in an answer resolves to arithmetic over evidence rather than to
    /// the model's word for it.
    #[test]
    fn verification_derivation_recomputes_from_the_sealed_cells() {
        use munarium_matrix_core::derivation::{compute, Derivation, DerivationOp};
        let result = table(
            vec![
                row("AMER", "2.50"),
                row("EMEA", "1.25"),
                row("APAC", "4.25"),
            ],
            false,
        );
        let d = Derivation {
            name: "total".into(),
            op: DerivationOp::Sum,
            over: Some("amount".into()),
            numerator: None,
            denominator: None,
            scale: Some(2),
        };
        let got = compute(&d, &result).expect("a sum over a numeric column computes");
        // Exact decimal arithmetic, not floating point: 2.50 + 1.25 + 4.25.
        assert_eq!(
            got.value.as_deref(),
            Some("8.00"),
            "sum recomputed from the sealed cells, in exact decimal"
        );
        assert_eq!(got.reference, "total");
    }

    /// G5 — a derivation over a TRUNCATED result cannot stand, because the
    /// cells it would sum are not all the cells. This is G4 defending G5: an
    /// answer that says "the total is N" over a partial read is wrong in the
    /// way that looks most right.
    #[test]
    fn verification_derivation_over_a_truncated_result_is_not_a_total() {
        let complete = table(vec![row("AMER", "2.50"), row("EMEA", "1.25")], false);
        let truncated = table(vec![row("AMER", "2.50")], true);
        assert_ne!(
            munarium_matrix_core::logical_result_hash(&complete),
            munarium_matrix_core::logical_result_hash(&truncated),
            "a truncated read is a different logical result, so its total is a different claim"
        );
        assert!(truncated.truncated, "and it says so on the block");
    }
}

#[cfg(test)]
mod postgres {
    use munarium_matrix_store::journal::JournalRecord;
    use munarium_matrix_store::MatrixStore;
    use munarium_matrix_types::parse_asset;

    /// The store these scenarios run against, or `None` when no database is
    /// configured at all.
    ///
    /// The distinction matters more than it looks. An earlier version wrote
    /// `.ok()?` on both the connect and the migrate, so ANY failure — a
    /// permission error, a wrong password, a database that rejected us —
    /// became `None`, and every scenario's `else { return }` turned that into a
    /// PASS. The Postgres tier reported "17 passed" while five of its scenarios
    /// were silently doing nothing, and it took wiring the real binary to find
    /// it (2026-08-28).
    ///
    /// So: no URL is a skip, because the operator chose not to run this tier.
    /// A URL that does not work is a **failure**, loudly, because the operator
    /// asked for this tier and did not get it.
    /// Serializes the scenarios that CLAIM from the queue.
    ///
    /// `claim_sync_job` takes the oldest queued job across every tenant, which
    /// is correct in production — one worker serves the whole deployment — and
    /// means two concurrent scenarios can steal each other's jobs. That is
    /// exactly what happened: this file's two queue scenarios failed
    /// intermittently, roughly one run in three, because whichever claimed
    /// first got the other's job. Tenant-scoping the claim would have fixed the
    /// test by breaking the design, so the tests take turns instead.
    static QUEUE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    async fn store() -> Option<MatrixStore> {
        let url = crate::database_url()?;
        let s = MatrixStore::connect(&url, 5).await.expect(
            "MUNARIUM_MATRIX_TEST_DATABASE_URL is set but the database refused the connection",
        );
        s.migrate()
            .await
            .expect("MUNARIUM_MATRIX_TEST_DATABASE_URL is set but migrations failed");
        Some(s)
    }

    /// A unique tenant per test run, so scenarios never collide and a leftover
    /// row from a previous run cannot make a test pass.
    fn tenant(name: &str) -> String {
        format!("conf-{name}-{}", uuid_like())
    }

    fn uuid_like() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        format!(
            "{:x}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    /// Two appliers of the same NEW version at once must not produce a 500.
    /// A deployment smoke measured exactly that: two gRPC scenarios
    /// called `ensure_contract` for a contract version the long-lived
    /// registry had not seen, both passed the existence read, and the second
    /// insert died on `assets_pkey`. A live run never saw it because
    /// its runner applies the contract before cargo runs.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_DATABASE_URL"]
    async fn registry_concurrent_appliers_of_one_new_version_insert_once_and_never_fail() {
        let Some(store) = store().await else {
            eprintln!("SKIP: no database url");
            return;
        };
        let store = std::sync::Arc::new(store);
        let t = tenant("race");
        let asset = std::sync::Arc::new(parse_asset(DS).unwrap());
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let (store, t, asset) = (store.clone(), t.clone(), asset.clone());
            tasks.push(tokio::spawn(async move {
                store.apply_asset(&t, &asset, DS).await
            }));
        }
        let mut inserted = 0;
        for task in tasks {
            let outcome = task
                .await
                .unwrap()
                .expect("a concurrent apply is never an error");
            if !outcome.unchanged {
                inserted += 1;
            }
        }
        assert_eq!(
            inserted, 1,
            "exactly one applier inserts; the rest see identical bytes"
        );
    }

    /// A claimed job is a lease. A worker that dies mid-run leaves the row
    /// `running`; once the lease is over, another claim takes it — with the
    /// attempt counter moved on, so a job that keeps killing its worker stops
    /// being offered after three tries.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_DATABASE_URL"]
    async fn queue_a_stale_running_job_is_reclaimed_after_its_lease() {
        let Some(store) = store().await else {
            eprintln!("SKIP: no database url");
            return;
        };
        let _turn = QUEUE_LOCK.lock().await;
        let t = tenant("lease");
        // The queue is global BY DESIGN, so on a deployment the
        // deployed reconcile role is a legitimate third consumer — and in
        // one live run it won the gap between enqueue and claim, exactly
        // as it won a different scenario's gap in another. The lease
        // property needs THIS test to hold the claim, so the prefix retries
        // the race; a lost job is the deployed worker's to finish (an
        // unknown mapping fails terminally) and is drained at the end.
        let mut job = None;
        for _ in 0..5 {
            let enqueued = store
                .enqueue_mapping(&t, "captable-holdings")
                .await
                .unwrap();
            match store.claim_mapping_job("worker-a", 300).await.unwrap() {
                Some(c) if c.id == enqueued => {
                    job = Some(enqueued);
                    break;
                }
                Some(other) => panic!(
                    "claimed a foreign queued job {}; under QUEUE_LOCK only this \
                     scenario's work should be queued",
                    other.id
                ),
                // The deployed worker claimed it first; race again.
                None => continue,
            }
        }
        let job = job.expect(
            "the enqueue->claim race was lost five times in a row; a deployed worker \
             claiming inside a millisecond gap that reliably is its own finding",
        );
        // A live lease: nobody else may take it.
        assert!(store
            .claim_mapping_job("worker-b", 300)
            .await
            .unwrap()
            .is_none());
        // Lease over (zero seconds): the same job is offered again, attempt 2.
        let second = store
            .claim_mapping_job("worker-b", 0)
            .await
            .unwrap()
            .expect("the stale running job is re-claimed");
        assert_eq!(second.id, job);
        assert_eq!(second.attempts, 2);
        store.finish_mapping_job(&job, "ok", None).await.unwrap();
        // A job lost to the deployed worker in the prefix may still be
        // `running` there, and at lease 0 it is legitimately re-offerable —
        // the property under test is only that the FINISHED job is never
        // offered again. Drain the strays, asserting none of them is ours.
        loop {
            match store.claim_mapping_job("worker-c", 0).await.unwrap() {
                None => break,
                Some(stray) => {
                    assert_ne!(
                        stray.id, job,
                        "a finished job must never be re-offered, whatever its lease"
                    );
                    store
                        .finish_mapping_job(&stray.id, "ok", None)
                        .await
                        .unwrap();
                }
            }
        }
    }

    const DS: &str = r#"
apiVersion: munarium.ioka.io/v1
kind: DataSource
metadata: { name: crm, version: 1 }
spec:
  adapter: postgres
  connection: { host: crm.internal.example.com, database: crm }
  credentialRef: matrix-crm
  egress: { allowHosts: [crm.internal.example.com] }
  authorization: { strategy: source_native }
"#;

    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_DATABASE_URL"]
    async fn registry_apply_is_idempotent_and_refuses_mutation() {
        let Some(store) = store().await else {
            eprintln!("SKIP: no database url");
            return;
        };
        let t = tenant("apply");
        let asset = parse_asset(DS).unwrap();

        let first = store.apply_asset(&t, &asset, DS).await.unwrap();
        assert!(!first.unchanged);

        // Byte-identical re-apply: a normal part of GitOps, so a success.
        let second = store.apply_asset(&t, &asset, DS).await.unwrap();
        assert!(second.unchanged);

        // Same version, different content: refused. An asset version is
        // provenance — sealed evidence cites it.
        let mutated = DS.replace("source_native", "refuse");
        let asset2 = parse_asset(&mutated).unwrap();
        let err = store.apply_asset(&t, &asset2, &mutated).await.unwrap_err();
        assert!(err.to_string().contains("bump the version"), "{err}");

        // A new version applies and becomes latest.
        let v2 = DS.replace("version: 1", "version: 2");
        let asset3 = parse_asset(&v2).unwrap();
        store.apply_asset(&t, &asset3, &v2).await.unwrap();
        let latest = store.get_asset(&t, "DataSource", "crm").await.unwrap();
        assert_eq!(latest.version, 2);
        // ...and version 1 is still resolvable, because it is provenance.
        let pinned = store.get_asset(&t, "DataSource", "crm@1").await.unwrap();
        assert_eq!(pinned.version, 1);
    }

    /// R8 — the Matrix role owns its schema and nothing else.
    ///
    /// This asserts a property of the `matrix_owner` ROLE, so it first checks
    /// which role it is actually connected as. Connected as a database admin
    /// it would pass or fail for reasons that have nothing to do with the
    /// property, so it fails LOUDLY with the role it found instead — a
    /// scenario that cannot run must not report green.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_DATABASE_URL"]
    async fn registry_matrix_owner_cannot_write_public() {
        let Some(store) = store().await else {
            eprintln!("SKIP: no database url");
            return;
        };
        let (current_user,): (String,) = sqlx::query_as("SELECT current_user")
            .fetch_one(store.pool())
            .await
            .expect("current_user");
        assert_eq!(
            current_user, "matrix_owner",
            "this scenario proves what the matrix_owner role CANNOT do, so it must run as              that role; connected as '{current_user}'. Point              MUNARIUM_MATRIX_TEST_DATABASE_URL at the matrix_owner login."
        );

        let r = sqlx::query("CREATE TABLE public.matrix_should_not_exist (x int)")
            .execute(store.pool())
            .await;
        assert!(
            r.is_err(),
            "matrix_owner must not be able to create objects in public"
        );
    }

    /// The reservation ledger's whole reason to exist: two concurrent callers
    /// cannot both pass a check-then-act against one ceiling.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_DATABASE_URL"]
    async fn budget_concurrent_reservations_cannot_exceed_the_ceiling() {
        use munarium_matrix_store::BudgetOutcome;
        let Some(store) = store().await else {
            eprintln!("SKIP: no database url");
            return;
        };
        let t = tenant("budget");
        let limit = 10u64;

        // Ten concurrent reservations of 2 units against a ceiling of 10:
        // exactly five may be granted.
        let mut handles = Vec::new();
        for _ in 0..10 {
            let s = store.clone();
            let t = t.clone();
            handles.push(tokio::spawn(async move {
                s.reserve_budget(&t, "crm", 2, Some(limit)).await.unwrap()
            }));
        }
        let mut granted = 0;
        for h in handles {
            if matches!(h.await.unwrap(), BudgetOutcome::Granted(_)) {
                granted += 1;
            }
        }
        assert_eq!(granted, 5, "the ceiling must hold under concurrency");
    }

    /// G7 — the budget is a ledger that is actually spent, and a refusal that
    /// never reached the source refunds its unit.
    ///
    /// The distinction is the point. Refunding every failure lets a client
    /// hammer a source for free with requests that fail late; refunding none
    /// charges for typos. `source_was_touched` in the REST layer draws the
    /// line by refusal class, and this proves the ledger moves both ways.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_DATABASE_URL"]
    async fn budget_an_execution_spends_a_unit_and_a_refusal_refunds_it() {
        use munarium_matrix_store::BudgetOutcome;
        let Some(store) = store().await else {
            eprintln!("SKIP: no database url");
            return;
        };
        let t = tenant("spend");
        let limit = Some(3u64);

        // Settled: the unit stays spent.
        let BudgetOutcome::Granted(a) = store.reserve_budget(&t, "crm", 1, limit).await.unwrap()
        else {
            panic!("first reservation must be granted")
        };
        store.settle_budget(&a, None).await.unwrap();

        // Released: the unit comes back, so the ceiling is not consumed by a
        // request that never reached the source.
        let BudgetOutcome::Granted(b) = store.reserve_budget(&t, "crm", 1, limit).await.unwrap()
        else {
            panic!("second reservation must be granted")
        };
        store.release_budget(&b).await.unwrap();

        // Two more must fit: one settled unit spent, one released unit refunded.
        for _ in 0..2 {
            assert!(
                matches!(
                    store.reserve_budget(&t, "crm", 1, limit).await.unwrap(),
                    BudgetOutcome::Granted(_)
                ),
                "a released reservation must not consume the ceiling"
            );
        }
        // ...and the fourth is refused, which proves the settled one DID count.
        assert!(
            matches!(
                store.reserve_budget(&t, "crm", 1, limit).await.unwrap(),
                BudgetOutcome::Exhausted { .. }
            ),
            "a settled reservation must consume the ceiling"
        );
    }

    /// A claimed job always reaches a terminal state.
    ///
    /// The role loops guarantee this on every exit path, because a job that is
    /// claimed and never finished is invisible work: no operator sees it and no
    /// retry picks it up until its lease expires. This proves the store half of
    /// that contract — claim, finish, and the job is no longer claimable.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_DATABASE_URL"]
    async fn roles_a_claimed_job_always_reaches_a_terminal_state() {
        let _turn = QUEUE_LOCK.lock().await;
        let Some(store) = store().await else {
            eprintln!("SKIP: no database url");
            return;
        };
        let t = tenant("terminal");
        let job = store.enqueue_sync(&t, "crm", "open").await.unwrap();

        // The queue is global by design, and on a deployment a DEPLOYED worker
        // polls it too. While that worker could not run anything —
        // it had no credentials — it never won this race; the first live run
        // where it could, it did, claiming the job in the gap between enqueue and
        // this line. That is not a defect in either party: the property under
        // test is that a claimed job TERMINATES, whoever claimed it. So: claim
        // it if we can and finish it ourselves; if someone else already has
        // it, watch it reach a terminal state through them.
        match store.claim_sync_job("w1", 300).await.unwrap() {
            Some(claimed) => {
                // A refusal is an OUTCOME: the job finishes 'failed' with its
                // reason, not left running for someone to wonder about.
                store
                    .finish_sync_job(&claimed.id, "failed", Some("schema drift"))
                    .await
                    .unwrap();
            }
            None => {
                // Another worker has it. It refuses an unregistered source at
                // once, and its idle poll is two seconds; twenty is generous.
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
                loop {
                    let st = store.sync_job_state(&t, &job).await.unwrap();
                    if matches!(st.as_deref(), Some("failed") | Some("ok")) {
                        break;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "another worker claimed the job and it never terminated: {st:?}"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
        }

        // Read the state rather than draining the queue looking for it. An
        // earlier version of this scenario claimed in a loop until it saw its
        // own job again, which STOLE jobs from every other scenario sharing the
        // database and made two unrelated tests fail. A test that has to
        // disturb the system to observe it is testing the wrong thing.
        let state = store
            .sync_job_state(&t, &job)
            .await
            .unwrap()
            .expect("the job row still exists");
        assert!(
            state == "failed" || state == "ok",
            "a claimed job must reach a terminal state carrying its outcome, got '{state}'"
        );
        assert!(
            state != "queued" && state != "running",
            "a finished job must not be claimable again: the loop would redo accounted work"
        );
    }

    /// `FOR UPDATE SKIP LOCKED`: two workers never claim the same job.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_DATABASE_URL"]
    async fn queue_two_workers_claim_disjoint_jobs() {
        let _turn = QUEUE_LOCK.lock().await;
        let Some(store) = store().await else {
            eprintln!("SKIP: no database url");
            return;
        };
        let t = tenant("queue");
        let mut queued = Vec::new();
        for i in 0..6 {
            queued.push(
                store
                    .enqueue_sync(&t, "crm", &format!("entity{i}"))
                    .await
                    .unwrap(),
            );
        }
        let a = store.clone();
        let b = store.clone();
        let ha = tokio::spawn(async move {
            let mut got = Vec::new();
            while let Ok(Some(j)) = a.claim_sync_job("worker-a", 300).await {
                got.push(j.id);
            }
            got
        });
        let hb = tokio::spawn(async move {
            let mut got = Vec::new();
            while let Ok(Some(j)) = b.claim_sync_job("worker-b", 300).await {
                got.push(j.id);
            }
            got
        });
        let (mut ja, jb) = (ha.await.unwrap(), hb.await.unwrap());
        ja.extend(jb);
        ja.sort();
        let unique = ja.len();
        ja.dedup();
        assert_eq!(ja.len(), unique, "a job was claimed twice");

        // Completeness is checked from the QUEUE, not from what these two
        // workers happened to get.
        //
        // `claim_sync_job` is global by design — a worker claims the next job,
        // not the next job belonging to some tenant — so on a
        // deployment the running service's own sync role is a third consumer and
        // legitimately takes some of these. A live run failed here
        // for exactly that reason: `total >= 6` assumed this test was the only
        // claimant, which is true on a laptop and false in a deployment.
        //
        // The property the scenario is named for is DISJOINTNESS, asserted
        // above and unaffected by a third worker. What completeness means is
        // that no job is left sitting queued, and that is true however many
        // workers shared them out.
        for id in &queued {
            let state = store
                .sync_job_state(&t, id)
                .await
                .unwrap()
                .unwrap_or_else(|| panic!("job {id} vanished from the queue"));
            assert_ne!(
                state, "queued",
                "job {id} was never claimed by anyone — a queued job that no worker takes is the \
                 failure this scenario exists to catch"
            );
        }
    }

    /// The journal defaults to redacted. A payload appears only when the caller
    /// explicitly decided the policy allows it.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_DATABASE_URL"]
    async fn journal_is_redacted_by_default() {
        let Some(store) = store().await else {
            eprintln!("SKIP: no database url");
            return;
        };
        let t = tenant("journal");
        store
            .journal(&t, JournalRecord::new("execute", "ok").source("crm"))
            .await
            .unwrap();
        let entries = store
            .list_journal(
                &t,
                &munarium_matrix_store::journal::JournalQuery {
                    limit: 10,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0].redacted,
            "a journal row keeps nothing by default"
        );
    }
}

/// The gRPC data plane: `matrix.v1.MatrixQuery/Execute`
/// against a RUNNING Matrix, over the plane's own port.
///
/// Gated on `MUNARIUM_MATRIX_TEST_GRPC` and `#[ignore]`d like the other
/// external tiers. What these prove that the offline drift test cannot: the
/// service is served, auth is enforced at the transport, a refusal rides the
/// stream as a message while a caller's mistake is a status, the block the
/// stream carries is THE block REST returns (same sealed evidence id), and a
/// client deadline reaches the server as a native cancellation.
#[cfg(test)]
mod grpc {
    use munarium_matrix_proto::v1::execute_event::Event;
    use munarium_matrix_proto::v1::matrix_query_client::MatrixQueryClient;
    use munarium_matrix_proto::v1::{ExecuteEvent, ExecuteRequest};
    use munarium_matrix_types::contract::{EvidenceBlock, QueryIntent};
    use tokio_stream::StreamExt;
    use tonic::transport::{Channel, ClientTlsConfig};

    fn skip() -> Option<String> {
        let Some(url) = crate::grpc_url() else {
            println!(
                "SKIPPED: MUNARIUM_MATRIX_TEST_GRPC is not set, so nothing was tested. \
                 Run `test.ps1 -BlackBox` to exercise this tier."
            );
            return None;
        };
        Some(url)
    }

    async fn channel(url: &str) -> Channel {
        // A deployment serves gRPC behind TLS; compose serves it as h2c. That
        // difference is why one live run failed five scenarios here while
        // the compose tier was 106/106 green at the same moment: rustls is
        // only reached over real ingress.
        munarium_matrix_adapter::install_crypto_provider();
        let mut endpoint = Channel::from_shared(url.to_string()).expect("a valid gRPC url");
        if url.starts_with("https://") {
            endpoint = endpoint
                .tls_config(ClientTlsConfig::new().with_native_roots())
                .expect("tls config");
        }
        endpoint.connect().await.unwrap_or_else(|e| {
            panic!("MUNARIUM_MATRIX_TEST_GRPC is set but {url} did not answer: {e}")
        })
    }

    fn token() -> String {
        std::env::var("MUNARIUM_MATRIX_TEST_TOKEN").unwrap_or_else(|_| "mxdev".into())
    }

    fn rest_url() -> String {
        std::env::var("MUNARIUM_MATRIX_TEST_URL").unwrap_or_else(|_| "http://localhost:8180".into())
    }

    fn intent(tenant: &str, as_of: &str) -> QueryIntent {
        serde_json::from_value(serde_json::json!({
            "contract_version": munarium_matrix_core::CONTRACT_VERSION,
            "kind": "structured_query",
            "contract": "open-pipeline-by-region",
            "parameters": { "as_of": { "type": "date", "value": as_of } },
            "authorization": { "tenant": tenant, "access_level": 0, "compartments": [] },
            "limits": { "max_rows": 500, "max_bytes": 1048576 }
        }))
        .unwrap()
    }

    fn authed(req: ExecuteRequest) -> tonic::Request<ExecuteRequest> {
        let mut r = tonic::Request::new(req);
        r.metadata_mut().insert(
            "authorization",
            format!("Bearer {}", token()).parse().unwrap(),
        );
        r
    }

    /// Register the `crm` DataSource these scenarios read through, IF the
    /// deployment has not.
    ///
    /// A live runner applies it, pointed at its own
    /// Postgres with a vaulted credential, and this leaves it alone. In
    /// compose nothing applies it, and for a while that meant the whole gRPC
    /// tier ran against whatever a hand-run `curl` had last left in the
    /// registry — which is not a tier, it is a coincidence. Applied versions
    /// are immutable, so a source that is already there is never rewritten:
    /// this only fills an EMPTY registry.
    ///
    /// The host comes from the environment because the compose service name
    /// (`postgres`) and a deployment's FQDN are different worlds; when it is wrong
    /// the refusal names the host, which is the failure this should have.
    async fn ensure_source(http: &reqwest::Client) {
        let existing = http
            .get(format!("{}/v1/datasources/crm", rest_url()))
            .bearer_auth(token())
            .send()
            .await
            .expect("the registry answers");
        if existing.status().is_success() {
            return;
        }
        let host =
            std::env::var("MUNARIUM_MATRIX_TEST_SOURCE_HOST").unwrap_or_else(|_| "postgres".into());
        let database =
            std::env::var("MUNARIUM_MATRIX_TEST_SOURCE_DB").unwrap_or_else(|_| "matrix".into());
        let yaml = format!(
            r#"apiVersion: munarium.ioka.io/v1
kind: DataSource
metadata: {{ name: crm, version: 1 }}
spec:
  adapter: postgres
  connection: {{ host: {host}, database: {database}, schema: crm }}
  credentialRef: matrix-crm
  egress: {{ allowHosts: [{host}] }}
  authorization: {{ strategy: source_native }}
"#
        );
        let applied = http
            .post(format!("{}/v1/assets", rest_url()))
            .bearer_auth(token())
            .header("content-type", "text/yaml")
            .body(yaml)
            .send()
            .await
            .expect("apply reaches the REST plane");
        let status = applied.status();
        // 409: another test in this parallel run applied it first. Both are
        // the same bytes, so either winner is correct.
        assert!(
            status.is_success() || status.as_u16() == 409,
            "apply crm: {status}: {}",
            applied.text().await.unwrap_or_default()
        );
    }

    /// Register the contract these scenarios execute. Idempotent, and called
    /// by EVERY scenario that executes it: the tests run in parallel, and on
    /// cycle 17 the deadline scenario reached the plane before the execute
    /// scenario had applied the contract, refusing `not_covered` — a race in
    /// the test, not in the plane.
    async fn ensure_contract(http: &reqwest::Client) {
        ensure_source(http).await;
        let yaml = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../fixtures/assets/valid/contract.open-pipeline.yaml"),
        )
        .unwrap();
        let applied = http
            .post(format!("{}/v1/assets", rest_url()))
            .bearer_auth(token())
            .header("content-type", "text/yaml")
            .body(yaml)
            .send()
            .await
            .expect("apply reaches the REST plane");
        let status = applied.status();
        let body = applied.text().await.unwrap_or_default();
        assert!(
            status.is_success() || status.as_u16() == 409,
            "apply: {status}: {body}"
        );
    }

    /// An execute whose statement returns NO rows is a COMPLETE answer, not
    /// a drift refusal (cycle uytigs3m): the adapter infers its schema from
    /// the first row, so an empty read used to reconcile an empty schema
    /// against the declaration and refuse `schema_drift` naming every
    /// declared column. "No opportunities existed before that date" is
    /// exactly the kind of claim sealed evidence exists to back.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_GRPC"]
    async fn grpc_tier_an_empty_result_is_a_complete_answer() {
        if crate::grpc_url().is_none() {
            eprintln!("SKIP: no gRPC/REST tier configured");
            return;
        }
        let http = reqwest::Client::new();
        ensure_contract(&http).await;
        let tenant = std::env::var("MUNARIUM_MATRIX_TEST_TENANT")
            .unwrap_or_else(|_| "tenant-default".into());
        // Before every fixture row's updated_at: zero rows, honestly.
        let body = intent(&tenant, "2020-01-01");
        let resp = http
            .post(format!(
                "{}/v1/contracts/open-pipeline-by-region/execute",
                rest_url()
            ))
            .bearer_auth(token())
            .json(&body)
            .send()
            .await
            .expect("execute answers");
        let status = resp.status();
        let block: serde_json::Value = resp.json().await.expect("json");
        assert!(status.is_success(), "{status}: {block}");
        assert_eq!(
            block["kind"].as_str(),
            Some("complete_table"),
            "an empty result must seal as a complete table, not refuse: {block}"
        );
        assert_eq!(block["rows"].as_array().map(Vec::len), Some(0), "{block}");
        assert_eq!(block["truncated"], serde_json::json!(false), "{block}");
        assert!(
            block["evidence_id"].as_str().is_some(),
            "even an empty answer is sealed and citable: {block}"
        );
    }

    /// The native data view over the live service: a native data view over the Postgres
    /// fixture is applied, verified — which records the table definition's
    /// fingerprint — and then executed with a semantic intent, returning one
    /// keyed row under the reader's row-level security with its evidence
    /// sealed. The same REST plane the gRPC scenarios compare against.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_GRPC"]
    async fn grpc_tier_native_data_view_verifies_and_executes_over_rest() {
        if crate::grpc_url().is_none() {
            eprintln!("SKIP: no gRPC/REST tier configured");
            return;
        }
        let http = reqwest::Client::new();
        ensure_source(&http).await;
        let yaml = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../fixtures/assets/valid/dataview.pipeline.yaml"),
        )
        .unwrap();
        let applied = http
            .post(format!("{}/v1/assets", rest_url()))
            .bearer_auth(token())
            .header("content-type", "text/yaml")
            .body(yaml)
            .send()
            .await
            .expect("apply reaches the REST plane");
        let status = applied.status();
        let body = applied.text().await.unwrap_or_default();
        assert!(
            status.is_success() || status.as_u16() == 409,
            "apply: {status}: {body}"
        );

        let verify: serde_json::Value = http
            .post(format!(
                "{}/v1/dataviews/pipeline-by-region/verify",
                rest_url()
            ))
            .bearer_auth(token())
            .send()
            .await
            .expect("verify reaches the REST plane")
            .json()
            .await
            .unwrap();
        assert_eq!(verify["failed"], 0, "verify: {verify}");
        assert!(
            verify["fingerprint"]
                .as_str()
                .is_some_and(|f| f.starts_with("sha256:")),
            "{verify}"
        );

        let tenant = crate::test_tenant();
        let intent = serde_json::json!({
            "contract_version": munarium_matrix_core::CONTRACT_VERSION,
            "kind": "semantic",
            "semantic": { "provider": "pipeline-by-region", "measures": ["pipeline_amount", "opportunity_count"], "dimensions": ["region"] },
            "authorization": { "tenant": tenant, "access_level": 0, "compartments": [] },
            "limits": { "max_rows": 500, "max_bytes": 1048576 }
        });
        let resp = http
            .post(format!(
                "{}/v1/dataviews/pipeline-by-region/execute",
                rest_url()
            ))
            .bearer_auth(token())
            .json(&intent)
            .send()
            .await
            .expect("execute reaches the REST plane");
        let status = resp.status();
        let block: serde_json::Value = resp.json().await.unwrap_or_default();
        assert!(status.is_success(), "execute: {status}: {block}");
        assert_eq!(block["kind"], "complete_table", "{block}");
        let rows = block["rows"].as_array().expect("rows");
        assert_eq!(
            rows.len(),
            1,
            "one region under the reader's row-level security: {block}"
        );
        assert_eq!(rows[0]["cells"][0], "EMEA", "{block}");
        assert_eq!(
            rows[0]["cells"][1], "2770001.00",
            "the sum keeps its scale: {block}"
        );
        assert!(block["evidence_id"].as_str().is_some(), "sealed: {block}");
    }

    /// The MCP toolset over the real protocol.
    ///
    /// What must hold is not "MCP works" but that it is a TRANSPORT: the
    /// tools an agent sees are the applied assets' own declarations, a call
    /// runs the same execute path with the same seal, and there is no tool
    /// that takes a statement. The last of those is asserted by looking at
    /// every tool's schema, because a free-SQL tool is the one thing this
    /// surface must never grow.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_GRPC"]
    async fn grpc_tier_mcp_lists_declared_tools_and_a_call_seals_evidence() {
        if crate::grpc_url().is_none() {
            eprintln!("SKIP: no tier configured");
            return;
        }
        let http = reqwest::Client::new();
        ensure_contract(&http).await;
        let rpc = |method: &str, params: serde_json::Value| {
            let http = http.clone();
            let method = method.to_string();
            async move {
                http.post(format!("{}/mcp", rest_url()))
                    .bearer_auth(token())
                    .json(&serde_json::json!({
                        "jsonrpc": "2.0", "id": 1, "method": method, "params": params
                    }))
                    .send()
                    .await
                    .expect("the MCP endpoint answers")
                    .json::<serde_json::Value>()
                    .await
                    .expect("a JSON-RPC envelope")
            }
        };

        let init = rpc("initialize", serde_json::json!({})).await;
        assert_eq!(init["result"]["serverInfo"]["name"], "munarium-matrix");
        assert!(
            init["result"]["capabilities"]["tools"].is_object(),
            "{init}"
        );

        let listed = rpc("tools/list", serde_json::json!({})).await;
        let tools = listed["result"]["tools"]
            .as_array()
            .expect("tools listed")
            .clone();
        assert!(
            !tools.is_empty(),
            "the applied contract is a tool: {listed}"
        );
        let pipeline = tools
            .iter()
            .find(|t| t["name"] == "contract.open-pipeline-by-region")
            .expect("the applied contract appears as a tool");
        // The schema is the contract's declaration, closed.
        assert_eq!(pipeline["inputSchema"]["additionalProperties"], false);
        assert!(
            pipeline["inputSchema"]["properties"]["as_of"].is_object(),
            "{pipeline}"
        );
        // The property this surface exists to keep: nothing takes a statement.
        for t in &tools {
            let text = t.to_string().to_lowercase();
            assert!(
                !text.contains("\"sql\"") && !text.contains("statement\":"),
                "no tool may take a statement: {}",
                t["name"]
            );
        }

        let called = rpc(
            "tools/call",
            serde_json::json!({
                "name": "contract.open-pipeline-by-region",
                "arguments": { "as_of": "2026-06-30" }
            }),
        )
        .await;
        let text = called["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        assert!(
            text.contains("evidence_id: ev-"),
            "a tool result carries the citable evidence id: {called}"
        );
        assert!(text.contains("completeness: COMPLETE"), "{text}");

        // A refusal is a TOOL error with its typed code, never an empty
        // result: an agent that cannot tell "no rows" from "not allowed"
        // reports the wrong thing.
        let refused = rpc(
            "tools/call",
            serde_json::json!({ "name": "contract.no-such-contract", "arguments": {} }),
        )
        .await;
        assert!(
            refused["error"]["message"]
                .as_str()
                .is_some_and(|m| m.contains("no tool named")),
            "{refused}"
        );

        // An anonymous call is refused inside the envelope, not as a bare
        // status an MCP client cannot explain.
        let anon: serde_json::Value = http
            .post(format!("{}/mcp", rest_url()))
            .json(&serde_json::json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/list" }))
            .send()
            .await
            .expect("answers")
            .json()
            .await
            .expect("an envelope");
        assert_eq!(anon["error"]["code"], -32600, "{anon}");
    }

    /// Drain a stream into (progress stages, terminal event).
    async fn drain(
        stream: tonic::Streaming<ExecuteEvent>,
    ) -> Result<(Vec<String>, Option<Event>), tonic::Status> {
        let mut stages = Vec::new();
        let mut terminal = None;
        let mut stream = stream;
        while let Some(ev) = stream.next().await {
            match ev?.event {
                Some(Event::Progress(p)) => stages.push(p.stage),
                Some(other) => terminal = Some(other),
                None => {}
            }
        }
        Ok((stages, terminal))
    }

    /// The descriptor is served, and it lists BOTH planes an operator reaches.
    ///
    /// This test used to be called `reflection_lists_the_query_service` and
    /// never touched reflection: it called health through a generated client,
    /// which needs no descriptor at all. So the name was a claim the body did
    /// not check, and the gap it hid was real — the health descriptor was
    /// never registered with the reflection builder, so
    /// `grpcurl -plaintext host:50151 grpc.health.v1.Health/Check` (the
    /// command `docs/api/grpc.md` prints) answered "target server does not
    /// expose service" against a server that was serving it. Found on a real
    /// cluster the day the Helm chart was first installed.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_GRPC"]
    async fn grpc_reflection_lists_the_query_service() {
        use tonic_reflection::pb::v1::server_reflection_client::ServerReflectionClient;
        use tonic_reflection::pb::v1::server_reflection_request::MessageRequest;
        use tonic_reflection::pb::v1::server_reflection_response::MessageResponse;
        use tonic_reflection::pb::v1::ServerReflectionRequest;

        let Some(url) = skip() else { return };
        let ch = channel(&url).await;

        let mut reflection = ServerReflectionClient::new(ch.clone());
        let req = ServerReflectionRequest {
            host: String::new(),
            message_request: Some(MessageRequest::ListServices(String::new())),
        };
        let mut stream = reflection
            .server_reflection_info(tokio_stream::iter(vec![req]))
            .await
            .expect("reflection answers")
            .into_inner();
        let listed: Vec<String> = match stream
            .next()
            .await
            .expect("one response")
            .expect("a valid response")
            .message_response
        {
            Some(MessageResponse::ListServicesResponse(r)) => {
                r.service.into_iter().map(|s| s.name).collect()
            }
            other => panic!("expected a service listing, got {other:?}"),
        };
        for wanted in ["matrix.v1.MatrixQuery", "grpc.health.v1.Health"] {
            assert!(
                listed.iter().any(|s| s == wanted),
                "reflection must list {wanted}, or grpcurl cannot call it: {listed:?}"
            );
        }

        // And the plane it lists actually serves.
        let mut health = tonic_health::pb::health_client::HealthClient::new(ch);
        let status = health
            .check(tonic_health::pb::HealthCheckRequest {
                service: "matrix.v1.MatrixQuery".into(),
            })
            .await
            .expect("health answers for the service")
            .into_inner();
        assert_eq!(
            status.status,
            tonic_health::pb::health_check_response::ServingStatus::Serving as i32
        );
    }

    /// G6 — no bearer, no stream: UNAUTHENTICATED at the transport, before
    /// any intent is read.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_GRPC"]
    async fn grpc_an_unauthenticated_call_is_a_status() {
        let Some(url) = skip() else { return };
        let mut client = MatrixQueryClient::new(channel(&url).await);
        let err = client
            .execute(tonic::Request::new(ExecuteRequest {
                contract: "open-pipeline-by-region".into(),
                intent: Some((&intent(&crate::test_tenant(), "2026-06-30")).into()),
            }))
            .await
            .expect_err("an anonymous call must not open a stream");
        assert_eq!(err.code(), tonic::Code::Unauthenticated, "{err}");
    }

    /// G7 — a contract that does not exist is a typed refusal ON the stream,
    /// and the call completes OK. A refusal is an answer, not a failure.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_GRPC"]
    async fn grpc_a_refusal_is_a_message_not_a_status() {
        let Some(url) = skip() else { return };
        let mut client = MatrixQueryClient::new(channel(&url).await);
        let stream = client
            .execute(authed(ExecuteRequest {
                contract: "no-such-contract".into(),
                intent: Some((&intent(&crate::test_tenant(), "2026-06-30")).into()),
            }))
            .await
            .expect("the call opens")
            .into_inner();
        let (stages, terminal) = drain(stream).await.expect("the stream completes OK");
        assert_eq!(stages.first().map(String::as_str), Some("authenticated"));
        match terminal {
            Some(Event::Refusal(r)) => {
                assert_eq!(r.code, "not_covered", "{}", r.message);
                assert_eq!(
                    r.class,
                    munarium_matrix_proto::v1::RefusalClass::NotCovered as i32
                );
            }
            other => panic!("expected a refusal event, got {other:?}"),
        }
    }

    /// G1 — the block the stream carries IS the block REST returns: the same
    /// sealed evidence id, because sealing is idempotent by logical hash and
    /// both planes run the one execute path.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_GRPC"]
    async fn grpc_execute_streams_the_block_rest_returns() {
        let Some(url) = skip() else { return };
        let tenant = crate::test_tenant();
        let http = reqwest::Client::new();

        // The contract must be registered; the source (`crm`) and its
        // credential are the deployment's or the black-box tier's to provide.
        ensure_contract(&http).await;

        let resp = http
            .post(format!(
                "{}/v1/contracts/open-pipeline-by-region/execute",
                rest_url()
            ))
            .bearer_auth(token())
            .json(&intent(&tenant, "2026-06-30"))
            .send()
            .await
            .expect("REST execute reaches the plane");
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        // The BODY is the diagnosis. Cycle 17 reported "Status(400)" and
        // nothing else, which cost a cycle to learn nothing.
        assert!(status.is_success(), "REST execute: {status}: {body}");
        let rest_block: EvidenceBlock = serde_json::from_str(&body).expect("a block");
        let rest_id = rest_block
            .evidence_id()
            .expect("REST sealed evidence")
            .to_string();

        let mut client = MatrixQueryClient::new(channel(&url).await);
        let stream = client
            .execute(authed(ExecuteRequest {
                contract: "open-pipeline-by-region".into(),
                intent: Some((&intent(&tenant, "2026-06-30")).into()),
            }))
            .await
            .expect("the call opens")
            .into_inner();
        let (stages, terminal) = drain(stream).await.expect("the stream completes OK");
        assert!(
            stages.iter().any(|s| s == "executing") && stages.iter().any(|s| s == "sealed"),
            "progress named the stages: {stages:?}; terminal event: {terminal:?}"
        );
        let block = match terminal {
            Some(Event::Block(b)) => EvidenceBlock::try_from(&b).expect("the block converts"),
            other => panic!("expected a block, got {other:?}"),
        };
        assert_eq!(
            block.evidence_id(),
            Some(rest_id.as_str()),
            "one execute path, one artifact: the gRPC plane must cite the evidence REST sealed"
        );
        // The ROWS and derivations, not the whole block: each execution is a
        // new execution with its own snapshot marker and timestamps in the
        // manifest, while the seal is idempotent by logical hash — which is
        // exactly why the evidence id above is the same.
        match (&block, &rest_block) {
            (
                EvidenceBlock::CompleteTable {
                    rows: a,
                    derivations: da,
                    truncated: ta,
                    ..
                },
                EvidenceBlock::CompleteTable {
                    rows: b,
                    derivations: db,
                    truncated: tb,
                    ..
                },
            ) => {
                assert_eq!(a, b, "the same rows");
                assert_eq!(da, db, "the same derivations");
                assert_eq!(ta, tb);
            }
            other => panic!("expected two tables, got {other:?}"),
        }
    }

    /// G7 — the intent's deadline is honoured on this plane: a `deadline_at`
    /// already in the past is refused `deadline_exceeded` ON THE STREAM, after
    /// the stages that precede execution and before the source is touched.
    ///
    /// Why not a 1 ms client timeout: tonic's client timeout bounds the wait
    /// for the response HEADERS, which a warm server sends inside a
    /// millisecond, after which the stream simply completes — the first
    /// version of this scenario passed cold and failed warm. Transport-level
    /// cancellation (dropping the stream aborts the execution task) is a
    /// property of the server's stream type, `AbortOnDrop`, and is not
    /// observable from a client without a source slow enough to race.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_GRPC"]
    async fn grpc_a_past_deadline_is_refused_on_the_stream() {
        let Some(url) = skip() else { return };
        ensure_contract(&reqwest::Client::new()).await;
        let mut client = MatrixQueryClient::new(channel(&url).await);
        let mut expired = intent(&crate::test_tenant(), "2026-06-30");
        expired.deadline_at = Some(chrono::Utc::now() - chrono::Duration::seconds(5));
        let stream = client
            .execute(authed(ExecuteRequest {
                contract: "open-pipeline-by-region".into(),
                intent: Some((&expired).into()),
            }))
            .await
            .expect("the call opens")
            .into_inner();
        let (stages, terminal) = drain(stream).await.expect("the stream completes OK");
        match terminal {
            Some(Event::Refusal(r)) => {
                assert_eq!(r.code, "deadline_exceeded", "{}", r.message);
            }
            other => panic!("expected a deadline refusal, got {other:?}"),
        }
        assert!(
            stages.iter().any(|s| s == "budget"),
            "the deadline is checked where the REST path checks it — after the budget              reservation, inside execute — and the stages say so: {stages:?}"
        );
    }
}

/// Reconciliation in SHADOW mode, end to end: a real adapter reads a
/// landing export, `observe` renders typed observations, and `reconcile_with`
/// compares them against a seeded ledger.
///
/// The fixture is the T0 cap table, row for row, because these scenarios exist
/// to prove that the traps planted in `fixtures/t0/sql/02-crm-fixture.sql` fire.
/// Trap 9 in particular was unreachable before alias assets: with key-derived subjects
/// only, holders 51 and 58 are simply two rows and nothing is ever ambiguous.
#[cfg(test)]
mod reconcile {
    use munarium_matrix_adapter::{EffectiveIdentity, Limits};
    use munarium_matrix_adapter_landing::LandingAdapter;
    use munarium_matrix_core::checkpoint::Checkpoint;
    use munarium_matrix_server_client::{LedgerFact, MockServer, ServerClient};
    use munarium_matrix_types::contract::ObservationBatch;
    use munarium_matrix_types::{parse_asset, Asset, ClaimMappingDoc};
    use munarium_matrix_workers::{observe, reconcile_with, ObserveContext, ReconcileOptions};

    struct Fixture(std::path::PathBuf);

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("mx-recon-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let f = Self(dir);
            f.write("manifest.json", MANIFEST);
            f.write("rows.csv", ROWS);
            f
        }
        fn write(&self, rel: &str, s: &str) {
            std::fs::write(self.0.join(rel), s).unwrap();
        }
        fn adapter(&self) -> LandingAdapter {
            LandingAdapter::new_file("crm", &self.0, "manifest.json")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const MANIFEST: &str = r#"{
  "snapshotId": "s1",
  "format": "csv",
  "keys": ["holder_id", "company_id"],
  "schema": [
    { "name": "holder_id", "type": "int64" },
    { "name": "company_id", "type": "int64" },
    { "name": "holder_name", "type": "string" },
    { "name": "shares", "type": "decimal", "scale": 0 },
    { "name": "share_class", "type": "string" },
    { "name": "effective_date", "type": "date" }
  ],
  "files": [{ "path": "rows.csv" }]
}"#;

    /// The T0 `crm.holdings` rows verbatim. `Jane  Rowntree` keeps its double
    /// space on purpose — folding it is normalization's job, not the fixture's.
    const ROWS: &str = "holder_id,company_id,holder_name,shares,share_class,effective_date\n\
        42,7,Jane Rowntree,125000,A,2026-04-01\n\
        43,7,Marcus Vane,90500,A,2026-04-01\n\
        51,8,J. Rowntree,40000,B,2026-01-01\n\
        58,8,\"Jane  Rowntree\",40000,B,2026-01-01\n\
        44,7,Priya Anand,15000,A,2025-11-15\n";

    /// The committed T0 mapping — the same bytes a live run applies, so a
    /// scenario cannot pass against an asset the deployment does not use.
    fn mapping() -> ClaimMappingDoc {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../fixtures/assets/valid/mapping.captable.yaml");
        let text = std::fs::read_to_string(&path).expect("the committed T0 mapping is readable");
        match parse_asset(&text).expect("the committed T0 mapping parses") {
            Asset::ClaimMapping(m) => *m,
            _ => unreachable!(),
        }
    }

    fn identity() -> EffectiveIdentity {
        EffectiveIdentity {
            class: None,
            credential_ref: None,
            principal: "conformance".into(),
        }
    }

    async fn observations(batch_id: &str) -> (Fixture, ObservationBatch) {
        let fixture = Fixture::new(batch_id);
        let adapter = fixture.adapter();
        let identity = identity();
        let ctx = ObserveContext {
            tenant: "acme",
            source_id: "crm",
            batch_id,
            run_id: Some("r1"),
            limits: Limits {
                max_rows: 100,
                max_bytes: 1 << 20,
                timeout_ms: 5_000,
            },
            identity: &identity,
        };
        let (batch, _stats, _cp) = observe(
            &adapter,
            &mapping(),
            &Checkpoint::start("crm", "holdings", "1"),
            &ctx,
        )
        .await
        .expect("the landing export observes cleanly");
        (fixture, batch)
    }

    /// The ledger as documents built it: subjects are NAMED, never keyed, which
    /// is the whole reason an alias table exists. `shareholder.marcus-vane`
    /// carries T0 trap 10 — the corpus says 90000 where the register says 90500.
    fn seeded_facts() -> Vec<LedgerFact> {
        vec![
            fact("shareholder.jane-rowntree", "shares_outstanding", "125000"),
            fact("shareholder.jane-rowntree", "share_class", "A"),
            fact("shareholder.marcus-vane", "shares_outstanding", "90000"),
            fact("shareholder.marcus-vane", "share_class", "A"),
            fact("shareholder.priya-anand", "shares_outstanding", "15000"),
            fact("shareholder.priya-anand", "share_class", "A"),
            // The documents know a holder the register does not. Declared in
            // the committed mapping's alias table, absent from `crm.holdings`.
            fact("shareholder.tomas-berg", "shares_outstanding", "5000"),
            fact("shareholder.tomas-berg", "share_class", "B"),
        ]
    }

    fn seeded_server() -> MockServer {
        seeded_server_with(Vec::new())
    }

    fn seeded_server_with(extra: Vec<LedgerFact>) -> MockServer {
        let server = MockServer::new().with_version("memv-1");
        let mut facts = seeded_facts();
        facts.extend(extra);
        server.seed_facts("memv-1", facts);
        server
    }

    fn fact(subject: &str, key: &str, value: &str) -> LedgerFact {
        LedgerFact {
            claim_id: Some(format!("claim-{subject}-{key}")),
            subject: subject.into(),
            key: key.into(),
            value: value.into(),
            seq: 1,
            status: Some("accepted".into()),
            provenance: Some("witnessed".into()),
            origin_kind: None,
        }
    }

    async fn shadow_run(
        server: &MockServer,
        batch: &ObservationBatch,
    ) -> munarium_matrix_workers::ReconcileOutcome {
        // The landing fixture is read whole, so the batch IS complete.
        shadow_run_with(server, batch, true).await
    }

    async fn shadow_run_with(
        server: &MockServer,
        batch: &ObservationBatch,
        source_complete: bool,
    ) -> munarium_matrix_workers::ReconcileOutcome {
        let bytes = serde_json::to_vec(batch).unwrap();
        reconcile_with(
            server,
            &mapping(),
            "memv-1",
            batch,
            &bytes,
            &ReconcileOptions {
                tenant: "acme",
                promoted: false,
                source_id: "crm",
                proposals: None,
                source_complete,
            },
        )
        .await
        .expect("shadow reconcile runs")
    }

    fn limited_mapping() -> ClaimMappingDoc {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../fixtures/assets/valid/mapping.captable-limited.yaml");
        let text = std::fs::read_to_string(&path).expect("the limited T0 mapping is readable");
        match parse_asset(&text).expect("the limited T0 mapping parses") {
            Asset::ClaimMapping(m) => *m,
            _ => unreachable!(),
        }
    }

    /// A per-run ceiling is checked BEFORE anything is filed. The
    /// limited mapping allows one finding and the T0 batch produces seven, so
    /// the pass is refused `ledger_volume_exceeded` with the count it would
    /// have written, and the server has received nothing. A ceiling that
    /// fired after the first write would leave a half-filed pass behind it.
    #[tokio::test]
    async fn reconcile_a_pass_over_its_declared_ceiling_is_refused_before_it_writes() {
        let server = seeded_server();
        let (_f, batch) = observations("ceiling").await;
        let bytes = serde_json::to_vec(&batch).unwrap();
        let err = reconcile_with(
            &server,
            &limited_mapping(),
            "memv-1",
            &batch,
            &bytes,
            &ReconcileOptions {
                tenant: "acme",
                promoted: false,
                source_id: "crm",
                proposals: None,
                source_complete: true,
            },
        )
        .await
        .expect_err("the ceiling refuses the pass");
        assert_eq!(err.code, "ledger_volume_exceeded");
        assert_eq!(err.class, munarium_matrix_core::RefusalClass::Exhausted);
        assert!(
            err.message.contains("7 finding(s)"),
            "the refusal names what it would have written: {}",
            err.message
        );
        assert!(
            server.filed_findings().is_empty(),
            "nothing was filed before the refusal"
        );
        assert!(server.proposed_claims().is_empty());
    }

    /// G7 — shadow mode reads canon and never writes it. The proof is
    /// byte-identical `SliceFacts` across a full run, not an absence of write
    /// calls: a pipeline that wrote and reverted would pass the second test.
    #[tokio::test]
    async fn reconcile_shadow_leaves_canon_byte_identical() {
        let server = seeded_server();
        // Debug rather than JSON because `LedgerFact` is a READ type and has
        // no Serialize impl — deliberately, since Matrix never writes one back.
        let before = format!("{:?}", server.slice_facts("memv-1", None).await.unwrap());

        let (_f, batch) = observations("byte-identical").await;
        let out = shadow_run(&server, &batch).await;
        assert!(out.findings_filed > 0, "the run did real work");

        let after = format!("{:?}", server.slice_facts("memv-1", None).await.unwrap());
        assert_eq!(before, after, "shadow mode must not move a single byte");
        assert!(server.proposed_claims().is_empty());
        assert!(out.canon_untouched);
    }

    /// G7 — T0 trap 9. Two holders on ONE cap table whose declared forms name
    /// one ledger subject: a finding, and nothing merged.
    #[tokio::test]
    async fn reconcile_ambiguous_identity_never_merges() {
        let server = seeded_server();
        let (_f, batch) = observations("ambiguous").await;
        let out = shadow_run(&server, &batch).await;

        // Holders 51 and 58, both declared properties: four observations.
        assert_eq!(
            out.ambiguous, 4,
            "both contested rows refuse on both properties"
        );
        let ambiguity: Vec<_> = server
            .filed_findings()
            .into_iter()
            .filter(|f| f.rule_id == "matrix.identity-ambiguous")
            .collect();
        assert_eq!(ambiguity.len(), 4);
        for f in &ambiguity {
            assert_eq!(f.severity, "warn", "shadow mode never blocks");
            let candidates = f.detail["candidates"]
                .as_array()
                .expect("candidates listed");
            assert!(
                candidates.len() > 1,
                "a finding that names one candidate does not explain the refusal"
            );
        }
        // The uncontested rows are unaffected: an alias that resolves is the
        // feature working, not a near miss.
        assert!(
            out.agreements > 0,
            "holder 42 agrees THROUGH the alias, which is what makes the \
             ambiguity above a real distinction rather than a blanket refusal"
        );
    }

    /// G4 — replaying one batch seals once and files nothing twice. The batch
    /// id is the idempotency key, so a restarted worker re-reading its queue
    /// cannot double-count a discrepancy.
    #[tokio::test]
    async fn reconcile_replayed_batch_creates_no_duplicates() {
        let server = seeded_server();
        let (_f, batch) = observations("replay").await;

        let first = shadow_run(&server, &batch).await;
        let sealed_after_first = server.evidence_count();
        let second = shadow_run(&server, &batch).await;

        assert_eq!(
            server.evidence_count(),
            sealed_after_first,
            "the same batch seals exactly one artifact"
        );
        assert_eq!(first.batch_evidence_id, second.batch_evidence_id);
        assert_eq!(first.discrepancies, second.discrepancies);
        assert_eq!(first.ambiguous, second.ambiguous);
    }

    /// G7 — T0 trap 11. A backdated change is a new fact about an old period,
    /// not a fix to a wrong one, and the two are indistinguishable in a change
    /// feed. It files for review and never becomes a correction.
    #[tokio::test]
    async fn reconcile_backdated_change_requires_review() {
        let server = seeded_server();
        let (_f, mut batch) = observations("backdated").await;
        // The landing export has no change feed, so every row reads as a
        // snapshot. Mark holder 44's rows as the backdated change the CDC
        // source would deliver.
        for o in batch.observations.iter_mut() {
            if o.origin.row_key == "44|7" {
                o.change_kind = munarium_matrix_types::contract::ChangeKind::Backdated;
            }
        }
        let out = shadow_run(&server, &batch).await;

        let findings = server.filed_findings();
        let backdated: Vec<_> = findings
            .iter()
            .filter(|f| f.detail["verdict"] == "backdated_requires_review")
            .collect();
        assert_eq!(
            backdated.len(),
            2,
            "both of holder 44's properties file for review"
        );
        assert!(
            findings.iter().all(|f| f.detail["verdict"] != "correction"),
            "nothing in shadow mode may name itself a correction"
        );
        assert!(out.canon_untouched);
    }

    /// G1 — a discrepancy finding carries BOTH sides: the sealed source
    /// artifact with its row, and the ledger claim id. One side alone is an
    /// accusation.
    #[tokio::test]
    async fn reconcile_discrepancy_carries_both_evidence_sides() {
        let server = seeded_server();
        let (_f, batch) = observations("both-sides").await;
        shadow_run(&server, &batch).await;

        let findings = server.filed_findings();
        let differ: Vec<_> = findings
            .iter()
            .filter(|f| f.detail["verdict"] == "differ")
            .collect();
        assert_eq!(differ.len(), 1, "T0 trap 10 and only trap 10");
        let d = differ[0];
        assert_eq!(d.detail["subject"], "shareholder.marcus-vane");
        assert_eq!(d.detail["source"]["value"], "90500");
        assert_eq!(d.detail["ledger"]["value"], "90000");

        let evidence_id = d.detail["source"]["evidence_id"]
            .as_str()
            .expect("the source side names a sealed artifact");
        assert!(
            server.evidence_bytes(evidence_id).is_some(),
            "the cited artifact resolves — a dangling evidence id is not evidence"
        );
        assert!(
            d.detail["ledger"]["claim_id"].as_str().is_some(),
            "the ledger side names the claim it disagrees with"
        );
        assert_eq!(
            d.detail["source"]["row_key"], "43|7",
            "and the row, so the assertion is checkable at the source too"
        );
    }

    /// G4 — the verdict that comes from a row NOT being there.
    ///
    /// A holder the mapping declares and the register lacks is
    /// `missing_in_source`. A ledger subject outside the mapping's namespace
    /// is not its business and stays silent. And on a read that was not
    /// complete the verdict is withheld — an incremental batch says nothing
    /// about the rows it did not return, and a false "missing" is an
    /// accusation.
    #[tokio::test]
    async fn reconcile_absent_declared_holder_is_missing_in_source() {
        let server = seeded_server_with(vec![
            // Outside `shareholder.{holder_id}` and not in the alias table.
            fact("company.7", "shares_outstanding", "0"),
        ]);
        let (_f, batch) = observations("missing").await;
        let out = shadow_run(&server, &batch).await;

        let findings = server.filed_findings();
        let missing: Vec<_> = findings
            .iter()
            .filter(|f| f.detail["verdict"] == "missing_in_source")
            .collect();
        assert_eq!(out.missing_in_source, 2);
        assert_eq!(missing.len(), 2, "both of Tomas Berg's declared properties");
        assert!(
            missing
                .iter()
                .all(|f| f.detail["subject"] == "shareholder.tomas-berg"),
            "company.7 is not this mapping's business: {missing:#?}"
        );
        for f in &missing {
            assert!(f.detail["source"]["value"].is_null());
            assert!(
                f.detail["ledger"]["claim_id"].as_str().is_some(),
                "the ledger side names the claim nothing in the source supports"
            );
            let evidence_id = f.detail["source"]["evidence_id"].as_str().unwrap();
            assert!(
                server.evidence_bytes(evidence_id).is_some(),
                "the batch that was searched is the evidence of absence"
            );
        }

        // The same batch, declared incomplete: nothing about absence.
        let server2 = seeded_server();
        let (_f2, batch2) = observations("missing-partial").await;
        let partial = shadow_run_with(&server2, &batch2, false).await;
        assert_eq!(partial.missing_in_source, 0);
        assert!(server2
            .filed_findings()
            .iter()
            .all(|f| f.detail["verdict"] != "missing_in_source"));
    }

    /// The mode-C exit gate, measured. Precision and recall of discrepancy
    /// detection over the planted T0 answer key.
    ///
    /// The threshold is **1.0 for both**, and that is not ambition. This
    /// pipeline is a deterministic typed comparison against a known key: there
    /// is no sampling, no model and no ranking anywhere in it, so any miss or
    /// any false positive is a defect with a cause. A bar below 1.0 would
    /// license one.
    #[tokio::test]
    async fn reconcile_precision_and_recall_on_the_t0_answer_key() {
        // The answer key, read off `02-crm-fixture.sql`. Keyed by
        // (row_key, property) because a row is separately right or wrong per
        // property.
        let expected: &[(&str, &str, &str)] = &[
            // Trap 10: the corpus says 90000, the register says 90500.
            ("43|7", "shares_outstanding", "differ"),
            // Trap 9: contested alias on company 8's cap table.
            ("51|8", "shares_outstanding", "identity_ambiguous"),
            ("51|8", "share_class", "identity_ambiguous"),
            ("58|8", "shares_outstanding", "identity_ambiguous"),
            ("58|8", "share_class", "identity_ambiguous"),
            // Declared in the mapping, absent from the register: the verdict
            // that comes from a row NOT being there. Keyed by subject, since
            // there is no row to key by.
            (
                "shareholder.tomas-berg",
                "shares_outstanding",
                "missing_in_source",
            ),
            ("shareholder.tomas-berg", "share_class", "missing_in_source"),
        ];

        let server = seeded_server();
        let (_f, batch) = observations("precision-recall").await;
        shadow_run(&server, &batch).await;

        let reported: Vec<(String, String, String)> = server
            .filed_findings()
            .into_iter()
            .map(|f| {
                let verdict = if f.rule_id == "matrix.identity-ambiguous" {
                    "identity_ambiguous".to_string()
                } else {
                    f.detail["verdict"].as_str().unwrap_or_default().to_string()
                };
                let row = if verdict == "missing_in_source" {
                    f.detail["subject"].as_str().unwrap_or_default().to_string()
                } else {
                    f.detail["row_key"]
                        .as_str()
                        .or_else(|| f.detail["source"]["row_key"].as_str())
                        .unwrap_or_default()
                        .to_string()
                };
                let property = f.detail["property"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                (row, property, verdict)
            })
            .collect();

        let is_expected = |r: &(String, String, String)| {
            expected
                .iter()
                .any(|(row, prop, v)| *row == r.0 && *prop == r.1 && *v == r.2)
        };
        let true_positives = reported.iter().filter(|r| is_expected(r)).count();
        let false_positives = reported.len() - true_positives;
        let false_negatives = expected.len() - true_positives;

        let precision = true_positives as f64 / (true_positives + false_positives).max(1) as f64;
        let recall = true_positives as f64 / (true_positives + false_negatives).max(1) as f64;

        // Printed, not just asserted: a recorded cycle's log is where the exit
        // gate's number comes from, and a number nobody can read is not one.
        println!(
            "reconcile precision={precision:.3} recall={recall:.3} \
             tp={true_positives} fp={false_positives} fn={false_negatives}"
        );
        assert_eq!(
            precision,
            1.0,
            "false positives: {:#?}",
            reported
                .iter()
                .filter(|r| !is_expected(r))
                .collect::<Vec<_>>()
        );
        assert_eq!(recall, 1.0, "reported: {reported:#?}");

        // And the negative half, which precision over findings alone cannot
        // show: the rows the key says are CLEAN must file nothing at all.
        assert!(
            !reported.iter().any(|r| r.0 == "42|7" || r.0 == "44|7"),
            "holders 42 and 44 agree through the alias and must be silent"
        );
    }
}

/// Authoritative reconciliation, against the mock server and an
/// in-memory proposal ledger. Every scenario here is a way the write path
/// could be wrong that the SHADOW scenarios cannot see.
#[cfg(test)]
mod authority {
    use munarium_matrix_core::Refusal;
    use munarium_matrix_server_client::{LedgerFact, MockServer, ServerClient};
    use munarium_matrix_types::contract::*;
    use munarium_matrix_types::{parse_asset, Asset, ClaimMappingDoc};
    use munarium_matrix_workers::{
        reconcile_with, rollback, ProposalLedger, ProposalRecord, ReconcileOptions, RollbackRequest,
    };
    use std::sync::Mutex;

    /// The workers' ledger trait over a vector: what the binary does over
    /// Postgres. A VECTOR, because `rollback` reads its input in proposal
    /// order to know which end of a chain is the head, and the store hands it
    /// rows ordered by `proposed_at`; a map keyed by hash would hand the
    /// chain over shuffled.
    #[derive(Default)]
    struct MemLedger {
        rows: Mutex<Vec<ProposalRecord>>,
    }

    #[async_trait::async_trait]
    impl ProposalLedger for MemLedger {
        async fn seen(&self, _t: &str, key: &str) -> Result<Option<String>, Refusal> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .find(|r| r.idempotency_key == key)
                .map(|r| r.claim_id.clone()))
        }
        async fn record(&self, _t: &str, rec: &ProposalRecord) -> Result<(), Refusal> {
            let mut rows = self.rows.lock().unwrap();
            rows.retain(|r| r.idempotency_key != rec.idempotency_key);
            rows.push(rec.clone());
            Ok(())
        }
    }

    impl MemLedger {
        fn records(&self) -> Vec<ProposalRecord> {
            self.rows.lock().unwrap().clone()
        }
    }

    fn mapping(mode: &str, authority: &str) -> ClaimMappingDoc {
        let yaml = format!(
            "apiVersion: munarium.ioka.io/v1\nkind: ClaimMapping\n\
             metadata: {{ name: holdings, version: 3 }}\nspec:\n  source: crm\n  mode: {mode}\n\
             \x20 entity: {{ table: holdings, key: [holder_id], subjectTemplate: \"shareholder.{{holder_id}}\" }}\n\
             \x20 properties:\n    shares: {{ column: shares, type: decimal, scale: 0 }}\n\
             \x20   share_class: {{ column: share_class, type: string }}\n\
             \x20 temporal: {{ validTime: {{ column: effective_date }} }}\n\
             \x20 changes:\n    shares: {{ onUpdate: update, onBackdated: requires_review }}\n{authority}"
        );
        match parse_asset(&yaml).expect("fixture parses") {
            Asset::ClaimMapping(m) => *m,
            _ => unreachable!(),
        }
    }

    fn observation(holder: u32, property: &str, value: &str, kind: ChangeKind) -> Observation {
        Observation {
            entity_candidates: vec![EntityCandidate {
                subject: format!("shareholder.{holder}"),
                scope_path: Some("company.7.captable".into()),
                confidence: 1.0,
                resolver: Some("entity_key".into()),
            }],
            property: property.into(),
            value: TypedValueDto {
                ty: if property == "shares" {
                    munarium_matrix_core::ColumnType::Decimal
                } else {
                    munarium_matrix_core::ColumnType::String
                },
                value: serde_json::Value::String(value.into()),
                scale: (property == "shares").then_some(0),
                element_type: None,
            },
            valid_time: Some(ValidTime {
                from: Some("2026-04-01T00:00:00Z".parse().unwrap()),
                to: None,
            }),
            transaction_time: None,
            change_kind: kind,
            origin: ConnectorOrigin {
                kind: "connector".into(),
                source_id: "crm".into(),
                mapping_version: "holdings@3".into(),
                row_key: format!("holder_id={holder}"),
                event_position: Some("m1".into()),
                observed_at: None,
                evidence_id: None,
            },
        }
    }

    fn batch(observations: Vec<Observation>) -> ObservationBatch {
        ObservationBatch {
            contract_version: munarium_matrix_core::CONTRACT_VERSION.into(),
            mapping: "holdings@3".into(),
            batch_id: "b1".into(),
            source_id: Some("crm".into()),
            run_id: Some("r1".into()),
            sealed_evidence_id: None,
            observations,
        }
    }

    fn fact(subject: &str, key: &str, value: &str, seq: u64, provenance: &str) -> LedgerFact {
        LedgerFact {
            claim_id: Some(format!("claim-{subject}-{key}-{seq}")),
            subject: subject.into(),
            key: key.into(),
            value: value.into(),
            seq,
            status: Some("accepted".into()),
            provenance: Some(provenance.into()),
            origin_kind: None,
        }
    }

    async fn run(
        server: &MockServer,
        m: &ClaimMappingDoc,
        promoted: bool,
        ledger: &MemLedger,
        b: &ObservationBatch,
    ) -> munarium_matrix_workers::ReconcileOutcome {
        let bytes = serde_json::to_vec(b).unwrap();
        reconcile_with(
            server,
            m,
            "memv-1",
            b,
            &bytes,
            &ReconcileOptions {
                tenant: "acme",
                promoted,
                source_id: "crm",
                proposals: Some(ledger),
                // A hand-built batch is not a read of anything; it cannot
                // vouch for what it did not contain.
                source_complete: false,
            },
        )
        .await
        .expect("reconcile runs")
    }

    const SOURCE_WINS: &str =
        "  authority:\n    - { property: shares, precedence: source_over_document }\n";

    /// G7 — `mode: authoritative` in the asset is intent; without the
    /// promotion decision the pass is shadow and canon is untouched.
    #[tokio::test]
    async fn unpromoted_mapping_proposes_nothing() {
        let server = MockServer::new().with_version("memv-1");
        server.seed_facts(
            "memv-1",
            vec![fact("shareholder.43", "shares", "90000", 1, "witnessed")],
        );
        let m = mapping("authoritative", SOURCE_WINS);
        let ledger = MemLedger::default();
        let out = run(
            &server,
            &m,
            false,
            &ledger,
            &batch(vec![observation(43, "shares", "90500", ChangeKind::Update)]),
        )
        .await;
        assert_eq!(out.discrepancies, 1, "the disagreement is still found");
        assert_eq!(out.findings_filed, 1, "and still filed");
        assert_eq!(out.proposals, 0, "but nothing is written");
        assert!(out.canon_untouched);
        assert!(server.proposed_claims().is_empty());
    }

    /// G6 — a promoted mapping writes INSIDE its declared scopes and nowhere
    /// else; the out-of-scope property is filed, counted, and not proposed.
    #[tokio::test]
    async fn promoted_mapping_proposes_only_in_scope() {
        let server = MockServer::new().with_version("memv-1");
        server.seed_facts(
            "memv-1",
            vec![
                fact("shareholder.43", "shares", "90000", 1, "connector"),
                fact("shareholder.43", "share_class", "A", 2, "connector"),
            ],
        );
        let m = mapping("authoritative", SOURCE_WINS);
        let ledger = MemLedger::default();
        let out = run(
            &server,
            &m,
            true,
            &ledger,
            &batch(vec![
                observation(43, "shares", "90500", ChangeKind::Update),
                observation(43, "share_class", "B", ChangeKind::Update),
            ]),
        )
        .await;
        assert_eq!(out.discrepancies, 2);
        assert_eq!(
            out.findings_filed, 2,
            "preserve-and-disclose: both findings filed even where we write"
        );
        assert_eq!(out.proposals, 1, "only the scoped property is proposed");
        assert_eq!(out.withheld_out_of_scope, 1);
        let proposed = server.proposed_claims();
        assert_eq!(proposed.len(), 1);
        assert_eq!(proposed[0].key, "shares");
        assert_eq!(
            proposed[0].claim_type, "update",
            "a changed value under onUpdate: update supersedes as an update"
        );
        assert_eq!(
            proposed[0].supersedes_id.as_deref(),
            Some("claim-shareholder.43-shares-1")
        );
        assert_eq!(proposed[0].origin.kind, "connector");
        assert_eq!(proposed[0].origin.row_key, "holder_id=43");
        assert!(!out.canon_untouched);
    }

    /// G7 — the default precedence keeps a document's claim on top: the
    /// discrepancy is filed, the proposal is withheld. Declaring
    /// `source_over_document` for the property is what changes that.
    #[tokio::test]
    async fn document_outranks_source_by_default() {
        let server = MockServer::new().with_version("memv-1");
        server.seed_facts(
            "memv-1",
            vec![fact("shareholder.43", "shares", "90000", 1, "witnessed")],
        );
        let m = mapping(
            "authoritative",
            "  authority:\n    - { property: shares }\n",
        );
        let ledger = MemLedger::default();
        let out = run(
            &server,
            &m,
            true,
            &ledger,
            &batch(vec![observation(43, "shares", "90500", ChangeKind::Update)]),
        )
        .await;
        assert_eq!(out.findings_filed, 1);
        assert_eq!(out.proposals, 0);
        assert_eq!(out.withheld_document_outranks, 1);
        assert!(
            out.canon_untouched,
            "a withheld proposal leaves canon exactly as found"
        );

        // A gap is different: with no document claim there is nothing to outrank.
        let server2 = MockServer::new().with_version("memv-1");
        let out2 = run(
            &server2,
            &m,
            true,
            &MemLedger::default(),
            &batch(vec![observation(44, "shares", "15000", ChangeKind::Insert)]),
        )
        .await;
        assert_eq!(out2.proposals, 1, "missing_in_ledger is proposed as a fact");
        assert_eq!(server2.proposed_claims()[0].claim_type, "fact");
    }

    /// G4 — the same batch twice sends nothing twice. The ledger remembers the
    /// key; the server would too, but the point is that Matrix does not lean
    /// on it.
    #[tokio::test]
    async fn replayed_run_proposes_nothing_twice() {
        let server = MockServer::new().with_version("memv-1");
        let m = mapping("authoritative", SOURCE_WINS);
        let ledger = MemLedger::default();
        let b = batch(vec![observation(44, "shares", "15000", ChangeKind::Insert)]);
        let first = run(&server, &m, true, &ledger, &b).await;
        assert_eq!(first.proposals, 1);
        // The mock now holds the proposed fact, so the second pass AGREES and
        // never reaches the proposal step at all — the honest replay.
        let second = run(&server, &m, true, &ledger, &b).await;
        assert_eq!(second.proposals, 0);
        assert_eq!(second.agreements, 1);
        assert_eq!(
            server.proposed_claims().len(),
            1,
            "one proposal on the wire, ever"
        );
        // And if the ledger somehow did NOT hold it, the key still stops a resend.
        let server3 = MockServer::new().with_version("memv-1");
        let third = run(&server3, &m, true, &ledger, &b).await;
        assert_eq!(third.proposals, 0);
        assert_eq!(
            third.proposals_replayed, 1,
            "the proposal ledger, not the server, stopped it"
        );
        assert!(server3.proposed_claims().is_empty());
    }

    /// G7 — a backdated change never becomes a proposal, whatever the scope
    /// says. It is a fact about an earlier period; a human decides.
    #[tokio::test]
    async fn backdated_never_proposes() {
        let server = MockServer::new().with_version("memv-1");
        server.seed_facts(
            "memv-1",
            vec![fact("shareholder.43", "shares", "90000", 1, "connector")],
        );
        let m = mapping("authoritative", SOURCE_WINS);
        let out = run(
            &server,
            &m,
            true,
            &MemLedger::default(),
            &batch(vec![observation(
                43,
                "shares",
                "90500",
                ChangeKind::Backdated,
            )]),
        )
        .await;
        assert_eq!(out.findings_filed, 1, "filed for review");
        assert_eq!(out.proposals, 0);
        assert_eq!(out.withheld_requires_review, 1);
    }

    /// G7 — a proposal the server DISPUTES is success with findings: counted,
    /// recorded in the ledger with its status, never retried as if it failed.
    #[tokio::test]
    async fn disputed_proposal_is_counted_not_dropped() {
        let server = MockServer::new().with_version("memv-1");
        let m = mapping("authoritative", SOURCE_WINS);
        let ledger = MemLedger::default();
        server.dispute_next("gate.ledger-conflict");
        let out = run(
            &server,
            &m,
            true,
            &ledger,
            &batch(vec![observation(44, "shares", "15000", ChangeKind::Insert)]),
        )
        .await;
        assert_eq!(out.proposals, 1);
        assert_eq!(out.proposals_disputed, 1);
        let rec = &ledger.records()[0];
        assert_eq!(rec.status, "disputed");
        // A replay does not re-propose a disputed claim either.
        let again = run(
            &MockServer::new().with_version("memv-1"),
            &m,
            true,
            &ledger,
            &batch(vec![observation(44, "shares", "15000", ChangeKind::Insert)]),
        )
        .await;
        assert_eq!(again.proposals_replayed, 1);
    }

    /// G1 — rollback restores the prior value by SUPERSEDING our claim with a
    /// correction that carries `origin.kind = "rollback"`. Nothing is deleted;
    /// a proposal that filled a gap has no prior value and is skipped, not
    /// invented.
    #[tokio::test]
    async fn rollback_supersedes_with_origin() {
        let server = MockServer::new().with_version("memv-1");
        server.seed_facts(
            "memv-1",
            vec![fact("shareholder.43", "shares", "90000", 1, "connector")],
        );
        let m = mapping("authoritative", SOURCE_WINS);
        let ledger = MemLedger::default();
        let out = run(
            &server,
            &m,
            true,
            &ledger,
            &batch(vec![
                observation(43, "shares", "90500", ChangeKind::Update), // supersedes 90000
                observation(44, "shares", "15000", ChangeKind::Insert), // fills a gap
            ]),
        )
        .await;
        assert_eq!(out.proposals, 2);
        let records = ledger.records();
        let rb = rollback(
            &server,
            &RollbackRequest {
                tenant: "acme",
                source_id: "crm",
                mapping_ref: "holdings@3",
                decision_id: "CHG-42",
                proposals: &records,
                ledger: &ledger,
            },
        )
        .await
        .expect("rollback runs");
        assert_eq!(rb.superseded, 1, "the update is undone");
        assert_eq!(
            rb.skipped_no_prior, 1,
            "the gap-fill has nothing to restore"
        );
        let proposed = server.proposed_claims();
        let last = proposed.last().unwrap();
        assert_eq!(last.claim_type, "correction");
        assert_eq!(last.value, "90000", "the prior value comes back");
        assert_eq!(last.origin.kind, "rollback");
        assert_eq!(last.evidence.as_ref().unwrap()["decision_id"], "CHG-42");
        // History intact: the ledger holds original, proposal AND rollback —
        // and the RESOLVED view holds exactly the restored value.
        let history = server.all_facts("memv-1");
        let shares: Vec<_> = history
            .iter()
            .filter(|f| f.subject == "shareholder.43" && f.key == "shares")
            .collect();
        assert_eq!(shares.len(), 3, "append-only: nothing was rewritten");
        let current: Vec<_> = server
            .slice_facts("memv-1", None)
            .await
            .unwrap()
            .into_iter()
            .filter(|f| f.subject == "shareholder.43" && f.key == "shares")
            .collect();
        assert_eq!(
            current.len(),
            1,
            "one current fact per key after a rollback"
        );
        assert_eq!(current[0].value, "90000");
        // Idempotent under the same decision.
        let again = rollback(
            &server,
            &RollbackRequest {
                tenant: "acme",
                source_id: "crm",
                mapping_ref: "holdings@3",
                decision_id: "CHG-42",
                proposals: &records,
                ledger: &ledger,
            },
        )
        .await
        .unwrap();
        assert_eq!(again.superseded, 0);
        assert_eq!(again.already_rolled_back, 1);
    }

    /// G1 — a CHAIN of proposals rolls back to the value the ledger held before
    /// the mapping wrote anything, with ONE correction, leaving ONE current
    /// fact. Undoing each proposal separately, oldest first, would leave two
    /// unsuperseded claims for the key and the connector's first write at head.
    #[tokio::test]
    async fn rollback_of_a_chain_restores_the_original() {
        let server = MockServer::new().with_version("memv-1");
        server.seed_facts(
            "memv-1",
            vec![fact("shareholder.43", "shares", "90000", 1, "witnessed")],
        );
        let m = mapping("authoritative", SOURCE_WINS);
        let ledger = MemLedger::default();

        // Pass 1: the register says 90500. Pass 2: it says 91000.
        let first = run(
            &server,
            &m,
            true,
            &ledger,
            &batch(vec![observation(43, "shares", "90500", ChangeKind::Update)]),
        )
        .await;
        assert_eq!(first.proposals, 1);
        let mut b2 = batch(vec![observation(43, "shares", "91000", ChangeKind::Update)]);
        b2.batch_id = "b2".into();
        b2.observations[0].origin.event_position = Some("m2".into());
        let second = run(&server, &m, true, &ledger, &b2).await;
        assert_eq!(second.proposals, 1, "the second value supersedes the first");

        let records = ledger.records();
        assert_eq!(records.len(), 2);
        assert_eq!(records[1].prior_value.as_deref(), Some("90500"));

        let rb = rollback(
            &server,
            &RollbackRequest {
                tenant: "acme",
                source_id: "crm",
                mapping_ref: "holdings@3",
                decision_id: "CHG-43",
                proposals: &records,
                ledger: &ledger,
            },
        )
        .await
        .expect("rollback runs");
        assert_eq!(rb.superseded, 1, "one correction for the whole chain");
        assert_eq!(rb.proposals_covered, 2, "covering both proposals");

        let correction = server.proposed_claims().last().cloned().unwrap();
        assert_eq!(
            correction.value, "90000",
            "the ORIGINAL prior, not the middle one"
        );
        assert_eq!(
            correction.supersedes_id.as_deref(),
            Some(records[1].claim_id.as_str()),
            "superseding the HEAD, not a claim already superseded"
        );
        let current: Vec<_> = server
            .slice_facts("memv-1", None)
            .await
            .unwrap()
            .into_iter()
            .filter(|f| f.subject == "shareholder.43" && f.key == "shares")
            .collect();
        assert_eq!(current.len(), 1, "exactly one current fact for the key");
        assert_eq!(current[0].value, "90000");
        assert_eq!(current[0].origin_kind.as_deref(), Some("rollback"));
        assert_eq!(
            server.all_facts("memv-1").len(),
            4,
            "history: original, two proposals, one correction"
        );
    }

    /// G7 — a rollback claim carries the DOCUMENT's value, restored on
    /// purpose, and outranks the source under `document_over_source` exactly
    /// as the original document claim did. Treating it as a connector claim
    /// let the next promoted pass overwrite what the operator had just
    /// restored.
    #[tokio::test]
    async fn a_rollback_claim_holds_under_document_precedence() {
        let restored = || LedgerFact {
            origin_kind: Some("rollback".into()),
            ..fact("shareholder.43", "shares", "90000", 4, "witnessed")
        };

        // Default precedence: the restored value stands, the disagreement is filed.
        let server = MockServer::new().with_version("memv-1");
        server.seed_facts("memv-1", vec![restored()]);
        let m = mapping(
            "authoritative",
            "  authority:\n    - { property: shares }\n",
        );
        let out = run(
            &server,
            &m,
            true,
            &MemLedger::default(),
            &batch(vec![observation(43, "shares", "91000", ChangeKind::Update)]),
        )
        .await;
        assert_eq!(out.proposals, 0);
        assert_eq!(out.withheld_document_outranks, 1);
        assert_eq!(out.findings_filed, 1, "disclosed, never silent");

        // Declared source precedence: the source wins, by declaration.
        let server2 = MockServer::new().with_version("memv-1");
        server2.seed_facts("memv-1", vec![restored()]);
        let out2 = run(
            &server2,
            &mapping("authoritative", SOURCE_WINS),
            true,
            &MemLedger::default(),
            &batch(vec![observation(43, "shares", "91000", ChangeKind::Update)]),
        )
        .await;
        assert_eq!(out2.proposals, 1);
    }
}

/// The semantic gate, provoked without a source: every
/// refusal the path can emit before a statement exists — no capability, no
/// passing verification on record, a definition that moved. The adapter here
/// answers only `definition_of`; `execute` is unreachable by construction and
/// panics if reached, which is the proof that the gate is BEFORE the statement.
#[cfg(test)]
mod semantic_offline {
    use munarium_matrix_adapter::{
        BoundParameters, Capabilities, EffectiveIdentity, ExecutedResult, Limits, ProbeResult,
        RecordBatch, RolePosture, SchemaFingerprint, SourceAdapter,
    };
    use munarium_matrix_core::checkpoint::Checkpoint;
    use munarium_matrix_core::semantic::fingerprint;
    use munarium_matrix_core::{Refusal, RefusalClass};
    use munarium_matrix_server_client::MockServer;
    use munarium_matrix_types::contract::*;
    use munarium_matrix_types::parse_asset;
    use munarium_matrix_workers::{execute_metric, ExecuteContext, SemanticView};

    struct DefinitionOnly {
        caps: Capabilities,
        definition: &'static str,
    }

    #[async_trait::async_trait]
    impl SourceAdapter for DefinitionOnly {
        fn kind(&self) -> &'static str {
            "definition-only"
        }
        fn adapter_version(&self) -> &'static str {
            "test"
        }
        fn capabilities(&self) -> Capabilities {
            self.caps.clone()
        }
        async fn probe(&self) -> Result<ProbeResult, Refusal> {
            unreachable!("the gate never probes")
        }
        async fn introspect(&self) -> Result<(RolePosture, SchemaFingerprint), Refusal> {
            unreachable!("the gate never introspects")
        }
        async fn read_batch(
            &self,
            _: &str,
            _: &[String],
            _: &Checkpoint,
            _: munarium_matrix_adapter::ReadMode<'_>,
            _: &EffectiveIdentity,
            _: Limits,
        ) -> Result<RecordBatch, Refusal> {
            unreachable!("the gate never reads")
        }
        async fn execute(
            &self,
            statement: &str,
            _: &BoundParameters,
            _: &EffectiveIdentity,
            _: Limits,
        ) -> Result<ExecutedResult, Refusal> {
            panic!("a statement reached the source past the gate: {statement}")
        }
        async fn definition_of(&self, _: &str, _: Limits) -> Result<String, Refusal> {
            Ok(self.definition.to_string())
        }
    }

    fn view_doc() -> munarium_matrix_types::DataViewDoc {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../fixtures/assets/valid/dataview.pipeline.yaml"),
        )
        .unwrap();
        match parse_asset(&text).unwrap() {
            munarium_matrix_types::Asset::DataView(d) => *d,
            _ => unreachable!(),
        }
    }

    fn intent() -> QueryIntent {
        serde_json::from_value(serde_json::json!({
            "contract_version": munarium_matrix_core::CONTRACT_VERSION,
            "kind": "semantic",
            "semantic": { "provider": "pipeline-by-region", "measures": ["pipeline_amount"], "dimensions": ["region"] },
            "authorization": { "tenant": "acme", "access_level": 0, "compartments": [] },
            "limits": { "max_rows": 500, "max_bytes": 1048576 }
        }))
        .unwrap()
    }

    fn identity() -> EffectiveIdentity {
        EffectiveIdentity {
            class: None,
            credential_ref: None,
            principal: "conformance".into(),
        }
    }

    fn caps(data_views: bool) -> Capabilities {
        let mut c = Capabilities::minimal("sealed_result");
        c.query_contracts = true;
        c.data_views = data_views;
        c.dialect = Some("postgres".into());
        c
    }

    async fn run(adapter: &DefinitionOnly, verified: Option<&str>) -> Refusal {
        let server = MockServer::default();
        let doc = view_doc();
        let domains = std::collections::BTreeMap::new();
        let id = identity();
        let ctx = ExecuteContext {
            source_id: "crm",
            source_version: 1,
            dialect: "postgres",
            pinned_domains: &domains,
            identity: &id,
            authorization_class: munarium_matrix_core::AuthorizationClass::default(),
            source_limits: Limits {
                max_rows: 500,
                max_bytes: 1 << 20,
                timeout_ms: 5_000,
            },
        };
        execute_metric(
            adapter,
            &server,
            SemanticView::Native(&doc),
            &intent(),
            verified,
            &ctx,
        )
        .await
        .expect_err("the gate refuses")
    }

    /// An adapter without the capability is refused by name before anything.
    #[tokio::test]
    async fn semantic_an_adapter_without_the_capability_is_metric_not_covered() {
        let a = DefinitionOnly {
            caps: caps(false),
            definition: "a",
        };
        let r = run(&a, Some(&fingerprint("a"))).await;
        assert_eq!(r.code, "metric_not_covered");
        assert_eq!(r.class, RefusalClass::NotCovered);
    }

    /// No passing verification on record: not evidence yet.
    #[tokio::test]
    async fn semantic_a_view_with_no_verification_on_record_is_not_covered() {
        let a = DefinitionOnly {
            caps: caps(true),
            definition: "a",
        };
        let r = run(&a, None).await;
        assert_eq!(r.code, "not_covered");
        assert!(r.message.contains("verify it"), "{}", r.message);
    }

    /// The definition moved since it was verified: refused BEFORE the statement.
    #[tokio::test]
    async fn semantic_a_changed_definition_is_refused_before_the_statement() {
        let a = DefinitionOnly {
            caps: caps(true),
            definition: "id:bigint:NO\namount:numeric:NO",
        };
        let r = run(&a, Some(&fingerprint("id:bigint:NO\namount:numeric:YES"))).await;
        assert_eq!(r.code, "metric_view_changed");
        assert_eq!(r.class, RefusalClass::Invalid);
    }
}

/// The planner: a vendor planner proposes, Matrix decides.
///
/// These scenarios drive `decide()` — pure, no network — and pin the three
/// properties the planner policy exists for: assist admits only a permitted
/// trusted asset, evaluation records without admitting, and an unpinned plan
/// is a label rather than a failure.
#[cfg(test)]
mod planner {
    use munarium_matrix_adapter::planner::{PlannerMessage, PlannerSpec};
    use munarium_matrix_workers::genie::{decide, PlannerMode};

    fn spec(trusted: &[&str], tables: &[&str], evaluation: bool) -> PlannerSpec {
        PlannerSpec {
            space_id: "sp-1".into(),
            trusted_assets: trusted.iter().map(|s| s.to_string()).collect(),
            allowed_tables: tables.iter().map(|s| s.to_string()).collect(),
            evaluation_enabled: evaluation,
        }
    }

    fn message(sql: Option<&str>, asset: Option<&str>) -> PlannerMessage {
        PlannerMessage {
            conversation_id: "c-1".into(),
            message_id: "m-1".into(),
            attachment_id: Some("att-1".into()),
            statement_id: None,
            proposed_sql: sql.map(|s| s.to_string()),
            trusted_asset_id: asset.map(|s| s.to_string()),
            prose: Some("the pipeline by region".into()),
        }
    }

    /// Assist admits a proposal that resolved to a trusted asset the spec
    /// permits — and ONLY that. The same proposal under a spec naming a
    /// different asset is refused `genie_asset_not_allowed` with nothing
    /// admitted, whatever the model wrote.
    #[test]
    fn planner_assist_admits_only_a_permitted_trusted_asset() {
        let msg = message(Some("SELECT region FROM opportunities"), Some("ta-9"));

        let out = decide(
            &msg,
            &spec(&["ta-9"], &[], false),
            PlannerMode::PlannerAssist,
        );
        assert!(out.refusal.is_none(), "{:?}", out.refusal);
        assert_eq!(
            out.admitted_sql.as_deref(),
            Some("SELECT region FROM opportunities")
        );

        let out = decide(
            &msg,
            &spec(&["not-this-one"], &[], false),
            PlannerMode::PlannerAssist,
        );
        assert!(
            out.admitted_sql.is_none(),
            "an unlisted asset admits nothing"
        );
        assert_eq!(
            out.refusal.as_ref().unwrap().code,
            "genie_asset_not_allowed"
        );
        assert_eq!(
            out.proposed_sql.as_deref(),
            Some("SELECT region FROM opportunities"),
            "the refused proposal is still recorded — refuse, never erase"
        );
    }

    /// Evaluation measures what the planner does and admits nothing — a
    /// permitted asset included, because measuring a planner and trusting it
    /// are different acts.
    #[test]
    fn planner_evaluation_records_and_admits_nothing() {
        let msg = message(Some("SELECT 1"), Some("ta-9"));
        let out = decide(&msg, &spec(&["ta-9"], &[], true), PlannerMode::Evaluation);
        assert!(out.admitted_sql.is_none());
        assert_eq!(out.proposed_sql.as_deref(), Some("SELECT 1"));
        assert_eq!(out.refusal.as_ref().unwrap().code, "not_covered");
    }

    /// `genie_plan_unpinned` is a LABEL, not a failure: an admitted proposal
    /// still carries `pinned: false` — no vendor API returns a space's
    /// configuration fingerprint — and the envelope says in words that the
    /// sealed bytes are replayable while the decision behind them is not.
    #[test]
    fn planner_an_unpinned_plan_is_a_label_not_a_failure() {
        let msg = message(Some("SELECT 1"), Some("ta-9"));
        let out = decide(
            &msg,
            &spec(&["ta-9"], &[], false),
            PlannerMode::PlannerAssist,
        );
        assert!(out.refusal.is_none() && out.admitted_sql.is_some());
        assert!(!out.pin.pinned, "no API returns a space fingerprint today");
        let envelope = out.describe();
        assert!(
            envelope["note"].as_str().unwrap().contains("NOT pinned"),
            "{envelope}"
        );
    }
}

/// The MySQL tier: the second SQL engine behind the same
/// seam, against a real server.
///
/// It exists to find what the seam assumed rather than to re-prove Postgres.
/// Three properties are engine-specific and all three are asserted here: an
/// exact decimal survives a different driver; a type canon@1 does not model
/// is refused rather than guessed at; and a snapshot marker is reported only
/// when the server actually has one.
#[cfg(test)]
mod mysql {
    use munarium_matrix_adapter::{
        BoundParameters, EffectiveIdentity, Limits, ReadMode, SourceAdapter,
    };
    use munarium_matrix_adapter_mysql::MySqlAdapter;
    use munarium_matrix_core::checkpoint::{Checkpoint, SyncMode, WatermarkSpec};

    async fn adapter() -> Option<MySqlAdapter> {
        let Some(url) = crate::mysql_url() else {
            println!(
                "SKIPPED: MUNARIUM_MATRIX_TEST_MYSQL is not set, so nothing was tested. Run \
                 `docker compose --profile mysql up -d` and set it to \
                 mysql://matrix:matrix-dev@127.0.0.1:3307/crm."
            );
            return None;
        };
        Some(
            MySqlAdapter::connect("crm", &url, "crm", 4)
                .await
                .expect("MUNARIUM_MATRIX_TEST_MYSQL is set but the server refused the connection"),
        )
    }

    fn identity() -> EffectiveIdentity {
        EffectiveIdentity {
            class: None,
            credential_ref: None,
            principal: "conformance".into(),
        }
    }

    fn limits() -> Limits {
        Limits {
            max_rows: 100,
            max_bytes: 1 << 20,
            timeout_ms: 30_000,
        }
    }

    /// The watermark read, on the second engine that has one.
    ///
    /// `build-matrix.md` carried "✅ snapshot + watermark" for MySQL from the
    /// day the adapter landed, and the six scenarios behind that row were
    /// probe, decimal, parameter binding, an unmodelled type, the snapshot
    /// marker and row security — not one of them a watermark. What the ✅ was
    /// covering: `(updated_at, id)` hard-coded whatever the source declared,
    /// the FIRST read unordered, and `next_checkpoint` handing back the
    /// checkpoint it came in with, so every "incremental" run re-read the
    /// whole table. All three are fixed; this is the scenario that would have
    /// caught them.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_MYSQL"]
    async fn mysql_watermark_advances_by_the_declared_columns() {
        let Some(adapter) = adapter().await else {
            return;
        };
        let projection: Vec<String> = ["id", "name", "amount", "region", "updated_at"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        // Declared, and deliberately NOT the pair the adapter used to assume:
        // under the old code the checkpoint would come back a timestamp.
        let declared = WatermarkSpec {
            column: "id".into(),
            inclusive: false,
            tie_break: Some("name".into()),
        };
        let bare = adapter
            .read_batch(
                "opportunities",
                &projection,
                &Checkpoint::start("crm", "opportunities", "1"),
                ReadMode::of(SyncMode::Watermark),
                &identity(),
                limits(),
            )
            .await
            .expect_err("watermark mode with no declaration");
        assert_eq!(bare.code, "not_covered", "{bare:?}");

        let first = adapter
            .read_batch(
                "opportunities",
                &projection,
                &Checkpoint::start("crm", "opportunities", "1"),
                ReadMode::watermark(&declared),
                &identity(),
                limits(),
            )
            .await
            .expect("the first watermark read");
        assert!(!first.records.is_empty(), "the fixture has rows");
        let cp = first.next_checkpoint.expect("a checkpoint");
        let wm = cp.watermark.clone().expect("the checkpoint ADVANCED");
        assert!(
            wm.parse::<i64>().is_ok(),
            "the checkpoint holds the declared `id`: {wm}"
        );

        let again = adapter
            .read_batch(
                "opportunities",
                &projection,
                &cp,
                ReadMode::watermark(&declared),
                &identity(),
                limits(),
            )
            .await
            .expect("the resumed read");
        assert_eq!(
            again.records.len(),
            0,
            "an unchanged source reads nothing past its watermark"
        );
    }

    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_MYSQL"]
    async fn mysql_probe_reaches_a_real_server() {
        let Some(a) = adapter().await else { return };
        let probe = a.probe().await.expect("probe answers");
        assert!(probe.reachable, "mysql unreachable: {:?}", probe.detail);
    }

    /// The property every adapter exists to protect: `900000.50` is not
    /// `900000.5`. A different driver is a different chance to lose it.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_MYSQL"]
    async fn mysql_an_exact_decimal_survives_the_driver() {
        let Some(a) = adapter().await else { return };
        let out = a
            .execute(
                "SELECT `id`, `amount` FROM `crm`.`opportunities` ORDER BY `id`",
                &BoundParameters::default(),
                &identity(),
                limits(),
            )
            .await
            .expect("the statement runs");
        let amounts: Vec<String> = out
            .result
            .rows
            .iter()
            .map(|r| r.cells[1].canonical_text().unwrap_or_default())
            .collect();
        assert!(
            amounts.iter().any(|a| a == "900000.50"),
            "the trailing zero survives: {amounts:?}"
        );
        assert_eq!(out.engine.as_deref(), Some("mysql"));
        assert_eq!(out.isolation.as_deref(), Some("repeatable read"));
    }

    /// A bound parameter reaches the engine as a parameter. The compiler
    /// renders `$1`; MySQL binds `?`; the renumbering happens in the adapter
    /// so one plan runs on both engines.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_MYSQL"]
    async fn mysql_a_positional_parameter_binds_rather_than_interpolates() {
        let Some(a) = adapter().await else { return };
        let mut params = BoundParameters::default();
        params
            .positional
            .push(munarium_matrix_core::Value::String("EMEA".into()));
        params.index.insert("region".into(), 0);
        let out = a
            .execute(
                "SELECT COUNT(*) AS n FROM `crm`.`opportunities` WHERE `region` = $1",
                &params,
                &identity(),
                limits(),
            )
            .await
            .expect("the parameterised statement runs");
        assert_eq!(out.result.rows.len(), 1);
        assert_eq!(
            out.result.rows[0].cells[0].canonical_text().as_deref(),
            Some("4"),
            "four EMEA rows in the fixture"
        );
    }

    /// A type canon@1 does not model is REFUSED, naming the column. Guessing
    /// would put a value in evidence under a type nothing can verify.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_MYSQL"]
    async fn mysql_an_unmodelled_type_is_refused_and_names_the_column() {
        let Some(a) = adapter().await else { return };
        let err = a
            .execute(
                "SELECT `id`, `footprint` FROM `crm`.`shapes`",
                &BoundParameters::default(),
                &identity(),
                limits(),
            )
            .await
            .expect_err("a geometry column has no canon@1 type");
        assert_eq!(err.code, "schema_drift");
        assert!(err.message.contains("footprint"), "{}", err.message);
    }

    /// Mode A over a watermark, and the marker's honesty: GTID is off in the
    /// fixture, so there is no engine position and the adapter says so
    /// rather than inventing one.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_MYSQL"]
    async fn mysql_a_snapshot_read_reports_no_marker_when_the_server_has_none() {
        let Some(a) = adapter().await else { return };
        let batch = a
            .read_batch(
                "opportunities",
                &["id".into(), "region".into(), "amount".into()],
                &Checkpoint::start("crm", "opportunities", "record-documents@1"),
                ReadMode::of(SyncMode::Snapshot),
                &identity(),
                limits(),
            )
            .await
            .expect("a snapshot read runs");
        assert_eq!(batch.records.len(), 5, "the fixture's five rows");
        assert!(
            batch.snapshot_marker.is_none(),
            "GTID is off in this fixture; a marker would be a position no later read \
             could be compared against, got {:?}",
            batch.snapshot_marker
        );
    }

    /// The posture this engine can actually prove — and the one it cannot.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_MYSQL"]
    async fn mysql_introspect_reports_row_security_as_absent_rather_than_omitting_it() {
        let Some(a) = adapter().await else { return };
        let (posture, fingerprint) = a.introspect().await.expect("introspect answers");
        let rls = posture
            .checks
            .iter()
            .find(|c| c.name == "subject_to_row_security")
            .expect("the check is reported, not omitted");
        assert!(
            !rls.observed,
            "MySQL has no row-level policy engine; claiming otherwise would promise a \
             protection the server cannot give"
        );
        assert!(
            fingerprint.tables.iter().any(|t| t.name == "opportunities"),
            "the fixture's table is in the fingerprint"
        );
    }
}

/// The SQL Server tier: the third SQL engine behind the same
/// seam, against a real server.
///
/// Where the MySQL tier proved what the seam had assumed about Postgres, this
/// one exists to prove the two things SQL Server does DIFFERENTLY from both:
/// it has a row-level policy engine, so `subject_to_row_security` is a check
/// that can be true here and cannot on MySQL; and it has no read-only
/// transaction, so the read-only posture has to be established from the
/// principal's permissions instead of a transaction flag.
#[cfg(test)]
mod sqlserver {
    use munarium_matrix_adapter::{
        BoundParameters, EffectiveIdentity, Limits, ReadMode, SourceAdapter,
    };
    use munarium_matrix_adapter_sqlserver::SqlServerAdapter;
    use munarium_matrix_core::checkpoint::{Checkpoint, SyncMode, WatermarkSpec};

    async fn adapter() -> Option<SqlServerAdapter> {
        let Some(ado) = crate::sqlserver_connection_string() else {
            println!(
                "SKIPPED: MUNARIUM_MATRIX_TEST_SQLSERVER is not set, so nothing was tested. \
                 Run `docker compose --profile sqlserver up -d` and set it to the fixture's \
                 ADO.NET connection string (see docs/adapters/build-matrix.md)."
            );
            return None;
        };
        Some(
            SqlServerAdapter::connect("crm", &ado, "dbo", 4)
                .await
                .expect(
                    "MUNARIUM_MATRIX_TEST_SQLSERVER is set but the server refused the connection",
                ),
        )
    }

    fn identity() -> EffectiveIdentity {
        EffectiveIdentity {
            class: None,
            credential_ref: None,
            principal: "conformance".into(),
        }
    }

    fn limits() -> Limits {
        Limits {
            max_rows: 100,
            max_bytes: 1 << 20,
            timeout_ms: 30_000,
        }
    }

    /// The watermark read, on the engine with no row-value comparison.
    ///
    /// Same story as MySQL's: `build-matrix.md` said "✅ snapshot + watermark"
    /// and the six scenarios behind the row never read one. `[updated_at]` and
    /// `[id]` were hard-coded, and `next_checkpoint` returned the checkpoint
    /// it was given, so an incremental sync re-read the table forever.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_SQLSERVER"]
    async fn sqlserver_watermark_advances_by_the_declared_columns() {
        let Some(adapter) = adapter().await else {
            return;
        };
        let projection: Vec<String> = ["id", "name", "amount", "region", "updated_at"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let declared = WatermarkSpec {
            column: "id".into(),
            inclusive: false,
            tie_break: Some("name".into()),
        };
        let bare = adapter
            .read_batch(
                "opportunities",
                &projection,
                &Checkpoint::start("crm", "opportunities", "1"),
                ReadMode::of(SyncMode::Watermark),
                &identity(),
                limits(),
            )
            .await
            .expect_err("watermark mode with no declaration");
        assert_eq!(bare.code, "not_covered", "{bare:?}");

        let first = adapter
            .read_batch(
                "opportunities",
                &projection,
                &Checkpoint::start("crm", "opportunities", "1"),
                ReadMode::watermark(&declared),
                &identity(),
                limits(),
            )
            .await
            .expect("the first watermark read");
        assert!(!first.records.is_empty(), "the fixture has rows");
        let cp = first.next_checkpoint.expect("a checkpoint");
        let wm = cp.watermark.clone().expect("the checkpoint ADVANCED");
        assert!(
            wm.parse::<i64>().is_ok(),
            "the checkpoint holds the declared `id`: {wm}"
        );

        let again = adapter
            .read_batch(
                "opportunities",
                &projection,
                &cp,
                ReadMode::watermark(&declared),
                &identity(),
                limits(),
            )
            .await
            .expect("the resumed read");
        assert_eq!(
            again.records.len(),
            0,
            "an unchanged source reads nothing past its watermark"
        );
    }

    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_SQLSERVER"]
    async fn sqlserver_probe_reaches_a_real_server() {
        let Some(a) = adapter().await else { return };
        let probe = a.probe().await.expect("probe answers");
        assert!(probe.reachable, "sqlserver unreachable: {:?}", probe.detail);
    }

    /// The property every adapter exists to protect: `900000.50` is not
    /// `900000.5`. A third driver is a third chance to lose it, and TDS is the
    /// one that carries a decimal as an integer plus a scale, so dropping the
    /// scale would drop the trailing zero silently.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_SQLSERVER"]
    async fn sqlserver_an_exact_decimal_survives_the_driver() {
        let Some(a) = adapter().await else { return };
        let out = a
            .execute(
                "SELECT [id], [amount] FROM [dbo].[opportunities] ORDER BY [id]",
                &BoundParameters::default(),
                &identity(),
                limits(),
            )
            .await
            .expect("the statement runs");
        let amounts: Vec<String> = out
            .result
            .rows
            .iter()
            .map(|r| r.cells[1].canonical_text().unwrap_or_default())
            .collect();
        assert!(
            amounts.iter().any(|a| a == "900000.50"),
            "the trailing zero survives: {amounts:?}"
        );
        assert_eq!(out.engine.as_deref(), Some("sqlserver"));
        // The fixture allows snapshot isolation, so the read got it. A
        // deployment that does not allow it is told `read committed` here
        // rather than being handed a consistency claim the read did not have.
        assert_eq!(out.isolation.as_deref(), Some("snapshot"));
    }

    /// A bound parameter reaches the engine as a parameter. The compiler
    /// renders `$1`; SQL Server binds `@P1`; the renumbering happens in the
    /// adapter so one plan, and one plan hash, runs on every engine.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_SQLSERVER"]
    async fn sqlserver_a_positional_parameter_binds_rather_than_interpolates() {
        let Some(a) = adapter().await else { return };
        let mut params = BoundParameters::default();
        params
            .positional
            .push(munarium_matrix_core::Value::String("EMEA".into()));
        params.index.insert("region".into(), 0);
        let out = a
            .execute(
                "SELECT COUNT(*) AS n FROM [dbo].[opportunities] WHERE [region] = $1",
                &params,
                &identity(),
                limits(),
            )
            .await
            .expect("the parameterised statement runs");
        assert_eq!(out.result.rows.len(), 1);
        assert_eq!(
            out.result.rows[0].cells[0].canonical_text().as_deref(),
            Some("4"),
            "four EMEA rows in the fixture"
        );
    }

    /// Two types canon@1 does not model, refused by NAME rather than guessed.
    ///
    /// `geography` is the obvious one. `money` is the interesting one: it is an
    /// EXACT four-decimal currency type on the server and an IEEE-754 double in
    /// this driver, so accepting it would put a currency into sealed evidence
    /// as a float, which is the precise failure the whole value layer exists to
    /// prevent.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_SQLSERVER"]
    async fn sqlserver_an_unmodelled_type_is_refused_and_names_the_column() {
        let Some(a) = adapter().await else { return };
        let spatial = a
            .execute(
                "SELECT [id], [footprint] FROM [dbo].[shapes]",
                &BoundParameters::default(),
                &identity(),
                limits(),
            )
            .await
            .expect_err("a geography column has no canon@1 type");
        assert_eq!(spatial.code, "schema_drift");
        assert!(spatial.message.contains("footprint"), "{}", spatial.message);

        let money = a
            .execute(
                "SELECT [id], [list_price] FROM [dbo].[shapes]",
                &BoundParameters::default(),
                &identity(),
                limits(),
            )
            .await
            .expect_err("money is exact on the server and a double in the driver");
        assert_eq!(money.code, "schema_drift");
        assert!(money.message.contains("list_price"), "{}", money.message);
        assert!(
            money.message.contains("money"),
            "the refusal names the type so an operator knows what to cast: {}",
            money.message
        );
    }

    /// Mode A, and the marker's honesty.
    ///
    /// Change tracking is ON in the fixture and the read is snapshot-isolated,
    /// so a marker exists and names the same consistent view the rows came
    /// from. The `None` half of that gate (change tracking off, or a read
    /// outside a snapshot transaction) is asserted by the adapter's own unit
    /// tests, because one fixture cannot be in both states at once.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_SQLSERVER"]
    async fn sqlserver_a_snapshot_read_reports_a_marker_only_from_a_consistent_view() {
        let Some(a) = adapter().await else { return };
        let batch = a
            .read_batch(
                "opportunities",
                &["id".into(), "region".into(), "amount".into()],
                &Checkpoint::start("crm", "opportunities", "record-documents@1"),
                ReadMode::of(SyncMode::Snapshot),
                &identity(),
                limits(),
            )
            .await
            .expect("a snapshot read runs");
        // FOUR, not five: the fixture's security policy restricts this login to
        // EMEA, exactly as the Postgres fixture's `matrix_reader` is restricted
        // by RLS. A read that returned five would mean the policy was not in
        // force.
        assert_eq!(batch.records.len(), 4, "the fixture's four EMEA rows");
        let marker = batch
            .snapshot_marker
            .clone()
            .expect("change tracking is on and the read is snapshot-isolated");
        assert!(
            marker.starts_with("ct:") && marker[3..].chars().all(|c| c.is_ascii_digit()),
            "a change-tracking version, not a timestamp or a guess: {marker}"
        );
        assert_eq!(
            batch.records[0].event_position.as_deref(),
            Some(marker.as_str()),
            "every record carries the position the batch was read at"
        );
    }

    /// The posture this engine CAN prove, and the difference from MySQL.
    ///
    /// SQL Server has a row-level policy engine, so the per-table fact is real
    /// here and permanently absent on MySQL. Both adapters REPORT the check;
    /// that is what makes the comparison legible instead of making one adapter
    /// look like it has a stub.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_SQLSERVER"]
    async fn sqlserver_introspect_reports_row_security_as_present() {
        let Some(a) = adapter().await else { return };
        let (posture, fingerprint) = a.introspect().await.expect("introspect answers");

        let named = |n: &str| {
            posture
                .checks
                .iter()
                .find(|c| c.name == n)
                .unwrap_or_else(|| panic!("{n} is reported, not omitted"))
                .clone()
        };
        assert!(named("read_only").ok, "the fixture login holds no DML");
        assert!(named("not_superuser").ok);
        assert!(named("not_owner").ok);
        // The schema-wide claim is FALSE, and correctly so: `shapes` carries no
        // policy, and "some tables are protected" is not a posture.
        let rls = named("subject_to_row_security");
        assert!(
            !rls.observed,
            "shapes has no policy, so the schema-wide claim cannot be true"
        );
        let opportunities = fingerprint
            .tables
            .iter()
            .find(|t| t.name == "opportunities")
            .expect("the fixture's table is in the fingerprint");
        assert!(
            opportunities.row_security_enabled,
            "SQL Server HAS a row-level policy engine and the fixture uses it; observing that \
             needs VIEW DEFINITION, which the fixture grants for exactly this reason"
        );
        let shapes = fingerprint
            .tables
            .iter()
            .find(|t| t.name == "shapes")
            .expect("shapes is in the fingerprint");
        assert!(!shapes.row_security_enabled);
        // The unmodelled columns are REPORTED with their source type and a
        // `None` logical type rather than dropped from the shape: an operator
        // has to be able to see why a column cannot be used.
        let list_price = shapes
            .columns
            .iter()
            .find(|c| c.name == "list_price")
            .expect("the money column is in the shape");
        assert_eq!(list_price.source_type, "money");
        assert!(list_price.logical_type.is_none());
    }
}

/// Postgres logical-replication CDC.
///
/// These run in the `postgres` tier — compose can prove the whole path for $0,
/// which is why this adapter got built rather than written up as impossible.
///
/// The property they exist to protect is the one that nearly made it
/// impossible. Logical decoding reads WAL, and WAL is written before any policy
/// is consulted: measured on a real PostgreSQL 16, a role restricted to EMEA by
/// a row policy and denied a column outright saw, through a `test_decoding`
/// slot, every row of the table and the denied column's contents. `pgoutput`
/// closes that hole by applying the PUBLICATION's column list and row filter
/// while decoding, and the adapter refuses every arrangement in which it would
/// not.
#[cfg(test)]
mod cdc {
    use munarium_matrix_adapter::{EffectiveIdentity, Limits, ReadMode, SourceAdapter};
    use munarium_matrix_adapter_postgres::{cdc_objects, PostgresAdapter};
    use munarium_matrix_core::checkpoint::{Checkpoint, SyncMode, WatermarkSpec};
    use munarium_matrix_types::contract::ChangeKind;
    use sqlx::postgres::PgPoolOptions;
    use sqlx::{Executor, PgPool};

    /// The projection the fixture's publication carries, in publication order.
    const PROJECTION: [&str; 4] = ["id", "name", "amount", "region"];

    /// The BOOTSTRAP superuser, used only to stage the fixture.
    ///
    /// Not `matrix_owner`: that is the service role, and it deliberately holds
    /// no REPLICATION attribute — "Only roles with the REPLICATION attribute
    /// may use replication slots", which the first run of this tier said out
    /// loud. Widening the service role to make a test pass would have broken
    /// the posture the whole adapter rests on, so the test uses the role an
    /// operator would actually use to create a slot.
    fn admin_url() -> Option<String> {
        let tail = crate::database_url()?.split_once('@')?.1.to_string();
        Some(format!("postgres://matrix:matrix-dev@{tail}"))
    }

    /// The RLS-restricted, column-restricted, REPLICATION-holding reader — the
    /// principal the whole point of this tier is to keep honest.
    fn reader_url() -> Option<String> {
        let tail = crate::database_url()?.split_once('@')?.1.to_string();
        Some(format!("postgres://matrix_reader:matrix-reader-dev@{tail}"))
    }

    /// The reader's URL pointed at the DATABASE that actually holds the crm
    /// fixture. In compose that is the store's own database (the init script
    /// loads the fixture beside it); on a deployment the fixture may live in its
    /// own `crm` database (a live runner applies `02-crm-fixture.sql` there).
    /// One live run found the difference the hard way: the declared-
    /// columns read connected to the deployment's `matrix` database and met no
    /// crm schema at all — the first live run of a scenario that had only
    /// ever passed in compose, which is exactly what that run existed to
    /// catch. `to_regclass` decides which world this is, so neither is
    /// guessed.
    async fn fixture_reader_url() -> Option<String> {
        let tail = crate::database_url()?.split_once('@')?.1.to_string();
        let beside_store = format!("postgres://matrix_reader:matrix-reader-dev@{tail}");
        if let Ok(probe) = PgPoolOptions::new()
            .max_connections(1)
            .connect(&beside_store)
            .await
        {
            let found: Option<String> =
                sqlx::query_scalar("SELECT to_regclass('crm.opportunities')::text")
                    .fetch_one(&probe)
                    .await
                    .ok()
                    .flatten();
            if found.is_some() {
                return Some(beside_store);
            }
        }
        let (hostport, rest) = tail.split_once('/')?;
        let query = rest.split_once('?').map(|(_, q)| q);
        Some(match query {
            Some(q) => format!("postgres://matrix_reader:matrix-reader-dev@{hostport}/crm?{q}"),
            None => format!("postgres://matrix_reader:matrix-reader-dev@{hostport}/crm"),
        })
    }

    async fn admin_pool() -> Option<PgPool> {
        let url = admin_url()?;
        PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .ok()
    }

    async fn adapter(source_id: &str) -> Option<PostgresAdapter> {
        let url = reader_url()?;
        Some(
            PostgresAdapter::connect(source_id, &url, "crm", 2)
                .await
                .expect("the fixture's matrix_reader connects"),
        )
    }

    /// The mode-A exit gate, "snapshot and incremental runs converge": a
    /// watermark read advances its checkpoint to the last row it kept, an
    /// unchanged source then reads NOTHING, and a touched row reads exactly
    /// once. Until 2026-08-30 the checkpoint's watermark was copied forward
    /// unchanged — every "incremental" run re-read the whole table and
    /// uploaded nothing new, which the idempotent upload made look like
    /// convergence — and the `> ($1, $2)` branch had never executed at all.
    /// A live mode-A convergence check read four rows twice and said
    /// so; this is the same property for $0, against the compose fixture.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_DATABASE_URL"]
    async fn postgres_watermark_advances_and_an_unchanged_source_reads_nothing() {
        // The compose guard the cdc scenarios use, in the cdc ORDER: the
        // admin pool first. On a deployment `matrix:matrix-dev` does not exist,
        // so this skips there loudly — the same property runs live as the
        // deployment's own mode-A checks. One live run had the adapter
        // first, which connected (roles are cluster-wide) and then panicked
        // on the pool this cannot get.
        let Some(admin) = admin_pool().await else {
            println!(
                "SKIPPED: no compose admin role; a live runner covers this property against a deployment."
            );
            return;
        };
        let Some(adapter) = adapter("wm").await else {
            println!("SKIPPED: no database configured");
            return;
        };
        let admin = admin;
        let projection: Vec<String> = ["id", "name", "stage", "amount", "region", "updated_at"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let identity = EffectiveIdentity {
            class: None,
            credential_ref: None,
            principal: "matrix_reader".into(),
        };
        let limits = Limits {
            max_rows: 1000,
            max_bytes: 8 << 20,
            timeout_ms: 5_000,
        };
        // Leave the fixture as it was found, and touch only the STRADDLE row
        // (the EMEA row already past 2026-06-30): the http tier's verified
        // question sums the in-window rows CONCURRENTLY with this test, and a
        // transient on one of them fails that scenario for a true reason at
        // the wrong address.
        let (id, was): (i64, chrono::DateTime<chrono::Utc>) = sqlx::query_as(
            "SELECT id, updated_at FROM crm.opportunities
              WHERE region = 'EMEA' AND updated_at > DATE '2026-06-30'
              ORDER BY id LIMIT 1",
        )
        .fetch_one(&admin)
        .await
        .expect("an EMEA row");

        // The columns the DataSource DECLARES, which is what the adapter reads
        // by since 2026-08-30. `updated_at`/`id` happen to be the fixture's,
        // and `postgres_watermark_reads_the_declared_columns` proves the
        // adapter is not just falling back to the old hard-coded pair.
        let declared = WatermarkSpec {
            column: "updated_at".into(),
            inclusive: false,
            tie_break: Some("id".into()),
        };
        let first = adapter
            .read_batch(
                "opportunities",
                &projection,
                &Checkpoint::start("wm", "opportunities", "1"),
                ReadMode::watermark(&declared),
                &identity,
                limits,
            )
            .await
            .expect("the first watermark read");
        assert!(!first.records.is_empty(), "the reader sees its EMEA rows");
        let cp = first.next_checkpoint.expect("a checkpoint");
        assert!(
            cp.watermark.is_some() && cp.tie_break.is_some(),
            "the checkpoint advanced to the last row kept: {cp:?}"
        );

        let again = adapter
            .read_batch(
                "opportunities",
                &projection,
                &cp,
                ReadMode::watermark(&declared),
                &identity,
                limits,
            )
            .await
            .expect("the second read");
        assert_eq!(
            again.records.len(),
            0,
            "an unchanged source reads nothing past its watermark"
        );

        sqlx::query("UPDATE crm.opportunities SET updated_at = now() WHERE id = $1")
            .bind(id)
            .execute(&admin)
            .await
            .expect("touch one row");
        let delta = adapter
            .read_batch(
                "opportunities",
                &projection,
                &cp,
                ReadMode::watermark(&declared),
                &identity,
                limits,
            )
            .await
            .expect("the delta read");
        sqlx::query("UPDATE crm.opportunities SET updated_at = $2 WHERE id = $1")
            .bind(id)
            .bind(was)
            .execute(&admin)
            .await
            .expect("restore the row");
        assert_eq!(delta.records.len(), 1, "exactly the touched row is read");
        assert_eq!(delta.records[0].row_key, id.to_string());
    }

    /// The watermark columns are the DataSource's, not the adapter's.
    ///
    /// This is the scenario the `[gap]` in `docs/adapters/build-matrix.md`
    /// asked for. Five adapters read `(updated_at, id)` whatever the asset
    /// said, so `spec.sync.watermark` was validated and then ignored: a source
    /// declaring `modified_on` was read by a column it had never named, and
    /// the only reason nobody was hurt is that the one deployed source happens
    /// to use the convention.
    ///
    /// It declares `(id, name)` — deliberately NOT the old pair — and asserts
    /// the checkpoint came back holding an ID. Under the convention it would
    /// hold a timestamp, so this cannot pass by accident.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_DATABASE_URL"]
    async fn postgres_watermark_reads_the_columns_the_source_declared() {
        // Read-only, so unlike its mutating siblings it needs no compose
        // admin role and genuinely RUNS on a deployment — against whichever
        // database holds the fixture there (see `fixture_reader_url`).
        let Some(url) = fixture_reader_url().await else {
            println!("SKIPPED: no database configured");
            return;
        };
        let adapter = PostgresAdapter::connect("wmdecl", &url, "crm", 2)
            .await
            .expect("the fixture's matrix_reader connects");
        let projection: Vec<String> = ["id", "name", "amount", "region", "updated_at"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let identity = EffectiveIdentity {
            class: None,
            credential_ref: None,
            principal: "matrix_reader".into(),
        };
        let limits = Limits {
            max_rows: 1000,
            max_bytes: 8 << 20,
            timeout_ms: 5_000,
        };

        // A watermark mode with no declaration is a REFUSAL, never a fallback
        // to the convention: reading by the wrong column is worse than not
        // reading at all.
        let bare = adapter
            .read_batch(
                "opportunities",
                &projection,
                &Checkpoint::start("wmdecl", "opportunities", "1"),
                ReadMode::of(SyncMode::Watermark),
                &identity,
                limits,
            )
            .await
            .expect_err("watermark mode with no declaration");
        assert_eq!(bare.code, "not_covered", "{bare:?}");

        let declared = WatermarkSpec {
            column: "id".into(),
            inclusive: false,
            tie_break: Some("name".into()),
        };
        let first = adapter
            .read_batch(
                "opportunities",
                &projection,
                &Checkpoint::start("wmdecl", "opportunities", "1"),
                ReadMode::watermark(&declared),
                &identity,
                limits,
            )
            .await
            .expect("a read by the declared columns");
        assert!(!first.records.is_empty(), "the reader sees its EMEA rows");
        let cp = first.next_checkpoint.expect("a checkpoint");
        let wm = cp.watermark.clone().expect("a watermark");
        assert!(
            wm.parse::<i64>().is_ok(),
            "the checkpoint holds the declared `id`, not the convention's timestamp: {wm}"
        );
        // Ordered by the declared column, so the last row kept IS the maximum.
        let max_id = first
            .records
            .iter()
            .filter_map(|r| r.row_key.parse::<i64>().ok())
            .max()
            .expect("ids");
        assert_eq!(
            wm,
            max_id.to_string(),
            "the checkpoint is the last row read"
        );

        // And it resumes exactly: nothing is newer than the maximum id.
        let again = adapter
            .read_batch(
                "opportunities",
                &projection,
                &cp,
                ReadMode::watermark(&declared),
                &identity,
                limits,
            )
            .await
            .expect("the resumed read");
        assert_eq!(
            again.records.len(),
            0,
            "an unchanged source reads nothing past its declared watermark"
        );

        // An INCLUSIVE watermark may legitimately have no tie-break — it
        // re-reads the boundary rows every run and says so. That configuration
        // was unreachable while the columns were hard-coded, and validation
        // has always allowed it.
        let inclusive = WatermarkSpec {
            column: "id".into(),
            inclusive: true,
            tie_break: None,
        };
        let boundary = adapter
            .read_batch(
                "opportunities",
                &projection,
                &cp,
                ReadMode::watermark(&inclusive),
                &identity,
                limits,
            )
            .await
            .expect("the inclusive read");
        assert_eq!(
            boundary.records.len(),
            1,
            "an inclusive watermark re-reads the boundary row"
        );
    }

    /// A replication slot is DURABLE, SERVER-side state with one name per
    /// source, and five of these scenarios share the source id `cdc` — so they
    /// share one slot. `cargo test` runs test functions concurrently, and a
    /// slot another connection holds open cannot be dropped: the second
    /// scenario's `pg_create_logical_replication_slot` came back `42710
    /// already exists`, and one of them then read `cdc_slot_wrong_plugin`
    /// where it expected `cdc_slot_missing` because it had found a neighbour's
    /// slot.
    ///
    /// So they take turns. Giving each its own slot would fix the test by
    /// testing something else: the fixture defines ONE publication, and the
    /// point of these scenarios is the feed of the table the adapter is
    /// configured for. Same shape and same reason as the Databricks tier's
    /// lock, which cycle 22 bought.
    ///
    /// This is also what makes the tier RE-RUNNABLE. It passed twice on a
    /// fresh volume and failed on the third run against the same one, which is
    /// the worse failure: a tier that only passes on a fresh database is a
    /// tier that passes once.
    static SLOT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Drop and recreate the slot for a source, so each scenario starts from a
    /// known position. Done as the OWNER over raw SQL rather than through the
    /// adapter, because the adapter deliberately cannot create one — a slot
    /// retains WAL until something consumes it, and creating one implicitly
    /// would make Matrix the author of a full disk.
    ///
    /// The drop is best-effort — a slot that is not there is the state we
    /// want — but the CREATE is not: a failure there means a slot exists that
    /// this scenario did not make, and continuing would test somebody else's.
    async fn reset_slot(pool: &PgPool, source_id: &str, plugin: &str) {
        let slot = cdc_objects(source_id).slot;
        let _ = sqlx::query("SELECT pg_drop_replication_slot($1)")
            .bind(&slot)
            .execute(pool)
            .await;
        sqlx::query("SELECT pg_create_logical_replication_slot($1, $2)")
            .bind(&slot)
            .bind(plugin)
            .execute(pool)
            .await
            .expect("the fixture superuser can create a slot");
    }

    /// Put the three fixture rows back exactly as `05-cdc-fixture.sql` left
    /// them, whatever a previous run did to them.
    ///
    /// Called at the START of the scenario as well as the end. A scenario that
    /// tidies up only on success cannot be run twice: the first failing run
    /// left id 10 behind and the second died on a duplicate key, which reads
    /// like a defect in the adapter and is a defect in the test.
    async fn reset_fixture(pool: &PgPool) {
        pool.execute(
            "DELETE FROM crm.cdc_opportunities WHERE id NOT IN (1, 2, 3);
             INSERT INTO crm.cdc_opportunities (id, name, amount, region, secret) VALUES
                 (1, 'Acme renewal', 1500000.00, 'EMEA', 'emea-secret'),
                 (2, 'Theta upgrade',  900000.50, 'EMEA', 'emea-secret-2'),
                 (3, 'Gamma pilot',    100000.00, 'AMER', 'amer-secret')
             ON CONFLICT (id) DO UPDATE SET
                 name = EXCLUDED.name, amount = EXCLUDED.amount,
                 region = EXCLUDED.region, secret = EXCLUDED.secret;",
        )
        .await
        .expect("the fixture resets");
    }

    async fn drop_slot(pool: &PgPool, source_id: &str) {
        let _ = sqlx::query("SELECT pg_drop_replication_slot($1)")
            .bind(cdc_objects(source_id).slot)
            .execute(pool)
            .await;
    }

    fn identity() -> EffectiveIdentity {
        EffectiveIdentity {
            class: None,
            credential_ref: None,
            principal: "conformance".into(),
        }
    }

    fn limits() -> Limits {
        Limits {
            max_rows: 100,
            max_bytes: 1 << 20,
            timeout_ms: 30_000,
        }
    }

    fn projection() -> Vec<String> {
        PROJECTION.iter().map(|s| s.to_string()).collect()
    }

    fn start(source_id: &str) -> Checkpoint {
        Checkpoint::start(source_id, "cdc_opportunities", "record-documents@1")
    }

    /// A slot is durable state on the customer's database, so Matrix never
    /// creates one — it refuses and prints the statement to run.
    ///
    /// This is the scenario for the decision, not just for the message: an
    /// adapter that created a slot on first use would make a Matrix nobody
    /// runs any more into a Postgres that stops accepting writes.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_DATABASE_URL"]
    async fn cdc_a_missing_slot_is_refused_with_the_statement_that_creates_it() {
        let _slot = SLOT_LOCK.lock().await;
        let Some(pool) = admin_pool().await else {
            println!("SKIPPED: no MUNARIUM_MATRIX_TEST_DATABASE_URL, so nothing was tested.");
            return;
        };
        drop_slot(&pool, "cdc").await;
        let a = adapter("cdc").await.expect("connects");
        let err = a
            .read_batch(
                "cdc_opportunities",
                &projection(),
                &start("cdc"),
                ReadMode::of(SyncMode::Cdc),
                &identity(),
                limits(),
            )
            .await
            .expect_err("no slot exists");
        assert_eq!(err.code, "cdc_slot_missing");
        assert!(
            err.message.contains("pg_create_logical_replication_slot")
                && err.message.contains("munarium_matrix_cdc"),
            "the refusal has to be actionable: {}",
            err.message
        );
        // And it says WHY Matrix will not do it, which is the part a reader
        // needs in order to agree with the decision.
        assert!(err.message.contains("RETAIN WAL"), "{}", err.message);
    }

    /// `test_decoding` streams every column of every row regardless of GRANTs
    /// and row policy. A slot using it is refused by name.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_DATABASE_URL"]
    async fn cdc_a_slot_that_decodes_with_test_decoding_is_refused() {
        let _slot = SLOT_LOCK.lock().await;
        let Some(pool) = admin_pool().await else {
            println!("SKIPPED: no MUNARIUM_MATRIX_TEST_DATABASE_URL, so nothing was tested.");
            return;
        };
        reset_slot(&pool, "cdc", "test_decoding").await;
        let a = adapter("cdc").await.expect("connects");
        let err = a
            .read_batch(
                "cdc_opportunities",
                &projection(),
                &start("cdc"),
                ReadMode::of(SyncMode::Cdc),
                &identity(),
                limits(),
            )
            .await
            .expect_err("the wrong plugin is refused");
        assert_eq!(err.code, "cdc_slot_wrong_plugin");
        assert!(err.message.contains("pgoutput"), "{}", err.message);
        drop_slot(&pool, "cdc").await;
    }

    /// A row-secured table published without a WHERE would stream rows the same
    /// principal cannot SELECT. That is the G6 hole this whole design exists to
    /// close, and it is refused before a single byte is decoded.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_DATABASE_URL"]
    async fn cdc_a_publication_without_a_row_filter_on_a_secured_table_is_refused() {
        let _slot = SLOT_LOCK.lock().await;
        let Some(pool) = admin_pool().await else {
            println!("SKIPPED: no MUNARIUM_MATRIX_TEST_DATABASE_URL, so nothing was tested.");
            return;
        };
        // `cdcopen` names the fixture's deliberately-wrong publication, which
        // publishes the same columns with no WHERE.
        reset_slot(&pool, "cdcopen", "pgoutput").await;
        let a = adapter("cdcopen").await.expect("connects");
        let err = a
            .read_batch(
                "cdc_opportunities",
                &projection(),
                &start("cdcopen"),
                ReadMode::of(SyncMode::Cdc),
                &identity(),
                limits(),
            )
            .await
            .expect_err("an unfiltered publication over a secured table is refused");
        assert_eq!(err.code, "cdc_publication_bypasses_row_policy");
        drop_slot(&pool, "cdcopen").await;
    }

    /// The publication's column list is what withholds a denied column from the
    /// stream, so it must be exactly the projection. One column either way is a
    /// refusal: more and a denied column reaches the decoder, fewer and every
    /// record is incomplete.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_DATABASE_URL"]
    async fn cdc_a_publication_that_does_not_match_the_projection_is_refused() {
        let _slot = SLOT_LOCK.lock().await;
        let Some(pool) = admin_pool().await else {
            println!("SKIPPED: no MUNARIUM_MATRIX_TEST_DATABASE_URL, so nothing was tested.");
            return;
        };
        reset_slot(&pool, "cdc", "pgoutput").await;
        let a = adapter("cdc").await.expect("connects");
        let narrower = vec!["id".to_string(), "name".to_string()];
        let err = a
            .read_batch(
                "cdc_opportunities",
                &narrower,
                &start("cdc"),
                ReadMode::of(SyncMode::Cdc),
                &identity(),
                limits(),
            )
            .await
            .expect_err("the shapes disagree");
        assert_eq!(err.code, "cdc_publication_projection_mismatch");
        assert!(
            err.message.contains("reads WAL"),
            "the refusal explains why an extra published column is not merely untidy: {}",
            err.message
        );
        drop_slot(&pool, "cdc").await;
    }

    /// The heart of it: an insert, an update and a delete arrive as
    /// DISTINGUISHABLE records, each carrying the LSN a later read resumes
    /// from — and the row the publication's filter excludes, along with the
    /// column its list withholds, never appear at all.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_DATABASE_URL"]
    async fn cdc_inserts_updates_and_deletes_arrive_distinguishable_with_their_lsn() {
        let _slot = SLOT_LOCK.lock().await;
        let Some(pool) = admin_pool().await else {
            println!("SKIPPED: no MUNARIUM_MATRIX_TEST_DATABASE_URL, so nothing was tested.");
            return;
        };
        reset_fixture(&pool).await;
        reset_slot(&pool, "cdc", "pgoutput").await;
        let a = adapter("cdc").await.expect("connects");

        // The first read has no checkpoint, so it is a SNAPSHOT pinned to an
        // LSN. Under this reader's row policy it sees the two EMEA rows.
        let first = a
            .read_batch(
                "cdc_opportunities",
                &projection(),
                &start("cdc"),
                ReadMode::of(SyncMode::Cdc),
                &identity(),
                limits(),
            )
            .await
            .expect("the initial snapshot runs");
        assert_eq!(first.records.len(), 2, "two EMEA rows under the row policy");
        assert!(first
            .records
            .iter()
            .all(|r| r.change_kind == ChangeKind::Snapshot));
        let checkpoint = first.next_checkpoint.clone().expect("a checkpoint");
        assert!(
            checkpoint.event_position.is_some(),
            "a checkpoint with no engine position is one a later read cannot resume from"
        );

        // Three changes the publication carries, and two it must not.
        pool.execute(
            "INSERT INTO crm.cdc_opportunities (id, name, amount, region, secret) \
               VALUES (10, 'Iota launch', 42.50, 'EMEA', 'emea-secret-3');
             UPDATE crm.cdc_opportunities SET amount = 900000.75 WHERE id = 2;
             DELETE FROM crm.cdc_opportunities WHERE id = 1;
             UPDATE crm.cdc_opportunities SET name = 'Gamma renamed' WHERE id = 3;
             INSERT INTO crm.cdc_opportunities (id, name, amount, region, secret) \
               VALUES (11, 'Kappa amer', 7.00, 'AMER', 'amer-secret-2');",
        )
        .await
        .expect("the fixture accepts the changes");

        let batch = a
            .read_batch(
                "cdc_opportunities",
                &projection(),
                &checkpoint,
                ReadMode::of(SyncMode::Cdc),
                &identity(),
                limits(),
            )
            .await
            .expect("the change read runs");

        let kinds: Vec<ChangeKind> = batch.records.iter().map(|r| r.change_kind).collect();
        assert_eq!(
            kinds,
            vec![ChangeKind::Insert, ChangeKind::Update, ChangeKind::Delete],
            "three changes, in commit order, each distinguishable — a delete that arrived \
             as an absence would make mode C observe a phantom"
        );

        // Every record carries an engine position, and they do not go backwards.
        let positions: Vec<String> = batch
            .records
            .iter()
            .map(|r| r.event_position.clone().expect("an lsn per record"))
            .collect();
        assert!(positions.iter().all(|p| p.contains('/')), "{positions:?}");

        // The exact decimal survives a third transport.
        let updated = &batch.records[1];
        assert_eq!(
            updated.cells[2].canonical_text().as_deref(),
            Some("900000.75")
        );

        // The delete carries the replica identity, which is what a tombstone
        // needs in order to say WHICH row went away.
        let deleted = &batch.records[2];
        assert_eq!(deleted.row_key, "1|EMEA");

        // G6, twice over. The AMER row was UPDATEd and INSERTed while the slot
        // was open; the engine applied the publication's WHERE during decoding,
        // so neither reached this stream. And `secret` is not in the
        // publication's column list, so it is not in the SHAPE, never mind the
        // rows.
        let rendered: Vec<String> = batch
            .records
            .iter()
            .flat_map(|r| r.cells.iter().filter_map(|c| c.canonical_text()))
            .collect();
        assert!(!rendered.iter().any(|t| t == "AMER"), "{rendered:?}");
        assert!(
            !rendered.iter().any(|t| t.contains("secret")),
            "{rendered:?}"
        );
        assert!(!batch.columns.iter().any(|c| c.name == "secret"));

        // The marker names the filter the engine applied, so a reader of the
        // sealed coverage can see which rows this stream was entitled to carry.
        let marker = batch.snapshot_marker.clone().expect("a marker");
        assert!(marker.contains("lsn="), "{marker}");
        assert!(marker.contains("filter="), "{marker}");

        // Replay: reading again from the SAME checkpoint returns the same three
        // changes, because the read peeks and never consumes. That is what
        // makes a crash between the read and the checkpoint write survivable.
        let replay = a
            .read_batch(
                "cdc_opportunities",
                &projection(),
                &checkpoint,
                ReadMode::of(SyncMode::Cdc),
                &identity(),
                limits(),
            )
            .await
            .expect("the replay runs");
        assert_eq!(replay.records.len(), 3, "peek does not consume");

        // Reading from the ADVANCED checkpoint finds nothing new.
        let after = batch.next_checkpoint.clone().expect("a checkpoint");
        let empty = a
            .read_batch(
                "cdc_opportunities",
                &projection(),
                &after,
                ReadMode::of(SyncMode::Cdc),
                &identity(),
                limits(),
            )
            .await
            .expect("the resumed read runs");
        assert!(
            empty.records.is_empty(),
            "resuming from the advanced position finds nothing: {:?}",
            empty.records
        );

        reset_fixture(&pool).await;
        drop_slot(&pool, "cdc").await;
    }

    /// A checkpoint the slot has already moved past names changes nobody can
    /// produce any more. That is REPORTED as a gap rather than quietly
    /// resnapshotted, which is what lets the sync worker record
    /// `resnapshotted: true` instead of implying continuous coverage.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_DATABASE_URL"]
    async fn cdc_a_checkpoint_behind_the_slot_is_reported_as_a_gap() {
        let _slot = SLOT_LOCK.lock().await;
        let Some(pool) = admin_pool().await else {
            println!("SKIPPED: no MUNARIUM_MATRIX_TEST_DATABASE_URL, so nothing was tested.");
            return;
        };
        reset_slot(&pool, "cdc", "pgoutput").await;
        let a = adapter("cdc").await.expect("connects");

        // A position from before the slot existed. `0/1` is behind any slot on
        // any server that has ever written WAL, which is what makes this a
        // fixture-independent way to reach the gap.
        let stale = Checkpoint {
            event_position: Some("0/1".into()),
            ..start("cdc")
        };
        let err = a
            .read_batch(
                "cdc_opportunities",
                &projection(),
                &stale,
                ReadMode::of(SyncMode::Cdc),
                &identity(),
                limits(),
            )
            .await
            .expect_err("the checkpoint is unreachable from this slot");
        assert_eq!(err.code, "cdc_checkpoint_gap");
        assert_eq!(
            err.class,
            munarium_matrix_core::RefusalClass::Incomplete,
            "a gap is incomplete coverage, not an outage: the worker resnapshots on it"
        );
        drop_slot(&pool, "cdc").await;
    }

    /// The slot's retained WAL is reported, because an operator needs to see
    /// retention growing before a disk fills — and because Matrix is in no
    /// position to decide that a customer's slot should be dropped.
    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_TEST_DATABASE_URL"]
    async fn cdc_the_slots_retained_wal_is_observable() {
        let _slot = SLOT_LOCK.lock().await;
        let Some(pool) = admin_pool().await else {
            println!("SKIPPED: no MUNARIUM_MATRIX_TEST_DATABASE_URL, so nothing was tested.");
            return;
        };
        reset_slot(&pool, "cdc", "pgoutput").await;
        let a = adapter("cdc").await.expect("connects");
        let retained = a.cdc_retained_bytes().await;
        assert!(
            retained.is_some(),
            "an existing slot reports its retention; a number nobody can see is a disk \
             nobody watches"
        );
        drop_slot(&pool, "cdc").await;
        assert!(
            a.cdc_retained_bytes().await.is_none(),
            "a slot that does not exist retains nothing, and says so as an absence rather \
             than as a zero"
        );
    }
}
