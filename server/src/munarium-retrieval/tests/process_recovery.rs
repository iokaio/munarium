// SPDX-License-Identifier: Apache-2.0
//! Real mirror publication process death, with PostgreSQL continuously running.
#[path = "../../../tests/support/process_crash.rs"]
mod harness;
use munarium_datastore::shard::{OpenShard, MANIFEST};
use munarium_datastore::verify::{Limits, ReaderCapabilities};
use munarium_retrieval::backfill::backfill_one;
use munarium_retrieval::mirror::{
    reconcile_attempts, BuildPhase, LocalStoreFactory, MirrorContext, MirrorOutcome, MirrorTarget,
    VectorPolicy,
};
use munarium_retrieval_pg::PgRetrieval;
use munarium_store_pg::{ArtifactCatalog, ArtifactState, BindingSlot, BuildAttempts, PgStore};
use serde_json::{json, Value};
use std::sync::Arc;

#[test]
fn artifact_process_recovery() {
    if std::env::var_os("MUNARIUM_TEST_DATABASE_URL").is_none() {
        eprintln!("UNAVAILABLE: artifact process recovery needs disposable PostgreSQL");
        return;
    }
    for phase in [
        "AfterClaim",
        "AfterExport",
        "AfterSeal",
        "AfterCatalogInsert",
        "AfterComponentUpload",
        "AfterManifestWrite",
        "BeforeBinding",
    ] {
        for crash in [false, true] {
            for owner in ["same-node", "replacement-node"] {
                harness::run(
                    "child",
                    owner,
                    phase,
                    crash,
                    &format!("p08-artifact-{}", uuid::Uuid::new_v4().simple()),
                );
            }
        }
    }
}

#[test]
#[ignore = "owned child fixture"]
fn child() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        tokio::time::timeout(harness::LIMIT, work())
            .await
            .expect("bounded artifact fixture");
    });
}

