// SPDX-License-Identifier: Apache-2.0
//! Chronology rules assets (2026-08-17) — the arming surface for the
//! kernel's sixth gate. `check_chronology` shipped complete in the milestone (interval
//! algebra, explicit precision, fire-only-on-certain) and was unreachable
//! from the wire until this module: rules are applied as a declarative YAML
//! asset (the shape/provider-config pattern), and a memory version arms itself by
//! naming one in its creation metadata: `{"chronology_rules": "<name>"}`.
//! The write path then runs the gate right after `run_gates` and merges its
//! findings into the same block/dispute lifecycle.
//!
//! Storage: raw YAML in the `chronology_rules` table (pg) or an in-process
//! map (memory store — dev/tests), parse-validated at apply time and parsed
//! again at use. No registry cache ON PURPOSE: armed writes are rare, one
//! indexed point-read per armed write is cheap, and no cache means no
//! cross-instance staleness to reason about.

use crate::error::ApiError;
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use munarium_api_types as dto;
use munarium_core::chrono_gate::ChronologyRules;
use munarium_core::{KernelError, Result};
use serde::Deserialize;
use std::sync::Arc;

type ApiResult<T> = std::result::Result<T, ApiError>;

/// The applied asset: the standard apiVersion/kind envelope over the
/// kernel's own `ChronologyRules` shape (order/contains/forbid_overlap/
/// deadlines/durations + temporal_keys).
#[derive(Debug, Deserialize)]
pub struct ChronologyRulesDoc {
    /// Envelope conformity only — parsed so a missing apiVersion fails
    /// loudly, never read past that.
    #[serde(rename = "apiVersion")]
    #[allow(dead_code)]
    pub api_version: String,
    pub kind: String,
    pub metadata: ChronologyMeta,
    pub spec: ChronologyRules,
}

#[derive(Debug, Deserialize)]
pub struct ChronologyMeta {
    pub name: String,
}

pub fn parse_rules_doc(yaml: &str) -> Result<ChronologyRulesDoc> {
    let doc: ChronologyRulesDoc = serde_yaml::from_str(yaml)
        .map_err(|e| KernelError::InvalidInput(format!("chronology rules: {e}")))?;
    if doc.kind != "ChronologyRules" {
        return Err(KernelError::InvalidInput(format!(
            "kind must be ChronologyRules, got '{}'",
            doc.kind
        )));
    }
    if doc.metadata.name.trim().is_empty() {
        return Err(KernelError::InvalidInput(
            "metadata.name must be non-empty".into(),
        ));
    }
    if doc.spec.all_targets().is_empty() {
        return Err(KernelError::InvalidInput(
            "rules declare no order/contains/forbid_overlap/deadlines/durations — an empty \
             asset arms nothing and is almost certainly a mistake"
                .into(),
        ));
    }
    Ok(doc)
}

/// POST /v1/chronology-rules — apply (upsert) a rules asset. rw role;
/// text/yaml body, like shapes. `mmctl apply -f` sniffs the kind.
#[utoipa::path(post, path = "/v1/chronology-rules",
    request_body(content = String, content_type = "text/yaml"),
    responses((status = 200, body = dto::ApplyChronologyRulesResponse),
              (status = 400, description = "invalid rules yaml")),
    tag = "command")]
pub async fn apply_rules(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    yaml: String,
) -> ApiResult<Json<dto::ApplyChronologyRulesResponse>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_rw()?;
    let doc = parse_rules_doc(&yaml)?;
    state
        .store_chronology_rules(&ctx.tenant_id, &doc.metadata.name, &yaml)
        .await?;
    Ok(Json(dto::ApplyChronologyRulesResponse {
        name: doc.metadata.name,
        rule_count: doc.spec.all_targets().len(),
    }))
}

/// GET /v1/chronology-rules/{name} — the applied YAML back, verbatim.
#[utoipa::path(get, path = "/v1/chronology-rules/{name}",
    params(("name" = String, Path)),
    responses((status = 200, description = "the applied rules yaml", content_type = "text/yaml"),
              (status = 404, description = "no such rules asset")),
    tag = "query")]
pub async fn get_rules(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> ApiResult<axum::response::Response> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    let yaml = state
        .load_chronology_rules_yaml(&ctx.tenant_id, &name)
        .await?
        .ok_or(KernelError::NotFound {
            kind: "chronology-rules",
            id: name,
        })?;
    Ok(([(axum::http::header::CONTENT_TYPE, "text/yaml")], yaml).into_response())
}

use axum::response::IntoResponse;
