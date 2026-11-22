//! `pdfxi form create` and `pdfxi form list`: build and inspect AcroForm fields.
//!
//! The typed field model and the standalone serializer live in `pdfxi-forms`.
//! `create` turns a compact field description into real `/AcroForm` field and
//! widget dictionaries, serializes them with `serialize_form_pdf`, and then
//! reloads the bytes through both `pdfxi-forms::load_acroform` and `pdfxi-core`
//! before writing them. `list` reports the terminal fields of any form PDF the
//! same way.
//!
//! Field syntax: `KIND:NAME:X,Y,W,H[:OPT1|OPT2]` where `KIND` is `text`,
//! `checkbox`, `radio`, `combo`, `list`, `button` or `signature`. Options are
//! required for `radio`, `combo` and `list` and refused for the other kinds.
//! The serializer emits a single US-Letter page (612x792 points).

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_forms::{
    FieldSpec, FormSource, PdfDocument, PdfValue, WidgetRect, load_acroform, parse_pdf,
    serialize_form_pdf,
};
use serde::Serialize;

/// Page width emitted by `serialize_form_pdf`.
const PAGE_WIDTH: f32 = 612.0;
/// Page height emitted by `serialize_form_pdf`.
const PAGE_HEIGHT: f32 = 792.0;
/// Deterministic page object number reserved by the serializer.
const PAGE_OBJECT: u32 = 5;

/// One parsed field request.
struct ParsedField {
    /// Field name.
    name: String,
    /// Canonical kind label.
    kind: &'static str,
    /// Widget rectangle.
    rect: WidgetRect,
    /// Choice or radio options.
    options: Vec<String>,
    /// The model spec handed to `pdfxi-forms`.
    spec: FieldSpec,
}

/// One field row in the creation report.
#[derive(Serialize)]
struct FieldRow {
    /// Field name.
    name: String,
    /// Canonical kind.
    kind: String,
    /// `[x, y, width, height]` in points.
    rect: [f32; 4],
    /// Options for radio, combo and list fields.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    options: Vec<String>,
}

/// JSON shape of `pdfxi form create`.
#[derive(Serialize)]
struct CreateReport {
    /// Written file.
    out: String,
    /// File size in bytes.
    bytes: usize,
    /// Page width in points.
    page_width: f32,
    /// Page height in points.
    page_height: f32,
    /// Fields requested, in creation order.
    fields: Vec<FieldRow>,
    /// Field names read back from the written bytes.
    loaded_fields: Vec<LoadedRow>,
}

/// One loaded field row.
#[derive(Serialize)]
struct LoadedRow {
    /// Fully qualified name.
    name: String,
    /// Field type label.
    kind: String,
    /// Current value rendered as text.
    value: String,
}

/// JSON shape of `pdfxi form list`.
#[derive(Serialize)]
struct ListReport {
    /// Inspected file.
    file: String,
    /// Whether the catalog has an `/AcroForm`.
    present: bool,
    /// Terminal fields in stable order.
    fields: Vec<LoadedRow>,
}

/// Adapter from the dependency-free serializer document to `FormSource`.
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

/// Parse `x,y,w,h`, rejecting non-finite or off-page rectangles.
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
        bail!("a field rectangle needs four comma-separated numbers (x,y,w,h)");
    }
    let rect = WidgetRect::new(values[0], values[1], values[2], values[3]);
    let finite = [rect.x, rect.y, rect.width, rect.height]
        .iter()
        .all(|value| value.is_finite());
    if !finite {
        bail!("a field rectangle must be finite");
    }
    if rect.width <= 0.0 || rect.height <= 0.0 {
        bail!("a field rectangle needs positive width and height");
    }
    if rect.x < 0.0
        || rect.y < 0.0
        || rect.x + rect.width > PAGE_WIDTH
        || rect.y + rect.height > PAGE_HEIGHT
    {
        bail!(
            "field rectangle {} {} {} {} is outside the {PAGE_WIDTH}x{PAGE_HEIGHT} page",
            rect.x,
            rect.y,
            rect.width,
            rect.height
        );
    }
    Ok(rect)
}

