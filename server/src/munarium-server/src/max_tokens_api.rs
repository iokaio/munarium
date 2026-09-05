// SPDX-License-Identifier: Apache-2.0
//! Per-call output-token budgets (2026-09-02): the eight `max_tokens`
//! ceilings the server hands a model provider, as ONE replaceable object.
//!
//! Three layers, first hit wins:
//!
//! 1. the runbook's own grammar where it has a knob (`completion.maxTokens`,
//!    `modelQueryExpansion.maxTokens`) — resolved by the callers, not here;
//! 2. the tenant's replacement (`POST /v1/max-tokens`), persisted in
//!    `max_tokens_budgets` on Postgres (migration 0031) and process-local on
//!    the memory store, which config validation already confines to one
//!    replica;
//! 3. the process defaults: `MUNARIUM_MAX_TOKENS_*` over the built-ins
//!    (`MaxTokensBudgets::default()`), parsed once at boot into
//!    `Config::max_tokens` and refused at boot when out of range.
//!
//! The registry mirrors `ProviderRegistry`: a per-tenant cache re-read from
//! the table after `MUNARIUM_REGISTRY_TTL_SECS`, so a replacement posted to
//! one replica converges on the others within the TTL — the same promise,
//! and the same limit, that provider configs make. The replica that took the
//! POST answers the new values immediately.
//!
//! There is no partial update by construction: the wire type has eight
//! required fields, a body missing one is `invalid-input`, and the store
//! writes the whole object.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use munarium_api_types as dto;
use munarium_core::{KernelError, Result};
use tokio::sync::{Mutex, RwLock};

use crate::error::ApiError;
use crate::state::AppState;

pub use dto::MaxTokensBudgets;

type ApiResult<T> = std::result::Result<T, ApiError>;

/// Mutable access to one budget of the object.
type FieldSlot = fn(&mut MaxTokensBudgets) -> &mut u32;

/// One line per budget: its JSON field, its environment variable, and the
/// accessor — kept as a single table so the env surface, the wire object and
/// the validation cannot drift apart.
const FIELDS: [(&str, &str, FieldSlot); 8] = [
    (
        "turn_completion",
        "MUNARIUM_MAX_TOKENS_TURN_COMPLETION",
        |b| &mut b.turn_completion,
    ),
    (
        "query_expansion",
        "MUNARIUM_MAX_TOKENS_QUERY_EXPANSION",
        |b| &mut b.query_expansion,
    ),
    (
        "complete_default",
        "MUNARIUM_MAX_TOKENS_COMPLETE_DEFAULT",
        |b| &mut b.complete_default,
    ),
    (
        "healthai_probe",
        "MUNARIUM_MAX_TOKENS_HEALTHAI_PROBE",
        |b| &mut b.healthai_probe,
    ),
    (
        "hierarchy_classifier",
        "MUNARIUM_MAX_TOKENS_HIERARCHY_CLASSIFIER",
        |b| &mut b.hierarchy_classifier,
    ),
    (
        "hierarchy_intent",
        "MUNARIUM_MAX_TOKENS_HIERARCHY_INTENT",
        |b| &mut b.hierarchy_intent,
    ),
    (
        "runbook_advisory",
        "MUNARIUM_MAX_TOKENS_RUNBOOK_ADVISORY",
        |b| &mut b.runbook_advisory,
    ),
    (
        "authoring_assist",
        "MUNARIUM_MAX_TOKENS_AUTHORING_ASSIST",
        |b| &mut b.authoring_assist,
    ),
];

/// The process defaults: `base` (normally the built-ins) with every set
/// `MUNARIUM_MAX_TOKENS_*` applied, then range-checked. A variable that does
/// not parse or a value out of range is an error — a budget that silently
/// fell back to a built-in would be the setting that lies.
pub fn from_env() -> std::result::Result<MaxTokensBudgets, String> {
    from_lookup(MaxTokensBudgets::default(), |k| std::env::var(k).ok())
}

pub fn from_lookup(
    base: MaxTokensBudgets,
    lookup: impl Fn(&str) -> Option<String>,
) -> std::result::Result<MaxTokensBudgets, String> {
    let mut out = base;
    for (_, var, get) in FIELDS {
        if let Some(raw) = lookup(var) {
            let v: u32 = raw
                .trim()
                .parse()
                .map_err(|e| format!("{var}: {e} (got {raw:?})"))?;
            *get(&mut out) = v;
        }
    }
    validate(&out).map_err(|e| format!("MUNARIUM_MAX_TOKENS_*: {e}"))?;
    Ok(out)
}

