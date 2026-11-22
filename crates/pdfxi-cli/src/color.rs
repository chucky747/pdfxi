//! `pdfxi color`: ICC profiles, output intents and analytic transforms.
//!
//! [`pdfxi_render::color`] owns a bounded ICC parser, a `/OutputIntents` model
//! and a backend-free conversion plan. This host exposes them over real files:
//! it validates an ICC profile on disk, reads a PDF's real output-intent
//! dictionaries (decoding the embedded `/DestOutputProfile` stream), and runs
//! the analytic matrix/TRC or gray-gamma transform on supplied components. Full
//! CMM rendering is reported as `needs-backend`, never approximated.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_core::filter::{Filter, FlateParams, Predictor, decode_stream};
use pdfxi_core::{Dictionary, Document, Object};
use pdfxi_render::color::{
    ConversionPlan, DeviceClass, DeviceColor, DeviceColorSpace, IccProfile, OutputIntentIssue,
    OutputIntentSet, Pcs, RawOutputIntent, RenderingIntent,
};
use serde::Serialize;

/// Stable facts about one ICC profile.
#[derive(Serialize)]
struct IccFacts {
    /// Declared profile size in bytes.
    size: u32,
    /// Version as `major.minor`.
    version: String,
    /// Preferred CMM signature.
    cmm: String,
    /// Device class label.
    device_class: String,
    /// Data colour space signature.
    colour_space: String,
    /// Profile connection space signature.
    pcs: String,
    /// Rendering intent label.
    rendering_intent: String,
    /// Number of tag-table entries.
    tags: usize,
    /// ASCII profile description, when present and valid.
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    /// Media white point, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    whitepoint: Option<[f32; 3]>,
    /// Analytic conversion plan label.
    plan: String,
}

/// Stable facts about one output intent.
#[derive(Serialize)]
struct IntentFacts {
    /// `/S` subtype name.
    subtype: String,
    /// `/OutputConditionIdentifier`.
    #[serde(skip_serializing_if = "Option::is_none")]
    output_condition_identifier: Option<String>,
    /// `/Info`.
    #[serde(skip_serializing_if = "Option::is_none")]
    info: Option<String>,
    /// `/RegistryName`.
    #[serde(skip_serializing_if = "Option::is_none")]
    registry_name: Option<String>,
    /// Embedded `/DestOutputProfile` byte length.
    #[serde(skip_serializing_if = "Option::is_none")]
    profile_bytes: Option<usize>,
    /// Parsed profile facts, when the embedded bytes are valid.
    #[serde(skip_serializing_if = "Option::is_none")]
    profile: Option<IccFacts>,
}

/// JSON shape of `pdfxi color icc`.
#[derive(Serialize)]
struct IccReport {
    /// Profile file.
    file: String,
    /// Parsed facts.
    profile: IccFacts,
}

/// JSON shape of `pdfxi color intents`.
#[derive(Serialize)]
struct IntentsReport {
    /// Inspected PDF.
    file: String,
    /// Number of `/OutputIntents` entries.
    intents: usize,
    /// Detected set problems.
    issues: Vec<String>,
    /// Whether exactly one usable intent is present.
    usable: bool,
    /// Intent rows.
    rows: Vec<IntentFacts>,
}

/// JSON shape of `pdfxi color convert`.
#[derive(Serialize)]
struct ConvertReport {
    /// Profile file.
    profile: String,
    /// Transform label.
    plan: String,
    /// Source colour space label.
    source_space: String,
    /// Source components.
    input: Vec<f32>,
    /// Output colour space label.
    output_space: String,
    /// Converted components.
    output: Vec<f32>,
}

/// Label one device class.
fn device_class_label(class: DeviceClass) -> String {
    match class {
        DeviceClass::Input => "input".to_owned(),
        DeviceClass::Display => "display".to_owned(),
        DeviceClass::Output => "output".to_owned(),
        DeviceClass::Link => "link".to_owned(),
        DeviceClass::ColorSpace => "colorspace".to_owned(),
        DeviceClass::Abstract => "abstract".to_owned(),
        DeviceClass::Named => "named".to_owned(),
        DeviceClass::Unknown(sig) => format!("unknown:{}", String::from_utf8_lossy(&sig)),
    }
}

/// Label the profile connection space.
fn pcs_label(pcs: Pcs) -> String {
    match pcs {
        Pcs::Xyz => "XYZ".to_owned(),
        Pcs::Lab => "Lab".to_owned(),
        Pcs::Unknown(sig) => String::from_utf8_lossy(&sig).trim().to_owned(),
    }
}

