// SPDX-License-Identifier: Apache-2.0
//! `extract_source` escalation rules with the isolated PDF profile's failure
//! classes (R3/B4). No database: the pool is lazy and extraction never
//! touches it.

use super::*;
use munarium_extract::{
    Budget, Extracted, ExtractionMethod, ExtractionStatus, ExtractorRegistry, FailureClass,
    TextExtractor,
};
use std::sync::atomic::{AtomicUsize, Ordering};

/// A PDF extractor that answers with a fixed result.
struct Fixed(Extracted);

impl TextExtractor for Fixed {
    fn media_types(&self) -> &[&str] {
        &["application/pdf"]
    }
    fn extract(&self, _bytes: &[u8]) -> Result<Extracted> {
        Ok(self.0.clone())
    }
    fn id(&self) -> &'static str {
        "fixed@1"
    }
}

#[derive(Default)]
struct Counting {
    calls: AtomicUsize,
}

#[async_trait]
impl DocumentIntelligence for Counting {
    fn supports(&self, media_type: &str) -> bool {
        media_type == "application/pdf"
    }
    fn id(&self) -> &'static str {
        "counting"
    }
    async fn analyze(
        &self,
        _media_type: &str,
        _bytes: &[u8],
    ) -> Result<munarium_core::docintel::AnalyzedDocument> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(munarium_core::docintel::AnalyzedDocument {
            text: "recovered by the analyzer".into(),
            pages_analyzed: 1,
            provider_fingerprint: "counting/v1".into(),
        })
    }
}

fn retrieval(local: Extracted, provider: Option<Arc<Counting>>) -> PgRetrieval {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
        .expect("lazy pool");
    let registry = ExtractorRegistry::with_extractors(vec![Box::new(Fixed(local))]);
    PgRetrieval::new(pool, "t-extraction")
        .with_extractors(Arc::new(registry))
        .with_doc_intel(provider.map(|p| p as Arc<dyn DocumentIntelligence>))
}

fn every_isolated_outcome() -> Vec<Extracted> {
    let m = ExtractionMethod::PdfTextLayer;
    vec![
        Extracted::failed(m, FailureClass::ParserFailed, "bad xref"),
        Extracted::failed(m, FailureClass::OverBudget(Budget::Memory), "memory"),
        Extracted::failed(m, FailureClass::OverBudget(Budget::Cpu), "cpu"),
        Extracted::failed(m, FailureClass::OverBudget(Budget::WallClock), "time"),
        Extracted::failed(m, FailureClass::OverBudget(Budget::Output), "output"),
        Extracted::failed(m, FailureClass::OverBudget(Budget::Input), "input"),
        Extracted::failed(m, FailureClass::Deferred, "busy"),
        Extracted::failed(m, FailureClass::Cancelled, "gone"),
        Extracted::failed(m, FailureClass::IsolationFailed, "no child"),
        Extracted::empty(m, "scan"),
    ]
}

#[tokio::test]
async fn zero_escalation_calls_without_a_configured_provider() {
    // `MUNARIUM_DOCINTEL=none` builds no provider at all, so there is
    // nothing that could be called: every local outcome must come back as
    // it was, with its class and label, rather than being rewritten.
    for local in every_isolated_outcome() {
        let r = retrieval(local.clone(), None);
        let out = r.extract_source("application/pdf", b"%PDF-1.4").await;
        assert_eq!(out.status, local.status);
        assert_eq!(out.failure, local.failure);
        assert_eq!(out.status_label(), local.status_label());
        assert_eq!(out.method, local.method);
        assert!(out.text.is_empty());
    }
}

#[tokio::test]
async fn deferred_cancelled_and_broken_children_never_escalate() {
    let m = ExtractionMethod::PdfTextLayer;
    for class in [
        FailureClass::Deferred,
        FailureClass::Cancelled,
        FailureClass::IsolationFailed,
    ] {
        let provider = Arc::new(Counting::default());
        let r = retrieval(
            Extracted::failed(m, class, "not read"),
            Some(Arc::clone(&provider)),
        );
        let out = r.extract_source("application/pdf", b"%PDF-1.4").await;
        assert_eq!(out.failure, Some(class));
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            0,
            "{class:?} must not reach the paid provider"
        );
    }
}

#[tokio::test]
async fn parser_failures_and_over_budget_keep_the_v14_escalation_rule() {
    // v1.4 escalates any PDF local extraction could not read; the isolated
    // profile keeps exactly that for a parser failure and a budget refusal.
    let m = ExtractionMethod::PdfTextLayer;
    for local in [
        Extracted::failed(m, FailureClass::ParserFailed, "bad"),
        Extracted::failed(m, FailureClass::OverBudget(Budget::Memory), "big"),
        Extracted::empty(m, "scan"),
    ] {
        let provider = Arc::new(Counting::default());
        let r = retrieval(local, Some(Arc::clone(&provider)));
        let out = r.extract_source("application/pdf", b"%PDF-1.4").await;
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(out.status, ExtractionStatus::Ok);
        assert_eq!(out.method, ExtractionMethod::Ocr);
    }
    // A readable document never reaches the provider.
    let provider = Arc::new(Counting::default());
    let r = retrieval(
        Extracted {
            text: "readable".into(),
            status: ExtractionStatus::Ok,
            method: m,
            pages: Vec::new(),
            note: None,
            failure: None,
        },
        Some(Arc::clone(&provider)),
    );
    r.extract_source("application/pdf", b"%PDF-1.4").await;
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_default_registry_keeps_the_v14_identity() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
        .expect("lazy pool");
    let r = PgRetrieval::new(pool, "t-extraction");
    assert_eq!(r.extractor_version(), "extract@1[docx@1,pdf-text@1]");
}
