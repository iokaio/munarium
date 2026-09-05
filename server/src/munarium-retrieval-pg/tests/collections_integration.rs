// SPDX-License-Identifier: Apache-2.0
//! Collection-store integration tests. Skip (pass vacuously) when
//! MUNARIUM_TEST_DATABASE_URL is unset — same contract as the pg-store tests.
//! Connecting exercises migrations 0010–0011 on the shared database.

use munarium_retrieval_pg::{
    merge_hits, select_collection_indices, CollectionSearchResult, ContentDemotionRule,
    PgRetrieval, QueryExpansionRule, SearchParams,
};
use munarium_store_pg::PgStore;

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

async fn retrieval_for(url: &str, tenant: &str) -> PgRetrieval {
    let store = PgStore::connect(url, tenant).await.expect("connect");
    PgRetrieval::new(store.pool().clone(), tenant)
}

/// Two collections, disjoint sources: each gets its own partition, its own
/// index version, and searches never leak across the partition boundary.
#[tokio::test]
async fn collections_isolate_and_merge() {
    let Some(url) = test_url() else { return };
    let tenant = fresh_tenant("col");
    let r = retrieval_for(&url, &tenant).await;

    let public = r
        .ensure_collection("public-docs", "shape-a@1", 0, &[], Some("open"))
        .await
        .expect("public");
    let secret = r
        .ensure_collection("internal-eng", "shape-a@1", 2, &["eng".to_string()], None)
        .await
        .expect("secret");
    assert_ne!(public.id, secret.id);
    assert_eq!(secret.access_level, 2);
    assert_eq!(secret.compartments, vec!["eng".to_string()]);

    // Partitions exist, named from the collection ids.
    let pool = r.pool();
    let parts: Vec<String> = sqlx::query_scalar(
        "SELECT c.relname::text FROM pg_inherits i
           JOIN pg_class c ON c.oid = i.inhrelid
           JOIN pg_class p ON p.oid = i.inhparent
          WHERE p.relname = 'collection_chunks'",
    )
    .fetch_all(pool)
    .await
    .expect("partitions");
    for info in [&public, &secret] {
        let expect = format!(
            "collection_chunks_p_{}",
            info.id.trim_start_matches("col-").replace('-', "_")
        );
        assert!(parts.contains(&expect), "missing partition {expect}");
    }

    let (s1, _h1, _) = r
        .put_source(
            "",
            "text/plain",
            "pub.txt",
            None,
            b"the public handbook covers vacation policy",
        )
        .await
        .expect("s1");
    let (s2, _h2, _) = r
        .put_source(
            "",
            "text/plain",
            "eng.txt",
            None,
            b"the secret engineering roadmap covers vacation blackouts",
        )
        .await
        .expect("s2");
    assert!(r
        .bind_source(&public.id, &s1, Some("tester"))
        .await
        .expect("bind1"));
    assert!(r
        .bind_source(&secret.id, &s2, Some("tester"))
        .await
        .expect("bind2"));
    // rebind is a no-op, unknown source is not-found
    assert!(!r.bind_source(&public.id, &s1, None).await.expect("rebind"));
    assert!(r.bind_source(&public.id, "feedbeef", None).await.is_err());

    let iv_pub = r
        .build_collection_index(&public.id, 2000, 1, true)
        .await
        .expect("build pub");
    let iv_sec = r
        .build_collection_index(&secret.id, 2000, 1, true)
        .await
        .expect("build sec");
    assert_ne!(iv_pub.id, iv_sec.id, "identity includes collection id");
    assert!(iv_pub.active && iv_sec.active);

    let params = SearchParams::default();
    let pub_hits = r
        .search_collection(&public.id, "vacation", params.clone(), None)
        .await
        .expect("search pub");
    assert!(!pub_hits.hits.is_empty());
    assert!(
        pub_hits.hits.iter().all(|h| h.source_id == s1),
        "public search leaked another collection's chunks"
    );

    let sec_hits = r
        .search_collection(&secret.id, "vacation", params.clone(), None)
        .await
        .expect("search sec");
    assert!(sec_hits.hits.iter().all(|h| h.source_id == s2));
    // provenance names the document, not just its bytes
    assert_eq!(sec_hits.envelope.source_paths, vec!["eng.txt".to_string()]);

    // multi_search + merge: both collections contribute, order by score.
    let results = r
        .multi_search(&[public.clone(), secret.clone()], "vacation", params)
        .await
        .expect("multi");
    assert_eq!(results.len(), 2);
    let merged = merge_hits(&results, 10, 60.0);
    let names: std::collections::HashSet<&str> = merged.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains("public-docs") && names.contains("internal-eng"));
    assert!(merged.windows(2).all(|w| w[0].1.score >= w[1].1.score));
}

