// SPDX-License-Identifier: Apache-2.0
//! The stage 0 benchmark baseline (§15.6): the PostgreSQL reference engine
//! and the datastore engine, measured side by side over one corpus and one
//! set of prepared queries.
//!
//! `#[ignore]`d: this is a MEASUREMENT, not a gate — it exists to be run by
//! hand and have its numbers recorded,
//! the way every measured number in this program lands in a committed
//! document. Run it with:
//!
//! ```text
//! cargo test -p munarium-retrieval --test benchmark_baseline --release -- --ignored --nocapture
//! ```
//!
//! What it deliberately is NOT: a load test (§16 stage 5's REST/gRPC
//! duplication measurements need a deployment), and not a quality benchmark
//! (that is the shadow comparison's job). It answers one stage 0 question —
//! is the datastore engine's query latency in the right neighbourhood to be
//! worth the rollout machinery — with wall-clock numbers over identical
//! prepared queries.

use std::sync::Arc;
use std::time::Instant;

use munarium_datastore::hydrate::{CacheBudget, L1Cache, Residency};
use munarium_datastore::verify::{Limits, ReaderCapabilities};
use munarium_retrieval::executor::{ArtifactExecutor, ExecutionOutcome, TextPayload};
use munarium_retrieval::mirror::{LocalStoreFactory, MirrorContext, MirrorOutcome, MirrorTarget};
use munarium_retrieval_pg::PgRetrieval;
use munarium_store_pg::artifacts::{ArtifactCatalog, BindingSlot};
use munarium_store_pg::attempts::BuildAttempts;
use munarium_store_pg::PgStore;

fn url() -> Option<String> {
    std::env::var("MUNARIUM_TEST_DATABASE_URL").ok()
}

/// Percentiles over raw sample vectors — no distribution assumptions.
fn percentile(samples: &mut [f64], p: f64) -> f64 {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let idx = ((samples.len() as f64 - 1.0) * p).round() as usize;
    samples[idx]
}

