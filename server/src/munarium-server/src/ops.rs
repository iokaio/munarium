// SPDX-License-Identifier: Apache-2.0
//! The ops plane (:9090): /healthz, /readyz, /metrics. Internal-only by
//! deployment posture (never exposed through the gateway); unauthenticated
//! by design — /metrics carries no tenant or user data (metrics.rs
//! cardinality rules forbid those labels), and the health endpoints return
//! a bare status. See docs/security-posture.md.
//!
//! /readyz probes the backing store through the SAME `AppState::store_ready`
//! the REST plane's /readyz uses, so the two planes cannot disagree about
//! readiness. Until 2026-08-17 this plane returned a static "ok" — a lie to
//! any orchestrator probe pointed at it (dev-guide §13 discussed it; the
//! deployed terraform envs happened to probe the REST plane, so this closed
//! a trap rather than a live incident).

use crate::state::AppState;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use std::sync::Arc;

async fn healthz() -> &'static str {
    "ok"
}

async fn readyz(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    if state.draining.load(std::sync::atomic::Ordering::Relaxed) {
        return (axum::http::StatusCode::SERVICE_UNAVAILABLE, "draining");
    }
    // The same three terms as the REST plane's /readyz — store readiness AND
    // datastore admission (§9.2). The two planes must not disagree: an
    // orchestrator probing this port would otherwise admit a datastore-mode
    // replica whose selected scopes are not yet hydrated, which is exactly
    // the wedge the readiness warmer exists to prevent.
    if state.store_ready().await && state.datastore_readiness().admits() {
        (axum::http::StatusCode::OK, "ok")
    } else {
        (axum::http::StatusCode::SERVICE_UNAVAILABLE, "not ready")
    }
}

