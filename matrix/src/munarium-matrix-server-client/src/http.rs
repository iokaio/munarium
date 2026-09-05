// SPDX-License-Identifier: Apache-2.0
//! The real HTTP client.
//!
//! One pooled client with HTTP/2 enabled, rustls only. The bearer token is
//! resolved once at construction from a secret reference and never logged; the
//! `Debug` impl below exists to make that structural rather than a habit.

use crate::*;
use munarium_matrix_types::contract::EvidenceManifest;
use std::time::Duration;

pub struct HttpServerClient {
    base_url: String,
    token: String,
    tenant_header: Option<String>,
    client: reqwest::Client,
}

impl std::fmt::Debug for HttpServerClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The token is deliberately absent. A struct that derives Debug with a
        // secret in it will print the secret the first time someone adds a
        // `?self` to a tracing call.
        f.debug_struct("HttpServerClient")
            .field("base_url", &self.base_url)
            .field("token", &"<redacted>")
            .finish()
    }
}

impl HttpServerClient {
    pub fn new(base_url: &str, token: &str, timeout: Duration) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            // Intra-environment calls to a known peer: prior knowledge avoids
            // the upgrade round-trip entirely.
            .http2_prior_knowledge()
            .build()
            .map_err(|e| ServerError::Transport(e.to_string()))?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            // The server's uid contract requires a uid on EVERY /v1 call; leaving
            // this None was a defect the mock never surfaced (2026-08-28).
            tenant_header: Some("matrix".to_string()),
            client,
        })
    }

    /// A client that negotiates rather than assuming h2 — needed when the peer
    /// is behind an ingress that only speaks HTTP/1.1.
    pub fn new_http1(base_url: &str, token: &str, timeout: Duration) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| ServerError::Transport(e.to_string()))?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            tenant_header: Some("matrix".to_string()),
            client,
        })
    }

    pub fn with_uid(mut self, uid: &str) -> Self {
        self.tenant_header = Some(uid.to_string());
        self
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut r = self
            .client
            .request(method, self.url(path))
            .bearer_auth(&self.token);
        // The server's uid contract: every /v1 call asserts one.
        if let Some(uid) = &self.tenant_header {
            r = r.header("X-Munarium-Uid", uid);
        }
        r
    }

    /// Turn a non-2xx response into a `Problem`, preferring the server's
    /// problem+json slug over its English text.
    async fn check(resp: reqwest::Response) -> Result<reqwest::Response> {
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        let (slug, detail) = match serde_json::from_str::<serde_json::Value>(&body) {
            Ok(v) => (
                v.get("type")
                    .and_then(|t| t.as_str())
                    .and_then(|t| t.rsplit('/').next())
                    .unwrap_or("unknown")
                    .to_string(),
                v.get("detail")
                    .and_then(|d| d.as_str())
                    .unwrap_or(&body)
                    .to_string(),
            ),
            Err(_) => ("unknown".to_string(), body),
        };
        Err(ServerError::Problem {
            status,
            slug,
            detail,
        })
    }

    async fn json<T: serde::de::DeserializeOwned>(resp: reqwest::Response) -> Result<T> {
        let text = resp
            .text()
            .await
            .map_err(|e| ServerError::Transport(e.to_string()))?;
        serde_json::from_str(&text).map_err(|e| ServerError::Malformed(format!("{e}: {text:.200}")))
    }
}

#[async_trait]
impl ServerClient for HttpServerClient {
    async fn server_version(&self) -> Result<String> {
        let resp = self
            .request(reqwest::Method::GET, "/version")
            .send()
            .await
            .map_err(|e| ServerError::Transport(e.to_string()))?;
        let v: serde_json::Value = Self::json(Self::check(resp).await?).await?;
        v.get("version")
            .and_then(|s| s.as_str())
            .map(String::from)
            .ok_or_else(|| ServerError::Malformed("no `version` field".into()))
    }

