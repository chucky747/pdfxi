//! `pdfxi create ocr`: write an invisible searchable-text layer over an image.
//!
//! [`pdfxi_convert::ocr_pipeline`] validates caller-supplied OCR results,
//! thresholds them, and plans word positions in PDF points without owning a
//! recognition backend. This host supplies a JSON-backed engine, embeds the
//! real image with `create_from_image`, and appends an invisible (`3 Tr`) text
//! layer whose words sit at the planned positions. The result is a searchable
//! PDF whose visible content is the original raster.
//!
//! The JSON words file describes one page in top-left pixel coordinates:
//!
//! ```json
//! { "language": "eng",
//!   "words": [ { "text": "Hello", "x": 10, "y": 20, "width": 40, "height": 14,
//!                "confidence": 95 } ] }
//! ```

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_convert::{
    ConfidencePolicy, CreateImageFormat, CreateOptions, Margins, OcrBox, OcrEngine, OcrImage,
    OcrOptions, OcrPageText, OcrPipelineError, OcrWord, PageSize, PixelFormat, create_from_image,
    run_ocr_pipeline,
};
use pdfxi_core::{Dictionary, Document, Object, ObjectId};
use serde::{Deserialize, Serialize};

/// Maximum pixels passed to the OCR model in one image.
const MAX_OCR_PIXELS: usize = 64 * 1024 * 1024;

/// One word from the JSON file.
#[derive(Debug, Deserialize)]
struct WordInput {
    /// Recognized text.
    text: String,
    /// Left edge in pixels.
    x: f64,
    /// Top edge in pixels.
    y: f64,
    /// Box width in pixels.
    width: f64,
    /// Box height in pixels.
    height: f64,
    /// Confidence on a 0..=100 scale.
    confidence: f32,
}

/// The JSON words document.
#[derive(Debug, Deserialize)]
struct WordsFile {
    /// Detected language tag; defaults to `eng`.
    #[serde(default)]
    language: Option<String>,
    /// Words in any order; the pipeline sorts them.
    words: Vec<WordInput>,
}

/// JSON-backed OCR engine.
struct JsonEngine {
    words: Vec<OcrWord>,
    language: String,
}

impl OcrEngine for JsonEngine {
    fn name(&self) -> &str {
        "json-words"
    }

    fn recognize(
        &self,
        image: &OcrImage,
        _options: &OcrOptions,
    ) -> Result<OcrPageText, OcrPipelineError> {
        Ok(OcrPageText::new(
            image.page(),
            self.words.clone(),
            self.language.clone(),
        ))
    }
}

/// Read image dimensions from a PNG or JPEG header.
fn image_dimensions(bytes: &[u8], format: CreateImageFormat) -> anyhow::Result<(u32, u32)> {
    match format {
        CreateImageFormat::Png => {
            if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
                bail!("not a PNG file");
            }
            if &bytes[12..16] != b"IHDR" {
                bail!("PNG has no IHDR chunk");
            }
            Ok((
                u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]),
                u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]),
            ))
        }
        CreateImageFormat::Jpeg => {
            if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
                bail!("not a JPEG file");
            }
            let mut index = 2usize;
            while index + 4 <= bytes.len() {
                if bytes[index] != 0xFF {
                    index += 1;
                    continue;
                }
                let marker = bytes[index + 1];
                index += 2;
                if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
                    continue;
                }
                if marker == 0xD9 || marker == 0xDA {
                    break;
                }
                let length = usize::from(u16::from_be_bytes([bytes[index], bytes[index + 1]]));
                if length < 2 || index + length > bytes.len() {
                    bail!("JPEG segment length is invalid");
                }
                let is_sof = matches!(
                    marker,
                    0xC0 | 0xC1
                        | 0xC2
                        | 0xC3
                        | 0xC5
                        | 0xC6
                        | 0xC7
                        | 0xC9
                        | 0xCA
                        | 0xCB
                        | 0xCD
                        | 0xCE
                        | 0xCF
                );
                if is_sof {
                    if length < 7 {
                        bail!("JPEG SOF segment is too short");
                    }
                    let height = u16::from_be_bytes([bytes[index + 3], bytes[index + 4]]) as u32;
                    let width = u16::from_be_bytes([bytes[index + 5], bytes[index + 6]]) as u32;
                    return Ok((width, height));
                }
                index += length;
            }
            bail!("JPEG has no SOF frame header")
        }
    }
}

