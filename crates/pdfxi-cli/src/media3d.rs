//! `pdfxi media3d`: read `/3D` annotations and extract their payloads.
//!
//! [`pdfxi_render::three_d`] models the `/3D` stream reference, format, named
//! views, activation event/state and animation flag, and resolves the backend
//! plan. This host finds real `/3D` annotations in a PDF's page `/Annots`,
//! reports the plan, and can extract the decoded `/3DD` payload for an external
//! 3D viewer. It never decodes U3D/PRC geometry itself.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_core::filter::decode_stream;
use pdfxi_core::{Dictionary, Document, Object};
use pdfxi_render::three_d::{ThreeDAnnotation, ThreeDDictionary, ThreeDStreamRef};
use serde::Serialize;

/// One 3D annotation row.
#[derive(Serialize)]
struct ThreeDRow {
    /// Zero-based page.
    page: usize,
    /// Annotation object number.
    annotation: u32,
    /// `/3D` stream reference descriptor.
    stream: String,
    /// Stream format code.
    format: String,
    /// Decoded payload length in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    payload_bytes: Option<usize>,
    /// Declared view names in document order.
    views: Vec<String>,
    /// Active view name.
    active_view: String,
    /// Activation event code.
    activation_event: String,
    /// Activation state code.
    activation_state: String,
    /// Whether animation is requested.
    animate: bool,
    /// Backend requirements.
    requirements: Vec<String>,
    /// Stable plan descriptor.
    plan: String,
}

/// JSON shape of `pdfxi media3d info`.
#[derive(Serialize)]
struct ThreeDReport {
    /// Source PDF.
    file: String,
    /// Pages inspected.
    pages: usize,
    /// Number of `/3D` annotations.
    annotations: usize,
    /// Rows in page order.
    rows: Vec<ThreeDRow>,
}

/// Resolve one reference chain to an owned object.
fn resolve(document: &Document, object: &Object) -> anyhow::Result<Object> {
    let mut current = object.clone();
    for _ in 0..32 {
        match current {
            Object::Reference(id) => {
                current = document
                    .object(id)
                    .with_context(|| format!("read object {} {}", id.number, id.generation))?;
            }
            other => return Ok(other),
        }
    }
    bail!("reference chain exceeds 32 objects")
}

/// Decode a stream object's bytes.
fn stream_bytes(dictionary: &Dictionary, data: &[u8]) -> anyhow::Result<Vec<u8>> {
    let names: Vec<String> = match dictionary.get("Filter") {
        None => Vec::new(),
        Some(Object::Name(name)) => vec![name.clone()],
        Some(Object::Array(items)) => items
            .iter()
            .filter_map(Object::as_name)
            .map(str::to_owned)
            .collect(),
        Some(other) => bail!("/Filter is not a name or array: {other:?}"),
    };
    let filters: Vec<pdfxi_core::filter::Filter> = names
        .iter()
        .map(|name| match name.as_str() {
            "FlateDecode" | "Fl" => pdfxi_core::filter::Filter::FlateDecode(Default::default()),
            "LZWDecode" | "LZW" => pdfxi_core::filter::Filter::LzwDecode,
            "DCTDecode" | "DCT" => pdfxi_core::filter::Filter::DctDecode,
            "JPXDecode" => pdfxi_core::filter::Filter::JpxDecode,
            "ASCII85Decode" | "A85" => pdfxi_core::filter::Filter::Ascii85Decode,
            "ASCIIHexDecode" | "AHx" => pdfxi_core::filter::Filter::AsciiHexDecode,
            other => pdfxi_core::filter::Filter::Unknown(other.to_owned()),
        })
        .collect();
    decode_stream(data, &filters).map_err(|error| anyhow::anyhow!("decode 3D stream: {error}"))
}

/// Read a name or string entry as text.
fn text_entry(document: &Document, dictionary: &Dictionary, key: &str) -> Option<String> {
    match dictionary.get(key) {
        Some(Object::Name(name)) => Some(name.clone()),
        Some(Object::String(bytes)) => Some(String::from_utf8_lossy(bytes).into_owned()),
        Some(Object::Reference(_)) => {
            let value = resolve(document, &dictionary.get(key)?.clone()).ok()?;
            value.as_name().map(str::to_owned)
        }
        _ => None,
    }
}