/// Label the header rendering intent.
fn intent_label(intent: RenderingIntent) -> String {
    match intent {
        RenderingIntent::Perceptual => "perceptual".to_owned(),
        RenderingIntent::RelativeColorimetric => "relative-colorimetric".to_owned(),
        RenderingIntent::Saturation => "saturation".to_owned(),
        RenderingIntent::AbsoluteColorimetric => "absolute-colorimetric".to_owned(),
        RenderingIntent::Unknown(value) => format!("unknown:{value}"),
    }
}

/// Label a conversion plan.
fn plan_label(plan: &ConversionPlan) -> String {
    match plan {
        ConversionPlan::Identity => "identity".to_owned(),
        ConversionPlan::RgbMatrixTrc(_) => "rgb-matrix-trc".to_owned(),
        ConversionPlan::GrayGamma(gamma) => format!("gray-gamma:{gamma}"),
        ConversionPlan::NeedsBackend { reason } => format!("needs-backend: {reason}"),
    }
}

/// Build the stable facts for one parsed profile.
fn icc_facts(profile: &IccProfile) -> IccFacts {
    let header = profile.header();
    let description = profile.description().ok();
    let whitepoint = profile.whitepoint().ok().map(|xyz| [xyz.x, xyz.y, xyz.z]);
    IccFacts {
        size: header.size,
        version: format!("{}.{}", header.version_major(), header.version_minor()),
        cmm: header.cmm_name(),
        device_class: device_class_label(header.device_class),
        colour_space: header.data_color_space.name(),
        pcs: pcs_label(header.pcs),
        rendering_intent: intent_label(header.rendering_intent),
        tags: profile.tags().len(),
        description,
        whitepoint,
        plan: plan_label(&ConversionPlan::from_profile(profile)),
    }
}

