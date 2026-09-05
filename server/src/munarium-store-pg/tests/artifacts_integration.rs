// SPDX-License-Identifier: Apache-2.0
//! The artifact catalog against a real PostgreSQL.
//!
//! Gated on `MUNARIUM_TEST_DATABASE_URL`, like the other integration suites
//! here. **Skips loudly rather than silently** when it is unset: a tier that
//! returns early and prints `ok` is indistinguishable from one that proved
//! something, and this repository has already paid once for exactly that.

use munarium_store_pg::artifacts::{
    ArtifactCatalog, ArtifactState, BindingSlot, InsertOutcome, NewArtifact,
};
use munarium_store_pg::PgStore;

fn url() -> Option<String> {
    std::env::var("MUNARIUM_TEST_DATABASE_URL").ok()
}

/// Every test guards on `url()` first and returns early with a loud message,
/// so this may assume the variable is set.
async fn catalog(tenant: &str) -> ArtifactCatalog {
    let url = url().expect("guarded by the caller");
    let store = PgStore::connect(&url, tenant).await.unwrap();
    ArtifactCatalog::new(store.pool().clone(), tenant)
}

fn artifact(version: &str, id: &str) -> NewArtifact {
    NewArtifact {
        index_version_id: version.into(),
        artifact_id: id.into(),
        engine_id: "tantivy".into(),
        format_version: 1,
        artifact_uri: format!("az://indexes/{version}/{id}"),
        artifact_plan: serde_json::json!({"plan_version": 1}),
        artifact_plan_sha256: "p".repeat(64),
        artifact_manifest: serde_json::json!({"manifest_version": 1}),
        bytes_len: 1024,
        file_count: 6,
        built_by: Some("test".into()),
        attempt_id: Some("att-1".into()),
    }
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

#[tokio::test]
async fn a_sealed_artifact_catalogs_and_reads_back() {
    let Some(_) = url() else {
        eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
        return;
    };
    let cat = catalog("tenant-artifacts-a").await;
    let v = unique("idx2");
    let a = artifact(&v, "aaa");

    assert_eq!(
        cat.insert_sealed(&a).await.unwrap(),
        InsertOutcome::Inserted
    );
    let got = cat.artifact(&v, "aaa").await.unwrap().expect("row");
    assert_eq!(got.state, ArtifactState::Sealed);
    assert_eq!(got.engine_id, "tantivy");
    assert_eq!(got.artifact_uri, a.artifact_uri);
}

/// §7.1 step 7. A rebuild that produces a byte-identical artifact has
/// CONVERGED, and the catalog must say so rather than erroring or
/// double-publishing. Tantivy is not byte-deterministic today, so this fires on
/// a re-publication of the same artifact rather than on a fresh build — which
/// is the same situation from the catalog's point of view.
#[tokio::test]
async fn a_duplicate_insert_converges_instead_of_failing() {
    let Some(_) = url() else {
        eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
        return;
    };
    let cat = catalog("tenant-artifacts-b").await;
    let v = unique("idx2");
    let a = artifact(&v, "bbb");

    assert_eq!(
        cat.insert_sealed(&a).await.unwrap(),
        InsertOutcome::Inserted
    );

    // Still sealed: the second builder should ADOPT and finish publication,
    // because nobody has verified the artifact yet.
    match cat.insert_sealed(&a).await.unwrap() {
        InsertOutcome::Adopted { existing_state } => {
            assert_eq!(existing_state, ArtifactState::Sealed)
        }
        other => panic!("expected Adopted while sealed, got {other:?}"),
    }

    // Once verified, the second builder should CONVERGE and discard its output.
    cat.mark_verified(&v, "bbb", "test").await.unwrap();
    match cat.insert_sealed(&a).await.unwrap() {
        InsertOutcome::Converged { existing_state } => {
            assert_eq!(existing_state, ArtifactState::Verified)
        }
        other => panic!("expected Converged once verified, got {other:?}"),
    }
}

/// A catalog row must never advertise a usable artifact before verification.
#[tokio::test]
async fn only_a_sealed_artifact_may_be_verified() {
    let Some(_) = url() else {
        eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
        return;
    };
    let cat = catalog("tenant-artifacts-c").await;
    let v = unique("idx2");
    cat.insert_sealed(&artifact(&v, "ccc")).await.unwrap();

    cat.mark_verified(&v, "ccc", "test").await.unwrap();
    // Twice is refused: the second call would be a state machine going
    // backwards, and silently succeeding would hide a double-publication.
    assert!(cat.mark_verified(&v, "ccc", "test").await.is_err());
}

/// A binding asserts that an artifact is servable, so an unverified one cannot
/// be bound. Without this, a `staged` binding could point at an artifact whose
/// bytes were never checked.
#[tokio::test]
async fn an_unverified_artifact_cannot_be_bound() {
    let Some(_) = url() else {
        eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
        return;
    };
    let cat = catalog("tenant-artifacts-d").await;
    let v = unique("idx2");
    cat.insert_sealed(&artifact(&v, "ddd")).await.unwrap();

    let err = cat
        .bind_new(&v, BindingSlot::Staged, "ddd", "test", None)
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("not verified"), "{err}");

    cat.mark_verified(&v, "ddd", "test").await.unwrap();
    let b = cat
        .bind_new(&v, BindingSlot::Staged, "ddd", "test", Some("first"))
        .await
        .unwrap();
    assert_eq!(b.generation, 1, "the first binding starts at generation 1");
}

