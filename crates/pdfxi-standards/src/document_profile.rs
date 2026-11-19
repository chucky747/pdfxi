//! Real preflight profiles that check a [`PreflightInput`] snapshot.
//!
//! The checks here go beyond the shallow header check of [`crate::HeaderProfile`]:
//! they validate the version range, encryption, output intents, XMP, font
//! embedding, colour spaces, image compression, transparency, page boxes, and
//! (for PDF/UA-1) tagging metadata. All rules are declarative in [`Rules`] and
//! executed by one shared engine, so each profile emits the same set of stable
//! finding codes and the same canonical order.
//!
//! A check only reports facts that are decidable from the snapshot. Missing
//! optional data (for example no per-page details) never causes a panic; at
//! most it yields a warning that manual inspection is needed.

use crate::Severity;
use crate::Standard;
use crate::finding::PreflightFinding;
use crate::input::{ColourSpace, ImageFilter, PdfVersion, PreflightInput};
use crate::profile::PreflightProfile;

/// Declarative rules for one standard's preflight profile.
#[derive(Debug, Clone, Copy)]
struct Rules {
    /// Standard reported in the profile and verdict.
    standard: Standard,
    /// Stable code prefix, for example `PDFA1B`.
    prefix: &'static str,
    /// Lowest acceptable PDF version.
    min_version: PdfVersion,
    /// Required `/OutputIntent` subtype, or `None` when not required.
    required_output_subtype: Option<&'static str>,
    /// Whether an XMP metadata stream is mandatory.
    xmp_required: bool,
    /// Whether transparency is forbidden.
    forbid_transparency: bool,
    /// Image compression filters that are forbidden.
    forbidden_image_filters: &'static [ImageFilter],
    /// Colour spaces that are forbidden.
    forbidden_colour_spaces: &'static [ColourSpace],
    /// Whether TrimBox and BleedBox are mandatory on every page.
    require_print_boxes: bool,
    /// Whether accessibility (tagging) metadata is checked.
    check_accessibility: bool,
}

const PDFA1B_RULES: Rules = Rules {
    standard: Standard::PdfA1b,
    prefix: "PDFA1B",
    min_version: PdfVersion::new(1, 4),
    required_output_subtype: Some("GTS_PDFA1"),
    xmp_required: true,
    forbid_transparency: true,
    forbidden_image_filters: &[ImageFilter::JpxDecode],
    forbidden_colour_spaces: &[],
    require_print_boxes: false,
    check_accessibility: false,
};

const PDFA2B_RULES: Rules = Rules {
    standard: Standard::PdfA2b,
    prefix: "PDFA2B",
    min_version: PdfVersion::new(1, 7),
    required_output_subtype: Some("GTS_PDFA1"),
    xmp_required: true,
    forbid_transparency: false,
    forbidden_image_filters: &[],
    forbidden_colour_spaces: &[],
    require_print_boxes: false,
    check_accessibility: false,
};

const PDFX1A_RULES: Rules = Rules {
    standard: Standard::PdfX1a,
    prefix: "PDFX1A",
    min_version: PdfVersion::new(1, 3),
    required_output_subtype: Some("GTS_PDFX"),
    xmp_required: false,
    forbid_transparency: true,
    forbidden_image_filters: &[ImageFilter::JpxDecode, ImageFilter::Jbig2Decode],
    forbidden_colour_spaces: &[
        ColourSpace::DeviceRgb,
        ColourSpace::CalRgb,
        ColourSpace::CalGray,
        ColourSpace::Lab,
    ],
    require_print_boxes: true,
    check_accessibility: false,
};

const PDFX4_RULES: Rules = Rules {
    standard: Standard::PdfX4,
    prefix: "PDFX4",
    min_version: PdfVersion::new(1, 6),
    required_output_subtype: Some("GTS_PDFX"),
    xmp_required: true,
    forbid_transparency: false,
    forbidden_image_filters: &[],
    forbidden_colour_spaces: &[],
    require_print_boxes: true,
    check_accessibility: false,
};

