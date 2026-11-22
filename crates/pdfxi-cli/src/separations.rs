//! `pdfxi separations`: derive print plates from real page content.
//!
//! [`pdfxi_app::print`] owns the canonical [`SeparationsPlan`](pdfxi_app::print::SeparationsPlan) (process-first
//! ordering, validated bounded names, stable ids). This host scans each page's
//! real content streams for colour operators, resolves `/ColorSpace`
//! resources (including `/Separation` and `/DeviceN` spot inks), counts paint
//! operations per plate and reports the plan. `--plate` is the output-preview
//! query: which pages image a given plate. Nothing is rasterized or separated.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_app::print::{SeparationInput, SeparationsPlan};
use pdfxi_core::filter::{Filter, FlateParams, decode_stream};
use pdfxi_core::{Dictionary, Document, Object, ObjectId};
use serde::Serialize;

/// The colour space currently selected in one graphics state.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Space {
    /// Device gray.
    Gray,
    /// Device RGB.
    Rgb,
    /// Device CMYK.
    Cmyk,
    /// A single spot colour.
    Separation(String),
    /// A DeviceN colourant list.
    DeviceN(Vec<String>),
    /// Any other space (ICC, Indexed, Pattern, unknown).
    Other,
}

/// Overprint flags declared by one `/ExtGState` entry.
#[derive(Clone, Copy, Debug, Default, Serialize)]
struct OverprintState {
    /// `/op`: overprint for non-stroking (fill) operations.
    fill: bool,
    /// `/OP`: overprint for stroking operations.
    stroke: bool,
    /// `/OPM` overprint mode (0 or 1).
    mode: u8,
}

/// The colour spaces and spot alternates declared by one page.
#[derive(Default)]
struct PageSpaces {
    /// Named colour spaces.
    spaces: BTreeMap<String, Space>,
    /// Spot ink to alternate process space label.
    alternates: BTreeMap<String, String>,
}

/// One plate row.
#[derive(Serialize)]
struct PlateRow {
    /// Stable one-based id from the plan.
    id: u32,
    /// Ink name.
    name: String,
    /// Whether this is a process ink.
    process: bool,
    /// Paint operations that image this plate.
    operations: usize,
    /// Paint operations executed under an overprint graphics state.
    overprint_operations: usize,
    /// Alternate process space declared by the spot colour space.
    #[serde(skip_serializing_if = "Option::is_none")]
    alternate: Option<String>,
    /// One-based pages that image this plate.
    pages: Vec<usize>,
}

/// One overprint graphics-state row.
#[derive(Serialize)]
struct OverprintRow {
    /// Resource name.
    name: String,
    /// `/op` fill overprint.
    fill: bool,
    /// `/OP` stroke overprint.
    stroke: bool,
    /// `/OPM` mode.
    mode: u8,
}

/// JSON shape of `pdfxi separations`.
#[derive(Serialize)]
struct SeparationsReport {
    /// Source PDF.
    file: String,
    /// Pages inspected.
    pages: usize,
    /// Plate rows in canonical order.
    plates: Vec<PlateRow>,
    /// Device-RGB paint operations (no fixed process plate).
    device_rgb_operations: usize,
    /// Device-gray paint operations.
    device_gray_operations: usize,
    /// Spot ink to alternate process space label.
    alternates: BTreeMap<String, String>,
    /// Overprint graphics states declared by the document.
    overprint_states: Vec<OverprintRow>,
    /// Stable plan summary.
    summary: String,
    /// Stable plan descriptor.
    descriptor: String,
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

/// Decode a content stream.
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
    decode_stream(data, &filters).map_err(|error| anyhow::anyhow!("decode content stream: {error}"))
}

/// Read the page's effective `/Resources` dictionary, walking `/Parent`.
fn page_resources(document: &Document, page: ObjectId) -> anyhow::Result<Option<Dictionary>> {
    let mut current = Object::Reference(page);
    for _ in 0..32 {
        let Object::Dictionary(dict) = resolve(document, &current)? else {
            return Ok(None);
        };
        if let Some(value) = dict.get("Resources") {
            return match resolve(document, value)? {
                Object::Dictionary(resources) => Ok(Some(resources)),
                _ => Ok(None),
            };
        }
        match dict.get("Parent") {
            Some(parent) => current = parent.clone(),
            None => return Ok(None),
        }
    }
    Ok(None)
}

