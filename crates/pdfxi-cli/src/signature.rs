//! `pdfxi signature`: electronic signature fields and the signer workflow.
//!
//! [`pdfxi_forms::signature`] and [`pdfxi_forms::signatureplan`] own the
//! `/FT /Sig` field model, the placeholder contract and the deterministic
//! `/ByteRange` planning; [`pdfxi_forms::signing`] owns the structure-only
//! verification boundary. This host creates a real one-page PDF with a
//! contract-checked signature field and a `/Contents` placeholder, plans the
//! exact byte range a signer must cover, and verifies caller-supplied signature
//! bytes structurally. No cryptography is performed.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_forms::{
    FormSource, ObjectRef, PdfDocument, PdfValue, SignatureField, SignaturePlan, SignatureVerifier,
    StructureOnlyVerifier, VerificationRequest, WidgetRect, load_acroform, parse_pdf,
    plan_byte_range, serialize_form_pdf, signed_bytes, validate_byte_range,
};
use serde::Serialize;

/// The page object number used by the serializer.
const PAGE_OBJECT: u32 = 5;
/// Page width in points.
const PAGE_WIDTH: f32 = 612.0;
/// Page height in points.
const PAGE_HEIGHT: f32 = 792.0;

/// JSON shape of `pdfxi signature create`.
#[derive(Serialize)]
struct CreateReport {
    /// Written file.
    out: String,
    /// Total bytes.
    bytes: usize,
    /// Field name.
    field: String,
    /// Signature line.
    signature_line: String,
    /// Placeholder hex characters.
    placeholder_bytes: usize,
    /// Widget rectangle.
    rect: [f32; 4],
    /// Whether the model reloaded the field.
    reloaded: bool,
}

/// JSON shape of `pdfxi signature plan`.
#[derive(Serialize)]
struct PlanReport {
    /// Source PDF.
    file: String,
    /// `/ByteRange` array.
    byte_range: [u64; 4],
    /// Bytes the signer must hash.
    signed_bytes: usize,
    /// Placeholder hex characters.
    placeholder_bytes: usize,
    /// `/Filter`.
    filter: String,
    /// `/SubFilter`.
    subfilter: String,
}

/// JSON shape of `pdfxi signature verify`.
#[derive(Serialize)]
struct VerifyReport {
    /// Source PDF.
    file: String,
    /// Field name.
    field: String,
    /// `/Filter`.
    filter: String,
    /// `/SubFilter`.
    subfilter: String,
    /// Structure-only validity.
    valid: bool,
    /// Authenticity token (always `structure-only`).
    authenticity: String,
    /// Verifier detail.
    detail: String,
    /// `/ByteRange` array.
    byte_range: [u64; 4],
    /// Signature byte count.
    contents_bytes: usize,
}

/// Parse `x,y,w,h`.
fn parse_rect(text: &str) -> anyhow::Result<WidgetRect> {
    let values: Vec<f32> = text
        .split(',')
        .map(|part| {
            part.trim()
                .parse::<f32>()
                .with_context(|| format!("rectangle value {:?} is not a number", part.trim()))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    if values.len() != 4 {
        bail!("--rect needs four comma-separated numbers (x,y,w,h)");
    }
    let rect = WidgetRect::new(values[0], values[1], values[2], values[3]);
    if !rect.is_finite() {
        bail!("--rect values must be finite");
    }
    if rect.width <= 0.0 || rect.height <= 0.0 {
        bail!("--rect needs positive width and height");
    }
    if rect.x < 0.0
        || rect.y < 0.0
        || rect.x + rect.width > PAGE_WIDTH
        || rect.y + rect.height > PAGE_HEIGHT
    {
        bail!("--rect is outside the {PAGE_WIDTH}x{PAGE_HEIGHT} page");
    }
    Ok(rect)
}

/// Find a byte sequence.
fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Convert the zero `/Contents` literal string into the hex placeholder.
///
/// The serializer emits a `/Contents` value of `N` zero characters as a
/// literal string `(000...)`; swapping the two delimiters yields the hex
/// placeholder `<000...>` **without changing the byte length**, so every xref
/// offset stays valid.
fn hexify_contents(bytes: &mut [u8], size: usize) -> anyhow::Result<()> {
    let token = format!("({})", "0".repeat(size));
    let start =
        find_bytes(bytes, token.as_bytes()).context("/Contents zero placeholder not found")?;
    bytes[start] = b'<';
    bytes[start + token.len() - 1] = b'>';
    Ok(())
}
/// Find the `/Contents <hex>` placeholder token in prepared bytes.
fn find_placeholder(bytes: &[u8]) -> anyhow::Result<String> {
    let start = find_bytes(bytes, b"/Contents <").context("/Contents placeholder not found")?
        + b"/Contents <".len();
    let tail = &bytes[start..];
    let length = tail
        .iter()
        .position(|byte| *byte == b'>')
        .context("unterminated /Contents placeholder")?;
    let hex = std::str::from_utf8(&tail[..length]).context("/Contents placeholder is not UTF-8")?;
    if hex.is_empty() || !hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        bail!("/Contents placeholder is not a hex string");
    }
    Ok(hex.to_owned())
}
/// A parsed-document form source.
struct ParsedSource<'a>(&'a PdfDocument);

