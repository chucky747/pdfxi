//! `pdfxi font add-text`: embed the bundled CID font and write text with it.
//!
//! [`pdfxi_edit::add_text_box`] embeds the bundled Courier Prime program as a
//! `/Type0` font with a `/CIDFontType2` descendant, a `/FontFile2` stream, a
//! `/CIDToGIDMap` and the CID-keyed `/W` widths and `/ToUnicode` CMap produced by
//! `pdfxi_core::font::stage_cid_font`. This host applies that path to a real
//! document, saves it, and then reopens the output to verify the font graph and
//! that the inserted text is extractable through the ToUnicode mapping.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_core::{Document, Object, ObjectId};
use pdfxi_edit::{TextAlign, add_text_box};
use serde::Serialize;

/// One `pdfxi font add-text` request.
pub(crate) struct AddTextRequest<'a> {
    /// Input file.
    pub file: &'a Path,
    /// Output file.
    pub out: &'a Path,
    /// One-based page.
    pub page: usize,
    /// `x0,y0,x1,y1` in points.
    pub rect: &'a str,
    /// Text to place.
    pub text: &'a str,
    /// Font size in points.
    pub font_size: f64,
    /// `left`, `center` or `right`.
    pub align: &'a str,
    /// Emit JSON.
    pub json: bool,
}

/// Verified font graph in the saved document.
#[derive(Serialize)]
struct FontSummary {
    /// Face name from the Type0 `/BaseFont`.
    base_font: String,
    /// Type0 object id.
    type0_object: u32,
    /// Descendant subtype (`CIDFontType2`).
    descendant_subtype: String,
    /// Whether the descriptor carries a font program.
    has_font_file: bool,
    /// Whether the Type0 dictionary carries `/ToUnicode`.
    has_to_unicode: bool,
}

/// JSON shape of `pdfxi font add-text`.
#[derive(Serialize)]
struct AddTextReport {
    /// Input file.
    file: String,
    /// Written file.
    out: String,
    /// One-based page.
    page: usize,
    /// Box in points.
    rect: [f32; 4],
    /// Lines emitted.
    lines: usize,
    /// Font size.
    font_size: f64,
    /// Alignment label.
    align: String,
    /// Page resource name of the font.
    resource: String,
    /// Font graph in the saved bytes.
    font: FontSummary,
    /// Words from the inserted text found by extraction.
    words_found: usize,
    /// Distinct words checked.
    words_checked: usize,
}

/// Parse the alignment flag.
fn parse_align(text: &str) -> anyhow::Result<TextAlign> {
    match text.to_ascii_lowercase().as_str() {
        "left" => Ok(TextAlign::Left),
        "center" | "centre" => Ok(TextAlign::Center),
        "right" => Ok(TextAlign::Right),
        other => bail!("unknown --align {other:?}; expected left, center or right"),
    }
}

/// Resolve an indirect or direct object.
fn resolve(document: &Document, object: &Object) -> anyhow::Result<Object> {
    match object {
        Object::Reference(id) => Ok(document.object(*id)?.clone()),
        other => Ok(other.clone()),
    }
}

/// Resolve the page `/Resources`, walking `/Parent` when it is inherited.
fn page_resources(document: &Document, page_id: ObjectId) -> anyhow::Result<Object> {
    let mut current = document.object(page_id)?;
    for _ in 0..32 {
        let Some(dictionary) = current.as_dictionary() else {
            bail!("page tree node is not a dictionary");
        };
        if let Some(resources) = dictionary.get("Resources") {
            return resolve(document, resources);
        }
        let Some(parent) = dictionary.get("Parent") else {
            bail!("page has no /Resources and no /Parent to inherit from");
        };
        current = resolve(document, parent)?;
    }
    bail!("page resource inheritance chain is too deep");
}