/// Read the page's colour-space resources and spot alternates.
fn colour_spaces(
    document: &Document,
    resources: Option<&Dictionary>,
) -> anyhow::Result<PageSpaces> {
    let Some(resources) = resources else {
        return Ok(PageSpaces::default());
    };
    let Some(colours) = resources.get("ColorSpace") else {
        return Ok(PageSpaces::default());
    };
    let Object::Dictionary(colours) = resolve(document, colours)? else {
        return Ok(PageSpaces::default());
    };
    let mut page = PageSpaces::default();
    for (name, value) in &colours {
        let resolved = resolve(document, value)?;
        let space = space_of_resolved(document, &resolved)?;
        if let Object::Array(items) = &resolved
            && let Some(alternate) = alternate_label(document, items)
        {
            match &space {
                Space::Separation(ink) => {
                    page.alternates.insert(ink.clone(), alternate);
                }
                Space::DeviceN(inks) => {
                    for ink in inks {
                        page.alternates.insert(ink.clone(), alternate.clone());
                    }
                }
                _ => {}
            }
        }
        page.spaces.insert(name.clone(), space);
    }
    Ok(page)
}

/// Read the alternate process space of a Separation/DeviceN array.
fn alternate_label(document: &Document, items: &[Object]) -> Option<String> {
    let value = items.get(2)?;
    match resolve(document, value).ok()? {
        Object::Name(name) => Some(name),
        Object::Array(items) => Some(
            items
                .first()
                .and_then(Object::as_name)
                .unwrap_or("Array")
                .to_owned(),
        ),
        _ => None,
    }
}

/// Read the page's overprint graphics states.
fn overprint_states(
    document: &Document,
    resources: Option<&Dictionary>,
) -> anyhow::Result<BTreeMap<String, OverprintState>> {
    let Some(resources) = resources else {
        return Ok(BTreeMap::new());
    };
    let Some(states) = resources.get("ExtGState") else {
        return Ok(BTreeMap::new());
    };
    let Object::Dictionary(states) = resolve(document, states)? else {
        return Ok(BTreeMap::new());
    };
    let mut out = BTreeMap::new();
    for (name, value) in &states {
        let Object::Dictionary(state) = resolve(document, value)? else {
            continue;
        };
        let fill = matches!(state.get("op"), Some(Object::Boolean(true)));
        let stroke = matches!(state.get("OP"), Some(Object::Boolean(true)));
        let mode = match state.get("OPM") {
            Some(Object::Integer(value)) if *value >= 0 => (*value).min(1) as u8,
            _ => 0,
        };
        if fill || stroke {
            out.insert(name.clone(), OverprintState { fill, stroke, mode });
        }
    }
    Ok(out)
}

/// Map a colour-space object to a [`Space`].
fn space_of_resolved(document: &Document, value: &Object) -> anyhow::Result<Space> {
    match value {
        Object::Name(name) => Ok(match name.as_str() {
            "DeviceGray" | "G" => Space::Gray,
            "DeviceRGB" | "RGB" => Space::Rgb,
            "DeviceCMYK" | "CMYK" => Space::Cmyk,
            _ => Space::Other,
        }),
        Object::Array(items) => {
            let family = items.first().and_then(Object::as_name).unwrap_or_default();
            match family {
                "Separation" => {
                    let name = items
                        .get(1)
                        .map(|value| ink_name(document, value))
                        .transpose()?
                        .unwrap_or_default();
                    Ok(Space::Separation(name))
                }
                "DeviceN" => {
                    let names = match items.get(1).map(|value| resolve(document, value)) {
                        Some(Ok(Object::Array(names))) => names
                            .iter()
                            .map(|value| ink_name(document, value))
                            .collect::<anyhow::Result<Vec<String>>>()?,
                        _ => Vec::new(),
                    };
                    Ok(Space::DeviceN(names))
                }
                _ => Ok(Space::Other),
            }
        }
        _ => Ok(Space::Other),
    }
}

