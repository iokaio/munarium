// SPDX-License-Identifier: Apache-2.0
//! Azure AI Document Intelligence as a `DocumentIntelligence` provider.
//!
//! This is the escalation path for documents the local extractors cannot
//! read: scanned PDFs, TIFF faxes, and — the case that motivated it — PDFs
//! whose page images are JBIG2 or CCITT encoded, for which no pure-Rust
//! decoder exists. It handles those natively.
//!
//! **It is off unless configured.** Every call costs money per page and
//! leaves the cluster, so nothing here runs by default; see
//! `docs/guides/document-intelligence.md` for why that default is what it is.
//!
//! No Azure SDK, matching the rest of the workspace: the surface needed is one
//! POST and a polling GET, over the rustls `reqwest` already present.
//!
//! Auth is managed identity by default (no secret exists), with an API-key
//! path through the same `CredentialRef` seam the BYOK provider keys use, for
//! tooling that runs outside Azure.

use async_trait::async_trait;
use base64::Engine as _;
use munarium_core::docintel::{AnalyzedDocument, DocumentIntelligence};
use munarium_core::{KernelError, Result};
use std::time::{Duration, Instant};

/// Pinned API version. Explicit because a hosted model's output is only
/// reproducible against a fixed version — it goes in the fingerprint.
pub const API_VERSION: &str = "2024-11-30";

/// `prebuilt-read` is OCR + text layout: the cheapest model that does what
/// this seam exists for. The richer prebuilts (layout, invoice) cost more and
/// return structure this pipeline would only flatten back to text.
pub const DEFAULT_MODEL: &str = "prebuilt-read";

const POLL_INTERVAL: Duration = Duration::from_millis(1200);

#[derive(Debug, Clone)]
pub enum DocIntelAuth {
    /// The workload's managed identity. No secret exists.
    ManagedIdentity { client_id: Option<String> },
    /// A resource key, for tooling that runs outside Azure.
    Key { key: String },
}

#[derive(Debug, Clone)]
pub struct AzureDocIntelConfig {
    /// e.g. `https://di-example.cognitiveservices.azure.com`
    pub endpoint: String,
    pub auth: DocIntelAuth,
    pub model: String,
    /// Refuse documents past this size rather than paying to fail: the
    /// service has its own limit and a rejected upload still costs latency.
    pub max_bytes: usize,
    /// Wall-clock ceiling for one document, polling included.
    pub timeout: Duration,
}

impl Default for AzureDocIntelConfig {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            auth: DocIntelAuth::ManagedIdentity { client_id: None },
            model: DEFAULT_MODEL.to_string(),
            max_bytes: 100 * 1024 * 1024,
            timeout: Duration::from_secs(180),
        }
    }
}

/// Media types this provider will attempt. Anything else is declined so the
/// caller skips it rather than paying for a guaranteed empty result.
const SUPPORTED: &[&str] = &[
    "application/pdf",
    "image/tiff",
    "image/jpeg",
    "image/png",
    "image/bmp",
    "image/heif",
];

pub struct AzureDocIntel {
    cfg: AzureDocIntelConfig,
    http: reqwest::Client,
    tokens: Option<munarium_azure_auth::ImdsTokenSource>,
    fingerprint: String,
}

impl AzureDocIntel {
    pub fn new(cfg: AzureDocIntelConfig) -> Result<Self> {
        if cfg.endpoint.trim().is_empty() {
            return Err(KernelError::InvalidInput(
                "document intelligence endpoint is required".into(),
            ));
        }
        if !cfg.endpoint.starts_with("https://") && !cfg.endpoint.starts_with("http://") {
            return Err(KernelError::InvalidInput(format!(
                "document intelligence endpoint '{}' must be an absolute URL",
                cfg.endpoint
            )));
        }
        let tokens = match &cfg.auth {
            DocIntelAuth::ManagedIdentity { client_id } => {
                Some(munarium_azure_auth::ImdsTokenSource::new(
                    munarium_azure_auth::RESOURCE_COGNITIVE,
                    client_id.clone(),
                ))
            }
            DocIntelAuth::Key { .. } => None,
        };
        let http = reqwest::Client::builder()
            // Per-request; the overall document budget is cfg.timeout.
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| KernelError::Provider(format!("http client: {e}")))?;
        let fingerprint = format!("azure-docintel/{}/{}", cfg.model, API_VERSION);
        Ok(Self {
            cfg,
            http,
            tokens,
            fingerprint,
        })
    }

    pub fn analyze_url(&self) -> String {
        format!(
            "{}/documentintelligence/documentModels/{}:analyze?api-version={API_VERSION}",
            self.cfg.endpoint.trim_end_matches('/'),
            self.cfg.model
        )
    }

    async fn authorize(&self, req: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder> {
        Ok(match (&self.cfg.auth, &self.tokens) {
            (DocIntelAuth::Key { key }, _) => req.header("Ocp-Apim-Subscription-Key", key),
            (_, Some(src)) => req.bearer_auth(src.token().await?),
            (_, None) => req,
        })
    }
}