/// Parse one `KIND:NAME:X,Y,W,H[:OPTIONS]` description.
fn parse_field(text: &str) -> anyhow::Result<ParsedField> {
    let mut parts = text.splitn(4, ':');
    let kind = parts
        .next()
        .map(str::trim)
        .filter(|kind| !kind.is_empty())
        .context("a field description starts with a kind, for example text:name:72,700,200,24")?;
    let name = parts
        .next()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .context("a field description needs a name after the kind")?
        .to_owned();
    let rect_text = parts
        .next()
        .map(str::trim)
        .filter(|rect| !rect.is_empty())
        .context("a field description needs a rectangle (x,y,w,h)")?;
    let rest = parts.next().unwrap_or("").trim();
    let options: Vec<String> = rest
        .split('|')
        .map(str::trim)
        .filter(|option| !option.is_empty())
        .map(str::to_owned)
        .collect();
    let rect = parse_rect(rect_text)?;

    let (kind_label, spec) = match kind.to_ascii_lowercase().as_str() {
        "text" => ("text", FieldSpec::text(&name, rect, 1)),
        "checkbox" => ("checkbox", FieldSpec::checkbox(&name, rect, 1)),
        "radio" => {
            require_options(&options, "radio")?;
            (
                "radio",
                FieldSpec::radio_group(&name, rect, 1, options.clone()),
            )
        }
        "combo" => {
            require_options(&options, "combo")?;
            (
                "combo",
                FieldSpec::choice(&name, rect, 1, options.clone(), true),
            )
        }
        "list" => {
            require_options(&options, "list")?;
            (
                "list",
                FieldSpec::choice(&name, rect, 1, options.clone(), false),
            )
        }
        "button" => ("button", FieldSpec::push_button(&name, rect, 1)),
        "signature" => ("signature", FieldSpec::signature(&name, rect, 1)),
        other => bail!(
            "unknown field kind {other:?}; expected text, checkbox, radio, combo, list, button or signature"
        ),
    };
    if !matches!(kind_label, "radio" | "combo" | "list") && !options.is_empty() {
        bail!("a {kind_label} field does not take options");
    }

    Ok(ParsedField {
        name,
        kind: kind_label,
        rect,
        options,
        spec,
    })
}

/// Require a non-empty option list for a kind.
fn require_options(options: &[String], kind: &str) -> anyhow::Result<()> {
    if options.is_empty() {
        bail!("a {kind} field needs at least one option after the rectangle");
    }
    Ok(())
}

/// Read the terminal fields of a serialized document.
fn loaded_fields(bytes: &[u8]) -> anyhow::Result<Option<Vec<LoadedRow>>> {
    let document =
        parse_pdf(bytes).map_err(|error| anyhow::anyhow!("reparse form PDF: {error}"))?;
    let source = ParsedSource(&document);
    let load =
        load_acroform(&source).map_err(|error| anyhow::anyhow!("load written form: {error}"))?;
    let Some(form) = load.form() else {
        return Ok(None);
    };
    Ok(Some(
        form.fields()
            .map(|field| LoadedRow {
                name: field.name().to_owned(),
                kind: field.field_type().to_string(),
                value: render_value(field.value()),
            })
            .collect(),
    ))
}

/// Render a field value for the text report.
fn render_value(value: &pdfxi_forms::FieldValue) -> String {
    format!("{value:?}")
}

