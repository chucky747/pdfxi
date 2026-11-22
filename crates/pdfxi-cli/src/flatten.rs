//! `pdfxi flatten`: bake annotation and widget appearances into page content.
//!
//! [`pdfxi_edit::flatten`] implements the headless flatten model but has no PDF
//! object store: it consumes page dictionaries, content and appearance bytes
//! and returns content plus filtered dictionaries, leaving the XObject
//! materialization to a downstream writer. This module is that writer.
//!
//! The document is parsed once, every page's `/Annots` entries are resolved to
//! dictionaries with their `/AP /N` streams decoded, and the model computes the
//! placements. The result is then written into the real document: the page
//! `/Contents` becomes the flattened stream, the appearance bytes become form
//! XObjects under a fresh page `/Resources /XObject`, flattened annotations are
//! removed from `/Annots`, and the catalog `/AcroForm` is removed when no
//! widget survives. The written file is reopened and checked before it is
//! reported.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, bail};
use pdfxi_core::{
    Dictionary, Document, Filter, Object, ObjectId, decode_stream, decode_text_string,
};
use pdfxi_edit::flatten::{
    Appearance, AppearanceResource, FlattenEvent, FlattenInput, FlattenOptions, FlattenedDocument,
    PageBox, flatten,
};
use pdfxi_edit::geometry::Rect;
use pdfxi_edit::pdf::{Dict, Value};
use pdfxi_edit::redaction::Matrix;
use serde::Serialize;

/// Original appearance bytes and the geometry the model used for placement.
#[derive(Debug)]
struct AppearanceSource {
    /// The source stream dictionary, cloned into the materialized XObject.
    dictionary: Dictionary,
    /// The appearance `/BBox`.
    bbox: Rect,
    /// The appearance `/Matrix` coefficients.
    matrix: [f64; 6],
}

/// One page's original annotation entries plus their edit-model projection.
#[derive(Debug, Default)]
struct PageAnnotations {
    /// Original `/Annots` entries in array order.
    entries: Vec<Object>,
    /// Projection of each entry for retention matching.
    values: Vec<Value>,
}

/// JSON shape of one event.
#[derive(Debug, Serialize)]
struct EventJson {
    /// Event kind: `flattened`, `kept-link`, `skipped`, `removed-widget`,
    /// `acroform-removed`.
    kind: String,
    /// One-based page, when the event belongs to a page.
    page: Option<u32>,
    /// Annotation subtype, when known.
    subtype: Option<String>,
    /// Widget field name, when known.
    field_name: Option<String>,
    /// Emitted resource name, when known.
    resource_name: Option<String>,
    /// Skip reason, when known.
    reason: Option<String>,
}

/// JSON shape of `pdfxi flatten`.
#[derive(Debug, Serialize)]
struct FlattenJson {
    /// Source PDF.
    file: String,
    /// Written PDF, or `null` in a dry run.
    out: Option<String>,
    /// Written size in bytes.
    bytes: Option<usize>,
    /// Page count.
    pages: usize,
    /// Number of flattened annotations and widgets.
    flattened: usize,
    /// Number of flattened widgets.
    widgets: usize,
    /// Number of skipped or removed entries.
    skipped: usize,
    /// Whether the catalog `/AcroForm` was removed.
    acroform_removed: bool,
    /// Distinct pages touched.
    pages_touched: Vec<u32>,
    /// Materialized appearance XObjects.
    resources: Vec<MaterializedResource>,
    /// Recorded events.
    events: Vec<EventJson>,
    /// Whether nothing was written.
    dry_run: bool,
}

/// JSON shape of one materialized appearance.
#[derive(Debug, Serialize)]
struct MaterializedResource {
    /// One-based page.
    page: u32,
    /// Resource name used by the content `Do`.
    name: String,
    /// XObject content size.
    bytes: usize,
}

/// Converts one core object into the dependency-free edit model.
///
/// References and streams become stable textual markers: the model never
/// interprets them, but they keep retention equality exact for annotations that
/// stay interactive.
fn convert_value(object: &Object) -> Value {
    match object {
        Object::Null => Value::Null,
        Object::Boolean(value) => Value::Bool(*value),
        Object::Integer(value) => Value::Int(*value),
        Object::Real(value) => Value::Real(*value),
        Object::Name(name) => Value::Name(name.clone()),
        Object::String(bytes) => Value::Text(decode_text_string(bytes)),
        Object::Array(items) => Value::Array(items.iter().map(convert_value).collect()),
        Object::Dictionary(dictionary) => Value::Dictionary(
            dictionary
                .iter()
                .map(|(key, value)| (key.clone(), convert_value(value)))
                .collect(),
        ),
        Object::Reference(id) => Value::Text(format!("#ref {} {}", id.number, id.generation)),
        Object::Stream { .. } => Value::Text("#stream".to_owned()),
    }
}