async fn work() {
    let tenant = harness::setting("P08_TENANT");
    let store = PgStore::connect(&harness::setting("MUNARIUM_TEST_DATABASE_URL"), &tenant)
        .await
        .unwrap_or_else(|_| panic!("fixture connection failed"));
    let pg = PgRetrieval::new(store.pool().clone(), &tenant);
    let mut ctx = MirrorContext {
        catalog: ArtifactCatalog::new(store.pool().clone(), &tenant),
        attempts: BuildAttempts::new(store.pool().clone(), &tenant),
        stores: Arc::new(LocalStoreFactory::new(harness::dir().join("artifacts"))),
        node_id: "original-node".into(),
        staging_root: harness::dir().join("staging"),
        artifact_prefix: "v1".into(),
        tenant_path_hash: "fictional-tenant".into(),
        faults: None,
        observer: None,
        vector_policy: VectorPolicy {
            approx_threshold: None,
        },
    };
    let mode = harness::setting("P08_MODE");
    if mode == "setup" {
        let collection = pg
            .ensure_collection("recovery", "para", 0, &[], None)
            .await
            .unwrap();
        let (source, _, _) = pg
            .put_source(
                "",
                "text/plain",
                "story.txt",
                Some("para"),
                b"The fictional library opens at noon.",
            )
            .await
            .unwrap();
        pg.bind_source(&collection.id, &source, None).await.unwrap();
        let old = pg
            .build_collection_index(&collection.id, 400, 1, true)
            .await
            .unwrap();
        let target = MirrorTarget::Collection {
            collection_id: &collection.id,
        };
        let old_artifact = match backfill_one(&ctx, &pg, target, &old.id).await.unwrap() {
            MirrorOutcome::Published { artifact_id, .. } => artifact_id,
            other => panic!("unexpected baseline {other:?}"),
        };
        ctx.catalog
            .bind_new(
                &old.id,
                BindingSlot::Serving,
                &old_artifact,
                "original-node",
                Some("test baseline"),
            )
            .await
            .unwrap();
        pg.put_source(
            "",
            "text/plain",
            "story.txt",
            Some("para"),
            b"The fictional library opens at dawn.",
        )
        .await
        .unwrap();
        let next = pg
            .build_collection_index(&collection.id, 400, 2, false)
            .await
            .unwrap();
        assert_ne!(old.id, next.id);
        std::fs::write(harness::dir().join("fixture"), json!({"collection":collection.id,"old":old.id,"old_artifact":old_artifact,"next":next.id}).to_string()).unwrap();
        return;
    }
    let data: Value =
        serde_json::from_slice(&std::fs::read(harness::dir().join("fixture")).unwrap()).unwrap();
    let collection = data["collection"].as_str().unwrap();
    let next = data["next"].as_str().unwrap();
    let target = MirrorTarget::Collection {
        collection_id: collection,
    };
    if mode == "write" {
        ctx.faults = Some(Arc::new(|phase: BuildPhase| {
            harness::barrier(&format!("{phase:?}"));
            Ok(())
        }));
        assert!(matches!(
            backfill_one(&ctx, &pg, target, next).await.unwrap(),
            MirrorOutcome::Published { .. }
        ));
        std::fs::write(harness::dir().join("reply"), b"published").unwrap();
        return;
    }
    old_pin(&ctx, &data).await;
    let phase = harness::setting("P08_PHASE");
    let interrupted = mode == "observe" || harness::setting("P08_CRASH") == "yes";
    let pre_catalog = matches!(phase.as_str(), "AfterClaim" | "AfterExport" | "AfterSeal");
    let artifacts = ctx.catalog.artifacts_for_version(next).await.unwrap();
    if interrupted {
        assert!(ctx
            .catalog
            .binding(next, BindingSlot::Staged)
            .await
            .unwrap()
            .is_none());
        assert_eq!(artifacts.len(), usize::from(!pre_catalog));
        if let Some(artifact) = artifacts.first() {
            assert_eq!(
                artifact.state,
                if phase == "BeforeBinding" {
                    ArtifactState::Verified
                } else {
                    ArtifactState::Sealed
                }
            );
            let files = ctx.stores.store_for_prefix(&artifact.artifact_uri).unwrap();
            let manifest_exists = files.get_component(MANIFEST, None).is_ok();
            assert_eq!(
                manifest_exists,
                matches!(phase.as_str(), "AfterManifestWrite" | "BeforeBinding")
            );
            assert_eq!(
                OpenShard::open(
                    files.as_ref(),
                    &artifact.artifact_id,
                    &ReaderCapabilities::v1(),
                    &Limits::default()
                )
                .is_ok(),
                manifest_exists
            );
        }
    }
    assert!(ctx
        .catalog
        .binding(next, BindingSlot::Serving)
        .await
        .unwrap()
        .is_none());
    if mode == "observe" {
        // A second process cannot take over a live lease or promote publication.
        ctx.node_id = "observer-node".into();
        let report = reconcile_attempts(&ctx).await.unwrap();
        assert_eq!(report.resumed, 0);
        assert_eq!(report.abandoned, 0);
        let result = backfill_one(&ctx, &pg, target, next).await.unwrap();
        assert!(matches!(
            result,
            MirrorOutcome::AlreadyRunning { .. } | MirrorOutcome::AlreadyBuilt { .. }
        ));
        return;
    }
    if interrupted {
        let replacement = harness::setting("P08_CASE") == "replacement-node";
        if replacement {
            ctx.node_id = "replacement-node".into();
        }
        // Make lease expiry deterministic after confirming the owner process died.
        // This changes only this run's interrupted attempts, never production time.
        if pre_catalog || replacement {
            sqlx::query("UPDATE index_build_attempts SET lease_expires_at=now()-interval '1 second' WHERE tenant_id=$1 AND index_version_id=$2")
                .bind(&tenant).bind(next).execute(store.pool()).await.unwrap();
        }
        let report = reconcile_attempts(&ctx).await.unwrap();
        if pre_catalog {
            assert_eq!(report.expired, 1);
            assert_eq!(report.resumed, 0);
            assert!(ctx
                .catalog
                .artifacts_for_version(next)
                .await
                .unwrap()
                .is_empty());
        } else if replacement {
            assert_eq!(
                report.abandoned, 1,
                "new node does not adopt another node's local staging"
            );
            assert_eq!(report.resumed, 0);
            assert!(ctx
                .catalog
                .binding(next, BindingSlot::Staged)
                .await
                .unwrap()
                .is_none());
        } else {
            assert_eq!(report.resumed, 1);
        }
        assert_eq!(
            reconcile_attempts(&ctx).await.unwrap(),
            Default::default(),
            "reconciliation is stable on a second pass"
        );
    }
    if let Some(binding) = ctx
        .catalog
        .binding(next, BindingSlot::Staged)
        .await
        .unwrap()
    {
        let artifact = ctx
            .catalog
            .artifact(next, &binding.artifact_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(artifact.state, ArtifactState::Verified);
        let files = ctx.stores.store_for_prefix(&artifact.artifact_uri).unwrap();
        let shard = OpenShard::open(
            files.as_ref(),
            &artifact.artifact_id,
            &ReaderCapabilities::v1(),
            &Limits::default(),
        )
        .unwrap();
        let mut expected = Vec::new();
        pg.export_collection_chunks(collection, next, |c| {
            expected.push(c);
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(shard.records().len(), expected.len());
        for claim in expected {
            let record = shard.record(&claim.chunk_id).unwrap();
            assert_eq!(record.text, claim.text);
        }
    } else {
        assert!(interrupted, "control must finish publication");
    }
    old_pin(&ctx, &data).await;
}

async fn old_pin(ctx: &MirrorContext, data: &Value) {
    let version = data["old"].as_str().unwrap();
    let binding = ctx
        .catalog
        .binding(version, BindingSlot::Serving)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(binding.artifact_id, data["old_artifact"].as_str().unwrap());
    let artifact = ctx
        .catalog
        .artifact(version, &binding.artifact_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(artifact.state, ArtifactState::Verified);
    let files = ctx.stores.store_for_prefix(&artifact.artifact_uri).unwrap();
    let shard = OpenShard::open(
        files.as_ref(),
        &artifact.artifact_id,
        &ReaderCapabilities::v1(),
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(shard.records().len(), 1);
    assert_eq!(
        shard.records()[0].text,
        "The fictional library opens at noon."
    );
}
