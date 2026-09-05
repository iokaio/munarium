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

use munarium_core::{KernelError, Result};

mod docx;
#[cfg(feature = "ocr")]
mod ocr;
mod pdf;

pub use docx::DocxExtractor;
#[cfg(feature = "ocr")]
pub use ocr::{OcrExtractor, OcrModels, DEFAULT_MAX_PAGES};
pub use pdf::PdfTextExtractor;

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
        }
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
        }
    }

    pub fn register(&mut self, extractor: Box<dyn TextExtractor>) {
        self.extractors.push(extractor);
    }

    /// Every extractor id, sorted — joins the index identity so an extractor
    /// change forces a rebuild rather than serving stale chunks.
    pub fn version(&self) -> String {
        let mut ids: Vec<&str> = self.extractors.iter().map(|e| e.id()).collect();
        ids.sort_unstable();
        format!("{EXTRACTOR_SET_VERSION}[{}]", ids.join(","))
    }

    /// Extract, never panicking and never failing the caller: a bad document
    /// is recorded as `failed` and the build moves on, mirroring the
    /// batch-ingest rule that one bad file never fails the batch.
    pub fn extract(&self, media_type: &str, bytes: &[u8]) -> Extracted {
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
                return match extractor.extract(bytes) {
                    Ok(extracted) => extracted,
                    Err(e) => Extracted {
                        text: String::new(),
                        status: ExtractionStatus::Failed,
                        method: ExtractionMethod::Text,
                        pages: Vec::new(),
                        note: Some(format!("{} failed: {e}", extractor.id())),
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
    fn version_covers_the_registered_set() {
        let v = ExtractorRegistry::new().version();
        assert!(v.starts_with("extract@1["));
        assert!(v.contains("docx@1"));
        assert!(v.contains("pdf-text@1"));
    }
}