impl FormSource for ParsedSource<'_> {
    fn catalog(&self) -> PdfValue {
        self.0
            .resolve(&PdfValue::Reference(self.0.root))
            .unwrap_or(PdfValue::Null)
    }

    fn resolve(&self, value: &PdfValue) -> Option<PdfValue> {
        self.0.resolve(value).ok()
    }
}

/// Read the `/Filter`/`/SubFilter` of a prepared signature document.
fn signature_metadata(bytes: &[u8]) -> anyhow::Result<(String, String, String)> {
    let document =
        parse_pdf(bytes).map_err(|error| anyhow::anyhow!("parse signature PDF: {error}"))?;
    let source = ParsedSource(&document);
    let load = load_acroform(&source).map_err(|error| anyhow::anyhow!("load form: {error}"))?;
    let form = load.form().context("the document has no /AcroForm")?;
    for field in form.fields() {
        if field.field_type().to_string() != "signature" {
            continue;
        }
        let name = field.name().to_owned();
        let (filter, subfilter) = parse_v_names(bytes)?;
        return Ok((name, filter, subfilter));
    }
    bail!("the document has no signature field")
}

/// Read `/Filter` and `/SubFilter` names from the serialized bytes.
fn parse_v_names(bytes: &[u8]) -> anyhow::Result<(String, String)> {
    let read = |marker: &[u8]| -> anyhow::Result<String> {
        let start = find_bytes(bytes, marker)
            .with_context(|| format!("{} not found", String::from_utf8_lossy(marker)))?;
        let tail = &bytes[start + marker.len()..];
        let end = tail
            .iter()
            .position(|byte| byte.is_ascii_whitespace() || *byte == b'/')
            .unwrap_or(tail.len());
        Ok(String::from_utf8_lossy(&tail[..end]).into_owned())
    };
    Ok((read(b"/Filter /")?, read(b"/SubFilter /")?))
}