/// Candidate selection and reranking policy comes from the caller (normally
/// a runbook). A metadata record that repeats the question vocabulary remains
/// searchable, but a substantive itinerary can outrank it once that policy is
/// applied before the lexical LIMIT and after vector candidate selection.
#[tokio::test]
async fn declarative_policy_promotes_substantive_itinerary_over_metadata() {
    let Some(url) = test_url() else { return };
    let tenant = fresh_tenant("policy");
    let r = retrieval_for(&url, &tenant).await;
    let col = r
        .ensure_collection("archive", "shape@1", 0, &[], None)
        .await
        .expect("collection");

    let (metadata, _, _) = r
        .put_source(
            "",
            "text/markdown",
            "catalog.md",
            None,
            b"# George Washington cities visit\n\nGeorge Washington visited cities.\n\n**Text:** none (metadata record)",
        )
        .await
        .expect("metadata source");
    let (itinerary, _, _) = r
        .put_source(
            "",
            "text/markdown",
            "itinerary.md",
            None,
            b"# George Washington's southern tour\n\nThe journey left Mount Vernon, reached Fredericksburg, and arrived at Richmond before lodging there.",
        )
        .await
        .expect("itinerary source");
    let (generic_travel, _, _) = r
        .put_source(
            "",
            "text/markdown",
            "generic-travel.md",
            None,
            b"# A travel handbook\n\nJourney tour itinerary route. Travelers arrived, reached, departed, left, stayed, and lodged in cities, towns, villages, and places.",
        )
        .await
        .expect("generic travel source");
    r.bind_source(&col.id, &metadata, None)
        .await
        .expect("bind metadata");
    r.bind_source(&col.id, &itinerary, None)
        .await
        .expect("bind itinerary");
    r.bind_source(&col.id, &generic_travel, None)
        .await
        .expect("bind generic travel");
    r.build_collection_index(&col.id, 2000, 1, true)
        .await
        .expect("build");

    let result = r
        .search_collection(
            &col.id,
            "What cities did George Washington visit?",
            SearchParams {
                top_k: 1,
                candidate_n: 50,
                query_expansion_weight: 0.2,
                query_expansions: vec![
                    QueryExpansionRule {
                        when_any: vec!["visit".into()],
                        add_terms: vec![
                            "journey".into(),
                            "tour".into(),
                            "reached".into(),
                            "arrived".into(),
                            "left".into(),
                            "lodging".into(),
                        ],
                    },
                    QueryExpansionRule {
                        when_any: vec!["cities".into()],
                        add_terms: vec!["town".into(), "place".into()],
                    },
                ],
                content_demotions: vec![ContentDemotionRule {
                    contains: "**Text:** none (metadata record)".into(),
                    lexical_multiplier: 0.05,
                    vector_distance_penalty: 0.75,
                    // The tsvector phrase form — the marker's words in
                    // sequence — must demote the catalog record exactly as
                    // the substring form did.
                    match_mode: ContentDemotionRule::PHRASE.into(),
                }],
                ..Default::default()
            },
            None,
        )
        .await
        .expect("search");

    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].source_id, itinerary);
    assert_eq!(result.envelope.source_paths, vec!["itinerary.md"]);
}

/// `minimumShouldMatch: 2` as a lexical prefilter: a chunk holding only one
/// of the query's words never enters the lexical candidate pool (it may
/// still arrive through the vector leg, carrying no lexical score), while a
/// chunk holding two does. The filter is built once per query formulation
/// from the normalized lexemes and evaluated by the GIN index.
#[tokio::test]
async fn minimum_should_match_two_excludes_single_term_rows_from_the_lexical_leg() {
    let Some(url) = test_url() else { return };
    let tenant = fresh_tenant("msm");
    let r = retrieval_for(&url, &tenant).await;
    let col = r
        .ensure_collection("letters", "shape@1", 0, &[], None)
        .await
        .expect("collection");
    let (single, _, _) = r
        .put_source(
            "",
            "text/markdown",
            "single.md",
            None,
            b"Washington county assessed its roads and bridges this spring.",
        )
        .await
        .expect("single-term source");
    let (double, _, _) = r
        .put_source(
            "",
            "text/markdown",
            "double.md",
            None,
            b"George Washington rode through the county to inspect the roads.",
        )
        .await
        .expect("two-term source");
    r.bind_source(&col.id, &single, None).await.expect("bind");
    r.bind_source(&col.id, &double, None).await.expect("bind");
    r.build_collection_index(&col.id, 2000, 1, true)
        .await
        .expect("build");

    let query = "Where did George Washington travel?";
    let lexemes = r.query_lexemes(query).await.expect("lexemes");
    assert_eq!(lexemes, vec!["'georg'", "'washington'", "'travel'"]);

    let lexical_sources = |result: &munarium_core::retrieval::SearchResult| -> Vec<String> {
        result
            .hits
            .iter()
            .filter(|h| h.lexical_score.is_some())
            .map(|h| h.source_id.clone())
            .collect()
    };

    // Knob-free: any query word is a candidate — both documents.
    let without = r
        .search_collection(
            &col.id,
            query,
            SearchParams {
                top_k: 10,
                query_lexemes: lexemes.clone(),
                ..Default::default()
            },
            None,
        )
        .await
        .expect("search");
    let found = lexical_sources(&without);
    assert!(found.contains(&single) && found.contains(&double));

    // minimumShouldMatch 2: the single-word document leaves the lexical leg.
    let with = r
        .search_collection(
            &col.id,
            query,
            SearchParams {
                top_k: 10,
                query_lexemes: lexemes.clone(),
                minimum_should_match: 2,
                ..Default::default()
            },
            None,
        )
        .await
        .expect("search");
    assert_eq!(lexical_sources(&with), vec![double.clone()]);

    // Corpus-adaptive stop terms: "washington" is in 100% of this
    // collection's chunks, so at a 0.5 fraction it stops generating
    // candidates (still ranked); the single-word document — whose only
    // query word IS the stop term — leaves the lexical leg even at
    // minimumShouldMatch 1. The statistics were captured by the build.
    let stopped = r
        .search_collection(
            &col.id,
            query,
            SearchParams {
                top_k: 10,
                query_lexemes: lexemes,
                stop_term_fraction: 0.5,
                ..Default::default()
            },
            None,
        )
        .await
        .expect("search");
    assert_eq!(lexical_sources(&stopped), vec![double]);
}