/// Ranges. The two budgets a runbook may also declare keep the grammar's
/// ranges (runbooks `validate.rs`: `completion.maxTokens` 256..=16,384,
/// `modelQueryExpansion.maxTokens` 32..=512), so the API cannot set a value
/// a runbook could not; the rest are bounded only against nonsense.
pub fn validate(b: &MaxTokensBudgets) -> std::result::Result<(), String> {
    fn check(name: &str, v: u32, lo: u32, hi: u32) -> std::result::Result<(), String> {
        if (lo..=hi).contains(&v) {
            Ok(())
        } else {
            Err(format!("{name} must be in {lo}..={hi}, got {v}"))
        }
    }
    check("turn_completion", b.turn_completion, 256, 16_384)?;
    check("query_expansion", b.query_expansion, 32, 512)?;
    check("complete_default", b.complete_default, 1, 65_536)?;
    check("healthai_probe", b.healthai_probe, 1, 65_536)?;
    check("hierarchy_classifier", b.hierarchy_classifier, 1, 65_536)?;
    check("hierarchy_intent", b.hierarchy_intent, 1, 65_536)?;
    check("runbook_advisory", b.runbook_advisory, 1, 65_536)?;
    check("authoring_assist", b.authoring_assist, 1, 65_536)?;
    Ok(())
}

#[derive(Clone)]
struct Override {
    budgets: MaxTokensBudgets,
    updated_at: String,
}

/// Per-tenant replacements over the process defaults, cached with the same
/// TTL discipline as the provider registry.
#[derive(Default)]
pub struct MaxTokensRegistry {
    overrides: RwLock<HashMap<String, Override>>,
    loaded: Mutex<HashMap<String, std::time::Instant>>,
}

impl MaxTokensRegistry {
    async fn ensure_loaded(&self, state: &AppState, tenant: &str) -> Result<()> {
        let ttl = state.config.registry_ttl_secs;
        {
            let loaded = self.loaded.lock().await;
            if let Some(at) = loaded.get(tenant) {
                if ttl == 0 || at.elapsed().as_secs() < ttl {
                    return Ok(());
                }
            }
        }
        if let Some(pool) = state.pg_pool() {
            let store = munarium_store_pg::PgMaxTokensStore::new(pool.clone());
            match store.get(tenant).await? {
                Some((value, updated_at)) => {
                    // A row this binary cannot read as a whole valid object
                    // (a newer writer's field, a hand edit) fails CLOSED to
                    // the process defaults, out loud — never to a mix.
                    match serde_json::from_value::<MaxTokensBudgets>(value) {
                        Ok(budgets) => match validate(&budgets) {
                            Ok(()) => {
                                self.overrides.write().await.insert(
                                    tenant.to_string(),
                                    Override {
                                        budgets,
                                        updated_at,
                                    },
                                );
                            }
                            Err(e) => {
                                tracing::warn!(tenant, error = %e, "stored max-tokens budgets out of range; using the process defaults");
                                self.overrides.write().await.remove(tenant);
                            }
                        },
                        Err(e) => {
                            tracing::warn!(tenant, error = %e, "stored max-tokens budgets do not decode; using the process defaults");
                            self.overrides.write().await.remove(tenant);
                        }
                    }
                }
                None => {
                    self.overrides.write().await.remove(tenant);
                }
            }
        }
        self.loaded
            .lock()
            .await
            .insert(tenant.to_string(), std::time::Instant::now());
        Ok(())
    }

    /// The budgets a call for `tenant` should use right now.
    pub async fn effective(&self, state: &AppState, tenant: &str) -> Result<MaxTokensBudgets> {
        self.ensure_loaded(state, tenant).await?;
        Ok(self
            .overrides
            .read()
            .await
            .get(tenant)
            .map(|o| o.budgets)
            .unwrap_or(state.config.max_tokens))
    }

    /// The effective set plus where it comes from — the GET body.
    pub async fn view(&self, state: &AppState, tenant: &str) -> Result<dto::MaxTokensResponse> {
        self.ensure_loaded(state, tenant).await?;
        Ok(match self.overrides.read().await.get(tenant) {
            Some(o) => dto::MaxTokensResponse {
                budgets: o.budgets,
                source: "tenant".into(),
                updated_at: Some(o.updated_at.clone()),
            },
            None => dto::MaxTokensResponse {
                budgets: state.config.max_tokens,
                source: "environment".into(),
                updated_at: None,
            },
        })
    }

