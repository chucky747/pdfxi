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
//!
//! With `--engine` (or no `--words`) the host recognizes the image itself:
//! `tesseract` runs the installed Tesseract CLI and parses its TSV output;
//! `windows` runs the bundled `scripts/windows-ocr.ps1` sidecar over the
//! built-in Windows.Media.Ocr engine. `auto` prefers Tesseract and falls back
//! to Windows OCR. A recognition failure is a typed error, never an empty
//! searchable layer.

use std::path::Path;
use std::process::Command;

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

/// The Windows OCR sidecar embedded at build time so an installed CLI does not
/// depend on the repository layout.
const WINDOWS_OCR_SCRIPT: &str = include_str!("../../../scripts/windows-ocr.ps1");

/// Recognition backend selected by `create ocr --engine`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum OcrBackend {
    /// Tesseract when installed, otherwise the Windows OCR engine.
    #[default]
    Auto,
    /// The Tesseract command-line engine.
    Tesseract,
    /// The built-in Windows.Media.Ocr engine.
    Windows,
}

impl OcrBackend {
    /// Stable label used in reports.
    const fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Tesseract => "tesseract",
            Self::Windows => "windows",
        }
    }
}

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

/// Parse Tesseract's TSV output into the words model.
///
/// Tesseract writes one header row plus one row per level; level 5 rows carry
/// word boxes in input-image pixels with a 0..=100 confidence and `-1` for
/// non-text rows. Empty rows and low-level rows are skipped.
fn parse_tesseract_tsv(text: &str) -> anyhow::Result<WordsFile> {
    let mut lines = text.lines();
    let header = lines.next().unwrap_or_default();
    if !header.starts_with("level\tpage_num\tblock_num") {
        bail!("tesseract output is not TSV: missing the column header");
    }
    let mut words = Vec::new();
    for line in lines {
        let columns: Vec<&str> = line.split('\t').collect();
        if columns.len() < 12 {
            continue;
        }
        let Ok(level) = columns[0].parse::<u8>() else {
            continue;
        };
        if level != 5 {
            continue;
        }
        let word = columns[11].trim();
        if word.is_empty() {
            continue;
        }
        let Ok(left) = columns[6].parse::<f64>() else {
            continue;
        };
        let Ok(top) = columns[7].parse::<f64>() else {
            continue;
        };
        let Ok(width) = columns[8].parse::<f64>() else {
            continue;
        };
        let Ok(height) = columns[9].parse::<f64>() else {
            continue;
        };
        let Ok(confidence) = columns[10].parse::<f32>() else {
            continue;
        };
        if confidence < 0.0 {
            continue;
        }
        words.push(WordInput {
            text: word.to_owned(),
            x: left,
            y: top,
            width,
            height,
            confidence,
        });
    }
    if words.is_empty() {
        bail!("tesseract reported no words");
    }
    Ok(WordsFile {
        language: None,
        words,
    })
}

/// Parse the Windows OCR sidecar's JSON into the words model.
fn parse_windows_json(text: &str) -> anyhow::Result<WordsFile> {
    let parsed: WordsFile =
        serde_json::from_str(text).context("parse the Windows OCR JSON output")?;
    if parsed.words.is_empty() {
        bail!("the Windows OCR engine reported no words");
    }
    Ok(parsed)
}

/// Run Tesseract over `image` and return its words.
fn tesseract_words(image: &Path, language: &str) -> anyhow::Result<WordsFile> {
    let output = Command::new("tesseract")
        .arg(image)
        .arg("stdout")
        .arg("-l")
        .arg(language)
        .arg("tsv")
        .output()
        .with_context(|| "start tesseract (install Tesseract or use --engine windows)")?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        bail!("tesseract exited {}: {}", output.status, error.trim());
    }
    parse_tesseract_tsv(&String::from_utf8_lossy(&output.stdout))
}

/// Map a Tesseract-style language tag to the Windows OCR tag.
///
/// Tesseract uses ISO 639-2 (`eng`, `deu`, `chi_sim`); Windows accepts BCP 47
/// base tags (`en`, `de`, `zh-Hans`). Unknown tags are passed through.
fn windows_language_tag(tag: &str) -> String {
    let lower = tag.to_ascii_lowercase();
    let mapped = match lower.as_str() {
        "eng" => "en",
        "deu" | "ger" => "de",
        "fra" | "fre" => "fr",
        "spa" => "es",
        "ita" => "it",
        "por" => "pt",
        "nld" | "dut" => "nl",
        "jpn" => "ja",
        "kor" => "ko",
        "rus" => "ru",
        "chi_sim" => "zh-Hans",
        "chi_tra" => "zh-Hant",
        other => other,
    };
    mapped.to_owned()
}