#[async_trait]
impl DocumentIntelligence for AzureDocIntel {
    fn supports(&self, media_type: &str) -> bool {
        let m = media_type
            .split(';')
            .next()
            .unwrap_or(media_type)
            .trim()
            .to_ascii_lowercase();
        SUPPORTED.contains(&m.as_str())
    }

    fn id(&self) -> &'static str {
        "azure-docintel"
    }

    async fn analyze(&self, media_type: &str, bytes: &[u8]) -> Result<AnalyzedDocument> {
        if bytes.len() > self.cfg.max_bytes {
            return Err(KernelError::InvalidInput(format!(
                "document is {} bytes, over the {} byte document-intelligence limit",
                bytes.len(),
                self.cfg.max_bytes
            )));
        }
        let started = Instant::now();

        // Submit. base64Source keeps this to one JSON body rather than a
        // second signed-URL round trip.
        let body = serde_json::json!({
            "base64Source": base64::engine::general_purpose::STANDARD.encode(bytes),
        });
        let req = self
            .http
            .post(self.analyze_url())
            .header("content-type", "application/json")
            .json(&body);
        let resp = self
            .authorize(req)
            .await?
            .send()
            .await
            .map_err(|e| KernelError::Provider(format!("document intelligence submit: {e}")))?;

        let status = resp.status();
        // 202 + Operation-Location is the documented success shape.
        let operation = resp
            .headers()
            .get("operation-location")
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        if !status.is_success() {
            let detail = resp.text().await.unwrap_or_default();
            return Err(KernelError::Provider(format!(
                "document intelligence submit returned {status}: {}",
                munarium_azure_auth::truncate(&detail)
            )));
        }
        let Some(operation) = operation else {
            return Err(KernelError::Provider(
                "document intelligence accepted the document but returned no Operation-Location"
                    .into(),
            ));
        };

        // Poll. Bounded by the document budget, so a stuck analysis cannot
        // stall an index build indefinitely.
        loop {
            if started.elapsed() > self.cfg.timeout {
                return Err(KernelError::Provider(format!(
                    "document intelligence timed out after {:?} (media {media_type})",
                    self.cfg.timeout
                )));
            }
            tokio::time::sleep(POLL_INTERVAL).await;

            let req = self.http.get(&operation);
            let resp =
                self.authorize(req).await?.send().await.map_err(|e| {
                    KernelError::Provider(format!("document intelligence poll: {e}"))
                })?;
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if !status.is_success() {
                return Err(KernelError::Provider(format!(
                    "document intelligence poll returned {status}: {}",
                    munarium_azure_auth::truncate(&text)
                )));
            }
            let parsed: AnalyzeOperation = serde_json::from_str(&text).map_err(|e| {
                KernelError::Provider(format!("document intelligence response parse: {e}"))
            })?;
            match parsed.status.as_str() {
                "succeeded" => {
                    let result = parsed.analyze_result.unwrap_or_default();
                    return Ok(build_document(result, &self.fingerprint));
                }
                "failed" => {
                    return Err(KernelError::Provider(format!(
                        "document intelligence failed: {}",
                        munarium_azure_auth::truncate(&text)
                    )));
                }
                // running | notStarted
                _ => continue,
            }
        }
    }
}