#[tokio::test]
#[ignore = "a measurement, run by hand"]
async fn measure_postgres_versus_datastore_query_latency() {
    let Some(url) = url() else {
        eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
        return;
    };
    let tenant = format!("tenant-bench-{}", uuid::Uuid::new_v4().simple());
    let store = PgStore::connect(&url, &tenant).await.unwrap();
    let pg = PgRetrieval::new(store.pool().clone(), &tenant);

    // A corpus big enough that both engines do real work: 400 documents,
    // ~8 paragraphs each, overlapping vocabulary so queries have real
    // candidate pools.
    const DOCS: usize = 400;
    let col = pg
        .ensure_collection("bench", "para", 0, &[], Some("benchmark baseline"))
        .await
        .unwrap();
    let subjects = [
        "the continental congress debated supply and provisions",
        "washington wrote from the encampment about muskets and powder",
        "colonial newspapers reported the destruction of the tea",
        "the treaty of alliance with france established mutual defence",
        "privateers sailed under letters of marque from salem harbour",
        "the quartermaster recorded 4,436,097 cartridges and 211.100 barrels",
        "smallpox inoculation divided the medical men of boston",
        "the committee of correspondence circulated resolves to every county",
    ];
    for i in 0..DOCS {
        let mut body = format!("# Benchmark document {i}\n\n");
        for (j, s) in subjects.iter().enumerate() {
            body.push_str(&format!(
                "Paragraph {j} of document {i}: {s}, as recorded in ledger {i}-{j}.\n\n"
            ));
        }
        let (source_id, _, _) = pg
            .put_source(
                "",
                "text/markdown",
                &format!("bench/doc-{i:04}.md"),
                Some("para"),
                body.as_bytes(),
            )
            .await
            .unwrap();
        pg.bind_source(&col.id, &source_id, None).await.unwrap();
    }
    let version = pg
        .build_collection_index(&col.id, 400, 1, true)
        .await
        .unwrap();

    // Mirror it, promote to serving, open once (warm) before measuring.
    let artifacts = tempfile::tempdir().unwrap();
    let staging = tempfile::tempdir().unwrap();
    let cache_dir = tempfile::tempdir().unwrap();
    let ctx = MirrorContext {
        catalog: ArtifactCatalog::new(store.pool().clone(), &tenant),
        attempts: BuildAttempts::new(store.pool().clone(), &tenant),
        stores: Arc::new(LocalStoreFactory::new(artifacts.path())),
        node_id: "bench".into(),
        staging_root: staging.path().to_path_buf(),
        artifact_prefix: "v1".into(),
        tenant_path_hash: "t0000".into(),
        faults: None,
        observer: None,
        vector_policy: munarium_retrieval::mirror::VectorPolicy {
            approx_threshold: None,
        },
    };
    let build_started = Instant::now();
    let _artifact_id = match munarium_retrieval::backfill::backfill_one(
        &ctx,
        &pg,
        MirrorTarget::Collection {
            collection_id: &col.id,
        },
        &version.id,
    )
    .await
    .unwrap()
    {
        MirrorOutcome::Published {
            artifact_id,
            chunks,
            ..
        } => {
            println!(
                "mirror build: {chunks} chunks in {:.1}s",
                build_started.elapsed().as_secs_f64()
            );
            artifact_id
        }
        other => panic!("expected a publication, got {other:?}"),
    };
    let staged = ctx
        .catalog
        .binding(&version.id, BindingSlot::Staged)
        .await
        .unwrap()
        .unwrap();
    ctx.catalog
        .promote_staged(&version.id, staged.generation, 0, "bench", None)
        .await
        .unwrap();

    let executor = ArtifactExecutor {
        catalog: ctx.catalog.clone(),
        stores: ctx.stores.clone(),
        l0: Arc::new(munarium_retrieval::executor::L0Cache::new(8)),
        cache: Arc::new(
            L1Cache::new(
                cache_dir.path(),
                CacheBudget::new(2 << 30, 1 << 30).unwrap(),
            )
            .unwrap(),
        ),
        reader: ReaderCapabilities::v1(),
        limits: Limits::default(),
        isolation_domain: "t0000".into(),
    };

    let queries = [
        "what did the quartermaster record about cartridges",
        "how did colonial newspapers report the tea",
        "letters of marque from salem",
        "the treaty of alliance with france",
        "supply and provisions for the encampment",
        "smallpox inoculation in boston",
        "resolves circulated to the counties",
        "muskets and powder at the encampment",
        "barrels recorded in the ledger",
        "mutual defence established by treaty",
    ];
    let prepared: Vec<_> = queries
        .iter()
        .map(|q| {
            Arc::new(PgRetrieval::prepare_query(
                q,
                &munarium_core::retrieval::SearchParams::default(),
                &munarium_retrieval_pg::LocalHashEmbedder,
            ))
        })
        .collect();

    // Warm both sides once — the cold artifact open is reported separately,
    // because it is a different number answering a different question.
    let cold_started = Instant::now();
    match executor
        .execute(
            &version.id,
            BindingSlot::Serving,
            Residency::ServingRequired,
            TextPayload::Served,
            &prepared[0],
        )
        .await
    {
        ExecutionOutcome::Executed(_) => {}
        other => panic!("warmup failed: {other:?}"),
    }
    let cold_open_ms = cold_started.elapsed().as_secs_f64() * 1000.0;
    pg.search_collection_prepared(&col.id, &prepared[0], None)
        .await
        .unwrap();

    const ROUNDS: usize = 10;
    let mut pg_ms: Vec<f64> = Vec::new();
    let mut ds_ms: Vec<f64> = Vec::new();
    for _ in 0..ROUNDS {
        for p in &prepared {
            let t = Instant::now();
            let r = pg
                .search_collection_prepared(&col.id, p, None)
                .await
                .unwrap();
            pg_ms.push(t.elapsed().as_secs_f64() * 1000.0);
            assert!(!r.hits.is_empty());

            let t = Instant::now();
            match executor
                .execute(
                    &version.id,
                    BindingSlot::Serving,
                    Residency::ServingRequired,
                    TextPayload::Served,
                    p,
                )
                .await
            {
                ExecutionOutcome::Executed(e) => {
                    ds_ms.push(t.elapsed().as_secs_f64() * 1000.0);
                    assert!(!e.hits.is_empty());
                }
                other => panic!("datastore execution failed: {other:?}"),
            }
        }
    }

    println!(
        "\n=== benchmark baseline: {DOCS} docs, {} queries x {ROUNDS} rounds ===",
        queries.len()
    );
    println!("cold artifact hydrate+open+first-query: {cold_open_ms:.1} ms");
    for (name, samples) in [("postgres", &mut pg_ms), ("datastore", &mut ds_ms)] {
        let p50 = percentile(samples, 0.50);
        let p95 = percentile(samples, 0.95);
        let p99 = percentile(samples, 0.99);
        println!("{name:9}  p50 {p50:7.2} ms   p95 {p95:7.2} ms   p99 {p99:7.2} ms");
    }
}
