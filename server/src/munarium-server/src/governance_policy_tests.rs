// SPDX-License-Identifier: Apache-2.0
use munarium_core::{
    governance::GovernancePolicy,
    ledger::FactQuery,
    storage::{FindingsQuery, NewClaim, StorageBackend},
    types::*,
};
use serde_json::{json, Value};

fn exact() -> Value {
    json!({"schema_version":1,"values":[{"subject":"file","key":"path","comparison":"exact-string-v1"}]})
}
fn request(value: &str) -> munarium_api_types::ProposeClaimRequest {
    serde_json::from_value(json!({"claim_type":"fact","subject":"file","key":"path","value":value}))
        .unwrap()
}
async fn propose(
    store: &dyn StorageBackend,
    version: &str,
    req: munarium_api_types::ProposeClaimRequest,
) -> crate::service::CommandOutcome {
    crate::service::append_events(
        store,
        &munarium_shapes::ShapeRegistry::default(),
        "test",
        version,
        &[req],
        None,
        None,
        None,
    )
    .await
    .unwrap()
}
async fn contract(store: &dyn StorageBackend) -> String {
    let root = store.create_version(None, None).await.unwrap();
    let first = propose(store, &root, request("/Docs/Readme"))
        .await
        .claims
        .remove(0);
    assert_eq!(
        propose(store, &root, request("/docs/readme")).await.claims[0].status,
        ClaimStatus::Accepted
    );
    store
        .lock_anchor(&root, "file", "path", "/Docs/Readme", None, None)
        .await
        .unwrap();
    let pin = store.head(&root).await.unwrap();
    let meta = json!({"governance_policy": exact(), "governance_transition": {
        "from_revision": GovernancePolicy::default().revision().unwrap(), "expected_head":pin,"reason":"Paths are case-sensitive"}});
    assert!(store
        .create_version(Some(&root), Some(json!({"governance_policy":exact()})))
        .await
        .is_err());
    let mut stale = meta.clone();
    stale["governance_transition"]["expected_head"] = json!(0);
    assert!(matches!(
        store.create_version(Some(&root), Some(stale)).await,
        Err(munarium_core::KernelError::HeadConflict { .. })
    ));
    let child = store.create_version(Some(&root), Some(meta)).await.unwrap();
    let metadata = store.version_metadata(&child).await.unwrap().unwrap();
    let assessment = &metadata["governance_assessment"];
    assert_eq!(assessment["as_of_seq"], pin);
    assert_eq!(assessment["claims"].as_array().unwrap().len(), 2);
    assert!(assessment["claims"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| !c["findings"].as_array().unwrap().is_empty()));
    assert_eq!(
        store.get_claim(&first.id).await.unwrap().unwrap().status,
        ClaimStatus::Accepted
    );
    let bad = propose(store, &child, request("/docs/readme")).await;
    assert_eq!(bad.claims[0].status, ClaimStatus::Disputed);
    assert_eq!(bad.findings.len(), 1); // anchor/ledger twins still deduplicate
    let mut correction = request("/docs/readme");
    correction.claim_type = munarium_api_types::ClaimTypeDto::Correction;
    correction.supersedes_id = Some(first.id.clone());
    assert_eq!(
        propose(store, &child, correction).await.claims[0].status,
        ClaimStatus::Disputed
    );
    let inherited = store.create_version(Some(&child), None).await.unwrap();
    assert_eq!(
        store.version_metadata(&inherited).await.unwrap().unwrap()["governance_revision"],
        metadata["governance_revision"]
    );
    assert_eq!(
        propose(store, &inherited, request(" /Docs/Readme "))
            .await
            .claims[0]
            .status,
        ClaimStatus::Disputed
    );
    let rows = store
        .findings(
            &inherited,
            &FindingsQuery {
                rule_prefix: Some("governance.".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(rows
        .iter()
        .any(|r| r.finding.rule_id == "governance.policy-revision"));
    assert!(rows
        .iter()
        .any(|r| r.finding.rule_id == "governance.command-evaluation"));
    let pinned = store
        .slice_facts(
            &child,
            &FactQuery {
                as_of_seq: Some(pin),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(pinned.len(), 2);
    assert!(store
        .create_version(
            None,
            Some(json!({"governance_policy":{"schema_version":99,"values":[]}}))
        )
        .await
        .is_err());
    // Failed append must not file an assessment without its claims.
    let before = store
        .findings(&child, &FindingsQuery::default())
        .await
        .unwrap()
        .len();
    assert!(store
        .append_evaluated_claims(
            &child,
            vec![NewClaim::fact("file", "path", "x")],
            0,
            &[GateFinding {
                rule_id: "test.atomic".into(),
                severity: Severity::Info,
                message: "must not persist".into(),
                scope_path: None,
                detail: None
            }]
        )
        .await
        .is_err());
    assert_eq!(
        before,
        store
            .findings(&child, &FindingsQuery::default())
            .await
            .unwrap()
            .len()
    );
    // Exact strings preserve leading/trailing whitespace even in durable storage.
    let fresh = store
        .create_version(None, Some(json!({"governance_policy":exact()})))
        .await
        .unwrap();
    let value = "  /Docs/Readme\t";
    let claim = propose(store, &fresh, request(value))
        .await
        .claims
        .remove(0);
    assert_eq!(
        store.get_claim(&claim.id).await.unwrap().unwrap().value,
        value
    );
    assert_eq!(
        propose(store, &fresh, request("/Docs/Readme")).await.claims[0].status,
        ClaimStatus::Disputed
    );
    child
}
#[tokio::test]
async fn governance_policy_memory_contract() {
    contract(&munarium_store_mem::MemStore::new()).await;
}

#[tokio::test]
async fn governance_policy_postgres_contract() {
    let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
        eprintln!("SKIP governance PostgreSQL: MUNARIUM_TEST_DATABASE_URL absent");
        return;
    };
    let tenant = format!("p09-{}", uuid::Uuid::new_v4().simple());
    let store = munarium_store_pg::PgStore::connect(&url, &tenant)
        .await
        .unwrap();
    let child = contract(&store).await;
    let head_before = store.head(&child).await.unwrap();
    assert!(store
        .append_evaluated_claims(
            &child,
            vec![NewClaim::fact("other", "key", "v")],
            head_before,
            &[GateFinding {
                rule_id: "test.reject".into(),
                severity: Severity::Info,
                message: "invalid postgres text\0".into(),
                scope_path: None,
                detail: None
            }]
        )
        .await
        .is_err());
    assert_eq!(
        store.head(&child).await.unwrap(),
        head_before,
        "receipt failure rolls back claims and events"
    );
    let metadata = store.version_metadata(&child).await.unwrap();
    let restarted = munarium_store_pg::PgStore::connect(&url, &tenant)
        .await
        .unwrap();
    assert_eq!(restarted.version_metadata(&child).await.unwrap(), metadata);
    assert_eq!(
        propose(&restarted, &child, request("/docs/readme"))
            .await
            .claims[0]
            .status,
        ClaimStatus::Disputed
    );
    let other = store.with_tenant(&format!("other-{tenant}")).await.unwrap();
    assert!(other.version_metadata(&child).await.is_err());
    // An old binary has no transaction-local writer marker.
    assert!(sqlx::query("INSERT INTO anchors (tenant_id,id,version_id,detail_key,locked_value,status,seq) VALUES ($1,'old-writer',$2,'file.path','x','locked',999)")
        .bind(&tenant).bind(&child).execute(store.pool()).await.is_err());
    assert!(
        sqlx::query("UPDATE memory_versions SET metadata = '{}' WHERE tenant_id=$1 AND id=$2")
            .bind(&tenant)
            .bind(&child)
            .execute(store.pool())
            .await
            .is_err()
    );
    let row: (String,Value) = sqlx::query_as("SELECT c.governance_revision,e.body FROM claims c JOIN ledger_events e ON e.tenant_id=c.tenant_id AND e.version_id=c.version_id AND e.seq=c.seq WHERE c.tenant_id=$1 AND c.version_id=$2 ORDER BY c.seq LIMIT 1")
        .bind(&tenant).bind(&child).fetch_one(store.pool()).await.unwrap();
    assert_eq!(row.1["governance_revision"], row.0);
    assert_eq!(row.1["value"], "/docs/readme");
    // Simulate a future writer, rather than asking the current API to accept it.
    let mut tx = store.pool().begin().await.unwrap();
    sqlx::query("SELECT set_config('munarium.governance_writer','1',true)")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("INSERT INTO memory_versions (tenant_id,id,lineage_root_id,metadata) VALUES ($1,'future','future',$2)")
        .bind(&tenant).bind(json!({"governance_policy":{"schema_version":2,"values":[]},"governance_revision":"governance-v2:future"}))
        .execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO claims (tenant_id,id,version_id,seq,claim_type,subject,key,value,status,provenance) VALUES ($1,'future-claim','future',1,'fact','file','path','x','accepted','witnessed')")
        .bind(&tenant).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    assert!(store
        .slice_facts("future", &FactQuery::default())
        .await
        .is_err());
    assert!(store.get_claim("future-claim").await.is_err());
    assert!(store.anchors("future", None).await.is_err());
}