/// Resolves one object through indirect references.
fn resolve(document: &Document, object: &Object) -> anyhow::Result<Object> {
    match object {
        Object::Reference(id) => document
            .object(*id)
            .map_err(|error| anyhow::anyhow!("resolve object {}: {error}", id.number)),
        other => Ok(other.clone()),
    }
}

/// Decodes a stream payload, supporting the unfiltered and Flate cases.
fn decode_stream_object(dictionary: &Dictionary, data: &[u8]) -> anyhow::Result<Vec<u8>> {
    let filters = match dictionary.get("Filter") {
        None => Vec::new(),
        Some(Object::Name(name)) if name == "FlateDecode" => {
            vec![Filter::FlateDecode(Default::default())]
        }
        Some(Object::Array(items)) => {
            let mut filters = Vec::with_capacity(items.len());
            for item in items {
                match item.as_name() {
                    Some("FlateDecode") => {
                        filters.push(Filter::FlateDecode(Default::default()));
                    }
                    Some(other) => bail!("appearance stream uses unsupported filter {other:?}"),
                    None => bail!("appearance stream /Filter array holds a non-name"),
                }
            }
            filters
        }
        Some(other) => bail!("appearance stream /Filter is not a name or array: {other:?}"),
    };
    decode_stream(data, &filters).map_err(|error| anyhow::anyhow!("decode appearance: {error}"))
}

/// Projects one annotation dictionary, registering any normal appearance.
fn project_annotation(
    document: &Document,
    dictionary: &Dictionary,
    appearances: &mut BTreeMap<String, Appearance>,
    sources: &mut BTreeMap<String, AppearanceSource>,
    next_key: &mut usize,
) -> anyhow::Result<Value> {
    let mut projected = Dict::new();
    for (key, value) in dictionary {
        if key == "AP" {
            let resolved = resolve(document, value)?;
            let Some(appearance_dict) = resolved.as_dictionary() else {
                projected.insert(key.clone(), convert_value(value));
                continue;
            };
            let normal = match appearance_dict.get("N") {
                Some(normal) => resolve(document, normal)?,
                None => {
                    projected.insert(key.clone(), convert_value(value));
                    continue;
                }
            };
            let Object::Stream {
                dictionary: stream_dict,
                data,
            } = &normal
            else {
                projected.insert(key.clone(), convert_value(value));
                continue;
            };
            let key_name = format!("xi-ap-{next_key}");
            *next_key += 1;
            let content = decode_stream_object(stream_dict, data)
                .with_context(|| format!("annotation appearance {key_name}"))?;
            let bbox = stream_rect(stream_dict.get("BBox"))
                .unwrap_or(Rect::saturating(0.0, 0.0, 1.0, 1.0));
            let matrix = stream_matrix(stream_dict.get("Matrix"));
            appearances.insert(
                key_name.clone(),
                Appearance::new(
                    content.clone(),
                    Matrix::new(
                        matrix[0], matrix[1], matrix[2], matrix[3], matrix[4], matrix[5],
                    ),
                    bbox,
                ),
            );
            sources.insert(
                key_name.clone(),
                AppearanceSource {
                    dictionary: stream_dict.clone(),
                    bbox,
                    matrix,
                },
            );
            let mut ap = Dict::new();
            ap.insert("N".to_owned(), Value::Name(key_name));
            projected.insert(key.clone(), Value::Dictionary(ap));
            continue;
        }
        let resolved = resolve(document, value)?;
        projected.insert(key.clone(), convert_value(&resolved));
    }
    Ok(Value::Dictionary(projected))
}

/// Reads a four-number rectangle from a stream dictionary value.
fn stream_rect(value: Option<&Object>) -> Option<Rect> {
    let Object::Array(items) = value? else {
        return None;
    };
    let mut numbers = [0.0_f32; 4];
    for (slot, item) in numbers.iter_mut().zip(items) {
        *slot = item.as_f64()? as f32;
    }
    Some(Rect::saturating(
        numbers[0], numbers[1], numbers[2], numbers[3],
    ))
}

