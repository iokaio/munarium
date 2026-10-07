// SPDX-License-Identifier: Apache-2.0
#[path = "../../munarium-store-mem/tests/support/authority.rs"]
mod support;
use munarium_core::platform_authority::*;
use munarium_store_pg::{platform_authority::PgAuthorityStore, PgStore};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires isolated MUNARIUM_TEST_DATABASE_URL; run with --ignored"]
async fn platform_authority_postgres_contract_and_restart() {
    let url = std::env::var("MUNARIUM_TEST_DATABASE_URL").expect("isolated database required");
    let tenant = format!("authority-{}", uuid::Uuid::new_v4());
    let pg = PgStore::connect(&url, &tenant).await.unwrap();
    let authority = PgAuthorityStore::new(pg.clone());
    support::contract(&authority, &tenant).await;
    let snapshot = authority.snapshot().await.unwrap();
    let reopened = PgAuthorityStore::new(PgStore::connect(&url, &tenant).await.unwrap());
    assert_eq!(reopened.snapshot().await.unwrap(), snapshot);
    assert_eq!(
        reopened.enroll(snapshot.config.clone()).await.unwrap(),
        snapshot
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM platform_authority_receipts WHERE tenant_id=$1")
            .bind(&tenant)
            .fetch_one(pg.pool())
            .await
            .unwrap();
    assert_eq!(count, 4);
    assert!(
        sqlx::query("DELETE FROM platform_authority_receipts WHERE tenant_id=$1")
            .bind(&tenant)
            .execute(pg.pool())
            .await
            .is_err()
    );
    assert!(sqlx::query("UPDATE platform_authority SET state=jsonb_set(state,'{bootstrap_retired}','false') WHERE tenant_id=$1")
        .bind(&tenant).execute(pg.pool()).await.is_err());
    let other = PgAuthorityStore::new(pg.with_tenant(&format!("other-{tenant}")).await.unwrap());
    assert!(other.snapshot().await.is_err());
    assert!(other.enroll(snapshot.config.clone()).await.is_err());
}