/// Label a colour-space issue.
fn issue_label(issue: &OutputIntentIssue) -> String {
    match issue {
        OutputIntentIssue::Missing => "missing".to_owned(),
        OutputIntentIssue::Multiple(count) => format!("multiple:{count}"),
        OutputIntentIssue::UnsupportedSubtype(name) => format!("unsupported-subtype:{name}"),
        OutputIntentIssue::MissingDestOutputProfile => "missing-dest-output-profile".to_owned(),
    }
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

/// Decode a text field from a PDF string or name.
fn text_field(document: &Document, object: &Object) -> anyhow::Result<Option<String>> {
    match resolve(document, object)? {
        Object::String(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
        Object::Name(name) => Ok(Some(name)),
        _ => Ok(None),
    }
}

/// Read the catalog dictionary.
fn catalog(document: &Document) -> anyhow::Result<Dictionary> {
    let root = document
        .root()
        .map_err(|error| anyhow::anyhow!("read catalog: {error}"))?;
    match document
        .object(root)
        .map_err(|error| anyhow::anyhow!("read catalog object: {error}"))?
    {
        Object::Dictionary(dict) => Ok(dict),
        other => bail!("catalog is not a dictionary: {other:?}"),
    }
}

/// Map a PDF filter name to the core filter enum.
fn filter_named(name: &str, params: FlateParams) -> Filter {
    match name {
        "FlateDecode" | "Fl" => Filter::FlateDecode(params),
        "LZWDecode" | "LZW" => Filter::LzwDecode,
        "DCTDecode" | "DCT" => Filter::DctDecode,
        "JPXDecode" => Filter::JpxDecode,
        "CCITTFaxDecode" | "CCF" => Filter::CcittFaxDecode,
        "JBIG2Decode" => Filter::Jbig2Decode,
        "RunLengthDecode" | "RL" => Filter::RunLengthDecode,
        "ASCII85Decode" | "A85" => Filter::Ascii85Decode,
        "ASCIIHexDecode" | "AHx" => Filter::AsciiHexDecode,
        "Crypt" => Filter::Crypt,
        other => Filter::Unknown(other.to_owned()),
    }
}

/// Read the `/Filter` chain and its `/DecodeParms` from a stream dictionary.
fn stream_filters(dictionary: &Dictionary) -> anyhow::Result<Vec<Filter>> {
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
    let mut params = FlateParams::default();
    if let Some(Object::Dictionary(parms)) = dictionary.get("DecodeParms") {
        let entry = |key: &str| parms.get(key).and_then(Object::as_i64);
        if let Some(predictor) = entry("Predictor") {
            params.predictor = Predictor::from_pdf(predictor)
                .ok_or_else(|| anyhow::anyhow!("unsupported /Predictor value {predictor}"))?;
        }
        if let Some(colors) = entry("Colors") {
            params.colors = u32::try_from(colors).unwrap_or(1);
        }
        if let Some(bits) = entry("BitsPerComponent") {
            params.bits_per_component = u32::try_from(bits).unwrap_or(8);
        }
        if let Some(columns) = entry("Columns") {
            params.columns = u32::try_from(columns).unwrap_or(1);
        }
    }
    Ok(names
        .iter()
        .map(|name| filter_named(name, params))
        .collect())
}

/// Decode one `/DestOutputProfile` stream.
fn profile_stream(document: &Document, object: &Object) -> anyhow::Result<Vec<u8>> {
    match resolve(document, object)? {
        Object::Stream { dictionary, data } => {
            let filters = stream_filters(&dictionary)?;
            decode_stream(&data, &filters)
                .map_err(|error| anyhow::anyhow!("decode profile stream: {error}"))
        }
        other => bail!("destination profile is not a stream: {other:?}"),
    }
}

/// Read the raw output intents of a document.
fn raw_intents(document: &Document) -> anyhow::Result<Vec<RawOutputIntent>> {
    let catalog = catalog(document)?;
    let Some(entries) = catalog.get("OutputIntents") else {
        return Ok(Vec::new());
    };
    let resolved = resolve(document, entries)?;
    let Object::Array(entries) = resolved else {
        bail!("/OutputIntents is not an array");
    };
    let mut raws = Vec::with_capacity(entries.len());
    for entry in &entries {
        let resolved = resolve(document, entry)?;
        let Some(dict) = resolved.as_dictionary() else {
            bail!("an /OutputIntents entry is not a dictionary");
        };
        let subtype = match dict.get("S") {
            Some(value) => text_field(document, value)?.unwrap_or_default(),
            None => String::new(),
        };
        let output_condition_identifier = match dict.get("OutputConditionIdentifier") {
            Some(value) => text_field(document, value)?,
            None => None,
        };
        let info = match dict.get("Info") {
            Some(value) => text_field(document, value)?,
            None => None,
        };
        let registry_name = match dict.get("RegistryName") {
            Some(value) => text_field(document, value)?,
            None => None,
        };
        let dest_output_profile = match dict.get("DestOutputProfile") {
            Some(value) => Some(profile_stream(document, value)?),
            None => None,
        };
        raws.push(RawOutputIntent {
            subtype,
            output_condition_identifier,
            info,
            registry_name,
            dest_output_profile,
        });
    }
    Ok(raws)
}

/// Print one ICC report.
fn print_icc(report: &IccReport) {
    let facts = &report.profile;
    println!("file: {}", report.file);
    println!(
        "size: {}  version: {}  cmm: {}",
        facts.size, facts.version, facts.cmm
    );
    println!(
        "class: {}  space: {}  pcs: {}  intent: {}",
        facts.device_class, facts.colour_space, facts.pcs, facts.rendering_intent
    );
    println!("tags: {}  plan: {}", facts.tags, facts.plan);
    if let Some(description) = &facts.description {
        println!("description: {description}");
    }
    if let Some(whitepoint) = facts.whitepoint {
        println!(
            "whitepoint: {:.6} {:.6} {:.6}",
            whitepoint[0], whitepoint[1], whitepoint[2]
        );
    }
}

/// `pdfxi color icc`.
pub(crate) fn icc(file: &Path, json: bool) -> anyhow::Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let profile = IccProfile::parse(&bytes)
        .map_err(|error| anyhow::anyhow!("parse ICC profile {}: {error}", file.display()))?;
    let report = IccReport {
        file: file.display().to_string(),
        profile: icc_facts(&profile),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_icc(&report);
    }
    Ok(())
}

/// `pdfxi color intents`.
pub(crate) fn intents(file: &Path, json: bool) -> anyhow::Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))?;
    let raws = raw_intents(&document)?;
    let set = OutputIntentSet::from_entries(&raws);
    let issues: Vec<String> = set.issues().iter().map(issue_label).collect();
    let rows: Vec<IntentFacts> = set
        .intents()
        .iter()
        .map(|intent| {
            let profile = intent.parse_dest_profile().ok().flatten();
            IntentFacts {
                subtype: intent.subtype.name(),
                output_condition_identifier: intent.output_condition_identifier.clone(),
                info: intent.info.clone(),
                registry_name: intent.registry_name.clone(),
                profile_bytes: intent.dest_output_profile.as_ref().map(Vec::len),
                profile: profile.as_ref().map(icc_facts),
            }
        })
        .collect();
    let report = IntentsReport {
        file: file.display().to_string(),
        intents: set.len(),
        issues,
        usable: set.validate().is_ok(),
        rows,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("file: {}", report.file);
    println!(
        "intents: {}  usable: {}  issues: {}",
        report.intents,
        report.usable,
        if report.issues.is_empty() {
            "none".to_owned()
        } else {
            report.issues.join(", ")
        }
    );
    for row in &report.rows {
        println!(
            "intent: {}  identifier: {}  profile: {}",
            row.subtype,
            row.output_condition_identifier.as_deref().unwrap_or("-"),
            row.profile_bytes
                .map_or_else(|| "none".to_owned(), |len| format!("{len} bytes"))
        );
        if let Some(facts) = &row.profile {
            println!(
                "  space: {}  class: {}  pcs: {}  plan: {}",
                facts.colour_space, facts.device_class, facts.pcs, facts.plan
            );
        }
        if let Some(info) = &row.info {
            println!("  info: {info}");
        }
        if let Some(registry) = &row.registry_name {
            println!("  registry: {registry}");
        }
    }
    Ok(())
}

