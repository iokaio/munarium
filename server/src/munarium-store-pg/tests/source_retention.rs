// SPDX-License-Identifier: Apache-2.0
use munarium_core::sources::{SourceKey, SourceStore};
use munarium_store_pg::{source_retention as retention, PgSourceStore, PgStore};
use std::sync::Arc;

async fn fixture() -> Option<(PgStore, String, SourceKey)> {
    let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
        eprintln!("UNAVAILABLE: source retention requires isolated PostgreSQL");
        return None;
    };
    let tenant = format!("retention-{}", uuid::Uuid::new_v4().simple());
    let store = PgStore::connect(&url, &tenant).await.unwrap();
    let key = SourceKey::new(&tenant, "fictional/original.txt", &"a".repeat(64)).unwrap();
    let bytes = PgSourceStore::new(store.pool().clone());
    let uri = bytes
        .put(&key, "text/plain", b"fictional original")
        .await
        .unwrap();
    sqlx::query("INSERT INTO sources(tenant_id,source_id,filename,content_hash,media_type,blob_uri,storage_backend,bytes_len) VALUES($1,$2,$3,$4,'text/plain',$5,'pg',18)")
        .bind(&tenant).bind(key.source_id()).bind(&key.path).bind(&key.content_hash).bind(uri).execute(store.pool()).await.unwrap();
    Some((store, tenant, key))
}

#[tokio::test]
async fn retention_hold_shared_ownership_failure_retry_and_restore_replay() {
    let Some((store, tenant, key)) = fixture().await else {
        return;
    };
    let pool = store.pool();
    let raw = Arc::new(PgSourceStore::new(pool.clone()));
    let guarded = retention::GuardedSources::new(pool.clone(), raw.clone());
    retention::change(pool, &tenant, &key.path, "hold")
        .await
        .unwrap();
    assert!(
        guarded.get(&key).await.is_ok(),
        "a hold never denies reading"
    );
    retention::change(pool, &tenant, &key.path, "deny-and-erase-pg-original")
        .await
        .unwrap();
    assert!(guarded.get(&key).await.is_err());
    assert!(guarded.exists(&key).await.is_err());
    assert!(!retention::cleanup_one(pool, &tenant, &key.source_id())
        .await
        .unwrap());
    assert!(raw.exists(&key).await.unwrap());
    retention::change(pool, &tenant, &key.path, "release-hold")
        .await
        .unwrap();
    // Fail the physical delete through a real PostgreSQL foreign key. The
    // transaction must preserve both bytes and pending work, then retry cleanly.
    let table = format!("retention_failure_{}", uuid::Uuid::new_v4().simple());
    sqlx::raw_sql(&format!("CREATE TABLE {table}(tenant_id TEXT,blob_name TEXT,FOREIGN KEY(tenant_id,blob_name) REFERENCES source_blobs(tenant_id,blob_name))")).execute(pool).await.unwrap();
    sqlx::query(&format!("INSERT INTO {table} VALUES ($1,$2)"))
        .bind(&tenant)
        .bind(key.blob_name())
        .execute(pool)
        .await
        .unwrap();
    assert!(retention::cleanup_one(pool, &tenant, &key.source_id())
        .await
        .is_err());
    assert!(raw.exists(&key).await.unwrap());
    let state: String =
        sqlx::query_scalar("SELECT cleanup_state FROM source_retention WHERE tenant_id=$1")
            .bind(&tenant)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(state, "pending");
    sqlx::raw_sql(&format!("DROP TABLE {table}"))
        .execute(pool)
        .await
        .unwrap();
    assert!(retention::cleanup_one(pool, &tenant, &key.source_id())
        .await
        .unwrap());
    assert!(!raw.exists(&key).await.unwrap());
    assert!(!retention::cleanup_one(pool, &tenant, &key.source_id())
        .await
        .unwrap());
    assert!(retention::change(pool, &tenant, &key.path, "hold")
        .await
        .is_err());
    assert!(
        raw.put(&key, "text/plain", b"resurrection").await.is_err(),
        "the PG trigger closes even an unguarded writer"
    );
    assert!(retention::change(pool, &tenant, "evidence/fixture", "deny")
        .await
        .is_err());

    // Controlled restored-state simulation in this fixture's tenant: an old
    // backup can restore the original if its journal is absent. Reapplying the
    // exported stable path/action restores denial before the instance serves.
    sqlx::query("DELETE FROM source_retention WHERE tenant_id=$1")
        .bind(&tenant)
        .execute(pool)
        .await
        .unwrap();
    raw.put(&key, "text/plain", b"restored original")
        .await
        .unwrap();
    assert!(guarded.get(&key).await.is_ok());
    retention::change(pool, &tenant, &key.path, "deny-and-erase-pg-original")
        .await
        .unwrap();
    assert!(guarded.get(&key).await.is_err());
    assert!(retention::cleanup_one(pool, &tenant, &key.source_id())
        .await
        .unwrap());
    assert!(!raw.exists(&key).await.unwrap());

    let Some((shared_store, shared_tenant, shared_key)) = fixture().await else {
        unreachable!();
    };
    let shared_pool = shared_store.pool();
    for owner in ["first", "second"] {
        sqlx::query(
            "INSERT INTO collection_sources(tenant_id,collection_id,source_id) VALUES($1,$2,$3)",
        )
        .bind(&shared_tenant)
        .bind(owner)
        .bind(shared_key.source_id())
        .execute(shared_pool)
        .await
        .unwrap();
    }
    retention::change(
        shared_pool,
        &shared_tenant,
        &shared_key.path,
        "deny-and-erase-pg-original",
    )
    .await
    .unwrap();
    assert!(
        !retention::cleanup_one(shared_pool, &shared_tenant, &shared_key.source_id())
            .await
            .unwrap()
    );
    let reason: String =
        sqlx::query_scalar("SELECT cleanup_blocked FROM source_retention WHERE tenant_id=$1")
            .bind(&shared_tenant)
            .fetch_one(shared_pool)
            .await
            .unwrap();
    assert_eq!(reason, "shared-source");
    assert!(PgSourceStore::new(shared_pool.clone())
        .exists(&shared_key)
        .await
        .unwrap());
    assert!(
        retention::assert_sources(shared_pool, &tenant, &[shared_key.source_id()])
            .await
            .is_ok(),
        "tenant isolation"
    );
}