/// Read an ink name from a name or string.
fn ink_name(document: &Document, value: &Object) -> anyhow::Result<String> {
    match resolve(document, value)? {
        Object::Name(name) => Ok(name),
        Object::String(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
        other => bail!("ink name is not a name or string: {other:?}"),
    }
}

/// Split a content stream into operands/operators, skipping strings, comments
/// and inline-image data.
fn tokens(content: &[u8]) -> Vec<Token> {
    let mut out = Vec::new();
    let mut index = 0;
    while index < content.len() {
        let byte = content[index];
        match byte {
            b'%' => {
                while index < content.len() && content[index] != b'\n' {
                    index += 1;
                }
            }
            b'(' => {
                index += 1;
                let mut depth = 1;
                while index < content.len() && depth > 0 {
                    match content[index] {
                        b'\\' => index += 1,
                        b'(' => depth += 1,
                        b')' => depth -= 1,
                        _ => {}
                    }
                    index += 1;
                }
                out.push(Token::String);
            }
            b'<' => {
                if content.get(index + 1) == Some(&b'<') {
                    index += 2;
                    out.push(Token::Operator("<<".to_owned()));
                } else {
                    while index < content.len() && content[index] != b'>' {
                        index += 1;
                    }
                    index += 1;
                    out.push(Token::String);
                }
            }
            b'[' | b']' => {
                out.push(Token::Delimiter(byte as char));
                index += 1;
            }
            b'/' => {
                let start = index + 1;
                index += 1;
                while index < content.len() && !is_separator(content[index]) {
                    index += 1;
                }
                out.push(Token::Name(
                    String::from_utf8_lossy(&content[start..index]).into_owned(),
                ));
            }
            b'B' if content.get(index..index + 2) == Some(b"BI") => {
                // Inline image: skip to the EI operator.
                index += 2;
                while index + 1 < content.len() {
                    if content[index].is_ascii_whitespace()
                        && content[index + 1] == b'E'
                        && content.get(index + 2) == Some(&b'I')
                        && content.get(index + 3).is_none_or(u8::is_ascii_whitespace)
                    {
                        index += 3;
                        break;
                    }
                    index += 1;
                }
            }
            _ if is_separator(byte) => index += 1,
            _ => {
                let start = index;
                while index < content.len() && !is_separator(content[index]) {
                    index += 1;
                }
                let token = String::from_utf8_lossy(&content[start..index]).into_owned();
                if token.chars().next().is_some_and(|ch| ch.is_ascii_digit())
                    || token.starts_with('-')
                    || token.starts_with('+')
                    || token.starts_with('.')
                {
                    out.push(Token::Number);
                } else {
                    out.push(Token::Operator(token));
                }
            }
        }
    }
    out
}

/// Whether a byte separates tokens.
fn is_separator(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || matches!(
            byte,
            b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
        )
}

/// One content-stream token.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    /// A numeric operand.
    Number,
    /// A literal or hexadecimal string.
    String,
    /// A name operand (without the slash).
    Name(String),
    /// An array delimiter.
    Delimiter(char),
    /// An operator (or an unrecognized bare token).
    Operator(String),
}

/// Per-plate usage accumulated across pages.
#[derive(Default)]
struct Usage {
    operations: usize,
    overprint: usize,
    pages: BTreeSet<usize>,
}

/// Record one paint operation on one plate.
fn record(name: &str, page: usize, overprint: bool, usage: &mut BTreeMap<String, Usage>) {
    let entry = usage.entry(name.to_owned()).or_default();
    entry.operations += 1;
    if overprint {
        entry.overprint += 1;
    }
    entry.pages.insert(page);
}

/// Add one paint operation for `space`.
fn paint(
    space: &Space,
    page: usize,
    overprint: bool,
    usage: &mut BTreeMap<String, Usage>,
    rgb: &mut usize,
    gray: &mut usize,
) {
    match space {
        Space::Cmyk => {
            for ink in ["Cyan", "Magenta", "Yellow", "Black"] {
                record(ink, page, overprint, usage);
            }
        }
        Space::Gray => {
            *gray += 1;
            record("Black", page, overprint, usage);
        }
        Space::Rgb => *rgb += 1,
        Space::Separation(name) => record(name, page, overprint, usage),
        Space::DeviceN(names) => {
            for name in names {
                record(name, page, overprint, usage);
            }
        }
        Space::Other => {}
    }
}

/// Resolve an operand name to a space.
fn named_space(spaces: &BTreeMap<String, Space>, name: &str) -> Space {
    if let Some(space) = spaces.get(name) {
        return space.clone();
    }
    match name {
        "DeviceGray" | "G" => Space::Gray,
        "DeviceRGB" | "RGB" => Space::Rgb,
        "DeviceCMYK" | "CMYK" => Space::Cmyk,
        _ => Space::Other,
    }
}

