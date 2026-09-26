// SPDX-License-Identifier: Apache-2.0
//! Physical-attempt admission and optional monetary catalog/reporting.
use crate::{error::ApiError, state::AppState};
use axum::{
    extract::{Query, State},
    http::HeaderMap,
    Json,
};
use chrono::{DateTime, Utc};
use munarium_api_conv::{convert, Convert};
use munarium_api_types::{json::LiteralValue, monetary as dto};
use munarium_core::{
    budget::{BudgetEstimate, BudgetOutcome, BudgetReservation, BudgetStore},
    money::{MoneyUsage, PriceSnapshot},
    provider::{UsageEvidence, UsageSource},
    KernelError, Result,
};
use munarium_providers::accounting::{AttemptObserver, Context, CONTEXT};
use munarium_store_pg::money::{MoneyStore, Observation};
use std::sync::Arc;

fn store(state: &AppState) -> Result<MoneyStore> {
    Ok(MoneyStore(crate::runbooks_api::pool(state)?.clone()))
}

struct Observer {
    store: Option<MoneyStore>,
    budgets: Arc<dyn BudgetStore>,
    config: String,
    limit: Option<u64>,
    reservations: tokio::sync::Mutex<std::collections::HashMap<String, BudgetReservation>>,
    admitted: Arc<std::sync::atomic::AtomicBool>,
    tenant: String,
    invocation: String,
    provider: String,
    model: String,
    estimate: u64,
}

#[async_trait::async_trait]
impl AttemptObserver for Observer {
    async fn begin(&self, route: &str) -> Result<String> {
        // This runs before EACH HTTP send, including retries. A cancelled or
        // failed attempt stays held; the janitor settles its estimate, never
        // refunds possibly executed work. The reserved scope cannot be a tier.
        let reservation = match self
            .budgets
            .reserve_estimated(
                &self.tenant,
                &self.config,
                "all",
                BudgetEstimate {
                    units: self.estimate,
                    revision: Some("physical-attempt-v1"),
                },
                self.limit,
            )
            .await?
        {
            BudgetOutcome::Unlimited => None,
            BudgetOutcome::Granted(r) => Some(r),
            BudgetOutcome::Exhausted { .. } => {
                return Err(KernelError::RateLimited(format!(
                    "{}provider config daily total exhausted; resets at midnight UTC",
                    crate::error::DAILY_CAP_PREFIX,
                )))
            }
        };
        let attempt = if let Some(store) = &self.store {
            store
                .begin(
                    &self.tenant,
                    &self.invocation,
                    &self.provider,
                    route,
                    &self.model,
                    self.estimate,
                )
                .await
        } else {
            Ok(uuid::Uuid::new_v4().simple().to_string())
        };
        match (attempt, reservation) {
            (Ok(id), Some(r)) => {
                self.reservations.lock().await.insert(id.clone(), r);
                self.admitted
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(id)
            }
            (Ok(id), None) => {
                self.admitted
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(id)
            }
            (Err(e), reservation) => {
                // The durable money write failed before dispatch. This is the
                // only refund path: no request has been sent by this attempt.
                if let Some(r) = reservation {
                    if self.budgets.release(&r).await.is_err() {
                        tracing::warn!("pre-dispatch budget release failed; estimate retained");
                    }
                }
                Err(e)
            }
        }
    }
    async fn finish(&self, attempt: &str, usage: MoneyUsage) -> Result<()> {
        if let Some(r) = self.reservations.lock().await.remove(attempt) {
            let evidence = UsageEvidence {
                input_tokens: usage.input,
                output_tokens: usage.output,
                source: if usage.unknown_categories {
                    UsageSource::Malformed
                } else if usage.input.is_none() && usage.output.is_none() {
                    UsageSource::Missing
                } else {
                    UsageSource::ProviderReported
                },
            };
            match evidence.accounted_units(self.estimate) {
                Ok(units) => {
                    if self
                        .budgets
                        .settle_with_evidence(&r, Some(units), Some(evidence))
                        .await
                        .is_err()
                    {
                        tracing::warn!("attempt usage not persisted; budget estimate retained");
                    }
                }
                Err(_) => tracing::warn!("attempt usage overflow; budget estimate retained"),
            }
        }
        let Some(store) = &self.store else {
            return Ok(());
        };
        let accounted = match (usage.input, usage.output) {
            (Some(i), Some(o)) if !usage.unknown_categories => i.checked_add(o),
            (i, o) => i
                .unwrap_or(0)
                .checked_add(o.unwrap_or(0))
                .map(|v| v.max(self.estimate)),
        };
        let Some(accounted_units) = accounted else {
            tracing::warn!("monetary usage overflow; attempt remains unresolved");
            return Ok(());
        };
        let observation = Observation {
            id: format!("{attempt}-response"),
            attempt_id: attempt.into(),
            previous_revision: 0,
            usage,
            accounted_units,
            resolved: true,
            evidence_ref: "provider-response-v1".into(),
            price_id: None,
        };
        if store.observe(&self.tenant, &observation).await.is_err() {
            // The durable pre-send attempt survives. A lost post-send write must
            // not turn an already completed provider call into a retry incentive.
            tracing::warn!("monetary observation not persisted; attempt remains unresolved");
        }
        Ok(())
    }
}

