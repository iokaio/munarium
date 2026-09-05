// SPDX-License-Identifier: Apache-2.0
//! PDF text-layer extraction.
//!
//! Pure Rust via `pdf-extract` (over `lopdf`). This reads the **text layer
//! only** — the characters the producer actually embedded. A scanned PDF has
//! no text layer and yields nothing here; that is not a failure to hide but
//! the exact signal the OCR fallback keys on, so it comes back `empty` with a
//! note rather than as an error.

use crate::{extract_err, Extracted, ExtractionMethod, TextExtractor};
use munarium_core::Result;

pub const PDF_MEDIA: &str = "application/pdf";

/// Below this many non-whitespace characters a "text layer" is almost
/// certainly page furniture (a stamped page number, a scanner watermark)
/// rather than content — treat it as absent so OCR gets its turn.
const MIN_MEANINGFUL_CHARS: usize = 16;

#[derive(Default)]
pub struct PdfTextExtractor;

impl TextExtractor for PdfTextExtractor {
    fn media_types(&self) -> &[&str] {
        &[PDF_MEDIA]
    }

    fn id(&self) -> &'static str {
        "pdf-text@1"
    }

    fn extract(&self, bytes: &[u8]) -> Result<Extracted> {
        // pdf-extract can panic on malformed input; a caller-uploaded PDF
        // must never take the indexer down, so contain it here.
        let extracted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pdf_extract::extract_text_from_mem(bytes)
        }));

        let text = match extracted {
            Ok(Ok(text)) => text,
            Ok(Err(e)) => return Err(extract_err("pdf text extraction", e)),
            Err(_) => {
                return Err(extract_err(
                    "pdf text extraction",
                    "the parser panicked on this document",
                ))
            }
        };

        let meaningful = text.chars().filter(|c| !c.is_whitespace()).count();
        if meaningful < MIN_MEANINGFUL_CHARS {
            return Ok(Extracted::empty(
                ExtractionMethod::PdfTextLayer,
                "pdf has no usable text layer (likely a scan) — OCR is the path for this document",
            ));
        }

        Ok(Extracted::ok(
            normalize(&text),
            ExtractionMethod::PdfTextLayer,
            Vec::new(),
        ))
    }
}

/// PDF text arrives with hard line wraps at the column width. Collapse single
/// newlines inside a block to spaces so sentences survive, while keeping
/// blank lines — the paragraph boundaries `para@1` chunks on.
fn normalize(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut blank_run = 0usize;
    for line in raw.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            blank_run += 1;
            continue;
        }
        if !out.is_empty() {
            if blank_run > 0 {
                out.push_str("\n\n");
            } else {
                out.push(' ');
            }
        }
        blank_run = 0;
        out.push_str(line.trim_start());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ExtractionStatus;

    /// A minimal single-page PDF with a real text layer, written by hand so
    /// the fixture is deterministic and needs no generator.
    fn pdf_with_text(text: &str) -> Vec<u8> {
        let content = format!("BT /F1 12 Tf 72 720 Td ({text}) Tj ET");
        let mut objects: Vec<String> = Vec::new();
        objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());
        objects.push("<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into());
        objects.push(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"
                .into(),
        );
        objects.push(format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ));
        objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());

        let mut pdf = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.push_str(&format!("{} 0 obj\n{body}\nendobj\n", i + 1));
        }
        let xref_at = pdf.len();
        pdf.push_str(&format!("xref\n0 {}\n", objects.len() + 1));
        pdf.push_str("0000000000 65535 f \n");
        for off in &offsets {
            pdf.push_str(&format!("{off:010} 00000 n \n"));
        }
        pdf.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
            objects.len() + 1
        ));
        pdf.into_bytes()
    }

    #[test]
    fn reads_an_embedded_text_layer() {
        let bytes = pdf_with_text("The quarterly settlement was approved on 14 March");
        let out = PdfTextExtractor.extract(&bytes).expect("extract");
        assert_eq!(out.status, ExtractionStatus::Ok, "note: {:?}", out.note);
        assert_eq!(out.method, ExtractionMethod::PdfTextLayer);
        assert!(
            out.text.contains("quarterly settlement"),
            "got: {:?}",
            out.text
        );
    }

    #[test]
    fn a_pdf_with_no_text_layer_reports_empty_and_points_at_ocr() {
        // A page with no content stream text — the scanned-document shape.
        let bytes = pdf_with_text("");
        let out = PdfTextExtractor.extract(&bytes).expect("extract");
        assert_eq!(out.status, ExtractionStatus::Empty);
        assert!(out.note.as_deref().unwrap_or("").contains("OCR"));
    }

    #[test]
    fn garbage_is_an_error_not_a_crash() {
        // The indexer must survive whatever a caller uploads.
        let out = PdfTextExtractor.extract(b"%PDF-1.4\nnot really a pdf");
        assert!(out.is_err());
    }

    #[test]
    fn hard_wraps_are_rejoined_but_paragraphs_are_kept() {
        let raw = "The settlement was\napproved on 14 March\n\nA second paragraph\nfollows here";
        let out = normalize(raw);
        assert!(out.contains("The settlement was approved on 14 March"));
        assert!(out.contains("\n\nA second paragraph follows here"));
    }
}