const PDFUA1_RULES: Rules = Rules {
    standard: Standard::PdfUa1,
    prefix: "PDFUA1",
    min_version: PdfVersion::new(1, 7),
    required_output_subtype: None,
    xmp_required: false,
    forbid_transparency: false,
    forbidden_image_filters: &[],
    forbidden_colour_spaces: &[],
    require_print_boxes: false,
    check_accessibility: true,
};

/// Appends a finding with a stable `PREFIX.CHECK` code.
fn push(
    out: &mut Vec<PreflightFinding>,
    rules: &Rules,
    check: &str,
    severity: Severity,
    message: String,
    location: Option<String>,
) {
    let finding = PreflightFinding::new(severity, format!("{}.{}", rules.prefix, check), message);
    let finding = match location {
        Some(location) => finding.with_location(location),
        None => finding,
    };
    out.push(finding);
}

/// Runs every check enabled by `rules` against `input`.
fn run_checks(input: &PreflightInput, rules: &Rules) -> Vec<PreflightFinding> {
    let mut out = Vec::new();

    if input.version < rules.min_version {
        push(
            &mut out,
            rules,
            "VERSION_TOO_LOW",
            Severity::Error,
            format!(
                "PDF version {} is below the minimum {} required by {}",
                input.version, rules.min_version, rules.standard
            ),
            Some("header".to_owned()),
        );
    }

    if input.is_encrypted {
        push(
            &mut out,
            rules,
            "ENCRYPTED",
            Severity::Error,
            format!("{} does not permit encryption", rules.standard),
            Some("Encrypt dictionary".to_owned()),
        );
    }

    if let Some(subtype) = rules.required_output_subtype {
        match input.output_intent_of(subtype) {
            Some(intent) => {
                if !intent.has_dest_output_profile {
                    push(
                        &mut out,
                        rules,
                        "OUTPUT_INTENT_NO_PROFILE",
                        Severity::Error,
                        format!(
                            "OutputIntent subtype {subtype} is missing a /DestOutputProfile ICC stream"
                        ),
                        Some("Catalog /OutputIntents".to_owned()),
                    );
                }
            }
            None => {
                let (check, message) = if input.catalog.output_intents.is_empty() {
                    (
                        "NO_OUTPUT_INTENT",
                        format!(
                            "{} requires an OutputIntent with subtype {subtype}",
                            rules.standard
                        ),
                    )
                } else {
                    (
                        "OUTPUT_INTENT_SUBTYPE",
                        format!(
                            "no OutputIntent declares the required subtype {subtype}; \
                             add one to the catalog"
                        ),
                    )
                };
                push(
                    &mut out,
                    rules,
                    check,
                    Severity::Error,
                    message,
                    Some("Catalog /OutputIntents".to_owned()),
                );
            }
        }
    }

    if rules.xmp_required && !input.catalog.has_xmp_metadata {
        push(
            &mut out,
            rules,
            "NO_XMP_METADATA",
            Severity::Error,
            format!(
                "{} requires an XMP /Metadata stream in the catalog",
                rules.standard
            ),
            Some("Catalog /Metadata".to_owned()),
        );
    }

    for font in &input.fonts {
        if !font.embedded {
            push(
                &mut out,
                rules,
                "FONT_NOT_EMBEDDED",
                Severity::Error,
                format!(
                    "font {} ({}) is not embedded; all fonts must be embedded",
                    font.name, font.font_type
                ),
                Some(format!("font {}", font.name)),
            );
        }
    }

    for space in &input.colour_spaces {
        if rules.forbidden_colour_spaces.contains(space) {
            push(
                &mut out,
                rules,
                "DISALLOWED_COLOUR_SPACE",
                Severity::Error,
                format!(
                    "colour space {space} is not permitted by {}; use a conforming colour space",
                    rules.standard
                ),
                Some("colour resources".to_owned()),
            );
        }
    }

    for image in &input.images {
        if rules.forbidden_image_filters.contains(&image.filter) {
            push(
                &mut out,
                rules,
                "IMAGE_FILTER_NOT_ALLOWED",
                Severity::Error,
                format!(
                    "image compression {} is not permitted by {}",
                    image.filter, rules.standard
                ),
                Some(format!("image {}", image.name)),
            );
        }
    }

    if rules.forbid_transparency && input.transparency.uses_transparency {
        let mut locations = input.transparency.locations.clone();
        locations.sort();
        locations.dedup();
        if locations.is_empty() {
            push(
                &mut out,
                rules,
                "TRANSPARENCY_FORBIDDEN",
                Severity::Error,
                format!("{} does not permit transparency", rules.standard),
                None,
            );
        } else {
            for location in locations {
                push(
                    &mut out,
                    rules,
                    "TRANSPARENCY_FORBIDDEN",
                    Severity::Error,
                    format!("{} does not permit transparency", rules.standard),
                    Some(location),
                );
            }
        }
    }

    if input.page_count > 0 && input.pages.is_empty() {
        push(
            &mut out,
            rules,
            "PAGE_DATA_UNAVAILABLE",
            Severity::Warning,
            format!(
                "{} pages were reported but no per-page box data was supplied; \
                 page geometry could not be checked",
                input.page_count
            ),
            Some("page tree".to_owned()),
        );
    }

    for page in &input.pages {
        let location = Some(format!("page {}", page.number));
        if !page.boxes.has_media_box {
            push(
                &mut out,
                rules,
                "PAGE_NO_MEDIABOX",
                Severity::Error,
                "page has no /MediaBox".to_owned(),
                location.clone(),
            );
        }
        if rules.require_print_boxes {
            if !page.boxes.has_trim_box {
                push(
                    &mut out,
                    rules,
                    "PAGE_NO_TRIMBOX",
                    Severity::Error,
                    format!("{} requires a /TrimBox on every page", rules.standard),
                    location.clone(),
                );
            }
            if !page.boxes.has_bleed_box {
                push(
                    &mut out,
                    rules,
                    "PAGE_NO_BLEEDBOX",
                    Severity::Error,
                    format!("{} requires a /BleedBox on every page", rules.standard),
                    location,
                );
            }
        }
    }

    if rules.check_accessibility {
        if !input.catalog.has_mark_info || !input.catalog.is_marked {
            push(
                &mut out,
                rules,
                "NOT_MARKED",
                Severity::Error,
                "tagged PDF requires /MarkInfo with /Marked true".to_owned(),
                Some("Catalog /MarkInfo".to_owned()),
            );
        }
        if !input.catalog.has_struct_tree_root {
            push(
                &mut out,
                rules,
                "NO_STRUCT_TREE_ROOT",
                Severity::Error,
                "tagged PDF requires a /StructTreeRoot".to_owned(),
                Some("Catalog /StructTreeRoot".to_owned()),
            );
        }
        if !input.catalog.has_lang {
            push(
                &mut out,
                rules,
                "NO_LANG",
                Severity::Error,
                "the document language (/Lang) is required".to_owned(),
                Some("Catalog /Lang".to_owned()),
            );
        }
        if input.catalog.has_acro_form {
            push(
                &mut out,
                rules,
                "FORM_FIELDS_MANUAL_CHECK",
                Severity::Info,
                "form fields must have accessible names; verify each field manually".to_owned(),
                Some("Catalog /AcroForm".to_owned()),
            );
        }
    }

    out
}