/// The measured 2026-08-25 selection failure, on real `ts_rank` pools: a
/// collection that USES the question's words densely (a travel narrative
/// about the city of Washington) out-scores the collection that is ABOUT
/// the question's subject on lexical density, and only the query's own
/// phrase ("george washington", found verbatim) separates them. The probe
/// is the session plane's shape — original query, no expansions, the whole
/// fused pool returned.
#[tokio::test]
async fn collection_selection_prefers_the_collection_about_the_subject() {
    let Some(url) = test_url() else { return };
    let tenant = fresh_tenant("select");
    let r = retrieval_for(&url, &tenant).await;

    let narrative = r
        .ensure_collection("narrative", "shape@1", 0, &[], None)
        .await
        .expect("narrative");
    let letterbook = r
        .ensure_collection("letterbook", "shape@1", 0, &[], None)
        .await
        .expect("letterbook");
    let decoy = r
        .ensure_collection("decoy", "shape@1", 0, &[], None)
        .await
        .expect("decoy");

    let docs: [(&str, &str, &[u8]); 6] = [
        (
            "narrative",
            "tour-1.md",
            b"We reached the city of Washington and visited the cities of Georgetown and Alexandria; of all the cities we did visit, Washington was the strangest.",
        ),
        (
            "narrative",
            "tour-2.md",
            b"Travellers visit Washington for its cities in embryo; visit the Capitol, visit the navy yard, and the cities around Washington.",
        ),
        (
            "narrative",
            "tour-3.md",
            b"Washington city: streets laid out for cities that never came; a visit of two days suffices.",
        ),
        (
            "letterbook",
            "letter-1.md",
            b"# George Washington Papers, Series 2, Letterbook 17\n\nDear Sir: Having set out on a tour through the Eastern States, your letter overtook me at this place.",
        ),
        (
            "letterbook",
            "letter-2.md",
            b"George Washington to Henry Knox. Washington left New York on his tour and lodged at Rye; he reached New Haven on the 17th.",
        ),
        (
            "decoy",
            "ordinance.md",
            b"An ordinance for the government of the territory north-west of the river Ohio.",
        ),
    ];
    for (collection, path, bytes) in docs {
        let id = match collection {
            "narrative" => &narrative.id,
            "letterbook" => &letterbook.id,
            _ => &decoy.id,
        };
        let (source, _, _) = r
            .put_source("", "text/markdown", path, None, bytes)
            .await
            .expect("source");
        r.bind_source(id, &source, None).await.expect("bind");
    }
    for info in [&narrative, &letterbook, &decoy] {
        r.build_collection_index(&info.id, 2000, 1, true)
            .await
            .expect("build");
    }

    let query = "What cities did George Washington visit?";
    let probe = SearchParams {
        top_k: 50,
        candidate_n: 50,
        query_expansion_weight: 0.0,
        ..Default::default()
    };
    let mut probes = Vec::new();
    for info in [&narrative, &letterbook, &decoy] {
        let result = r
            .search_collection(&info.id, query, probe.clone(), None)
            .await
            .expect("probe");
        probes.push(CollectionSearchResult {
            collection_id: info.id.clone(),
            collection_name: info.name.clone(),
            result,
        });
    }

    // Density alone prefers the narrative — the premise of the test.
    let density: Vec<f64> = probes
        .iter()
        .map(|p| {
            let mut scores: Vec<f64> = p
                .result
                .hits
                .iter()
                .filter_map(|h| h.lexical_score)
                .collect();
            scores.sort_by(|a, b| b.partial_cmp(a).unwrap());
            scores.into_iter().take(3).sum()
        })
        .collect();
    assert!(
        density[0] > density[1],
        "premise: narrative density {} should exceed letterbook density {}",
        density[0],
        density[1]
    );

    let selected = select_collection_indices(&probes, 1, query, 3.0);
    assert_eq!(
        selected
            .iter()
            .map(|&i| probes[i].collection_name.as_str())
            .collect::<Vec<_>>(),
        vec!["letterbook"]
    );
    // With room for two, density orders the phrase-less remainder.
    let selected = select_collection_indices(&probes, 2, query, 3.0);
    assert_eq!(
        selected
            .iter()
            .map(|&i| probes[i].collection_name.as_str())
            .collect::<Vec<_>>(),
        vec!["letterbook", "narrative"]
    );
}

/// Concurrent ensure_collection with the same name: the advisory lock makes
/// it one collection, one partition, no error on either side.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ensure_collection_is_race_safe() {
    let Some(url) = test_url() else { return };
    let tenant = fresh_tenant("race");
    let store = PgStore::connect(&url, &tenant).await.expect("connect");
    let pool = store.pool().clone();
    let (a, b) = tokio::join!(
        {
            let r = PgRetrieval::new(pool.clone(), &tenant);
            async move { r.ensure_collection("raced", "shape@1", 1, &[], None).await }
        },
        {
            let r = PgRetrieval::new(pool.clone(), &tenant);
            async move { r.ensure_collection("raced", "shape@1", 1, &[], None).await }
        }
    );
    let (a, b) = (a.expect("a"), b.expect("b"));
    assert_eq!(a.id, b.id, "same name must converge on one collection");

    // A different shape for the same name is refused.
    let r = PgRetrieval::new(pool, &tenant);
    assert!(r
        .ensure_collection("raced", "other-shape@1", 1, &[], None)
        .await
        .is_err());
}

