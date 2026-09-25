// SPDX-License-Identifier: Apache-2.0
//! Real PostgreSQL boundaries; no production sequence-setting API or large corpus.
use munarium_core::KernelError;
use munarium_retrieval_pg::PgRetrieval;
use munarium_store_pg::PgStore;

#[tokio::test]
async fn wire_integer_watermarks_are_exact_and_overflow_does_not_mutate() {
    let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
        eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
        return;
    };
    let tenant = format!("wire-watermark-{}", uuid::Uuid::new_v4());
    let store = PgStore::connect(&url, &tenant).await.unwrap();
    let r = PgRetrieval::new(store.pool().clone(), &tenant);
    let col = r
        .ensure_collection("wire", "wire@1", 0, &[], None)
        .await
        .unwrap();
    let (source, _, _) = r
        .put_source(
            "",
            "text/plain",
            "wire.txt",
            Some("wire@1"),
            b"A small fictional boundary fixture.",
        )
        .await
        .unwrap();
    r.bind_source(&col.id, &source, None).await.unwrap();
    let mut prepared = r.extract_collection_prepared(&col.id, 400).await.unwrap();
    for watermark in [(1_u64 << 53) - 1, 1 << 53, (1 << 53) + 1, i64::MAX as u64] {
        let legacy = r
            .build_index("wire@1", 400, watermark, false)
            .await
            .unwrap();
        assert_eq!(legacy.event_watermark, watermark);
        assert_eq!(
            r.resolve_index("wire@1", Some(&legacy.id)).await.unwrap().1,
            watermark
        );
        let collection = r
            .build_collection_index(&col.id, 400, watermark, false)
            .await
            .unwrap();
        assert_eq!(collection.event_watermark, watermark);
        let direct_id = format!("idx-wire-{watermark}");
        assert!(r
            .commit_prepared_index(
                &col.id,
                &direct_id,
                &serde_json::json!({}),
                watermark,
                &prepared
            )
            .await
            .unwrap());
        assert_eq!(
            r.index_version_by_id(&direct_id)
                .await
                .unwrap()
                .event_watermark,
            watermark
        );
    }
    let legacy = r.build_index("wire@1", 400, 7, false).await.unwrap();
    let collection = r
        .build_collection_index(&col.id, 400, 7, false)
        .await
        .unwrap();
    for overflow in [i64::MAX as u64 + 1, u64::MAX] {
        assert!(matches!(
            r.build_index("wire@1", 400, overflow, false).await,
            Err(KernelError::Storage(_))
        ));
        assert!(matches!(
            r.build_collection_index(&col.id, 400, overflow, false)
                .await,
            Err(KernelError::Storage(_))
        ));
        assert!(matches!(
            r.commit_prepared_index(
                &col.id,
                "idx-overflow",
                &serde_json::json!({}),
                overflow,
                &prepared
            )
            .await,
            Err(KernelError::Storage(_))
        ));
        assert_eq!(
            r.index_version_by_id(&legacy.id)
                .await
                .unwrap()
                .event_watermark,
            7
        );
        assert_eq!(
            r.index_version_by_id(&collection.id)
                .await
                .unwrap()
                .event_watermark,
            7
        );
        assert!(matches!(
            r.index_version_by_id("idx-overflow").await,
            Err(KernelError::NotFound { .. })
        ));
    }
    // Prepared builds are public input to the persistence boundary, so an
    // ordinal exceeding INTEGER must not wrap into a negative chunk ordinal.
    prepared.chunks[0].ordinal = i32::MAX as u32;
    assert!(r
        .commit_prepared_index(
            &col.id,
            "idx-max-ordinal",
            &serde_json::json!({}),
            7,
            &prepared
        )
        .await
        .unwrap());
    let ordinal: i32 = sqlx::query_scalar("SELECT ordinal FROM collection_chunks WHERE tenant_id=$1 AND collection_id=$2 AND index_version_id='idx-max-ordinal'")
        .bind(&tenant).bind(&col.id).fetch_one(store.pool()).await.unwrap();
    assert_eq!(ordinal, i32::MAX);
    prepared.chunks[0].ordinal = i32::MAX as u32 + 1;
    assert!(matches!(
        r.commit_prepared_index(
            &col.id,
            "idx-bad-ordinal",
            &serde_json::json!({}),
            7,
            &prepared
        )
        .await,
        Err(KernelError::Storage(_))
    ));
    assert!(matches!(
        r.index_version_by_id("idx-bad-ordinal").await,
        Err(KernelError::NotFound { .. })
    ));
}

#[tokio::test]
async fn wire_integer_negative_persisted_watermark_is_not_unsigned_success() {
    let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
        eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
        return;
    };
    let tenant = format!("wire-negative-{}", uuid::Uuid::new_v4());
    let store = PgStore::connect(&url, &tenant).await.unwrap();
    let r = PgRetrieval::new(store.pool().clone(), &tenant);
    sqlx::query("INSERT INTO index_versions (tenant_id,id,shape_ref,manifest,watermark_seq,active) VALUES ($1,'idx-negative','wire@1','{}',-1,false)")
        .bind(&tenant).execute(store.pool()).await.unwrap();
    assert!(matches!(
        r.index_version_by_id("idx-negative").await,
        Err(KernelError::Storage(_))
    ));
    assert!(matches!(
        r.resolve_index("wire@1", Some("idx-negative")).await,
        Err(KernelError::Storage(_))
    ));
}

#[tokio::test]
async fn wire_integer_negative_source_length_is_not_unsigned_success() {
    let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
        eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
        return;
    };
    let tenant = format!("wire-source-{}", uuid::Uuid::new_v4());
    let store = PgStore::connect(&url, &tenant).await.unwrap();
    let r = PgRetrieval::new(store.pool().clone(), &tenant);
    let (source, _, _) = r
        .put_source("", "text/plain", "wire.txt", None, b"fictional")
        .await
        .unwrap();
    assert_eq!(r.source_info(&source).await.unwrap().bytes_len, 9);
    sqlx::query("UPDATE sources SET bytes_len = -1 WHERE tenant_id = $1 AND source_id = $2")
        .bind(&tenant)
        .bind(&source)
        .execute(store.pool())
        .await
        .unwrap();
    assert!(matches!(
        r.source_info(&source).await,
        Err(KernelError::Storage(_))
    ));
}