/// Scan one page's content streams.
#[allow(clippy::too_many_arguments)]
fn scan_page(
    document: &Document,
    page_index: usize,
    usage: &mut BTreeMap<String, Usage>,
    rgb: &mut usize,
    gray: &mut usize,
    alternates: &mut BTreeMap<String, String>,
    states: &mut BTreeMap<String, OverprintState>,
) -> anyhow::Result<()> {
    let page = document
        .page(page_index)
        .map_err(|error| anyhow::anyhow!("read page {}: {error}", page_index + 1))?;
    let resources = page_resources(document, page.id)?;
    let space_set = colour_spaces(document, resources.as_ref())?;
    let spaces = space_set.spaces;
    for (ink, alternate) in space_set.alternates {
        alternates.entry(ink).or_insert(alternate);
    }
    let page_states = overprint_states(document, resources.as_ref())?;
    for (name, state) in &page_states {
        states.entry(name.clone()).or_insert(*state);
    }
    let page_number = page_index + 1;
    for id in &page.contents {
        let Object::Stream { dictionary, data } = document
            .object(*id)
            .map_err(|error| anyhow::anyhow!("read content stream: {error}"))?
        else {
            continue;
        };
        let bytes = stream_bytes(&dictionary, &data)?;
        let tokens = tokens(&bytes);
        let mut fill = Space::Gray;
        let mut stroke = Space::Gray;
        let mut fill_overprint = false;
        let mut stroke_overprint = false;
        let mut stack: Vec<(Space, Space, bool, bool)> = Vec::new();
        let mut operands: Vec<&Token> = Vec::new();
        for token in &tokens {
            match token {
                Token::Number | Token::String | Token::Name(_) | Token::Delimiter(_) => {
                    operands.push(token);
                }
                Token::Operator(op) => {
                    match op.as_str() {
                        "q" => stack.push((
                            fill.clone(),
                            stroke.clone(),
                            fill_overprint,
                            stroke_overprint,
                        )),
                        "Q" => {
                            if let Some((fill_restored, stroke_restored, fill_op, stroke_op)) =
                                stack.pop()
                            {
                                fill = fill_restored;
                                stroke = stroke_restored;
                                fill_overprint = fill_op;
                                stroke_overprint = stroke_op;
                            }
                        }
                        "gs" => {
                            if let Some(Token::Name(name)) = operands
                                .iter()
                                .rev()
                                .find(|token| matches!(token, Token::Name(_)))
                                && let Some(state) = page_states.get(name)
                            {
                                fill_overprint = state.fill;
                                stroke_overprint = state.stroke;
                            }
                        }
                        "cs" | "CS" => {
                            if let Some(Token::Name(name)) = operands
                                .iter()
                                .rev()
                                .find(|token| matches!(token, Token::Name(_)))
                            {
                                let space = named_space(&spaces, name);
                                if op == "cs" {
                                    fill = space;
                                } else {
                                    stroke = space;
                                }
                            }
                        }
                        "g" | "G" => {
                            if op == "g" {
                                fill = Space::Gray;
                            } else {
                                stroke = Space::Gray;
                            }
                            paint(&Space::Gray, page_number, fill_overprint, usage, rgb, gray);
                        }
                        "rg" | "RG" => {
                            paint(&Space::Rgb, page_number, fill_overprint, usage, rgb, gray);
                        }
                        "k" | "K" => {
                            paint(&Space::Cmyk, page_number, fill_overprint, usage, rgb, gray);
                        }
                        "sc" | "scn" | "SC" | "SCN" => {
                            let (space, overprint) = if op.starts_with('s') {
                                (&fill, fill_overprint)
                            } else {
                                (&stroke, stroke_overprint)
                            };
                            if matches!(space, Space::Separation(_) | Space::DeviceN(_)) {
                                paint(space, page_number, overprint, usage, rgb, gray);
                            }
                        }
                        "sh" => {
                            paint(&Space::Rgb, page_number, fill_overprint, usage, rgb, gray);
                        }
                        _ => {}
                    }
                    operands.clear();
                }
            }
        }
    }
    Ok(())
}

