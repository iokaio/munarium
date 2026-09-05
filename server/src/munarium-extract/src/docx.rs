// SPDX-License-Identifier: Apache-2.0
//! DOCX extraction.
//!
//! A `.docx` is a ZIP whose `word/document.xml` holds the body. Text lives in
//! `<w:t>` runs; paragraphs are `<w:p>`; `<w:br>` and `<w:tab>` are explicit.
//! Reading the XML directly (rather than through a document model) keeps this
//! to two pure-Rust dependencies and makes the paragraph boundaries — which
//! the `para@1` chunker splits on — come out exactly where Word put them.

use crate::{extract_err, Extracted, ExtractionMethod, PageSpan, TextExtractor};
use munarium_core::Result;
use quick_xml::events::Event;
use quick_xml::Reader;
use std::io::{Cursor, Read};

pub const DOCX_MEDIA: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

/// Guard against a zip bomb: a DOCX body far past this is not a document
/// anyone is retrieving over, it is an attack on the indexer.
const MAX_DOCUMENT_XML: u64 = 128 * 1024 * 1024;

#[derive(Default)]
pub struct DocxExtractor;

impl TextExtractor for DocxExtractor {
    fn media_types(&self) -> &[&str] {
        &[DOCX_MEDIA]
    }

    fn id(&self) -> &'static str {
        "docx@1"
    }

    fn extract(&self, bytes: &[u8]) -> Result<Extracted> {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|e| extract_err("docx is not a readable zip", e))?;
        let entry = archive
            .by_name("word/document.xml")
            .map_err(|e| extract_err("docx has no word/document.xml", e))?;
        if entry.size() > MAX_DOCUMENT_XML {
            return Err(extract_err(
                "docx document.xml is implausibly large",
                format!("{} bytes uncompressed", entry.size()),
            ));
        }
        // `entry.size()` is the DECLARED uncompressed size from the central
        // directory, which the archive's author wrote; the zip reader bounds
        // itself by the compressed size only and never enforces the declared
        // one. So the header check above is a cheap pre-filter and this
        // `take` is the guard: a crafted entry declaring 100 bytes over a
        // deflate stream that inflates to gigabytes stops one byte past the
        // ceiling instead of allocating the whole inflation.
        let mut xml = String::new();
        let mut bounded = entry.take(MAX_DOCUMENT_XML + 1);
        bounded
            .read_to_string(&mut xml)
            .map_err(|e| extract_err("docx document.xml is not valid UTF-8", e))?;
        if xml.len() as u64 > MAX_DOCUMENT_XML {
            return Err(extract_err(
                "docx document.xml is implausibly large",
                format!(
                    "inflates past {MAX_DOCUMENT_XML} bytes while declaring {}",
                    bounded.get_ref().size()
                ),
            ));
        }

        let (text, pages) = parse_document_xml(&xml)?;
        Ok(Extracted::ok(text, ExtractionMethod::Docx, pages))
    }
}

