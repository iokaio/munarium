// SPDX-License-Identifier: Apache-2.0
//! The document-intelligence seam: a remote/heavyweight document analyzer.
//!
//! ## Why this is a separate trait from `TextExtractor`
//!
//! `munarium-extract`'s `TextExtractor` is local, synchronous, free, and
//! deterministic — unzip a DOCX, read a PDF's text layer. A document
//! intelligence service is none of those: it is a network call, it costs money
//! per page, and its output can change when the vendor updates a model. Those
//! differences are the whole reason they are separate seams rather than one
//! trait with a slow implementation:
//!
//! - **Cost has to be a deliberate choice.** A local extractor can run on
//!   every source in a build. A per-page billed service cannot, so it is the
//!   ESCALATION path — reached only when local extraction produced nothing.
//! - **Determinism differs.** Local extraction is reproducible from bytes
//!   forever; a hosted model is reproducible only against a pinned API
//!   version, which is why implementations record one.
//! - **It is optional.** The system must run, and every test must pass, with
//!   no provider configured at all.
//!
//! ## Implementing a provider
//!
//! Implement `DocumentIntelligence` in its own crate (`munarium-docintel-<x>`),
//! depending only on `munarium-core` and an HTTP client. Nothing above this trait
//! knows which provider is configured; the server picks one from config and
//! hands it to the retrieval tier as `Arc<dyn DocumentIntelligence>`.
//!
//! A conforming provider must:
//!
//! 1. Report `supports(media_type)` honestly — the escalation path skips
//!    anything you decline, and a false yes turns into a billed no-op.
//! 2. Return `AnalyzedDocument::empty()` rather than an error when the service
//!    genuinely found no text. "Nothing there" and "the call failed" lead to
//!    different operator actions, and collapsing them hides real gaps.
//! 3. Bound itself: a page cap and a wall-clock timeout, so one pathological
//!    document cannot stall a build or run up a bill.
//! 4. Never let a provider failure fail the build. The caller records the
//!    outcome per source and continues; your errors are data, not panics.
//! 5. Carry no credentials in `provider_fingerprint` — it lands in stored
//!    metadata.

use crate::Result;
use async_trait::async_trait;

/// What a provider returned for one document.
#[derive(Debug, Clone)]
pub struct AnalyzedDocument {
    /// Extracted text, page blocks separated by blank lines so the
    /// paragraph-based chunker sees page boundaries.
    pub text: String,
    /// Pages actually analyzed — the unit most services bill on, so this is
    /// what cost reporting and the page cap key off.
    pub pages_analyzed: usize,
    /// Provider + model + API version, e.g.
    /// `azure-docintel/prebuilt-read/2024-11-30`. Recorded so an answer's
    /// provenance can say which model produced its text. Never secrets.
    pub provider_fingerprint: String,
}

impl AnalyzedDocument {
    pub fn empty(provider_fingerprint: impl Into<String>) -> Self {
        Self {
            text: String::new(),
            pages_analyzed: 0,
            provider_fingerprint: provider_fingerprint.into(),
        }
    }

    /// True when the service returned no usable text. Not an error — a blank
    /// scan is a legitimate and expected answer.
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }
}

/// A hosted or on-premises document analyzer.
#[async_trait]
pub trait DocumentIntelligence: Send + Sync {
    /// Whether this provider will attempt the media type. Decline anything
    /// you cannot handle: the caller skips you rather than paying for a
    /// guaranteed empty result.
    fn supports(&self, media_type: &str) -> bool;

    /// Analyze one document. Implementations bound their own page count and
    /// wall-clock time.
    async fn analyze(&self, media_type: &str, bytes: &[u8]) -> Result<AnalyzedDocument>;

    /// Stable id for logs, metrics, and the extraction record.
    fn id(&self) -> &'static str;
}
