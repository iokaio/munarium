// SPDX-License-Identifier: Apache-2.0
//! Text extraction for indexable documents.
//!
//! Before this crate, `build_collection_index` ran `String::from_utf8_lossy`
//! over every source, so a PDF chunked and embedded as replacement
//! characters — retrieval returned confident noise. This turns bytes into
//! text the chunker can actually use, and says honestly when it cannot.
//!
//! **Where this runs: index time, not ingest time.** Raw bytes stay pristine
//! in the object store and remain the source of truth, which is what makes an
//! extractor improvement a rebuild rather than a re-upload — the same
//! "rebuild, don't migrate" rule the rest of the system follows.
//!
//! **Pure Rust only.** The image is a static musl binary on distroless, so
//! pdfium, poppler, mupdf and tesseract are not tradeoffs to weigh — they
//! cannot link at all.

// Production code returns typed errors instead of panicking; tests are exempt.
// The policy, its two exemptions and the per-site record are in
// server/docs/panic-boundaries.md (P15/R32).
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented
    )
)]

use munarium_core::{KernelError, Result};

mod docx;
pub mod isolated;
#[cfg(feature = "ocr")]
mod ocr;
mod pdf;

pub use docx::DocxExtractor;
pub use isolated::{
    DeferredPolicy, IsolatedPdfExtractor, IsolatedPdfLimits, IsolatedPdfPolicy, IsolatedRun,
    RunReport,
};
#[cfg(feature = "ocr")]
pub use ocr::{OcrExtractor, OcrModels, DEFAULT_MAX_PAGES};
pub use pdf::PdfTextExtractor;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Bump when any extractor changes its output for the same bytes. It joins
/// the index identity, so a change here correctly invalidates and rebuilds.
pub const EXTRACTOR_SET_VERSION: &str = "extract@1";

/// How the text was obtained. Recorded on the source row because an OCR'd
/// document and a text-layer document are not equivalent evidence, and
/// anything reasoning over citations deserves to know which it got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractionMethod {
    /// Bytes were already text (markdown, plain text, CSV…).
    Text,
    Docx,
    PdfTextLayer,
    Ocr,
}

impl ExtractionMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExtractionMethod::Text => "text",
            ExtractionMethod::Docx => "docx",
            ExtractionMethod::PdfTextLayer => "pdf-text",
            ExtractionMethod::Ocr => "ocr",
        }
    }
}

/// Outcome of extracting one source. `empty` is a first-class result, not an
/// error: a scanned PDF with no text layer is a real and expected case, and
/// it must be visible rather than silently contributing zero chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractionStatus {
    Ok,
    Empty,
    Failed,
}

impl ExtractionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExtractionStatus::Ok => "ok",
            ExtractionStatus::Empty => "empty",
            ExtractionStatus::Failed => "failed",
        }
    }
}

/// Which budget of the isolated PDF profile a document exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Budget {
    /// The document is larger than the child's memory budget can hold.
    Input,
    /// The child ran out of its OS-enforced memory limit.
    Memory,
    /// The child used its OS-enforced CPU time.
    Cpu,
    /// The child did not finish before the wall-clock deadline.
    WallClock,
    /// The extracted text was larger than the output cap.
    Output,
}

impl Budget {
    pub fn as_str(&self) -> &'static str {
        match self {
            Budget::Input => "input",
            Budget::Memory => "memory",
            Budget::Cpu => "cpu",
            Budget::WallClock => "wall_clock",
            Budget::Output => "output",
        }
    }
}

/// Why a non-`ok` result has no text, when the extractor can say more than
/// its status. Only the opt-in isolated PDF profile sets this; every v1.4
/// extractor leaves it `None`, so v1.4 status values and escalation are
/// unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// The parser rejected the document, or crashed on it.
    ParserFailed,
    /// The document exceeded a budget of the isolated profile.
    OverBudget(Budget),
    /// Admission did not grant an extraction slot within the queue wait.
    /// The document was never parsed.
    Deferred,
    /// The caller went away (a cancelled or dropped build) before the
    /// extraction finished. The child was killed and reaped.
    Cancelled,
    /// The isolated child could not be started, limited or understood.
    /// This is a fault of the deployment, not of the document.
    IsolationFailed,
}

