// SPDX-License-Identifier: Apache-2.0
//! Management-only token ledger corrections. Monetary reconciliation is separate.
use crate::{error::ApiError, state::AppState};
use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    Json,
};
use munarium_api_types::{budget_evidence as dto, json::LiteralValue};
use munarium_core::{
    budget,
    provider::{UsageEvidence, UsageSource},
    KernelError, Result,
};
use std::sync::Arc;

fn decimal(value: &str) -> Result<u64> {
    if value.is_empty()
        || !value.bytes().all(|b| b.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return Err(KernelError::InvalidInput(
            "expected a canonical unsigned decimal string".into(),
        ));
    }
    value
        .parse()
        .map_err(|_| KernelError::InvalidInput("unsigned decimal value exceeds u64".into()))
}

fn usage(value: UsageEvidence) -> dto::BudgetUsage {
    dto::BudgetUsage {
        input_tokens: value.input_tokens.map(|v| v.to_string()),
        output_tokens: value.output_tokens.map(|v| v.to_string()),
        source: match value.source {
            UsageSource::ProviderReported => dto::UsageSource::ProviderReported,
            UsageSource::LegacyUnverified => dto::UsageSource::LegacyUnverified,
            UsageSource::Missing => dto::UsageSource::Missing,
            UsageSource::Malformed => dto::UsageSource::Malformed,
        },
    }
}

fn evidence(value: budget::BudgetEvidence) -> dto::BudgetEvidence {
    dto::BudgetEvidence {
        reservation_id: value.reservation_id,
        config: value.config,
        tier: value.tier,
        day: value.day,
        state: value.state,
        original_units: value.original_units.map(|v| v.to_string()),
        accounted_units: value.accounted_units.to_string(),
        revision: value.revision.to_string(),
        usage: value.usage.map(usage),
        estimator_revision: value.estimator_revision,
    }
}

fn correction(value: budget::BudgetCorrection) -> dto::BudgetCorrection {
    dto::BudgetCorrection {
        id: value.id,
        expected_revision: value.expected_revision.to_string(),
        accounted_units: value.accounted_units.to_string(),
        usage: usage(value.usage),
        evidence_ref: value.evidence_ref,
    }
}

#[utoipa::path(get, path = "/v1/budgets/{id}/evidence", params(("id" = String, Path)), responses((status = 200, body = dto::BudgetEvidence)), tag = "reports")]
pub async fn budget_evidence(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> std::result::Result<Json<dto::BudgetEvidence>, ApiError> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    let value =
        state
            .budgets()
            .evidence(&ctx.tenant_id, &id)
            .await?
            .ok_or(KernelError::NotFound {
                kind: "budget-reservation",
                id,
            })?;
    Ok(Json(evidence(value)))
}

#[utoipa::path(post, path = "/v1/budgets/{id}/adjustments", params(("id" = String, Path)), request_body = dto::BudgetCorrection, responses((status = 200, body = dto::BudgetEvidence)), tag = "reports")]
pub async fn reconcile_budget(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<LiteralValue>,
) -> std::result::Result<Json<dto::BudgetEvidence>, ApiError> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    let request: dto::BudgetCorrection =
        serde_json::from_value(body.0).map_err(|e| KernelError::InvalidInput(e.to_string()))?;
    let change = budget::BudgetCorrection {
        id: request.id,
        expected_revision: decimal(&request.expected_revision)?,
        accounted_units: decimal(&request.accounted_units)?,
        evidence_ref: request.evidence_ref,
        usage: UsageEvidence {
            input_tokens: request
                .usage
                .input_tokens
                .as_deref()
                .map(decimal)
                .transpose()?,
            output_tokens: request
                .usage
                .output_tokens
                .as_deref()
                .map(decimal)
                .transpose()?,
            source: match request.usage.source {
                dto::UsageSource::ProviderReported => UsageSource::ProviderReported,
                dto::UsageSource::LegacyUnverified => UsageSource::LegacyUnverified,
                dto::UsageSource::Missing => UsageSource::Missing,
                dto::UsageSource::Malformed => UsageSource::Malformed,
            },
        },
    };
    Ok(Json(evidence(
        state
            .budgets()
            .reconcile(&ctx.tenant_id, &id, &change)
            .await?,
    )))
}

#[utoipa::path(get, path = "/v1/budgets/{id}/adjustments", params(("id" = String, Path)), responses((status = 200, body = Vec<dto::BudgetAdjustment>)), tag = "reports")]
pub async fn budget_adjustments(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> std::result::Result<Json<Vec<dto::BudgetAdjustment>>, ApiError> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    Ok(Json(
        state
            .budgets()
            .adjustments(&ctx.tenant_id, &id)
            .await?
            .into_iter()
            .map(|a| dto::BudgetAdjustment {
                correction: correction(a.correction),
                previous: evidence(a.previous),
                result: evidence(a.result),
            })
            .collect(),
    ))
}

