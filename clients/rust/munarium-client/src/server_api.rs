// SPDX-License-Identifier: Apache-2.0
//! Complete named Server API over either transport. Calls send once. Request
//! bodies retain the normative JSON/YAML/binary representation and precision.
use crate::{MunariumClientOptions, MunariumError, Result};
use futures_core::stream::BoxStream;
use futures_util::StreamExt;
use std::collections::HashMap;

#[path = "server_api_generated.rs"]
#[rustfmt::skip]
mod generated;

const MAX_BYTES: usize = 256 * 1024 * 1024;
pub type ApiStream = BoxStream<'static, Result<ApiResponse>>;
#[derive(Clone, Debug, Default)]
pub struct ApiRequest {
    pub path: HashMap<String, String>,
    pub query: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub content_type: String,
    pub source_headers: HashMap<String, String>,
    pub idempotency_key: Option<String>,
}
impl ApiRequest {
    pub fn json(value: &impl serde::Serialize) -> Result<Self> {
        Ok(Self {
            body: serde_json::to_vec(value).map_err(|_| invalid("invalid JSON request"))?,
            content_type: "application/json".into(),
            ..Self::default()
        })
    }
    pub fn with_path(mut self, name: &str, value: &str) -> Self {
        self.path.insert(name.into(), value.into());
        self
    }
}
#[derive(Clone, Debug)]
pub struct ApiResponse {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}
impl ApiResponse {
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_slice(&self.body).map_err(|_| invalid("invalid JSON response"))
    }
}
enum Backend {
    #[cfg(feature = "rest")]
    Rest(reqwest::Client),
    #[cfg(feature = "grpc")]
    Grpc(tonic::transport::Channel),
}
pub struct ServerApiClient {
    options: MunariumClientOptions,
    backend: Backend,
}
fn invalid(detail: &str) -> MunariumError {
    MunariumError::InvalidInput {
        detail: detail.into(),
    }
}
fn transport(error: impl std::fmt::Display) -> MunariumError {
    MunariumError::Transport {
        detail: error.to_string(),
        may_have_reached_server: true,
    }
}
#[cfg(feature = "rest")]
fn component(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
#[cfg(feature = "rest")]
fn uri(template: &str, input: &ApiRequest) -> Result<String> {
    let mut result = template.to_string();
    for (key, value) in &input.path {
        let marker = format!("{{{key}}}");
        if !result.contains(&marker) || matches!(value.as_str(), "" | "." | "..") {
            return Err(invalid("invalid path parameter"));
        }
        result = result.replace(&marker, &component(value));
    }
    if result.contains('{') {
        return Err(invalid("missing path parameter"));
    }
    for (i, (key, value)) in input.query.iter().enumerate() {
        result.push(if i == 0 { '?' } else { '&' });
        result.push_str(&format!("{}={}", component(key), component(value)));
    }
    Ok(result)
}
impl ServerApiClient {
    #[cfg(feature = "rest")]
    pub fn rest(options: MunariumClientOptions) -> Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(options.connect_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(transport)?;
        Ok(Self {
            options,
            backend: Backend::Rest(client),
        })
    }
    #[cfg(feature = "grpc")]
    pub async fn grpc(options: MunariumClientOptions) -> Result<Self> {
        let mut endpoint = tonic::transport::Channel::from_shared(options.endpoint.clone())
            .map_err(transport)?
            .connect_timeout(options.connect_timeout);
        if options.endpoint.starts_with("https://") {
            endpoint = endpoint
                .tls_config(tonic::transport::ClientTlsConfig::new().with_native_roots())
                .map_err(transport)?;
        }
        let channel = endpoint.connect().await.map_err(transport)?;
        Ok(Self {
            options,
            backend: Backend::Grpc(channel),
        })
    }
    #[cfg(feature = "grpc")]
    fn request(
        &self,
        input: ApiRequest,
        method: &str,
    ) -> Result<tonic::Request<munarium_proto::mmp::v1::ServerApiRequest>> {
        use munarium_proto::mmp::v1 as pb;
        let idem = input
            .idempotency_key
            .or_else(|| (method != "GET").then(crate::new_idem_key));
        let mut request = tonic::Request::new(pb::ServerApiRequest {
            path_parameters: input.path,
            query_parameters: input
                .query
                .into_iter()
                .map(|(name, value)| pb::ServerApiParameter { name, value })
                .collect(),
            body: input.body,
            content_type: input.content_type,
            source_headers: input.source_headers,
        });
        for (name, value) in [
            (
                "authorization",
                self.options.token.as_ref().map(|v| format!("Bearer {v}")),
            ),
            ("munarium-uid", self.options.uid.clone()),
            ("idempotency-key", idem),
        ] {
            if let Some(value) = value {
                request.metadata_mut().insert(
                    name,
                    value
                        .parse()
                        .map_err(|_| invalid("invalid request metadata"))?,
                );
            }
        }
        if method == "GET" {
            request.set_timeout(self.options.request_timeout);
        }
        Ok(request)
    }
    #[cfg(feature = "rest")]
    async fn http(
        &self,
        client: &reqwest::Client,
        method: &str,
        path: &str,
        media: &str,
        input: ApiRequest,
    ) -> Result<reqwest::Response> {
        let url = self.options.endpoint.trim_end_matches('/').to_string() + &uri(path, &input)?;
        let mut request = client.request(method.parse().map_err(transport)?, url);
        if let Some(token) = &self.options.token {
            request = request.bearer_auth(token);
        }
        if let Some(uid) = &self.options.uid {
            request = request.header("x-munarium-uid", uid);
        }
        if let Some(idem) = input
            .idempotency_key
            .or_else(|| (method != "GET").then(crate::new_idem_key))
        {
            request = request.header("idempotency-key", idem);
        }
        for (name, value) in input.source_headers {
            if !["x-filename", "x-content-sha256", "x-shape-ref"].contains(&name.as_str()) {
                return Err(invalid("unsupported source header"));
            }
            request = request.header(name, value);
        }
        if method == "GET" {
            request = request.timeout(self.options.request_timeout);
        }
        let response = request
            .header(
                "content-type",
                if input.content_type.is_empty() {
                    media
                } else {
                    &input.content_type
                },
            )
            .body(input.body)
            .send()
            .await
            .map_err(transport)?;
        if response.status().is_client_error() || response.status().is_server_error() {
            let status = response.status().as_u16();
            let body: serde_json::Value = response.json().await.unwrap_or_default();
            return Err(MunariumError::from_problem(status, None, &body));
        }
        Ok(response)
    }
    async fn call(
        &self,
        rpc: &'static str,
        method: &str,
        path: &str,
        media: &str,
        input: ApiRequest,
    ) -> Result<ApiResponse> {
        #[cfg(not(feature = "grpc"))]
        let _ = rpc;
        #[cfg(not(feature = "rest"))]
        let _ = (path, media);
        if input.body.len() > MAX_BYTES {
            return Err(invalid("request exceeds payload budget"));
        }
        match &self.backend {
            #[cfg(feature = "rest")]
            Backend::Rest(client) => {
                let response = self.http(client, method, path, media, input).await?;
                let status = response.status().as_u16();
                let content_type = response
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                let mut stream = response.bytes_stream();
                let mut body = Vec::new();
                while let Some(part) = stream.next().await {
                    let part = part.map_err(transport)?;
                    if body.len() + part.len() > MAX_BYTES {
                        return Err(transport("response exceeds payload budget"));
                    }
                    body.extend_from_slice(&part);
                }
                Ok(ApiResponse {
                    status,
                    content_type,
                    body,
                })
            }
            #[cfg(feature = "grpc")]
            Backend::Grpc(channel) => {
                let mut client = tonic::client::Grpc::new(channel.clone())
                    .max_decoding_message_size(MAX_BYTES + 65536)
                    .max_encoding_message_size(MAX_BYTES + 65536);
                client.ready().await.map_err(transport)?;
                let route = format!("/mmp.v1.ServerApiService/{rpc}")
                    .parse()
                    .map_err(transport)?;
                let response: tonic::Response<munarium_proto::mmp::v1::ServerApiResponse> = client
                    .unary(
                        self.request(input, method)?,
                        route,
                        tonic::codec::ProstCodec::default(),
                    )
                    .await
                    .map_err(crate::error::from_status)?;
                let response = response.into_inner();
                Ok(ApiResponse {
                    status: response.status as u16,
                    content_type: response.content_type,
                    body: response.body,
                })
            }
        }
    }
    async fn stream(
        &self,
        rpc: &'static str,
        method: &str,
        path: &str,
        media: &str,
        input: ApiRequest,
    ) -> Result<ApiStream> {
        if input.body.len() > MAX_BYTES {
            return Err(invalid("request exceeds payload budget"));
        }
        #[cfg(not(feature = "grpc"))]
        let _ = rpc;
        #[cfg(not(feature = "rest"))]
        let _ = (path, media);
        match &self.backend {
            #[cfg(feature = "rest")]
            Backend::Rest(client) => {
                let response = self.http(client, method, path, media, input).await?;
                let status = response.status().as_u16();
                let content_type = response
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                Ok(Box::pin(response.bytes_stream().map(move |p| {
                    p.map(|p| ApiResponse {
                        status,
                        content_type: content_type.clone(),
                        body: p.to_vec(),
                    })
                    .map_err(transport)
                })))
            }
            #[cfg(feature = "grpc")]
            Backend::Grpc(channel) => {
                let mut client = tonic::client::Grpc::new(channel.clone())
                    .max_decoding_message_size(MAX_BYTES + 65536)
                    .max_encoding_message_size(MAX_BYTES + 65536);
                client.ready().await.map_err(transport)?;
                let route = format!("/mmp.v1.ServerApiService/{rpc}")
                    .parse()
                    .map_err(transport)?;
                let response: tonic::Response<
                    tonic::Streaming<munarium_proto::mmp::v1::ServerApiResponse>,
                > = client
                    .server_streaming(
                        self.request(input, method)?,
                        route,
                        tonic::codec::ProstCodec::default(),
                    )
                    .await
                    .map_err(crate::error::from_status)?;
                Ok(Box::pin(response.into_inner().map(|p| {
                    p.map(|p| ApiResponse {
                        status: p.status as u16,
                        content_type: p.content_type,
                        body: p.body,
                    })
                    .map_err(crate::error::from_status)
                })))
            }
        }
    }
}
