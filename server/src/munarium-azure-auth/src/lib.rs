// SPDX-License-Identifier: Apache-2.0
//! Managed-identity tokens, cached until shortly before expiry.
//!
//! Every Azure-backed crate needs the same three things — ask IMDS for a token
//! scoped to a resource, cache it, refresh before it expires — and a second
//! subtly-different copy of that lifecycle is how you get an outage at the
//! 24-hour mark. One implementation, two consumers (Blob, Document
//! Intelligence), and any future Azure plane.
//!
//! Deliberately no Azure SDK: this is one GET against the platform identity
//! endpoint. Azure Container Apps/App Service inject `IDENTITY_ENDPOINT` plus
//! a rotating `IDENTITY_HEADER`; VM/AKS workloads fall back to IMDS.

use munarium_core::{KernelError, Result};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

/// Blob data plane.
pub const RESOURCE_STORAGE: &str = "https://storage.azure.com/";
/// Azure AI services, including Document Intelligence.
pub const RESOURCE_COGNITIVE: &str = "https://cognitiveservices.azure.com/";

/// IMDS is link-local and non-routable — unreachable off-Azure by design,
/// which is why every consumer also offers a key/SAS path for local tooling.
const IMDS_BASE: &str =
    "http://169.254.169.254/metadata/identity/oauth2/token?api-version=2018-02-01";

const PLATFORM_API_VERSION: &str = "2019-08-01";

/// Refresh this long before actual expiry.
const TOKEN_SKEW: Duration = Duration::from_secs(300);

struct Cached {
    value: String,
    expires_at: SystemTime,
}

#[derive(Clone)]
enum IdentityEndpoint {
    Imds,
    Platform { base_url: String, header: String },
    InvalidEnvironment(String),
}

impl IdentityEndpoint {
    fn from_env() -> Self {
        let endpoint = std::env::var("IDENTITY_ENDPOINT").ok();
        let header = std::env::var("IDENTITY_HEADER").ok();
        match (endpoint, header) {
            (Some(base_url), Some(header)) if !base_url.trim().is_empty() && !header.is_empty() => {
                Self::Platform { base_url, header }
            }
            (None, None) => Self::Imds,
            _ => Self::InvalidEnvironment(
                "IDENTITY_ENDPOINT and IDENTITY_HEADER must either both be set or both be absent"
                    .into(),
            ),
        }
    }
}

/// A cached managed-identity token for one resource.
pub struct ImdsTokenSource {
    resource: String,
    /// User-assigned identity client id. None = system-assigned.
    client_id: Option<String>,
    endpoint: IdentityEndpoint,
    http: reqwest::Client,
    cached: Mutex<Option<Cached>>,
}

impl ImdsTokenSource {
    pub fn new(resource: &str, client_id: Option<String>) -> Self {
        Self {
            resource: resource.to_string(),
            client_id,
            endpoint: IdentityEndpoint::from_env(),
            http: reqwest::Client::builder()
                // Both platform identity endpoints are local and answer in
                // milliseconds. A long timeout only prolongs failures when a
                // network black-holes the IMDS link-local range.
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
            cached: Mutex::new(None),
        }
    }

    /// The IMDS URL this source will call. Exposed for tests and for error
    /// messages that need to say exactly what was attempted.
    pub fn token_url(&self) -> String {
        let mut url = match &self.endpoint {
            IdentityEndpoint::Imds => {
                format!("{IMDS_BASE}&resource={}", percent_encode(&self.resource))
            }
            IdentityEndpoint::Platform { base_url, .. } => {
                let separator = if base_url.contains('?') { '&' } else { '?' };
                format!(
                    "{}{separator}api-version={PLATFORM_API_VERSION}&resource={}",
                    base_url.trim_end_matches('&'),
                    percent_encode(&self.resource)
                )
            }
            IdentityEndpoint::InvalidEnvironment(_) => {
                "<invalid-managed-identity-environment>".into()
            }
        };
        if let Some(id) = &self.client_id {
            url.push_str(&format!("&client_id={}", percent_encode(id)));
        }
        url
    }

