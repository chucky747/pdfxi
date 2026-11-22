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
//!
//! # Scope: subset preflight, not certification
//!
//! These profiles and their opt-in fixups are a **deterministic subset
//! preflight**, not a conformance certification: [`crate::CERTIFICATION`] is
//! `false`. They inspect a caller-built `PreflightInput` snapshot rather than
//! raw PDF bytes. The PDF/A and PDF/X profiles now validate the header and tag
//! table of a caller-supplied output-intent ICC stream (see [`crate::icc`]), and
//! the PDF/UA profile checks role-map, alt-text, and reading-order facts (see
//! [`crate::structure`]). Embedded font programs, colourimetric correctness,
//! full content-stream semantics, and a rendering oracle remain out of scope.
//! Fixups only edit a cloned snapshot and add *placeholders* (XMP skeleton,
//! output intent without an ICC stream, version bump); a producer must supply
//! real data and an independent validator (for example veraPDF in CI) before
//! claiming PDF/A, PDF/X, PDF/E, PDF/UA, or PDF/VT-1 conformance. A passing
//! report means
//! "no decidable rule in this subset fired", not "certified conforming".

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
    /// Whether every annotation must carry an `/AP` appearance stream.
    require_annotation_appearance: bool,
    /// Annotation subtypes that the standard forbids.
    forbidden_annotation_subtypes: &'static [&'static str],
    /// Catalog or document entries that the standard forbids.
    forbidden_catalog_entries: &'static [&'static str],
    /// Whether the profile inspects the output intent's ICC bytes when supplied.
    validate_output_intent_icc: bool,
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
    require_annotation_appearance: true,
    forbidden_annotation_subtypes: &["/Popup", "/Sound", "/Movie", "/3D"],
    forbidden_catalog_entries: &["/EmbeddedFiles", "/JavaScript", "/AA"],
    validate_output_intent_icc: true,
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
    require_annotation_appearance: true,
    forbidden_annotation_subtypes: &["/Popup", "/Sound", "/Movie", "/3D"],
    forbidden_catalog_entries: &["/EmbeddedFiles", "/JavaScript", "/AA"],
    validate_output_intent_icc: true,
};

const PDFA3B_RULES: Rules = Rules {
    standard: Standard::PdfA3b,
    prefix: "PDFA3B",
    min_version: PdfVersion::new(1, 7),
    required_output_subtype: Some("GTS_PDFA1"),
    xmp_required: true,
    forbid_transparency: false,
    forbidden_image_filters: &[],
    forbidden_colour_spaces: &[],
    require_print_boxes: false,
    check_accessibility: false,
    require_annotation_appearance: true,
    forbidden_annotation_subtypes: &["/Popup", "/Sound", "/Movie", "/3D"],
    // PDF/A-3, unlike PDF/A-1 and PDF/A-2, permits embedded files.
    forbidden_catalog_entries: &["/JavaScript", "/AA"],
    validate_output_intent_icc: true,
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
    require_annotation_appearance: true,
    forbidden_annotation_subtypes: &["/Popup", "/Sound", "/Movie", "/3D", "/FileAttachment"],
    forbidden_catalog_entries: &["/EmbeddedFiles", "/JavaScript", "/AA"],
    validate_output_intent_icc: true,
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
    require_annotation_appearance: true,
    forbidden_annotation_subtypes: &["/Popup", "/Sound", "/Movie", "/3D", "/FileAttachment"],
    forbidden_catalog_entries: &["/EmbeddedFiles", "/JavaScript", "/AA"],
    validate_output_intent_icc: true,
};

const PDFE1_RULES: Rules = Rules {
    standard: Standard::PdfE1,
    prefix: "PDFE1",
    min_version: PdfVersion::new(1, 6),
    required_output_subtype: None,
    xmp_required: false,
    forbid_transparency: false,
    forbidden_image_filters: &[],
    forbidden_colour_spaces: &[],
    require_print_boxes: false,
    check_accessibility: false,
    require_annotation_appearance: false,
    forbidden_annotation_subtypes: &[],
    // PDF/E is an engineering exchange format; embedded files and scripting
    // are not prohibited by this subset of checks.
    forbidden_catalog_entries: &[],
    validate_output_intent_icc: false,
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
    require_annotation_appearance: false,
    forbidden_annotation_subtypes: &[],
    forbidden_catalog_entries: &[],
    validate_output_intent_icc: false,
};

const PDFVT1_RULES: Rules = Rules {
    standard: Standard::PdfVt1,
    prefix: "PDFVT1",
    min_version: PdfVersion::new(1, 6),
    required_output_subtype: Some("GTS_PDFX"),
    xmp_required: true,
    forbid_transparency: false,
    forbidden_image_filters: &[],
    forbidden_colour_spaces: &[],
    require_print_boxes: true,
    check_accessibility: false,
    require_annotation_appearance: true,
    forbidden_annotation_subtypes: &["/Popup", "/Sound", "/Movie", "/3D", "/FileAttachment"],
    forbidden_catalog_entries: &["/EmbeddedFiles", "/JavaScript", "/AA"],
    validate_output_intent_icc: true,
};

/// Returns the catalog entries the profile for `standard` forbids.
///
/// The per-standard [`Rules`] constants above are the single source of truth
/// for prohibited entries. This accessor lets the public
/// [`crate::prohibited_entries`] fixup helper delegate to those same rules so
/// preflight checks and fixups can never disagree about what is prohibited.
#[must_use]
pub(crate) fn forbidden_catalog_entries_for(standard: Standard) -> &'static [&'static str] {
    match standard {
        Standard::PdfA1b => PDFA1B_RULES.forbidden_catalog_entries,
        Standard::PdfA2b => PDFA2B_RULES.forbidden_catalog_entries,
        Standard::PdfA3b => PDFA3B_RULES.forbidden_catalog_entries,
        Standard::PdfX1a => PDFX1A_RULES.forbidden_catalog_entries,
        Standard::PdfX4 => PDFX4_RULES.forbidden_catalog_entries,
        Standard::PdfE1 => PDFE1_RULES.forbidden_catalog_entries,
        Standard::PdfUa1 => PDFUA1_RULES.forbidden_catalog_entries,
        Standard::PdfVt1 => PDFVT1_RULES.forbidden_catalog_entries,
    }
}

