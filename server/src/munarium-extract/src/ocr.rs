// SPDX-License-Identifier: Apache-2.0
//! OCR for scanned PDFs and TIFFs (feature `ocr`).
//!
//! ## Why rasterization is the hard part
//!
//! OCR needs page bitmaps, and pure-Rust PDF *rendering* is weak — the usual
//! engines (pdfium, mupdf) are native and cannot exist in a static-musl
//! distroless build. The path around it: a scanned PDF is almost always a
//! thin container holding **one full-page image XObject per page**, which
//! `lopdf` can pull out directly as an already-encoded stream. Decode that
//! image and OCR it; no rasterizer required.
//!
//! ## The honest coverage limit
//!
//! JPEG (`DCTDecode`) and raw/Flate bitmaps work. **JBIG2 does not** — there
//! is no pure-Rust decoder, and it is common in older court filings. CCITT
//! G4 (`CCITTFaxDecode`) is likewise not decoded here. Those pages come back
//! `empty` with a note naming the filter, so the gap is visible in the data
//! rather than looking like a blank document.
//!
//! Models are `.rten` files fetched at startup (see `OcrEngine::load`), not
//! embedded: they are tens of MB against a <30 MB image target.

use crate::{extract_err, Extracted, ExtractionMethod, PageSpan, TextExtractor};
use munarium_core::Result;
use std::path::Path;
use std::sync::Arc;

pub const TIFF_MEDIA: &str = "image/tiff";
pub const PNG_MEDIA: &str = "image/png";
pub const JPEG_MEDIA: &str = "image/jpeg";

/// Hard ceiling on pages OCR'd per document. OCR on a 0.5-vCPU container is
/// the slow path; without a cap one pathological upload pins the indexer.
pub const DEFAULT_MAX_PAGES: usize = 50;

/// Loaded detection + recognition models.
pub struct OcrModels {
    engine: ocrs::OcrEngine,
}

impl OcrModels {
    /// Load from `.rten` model files on disk. The caller fetches them (from
    /// the blob container in production) and hands over the paths.
    pub fn load(detection: &Path, recognition: &Path) -> Result<Self> {
        let det = rten::Model::load_file(detection).map_err(|e| {
            extract_err(&format!("ocr detection model '{}'", detection.display()), e)
        })?;
        let rec = rten::Model::load_file(recognition).map_err(|e| {
            extract_err(
                &format!("ocr recognition model '{}'", recognition.display()),
                e,
            )
        })?;
        let engine = ocrs::OcrEngine::new(ocrs::OcrEngineParams {
            detection_model: Some(det),
            recognition_model: Some(rec),
            ..Default::default()
        })
        .map_err(|e| extract_err("ocr engine", e))?;
        Ok(Self { engine })
    }