pub(crate) async fn capture<T>(
    state: &AppState,
    tenant: &str,
    entry: &crate::providers_api::ProviderEntry,
    model: &str,
    estimate: u64,
    work: impl std::future::Future<Output = T>,
) -> (T, bool) {
    let limit = entry.doc.spec.budgets.daily_total_tokens;
    if state.pg_pool().is_none() && limit.is_none() {
        return (work.await, true);
    }
    // For opt-in admission, tell the logical tier ledger when no attempt was
    // permitted at all. Legacy configurations retain their settlement rules.
    let admitted = Arc::new(std::sync::atomic::AtomicBool::new(limit.is_none()));
    let observer = Observer {
        store: state.pg_pool().map(|pool| MoneyStore(pool.clone())),
        budgets: state.budgets().clone(),
        config: entry.doc.metadata.name.clone(),
        limit,
        reservations: Default::default(),
        admitted: admitted.clone(),
        tenant: tenant.into(),
        invocation: uuid::Uuid::new_v4().simple().to_string(),
        provider: entry.doc.spec.provider.clone(),
        model: model.into(),
        estimate,
    };
    let result = CONTEXT
        .scope(
            Context::new(
                Arc::new(observer),
                entry.doc.spec.openrouter_provider.clone(),
            ),
            work,
        )
        .await;
    (result, admitted.load(std::sync::atomic::Ordering::SeqCst))
}

#[utoipa::path(get, path = "/v1/monetary/prices", responses((status = 200, body = [dto::MonetaryPrice])), tag = "reports")]
pub async fn monetary_prices(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> std::result::Result<Json<Vec<dto::MonetaryPrice>>, ApiError> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    Ok(Json(
        store(&state)?
            .prices(&ctx.tenant_id)
            .await?
            .into_iter()
            .map(convert)
            .collect(),
    ))
}

#[utoipa::path(post, path = "/v1/monetary/prices", request_body = dto::MonetaryPrice, responses((status = 200, body = dto::MonetaryPriceReceipt)), tag = "reports")]
pub async fn add_monetary_price(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<LiteralValue>,
) -> std::result::Result<Json<dto::MonetaryPriceReceipt>, ApiError> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    let request: dto::MonetaryPrice =
        serde_json::from_value(body.0).map_err(|e| KernelError::InvalidInput(e.to_string()))?;
    let price: PriceSnapshot = request.convert()?;
    store(&state)?.add_price(&ctx.tenant_id, &price).await?;
    Ok(Json(dto::MonetaryPriceReceipt { id: price.id }))
}

#[utoipa::path(post, path = "/v1/monetary/observations", request_body = dto::MonetaryObservation, responses((status = 200, body = dto::MonetaryCalculation)), tag = "reports")]
pub async fn add_monetary_observation(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<LiteralValue>,
) -> std::result::Result<Json<dto::MonetaryCalculation>, ApiError> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    let body: dto::MonetaryObservation =
        serde_json::from_value(body.0).map_err(|e| KernelError::InvalidInput(e.to_string()))?;
    let observation = Observation {
        id: body.id,
        attempt_id: body.attempt_id,
        previous_revision: body.previous_revision,
        usage: body.usage.convert(),
        accounted_units: body.accounted_units,
        resolved: body.resolved,
        evidence_ref: body.evidence_ref,
        price_id: body.price_id,
    };
    Ok(Json(
        store(&state)?
            .observe(&ctx.tenant_id, &observation)
            .await?
            .convert(),
    ))
}

