// SPDX-License-Identifier: Apache-2.0
//! Actual DTO/command/JSONB paths, qualified in two separate Cargo graphs.
use crate::config::{AuthMode, Config, DocIntelConfig, SourceStoreConfig, StoreKind};
use crate::state::AppState;
use munarium_api_types as dto;
use munarium_core::hierarchy::{CountBlock, EvidenceBlock, TableBlock};
use munarium_proto::mmp::v1 as pb;
use prost::Message;
use serde_json::{json, Value};

// Old JSONB data, inserted as literal SQL JSON rather than through the new
// decoder. The expected value is built independently of any JSON parser.
pub(crate) const PRE_FIX_JSON: &str = r#"[{"$serde_json::private::Number":"123"},{"$serde_json::private::Number":"ordinary text"},null]"#;

pub(crate) fn pre_fix_value() -> Value {
    Value::Array(vec![
        Value::Object(serde_json::Map::from_iter([(
            "$serde_json::private::Number".into(),
            Value::String("123".into()),
        )])),
        Value::Object(serde_json::Map::from_iter([(
            "$serde_json::private::Number".into(),
            Value::String("ordinary text".into()),
        )])),
        Value::Null,
    ])
}

pub(crate) fn values() -> Vec<Value> {
    // Build expected objects directly: parsing the expected JSON with the same
    // decoder as the subject would make the reserved-key control circular.
    let special = |text: &str| {
        Value::Object(serde_json::Map::from_iter([(
            "$serde_json::private::Number".into(),
            Value::String(text.into()),
        )]))
    };
    let mut nested = Value::Null;
    for _ in 0..24 {
        nested = json!([nested]);
    }
    vec![
        Value::Null,
        json!(true),
        json!(i64::MIN),
        json!(i64::MAX),
        json!(u64::MAX),
        json!(0.5),
        json!(-1.25),
        json!("12345678901234567890.123456789"),
        special("123"),
        special("ordinary text"),
        Value::Object(serde_json::Map::from_iter([(
            "$serde_json::private::RawValue".into(),
            Value::String("ordinary text".into()),
        )])),
        json!({"nested": [special("123"), special("not a number")], "ordinary": 7}),
        Value::Object(serde_json::Map::from_iter([
            ("$serde_json::private::Number".into(), json!("123")),
            ("ordinary".into(), json!(7)),
        ])),
        Value::Object(serde_json::Map::from_iter([
            (
                "$serde_json::private::Number".into(),
                json!("ordinary text"),
            ),
            ("ordinary".into(), json!(7)),
        ])),
        json!({"array": [null, false, {}, [], "東京"], "large": "x".repeat(16384)}),
        nested,
    ]
}

#[test]
fn json_feature_dto_and_native_grpc_bytes_preserve_values() {
    for expected in values() {
        // Null metadata is absence under the existing Option<Value> DTO contract.
        let body = serde_json::to_vec(&json!({"metadata": expected})).unwrap();
        let envelope = pb::ServerApiRequest {
            body: body.clone(),
            ..Default::default()
        };
        let decoded = pb::ServerApiRequest::decode(envelope.encode_to_vec().as_slice()).unwrap();
        assert_eq!(
            decoded.body, body,
            "native gRPC carries bytes, never Struct doubles"
        );
        for transport in [body, decoded.body] {
            let request: dto::CreateVersionRequest = serde_json::from_slice(&transport).unwrap();
            assert_eq!(
                request.metadata,
                if expected.is_null() {
                    None
                } else {
                    Some(expected.clone())
                }
            );
        }
    }
    for invalid in [r#"{"metadata":[}"#, r#"{"metadata":NaN}"#] {
        assert!(serde_json::from_str::<dto::CreateVersionRequest>(invalid).is_err());
    }
}