/// Prefer per-page content so page boundaries survive as blank-line blocks
/// the chunker splits on; fall back to the flat `content` field.
fn build_document(result: AnalyzeResult, fingerprint: &str) -> AnalyzedDocument {
    let pages_analyzed = result.pages.len();
    let mut text = String::new();
    for page in &result.pages {
        let page_text = page
            .lines
            .iter()
            .map(|l| l.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if page_text.trim().is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(page_text.trim());
    }
    if text.trim().is_empty() {
        text = result.content.unwrap_or_default();
    }
    AnalyzedDocument {
        text,
        pages_analyzed,
        provider_fingerprint: fingerprint.to_string(),
    }
}

#[derive(Debug, serde::Deserialize)]
struct AnalyzeOperation {
    status: String,
    #[serde(rename = "analyzeResult")]
    analyze_result: Option<AnalyzeResult>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct AnalyzeResult {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    pages: Vec<AnalyzePage>,
}

#[derive(Debug, serde::Deserialize)]
struct AnalyzePage {
    #[serde(default)]
    lines: Vec<AnalyzeLine>,
}

#[derive(Debug, serde::Deserialize)]
struct AnalyzeLine {
    #[serde(default)]
    content: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> AzureDocIntelConfig {
        AzureDocIntelConfig {
            endpoint: "https://di-example.cognitiveservices.azure.com".into(),
            ..Default::default()
        }
    }

    #[test]
    fn analyze_url_pins_model_and_api_version() {
        let p = AzureDocIntel::new(cfg()).expect("provider");
        assert_eq!(
            p.analyze_url(),
            "https://di-example.cognitiveservices.azure.com/documentintelligence/documentModels/prebuilt-read:analyze?api-version=2024-11-30"
        );
    }

    #[test]
    fn only_scannable_media_is_accepted() {
        let p = AzureDocIntel::new(cfg()).expect("provider");
        for yes in [
            "application/pdf",
            "image/tiff",
            "IMAGE/PNG",
            "application/pdf; charset=binary",
        ] {
            assert!(p.supports(yes), "should support {yes}");
        }
        // Declining these matters: the caller skips the provider instead of
        // paying for a call that can only come back empty.
        for no in [
            "text/markdown",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "application/zip",
        ] {
            assert!(!p.supports(no), "should decline {no}");
        }
    }

    #[test]
    fn an_endpoint_is_required_and_must_be_absolute() {
        let mut c = cfg();
        c.endpoint = String::new();
        assert!(AzureDocIntel::new(c).is_err());
        let mut c = cfg();
        c.endpoint = "di-example.cognitiveservices.azure.com".into();
        assert!(AzureDocIntel::new(c).is_err());
    }

    #[test]
    fn the_fingerprint_names_the_model_and_carries_no_secret() {
        let mut c = cfg();
        c.auth = DocIntelAuth::Key {
            key: "SUPERSECRET".into(),
        };
        let p = AzureDocIntel::new(c).expect("provider");
        let doc = AnalyzedDocument::empty(p.fingerprint.clone());
        assert_eq!(
            doc.provider_fingerprint,
            "azure-docintel/prebuilt-read/2024-11-30"
        );
        assert!(!doc.provider_fingerprint.contains("SUPERSECRET"));
    }

    #[test]
    fn pages_become_blank_line_separated_blocks() {
        let result: AnalyzeResult = serde_json::from_str(
            r#"{"content":"flat fallback",
                "pages":[{"lines":[{"content":"Page one line one"},{"content":"line two"}]},
                         {"lines":[{"content":"Page two"}]}]}"#,
        )
        .expect("parse");
        let doc = build_document(result, "fp");
        assert_eq!(doc.pages_analyzed, 2);
        // Page boundary is a blank line — what `para@1` chunks on.
        assert_eq!(doc.text, "Page one line one\nline two\n\nPage two");
    }

    #[test]
    fn a_scan_with_no_recognized_text_is_empty_not_an_error() {
        let result: AnalyzeResult =
            serde_json::from_str(r#"{"pages":[{"lines":[]}]}"#).expect("parse");
        let doc = build_document(result, "fp");
        assert!(doc.is_empty());
        assert_eq!(doc.pages_analyzed, 1, "a blank page was still billed");
    }

    #[test]
    fn flat_content_is_the_fallback_when_pages_carry_no_lines() {
        let result: AnalyzeResult =
            serde_json::from_str(r#"{"content":"whole document text","pages":[]}"#).expect("parse");
        assert_eq!(build_document(result, "fp").text, "whole document text");
    }

    #[tokio::test]
    async fn an_oversized_document_is_refused_before_the_call() {
        // Refusing locally beats paying latency for a service-side rejection.
        let mut c = cfg();
        c.max_bytes = 16;
        let p = AzureDocIntel::new(c).expect("provider");
        let err = p
            .analyze("application/pdf", &[0u8; 32])
            .await
            .expect_err("too big");
        assert!(err.to_string().contains("over the"), "{err}");
    }
}
