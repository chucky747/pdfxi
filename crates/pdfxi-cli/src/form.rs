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
use pdfxi_core::{Document, Object, ObjectId};
use pdfxi_forms::{
    AcroFormModel, ActionCollector, ActionReport, BlankBundle, BundleMeta, Calculation,
    CalculationOp, CalculationPlan, CalculationStep, DocumentRights, Field, FieldProperties,
    FieldSpec, FieldType, FieldValue, Form, FormHint, FormSource, HintKind, NavPolicy, ObjectRef,
    PdfDocument, PdfValue, PropertySet, ReaderEnablePlan, ResponseFile, ResponseFormat, TabOrder,
    TabPolicy, TabWidget, UsageRight, UsageRightsSignature, WidgetRect, load_acroform,
    merge_responses, parse_bundle, parse_pdf, parse_rights_dictionary, read_acroform, recognize,
    serialize_form_pdf,
};
use serde::{Deserialize, Serialize};

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

/// Convert a core object into the forms model's value type.
fn to_forms_value(object: &Object, depth: u8) -> PdfValue {
    if depth > 64 {
        return PdfValue::Null;
    }
    match object {
        Object::Null => PdfValue::Null,
        Object::Boolean(flag) => PdfValue::Bool(*flag),
        Object::Integer(value) => PdfValue::Number(*value as f64),
        Object::Real(value) => PdfValue::Number(*value),
        Object::Name(name) => PdfValue::Name(name.clone()),
        Object::String(bytes) => PdfValue::Text(pdfxi_core::decode_text_string(bytes)),
        Object::Array(items) => PdfValue::Array(
            items
                .iter()
                .map(|item| to_forms_value(item, depth + 1))
                .collect(),
        ),
        Object::Dictionary(entries) => PdfValue::Dictionary(
            entries
                .iter()
                .map(|(key, value)| (key.clone(), to_forms_value(value, depth + 1)))
                .collect(),
        ),
        Object::Reference(id) => PdfValue::Reference(ObjectRef::new(id.number, id.generation)),
        Object::Stream { dictionary, .. } => PdfValue::Dictionary(
            dictionary
                .iter()
                .map(|(key, value)| (key.clone(), to_forms_value(value, depth + 1)))
                .collect(),
        ),
    }
}

/// Adapter from a core document to the forms `FormSource` boundary.
struct CoreSource<'a> {
    document: &'a Document,
}

impl FormSource for CoreSource<'_> {
    fn catalog(&self) -> PdfValue {
        self.document
            .root()
            .ok()
            .and_then(|id| self.document.object(id).ok())
            .map(|object| to_forms_value(&object, 0))
            .unwrap_or(PdfValue::Null)
    }

    fn resolve(&self, value: &PdfValue) -> Option<PdfValue> {
        match value {
            PdfValue::Reference(id) => self
                .document
                .object(ObjectId::new(id.number, id.generation))
                .ok()
                .map(|object| to_forms_value(&object, 0)),
            other => Some(other.clone()),
        }
    }
}

/// Parse a `--policy` value (`rows`, `columns`, `structure`, `annotations`).
fn parse_tab_policy(text: &str) -> anyhow::Result<TabPolicy> {
    match text.to_ascii_lowercase().as_str() {
        "r" | "rows" | "row" => Ok(TabPolicy::Rows),
        "c" | "columns" | "column" => Ok(TabPolicy::Columns),
        "s" | "structure" => Ok(TabPolicy::Structure),
        "a" | "annotations" | "annotation" => Ok(TabPolicy::Annotations),
        other => {
            bail!("unknown tab policy {other:?}; expected rows, columns, structure or annotations")
        }
    }
}

/// Read the AcroForm `/Tabs` policy from the catalog.
fn read_tabs_policy(document: &Document) -> TabPolicy {
    let policy = document.root().ok().and_then(|root| {
        let catalog = document.object(root).ok()?;
        let catalog = catalog.as_dictionary()?;
        let acroform = match catalog.get("AcroForm")? {
            Object::Reference(id) => document.object(*id).ok()?,
            other => other.clone(),
        };
        let acroform = acroform.as_dictionary()?;
        acroform
            .get("Tabs")
            .and_then(Object::as_name)
            .map(str::to_owned)
    });
    policy
        .as_deref()
        .map_or_else(TabPolicy::default, TabPolicy::from_name)
}

/// Build a tab order from the loaded field widgets.
fn build_tab_order(model: &AcroFormModel, policy: TabPolicy) -> TabOrder {
    let mut widgets = Vec::new();
    let mut annotation_index = 0;
    for field in &model.fields {
        for widget in &field.widgets {
            let mut tab = TabWidget::new(
                field.name.clone(),
                widget.page.unwrap_or(0),
                annotation_index,
            )
            .with_rect(
                widget.rect.left,
                widget.rect.bottom,
                widget.rect.right,
                widget.rect.top,
            );
            tab.set_read_only(field.read_only);
            widgets.push(tab);
            annotation_index += 1;
        }
    }
    TabOrder::new(policy, widgets)
}