    fn recognize(&self, img: &image::DynamicImage) -> Result<String> {
        let rgb = img.to_rgb8();
        let (w, h) = (rgb.width() as usize, rgb.height() as usize);
        let source = ocrs::ImageSource::from_bytes(rgb.as_raw(), (w as u32, h as u32))
            .map_err(|e| extract_err("ocr image source", e))?;
        let input = self
            .engine
            .prepare_input(source)
            .map_err(|e| extract_err("ocr prepare", e))?;
        let words = self
            .engine
            .detect_words(&input)
            .map_err(|e| extract_err("ocr detect", e))?;
        let lines = self.engine.find_text_lines(&input, &words);
        let text = self
            .engine
            .recognize_text(&input, &lines)
            .map_err(|e| extract_err("ocr recognize", e))?;
        Ok(text
            .iter()
            .flatten()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

/// OCR over rasters and scanned PDFs.
pub struct OcrExtractor {
    models: Arc<OcrModels>,
    max_pages: usize,
}

impl OcrExtractor {
    pub fn new(models: Arc<OcrModels>) -> Self {
        Self {
            models,
            max_pages: DEFAULT_MAX_PAGES,
        }
    }

    pub fn with_max_pages(mut self, max_pages: usize) -> Self {
        self.max_pages = max_pages.max(1);
        self
    }

    /// OCR every page image, joining pages as blank-line-separated blocks so
    /// the `para@1` chunker sees page boundaries.
    fn ocr_images(&self, images: Vec<image::DynamicImage>) -> (String, Vec<PageSpan>) {
        let mut out = String::new();
        let mut pages = Vec::new();
        for (i, img) in images.into_iter().take(self.max_pages).enumerate() {
            let text = match self.models.recognize(&img) {
                Ok(t) => t,
                Err(e) => {
                    // One unreadable page must not lose the other 49.
                    tracing::warn!(page = i + 1, error = %e, "ocr failed for a page");
                    continue;
                }
            };
            if text.trim().is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            let start = out.len();
            out.push_str(text.trim());
            pages.push(PageSpan {
                number: i + 1,
                start,
                end: out.len(),
            });
        }
        (out, pages)
    }
}

impl TextExtractor for OcrExtractor {
    fn media_types(&self) -> &[&str] {
        &[TIFF_MEDIA, PNG_MEDIA, JPEG_MEDIA]
    }

    fn id(&self) -> &'static str {
        "ocr@1"
    }

    fn extract(&self, bytes: &[u8]) -> Result<Extracted> {
        let images = decode_raster_pages(bytes)?;
        if images.is_empty() {
            return Ok(Extracted::empty(
                ExtractionMethod::Ocr,
                "no decodable page images",
            ));
        }
        let (text, pages) = self.ocr_images(images);
        Ok(Extracted::ok(text, ExtractionMethod::Ocr, pages))
    }
}

/// Decode a raster to pages. Multi-page TIFF yields one image per frame —
/// scanned faxes and court exhibits are routinely multi-page, and taking only
/// the first would silently lose the rest of the document.
fn decode_raster_pages(bytes: &[u8]) -> Result<Vec<image::DynamicImage>> {
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| extract_err("image format", e))?;

    if reader.format() == Some(image::ImageFormat::Tiff) {
        if let Ok(pages) = decode_tiff_pages(bytes) {
            if !pages.is_empty() {
                return Ok(pages);
            }
        }
    }
    let img = reader
        .decode()
        .map_err(|e| extract_err("image decode", e))?;
    Ok(vec![img])
}

fn decode_tiff_pages(bytes: &[u8]) -> Result<Vec<image::DynamicImage>> {
    let mut decoder = tiff::decoder::Decoder::new(std::io::Cursor::new(bytes))
        .map_err(|e| extract_err("tiff decode", e))?;
    let mut pages = Vec::new();
    loop {
        let (w, h) = decoder
            .dimensions()
            .map_err(|e| extract_err("tiff dimensions", e))?;
        let img = decoder
            .read_image()
            .map_err(|e| extract_err("tiff read", e))?;
        if let Some(dynamic) = tiff_frame_to_image(img, w, h) {
            pages.push(dynamic);
        }
        if !decoder.more_images() {
            break;
        }
        decoder
            .next_image()
            .map_err(|e| extract_err("tiff next page", e))?;
    }
    Ok(pages)
}

fn tiff_frame_to_image(
    result: tiff::decoder::DecodingResult,
    w: u32,
    h: u32,
) -> Option<image::DynamicImage> {
    match result {
        tiff::decoder::DecodingResult::U8(buf) => {
            // Infer channel count from the buffer; scans are commonly gray.
            let px = (w as usize) * (h as usize);
            if px == 0 {
                return None;
            }
            match buf.len() / px {
                1 => image::GrayImage::from_raw(w, h, buf).map(image::DynamicImage::ImageLuma8),
                3 => image::RgbImage::from_raw(w, h, buf).map(image::DynamicImage::ImageRgb8),
                4 => image::RgbaImage::from_raw(w, h, buf).map(image::DynamicImage::ImageRgba8),
                _ => None,
            }
        }
        // 16-bit and float scans are rare; OCR wants 8-bit anyway.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tiff_decodes_to_pages() {
        let mut buf = Vec::new();
        {
            use image::ImageEncoder;
            let enc = image::codecs::tiff::TiffEncoder::new(std::io::Cursor::new(&mut buf));
            let img = image::RgbImage::from_pixel(8, 8, image::Rgb([255, 255, 255]));
            enc.write_image(img.as_raw(), 8, 8, image::ExtendedColorType::Rgb8)
                .expect("encode");
        }
        let pages = decode_raster_pages(&buf).expect("decode");
        assert_eq!(pages.len(), 1);
        assert_eq!((pages[0].width(), pages[0].height()), (8, 8));
    }

    #[test]
    fn undecodable_bytes_error_rather_than_panic() {
        assert!(decode_raster_pages(b"not an image").is_err());
    }
}
