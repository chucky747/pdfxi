//! Import and export converters for images, text, HTML, Office formats, and print capture.
//!
//! This crate freezes the conversion contracts described in
//! `docs/ARCHITECTURE.md` without pulling in an external converter:
//!
//! - [`SourceKind`] and [`ExportKind`] classify file names into adapter
//!   families. Classification is pure and never touches the file system.
//! - [`ConversionOptions`] carries page range, image, annotation, and OCR
//!   settings shared by import and export.
//! - [`ConversionError`] reports unsupported formats and invalid input with
//!   actionable messages.
//! - [`Converter`] is the trait implemented by optional external adapters;
//!   [`MockConverter`] is a deterministic in-memory implementation for tests.
//! - [`encode_png`], [`encode_ppm`], [`encode_bmp`], [`encode_tiff`], and
//!   [`encode`] are pure-Rust image encoders that need no image crate. They turn
//!   caller-supplied RGBA pixels into bounded, deterministic bytes and are
//!   always available, even with `--no-default-features`. A baseline JPEG
//!   encoder remains in the verified `pdfxi-render` export module.
//! - [`export_pages`], [`ImageExportPlan`], [`PageSelection`], and
//!   [`ExportedImage`] export selected pages through any renderer. They are
//!   gated behind the default `renderer` feature so the crate can be used
//!   dependency-free by callers that only need classification or encoding.
//!   The `pdfium` feature adds `export_pdf_bytes` and `export_pdf_file`,
//!   which construct a `PdfiumRenderer` for convenience; the core API stays
//!   renderer-agnostic.
//! - [`export_text`], [`TextExportPlan`], [`TextPageSelection`], and
//!   [`TextEncoding`] turn pre-extracted `(page, text)` pairs into deterministic
//!   plain text with documented separators, headers, and trailing-newline
//!   rules.
//! - [`DocumentJson`], [`DocumentMetadata`], [`JsonPage`], and [`JsonSpan`]
//!   model a versioned structured snapshot serialized by a hand-written,
//!   dependency-free JSON writer with stable key order and correct escaping.
//! - [`RtfPage`], [`RtfOptions`], [`export_rtf`], and [`export_rtf_string`]
//!   write deterministic RTF with a fixed header/font/colour table, paragraph
//!   breaks, page breaks, and signed `\uN?` escapes for non-ASCII text.
//! - [`TaggedTextOptions`], [`TaggedExport`], [`export_tagged_text`],
//!   [`export_structured_text`], and [`export_plain_text`] serialize a
//!   `pdfxi_accessibility::A11yTree` into a nested `@role` outline and a flat
//!   `[role]` plain-text variant, honouring page, alt-text, heading-level, and
//!   line-ending options.
//! - [`LineEnding`], [`ExportPageSelection`], and [`ExportOptionError`] are the shared,
//!   typed option primitives used by both writers.
//! - [`TextConverter`] is a [`Converter`] adapter that exports pre-extracted
//!   page data as [`ExportKind::Text`] or [`ExportKind::Json`], keeping parsing
//!   and rendering out of the export path.
//! - [`create`] writes new PDFs from plain text, a small Markdown subset, and
//!   PNG/JPEG images. It has its own deterministic classic-xref writer, a
//!   [`CreateError`] derived from `thiserror`, documented
//!   page/margin/font metrics and DPI/page-fit rules, and embeds raw image
//!   streams without a decoder dependency. This is the only module that adds a
//!   runtime dependency (`thiserror`, already in the workspace and lockfile);
//!   `pdfxi-core` is used only as a dev-dependency for round-trip tests.
//! - [`conformance`] normalizes raw PDF bytes for a declared subset of
//!   PDF/A, PDF/X and PDF/E targets. It adds XMP and output-intent placeholders,
//!   raises the header version to the target minimum, regenerates the xref,
//!   rejects unsupported or contradictory requests with typed errors, and
//!   returns a report of enforced versus reported-only constraints. It never
//!   claims certification.
//! - [`create_from_html`], [`create_from_html_with_options`], [`HtmlOptions`],
//!   and [`create_pdf_from_html_file`] add a bounded, dependency-free HTML
//!   importer: a documented element/CSS subset, typed rejection of active
//!   content, remote URLs, path traversal and oversized input, local PNG/JPEG
//!   embedding, and deterministic pagination through the [`create`] engine.
//! - [`html_export`] writes deterministic, inert HTML from a positioned
//!   page/text model: absolutely positioned spans (with PDF-to-CSS coordinate
//!   inversion), page navigation, allow-listed hyperlinks, and local image
//!   references only when the caller supplies an already-extracted, safe
//!   filename. It emits no script and fetches nothing.
//! - [`capture`] replaces the retired browser-capture extension with local,
//!   caller-supplied inputs: a saved HTML file, supplied markup, or an MHTML-like
//!   payload. It renders through the bounded HTML subset, fetches nothing, and
//!   exposes typed size/active-content/traversal errors.
//! - [`email`] replaces retired email-capture services by parsing a saved
//!   `.eml` message (folded headers, MIME parts, text/plain and text/html bodies)
//!   into a PDF. Attachments are listed by name only and never decoded.
//! - [`office_export`] writes editable `.docx`, `.xlsx`, and `.pptx` archives
//!   from a caller-supplied extraction model using only self-generated ZIP and
//!   XML. The three local adapters implement a typed [`OfficeExporter`]
//!   contract, validate their own ZIP structure and required parts, and return
//!   an explicit report of unsupported content. No Office installation or
//!   network access is involved.
//! - [`postscript`] writes a deterministic, DSC-conformant PostScript program
//!   that draws text, rectangles, and lines with standard fonts and shows every
//!   page. Unrepresentable glyphs and image content are skipped and recorded in
//!   an explicit [`PsExportReport`] rather than silently faked.
//! - [`postscript_ingest`] is the bounded read counterpart: it parses DSC headers
//!   (`%%BoundingBox`, `%%Pages`, `%%Page`), detects EPS versus PS, and extracts
//!   a documented text/line/rectangle subset into the same [`PsContent`] model.
//!   A full interpreter is deliberately out of scope; every other construct is
//!   surfaced in a deterministic [`PsIngestReport`] or a typed strict-mode error.
//! - [`office_import`] accepts caller-supplied Office-derived HTML/XML bundles
//!   (no ZIP, COM, or network) and maps a documented markup subset onto the
//!   [`OfficeDocument`] model, with bounded input, a deterministic report, and
//!   typed errors for active content and remote references.
//! - [`print_capture`] is the local print-capture ingestion contract for
//!   "create PDF from any printable application" (CREATE-001). It orders
//!   caller-supplied rendered pages, PostScript, and HTML into a deterministic
//!   assembly plan with shared page-size validation and typed errors.
//! - [`print_job`] models a virtual PDF printer's job envelope (CREATE-002):
//!   id, source app, paper, orientation, copies, and an expanded page range,
//!   validated into a deterministic plan. No spooler or driver is used.
//! - [`portfolio`] models a customizable PDF Portfolio manifest (CREATE-010):
//!   named, described, ordered entries with traversal/duplicate validation and
//!   a deterministic XML descriptor serialized through the Office XML helpers.
//!
//! The text/JSON modules are always compiled and add no dependencies, so a
//! `--no-default-features` build stays dependency-free.
//!
//! PDF documents cross the trait boundary as raw bytes. A caller holding a
//! `pdfxi_core::Document` can pass its serialized bytes, which keeps this crate
//! free of workspace and third-party dependencies (veraPDF and LibreOffice are
//! never run here).
//!
//! # Examples
//!
//! The example below is rendered as text rather than compiled as a doctest:
//! the build host denies execution of generated doctest binaries (os error 5).
//! The same behaviour is covered by executable unit tests in
//! `converter::tests`.
//!
//! ```text
//! use std::path::Path;
//! use pdfxi_convert::{Converter, ExportKind, MockConverter, SourceKind};
//!
//! let converter = MockConverter::new().with_input("report.txt", b"pdf".to_vec());
//! assert!(converter.can_import(SourceKind::Text));
//! let pdf = converter.import(Path::new("report.txt")).expect("import");
//! let text = converter.export(&pdf, ExportKind::Text).expect("export");
//! assert!(text.starts_with(b"mock:text:"));
//! ```

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod capture;
pub mod conformance;
mod converter;
pub mod create;
pub mod email;
mod error;
mod export;
mod export_options;
pub mod html_export;
pub mod image;
mod json;
mod mime;
pub mod office_export;
pub mod office_import;
mod options;
#[cfg(feature = "renderer")]
mod page_export;
pub mod portfolio;
pub mod postscript;
pub mod postscript_ingest;
pub mod preflight;
pub mod print_capture;
pub mod print_job;
mod rtf;
mod source;
mod tagged_text;
mod text;
mod text_converter;

