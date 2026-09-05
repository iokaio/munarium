// SPDX-License-Identifier: Apache-2.0
//! `cargo test -p mmp-conformance` wrapper: every scenario is its own
//! assertion against the in-memory backend.

use munarium_store_mem::MemStore;

#[tokio::test]
async fn all_scenarios_pass_in_process() {
    let store = MemStore::new();
    let results = mmp_conformance::run_all(&store).await;
    let failures: Vec<String> = results
        .iter()
        .filter_map(|(name, r)| r.as_ref().err().map(|e| format!("{name}: {e}")))
        .collect();
    assert!(
        failures.is_empty(),
        "conformance failures:\n{}",
        failures.join("\n")
    );
}
