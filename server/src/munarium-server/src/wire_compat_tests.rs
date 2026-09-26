// SPDX-License-Identifier: Apache-2.0
//! Integer boundaries use synthetic values; no production sequence mutation API.
use crate::config::{AuthMode, Config, DocIntelConfig, SourceStoreConfig, StoreKind};
use crate::state::AppState;
use munarium_api_types as dto;
use munarium_core::{budget::BudgetOutcome, KernelError};
use munarium_proto::mmp::v1 as pb;
use pb::command_service_server::CommandService;
use pb::query_service_server::QueryService;
use pb::server_api_service_server::ServerApiService;
use prost::Message;
use std::sync::Arc;
use tonic::{Code, Request};
use tonic_types::StatusExt;

async fn state() -> Arc<AppState> {
    AppState::new(Config {
        http_addr: "127.0.0.1:0".into(),
        grpc_addr: None,
        ops_addr: "127.0.0.1:0".into(),
        store: StoreKind::Memory,
        database_url: None,
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
        idempotency_ttl_secs: 0,
        replica_count: 1,
        registry_ttl_secs: 15,
        session_idle_ttl_secs: 0,
        evidence_purge_interval_secs: 0,
        managed_provider_diagnostics: false,
        max_tokens: dto::MaxTokensBudgets::default(),
        instance_id: "wire-compatibility".into(),
        source_store: SourceStoreConfig::Mem,
        doc_intel: DocIntelConfig::None,
    })
    .await
    .unwrap()
}

fn command<T>(body: T, key: &str) -> Request<T> {
    let mut request = Request::new(body);
    request
        .metadata_mut()
        .insert("idempotency-key", key.parse().unwrap());
    request
}

#[tokio::test]
async fn wire_digest_tier_rejects_narrowing_before_storage() {
    let state = state().await;
    let store = state.store_for("tenant-default").await.unwrap();
    let version_id = store.create_version(None, None).await.unwrap();
    let service = crate::grpc::CommandSvc { state };
    for tier in [256, 257, u32::MAX] {
        let response = service
            .upsert_digest(command(
                pb::UpsertDigestRequest {
                    digest: Some(pb::Digest {
                        version_id: version_id.clone(),
                        tier,
                        content: "fixture".into(),
                        ..Default::default()
                    }),
                },
                &format!("invalid-tier-{tier}"),
            ))
            .await;
        assert_eq!(response.unwrap_err().code(), Code::InvalidArgument);
        assert!(store.digests(&version_id).await.unwrap().is_empty());
    }
    for tier in [0, 255] {
        service
            .upsert_digest(command(
                pb::UpsertDigestRequest {
                    digest: Some(pb::Digest {
                        version_id: version_id.clone(),
                        tier,
                        content: "fixture".into(),
                        ..Default::default()
                    }),
                },
                &format!("valid-tier-{tier}"),
            ))
            .await
            .unwrap();
    }
    let stored = store.digests(&version_id).await.unwrap();
    assert_eq!(stored.len(), 2);
    assert!(stored.iter().any(|d| d.tier == 255));
}

#[tokio::test]
async fn wire_bulk_lengths_reject_signed_overflow_before_database_access() {
    let service = crate::grpc_api::ServerApiSvc::new(state().await);
    for bytes_len in [i64::MAX as u64 + 1, u64::MAX] {
        let error = service
            .bulk_open(command(
                pb::ServerApiRequest {
                    body: serde_json::to_vec(&serde_json::json!({"files":[{
                        "filename":"fixture.txt", "sha256":"a".repeat(64),
                        "bytes_len":bytes_len, "media_type":"text/plain"
                    }]}))
                    .unwrap(),
                    ..Default::default()
                },
                &format!("bulk-overflow-{bytes_len}"),
            ))
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::InvalidArgument, "{bytes_len}");
        assert!(error.message().contains("bytes_len"), "{error}");
        assert_eq!(
            error.get_error_details().error_info().unwrap().reason,
            "invalid-input"
        );
    }
}

#[tokio::test]
async fn wire_budget_report_rejects_signed_overflow() {
    let state = state().await;
    for amount in [i64::MAX as u64 + 1, u64::MAX] {
        let tenant = format!("report-{amount}");
        state
            .budgets()
            .reserve(&tenant, "fixture", "fast", amount, Some(u64::MAX))
            .await
            .unwrap();
        assert!(matches!(
            crate::reports_api::op_budgets(&state, &tenant).await,
            Err(KernelError::Storage(_))
        ));
    }
}