/// The partial unique index allows exactly one active index per collection;
/// re-pointing is atomic.
#[tokio::test]
async fn one_active_index_per_collection() {
    let Some(url) = test_url() else { return };
    let tenant = fresh_tenant("act");
    let r = retrieval_for(&url, &tenant).await;
    let col = r
        .ensure_collection("versions", "shape@1", 0, &[], None)
        .await
        .expect("col");

    let (s1, _h1, _) = r
        .put_source("", "text/plain", "first.txt", None, b"first corpus body")
        .await
        .expect("s1");
    r.bind_source(&col.id, &s1, None).await.expect("bind1");
    let v1 = r
        .build_collection_index(&col.id, 2000, 1, true)
        .await
        .expect("v1");

    let (s2, _h2, _) = r
        .put_source(
            "",
            "text/plain",
            "second.txt",
            None,
            b"second corpus body arrives later",
        )
        .await
        .expect("s2");
    r.bind_source(&col.id, &s2, None).await.expect("bind2");
    let v2 = r
        .build_collection_index(&col.id, 2000, 2, true)
        .await
        .expect("v2");
    assert_ne!(v1.id, v2.id);

    assert_eq!(
        r.active_collection_index(&col.id).await.expect("active"),
        Some(v2.id.clone())
    );
    let actives: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM index_versions
          WHERE tenant_id = $1 AND collection_id = $2 AND active",
    )
    .bind(&tenant)
    .bind(&col.id)
    .fetch_one(r.pool())
    .await
    .expect("count");
    assert_eq!(actives, 1);

    // The old version still resolves (provenance never breaks)…
    assert!(r.index_version_by_id(&v1.id).await.is_ok());
    // …and verify + retire keep working on the partitioned store.
    r.verify_collection_index(&v2.id).await.expect("verify");
    r.retire_old_collection(&col.id, 0).await.expect("retire");
    let leftover: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM collection_chunks
          WHERE tenant_id = $1 AND index_version_id = $2",
    )
    .bind(&tenant)
    .bind(&v1.id)
    .fetch_one(r.pool())
    .await
    .expect("leftover");
    assert_eq!(leftover, 0, "retired version's chunks reclaimed");
}

/// A collection's access requirement may be raised (or kept) by a
/// re-declaration but never lowered — otherwise a second runbook reusing the
/// name could downgrade the first's compartmentalization and leak its data.
#[tokio::test]
async fn ensure_collection_refuses_access_downgrade() {
    let Some(url) = test_url() else { return };
    let tenant = fresh_tenant("downgrade");
    let r = retrieval_for(&url, &tenant).await;

    r.ensure_collection("shared", "s@1", 2, &["eng".to_string()], None)
        .await
        .expect("create at level 2 + eng");

    // Lowering the level is refused.
    assert!(
        r.ensure_collection("shared", "s@1", 0, &["eng".to_string()], None)
            .await
            .is_err(),
        "lowering access_level must be refused"
    );
    // Dropping a compartment is refused.
    assert!(
        r.ensure_collection("shared", "s@1", 2, &[], None)
            .await
            .is_err(),
        "dropping a compartment must be refused"
    );
    // Keeping the requirement is fine (idempotent re-apply).
    r.ensure_collection("shared", "s@1", 2, &["eng".to_string()], None)
        .await
        .expect("same requirement ok");
    // Raising is fine (tightening is always safe).
    let raised = r
        .ensure_collection(
            "shared",
            "s@1",
            3,
            &["eng".to_string(), "fin".to_string()],
            None,
        )
        .await
        .expect("raising ok");
    assert_eq!(raised.access_level, 3);
    assert_eq!(raised.compartments.len(), 2);
}