/// One widget row in the tab-order report.
#[derive(Serialize)]
struct TabRow {
    /// Fully qualified field name.
    name: String,
    /// One-based page number.
    page: usize,
    /// `[x0, y0, x1, y1]` in points.
    rect: [f64; 4],
    /// Whether the field is read-only.
    read_only: bool,
}

/// One navigation transition.
#[derive(Serialize)]
struct NavRow {
    /// Starting field.
    from: String,
    /// Next field, or `null` at the end when wrapping is disabled.
    next: Option<String>,
    /// Previous field, or `null` at the start when wrapping is disabled.
    previous: Option<String>,
}

/// JSON shape of `pdfxi form tabs`.
#[derive(Serialize)]
struct TabsReport {
    /// Inspected file.
    file: String,
    /// `/Tabs` name (`R`, `C`, `S` or `A`).
    tabs_name: String,
    /// Resolved policy label.
    policy: String,
    /// Number of widgets.
    widgets: usize,
    /// Widgets in tab order.
    order: Vec<TabRow>,
    /// Next/previous transitions under the navigation policy.
    navigation: Vec<NavRow>,
}

/// `pdfxi form tabs`.
pub(crate) fn run_tabs(
    file: &Path,
    policy_override: Option<&str>,
    nav: NavPolicy,
    json: bool,
) -> anyhow::Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))?;
    let model =
        read_acroform(&document).map_err(|error| anyhow::anyhow!("read AcroForm: {error}"))?;
    let policy = match policy_override {
        Some(text) => parse_tab_policy(text)?,
        None => read_tabs_policy(&document),
    };
    let order = build_tab_order(&model, policy);
    let navigation = order
        .widgets()
        .iter()
        .map(|widget| NavRow {
            from: widget.name().to_owned(),
            next: order.next_after(widget.name(), &nav).map(str::to_owned),
            previous: order
                .previous_before(widget.name(), &nav)
                .map(str::to_owned),
        })
        .collect();
    let report = TabsReport {
        file: file.display().to_string(),
        tabs_name: policy.name().to_owned(),
        policy: policy.label().to_owned(),
        widgets: order.len(),
        order: order
            .widgets()
            .iter()
            .map(|widget| TabRow {
                name: widget.name().to_owned(),
                page: widget.page() + 1,
                rect: widget.rect(),
                read_only: widget.is_read_only(),
            })
            .collect(),
        navigation,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("file: {}", report.file);
    println!("tabs: {} ({})", report.tabs_name, report.policy);
    for (index, row) in report.order.iter().enumerate() {
        println!(
            "{}. {}  page {}  rect: {} {} {} {}",
            index + 1,
            row.name,
            row.page,
            row.rect[0],
            row.rect[1],
            row.rect[2],
            row.rect[3]
        );
    }
    for row in &report.navigation {
        println!(
            "{} -> {} / {}",
            row.from,
            row.next.as_deref().unwrap_or("(none)"),
            row.previous.as_deref().unwrap_or("(none)")
        );
    }
    println!("widgets: {}", report.widgets);
    Ok(())
}

/// One accepted action row.
#[derive(Serialize)]
struct ActionStepRow {
    /// Field carrying the action.
    field: String,
    /// Site: `A`, `K`, `F`, `V` or `C`.
    site: String,
    /// Action subtype.
    action: String,
    /// Fields the action names.
    fields: Vec<String>,
}

/// One rejected action row.
#[derive(Serialize)]
struct UnsupportedRow {
    /// Field carrying the action.
    field: String,
    /// Site: `A`, `K`, `F`, `V` or `C`.
    site: String,
    /// PDF subtype that was found.
    action_type: String,
    /// Why it was rejected.
    reason: String,
}

/// JSON shape of `pdfxi form actions`.
#[derive(Serialize)]
struct ActionsReport {
    /// Inspected file.
    file: String,
    /// Whether the catalog has an `/AcroForm`.
    present: bool,
    /// Accepted actions in execution order.
    steps: Vec<ActionStepRow>,
    /// Rejected actions in stable order.
    unsupported: Vec<UnsupportedRow>,
}