/// Filling an occupied slot is refused: replacing a binding is a promotion,
/// which carries a different safety argument (a compare-and-swap against the
/// generation the caller read).
#[tokio::test]
async fn binding_an_occupied_slot_is_refused() {
    let Some(_) = url() else {
        eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
        return;
    };
    let cat = catalog("tenant-artifacts-e").await;
    let v = unique("idx2");
    for id in ["e1", "e2"] {
        cat.insert_sealed(&artifact(&v, id)).await.unwrap();
        cat.mark_verified(&v, id, "test").await.unwrap();
    }
    cat.bind_new(&v, BindingSlot::Serving, "e1", "test", None)
        .await
        .unwrap();
    let err = cat
        .bind_new(&v, BindingSlot::Serving, "e2", "test", None)
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("already occupied"), "{err}");
}

/// Every binding change appends history, in the same transaction. A binding
/// with no event is a promotion nobody can explain afterwards.
#[tokio::test]
async fn a_binding_appends_history_atomically() {
    let Some(_) = url() else {
        eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
        return;
    };
    let cat = catalog("tenant-artifacts-f").await;
    let v = unique("idx2");
    cat.insert_sealed(&artifact(&v, "fff")).await.unwrap();
    cat.mark_verified(&v, "fff", "test").await.unwrap();
    cat.bind_new(&v, BindingSlot::Shadow, "fff", "operator", Some("why"))
        .await
        .unwrap();

    let events = cat.binding_events(&v).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0], ("bind".to_string(), "fff".to_string()));
}

/// The isolation property. Two tenants may hold the SAME artifact id — the
/// same corpus legitimately hashes the same — and neither may see the other's
/// row. `artifact_id` is content, never authority.
#[tokio::test]
async fn identical_content_in_two_tenants_stays_separate() {
    let Some(_) = url() else {
        eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
        return;
    };
    let v = unique("idx2");
    let a = catalog("tenant-iso-a").await;
    let b = catalog("tenant-iso-b").await;

    a.insert_sealed(&artifact(&v, "same-hash")).await.unwrap();
    // Tenant B inserting the identical id is an INSERT, not a conflict: the
    // primary key carries the tenant.
    assert_eq!(
        b.insert_sealed(&artifact(&v, "same-hash")).await.unwrap(),
        InsertOutcome::Inserted
    );

    a.mark_verified(&v, "same-hash", "a").await.unwrap();
    // B's row is untouched by A's verification.
    assert_eq!(
        b.artifact(&v, "same-hash").await.unwrap().unwrap().state,
        ArtifactState::Sealed
    );
    assert_eq!(
        a.artifact(&v, "same-hash").await.unwrap().unwrap().state,
        ArtifactState::Verified
    );
}

// --- rollout selector and plane expectations --------------------------------

use munarium_store_pg::rollout::{PlaneExpectations, RolloutChange, RolloutSelector};

fn change<'a>(serving: &'a str, by: &'a str, reason: Option<&'a str>) -> RolloutChange<'a> {
    RolloutChange {
        serving,
        prewarm_staged: false,
        changed_by: by,
        reason,
    }
}