#[derive(serde::Deserialize)]
pub struct DayQuery {
    day: String,
}

#[utoipa::path(get, path = "/v1/reports/budget-usage", params(("day" = String, Query, description = "Original UTC accounting day, YYYY-MM-DD")), responses((status = 200, body = dto::BudgetUsageReport)), tag = "reports")]
pub async fn budget_usage_report(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<DayQuery>,
) -> std::result::Result<Json<dto::BudgetUsageReport>, ApiError> {
    let ctx = crate::rest::auth_ctx(&state, &headers)?;
    ctx.require_mgmt()?;
    let date = chrono::NaiveDate::parse_from_str(&q.day, "%Y-%m-%d")
        .map_err(|_| KernelError::InvalidInput("day must be YYYY-MM-DD".into()))?;
    if q.day.len() != 10
        || chrono::Datelike::year(&date) < 1
        || date.format("%Y-%m-%d").to_string() != q.day
    {
        return Err(KernelError::InvalidInput("day must be YYYY-MM-DD".into()).into());
    }
    let rows = state
        .budgets()
        .evidence_for_day(&ctx.tenant_id, &q.day)
        .await?;
    let mut report = dto::BudgetUsageReport {
        scope: "recorded_token_reservations".into(),
        day: q.day,
        reservations: vec![],
        complete_observations: 0,
        partial_observations: 0,
        unknown_observations: 0,
        unrecorded_invocations: None,
    };
    for row in rows {
        match row.usage {
            Some(UsageEvidence {
                source: UsageSource::ProviderReported,
                input_tokens: Some(input),
                output_tokens: Some(output),
            }) if input.checked_add(output).is_some() => report.complete_observations += 1,
            Some(UsageEvidence {
                source: UsageSource::ProviderReported,
                input_tokens,
                output_tokens,
            }) if input_tokens.is_some() != output_tokens.is_some() => {
                report.partial_observations += 1
            }
            _ => report.unknown_observations += 1,
        }
        report.reservations.push(evidence(row));
    }
    Ok(Json(report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use munarium_proto::mmp::v1::{self as pb, server_api_service_server::ServerApiService};
    use serde_json::{json, Value};
    use tower::ServiceExt;

    async fn call(
        router: &axum::Router,
        method: &str,
        path: &str,
        token: &str,
        body: Value,
    ) -> (StatusCode, Value) {
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
        let status = response.status();
        let body = to_bytes(response.into_body(), 1_000_000).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    #[tokio::test]
    async fn management_corrections_are_exact_tenant_scoped_and_report_unknown_coverage() {
        let mut databases = vec![None];
        if let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") {
            databases.push(Some(url));
        }
        for database in databases {
            let tenant = format!("budget-api-{}", uuid::Uuid::new_v4());
            let state = crate::providers_api::usage_tests::test_state_with_auth(
                database,
                crate::config::AuthMode::Static(vec![
                    ("admin".into(), tenant.clone(), "mgmt".into()),
                    ("reader".into(), tenant.clone(), "ro".into()),
                    ("writer".into(), tenant.clone(), "rw".into()),
                    ("other".into(), format!("other-{tenant}"), "mgmt".into()),
                ]),
            )
            .await;
            let budget::BudgetOutcome::Granted(reservation) = state
                .budgets()
                .reserve(&tenant, "cfg", "fast", 100, Some(100))
                .await
                .unwrap()
            else {
                panic!("grant")
            };
            state.budgets().settle(&reservation, None).await.unwrap();
            let router = crate::rest::router(state.clone());
            let grpc = crate::grpc_api::ServerApiSvc::new(state.clone());
            let grpc_call = |token: &str, body: &Value| {
                let mut req = tonic::Request::new(pb::ServerApiRequest {
                    path_parameters: [("id".into(), reservation.id.clone())].into(),
                    body: serde_json::to_vec(body).unwrap(),
                    ..Default::default()
                });
                req.metadata_mut()
                    .insert("authorization", format!("Bearer {token}").parse().unwrap());
                req
            };
            let path = format!("/v1/budgets/{}/adjustments", reservation.id);
            let change = json!({"id":"receipt-1","expected_revision":"0","accounted_units":"9007199254740993",
                "usage":{"input_tokens":"9007199254740990","output_tokens":"3","source":"provider_reported"},"evidence_ref":"fictional-receipt"});
            for token in ["reader", "writer"] {
                assert_eq!(
                    grpc.reconcile_budget(grpc_call(token, &change))
                        .await
                        .unwrap_err()
                        .code(),
                    tonic::Code::PermissionDenied
                );
                assert_eq!(
                    call(&router, "POST", &path, token, change.clone()).await.0,
                    StatusCode::FORBIDDEN
                );
            }
            assert_eq!(
                call(&router, "POST", &path, "other", change.clone())
                    .await
                    .0,
                StatusCode::NOT_FOUND
            );
            for bad in [
                json!("01"),
                json!("18446744073709551616"),
                json!(9007199254740993u64),
                json!({"$serde_json::private::Number":"1"}),
            ] {
                let mut request = change.clone();
                request["accounted_units"] = bad;
                assert_eq!(
                    grpc.reconcile_budget(grpc_call("admin", &request))
                        .await
                        .unwrap_err()
                        .code(),
                    tonic::Code::InvalidArgument
                );
                assert_eq!(
                    call(&router, "POST", &path, "admin", request).await.0,
                    StatusCode::BAD_REQUEST
                );
            }
            let (status, first) = call(&router, "POST", &path, "admin", change.clone()).await;
            assert_eq!(status, StatusCode::OK, "{first}");
            assert_eq!(first["accounted_units"], "9007199254740993");
            assert_eq!(first["revision"], "1");
            assert_eq!(first["original_units"], "100");
            let replay = grpc
                .reconcile_budget(grpc_call("admin", &change))
                .await
                .unwrap()
                .into_inner();
            assert_eq!(
                serde_json::from_slice::<Value>(&replay.body).unwrap(),
                first
            );
            assert_eq!(
                grpc.budget_evidence(grpc_call("other", &Value::Null))
                    .await
                    .unwrap_err()
                    .code(),
                tonic::Code::NotFound
            );
            let native_history = grpc
                .budget_adjustments(grpc_call("admin", &Value::Null))
                .await
                .unwrap()
                .into_inner();
            assert_eq!(
                serde_json::from_slice::<Value>(&native_history.body)
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            assert_eq!(
                call(&router, "POST", &path, "admin", change.clone())
                    .await
                    .1,
                first
            );
            let mut conflicting = change.clone();
            conflicting["accounted_units"] = json!("9007199254740994");
            assert_eq!(
                call(&router, "POST", &path, "admin", conflicting).await.0,
                StatusCode::UNPROCESSABLE_ENTITY
            );
            let (_, history) = call(&router, "GET", &path, "admin", Value::Null).await;
            assert_eq!(history.as_array().unwrap().len(), 1);
            assert_eq!(history[0]["previous"]["usage"], Value::Null);
            let (_, report) = call(
                &router,
                "GET",
                &format!("/v1/reports/budget-usage?day={}", reservation.day),
                "admin",
                Value::Null,
            )
            .await;
            assert_eq!(report["complete_observations"], 1);
            assert_eq!(report["unrecorded_invocations"], Value::Null);
            assert_eq!(report["reservations"][0], first);
            let mut native_report = tonic::Request::new(pb::ServerApiRequest {
                query_parameters: vec![pb::ServerApiParameter {
                    name: "day".into(),
                    value: reservation.day.clone(),
                }],
                ..Default::default()
            });
            native_report
                .metadata_mut()
                .insert("authorization", "Bearer admin".parse().unwrap());
            let native_report = grpc
                .budget_usage_report(native_report)
                .await
                .unwrap()
                .into_inner();
            assert_eq!(
                serde_json::from_slice::<Value>(&native_report.body).unwrap(),
                report
            );
            for day in [
                "invalid",
                "0000-01-01",
                "-0001-01-01",
                "10000-01-01",
                "2026-02-30",
            ] {
                assert_eq!(
                    call(
                        &router,
                        "GET",
                        &format!("/v1/reports/budget-usage?day={day}"),
                        "admin",
                        Value::Null
                    )
                    .await
                    .0,
                    StatusCode::BAD_REQUEST
                );
            }
            for usage in [
                None,
                Some(UsageEvidence {
                    input_tokens: Some(1),
                    output_tokens: None,
                    source: UsageSource::ProviderReported,
                }),
                Some(UsageEvidence {
                    input_tokens: Some(u64::MAX),
                    output_tokens: Some(1),
                    source: UsageSource::ProviderReported,
                }),
            ] {
                let budget::BudgetOutcome::Granted(row) = state
                    .budgets()
                    .reserve(&tenant, "quality", "fast", 10, Some(100))
                    .await
                    .unwrap()
                else {
                    panic!("fixture grant")
                };
                state
                    .budgets()
                    .settle_with_evidence(&row, None, usage)
                    .await
                    .unwrap();
            }
            let (_, quality) = call(
                &router,
                "GET",
                &format!("/v1/reports/budget-usage?day={}", reservation.day),
                "admin",
                Value::Null,
            )
            .await;
            assert_eq!(quality["complete_observations"], 1);
            assert_eq!(quality["partial_observations"], 1);
            assert_eq!(
                quality["unknown_observations"], 2,
                "legacy absence and an overflowing total are not complete observations"
            );
        }
    }
}