/// Collect the actions attached to the loaded fields and widgets.
fn collect_actions(document: &Document, model: &AcroFormModel) -> ActionReport {
    let source = CoreSource { document };
    let mut collector = ActionCollector::new();
    for field in &model.fields {
        let mut ids = Vec::new();
        if let Some(id) = field.field_object {
            ids.push(id);
        }
        for widget in &field.widgets {
            if let Some(id) = widget.object
                && !ids.contains(&id)
            {
                ids.push(id);
            }
        }
        for id in ids {
            let Ok(object) = document.object(id) else {
                continue;
            };
            let Some(dictionary) = object.as_dictionary() else {
                continue;
            };
            let converted = to_forms_value(&Object::Dictionary(dictionary.clone()), 0);
            if let PdfValue::Dictionary(entries) = converted {
                collector.add_field(&field.name, &entries, &source);
            }
        }
    }
    collector.finish()
}

/// `pdfxi form actions`.
pub(crate) fn run_actions(file: &Path, json: bool) -> anyhow::Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))?;
    let model =
        read_acroform(&document).map_err(|error| anyhow::anyhow!("read AcroForm: {error}"))?;
    let report = collect_actions(&document, &model);
    let payload = ActionsReport {
        file: file.display().to_string(),
        present: model.present,
        steps: report
            .plan()
            .steps()
            .iter()
            .map(|step| ActionStepRow {
                field: step.field.clone(),
                site: step.site.label().to_owned(),
                action: step.action.subtype().to_owned(),
                fields: step.action.fields().to_vec(),
            })
            .collect(),
        unsupported: report
            .unsupported()
            .iter()
            .map(|row| UnsupportedRow {
                field: row.field.clone(),
                site: row.site.label().to_owned(),
                action_type: row.action_type.clone(),
                reason: row.reason.clone(),
            })
            .collect(),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }
    println!("file: {}", payload.file);
    println!("present: {}", payload.present);
    for step in &payload.steps {
        if step.fields.is_empty() {
            println!("action: {} [{}] {}", step.field, step.site, step.action);
        } else {
            println!(
                "action: {} [{}] {} fields: {}",
                step.field,
                step.site,
                step.action,
                step.fields.join(",")
            );
        }
    }
    for row in &payload.unsupported {
        println!(
            "unsupported: {} [{}] {}: {}",
            row.field, row.site, row.action_type, row.reason
        );
    }
    println!("actions: {}", payload.steps.len());
    println!("unsupported_actions: {}", payload.unsupported.len());
    Ok(())
}

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

/// One parsed reader-enable right.
fn parse_rights(text: &str) -> anyhow::Result<DocumentRights> {
    let mut rights = DocumentRights::new();
    for token in text.split(',') {
        let key = token.trim().to_ascii_lowercase().replace('_', "-");
        let right = match key.as_str() {
            "" => continue,
            "fill" | "fill-in" | "form-fill" => UsageRight::FillIn,
            "save" => UsageRight::Save,
            "annotate" | "annotation" => UsageRight::Annotate,
            "assemble" => UsageRight::Assemble,
            "copy" | "copy-extract" | "extract" => UsageRight::CopyExtract,
            "accessibility" | "a11y" => UsageRight::Accessibility,
            "all" => {
                for right in UsageRight::ALL {
                    rights = rights.with(right);
                }
                continue;
            }
            other => bail!(
                "unknown right {other:?}; expected fill, save, annotate, assemble, copy-extract, accessibility or all"
            ),
        };
        rights = rights.with(right);
    }
    if rights.bits() == 0 {
        bail!("--rights must name at least one right");
    }
    Ok(rights)
}

/// Convert a forms value into a core object for writing.
fn to_core_object(value: &PdfValue, depth: u8) -> anyhow::Result<Object> {
    if depth > 64 {
        bail!("reader-enable dictionary is nested too deeply");
    }
    Ok(match value {
        PdfValue::Null => Object::Null,
        PdfValue::Bool(flag) => Object::Boolean(*flag),
        PdfValue::Number(number) => {
            if !number.is_finite() {
                bail!("reader-enable dictionary carries a non-finite number");
            }
            if number.fract() == 0.0 && number.abs() <= 9_007_199_254_740_992.0 {
                Object::Integer(*number as i64)
            } else {
                Object::Real(*number)
            }
        }
        PdfValue::Integer(value) => {
            Object::Integer(i64::try_from(*value).context("reader-enable integer out of range")?)
        }
        PdfValue::Text(text) => Object::String(pdfxi_core::encode_text_string(text)),
        PdfValue::Name(name) => Object::Name(name.clone()),
        PdfValue::Array(items) => Object::Array(
            items
                .iter()
                .map(|item| to_core_object(item, depth + 1))
                .collect::<anyhow::Result<Vec<_>>>()?,
        ),
        PdfValue::Dictionary(entries) => Object::Dictionary(
            entries
                .iter()
                .map(|(key, item)| Ok((key.clone(), to_core_object(item, depth + 1)?)))
                .collect::<anyhow::Result<std::collections::BTreeMap<_, _>>>()?,
        ),
        PdfValue::Reference(_) => bail!("reader-enable dictionary must not carry references"),
        PdfValue::Stream { .. } => bail!("reader-enable dictionary must not carry streams"),
    })
}