/// `pdfxi form create`.
pub(crate) fn run_create(out: &Path, descriptions: &[String], json: bool) -> anyhow::Result<()> {
    if descriptions.is_empty() {
        bail!("pass at least one --field KIND:NAME:X,Y,W,H[:OPTIONS]");
    }
    let mut parsed = Vec::with_capacity(descriptions.len());
    for description in descriptions {
        parsed.push(parse_field(description)?);
    }
    for (index, field) in parsed.iter().enumerate() {
        if parsed[..index].iter().any(|other| other.name == field.name) {
            bail!("duplicate field name {:?}", field.name);
        }
    }

    let fields: Vec<PdfValue> = parsed
        .iter()
        .map(|field| {
            field
                .spec
                .field_dictionary()
                .map_err(|error| anyhow::anyhow!("build field {:?}: {error}", field.name))
        })
        .collect::<anyhow::Result<_>>()?;
    let widgets: Vec<PdfValue> = parsed
        .iter()
        .map(|field| {
            field
                .spec
                .widget_annotation()
                .map_err(|error| anyhow::anyhow!("build widget {:?}: {error}", field.name))
        })
        .collect::<anyhow::Result<_>>()?;
    let page = pdfxi_forms::ObjectRef::new(PAGE_OBJECT, 0);
    let bytes = serialize_form_pdf(&fields, &widgets, Some(page))
        .map_err(|error| anyhow::anyhow!("serialize form: {error}"))?;

    // Fail closed: the file must load through the forms model and the general
    // core reader before it is written.
    let loaded = loaded_fields(&bytes)?.context("the serialized document has no /AcroForm")?;
    if loaded.len() != parsed.len() {
        bail!(
            "wrote {} fields but reloaded {}",
            parsed.len(),
            loaded.len()
        );
    }
    for (expected, actual) in parsed.iter().zip(&loaded) {
        if expected.name != actual.name {
            bail!(
                "field order changed: wrote {:?}, reloaded {:?}",
                expected.name,
                actual.name
            );
        }
    }
    let core = pdfxi_core::Document::from_bytes(bytes.clone())
        .map_err(|error| anyhow::anyhow!("general reader rejected the form PDF: {error}"))?;
    let pages = core
        .pages()
        .map_err(|error| anyhow::anyhow!("read form page tree: {error}"))?;
    if pages.len() != 1 {
        bail!("expected one page in the form PDF, found {}", pages.len());
    }

    std::fs::write(out, &bytes).with_context(|| format!("write {}", out.display()))?;

    let report = CreateReport {
        out: out.display().to_string(),
        bytes: bytes.len(),
        page_width: PAGE_WIDTH,
        page_height: PAGE_HEIGHT,
        fields: parsed
            .iter()
            .map(|field| FieldRow {
                name: field.name.clone(),
                kind: field.kind.to_owned(),
                rect: [
                    field.rect.x,
                    field.rect.y,
                    field.rect.width,
                    field.rect.height,
                ],
                options: field.options.clone(),
            })
            .collect(),
        loaded_fields: loaded,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("out:  {}", report.out);
    println!("bytes: {}", report.bytes);
    println!(
        "page: {} x {} points",
        report.page_width, report.page_height
    );
    for row in &report.fields {
        let rect = format!(
            "{} {} {} {}",
            row.rect[0], row.rect[1], row.rect[2], row.rect[3]
        );
        if row.options.is_empty() {
            println!("field: {} [{}] rect: {rect}", row.name, row.kind);
        } else {
            println!(
                "field: {} [{}] rect: {rect} options: {}",
                row.name,
                row.kind,
                row.options.join("|")
            );
        }
    }
    println!("fields: {}", report.fields.len());
    Ok(())
}

/// `pdfxi form list`.
pub(crate) fn run_list(file: &Path, json: bool) -> anyhow::Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let document =
        parse_pdf(&bytes).map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))?;
    let source = ParsedSource(&document);
    let load = load_acroform(&source).map_err(|error| anyhow::anyhow!("load form: {error}"))?;
    let present = !load.is_no_form();
    let fields = load
        .into_form()
        .map(|form| {
            form.fields()
                .map(|field| LoadedRow {
                    name: field.name().to_owned(),
                    kind: field.field_type().to_string(),
                    value: render_value(field.value()),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let report = ListReport {
        file: file.display().to_string(),
        present,
        fields,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("file: {}", report.file);
    println!("present: {}", report.present);
    for row in &report.fields {
        println!("{}  [{}]  {}", row.name, row.kind, row.value);
    }
    println!("fields: {}", report.fields.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{parse_field, parse_rect};

    #[test]
    fn field_descriptions_parse_all_supported_kinds() {
        for text in [
            "text:name:72,700,200,24",
            "checkbox:agree:72,660,14,14",
            "radio:color:72,620,14,14:red|green",
            "combo:state:72,580,200,24:a|b",
            "list:state:72,540,200,60:a|b",
            "button:submit:72,500,100,24",
            "signature:sig:72,440,200,40",
        ] {
            let field = parse_field(text).unwrap_or_else(|error| panic!("{text}: {error}"));
            assert!(!field.name.is_empty());
        }
    }

    #[test]
    fn field_descriptions_reject_bad_input() {
        assert!(
            parse_field("text:name:72,700,200").is_err(),
            "three rect values"
        );
        assert!(parse_field("text::72,700,200,24").is_err(), "empty name");
        assert!(
            parse_field("text:name:72,700,-10,24").is_err(),
            "negative width"
        );
        assert!(
            parse_field("text:name:72,700,200,24:unexpected").is_err(),
            "text options"
        );
        assert!(
            parse_field("radio:color:72,620,14,14").is_err(),
            "radio without options"
        );
        assert!(
            parse_field("blob:name:72,700,200,24").is_err(),
            "unknown kind"
        );
        assert!(
            parse_field("text:name:600,700,200,24").is_err(),
            "off-page rectangle"
        );
    }

    #[test]
    fn rectangles_validate_geometry() {
        let rect = parse_rect("10,20,30,40").expect("rect");
        assert_eq!(
            (rect.x, rect.y, rect.width, rect.height),
            (10.0, 20.0, 30.0, 40.0)
        );
        assert!(parse_rect("10,20,0,40").is_err());
        assert!(parse_rect("NaN,20,30,40").is_err());
        assert!(parse_rect("10,20,30").is_err());
    }
}
