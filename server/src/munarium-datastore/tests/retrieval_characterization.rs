// SPDX-License-Identifier: Apache-2.0
//! Engine characterization over already selected collection/generation rows.
//! This is not a record-level ACL implementation or authorization test.
use munarium_datastore::vector::{FlatVectorIndex, VectorIndex};

fn corpus() -> Vec<(String, Vec<f32>)> {
    (0..1024)
        .map(|i| {
            let vector = (0..8)
                .map(|d| (((i * 127 + d * 71 + i * d * 13) % 997) as f32 - 498.0) / 499.0)
                .collect();
            (format!("chunk-{i:04}"), vector)
        })
        .collect()
}

// Independent f64 normalized-dot implementation: never invokes an engine's
// distance routine, candidate selection, or fusion.
fn oracle(rows: &[(String, Vec<f32>)], query: &[f32], k: usize) -> Vec<String> {
    let norm = |v: &[f32]| v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
    let qnorm = norm(query);
    let mut scores: Vec<_> = rows
        .iter()
        .map(|(id, v)| {
            let denom = norm(v) * qnorm;
            let score = if denom == 0.0 {
                1.0
            } else {
                1.0 - v
                    .iter()
                    .zip(query)
                    .map(|(x, y)| f64::from(*x) * f64::from(*y))
                    .sum::<f64>()
                    / denom
            };
            (id.clone(), score)
        })
        .collect();
    scores.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    scores.into_iter().take(k).map(|(id, _)| id).collect()
}

#[test]
fn exact_sparse_generation_matches_independent_oracle_and_reports_full_scan() {
    let all = corpus();
    for stride in [1, 8, 64, 1024] {
        // Keep the last row, so insertion position cannot masquerade as rank.
        let rows: Vec<_> = all
            .iter()
            .enumerate()
            .filter(|(i, _)| (i + 1) % stride == 0)
            .map(|(_, row)| row.clone())
            .collect();
        let mut flat = FlatVectorIndex::new(8);
        for (id, v) in &rows {
            flat.push(id, v).unwrap();
        }
        let reopened = FlatVectorIndex::from_bytes(&flat.to_bytes().unwrap()).unwrap();
        for query in [&all[1023].1[..], &[0.0; 8][..]] {
            for k in [0, 1, 10, rows.len(), rows.len() + 1] {
                let batch = reopened.vector_candidates_diagnosed(query, k).unwrap();
                assert_eq!(
                    batch
                        .candidates
                        .iter()
                        .map(|c| c.chunk_id.clone())
                        .collect::<Vec<_>>(),
                    oracle(&rows, query, k)
                );
                assert_eq!(batch.diagnostics.visited, Some(rows.len()));
                assert_eq!(batch.diagnostics.work_limit, Some(rows.len()));
                assert_eq!(batch.diagnostics.distance_computations, Some(rows.len()));
                assert_eq!(batch.diagnostics.accepted, k.min(rows.len()));
                assert_eq!(batch.diagnostics.refill_count, 0);
                assert_eq!(batch.diagnostics.exhausted, Some(true));
            }
        }
    }
}

#[cfg(feature = "vector-diskann")]
#[test]
fn ann_sparse_generation_records_recall_and_actual_distance_work() {
    use munarium_datastore::vector_diskann::{DiskAnnVectorIndex, GraphParams};
    let all = corpus();
    for stride in [1, 8, 64] {
        let rows: Vec<_> = all
            .iter()
            .enumerate()
            .filter(|(i, _)| (i + 1) % stride == 0)
            .map(|(_, row)| row.clone())
            .collect();
        for search_list in [10, 100] {
            let graph = DiskAnnVectorIndex::build(
                8,
                &rows,
                GraphParams {
                    l_search: search_list,
                    ..Default::default()
                },
            )
            .unwrap();
            let graph = DiskAnnVectorIndex::from_bytes(&graph.to_bytes().unwrap()).unwrap();
            let mut overlap = 0;
            let mut computations = 0;
            for index in [17, 399, 777, 1023] {
                let query = &all[index].1;
                let exact = oracle(&rows, query, 10);
                let batch = graph.vector_candidates_diagnosed(query, 10).unwrap();
                let ids: std::collections::HashSet<_> =
                    batch.candidates.iter().map(|c| &c.chunk_id).collect();
                assert_eq!(ids.len(), batch.candidates.len());
                assert!(ids.iter().all(|id| rows.iter().any(|r| &r.0 == *id)));
                assert!(ids.len() <= 10);
                overlap += exact.iter().filter(|id| ids.contains(id)).count();
                computations += batch.diagnostics.distance_computations.unwrap();
                assert_eq!(batch.diagnostics.search_list, Some(search_list as usize));
                assert_eq!(batch.diagnostics.work_limit, None);
                assert_eq!(batch.diagnostics.visited, None);
                assert_eq!(batch.diagnostics.exhausted, None);
                assert_eq!(batch.diagnostics.refill_count, 0);
            }
            println!("eligible={}/1024 k=10 search_list={search_list} recall={} distance_computations={computations} hard_work_limit=unavailable", rows.len(), overlap as f64 / 40.0);
            // Detect a broken fixture/engine, not a universal recall guarantee.
            assert!(overlap > 0);
        }
    }
}