#[tokio::test]
async fn wire_budget_report_rejects_combined_overflow() {
    let state = state().await;
    let tenant = "combined-report";
    let first = state
        .budgets()
        .reserve(tenant, "fixture", "fast", 1, Some(u64::MAX))
        .await
        .unwrap();
    state
        .budgets()
        .reserve(tenant, "fixture", "fast", 1, Some(u64::MAX))
        .await
        .unwrap();
    let BudgetOutcome::Granted(first) = first else {
        panic!("fixture reservation was not granted")
    };
    state
        .budgets()
        .settle(&first, Some(u64::MAX))
        .await
        .unwrap();
    assert!(matches!(
        crate::reports_api::op_budgets(&state, tenant).await,
        Err(KernelError::Storage(_))
    ));
}

#[tokio::test]
async fn wire_numeric_rest_native_and_typed_grpc_replay_and_pins_are_exact() {
    use axum::{body::Body, http};
    use tower::ServiceExt;

    let state = state().await;
    let store = state.store_for("tenant-default").await.unwrap();
    let native = crate::grpc_api::ServerApiSvc::new(state.clone());
    let typed_command = crate::grpc::CommandSvc {
        state: state.clone(),
    };
    let typed_query = crate::grpc::QuerySvc {
        state: state.clone(),
    };
    for value in [
        (1u64 << 53) - 1,
        1u64 << 53,
        (1u64 << 53) + 1,
        i64::MAX as u64,
        u64::MAX,
    ] {
        let version = store.create_version(None, None).await.unwrap();
        let input = pb::ServerApiRequest {
            path_parameters: [("version_id".into(), version.clone())].into(),
            body: format!(
                r#"{{"key":"native","scope_path":"fixture","count":{value},"future_field":true}}"#
            )
            .into_bytes(),
            ..Default::default()
        };
        // Simulate an added protobuf field from a newer peer. Prost discards
        // the unknown envelope field while preserving JSON payload bytes.
        let mut encoded = input.encode_to_vec();
        encoded.extend_from_slice(&[0xa0, 0x06, 0x01]); // field 100, varint 1
        let decoded = pb::ServerApiRequest::decode(encoded.as_slice()).unwrap();
        assert_eq!(decoded.body, input.body);
        let first = native
            .record_counts(command(decoded.clone(), &format!("native-{value}")))
            .await
            .unwrap()
            .into_inner();
        let replay = native
            .record_counts(command(decoded, &format!("native-{value}")))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(first.body, replay.body);
        assert_eq!(store.head(&version).await.unwrap(), 1);

        let request = pb::RecordCountsRequest {
            version_id: version.clone(),
            key: "typed".into(),
            scope_path: "fixture".into(),
            count: value,
            budget: 0,
        };
        for _ in 0..2 {
            typed_command
                .record_counts(command(request.clone(), &format!("typed-{value}")))
                .await
                .unwrap();
        }
        assert_eq!(store.head(&version).await.unwrap(), 2);
        let counters = typed_query
            .counter_totals(Request::new(pb::CounterTotalsRequest {
                version_id: version.clone(),
                as_of_seq: 1,
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(counters.counters.len(), 1);
        assert_eq!(counters.counters[0].total, value);
        let response = crate::rest::router(state.clone())
            .oneshot(
                http::Request::builder()
                    .uri(format!("/v1/versions/{version}/counters?as_of_seq=1"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), http::StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let counters: dto::CountersResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(counters.counters.len(), 1);
        assert_eq!(counters.counters[0].total, value);

        let error = native.propose_claim(command(pb::ServerApiRequest {
            path_parameters: [("version_id".into(), version.clone())].into(),
            body: format!(r#"{{"expected_head":{value},"claim_type":"fact","subject":"fixture","key":"key","value":"value"}}"#).into_bytes(),
            ..Default::default()
        }, &format!("conflict-{value}"))).await.unwrap_err();
        assert_eq!(error.code(), Code::Aborted);
        let details = error.get_error_details();
        let info = details.error_info().unwrap();
        assert_eq!(info.reason, "head-conflict");
        assert_eq!(info.metadata["expected"], value.to_string());
        assert_eq!(info.metadata["actual"], "2");

        let facts = native
            .get_facts(Request::new(pb::ServerApiRequest {
                path_parameters: [("version_id".into(), version.clone())].into(),
                query_parameters: vec![
                    pb::ServerApiParameter {
                        name: "as_of_seq".into(),
                        value: value.to_string(),
                    },
                    pb::ServerApiParameter {
                        name: "limit".into(),
                        value: "1".into(),
                    },
                ],
                ..Default::default()
            }))
            .await
            .unwrap()
            .into_inner();
        let facts: dto::FactsResponse = serde_json::from_slice(&facts.body).unwrap();
        assert_eq!(facts.as_of_seq, value);
        assert_eq!(facts.head_seq, 2);
    }
}

#[tokio::test]
async fn wire_budget_report_retains_exact_supported_values() {
    let state = state().await;
    for amount in [
        (1u64 << 53) - 1,
        1u64 << 53,
        (1u64 << 53) + 1,
        i64::MAX as u64,
    ] {
        let tenant = format!("exact-report-{amount}");
        let BudgetOutcome::Granted(reservation) = state
            .budgets()
            .reserve(&tenant, "fixture", "fast", amount, Some(amount))
            .await
            .unwrap()
        else {
            panic!("fixture reservation was not granted")
        };
        let held = crate::reports_api::op_budgets(&state, &tenant)
            .await
            .unwrap();
        assert_eq!(held[0].held_tokens, i64::try_from(amount).unwrap());
        state.budgets().settle(&reservation, None).await.unwrap();
        let settled = crate::reports_api::op_budgets(&state, &tenant)
            .await
            .unwrap();
        assert_eq!(settled[0].settled_tokens, i64::try_from(amount).unwrap());
        assert_eq!(settled[0].held_tokens, 0);
    }
}

#[tokio::test]
async fn wire_budget_report_validates_configured_limits_without_traffic() {
    let state = state().await;
    for amount in [
        (1u64 << 53) + 1,
        i64::MAX as u64,
        i64::MAX as u64 + 1,
        u64::MAX,
    ] {
        let tenant = format!("configured-limit-{amount}");
        let yaml = format!("apiVersion: munarium.ioka.io/v1\nkind: ProviderConfig\nmetadata: {{ name: fixture }}\nspec:\n  provider: ollama\n  endpoint: http://127.0.0.1:1\n  models: {{ fast: fixture }}\n  budgets:\n    dailyTokens: {{ fast: {amount} }}\n");
        state.providers.apply(&state, &tenant, &yaml).await.unwrap();
        let result = crate::reports_api::op_budgets(&state, &tenant).await;
        if let Ok(expected) = i64::try_from(amount) {
            let rows = result.unwrap();
            assert_eq!(rows[0].limit, Some(expected));
            assert_eq!(rows[0].remaining, Some(expected));
        } else {
            assert!(matches!(result, Err(KernelError::Storage(_))));
        }
    }
}

#[tokio::test]
async fn wire_approval_ordinal_rejects_narrowing_before_database_access() {
    let state = state().await;
    for ordinal in [i32::MAX as usize + 1, u32::MAX as usize, usize::MAX] {
        let result =
            crate::runbooks_api::op_approve_step(&state, "fixture", "missing", ordinal).await;
        assert!(
            matches!(result, Err(KernelError::InvalidInput(ref detail)) if detail.contains("ordinal")),
            "{result:?}"
        );
    }
}

#[cfg(target_pointer_width = "64")]
#[tokio::test]
async fn wire_evidence_pagination_rejects_signed_overflow_before_reading() {
    let state = state().await;
    let access = munarium_access::AccessCtx {
        uid: "fixture".into(),
        tenant_id: "fixture".into(),
        level: 0,
        compartments: vec![],
        all_compartments: true,
        scopes: vec![],
        runbooks: None,
        jti: String::new(),
    };
    for from in [i64::MAX as usize + 1, usize::MAX] {
        let result = crate::evidence_api::op_get_rows(&state, &access, "missing", from, 1).await;
        assert!(
            matches!(result, Err(crate::error::ApiError::Mesh(KernelError::InvalidInput(ref detail))) if detail.contains("from")),
            "{result:?}"
        );
    }
}
