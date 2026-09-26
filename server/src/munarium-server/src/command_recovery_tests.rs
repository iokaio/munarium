// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::crash_recovery::{self, harness};
use axum::{
    body::{to_bytes, Body},
    http::Request,
};
use munarium_proto::mmp::v1::{
    self as pb, command_service_server::CommandService, server_api_service_server::ServerApiService,
};
use serde_json::{json, Value};
use tonic_types::StatusExt;
use tower::ServiceExt;

fn native(token: &str, body: Value, key: &str) -> tonic::Request<pb::ServerApiRequest> {
    let mut request = tonic::Request::new(pb::ServerApiRequest {
        body: serde_json::to_vec(&body).unwrap(),
        query_parameters: vec![pb::ServerApiParameter {
            name: "key".into(),
            value: key.into(),
        }],
        ..Default::default()
    });
    request
        .metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    request
        .metadata_mut()
        .insert("idempotency-key", key.parse().unwrap());
    request
}

#[test]
fn guarded_command_process_recovery() {
    if !crash_recovery::available() {
        return;
    }
    for plane in ["rest", "grpc"] {
        for phase in ["command_claimed", "command_completed", "receipt_persisted"] {
            for crash in [false, true] {
                harness::run(
                    "command_recovery::tests::guarded_child",
                    plane,
                    phase,
                    crash,
                    &format!("p08-guarded-{}", uuid::Uuid::new_v4().simple()),
                );
            }
        }
    }
}

#[test]
#[ignore = "owned child fixture"]
fn guarded_child() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        tokio::time::timeout(harness::LIMIT, guarded_child_async())
            .await
            .expect("bounded guarded command fixture");
    });
}

async fn guarded_child_async() {
    let tenant = harness::setting("P08_TENANT");
    let state = crash_recovery::state(&tenant).await;
    let mode = harness::setting("P08_MODE");
    let plane = harness::setting("P08_CASE");
    let pool = state.pg_pool().unwrap();
    if mode == "setup" {
        sqlx::query("INSERT INTO command_recovery_policies(tenant_id) VALUES ($1)")
            .bind(&tenant)
            .execute(pool)
            .await
            .unwrap();
        return;
    }
    let (url, task) = crash_recovery::endpoint(state.clone(), &plane).await;
    if mode == "write" {
        let response = crash_recovery::command(&url, &plane, false).await.unwrap();
        std::fs::write(harness::dir().join("reply"), response).unwrap();
        task.abort();
        return;
    }
    let completed = harness::setting("P08_PHASE") == "receipt_persisted"
        || (mode == "recover" && harness::setting("P08_CRASH") == "no");
    let expected_effects =
        i64::from(harness::setting("P08_PHASE") != "command_claimed" || completed);
    let row = sqlx::query(
        "SELECT state,response_body FROM command_receipts WHERE tenant_id=$1 AND key='p08-command'",
    )
    .bind(&tenant)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(
        row.get::<String, _>("state"),
        if completed { "completed" } else { "unresolved" }
    );
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM memory_versions WHERE tenant_id=$1")
        .bind(&tenant)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(before, expected_effects);
    let result = crash_recovery::command(&url, &plane, false).await;
    if completed {
        assert_eq!(
            result.unwrap(),
            std::fs::read_to_string(harness::dir().join("original-response")).unwrap()
        );
    } else {
        assert!(result
            .unwrap_err()
            .contains("in progress or may have executed"));
    }
    assert!(crash_recovery::command(&url, &plane, true)
        .await
        .unwrap_err()
        .contains("idempotency"));
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM memory_versions WHERE tenant_id=$1")
        .bind(&tenant)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(
        after, before,
        "neither a live owner nor a crashed owner permits duplicate execution"
    );
    task.abort();
}

