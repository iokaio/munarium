// SPDX-License-Identifier: Apache-2.0
//! The exact/approximate crossover measurement (§6.3: the threshold in the
//! build policy is BENCHMARK-DERIVED, and this is the benchmark).
//!
//! `#[ignore]`d like the query-latency baseline: a measurement run by hand,
//! whose numbers become
//! `DEFAULT_APPROX_THRESHOLD`. Run with:
//!
//! ```text
//! cargo test -p munarium-datastore --features vector-diskann --release \
//!     --test vector_crossover -- --ignored --nocapture
//! ```
//!
//! What it measures, per corpus size: flat exact scan vs graph traversal
//! wall-clock (the crossover), graph build time (the seal cost the threshold
//! buys), and recall@10 against the exact oracle (the quality gate at scale,
//! not just at the unit-test corpus size).
//!
//! The corpus is CLUSTERED synthetic data — points around a few hundred
//! centers — because that is the geometry real text embeddings have (low
//! intrinsic dimension, local structure), and it is the geometry a graph
//! index exploits. The first run of this bench used uniform random vectors
//! and measured recall COLLAPSING with corpus size (1.000 at 1k down to
//! 0.297 at 64k): on structureless data nearest-neighbor search is
//! information-theoretically hard and every ANN method degrades toward scan.
//! That number is kept as the adversarial row below rather than papered
//! over — it is the reason the per-corpus shadow comparison, not this
//! bench, is the rollout gate for any REAL corpus.
#![cfg(feature = "vector-diskann")]

use std::time::Instant;

use munarium_datastore::vector::{FlatVectorIndex, VectorIndex};
use munarium_datastore::vector_diskann::{DiskAnnVectorIndex, GraphParams};

fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E3779B97F4A7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D049BB133111EB);
    x ^ (x >> 31)
}

fn uniform(seed: u64, i: usize, dims: usize) -> Vec<f32> {
    (0..dims)
        .map(|d| {
            let h = splitmix(seed ^ ((i as u64) << 32 | d as u64));
            (h as f64 / u64::MAX as f64) as f32 * 2.0 - 1.0
        })
        .collect()
}

/// A corpus point: one of `centers` cluster centers plus small noise — the
/// standard ANN-benchmark geometry, and the one embeddings actually have.
fn clustered(i: usize, dims: usize, centers: usize) -> Vec<f32> {
    let center = uniform(0xC0FFEE, i % centers, dims);
    let noise = uniform(0xAB1E, i, dims);
    center
        .iter()
        .zip(&noise)
        .map(|(c, n)| c + 0.25 * n)
        .collect()
}

/// A query: near some cluster but NOT a corpus point.
fn query_near(q: usize, dims: usize, centers: usize) -> Vec<f32> {
    let center = uniform(0xC0FFEE, q % centers, dims);
    let noise = uniform(0x5EED, q, dims);
    center
        .iter()
        .zip(&noise)
        .map(|(c, n)| c + 0.25 * n)
        .collect()
}

fn p50(samples: &mut [f64]) -> f64 {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    samples[(samples.len() - 1) / 2]
}

#[test]
#[ignore = "a measurement, run by hand"]
fn measure_the_exact_approximate_crossover() {
    // 256 dimensions: what LocalHashEmbedder actually produces, so the
    // crossover is measured at the served dimensionality.
    const DIMS: usize = 256;
    const K: usize = 10;
    const QUERIES: usize = 30;
    const ROUNDS: usize = 5;

    println!("\n=== exact/approximate crossover: {DIMS} dims, top-{K}, {QUERIES} queries x {ROUNDS} rounds ===");
    println!(
        "{:>22}  {:>12}  {:>12}  {:>10}  {:>8}",
        "corpus", "flat p50 ms", "graph p50 ms", "build s", "recall"
    );

    // (label, count, centers). `centers: 0` = uniform random — the recorded
    // adversarial bound, not a served geometry.
    let cases: &[(&str, usize, usize)] = &[
        ("clustered 1k", 1_000, 50),
        ("clustered 4k", 4_000, 100),
        ("clustered 16k", 16_000, 200),
        ("clustered 64k", 64_000, 400),
        ("uniform 64k (adversarial)", 64_000, 0),
    ];
    for &(label, n, centers) in cases {
        let entries: Vec<(String, Vec<f32>)> = (0..n)
            .map(|i| {
                let v = if centers == 0 {
                    uniform(0, i, DIMS)
                } else {
                    clustered(i, DIMS, centers)
                };
                (format!("c{i:06}"), v)
            })
            .collect();

        let mut flat = FlatVectorIndex::new(DIMS);
        for (id, v) in &entries {
            flat.push(id.clone(), v).unwrap();
        }

        let t = Instant::now();
        let graph = DiskAnnVectorIndex::build(DIMS, &entries, GraphParams::default()).unwrap();
        let build_s = t.elapsed().as_secs_f64();

        let queries: Vec<Vec<f32>> = (0..QUERIES)
            .map(|q| {
                if centers == 0 {
                    uniform(0, 10_000_000 + q, DIMS)
                } else {
                    query_near(7_777 + q, DIMS, centers)
                }
            })
            .collect();

        let mut flat_ms = Vec::new();
        let mut graph_ms = Vec::new();
        let mut hits = 0usize;
        for _ in 0..ROUNDS {
            for q in &queries {
                let t = Instant::now();
                let exact = flat.vector_candidates(q, K).unwrap();
                flat_ms.push(t.elapsed().as_secs_f64() * 1000.0);

                let t = Instant::now();
                let approx = graph.vector_candidates(q, K).unwrap();
                graph_ms.push(t.elapsed().as_secs_f64() * 1000.0);

                let kth = exact.last().unwrap().score + 1e-6;
                hits += approx.iter().filter(|c| c.score <= kth).count();
            }
        }
        let recall = hits as f64 / (QUERIES * ROUNDS * K) as f64;
        println!(
            "{label:>22}  {:>12.3}  {:>12.3}  {build_s:>10.1}  {recall:>8.3}",
            p50(&mut flat_ms),
            p50(&mut graph_ms),
        );
    }
    println!("\ncrossover = smallest corpus where the graph column beats the flat column;");
    println!("DEFAULT_APPROX_THRESHOLD in munarium-retrieval/src/mirror.rs records it.");
}
