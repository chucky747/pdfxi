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
    Calculation, CalculationOp, CalculationPlan, CalculationStep, Field, FieldProperties,
    FieldSpec, FieldType, FieldValue, Form, FormSource, PdfDocument, PdfValue, PropertySet,
    WidgetRect, load_acroform, parse_pdf, serialize_form_pdf,
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
    match value {
        FieldValue::Empty => String::new(),
        FieldValue::Text(text) => text.clone(),
        FieldValue::Bool(flag) => flag.to_string(),
        FieldValue::Choice(choices) => choices.join("|"),
        FieldValue::Number(number) => format!("{number}"),
    }
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

/// Exit code used when a form has validation errors.
pub(crate) const EXIT_INVALID_FORM: i32 = 2;

/// One validation error row.
#[derive(Serialize)]
struct ValidationRow {
    /// Field the error belongs to.
    field: String,
    /// Human-readable message.
    message: String,
}

/// JSON shape of `pdfxi form validate`.
#[derive(Serialize)]
struct ValidateReport {
    /// Inspected file.
    file: String,
    /// Number of terminal fields.
    fields: usize,
    /// Whether every definition and value is valid.
    valid: bool,
    /// Number of errors.
    error_count: usize,
    /// Errors in stable order.
    errors: Vec<ValidationRow>,
}

/// One calculation result row.
#[derive(Serialize)]
struct CalculationRow {
    /// Target field.
    target: String,
    /// Evaluated value.
    value: String,
}

/// JSON shape of `pdfxi form calculate`.
#[derive(Serialize)]
struct CalculateReport {
    /// Inspected file.
    file: String,
    /// Steps evaluated.
    calculations: usize,
    /// Number of decimal places.
    decimals: u8,
    /// Results in evaluation order.
    results: Vec<CalculationRow>,
}

/// Load the terminal form model of a PDF written in the classic layout.
fn load_form(file: &Path) -> anyhow::Result<Form> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let document =
        parse_pdf(&bytes).map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))?;
    let source = ParsedSource(&document);
    let load = load_acroform(&source).map_err(|error| anyhow::anyhow!("load form: {error}"))?;
    load.into_form()
        .with_context(|| format!("{} has no /AcroForm", file.display()))
}

/// Parse `NAME=VALUE`.
fn parse_assignment(text: &str) -> anyhow::Result<(String, String)> {
    let (name, value) = text
        .split_once('=')
        .with_context(|| format!("assignment {text:?} needs the form NAME=VALUE"))?;
    let name = name.trim();
    if name.is_empty() {
        bail!("assignment {text:?} has an empty field name");
    }
    Ok((name.to_owned(), value.trim().to_owned()))
}

/// Build a typed value matching the field's widget type.
fn typed_value(field: &Field, text: &str) -> anyhow::Result<FieldValue> {
    match field.field_type() {
        FieldType::CheckBox => match text {
            "true" => Ok(FieldValue::Bool(true)),
            "false" => Ok(FieldValue::Bool(false)),
            other => bail!(
                "checkbox {:?} needs true or false, got {other:?}",
                field.name()
            ),
        },
        FieldType::Choice | FieldType::Radio => {
            let choices: Vec<String> = text
                .split('|')
                .map(str::trim)
                .filter(|choice| !choice.is_empty())
                .map(str::to_owned)
                .collect();
            if choices.is_empty() {
                bail!(
                    "{:?} needs at least one option name separated by |",
                    field.name()
                );
            }
            Ok(FieldValue::Choice(choices))
        }
        _ => {
            if let Ok(number) = text.parse::<f64>() {
                if !number.is_finite() {
                    bail!("value {text:?} is not finite");
                }
                Ok(FieldValue::Number(number))
            } else {
                Ok(FieldValue::Text(text.to_owned()))
            }
        }
    }
}

/// Apply `NAME=VALUE` assignments to the model.
fn apply_assignments(form: &mut Form, assignments: &[String]) -> anyhow::Result<()> {
    for assignment in assignments {
        let (name, text) = parse_assignment(assignment)?;
        let field = form
            .field(&name)
            .with_context(|| format!("no field named {name:?}"))?;
        let value = typed_value(field, &text)?;
        form.set_value(&name, value)
            .map_err(|error| anyhow::anyhow!("set {name:?}: {error}"))?;
    }
    Ok(())
}

/// Build a property set from the loaded fields.
fn properties_of(form: &Form) -> anyhow::Result<PropertySet> {
    let mut set = PropertySet::new();
    for field in form.fields() {
        let mut properties = FieldProperties::new(field.name(), field.field_type())
            .with_required(field.flags().is_required())
            .with_read_only(field.flags().is_read_only());
        if let Some(max_len) = field.constraint().max_len() {
            properties = properties.with_max_len(max_len);
        }
        if matches!(field.field_type(), FieldType::Choice | FieldType::Radio) {
            properties = properties.with_choices(field.choices().iter().cloned());
        }
        set.add(properties)
            .map_err(|error| anyhow::anyhow!("build properties for {:?}: {error}", field.name()))?;
    }
    Ok(set)
}