/// Report written by `pdfxi form reader-enable`.
#[derive(Serialize)]
struct ReaderEnableReport {
    /// Input file.
    file: String,
    /// Written file.
    out: String,
    /// Pages in the document.
    pages: usize,
    /// Granted rights, sorted by label.
    rights: Vec<String>,
    /// Local filter name.
    filter: String,
    /// Placeholder `/Contents` size.
    placeholder_bytes: usize,
    /// Optional reason metadata.
    reason: Option<String>,
    /// Whether the local policy allows filling.
    can_fill: bool,
    /// Whether the local policy allows saving.
    can_save: bool,
    /// Always `false`: this is compatibility metadata, not certification.
    certified: bool,
    /// Bytes written.
    bytes: usize,
}

/// `pdfxi form reader-enable`.
pub(crate) fn run_reader_enable(
    file: &Path,
    out: &Path,
    rights_text: &str,
    reason: Option<&str>,
    placeholder_bytes: usize,
    json: bool,
) -> anyhow::Result<()> {
    let rights = parse_rights(rights_text)?;
    let mut signature = UsageRightsSignature::local().with_placeholder_bytes(placeholder_bytes);
    if let Some(reason) = reason {
        signature = signature.with_reason(reason);
    }
    let plan = ReaderEnablePlan::new(rights).with_signature(signature);
    plan.validate()
        .map_err(|error| anyhow::anyhow!("reader-enable plan: {error}"))?;
    let generated = to_core_object(
        &plan
            .to_dictionary()
            .map_err(|error| anyhow::anyhow!("reader-enable dictionary: {error}"))?,
        0,
    )?;
    let Object::Dictionary(generated) = generated else {
        bail!("reader-enable dictionary is not a dictionary");
    };
    let ur = generated
        .get("Perms")
        .and_then(Object::as_dictionary)
        .and_then(|perms| perms.get("UR"))
        .cloned()
        .context("generated reader-enable dictionary has no /Perms /UR")?;

    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let mut document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))?;
    let pages = document
        .page_count()
        .map_err(|error| anyhow::anyhow!("count pages: {error}"))?;
    let catalog_id = document
        .root()
        .map_err(|error| anyhow::anyhow!("read catalog: {error}"))?;
    let mut catalog = document
        .object(catalog_id)
        .map_err(|error| anyhow::anyhow!("load catalog: {error}"))?
        .as_dictionary()
        .context("catalog is not a dictionary")?
        .clone();
    let mut perms = catalog
        .get("Perms")
        .and_then(Object::as_dictionary)
        .cloned()
        .unwrap_or_default();
    perms.insert("UR".to_owned(), ur);
    catalog.insert("Perms".to_owned(), Object::Dictionary(perms));
    document
        .set_object(catalog_id, Object::Dictionary(catalog))
        .map_err(|error| anyhow::anyhow!("stage catalog: {error}"))?;
    let saved = document
        .save()
        .with_context(|| format!("save {}", out.display()))?;
    std::fs::write(out, &saved).with_context(|| format!("write {}", out.display()))?;

    let reopened = Document::from_bytes(saved.clone())
        .map_err(|error| anyhow::anyhow!("reopen {}: {error}", out.display()))?;
    let catalog = reopened
        .object(
            reopened
                .root()
                .map_err(|error| anyhow::anyhow!("read saved catalog: {error}"))?,
        )
        .map_err(|error| anyhow::anyhow!("load saved catalog: {error}"))?;
    let parsed = parse_rights_dictionary(&to_forms_value(&catalog, 0)).map_err(|error| {
        anyhow::anyhow!("saved /Perms /UR is not a valid reader-enable dictionary: {error}")
    })?;
    let expected = plan.report();
    let actual = parsed.report();
    if actual.rights() != expected.rights() {
        bail!(
            "saved rights {:?} do not match the requested {:?}",
            actual.rights(),
            expected.rights()
        );
    }
    if actual.filter() != expected.filter() {
        bail!(
            "saved filter {:?} does not match {:?}",
            actual.filter(),
            expected.filter()
        );
    }
    let can_fill = parsed
        .can_fill()
        .map_err(|error| anyhow::anyhow!("fill decision: {error}"))?
        .is_allowed();
    let can_save = parsed
        .can_save()
        .map_err(|error| anyhow::anyhow!("save decision: {error}"))?
        .is_allowed();

    let report = ReaderEnableReport {
        file: file.display().to_string(),
        out: out.display().to_string(),
        pages,
        rights: actual.rights().to_vec(),
        filter: actual.filter().to_owned(),
        placeholder_bytes: actual.placeholder_bytes(),
        reason: reason.map(str::to_owned),
        can_fill,
        can_save,
        certified: false,
        bytes: saved.len(),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("file: {}", report.file);
    println!("out:  {}", report.out);
    println!("pages: {}", report.pages);
    println!("rights: {}", report.rights.join(","));
    println!("filter: {}", report.filter);
    println!("placeholder_bytes: {}", report.placeholder_bytes);
    if let Some(reason) = &report.reason {
        println!("reason: {reason}");
    }
    println!("can_fill: {}", report.can_fill);
    println!("can_save: {}", report.can_save);
    println!("certified: false");
    println!("bytes: {}", report.bytes);
    Ok(())
}