pub use capture::{
    CAPTURE_MAX_BYTES, CAPTURE_MAX_PARTS, CaptureError, CaptureOptions, CaptureResult,
    capture_html_file, capture_html_markup, capture_mhtml,
};
pub use conformance::{
    ConformanceExport, ConformanceExportError, ConformanceExportReport, ConformanceFinding,
    ConformanceRequest, ConformanceTarget, ExportSeverity, NormalizationOptions,
    REPORTED_ONLY_CONSTRAINTS, SCOPE_NOTE, export_conformance_pdf, looks_like_normalized_pdf,
};
pub use converter::{Converter, MockConverter};
pub use create::{
    CreateError, CreateImageFormat, CreateOptions, DEFAULT_DPI, HTML_MAX_INPUT_BYTES, HtmlOptions,
    Margins, PageSize, create_from_html, create_from_html_with_options, create_from_image,
    create_from_markdown, create_from_text, create_pdf_from_file, create_pdf_from_html_file,
    create_pdf_from_html_file_with_options, create_pdf_from_path,
};
pub use email::{
    EMAIL_MAX_ATTACHMENTS, EMAIL_MAX_BYTES, EMAIL_MAX_DEPTH, EMAIL_MAX_PARTS, EmailBodyKind,
    EmailDocument, EmailError, EmailLimits, EmailOptions, EmailPdf, create_pdf_from_eml,
    create_pdf_from_eml_file, parse_eml, parse_eml_with_limits, render_email_pdf,
};
pub use error::ConversionError;
pub use export::ExportKind;
pub use export_options::{ExportOptionError, ExportPageSelection, LineEnding};
pub use html_export::{
    HtmlDocument, HtmlExportError, HtmlLink, HtmlOptions as HtmlExportOptions, HtmlOutputMode,
    HtmlPage, HtmlSpan, MAX_PAGES as HTML_MAX_PAGES, NamedHtml, export_html, export_html_string,
};
pub use image::{
    ImageFormat, MAX_IMAGE_BYTES, MAX_IMAGE_DIMENSION, MAX_IMAGE_PIXELS, encode, encode_bmp,
    encode_png, encode_ppm, encode_tiff,
};
pub use json::{DocumentJson, DocumentMetadata, JSON_SCHEMA_VERSION, JsonPage, JsonSpan};
pub use office_export::{
    CellValue, DocxExportError, DocxExporter, ModelError, OfficeBlock, OfficeCell, OfficeDocument,
    OfficeExport, OfficeExportError, OfficeExportReport, OfficeExporter, OfficeFormat, OfficePage,
    PptxExportError, PptxExporter, ReportedUnsupported, SheetMode, XlsxExportError, XlsxExporter,
};
pub use office_import::{
    OFFICE_BUNDLE_MAX_BYTES, OFFICE_BUNDLE_PAGE_HEIGHT, OFFICE_BUNDLE_PAGE_WIDTH, OfficeBundle,
    OfficeBundleKind, OfficeImport, OfficeImportError, OfficeImportFinding, OfficeImportReport,
    PAGE_BREAK_MARKER, import_office_bundle, import_office_bundle_strict,
    import_office_bundle_with_report,
};
pub use options::ConversionOptions;
#[cfg(feature = "renderer")]
pub use page_export::{
    ExportedImage, ImageExportPlan, PageSelection, RenderResolution, export_pages,
};
#[cfg(feature = "pdfium")]
pub use page_export::{export_pdf_bytes, export_pdf_file};
pub use portfolio::{
    PORTFOLIO_MAX_DESCRIPTION_BYTES, PORTFOLIO_MAX_ENTRIES, PORTFOLIO_MAX_NAME_BYTES,
    PORTFOLIO_MAX_TITLE_BYTES, PortfolioEntry, PortfolioError, PortfolioManifest,
    serialize_portfolio,
};
pub use postscript::{
    MAX_PAGES as POSTSCRIPT_MAX_PAGES, PsContent, PsDocument, PsExportError, PsExportReport,
    PsFont, PsImageRef, PsLine, PsPage, PsRect, PsText, PsUnsupportedItem, export_postscript,
    export_postscript_with_report,
};
pub use postscript_ingest::{
    PS_INGEST_MAX_BYTES, PS_INGEST_MAX_LINES, PS_INGEST_MAX_PAGES, PS_INGEST_MAX_TOKENS_PER_PAGE,
    PostScriptKind, PsBoundingBox, PsIngestError, PsIngestOptions, PsIngestReport,
    PsUnsupportedFinding, ingest_postscript, ingest_postscript_str, ingest_postscript_strict,
    ingest_postscript_with_options,
};
pub use preflight::{
    BytePreflightError, BytePreflightReport, PreflightOptions, PreflightReport,
    REPORTED_ONLY_CHECKS, build_preflight_input, preflight_bytes,
};
pub use print_capture::{
    CapturePage, CaptureSource, PRINT_CAPTURE_MAX_LABEL_BYTES, PRINT_CAPTURE_MAX_PAGES,
    PlannedCapturePage, PrintCaptureError, PrintCaptureOptions, PrintCapturePlan,
    build_print_capture_plan,
};
pub use print_job::{
    Orientation, PRINT_MAX_COPIES, PRINT_MAX_JOB_ID_BYTES, PRINT_MAX_PAGES, PRINT_MAX_RANGE_TOKENS,
    PRINT_MAX_SOURCE_APP_BYTES, PageRange, PaperSize, PrintJob, PrintJobError, PrintJobPlan,
    parse_page_range,
};
pub use rtf::{RtfOptions, RtfPage, export_rtf, export_rtf_string};
pub use source::SourceKind;
pub use tagged_text::{
    TaggedExport, TaggedTextOptions, export_plain_text, export_structured_text, export_tagged_text,
};
pub use text::{
    DEFAULT_PAGE_SEPARATOR, PAGE_HEADER_PATTERN, TextEncoding, TextExportPlan, TextPageSelection,
    export_text,
};
pub use text_converter::TextConverter;

/// Return the implementation name for diagnostics.
pub fn engine_name() -> &'static str {
    "pdfxi-convert"
}