/// Preflight profile for ISO 19005-1 Level B (PDF/A-1b).
///
/// Requires PDF 1.4+, a `GTS_PDFA1` output intent with an ICC profile, XMP,
/// embedded fonts, and no transparency or JPEG 2000 images.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PdfA1bProfile;

impl PdfA1bProfile {
    /// Creates a PDF/A-1b profile.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for PdfA1bProfile {
    fn default() -> Self {
        Self::new()
    }
}

impl PreflightProfile for PdfA1bProfile {
    type Input = PreflightInput;

    fn standard(&self) -> Standard {
        Standard::PdfA1b
    }

    fn check(&self, input: &PreflightInput) -> Vec<PreflightFinding> {
        run_checks(input, &PDFA1B_RULES)
    }
}

/// Preflight profile for ISO 19005-2 Level B (PDF/A-2b).
///
/// Requires PDF 1.7+, a `GTS_PDFA1` output intent with an ICC profile, XMP, and
/// embedded fonts. Transparency and JPEG 2000 images are permitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PdfA2bProfile;

impl PdfA2bProfile {
    /// Creates a PDF/A-2b profile.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for PdfA2bProfile {
    fn default() -> Self {
        Self::new()
    }
}

impl PreflightProfile for PdfA2bProfile {
    type Input = PreflightInput;