/// Parse `--space`.
fn parse_space(text: &str) -> anyhow::Result<DeviceColorSpace> {
    match text.trim().to_ascii_lowercase().as_str() {
        "gray" | "grey" => Ok(DeviceColorSpace::DeviceGray),
        "rgb" => Ok(DeviceColorSpace::DeviceRgb),
        "cmyk" => Ok(DeviceColorSpace::DeviceCmyk),
        other => bail!("unknown space {other:?}; expected gray, rgb or cmyk"),
    }
}

/// Parse comma-separated normalised components.
fn parse_values(text: &str) -> anyhow::Result<Vec<f32>> {
    let mut values = Vec::new();
    for piece in text.split(',') {
        let value: f32 = piece
            .trim()
            .parse()
            .with_context(|| format!("component {piece:?} is not a number"))?;
        if !(0.0..=1.0).contains(&value) {
            bail!("component {value} is outside 0..=1");
        }
        values.push(value);
    }
    if values.is_empty() {
        bail!("--values must contain at least one component");
    }
    Ok(values)
}

/// `pdfxi color convert`.
pub(crate) fn convert(file: &Path, space: &str, values: &str, json: bool) -> anyhow::Result<()> {
    let space = parse_space(space)?;
    let input = parse_values(values)?;
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let profile = IccProfile::parse(&bytes)
        .map_err(|error| anyhow::anyhow!("parse ICC profile {}: {error}", file.display()))?;
    let plan = ConversionPlan::from_profile(&profile);
    let colour = DeviceColor::new(space.clone(), input.clone())
        .map_err(|error| anyhow::anyhow!("source colour: {error}"))?;
    let output = colour
        .convert(&plan)
        .map_err(|error| anyhow::anyhow!("convert through {}: {error}", file.display()))?;
    let report = ConvertReport {
        profile: file.display().to_string(),
        plan: plan_label(&plan),
        source_space: format!("{space:?}"),
        input,
        output_space: match plan {
            ConversionPlan::Identity => "same-as-source".to_owned(),
            ConversionPlan::RgbMatrixTrc(_) => "PCS-XYZ".to_owned(),
            ConversionPlan::GrayGamma(_) => "linear-luminance".to_owned(),
            ConversionPlan::NeedsBackend { .. } => "backend-required".to_owned(),
        },
        output,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("profile: {}", report.profile);
    println!("plan: {}", report.plan);
    println!("source: {} {:?}", report.source_space, report.input);
    println!("output: {} {:?}", report.output_space, report.output);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spaces_and_components_are_validated() {
        assert_eq!(
            parse_space("RGB").expect("rgb"),
            DeviceColorSpace::DeviceRgb
        );
        assert_eq!(
            parse_space("grey").expect("grey"),
            DeviceColorSpace::DeviceGray
        );
        assert!(parse_space("hsl").is_err());
        assert_eq!(
            parse_values("0, 0.5 ,1").expect("values"),
            vec![0.0, 0.5, 1.0]
        );
        assert!(parse_values("1.2").is_err());
        assert!(parse_values("-0.1").is_err());
        assert!(parse_values("abc").is_err());
    }

    #[test]
    fn filter_names_cover_the_core_variants() {
        let params = FlateParams::default();
        assert_eq!(
            filter_named("FlateDecode", params),
            Filter::FlateDecode(params)
        );
        assert_eq!(filter_named("AHx", params), Filter::AsciiHexDecode);
        assert_eq!(
            filter_named("Custom", params),
            Filter::Unknown("Custom".to_owned())
        );
    }
}