/// A legacy shape-scoped index and a collection-scoped index that share a
/// shape_ref have independent active pointers: a v1 cutover must not
/// deactivate the collection's index, and legacy resolution must never pick
/// the collection index (whose chunks live in collection_chunks).
#[tokio::test]
async fn legacy_and_collection_indexes_are_independent() {
    let Some(url) = test_url() else { return };
    let tenant = fresh_tenant("mixed");
    let r = retrieval_for(&url, &tenant).await;
    let shape = "shared-shape@1";

    // Legacy shape-scoped source + index (chunks in index_chunks).
    let (legacy_src, _legacy_hash, _) = r
        .put_source(
            "",
            "text/plain",
            "legacy.txt",
            Some(shape),
            b"legacy corpus body",
        )
        .await
        .expect("legacy source");
    let legacy_iv = r
        .build_index(shape, 2000, 1, true)
        .await
        .expect("legacy build");
    assert!(legacy_iv.active);

    // Collection on the SAME shape (chunks in collection_chunks).
    let col = r
        .ensure_collection("mixed-col", shape, 0, &[], None)
        .await
        .expect("collection");
    let (col_src, _col_hash, _) = r
        .put_source("", "text/plain", "col.txt", None, b"collection corpus body")
        .await
        .expect("col source");
    let _ = legacy_src; // both distinct sources
    r.bind_source(&col.id, &col_src, None).await.expect("bind");
    let col_iv = r
        .build_collection_index(&col.id, 2000, 1, true)
        .await
        .expect("col build");

    // Both active simultaneously, independent pointers.
    assert_eq!(
        r.active_collection_index(&col.id)
            .await
            .expect("col active"),
        Some(col_iv.id.clone())
    );

    // A second legacy cutover must NOT touch the collection's active pointer.
    let (_s2, _h2b, _) = r
        .put_source(
            "",
            "text/plain",
            "legacy2.txt",
            Some(shape),
            b"second legacy body",
        )
        .await
        .expect("legacy2");
    let legacy_iv2 = r
        .build_index(shape, 2000, 2, true)
        .await
        .expect("legacy build2");
    assert_ne!(legacy_iv.id, legacy_iv2.id);
    assert_eq!(
        r.active_collection_index(&col.id)
            .await
            .expect("col still active"),
        Some(col_iv.id.clone()),
        "legacy cutover must not deactivate the collection index"
    );

    // Legacy resolution (index_version by shape) must pick the LEGACY index,
    // never the collection one.
    use munarium_core::retrieval::RetrievalBackend;
    let resolved = r.index_version(shape).await.expect("legacy resolve");
    assert_eq!(resolved.id, legacy_iv2.id);
    assert_ne!(resolved.id, col_iv.id);
}

/// The regression test for the identity change: identical BYTES uploaded to
/// two logical paths are two independently bindable sources.
///
/// Before identity moved to the path, `ON CONFLICT (tenant, content_hash) DO
/// NOTHING` silently kept the FIRST filename, so the second upload vanished
/// and could never match its own collection's prefix binding.
#[tokio::test]
async fn identical_bytes_at_two_paths_are_two_sources() {
    let Some(url) = test_url() else { return };
    let tenant = fresh_tenant("dup");
    let r = retrieval_for(&url, &tenant).await;
    let body = b"the very same handbook bytes, uploaded twice under two names";

    let (src_a, hash_a, existed_a) = r
        .put_source("", "text/markdown", "northgate/policy.md", None, body)
        .await
        .expect("a");
    let (src_b, hash_b, existed_b) = r
        .put_source("", "text/markdown", "smoke/policy.md", None, body)
        .await
        .expect("b");

    assert_ne!(src_a, src_b, "two paths must be two sources");
    assert_eq!(hash_a, hash_b, "identical bytes keep one content hash");
    assert!(!existed_a && !existed_b, "neither is a replay of the other");

    // Each binds to its own collection, and only its own.
    let north = r
        .ensure_collection("north", "shape@1", 0, &[], None)
        .await
        .expect("north");
    let smoke = r
        .ensure_collection("smoke", "shape@1", 0, &[], None)
        .await
        .expect("smoke");
    assert!(r
        .bind_source(&north.id, &src_a, None)
        .await
        .expect("bind a"));
    assert!(r
        .bind_source(&smoke.id, &src_b, None)
        .await
        .expect("bind b"));
    assert_eq!(r.collection_source_count(&north.id).await.expect("n"), 1);
    assert_eq!(r.collection_source_count(&smoke.id).await.expect("s"), 1);

    // Distinct chunk ids, so the two indexes cannot corrupt each other.
    let iv_n = r
        .build_collection_index(&north.id, 2000, 1, true)
        .await
        .expect("build n");
    let iv_s = r
        .build_collection_index(&smoke.id, 2000, 1, true)
        .await
        .expect("build s");
    assert_ne!(iv_n.id, iv_s.id);

    // Provenance names the DOCUMENT, which a bare hash never could: same
    // bytes, same hash, different answer about where it came from.
    let hits_n = r
        .search_collection(&north.id, "handbook", SearchParams::default(), None)
        .await
        .expect("search n");
    let hits_s = r
        .search_collection(&smoke.id, "handbook", SearchParams::default(), None)
        .await
        .expect("search s");
    assert_eq!(hits_n.envelope.source_paths, vec!["northgate/policy.md"]);
    assert_eq!(hits_s.envelope.source_paths, vec!["smoke/policy.md"]);
    assert_eq!(
        hits_n.envelope.source_content_hashes, hits_s.envelope.source_content_hashes,
        "the bytes really are identical — only identity differs"
    );
}

/// Re-putting the SAME path with NEW bytes is an update, not a duplicate: one
/// source row, new hash, and `existed: false` because a rebuild is now owed.
#[tokio::test]
async fn same_path_new_bytes_updates_in_place() {
    let Some(url) = test_url() else { return };
    let tenant = fresh_tenant("update");
    let r = retrieval_for(&url, &tenant).await;

    let (src1, hash1, existed1) = r
        .put_source(
            "",
            "text/markdown",
            "kb/article.md",
            None,
            b"first revision",
        )
        .await
        .expect("v1");
    assert!(!existed1);
    let (src_replay, hash_replay, existed_replay) = r
        .put_source(
            "",
            "text/markdown",
            "kb/article.md",
            None,
            b"first revision",
        )
        .await
        .expect("replay");
    assert_eq!(src_replay, src1);
    assert_eq!(hash_replay, hash1);
    assert!(
        existed_replay,
        "same path + same bytes is an idempotent replay"
    );

    let (src2, hash2, existed2) = r
        .put_source(
            "",
            "text/markdown",
            "kb/article.md",
            None,
            b"second revision",
        )
        .await
        .expect("v2");
    assert_eq!(src2, src1, "identity is the path, so it is stable");
    assert_ne!(hash2, hash1, "content hash follows the bytes");
    assert!(!existed2, "new bytes are not a replay — a rebuild is owed");

    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sources WHERE tenant_id = $1")
        .bind(&tenant)
        .fetch_one(r.pool())
        .await
        .expect("count");
    assert_eq!(rows, 1, "an update must not leave a second row behind");
}