impl FailureClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            FailureClass::ParserFailed => "parser_failed",
            FailureClass::OverBudget(_) => "over_budget",
            FailureClass::Deferred => "deferred",
            FailureClass::Cancelled => "cancelled",
            FailureClass::IsolationFailed => "isolation_failed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Extracted {
    pub text: String,
    pub status: ExtractionStatus,
    pub method: ExtractionMethod,
    /// Page or paragraph boundaries, when the format carries them, so a
    /// citation can name *page 14* rather than a character offset.
    pub pages: Vec<PageSpan>,
    /// Set when the source was recognized but yielded nothing usable.
    pub note: Option<String>,
    /// Set only by the opt-in isolated PDF profile. See [`FailureClass`].
    pub failure: Option<FailureClass>,
}

impl Extracted {
    fn ok(text: String, method: ExtractionMethod, pages: Vec<PageSpan>) -> Self {
        // Whitespace-only output is empty in every way that matters: it
        // produces no chunks, so calling it "ok" would hide the miss.
        let status = if text.trim().is_empty() {
            ExtractionStatus::Empty
        } else {
            ExtractionStatus::Ok
        };
        Self {
            text,
            status,
            method,
            pages,
            note: None,
            failure: None,
        }
    }

    /// An extraction that produced nothing, with the reason. Public so the
    /// caller running extraction on the blocking pool can record a task
    /// failure the same way an extractor failure is recorded.
    pub fn empty(method: ExtractionMethod, note: impl Into<String>) -> Self {
        Self {
            text: String::new(),
            status: ExtractionStatus::Empty,
            method,
            pages: Vec::new(),
            note: Some(note.into()),
            failure: None,
        }
    }

    /// A failed extraction with a class. Used by the isolated PDF profile.
    pub fn failed(
        method: ExtractionMethod,
        failure: FailureClass,
        note: impl Into<String>,
    ) -> Self {
        Self {
            text: String::new(),
            status: ExtractionStatus::Failed,
            method,
            pages: Vec::new(),
            note: Some(note.into()),
            failure: Some(failure),
        }
    }

    /// The value recorded in `sources.extraction_status`. Without a failure
    /// class this is exactly the v1.4 status (`ok`, `empty`, `failed`). The
    /// isolated profile adds `over_budget`, `deferred` and `cancelled`, so a
    /// document refused by a budget or never admitted is not mistaken for one
    /// the parser rejected.
    pub fn status_label(&self) -> &'static str {
        match self.failure {
            None | Some(FailureClass::ParserFailed) | Some(FailureClass::IsolationFailed) => {
                self.status.as_str()
            }
            Some(FailureClass::OverBudget(_)) => "over_budget",
            Some(FailureClass::Deferred) => "deferred",
            Some(FailureClass::Cancelled) => "cancelled",
        }
    }

    /// Whether this local result may be escalated to a paid document
    /// intelligence provider. The provider must still be configured and must
    /// accept the media type, and its own size budget still applies.
    ///
    /// Without a failure class this is the v1.4 rule unchanged: anything but
    /// `ok` is eligible. A parser failure is eligible under that same rule,
    /// because v1.4 already escalates a PDF local extraction could not read.
    /// An over-budget refusal is eligible only when `policy` enables it (off
    /// by default): a document past the budget is the one most likely to be
    /// expensive to analyze. Deferred, cancelled and isolation-failed results
    /// are never eligible: the document was not read, so escalating would
    /// turn local congestion or a broken deployment into paid calls.
    pub fn escalation_eligible(&self, policy: &IsolatedPdfPolicy) -> bool {
        if self.status == ExtractionStatus::Ok {
            return false;
        }
        match self.failure {
            None | Some(FailureClass::ParserFailed) => true,
            Some(FailureClass::OverBudget(_)) => policy.escalate_over_budget,
            Some(FailureClass::Deferred)
            | Some(FailureClass::Cancelled)
            | Some(FailureClass::IsolationFailed) => false,
        }
    }
}