    async fn seal_evidence(
        &self,
        manifest: &EvidenceManifest,
        bytes: &[u8],
        idempotency_key: Option<&str>,
    ) -> Result<String> {
        // The inline form: manifest + bytes in one request, which is what keeps
        // the turn path at one server round-trip. Above the cap the plan's
        // grant flow applies; this client refuses rather than silently
        // half-sealing, because a partial seal is worse than no seal.
        const INLINE_CAP: usize = 1024 * 1024;
        if bytes.len() > INLINE_CAP {
            return Err(ServerError::Problem {
                status: 413,
                slug: "result-too-large".into(),
                detail: format!(
                    "artifact is {} bytes, over the {INLINE_CAP}-byte inline cap; the grant flow \
                     lands with the server's evidence plane",
                    bytes.len()
                ),
            });
        }
        let body = serde_json::json!({
            "manifest": manifest,
            "bytes_base64": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes),
        });
        let mut req = self
            .request(reqwest::Method::POST, "/v1/evidence")
            .json(&body);
        if let Some(k) = idempotency_key {
            req = req.header("Idempotency-Key", k);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| ServerError::Transport(e.to_string()))?;
        let v: serde_json::Value = Self::json(Self::check(resp).await?).await?;
        v.get("evidence_id")
            .and_then(|s| s.as_str())
            .map(String::from)
            .ok_or_else(|| ServerError::Malformed("no `evidence_id` in seal response".into()))
    }

    async fn get_evidence(&self, evidence_id: &str) -> Result<EvidenceManifest> {
        let resp = self
            .request(reqwest::Method::GET, &format!("/v1/evidence/{evidence_id}"))
            .send()
            .await
            .map_err(|e| ServerError::Transport(e.to_string()))?;
        Self::json(Self::check(resp).await?).await
    }

    async fn bulk_upload(
        &self,
        label: &str,
        documents: &[UploadDocument],
    ) -> Result<UploadOutcome> {
        // Open a session with the manifest; the server diffs it against what it
        // already holds and answers with the work that is actually needed. That
        // diff is the reason mode A uses this plane rather than streaming
        // ingest: a re-sync of an unchanged corpus uploads nothing.
        let manifest: Vec<serde_json::Value> = documents
            .iter()
            .map(|d| {
                serde_json::json!({
                    "filename": d.path,
                    "sha256": d.content_hash(),
                    "bytes_len": d.bytes.len(),
                    "media_type": d.media_type,
                })
            })
            .collect();
        let resp = self
            .request(reqwest::Method::POST, "/v1/ingest/bulk")
            .json(&serde_json::json!({ "label": label, "files": manifest }))
            .send()
            .await
            .map_err(|e| ServerError::Transport(e.to_string()))?;
        let open: serde_json::Value = Self::json(Self::check(resp).await?).await?;
        let bulk_id = open
            .get("bulk_id")
            .and_then(|s| s.as_str())
            .ok_or_else(|| ServerError::Malformed("no `bulk_id`".into()))?
            .to_string();
        let needed: std::collections::BTreeSet<String> = open
            .get("needed")
            .and_then(|n| n.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| {
                        v.as_str().map(String::from).or_else(|| {
                            v.get("filename").and_then(|f| f.as_str()).map(String::from)
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let mut outcome = UploadOutcome {
            skipped_existing: documents.len().saturating_sub(needed.len()) as u64,
            ..Default::default()
        };

        // Chunks of at most 500 files, the server's documented ceiling.
        let to_send: Vec<&UploadDocument> = documents
            .iter()
            .filter(|d| needed.contains(&d.path))
            .collect();
        for chunk in to_send.chunks(500) {
            let files: Vec<serde_json::Value> = chunk
                .iter()
                .map(|d| {
                    serde_json::json!({
                        "filename": d.path,
                        "media_type": d.media_type,
                        "content_base64": base64::Engine::encode(
                            &base64::engine::general_purpose::STANDARD, &d.bytes),
                        "metadata": d.metadata.iter().cloned().collect::<std::collections::BTreeMap<_,_>>(),
                    })
                })
                .collect();
            let resp = self
                .request(
                    reqwest::Method::POST,
                    &format!("/v1/ingest/bulk/{bulk_id}/chunk"),
                )
                .json(&serde_json::json!({ "files": files }))
                .send()
                .await
                .map_err(|e| ServerError::Transport(e.to_string()))?;
            let v: serde_json::Value = Self::json(Self::check(resp).await?).await?;
            outcome.stored += v.get("stored").and_then(|n| n.as_u64()).unwrap_or(0);
            outcome.failed += v.get("failed").and_then(|n| n.as_u64()).unwrap_or(0);
            outcome.skipped_existing += v
                .get("skipped_existing")
                .and_then(|n| n.as_u64())
                .unwrap_or(0);
        }

        let resp = self
            .request(
                reqwest::Method::POST,
                &format!("/v1/ingest/bulk/{bulk_id}/complete"),
            )
            .send()
            .await
            .map_err(|e| ServerError::Transport(e.to_string()))?;
        Self::check(resp).await?;
        Ok(outcome)
    }

    async fn slice_facts(
        &self,
        version_id: &str,
        as_of_seq: Option<u64>,
    ) -> Result<Vec<LedgerFact>> {
        let mut path = format!("/v1/versions/{version_id}/facts");
        if let Some(seq) = as_of_seq {
            path.push_str(&format!("?as_of_seq={seq}"));
        }
        let resp = self
            .request(reqwest::Method::GET, &path)
            .send()
            .await
            .map_err(|e| ServerError::Transport(e.to_string()))?;
        let v: serde_json::Value = Self::json(Self::check(resp).await?).await?;
        let facts = v
            .get("facts")
            .and_then(|f| f.as_array())
            .ok_or_else(|| ServerError::Malformed("no `facts` array".into()))?;
        Ok(facts
            .iter()
            .filter_map(|f| {
                Some(LedgerFact {
                    // The server returns full ClaimDto rows, whose id field is
                    // `id`. Reading `claim_id` here was a latent parity bug
                    // the mock could not catch: it builds LedgerFact directly.
                    // Found the day the real server's DTO was read.
                    claim_id: f
                        .get("id")
                        .or_else(|| f.get("claim_id"))
                        .and_then(|s| s.as_str())
                        .map(String::from),
                    subject: f.get("subject")?.as_str()?.to_string(),
                    key: f.get("key")?.as_str()?.to_string(),
                    value: f.get("value")?.as_str()?.to_string(),
                    seq: f.get("seq").and_then(|s| s.as_u64()).unwrap_or(0),
                    status: f.get("status").and_then(|s| s.as_str()).map(String::from),
                    provenance: f
                        .get("provenance")
                        .and_then(|s| s.as_str())
                        .map(String::from),
                    origin_kind: f
                        .get("origin")
                        .and_then(|o| o.get("kind"))
                        .and_then(|s| s.as_str())
                        .map(String::from),
                })
            })
            .collect())
    }

    async fn head_seq(&self, version_id: &str) -> Result<u64> {
        let resp = self
            .request(
                reqwest::Method::GET,
                &format!("/v1/versions/{version_id}/head"),
            )
            .send()
            .await
            .map_err(|e| ServerError::Transport(e.to_string()))?;
        let v: serde_json::Value = Self::json(Self::check(resp).await?).await?;
        Ok(v.get("head_seq").and_then(|s| s.as_u64()).unwrap_or(0))
    }

    async fn file_finding(&self, req: &FindingRequest) -> Result<String> {
        let resp = self
            .request(
                reqwest::Method::POST,
                &format!("/v1/versions/{}/findings", req.version_id),
            )
            // The route takes a batch and is idempotent by CONTENT:
            // identity is (rule_id, detail.evidence_ref, detail.claim_id), so a
            // replayed reconciliation files nothing twice. No Idempotency-Key.
            .json(&serde_json::json!({
                "findings": [{
                    "rule_id": req.rule_id,
                    "severity": req.severity,
                    "message": req.message,
                    "scope_path": req.scope_path,
                    "detail": req.detail,
                }]
            }))
            .send()
            .await
            .map_err(|e| ServerError::Transport(e.to_string()))?;
        let v: serde_json::Value = Self::json(Self::check(resp).await?).await?;
        // The server returns counts, not an id; the seq it stamped is the
        // finding's identity for a pinned read, so that is what comes back.
        let seq = v.get("seq").and_then(|s| s.as_u64()).unwrap_or(0);
        let recorded = v.get("recorded").and_then(|s| s.as_u64()).unwrap_or(0);
        Ok(format!(
            "seq-{seq}:{}",
            if recorded > 0 {
                "recorded"
            } else {
                "duplicate"
            }
        ))
    }

    async fn propose_claim(
        &self,
        req: &crate::ProposeClaimRequest,
        idempotency_key: &str,
    ) -> Result<crate::ProposeOutcome> {
        let resp = self
            .request(
                reqwest::Method::POST,
                &format!("/v1/versions/{}/claims", req.version_id),
            )
            .header("idempotency-key", idempotency_key)
            .json(&serde_json::json!({
                "claim_type": req.claim_type,
                "subject": req.subject,
                "key": req.key,
                "value": req.value,
                "scope_path": req.scope_path,
                "supersedes_id": req.supersedes_id,
                "evidence": req.evidence,
                "origin": req.origin,
            }))
            .send()
            .await
            .map_err(|e| ServerError::Transport(e.to_string()))?;
        let v: serde_json::Value = Self::json(Self::check(resp).await?).await?;
        let claim = v
            .get("claim")
            .ok_or_else(|| ServerError::Malformed("no `claim` in propose response".into()))?;
        Ok(crate::ProposeOutcome {
            claim_id: claim
                .get("id")
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_string(),
            status: claim
                .get("status")
                .and_then(|s| s.as_str())
                .unwrap_or("accepted")
                .to_string(),
            head_seq: v.get("head_seq").and_then(|s| s.as_u64()).unwrap_or(0),
            findings: v
                .get("findings")
                .and_then(|f| f.as_array())
                .map(|fs| {
                    fs.iter()
                        .filter_map(|f| f.get("rule_id").and_then(|r| r.as_str()))
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_prints_the_token() {
        let c = HttpServerClient::new(
            "http://localhost:8080",
            "super-secret-token",
            Duration::from_secs(5),
        )
        .unwrap();
        let printed = format!("{c:?}");
        assert!(!printed.contains("super-secret-token"), "{printed}");
        assert!(printed.contains("redacted"));
    }

    #[test]
    fn server_failures_map_to_the_right_refusal_class() {
        use munarium_matrix_core::RefusalClass;
        let denied = ServerError::Problem {
            status: 403,
            slug: "forbidden".into(),
            detail: "no".into(),
        };
        assert_eq!(denied.to_refusal().class, RefusalClass::Denied);
        assert!(!denied.to_refusal().class.retryable());

        let down = ServerError::Transport("connection refused".into());
        assert_eq!(down.to_refusal().class, RefusalClass::Unavailable);
        assert!(down.to_refusal().class.retryable());

        let bad = ServerError::Problem {
            status: 422,
            slug: "invalid".into(),
            detail: "no".into(),
        };
        assert_eq!(bad.to_refusal().class, RefusalClass::Invalid);
    }
}