/// Returns whether the profile for `standard` forbids transparency.
///
/// This delegates to the same declarative rule constants the checks run, so a
/// request dispatcher can reject a contradictory "allow transparency" request
/// without duplicating the per-standard decision.
#[must_use]
pub(crate) fn forbids_transparency(standard: Standard) -> bool {
    match standard {
        Standard::PdfA1b => PDFA1B_RULES.forbid_transparency,
        Standard::PdfA2b => PDFA2B_RULES.forbid_transparency,
        Standard::PdfA3b => PDFA3B_RULES.forbid_transparency,
        Standard::PdfX1a => PDFX1A_RULES.forbid_transparency,
        Standard::PdfX4 => PDFX4_RULES.forbid_transparency,
        Standard::PdfE1 => PDFE1_RULES.forbid_transparency,
        Standard::PdfUa1 => PDFUA1_RULES.forbid_transparency,
        Standard::PdfVt1 => PDFVT1_RULES.forbid_transparency,
    }
}

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

    for entry in &input.catalog.forbidden_entries {
        if rules
            .forbidden_catalog_entries
            .iter()
            .any(|forbidden| forbidden.eq_ignore_ascii_case(entry.as_str()))
        {
            push(
                &mut out,
                rules,
                "FORBIDDEN_ENTRY",
                Severity::Error,
                format!(
                    "entry {entry} is not permitted by {}; remove it before delivery",
                    rules.standard
                ),
                Some(format!("Catalog {entry}")),
            );
        }
    }

    for annotation in &input.annotations {
        if rules
            .forbidden_annotation_subtypes
            .iter()
            .any(|forbidden| annotation.is_subtype(forbidden))
        {
            push(
                &mut out,
                rules,
                "ANNOTATION_FORBIDDEN",
                Severity::Error,
                format!(
                    "annotation subtype {} is not permitted by {}",
                    annotation.subtype, rules.standard
                ),
                Some(format!("annotation {}", annotation.subtype)),
            );
        }
        if rules.require_annotation_appearance && !annotation.has_appearance_stream {
            push(
                &mut out,
                rules,
                "ANNOTATION_NO_APPEARANCE",
                Severity::Error,
                format!(
                    "annotation {} has no /AP appearance stream, required by {}",
                    annotation.subtype, rules.standard
                ),
                Some(format!("annotation {}", annotation.subtype)),
            );
        }
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

    if rules.validate_output_intent_icc
        && let Some(bytes) = input.output_intent_icc.as_deref()
        && let Err(error) = crate::icc::validate_icc(bytes)
    {
        push(
            &mut out,
            rules,
            "ICC_PROFILE_INVALID",
            Severity::Error,
            format!(
                "output-intent /DestOutputProfile is not a valid ICC profile ({}): {}",
                error.code(),
                error
            ),
            Some("Catalog /OutputIntents /DestOutputProfile".to_owned()),
        );
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
        for violation in crate::structure::check_structure(input) {
            push(
                &mut out,
                rules,
                violation.code(),
                violation.severity(),
                violation.message(),
                Some(violation.location()),
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

/// Preflight profile for ISO 19005-3 Level B (PDF/A-3b).
///
/// Requires PDF 1.7+, a `GTS_PDFA1` output intent with an ICC profile, XMP, and
/// embedded fonts. Transparency and embedded files are permitted; appearance
/// streams are required on annotations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PdfA3bProfile;

impl PdfA3bProfile {
    /// Creates a PDF/A-3b profile.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for PdfA3bProfile {
    fn default() -> Self {
        Self::new()
    }
}

impl PreflightProfile for PdfA3bProfile {
    type Input = PreflightInput;

    fn standard(&self) -> Standard {
        Standard::PdfA3b
    }

    fn check(&self, input: &PreflightInput) -> Vec<PreflightFinding> {
        run_checks(input, &PDFA3B_RULES)
    }
}

/// Preflight profile for ISO 14739-1 (PDF/E-1).
///
/// Checks the decidable engineering-exchange facts: PDF 1.6+, no encryption,
/// embedded fonts, and the absence of forbidden entries. Output intents, XMP,
/// transparency, and page-box requirements are not part of this subset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PdfE1Profile;

impl PdfE1Profile {
    /// Creates a PDF/E-1 profile.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for PdfE1Profile {
    fn default() -> Self {
        Self::new()
    }
}

impl PreflightProfile for PdfE1Profile {
    type Input = PreflightInput;

    fn standard(&self) -> Standard {
        Standard::PdfE1
    }

    fn check(&self, input: &PreflightInput) -> Vec<PreflightFinding> {
        run_checks(input, &PDFE1_RULES)
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

/// Preflight profile for ISO 16612-2 (PDF/VT-1).
///
/// PDF/VT-1 is PDF/X-4 plus a document-part hierarchy. The profile runs the
/// same decidable PDF/X-4 checks (PDF 1.6+, `GTS_PDFX` output intent, XMP,
/// embedded fonts, print boxes) and additionally requires a catalog
/// `/DPartRoot` and a `/DPart` association on every page. DPart record levels,
/// metadata streams, and multi-file PDF/VT-2 packaging remain reported-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PdfVt1Profile;

impl PdfVt1Profile {
    /// Creates a PDF/VT-1 profile.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for PdfVt1Profile {
    fn default() -> Self {
        Self::new()
    }
}

impl PreflightProfile for PdfVt1Profile {
    type Input = PreflightInput;

    fn standard(&self) -> Standard {
        Standard::PdfVt1
    }

    fn check(&self, input: &PreflightInput) -> Vec<PreflightFinding> {
        let mut findings = run_checks(input, &PDFVT1_RULES);
        if !input.catalog.has_dpart_root {
            push(
                &mut findings,
                &PDFVT1_RULES,
                "DPART_ROOT_MISSING",
                Severity::Error,
                "PDF/VT-1 requires a /DPartRoot entry in the catalog".to_owned(),
                Some("Catalog /DPartRoot".to_owned()),
            );
        }
        let associated = input.pages.iter().filter(|page| page.has_dpart).count();
        if associated != input.page_count {
            push(
                &mut findings,
                &PDFVT1_RULES,
                "DPART_PAGE_UNASSIGNED",
                Severity::Error,
                format!(
                    "{} of {} page(s) carry a /DPart association; every page must belong to one document part",
                    associated, input.page_count
                ),
                Some("Page /DPart".to_owned()),
            );
        }
        findings
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{
        AnnotationInfo, CatalogInfo, FontInfo, FontType, ImageInfo, OutputIntent, PageBoxes,
        PageInfo, TransparencyUsage,
    };

    fn profiles() -> [(&'static str, Standard); 6] {
        [
            ("PDFA1B", Standard::PdfA1b),
            ("PDFA2B", Standard::PdfA2b),
            ("PDFX1A", Standard::PdfX1a),
            ("PDFX4", Standard::PdfX4),
            ("PDFUA1", Standard::PdfUa1),
            ("PDFVT1", Standard::PdfVt1),
        ]
    }

    fn check_with_prefix(prefix: &str, input: &PreflightInput) -> Vec<PreflightFinding> {
        match prefix {
            "PDFA1B" => PdfA1bProfile::new().check(input),
            "PDFA2B" => PdfA2bProfile::new().check(input),
            "PDFX1A" => PdfX1aProfile::new().check(input),
            "PDFX4" => PdfX4Profile::new().check(input),
            "PDFUA1" => PdfUa1Profile::new().check(input),
            "PDFVT1" => PdfVt1Profile::new().check(input),
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
            "PDFVT1" => PdfVt1Profile::new().report(input),
            other => panic!("unknown profile prefix {other}"),
        }
    }

    fn conforming_input(standard: Standard) -> PreflightInput {
        let (version, subtype) = match standard {
            Standard::PdfA1b => (PdfVersion::new(1, 4), "GTS_PDFA1"),
            Standard::PdfA2b => (PdfVersion::new(1, 7), "GTS_PDFA1"),
            Standard::PdfA3b => (PdfVersion::new(1, 7), "GTS_PDFA1"),
            Standard::PdfX1a => (PdfVersion::new(1, 3), "GTS_PDFX"),
            Standard::PdfX4 | Standard::PdfVt1 => (PdfVersion::new(1, 6), "GTS_PDFX"),
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
        if standard == Standard::PdfVt1 {
            catalog.has_dpart_root = true;
        }
        PreflightInput {
            version,
            is_encrypted: false,
            catalog,
            page_count: 1,
            pages: vec![PageInfo {
                number: 1,
                boxes: PageBoxes::all_present(),
                has_dpart: standard == Standard::PdfVt1,
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
            annotations: Vec::new(),
            ..Default::default()
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
        input.catalog.has_dpart_root = false;
        for page in &mut input.pages {
            page.has_dpart = false;
        }
        if let Some(page) = input.pages.first_mut() {
            page.boxes = PageBoxes::default();
        }
        input
    }

    #[test]
    fn pdf_vt1_requires_document_parts_and_page_associations() {
        let mut input = conforming_input(Standard::PdfVt1);
        let report = PdfVt1Profile::new().report(&input);
        assert!(
            report.is_conforming(),
            "PDF/VT-1 rejected a conforming input: {:?}",
            report.findings()
        );
        assert_eq!(PdfVt1Profile::new().standard().label(), "PDF/VT-1");

        input.catalog.has_dpart_root = false;
        let findings = PdfVt1Profile::new().check(&input);
        let observed = codes(&findings);
        assert!(
            observed.contains(&"PDFVT1.DPART_ROOT_MISSING"),
            "{observed:?}"
        );

        input.catalog.has_dpart_root = true;
        input.pages[0].has_dpart = false;
        let findings = PdfVt1Profile::new().check(&input);
        let observed = codes(&findings);
        assert!(
            observed.contains(&"PDFVT1.DPART_PAGE_UNASSIGNED"),
            "{observed:?}"
        );
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
                "PDFVT1",
                &[
                    "PDFVT1.DPART_PAGE_UNASSIGNED",
                    "PDFVT1.DPART_ROOT_MISSING",
                    "PDFVT1.ENCRYPTED",
                    "PDFVT1.FONT_NOT_EMBEDDED",
                    "PDFVT1.NO_OUTPUT_INTENT",
                    "PDFVT1.NO_XMP_METADATA",
                    "PDFVT1.PAGE_NO_BLEEDBOX",
                    "PDFVT1.PAGE_NO_MEDIABOX",
                    "PDFVT1.PAGE_NO_TRIMBOX",
                    "PDFVT1.VERSION_TOO_LOW",
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

    // ---- T-0079 named coverage: ordered rules over the plain-data snapshot ----

    #[test]
    fn pdf_a_reports_unembedded_font() {
        let mut input = conforming_input(Standard::PdfA1b);
        input.fonts.push(FontInfo {
            name: "/F2".to_owned(),
            font_type: FontType::Type1,
            embedded: false,
        });
        let findings = PdfA1bProfile::new().check(&input);
        let font = findings
            .iter()
            .find(|finding| finding.code() == "PDFA1B.FONT_NOT_EMBEDDED")
            .expect("test: unembedded font must be reported");
        assert_eq!(font.severity(), Severity::Error);
        assert_eq!(font.location(), Some("font /F2"));
        assert!(font.message().contains("/F2"));
        assert!(!PdfA1bProfile::new().report(&input).is_conforming());
    }

    #[test]
    fn pdf_x_reports_missing_output_intent() {
        let mut input = conforming_input(Standard::PdfX1a);
        input.catalog.output_intents.clear();
        let report = PdfX1aProfile::new().report(&input);
        assert!(
            report
                .findings()
                .iter()
                .any(|finding| finding.code() == "PDFX1A.NO_OUTPUT_INTENT")
        );
        assert_eq!(report.error_count(), 1);
        assert!(!report.is_conforming());
    }

    #[test]
    fn version_mismatch_is_reported() {
        let mut input = conforming_input(Standard::PdfA2b);
        input.version = PdfVersion::new(1, 4);
        let findings = PdfA2bProfile::new().check(&input);
        let version = findings
            .iter()
            .find(|finding| finding.code() == "PDFA2B.VERSION_TOO_LOW")
            .expect("test: version below the minimum must be reported");
        assert_eq!(version.severity(), Severity::Error);
        assert_eq!(version.location(), Some("header"));
        assert!(version.message().contains("1.7"));
    }

    #[test]
    fn transparency_is_reported() {
        let mut input = conforming_input(Standard::PdfA1b);
        input.transparency = TransparencyUsage {
            uses_transparency: true,
            locations: vec!["page 2".to_owned(), "page 1".to_owned()],
        };
        let findings = PdfA1bProfile::new().check(&input);
        let locations: Vec<&str> = findings
            .iter()
            .filter(|finding| finding.code() == "PDFA1B.TRANSPARENCY_FORBIDDEN")
            .filter_map(PreflightFinding::location)
            .collect();
        // Locations are normalised (sorted, deduplicated) by the check itself.
        assert_eq!(locations, vec!["page 1", "page 2"]);
        assert!(
            findings
                .iter()
                .all(|finding| finding.severity() == Severity::Error)
        );
    }

    #[test]
    fn encryption_is_prohibited() {
        for standard in [
            Standard::PdfA1b,
            Standard::PdfA3b,
            Standard::PdfX4,
            Standard::PdfE1,
        ] {
            let mut input = conforming_input(standard);
            input.is_encrypted = true;
            let findings = match standard {
                Standard::PdfA1b => PdfA1bProfile::new().check(&input),
                Standard::PdfA3b => PdfA3bProfile::new().check(&input),
                Standard::PdfX4 => PdfX4Profile::new().check(&input),
                Standard::PdfE1 => PdfE1Profile::new().check(&input),
                _ => Vec::new(),
            };
            assert!(
                findings
                    .iter()
                    .any(|finding| finding.code().ends_with(".ENCRYPTED")),
                "{standard} must prohibit encryption"
            );
        }
    }

    #[test]
    fn clean_input_passes_all_named_profiles() {
        let cases = [
            (
                Standard::PdfA1b,
                PdfA1bProfile::new().report(&conforming_input(Standard::PdfA1b)),
            ),
            (
                Standard::PdfA2b,
                PdfA2bProfile::new().report(&conforming_input(Standard::PdfA2b)),
            ),
            (
                Standard::PdfA3b,
                PdfA3bProfile::new().report(&conforming_input(Standard::PdfA3b)),
            ),
            (
                Standard::PdfX1a,
                PdfX1aProfile::new().report(&conforming_input(Standard::PdfX1a)),
            ),
            (
                Standard::PdfX4,
                PdfX4Profile::new().report(&conforming_input(Standard::PdfX4)),
            ),
            (
                Standard::PdfE1,
                PdfE1Profile::new().report(&conforming_input(Standard::PdfE1)),
            ),
        ];
        for (standard, report) in cases {
            assert_eq!(report.standard(), standard);
            assert!(
                report.findings().is_empty(),
                "{standard} rejected a clean input: {}",
                report.summary()
            );
            assert!(report.is_conforming(), "{standard}");
        }
    }

    #[test]
    fn severity_and_order_are_deterministic() {
        let input = nonconforming_input(Standard::PdfX1a);
        let profile = PdfX1aProfile::new();
        let a = profile.report(&input);
        let b = profile.report(&input);
        assert_eq!(a, b, "the same input must yield the same report");

        // Canonical order is severity (error first), then code, message, location.
        let ordered = profile.check_sorted(&input);
        let severities: Vec<Severity> = ordered.iter().map(PreflightFinding::severity).collect();
        let first_warning = severities
            .iter()
            .position(|severity| *severity != Severity::Error)
            .unwrap_or(severities.len());
        assert!(
            severities[..first_warning]
                .iter()
                .all(|severity| *severity == Severity::Error)
        );
        assert!(
            severities[first_warning..]
                .iter()
                .all(|severity| *severity != Severity::Error)
        );
        assert_eq!(a.findings(), ordered.as_slice());
    }

    #[test]
    fn annotations_and_forbidden_entries_are_reported() {
        let mut input = conforming_input(Standard::PdfA1b);
        input.catalog.forbidden_entries = vec!["/JavaScript".to_owned(), "/MediaBox".to_owned()];
        input.annotations = vec![
            AnnotationInfo::new("/Popup", true),
            AnnotationInfo::new("/Text", false),
        ];
        let findings = PdfA1bProfile::new().check(&input);
        let codes: Vec<&str> = findings.iter().map(PreflightFinding::code).collect();
        assert_eq!(
            codes
                .iter()
                .filter(|code| **code == "PDFA1B.FORBIDDEN_ENTRY")
                .count(),
            1,
            "only entries the profile forbids are reported"
        );
        assert_eq!(
            codes
                .iter()
                .filter(|code| **code == "PDFA1B.ANNOTATION_FORBIDDEN")
                .count(),
            1,
            "only annotation subtypes the profile forbids are reported"
        );
        assert_eq!(
            codes
                .iter()
                .filter(|code| **code == "PDFA1B.ANNOTATION_NO_APPEARANCE")
                .count(),
            1,
            "annotations without /AP are reported for PDF/A-1b"
        );
    }

    #[test]
    fn pdf_a3b_allows_embedded_files_but_pdf_a2b_does_not() {
        let mut input = conforming_input(Standard::PdfA3b);
        input.catalog.forbidden_entries = vec!["/EmbeddedFiles".to_owned()];
        assert!(
            PdfA3bProfile::new().check(&input).is_empty(),
            "PDF/A-3 permits embedded files"
        );

        let mut older = conforming_input(Standard::PdfA2b);
        older.catalog.forbidden_entries = vec!["/EmbeddedFiles".to_owned()];
        assert!(
            PdfA2bProfile::new()
                .check(&older)
                .iter()
                .any(|finding| finding.code() == "PDFA2B.FORBIDDEN_ENTRY")
        );
    }

    #[test]
    fn new_profiles_are_wired_to_their_standards() {
        assert_eq!(PdfA3bProfile::new().standard(), Standard::PdfA3b);
        assert_eq!(PdfE1Profile::new().standard(), Standard::PdfE1);
        assert_eq!(PdfA3bProfile::new(), PdfA3bProfile);
        assert_eq!(PdfE1Profile::new(), PdfE1Profile);
        assert_eq!(PdfA3bProfile::new().standard().label(), "PDF/A-3b");
        assert_eq!(PdfE1Profile::new().standard().label(), "PDF/E-1");
    }
}

/// T-0093 focused conformance coverage: one conforming and one violating
/// fixture per required profile family, plus rule order, context and fixups.
#[cfg(test)]
mod t0093_tests {
    use super::*;
    use crate::input::{
        AnnotationInfo, CatalogInfo, FontInfo, FontType, ImageFilter, ImageInfo, OutputIntent,
        PageBoxes, PageInfo, TransparencyUsage,
    };
    use crate::{
        FixupPolicy, PdfA1bProfile, PdfA2bProfile, PdfX4Profile, PreflightProfile, apply_fixups,
        suggested_fixups,
    };

    /// A minimal conforming snapshot for one of the four required profiles.
    fn conforming(standard: Standard) -> PreflightInput {
        let (version, subtype) = match standard {
            Standard::PdfA1b => (PdfVersion::new(1, 4), "GTS_PDFA1"),
            Standard::PdfA2b => (PdfVersion::new(1, 7), "GTS_PDFA1"),
            Standard::PdfX4 => (PdfVersion::new(1, 6), "GTS_PDFX"),
            Standard::PdfE1 => (PdfVersion::new(1, 6), ""),
            other => panic!("test: unexpected standard {other}"),
        };
        let mut catalog = CatalogInfo {
            has_xmp_metadata: true,
            ..Default::default()
        };
        if standard != Standard::PdfE1 {
            catalog
                .output_intents
                .push(OutputIntent::new(subtype, true));
        }
        PreflightInput {
            version,
            catalog,
            page_count: 1,
            pages: vec![PageInfo {
                number: 1,
                boxes: PageBoxes::all_present(),
                has_dpart: standard == Standard::PdfVt1,
            }],
            fonts: vec![FontInfo {
                name: "/F1".to_owned(),
                font_type: FontType::TrueType,
                embedded: true,
            }],
            images: vec![ImageInfo {
                name: "/Im1".to_owned(),
                filter: ImageFilter::FlateDecode,
            }],
            ..Default::default()
        }
    }

    fn profile(standard: Standard) -> crate::ConformanceProfile {
        crate::ConformanceProfile::from_standard(standard)
            .unwrap_or_else(|_| panic!("test: standard must resolve"))
    }

    #[test]
    fn t0093_a1b_conforming_fixture_passes() {
        let report = profile(Standard::PdfA1b).report(&conforming(Standard::PdfA1b));
        assert!(report.is_conforming(), "{}", report.summary());
        assert_eq!(report.error_count(), 0);
        assert!(report.findings().is_empty());
    }

    #[test]
    fn t0093_a2b_conforming_fixture_passes() {
        let report = profile(Standard::PdfA2b).report(&conforming(Standard::PdfA2b));
        assert!(report.is_conforming(), "{}", report.summary());
        assert_eq!(report.error_count(), 0);
    }

    #[test]
    fn t0093_x4_conforming_fixture_passes() {
        let report = profile(Standard::PdfX4).report(&conforming(Standard::PdfX4));
        assert!(report.is_conforming(), "{}", report.summary());
        assert_eq!(report.error_count(), 0);
    }

    #[test]
    fn t0093_e1_conforming_fixture_passes() {
        let report = profile(Standard::PdfE1).report(&conforming(Standard::PdfE1));
        assert!(report.is_conforming(), "{}", report.summary());
        assert_eq!(report.error_count(), 0);
    }

    #[test]
    fn t0093_missing_xmp_is_an_error_for_pdf_a_and_x4() {
        for standard in [Standard::PdfA1b, Standard::PdfA2b, Standard::PdfX4] {
            let mut input = conforming(standard);
            input.catalog.has_xmp_metadata = false;
            let report = profile(standard).report(&input);
            assert!(
                report.findings().iter().any(|finding| {
                    finding.code().ends_with(".NO_XMP_METADATA")
                        && finding.severity() == Severity::Error
                }),
                "{standard} must require XMP"
            );
            assert!(!report.is_conforming());
        }
    }

    #[test]
    fn t0093_low_version_is_an_error() {
        let mut input = conforming(Standard::PdfA2b);
        input.version = PdfVersion::new(1, 4);
        let findings = PdfA2bProfile::new().check(&input);
        let finding = findings
            .iter()
            .find(|item| item.code() == "PDFA2B.VERSION_TOO_LOW")
            .unwrap_or_else(|| panic!("test: version finding missing"));
        assert_eq!(finding.severity(), Severity::Error);
        assert_eq!(finding.location(), Some("header"));
        assert!(finding.message().contains("1.7"));
    }

    #[test]
    fn t0093_missing_output_intent_is_an_error() {
        for standard in [Standard::PdfA1b, Standard::PdfX4] {
            let mut input = conforming(standard);
            input.catalog.output_intents.clear();
            let findings = profile(standard).check(&input);
            assert!(
                findings
                    .iter()
                    .any(|item| item.code().ends_with(".NO_OUTPUT_INTENT")),
                "{standard} must require an output intent"
            );
        }
    }

    #[test]
    fn t0093_encryption_is_an_error() {
        for standard in [
            Standard::PdfA1b,
            Standard::PdfA2b,
            Standard::PdfX4,
            Standard::PdfE1,
        ] {
            let mut input = conforming(standard);
            input.is_encrypted = true;
            let findings = profile(standard).check(&input);
            assert!(
                findings
                    .iter()
                    .any(|item| item.code().ends_with(".ENCRYPTED")),
                "{standard} must reject encryption"
            );
        }
    }

    #[test]
    fn t0093_forbidden_javascript_is_an_error() {
        let mut input = conforming(Standard::PdfA1b);
        input.catalog.forbidden_entries = vec!["/JavaScript".to_owned()];
        let findings = PdfA1bProfile::new().check(&input);
        let finding = findings
            .iter()
            .find(|item| item.code() == "PDFA1B.FORBIDDEN_ENTRY")
            .unwrap_or_else(|| panic!("test: forbidden-entry finding missing"));
        assert_eq!(finding.location(), Some("Catalog /JavaScript"));
        assert!(finding.message().contains("/JavaScript"));
        assert_eq!(
            findings
                .iter()
                .filter(|item| item.code() == "PDFA1B.FORBIDDEN_ENTRY")
                .count(),
            1
        );
    }

    #[test]
    fn t0093_transparency_is_an_error_where_forbidden() {
        let mut input = conforming(Standard::PdfA1b);
        input.transparency = TransparencyUsage {
            uses_transparency: true,
            locations: vec!["page 2".to_owned(), "page 1".to_owned()],
        };
        let findings = PdfA1bProfile::new().check(&input);
        let codes: Vec<&str> = findings
            .iter()
            .filter(|item| item.code() == "PDFA1B.TRANSPARENCY_FORBIDDEN")
            .map(PreflightFinding::code)
            .collect();
        assert_eq!(codes.len(), 2);
        assert!(
            findings
                .iter()
                .all(|item| item.severity() == Severity::Error)
        );

        let mut allowed = conforming(Standard::PdfA2b);
        allowed.transparency.uses_transparency = true;
        assert!(PdfA2bProfile::new().check(&allowed).is_empty());
    }

    #[test]
    fn t0093_rule_ids_are_deterministic_and_ordered() {
        let mut input = conforming(Standard::PdfX4);
        input.is_encrypted = true;
        input.catalog.has_xmp_metadata = false;
        input.version = PdfVersion::new(1, 3);
        input.fonts[0].embedded = false;
        let profile = PdfX4Profile::new();
        let first = profile.report(&input);
        let second = profile.report(&input);
        assert_eq!(first, second);
        for finding in first.findings() {
            assert!(finding.code().starts_with("PDFX4."));
            let suffix = finding.code().trim_start_matches("PDFX4.");
            assert_eq!(suffix, suffix.to_uppercase());
            assert!(!suffix.contains(' '));
        }
        let severities: Vec<Severity> = first
            .findings()
            .iter()
            .map(PreflightFinding::severity)
            .collect();
        let last_error = severities.iter().rposition(|item| *item == Severity::Error);
        assert!(last_error.is_some());
        assert!(
            severities[..=last_error.unwrap_or(0)]
                .iter()
                .all(|item| *item == Severity::Error)
        );
    }

    #[test]
    fn t0093_findings_carry_page_and_object_context() {
        let mut input = conforming(Standard::PdfX4);
        input.pages[0].boxes = PageBoxes::default();
        input.annotations.push(AnnotationInfo::new("/Popup", true));
        let findings = PdfX4Profile::new().check(&input);
        assert!(findings.iter().any(
            |item| item.code() == "PDFX4.PAGE_NO_MEDIABOX" && item.location() == Some("page 1")
        ));
        assert!(
            findings
                .iter()
                .any(|item| item.code() == "PDFX4.ANNOTATION_FORBIDDEN"
                    && item.location() == Some("annotation /Popup"))
        );
    }

    #[test]
    fn t0093_fixup_suggestions_do_not_mutate_the_source() {
        let mut input = conforming(Standard::PdfA2b);
        input.catalog.has_xmp_metadata = false;
        input.catalog.output_intents.clear();
        input.version = PdfVersion::new(1, 4);
        input
            .catalog
            .forbidden_entries
            .push("/JavaScript".to_owned());
        let before = input.clone();
        let report = PdfA2bProfile::new().report(&input);
        let suggestions = suggested_fixups(Standard::PdfA2b, &report);
        assert_eq!(suggestions, suggested_fixups(Standard::PdfA2b, &report));
        let outcome = apply_fixups(Standard::PdfA2b, &input, &FixupPolicy::all(), &suggestions)
            .unwrap_or_else(|error| panic!("test: suggestions must be applicable: {error}"));
        assert_eq!(input, before);
        assert!(outcome.input.catalog.has_xmp_metadata);
        let after = PdfA2bProfile::new().report(&outcome.input);
        assert!(after.error_count() < report.error_count());
        assert!(
            after
                .findings()
                .iter()
                .any(|item| item.code() == "PDFA2B.OUTPUT_INTENT_NO_PROFILE"),
            "a real ICC profile is still required"
        );
    }
}

/// T-0127 focused coverage: deterministic subset-preflight findings across the
/// PDF/A-3b, PDF/E-1 and PDF/X-4 profiles, including page-box findings and a
/// stable canonical report ordering.
#[cfg(test)]
mod t0127_tests {
    use super::*;
    use crate::input::{
        AnnotationInfo, CatalogInfo, FontInfo, FontType, OutputIntent, PageBoxes, PageInfo,
    };
    use crate::{PreflightFinding, PreflightProfile, Severity};

    /// A conforming snapshot for the profiles exercised in this module.
    fn conforming(standard: Standard) -> PreflightInput {
        let (version, subtype) = match standard {
            Standard::PdfA3b => (PdfVersion::new(1, 7), "GTS_PDFA1"),
            Standard::PdfX4 => (PdfVersion::new(1, 6), "GTS_PDFX"),
            Standard::PdfE1 => (PdfVersion::new(1, 6), ""),
            other => panic!("test: unexpected standard {other}"),
        };
        let mut catalog = CatalogInfo {
            has_xmp_metadata: true,
            ..Default::default()
        };
        if standard != Standard::PdfE1 {
            catalog
                .output_intents
                .push(OutputIntent::new(subtype, true));
        }
        PreflightInput {
            version,
            catalog,
            page_count: 1,
            pages: vec![PageInfo {
                number: 1,
                boxes: PageBoxes::all_present(),
                has_dpart: standard == Standard::PdfVt1,
            }],
            fonts: vec![FontInfo {
                name: "/F1".to_owned(),
                font_type: FontType::TrueType,
                embedded: true,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn t0127_pdfa3b_and_e1_profiles_report_deterministic_findings() {
        let mut input = conforming(Standard::PdfA3b);
        input.catalog.has_xmp_metadata = false;
        input.version = PdfVersion::new(1, 4);
        input.fonts[0].embedded = false;
        let report = PdfA3bProfile::new().report(&input);
        let codes: Vec<&str> = report
            .findings()
            .iter()
            .map(PreflightFinding::code)
            .collect();
        assert!(codes.contains(&"PDFA3B.NO_XMP_METADATA"));
        assert!(codes.contains(&"PDFA3B.VERSION_TOO_LOW"));
        assert!(codes.contains(&"PDFA3B.FONT_NOT_EMBEDDED"));
        assert!(!report.is_conforming());

        // PDF/A-3 permits embedded files, unlike PDF/A-1 and PDF/A-2.
        input.catalog.forbidden_entries = vec!["/EmbeddedFiles".to_owned()];
        assert!(
            !PdfA3bProfile::new()
                .check(&input)
                .iter()
                .any(|finding| finding.code().ends_with("FORBIDDEN_ENTRY"))
        );

        // PDF/E-1 needs neither an output intent nor XMP metadata.
        let e1 = conforming(Standard::PdfE1);
        let e1_report = PdfE1Profile::new().report(&e1);
        assert!(e1_report.findings().is_empty(), "{}", e1_report.summary());
        assert!(e1_report.is_conforming());
    }

    #[test]
    fn t0127_missing_print_boxes_are_reported_with_page_location() {
        let mut input = conforming(Standard::PdfX4);
        if let Some(page) = input.pages.first_mut() {
            page.boxes = PageBoxes::default();
        }
        let findings = PdfX4Profile::new().check(&input);
        let mut missing: Vec<&str> = findings
            .iter()
            .filter(|finding| finding.code().starts_with("PDFX4.PAGE_NO_"))
            .map(PreflightFinding::code)
            .collect();
        missing.sort_unstable();
        assert_eq!(
            missing,
            vec![
                "PDFX4.PAGE_NO_BLEEDBOX",
                "PDFX4.PAGE_NO_MEDIABOX",
                "PDFX4.PAGE_NO_TRIMBOX"
            ]
        );
        assert!(
            findings
                .iter()
                .filter(|finding| finding.code().starts_with("PDFX4.PAGE_NO_"))
                .all(|finding| finding.location() == Some("page 1"))
        );
    }

    #[test]
    fn t0127_report_is_stable_and_findings_are_canonical() {
        let mut input = conforming(Standard::PdfX4);
        input.is_encrypted = true;
        input.catalog.has_xmp_metadata = false;
        input.version = PdfVersion::new(1, 3);
        input.annotations.push(AnnotationInfo::new("/Popup", true));
        let profile = PdfX4Profile::new();
        let first = profile.report(&input);
        let second = profile.report(&input);
        assert_eq!(first, second, "report() must be deterministic");
        for finding in first.findings() {
            let code = finding.code();
            assert!(code.starts_with("PDFX4."));
            let suffix = code.trim_start_matches("PDFX4.");
            assert_eq!(suffix, suffix.to_uppercase());
            assert!(!suffix.contains(' '));
            assert!(
                finding.severity() == Severity::Error || finding.severity() == Severity::Warning
            );
        }
        assert_eq!(first.findings(), profile.check_sorted(&input).as_slice());
    }
}

/// T-0178 coverage: ICC byte validation wired into PDF/A and PDF/X, and the
/// deterministic PDF/UA structure checks wired into the accessibility profile.
#[cfg(test)]
mod t0178_tests {
    use super::*;
    use crate::CERTIFICATION;
    use crate::input::{
        CatalogInfo, FontInfo, FontType, ImageInfo, OutputIntent, PageBoxes, PageInfo,
        PageStructureInfo, StructElementInfo, TransparencyUsage,
    };

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }

    fn put_cc(bytes: &mut [u8], offset: usize, code: &[u8; 4]) {
        bytes[offset..offset + 4].copy_from_slice(code);
    }

    /// A minimal, structurally valid ICC profile with no tags.
    fn valid_icc() -> Vec<u8> {
        let mut bytes = vec![0u8; 132];
        put_u32(&mut bytes, 0, 132);
        put_cc(&mut bytes, 12, b"mntr");
        put_cc(&mut bytes, 16, b"CMYK");
        put_cc(&mut bytes, 20, b"XYZ ");
        put_cc(&mut bytes, 36, b"acsp");
        put_u32(&mut bytes, 128, 0);
        bytes
    }

    fn bad_icc() -> Vec<u8> {
        let mut bytes = valid_icc();
        bytes[36] = b'x';
        bytes
    }

    fn conforming(standard: Standard) -> PreflightInput {
        let (version, subtype) = match standard {
            Standard::PdfA1b => (PdfVersion::new(1, 4), "GTS_PDFA1"),
            Standard::PdfA2b | Standard::PdfA3b => (PdfVersion::new(1, 7), "GTS_PDFA1"),
            Standard::PdfX1a => (PdfVersion::new(1, 3), "GTS_PDFX"),
            Standard::PdfX4 | Standard::PdfVt1 => (PdfVersion::new(1, 6), "GTS_PDFX"),
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
        if standard == Standard::PdfVt1 {
            catalog.has_dpart_root = true;
        }
        PreflightInput {
            version,
            catalog,
            page_count: 1,
            pages: vec![PageInfo {
                number: 1,
                boxes: PageBoxes::all_present(),
                has_dpart: standard == Standard::PdfVt1,
            }],
            fonts: vec![FontInfo {
                name: "/F1".to_owned(),
                font_type: FontType::TrueType,
                embedded: true,
            }],
            colour_spaces: vec![ColourSpace::DeviceCmyk],
            images: vec![ImageInfo {
                name: "/Im1".to_owned(),
                filter: ImageFilter::FlateDecode,
            }],
            transparency: TransparencyUsage::default(),
            annotations: Vec::new(),
            output_intent_icc: Some(valid_icc()),
            ..Default::default()
        }
    }

    fn check(standard: Standard, input: &PreflightInput) -> Vec<PreflightFinding> {
        match standard {
            Standard::PdfA1b => PdfA1bProfile::new().check(input),
            Standard::PdfA2b => PdfA2bProfile::new().check(input),
            Standard::PdfA3b => PdfA3bProfile::new().check(input),
            Standard::PdfX1a => PdfX1aProfile::new().check(input),
            Standard::PdfX4 => PdfX4Profile::new().check(input),
            Standard::PdfE1 => PdfE1Profile::new().check(input),
            Standard::PdfUa1 => PdfUa1Profile::new().check(input),
            Standard::PdfVt1 => PdfVt1Profile::new().check(input),
        }
    }

    #[test]
    fn t0178_invalid_icc_is_an_error_in_every_pdf_a_and_pdf_x_profile() {
        for standard in [
            Standard::PdfA1b,
            Standard::PdfA2b,
            Standard::PdfA3b,
            Standard::PdfX1a,
            Standard::PdfX4,
        ] {
            let mut input = conforming(standard);
            input.output_intent_icc = Some(bad_icc());
            let findings = check(standard, &input);
            let finding = findings
                .iter()
                .find(|item| item.code().ends_with(".ICC_PROFILE_INVALID"))
                .unwrap_or_else(|| panic!("test: {standard} must report an invalid ICC profile"));
            assert_eq!(finding.severity(), Severity::Error);
            assert!(finding.message().contains("ICC.BAD_SIGNATURE"));
            assert_eq!(
                finding.location(),
                Some("Catalog /OutputIntents /DestOutputProfile")
            );
        }
    }

    #[test]
    fn t0178_valid_icc_does_not_add_a_finding() {
        for standard in [
            Standard::PdfA1b,
            Standard::PdfA2b,
            Standard::PdfA3b,
            Standard::PdfX1a,
            Standard::PdfX4,
        ] {
            let input = conforming(standard);
            assert!(
                check(standard, &input).is_empty(),
                "{standard} rejected a valid ICC profile"
            );
        }
    }

    #[test]
    fn t0178_missing_icc_keeps_the_existing_stable_codes() {
        let mut input = conforming(Standard::PdfA2b);
        input.catalog.output_intents.clear();
        let findings = check(Standard::PdfA2b, &input);
        let codes: Vec<&str> = findings.iter().map(PreflightFinding::code).collect();
        assert!(codes.contains(&"PDFA2B.NO_OUTPUT_INTENT"));
        assert!(
            !codes
                .iter()
                .any(|code| code.contains("ICC_PROFILE_INVALID"))
        );

        let mut input = conforming(Standard::PdfX4);
        input.catalog.output_intents[0].has_dest_output_profile = false;
        let findings = check(Standard::PdfX4, &input);
        let codes: Vec<&str> = findings.iter().map(PreflightFinding::code).collect();
        assert!(codes.contains(&"PDFX4.OUTPUT_INTENT_NO_PROFILE"));
        assert!(
            !codes
                .iter()
                .any(|code| code.contains("ICC_PROFILE_INVALID"))
        );
    }

    #[test]
    fn t0178_absent_icc_bytes_are_not_claimed_as_validated() {
        for standard in [Standard::PdfA1b, Standard::PdfX1a] {
            let mut input = conforming(standard);
            input.output_intent_icc = None;
            assert!(
                check(standard, &input).is_empty(),
                "{standard} must stay green when bytes are not supplied"
            );
            assert_eq!(
                crate::validated_output_intent_icc(&input),
                Ok(None),
                "no validation may be claimed when bytes are absent"
            );
        }
    }

    #[test]
    fn t0178_pdf_e_and_ua_do_not_validate_output_intent_icc() {
        for standard in [Standard::PdfE1, Standard::PdfUa1] {
            let mut input = conforming(standard);
            input.output_intent_icc = Some(bad_icc());
            assert!(
                check(standard, &input).is_empty(),
                "{standard} has no ICC rule in this subset"
            );
        }
    }

    #[test]
    fn t0178_ua_structure_violations_produce_stable_codes() {
        let mut input = conforming(Standard::PdfUa1);
        input.struct_elements = vec![
            StructElementInfo {
                id: 0,
                role: "Document".to_owned(),
                alt_text: None,
                is_artifact: false,
                parent: None,
                reading_order: 0,
            },
            StructElementInfo {
                id: 1,
                role: "Figure".to_owned(),
                alt_text: None,
                is_artifact: false,
                parent: Some(0),
                reading_order: 1,
            },
            StructElementInfo {
                id: 2,
                role: "CustomWidget".to_owned(),
                alt_text: None,
                is_artifact: false,
                parent: Some(0),
                reading_order: 2,
            },
        ];
        input.page_structure = vec![PageStructureInfo {
            page_number: 1,
            has_struct_parents: true,
            has_text: true,
            has_tagged_text: false,
            mcids: vec![0, 0],
            struct_parents: vec![1, 1],
        }];

        let findings = PdfUa1Profile::new().check(&input);
        let codes: Vec<&str> = findings.iter().map(PreflightFinding::code).collect();
        assert!(codes.contains(&"PDFUA1.ALT_TEXT_MISSING"));
        assert!(codes.contains(&"PDFUA1.ROLE_MAP_MISSING"));
        assert!(codes.contains(&"PDFUA1.UNTAGGED_TEXT"));
        assert!(codes.contains(&"PDFUA1.DUPLICATE_MCID"));
        assert!(
            findings
                .iter()
                .all(|item| item.severity() == Severity::Error),
            "structure violations must block conformance"
        );
        assert_eq!(findings, PdfUa1Profile::new().check(&input));
        let sorted = PdfUa1Profile::new().check_sorted(&input);
        assert_eq!(findings, sorted);
    }

    #[test]
    fn t0178_ua_clean_structure_adds_no_findings() {
        let mut input = conforming(Standard::PdfUa1);
        input.struct_elements = vec![
            StructElementInfo {
                id: 0,
                role: "Document".to_owned(),
                alt_text: None,
                is_artifact: false,
                parent: None,
                reading_order: 0,
            },
            StructElementInfo {
                id: 1,
                role: "P".to_owned(),
                alt_text: None,
                is_artifact: false,
                parent: Some(0),
                reading_order: 1,
            },
        ];
        input.page_structure = vec![PageStructureInfo {
            page_number: 1,
            has_struct_parents: true,
            has_text: true,
            has_tagged_text: true,
            mcids: vec![0],
            struct_parents: vec![1],
        }];
        assert!(check(Standard::PdfUa1, &input).is_empty());
    }

    #[test]
    fn t0178_structure_findings_ignore_input_collection_order() {
        let mut input = conforming(Standard::PdfUa1);
        input.struct_elements = vec![
            StructElementInfo {
                id: 0,
                role: "Document".to_owned(),
                alt_text: None,
                is_artifact: false,
                parent: None,
                reading_order: 0,
            },
            StructElementInfo {
                id: 1,
                role: "Figure".to_owned(),
                alt_text: Some(String::new()),
                is_artifact: false,
                parent: Some(0),
                reading_order: 1,
            },
        ];
        input.page_structure = vec![PageStructureInfo {
            page_number: 1,
            has_struct_parents: true,
            has_text: true,
            has_tagged_text: true,
            mcids: vec![0],
            struct_parents: vec![1],
        }];
        let mut reversed = input.clone();
        reversed.struct_elements.reverse();
        reversed.page_structure.reverse();
        let a = PdfUa1Profile::new().report(&input);
        let b = PdfUa1Profile::new().report(&reversed);
        assert_eq!(a, b);
        assert!(
            a.findings()
                .iter()
                .any(|item| item.code() == "PDFUA1.ALT_TEXT_MISSING")
        );
    }

    #[test]
    fn t0178_certification_is_false_and_never_claimed() {
        const { assert!(!CERTIFICATION) };
        let input = conforming(Standard::PdfA1b);
        let report = check(Standard::PdfA1b, &input);
        assert!(report.is_empty());
        let summary = PdfA1bProfile::new().report(&input).summary();
        assert!(summary.contains("conforming"));
        assert!(!summary.contains("certified"));
    }
}