    /// A valid bearer token, from cache when one is still good.
    pub async fn token(&self) -> Result<String> {
        if let Some(t) = self.cached.lock().expect("token lock").as_ref() {
            if t.expires_at > SystemTime::now() {
                return Ok(t.value.clone());
            }
        }
        // BOTH failure shapes get the same escape-hatch hint, because they
        // are the same operator situation — "this machine cannot mint a
        // managed-identity token":
        //   - off Azure, IMDS is unreachable (connect error);
        //   - ON an Azure VM with no usable identity — a GitHub-hosted CI
        //     runner, say — IMDS answers 400 "Identity not found".
        // An error that only hints on one branch reads as nonsense on the
        // other machine.
        const HINT: &str = "assign a managed identity to this workload, \
             or use the key/SAS auth mode (the off-Azure path)";
        let mut request = self.http.get(self.token_url());
        request = match &self.endpoint {
            IdentityEndpoint::Imds => request.header("Metadata", "true"),
            IdentityEndpoint::Platform { header, .. } => {
                request.header("X-IDENTITY-HEADER", header)
            }
            IdentityEndpoint::InvalidEnvironment(message) => {
                return Err(KernelError::Storage(format!(
                    "managed-identity environment is invalid: {message}. {HINT}"
                )))
            }
        };
        let resp = request.send().await.map_err(|e| {
            KernelError::Storage(format!(
                "managed-identity token request for {} failed: {e}. {HINT}",
                self.resource
            ))
        })?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(KernelError::Storage(format!(
                "managed-identity token request for {} returned {status}: {}. {HINT}",
                self.resource,
                truncate(&body)
            )));
        }
        let parsed: TokenResponse = serde_json::from_str(&body)
            .map_err(|e| KernelError::Storage(format!("managed-identity token parse: {e}")))?;
        // `expires_in` is classic IMDS. The App Service / Container Apps
        // endpoint (api-version 2019-08-01) sends `expires_on` — epoch
        // seconds, as a string — and no `expires_in`, so on the production
        // path the fixed 3600 s fallback was always what ran. That only
        // UNDER-cached while managed-identity tokens outlived an hour; a
        // shorter token lifetime policy would have flipped it into serving an
        // expired token.
        let now = SystemTime::now();
        let ttl = parsed
            .expires_in
            .as_deref()
            .and_then(|s| s.parse::<u64>().ok())
            .or_else(|| {
                let expires_on = parsed.expires_on.as_deref()?.parse::<u64>().ok()?;
                let now_secs = now.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_secs();
                Some(expires_on.saturating_sub(now_secs))
            })
            .unwrap_or(3600);
        let expires_at = now + Duration::from_secs(ttl).saturating_sub(TOKEN_SKEW);
        *self.cached.lock().expect("token lock") = Some(Cached {
            value: parsed.access_token.clone(),
            expires_at,
        });
        Ok(parsed.access_token)
    }
}

#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    /// Seconds. IMDS sends it as a STRING, not a number.
    #[serde(default)]
    expires_in: Option<String>,
    /// Epoch seconds, as a string. What the App Service / Container Apps
    /// identity endpoint sends INSTEAD of `expires_in`.
    #[serde(default)]
    expires_on: Option<String>,
}

/// Percent-encode for a query value (RFC 3986 unreserved set passes through).
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.as_bytes() {
        let c = *byte as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

pub fn truncate(s: &str) -> String {
    s.chars().take(300).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_with_endpoint(
        resource: &str,
        client_id: Option<String>,
        endpoint: IdentityEndpoint,
    ) -> ImdsTokenSource {
        let mut source = ImdsTokenSource::new(resource, client_id);
        source.endpoint = endpoint;
        source
    }

    #[test]
    fn resource_and_client_id_are_encoded_into_the_url() {
        let src = source_with_endpoint(
            RESOURCE_COGNITIVE,
            Some("abc-123".into()),
            IdentityEndpoint::Imds,
        );
        let url = src.token_url();
        assert!(url.contains("resource=https%3A%2F%2Fcognitiveservices.azure.com%2F"));
        assert!(url.contains("client_id=abc-123"));
        assert!(url.starts_with("http://169.254.169.254/"));
    }

    #[test]
    fn system_assigned_sends_no_client_id() {
        let src = source_with_endpoint(RESOURCE_STORAGE, None, IdentityEndpoint::Imds);
        assert!(!src.token_url().contains("client_id"));
    }

    #[test]
    fn container_apps_endpoint_uses_platform_api_and_client_id() {
        let src = source_with_endpoint(
            RESOURCE_STORAGE,
            Some("uami-client".into()),
            IdentityEndpoint::Platform {
                base_url: "http://localhost:42356/msi/token".into(),
                header: "rotating-secret".into(),
            },
        );
        let url = src.token_url();
        assert!(url.starts_with("http://localhost:42356/msi/token?"));
        assert!(url.contains("api-version=2019-08-01"));
        assert!(url.contains("resource=https%3A%2F%2Fstorage.azure.com%2F"));
        assert!(url.contains("client_id=uami-client"));
        assert!(!url.contains("rotating-secret"));
    }

    #[tokio::test]
    async fn incomplete_platform_environment_fails_before_http() {
        let src = source_with_endpoint(
            RESOURCE_STORAGE,
            None,
            IdentityEndpoint::InvalidEnvironment("both variables are required".into()),
        );
        let message = src
            .token()
            .await
            .expect_err("invalid environment")
            .to_string();
        assert!(message.contains("environment is invalid"), "{message}");
    }

    #[tokio::test]
    async fn token_failure_names_the_escape_hatch_wherever_it_runs() {
        // This test runs in two genuinely different worlds and must pass in
        // both: on a laptop IMDS is unreachable (connect error), while on a
        // GitHub-hosted CI runner — an Azure VM — IMDS ANSWERS and returns
        // 400 "Identity not found". Either way no token exists, and either
        // way the error must tell the operator the way out rather than
        // asserting which failure branch this machine happens to take.
        let src = source_with_endpoint(RESOURCE_STORAGE, None, IdentityEndpoint::Imds);
        let err = src.token().await.expect_err("no usable identity here");
        let msg = err.to_string();
        assert!(msg.contains("key/SAS"), "{msg}");
    }
}