#[tokio::test]
async fn retention_hold_cleanup_race_has_one_linearized_outcome() {
    for _ in 0..8 {
        let Some((store, tenant, key)) = fixture().await else {
            return;
        };
        retention::change(
            store.pool(),
            &tenant,
            &key.path,
            "deny-and-erase-pg-original",
        )
        .await
        .unwrap();
        let id = key.source_id();
        let (hold, cleanup) = tokio::join!(
            retention::change(store.pool(), &tenant, &key.path, "hold"),
            retention::cleanup_one(store.pool(), &tenant, &id)
        );
        let cleanup = cleanup.unwrap();
        let exists = PgSourceStore::new(store.pool().clone())
            .exists(&key)
            .await
            .unwrap();
        if hold.is_ok() {
            assert!(!cleanup);
            assert!(exists);
        } else {
            assert!(cleanup);
            assert!(!exists);
        }
    }
}

#[path = "../../../tests/support/process_crash.rs"]
mod harness;

#[test]
fn source_retention_process_recovery() {
    if std::env::var_os("MUNARIUM_TEST_DATABASE_URL").is_none() {
        eprintln!("UNAVAILABLE: isolated PostgreSQL required");
        return;
    }
    for phase in ["denial_committed", "cleanup_committed"] {
        for crash in [false, true] {
            harness::run(
                "source_retention_child",
                "postgres",
                phase,
                crash,
                &format!("p08-source-{}", uuid::Uuid::new_v4().simple()),
            );
        }
    }
}

#[test]
#[ignore = "owned child fixture"]
fn source_retention_child() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        tokio::time::timeout(harness::LIMIT, async {
            let tenant=harness::setting("P08_TENANT");
            let store=PgStore::connect(&harness::setting("MUNARIUM_TEST_DATABASE_URL"),&tenant).await.unwrap();
            let key=SourceKey::new(&tenant,"fixture.txt",&"b".repeat(64)).unwrap();
            let raw=Arc::new(PgSourceStore::new(store.pool().clone()));
            let guarded=retention::GuardedSources::new(store.pool().clone(),raw.clone());
            let mode=harness::setting("P08_MODE");
            if mode=="setup" {
                let uri=raw.put(&key,"text/plain",b"fictional original").await.unwrap();
                sqlx::query("INSERT INTO sources(tenant_id,source_id,filename,content_hash,media_type,blob_uri,storage_backend,bytes_len) VALUES($1,$2,$3,$4,'text/plain',$5,'pg',18)")
                    .bind(&tenant).bind(key.source_id()).bind(&key.path).bind(&key.content_hash).bind(uri).execute(store.pool()).await.unwrap();
                return;
            }
            if mode=="write" {
                retention::change(store.pool(),&tenant,&key.path,"deny-and-erase-pg-original").await.unwrap();
                harness::barrier("denial_committed");
                assert!(retention::cleanup_one(store.pool(),&tenant,&key.source_id()).await.unwrap());
                harness::barrier("cleanup_committed");
                std::fs::write(harness::dir().join("reply"), b"completed").unwrap();
                return;
            }
            assert!(guarded.get(&key).await.is_err());
            let completed=harness::setting("P08_PHASE")=="cleanup_committed" || (mode=="recover" && harness::setting("P08_CRASH")=="no");
            assert_eq!(raw.exists(&key).await.unwrap(),!completed);
            if mode=="recover" {
                retention::cleanup_one(store.pool(),&tenant,&key.source_id()).await.unwrap();
                assert!(!raw.exists(&key).await.unwrap());
            }
        }).await.expect("bounded source recovery fixture");
    });
}
