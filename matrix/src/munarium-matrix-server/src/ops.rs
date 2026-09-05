// SPDX-License-Identifier: Apache-2.0
//! The ops plane on :9190 — never exposed through an ingress.
//!
//! `/metrics` carries no tenant, uid or instance label by construction
//! (metrics.rs states the rule), so it is safe to scrape and safe to curl.
//! `/healthz` and `/readyz` mirror the REST plane's, sharing the same store
//! probe so the two planes cannot disagree about readiness.

use crate::state::AppState;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use std::sync::Arc;

async fn healthz() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true }))
}

async fn readyz(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    if state.draining.load(std::sync::atomic::Ordering::Relaxed) {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "ok": false, "status": "draining" })),
        );
    }
    if state.store.ready().await {
        (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({ "ok": true })),
        )
    } else {
        (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "ok": false, "status": "store not ready" })),
        )
    }
}

async fn metrics(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        state
            .metrics
            .render(state.config.role.as_str(), env!("CARGO_PKG_VERSION")),
    )
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics))
        .with_state(state)
}