async fn metrics(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        crate::metrics::render(&state),
    )
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AuthMode, Config, DocIntelConfig, SourceStoreConfig, StoreKind};
    use tower::ServiceExt;

    fn test_config() -> Config {
        Config {
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
            db_max_conns: 2,
            idempotency_ttl_secs: 86_400,
            replica_count: 1,
            registry_ttl_secs: 15,
            session_idle_ttl_secs: 0,
            evidence_purge_interval_secs: 0,
            max_tokens: munarium_api_types::MaxTokensBudgets::default(),
            instance_id: "test-instance".into(),
            source_store: SourceStoreConfig::Mem,
            doc_intel: DocIntelConfig::None,
        }
    }

    async fn body_string(resp: axum::response::Response) -> String {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    #[tokio::test]
    async fn metrics_exposition_counts_served_requests() {
        let state = AppState::new(test_config()).await.expect("state");
        // Drive one meta request through the REST router so the RED metrics
        // see it (meta routes are counted, never captured).
        let rest = crate::rest::router(state.clone());
        let resp = rest
            .oneshot(
                axum::http::Request::get("/healthz")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        let ops = router(state.clone());
        let resp = ops
            .oneshot(
                axum::http::Request::get("/metrics")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        assert_eq!(
            resp.headers()[axum::http::header::CONTENT_TYPE],
            "text/plain; version=0.0.4"
        );
        let body = body_string(resp).await;
        assert!(body.contains("munarium_build_info{version="), "{body}");
        assert!(
            body.contains(r#"munarium_http_requests_total{plane="rest",route="/healthz",method="GET",status_class="2xx"} 1"#),
            "healthz request must be counted:\n{body}"
        );
        // Simple exposition-format sanity: every non-comment line is
        // `name value` or `name{labels} value`.
        for line in body
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
        {
            let (metric, value) = line.rsplit_once(' ').expect("line has a value");
            assert!(value.parse::<f64>().is_ok(), "non-numeric value: {line}");
            assert!(
                metric.starts_with("munarium_"),
                "unexpected metric name: {line}"
            );
        }
    }

    #[tokio::test]
    async fn ops_readyz_is_real_and_memory_store_is_ready() {
        let state = AppState::new(test_config()).await.expect("state");
        let resp = router(state)
            .oneshot(
                axum::http::Request::get("/readyz")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        assert_eq!(body_string(resp).await, "ok");
    }

    #[tokio::test]
    async fn v1_sheds_at_the_concurrency_ceiling_with_retry_after() {
        let mut cfg = test_config();
        cfg.max_concurrency = 1;
        let state = AppState::new(cfg).await.expect("state");
        // Hold the only permit: the next /v1 request must shed, before auth.
        let _held = state.rest_permits.clone().try_acquire_owned().unwrap();
        let resp = crate::rest::router(state.clone())
            .oneshot(
                axum::http::Request::get("/v1/versions/v-x/head")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(resp.headers()["retry-after"], "1");
        let body = body_string(resp).await;
        assert!(body.contains("overloaded"), "{body}");
        // Meta routes never shed: health must still answer with zero permits.
        let resp = crate::rest::router(state.clone())
            .oneshot(
                axum::http::Request::get("/healthz")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let metrics_body = crate::metrics::render(&state);
        assert!(
            metrics_body.contains("munarium_load_shed_total 1"),
            "{metrics_body}"
        );
    }

    #[tokio::test]
    async fn connector_findings_are_filed_once_and_never_block() {
        // The POST route, on the mem store. Content-idempotent, warn
        // only, stamped at head, filtered by prefix.
        let mut cfg = test_config();
        cfg.auth = crate::config::AuthMode::Disabled;
        let state = AppState::new(cfg).await.expect("state");
        let call = |method: &'static str, path: String, body: Option<serde_json::Value>| {
            let state = state.clone();
            async move {
                let mut req = axum::http::Request::builder()
                    .method(method)
                    .uri(path)
                    .header("x-munarium-uid", "matrix-svc");
                if body.is_some() {
                    // Command routes require an idempotency key; the findings
                    // route deliberately does NOT (content is the key), but
                    // sending one everywhere is harmless and keeps one helper.
                    req = req
                        .header("content-type", "application/json")
                        .header("idempotency-key", uuid::Uuid::new_v4().to_string());
                }
                let resp = crate::rest::router(state)
                    .oneshot(
                        req.body(axum::body::Body::from(
                            body.map(|b| b.to_string()).unwrap_or_default(),
                        ))
                        .unwrap(),
                    )
                    .await
                    .unwrap();
                let status = resp.status();
                let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                    .await
                    .unwrap();
                let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
                (status, v)
            }
        };
        let (_, created) = call("POST", "/v1/versions".into(), Some(serde_json::json!({}))).await;
        let vid = created["version_id"].as_str().unwrap().to_string();
        let (status, body) = call(
            "POST",
            format!("/v1/versions/{vid}/claims"),
            Some(serde_json::json!({"claim_type":"fact","subject":"s","key":"k","value":"1",
                "origin": {"kind":"connector","source_id":"crm","mapping_version":"m@1","row_key":"id=1"}})),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(
            body["claim"]["origin"]["row_key"], "id=1",
            "origin must round-trip through REST: {body}"
        );
        let head = body["head_seq"].as_u64().unwrap();

        let finding = serde_json::json!({"rule_id":"matrix.discrepancy-candidate","severity":"warn",
            "message":"differ","detail":{"evidence_ref":"evidence/e#r1","claim_id":"c1"}});
        let (status, body) = call(
            "POST",
            format!("/v1/versions/{vid}/findings"),
            Some(serde_json::json!({"findings":[finding.clone(), finding.clone()]})),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(
            body["recorded"], 1,
            "two identical findings in one call write once: {body}"
        );
        assert_eq!(body["skipped_duplicates"], 1);
        assert_eq!(body["seq"].as_u64().unwrap(), head, "stamped at head");

        let (_, body) = call(
            "POST",
            format!("/v1/versions/{vid}/findings"),
            Some(serde_json::json!({"findings":[finding]})),
        )
        .await;
        assert_eq!(body["recorded"], 0, "a replay files nothing: {body}");

        let (status, body) = call(
            "POST",
            format!("/v1/versions/{vid}/findings"),
            Some(serde_json::json!({"findings":[{"rule_id":"matrix.x","severity":"block","message":"no"}]})),
        )
        .await;
        assert_eq!(
            status,
            axum::http::StatusCode::BAD_REQUEST,
            "block is refused: {body}"
        );

        let (_, body) = call(
            "GET",
            format!("/v1/versions/{vid}/findings?rule_prefix=matrix."),
            None,
        )
        .await;
        assert_eq!(
            body["findings"].as_array().map(|a| a.len()),
            Some(1),
            "{body}"
        );
        let (_, body) = call(
            "GET",
            format!("/v1/versions/{vid}/findings?rule_prefix=gate."),
            None,
        )
        .await;
        assert_eq!(
            body["findings"].as_array().map(|a| a.len()),
            Some(0),
            "prefix excludes: {body}"
        );
    }

    #[tokio::test]
    async fn findings_persist_on_gated_writes_and_are_queryable() {
        // The full wire loop for §13 entry 12: a conflicting claim draws a
        // gate.ledger-conflict finding in the write response AND lands in
        // the persisted store, readable via GET .../findings with the
        // pin/severity filters.
        let mut cfg = test_config();
        cfg.auth = crate::config::AuthMode::Disabled;
        let state = AppState::new(cfg).await.expect("state");
        let post = |path: String, body: serde_json::Value, idem: &'static str| {
            let state = state.clone();
            async move {
                let resp = crate::rest::router(state)
                    .oneshot(
                        axum::http::Request::post(path)
                            .header("content-type", "application/json")
                            .header("x-munarium-uid", "finding-test")
                            .header("idempotency-key", idem)
                            .body(axum::body::Body::from(body.to_string()))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let status = resp.status();
                let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                    .await
                    .unwrap();
                let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                (status, v)
            }
        };
        let (status, created) = post("/v1/versions".into(), serde_json::json!({}), "f-0").await;
        assert_eq!(status, axum::http::StatusCode::OK, "{created}");
        let vid = created["version_id"].as_str().unwrap().to_string();
        let claim = |k: &str, v: &str| serde_json::json!({ "claim_type": "fact", "subject": "hero", "key": k, "value": v });
        let (status, _) = post(
            format!("/v1/versions/{vid}/claims"),
            claim("eyes", "green"),
            "f-1",
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        let (status, second) = post(
            format!("/v1/versions/{vid}/claims"),
            claim("eyes", "blue"),
            "f-2",
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "disputed is success");
        assert!(
            second["findings"]
                .as_array()
                .is_some_and(|f| f.iter().any(|x| x["rule_id"] == "gate.ledger-conflict")),
            "write response carries the finding: {second}"
        );

        let get = |path: String| {
            let state = state.clone();
            async move {
                let resp = crate::rest::router(state)
                    .oneshot(
                        axum::http::Request::get(path)
                            .header("x-munarium-uid", "finding-test")
                            .body(axum::body::Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                    .await
                    .unwrap();
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()
            }
        };
        let stored = get(format!("/v1/versions/{vid}/findings")).await;
        let rows = stored["findings"].as_array().expect("findings array");
        assert!(
            rows.iter()
                .any(|r| r["finding"]["rule_id"] == "gate.ledger-conflict"),
            "persisted store must hold the finding: {stored}"
        );
        // One pin bounds this store too: before the conflicting write
        // (seq 1) there are no findings.
        let pinned = get(format!("/v1/versions/{vid}/findings?as_of_seq=1")).await;
        assert_eq!(
            pinned["findings"].as_array().map(|a| a.len()),
            Some(0),
            "{pinned}"
        );
        // The severity filter narrows without lying.
        let blocks = get(format!("/v1/versions/{vid}/findings?severity=block")).await;
        assert!(
            blocks["findings"].as_array().is_some_and(|a| !a.is_empty()),
            "{blocks}"
        );
    }

    #[tokio::test]
    async fn armed_chronology_gates_certain_order_violations() {
        // End-to-end for §13 entry 13: apply a rules asset, arm a version
        // via metadata, write two dated facts in the wrong order — the
        // sixth gate fires gate.chronology-order over the wire, and the
        // finding lands in the persisted store like every other.
        let mut cfg = test_config();
        cfg.auth = crate::config::AuthMode::Disabled;
        let state = AppState::new(cfg).await.expect("state");
        let send = |method: &'static str, path: String, ct: &'static str, body: String| {
            let state = state.clone();
            async move {
                let resp = crate::rest::router(state)
                    .oneshot(
                        axum::http::Request::builder()
                            .method(method)
                            .uri(path)
                            .header("content-type", ct)
                            .header("x-munarium-uid", "chrono-test")
                            .header("idempotency-key", uuid::Uuid::now_v7().to_string())
                            .body(axum::body::Body::from(body))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let status = resp.status();
                let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                    .await
                    .unwrap();
                let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
                (status, v)
            }
        };
        let rules = "apiVersion: munarium.ioka.io/v1\nkind: ChronologyRules\nmetadata: { name: story-order }\nspec:\n  order:\n    - { before: battle.date, after: treaty.date }\n";
        let (status, applied) = send(
            "POST",
            "/v1/chronology-rules".into(),
            "text/yaml",
            rules.into(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{applied}");
        assert_eq!(applied["name"], "story-order");

        let (status, created) = send(
            "POST",
            "/v1/versions".into(),
            "application/json",
            serde_json::json!({ "metadata": { "chronology_rules": "story-order" } }).to_string(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{created}");
        let vid = created["version_id"].as_str().unwrap().to_string();

        let claim = |subject: &str, value: &str| {
            serde_json::json!({ "claim_type": "fact", "subject": subject, "key": "date", "value": value })
                .to_string()
        };
        // Treaty first (1785), then a battle AFTER it (1790) — the order
        // rule says battle must precede treaty: a CERTAIN violation.
        let (status, first) = send(
            "POST",
            format!("/v1/versions/{vid}/claims"),
            "application/json",
            claim("treaty", "1785"),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{first}");
        let (status, second) = send(
            "POST",
            format!("/v1/versions/{vid}/claims"),
            "application/json",
            claim("battle", "1790"),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{second}");
        assert!(
            second["findings"]
                .as_array()
                .is_some_and(|f| f.iter().any(|x| x["rule_id"]
                    .as_str()
                    .unwrap_or("")
                    .starts_with("gate.chronology"))),
            "the armed sixth gate must fire on a certain order violation: {second}"
        );

        // Arming with a MISSING asset fails loud, never silently un-gates.
        let (status, orphan) = send(
            "POST",
            "/v1/versions".into(),
            "application/json",
            serde_json::json!({ "metadata": { "chronology_rules": "no-such-rules" } }).to_string(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{orphan}");
        let ovid = orphan["version_id"].as_str().unwrap().to_string();
        let (status, refused) = send(
            "POST",
            format!("/v1/versions/{ovid}/claims"),
            "application/json",
            claim("treaty", "1785"),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{refused}");
        assert!(
            refused["detail"]
                .as_str()
                .is_some_and(|d| d.contains("no-such-rules")),
            "{refused}"
        );
    }

    #[tokio::test]
    async fn admin_requires_mgmt_and_login_page_is_public() {
        // Static auth: without a cookie or bearer, /admin redirects to login.
        let mut cfg = test_config();
        cfg.auth = AuthMode::Static(vec![("m".into(), "t".into(), "mgmt".into())]);
        let state = AppState::new(cfg).await.expect("state");
        let resp = crate::rest::router(state.clone())
            .oneshot(
                axum::http::Request::get("/admin")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::SEE_OTHER);
        assert_eq!(resp.headers()["location"], "/admin/login");
        let resp = crate::rest::router(state.clone())
            .oneshot(
                axum::http::Request::get("/admin/login")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        // With the mgmt bearer, the health page renders (memory store — the
        // process-state page needs no postgres).
        let resp = crate::rest::router(state.clone())
            .oneshot(
                axum::http::Request::get("/admin/health")
                    .header("authorization", "Bearer m")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let body = body_string(resp).await;
        assert!(body.contains("<svg") || body.contains("tile"), "{body}");
    }
}
