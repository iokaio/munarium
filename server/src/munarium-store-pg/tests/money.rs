// SPDX-License-Identifier: Apache-2.0
//! Requires an isolated MUNARIUM_TEST_DATABASE_URL; unavailable without it.
use chrono::{Duration, Utc};
use munarium_core::money::*;
use munarium_store_pg::{
    money::{MoneyStore, Observation},
    PgStore,
};
use std::collections::BTreeMap;

fn price(id: &str, route: &str, currency: &str) -> PriceSnapshot {
    PriceSnapshot {
        id: id.into(),
        provider: "fictional".into(),
        route: route.into(),
        model: "fixture".into(),
        currency: currency.into(),
        valid_from: Utc::now() - Duration::days(1),
        valid_until: Utc::now() + Duration::days(1),
        basis: Basis::Inclusive,
        rates: BTreeMap::from([
            (
                "input".into(),
                Rate {
                    micro_units: 1,
                    per_tokens: 3,
                },
            ),
            (
                "output".into(),
                Rate {
                    micro_units: 0,
                    per_tokens: 1,
                },
            ),
        ]),
    }
}
fn observation(id: &str) -> Observation {
    Observation {
        id: format!("{id}-observation"),
        attempt_id: id.into(),
        previous_revision: 0,
        usage: MoneyUsage {
            input: Some(4),
            output: Some(2),
            ..Default::default()
        },
        accounted_units: 6,
        resolved: true,
        evidence_ref: "fictional-receipt".into(),
        price_id: None,
    }
}

#[tokio::test]
async fn money_durable_pins_idempotency_reconciliation_and_currency_coverage() {
    let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
        eprintln!("unavailable: isolated PostgreSQL not configured");
        return;
    };
    let tenant = format!("money-{}", uuid::Uuid::new_v4());
    let pg = PgStore::connect(&url, &tenant).await.unwrap();
    let store = MoneyStore(pg.pool().clone());
    let usd = price("fictional-usd", &"a".repeat(64), "USD");
    let eur = price("fictional-eur", &"b".repeat(64), "EUR");
    store.add_price(&tenant, &usd).await.unwrap();
    store.add_price(&tenant, &usd).await.unwrap();
    let mut collision = usd.clone();
    collision.currency = "EUR".into();
    assert!(store.add_price(&tenant, &collision).await.is_err());
    collision.id = "overlapping".into();
    assert!(store.add_price(&tenant, &collision).await.is_err());
    store.add_price(&tenant, &eur).await.unwrap();
    for tariff in [&usd, &eur] {
        let id = store
            .begin(
                &tenant,
                "invocation",
                "fictional",
                &tariff.route,
                "fixture",
                100,
            )
            .await
            .unwrap();
        let obs = observation(&id);
        let (a, b) = tokio::join!(store.observe(&tenant, &obs), store.observe(&tenant, &obs));
        assert_eq!(a.unwrap(), b.unwrap());
        let mut correction = obs.clone();
        correction.id.push_str("-correction");
        correction.previous_revision = 1;
        correction.usage.input = Some(7);
        correction.accounted_units = 9;
        let calc = store.observe(&tenant, &correction).await.unwrap();
        assert_eq!(calc.micro_units.as_deref(), Some("3"));
        assert_eq!(calc.price_id.as_ref(), Some(&tariff.id));
        assert!(store
            .observe(
                &tenant,
                &Observation {
                    id: "stale".into(),
                    ..obs.clone()
                }
            )
            .await
            .is_err());
        assert!(store.observe("other-tenant", &obs).await.is_err());
        assert!(store
            .observe(
                &tenant,
                &Observation {
                    accounted_units: 99,
                    ..obs
                }
            )
            .await
            .is_err());
    }
    let unpriced = store
        .begin(
            &tenant,
            "invocation",
            "fictional",
            &"c".repeat(64),
            "fixture",
            100,
        )
        .await
        .unwrap();
    store
        .observe(&tenant, &observation(&unpriced))
        .await
        .unwrap();
    let unknown = store
        .begin(
            &tenant,
            "invocation",
            "fictional",
            &usd.route,
            "fixture",
            100,
        )
        .await
        .unwrap();
    // Simulated process loss: no observation. Estimate remains unresolved.
    let from = Utc::now() - Duration::days(1);
    let to = Utc::now() + Duration::days(1);
    let report = store.report(&tenant, from, to).await.unwrap();
    assert_eq!(report.coverage.attempts, 4);
    assert_eq!(report.coverage.priced, 2);
    assert_eq!(report.coverage.missing_price, 1);
    assert_eq!(report.coverage.missing_usage, 1);
    assert_eq!(report.coverage.unresolved, 1);
    assert_eq!(
        report.known_subtotals_micro_units,
        BTreeMap::from([("USD".into(), "3".into()), ("EUR".into(), "3".into())])
    );
    assert_eq!(report.accounted_units, "124");
    assert!(!report.entire_tenant_bill);
    assert_eq!(
        store
            .report("other-tenant", from, to)
            .await
            .unwrap()
            .coverage
            .attempts,
        0
    );
    // A later catalog never retroactively prices unknown work.
    let later = price("late-applicability", &"c".repeat(64), "USD");
    store.add_price(&tenant, &later).await.unwrap();
    assert_eq!(
        store
            .report(&tenant, from, to)
            .await
            .unwrap()
            .coverage
            .missing_price,
        1
    );
    let mut late = observation(&unpriced);
    late.id = "late-confirmation".into();
    late.previous_revision = 1;
    late.price_id = Some(later.id.clone());
    store.observe(&tenant, &late).await.unwrap();
    late.id = "reprice-refused".into();
    late.previous_revision = 2;
    late.price_id = Some(usd.id.clone());
    assert!(store.observe(&tenant, &late).await.is_err());
    // Reopen from another connection: durable rows, original observations intact.
    let reopened = MoneyStore(sqlx::PgPool::connect(&url).await.unwrap());
    assert_eq!(
        reopened
            .report(&tenant, from, to)
            .await
            .unwrap()
            .coverage
            .priced,
        3
    );
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM monetary_observations WHERE tenant_id=$1")
            .bind(&tenant)
            .fetch_one(pg.pool())
            .await
            .unwrap();
    assert_eq!(n, 6);
    assert!(
        sqlx::query("DELETE FROM monetary_attempts WHERE tenant_id=$1 AND id=$2")
            .bind(&tenant)
            .bind(&unknown)
            .execute(pg.pool())
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE monetary_prices SET snapshot=snapshot WHERE tenant_id=$1")
            .bind(&tenant)
            .execute(pg.pool())
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM monetary_observations WHERE tenant_id=$1")
            .bind(&tenant)
            .execute(pg.pool())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn money_late_usage_uses_submission_time_and_expired_prices_stay_unknown() {
    let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
        eprintln!("unavailable: isolated PostgreSQL not configured");
        return;
    };
    let tenant = format!("money-expiry-{}", uuid::Uuid::new_v4());
    let pg = PgStore::connect(&url, &tenant).await.unwrap();
    let store = MoneyStore(pg.pool().clone());
    let mut tariff = price("expired", &"a".repeat(64), "USD");
    tariff.valid_until = Utc::now() - Duration::hours(1);
    store.add_price(&tenant, &tariff).await.unwrap();
    // Fixture represents work submitted while the now-expired tariff was valid.
    let submitted = tariff.valid_until - Duration::seconds(1);
    sqlx::query("INSERT INTO monetary_attempts (tenant_id,id,invocation_id,provider,route,model,submitted_at,accounted_units,price_id) VALUES ($1,'historical','invocation','fictional',$2,'fixture',$3,100,$4)")
        .bind(&tenant).bind(&tariff.route).bind(submitted).bind(&tariff.id).execute(pg.pool()).await.unwrap();
    assert_eq!(
        store
            .observe(&tenant, &observation("historical"))
            .await
            .unwrap()
            .micro_units
            .as_deref(),
        Some("2")
    );
    let now = store
        .begin(
            &tenant,
            "invocation",
            "fictional",
            &tariff.route,
            "fixture",
            100,
        )
        .await
        .unwrap();
    let mut obs = observation(&now);
    obs.price_id = Some(tariff.id);
    assert!(store.observe(&tenant, &obs).await.is_err());
    obs.price_id = None;
    assert!(store.observe(&tenant, &obs).await.unwrap().missing_price);
}