/// Classify an image path by extension.
fn image_format(path: &Path) -> anyhow::Result<CreateImageFormat> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("");
    CreateImageFormat::from_extension(extension).ok_or_else(|| {
        anyhow::anyhow!(
            "unsupported image {extension:?} for {}; expected png, jpg or jpeg",
            path.display()
        )
    })
}

/// Escape and Latin-1-encode one word for a PDF literal string.
fn escape_text(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for character in text.chars() {
        let code = u32::from(character);
        let byte = if code <= 0xFF { code as u8 } else { b'?' };
        match byte {
            b'\\' | b'(' | b')' => {
                out.push(b'\\');
                out.push(byte);
            }
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            0x20..=0x7e | 0xa0..=0xFF => out.push(byte),
            _ => out.push(b'?'),
        }
    }
    out
}

/// Format a number with two decimals and no trailing zeros.
fn number(value: f64) -> String {
    let text = format!("{:.2}", value);
    let trimmed = text.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "-0" {
        "0".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Add total fonts and streams while appending the invisible layer.
fn overlay_page(
    document: &mut Document,
    page_index: usize,
    words: &[(String, pdfxi_convert::TextPosition)],
) -> anyhow::Result<()> {
    let page = document
        .page(page_index)
        .map_err(|error| anyhow::anyhow!("read page {}: {error}", page_index + 1))?;
    if words.is_empty() {
        return Ok(());
    }
    let font_number = document
        .allocate_object_number()
        .map_err(|error| anyhow::anyhow!("allocate font: {error}"))?;
    let font_id = ObjectId::new(font_number, 0);
    let mut font = Dictionary::new();
    font.insert("Type".to_owned(), Object::Name("Font".to_owned()));
    font.insert("Subtype".to_owned(), Object::Name("Type1".to_owned()));
    font.insert("BaseFont".to_owned(), Object::Name("Helvetica".to_owned()));
    font.insert(
        "Encoding".to_owned(),
        Object::Name("WinAnsiEncoding".to_owned()),
    );
    document
        .set_object(font_id, Object::Dictionary(font))
        .map_err(|error| anyhow::anyhow!("stage OCR font: {error}"))?;

    let mut content = String::new();
    for (text, position) in words {
        let size = position.height_pt.max(1.0);
        content.push_str(&format!(
            "BT 3 Tr /FOcr {} Tf {} {} Td (",
            number(size),
            number(position.x_pt),
            number(position.y_pt)
        ));
        content.push_str(&String::from_utf8_lossy(&escape_text(text)));
        content.push_str(") Tj ET\n");
    }
    let stream_number = document
        .allocate_object_number()
        .map_err(|error| anyhow::anyhow!("allocate content stream: {error}"))?;
    let stream_id = ObjectId::new(stream_number, 0);
    let mut stream = Dictionary::new();
    stream.insert(
        "Length".to_owned(),
        Object::Integer(i64::try_from(content.len()).unwrap_or(i64::MAX)),
    );
    document
        .set_object(
            stream_id,
            Object::Stream {
                dictionary: stream,
                data: content.into_bytes(),
            },
        )
        .map_err(|error| anyhow::anyhow!("stage OCR content: {error}"))?;

    let mut page_dictionary = match document.object(page.id)? {
        Object::Dictionary(dictionary) => dictionary,
        Object::Stream { dictionary, .. } => dictionary,
        _ => bail!("page {} is not a dictionary", page_index + 1),
    };
    let mut resources = match page_dictionary.get("Resources") {
        Some(Object::Reference(id)) => document
            .object(*id)?
            .as_dictionary()
            .cloned()
            .unwrap_or_default(),
        Some(Object::Dictionary(dictionary)) => dictionary.clone(),
        _ => Dictionary::new(),
    };
    let mut fonts = match resources.get("Font") {
        Some(Object::Reference(id)) => document
            .object(*id)?
            .as_dictionary()
            .cloned()
            .unwrap_or_default(),
        Some(Object::Dictionary(dictionary)) => dictionary.clone(),
        _ => Dictionary::new(),
    };
    fonts.insert("FOcr".to_owned(), Object::Reference(font_id));
    resources.insert("Font".to_owned(), Object::Dictionary(fonts));
    page_dictionary.insert("Resources".to_owned(), Object::Dictionary(resources));
    let contents = match page_dictionary.get("Contents") {
        None | Some(Object::Null) => Object::Reference(stream_id),
        Some(Object::Reference(id)) => {
            Object::Array(vec![Object::Reference(*id), Object::Reference(stream_id)])
        }
        Some(Object::Array(items)) => {
            let mut items = items.clone();
            items.push(Object::Reference(stream_id));
            Object::Array(items)
        }
        Some(_) => bail!("page {} has unsupported /Contents", page_index + 1),
    };
    page_dictionary.insert("Contents".to_owned(), contents);
    document
        .set_object(page.id, Object::Dictionary(page_dictionary))
        .map_err(|error| anyhow::anyhow!("stage OCR page: {error}"))?;
    Ok(())
}

/// JSON shape of `pdfxi create ocr`.
#[derive(Serialize)]
struct OcrReport {
    /// Source image.
    image: String,
    /// Written PDF.
    out: String,
    /// Declared image resolution.
    dpi: f64,
    /// Words supplied.
    words_received: usize,
    /// Engine name.
    engine: String,
    /// Planned pages.
    pages: usize,
    /// Words kept in the layer.
    kept: usize,
    /// Words dropped by confidence.
    rejected: usize,
    /// Kept words flagged for review.
    suspect: usize,
    /// Pages with no kept words.
    empty_pages: Vec<usize>,
    /// Plan summary.
    summary: String,
    /// Page width in points.
    page_width: f64,
    /// Page height in points.
    page_height: f64,
    /// Kept words found by text extraction.
    verified_words: usize,
    /// Characters extracted from the output.
    extracted_chars: usize,
    /// Bytes written.
    bytes: usize,
}

/// `pdfxi create ocr`.
pub(crate) fn create_ocr(
    image: &Path,
    out: &Path,
    words_path: &Path,
    dpi: f64,
    language: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let image_bytes =
        std::fs::read(image).with_context(|| format!("read image {}", image.display()))?;
    let format = image_format(image)?;
    let (width_px, height_px) = image_dimensions(&image_bytes, format)
        .with_context(|| format!("read image header of {}", image.display()))?;
    let pixels = usize::try_from(u64::from(width_px) * u64::from(height_px))
        .context("image pixel count overflows")?;
    if pixels > MAX_OCR_PIXELS {
        bail!(
            "image {} has {pixels} pixels; the OCR limit is {MAX_OCR_PIXELS}",
            image.display()
        );
    }

    let words_text = std::fs::read_to_string(words_path)
        .with_context(|| format!("read words {}", words_path.display()))?;
    let parsed: WordsFile = serde_json::from_str(&words_text)
        .with_context(|| format!("parse words {}", words_path.display()))?;
    if parsed.words.is_empty() {
        bail!("{} contains no words", words_path.display());
    }
    let mut engine_words = Vec::with_capacity(parsed.words.len());
    for (index, word) in parsed.words.iter().enumerate() {
        if word.text.trim().is_empty() {
            bail!("word {index} has empty text");
        }
        engine_words.push(OcrWord::new(
            word.text.clone(),
            OcrBox::new(word.x, word.y, word.width, word.height),
            word.confidence,
        ));
    }
    let detected_language = language
        .map(str::to_owned)
        .or(parsed.language)
        .unwrap_or_else(|| "eng".to_owned());
    let engine = JsonEngine {
        words: engine_words,
        language: detected_language.clone(),
    };
    let ocr_image = OcrImage::new(
        0,
        width_px,
        height_px,
        dpi,
        PixelFormat::Grayscale,
        vec![0u8; pixels],
    )
    .map_err(|error| anyhow::anyhow!("ocr image: {error}"))?;
    let options = OcrOptions::new(vec![detected_language.clone()]);
    let plan = run_ocr_pipeline(
        &[ocr_image],
        &engine,
        &options,
        &ConfidencePolicy::default(),
    )
    .map_err(|error| anyhow::anyhow!("ocr pipeline: {error}"))?;

    let scale = 72.0 / dpi;
    let page_width = f64::from(width_px) * scale;
    let page_height = f64::from(height_px) * scale;
    let create = CreateOptions::new()
        .with_page_size(PageSize::new(page_width, page_height)?)
        .with_margins(Margins::NONE);
    let pdf = create_from_image(&image_bytes, format, &create)
        .with_context(|| format!("embed image {}", image.display()))?;
    let mut document = Document::from_bytes(pdf)
        .map_err(|error| anyhow::anyhow!("parse embedded image: {error}"))?;
    for layer in plan.layers() {
        let words: Vec<(String, pdfxi_convert::TextPosition)> = layer
            .placed_words()
            .iter()
            .map(|word| (word.text().to_owned(), word.position()))
            .collect();
        overlay_page(&mut document, layer.page(), &words)?;
    }
    let saved = document
        .save()
        .with_context(|| format!("save {}", out.display()))?;
    std::fs::write(out, &saved).with_context(|| format!("write {}", out.display()))?;

    let reopened = Document::from_bytes(saved.clone())
        .map_err(|error| anyhow::anyhow!("reopen {}: {error}", out.display()))?;
    let page_text = reopened
        .page_text(0)
        .map_err(|error| anyhow::anyhow!("extract page text: {error}"))?;
    let mut verified_words = 0usize;
    for layer in plan.layers() {
        verified_words += layer
            .placed_words()
            .iter()
            .filter(|word| page_text.contains(word.text()))
            .count();
    }
    let kept = plan.report().kept_words();
    if verified_words < kept {
        bail!("only {verified_words} of {kept} searchable words extract from the output");
    }

    let report = OcrReport {
        image: image.display().to_string(),
        out: out.display().to_string(),
        dpi,
        words_received: parsed.words.len(),
        engine: plan.report().engine().to_owned(),
        pages: plan.report().page_count(),
        kept,
        rejected: plan.report().rejected_words(),
        suspect: plan.report().suspect_words(),
        empty_pages: plan.report().empty_pages().to_vec(),
        summary: plan.summary().to_owned(),
        page_width,
        page_height,
        verified_words,
        extracted_chars: page_text.chars().count(),
        bytes: saved.len(),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("image: {}", report.image);
    println!("out:   {}", report.out);
    println!(
        "dpi: {}  page: {:.2}x{:.2} pt",
        report.dpi, report.page_width, report.page_height
    );
    println!(
        "words: {} received, {} kept, {} rejected, {} suspect",
        report.words_received, report.kept, report.rejected, report.suspect
    );
    println!("engine: {}", report.engine);
    println!("summary: {}", report.summary);
    println!("verified_words: {}", report.verified_words);
    println!("extracted_chars: {}", report.extracted_chars);
    println!("bytes: {}", report.bytes);
    Ok(())
}