/// Walk the body, emitting one blank-line-separated block per `<w:p>` so the
/// paragraph structure survives into the chunker.
fn parse_document_xml(xml: &str) -> Result<(String, Vec<PageSpan>)> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);

    let mut out = String::new();
    let mut pages: Vec<PageSpan> = Vec::new();
    let mut para = String::new();
    let mut in_text_run = false;
    // Field instructions (TOC codes etc.) are markup, not prose.
    let mut in_instr = false;

    loop {
        match reader
            .read_event()
            .map_err(|e| extract_err("docx xml", e))?
        {
            Event::Start(e) => match local_name(e.name().as_ref()) {
                b"t" => in_text_run = true,
                b"instrText" => in_instr = true,
                _ => {}
            },
            Event::End(e) => match local_name(e.name().as_ref()) {
                b"t" => in_text_run = false,
                b"instrText" => in_instr = false,
                b"p" => {
                    let block = para.trim_end().to_string();
                    para.clear();
                    if block.trim().is_empty() {
                        continue;
                    }
                    if !out.is_empty() {
                        out.push_str("\n\n");
                    }
                    let para_start = out.len();
                    out.push_str(&block);
                    pages.push(PageSpan {
                        number: pages.len() + 1,
                        start: para_start,
                        end: out.len(),
                    });
                }
                _ => {}
            },
            Event::Empty(e) => match local_name(e.name().as_ref()) {
                // Explicit line break and tab inside a paragraph.
                b"br" | b"cr" => para.push('\n'),
                b"tab" => para.push('\t'),
                _ => {}
            },
            Event::Text(e) => {
                if in_text_run && !in_instr {
                    let raw = e.decode().map_err(|e| extract_err("docx text decode", e))?;
                    para.push_str(raw.as_ref());
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    // A final paragraph with no closing tag (malformed but recoverable).
    let tail = para.trim_end();
    if !tail.is_empty() {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        let start = out.len();
        out.push_str(tail);
        pages.push(PageSpan {
            number: pages.len() + 1,
            start,
            end: out.len(),
        });
    }

    Ok((out, pages))
}

/// Strip the `w:` (or any) namespace prefix — DOCX files in the wild use
/// several, and matching on the prefixed name silently misses them.
fn local_name(name: &[u8]) -> &[u8] {
    match name.iter().rposition(|b| *b == b':') {
        Some(i) => &name[i + 1..],
        None => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ExtractionStatus;
    use std::io::Write;

    /// Build a minimal but real .docx in memory.
    fn docx(document_xml: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            zw.start_file("word/document.xml", opts).expect("start");
            zw.write_all(document_xml.as_bytes()).expect("write");
            zw.finish().expect("finish");
        }
        buf
    }

    const BODY: &str = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
 <w:body>
  <w:p><w:r><w:t>Vacation Policy</w:t></w:r></w:p>
  <w:p><w:r><w:t>Employees accrue </w:t></w:r><w:r><w:t>15 days</w:t></w:r></w:p>
  <w:p></w:p>
  <w:p><w:r><w:t>Line one</w:t></w:r><w:r><w:br/><w:t>line two</w:t></w:r></w:p>
 </w:body>
</w:document>"#;

    #[test]
    fn extracts_paragraphs_and_joins_split_runs() {
        let out = DocxExtractor.extract(&docx(BODY)).expect("extract");
        assert_eq!(out.status, ExtractionStatus::Ok);
        assert_eq!(out.method, ExtractionMethod::Docx);
        // Word splits a sentence across runs; the text must come back whole.
        assert!(out.text.contains("Employees accrue 15 days"));
        // Paragraphs are blank-line separated, which is what `para@1` splits on.
        assert!(out.text.starts_with("Vacation Policy\n\nEmployees accrue"));
        // Empty paragraphs do not produce empty blocks.
        assert!(!out.text.contains("\n\n\n"));
        // <w:br/> is a line break inside the paragraph, not a new one.
        assert!(out.text.contains("Line one\nline two"));
        // Spans line up with the text they claim.
        for span in &out.pages {
            assert!(span.end <= out.text.len());
            assert!(!out.text[span.start..span.end].trim().is_empty());
        }
    }

    #[test]
    fn field_instructions_are_not_prose() {
        let xml = r#"<w:document xmlns:w="x"><w:body>
          <w:p><w:r><w:instrText>TOC \o "1-3"</w:instrText></w:r><w:r><w:t>Real text</w:t></w:r></w:p>
        </w:body></w:document>"#;
        let out = DocxExtractor.extract(&docx(xml)).expect("extract");
        assert_eq!(out.text, "Real text");
        assert!(!out.text.contains("TOC"));
    }

    #[test]
    fn namespace_prefix_variations_still_parse() {
        // Same document with a different prefix — common in the wild.
        let xml = r#"<ns0:document xmlns:ns0="x"><ns0:body>
          <ns0:p><ns0:r><ns0:t>Prefixed</ns0:t></ns0:r></ns0:p>
        </ns0:body></ns0:document>"#;
        let out = DocxExtractor.extract(&docx(xml)).expect("extract");
        assert_eq!(out.text, "Prefixed");
    }

    #[test]
    fn an_empty_document_reports_empty_not_ok() {
        let xml = r#"<w:document xmlns:w="x"><w:body></w:body></w:document>"#;
        let out = DocxExtractor.extract(&docx(xml)).expect("extract");
        assert_eq!(out.status, ExtractionStatus::Empty);
    }

    #[test]
    fn a_zip_without_document_xml_is_an_error() {
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            zw.start_file("other.txt", opts).expect("start");
            zw.write_all(b"hi").expect("write");
            zw.finish().expect("finish");
        }
        assert!(DocxExtractor.extract(&buf).is_err());
    }

    #[test]
    fn a_non_zip_is_an_error_not_a_panic() {
        assert!(DocxExtractor.extract(b"not a zip at all").is_err());
    }
}