/// Reads a six-number matrix, defaulting to identity.
fn stream_matrix(value: Option<&Object>) -> [f64; 6] {
    let Some(Object::Array(items)) = value else {
        return [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    };
    let mut numbers = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    for (slot, item) in numbers.iter_mut().zip(items) {
        match item.as_f64() {
            Some(number) => *slot = number,
            None => return [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        }
    }
    numbers
}

/// Converts a sprite-free numeric array from the model rect.
fn rect_array(rect: Rect) -> Object {
    Object::Array(vec![
        Object::Real(f64::from(rect.x0())),
        Object::Real(f64::from(rect.y0())),
        Object::Real(f64::from(rect.x1())),
        Object::Real(f64::from(rect.y1())),
    ])
}

/// Converts the model matrix into a PDF array.
fn matrix_array(matrix: [f64; 6]) -> Object {
    Object::Array(matrix.into_iter().map(Object::Real).collect())
}

/// Builds the retained `/Annots` array by matching the model's projection.
fn retained_annots(
    original: &PageAnnotations,
    flattened: &FlattenedDocument,
    page: u32,
) -> Vec<Object> {
    let retained: &[Value] = flattened
        .page_dictionary(page)
        .and_then(|dictionary| dictionary.get("Annots"))
        .and_then(Value::as_array)
        .unwrap_or_default();
    let mut remaining: Vec<Value> = retained.to_vec();
    let mut kept = Vec::new();
    for (entry, value) in original.entries.iter().zip(&original.values) {
        if let Some(index) = remaining.iter().position(|candidate| candidate == value) {
            remaining.remove(index);
            kept.push(entry.clone());
        }
    }
    kept
}

/// Materializes one appearance stream as a form XObject.
fn materialize_xobject(
    document: &mut Document,
    source: &AppearanceSource,
    bytes: &[u8],
) -> anyhow::Result<ObjectId> {
    let mut dictionary = source.dictionary.clone();
    dictionary.remove("Length");
    dictionary.remove("Filter");
    dictionary.remove("DecodeParms");
    dictionary.insert("Type".to_owned(), Object::Name("XObject".to_owned()));
    dictionary.insert("Subtype".to_owned(), Object::Name("Form".to_owned()));
    dictionary.insert("BBox".to_owned(), rect_array(source.bbox));
    dictionary.insert("Matrix".to_owned(), matrix_array(source.matrix));
    let id = ObjectId::new(
        document
            .allocate_object_number()
            .map_err(|error| anyhow::anyhow!("allocate object number: {error}"))?,
        0,
    );
    document
        .set_object(
            id,
            Object::Stream {
                dictionary,
                data: bytes.to_vec(),
            },
        )
        .map_err(|error| anyhow::anyhow!("write appearance XObject: {error}"))?;
    Ok(id)
}

/// Rewrites one page with the flattened content, resources and annotations.
fn rewrite_page(
    document: &mut Document,
    page_number: usize,
    flattened: &FlattenedDocument,
    annots: Vec<Object>,
    resources: &[AppearanceResource],
    sources: &BTreeMap<String, AppearanceSource>,
) -> anyhow::Result<Vec<MaterializedResource>> {
    let page = document
        .page(page_number - 1)
        .map_err(|error| anyhow::anyhow!("page {page_number}: {error}"))?;
    let mut dictionary = document
        .object(page.id)
        .map_err(|error| anyhow::anyhow!("page {page_number} dictionary: {error}"))?
        .as_dictionary()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("page {page_number} dictionary is not a dictionary"))?;

    let content = flattened
        .page_content(page_number as u32)
        .ok_or_else(|| anyhow::anyhow!("the model produced no content for page {page_number}"))?;
    let content_id = ObjectId::new(
        document
            .allocate_object_number()
            .map_err(|error| anyhow::anyhow!("allocate object number: {error}"))?,
        0,
    );
    document
        .set_object(
            content_id,
            Object::Stream {
                dictionary: Dictionary::new(),
                data: content.to_vec(),
            },
        )
        .map_err(|error| anyhow::anyhow!("write page content: {error}"))?;
    dictionary.insert("Contents".to_owned(), Object::Reference(content_id));

    let mut page_resources = match dictionary.get("Resources") {
        Some(resources) => resolve(document, resources)?
            .as_dictionary()
            .cloned()
            .unwrap_or_default(),
        // Resources may be inherited from an ancestor page-tree node.
        None => match page.resources {
            Some(resources) => document
                .object(resources)
                .map_err(|error| anyhow::anyhow!("page {page_number} resources: {error}"))?
                .as_dictionary()
                .cloned()
                .unwrap_or_default(),
            None => Dictionary::new(),
        },
    };
    let mut xobjects = match page_resources.get("XObject") {
        Some(xobjects) => resolve(document, xobjects)?
            .as_dictionary()
            .cloned()
            .unwrap_or_default(),
        None => Dictionary::new(),
    };
    let mut materialized = Vec::with_capacity(resources.len());
    for resource in resources
        .iter()
        .filter(|resource| resource.page as usize == page_number)
    {
        let source = sources.get(&resource.appearance_key).ok_or_else(|| {
            anyhow::anyhow!(
                "the model referenced appearance {:?} without a source",
                resource.appearance_key
            )
        })?;
        let id = materialize_xobject(document, source, &resource.content)?;
        xobjects.insert(resource.resource_name.clone(), Object::Reference(id));
        materialized.push(MaterializedResource {
            page: resource.page,
            name: resource.resource_name.clone(),
            bytes: resource.content.len(),
        });
    }
    page_resources.insert("XObject".to_owned(), Object::Dictionary(xobjects));
    let resources_id = ObjectId::new(
        document
            .allocate_object_number()
            .map_err(|error| anyhow::anyhow!("allocate object number: {error}"))?,
        0,
    );
    document
        .set_object(resources_id, Object::Dictionary(page_resources))
        .map_err(|error| anyhow::anyhow!("write page resources: {error}"))?;
    dictionary.insert("Resources".to_owned(), Object::Reference(resources_id));

    if annots.is_empty() {
        dictionary.remove("Annots");
    } else {
        dictionary.insert("Annots".to_owned(), Object::Array(annots));
    }
    document
        .set_object(page.id, Object::Dictionary(dictionary))
        .map_err(|error| anyhow::anyhow!("write page {page_number}: {error}"))?;
    Ok(materialized)
}

/// Turns one model event into the JSON projection.
fn event_json(event: &FlattenEvent) -> EventJson {
    match event {
        FlattenEvent::Flattened {
            page,
            subtype,
            field_name,
            resource_name,
        } => EventJson {
            kind: "flattened".to_owned(),
            page: Some(*page),
            subtype: Some(subtype.clone()),
            field_name: field_name.clone(),
            resource_name: Some(resource_name.clone()),
            reason: None,
        },
        FlattenEvent::KeptLink { page, subtype } => EventJson {
            kind: "kept-link".to_owned(),
            page: Some(*page),
            subtype: Some(subtype.clone()),
            field_name: None,
            resource_name: None,
            reason: None,
        },
        FlattenEvent::Skipped {
            page,
            subtype,
            reason,
        } => EventJson {
            kind: "skipped".to_owned(),
            page: Some(*page),
            subtype: Some(subtype.clone()),
            field_name: None,
            resource_name: None,
            reason: Some(reason.to_string()),
        },
        FlattenEvent::RemovedWidget {
            page,
            field_name,
            reason,
        } => EventJson {
            kind: "removed-widget".to_owned(),
            page: Some(*page),
            subtype: None,
            field_name: field_name.clone(),
            resource_name: None,
            reason: Some(reason.to_string()),
        },
        FlattenEvent::AcroFormRemoved => EventJson {
            kind: "acroform-removed".to_owned(),
            page: None,
            subtype: None,
            field_name: None,
            resource_name: None,
            reason: None,
        },
    }
}

/// `pdfxi flatten`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn flatten_document(
    file: &std::path::Path,
    out: &std::path::Path,
    pages: &[usize],
    keep_widgets_without_appearance: bool,
    drop_links: bool,
    dry_run: bool,
    json: bool,
) -> anyhow::Result<()> {
    let mut page_set = BTreeSet::new();
    for page in pages {
        if *page == 0 {
            bail!("--page is one-based and must be at least 1");
        }
        if !page_set.insert(*page as u32) {
            bail!("--page {page} was given more than once");
        }
    }
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let mut document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))?;
    let page_count = document
        .page_count()
        .map_err(|error| anyhow::anyhow!("count pages in {}: {error}", file.display()))?;
    if let Some(page) = page_set.iter().find(|page| **page as usize > page_count) {
        bail!(
            "--page {page} is outside 1..={page_count} ({} has {page_count} page(s))",
            file.display()
        );
    }

    let mut input = FlattenInput::default();
    let mut page_annotations: BTreeMap<usize, PageAnnotations> = BTreeMap::new();
    let mut appearance_sources: BTreeMap<String, AppearanceSource> = BTreeMap::new();
    let mut next_key = 0usize;

    for index in 0..page_count {
        let page = document
            .page(index)
            .map_err(|error| anyhow::anyhow!("page {}: {error}", index + 1))?;
        let content = document
            .page_content(index)
            .map_err(|error| anyhow::anyhow!("page {} content: {error}", index + 1))?;
        let frame = document
            .page_frame(index)
            .map_err(|error| anyhow::anyhow!("page {} frame: {error}", index + 1))?;
        let page_box = PageBox::new(
            Rect::saturating(
                frame.media[0] as f32,
                frame.media[1] as f32,
                frame.media[2] as f32,
                frame.media[3] as f32,
            ),
            frame.rotation,
        )
        .map_err(|error| anyhow::anyhow!("page {} box: {error}", index + 1))?;
        input.pages.insert(index as u32 + 1, content);
        input.boxes.insert(index as u32 + 1, page_box);

        let page_dictionary = document
            .object(page.id)
            .map_err(|error| anyhow::anyhow!("page {} dictionary: {error}", index + 1))?
            .as_dictionary()
            .cloned()
            .unwrap_or_default();
        let annots = match page_dictionary.get("Annots") {
            Some(annots) => resolve(&document, annots)?,
            None => Object::Array(Vec::new()),
        };
        let entries: Vec<Object> = annots
            .as_array()
            .map(<[Object]>::to_vec)
            .unwrap_or_default();
        let mut record = PageAnnotations::default();
        let mut projected = Vec::with_capacity(entries.len());
        for entry in &entries {
            let annotation = resolve(&document, entry)?;
            let Some(dictionary) = annotation.as_dictionary() else {
                record.entries.push(entry.clone());
                record.values.push(convert_value(entry));
                projected.push(convert_value(entry));
                continue;
            };
            let value = project_annotation(
                &document,
                dictionary,
                &mut input.appearances,
                &mut appearance_sources,
                &mut next_key,
            )?;
            record.entries.push(entry.clone());
            record.values.push(value.clone());
            projected.push(value);
        }
        let mut dictionary = Dict::new();
        dictionary.insert("Annots".to_owned(), Value::Array(projected));
        input.dictionaries.insert(index as u32 + 1, dictionary);
        page_annotations.insert(index + 1, record);
    }

    let catalog_id = document
        .root()
        .map_err(|error| anyhow::anyhow!("resolve catalog: {error}"))?;
    let catalog = document
        .object(catalog_id)
        .map_err(|error| anyhow::anyhow!("catalog: {error}"))?
        .as_dictionary()
        .cloned()
        .unwrap_or_default();
    if catalog.contains_key("AcroForm") {
        input.acroform = Some(Dict::new());
    }

    let options = FlattenOptions {
        pages: page_set.clone(),
        keep_links: !drop_links,
        keep_widgets_without_appearance,
        dry_run,
    };
    let flattened = flatten(&input, &options, &|| false)
        .map_err(|error| anyhow::anyhow!("flatten {}: {error}", file.display()))?;
    let report = flattened.report();

    let mut resources = Vec::new();
    if !dry_run {
        let targets: BTreeSet<u32> = if page_set.is_empty() {
            (1..=page_count as u32).collect()
        } else {
            page_set.clone()
        };
        for page in &targets {
            let record = page_annotations.get(&(*page as usize)).ok_or_else(|| {
                anyhow::anyhow!("page {page} disappeared from the annotation map")
            })?;
            let annots = retained_annots(record, &flattened, *page);
            let page_resources: Vec<AppearanceResource> = report
                .unresolved_appearances()
                .iter()
                .filter(|resource| resource.page == *page)
                .cloned()
                .collect();
            resources.extend(rewrite_page(
                &mut document,
                *page as usize,
                &flattened,
                annots,
                &page_resources,
                &appearance_sources,
            )?);
        }
        if report.acroform_removed() {
            let mut catalog = catalog;
            catalog.remove("AcroForm");
            document
                .set_object(catalog_id, Object::Dictionary(catalog))
                .map_err(|error| anyhow::anyhow!("remove catalog /AcroForm: {error}"))?;
        }
        let saved = document
            .save()
            .map_err(|error| anyhow::anyhow!("save {}: {error}", file.display()))?;
        std::fs::write(out, &saved).with_context(|| format!("write {}", out.display()))?;
        // Reopen the written bytes and prove the flattened resources are real.
        let written = std::fs::read(out).with_context(|| format!("read back {}", out.display()))?;
        let reopened = Document::from_bytes(written.clone())
            .map_err(|error| anyhow::anyhow!("reparse {}: {error}", out.display()))?;
        let reopened_pages = reopened
            .page_count()
            .map_err(|error| anyhow::anyhow!("count {} pages: {error}", out.display()))?;
        if reopened_pages != page_count {
            bail!(
                "{} has {reopened_pages} pages after flattening, expected {page_count}",
                out.display()
            );
        }
        for resource in &resources {
            let page = reopened
                .page(resource.page as usize - 1)
                .map_err(|error| anyhow::anyhow!("reopen page {}: {error}", resource.page))?;
            let dictionary = reopened
                .object(page.id)
                .map_err(|error| anyhow::anyhow!("reopen page dictionary: {error}"))?
                .as_dictionary()
                .cloned()
                .unwrap_or_default();
            let page_resources = match dictionary.get("Resources") {
                Some(resources) => resolve(&reopened, resources)?,
                None => bail!("page {} lost its resources", resource.page),
            };
            let xobjects = page_resources
                .as_dictionary()
                .and_then(|dictionary| dictionary.get("XObject"))
                .map(|xobjects| resolve(&reopened, xobjects))
                .transpose()?
                .and_then(|xobjects| xobjects.as_dictionary().cloned())
                .unwrap_or_default();
            if !xobjects.contains_key(&resource.name) {
                bail!(
                    "page {} is missing flattened resource {}",
                    resource.page,
                    resource.name
                );
            }
        }
    }

    let events: Vec<EventJson> = report.events().iter().map(event_json).collect();
    let payload = FlattenJson {
        file: file.display().to_string(),
        out: if dry_run {
            None
        } else {
            Some(out.display().to_string())
        },
        bytes: if dry_run {
            None
        } else {
            Some(
                std::fs::metadata(out)
                    .with_context(|| format!("stat {}", out.display()))?
                    .len() as usize,
            )
        },
        pages: page_count,
        flattened: report.flattened_count(),
        widgets: report.widget_count(),
        skipped: report.skipped_count(),
        acroform_removed: report.acroform_removed(),
        pages_touched: report.pages_touched(),
        resources,
        events,
        dry_run,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        println!("file: {}", payload.file);
        if let Some(out) = &payload.out {
            println!("out: {out} ({} bytes)", payload.bytes.unwrap_or(0));
        } else {
            println!("out: <dry run>");
        }
        println!("pages: {}", payload.pages);
        println!(
            "flattened: {}  widgets: {}  skipped: {}  acroform-removed: {}",
            payload.flattened, payload.widgets, payload.skipped, payload.acroform_removed
        );
        if !payload.pages_touched.is_empty() {
            println!(
                "pages-touched: {}",
                payload
                    .pages_touched
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            );
        }
        for event in &payload.events {
            let mut line = event.kind.clone();
            if let Some(page) = event.page {
                line.push_str(&format!(" page={page}"));
            }
            if let Some(subtype) = &event.subtype {
                line.push_str(&format!(" subtype={subtype}"));
            }
            if let Some(field) = &event.field_name {
                line.push_str(&format!(" field={field}"));
            }
            if let Some(resource) = &event.resource_name {
                line.push_str(&format!(" resource={resource}"));
            }
            if let Some(reason) = &event.reason {
                line.push_str(&format!(" reason={reason}"));
            }
            println!("{line}");
        }
        for resource in &payload.resources {
            println!(
                "resource: page={} name={} bytes={}",
                resource.page, resource.name, resource.bytes
            );
        }
    }
    Ok(())
}