async fn call(
    router: &axum::Router,
    method: &str,
    path: &str,
    token: &str,
    key: &str,
    body: Value,
) -> (u16, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .header("idempotency-key", key)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn guarded_policy_authority_legacy_replay_scope_and_two_pool_race() {
    let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
        eprintln!("unavailable: isolated PostgreSQL required");
        return;
    };
    let tenant = format!("guarded-{}", uuid::Uuid::new_v4());
    let auth = crate::config::AuthMode::Static(vec![
        ("admin".into(), tenant.clone(), "mgmt".into()),
        ("writer".into(), tenant.clone(), "rw".into()),
        ("reader".into(), tenant.clone(), "ro".into()),
        ("other".into(), format!("other-{tenant}"), "mgmt".into()),
    ]);
    let first =
        crate::providers_api::usage_tests::test_state_with_auth(Some(url.clone()), auth.clone())
            .await;
    let second = crate::providers_api::usage_tests::test_state_with_auth(Some(url), auth).await;
    // Admit the entire race at the HTTP boundary; test the database claim,
    // rather than the fixture's unrelated four-request overload limit.
    first.rest_permits.add_permits(10);
    second.rest_permits.add_permits(10);
    let router = crate::rest::router(first.clone());
    let other_router = crate::rest::router(second.clone());
    let rpc = crate::grpc_api::ServerApiSvc::new(first.clone());
    for role in ["reader", "writer"] {
        assert_eq!(
            rpc.enable_command_recovery(native(role, json!({"mode":"guarded-v1"}), "unused"))
                .await
                .unwrap_err()
                .code(),
            tonic::Code::PermissionDenied
        );
        assert_eq!(
            rpc.command_recovery_policy(native(role, Value::Null, "unused"))
                .await
                .unwrap_err()
                .code(),
            tonic::Code::PermissionDenied
        );
    }
    for role in ["reader", "writer"] {
        assert_eq!(
            call(
                &router,
                "POST",
                "/v1/command-recovery",
                role,
                "unused",
                json!({"mode":"guarded-v1"})
            )
            .await
            .0,
            403
        );
    }
    // A successful old-format receipt remains replayable after activation.
    let legacy = call(
        &router,
        "POST",
        "/v1/versions",
        "writer",
        "legacy",
        json!({}),
    )
    .await;
    assert_eq!(legacy.0, 200);
    assert_eq!(
        call(
            &router,
            "POST",
            "/v1/command-recovery",
            "admin",
            "unused",
            json!({"mode":"guarded-v1"})
        )
        .await
        .0,
        200
    );
    assert_eq!(
        call(
            &other_router,
            "POST",
            "/v1/versions",
            "writer",
            "legacy",
            json!({})
        )
        .await,
        legacy
    );
    assert_eq!(
        call(
            &router,
            "POST",
            "/v1/command-recovery",
            "admin",
            "unused",
            json!({"mode":"legacy"})
        )
        .await
        .0,
        400
    );
    let mut tasks = Vec::new();
    let barrier = Arc::new(tokio::sync::Barrier::new(10));
    for i in 0..10 {
        let router = if i % 2 == 0 {
            router.clone()
        } else {
            other_router.clone()
        };
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            call(
                &router,
                "POST",
                "/v1/versions",
                "writer",
                "race",
                json!({"metadata":{"fixture":true}}),
            )
            .await
        }));
    }
    let mut version = None;
    for task in tasks {
        let (status, body) = task.await.unwrap();
        match status {
            200 => {
                if let Some(prior) = &version {
                    assert_eq!(prior, &body);
                } else {
                    version = Some(body);
                }
            }
            409 => assert!(body["type"]
                .as_str()
                .unwrap()
                .ends_with("command-unresolved")),
            other => panic!("unexpected status {other}"),
        }
    }
    let version = version.unwrap();
    let (legacy_hash, legacy_body): (String, String) = sqlx::query_as(
        "SELECT request_hash,response_body FROM idempotency_keys WHERE tenant_id=$1 AND key='race'",
    )
    .bind(&tenant)
    .fetch_one(first.pg_pool().unwrap())
    .await
    .unwrap();
    assert_eq!(
        second
            .idem_check(&tenant, "race", &legacy_hash)
            .await
            .unwrap(),
        Some(legacy_body)
    );
    assert_eq!(
        call(
            &other_router,
            "POST",
            "/v1/versions",
            "writer",
            "race",
            json!({"metadata":{"fixture":true}})
        )
        .await
        .1,
        version
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM memory_versions WHERE tenant_id=$1")
        .bind(&tenant)
        .fetch_one(first.pg_pool().unwrap())
        .await
        .unwrap();
    assert_eq!(count, 2, "legacy call plus exactly one guarded call");
    assert_eq!(
        call(
            &router,
            "POST",
            "/v1/versions",
            "writer",
            "race",
            json!({"metadata":{"fixture":false}})
        )
        .await
        .0,
        422
    );
    assert_eq!(
        call(
            &router,
            "GET",
            "/v1/command-recovery/receipt?key=race",
            "other",
            "unused",
            Value::Null
        )
        .await
        .0,
        404
    );
    assert_eq!(
        rpc.command_recovery_receipt(native("other", Value::Null, "race"))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::NotFound
    );
    let metadata = rpc
        .command_recovery_receipt(native("admin", Value::Null, "race"))
        .await
        .unwrap()
        .into_inner();
    let metadata: Value = serde_json::from_slice(&metadata.body).unwrap();
    assert_eq!(metadata["state"], "completed");
    assert!(metadata.get("response_body").is_none());
    assert_eq!(
        call(
            &router,
            "GET",
            "/v1/command-recovery/receipt?key=race",
            "reader",
            "unused",
            Value::Null
        )
        .await
        .0,
        403
    );
    // Body-equivalent requests targeting distinct versions must not replay.
    let a = version["version_id"].as_str().unwrap();
    let b = legacy.1["version_id"].as_str().unwrap();
    let counts = json!({"key":"fixture","scope_path":"root","count":1});
    assert_eq!(
        call(
            &router,
            "POST",
            &format!("/v1/versions/{a}/counters"),
            "writer",
            "scope",
            counts.clone()
        )
        .await
        .0,
        200
    );
    assert_eq!(
        call(
            &router,
            "POST",
            &format!("/v1/versions/{b}/counters"),
            "writer",
            "scope",
            counts
        )
        .await
        .0,
        422
    );
    assert!(matches!(
        begin(&first, &tenant, &"x".repeat(256), "fixture", "rest:limit")
            .await
            .unwrap(),
        Admission::Claimed
    ));
    assert!(matches!(
        begin(&first, &tenant, &"x".repeat(257), "fixture", "rest:limit").await,
        Err(KernelError::InvalidInput(_))
    ));
    assert!(matches!(
        begin(&first, &tenant, "", "fixture", "rest:limit").await,
        Err(KernelError::InvalidInput(_))
    ));
    assert!(matches!(
        begin(&first, &tenant, "unresolved", "fixture", "rest:hash")
            .await
            .unwrap(),
        Admission::Claimed
    ));
    assert!(finish(
        &first,
        &tenant,
        "unresolved",
        "wrong-operation",
        "rest:hash",
        "{}"
    )
    .await
    .is_err());
    assert!(matches!(
        begin(
            &first,
            &tenant,
            "unresolved",
            "another-operation",
            "rest:hash"
        )
        .await,
        Err(KernelError::IdempotencyMismatch)
    ));

    // A handler error is conservatively unresolved: this layer cannot prove
    // arbitrary commands applied no effects. Both gRPC planes must preserve
    // the non-retryable category, including rich error details.
    let missing_parent = json!({"parent_version_id":"missing-parent"});
    assert_eq!(
        call(
            &router,
            "POST",
            "/v1/versions",
            "writer",
            "failed",
            missing_parent.clone()
        )
        .await
        .0,
        404
    );
    let error = rpc
        .create_version(native("writer", missing_parent, "failed"))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert_eq!(
        error.get_error_details().error_info().unwrap().reason,
        "command-unresolved"
    );
    let typed = crate::grpc::CommandSvc {
        state: second.clone(),
    };
    let typed_request = || {
        let mut request = tonic::Request::new(pb::CreateVersionRequest {
            parent_version_id: "missing-parent".into(),
            metadata_json: String::new(),
        });
        request
            .metadata_mut()
            .insert("authorization", "Bearer writer".parse().unwrap());
        request
            .metadata_mut()
            .insert("idempotency-key", "typed-failed".parse().unwrap());
        request
    };
    assert_eq!(
        typed
            .create_version(typed_request())
            .await
            .unwrap_err()
            .code(),
        tonic::Code::NotFound
    );
    let error = typed.create_version(typed_request()).await.unwrap_err();
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert_eq!(
        error.get_error_details().error_info().unwrap().reason,
        "command-unresolved"
    );
    sqlx::query("UPDATE command_receipts SET created_at=now()-interval '10 days',completed_at=CASE WHEN state='completed' THEN now()-interval '10 days' END WHERE tenant_id=$1")
        .bind(&tenant).execute(first.pg_pool().unwrap()).await.unwrap();
    assert_eq!(
        prune_completed(first.pg_pool().unwrap(), 0).await.unwrap(),
        0
    );
    assert_eq!(
        call(
            &router,
            "GET",
            "/v1/command-recovery/receipt?key=race",
            "admin",
            "unused",
            Value::Null
        )
        .await
        .0,
        200
    );
    assert!(
        prune_completed(first.pg_pool().unwrap(), 3600)
            .await
            .unwrap()
            >= 2
    );
    assert_eq!(
        call(
            &router,
            "GET",
            "/v1/command-recovery/receipt?key=race",
            "admin",
            "unused",
            Value::Null
        )
        .await
        .0,
        404
    );
    assert!(
        begin(&second, &tenant, "unresolved", "fixture", "rest:hash")
            .await
            .is_err()
    );
    assert_eq!(
        call(
            &router,
            "GET",
            "/v1/command-recovery/receipt?key=unresolved",
            "admin",
            "unused",
            Value::Null
        )
        .await
        .1["state"],
        "unresolved"
    );
}