/// `pdfxi signature create`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn create(
    out: &Path,
    field: &str,
    signer: Option<&str>,
    reason: Option<&str>,
    location: Option<&str>,
    rect: &str,
    placeholder_bytes: usize,
    json: bool,
) -> anyhow::Result<()> {
    let rect = parse_rect(rect)?;
    let mut signature = SignatureField::new(field, rect, 0);
    if let Some(signer) = signer {
        signature = signature.with_name(signer);
    }
    if let Some(reason) = reason {
        signature = signature.with_reason(reason);
    }
    if let Some(location) = location {
        signature = signature.with_location(location);
    }
    let signature_line = signature.signature_line();
    let plan = SignaturePlan::new(&[signature], placeholder_bytes)
        .map_err(|error| anyhow::anyhow!("signature plan: {error}"))?;
    let entry = plan
        .entries()
        .first()
        .context("signature plan has no entry")?;
    let mut field = entry.field_dictionary().clone();
    if let PdfValue::Dictionary(dict) = &mut field
        && let Some(PdfValue::Dictionary(signature)) = dict.get_mut("V")
    {
        signature.insert(
            "Contents".to_owned(),
            PdfValue::Text("0".repeat(placeholder_bytes)),
        );
    }
    let fields = vec![field];
    let widgets = vec![entry.widget().clone()];
    let page = ObjectRef::new(PAGE_OBJECT, 0);
    let mut bytes = serialize_form_pdf(&fields, &widgets, Some(page))
        .map_err(|error| anyhow::anyhow!("serialize signature form: {error}"))?;
    hexify_contents(&mut bytes, placeholder_bytes)?;

    let document =
        parse_pdf(&bytes).map_err(|error| anyhow::anyhow!("reparse signature PDF: {error}"))?;
    let source = ParsedSource(&document);
    let loaded = load_acroform(&source)
        .map_err(|error| anyhow::anyhow!("reload signature form: {error}"))?;
    let form = loaded.form().context("the written PDF has no /AcroForm")?;

    let reloaded = form.fields().any(|loaded| {
        loaded.name() == entry.field() && loaded.field_type().to_string() == "signature"
    });
    if !reloaded {
        bail!(
            "the written PDF did not reload the signature field {:?}",
            entry.field()
        );
    }
    find_placeholder(&bytes)?;

    std::fs::write(out, &bytes).with_context(|| format!("write {}", out.display()))?;
    let report = CreateReport {
        out: out.display().to_string(),
        bytes: bytes.len(),
        field: entry.field().to_owned(),
        signature_line,
        placeholder_bytes,
        rect: [rect.x, rect.y, rect.width, rect.height],
        reloaded,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!(
        "out: {}  bytes: {}  field: {}  placeholder: {} hex chars",
        report.out, report.bytes, report.field, report.placeholder_bytes
    );
    println!("signature: {}", report.signature_line);
    println!(
        "rect: [{}, {}, {}, {}]  reloaded: {}",
        report.rect[0], report.rect[1], report.rect[2], report.rect[3], report.reloaded
    );
    Ok(())
}

/// `pdfxi signature plan`.
pub(crate) fn plan(file: &Path, json: bool) -> anyhow::Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let placeholder = find_placeholder(&bytes)?;
    let plan = plan_byte_range(&bytes, placeholder.as_bytes())
        .map_err(|error| anyhow::anyhow!("byte range: {error}"))?;
    validate_byte_range(&bytes, &plan, placeholder.as_bytes())
        .map_err(|error| anyhow::anyhow!("validate byte range: {error}"))?;
    let array = plan.as_array().context("byte range is incomplete")?;
    let signed =
        signed_bytes(&bytes, &plan).map_err(|error| anyhow::anyhow!("signed bytes: {error}"))?;
    let (_, filter, subfilter) = signature_metadata(&bytes)?;
    let report = PlanReport {
        file: file.display().to_string(),
        byte_range: array,
        signed_bytes: signed.len(),
        placeholder_bytes: placeholder.len(),
        filter,
        subfilter,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("file: {}", report.file);
    println!(
        "byte-range: [{}, {}, {}, {}]  signed-bytes: {}",
        report.byte_range[0],
        report.byte_range[1],
        report.byte_range[2],
        report.byte_range[3],
        report.signed_bytes
    );
    println!(
        "placeholder: {} hex chars  filter: {}  subfilter: {}",
        report.placeholder_bytes, report.filter, report.subfilter
    );
    Ok(())
}

/// Parse hex signature bytes.
fn parse_hex(text: &str) -> anyhow::Result<Vec<u8>> {
    let cleaned: String = text
        .chars()
        .filter(|ch| !ch.is_ascii_whitespace())
        .collect();
    if cleaned.is_empty() || !cleaned.chars().all(|ch| ch.is_ascii_hexdigit()) {
        bail!("--contents must be a non-empty hex string");
    }
    if !cleaned.len().is_multiple_of(2) {
        bail!("--contents must have an even number of hex digits");
    }
    let mut bytes = Vec::with_capacity(cleaned.len() / 2);
    for offset in (0..cleaned.len()).step_by(2) {
        let pair = &cleaned[offset..offset + 2];
        bytes.push(u8::from_str_radix(pair, 16).with_context(|| format!("hex {pair:?}"))?);
    }
    Ok(bytes)
}

/// `pdfxi signature verify`.
pub(crate) fn verify(file: &Path, contents: &str, json: bool) -> anyhow::Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let contents = parse_hex(contents)?;
    if contents.iter().all(|byte| *byte == 0) {
        bail!("--contents is all zero bytes: this is an unsigned placeholder, not a signature");
    }
    let placeholder = find_placeholder(&bytes)?;
    let plan = plan_byte_range(&bytes, placeholder.as_bytes())
        .map_err(|error| anyhow::anyhow!("byte range: {error}"))?;
    validate_byte_range(&bytes, &plan, placeholder.as_bytes())
        .map_err(|error| anyhow::anyhow!("validate byte range: {error}"))?;
    let signed =
        signed_bytes(&bytes, &plan).map_err(|error| anyhow::anyhow!("signed bytes: {error}"))?;
    let ranged = plan.as_array().context("byte range is incomplete")?;
    let (field, filter, subfilter) = signature_metadata(&bytes)?;
    let request = VerificationRequest::new(&filter, &subfilter, &signed, &contents);
    let outcome = StructureOnlyVerifier
        .verify(&request)
        .map_err(|error| anyhow::anyhow!("structure verification: {error}"))?;
    let report = VerifyReport {
        file: file.display().to_string(),
        field,
        filter,
        subfilter,
        valid: outcome.is_valid(),
        authenticity: outcome.authenticity().as_str().to_owned(),
        detail: outcome.detail().to_owned(),
        byte_range: ranged,
        contents_bytes: contents.len(),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!(
        "file: {}  field: {}  filter: {}  subfilter: {}",
        report.file, report.field, report.filter, report.subfilter
    );
    println!(
        "valid: {}  authenticity: {}  contents: {} bytes",
        report.valid, report.authenticity, report.contents_bytes
    );
    println!("detail: {}", report.detail);
    println!(
        "byte-range: [{}, {}, {}, {}]",
        report.byte_range[0], report.byte_range[1], report.byte_range[2], report.byte_range[3]
    );
    Ok(())
}