/// A cancellation flag shared between an async caller and the blocking
/// extraction it started. Dropping the guard from [`Cancellation::guard`]
/// cancels, so a dropped build future stops its child process.
///
/// A [`child`](Cancellation::child) also observes its parent: a queued build
/// holds one parent for the whole build, cancelled by the job's stop signal,
/// and each source's extraction gets a child that its own drop guard
/// cancels without cancelling the build.
#[derive(Debug, Clone, Default)]
pub struct Cancellation {
    flag: Arc<AtomicBool>,
    parent: Option<Box<Cancellation>>,
}

impl Cancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// Cancelled itself, or through any ancestor.
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst) || self.parent.as_ref().is_some_and(|p| p.is_cancelled())
    }

    /// A new cancellation that is cancelled whenever `self` is. Cancelling
    /// the child leaves `self` untouched.
    pub fn child(&self) -> Self {
        Self {
            flag: Arc::default(),
            parent: Some(Box::new(self.clone())),
        }
    }

    /// A guard that cancels when dropped.
    pub fn guard(&self) -> CancelOnDrop {
        CancelOnDrop(self.clone())
    }
}

/// Cancels its [`Cancellation`] when dropped.
#[derive(Debug)]
pub struct CancelOnDrop(Cancellation);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// One page (PDF) or logical block (DOCX) within the extracted text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageSpan {
    /// 1-based.
    pub number: usize,
    /// Byte range into `Extracted::text`.
    pub start: usize,
    pub end: usize,
}

/// A format handler.
pub trait TextExtractor: Send + Sync {
    fn media_types(&self) -> &[&str];
    fn extract(&self, bytes: &[u8]) -> Result<Extracted>;
    fn id(&self) -> &'static str;

    /// The identity this extractor contributes to the extractor-set version.
    /// Defaults to `id()`; an extractor whose settings change its indexed
    /// output includes those settings here.
    fn identity(&self) -> String {
        self.id().to_string()
    }

    /// Extract, stopping early when `cancel` is set. In-process extractors
    /// cannot be preempted and ignore it.
    fn extract_cancellable(&self, bytes: &[u8], cancel: &Cancellation) -> Result<Extracted> {
        let _ = cancel;
        self.extract(bytes)
    }
}

/// Media types treated as already-text: decoded UTF-8 (lossy), no handler.
const TEXT_MEDIA_PREFIXES: &[&str] = &["text/"];
const TEXT_MEDIA_EXACT: &[&str] = &[
    "application/json",
    "application/xml",
    "application/x-ndjson",
    "application/yaml",
    "application/x-yaml",
];

/// Dispatches a source to the right extractor.
pub struct ExtractorRegistry {
    extractors: Vec<Box<dyn TextExtractor>>,
    /// How builds treat the isolated profile's refusals. The default unless
    /// [`ExtractorRegistry::with_isolated_policy`] sets it; it acts only on
    /// failure classes the isolated profile produces.
    isolated_policy: IsolatedPdfPolicy,
}

impl Default for ExtractorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ExtractorRegistry {
    /// DOCX + PDF text layer. OCR is added by `with_ocr` (feature `ocr`).
    pub fn new() -> Self {
        Self {
            extractors: vec![Box::new(DocxExtractor), Box::new(PdfTextExtractor)],
            isolated_policy: IsolatedPdfPolicy::default(),
        }
    }

    /// A registry holding exactly these extractors, for callers and tests
    /// that need a different set than `new()`.
    pub fn with_extractors(extractors: Vec<Box<dyn TextExtractor>>) -> Self {
        Self {
            extractors,
            isolated_policy: IsolatedPdfPolicy::default(),
        }
    }