/// Collect `/3D` annotation rows for one document.
fn scan(document: &Document, extract: Option<&Path>) -> anyhow::Result<Vec<ThreeDRow>> {
    let pages = document
        .page_count()
        .map_err(|error| anyhow::anyhow!("count pages: {error}"))?;
    let mut rows = Vec::new();
    for index in 0..pages {
        let page = document
            .page(index)
            .map_err(|error| anyhow::anyhow!("read page {}: {error}", index + 1))?;
        let Object::Dictionary(page_dict) = document
            .object(page.id)
            .map_err(|error| anyhow::anyhow!("read page object: {error}"))?
        else {
            continue;
        };
        let Some(annots) = page_dict.get("Annots") else {
            continue;
        };
        let Object::Array(annotations) = resolve(document, annots)? else {
            continue;
        };
        for annotation in &annotations {
            let number = annotation.as_reference().map_or(0, |id| id.number);
            let Object::Dictionary(annot) = resolve(document, annotation)? else {
                continue;
            };
            if text_entry(document, &annot, "Subtype").as_deref() != Some("3D") {
                continue;
            }
            let Some(stream_object) = annot.get("3DD") else {
                continue;
            };
            let stream_ref = match stream_object {
                Object::Reference(id) => ThreeDStreamRef::Object {
                    number: id.number,
                    generation: id.generation,
                },
                Object::Name(name) => ThreeDStreamRef::Named(name.clone()),
                other => bail!("annotation {number}: /3DD is not a reference: {other:?}"),
            };
            let stream = resolve(document, stream_object)?;
            let (format, payload) = match &stream {
                Object::Stream { dictionary, data } => (
                    text_entry(document, dictionary, "Subtype"),
                    Some((dictionary, data)),
                ),
                other => bail!("annotation {number}: /3DD is not a stream: {other:?}"),
            };
            let format = format.unwrap_or_else(|| "U3D".to_owned());
            let views = match annot.get("3DV") {
                None => vec!["Default".to_owned()],
                Some(value) => {
                    let value = resolve(document, value)?;
                    let entries: Vec<Object> = match value {
                        Object::Array(items) => items,
                        other => vec![other],
                    };
                    entries
                        .iter()
                        .filter_map(|entry| {
                            let value = resolve(document, entry).ok()?;
                            let dict = value.as_dictionary()?;
                            text_entry(document, dict, "XN")
                        })
                        .collect::<Vec<String>>()
                }
            };
            let views = if views.is_empty() {
                vec!["Default".to_owned()]
            } else {
                views
            };
            let default_view = match annot.get("3DV") {
                None => None,
                Some(value) => {
                    let value = resolve(document, value)?;
                    let entries: Vec<Object> = match value {
                        Object::Array(items) => items,
                        other => vec![other],
                    };
                    entries.iter().find_map(|entry| {
                        let value = resolve(document, entry).ok()?;
                        let dict = value.as_dictionary()?;
                        let is_default = matches!(dict.get("IN"), Some(Object::Boolean(true)));
                        if !is_default {
                            return None;
                        }
                        text_entry(document, dict, "XN")
                    })
                }
            };
            let (activation_event, activation_state, animate) = match annot.get("3DA") {
                Some(value) => match resolve(document, value)? {
                    Object::Dictionary(dict) => (
                        text_entry(document, &dict, "A").unwrap_or_else(|| "PV".to_owned()),
                        text_entry(document, &dict, "D").unwrap_or_else(|| "U".to_owned()),
                        matches!(dict.get("AN"), Some(Object::Boolean(true))),
                    ),
                    _ => ("PV".to_owned(), "U".to_owned(), false),
                },
                None => ("PV".to_owned(), "U".to_owned(), false),
            };
            let model = ThreeDAnnotation::parse(ThreeDDictionary {
                stream: stream_ref,
                format,
                views,
                default_view,
                activation_event,
                activation_state,
                animate,
            })
            .map_err(|error| anyhow::anyhow!("annotation {number}: {error}"))?;
            let plan = model.resolve_plan();
            let payload_bytes = match payload {
                Some((dictionary, data)) => {
                    let bytes = stream_bytes(dictionary, data)?;
                    if let Some(path) = extract {
                        std::fs::write(path, &bytes)
                            .with_context(|| format!("write {}", path.display()))?;
                    }
                    Some(bytes.len())
                }
                None => None,
            };
            rows.push(ThreeDRow {
                page: index + 1,
                annotation: number,
                stream: model.stream().descriptor(),
                format: model.format().code(),
                payload_bytes,
                views: model
                    .views()
                    .iter()
                    .map(|view| view.name().to_owned())
                    .collect(),
                active_view: model
                    .active_view()
                    .map_or_else(String::new, |view| view.name().to_owned()),
                activation_event: model.activation_event().code().to_owned(),
                activation_state: model.activation_state().code().to_owned(),
                animate: model.animate(),
                requirements: plan.requirements().iter().map(|req| req.code()).collect(),
                plan: plan.descriptor(),
            });
        }
    }
    Ok(rows)
}

/// `pdfxi media3d info`.
pub(crate) fn info(file: &Path, extract: Option<&Path>, json: bool) -> anyhow::Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))?;
    let pages = document
        .page_count()
        .map_err(|error| anyhow::anyhow!("count pages: {error}"))?;
    let rows = scan(&document, extract)?;
    if rows.is_empty() {
        bail!("{} declares no /3D annotations", file.display());
    }
    let report = ThreeDReport {
        file: file.display().to_string(),
        pages,
        annotations: rows.len(),
        rows,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!(
        "file: {}  pages: {}  annotations: {}",
        report.file, report.pages, report.annotations
    );
    for row in &report.rows {
        println!(
            "page {}  annotation {}  format {}  stream {}{}",
            row.page,
            row.annotation,
            row.format,
            row.stream,
            row.payload_bytes
                .map_or_else(String::new, |len| format!("  payload {len} bytes"))
        );
        println!(
            "  views [{}]  active {}  activation {}/{}  animate {}",
            row.views.join(", "),
            row.active_view,
            row.activation_event,
            row.activation_state,
            row.animate
        );
        println!("  requirements: {}", row.requirements.join(", "));
        println!("  plan: {}", row.plan);
    }
    if let Some(path) = extract {
        println!("extracted: {}", path.display());
    }
    Ok(())
}