    /// Replace the tenant's whole set: range-checked, written to the table
    /// when there is one, and cached so this replica answers the new values
    /// immediately (the others converge within the registry TTL).
    pub async fn replace(
        &self,
        state: &AppState,
        tenant: &str,
        budgets: MaxTokensBudgets,
    ) -> Result<dto::MaxTokensResponse> {
        validate(&budgets).map_err(KernelError::InvalidInput)?;
        let updated_at = match state.pg_pool() {
            Some(pool) => {
                let value = serde_json::to_value(budgets)
                    .map_err(|e| KernelError::Storage(e.to_string()))?;
                munarium_store_pg::PgMaxTokensStore::new(pool.clone())
                    .replace(tenant, &value)
                    .await?
            }
            None => chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        };
        self.overrides.write().await.insert(
            tenant.to_string(),
            Override {
                budgets,
                updated_at: updated_at.clone(),
            },
        );
        self.loaded
            .lock()
            .await
            .insert(tenant.to_string(), std::time::Instant::now());
        Ok(dto::MaxTokensResponse {
            budgets,
            source: "tenant".into(),
            updated_at: Some(updated_at),
        })
    }
}

/// GET /v1/max-tokens — the effective per-call output-token budgets for the
/// caller's tenant and where they come from (`source`: `tenant` after a
/// replacement, else `environment`). Any authenticated role: the numbers
/// shape spend, they are not secrets.
#[utoipa::path(get, path = "/v1/max-tokens",
    responses((status = 200, body = dto::MaxTokensResponse)), tag = "providers")]
pub async fn get_max_tokens(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<dto::MaxTokensResponse>> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    Ok(Json(state.max_tokens.view(&state, &ctx.tenant_id).await?))
}

/// POST /v1/max-tokens — replace the tenant's WHOLE set. Every field is
/// required (a body missing one is 400 `invalid-input`: there is no partial
/// update), each is range-checked, and the answer is the same shape GET
/// returns. Static **rw** only, like provider configs and runbooks.
#[utoipa::path(post, path = "/v1/max-tokens",
    request_body = dto::MaxTokensBudgets,
    responses((status = 200, body = dto::MaxTokensResponse)), tag = "providers")]
