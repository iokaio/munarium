// SPDX-License-Identifier: Apache-2.0
//! The ModelProvider trait — the BYOK seam. Implementations (anthropic,
//! openai, openrouter, ollama in munarium-providers) own auth, retries, and dialects;
//! the kernel sees only these neutral shapes. Types only here: this crate
//! never performs a provider call.

use crate::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderId {
    Anthropic,
    Openai,
    Openrouter,
    Ollama,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub model: String,
    pub system: Option<String>,
    pub prompt: String,
    pub max_tokens: u32,
    pub temperature: Option<f64>,
    pub tools: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub text: String,
    pub stop_reason: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// sha-256 of the canonical request — invocation-provenance identity.
    pub request_hash: String,
}

/// Internal accounting evidence, separate from the stable numeric response DTO.
/// Completeness requires both counts and a provider-reported source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageEvidence {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub source: UsageSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageSource {
    ProviderReported,
    LegacyUnverified,
    Missing,
    Malformed,
}

impl UsageEvidence {
    /// Complete observed counts may reduce a reservation (including to zero).
    /// Otherwise retain at least the reservation and the available subtotal.
    /// This first slice has no per-component estimator. Overflow is an error,
    /// never a wrapped charge; the caller must retain unresolved capacity.
    pub fn accounted_units(&self, reserved: u64) -> Result<u64> {
        let subtotal = self
            .input_tokens
            .unwrap_or(0)
            .checked_add(self.output_tokens.unwrap_or(0))
            .ok_or_else(|| {
                crate::KernelError::Provider("completion usage total exceeds u64".into())
            })?;
        if self.source == UsageSource::ProviderReported
            && self.input_tokens.is_some()
            && self.output_tokens.is_some()
        {
            Ok(subtotal)
        } else {
            Ok(reserved.max(subtotal))
        }
    }
}

/// Companion to `CompletionResponse`; existing struct literals and trait
/// implementations remain valid. This is not a wire or persistence format.
#[derive(Debug, Clone)]
pub struct DetailedCompletionResponse {
    pub response: CompletionResponse,
    pub usage: UsageEvidence,
}

impl DetailedCompletionResponse {
    fn legacy(response: CompletionResponse) -> Self {
        Self {
            usage: UsageEvidence {
                input_tokens: Some(response.input_tokens),
                output_tokens: Some(response.output_tokens),
                source: UsageSource::LegacyUnverified,
            },
            response,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingRequest {
    pub model: String,
    pub inputs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingResponse {
    pub vectors: Vec<Vec<f32>>,
    pub dimensions: usize,
    pub request_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderHealth {
    pub healthy: bool,
    pub endpoint_fingerprint: String,
    /// Key validity / reachability detail — never key material.
    pub detail: String,
}

#[async_trait]
pub trait ModelProvider: Send + Sync {
    fn id(&self) -> ProviderId;
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse>;
    /// Request structured output for Server-owned protocols. Providers without
    /// constrained decoding retain prompt-based completion; consumers still
    /// validate the returned structure and provenance before using it.
    async fn complete_structured(
        &self,
        req: CompletionRequest,
        _schema: serde_json::Value,
    ) -> Result<CompletionResponse> {
        self.complete(req).await
    }
    /// Accounting-aware completion. Legacy providers cannot attest whether a
    /// zero was observed; the default calls the original method exactly once.
    async fn complete_detailed(
        &self,
        req: CompletionRequest,
    ) -> Result<DetailedCompletionResponse> {
        self.complete(req)
            .await
            .map(DetailedCompletionResponse::legacy)
    }
    /// Delegate to the legacy structured override, not to ordinary completion,
    /// so existing custom providers retain their schema handling.
    async fn complete_structured_detailed(
        &self,
        req: CompletionRequest,
        schema: serde_json::Value,
    ) -> Result<DetailedCompletionResponse> {
        self.complete_structured(req, schema)
            .await
            .map(DetailedCompletionResponse::legacy)
    }
    async fn embed(&self, req: EmbeddingRequest) -> Result<EmbeddingResponse>;
    async fn health(&self) -> Result<ProviderHealth>;
}

#[cfg(test)]
mod usage_tests {
    use super::*;

    #[test]
    fn incomplete_usage_never_refunds_known_work_or_the_reservation() {
        for (input, output, source, expected) in [
            (Some(0), Some(0), UsageSource::ProviderReported, 0),
            (Some(3), Some(2), UsageSource::ProviderReported, 5),
            (None, None, UsageSource::Missing, 10),
            (Some(3), None, UsageSource::ProviderReported, 10),
            (None, Some(15), UsageSource::ProviderReported, 15),
            (Some(15), None, UsageSource::Malformed, 15),
            (Some(0), Some(0), UsageSource::LegacyUnverified, 10),
            (Some(12), Some(3), UsageSource::LegacyUnverified, 15),
        ] {
            let usage = UsageEvidence {
                input_tokens: input,
                output_tokens: output,
                source,
            };
            assert_eq!(usage.accounted_units(10).unwrap(), expected, "{usage:?}");
        }
    }

    #[test]
    fn usage_total_checks_the_integer_boundary() {
        let mut usage = UsageEvidence {
            input_tokens: Some(u64::MAX),
            output_tokens: Some(0),
            source: UsageSource::ProviderReported,
        };
        assert_eq!(usage.accounted_units(10).unwrap(), u64::MAX);
        usage.output_tokens = Some(1);
        assert!(usage.accounted_units(10).is_err());
        usage.source = UsageSource::LegacyUnverified;
        assert!(usage.accounted_units(10).is_err());
    }
}