    fn standard(&self) -> Standard {
        Standard::PdfA2b
    }

    fn check(&self, input: &PreflightInput) -> Vec<PreflightFinding> {
        run_checks(input, &PDFA2B_RULES)
    }
}

/// Preflight profile for ISO 15930-1 (PDF/X-1a).
///
/// Requires PDF 1.3+, a `GTS_PDFX` output intent, embedded fonts, printable
/// page boxes, CMYK/spot colour, and no transparency, JPEG 2000, or JBIG2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PdfX1aProfile;

impl PdfX1aProfile {
    /// Creates a PDF/X-1a profile.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for PdfX1aProfile {
    fn default() -> Self {
        Self::new()
    }
}

impl PreflightProfile for PdfX1aProfile {
    type Input = PreflightInput;

    fn standard(&self) -> Standard {
        Standard::PdfX1a
    }

    fn check(&self, input: &PreflightInput) -> Vec<PreflightFinding> {
        run_checks(input, &PDFX1A_RULES)
    }
}

/// Preflight profile for ISO 15930-7 (PDF/X-4).
///
/// Requires PDF 1.6+, a `GTS_PDFX` output intent with an ICC profile, XMP,
/// embedded fonts, and printable page boxes. Transparency and modern image
/// compression are permitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PdfX4Profile;

impl PdfX4Profile {
    /// Creates a PDF/X-4 profile.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for PdfX4Profile {
    fn default() -> Self {
        Self::new()
    }
}

impl PreflightProfile for PdfX4Profile {
    type Input = PreflightInput;

    fn standard(&self) -> Standard {
        Standard::PdfX4
    }

    fn check(&self, input: &PreflightInput) -> Vec<PreflightFinding> {
        run_checks(input, &PDFX4_RULES)
    }
}

/// Preflight profile for ISO 14289-1 (PDF/UA-1).
///
/// Checks the cheap, decidable accessibility facts: PDF 1.7+, no encryption,
/// `/MarkInfo` with `/Marked true`, a `/StructTreeRoot`, and `/Lang`. Form
/// fields, when present, produce an informational manual-check finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PdfUa1Profile;

impl PdfUa1Profile {
    /// Creates a PDF/UA-1 profile.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for PdfUa1Profile {
    fn default() -> Self {
        Self::new()
    }
}

impl PreflightProfile for PdfUa1Profile {
    type Input = PreflightInput;

    fn standard(&self) -> Standard {
        Standard::PdfUa1
    }