/// A DOCX must be searchable by words that exist ONLY inside the zipped XML.
///
/// This is the check that cannot pass vacuously: before extraction, the bytes
/// were `from_utf8_lossy`'d into replacement characters, so the build still
/// "succeeded" and search returned nothing. Asserting on a hit for text that
/// is only retrievable via extraction is the difference.
#[tokio::test]
async fn docx_sources_are_extracted_before_indexing() {
    let Some(url) = test_url() else { return };
    let tenant = fresh_tenant("docx");
    let r = retrieval_for(&url, &tenant).await;

    let docx = {
        use std::io::{Cursor, Write};
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            zw.start_file("word/document.xml", opts).expect("start");
            zw.write_all(
                br#"<w:document xmlns:w="x"><w:body>
                  <w:p><w:r><w:t>Sabbatical eligibility requires seven years</w:t></w:r></w:p>
                </w:body></w:document>"#,
            )
            .expect("write");
            zw.finish().expect("finish");
        }
        buf
    };

    let (src, _hash, _) = r
        .put_source(
            "",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "policies/sabbatical.docx",
            None,
            &docx,
        )
        .await
        .expect("put docx");
    let col = r
        .ensure_collection("policies", "shape@1", 0, &[], None)
        .await
        .expect("collection");
    r.bind_source(&col.id, &src, None).await.expect("bind");
    r.build_collection_index(&col.id, 2000, 1, true)
        .await
        .expect("build");

    let hits = r
        .search_collection(&col.id, "sabbatical", SearchParams::default(), None)
        .await
        .expect("search");
    assert!(
        !hits.hits.is_empty(),
        "a word only present inside the DOCX xml must be retrievable"
    );
    assert!(hits.hits[0].text.contains("seven years"));
    assert_eq!(hits.envelope.source_paths, vec!["policies/sabbatical.docx"]);

    // And the source row records HOW the text was obtained.
    let (status, method): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT extraction_status, extraction_method FROM sources
          WHERE tenant_id = $1 AND source_id = $2",
    )
    .bind(&tenant)
    .bind(&src)
    .fetch_one(r.pool())
    .await
    .expect("row");
    assert_eq!(status.as_deref(), Some("ok"));
    assert_eq!(method.as_deref(), Some("docx"));
}

/// A source that yields no text is recorded `empty` and does not fail the
/// build — the quiet-gap case the experiments exist to catch.
#[tokio::test]
async fn an_unextractable_source_is_marked_empty_and_the_build_continues() {
    let Some(url) = test_url() else { return };
    let tenant = fresh_tenant("empty");
    let r = retrieval_for(&url, &tenant).await;

    let (bad, _, _) = r
        .put_source(
            "",
            "application/octet-stream",
            "odd/blob.bin",
            None,
            b"\x00\x01\x02\x00",
        )
        .await
        .expect("put binary");
    let (good, _, _) = r
        .put_source(
            "",
            "text/markdown",
            "odd/readme.md",
            None,
            b"Ostrich husbandry notes",
        )
        .await
        .expect("put md");
    let col = r
        .ensure_collection("mixed", "shape@1", 0, &[], None)
        .await
        .expect("collection");
    r.bind_source(&col.id, &bad, None).await.expect("bind bad");
    r.bind_source(&col.id, &good, None)
        .await
        .expect("bind good");

    // The unindexable document must not take the whole build down.
    r.build_collection_index(&col.id, 2000, 1, true)
        .await
        .expect("build survives an unextractable source");

    let hits = r
        .search_collection(&col.id, "ostrich", SearchParams::default(), None)
        .await
        .expect("search");
    assert!(!hits.hits.is_empty(), "the good document still indexed");

    let status: Option<String> = sqlx::query_scalar(
        "SELECT extraction_status FROM sources WHERE tenant_id = $1 AND source_id = $2",
    )
    .bind(&tenant)
    .bind(&bad)
    .fetch_one(r.pool())
    .await
    .expect("row");
    assert_eq!(
        status.as_deref(),
        Some("empty"),
        "a document contributing zero chunks must be findable, not silently absent"
    );
}

/// A stub provider standing in for any document-intelligence backend. Its
/// existence is the point of the trait: the escalation is testable with no
/// cloud account, no key, and no network.
#[derive(Default)]
struct StubDocIntel {
    calls: std::sync::atomic::AtomicUsize,
    reply: Option<&'static str>,
}