#[derive(serde::Deserialize)]
pub struct MoneyQuery {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
}

#[utoipa::path(get, path = "/v1/reports/money", params(("from" = String, Query, description = "Required RFC 3339 inclusive submission bound"), ("to" = String, Query, description = "Required RFC 3339 exclusive submission bound")), responses((status = 200, body = dto::MonetaryReport)), tag = "reports")]
pub async fn monetary_report(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<MoneyQuery>,
) -> std::result::Result<Json<dto::MonetaryReport>, ApiError> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    let report = store(&state)?.report(&ctx.tenant_id, q.from, q.to).await?;
    Ok(Json(dto::MonetaryReport {
        scope: report.scope.into(),
        entire_tenant_bill: report.entire_tenant_bill,
        unrecorded_attempts: report.unrecorded_attempts,
        coverage: dto::MonetaryCoverage {
            attempts: report.coverage.attempts,
            priced: report.coverage.priced,
            missing_price: report.coverage.missing_price,
            missing_usage: report.coverage.missing_usage,
            unknown_categories: report.coverage.unknown_categories,
            unresolved: report.coverage.unresolved,
        },
        known_subtotals_micro_units: report.known_subtotals_micro_units,
        accounted_units: report.accounted_units,
        observed_input_tokens: report.observed_input_tokens,
        observed_output_tokens: report.observed_output_tokens,
        attempts: report
            .attempts
            .into_iter()
            .map(|a| dto::MonetaryAttempt {
                id: a.id,
                invocation_id: a.invocation_id,
                provider: a.provider,
                route: a.route,
                model: a.model,
                submitted_at: a.submitted_at.to_rfc3339(),
                revision: a.revision,
                accounted_units: a.accounted_units,
                usage: a.usage.convert(),
                unresolved: a.unresolved,
                calculation: a.calculation.convert(),
            })
            .collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use munarium_proto::mmp::v1::{self as pb, server_api_service_server::ServerApiService};
    use serde_json::{json, Value};
    use tower::ServiceExt;

    #[tokio::test]
    async fn monetary_rest_grpc_authority_tenant_isolation_and_large_amounts() {
        let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
            eprintln!("unavailable: isolated PostgreSQL not configured");
            return;
        };
        let tenant = format!("money-api-{}", uuid::Uuid::new_v4());
        let auth = crate::config::AuthMode::Static(vec![
            ("money-admin".into(), tenant.clone(), "mgmt".into()),
            ("money-reader".into(), tenant.clone(), "ro".into()),
            ("money-writer".into(), tenant.clone(), "rw".into()),
            (
                "other-admin".into(),
                format!("other-{tenant}"),
                "mgmt".into(),
            ),
        ]);
        let state = crate::providers_api::usage_tests::test_state_with_auth(Some(url), auth).await;
        let router = crate::rest::router(state.clone());
        let grpc = crate::grpc_api::ServerApiSvc::new(state.clone());
        let price = json!({"id":"fictional-large","provider":"fictional","route":"a".repeat(64),"model":"fixture","currency":"USD",
            "valid_from":"2020-01-01T00:00:00Z","valid_until":"2090-01-01T00:00:00Z","basis":"inclusive",
            "rates":{"input":{"micro_units":9007199254740993u64,"per_tokens":1},"output":{"micro_units":0,"per_tokens":1}}});
        let call = |token: &str, body: Vec<u8>| {
            let mut req = tonic::Request::new(pb::ServerApiRequest {
                body,
                ..Default::default()
            });
            req.metadata_mut()
                .insert("authorization", format!("Bearer {token}").parse().unwrap());
            req
        };
        // The normative integer field must not accept a JSON marker object,
        // including when another crate enables serde_json/arbitrary_precision.
        let mut invalid = price.clone();
        invalid["rates"]["input"]["micro_units"] = json!({"$serde_json::private::Number":"1"});
        assert_eq!(
            grpc.add_monetary_price(call("money-admin", serde_json::to_vec(&invalid).unwrap()))
                .await
                .unwrap_err()
                .code(),
            tonic::Code::InvalidArgument
        );
        let too_wide = serde_json::to_string(&price)
            .unwrap()
            .replace("9007199254740993", "18446744073709551616");
        assert_eq!(
            grpc.add_monetary_price(call("money-admin", too_wide.into_bytes()))
                .await
                .unwrap_err()
                .code(),
            tonic::Code::InvalidArgument
        );
        for token in ["money-reader", "money-writer"] {
            for (method, path, body) in [
                ("GET", "/v1/monetary/prices", Value::Null),
                ("POST", "/v1/monetary/prices", price.clone()),
                ("POST", "/v1/monetary/observations", json!({})),
                (
                    "GET",
                    "/v1/reports/money?from=2020-01-01T00:00:00Z&to=2090-01-01T00:00:00Z",
                    Value::Null,
                ),
            ] {
                let response = router
                    .clone()
                    .oneshot(
                        Request::builder()
                            .method(method)
                            .uri(path)
                            .header("authorization", format!("Bearer {token}"))
                            .header("content-type", "application/json")
                            .body(Body::from(serde_json::to_vec(&body).unwrap()))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), 403);
            }
            assert_eq!(
                grpc.add_monetary_price(call(token, serde_json::to_vec(&price).unwrap()))
                    .await
                    .unwrap_err()
                    .code(),
                tonic::Code::PermissionDenied
            );
            assert_eq!(
                grpc.monetary_prices(call(token, vec![]))
                    .await
                    .unwrap_err()
                    .code(),
                tonic::Code::PermissionDenied
            );
            assert_eq!(
                grpc.add_monetary_observation(call(token, b"{}".to_vec()))
                    .await
                    .unwrap_err()
                    .code(),
                tonic::Code::PermissionDenied
            );
        }
        let response = grpc
            .add_monetary_price(call("money-admin", serde_json::to_vec(&price).unwrap()))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(response.status, 200);
        let ledger = store(&state).unwrap();
        let attempt = ledger
            .begin(
                &tenant,
                "invocation",
                "fictional",
                &"a".repeat(64),
                "fixture",
                10,
            )
            .await
            .unwrap();
        let obs = json!({"id":"observation","attempt_id":attempt,"previous_revision":0,"usage":{"input":1,"output":0,"cache_read":null,"cache_write":null,"reasoning":null,"context":null,"unknown_categories":false},"accounted_units":1,"resolved":true,"evidence_ref":"fictional-receipt","price_id":null});
        let result = grpc
            .add_monetary_observation(call("money-admin", serde_json::to_vec(&obs).unwrap()))
            .await
            .unwrap()
            .into_inner();
        let calc: Value = serde_json::from_slice(&result.body).unwrap();
        assert_eq!(calc["micro_units"], "9007199254740993");
        let rest = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/reports/money?from=2020-01-01T00:00:00Z&to=2090-01-01T00:00:00Z")
                    .header("authorization", "Bearer money-admin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rest.status(), 200);
        let rest: Value =
            serde_json::from_slice(&to_bytes(rest.into_body(), usize::MAX).await.unwrap()).unwrap();
        for token in ["money-admin", "other-admin", "money-reader"] {
            let mut request = call(token, vec![]);
            request.get_mut().query_parameters = vec![
                pb::ServerApiParameter {
                    name: "from".into(),
                    value: "2020-01-01T00:00:00Z".into(),
                },
                pb::ServerApiParameter {
                    name: "to".into(),
                    value: "2090-01-01T00:00:00Z".into(),
                },
            ];
            let response = grpc.monetary_report(request).await;
            if token == "money-reader" {
                assert_eq!(response.unwrap_err().code(), tonic::Code::PermissionDenied);
                continue;
            }
            let value: Value =
                serde_json::from_slice(&response.unwrap().into_inner().body).unwrap();
            if token == "money-admin" {
                assert_eq!(rest, value);
                assert_eq!(
                    value["known_subtotals_micro_units"]["USD"],
                    "9007199254740993"
                );
            } else {
                assert_eq!(value["coverage"]["attempts"], 0);
            }
        }
        assert_eq!(
            grpc.add_monetary_observation(call("other-admin", serde_json::to_vec(&obs).unwrap()))
                .await
                .unwrap_err()
                .code(),
            tonic::Code::NotFound
        );
    }
}
