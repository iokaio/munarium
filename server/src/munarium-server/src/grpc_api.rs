// SPDX-License-Identifier: Apache-2.0
//! Complete named RPC surface. Dispatch is in-process through the same bounded,
//! authorized and audited handlers as REST; no network request or alternate
//! service credential is involved. Clients cannot choose an arbitrary URI.
use crate::{rest, state::AppState};
use axum::{body::Body, http};
use munarium_proto::mmp::v1 as pb;
use std::{collections::HashMap, pin::Pin, sync::Arc};
use tokio_stream::{Stream, StreamExt};
use tonic::{Request, Response, Status};
use tonic_types::{ErrorDetails, StatusExt};
use tower::ServiceExt;

#[path = "grpc_api_generated.rs"]
#[rustfmt::skip]
mod generated;

pub type ApiStream = Pin<Box<dyn Stream<Item = Result<pb::ServerApiResponse, Status>> + Send>>;
pub struct ServerApiSvc {
    router: axum::Router,
}
impl ServerApiSvc {
    pub fn new(state: Arc<AppState>) -> Self {
        Self {
            router: rest::router(state),
        }
    }

    async fn dispatch(
        &self,
        request: Request<pb::ServerApiRequest>,
        method: &str,
        template: &str,
        content_type: &str,
    ) -> Result<http::Response<Body>, Status> {
        let (metadata, _, input) = request.into_parts();
        let uri = request_uri(template, &input)?;
        let mut request = http::Request::builder().method(method).uri(uri);
        // Forward only contract metadata. No caller-controlled Host, proxy
        // identity, tenant header or authorization override can enter here.
        for (from, to) in [
            ("authorization", "authorization"),
            ("munarium-uid", "x-munarium-uid"),
            ("idempotency-key", "idempotency-key"),
        ] {
            if let Some(value) = metadata.get(from) {
                request = request.header(
                    to,
                    value
                        .to_str()
                        .map_err(|_| Status::invalid_argument("invalid request metadata"))?,
                );
            }
        }
        for (name, value) in &input.source_headers {
            if !["x-filename", "x-content-sha256", "x-shape-ref"].contains(&name.as_str()) {
                return Err(Status::invalid_argument("unsupported source header"));
            }
            request = request.header(name, value);
        }
        let media = if input.content_type.is_empty() {
            content_type
        } else {
            &input.content_type
        };
        let request = request
            .header("content-type", media)
            .body(Body::from(input.body))
            .map_err(|_| Status::invalid_argument("invalid request"))?;
        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .map_err(|_| Status::internal("request dispatch failed"))?;
        if response.status().is_client_error() || response.status().is_server_error() {
            let code = response.status().as_u16();
            let body = axum::body::to_bytes(response.into_body(), rest::DEFAULT_BODY_LIMIT)
                .await
                .map_err(|_| Status::internal("invalid error response"))?;
            return Err(problem_status(code, &body));
        }
        Ok(response)
    }

    async fn unary(
        &self,
        request: Request<pb::ServerApiRequest>,
        method: &str,
        template: &str,
        media: &str,
    ) -> Result<Response<pb::ServerApiResponse>, Status> {
        let response = self.dispatch(request, method, template, media).await?;
        let status = response.status().as_u16().into();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = axum::body::to_bytes(response.into_body(), rest::MAX_SOURCE_BYTES)
            .await
            .map_err(|_| Status::resource_exhausted("response exceeds unary payload budget"))?;
        Ok(Response::new(pb::ServerApiResponse {
            status,
            content_type,
            body: body.to_vec(),
        }))
    }

    async fn stream(
        &self,
        request: Request<pb::ServerApiRequest>,
        method: &str,
        template: &str,
        media: &str,
    ) -> Result<Response<ApiStream>, Status> {
        let response = self.dispatch(request, method, template, media).await?;
        let status = response.status().as_u16().into();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        // Body fragments are polled on demand. Dropping the gRPC stream drops
        // the handler body, preserving its normal cancellation behavior.
        let stream = response.into_body().into_data_stream().map(move |part| {
            part.map(|body| pb::ServerApiResponse {
                status,
                content_type: content_type.clone(),
                body: body.to_vec(),
            })
            .map_err(|_| Status::internal("response stream interrupted"))
        });
        Ok(Response::new(Box::pin(stream)))
    }
}

fn component(value: &str) -> String {
    let mut result = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            result.push(b as char);
        } else {
            result.push_str(&format!("%{b:02X}"));
        }
    }
    result
}