    pub fn register(&mut self, extractor: Box<dyn TextExtractor>) {
        self.extractors.push(extractor);
    }

    /// The opt-in isolated PDF profile: replaces the in-process
    /// `pdf-text@1` with `pdf-text-isolated@1{...}`, which changes the set
    /// identity, so an index built this way never matches a v1.4 index.
    pub fn with_isolated_pdf(mut self, extractor: IsolatedPdfExtractor) -> Self {
        self.extractors
            .retain(|e| !e.media_types().contains(&pdf::PDF_MEDIA));
        self.extractors.push(Box::new(extractor));
        self
    }

    /// Set how builds treat the isolated profile's refusals. Not part of
    /// [`ExtractorRegistry::version`]; see [`IsolatedPdfPolicy`].
    pub fn with_isolated_policy(mut self, policy: IsolatedPdfPolicy) -> Self {
        self.isolated_policy = policy;
        self
    }

    pub fn isolated_policy(&self) -> IsolatedPdfPolicy {
        self.isolated_policy
    }

    /// Every extractor identity, sorted — joins the index identity so an
    /// extractor change forces a rebuild rather than serving stale chunks.
    pub fn version(&self) -> String {
        let mut ids: Vec<String> = self.extractors.iter().map(|e| e.identity()).collect();
        ids.sort_unstable();
        format!("{EXTRACTOR_SET_VERSION}[{}]", ids.join(","))
    }

    /// Extract, never panicking and never failing the caller: a bad document
    /// is recorded as `failed` and the build moves on, mirroring the
    /// batch-ingest rule that one bad file never fails the batch.
    pub fn extract(&self, media_type: &str, bytes: &[u8]) -> Extracted {
        self.extract_cancellable(media_type, bytes, &Cancellation::new())
    }

    /// [`ExtractorRegistry::extract`], stopping an isolated extraction when
    /// `cancel` is set.
    pub fn extract_cancellable(
        &self,
        media_type: &str,
        bytes: &[u8],
        cancel: &Cancellation,
    ) -> Extracted {
        let media = media_type
            .split(';')
            .next()
            .unwrap_or(media_type)
            .trim()
            .to_ascii_lowercase();

        if is_text_media(&media) {
            return Extracted::ok(
                String::from_utf8_lossy(bytes).into_owned(),
                ExtractionMethod::Text,
                Vec::new(),
            );
        }

        for extractor in &self.extractors {
            if extractor.media_types().iter().any(|m| *m == media) {
                return match extractor.extract_cancellable(bytes, cancel) {
                    Ok(extracted) => extracted,
                    Err(e) => Extracted {
                        text: String::new(),
                        status: ExtractionStatus::Failed,
                        method: ExtractionMethod::Text,
                        pages: Vec::new(),
                        note: Some(format!("{} failed: {e}", extractor.id())),
                        failure: None,
                    },
                };
            }
        }

        // Unknown binary. Decoding it as UTF-8 would index replacement
        // characters and look like success, so refuse instead.
        if looks_binary(bytes) {
            return Extracted::empty(
                ExtractionMethod::Text,
                format!("no extractor for media type '{media}'; bytes look binary"),
            );
        }
        Extracted::ok(
            String::from_utf8_lossy(bytes).into_owned(),
            ExtractionMethod::Text,
            Vec::new(),
        )
    }
}

fn is_text_media(media: &str) -> bool {
    TEXT_MEDIA_PREFIXES.iter().any(|p| media.starts_with(p)) || TEXT_MEDIA_EXACT.contains(&media)
}

/// A NUL byte in the first 8 KiB is the classic binary sniff, and it is the
/// one that matters here: it is exactly what a PDF or image yields.
fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8192).any(|b| *b == 0)
}