#[async_trait::async_trait]
impl munarium_core::docintel::DocumentIntelligence for StubDocIntel {
    fn supports(&self, media_type: &str) -> bool {
        media_type == "application/pdf"
    }
    fn id(&self) -> &'static str {
        "stub"
    }
    async fn analyze(
        &self,
        _media_type: &str,
        _bytes: &[u8],
    ) -> munarium_core::Result<munarium_core::docintel::AnalyzedDocument> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        match self.reply {
            Some(text) => Ok(munarium_core::docintel::AnalyzedDocument {
                text: text.to_string(),
                pages_analyzed: 1,
                provider_fingerprint: "stub/model/v1".into(),
            }),
            None => Ok(munarium_core::docintel::AnalyzedDocument::empty(
                "stub/model/v1",
            )),
        }
    }
}

/// The escalation recovers text local extraction could not read — and, just
/// as importantly, is NOT called for documents local extraction handled.
#[tokio::test]
async fn document_intelligence_escalates_only_for_unreadable_documents() {
    let Some(url) = test_url() else { return };
    let tenant = fresh_tenant("docintel");
    let store = PgStore::connect(&url, &tenant).await.expect("connect");
    let stub = std::sync::Arc::new(StubDocIntel {
        calls: Default::default(),
        reply: Some("Recovered by the analyzer: quokka husbandry permit"),
    });
    let r = PgRetrieval::new(store.pool().clone(), &tenant).with_doc_intel(Some(stub.clone()));

    // A PDF with no text layer — the case local extraction cannot read.
    let scan = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n%%EOF\n";
    let (scanned, _, _) = r
        .put_source("", "application/pdf", "fda/scan.pdf", None, scan)
        .await
        .expect("put pdf");
    // Plain markdown — local extraction handles it, so the paid path must
    // never be touched.
    let (plain, _, _) = r
        .put_source(
            "",
            "text/markdown",
            "fda/notes.md",
            None,
            b"Ordinary readable notes",
        )
        .await
        .expect("put md");

    let col = r
        .ensure_collection("escalation", "shape@1", 0, &[], None)
        .await
        .expect("collection");
    r.bind_source(&col.id, &scanned, None)
        .await
        .expect("bind pdf");
    r.bind_source(&col.id, &plain, None).await.expect("bind md");
    r.build_collection_index(&col.id, 2000, 1, true)
        .await
        .expect("build");

    assert_eq!(
        stub.calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the analyzer must be called for the scan and NOT for the markdown"
    );

    // Text only the analyzer could produce is now retrievable.
    let hits = r
        .search_collection(&col.id, "quokka", SearchParams::default(), None)
        .await
        .expect("search");
    assert!(!hits.hits.is_empty(), "escalated text must be indexed");

    let (status, method): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT extraction_status, extraction_method FROM sources
          WHERE tenant_id = $1 AND source_id = $2",
    )
    .bind(&tenant)
    .bind(&scanned)
    .fetch_one(r.pool())
    .await
    .expect("row");
    assert_eq!(status.as_deref(), Some("ok"));
    assert_eq!(
        method.as_deref(),
        Some("ocr"),
        "OCR'd text must be distinguishable from a real text layer"
    );
}

/// With no provider configured the pipeline is complete, not degraded: local
/// extraction still runs and unreadable documents are recorded `empty`.
#[tokio::test]
async fn without_a_provider_the_build_still_succeeds() {
    let Some(url) = test_url() else { return };
    let tenant = fresh_tenant("nodocintel");
    let r = retrieval_for(&url, &tenant).await; // no provider attached

    let scan = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n%%EOF\n";
    let (scanned, _, _) = r
        .put_source("", "application/pdf", "fda/scan.pdf", None, scan)
        .await
        .expect("put pdf");
    let col = r
        .ensure_collection("noprovider", "shape@1", 0, &[], None)
        .await
        .expect("collection");
    r.bind_source(&col.id, &scanned, None).await.expect("bind");
    r.build_collection_index(&col.id, 2000, 1, true)
        .await
        .expect("build succeeds with no provider configured");

    let status: Option<String> = sqlx::query_scalar(
        "SELECT extraction_status FROM sources WHERE tenant_id = $1 AND source_id = $2",
    )
    .bind(&tenant)
    .bind(&scanned)
    .fetch_one(r.pool())
    .await
    .expect("row");
    // Either visible state is correct, and the difference is meaningful:
    // `empty` = parsed fine, no text found (a clean scan); `failed` = could
    // not be parsed at all (this stub fixture). What must NOT happen is `ok`
    // with no chunks, which is the silent gap.
    assert!(
        matches!(status.as_deref(), Some("empty") | Some("failed")),
        "unreadable without a provider must be visible, got {status:?}"
    );
}