    fn check(&self, input: &PreflightInput) -> Vec<PreflightFinding> {
        run_checks(input, &PDFUA1_RULES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{
        CatalogInfo, FontInfo, FontType, ImageInfo, OutputIntent, PageBoxes, PageInfo,
        TransparencyUsage,
    };

    fn profiles() -> [(&'static str, Standard); 5] {
        [
            ("PDFA1B", Standard::PdfA1b),
            ("PDFA2B", Standard::PdfA2b),
            ("PDFX1A", Standard::PdfX1a),
            ("PDFX4", Standard::PdfX4),
            ("PDFUA1", Standard::PdfUa1),
        ]
    }

    fn check_with_prefix(prefix: &str, input: &PreflightInput) -> Vec<PreflightFinding> {
        match prefix {
            "PDFA1B" => PdfA1bProfile::new().check(input),
            "PDFA2B" => PdfA2bProfile::new().check(input),
            "PDFX1A" => PdfX1aProfile::new().check(input),
            "PDFX4" => PdfX4Profile::new().check(input),
            "PDFUA1" => PdfUa1Profile::new().check(input),
            other => panic!("unknown profile prefix {other}"),
        }
    }

    fn report_with_prefix(prefix: &str, input: &PreflightInput) -> crate::PreflightReport {
        match prefix {
            "PDFA1B" => PdfA1bProfile::new().report(input),
            "PDFA2B" => PdfA2bProfile::new().report(input),
            "PDFX1A" => PdfX1aProfile::new().report(input),
            "PDFX4" => PdfX4Profile::new().report(input),
            "PDFUA1" => PdfUa1Profile::new().report(input),
            other => panic!("unknown profile prefix {other}"),
        }
    }

    fn conforming_input(standard: Standard) -> PreflightInput {
        let (version, subtype) = match standard {
            Standard::PdfA1b => (PdfVersion::new(1, 4), "GTS_PDFA1"),
            Standard::PdfA2b => (PdfVersion::new(1, 7), "GTS_PDFA1"),
            Standard::PdfX1a => (PdfVersion::new(1, 3), "GTS_PDFX"),
            Standard::PdfX4 => (PdfVersion::new(1, 6), "GTS_PDFX"),
            Standard::PdfE1 | Standard::PdfUa1 => (PdfVersion::new(1, 7), "GTS_PDFA1"),
        };
        let mut catalog = CatalogInfo {
            has_xmp_metadata: true,
            has_mark_info: true,
            is_marked: true,
            has_lang: true,
            has_struct_tree_root: true,
            ..Default::default()
        };
        if standard != Standard::PdfUa1 {
            catalog
                .output_intents
                .push(OutputIntent::new(subtype, true));
        }
        PreflightInput {
            version,
            is_encrypted: false,
            catalog,
            page_count: 1,
            pages: vec![PageInfo {
                number: 1,
                boxes: PageBoxes::all_present(),
            }],
            fonts: vec![FontInfo {
                name: "/F1".to_owned(),
                font_type: FontType::TrueType,
                embedded: true,
            }],
            colour_spaces: vec![if standard == Standard::PdfX1a {
                ColourSpace::DeviceCmyk
            } else {
                ColourSpace::DeviceRgb
            }],
            images: vec![ImageInfo {
                name: "/Im1".to_owned(),
                filter: ImageFilter::FlateDecode,
            }],
            transparency: TransparencyUsage::default(),
        }
    }

    fn nonconforming_input(standard: Standard) -> PreflightInput {
        let mut input = conforming_input(standard);
        input.version = PdfVersion::new(1, 0);
        input.is_encrypted = true;
        input.catalog.output_intents.clear();
        input.catalog.has_xmp_metadata = false;
        input.catalog.has_mark_info = false;
        input.catalog.is_marked = false;
        input.catalog.has_struct_tree_root = false;
        input.catalog.has_lang = false;
        input.catalog.has_acro_form = true;
        if let Some(font) = input.fonts.first_mut() {
            font.embedded = false;
        }
        input.colour_spaces = vec![ColourSpace::DeviceRgb, ColourSpace::Lab];
        input.images = vec![
            ImageInfo {
                name: "/Im1".to_owned(),
                filter: ImageFilter::JpxDecode,
            },
            ImageInfo {
                name: "/Im2".to_owned(),
                filter: ImageFilter::Jbig2Decode,
            },
        ];
        input.transparency = TransparencyUsage {
            uses_transparency: true,
            locations: vec!["page 1".to_owned(), "page 2".to_owned()],
        };
        if let Some(page) = input.pages.first_mut() {
            page.boxes = PageBoxes::default();
        }
        input
    }

    fn codes(findings: &[PreflightFinding]) -> Vec<&str> {
        let mut codes: Vec<&str> = findings.iter().map(PreflightFinding::code).collect();
        codes.sort_unstable();
        codes
    }

    #[test]
    fn conforming_input_is_conforming_for_every_profile() {
        for (prefix, standard) in profiles() {
            let input = conforming_input(standard);
            let report = report_with_prefix(prefix, &input);
            assert!(
                report.is_conforming(),
                "{prefix} rejected a conforming input: {:?}",
                report.findings()
            );
            assert_eq!(report.error_count(), 0, "{prefix}");
            assert!(report.findings().is_empty(), "{prefix}");
            assert_eq!(report.standard(), standard);
        }
    }

    #[test]
    fn nonconforming_input_fails_every_profile() {
        for (prefix, standard) in profiles() {
            let input = nonconforming_input(standard);
            let report = report_with_prefix(prefix, &input);
            assert!(!report.is_conforming(), "{prefix} accepted a bad input");
            assert!(report.error_count() > 0, "{prefix}");
            assert_eq!(report.standard(), standard);
        }
    }

    #[test]
    fn finding_codes_are_stable_per_profile() {
        let expected: &[(&str, &[&str])] = &[
            (
                "PDFA1B",
                &[
                    "PDFA1B.ENCRYPTED",
                    "PDFA1B.FONT_NOT_EMBEDDED",
                    "PDFA1B.IMAGE_FILTER_NOT_ALLOWED",
                    "PDFA1B.NO_OUTPUT_INTENT",
                    "PDFA1B.NO_XMP_METADATA",
                    "PDFA1B.PAGE_NO_MEDIABOX",
                    "PDFA1B.TRANSPARENCY_FORBIDDEN",
                    "PDFA1B.TRANSPARENCY_FORBIDDEN",
                    "PDFA1B.VERSION_TOO_LOW",
                ],
            ),
            (
                "PDFA2B",
                &[
                    "PDFA2B.ENCRYPTED",
                    "PDFA2B.FONT_NOT_EMBEDDED",
                    "PDFA2B.NO_OUTPUT_INTENT",
                    "PDFA2B.NO_XMP_METADATA",
                    "PDFA2B.PAGE_NO_MEDIABOX",
                    "PDFA2B.VERSION_TOO_LOW",
                ],
            ),
            (
                "PDFX1A",
                &[
                    "PDFX1A.DISALLOWED_COLOUR_SPACE",
                    "PDFX1A.DISALLOWED_COLOUR_SPACE",
                    "PDFX1A.ENCRYPTED",
                    "PDFX1A.FONT_NOT_EMBEDDED",
                    "PDFX1A.IMAGE_FILTER_NOT_ALLOWED",
                    "PDFX1A.IMAGE_FILTER_NOT_ALLOWED",
                    "PDFX1A.NO_OUTPUT_INTENT",
                    "PDFX1A.PAGE_NO_BLEEDBOX",
                    "PDFX1A.PAGE_NO_MEDIABOX",
                    "PDFX1A.PAGE_NO_TRIMBOX",
                    "PDFX1A.TRANSPARENCY_FORBIDDEN",
                    "PDFX1A.TRANSPARENCY_FORBIDDEN",
                    "PDFX1A.VERSION_TOO_LOW",
                ],
            ),
            (
                "PDFX4",
                &[
                    "PDFX4.ENCRYPTED",
                    "PDFX4.FONT_NOT_EMBEDDED",
                    "PDFX4.NO_OUTPUT_INTENT",
                    "PDFX4.NO_XMP_METADATA",
                    "PDFX4.PAGE_NO_BLEEDBOX",
                    "PDFX4.PAGE_NO_MEDIABOX",
                    "PDFX4.PAGE_NO_TRIMBOX",
                    "PDFX4.VERSION_TOO_LOW",
                ],
            ),
            (
                "PDFUA1",
                &[
                    "PDFUA1.ENCRYPTED",
                    "PDFUA1.FONT_NOT_EMBEDDED",
                    "PDFUA1.FORM_FIELDS_MANUAL_CHECK",
                    "PDFUA1.NOT_MARKED",
                    "PDFUA1.NO_LANG",
                    "PDFUA1.NO_STRUCT_TREE_ROOT",
                    "PDFUA1.PAGE_NO_MEDIABOX",
                    "PDFUA1.VERSION_TOO_LOW",
                ],
            ),
        ];
        for (prefix, standard) in profiles() {
            let input = nonconforming_input(standard);
            let findings = check_with_prefix(prefix, &input);
            let observed = codes(&findings);
            let wanted = expected
                .iter()
                .find(|(name, _)| *name == prefix)
                .map(|(_, list)| *list)
                .unwrap_or(&[]);
            assert_eq!(observed, wanted, "unstable codes for {prefix}");
            for code in &observed {
                assert!(code.starts_with(&format!("{prefix}.")), "{code}");
            }
        }
    }

    #[test]
    fn ordering_is_independent_of_input_collection_order() {
        for (prefix, standard) in profiles() {
            let mut forward = nonconforming_input(standard);
            let mut reverse = forward.clone();
            reverse.pages.reverse();
            reverse.fonts.reverse();
            reverse.colour_spaces.reverse();
            reverse.images.reverse();
            reverse.catalog.output_intents.reverse();
            reverse.transparency.locations.reverse();

            let a = report_with_prefix(prefix, &forward);
            let b = report_with_prefix(prefix, &reverse);
            assert_eq!(a, b, "report changed with input order for {prefix}");

            let sorted_a = {
                let mut findings = check_with_prefix(prefix, &forward);
                crate::sort_findings(&mut findings);
                findings
            };
            assert_eq!(a.findings(), sorted_a.as_slice(), "{prefix}");
            forward.transparency.locations.sort();
            assert_eq!(
                check_with_prefix(prefix, &forward),
                check_with_prefix(prefix, &forward),
                "check() is not deterministic for {prefix}"
            );
        }
    }

    #[test]
    fn findings_are_ordered_by_severity_then_code() {
        let input = nonconforming_input(Standard::PdfUa1);
        let findings = PdfUa1Profile::new().check_sorted(&input);
        let severities: Vec<Severity> = findings.iter().map(PreflightFinding::severity).collect();
        let info_index = severities.iter().position(|s| *s == Severity::Info);
        assert_eq!(info_index, Some(severities.len() - 1));
        assert!(
            severities[..info_index.unwrap_or(0)]
                .iter()
                .all(|s| *s == Severity::Error)
        );
    }

    #[test]
    fn severity_filtering_keeps_only_requested_levels() {
        let mut input = conforming_input(Standard::PdfUa1);
        input.catalog.has_lang = false;
        input.catalog.has_acro_form = true;
        input.pages.clear();
        input.page_count = 2;

        let profile = PdfUa1Profile::new();
        let errors = profile.check_filtered(&input, Severity::Error);
        assert!(!errors.is_empty());
        assert!(errors.iter().all(|f| f.severity() == Severity::Error));

        let with_warnings = profile.check_filtered(&input, Severity::Warning);
        assert!(with_warnings.len() > errors.len());
        assert!(
            with_warnings
                .iter()
                .all(|f| f.severity() >= Severity::Warning)
        );
        assert!(
            with_warnings
                .iter()
                .any(|f| f.code() == "PDFUA1.PAGE_DATA_UNAVAILABLE")
        );

        let all = profile.check_filtered(&input, Severity::Info);
        assert!(all.len() > with_warnings.len());
        assert!(
            all.iter()
                .any(|f| f.code() == "PDFUA1.FORM_FIELDS_MANUAL_CHECK")
        );
    }

    #[test]
    fn report_summary_is_stable() {
        let good = conforming_input(Standard::PdfA1b);
        assert_eq!(
            PdfA1bProfile::new().report(&good).summary(),
            "PDF/A-1b: conforming (0 errors, 0 warnings, 0 info)"
        );

        let bad = nonconforming_input(Standard::PdfA1b);
        let report = PdfA1bProfile::new().report(&bad);
        assert_eq!(
            report.summary(),
            "PDF/A-1b: non-conforming (9 errors, 0 warnings, 0 info)"
        );
        assert_eq!(
            report.summary(),
            PdfA1bProfile::new().report(&bad).summary()
        );
    }

    #[test]
    fn profile_standard_and_default_match_expected() {
        assert_eq!(PdfA1bProfile::new().standard(), Standard::PdfA1b);
        assert_eq!(PdfA2bProfile::new().standard(), Standard::PdfA2b);
        assert_eq!(PdfX1aProfile::new().standard(), Standard::PdfX1a);
        assert_eq!(PdfX4Profile::new().standard(), Standard::PdfX4);
        assert_eq!(PdfUa1Profile::new().standard(), Standard::PdfUa1);
    }
}
