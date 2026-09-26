// SPDX-License-Identifier: Apache-2.0
//! Actual authenticated REST/gRPC command receipt windows across process death.
#[path = "../../../tests/support/process_crash.rs"]
pub(crate) mod harness;
use crate::config::{AuthMode, Config, DocIntelConfig, SourceStoreConfig, StoreKind};
use crate::state::AppState;
pub(crate) use harness::barrier;
use munarium_proto::mmp::v1 as pb;
use prost::Message;
use std::sync::Arc;

pub(crate) fn receipt_barrier(hash: &str, response: &str) {
    if std::env::var("P08_MODE").as_deref() == Ok("write") {
        std::fs::write(harness::dir().join("original-response"), response).unwrap();
        std::fs::write(harness::dir().join("request-hash"), hash).unwrap();
        barrier("command_completed");
    }
}

pub(crate) async fn state(tenant: &str) -> Arc<AppState> {
    AppState::new(Config {
        http_addr: "127.0.0.1:0".into(),
        grpc_addr: None,
        ops_addr: "127.0.0.1:0".into(),
        store: StoreKind::Postgres,
        database_url: Some(harness::setting("MUNARIUM_TEST_DATABASE_URL")),
        auth: AuthMode::Static(vec![("p08-test-token".into(), tenant.into(), "rw".into())]),
        shutdown_grace_secs: 1,
        token_secret: None,
        token_ttl_secs: 3600,
        require_uid: true,
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
        max_tokens: Default::default(),
        instance_id: "p08-fixture".into(),
        source_store: SourceStoreConfig::Pg,
        doc_intel: DocIntelConfig::None,
    })
    .await
    .unwrap_or_else(|_| panic!("fixture state connection failed"))
}

pub(crate) fn available() -> bool {
    let available = std::env::var_os("MUNARIUM_TEST_DATABASE_URL").is_some();
    if !available {
        eprintln!("UNAVAILABLE: P08 requires a disposable MUNARIUM_TEST_DATABASE_URL");
    }
    available
}

#[test]
fn command_receipt_process_recovery() {
    if !available() {
        return;
    }
    for plane in ["rest", "grpc"] {
        for phase in ["command_completed", "receipt_persisted"] {
            for crash in [false, true] {
                harness::run(
                    "crash_recovery::child",
                    plane,
                    phase,
                    crash,
                    &format!("p08-command-{}", uuid::Uuid::new_v4().simple()),
                );
            }
        }
    }
}

async fn endpoint(state: Arc<AppState>, plane: &str) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = if plane == "rest" {
        tokio::spawn(async move {
            axum::serve(listener, crate::rest::router(state))
                .await
                .unwrap();
        })
    } else {
        tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(pb::command_service_server::CommandServiceServer::new(
                    crate::grpc::CommandSvc { state },
                ))
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
                .await
                .unwrap();
        })
    };
    (format!("http://{addr}"), task)
}

async fn command(url: &str, plane: &str, changed: bool) -> Result<String, String> {
    if plane == "rest" {
        let response = reqwest::Client::new()
            .post(format!("{url}/v1/versions"))
            .bearer_auth("p08-test-token")
            .header("X-Munarium-Uid", "p08-user")
            .header("Idempotency-Key", "p08-command")
            .json(&serde_json::json!({"metadata": {"changed": changed}}))
            .send()
            .await
            .unwrap();
        if !response.status().is_success() {
            let body: serde_json::Value = response.json().await.unwrap();
            return Err(body.to_string());
        }
        Ok(response
            .json::<serde_json::Value>()
            .await
            .unwrap()
            .to_string())
    } else {
        let mut client = pb::command_service_client::CommandServiceClient::connect(url.to_string())
            .await
            .unwrap();
        let mut request = tonic::Request::new(pb::CreateVersionRequest {
            parent_version_id: String::new(),
            metadata_json: serde_json::json!({"changed": changed}).to_string(),
        });
        request
            .metadata_mut()
            .insert("authorization", "Bearer p08-test-token".parse().unwrap());
        request
            .metadata_mut()
            .insert("munarium-uid", "p08-user".parse().unwrap());
        request
            .metadata_mut()
            .insert("idempotency-key", "p08-command".parse().unwrap());
        client
            .create_version(request)
            .await
            .map(|r| hex::encode(r.into_inner().encode_to_vec()))
            .map_err(|e| e.to_string())
    }
}

#[test]
#[ignore = "owned child fixture"]
fn child() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        tokio::time::timeout(harness::LIMIT, command_child())
            .await
            .expect("bounded command fixture");
    });
}

async fn command_child() {
    let tenant = harness::setting("P08_TENANT");
    let state = state(&tenant).await;
    let mode = harness::setting("P08_MODE");
    let plane = harness::setting("P08_CASE");
    let pool = state.pg_pool().unwrap();
    if mode == "setup" {
        return;
    }
    if mode == "write" {
        let (url, task) = endpoint(state.clone(), &plane).await;
        let response = command(&url, &plane, false).await.unwrap();
        std::fs::write(harness::dir().join("reply"), response).unwrap();
        task.abort();
        return;
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM memory_versions WHERE tenant_id=$1")
        .bind(&tenant)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "the command committed before either barrier");
    let receipt_exists = harness::setting("P08_PHASE") == "receipt_persisted"
        || (mode == "recover" && harness::setting("P08_CRASH") == "no");
    let hash = std::fs::read_to_string(harness::dir().join("request-hash")).unwrap();
    let original = std::fs::read_to_string(harness::dir().join("original-response")).unwrap();
    assert_eq!(
        state
            .idem_check(&tenant, "p08-command", &hash)
            .await
            .unwrap(),
        receipt_exists.then_some(original.clone())
    );
    assert!(state
        .idem_check("unrelated-p08-tenant", "p08-command", &hash)
        .await
        .unwrap()
        .is_none());
    if mode == "observe" {
        return;
    }
    let (url, task) = endpoint(state.clone(), &plane).await;
    let retried = command(&url, &plane, false).await.unwrap();
    assert_eq!(retried == original, receipt_exists);
    if receipt_exists && harness::dir().join("reply").exists() {
        assert_eq!(
            retried,
            std::fs::read_to_string(harness::dir().join("reply")).unwrap()
        );
    }
    assert_eq!(command(&url, &plane, false).await.unwrap(), retried);
    assert!(command(&url, &plane, true)
        .await
        .unwrap_err()
        .contains("idempotency"));
    let other_hash = if plane == "rest" {
        "grpc:other"
    } else {
        "rest:other"
    };
    assert!(matches!(
        state.idem_check(&tenant, "p08-command", other_hash).await,
        Err(munarium_core::KernelError::IdempotencyMismatch)
    ));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM memory_versions WHERE tenant_id=$1")
        .bind(&tenant)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(
        count,
        if receipt_exists { 1 } else { 2 },
        "the unreceipted gap can execute twice"
    );
    task.abort();
}