/// One distributed bundle field.
#[derive(Serialize)]
struct BundleRow {
    /// Fully qualified field name.
    name: String,
    /// Stable type label.
    kind: String,
    /// Whether a response must supply it.
    required: bool,
}

/// JSON shape of `pdfxi form distribute`.
#[derive(Serialize)]
struct DistributeReport {
    /// Source PDF.
    file: String,
    /// Written bundle file.
    out: String,
    /// Stable form id.
    form_id: String,
    /// Human title.
    title: String,
    /// Bundle version.
    version: u32,
    /// Advertised fields.
    fields: Vec<BundleRow>,
    /// Field count.
    field_count: usize,
    /// Bytes written.
    bytes: usize,
}

/// `pdfxi form distribute`.
pub(crate) fn run_distribute(
    file: &Path,
    out: &Path,
    form_id: Option<&str>,
    title: Option<&str>,
    version: u32,
    json: bool,
) -> anyhow::Result<()> {
    let form = load_form(file)?;
    let stem = file
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::trim)
        .filter(|stem| !stem.is_empty())
        .unwrap_or("PDFXI form")
        .chars()
        .take(256)
        .collect::<String>();
    let form_id = form_id.unwrap_or("pdfxi-form").trim();
    if form_id.is_empty() {
        bail!("--form-id must not be empty");
    }
    let title = title.unwrap_or(&stem).trim();
    if title.is_empty() {
        bail!("--title must not be empty");
    }
    let meta = BundleMeta::new(form_id, title, version)
        .map_err(|error| anyhow::anyhow!("bundle metadata: {error}"))?;
    let bundle = BlankBundle::from_form(meta, &form)
        .map_err(|error| anyhow::anyhow!("build blank bundle: {error}"))?;
    let text = bundle.to_text();
    std::fs::write(out, text.as_bytes())
        .with_context(|| format!("write bundle {}", out.display()))?;

    let written =
        std::fs::read_to_string(out).with_context(|| format!("read bundle {}", out.display()))?;
    let parsed = parse_bundle(&written)
        .map_err(|error| anyhow::anyhow!("reparse bundle {}: {error}", out.display()))?;
    if parsed != bundle {
        bail!("written bundle {} does not round-trip", out.display());
    }

    let report = DistributeReport {
        file: file.display().to_string(),
        out: out.display().to_string(),
        form_id: bundle.meta().form_id().to_owned(),
        title: bundle.meta().title().to_owned(),
        version: bundle.meta().version(),
        fields: bundle
            .fields()
            .iter()
            .map(|field| BundleRow {
                name: field.name().to_owned(),
                kind: field.field_type().to_string(),
                required: field.required(),
            })
            .collect(),
        field_count: bundle.fields().len(),
        bytes: written.len(),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("file: {}", report.file);
    println!("out:  {}", report.out);
    println!("form_id: {}", report.form_id);
    println!("title: {}", report.title);
    println!("version: {}", report.version);
    for field in &report.fields {
        println!(
            "{}\t{}\t{}",
            field.name,
            field.kind,
            if field.required {
                "required"
            } else {
                "optional"
            }
        );
    }
    println!("fields: {}", report.field_count);
    println!("bytes: {}", report.bytes);
    Ok(())
}

/// One merged field in a collection report.
#[derive(Serialize)]
struct CollectedFieldRow {
    /// Field name.
    name: String,
    /// Winning value.
    value: String,
    /// Respondent ids that supplied a value.
    contributors: Vec<String>,
    /// Whether respondents disagreed.
    conflict: bool,
}

/// One contribution to a conflict.
#[derive(Serialize)]
struct ConflictValueRow {
    /// Respondent id.
    respondent: String,
    /// Supplied value.
    value: String,
}

/// One conflicting field.
#[derive(Serialize)]
struct ConflictRow {
    /// Field name.
    name: String,
    /// Every differing contribution.
    values: Vec<ConflictValueRow>,
}

/// One rejected response.
#[derive(Serialize)]
struct RejectedRow {
    /// Respondent id.
    respondent: String,
    /// Rejection reason.
    reason: String,
}