async fn selector(tenant: &str) -> RolloutSelector {
    let url = url().expect("guarded by the caller");
    let store = PgStore::connect(&url, tenant).await.unwrap();
    RolloutSelector::new(store.pool().clone(), tenant)
}

/// An absent row and an explicit `postgres` row mean the same thing. A
/// selector that failed open onto the unproven engine would be the wrong
/// convenience, so "no row" must read as PostgreSQL.
#[tokio::test]
async fn an_unknown_scope_has_no_selector_row_and_that_means_postgres() {
    let Some(_) = url() else {
        eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
        return;
    };
    let s = selector("tenant-rollout-a").await;
    assert!(s
        .get("collection", &unique("never"))
        .await
        .unwrap()
        .is_none());
}

/// Changing a selector is a compare-and-swap. A stale generation loses rather
/// than overwriting, and loses as `Ok(None)` rather than as an error: a
/// concurrent change is an ordinary outcome to re-read and retry.
#[tokio::test]
async fn a_stale_generation_loses_the_compare_and_swap() {
    let Some(_) = url() else {
        eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
        return;
    };
    let s = selector("tenant-rollout-b").await;
    let scope = unique("col");

    let first = s
        .create(
            "collection",
            &scope,
            change("postgres", "op", Some("initial")),
        )
        .await
        .unwrap();
    assert_eq!(first.generation, 1);

    // Two readers hold generation 1. The first write wins.
    let won = s
        .update("collection", &scope, change("datastore", "op", None), 1)
        .await
        .unwrap()
        .expect("the first CAS wins");
    assert_eq!(won.generation, 2);
    assert_eq!(won.serving, "datastore");

    // The second, still holding 1, loses -- without an error.
    assert!(s
        .update("collection", &scope, change("postgres", "op", None), 1)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        s.get("collection", &scope).await.unwrap().unwrap().serving,
        "datastore",
        "the losing write must not have taken effect"
    );
}

#[tokio::test]
async fn creating_an_existing_selector_row_is_refused() {
    let Some(_) = url() else {
        eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
        return;
    };
    let s = selector("tenant-rollout-c").await;
    let scope = unique("col");
    s.create("collection", &scope, change("postgres", "op", None))
        .await
        .unwrap();
    let err = s
        .create("collection", &scope, change("datastore", "op", None))
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("compare-and-swap"), "{err}");
}

#[tokio::test]
async fn an_invalid_serving_value_is_refused() {
    let Some(_) = url() else {
        eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
        return;
    };
    let s = selector("tenant-rollout-d").await;
    assert!(s
        .create(
            "collection",
            &unique("col"),
            change("elasticsearch", "op", None)
        )
        .await
        .is_err());
}

/// The database refuses a gate that always passes, so no code path can write
/// one. Asserted here as well as in the schema, because the constraint is the
/// reason this table exists.
#[tokio::test]
async fn a_zero_node_expectation_is_refused_by_the_database() {
    let Some(_) = url() else {
        eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
        return;
    };
    let db = url().unwrap();
    let store = PgStore::connect(&db, "tenant-exp").await.unwrap();
    let env = unique("env");
    let e = PlaneExpectations::new(store.pool().clone(), &env);

    assert!(
        e.record(
            "rest",
            "rev-1",
            0,
            0,
            None,
            "datastore",
            Some(1),
            "op",
            None
        )
        .await
        .is_err(),
        "a zero-node expectation is a cutover with no gate"
    );
    assert!(
        e.record(
            "rest",
            "rev-1",
            1,
            1,
            Some(1.5),
            "datastore",
            Some(1),
            "op",
            None
        )
        .await
        .is_err(),
        "a fraction above 1 cannot be satisfied"
    );

    let ok = e
        .record(
            "rest",
            "rev-1",
            2,
            2,
            Some(0.5),
            "datastore",
            Some(2),
            "op",
            Some("canary"),
        )
        .await
        .unwrap();
    assert_eq!(ok.minimum_fresh_nodes, 2);
    assert_eq!(ok.generation, 1);

    // Re-recording bumps the generation rather than silently replacing history.
    let again = e
        .record(
            "rest",
            "rev-1",
            3,
            3,
            None,
            "datastore",
            Some(3),
            "op",
            Some("scaled up"),
        )
        .await
        .unwrap();
    assert_eq!(again.generation, 2);
    assert_eq!(again.minimum_fresh_nodes, 3);
}