#[test]
fn json_feature_evidence_discriminators_and_decimal_strings() {
    for block in [
        EvidenceBlock::Count(CountBlock {
            value: i64::MAX,
            rows_covered: Some(1),
            rows_excluded: None,
            exclusion_reason: None,
            evidence_id: Some("fixture-count".into()),
        }),
        EvidenceBlock::CompleteTable(TableBlock {
            columns: vec!["decimal".into()],
            rows: vec![vec![Some("12345678901234567890.123456789".into())]],
            row_ids: vec!["row-1".into()],
            truncated: false,
            evidence_id: Some("fixture-table".into()),
        }),
    ] {
        let bytes = serde_json::to_vec(&block).unwrap();
        let restored: EvidenceBlock = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(restored, block);
        assert_eq!(
            restored.supports_completeness(),
            block.supports_completeness()
        );
    }
    assert!(serde_json::from_str::<EvidenceBlock>(r#"{"kind":"unknown"}"#).is_err());
}

pub(crate) async fn state() -> std::sync::Arc<AppState> {
    let database_url =
        std::env::var("MUNARIUM_TEST_DATABASE_URL").expect("required PostgreSQL qualification URL");
    AppState::new(Config {
        http_addr: "127.0.0.1:0".into(),
        grpc_addr: None,
        ops_addr: "127.0.0.1:0".into(),
        store: StoreKind::Postgres,
        database_url: Some(database_url),
        auth: AuthMode::Disabled,
        shutdown_grace_secs: 1,
        token_secret: None,
        token_ttl_secs: 3600,
        require_uid: false,
        interaction_body_max: 32768,
        token_revocation_check: false,
        matrix_base_url: None,
        matrix_admin_url: None,
        max_concurrency: 4,
        db_max_conns: 4,
        idempotency_ttl_secs: 86400,
        replica_count: 1,
        registry_ttl_secs: 15,
        session_idle_ttl_secs: 0,
        evidence_purge_interval_secs: 0,
        max_tokens: dto::MaxTokensBudgets::default(),
        instance_id: "json-qualification".into(),
        source_store: SourceStoreConfig::Pg,
        doc_intel: DocIntelConfig::None,
    })
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; tools/test-json-features.ps1 -Postgres"]
async fn json_feature_pg_ledger_reopens_from_validated_dto() {
    let state = state().await;
    let tenant = format!("json-{}", uuid::Uuid::new_v4());
    let store = state.store_for(&tenant).await.unwrap();
    let mut ids = Vec::new();
    for expected in values().into_iter().filter(|v| !v.is_null()) {
        let request: dto::CreateVersionRequest =
            serde_json::from_slice(&serde_json::to_vec(&json!({"metadata": expected})).unwrap())
                .unwrap();
        assert_eq!(
            request.metadata,
            Some(expected.clone()),
            "decode must not silently change a value before write"
        );
        let version = store.create_version(None, request.metadata).await.unwrap();
        let request: dto::ProposeClaimRequest = serde_json::from_slice(&serde_json::to_vec(&json!({"claim_type":"fact","subject":"fixture","key":"value","value":"documented","evidence":expected})).unwrap()).unwrap();
        let result = crate::service::append_events(
            store.as_ref(),
            &munarium_shapes::ShapeRegistry::default(),
            &tenant,
            &version,
            &[request],
            None,
            None,
            None,
        )
        .await
        .unwrap();
        ids.push((version, result.claims[0].id.clone(), expected));
    }
    sqlx::query("INSERT INTO memory_versions (tenant_id,id,lineage_root_id,metadata) VALUES ($1,'pre-fix','pre-fix',$2::jsonb)")
        .bind(&tenant).bind(PRE_FIX_JSON).execute(state.pg_pool().unwrap()).await.unwrap();
    drop(store);
    state.pg_pool().unwrap().close().await;
    drop(state);
    let reopened = self::state().await;
    let store = reopened.store_for(&tenant).await.unwrap();
    assert_eq!(
        store.version_metadata("pre-fix").await.unwrap(),
        Some(pre_fix_value())
    );
    for (version, claim, expected) in ids {
        assert_eq!(
            store.version_metadata(&version).await.unwrap(),
            Some(expected.clone())
        );
        assert_eq!(
            store.get_claim(&claim).await.unwrap().unwrap().evidence,
            Some(expected)
        );
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; tools/test-json-features.ps1 -Postgres"]
async fn json_feature_pg_session_writer_and_reader_reopen() {
    let state = state().await;
    let tenant = format!("json-session-{}", uuid::Uuid::new_v4());
    crate::runbooks_api::op_apply_shape(
        &state, &tenant,
        "apiVersion: munarium.ioka.io/v1\nkind: Shape\nmetadata: { name: article, version: 1 }\nspec:\n  fact:\n    schema: { type: object }\n",
        None, None,
    ).await.unwrap();
    let retrieval = state.retrieval_for(&tenant).unwrap();
    let collection = retrieval
        .ensure_collection("articles", "article@1", 0, &[], None)
        .await
        .unwrap();
    let text =
        r#"The fixture documents the literal {"$serde_json::private::Number":"ordinary text"}."#;
    let (source_id, _, _) = retrieval
        .put_source(
            "",
            "text/plain",
            "fixture/literal.txt",
            None,
            text.as_bytes(),
        )
        .await
        .unwrap();
    retrieval
        .bind_source(&collection.id, &source_id, None)
        .await
        .unwrap();
    retrieval
        .build_collection_index(&collection.id, 500, 1, true)
        .await
        .unwrap();
    let yaml = "apiVersion: munarium.ioka.io/v1\nkind: Runbook\nmetadata: { name: json-session, version: 1 }\nspec:\n  collections: [{ name: articles, shape: article@1 }]\n  steps: [{ buildIndex: {} }]\n";
    crate::runbooks_api::op_apply_runbook(&state, &tenant, yaml)
        .await
        .unwrap();
    let access = munarium_access::AccessCtx::unrestricted("fixture", &tenant);
    let session = crate::sessions_api::op_create_session(&state, &access, "json-session")
        .await
        .unwrap();
    let query = r#"What does {"$serde_json::private::Number":"ordinary text"} mean?"#;
    let request = serde_json::from_value(json!({"query":query,"complete":false})).unwrap();
    let (response, _) =
        crate::sessions_api::op_turn(&state, &access, &session.session_id, request, None)
            .await
            .unwrap();
    assert!(
        !response.hits.is_empty(),
        "the persistence test requires real evidence"
    );
    assert!(!response.envelopes.is_empty());
    assert_eq!(response.hits[0].source_id, source_id);
    sqlx::query("INSERT INTO session_turns (tenant_id,session_id,ordinal,uid,query,collections_searched,hits,envelope,completion) VALUES ($1,$2,2,'fixture','pre-fix','{}',$3::jsonb,$3::jsonb,$3::jsonb)")
        .bind(&tenant).bind(&session.session_id).bind(PRE_FIX_JSON)
        .execute(state.pg_pool().unwrap()).await.unwrap();
    state.pg_pool().unwrap().close().await;
    drop(state);
    let reopened = self::state().await;
    let saved = crate::sessions_api::op_get_session(&reopened, &tenant, &session.session_id)
        .await
        .unwrap();
    assert_eq!(saved.turns.len(), 2);
    assert_eq!(saved.turns[0].query, query);
    assert_eq!(
        saved.turns[0].hits,
        serde_json::to_value(response.hits).unwrap()
    );
    assert_eq!(
        saved.turns[0].envelope,
        serde_json::to_value(response.envelopes).unwrap()
    );
    assert!(saved.turns[0].completion.is_none());
    assert_eq!(saved.turns[1].hits, pre_fix_value());
    assert_eq!(saved.turns[1].envelope, pre_fix_value());
    assert_eq!(saved.turns[1].completion, Some(pre_fix_value()));
    let decoded: dto::SessionResponse =
        serde_json::from_slice(&serde_json::to_vec(&saved).unwrap()).unwrap();
    assert_eq!(decoded.turns[1].hits, pre_fix_value());
    assert_eq!(decoded.turns[1].envelope, pre_fix_value());
    assert_eq!(decoded.turns[1].completion, Some(pre_fix_value()));
}