/// Collection counters.
#[derive(Serialize)]
struct CollectSummary {
    /// Responses supplied.
    responses_received: usize,
    /// Responses accepted.
    responses_accepted: usize,
    /// Responses rejected.
    responses_rejected: usize,
    /// Duplicate respondent ids.
    duplicate_respondents: usize,
    /// Distinct fields merged.
    fields_merged: usize,
    /// Conflicting fields.
    conflicts: usize,
}

/// JSON shape of `pdfxi form collect`.
#[derive(Serialize)]
struct CollectReport {
    /// Source PDF.
    file: String,
    /// Form fields in the PDF.
    pdf_fields: usize,
    /// Counters.
    summary: CollectSummary,
    /// Merged fields.
    fields: Vec<CollectedFieldRow>,
    /// Conflicts.
    conflicts: Vec<ConflictRow>,
    /// Duplicate respondent ids.
    duplicates: Vec<String>,
    /// Rejected responses.
    rejected: Vec<RejectedRow>,
    /// Required fields with no merged value.
    missing_required: Vec<String>,
}

/// Derive a respondent id from a response path.
fn respondent_of(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("")
        .trim();
    if stem.is_empty() {
        "response".to_owned()
    } else {
        stem.chars().take(256).collect()
    }
}

/// Detect the response wire format from its first bytes.
fn response_format(content: &str) -> ResponseFormat {
    if content.trim_start().starts_with("%FDF") {
        ResponseFormat::Fdf
    } else {
        ResponseFormat::Xfdf
    }
}

/// Split `[RESPONDENT=]PATH`, rejecting an empty explicit respondent.
fn parse_response(text: &str) -> anyhow::Result<(Option<String>, std::path::PathBuf)> {
    if let Some((respondent, path)) = text.split_once('=') {
        let looks_like_path =
            respondent.contains('/') || respondent.contains('\\') || respondent.contains(':');
        if !looks_like_path {
            if respondent.trim().is_empty() {
                bail!("response {text:?} has an empty respondent id");
            }
            return Ok((Some(respondent.trim().to_owned()), path.into()));
        }
    }
    Ok((None, text.into()))
}