/// `pdfxi form validate`.
pub(crate) fn run_validate(file: &Path, assignments: &[String], json: bool) -> anyhow::Result<()> {
    let mut form = load_form(file)?;
    apply_assignments(&mut form, assignments)?;
    let properties = properties_of(&form)?;
    let summary = properties.validate(&form);
    let report = ValidateReport {
        file: file.display().to_string(),
        fields: form.len(),
        valid: summary.is_valid(),
        error_count: summary.error_count(),
        errors: summary
            .errors()
            .iter()
            .map(|error| ValidationRow {
                field: error.field().to_owned(),
                message: error.to_string(),
            })
            .collect(),
    };
    let exit = if report.valid { 0 } else { EXIT_INVALID_FORM };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("file: {}", report.file);
        println!("fields: {}", report.fields);
        println!("valid: {}", report.valid);
        for row in &report.errors {
            println!("error: {}  {}", row.field, row.message);
        }
        println!("errors: {}", report.error_count);
    }
    if exit != 0 {
        std::process::exit(exit);
    }
    Ok(())
}

/// One requested calculation: an operator and a `TARGET=SOURCE,...` spec.
pub(crate) struct CalculationRequest<'a> {
    /// Target and source spec.
    pub spec: &'a str,
    /// Arithmetic operation.
    pub op: CalculationOp,
}

/// Parse `TARGET=SRC1,SRC2` into a calculation.
fn parse_calculation(
    request: &CalculationRequest<'_>,
    decimals: u8,
) -> anyhow::Result<Calculation> {
    let (target, sources) = request.spec.split_once('=').with_context(|| {
        format!(
            "calculation {:?} needs the form TARGET=SRC1,SRC2",
            request.spec
        )
    })?;
    let target = target.trim();
    if target.is_empty() {
        bail!("calculation {:?} has an empty target", request.spec);
    }
    let sources: Vec<String> = sources
        .split(',')
        .map(str::trim)
        .filter(|source| !source.is_empty())
        .map(str::to_owned)
        .collect();
    if sources.is_empty() {
        bail!(
            "calculation {:?} needs at least one source field",
            request.spec
        );
    }
    Ok(Calculation::new(target, request.op, sources).with_decimals(decimals))
}

/// `pdfxi form calculate`.
pub(crate) fn run_calculate(
    file: &Path,
    assignments: &[String],
    requests: &[CalculationRequest<'_>],
    decimals: u8,
    json: bool,
) -> anyhow::Result<()> {
    if requests.is_empty() {
        bail!("give at least one of --sum, --product, --average, --min or --max");
    }
    if decimals > 12 {
        bail!("--decimals must be at most 12");
    }
    let mut form = load_form(file)?;
    apply_assignments(&mut form, assignments)?;
    let steps: Vec<CalculationStep> = requests
        .iter()
        .map(|request| parse_calculation(request, decimals))
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
        .map(CalculationStep::Standard)
        .collect();
    let plan = CalculationPlan::new(steps);
    let report = plan
        .apply(&mut form)
        .map_err(|error| anyhow::anyhow!("evaluate calculations: {error}"))?;
    let payload = CalculateReport {
        file: file.display().to_string(),
        calculations: report.len(),
        decimals,
        results: report
            .results()
            .iter()
            .map(|result| CalculationRow {
                target: result.target().to_owned(),
                value: render_value(result.value()),
            })
            .collect(),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }
    println!("file: {}", payload.file);
    println!("calculations: {}", payload.calculations);
    println!("decimals: {}", payload.decimals);
    for row in &payload.results {
        println!("{} = {}", row.target, row.value);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        CalculationRequest, parse_assignment, parse_calculation, parse_field, parse_rect,
        typed_value,
    };

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

    #[test]
    fn assignments_parse_names_and_values() {
        assert_eq!(
            parse_assignment(" total = 12 ").expect("assignment"),
            ("total".to_owned(), "12".to_owned())
        );
        assert!(parse_assignment("total").is_err());
        assert!(parse_assignment("=12").is_err());
    }

    #[test]
    fn typed_values_follow_the_widget_type() {
        use pdfxi_forms::{Field, FieldType, FieldValue};

        let checkbox = Field::new("agree", FieldType::CheckBox);
        assert_eq!(
            typed_value(&checkbox, "true").expect("true"),
            FieldValue::Bool(true)
        );
        assert!(typed_value(&checkbox, "yes").is_err());

        let radio = Field::new("color", FieldType::Radio);
        assert_eq!(
            typed_value(&radio, "red|green").expect("choice"),
            FieldValue::Choice(vec!["red".to_owned(), "green".to_owned()])
        );

        let text = Field::new("amount", FieldType::Text);
        assert_eq!(
            typed_value(&text, "12.5").expect("number"),
            FieldValue::Number(12.5)
        );
        assert_eq!(
            typed_value(&text, "hello").expect("text"),
            FieldValue::Text("hello".to_owned())
        );
    }

    #[test]
    fn calculation_specs_parse_targets_and_sources() {
        use pdfxi_forms::CalculationOp;

        let request = CalculationRequest {
            spec: " total = a, b , c ",
            op: CalculationOp::Sum,
        };
        let calculation = parse_calculation(&request, 2).expect("calculation");
        assert_eq!(calculation.target(), "total");
        assert_eq!(calculation.sources(), &["a", "b", "c"]);
        assert_eq!(calculation.decimals(), 2);
        assert!(
            parse_calculation(
                &CalculationRequest {
                    spec: "total=",
                    op: CalculationOp::Sum
                },
                6
            )
            .is_err()
        );
        assert!(
            parse_calculation(
                &CalculationRequest {
                    spec: "a,b",
                    op: CalculationOp::Sum
                },
                6
            )
            .is_err()
        );
    }
}