#[tokio::test]
async fn money_upgrade_preserves_legacy_records_and_report_boundaries() {
    let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
        eprintln!("unavailable: isolated PostgreSQL not configured");
        return;
    };
    let admin = sqlx::PgPool::connect(&url).await.unwrap();
    // Unique test-owned schema; never reset the supplied database's schema.
    let schema = format!("money_upgrade_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let search = format!("SET search_path TO {schema},public");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .after_connect(move |c, _| {
            let sql = search.clone();
            Box::pin(async move {
                sqlx::query(&sql).execute(c).await?;
                Ok(())
            })
        })
        .connect(&url)
        .await
        .unwrap();
    let migrations = sqlx::migrate!("./migrations");
    let old = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            migrations
                .iter()
                .filter(|m| m.version < 37)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    old.run(&pool).await.unwrap();
    sqlx::query("INSERT INTO tenants (id,slug) VALUES ('legacy','legacy')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO token_budget_reservations (id,tenant_id,config_name,tier,day,units,state) VALUES ('legacy','legacy','fictional','fast',current_date,42,'held')").execute(&pool).await.unwrap();
    migrations.run(&pool).await.unwrap();
    migrations.run(&pool).await.unwrap(); // checksum validation remains enabled
    let (units, evidence): (i64, Option<serde_json::Value>) = sqlx::query_as(
        "SELECT units,usage_evidence FROM token_budget_reservations WHERE id='legacy'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(units, 42);
    assert!(evidence.is_none());
    let store = MoneyStore(pool.clone());
    let from = Utc::now() - Duration::hours(1);
    let to = Utc::now() + Duration::hours(1);
    let legacy = store.report("legacy", from, to).await.unwrap();
    assert_eq!(legacy.coverage.attempts, 0);
    assert!(legacy.unrecorded_attempts.is_none());
    assert!(!legacy.entire_tenant_bill);
    assert!(store.report("legacy", to, from).await.is_err());
    // The report must never silently drop attempt 10,001.
    sqlx::query("INSERT INTO monetary_attempts (tenant_id,id,invocation_id,provider,route,model,accounted_units) SELECT 'legacy',n::text,'fixture','fictional','unpriceable-route','fixture',1 FROM generate_series(1,10000) n").execute(&pool).await.unwrap();
    assert_eq!(
        store
            .report("legacy", from, to)
            .await
            .unwrap()
            .coverage
            .attempts,
        10000
    );
    store
        .begin(
            "legacy",
            "fixture",
            "fictional",
            "unpriceable-route",
            "fixture",
            1,
        )
        .await
        .unwrap();
    assert!(store.report("legacy", from, to).await.is_err());
    pool.close().await;
    // Only the schema created above belongs to this test.
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
}