/// `pdfxi form collect`.
pub(crate) fn run_collect(file: &Path, responses: &[String], json: bool) -> anyhow::Result<()> {
    if responses.is_empty() {
        bail!("pass at least one --response [RESPONDENT=]PATH");
    }
    let form = load_form(file)?;
    let known: std::collections::BTreeSet<String> =
        form.fields().map(|field| field.name().to_owned()).collect();
    let mut candidates = Vec::with_capacity(responses.len());
    for text in responses {
        let (explicit, path) = parse_response(text)?;
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("read response {}", path.display()))?;
        candidates.push(ResponseFile::new(
            explicit.unwrap_or_else(|| respondent_of(&path)),
            response_format(&content),
            content,
        ));
    }
    let report = merge_responses(&candidates)
        .map_err(|error| anyhow::anyhow!("merge responses: {error}"))?;
    let unknown: Vec<String> = report
        .fields()
        .iter()
        .map(|field| field.name().to_owned())
        .filter(|name| !known.contains(name))
        .collect();
    if !unknown.is_empty() {
        bail!(
            "responses carry field(s) that {} does not define: {}",
            file.display(),
            unknown.join(", ")
        );
    }
    let merged: std::collections::BTreeMap<String, FieldValue> = report
        .fields()
        .iter()
        .map(|field| (field.name().to_owned(), field.value().clone()))
        .collect();
    let missing_required = form
        .fields()
        .filter(|field| field.is_required())
        .filter(|field| {
            merged
                .get(field.name())
                .is_none_or(|value| matches!(value, FieldValue::Empty))
        })
        .map(|field| field.name().to_owned())
        .collect::<Vec<_>>();
    let summary = report.summary();
    let payload = CollectReport {
        file: file.display().to_string(),
        pdf_fields: known.len(),
        summary: CollectSummary {
            responses_received: summary.responses_received,
            responses_accepted: summary.responses_accepted,
            responses_rejected: summary.responses_rejected,
            duplicate_respondents: summary.duplicate_respondents,
            fields_merged: summary.fields_merged,
            conflicts: summary.conflicts,
        },
        fields: report
            .fields()
            .iter()
            .map(|field| CollectedFieldRow {
                name: field.name().to_owned(),
                value: render_value(field.value()),
                contributors: field.contributors().to_vec(),
                conflict: field.has_conflict(),
            })
            .collect(),
        conflicts: report
            .conflicts()
            .iter()
            .map(|conflict| ConflictRow {
                name: conflict.name().to_owned(),
                values: conflict
                    .values()
                    .iter()
                    .map(|contribution| ConflictValueRow {
                        respondent: contribution.respondent().to_owned(),
                        value: render_value(contribution.value()),
                    })
                    .collect(),
            })
            .collect(),
        duplicates: report.duplicates().to_vec(),
        rejected: report
            .rejected()
            .iter()
            .map(|rejected| RejectedRow {
                respondent: rejected.respondent().to_owned(),
                reason: rejected.reason().to_owned(),
            })
            .collect(),
        missing_required,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }
    println!("file: {}", payload.file);
    println!("pdf_fields: {}", payload.pdf_fields);
    println!(
        "responses: {} received, {} accepted, {} rejected",
        payload.summary.responses_received,
        payload.summary.responses_accepted,
        payload.summary.responses_rejected
    );
    println!(
        "fields_merged: {}  conflicts: {}  duplicates: {}",
        payload.summary.fields_merged,
        payload.summary.conflicts,
        payload.summary.duplicate_respondents
    );
    for field in &payload.fields {
        println!(
            "{}\t{}{}",
            field.name,
            field.value,
            if field.conflict { "\t(conflict)" } else { "" }
        );
    }
    for conflict in &payload.conflicts {
        let values: Vec<String> = conflict
            .values
            .iter()
            .map(|value| format!("{}={}", value.respondent, value.value))
            .collect();
        println!("conflict {}: {}", conflict.name, values.join(", "));
    }
    for duplicate in &payload.duplicates {
        println!("duplicate: {duplicate}");
    }
    for rejected in &payload.rejected {
        println!("rejected {}: {}", rejected.respondent, rejected.reason);
    }
    if !payload.missing_required.is_empty() {
        println!("missing_required: {}", payload.missing_required.join(","));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        CalculationRequest, parse_assignment, parse_calculation, parse_field, parse_rect,
        parse_tab_policy, to_forms_value, typed_value,
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

    #[test]
    fn tab_policies_parse_names_and_labels() {
        use pdfxi_forms::TabPolicy;

        assert_eq!(parse_tab_policy("rows").expect("rows"), TabPolicy::Rows);
        assert_eq!(parse_tab_policy("C").expect("columns"), TabPolicy::Columns);
        assert_eq!(
            parse_tab_policy("Structure").expect("structure"),
            TabPolicy::Structure
        );
        assert_eq!(
            parse_tab_policy("annotations").expect("annotations"),
            TabPolicy::Annotations
        );
        assert!(parse_tab_policy("diagonal").is_err());
    }

    #[test]
    fn core_objects_convert_to_form_values() {
        use pdfxi_core::Object;
        use pdfxi_forms::PdfValue;

        assert_eq!(to_forms_value(&Object::Null, 0), PdfValue::Null);
        assert_eq!(
            to_forms_value(&Object::Boolean(true), 0),
            PdfValue::Bool(true)
        );
        assert_eq!(
            to_forms_value(&Object::Name("Tabs".to_owned()), 0),
            PdfValue::Name("Tabs".to_owned())
        );
        let mut dictionary = std::collections::BTreeMap::new();
        dictionary.insert("K".to_owned(), Object::Integer(3));
        assert_eq!(
            to_forms_value(&Object::Dictionary(dictionary), 0),
            PdfValue::Dictionary(
                [("K".to_owned(), PdfValue::Number(3.0))]
                    .into_iter()
                    .collect()
            )
        );
        assert_eq!(
            to_forms_value(&Object::Reference(pdfxi_core::ObjectId::new(7, 0)), 0),
            PdfValue::Reference(pdfxi_forms::ObjectRef::new(7, 0))
        );
    }
}

/// One hint in the recognition manifest.
#[derive(Debug, Deserialize)]
struct HintInput {
    /// `label`, `input`, `checkbox`, `radio` or `choice`.
    kind: String,
    /// Zero-based page index.
    page: u32,
    /// Lower-left x in points.
    x: f32,
    /// Lower-left y in points.
    y: f32,
    /// Width in points.
    width: f32,
    /// Height in points.
    height: f32,
    /// Visible label or prompt.
    #[serde(default)]
    text: Option<String>,
    /// Explicit field name.
    #[serde(default)]
    name: Option<String>,
}

/// The recognition manifest.
#[derive(Debug, Deserialize)]
struct RecognitionFile {
    /// Static control and label hints.
    hints: Vec<HintInput>,
}

/// One recognized field row.
#[derive(Serialize)]
struct RecognizedRow {
    /// Generated field name.
    name: String,
    /// Field type label.
    kind: String,
    /// Zero-based page.
    page: u32,
    /// Widget rectangle.
    rect: [f32; 4],
    /// Paired label, when any.
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    /// Heuristic confidence.
    confidence: f32,
    /// Reading-order rank.
    rank: usize,
}

/// JSON shape of `pdfxi form recognize`.
#[derive(Serialize)]
struct RecognizeReport {
    /// Source manifest.
    manifest: String,
    /// Written form PDF.
    out: String,
    /// Hints supplied.
    hints: usize,
    /// Controls recognized.
    recognized: usize,
    /// Label texts with no control.
    orphans: Vec<String>,
    /// Recognized fields in reading order.
    fields: Vec<RecognizedRow>,
    /// Bytes written.
    bytes: usize,
    /// Fields read back from the written PDF.
    loaded: usize,
}

