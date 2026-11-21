// SPDX-License-Identifier: Apache-2.0
//! `extract_source` escalation rules with the isolated PDF profile's failure
//! classes (R3/B4). No database: the pool is lazy and extraction never
//! touches it.

use super::*;
use munarium_extract::{
    Budget, DeferredPolicy, Extracted, ExtractionMethod, ExtractionStatus, ExtractorRegistry,
    FailureClass, IsolatedPdfPolicy, TextExtractor,
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
    retrieval_with(local, provider, IsolatedPdfPolicy::default())
}

fn retrieval_with(
    local: Extracted,
    provider: Option<Arc<Counting>>,
    policy: IsolatedPdfPolicy,
) -> PgRetrieval {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
        .expect("lazy pool");
    let registry = ExtractorRegistry::with_extractors(vec![Box::new(Fixed(local))])
        .with_isolated_policy(policy);
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

const ESCALATE: IsolatedPdfPolicy = IsolatedPdfPolicy {
    escalate_over_budget: true,
    deferred: DeferredPolicy::Skip,
};

fn over_budget_outcomes() -> Vec<Extracted> {
    [
        Budget::Input,
        Budget::Memory,
        Budget::Cpu,
        Budget::WallClock,
        Budget::Output,
    ]
    .into_iter()
    .map(|b| {
        Extracted::failed(
            ExtractionMethod::PdfTextLayer,
            FailureClass::OverBudget(b),
            "over budget: bound",
        )
    })
    .collect()
}

#[tokio::test]
async fn deferred_cancelled_and_broken_children_never_escalate() {
    let m = ExtractionMethod::PdfTextLayer;
    for (class, policy) in [
        FailureClass::Deferred,
        FailureClass::Cancelled,
        FailureClass::IsolationFailed,
    ]
    .into_iter()
    .flat_map(|c| [(c, IsolatedPdfPolicy::default()), (c, ESCALATE)])
    {
        let provider = Arc::new(Counting::default());
        let r = retrieval_with(
            Extracted::failed(m, class, "not read"),
            Some(Arc::clone(&provider)),
            policy,
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
async fn parser_failures_keep_the_v14_escalation_rule() {
    // v1.4 escalates any PDF local extraction could not read; the isolated
    // profile keeps exactly that for a parser failure, under either policy.
    let m = ExtractionMethod::PdfTextLayer;
    for policy in [IsolatedPdfPolicy::default(), ESCALATE] {
        for local in [
            Extracted::failed(m, FailureClass::ParserFailed, "bad"),
            Extracted::empty(m, "scan"),
        ] {
            let provider = Arc::new(Counting::default());
            let r = retrieval_with(local, Some(Arc::clone(&provider)), policy);
            let out = r.extract_source("application/pdf", b"%PDF-1.4").await;
            assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
            assert_eq!(out.status, ExtractionStatus::Ok);
            assert_eq!(out.method, ExtractionMethod::Ocr);
        }
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

/// Maintainer decision (20 November 2026): an over-budget document does not
/// reach the paid provider by default, and keeps its `over_budget` result.
#[tokio::test]
async fn over_budget_documents_are_not_escalated_by_default() {
    for local in over_budget_outcomes() {
        let provider = Arc::new(Counting::default());
        let r = retrieval(local.clone(), Some(Arc::clone(&provider)));
        let out = r.extract_source("application/pdf", b"%PDF-1.4").await;
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            0,
            "{:?} must not reach the paid provider by default",
            local.failure
        );
        assert_eq!(out.failure, local.failure);
        assert_eq!(out.status_label(), "over_budget");
        assert!(out.text.is_empty());
    }
}

#[tokio::test]
async fn over_budget_documents_escalate_when_the_setting_enables_it() {
    for local in over_budget_outcomes() {
        let provider = Arc::new(Counting::default());
        let r = retrieval_with(local.clone(), Some(Arc::clone(&provider)), ESCALATE);
        let out = r.extract_source("application/pdf", b"%PDF-1.4").await;
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            1,
            "{:?}",
            local.failure
        );
        assert_eq!(out.status, ExtractionStatus::Ok);
        assert_eq!(out.method, ExtractionMethod::Ocr);
        assert_eq!(out.text, "recovered by the analyzer");
    }
}

// ---------------------------------------------------------------------------
// deferred documents
// ---------------------------------------------------------------------------

fn deferred() -> Extracted {
    Extracted::failed(
        ExtractionMethod::PdfTextLayer,
        FailureClass::Deferred,
        "busy",
    )
}

#[test]
fn deferred_documents_are_skipped_by_default_and_collected() {
    let mut d = DeferredDocuments::new(IsolatedPdfPolicy::default());
    d.observe("s-1", &deferred()).expect("skip");
    d.observe(
        "s-2",
        &Extracted::empty(ExtractionMethod::PdfTextLayer, "scan"),
    )
    .expect("not deferred");
    for other in every_isolated_outcome() {
        if other.failure != Some(FailureClass::Deferred) {
            d.observe("s-other", &other).expect("only deferred counts");
        }
    }
    d.observe("s-3", &deferred()).expect("skip");
    assert_eq!(d.skipped(), ["s-1".to_string(), "s-3".to_string()]);
}

#[test]
fn deferred_documents_fail_the_build_when_set_to_fail() {
    let mut d = DeferredDocuments::new(IsolatedPdfPolicy {
        escalate_over_budget: false,
        deferred: DeferredPolicy::Fail,
    });
    // Other outcomes never fail the build.
    for other in every_isolated_outcome() {
        if other.failure != Some(FailureClass::Deferred) {
            d.observe("s-other", &other).expect("not deferred");
        }
    }
    let err = d.observe("s-1", &deferred()).expect_err("fail");
    let KernelError::ExtractionDeferred(message) = &err else {
        panic!("expected ExtractionDeferred, got {err:?}");
    };
    assert_eq!(message, DEFERRED_BUILD_MESSAGE);
    assert!(!message.contains("s-1"), "names no source");
    assert!(d.skipped().is_empty());
    use munarium_store_pg::job_failure::JobFailureCode;
    let failure = munarium_store_pg::job_failure::classify(&err);
    assert_eq!(failure.code, Some(JobFailureCode::ExtractionDeferred));
    assert!(!failure.message.contains("s-1"));
}

#[test]
fn a_version_that_skipped_deferred_documents_does_not_satisfy_a_rebuild() {
    assert!(!manifest_skipped_deferred(
        &serde_json::json!({"max_chars": 400})
    ));
    assert!(!manifest_skipped_deferred(
        &serde_json::json!({ DEFERRED_SOURCES_MEMBER: [] })
    ));
    assert!(manifest_skipped_deferred(
        &serde_json::json!({ DEFERRED_SOURCES_MEMBER: ["s-1"] })
    ));
    // The chain's next id differs from the first, deterministically.
    let first = recipe_version_id("shape|para@1|local|extract@1[...]|s-1:h");
    let next = recipe_version_id(&format!(
        "shape|para@1|local|extract@1[...]|s-1:h|after-deferred:{first}"
    ));
    assert_ne!(first, next);
    assert!(first.starts_with("idx-") && first.len() == 20, "{first}");
}

#[tokio::test]
async fn the_default_registry_keeps_the_v14_identity() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
        .expect("lazy pool");
    let r = PgRetrieval::new(pool, "t-extraction");
    assert_eq!(r.extractor_version(), "extract@1[docx@1,pdf-text@1]");
}
