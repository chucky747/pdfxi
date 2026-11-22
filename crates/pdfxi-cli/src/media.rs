//! `pdfxi media`: media annotations and the HTML5 replacement envelope.
//!
//! [`pdfxi_render::media`] models `/Sound`, `/Movie` and `/Screen` annotations,
//! rendition selection and the playback plan a host player must realise.
//! [`pdfxi_render::html5_media`] builds a deterministic, local-only HTML5
//! `<audio>`/`<video>` envelope. This module reads real media annotations,
//! resolves their playback plans, can extract an embedded payload, and writes
//! the HTML5 envelope that replaces a Flash-era player.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_core::filter::{Filter, FlateParams, decode_stream};
use pdfxi_core::{Dictionary, Document, Object};
use pdfxi_render::html5_media::{Html5Descriptor, Html5EnvelopeInput, Html5SourceInput};
use pdfxi_render::media::{
    MediaAnnotation, MediaDictionary, MediaSource, RenditionInput, RenditionSelection,
};
use serde::Serialize;

/// One media annotation row.
#[derive(Serialize)]
struct MediaRow {
    /// Zero-based page.
    page: usize,
    /// Annotation object number.
    annotation: u32,
    /// Annotation subtype.
    kind: String,
    /// Primary source descriptor.
    source: String,
    /// Advertised MIME type.
    mime_type: String,
    /// Active rendition name.
    rendition: String,
    /// Activation trigger code.
    activation: String,
    /// Auto-play flag.
    auto_play: bool,
    /// Loop flag.
    loop_play: bool,
    /// Volume in 0..=1.
    volume: f32,
    /// Host requirements.
    requirements: Vec<String>,
    /// Whether the host must open an external player.
    needs_host_player: bool,
    /// Stable playback descriptor.
    descriptor: String,
}