pub(crate) fn extract_err(what: &str, e: impl std::fmt::Display) -> KernelError {
    KernelError::InvalidInput(format!("{what}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_child_cancellation_follows_its_parent_but_not_the_reverse() {
        let build = Cancellation::new();
        let source = build.child();
        let guard = source.guard();
        drop(guard);
        assert!(source.is_cancelled());
        assert!(
            !build.is_cancelled(),
            "a source's drop guard spares the build"
        );
        let next = build.child();
        let grandchild = next.child();
        assert!(!next.is_cancelled() && !grandchild.is_cancelled());
        build.cancel();
        assert!(next.is_cancelled() && grandchild.is_cancelled());
    }

    #[test]
    fn text_media_passes_through_unchanged() {
        let r = ExtractorRegistry::new();
        let out = r.extract("text/markdown; charset=utf-8", b"# Title\n\nBody");
        assert_eq!(out.status, ExtractionStatus::Ok);
        assert_eq!(out.method, ExtractionMethod::Text);
        assert_eq!(out.text, "# Title\n\nBody");
    }

    #[test]
    fn whitespace_only_output_is_empty_not_ok() {
        // Otherwise a document contributing zero chunks looks like success.
        let out = Extracted::ok("   \n\t ".into(), ExtractionMethod::Text, Vec::new());
        assert_eq!(out.status, ExtractionStatus::Empty);
    }

    #[test]
    fn unknown_binary_is_refused_rather_than_indexed_as_mojibake() {
        let r = ExtractorRegistry::new();
        let out = r.extract("application/octet-stream", b"\x00\x01\x02binary\x00");
        assert_eq!(out.status, ExtractionStatus::Empty);
        assert!(out.note.as_deref().unwrap_or("").contains("binary"));
        assert!(out.text.is_empty());
    }

    #[test]
    fn unknown_but_textual_media_still_indexes() {
        let r = ExtractorRegistry::new();
        let out = r.extract("application/vnd.custom+text", b"plain enough");
        assert_eq!(out.status, ExtractionStatus::Ok);
        assert_eq!(out.text, "plain enough");
    }

    #[test]
    fn a_corrupt_document_is_failed_not_a_panic() {
        let r = ExtractorRegistry::new();
        // Claims to be DOCX, is not a ZIP at all.
        let out = r.extract(
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            b"definitely not a zip",
        );
        assert_eq!(out.status, ExtractionStatus::Failed);
        assert!(out.note.is_some());
    }

    #[test]
    fn the_v14_identity_is_unchanged() {
        // B4-T1: with the isolated profile off, v1.4 rebuilds keep this exact
        // identity. Changing it invalidates every existing index.
        assert_eq!(
            ExtractorRegistry::new().version(),
            "extract@1[docx@1,pdf-text@1]"
        );
    }

    #[test]
    fn v14_results_carry_no_failure_class_and_keep_v14_labels_and_escalation() {
        let r = ExtractorRegistry::new();
        let failed = r.extract(
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            b"definitely not a zip",
        );
        assert_eq!(failed.failure, None);
        assert_eq!(failed.status_label(), "failed");
        let empty = Extracted::empty(ExtractionMethod::PdfTextLayer, "scan");
        assert_eq!(empty.status_label(), "empty");
        let ok = r.extract("text/plain", b"hello");
        assert_eq!(ok.status_label(), "ok");
        // The isolated policy acts only on isolated failure classes, so no
        // setting changes a v1.4 result's escalation.
        for policy in every_policy() {
            assert!(failed.escalation_eligible(&policy));
            assert!(empty.escalation_eligible(&policy));
            assert!(!ok.escalation_eligible(&policy));
        }
    }

    #[test]
    fn failure_classes_have_distinct_labels_and_escalation_rules() {
        let m = ExtractionMethod::PdfTextLayer;
        let parser = Extracted::failed(m, FailureClass::ParserFailed, "bad");
        let over = Extracted::failed(m, FailureClass::OverBudget(Budget::Memory), "big");
        let deferred = Extracted::failed(m, FailureClass::Deferred, "busy");
        let cancelled = Extracted::failed(m, FailureClass::Cancelled, "gone");
        let broken = Extracted::failed(m, FailureClass::IsolationFailed, "no child");
        assert_eq!(parser.status_label(), "failed");
        assert_eq!(over.status_label(), "over_budget");
        assert_eq!(deferred.status_label(), "deferred");
        assert_eq!(cancelled.status_label(), "cancelled");
        assert_eq!(broken.status_label(), "failed");
        // Today's rule escalates any unreadable PDF; keep that for a parser
        // failure under every policy, and never escalate work that was not
        // attempted.
        for policy in every_policy() {
            assert!(parser.escalation_eligible(&policy));
            assert!(!deferred.escalation_eligible(&policy));
            assert!(!cancelled.escalation_eligible(&policy));
            assert!(!broken.escalation_eligible(&policy));
        }
        // Maintainer decision (20 November 2026): an over-budget refusal
        // does not escalate by default, and does when the setting enables it.
        let default = IsolatedPdfPolicy::default();
        assert!(!default.escalate_over_budget);
        assert_eq!(default.deferred, DeferredPolicy::Skip);
        assert!(!over.escalation_eligible(&default));
        for budget in [
            Budget::Input,
            Budget::Memory,
            Budget::Cpu,
            Budget::WallClock,
            Budget::Output,
        ] {
            let over = Extracted::failed(m, FailureClass::OverBudget(budget), "bound");
            assert!(!over.escalation_eligible(&default), "{budget:?}");
            let enabled = IsolatedPdfPolicy {
                escalate_over_budget: true,
                ..default
            };
            assert!(over.escalation_eligible(&enabled), "{budget:?}");
        }
    }

    fn every_policy() -> [IsolatedPdfPolicy; 4] {
        [
            (false, DeferredPolicy::Skip),
            (true, DeferredPolicy::Skip),
            (false, DeferredPolicy::Fail),
            (true, DeferredPolicy::Fail),
        ]
        .map(|(escalate_over_budget, deferred)| IsolatedPdfPolicy {
            escalate_over_budget,
            deferred,
        })
    }

    #[test]
    fn the_isolated_policy_defaults_and_is_not_part_of_the_identity() {
        let r = ExtractorRegistry::new();
        assert_eq!(r.isolated_policy(), IsolatedPdfPolicy::default());
        let policy = IsolatedPdfPolicy {
            escalate_over_budget: true,
            deferred: DeferredPolicy::Fail,
        };
        let set = ExtractorRegistry::new().with_isolated_policy(policy);
        assert_eq!(set.isolated_policy(), policy);
        assert_eq!(set.version(), r.version());
    }

    #[test]
    fn the_isolated_profile_replaces_pdf_text_and_changes_the_identity() {
        let isolated =
            IsolatedPdfExtractor::new("munarium-server".into(), IsolatedPdfLimits::PROVISIONAL)
                .expect("valid limits");
        let r = ExtractorRegistry::new().with_isolated_pdf(isolated);
        let v = r.version();
        assert_ne!(v, ExtractorRegistry::new().version());
        assert!(
            v.starts_with("extract@1[docx@1,pdf-text-isolated@1{"),
            "{v}"
        );
        assert!(!v.contains("pdf-text@1"), "{v}");
        assert_eq!(r.extractors.len(), 2);
    }

    #[test]
    fn cancel_on_drop_cancels() {
        let c = Cancellation::new();
        {
            let _g = c.guard();
            assert!(!c.is_cancelled());
        }
        assert!(c.is_cancelled());
    }

    #[test]
    fn version_covers_the_registered_set() {
        let v = ExtractorRegistry::new().version();
        assert!(v.starts_with("extract@1["));
        assert!(v.contains("docx@1"));
        assert!(v.contains("pdf-text@1"));
    }
}