/// Build the report for one document.
fn report(document: &Document, file: &Path) -> anyhow::Result<SeparationsReport> {
    let pages = document
        .pages()
        .map_err(|error| anyhow::anyhow!("read page tree: {error}"))?;
    let mut usage: BTreeMap<String, Usage> = BTreeMap::new();
    let mut alternates: BTreeMap<String, String> = BTreeMap::new();
    let mut states: BTreeMap<String, OverprintState> = BTreeMap::new();
    let mut rgb = 0;
    let mut gray = 0;
    for index in 0..pages.len() {
        scan_page(
            document,
            index,
            &mut usage,
            &mut rgb,
            &mut gray,
            &mut alternates,
            &mut states,
        )?;
    }
    let process_names = ["Cyan", "Magenta", "Yellow", "Black"];
    let inputs: Vec<SeparationInput> = if usage.is_empty() {
        Vec::new()
    } else {
        usage
            .keys()
            .map(|name| SeparationInput {
                name: name.clone(),
                process: process_names.contains(&name.as_str()),
            })
            .collect()
    };
    let plan = if inputs.is_empty() {
        None
    } else {
        Some(
            SeparationsPlan::build(&inputs)
                .map_err(|error| anyhow::anyhow!("separations plan: {error}"))?,
        )
    };
    let plates: Vec<PlateRow> = plan
        .as_ref()
        .map(|plan| {
            plan.separations
                .iter()
                .map(|separation| {
                    let entry = usage.get(&separation.name);
                    PlateRow {
                        id: separation.id,
                        name: separation.name.clone(),
                        process: separation.process,
                        operations: entry.map_or(0, |entry| entry.operations),
                        overprint_operations: entry.map_or(0, |entry| entry.overprint),
                        alternate: alternates.get(&separation.name).cloned(),
                        pages: entry
                            .map(|entry| entry.pages.iter().copied().collect())
                            .unwrap_or_default(),
                    }
                })
                .collect::<Vec<PlateRow>>()
        })
        .unwrap_or_default();
    let descriptor = plates_descriptor(&plates);
    let overprint_states: Vec<OverprintRow> = states
        .into_iter()
        .map(|(name, state)| OverprintRow {
            name,
            fill: state.fill,
            stroke: state.stroke,
            mode: state.mode,
        })
        .collect();
    Ok(SeparationsReport {
        file: file.display().to_string(),
        pages: pages.len(),
        plates,
        device_rgb_operations: rgb,
        device_gray_operations: gray,
        alternates,
        overprint_states,
        summary: plan.as_ref().map_or_else(
            || "separations: none".to_owned(),
            |plan| plan.summary.clone(),
        ),
        descriptor,
    })
}

/// A stable single-line descriptor of the plate table.
fn plates_descriptor(plates: &[PlateRow]) -> String {
    let mut out = String::from("separations-plan/v1");
    for plate in plates {
        out.push_str(&format!(
            "|{}:{}:{}:{}",
            plate.id,
            if plate.process { "process" } else { "spot" },
            plate.operations,
            plate.name.replace('|', "\\|")
        ));
    }
    out
}

/// `pdfxi separations`.
pub(crate) fn separations(file: &Path, plate: Option<&str>, json: bool) -> anyhow::Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))?;
    let report = report(&document, file)?;
    if let Some(query) = plate {
        let row = report
            .plates
            .iter()
            .find(|row| row.name.eq_ignore_ascii_case(query))
            .with_context(|| format!("no plate named {query:?}"))?;
        if json {
            println!("{}", serde_json::to_string_pretty(row)?);
            return Ok(());
        }
        println!(
            "plate: {}  process: {}  operations: {}  overprint: {}  alternate: {}  pages: {:?}",
            row.name,
            row.process,
            row.operations,
            row.overprint_operations,
            row.alternate.as_deref().unwrap_or("-"),
            row.pages
        );
        return Ok(());
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!(
        "file: {}  pages: {}  device-rgb-ops: {}  device-gray-ops: {}",
        report.file, report.pages, report.device_rgb_operations, report.device_gray_operations
    );
    println!("{}", report.summary);
    for row in &report.plates {
        println!(
            "  {}  {}  {}  {} op(s)  overprint {}  alternate {}  pages {:?}",
            row.id,
            if row.process { "process" } else { "spot" },
            row.name,
            row.operations,
            row.overprint_operations,
            row.alternate.as_deref().unwrap_or("-"),
            row.pages
        );
    }
    for state in &report.overprint_states {
        println!(
            "  overprint state {}  fill {}  stroke {}  mode {}",
            state.name, state.fill, state.stroke, state.mode
        );
    }
    println!("descriptor: {}", report.descriptor);
    Ok(())
}

/// `pdfxi ink`: the ink-manager view (alternates and overprint counts).
pub(crate) fn ink(file: &Path, json: bool) -> anyhow::Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))?;
    let report = report(&document, file)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!(
        "file: {}  pages: {}  inks: {}",
        report.file,
        report.pages,
        report.plates.len()
    );
    for row in &report.plates {
        println!(
            "  {}  {}  {} op(s)  overprint {}  alternate {}",
            row.name,
            if row.process { "process" } else { "spot" },
            row.operations,
            row.overprint_operations,
            row.alternate.as_deref().unwrap_or("-")
        );
    }
    for state in &report.overprint_states {
        println!(
            "  overprint state {}  fill {}  stroke {}  mode {}",
            state.name, state.fill, state.stroke, state.mode
        );
    }
    println!("descriptor: {}", report.descriptor);
    Ok(())
}