/// JSON shape of `pdfxi media list`.
#[derive(Serialize)]
struct MediaReport {
    /// Source PDF.
    file: String,
    /// Pages inspected.
    pages: usize,
    /// Number of media annotations.
    annotations: usize,
    /// Extracted payload paths.
    extracted: Vec<String>,
    /// Rows in page order.
    rows: Vec<MediaRow>,
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

/// Read a name or string entry as text.
fn text_entry(document: &Document, dictionary: &Dictionary, key: &str) -> Option<String> {
    match dictionary.get(key) {
        Some(Object::Name(name)) => Some(name.clone()),
        Some(Object::String(bytes)) => Some(String::from_utf8_lossy(bytes).into_owned()),
        Some(Object::Reference(_)) => {
            let value = resolve(document, &dictionary.get(key)?.clone()).ok()?;
            match value {
                Object::Name(name) => Some(name),
                Object::String(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Decode a stream's bytes with the common single-step filters.
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
    let filters: Vec<Filter> = names
        .iter()
        .map(|name| match name.as_str() {
            "FlateDecode" | "Fl" => Filter::FlateDecode(FlateParams::default()),
            "LZWDecode" | "LZW" => Filter::LzwDecode,
            "ASCII85Decode" | "A85" => Filter::Ascii85Decode,
            "ASCIIHexDecode" | "AHx" => Filter::AsciiHexDecode,
            "RunLengthDecode" | "RL" => Filter::RunLengthDecode,
            other => Filter::Unknown(other.to_owned()),
        })
        .collect();
    decode_stream(data, &filters).map_err(|error| anyhow::anyhow!("decode media stream: {error}"))
}

/// Guess a MIME type from a file extension.
fn mime_for(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    let extension = lower.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    match extension {
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "ogg" | "oga" => "audio/ogg",
        _ => "application/octet-stream",
    }
}

/// Read a file name from a string, name or file-specification dictionary.
fn file_name(document: &Document, value: &Object) -> Option<String> {
    let resolved = resolve(document, value).ok()?;
    match resolved {
        Object::String(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
        Object::Name(name) => Some(name),
        Object::Dictionary(dict) => {
            let inner = dict.get("UF").or_else(|| dict.get("F"))?;
            match resolve(document, inner).ok()? {
                Object::String(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
                Object::Name(name) => Some(name),
                _ => None,
            }
        }
        _ => None,
    }
}
/// A decoded stream payload ready to be written to disk.
type MediaPayload = (Dictionary, Vec<u8>);

/// Build one annotation's model input.
fn annotation_dictionary(
    document: &Document,
    annot: &Dictionary,
    kind: &str,
) -> anyhow::Result<(MediaDictionary, Option<MediaPayload>)> {
    let (source, mime_type, payload, name) = match kind {
        "Sound" => {
            let Some(value) = annot.get("Sound") else {
                bail!("a /Sound annotation has no /Sound stream");
            };
            let resolved = resolve(document, value)?;
            let reference = match value {
                Object::Reference(id) => MediaSource::Object {
                    number: id.number,
                    generation: id.generation,
                },
                _ => MediaSource::Embedded("sound".to_owned()),
            };
            let payload = match resolved {
                Object::Stream { dictionary, data } => Some((dictionary, data)),
                _ => None,
            };
            (
                reference,
                "audio/x-pdf-sound".to_owned(),
                payload,
                "sound".to_owned(),
            )
        }
        "Movie" => {
            let Some(movie_value) = annot.get("Movie") else {
                bail!("a /Movie annotation has no /Movie dictionary");
            };
            let Object::Dictionary(movie) = resolve(document, movie_value)? else {
                bail!("a /Movie annotation /Movie entry is not a dictionary");
            };
            let name = movie
                .get("F")
                .and_then(|value| file_name(document, value))
                .unwrap_or_else(|| {
                    text_entry(document, &movie, "T").unwrap_or_else(|| "movie".to_owned())
                });
            let mime_type = mime_for(&name).to_owned();
            (MediaSource::File(name.clone()), mime_type, None, name)
        }
        _ => (
            MediaSource::Embedded("screen".to_owned()),
            "application/octet-stream".to_owned(),
            None,
            "screen".to_owned(),
        ),
    };
    let volume = match annot.get("Volume") {
        Some(value) => value
            .as_f64()
            .map_or(1.0, |value| (value as f32).clamp(0.0, 1.0)),
        None => 1.0,
    };
    let primary = source.clone();
    Ok((
        MediaDictionary {
            kind: kind.to_owned(),
            source,
            mime_type: mime_type.clone(),
            poster: None,
            renditions: vec![RenditionInput {
                name,
                source: primary,
                mime_type,
            }],
            selection: RenditionSelection::Default,
            activation: "PV".to_owned(),
            auto_play: false,
            loop_play: false,
            volume,
            controls: Vec::new(),
        },
        payload,
    ))
}
/// Read all media annotation rows for one document.
fn scan(
    document: &Document,
    extract: Option<&Path>,
) -> anyhow::Result<(Vec<MediaRow>, Vec<String>)> {
    let pages = document
        .page_count()
        .map_err(|error| anyhow::anyhow!("count pages: {error}"))?;
    let mut rows = Vec::new();
    let mut extracted = Vec::new();
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
            let Some(subtype) = text_entry(document, &annot, "Subtype") else {
                continue;
            };
            if !matches!(subtype.as_str(), "Sound" | "Movie" | "Screen") {
                continue;
            }
            let (model, payload) = annotation_dictionary(document, &annot, &subtype)
                .with_context(|| format!("annotation {number}"))?;
            let model = MediaAnnotation::parse(model)
                .map_err(|error| anyhow::anyhow!("annotation {number}: {error}"))?;
            let plan = model.resolve_plan();
            if let (Some(dir), Some((dictionary, data))) = (extract, payload) {
                let bytes = stream_bytes(&dictionary, &data)?;
                let path = dir.join(format!("{number}-{}.bin", plan.kind().code()));
                std::fs::write(&path, &bytes)
                    .with_context(|| format!("write {}", path.display()))?;
                extracted.push(path.display().to_string());
            }
            rows.push(MediaRow {
                page: index + 1,
                annotation: number,
                kind: plan.kind().code().to_owned(),
                source: plan.source().descriptor(),
                mime_type: plan.mime_type().to_owned(),
                rendition: plan.rendition_name().to_owned(),
                activation: plan.activation().code().to_owned(),
                auto_play: plan.auto_play(),
                loop_play: plan.loop_play(),
                volume: plan.volume(),
                requirements: plan
                    .requirements()
                    .iter()
                    .map(|requirement| requirement.code().to_owned())
                    .collect(),
                needs_host_player: plan.needs_host_player(),
                descriptor: plan.descriptor(),
            });
        }
    }
    Ok((rows, extracted))
}

/// `pdfxi media list`.
pub(crate) fn list(file: &Path, extract: Option<&Path>, json: bool) -> anyhow::Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))?;
    let pages = document
        .page_count()
        .map_err(|error| anyhow::anyhow!("count pages: {error}"))?;
    let (rows, extracted) = scan(&document, extract)?;
    if rows.is_empty() {
        bail!(
            "{} declares no /Sound, /Movie or /Screen annotations",
            file.display()
        );
    }
    let report = MediaReport {
        file: file.display().to_string(),
        pages,
        annotations: rows.len(),
        extracted,
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
            "page {}  annotation {}  {}  {}  {}  rendition {}",
            row.page, row.annotation, row.kind, row.source, row.mime_type, row.rendition
        );
        println!(
            "  activation {}  autoplay {}  loop {}  volume {:.2}  host-player {}",
            row.activation, row.auto_play, row.loop_play, row.volume, row.needs_host_player
        );
        println!("  requirements: {}", row.requirements.join(", "));
        println!("  plan: {}", row.descriptor);
    }
    for path in &report.extracted {
        println!("extracted: {path}");
    }
    Ok(())
}

/// Parse `--source SRC=TYPE`.
fn parse_sources(values: &[String]) -> anyhow::Result<Vec<Html5SourceInput>> {
    if values.is_empty() {
        bail!("pass at least one --source SRC=MIME");
    }
    let mut sources = Vec::with_capacity(values.len());
    for value in values {
        let (src, mime_type) = value
            .rsplit_once('=')
            .with_context(|| format!("source {value:?} must be SRC=MIME"))?;
        sources.push(Html5SourceInput {
            src: src.to_owned(),
            mime_type: mime_type.to_owned(),
        });
    }
    Ok(sources)
}

/// A complete, inert HTML document around the media element.
fn document(descriptor: &Html5Descriptor) -> String {
    let mut out = String::from(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta http-equiv=\"Content-Security-Policy\" \
         content=\"default-src 'none'; media-src 'self'; img-src 'self'; style-src 'unsafe-inline'\">\n\
         <title>PDFXI media</title>\n\
         <style>html,body{margin:0;height:100%}body{display:grid;place-items:center;\
         background:#111}audio,video{max-width:100%;max-height:100%}</style>\n\
         </head>\n<body>\n",
    );
    out.push_str(&descriptor.to_markup());
    out.push_str("\n</body>\n</html>\n");
    out
}

/// `pdfxi media html5`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn html5(
    sources: &[String],
    poster: Option<&str>,
    controls: bool,
    autoplay: bool,
    loop_play: bool,
    muted: bool,
    width: u32,
    height: u32,
    out: Option<&Path>,
    json: bool,
) -> anyhow::Result<()> {
    let descriptor = Html5Descriptor::parse(Html5EnvelopeInput {
        sources: parse_sources(sources)?,
        poster: poster.map(str::to_owned),
        controls,
        autoplay,
        loop_play,
        muted,
        width,
        height,
    })
    .map_err(|error| anyhow::anyhow!("HTML5 envelope: {error}"))?;
    let markup = descriptor.to_markup();
    let page = document(&descriptor);
    if let Some(path) = out {
        std::fs::write(path, &page).with_context(|| format!("write {}", path.display()))?;
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "element": descriptor.element().code(),
                "descriptor": descriptor.descriptor(),
                "markup": markup,
                "output": out.map(|path| path.display().to_string()),
            }))?
        );
        return Ok(());
    }
    if out.is_none() {
        print!("{markup}");
        return Ok(());
    }
    println!("element: {}", descriptor.element().code());
    println!("descriptor: {}", descriptor.descriptor());
    println!(
        "output: {}",
        out.map_or_else(String::new, |path| path.display().to_string())
    );
    Ok(())
}