/// Parse one hint kind.
fn parse_hint_kind(text: &str) -> anyhow::Result<HintKind> {
    match text.to_ascii_lowercase().as_str() {
        "label" => Ok(HintKind::Label),
        "input" | "input-box" | "text" => Ok(HintKind::InputBox),
        "checkbox" | "check" => Ok(HintKind::CheckBox),
        "radio" | "radio-group" => Ok(HintKind::RadioGroup),
        "choice" | "combo" | "list" => Ok(HintKind::Choice),
        other => {
            bail!("unknown hint kind {other:?}; expected label, input, checkbox, radio or choice")
        }
    }
}

/// `pdfxi form recognize`.
pub(crate) fn run_recognize(manifest: &Path, out: &Path, json: bool) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(manifest)
        .with_context(|| format!("read manifest {}", manifest.display()))?;
    let file: RecognitionFile = serde_json::from_str(&text)
        .with_context(|| format!("parse manifest {}", manifest.display()))?;
    let mut hints = Vec::with_capacity(file.hints.len());
    for (index, hint) in file.hints.iter().enumerate() {
        let kind =
            parse_hint_kind(&hint.kind).with_context(|| format!("hint {index}: {}", hint.kind))?;
        let mut model = FormHint::new(
            kind,
            WidgetRect::new(hint.x, hint.y, hint.width, hint.height),
            hint.page,
        );
        if let Some(label) = &hint.text {
            model = model.with_text(label.clone());
        }
        if let Some(name) = &hint.name {
            model = model.with_name(name.clone());
        }
        hints.push(model);
    }
    let plan = recognize(&hints).map_err(|error| anyhow::anyhow!("recognize hints: {error}"))?;
    if plan.fields().is_empty() {
        bail!("no form controls were recognized");
    }
    let conversion = plan
        .conversion_plan()
        .map_err(|error| anyhow::anyhow!("convert recognized hints: {error}"))?;
    let specs = conversion.specs();
    let fields: Vec<PdfValue> = specs
        .iter()
        .map(|spec| {
            spec.field_dictionary()
                .map_err(|error| anyhow::anyhow!("build field {:?}: {error}", spec.name()))
        })
        .collect::<anyhow::Result<_>>()?;
    let widgets: Vec<PdfValue> = specs
        .iter()
        .map(|spec| {
            spec.widget_annotation()
                .map_err(|error| anyhow::anyhow!("build widget {:?}: {error}", spec.name()))
        })
        .collect::<anyhow::Result<_>>()?;
    let page = pdfxi_forms::ObjectRef::new(PAGE_OBJECT, 0);
    let bytes = serialize_form_pdf(&fields, &widgets, Some(page))
        .map_err(|error| anyhow::anyhow!("serialize recognized form: {error}"))?;
    let loaded = loaded_fields(&bytes)?.context("the serialized document has no /AcroForm")?;
    if loaded.len() != plan.fields().len() {
        bail!(
            "wrote {} fields but reloaded {}",
            plan.fields().len(),
            loaded.len()
        );
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

    let report = RecognizeReport {
        manifest: manifest.display().to_string(),
        out: out.display().to_string(),
        hints: hints.len(),
        recognized: plan.fields().len(),
        orphans: plan.orphan_labels().to_vec(),
        fields: plan
            .fields()
            .iter()
            .map(|field| RecognizedRow {
                name: field.name().to_owned(),
                kind: field.field_type().to_string(),
                page: field.page(),
                rect: [
                    field.rect().x,
                    field.rect().y,
                    field.rect().width,
                    field.rect().height,
                ],
                label: field.label().map(str::to_owned),
                confidence: field.confidence(),
                rank: field.rank(),
            })
            .collect(),
        bytes: bytes.len(),
        loaded: loaded.len(),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("out: {}", report.out);
    println!(
        "hints: {}  recognized: {}  orphans: {}  bytes: {}  loaded: {}",
        report.hints,
        report.recognized,
        report.orphans.len(),
        report.bytes,
        report.loaded
    );
    for row in &report.fields {
        let rect = format!(
            "{} {} {} {}",
            row.rect[0], row.rect[1], row.rect[2], row.rect[3]
        );
        match &row.label {
            Some(label) => println!(
                "field: {} [{}] page {} rect: {rect} label: {} confidence: {:.2}",
                row.name, row.kind, row.page, label, row.confidence
            ),
            None => println!(
                "field: {} [{}] page {} rect: {rect} confidence: {:.2}",
                row.name, row.kind, row.page, row.confidence
            ),
        }
    }
    Ok(())
}