/// Run the embedded Windows OCR sidecar over `image` and return its words.
fn windows_words(image: &Path, language: Option<&str>) -> anyhow::Result<WordsFile> {
    let script = std::env::temp_dir().join(format!("pdfxi-windows-ocr-{}.ps1", std::process::id()));
    std::fs::write(&script, WINDOWS_OCR_SCRIPT)
        .with_context(|| format!("write the OCR sidecar to {}", script.display()))?;
    let mut command = Command::new("powershell");
    command
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .arg("-Image")
        .arg(image);
    if let Some(tag) = language {
        command.arg("-Language").arg(windows_language_tag(tag));
    }
    let output = command
        .output()
        .with_context(|| "start the Windows OCR sidecar (PowerShell is required)")?;
    let _ = std::fs::remove_file(&script);
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        bail!(
            "the Windows OCR engine failed ({}): {}",
            output.status,
            error.trim()
        );
    }
    parse_windows_json(&String::from_utf8_lossy(&output.stdout))
}

/// Recognize `image` with the selected backend.
fn recognize_with_engine(
    image: &Path,
    backend: OcrBackend,
    language: Option<&str>,
) -> anyhow::Result<WordsFile> {
    match backend {
        OcrBackend::Tesseract => tesseract_words(image, language.unwrap_or("eng")),
        OcrBackend::Windows => windows_words(image, language),
        OcrBackend::Auto => match tesseract_words(image, language.unwrap_or("eng")) {
            Ok(words) => Ok(words),
            Err(tesseract_error) => windows_words(image, language).map_err(|windows_error| {
                anyhow::anyhow!(
                    "no OCR engine could recognize {}: tesseract: {tesseract_error}; windows: {windows_error}",
                    image.display()
                )
            }),
        },
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
    words_path: Option<&Path>,
    backend: OcrBackend,
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

    let parsed = match words_path {
        Some(path) => {
            let words_text = std::fs::read_to_string(path)
                .with_context(|| format!("read words {}", path.display()))?;
            serde_json::from_str::<WordsFile>(&words_text)
                .with_context(|| format!("parse words {}", path.display()))?
        }
        None => {
            let recognized = recognize_with_engine(image, backend, language)?;
            if backend == OcrBackend::Auto {
                eprintln!("note: recognized with an automatic OCR backend");
            } else {
                eprintln!("note: recognized with {}", backend.label());
            }
            recognized
        }
    };
    if parsed.words.is_empty() {
        match words_path {
            Some(path) => bail!("{} contains no words", path.display()),
            None => bail!("no OCR words were found for {}", image.display()),
        }
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

#[cfg(test)]
mod tests {
    use super::{OcrBackend, parse_tesseract_tsv, parse_windows_json, windows_language_tag};

    const TSV: &str = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
1\t1\t0\t0\t0\t0\t0\t0\t100\t50\t-1\t\n\
5\t1\t1\t1\t1\t1\t10\t20\t40\t14\t95.5\tHello\n\
5\t1\t1\t1\t1\t2\t60\t20\t30\t14\t88\tworld\n";

    #[test]
    fn tesseract_tsv_keeps_level_five_words_with_boxes_and_confidence() {
        let words = parse_tesseract_tsv(TSV).expect("parse TSV");
        assert_eq!(words.words.len(), 2);
        assert_eq!(words.words[0].text, "Hello");
        assert_eq!(words.words[0].x, 10.0);
        assert_eq!(words.words[0].y, 20.0);
        assert_eq!(words.words[0].width, 40.0);
        assert_eq!(words.words[0].height, 14.0);
        assert!((words.words[0].confidence - 95.5).abs() < f32::EPSILON);
        assert_eq!(words.words[1].text, "world");
    }

    #[test]
    fn tesseract_tsv_rejects_non_tsv_and_empty_output() {
        assert!(parse_tesseract_tsv("not a table").is_err());
        let header_only = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n";
        assert!(parse_tesseract_tsv(header_only).is_err());
    }

    #[test]
    fn windows_ocr_json_parses_words_and_rejects_empty_output() {
        let json = r#"{"language":"en-GB","words":[{"text":"PDFXI","x":107.0,"y":116.0,"width":49.0,"height":14.0,"confidence":100.0}]}"#;
        let words = parse_windows_json(json).expect("parse JSON");
        assert_eq!(words.language.as_deref(), Some("en-GB"));
        assert_eq!(words.words.len(), 1);
        assert_eq!(words.words[0].text, "PDFXI");
        assert_eq!(words.words[0].x, 107.0);
        assert!(parse_windows_json(r#"{"words":[]}"#).is_err());
        assert!(parse_windows_json("not json").is_err());
    }

    #[test]
    fn tesseract_tags_map_to_windows_ocr_tags() {
        assert_eq!(windows_language_tag("eng"), "en");
        assert_eq!(windows_language_tag("DEU"), "de");
        assert_eq!(windows_language_tag("chi_sim"), "zh-Hans");
        assert_eq!(windows_language_tag("en-GB"), "en-gb");
        assert_eq!(OcrBackend::default(), OcrBackend::Auto);
    }
}