/// A provider outage degrades the index; it must never fail the build.
#[tokio::test]
async fn a_provider_failure_does_not_fail_the_build() {
    let Some(url) = test_url() else { return };
    let tenant = fresh_tenant("difail");
    let store = PgStore::connect(&url, &tenant).await.expect("connect");

    struct Failing;
    #[async_trait::async_trait]
    impl munarium_core::docintel::DocumentIntelligence for Failing {
        fn supports(&self, m: &str) -> bool {
            m == "application/pdf"
        }
        fn id(&self) -> &'static str {
            "failing"
        }
        async fn analyze(
            &self,
            _m: &str,
            _b: &[u8],
        ) -> munarium_core::Result<munarium_core::docintel::AnalyzedDocument> {
            Err(munarium_core::KernelError::Provider(
                "service unavailable".into(),
            ))
        }
    }

    let r = PgRetrieval::new(store.pool().clone(), &tenant)
        .with_doc_intel(Some(std::sync::Arc::new(Failing)));
    let (scanned, _, _) = r
        .put_source(
            "",
            "application/pdf",
            "fda/scan.pdf",
            None,
            b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n%%EOF\n",
        )
        .await
        .expect("put");
    let (plain, _, _) = r
        .put_source(
            "",
            "text/markdown",
            "fda/ok.md",
            None,
            b"Readable content survives",
        )
        .await
        .expect("put md");
    let col = r
        .ensure_collection("outage", "shape@1", 0, &[], None)
        .await
        .expect("collection");
    r.bind_source(&col.id, &scanned, None).await.expect("bind");
    r.bind_source(&col.id, &plain, None).await.expect("bind md");

    r.build_collection_index(&col.id, 2000, 1, true)
        .await
        .expect("a provider outage must degrade the index, not fail the build");

    let hits = r
        .search_collection(&col.id, "readable", SearchParams::default(), None)
        .await
        .expect("search");
    assert!(!hits.hits.is_empty(), "the readable document still indexed");
}

/// The class-A property end to end (2026-08-30, §13.5 entry 25): a corpus
/// writing `US4436097` is reached by `4,436,097`, `4436097` and `US4436097`
/// alike — the three forms the demo's patents page taught us people actually
/// type. The derived table is populated LAZILY here (no cutover ran this
/// scan), which is the path an index built before migration 0025 takes.
#[tokio::test]
async fn number_forms_reach_the_corpus_spelling() {
    let Some(url) = test_url() else { return };
    let tenant = fresh_tenant("numf");
    let r = retrieval_for(&url, &tenant).await;
    let info = r
        .ensure_collection("patents", "shape-a@1", 0, &[], None)
        .await
        .expect("collection");
    let (target, _, _) = r
        .put_source(
            "",
            "text/plain",
            "us4436097.txt",
            None,
            b"US4436097 Cardiovascular exercise apparatus issued 1984-03-13",
        )
        .await
        .expect("target source");
    let (noise, _, _) = r
        .put_source(
            "",
            "text/plain",
            "noise.txt",
            None,
            b"An unrelated fitness note about rowing machines",
        )
        .await
        .expect("noise source");
    r.bind_source(&info.id, &target, None).await.expect("bind");
    r.bind_source(&info.id, &noise, None).await.expect("bind");
    r.build_collection_index(&info.id, 2000, 1, true)
        .await
        .expect("build");

    let infos = vec![info.clone()];
    for query in [
        "issue date of 4,436,097",
        "issue date of 4436097",
        "issue date of US4436097",
    ] {
        let digits = munarium_retrieval_pg::number_query_digits(query);
        let forms = r.number_form_lexemes(&infos, &digits).await.expect("forms");
        let effective = if forms.is_empty() && digits.is_empty() {
            query.to_string()
        } else {
            let mut extra = digits.clone();
            extra.extend(forms);
            format!("{} {}", query, extra.join(" "))
        };
        let hits = r
            .search_collection(&info.id, &effective, SearchParams::default(), None)
            .await
            .expect("search")
            .hits;
        assert!(
            hits.iter().any(|h| h.source_id == target),
            "{query:?} did not reach the corpus form; effective {effective:?}"
        );
    }

    // Access isolation: a collection OUTSIDE the caller's permitted list
    // contributes no forms — the lookup is keyed by that list, not by the
    // tenant's whole registry. A lexeme leak is a smaller cousin of serving
    // the document.
    let other = r
        .ensure_collection("elsewhere", "shape-a@1", 3, &["secret".to_string()], None)
        .await
        .expect("other");
    let (hidden, _, _) = r
        .put_source(
            "",
            "text/plain",
            "secret.txt",
            None,
            b"EP9990001 hidden identifier",
        )
        .await
        .expect("hidden source");
    r.bind_source(&other.id, &hidden, None).await.expect("bind");
    r.build_collection_index(&other.id, 2000, 1, true)
        .await
        .expect("build");
    let leaked = r
        .number_form_lexemes(&infos, &["9990001".to_string()])
        .await
        .expect("lookup");
    assert!(
        leaked.is_empty(),
        "a form leaked from a collection outside the permitted set: {leaked:?}"
    );

    // Concurrent lazy population is free: two racing callers, one sentinel,
    // one answer.
    let tenant2 = fresh_tenant("numc");
    let r2 = retrieval_for(&url, &tenant2).await;
    let info2 = r2
        .ensure_collection("race", "shape-a@1", 0, &[], None)
        .await
        .expect("collection");
    let (boxing, _, _) = r2
        .put_source(
            "",
            "text/plain",
            "us7909749.txt",
            None,
            b"US7909749 Boxing device",
        )
        .await
        .expect("source");
    r2.bind_source(&info2.id, &boxing, None)
        .await
        .expect("bind");
    r2.build_collection_index(&info2.id, 2000, 1, true)
        .await
        .expect("build");
    let list = vec![info2.clone()];
    let key = vec!["7909749".to_string()];
    let (a, b) = tokio::join!(
        r2.number_form_lexemes(&list, &key),
        r2.number_form_lexemes(&list, &key)
    );
    assert_eq!(a.expect("a"), vec!["us7909749".to_string()]);
    assert_eq!(b.expect("b"), vec!["us7909749".to_string()]);
}