/// Verify the embedded graph of the named font resource.
fn verify_font(
    document: &Document,
    page_index: usize,
    resource: &str,
) -> anyhow::Result<FontSummary> {
    let page = document
        .page(page_index)
        .map_err(|error| anyhow::anyhow!("read page {}: {error}", page_index + 1))?;
    let resources = page_resources(document, page.id)?;
    let resources = resources
        .as_dictionary()
        .context("page /Resources is not a dictionary")?;
    let fonts = resources
        .get("Font")
        .context("page has no /Font resources")?;
    let fonts = resolve(document, fonts)?;
    let fonts = fonts
        .as_dictionary()
        .context("page /Font is not a dictionary")?;
    let value = fonts
        .get(resource)
        .with_context(|| format!("page /Font has no resource {resource:?}"))?;
    let (type0_id, type0_object) = match value {
        Object::Reference(id) => (Some(*id), document.object(*id)?),
        other => (None, other.clone()),
    };
    let type0 = type0_object
        .as_dictionary()
        .context("font resource is not a dictionary")?;
    let subtype = type0.get("Subtype").and_then(Object::as_name);
    if subtype != Some("Type0") {
        bail!("font resource subtype is {subtype:?}, expected Type0");
    }
    let base_font = type0
        .get("BaseFont")
        .and_then(Object::as_name)
        .unwrap_or("")
        .to_owned();
    let has_to_unicode = type0.contains_key("ToUnicode");
    let descendants = type0
        .get("DescendantFonts")
        .context("Type0 font has no /DescendantFonts")?;
    let descendants = resolve(document, descendants)?;
    let descendants = descendants
        .as_array()
        .context("/DescendantFonts is not an array")?;
    let descendant = descendants.first().context("/DescendantFonts is empty")?;
    let descendant = resolve(document, descendant)?;
    let descendant = descendant
        .as_dictionary()
        .context("descendant font is not a dictionary")?;
    let descendant_subtype = descendant
        .get("Subtype")
        .and_then(Object::as_name)
        .unwrap_or("")
        .to_owned();
    if descendant_subtype != "CIDFontType2" {
        bail!("descendant subtype is {descendant_subtype:?}, expected CIDFontType2");
    }
    let descriptor = descendant
        .get("FontDescriptor")
        .context("CID font has no /FontDescriptor")?;
    let descriptor = resolve(document, descriptor)?;
    let descriptor = descriptor
        .as_dictionary()
        .context("font descriptor is not a dictionary")?;
    let has_font_file = descriptor.contains_key("FontFile2")
        || descriptor.contains_key("FontFile3")
        || descriptor.contains_key("FontFile");
    if !has_font_file {
        bail!("font descriptor carries no embedded program");
    }
    if !has_to_unicode {
        bail!("Type0 font has no /ToUnicode CMap");
    }

    Ok(FontSummary {
        base_font,
        type0_object: type0_id.map_or(0, |id: ObjectId| id.number),
        descendant_subtype,
        has_font_file,
        has_to_unicode,
    })
}

/// `pdfxi font add-text`.
pub(crate) fn run_add_text(request: AddTextRequest<'_>) -> anyhow::Result<()> {
    if request.page == 0 {
        bail!("--page is one-based and must be at least 1");
    }
    if request.text.trim().is_empty() {
        bail!("--text must not be blank");
    }
    let align = parse_align(request.align)?;
    let rect = crate::annotate::parse_rect(request.rect)?;
    let bytes =
        std::fs::read(request.file).with_context(|| format!("read {}", request.file.display()))?;
    let mut document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", request.file.display()))?;
    let placement = add_text_box(
        &mut document,
        request.page - 1,
        rect,
        request.text,
        request.font_size,
        align,
    )
    .map_err(|error| anyhow::anyhow!("add text box: {error}"))?;
    let saved = document
        .save()
        .with_context(|| format!("save {}", request.out.display()))?;
    std::fs::write(request.out, &saved)
        .with_context(|| format!("write {}", request.out.display()))?;

    let reopened = Document::from_bytes(saved)
        .map_err(|error| anyhow::anyhow!("reopen {}: {error}", request.out.display()))?;
    let font = verify_font(&reopened, request.page - 1, &placement.font.resource)?;
    let page_text = reopened
        .page_text(request.page - 1)
        .with_context(|| format!("extract text from page {}", request.page))?;
    let words: Vec<&str> = request
        .text
        .split_whitespace()
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .collect();
    let words_found = words
        .iter()
        .filter(|word| page_text.contains(**word))
        .count();
    if words.is_empty() {
        bail!("--text needs at least one word with letters or digits");
    }
    if words_found != words.len() {
        bail!(
            "only {words_found} of {} inserted words were extractable from the saved page",
            words.len()
        );
    }

    let report = AddTextReport {
        file: request.file.display().to_string(),
        out: request.out.display().to_string(),
        page: request.page,
        rect: [rect.x0(), rect.y0(), rect.x1(), rect.y1()],
        lines: placement.lines,
        font_size: request.font_size,
        align: request.align.to_owned(),
        resource: placement.font.resource.clone(),
        font,
        words_found,
        words_checked: words.len(),
    };
    if request.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("file: {}", report.file);
    println!("out:  {}", report.out);
    println!("page: {}", report.page);
    println!(
        "rect: {} {} {} {}",
        report.rect[0], report.rect[1], report.rect[2], report.rect[3]
    );
    println!("lines: {}", report.lines);
    println!("font_size: {}", report.font_size);
    println!("align: {}", report.align);
    println!("resource: {}", report.resource);
    println!(
        "font: {}  Type0 object {}  descendant {}  font_file: {}  to_unicode: {}",
        report.font.base_font,
        report.font.type0_object,
        report.font.descendant_subtype,
        report.font.has_font_file,
        report.font.has_to_unicode
    );
    println!("words: {}/{}", report.words_found, report.words_checked);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_align;
    use pdfxi_edit::TextAlign;

    #[test]
    fn alignments_parse_and_reject_unknown_names() {
        assert_eq!(parse_align("Left").expect("left"), TextAlign::Left);
        assert_eq!(parse_align("center").expect("center"), TextAlign::Center);
        assert_eq!(parse_align("centre").expect("centre"), TextAlign::Center);
        assert_eq!(parse_align("right").expect("right"), TextAlign::Right);
        assert!(parse_align("justified").is_err());
    }
}
