// SPDX-License-Identifier: Apache-2.0
//! Interaction capture: every /v1 request/response is recorded,
//! attributed to the caller-asserted uid, for audit and reporting.
//!
//! Records ride a bounded mpsc channel to one writer task so capture never
//! blocks the request path. When the channel is saturated the record is
//! dropped with a warning (audit is best-effort under overload by design —
//! the alternative, backpressuring the data plane on audit writes, is worse).
//! On the memory store records are logged at debug and discarded.

use serde_json::json;

/// Body capture cap comes from MUNARIUM_INTERACTION_BODY_MAX; bodies above it
/// (or non-JSON bodies) are stored as a {sha256, bytes_len} summary.
pub fn body_json(body: &[u8], cap: usize) -> Option<serde_json::Value> {
    if body.is_empty() {
        return None;
    }
    if body.len() <= cap {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) {
            return Some(v);
        }
    }
    Some(json!({
        "sha256": crate::state::request_hash(body),
        "bytes_len": body.len(),
    }))
}

#[derive(Debug)]
pub struct InteractionRecord {
    pub tenant_id: String,
    pub uid: String,
    pub session_id: Option<String>,
    pub request_id: String,
    pub plane: &'static str,
    pub method: String,
    pub runbook_ref: Option<String>,
    pub collection_ids: Option<Vec<String>>,
    pub token_jti: Option<String>,
    pub request: Option<serde_json::Value>,
    pub response: Option<serde_json::Value>,
    pub status: Option<i32>,
    pub latency_ms: i32,
}

/// Handlers that know their domain context (sessions/turns, runbook ops)
/// insert this into the RESPONSE extensions; the capture middleware folds it
/// into the interaction row.
#[derive(Debug, Clone, Default)]
pub struct InteractionMeta {
    pub session_id: Option<String>,
    pub runbook_ref: Option<String>,
    pub collection_ids: Option<Vec<String>>,
    /// Set by handlers whose response carries a secret (e.g. a minted
    /// capability JWT): the capture middleware then stores a redaction
    /// marker instead of the response body, upholding the "token material is
    /// NEVER stored" contract (tokens_api / migration 0010 / IssueTokenResponse).
    pub redact_response: bool,
}

/// The stored stand-in for a secret-bearing response body.
pub fn redacted() -> serde_json::Value {
    json!({ "redacted": "response contains secret material; not stored" })
}

pub type InteractionSender = tokio::sync::mpsc::Sender<InteractionRecord>;

/// Non-blocking enqueue; drops with a warning (and a counter — dashboards
/// see saturation, log greps are not the only witness) when the writer is
/// saturated.
pub fn record(state: &crate::state::AppState, rec: InteractionRecord) {
    if let Err(e) = state.interactions_tx.try_send(rec) {
        state
            .metrics
            .inc("munarium_interactions_dropped_total", String::new());
        tracing::warn!(error = %e, "interaction record dropped (writer saturated or closed)");
    }
}

/// One writer task per process, draining to the interactions table. Every
/// row is stamped with this instance's id (MUNARIUM_INSTANCE_ID resolution) so
/// N-replica debugging can tell which instance served a call.
pub fn spawn_writer(
    pool: Option<sqlx::PgPool>,
    metrics: std::sync::Arc<crate::metrics::Metrics>,
    instance_id: String,
) -> InteractionSender {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<InteractionRecord>(1024);
    tokio::spawn(async move {
        while let Some(rec) = rx.recv().await {
            let Some(pool) = pool.as_ref() else {
                tracing::debug!(uid = %rec.uid, method = %rec.method, "interaction (memory store; not persisted)");
                continue;
            };
            let id = format!("int-{}", uuid::Uuid::now_v7().simple());
            let result = sqlx::query(
                "INSERT INTO interactions
                   (tenant_id, id, uid, session_id, request_id, plane, method,
                    runbook_ref, collection_ids, token_jti, request, response,
                    status, latency_ms, instance_id)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)",
            )
            .bind(&rec.tenant_id)
            .bind(&id)
            .bind(&rec.uid)
            .bind(&rec.session_id)
            .bind(&rec.request_id)
            .bind(rec.plane)
            .bind(&rec.method)
            .bind(&rec.runbook_ref)
            .bind(&rec.collection_ids)
            .bind(&rec.token_jti)
            .bind(&rec.request)
            .bind(&rec.response)
            .bind(rec.status)
            .bind(rec.latency_ms)
            .bind(&instance_id)
            .execute(pool)
            .await;
            if let Err(e) = result {
                metrics.inc("munarium_interactions_insert_failures_total", String::new());
                tracing::warn!(error = %e, "interaction insert failed");
            }
        }
    });
    tx
}