fn request_uri(template: &str, input: &pb::ServerApiRequest) -> Result<String, Status> {
    let mut uri = template.to_string();
    for (key, value) in &input.path_parameters {
        let marker = format!("{{{key}}}");
        if !uri.contains(&marker) || value.is_empty() || value == "." || value == ".." {
            return Err(Status::invalid_argument("invalid path parameter"));
        }
        uri = uri.replace(&marker, &component(value));
    }
    if uri.contains('{') {
        return Err(Status::invalid_argument("missing path parameter"));
    }
    for (i, p) in input.query_parameters.iter().enumerate() {
        uri.push(if i == 0 { '?' } else { '&' });
        uri.push_str(&component(&p.name));
        uri.push('=');
        uri.push_str(&component(&p.value));
    }
    if uri.len() > 65536 {
        return Err(Status::invalid_argument("request parameters too large"));
    }
    Ok(uri)
}

fn problem_status(status: u16, body: &[u8]) -> Status {
    let value: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    let slug = value["type"]
        .as_str()
        .unwrap_or_default()
        .rsplit('/')
        .next()
        .unwrap_or_default();
    let detail = value["detail"].as_str().unwrap_or("request failed");
    let code = match status {
        400 | 422 => tonic::Code::InvalidArgument,
        401 => tonic::Code::Unauthenticated,
        403 => tonic::Code::PermissionDenied,
        404 => tonic::Code::NotFound,
        409 => tonic::Code::Aborted,
        410 | 412 => tonic::Code::FailedPrecondition,
        413 | 429 => tonic::Code::ResourceExhausted,
        501 => tonic::Code::Unimplemented,
        502..=504 => tonic::Code::Unavailable,
        _ => tonic::Code::Internal,
    };
    // Rich errors travel in HTTP/2 trailers, whose normal limit is 8 KiB.
    // Leave room for protobuf/base64 framing and the escaped status message.
    const METADATA_BUDGET: usize = 3500;
    let mut metadata = HashMap::new();
    let mut remaining = METADATA_BUDGET;
    let mut truncated = false;
    if let Some(fields) = value.as_object() {
        for (key, value) in fields {
            if !["type", "title", "status", "detail", "instance"].contains(&key.as_str()) {
                let mut encoded = value
                    .as_str()
                    .map(String::from)
                    .unwrap_or_else(|| value.to_string());
                if key == "gate_findings" {
                    if let Some(findings) = value.as_array() {
                        let mut kept = Vec::new();
                        encoded = "[]".into();
                        for finding in findings {
                            kept.push(finding);
                            let candidate = serde_json::to_string(&kept).unwrap();
                            if candidate.len() + key.len() + 160 > remaining {
                                kept.pop();
                                break;
                            }
                            encoded = candidate;
                        }
                        metadata.insert("findings_total".into(), findings.len().to_string());
                        if kept.len() < findings.len() {
                            metadata.insert("findings_truncated".into(), "true".into());
                        }
                        remaining = remaining.saturating_sub(120);
                    }
                }
                let size = key.len() + encoded.len() + 16;
                if size <= remaining {
                    remaining -= size;
                    metadata.insert(key.clone(), encoded);
                } else {
                    truncated = true;
                }
            }
        }
    }
    if truncated {
        metadata.insert("metadata_truncated".into(), "true".into());
    }
    if detail.len() > 512 {
        metadata.insert("detail_truncated".into(), "true".into());
    }
    let mut details = ErrorDetails::new();
    details.set_error_info(utf8_prefix(slug, 128), "mmp.ioka.io", metadata);
    Status::with_error_details(code, utf8_prefix(detail, 512), details)
}

fn utf8_prefix(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rich_errors_fit_trailers_without_changing_their_category() {
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "https://errors.example.invalid/policy-rejection",
            "detail": "説明".repeat(4000),
            "gate_findings": (0..100).map(|i| serde_json::json!({
                "code": format!("finding-{i}"), "detail": "x".repeat(200)
            })).collect::<Vec<_>>(),
            "large_extension": "x".repeat(10000)
        }))
        .unwrap();
        let status = problem_status(422, &body);
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().len() <= 512);
        assert!(status.details().len() < 4096);
        let info = status.get_error_details().error_info().unwrap().clone();
        assert_eq!(info.reason, "policy-rejection");
        assert_eq!(info.metadata["findings_total"], "100");
        assert_eq!(info.metadata["findings_truncated"], "true");
        assert_eq!(info.metadata["detail_truncated"], "true");
        assert_eq!(info.metadata["metadata_truncated"], "true");
        let findings: Vec<serde_json::Value> =
            serde_json::from_str(&info.metadata["gate_findings"]).unwrap();
        assert!(!findings.is_empty());
        assert!(findings.len() < 100);
    }

    #[test]
    fn path_values_cannot_change_the_selected_operation() {
        let mut request = pb::ServerApiRequest::default();
        request
            .path_parameters
            .insert("id".into(), "a/b?c=d&admin=true".into());
        assert_eq!(
            request_uri("/v1/sources/{id}", &request).unwrap(),
            "/v1/sources/a%2Fb%3Fc%3Dd%26admin%3Dtrue"
        );
        assert!(request_uri("/v1/sources", &request).is_err());
        request.path_parameters.insert("id".into(), "..".into());
        assert!(request_uri("/v1/sources/{id}", &request).is_err());
    }
}