pub async fn replace_max_tokens(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    crate::rest::ProblemJson(body): crate::rest::ProblemJson<dto::MaxTokensBudgets>,
) -> ApiResult<Json<dto::MaxTokensResponse>> {
    let (ctx, _store) = crate::rest::auth(&state, &headers).await?;
    ctx.require_rw()?;
    Ok(Json(
        state
            .max_tokens
            .replace(&state, &ctx.tenant_id, body)
            .await?,
    ))
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
            auth: AuthMode::Static(vec![
                ("rw-token".into(), "tenant-default".into(), "rw".into()),
                ("mgmt-token".into(), "tenant-default".into(), "mgmt".into()),
                ("other-rw".into(), "tenant-other".into(), "rw".into()),
            ]),
            shutdown_grace_secs: 1,
            token_secret: Some(b"max-tokens-test-secret-32-bytes!!!".to_vec()),
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
            instance_id: "test-instance".into(),
            source_store: SourceStoreConfig::Mem,
            doc_intel: DocIntelConfig::None,
            max_tokens: MaxTokensBudgets::default(),
        }
    }

    async fn router() -> axum::Router {
        crate::rest::router(AppState::new(test_config()).await.expect("state"))
    }

    fn request(
        method: &str,
        token: &str,
        body: Option<serde_json::Value>,
    ) -> axum::http::Request<axum::body::Body> {
        let builder = axum::http::Request::builder()
            .method(method)
            .uri("/v1/max-tokens")
            .header("authorization", format!("Bearer {token}"))
            .header("x-munarium-uid", "tester");
        match body {
            Some(v) => builder
                .header("content-type", "application/json")
                .body(axum::body::Body::from(v.to_string()))
                .unwrap(),
            None => builder.body(axum::body::Body::empty()).unwrap(),
        }
    }

    async fn json(resp: axum::response::Response) -> (axum::http::StatusCode, serde_json::Value) {
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    fn full(turn: u32) -> serde_json::Value {
        serde_json::json!({
            "turn_completion": turn,
            "query_expansion": 300,
            "complete_default": 2048,
            "healthai_probe": 640,
            "hierarchy_classifier": 48,
            "hierarchy_intent": 600,
            "runbook_advisory": 3000,
            "authoring_assist": 9000,
        })
    }

    #[test]
    fn env_overrides_win_over_builtins_and_bad_values_fail_closed() {
        let base = MaxTokensBudgets::default();
        let set = |k: &str| match k {
            "MUNARIUM_MAX_TOKENS_TURN_COMPLETION" => Some(" 4096 ".to_string()),
            "MUNARIUM_MAX_TOKENS_HEALTHAI_PROBE" => Some("1024".to_string()),
            _ => None,
        };
        let got = from_lookup(base, set).expect("parses");
        assert_eq!(got.turn_completion, 4096);
        assert_eq!(got.healthai_probe, 1024);
        // Untouched fields keep the built-ins.
        assert_eq!(got.query_expansion, base.query_expansion);
        assert_eq!(got.authoring_assist, base.authoring_assist);
        // Unparseable and out-of-range are boot errors, not silent built-ins.
        let bad = from_lookup(base, |k| {
            (k == "MUNARIUM_MAX_TOKENS_QUERY_EXPANSION").then(|| "lots".to_string())
        });
        assert!(bad.unwrap_err().contains("QUERY_EXPANSION"));
        let low = from_lookup(base, |k| {
            (k == "MUNARIUM_MAX_TOKENS_TURN_COMPLETION").then(|| "16".to_string())
        });
        assert!(low
            .unwrap_err()
            .contains("turn_completion must be in 256..=16384"));
        // Every field has an env var, named for it, and the table names them
        // all — the env surface cannot drift from the object.
        assert_eq!(FIELDS.len(), 8);
        for (field, var, _) in FIELDS {
            assert_eq!(
                var,
                format!("MUNARIUM_MAX_TOKENS_{}", field.to_ascii_uppercase())
            );
        }
    }

    #[test]
    fn builtins_are_the_doubled_2026_09_02_values_and_validate() {
        let b = MaxTokensBudgets::default();
        assert_eq!(
            (
                b.turn_completion,
                b.query_expansion,
                b.complete_default,
                b.healthai_probe
            ),
            (2048, 256, 1024, 512)
        );
        assert_eq!(
            (
                b.hierarchy_classifier,
                b.hierarchy_intent,
                b.runbook_advisory,
                b.authoring_assist
            ),
            (32, 480, 2048, 8192)
        );
        validate(&b).expect("the built-ins are inside every range");
    }

    #[tokio::test]
    async fn get_answers_the_process_defaults_as_environment() {
        let (status, body) = json(
            router()
                .await
                .oneshot(request("GET", "rw-token", None))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(body["source"], "environment");
        assert_eq!(body["turn_completion"], 2048);
        assert_eq!(body["authoring_assist"], 8192);
        assert!(body.get("updated_at").is_none(), "{body}");
    }

    #[tokio::test]
    async fn post_replaces_the_whole_set_and_get_reads_it_back_as_tenant() {
        let app = router().await;
        let (status, body) = json(
            app.clone()
                .oneshot(request("POST", "rw-token", Some(full(4096))))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["source"], "tenant");
        assert_eq!(body["turn_completion"], 4096);
        assert!(body["updated_at"].is_string(), "{body}");
        let (status, body) = json(
            app.clone()
                .oneshot(request("GET", "mgmt-token", None))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(body["source"], "tenant");
        assert_eq!(body["turn_completion"], 4096);
        assert_eq!(body["healthai_probe"], 640);
        // The other tenant is untouched: a replacement is tenant-scoped.
        let (_, other) = json(app.oneshot(request("GET", "other-rw", None)).await.unwrap()).await;
        assert_eq!(other["source"], "environment");
        assert_eq!(other["turn_completion"], 2048);
    }

    #[tokio::test]
    async fn a_missing_field_is_invalid_input_never_a_partial_update() {
        let app = router().await;
        let mut partial = full(4096);
        partial.as_object_mut().unwrap().remove("authoring_assist");
        let (status, body) = json(
            app.clone()
                .oneshot(request("POST", "rw-token", Some(partial)))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{body}");
        assert!(
            body["type"]
                .as_str()
                .unwrap_or("")
                .ends_with("/invalid-input"),
            "{body}"
        );
        // Nothing changed.
        let (_, after) = json(app.oneshot(request("GET", "rw-token", None)).await.unwrap()).await;
        assert_eq!(after["source"], "environment");
    }

    #[tokio::test]
    async fn an_out_of_range_value_is_refused_and_nothing_changes() {
        let app = router().await;
        let mut wild = full(4096);
        wild["query_expansion"] = serde_json::json!(9999);
        let (status, body) = json(
            app.clone()
                .oneshot(request("POST", "rw-token", Some(wild)))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{body}");
        assert!(
            body["detail"]
                .as_str()
                .unwrap_or("")
                .contains("query_expansion must be in 32..=512"),
            "{body}"
        );
        let (_, after) = json(app.oneshot(request("GET", "rw-token", None)).await.unwrap()).await;
        assert_eq!(after["source"], "environment");
    }

    #[tokio::test]
    async fn replacing_needs_the_rw_role() {
        let app = router().await;
        let (status, _) = json(
            app.clone()
                .oneshot(request("POST", "mgmt-token", Some(full(4096))))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
        // Reading is open to any authenticated role.
        let (status, _) = json(
            app.oneshot(request("GET", "mgmt-token", None))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
    }
}
