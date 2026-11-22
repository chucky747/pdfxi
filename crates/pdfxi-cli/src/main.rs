//! Command-line interface for PDFXI.
//!
//! The CLI is the headless surface for automation and CI. It shares the same
//! core and feature crates as the desktop application, so every operation that
//! can be scripted is also available to the UI.
//!
//! The `render` subcommand is compiled in both configurations. With the
//! `pdfium` Cargo feature it rasterizes a page through the `pdfxi-render`
//! `Renderer` contract and writes a PNG or PPM; without it, the command exits
//! with an explanatory error instead of failing to compile.
//!
//! The `search` subcommand builds a `pdfxi-text` [`SearchIndex`]
//! from one text page per PDF page and reports matches as one-based page numbers
//! with zero-based character offsets. Offsets are Unicode scalar values (not
//! bytes) into each page's concatenated text, the same offset space used by
//! `pdfxi-text`.
//!
//! The `action` command group loads named actions from a tab-delimited action
//! file and runs them through the `pdfxi-automation` `DocumentExecutor`. The
//! companion `droplet` command is the same run with Explorer-friendly defaults;
//! see the `action` module for the staging-only executor and drop-handler notes.
//!
//! The `create` command group builds new PDFs from PNG/JPEG images and from
//! plain text or a small Markdown subset. The `export tagged` command writes a
//! structured text representation of a PDF's accessible content (roles, reading
//! order and text); it is not a tagged-PDF producer or a PDF/UA certification.
//!
//! The `api` command exposes the same automation operations as a local,
//! versioned, line-delimited JSON protocol on stdin/stdout. It opens no sockets
//! and shares the action executor with `action run`; `--once` answers a single
//! request so scripts can use it as a one-shot filter.
//!
//! The `preflight` command runs a deterministic, byte-level subset preflight
//! profile through `pdfxi-convert`'s adapter over `pdfxi-standards`; it exits 0
//! for a clean report and 2 for error-severity findings, and never claims
//! certification. The `marks` command applies crop, registration, colour-bar
//! and page-information marks to every page through `pdfxi-core`'s application
//! API, reparses the result, and writes deterministic bytes.
//!
//! The `repair` command exposes `pdfxi-core`'s `repair_document` path: it reads
//! a damaged PDF, writes a verified standalone PDF to a distinct output path,
//! and reports the deterministic typed repairs. It refuses to overwrite its
//! input, rejects `..` traversal in the output path, and exits 0 on success,
//! 1 for CLI/I/O errors or 3 when the input is unrecoverable. `--json` emits a
//! stable report whose `warnings` array carries one code/message pair per typed
//! core warning.

//! The `compat` command prints the PDF version compatibility report for a file:
//! the header and effective version, a catalog `/Version` override, warnings,
//! every version-dependent feature gate (1.0-2.0), and the constructs a byte
//! scan can see. `--strict` exits 3 when the file uses a construct its version
//! does not allow, so scripts can gate on it.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

// The encoders are only reachable from the PDFium-backed `render` path; gating
// the module keeps default builds free of unused-code warnings.
#[cfg(feature = "pdfium")]
mod image;

// Action-file commands (`action` and `droplet`) built on pdfxi-automation.
mod action;

// Local line-delimited JSON automation API over stdin/stdout (`api`).
/// pdfxi accessibility alt/table: edit tagged structure entries.
mod a11y_edit;
/// pdfxi accessibility tree/nav/layout: window-free accessibility hosts.
mod a11y_nav;
mod api;
/// pdfxi color: ICC profiles, output intents and analytic transforms.
mod color;
/// pdfxi create portfolio: assemble a PDF portfolio from real files.
mod create_portfolio;
/// pdfxi envelope: authenticated local security envelopes.
mod envelope;
/// pdfxi export postscript: positioned text subset as DSC PostScript.
mod export_postscript;
/// pdfxi find-files: cross-file text search over a folder.
mod find_files;
/// pdfxi geo: geospatial viewports and coordinate mapping.
mod geo;
/// pdfxi handoff: hand a local export to the browser.
mod handoff;
/// pdfxi home: recents, favorites and collections over a state file.
mod home;
/// pdfxi id: a metadata-only catalog of local digital IDs.
mod id;
/// pdfxi interactions: audit /AA additional actions.
mod interactions;
/// pdfxi media: media annotations and the HTML5 replacement envelope.
mod media;
/// pdfxi media3d: read /3D annotations and extract payloads.
mod media3d;
/// pdfxi create ocr: overlay an invisible searchable-text layer.
mod ocr;
/// pdfxi create office: convert an Office intake bundle into a PDF.
mod office;
/// pdfxi office: plan and execute the Office integration contract.
mod office_roundtrip;
/// pdfxi ops: enumerate and resolve scriptable automation operations.
mod ops;
/// pdfxi plugin: plan plugin invocations and validate typed outputs.
mod plugin;
/// pdfxi policy: deployment preferences and customization enforcement.
mod policy;
/// pdfxi protect/unprotect: standard security handler output.
mod protect;
/// pdfxi restyle: change font size, spacing, scaling and colour.
mod restyle;
/// pdfxi create scan: assemble scanned PNG pages into a PDF.
mod scan;
/// pdfxi script: run a file in the sandboxed JavaScript subset.
mod script;
/// pdfxi separations: derive print plates from real page content.
mod separations;
/// pdfxi shell: file associations and the pdfxi:// protocol.
mod shell;
/// pdfxi sign: real CMS/RSA signatures over a PDF byte range.
mod sign;
/// pdfxi signature: signature fields, byte-range planning and verification.
mod signature;
/// pdfxi toolset: parse, inspect and run named tool aliases.
mod toolset;
/// pdfxi transparency inspect: read blend modes and transparency groups.
mod transparency;
/// pdfxi unicode analyze: scripts, bidi runs and font fallback.
mod unicode;
/// pdfxi watch: one host-driven poll of a watched folder.
mod watch;

// PDF creation (`create`) and accessible-text export (`export`) built on
// pdfxi-convert and the accessibility tree builder.
mod create;
mod export;

// Byte-level subset preflight (`preflight`) and printer-mark placement
// (`marks`) added for T-0131. Both use the core application API.
/// pdfxi loupe: window-free magnifier and gesture navigation.
mod loupe;
mod marks;
mod preflight;
/// pdfxi prepress: trapping presets and flattening policy.
mod prepress;
/// pdfxi present: window-free presentation-mode planning.
mod present;
mod production;
/// pdfxi protection: policy-driven document protection.
mod protection;
/// pdfxi trust: offline chain and revocation evaluation.
mod trust;
// Virtual PDF printer control: install/status/remove the "PDFXI PDF" queue and
// capture one finished job (`printer`).
mod printer;
// Page-level editing: extract, delete, move, reorder, rotate, insert, replace,
// merge and split (`pages`).
mod pages;
// Link annotations and embedded-file attachments (`links`, `attachments`).
/// `pdfxi accessibility check`: run the accessibility and PDF/UA-1 checkers.
mod accessibility;
/// `pdfxi annotate`: add and list drawing annotations.
mod annotate;
/// `pdfxi compare`: side-by-side revision compare of two PDFs.
mod compare;
/// `pdfxi export conformance`: normalize for PDF/A, PDF/X and PDF/E targets.
mod conformance;
/// `pdfxi decorate`: stamp headers and footers on pages.
mod decorate;
/// `pdfxi font add-text`: embed the CID font and write text with it.
mod font;
/// `pdfxi form create` and `pdfxi form list`: build and inspect AcroForm fields.
mod form;
/// `pdfxi linearize` and `pdfxi pack`: choose the on-disk object layout.
mod layout;
mod links;
/// `pdfxi measure`: page-space distance, area and perimeter.
mod measure;
/// `pdfxi outline`, `pdfxi link` and `pdfxi attach`: edit bookmarks, links
/// and embedded files (`navigation`).
mod navigation;
/// `pdfxi optimize`: rewrite a document with the core optimizer.
mod optimize;
/// `pdfxi portfolio`: list and extract a PDF portfolio collection.
mod portfolio;
/// `pdfxi read-aloud`: build the deterministic utterance queue.
mod read_aloud;
/// `pdfxi reflow`: lay page text out as a narrow reading column.
mod reflow;
/// `pdfxi replace`: literal find-and-replace across a document.
mod replace;
/// `pdfxi structure apply`: write a tagged-PDF structure tree.
mod structure;
/// `pdfxi update`: append an incremental revision with new information.
mod update;
/// `pdfxi versions`: store page-text snapshots and diff revisions.
mod versions;
// Optional-content layers (`layers`).
mod layers;
mod repair;
/// `pdfxi action sequence`: run Action Wizard steps over real documents.
mod sequence;
/// `pdfxi timestamp`: mint and validate RFC 3161 timestamp tokens.
mod timestamp;

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use pdfxi_core::Document;
use pdfxi_text::{SearchIndex, SearchOptions, TextPage, TextSpan};
use serde::Serialize;

#[cfg(feature = "pdfium")]
use pdfxi_render::{CancellationToken, PageIndex, PdfiumRenderer, Renderer};

/// Largest scale accepted by `render`, bounding raster memory use.
#[cfg(feature = "pdfium")]
const MAX_RENDER_SCALE: f32 = 8.0;

/// PDFXI command-line interface.
#[derive(Debug, Parser)]
#[command(
    name = "pdfxi",
    version,
    about = "PDFXI ÃƒÂ¢Ã¢â€šÂ¬Ã¢â‚¬Â a Rust PDF workbench"
)]
struct Cli {
    /// The command to run.
    #[command(subcommand)]
    command: Command,
}

/// Available commands.
#[derive(Debug, Subcommand)]
enum Command {
    /// Print basic information about a PDF file.
    Info {
        /// Path to the PDF file.
        file: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Report the PDF version (1.0-2.0) and which version-dependent features
    /// the file may use.
    ///
    /// Reads the `%PDF-` header, honours a catalog `/Version` override, evaluates
    /// the twelve version-dependent feature gates, and scans the bytes for the
    /// constructs those gates describe. `--strict` exits 3 when a construct is
    /// present but the effective version does not allow it.
    Compat {
        /// Path to the PDF file.
        file: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
        /// Exit 3 when a detected construct exceeds the effective version.
        #[arg(long)]
        strict: bool,
    },
    /// Extract plain text from a PDF file.
    ///
    /// Text is read from uncompressed page content streams. The `Tj`, `TJ`, `'`
    /// and `"` operators are supported; a file whose content stream uses a
    /// filter is reported as an error instead of producing undecoded output.
    Text {
        /// Path to the PDF file.
        file: PathBuf,
    },
    /// Find text across every page of a PDF.
    ///
    /// One `pdfxi-text` `TextPage` is built per page from the core's extracted
    /// text, then searched through a `SearchIndex`. Results are printed as
    /// `page N offset M matched text` with `N` one-based and `M` a zero-based
    /// character offset (Unicode scalar values, not bytes) into the matched
    /// page's concatenated text. Matching is case-insensitive by default.
    /// When nothing matches, the command exits 0, writes no lines to stdout and
    /// writes `no matches` to stderr so an empty result is never mistaken for
    /// an error.
    Search {
        /// Path to the PDF file.
        file: PathBuf,
        /// Text to search for; an empty query yields no matches.
        query: String,
        /// Match case exactly instead of folding ASCII case.
        #[arg(long)]
        case_sensitive: bool,
        /// Accept a match only when its neighbours are non-word characters or
        /// page boundaries.
        #[arg(long)]
        whole_word: bool,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Replace literal text across the document's extracted page text.
    ///
    /// Matches are rewritten inside the single text run that contains them and
    /// the result is saved, reopened and re-extracted. Matches spanning two runs
    /// are reported as remaining rather than merged.
    Replace {
        /// Path to the PDF file.
        file: PathBuf,
        /// Destination PDF.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Literal text to find.
        #[arg(long, value_name = "TEXT")]
        find: String,
        /// Replacement text.
        #[arg(long = "replace", value_name = "TEXT")]
        replacement: String,
        /// Match case exactly.
        #[arg(long)]
        case_sensitive: bool,
        /// Accept a match only at word boundaries.
        #[arg(long)]
        whole_word: bool,
        /// Fold common diacritics before matching.
        #[arg(long)]
        diacritic_insensitive: bool,
        /// Maximum replacements; zero uses the built-in limit of 1000.
        #[arg(long, default_value_t = 0)]
        limit: usize,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Measure distance, area or perimeter on a page.
    ///
    /// Two points produce a distance; three or more produce polygon area and
    /// perimeter. Points are page coordinates and must lie inside the media box.
    Measure {
        /// Path to the PDF file.
        file: PathBuf,
        /// One-based page number.
        #[arg(long, default_value_t = 1)]
        page: usize,
        /// Points as `x,y;x,y[;x,y...]` in PDF page coordinates.
        #[arg(long, value_name = "POINTS")]
        points: String,
        /// Display unit: pt (default), in, mm, cm or ft.
        #[arg(long, value_name = "UNIT")]
        unit: Option<String>,
        /// Treat the page as scanned at this DPI and report inches.
        #[arg(long, value_name = "DPI")]
        dpi: Option<f64>,
        /// Rounding precision, 0-6.
        #[arg(long, default_value_t = 2)]
        decimals: u8,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Rasterize one page to a PNG or PPM image.
    ///
    /// `PAGE` is zero-based and `SCALE` is a factor applied to a 72-DPI page
    /// (1.0 means one pixel per PDF point). Requires the `pdfium` Cargo
    /// feature; builds without it print a clear error and exit non-zero.
    Render {
        /// Path to the PDF file.
        file: PathBuf,
        /// Zero-based page index to render.
        #[arg(long, value_name = "N")]
        page: usize,
        /// Positive, finite scale factor; 1.0 is 72 DPI.
        #[arg(long, value_name = "S", allow_negative_numbers = true)]
        scale: f32,
        /// Destination path for the image.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Output encoding: `png` (the default) or `ppm`.
        #[arg(long, value_enum, default_value = "png")]
        format: OutputFormat,
    },
    /// Edit pages: extract, delete, move, reorder, rotate, insert, replace,
    /// merge or split.
    ///
    /// Every command writes a standalone PDF built from the real page objects;
    /// the source file is never modified. Page numbers are one-based; `--pages`
    /// accepts `all`, lists (`1,3`) and ranges (`2-4`).
    Pages {
        /// The page operation to perform.
        #[command(subcommand)]
        command: PagesCommand,
    },
    /// List optional-content layers and whether the default view shows them.
    Layers {
        /// Path to the PDF file.
        file: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// List link annotations and where they point.
    Links {
        /// Path to the PDF file.
        file: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// List embedded-file attachments.
    Attachments {
        /// Path to the PDF file.
        file: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
        /// Write every attachment into this directory.
        #[arg(long, value_name = "DIR")]
        extract: Option<PathBuf>,
    },
    /// Run a script in the deterministic Acrobat-compatible sandbox.
    ///
    /// Scripts have no file, network, process, clock or random access. Form
    /// fields are seeded in memory with `--field NAME=VALUE`, and the final
    /// store is reported. Explicit CPU, call, loop, string, array, object and
    /// nesting budgets apply.
    Script {
        /// Script action.
        #[command(subcommand)]
        command: ScriptCommand,
    },
    /// Plan or execute the Word/Excel/PowerPoint integration contract.
    ///
    /// `plan` validates the envelope and prints the deterministic adapter
    /// steps; `convert` executes them and re-reads the output. `import-to-pdf`
    /// turns Office-derived markup into a PDF; `pdf-to-office` turns recovered
    /// page runs and tables into a DOCX/XLSX/PPTX archive. No Office process or
    /// COM object is involved.
    Office {
        /// Integration action.
        #[command(subcommand)]
        command: OfficeCommand,
    },
    /// Inspect ICC profiles and output intents, and convert colours through a
    /// profile's analytic plan.
    Color {
        /// Colour action.
        #[command(subcommand)]
        command: ColorCommand,
    },
    /// Read geospatial viewports and map PDF/geo coordinates.
    Geo {
        /// Geospatial action.
        #[command(subcommand)]
        command: GeoCommand,
    },
    /// Inspect media annotations or write the HTML5 replacement envelope.
    Media {
        /// Media action.
        #[command(subcommand)]
        command: MediaCommand,
    },
    /// Audit `/AA` additional actions against the safe-action whitelist.
    Interactions {
        /// Path to the PDF file.
        file: PathBuf,
        /// Exit non-zero when any action is rejected.
        #[arg(long)]
        strict: bool,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Seal, open or verify an authenticated local security envelope.
    Envelope {
        /// Envelope action.
        #[command(subcommand)]
        command: EnvelopeCommand,
    },
    /// Mint and validate RFC 3161 timestamp tokens.
    Timestamp {
        /// Timestamp action.
        #[command(subcommand)]
        command: TimestampCommand,
    },
    /// Inspect, catalogue, list or remove local digital IDs.
    Id {
        /// Digital-ID action.
        #[command(subcommand)]
        command: IdCommand,
    },
    /// Sign a PDF with a real CMS/RSA signature, or verify one.
    Sign {
        /// Signature action.
        #[command(subcommand)]
        command: SignCommand,
    },
    /// Create signature fields and plan or verify the signer byte range.
    Signature {
        /// Signature action.
        #[command(subcommand)]
        command: SignatureCommand,
    },
    /// Hand a local export to the browser, or build its `file:` URL.
    Handoff {
        /// Handoff action.
        #[command(subcommand)]
        command: HandoffCommand,
    },
    /// Write a password-protected PDF with the standard security handler.
    Protect {
        /// Input PDF.
        file: PathBuf,
        /// Output PDF.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// User (open) password.
        #[arg(long, value_name = "PASSWORD")]
        password: String,
        /// Owner (permissions) password.
        #[arg(long, value_name = "PASSWORD")]
        owner: Option<String>,
        /// Method: `rc4-40`, `rc4-128`, `aes-128` or `aes-256`.
        #[arg(long, default_value = "aes-256")]
        method: String,
        /// Permissions: `all`, `none`, `read-only` or a comma list.
        #[arg(long, default_value = "all")]
        permissions: String,
        /// Security handler revision override.
        #[arg(long, value_name = "N")]
        revision: Option<u8>,
        /// Encrypt document metadata.
        #[arg(long, default_value_t = false, action = clap::ArgAction::Set)]
        encrypt_metadata: bool,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Remove password protection from a PDF.
    Unprotect {
        /// Input PDF.
        file: PathBuf,
        /// Output PDF.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// User or owner password.
        #[arg(long, value_name = "PASSWORD")]
        password: String,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Evaluate an X.509 chain against caller-supplied anchors and CRLs.
    Trust {
        /// Trust action.
        #[command(subcommand)]
        command: TrustCommand,
    },
    /// Register, inspect or remove per-user file associations and the protocol.
    Shell {
        /// Shell action.
        #[command(subcommand)]
        command: ShellCommand,
    },
    /// Read, write, merge and enforce deployment preferences.
    Policy {
        /// Policy action.
        #[command(subcommand)]
        command: PolicyCommand,
    },
    /// Show the ink-manager view: plates, alternates and overprint counts.
    Ink {
        /// Path to the PDF file.
        file: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Derive print plates (process and spot inks) from real page content.
    Separations {
        /// Path to the PDF file.
        file: PathBuf,
        /// Show the output preview for one plate name.
        #[arg(long, value_name = "NAME")]
        plate: Option<String>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Inspect `/3D` annotations and optionally extract the U3D/PRC payload.
    Media3d {
        /// Path to the PDF file.
        file: PathBuf,
        /// Write the decoded `/3DD` payload to this file.
        #[arg(long, value_name = "PATH")]
        extract: Option<PathBuf>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Manage recent documents, favorites and collections in a state file.
    Home {
        /// Home action.
        #[command(subcommand)]
        command: HomeCommand,
    },
    /// Analyze scripts, bidi runs and font fallback for PDF text.
    ///
    /// Reads the document's real page text (or `--text`) and reports the
    /// dominant script, base direction, logical bidi runs and, when
    /// `--font FAMILY=SCRIPTS` candidates are supplied, the ordered fallback
    /// chain per run. No shaping or rasterization is performed.
    Unicode {
        /// Analysis action.
        #[command(subcommand)]
        command: UnicodeCommand,
    },
    /// Search the text of PDF files in a folder or explicit list.
    ///
    /// Files rank by total match count, with a path tie-break; matches are
    /// grouped by file then page. `--max-results` and `--max-per-file` cap the
    /// reported matches and set `truncated`.
    FindFiles {
        /// Files or directories to search.
        #[arg(value_name = "PATH", required = true, num_args = 1..)]
        inputs: Vec<PathBuf>,
        /// Text to find.
        #[arg(long, value_name = "TEXT")]
        query: String,
        /// Match case exactly.
        #[arg(long)]
        case_sensitive: bool,
        /// Require non-word boundaries around a match.
        #[arg(long)]
        whole_word: bool,
        /// Maximum matches reported overall.
        #[arg(long, value_name = "N")]
        max_results: Option<usize>,
        /// Maximum matches reported per file.
        #[arg(long, value_name = "N")]
        max_per_file: Option<usize>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Inspect page transparency groups and graphics-state blend modes.
    ///
    /// Reads the real `/Group` and `/ExtGState` dictionaries, validates every
    /// mode and alpha through `pdfxi-core::transparency`, and prints canonical
    /// descriptors. Pixel compositing stays a render-backend responsibility.
    Transparency {
        /// Inspection action.
        #[command(subcommand)]
        command: TransparencyCommand,
    },
    /// Change font size, spacing, scaling or colour of a paragraph.
    ///
    /// The run selects the block; the whole block is re-emitted with the
    /// requested style and reflowed. The font family is unchanged. The output is
    /// reopened and the target text must still extract.
    Restyle {
        /// Path to the PDF file.
        file: PathBuf,
        /// Where to write the restyled document.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Zero-based page index.
        #[arg(long, default_value_t = 0)]
        page: usize,
        /// Zero-based run index on that page.
        #[arg(long, default_value_t = 0)]
        run: usize,
        /// New font size in points.
        #[arg(long, value_name = "PT")]
        size: Option<f64>,
        /// Horizontal scaling in percent (`100` is normal).
        #[arg(long, value_name = "PCT")]
        scale: Option<f64>,
        /// Extra character spacing in unscaled text units.
        #[arg(long, value_name = "PT")]
        char_spacing: Option<f64>,
        /// Extra word spacing in unscaled text units.
        #[arg(long, value_name = "PT")]
        word_spacing: Option<f64>,
        /// Fill colour as `R,G,B` in 0..=1.
        #[arg(long, value_name = "R,G,B")]
        color: Option<String>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Browse or extract a PDF portfolio collection.
    ///
    /// A portfolio is a PDF whose `/Collection` dictionary and
    /// `/Names /EmbeddedFiles` tree describe a folder of documents. Listing
    /// prints one row per file with its `/UF` folder path, media type and
    /// description; `--extract` writes the embedded bytes into a directory,
    /// recreating the folders and refusing every path that escapes it.
    Portfolio {
        /// Path to the PDF file.
        file: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
        /// Write every portfolio file into this directory.
        #[arg(long, value_name = "DIR")]
        extract: Option<PathBuf>,
        /// Destination for a management edit.
        #[arg(long, value_name = "PATH")]
        out: Option<PathBuf>,
        /// Add a file as `LOCAL=IN-PORTFOLIO-NAME`.
        #[arg(long = "add", value_name = "LOCAL=NAME")]
        add: Vec<String>,
        /// Rename or move an entry as `OLD=NEW`.
        #[arg(long = "rename", value_name = "OLD=NEW")]
        rename: Vec<String>,
        /// Set an entry description as `NAME=TEXT`.
        #[arg(long = "describe", value_name = "NAME=TEXT")]
        describe: Vec<String>,
        /// Remove an entry by name.
        #[arg(long = "remove", value_name = "NAME")]
        remove: Vec<String>,
    },
    /// Compare two PDFs page by page, side by side.
    ///
    /// Pages are aligned by their extracted text, so an inserted or deleted page
    /// is reported as such instead of shifting every later page. Aligned pairs
    /// whose text differs are reported as `changed`; `--lines` prints the
    /// differing lines as two columns and `--fail-on-difference` makes a
    /// difference fail the command for scripts.
    Compare {
        /// Left document (the "before" revision).
        left: PathBuf,
        /// Right document (the "after" revision).
        right: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
        /// Print the differing lines of each changed page as two columns.
        #[arg(long, conflicts_with = "hunks")]
        lines: bool,
        /// Report Added/Removed/Changed line hunks with page and line numbers.
        #[arg(long)]
        hunks: bool,
        /// Exit non-zero when the documents are not identical.
        #[arg(long)]
        fail_on_difference: bool,
    },
    /// Reflow a page's extracted text into a narrow reading column.
    ///
    /// The column width is plain character count: paragraphs are split on blank
    /// lines and each paragraph is greedily wrapped at word boundaries. A word
    /// longer than the width is kept whole, so a very long token can exceed it.
    Reflow {
        /// Path to the PDF file.
        file: PathBuf,
        /// Column width in characters (20-200).
        #[arg(long, default_value_t = reflow::default_width())]
        width: usize,
        /// One-based page to reflow; every page when omitted.
        #[arg(long, value_name = "N")]
        page: Option<usize>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Build a deterministic utterance queue and optionally speak it.
    ///
    /// The plan itself uses no audio device: each sentence becomes one
    /// utterance with integer-millisecond start, duration and trailing pause
    /// derived from `--wpm`; `--seek` starts at an utterance and rebases the
    /// clock. `--voices` lists the installed Windows voices. `--speak` hands
    /// the planned text to the Windows `System.Speech` synthesizer with the
    /// selected `--voice` and a rate mapped from `--wpm`.
    ReadAloud {
        /// Path to the PDF file; omit with `--voices`.
        #[arg(conflicts_with = "voices")]
        file: Option<PathBuf>,
        /// Speaking rate in words per minute (80-400).
        #[arg(long, default_value_t = read_aloud::default_wpm())]
        wpm: u16,
        /// One-based page to narrate; every page when omitted.
        #[arg(long, value_name = "N")]
        page: Option<usize>,
        /// Zero-based utterance to start from.
        #[arg(long, default_value_t = 0)]
        seek: usize,
        /// Installed voice name; `default` uses the system default.
        #[arg(long, default_value = "default")]
        voice: String,
        /// Speak the plan through the Windows system synthesizer.
        #[arg(long)]
        speak: bool,
        /// List the installed Windows speech voices instead of a document.
        #[arg(long)]
        voices: bool,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Stamp page decoration (headers, footers) onto every page.
    Decorate {
        /// The decoration to apply.
        #[command(subcommand)]
        command: DecorateCommand,
    },
    /// Add or list drawing annotations on a page.
    Annotate {
        /// The annotation operation to perform.
        #[command(subcommand)]
        command: AnnotateCommand,
    },
    /// Edit bookmarks in the document outline.
    Outline {
        /// The outline operation to perform.
        #[command(subcommand)]
        command: OutlineCommand,
    },
    /// Add or remove link annotations.
    Link {
        /// The link operation to perform.
        #[command(subcommand)]
        command: LinkCommand,
    },
    /// Add or remove embedded-file attachments.
    Attach {
        /// The attachment operation to perform.
        #[command(subcommand)]
        command: AttachCommand,
    },
    /// Create and inspect AcroForm fields.
    ///
    /// `create` builds real `/AcroForm` field and widget dictionaries from a
    /// compact description, serializes them into a standalone US-Letter page,
    /// and reloads the bytes before writing. `list` reports the terminal fields
    /// of any form PDF.
    Form {
        /// The form operation to perform.
        #[command(subcommand)]
        command: FormCommand,
    },
    /// Check accessibility metadata and PDF/UA-1 structure.
    ///
    /// Runs the accessibility checker over a tree derived from recovered page
    /// text and the PDF/UA-1 preflight profile over the file's real catalog and
    /// structure, then merges both reports. No conformance certification is
    /// claimed.
    Accessibility {
        /// The accessibility operation to perform.
        #[command(subcommand)]
        command: AccessibilityCommand,
    },
    /// Store page-text snapshots and diff revisions.
    Versions {
        /// The version-history operation to perform.
        #[command(subcommand)]
        command: VersionsCommand,
    },
    /// Embed a CID font and write text with it.
    Font {
        /// The font operation to perform.
        #[command(subcommand)]
        command: FontCommand,
    },
    /// Write a tagged-PDF structure tree into a document.
    Structure {
        /// The structure operation to perform.
        #[command(subcommand)]
        command: StructureCommand,
    },
    /// Append an incremental revision with updated document information.
    ///
    /// Every original byte stays an exact prefix; only the changed objects and
    /// a new cross-reference section are appended. The saved file is validated
    /// against the original prefix, xref chain, page count and page text before
    /// the report is printed.
    Update {
        /// Path to the PDF file.
        file: PathBuf,
        /// Where to write the incremental revision.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// New document title (`/Title`).
        #[arg(long, value_name = "TEXT")]
        title: Option<String>,
        /// New author (`/Author`).
        #[arg(long, value_name = "TEXT")]
        author: Option<String>,
        /// New subject (`/Subject`).
        #[arg(long, value_name = "TEXT")]
        subject: Option<String>,
        /// New keywords (`/Keywords`).
        #[arg(long, value_name = "TEXT")]
        keywords: Option<String>,
        /// New producer (`/Producer`).
        #[arg(long, value_name = "TEXT")]
        producer: Option<String>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Write a fully linearized file with hint tables.
    Linearize {
        /// Path to the PDF file.
        file: PathBuf,
        /// Where to write the linearized document.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// PDF version token written in the header.
        #[arg(long, default_value = "1.7")]
        version: String,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Pack objects into object streams and write an xref stream.
    Pack {
        /// Path to the PDF file.
        file: PathBuf,
        /// Where to write the packed document.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Leave generated streams uncompressed.
        #[arg(long)]
        no_compress: bool,
        /// Maximum objects per `/ObjStm` (1-65535).
        #[arg(long, default_value_t = 128)]
        max_objects: usize,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Rewrite a document with the optimizer and report what changed.
    ///
    /// Compacts the object graph, drops unreachable objects, Flate-compresses
    /// streams only when that strictly helps, and writes a fresh standalone PDF.
    /// The input is never modified: `--out` is required and must differ from it.
    Optimize {
        /// Path to the PDF file.
        file: PathBuf,
        /// Where to write the optimized document.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Remove the trailer `/Info` entry and the catalog `/Metadata` stream.
        #[arg(long)]
        remove_metadata: bool,
        /// Remove optional `/Thumb` and `/PieceInfo` entries.
        #[arg(long)]
        remove_optional: bool,
        /// Keep streams uncompressed (compression is on by default).
        #[arg(long)]
        no_compress: bool,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Manage the virtual PDF printer and capture one printed job.
    ///
    /// `install` creates a local printer port that is a file path plus a
    /// `PDFXI PDF` queue using the Windows PDF driver, so any application can
    /// print to PDF and the result lands in a known file. `watch` waits for one
    /// finished job and copies it to `--out`. No elevation is required.
    Printer {
        /// The printer operation to perform.
        #[command(subcommand)]
        command: PrinterCommand,
    },
    /// Create a new PDF from an image or text/Markdown source.
    Create {
        /// The creation operation to perform.
        #[command(subcommand)]
        command: CreateCommand,
    },
    /// Export a PDF's accessible text as a structured representation.
    ///
    /// This writes a structured text representation (roles, reading order and
    /// text); it does not produce a tagged-PDF file or certify PDF/UA.
    Export {
        /// The export operation to perform.
        #[command(subcommand)]
        command: ExportCommand,
    },
    /// Manage named actions stored in a tab-delimited action file.
    ///
    /// An action file holds one or more action blocks separated by blank lines.
    /// Each block uses the stable `ACTION` / `STEP` / `PARAM` text format defined
    /// by `pdfxi-automation`. Names are unique within a file.
    Action {
        /// The action operation to perform.
        #[command(subcommand)]
        command: ActionCommand,
    },
    /// Droplet-style entry point: run a named action over one or more inputs.
    ///
    /// This is exactly equivalent to
    /// `pdfxi action run <ACTION_FILE> <NAME> --input <INPUT>...` with the
    /// default text output and no output-directory override. It accepts any
    /// number of dropped paths, so an Explorer "drop onto" verb can pass `%1`
    /// (or `%*` for several files) directly to the executable. `--json` selects
    /// the same stable JSON shape as `action run --json`. See the `action`
    /// module documentation for a registration example.
    Droplet {
        /// Path to the action file holding the named action.
        action_file: PathBuf,
        /// Name of the action to run.
        name: String,
        /// One or more input paths to stage through the action.
        #[arg(required = true, num_args = 1..)]
        inputs: Vec<PathBuf>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Run a deterministic, byte-level subset preflight profile.
    ///
    /// `PROFILE` is a canonical label such as `PDF/A-2b` (ASCII case is
    /// ignored). The command loads real PDF bytes, runs the `pdfxi-standards`
    /// profile over a derived snapshot and exits 0 when no error-severity
    /// finding fires, or 2 when the check fails. A passing verdict is not a
    /// conformance certification; the report always lists reported-only
    /// constraints and states that certification is not claimed.
    ///
    /// `--fix` applies the opt-in, byte-expressible normalizations (minimum
    /// version, XMP presence, output-intent presence/subtype) and writes the
    /// normalized PDF to `--output`, or to a deterministic
    /// `<input-stem>.preflight.pdf` sibling when `--output` is omitted.
    Preflight {
        /// Canonical profile label, for example `PDF/A-2b` or `PDF/X-4`.
        profile: String,
        /// Path to the PDF file to check.
        input: PathBuf,
        /// Apply opt-in byte-level fixups and write the normalized PDF.
        #[arg(long)]
        fix: bool,
        /// Destination for the normalized PDF; defaults to a sibling file.
        #[arg(long, value_name = "PATH")]
        output: Option<PathBuf>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Inspect the loupe magnifier and apply gesture navigation.
    Loupe {
        /// Source PDF.
        input: PathBuf,
        /// One-based page.
        #[arg(long, default_value_t = 1)]
        page: usize,
        /// Magnification (clamped to 1..=16).
        #[arg(long, default_value_t = 2.0)]
        zoom: f32,
        /// Lens size as `WxH`.
        #[arg(long, default_value = "200x200")]
        lens: String,
        /// Lens shape: `rectangle` or `circle`.
        #[arg(long, default_value = "rectangle")]
        shape: String,
        /// Viewport size as `WxH` (defaults to the page size).
        #[arg(long, value_name = "WxH")]
        viewport: Option<String>,
        /// Lens centre in page space as `X,Y`.
        #[arg(long, value_name = "X,Y")]
        center: Option<String>,
        /// Semicolon-separated gestures: `drag:DX,DY`, `scroll:DX,DY`,
        /// `pinch:FACTOR`, `zoom-in`, `zoom-out`, `center:X,Y`, `reset`.
        #[arg(long, default_value = "")]
        commands: String,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Drive presentation-mode slides and full-screen layout without a window.
    Present {
        /// Source PDF.
        input: PathBuf,
        /// Comma-separated commands: next, prev, first, last, auto, exit,
        /// `tick:SECONDS`, `key:NAME` or `gesture:NAME`.
        #[arg(long, default_value = "next")]
        commands: String,
        /// Screen size as `WxH`.
        #[arg(long, default_value = "1280x720")]
        screen: String,
        /// Fit mode: `fit` or `actual`.
        #[arg(long, default_value = "fit")]
        fit: String,
        /// Per-slide auto-advance duration in seconds.
        #[arg(long, default_value_t = 0.0)]
        duration: f32,
        /// Transition: `none`, `fade` or `push`.
        #[arg(long, default_value = "none")]
        transition: String,
        /// One-based page subsequence, for example `1,3,5`.
        #[arg(long, value_name = "PAGES")]
        slides: Option<String>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Apply a named protection policy to a document.
    Protection {
        /// Protection action.
        #[command(subcommand)]
        command: ProtectionCommand,
    },
    /// Plan trapping presets and transparency flattening for a document.
    Prepress {
        /// Prepress action.
        #[command(subcommand)]
        command: PrepressCommand,
    },
    /// Plan or apply print-production marks, bleed, separations and intent.
    Production {
        /// Production action.
        #[command(subcommand)]
        command: ProductionCommand,
    },
    /// Apply printer marks to every page of a PDF.
    ///
    /// With no family switch, all four families (crop, registration, colour
    /// bar and page information) are applied. When at least one switch is
    /// given, only the requested families are drawn. The marked bytes are
    /// reparsed before they are written, and output is deterministic.
    Marks {
        /// Path to the source PDF.
        input: PathBuf,
        /// Destination path for the marked PDF.
        output: PathBuf,
        /// Draw crop marks just outside the TrimBox.
        #[arg(long)]
        crop_marks: bool,
        /// Draw registration targets on the MediaBox corners.
        #[arg(long)]
        registration: bool,
        /// Draw the CMYK colour bar.
        #[arg(long)]
        colour_bars: bool,
        /// Draw page-information text.
        #[arg(long)]
        page_info: bool,
    },
    /// Repair a damaged PDF into a new, verified standalone file.
    ///
    /// `INPUT` is read but never modified. `OUTPUT` must name a distinct file:
    /// the command refuses to overwrite the input, rejects `..` traversal in
    /// the output path, and reparses the result before reporting success. Exit
    /// codes are 0 on success, 1 for CLI/I/O errors and 3 when the input cannot
    /// be repaired. `--json` prints a stable report whose `warnings` array holds
    /// one code/message pair per typed repair applied by the core crate.
    Repair {
        /// Path to the damaged source PDF (never modified).
        input: PathBuf,
        /// Destination path for the repaired PDF; must differ from `INPUT`.
        output: PathBuf,
        /// Emit a machine-readable JSON repair report with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Run a local, line-delimited JSON automation API over stdin and stdout.
    ///
    /// Each input line is one typed request and produces one canonical JSON
    /// response line. No sockets are opened. Commands can point at saved
    /// action files, run batches, validate with a dry run, and use named
    /// toolsets. `--once` answers exactly one request and exits.
    Api {
        /// Read and answer one request, then exit instead of looping to EOF.
        #[arg(long)]
        once: bool,
    },
    /// Enumerate and resolve scriptable automation operations.
    Ops {
        /// The registry operation to perform.
        #[command(subcommand)]
        command: OpsCommand,
    },
    /// Plan a plugin invocation or validate a typed plugin output.
    Plugin {
        /// The plugin operation to perform.
        #[command(subcommand)]
        command: PluginCommand,
    },
    /// Manage named toolsets that map aliases to saved actions.
    Toolset {
        /// The toolset operation to perform.
        #[command(subcommand)]
        command: ToolsetCommand,
    },
    /// Poll a watched folder once through a deterministic schedule.
    ///
    /// Advances host ticks through `PollSchedule`, polls when due, writes
    /// converted files beneath `--out`, and persists the processed-file markers.
    /// No thread or OS watcher is started; invoke the command from a scheduler.
    Watch {
        /// Folder to scan (must exist).
        folder: PathBuf,
        /// Output directory; created when absent and must be outside the folder.
        #[arg(long, value_name = "DIR")]
        out: PathBuf,
        /// Eligible extension; repeat for several (default: pdf).
        #[arg(long = "ext", value_name = "EXT", default_value = "pdf")]
        extensions: Vec<String>,
        /// Output-name suffix (default: pdf).
        #[arg(long, value_name = "EXT", default_value = "pdf")]
        suffix: String,
        /// Marker snapshot to load and atomically replace.
        #[arg(long, value_name = "PATH")]
        markers: Option<PathBuf>,
        /// Poll every N ticks; zero polls on every tick.
        #[arg(long, default_value_t = 0)]
        poll_every: u64,
        /// Backoff base delay in ticks.
        #[arg(long, default_value_t = 1)]
        base_ticks: u64,
        /// Backoff ceiling in ticks.
        #[arg(long, default_value_t = 64)]
        max_ticks: u64,
        /// Backoff growth per thousand (1000..=10000).
        #[arg(long, default_value_t = 2000)]
        multiplier: u32,
        /// Number of host ticks to advance (default: 1).
        #[arg(long, default_value_t = 1)]
        ticks: u64,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Operations on the `pdfxi plugin` command group.
#[derive(Debug, Subcommand)]
enum PluginCommand {
    /// Build a deterministic, non-executing execution plan from a manifest.
    Plan {
        /// Path to the plugin manifest JSON.
        manifest: PathBuf,
        /// Operation to plan.
        #[arg(long, value_name = "NAME")]
        operation: String,
        /// Grant a capability; repeat for several.
        #[arg(long = "grant", value_name = "CAP")]
        grants: Vec<String>,
        /// Expected output type: null, bool, int, text or bytes.
        #[arg(long, default_value = "text")]
        expect: String,
        /// Invocation step budget.
        #[arg(long, default_value_t = 1_000_000)]
        max_steps: u64,
        /// Invocation memory budget in bytes.
        #[arg(long, default_value_t = 16_777_216)]
        max_memory_bytes: u64,
        /// Invocation wall-clock budget in milliseconds.
        #[arg(long, default_value_t = 5_000)]
        max_wall_clock_ms: u64,
        /// Output text budget in bytes.
        #[arg(long, default_value_t = 65_536)]
        max_text_bytes: usize,
        /// Output byte budget in bytes.
        #[arg(long, default_value_t = 1_048_576)]
        max_bytes: usize,
        /// Output total budget in bytes.
        #[arg(long, default_value_t = 2_097_152)]
        max_total_bytes: usize,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Validate one typed, size-bounded plugin output.
    Output {
        /// Value type: null, bool, int, text or bytes.
        #[arg(long)]
        r#type: String,
        /// Producing plugin id.
        #[arg(long, default_value = "host.plugin")]
        plugin_id: String,
        /// Operation that produced the value.
        #[arg(long, default_value = "output")]
        operation: String,
        /// Text value.
        #[arg(long, value_name = "TEXT")]
        text: Option<String>,
        /// Generate a text value of this many bytes.
        #[arg(long, value_name = "N")]
        text_len: Option<usize>,
        /// Integer value.
        #[arg(long, allow_hyphen_values = true)]
        int: Option<i64>,
        /// Boolean value.
        #[arg(long, value_name = "true|false")]
        bool: Option<bool>,
        /// Generate an opaque bytes value of this length.
        #[arg(long, value_name = "N")]
        bytes_len: Option<usize>,
        /// Expected type; defaults to the produced type.
        #[arg(long)]
        expect: Option<String>,
        /// Output text budget in bytes.
        #[arg(long, default_value_t = 65_536)]
        max_text_bytes: usize,
        /// Output byte budget in bytes.
        #[arg(long, default_value_t = 1_048_576)]
        max_bytes: usize,
        /// Output total budget in bytes.
        #[arg(long, default_value_t = 2_097_152)]
        max_total_bytes: usize,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Operations on the `pdfxi toolset` command group.
#[derive(Debug, Subcommand)]
enum ToolsetCommand {
    /// List the aliases and defaults in a toolset file.
    List {
        /// Path to the toolset JSON file.
        file: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Validate a toolset file.
    Validate {
        /// Path to the toolset JSON file.
        file: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Show one alias and its parameter defaults.
    Show {
        /// Path to the toolset JSON file.
        file: PathBuf,
        /// Alias to display.
        alias: String,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Run one alias over input files with its defaults applied.
    Run {
        /// Path to the toolset JSON file.
        file: PathBuf,
        /// Alias to run.
        alias: String,
        /// Input file; repeat for each job.
        #[arg(long = "input", value_name = "PATH", required = true)]
        inputs: Vec<PathBuf>,
        /// Directory that staged export targets resolve against.
        #[arg(long, value_name = "DIR")]
        output_dir: Option<PathBuf>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Operations on the `pdfxi ops` command group.
#[derive(Debug, Subcommand)]
enum OpsCommand {
    /// List every scriptable operation and its typed parameters.
    List {
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Resolve `name=value` arguments into a validated call plan.
    Resolve {
        /// Operation name, for example `run-batch`.
        name: String,
        /// Typed argument `name=value`; repeat for each parameter.
        #[arg(value_name = "NAME=VALUE")]
        assignments: Vec<String>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Operations on the `pdfxi action` command group.
#[derive(Debug, Subcommand)]
enum ActionCommand {
    /// List every action in a file, in file order.
    List {
        /// Path to the action file.
        file: PathBuf,
    },
    /// Create one validated action file from flags.
    ///
    /// Steps use `ID:KIND` (`import`, `convert`, `watermark`, `security`,
    /// `export`, `custom-command`); parameters use `ID:KEY=VALUE`.
    Create {
        /// Action file to write.
        file: PathBuf,
        /// Action name.
        #[arg(long)]
        name: String,
        /// Action description.
        #[arg(long, default_value = "")]
        description: String,
        /// Step as `ID:KIND`; repeat in execution order.
        #[arg(long = "step", value_name = "ID:KIND", required = true)]
        step: Vec<String>,
        /// Parameter as `ID:KEY=VALUE`; repeat for each value.
        #[arg(long = "param", value_name = "ID:KEY=VALUE")]
        param: Vec<String>,
        /// Overwrite an existing file.
        #[arg(long)]
        force: bool,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Show the steps and parameters of one named action.
    Show {
        /// Path to the action file.
        file: PathBuf,
        /// Name of the action to display.
        name: String,
    },
    /// Run a named action over one or more input files.
    ///
    /// Every input is validated before any job is run. Jobs execute in the
    /// order inputs are given; one failing input never stops its siblings.
    /// The command exits non-zero if any job failed. Use `--json` for a stable
    /// machine-readable result containing the job state, each per-file outcome,
    /// and the per-step results recorded by the executor.
    Run {
        /// Path to the action file.
        file: PathBuf,
        /// Name of the action to run.
        name: String,
        /// One or more input paths, in execution order.
        #[arg(long, required = true, num_args = 1.., value_name = "PATH")]
        input: Vec<PathBuf>,
        /// Directory used to resolve relative export targets. No file is
        /// written: the resolved path is only recorded in the outcome.
        #[arg(long, value_name = "DIR")]
        output_dir: Option<PathBuf>,
        /// Apply real in-process PDF operations and write export targets.
        ///
        /// Without this flag the command only stages inputs and records the
        /// resolved export path. With it, import validates PDF bytes, watermark
        /// stamps real page content, and export writes the result beneath
        /// `--output-dir`. Security and custom-command steps are refused.
        #[arg(long)]
        execute: bool,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Run Action Wizard sequence steps over one or more real documents.
    ///
    /// Every input is parsed before any job runs. Steps execute in the order
    /// given; a failing item never stops its siblings. Each succeeded item is
    /// written to `<stem>.sequence.pdf` (or beneath `--out-dir`) and reparsed.
    Sequence {
        /// One or more input PDFs, in execution order.
        #[arg(long, required = true, num_args = 1.., value_name = "PATH")]
        input: Vec<PathBuf>,
        /// Ordered step: `script:SOURCE`, `metadata:KEY=VALUE` or
        /// `command:strip-metadata|normalize-boxes`.
        #[arg(long = "step", required = true, value_name = "STEP")]
        step: Vec<String>,
        /// Directory for the written PDFs (defaults to each input's folder).
        #[arg(long, value_name = "DIR")]
        out_dir: Option<PathBuf>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Operations on the `pdfxi timestamp` command group.
#[derive(Debug, Subcommand)]
enum TimestampCommand {
    /// Mint an RFC 3161 timestamp token locally from an RSA identity.
    ///
    /// This is an offline TSA: no network request is made. The token covers the
    /// digest of `--content` and is signed with `--key` for `--cert`.
    Sign {
        /// Content file whose digest is stamped.
        content: PathBuf,
        /// Written DER token.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Unencrypted RSA private key (PKCS#8 or PKCS#1 PEM).
        #[arg(long, value_name = "PATH")]
        key: PathBuf,
        /// TSA certificate PEM or DER.
        #[arg(long, value_name = "PATH")]
        cert: PathBuf,
        /// Imprint digest: `sha256`, `sha384` or `sha512`.
        #[arg(long, default_value = "sha256")]
        digest: String,
        /// TSA policy OID.
        #[arg(long, default_value = "1.3.6.1.4.1.13762.3.1")]
        policy: String,
        /// TSA serial number.
        #[arg(long, default_value_t = 1)]
        serial: u64,
        /// `genTime` in Unix seconds (defaults to the system clock).
        #[arg(long, value_name = "UNIX")]
        time: Option<u64>,
        /// Nonce echoed in the token.
        #[arg(long)]
        nonce: Option<u64>,
        /// Set the ordering flag.
        #[arg(long)]
        ordering: bool,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Validate an RFC 3161 timestamp token against caller-supplied trust.
    Verify {
        /// DER token to validate.
        token: PathBuf,
        /// Content file the token must cover.
        #[arg(long, value_name = "PATH")]
        content: PathBuf,
        /// TSA certificate PEM or DER (the first certificate is the signer).
        #[arg(long, value_name = "PATH")]
        cert: PathBuf,
        /// Trust anchor PEM or DER; repeatable. Defaults to `--cert`.
        #[arg(long = "anchor", value_name = "PATH")]
        anchor: Vec<PathBuf>,
        /// Intermediate certificate PEM or DER; repeatable.
        #[arg(long = "chain", value_name = "PATH")]
        chain: Vec<PathBuf>,
        /// Expected nonce; the token must echo it.
        #[arg(long)]
        nonce: Option<u64>,
        /// Validation clock in Unix seconds for the future-skew guard.
        #[arg(long, value_name = "UNIX")]
        at: Option<u64>,
        /// Do not require the RFC 3161 timeStamping EKU on the TSA certificate.
        #[arg(long)]
        allow_any_eku: bool,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Operations on the `pdfxi decorate` command group.
#[derive(Debug, Subcommand)]
enum DecorateCommand {
    /// Stamp a header and/or footer on every page.
    ///
    /// Text placeholders are `{page}`, `{pages}`, `{date}`, `{title}` and
    /// `{filename}`; everything else is literal. `--side` chooses which of the
    /// three header/footer slots the text occupies.
    HeaderFooter {
        /// Path to the PDF file.
        file: PathBuf,
        /// Where to write the decorated document.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Header template.
        #[arg(long, value_name = "TEXT")]
        header: Option<String>,
        /// Footer template.
        #[arg(long, value_name = "TEXT")]
        footer: Option<String>,
        /// Which slot the header and footer occupy.
        #[arg(long, value_enum, default_value_t = decorate::Side::Center)]
        side: decorate::Side,
        /// Font size in points (4-48).
        #[arg(long, default_value_t = 10.0)]
        font_size: f32,
        /// Page margin in points.
        #[arg(long, default_value_t = 36.0)]
        margin: f32,
        /// Document title substituted for `{title}`.
        #[arg(long, default_value = "")]
        title: String,
        /// Preformatted date substituted for `{date}`.
        #[arg(long, default_value = "")]
        date: String,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Stamp a text watermark on every page.
    ///
    /// The mark is real page content: its text is extracted and searched like
    /// any other text, and `--opacity` adds an `/ExtGState` alpha to the page.
    Watermark {
        /// Path to the PDF file.
        file: PathBuf,
        /// Where to write the decorated document.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Watermark text.
        #[arg(long, value_name = "TEXT")]
        text: String,
        /// Font size in points (4-200).
        #[arg(long, default_value_t = 72.0)]
        font_size: f32,
        /// Alpha in 0..=1.
        #[arg(long, default_value_t = 0.15)]
        opacity: f32,
        /// Rotation in degrees.
        #[arg(long, default_value_t = 45.0)]
        rotation: f32,
        /// Repeat the mark on a grid instead of centring it.
        #[arg(long)]
        tile: bool,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Stamp a sequential Bates number on every page.
    Bates {
        /// Path to the PDF file.
        file: PathBuf,
        /// Where to write the decorated document.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Number prefix.
        #[arg(long, default_value = "", allow_hyphen_values = true)]
        prefix: String,
        /// Number suffix.
        #[arg(long, default_value = "", allow_hyphen_values = true)]
        suffix: String,
        /// First number (must not be zero).
        #[arg(long, default_value_t = 1)]
        start: u64,
        /// Zero-padded digit width (1-20).
        #[arg(long, default_value_t = 6)]
        digits: usize,
        /// Per-page increment.
        #[arg(long, default_value_t = 1)]
        increment: u64,
        /// Corner the number is anchored to.
        #[arg(long, value_enum, default_value_t = decorate::BatesCorner::BottomRight)]
        corner: decorate::BatesCorner,
        /// Font size in points (4-200).
        #[arg(long, default_value_t = 10.0)]
        font_size: f32,
        /// Page margin in points.
        #[arg(long, default_value_t = 36.0)]
        margin: f32,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Operations on the `pdfxi annotate` command group.
///
/// The `Add` variant carries every option for six annotation kinds, so it is
/// much larger than `List`. The value is constructed once from the command line
/// and never stored in a collection, so the size difference costs nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Subcommand)]
enum AnnotateCommand {
    /// Add one free-text, callout or cloud annotation and save the result.
    Add {
        /// Path to the PDF file.
        file: PathBuf,
        /// Where to write the annotated document.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// One-based page to annotate.
        #[arg(long, value_name = "N", default_value_t = 1)]
        page: usize,
        /// Annotation kind.
        #[arg(long, value_enum)]
        kind: annotate::AnnotationKind,
        /// Rectangle in points: `x0,y0,x1,y1` (not used by pencil).
        #[arg(long, value_name = "RECT")]
        rect: Option<String>,
        /// Text for a free text or callout annotation.
        #[arg(long, value_name = "TEXT")]
        text: Option<String>,
        /// Leader-line anchor in points: `x,y` (callouts).
        #[arg(long, value_name = "POINT")]
        anchor: Option<String>,
        /// File to embed (file-attachment annotations).
        #[arg(long, value_name = "PATH")]
        attachment: Option<PathBuf>,
        /// Attachment name written to `/FS`; defaults to the file name.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        /// Pencil strokes: `x0,y0 x1,y1; x2,y2 ...`.
        #[arg(long, value_name = "STROKES")]
        ink: Option<String>,
        /// Pencil stroke width in points (0.1-32).
        #[arg(long, default_value_t = 2.0)]
        stroke_width: f32,
        /// Stamp caption, e.g. Approved or Draft.
        #[arg(long, value_name = "NAME")]
        stamp_name: Option<String>,
        /// Font size in points (4-72).
        #[arg(long, default_value_t = 11.0)]
        font_size: f32,
        /// Author name written to `/T`.
        #[arg(long, default_value = "")]
        author: String,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// List the annotations on one page.
    List {
        /// Path to the PDF file.
        file: PathBuf,
        /// One-based page to inspect.
        #[arg(long, value_name = "N", default_value_t = 1)]
        page: usize,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Operations on the `pdfxi outline` command group.
#[derive(Debug, Subcommand)]
enum OutlineCommand {
    /// List the bookmark tree with destinations.
    List {
        /// Path to the PDF file.
        file: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Append a bookmark under a parent path.
    Add {
        /// Path to the PDF file.
        file: PathBuf,
        /// Where to write the edited document.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Bookmark title.
        #[arg(long, value_name = "TEXT")]
        title: String,
        /// One-based destination page.
        #[arg(long, value_name = "N")]
        page: usize,
        /// Parent path such as `0.1`; the root list when omitted.
        #[arg(long, value_name = "PATH")]
        parent: Option<String>,
        /// Insert position under the parent; appends when omitted.
        #[arg(long, value_name = "N")]
        index: Option<usize>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Change one bookmark's title.
    Rename {
        /// Path to the PDF file.
        file: PathBuf,
        /// Where to write the edited document.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Dotted bookmark path such as `0.1`.
        #[arg(long, value_name = "PATH")]
        path: String,
        /// New title.
        #[arg(long, value_name = "TEXT")]
        title: String,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Move a bookmark under another parent.
    Move {
        /// Path to the PDF file.
        file: PathBuf,
        /// Where to write the edited document.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Dotted bookmark path to move.
        #[arg(long, value_name = "PATH")]
        path: String,
        /// New parent path; the root list when omitted.
        #[arg(long, value_name = "PATH")]
        parent: Option<String>,
        /// Position under the new parent; appends when omitted.
        #[arg(long, value_name = "N")]
        index: Option<usize>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Remove one bookmark and its children.
    Remove {
        /// Path to the PDF file.
        file: PathBuf,
        /// Where to write the edited document.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Dotted bookmark path such as `0.1`.
        #[arg(long, value_name = "PATH")]
        path: String,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Operations on the `pdfxi link` command group.
#[derive(Debug, Subcommand)]
enum LinkCommand {
    /// Add a link annotation to one page.
    Add {
        /// Path to the PDF file.
        file: PathBuf,
        /// Where to write the edited document.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// One-based page that receives the link.
        #[arg(long, value_name = "N", default_value_t = 1)]
        page: usize,
        /// Rectangle in points: `x0,y0,x1,y1`.
        #[arg(long, value_name = "RECT")]
        rect: String,
        /// External URI target.
        #[arg(long, value_name = "URL", conflicts_with = "to_page")]
        uri: Option<String>,
        /// One-based internal page target.
        #[arg(long, value_name = "N")]
        to_page: Option<usize>,
        /// Hover highlight: none, invert, outline or push.
        #[arg(long, default_value = "none")]
        highlight: String,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Remove one link annotation from a page.
    Remove {
        /// Path to the PDF file.
        file: PathBuf,
        /// Where to write the edited document.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// One-based page that carries the link.
        #[arg(long, value_name = "N", default_value_t = 1)]
        page: usize,
        /// Zero-based link index in page annotation order.
        #[arg(long, value_name = "N", default_value_t = 0)]
        index: usize,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Operations on the `pdfxi attach` command group.
#[derive(Debug, Subcommand)]
enum AttachCommand {
    /// Embed a file in the catalog name tree.
    Add {
        /// Path to the PDF file.
        file: PathBuf,
        /// Where to write the edited document.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// File whose bytes are embedded.
        #[arg(long, value_name = "PATH")]
        attachment: PathBuf,
        /// Name-tree key; defaults to the attachment file name.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        /// Optional `/Desc` text.
        #[arg(long, default_value = "")]
        description: String,
        /// MIME type; defaults to application/octet-stream.
        #[arg(long, default_value = "")]
        mime: String,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Remove a file from the catalog name tree.
    Remove {
        /// Path to the PDF file.
        file: PathBuf,
        /// Where to write the edited document.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Name-tree key to remove.
        #[arg(long, value_name = "NAME")]
        name: String,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}
/// Operations on the `pdfxi form` command group.
#[derive(Debug, Subcommand)]
enum FormCommand {
    /// Build a one-page PDF with typed AcroForm fields.
    Create {
        /// Where to write the form PDF.
        out: PathBuf,
        /// Field description `KIND:NAME:X,Y,W,H[:OPT1|OPT2]`; repeat for each field.
        #[arg(
            long = "field",
            value_name = "KIND:NAME:X,Y,W,H[:OPTIONS]",
            required = true
        )]
        fields: Vec<String>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Recognize static label/control hints and write an AcroForm PDF.
    ///
    /// The manifest supplies caller-classified rectangles (label, input,
    /// checkbox, radio, choice) in page points. Recognition pairs each control
    /// with at most one nearby label, assigns stable names and confidence, and
    /// the written form is reloaded before it is reported.
    Recognize {
        /// Recognition manifest JSON.
        manifest: PathBuf,
        /// Destination form PDF.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// List the terminal fields of a form PDF.
    List {
        /// Path to the PDF file.
        file: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Validate field definitions and values.
    Validate {
        /// Path to the PDF file.
        file: PathBuf,
        /// Value assignment `NAME=VALUE`; repeat for each field.
        #[arg(long = "set", value_name = "NAME=VALUE")]
        assignments: Vec<String>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Evaluate sum/product/average/min/max calculations over field values.
    Calculate {
        /// Path to the PDF file.
        file: PathBuf,
        /// Value assignment `NAME=VALUE`; repeat for each field.
        #[arg(long = "set", value_name = "NAME=VALUE")]
        assignments: Vec<String>,
        /// Sum `TARGET=SRC1,SRC2`; repeat for each calculation.
        #[arg(long = "sum", value_name = "TARGET=SOURCES")]
        sums: Vec<String>,
        /// Product `TARGET=SRC1,SRC2`; repeat for each calculation.
        #[arg(long = "product", value_name = "TARGET=SOURCES")]
        products: Vec<String>,
        /// Average `TARGET=SRC1,SRC2`; repeat for each calculation.
        #[arg(long = "average", value_name = "TARGET=SOURCES")]
        averages: Vec<String>,
        /// Minimum `TARGET=SRC1,SRC2`; repeat for each calculation.
        #[arg(long = "min", value_name = "TARGET=SOURCES")]
        mins: Vec<String>,
        /// Maximum `TARGET=SRC1,SRC2`; repeat for each calculation.
        #[arg(long = "max", value_name = "TARGET=SOURCES")]
        maxs: Vec<String>,
        /// Decimal places in results (0-12).
        #[arg(long, default_value_t = 6)]
        decimals: u8,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Show the widget tab order and next/previous navigation.
    Tabs {
        /// Path to the PDF file.
        file: PathBuf,
        /// Override the `/Tabs` policy: rows, columns, structure or annotations.
        #[arg(long, value_name = "POLICY")]
        policy: Option<String>,
        /// Include read-only widgets in the traversal.
        #[arg(long)]
        include_read_only: bool,
        /// Do not wrap at the ends of the order.
        #[arg(long)]
        no_wrap: bool,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Show accepted and rejected field actions.
    Actions {
        /// Path to the PDF file.
        file: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Write local reader-enable rights metadata into `/Perms /UR`.
    ///
    /// This is compatibility metadata for form filling and saving, not Adobe
    /// certification and not a cryptographic signature.
    ReaderEnable {
        /// Path to the PDF file.
        file: PathBuf,
        /// Destination PDF.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Comma-separated rights: fill, save, annotate, assemble, copy-extract,
        /// accessibility or all.
        #[arg(long, default_value = "fill,save")]
        rights: String,
        /// Optional reason metadata.
        #[arg(long, value_name = "TEXT")]
        reason: Option<String>,
        /// Placeholder `/Contents` size in bytes (1-65536).
        #[arg(long, default_value_t = 4096)]
        placeholder_bytes: usize,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Export a blank bundle describing a form's fields.
    Distribute {
        /// Path to the PDF file.
        file: PathBuf,
        /// Destination bundle file.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Stable form identifier.
        #[arg(long, value_name = "ID")]
        form_id: Option<String>,
        /// Human-readable title.
        #[arg(long, value_name = "TEXT")]
        title: Option<String>,
        /// Bundle version.
        #[arg(long, default_value_t = 1)]
        version: u32,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Merge FDF/XFDF response files collected for a form.
    Collect {
        /// Path to the PDF file.
        file: PathBuf,
        /// Response document as `[RESPONDENT=]PATH`; repeat for each respondent.
        #[arg(long = "response", value_name = "[RESPONDENT=]PATH", required = true)]
        responses: Vec<String>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Operations on the `pdfxi accessibility` command group.
#[derive(Debug, Subcommand)]
enum AccessibilityCommand {
    /// Print the structure-tree reading order.
    Order {
        /// Path to the tagged PDF.
        file: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Move one structure element before a sibling (or to the end).
    Move {
        /// Path to the tagged PDF.
        file: PathBuf,
        /// Where to write the reordered document.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// `/XI_NodeId` of the element to move.
        #[arg(long, value_name = "N")]
        node: u64,
        /// Move before this sibling id; omit to move to the end.
        #[arg(long, value_name = "N")]
        before: Option<u64>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Set or clear `/Alt` text on one tagged structure element.
    Alt {
        /// Path to the tagged PDF.
        file: PathBuf,
        /// Where to write the edited document.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// `/XI_NodeId` of the structure element.
        #[arg(long, value_name = "N")]
        node: u64,
        /// Alternative text; omit with `--clear`.
        #[arg(long, value_name = "TEXT", conflicts_with = "clear")]
        text: Option<String>,
        /// Remove the existing `/Alt` entry.
        #[arg(long)]
        clear: bool,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Set or clear the `/Scope` of a table header/data cell.
    Table {
        /// Path to the tagged PDF.
        file: PathBuf,
        /// Where to write the edited document.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// `/XI_NodeId` of the table cell.
        #[arg(long, value_name = "N")]
        node: u64,
        /// `row`, `column` or `both`; omit to clear.
        #[arg(long, value_name = "SCOPE")]
        scope: Option<String>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Check document accessibility metadata and PDF/UA-1 structure.
    Check {
        /// Path to the PDF file.
        file: PathBuf,
        /// Override the document title used by the checker.
        #[arg(long, value_name = "TEXT")]
        title: Option<String>,
        /// Override the natural language used by the checker.
        #[arg(long, value_name = "TAG")]
        lang: Option<String>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Build the accessibility tree a screen reader would receive.
    ///
    /// Uses the real page count from the document, the current page and the
    /// selected focus region, then prints every node with its AccessKit role,
    /// children and focus announcement. No screen-reader API is called; this is
    /// the deterministic tree a platform bridge publishes.
    Tree {
        /// Path to the PDF file.
        file: PathBuf,
        /// One-based current page.
        #[arg(long, value_name = "N")]
        page: Option<usize>,
        /// Focus region: menubar, toolbar, tabs, thumbnails, canvas, find,
        /// layers, bookmarks, attachments, links or status.
        #[arg(long, value_name = "REGION")]
        focus: Option<String>,
        /// Report the high-contrast accessibility labels.
        #[arg(long)]
        high_contrast: bool,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Audit the high-contrast palette, focus ring and shortcut map.
    Nav {
        /// Theme: `standard` or `high-contrast`.
        #[arg(long, default_value = "high-contrast")]
        theme: String,
        /// Number of forward Tabs to simulate.
        #[arg(long, default_value_t = 3)]
        tabs: usize,
        /// Traverse the focus ring backwards (Shift+Tab).
        #[arg(long)]
        reverse: bool,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Compute reflow metrics from a real page and a viewport.
    Layout {
        /// Path to the PDF file.
        file: PathBuf,
        /// One-based page.
        #[arg(long, value_name = "N")]
        page: Option<usize>,
        /// Viewport size as `WIDTHxHEIGHT` in points.
        #[arg(long, value_name = "WxH", default_value = "612x792")]
        viewport: String,
        /// Base font size in points.
        #[arg(long, default_value_t = 12.0)]
        font_size: f32,
        /// Line spacing factor.
        #[arg(long, default_value_t = 1.4)]
        line_spacing: f32,
        /// Margin in points.
        #[arg(long, default_value_t = 36.0)]
        margin: f32,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Operations on the `pdfxi versions` command group.
#[derive(Debug, Subcommand)]
enum VersionsCommand {
    /// Capture the page text of a PDF into the store.
    Snapshot {
        /// Path to the PDF file.
        file: PathBuf,
        /// Version store to create or update.
        #[arg(long, value_name = "PATH")]
        store: PathBuf,
        /// Optional label for this revision.
        #[arg(long, default_value = "")]
        label: String,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// List the snapshots in a store.
    List {
        /// Version store to read.
        #[arg(long, value_name = "PATH")]
        store: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Diff two snapshots, or a snapshot and a PDF file.
    Diff {
        /// Left side: a snapshot id or a PDF path.
        left: String,
        /// Right side: a snapshot id or a PDF path.
        right: String,
        /// Version store used to resolve snapshot ids.
        #[arg(long, value_name = "PATH")]
        store: Option<PathBuf>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Remove a snapshot from the store.
    Remove {
        /// Snapshot id to remove.
        id: u64,
        /// Version store to update.
        #[arg(long, value_name = "PATH")]
        store: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Operations on the `pdfxi font` command group.
#[derive(Debug, Subcommand)]
enum FontCommand {
    /// Embed the bundled Type0/CID font and insert a wrapped text box.
    AddText {
        /// Path to the PDF file.
        file: PathBuf,
        /// Where to write the edited document.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Text to place; newlines start a new paragraph.
        #[arg(long, value_name = "TEXT")]
        text: String,
        /// Rectangle in points: `x0,y0,x1,y1`.
        #[arg(long, value_name = "RECT")]
        rect: String,
        /// One-based page to edit.
        #[arg(long, value_name = "N", default_value_t = 1)]
        page: usize,
        /// Font size in points.
        #[arg(long, default_value_t = 12.0)]
        font_size: f64,
        /// Alignment: left, center or right.
        #[arg(long, default_value = "left")]
        align: String,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Operations on the `pdfxi structure` command group.
#[derive(Debug, Subcommand)]
enum StructureCommand {
    /// Write a tagged structure tree from a JSON description.
    Apply {
        /// Path to the PDF file.
        file: PathBuf,
        /// Where to write the tagged document.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// JSON tree description with `lang`, `nodes` and `roots`.
        #[arg(long, value_name = "PATH")]
        tree: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Actions of the `protection` command.
#[derive(Debug, Subcommand)]
enum ProtectionCommand {
    /// Validate a policy, optionally watermark, and write protected output.
    Apply {
        /// Input PDF.
        file: PathBuf,
        /// Protected output PDF.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Policy name.
        #[arg(long, value_name = "NAME")]
        name: String,
        /// User (open) password.
        #[arg(long, value_name = "PASSWORD")]
        password: String,
        /// Owner (permissions) password.
        #[arg(long, value_name = "PASSWORD")]
        owner: Option<String>,
        /// Security handler: `standard-r2`, `standard-r3`, `standard-r4` or `standard-r6`.
        #[arg(long, default_value = "standard-r6")]
        handler: String,
        /// Permissions: `all`, `none`, `read-only` or a comma list.
        #[arg(long, default_value = "all")]
        permissions: String,
        /// Declared recipient label (repeatable).
        #[arg(long = "recipient", value_name = "LABEL")]
        recipient: Vec<String>,
        /// Policy expiry in Unix seconds.
        #[arg(long, value_name = "UNIX")]
        expiry: Option<i64>,
        /// Evaluation time in Unix seconds (default: current time).
        #[arg(long, value_name = "UNIX")]
        now: Option<i64>,
        /// Stamp a visible watermark before protecting.
        #[arg(long)]
        watermark: bool,
        /// Watermark text.
        #[arg(long, default_value = "CONFIDENTIAL")]
        watermark_text: String,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Actions of the `prepress` command.
#[derive(Debug, Subcommand)]
enum PrepressCommand {
    /// Resolve a trapping preset and flattening policy against a real page.
    Plan {
        /// Source PDF.
        input: PathBuf,
        /// Trapping preset name.
        #[arg(long, default_value = "Default")]
        preset: String,
        /// Default trap width in points.
        #[arg(long, default_value_t = pdfxi_render::trapping::DEFAULT_TRAP_WIDTH_PT)]
        trap_width: f32,
        /// Black trap width in points.
        #[arg(long, default_value_t = pdfxi_render::trapping::DEFAULT_BLACK_WIDTH_PT)]
        black_width: f32,
        /// Black trap colour: `auto`, `knockout` or `c,m,y,k`.
        #[arg(long, default_value = "auto")]
        black_color: String,
        /// Trap join style: `miter`, `round` or `bevel`.
        #[arg(long, default_value = "miter")]
        join: String,
        /// Sliding traps: `none`, `auto` or a maximum distance in points.
        #[arg(long, default_value = "none")]
        sliding: String,
        /// Flattening resolution in DPI.
        #[arg(long, default_value_t = pdfxi_render::trapping::DEFAULT_FLATTEN_DPI)]
        dpi: u32,
        /// Vector/raster balance: `vector`, `balanced` or `raster`.
        #[arg(long, default_value = "balanced")]
        balance: String,
        /// Overprint policy: `preserve`, `simulate` or `knockout`.
        #[arg(long, default_value = "preserve")]
        overprint: String,
        /// Maximum transparency-group depth.
        #[arg(long, default_value_t = pdfxi_render::trapping::DEFAULT_FLATTEN_DEPTH)]
        max_depth: u8,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Options shared by the `production` actions.
#[derive(Debug, Args)]
struct ProductionArgs {
    /// Source PDF.
    input: PathBuf,
    /// Bleed as one value or `T,R,B,L` in points.
    #[arg(long, default_value = "0")]
    bleed: String,
    /// Printer marks: `crop`, `registration`, `colour-bars`, `page-info`, `all` or `none`.
    #[arg(long, default_value = "none")]
    marks: String,
    /// Separation as `NAME:process` or `NAME:spot` (repeatable).
    #[arg(long = "separation", value_name = "NAME:KIND")]
    separations: Vec<String>,
    /// Output intent as `SUBTYPE|NAME|REGISTRY|INTENT`.
    #[arg(long, value_name = "SPEC")]
    intent: Option<String>,
    /// Emit machine-readable JSON with stable field names.
    #[arg(long)]
    json: bool,
}

/// Actions of the `production` command.
#[derive(Debug, Subcommand)]
enum ProductionCommand {
    /// Resolve the production plan without writing a document.
    Plan {
        /// Production options.
        #[command(flatten)]
        options: ProductionArgs,
    },
    /// Apply the resolved bleed box and printer marks, then write a new PDF.
    Apply {
        /// Production options.
        #[command(flatten)]
        options: ProductionArgs,
        /// Destination PDF.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
    },
}

/// Actions of the `envelope` command.
#[derive(Debug, Subcommand)]
enum EnvelopeCommand {
    /// Wrap a file with recipients, permissions and an optional HMAC tag.
    Seal {
        /// Payload file.
        input: PathBuf,
        /// Written envelope.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Recipient as `LABEL[:PERM,PERM]` (repeatable).
        #[arg(long = "recipient", value_name = "LABEL[:PERM]")]
        recipient: Vec<String>,
        /// Envelope-wide permissions.
        #[arg(long, default_value = "all")]
        permissions: String,
        /// Raw HMAC key file.
        #[arg(long, value_name = "PATH")]
        key_file: Option<PathBuf>,
        /// HMAC key as hex.
        #[arg(long, value_name = "HEX")]
        key_hex: Option<String>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Open an envelope, optionally checking one recipient's permission.
    Open {
        /// Envelope file.
        envelope: PathBuf,
        /// Destination for the payload.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Raw HMAC key file.
        #[arg(long, value_name = "PATH")]
        key_file: Option<PathBuf>,
        /// HMAC key as hex.
        #[arg(long, value_name = "HEX")]
        key_hex: Option<String>,
        /// Recipient label for an authorization check.
        #[arg(long, value_name = "LABEL")]
        recipient: Option<String>,
        /// Permission the recipient must hold.
        #[arg(long, value_name = "PERM")]
        require: Option<String>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Verify an envelope without writing the payload.
    Verify {
        /// Envelope file.
        envelope: PathBuf,
        /// Raw HMAC key file.
        #[arg(long, value_name = "PATH")]
        key_file: Option<PathBuf>,
        /// HMAC key as hex.
        #[arg(long, value_name = "HEX")]
        key_hex: Option<String>,
        /// Recipient label for an authorization check.
        #[arg(long, value_name = "LABEL")]
        recipient: Option<String>,
        /// Permission the recipient must hold.
        #[arg(long, value_name = "PERM")]
        require: Option<String>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Identity file inputs for `pdfxi id`.
#[derive(Debug, Args)]
struct IdentityArgs {
    /// Password-protected PKCS#12 container.
    #[arg(long, value_name = "PATH")]
    pfx: Option<PathBuf>,
    /// PKCS#12 password (empty when omitted).
    #[arg(long, value_name = "PASSWORD", default_value = "")]
    password: String,
    /// PEM or DER PKCS#8 private key.
    #[arg(long, value_name = "PATH")]
    key: Option<PathBuf>,
    /// PEM or DER certificate.
    #[arg(long, value_name = "PATH")]
    cert: Option<PathBuf>,
}

impl IdentityArgs {
    /// Convert to the host's file description.
    fn into_files(self) -> id::IdentityFiles {
        id::IdentityFiles {
            pfx: self.pfx,
            password: self.password,
            key: self.key,
            cert: self.cert,
        }
    }
}

/// Actions of the `id` command.
#[derive(Debug, Subcommand)]
enum IdCommand {
    /// Validate and describe one identity without touching a catalog.
    Inspect {
        /// Identity files.
        #[command(flatten)]
        identity: IdentityArgs,
        /// Label used in the report.
        #[arg(long, default_value = "Identity")]
        label: String,
        /// Evaluation time in Unix seconds (default: current time).
        #[arg(long, value_name = "UNIX")]
        time: Option<u64>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Validate an identity and add its metadata to the catalog.
    Import {
        /// Metadata catalog file.
        #[arg(long, value_name = "PATH")]
        store: PathBuf,
        /// Catalog label.
        #[arg(long, value_name = "LABEL")]
        label: String,
        /// Identity files.
        #[command(flatten)]
        identity: IdentityArgs,
        /// Import time in Unix seconds (default: current time).
        #[arg(long, value_name = "UNIX")]
        time: Option<u64>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// List catalogued identities with their validity at a given time.
    List {
        /// Metadata catalog file.
        #[arg(long, value_name = "PATH")]
        store: PathBuf,
        /// Evaluation time in Unix seconds (default: current time).
        #[arg(long, value_name = "UNIX")]
        time: Option<u64>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Remove one catalogued identity.
    Remove {
        /// Metadata catalog file.
        #[arg(long, value_name = "PATH")]
        store: PathBuf,
        /// Catalog label.
        #[arg(long, value_name = "LABEL")]
        label: String,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Actions of the `trust` command.
#[derive(Debug, Subcommand)]
enum TrustCommand {
    /// Verify a leaf certificate against anchors, intermediates and CRLs.
    Verify {
        /// PEM bundle or DER file of trust anchors (repeatable entries).
        #[arg(long, value_name = "PATH")]
        anchor: PathBuf,
        /// Leaf certificate as PEM or DER.
        #[arg(long, value_name = "PATH")]
        leaf: PathBuf,
        /// Intermediate certificate or PEM bundle (repeatable).
        #[arg(long, value_name = "PATH")]
        intermediate: Vec<PathBuf>,
        /// DER-encoded CRL (repeatable).
        #[arg(long, value_name = "PATH")]
        crl: Vec<PathBuf>,
        /// Evaluation time in Unix seconds (default: current time).
        #[arg(long, value_name = "UNIX")]
        time: Option<u64>,
        /// Require revocation presence, freshness and CRL signatures.
        #[arg(long)]
        strict: bool,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Actions of the `sign` command.
///
/// `Create` carries the whole identity and output description while `Verify`
/// carries one path, so the variants differ in size. The value is constructed
/// once from the command line and never stored in a collection.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Subcommand)]
enum SignCommand {
    /// Sign a PDF with a CMS/RSA signature and write the signed file.
    Create {
        /// Input PDF.
        file: PathBuf,
        /// Signed output PDF.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// PEM private key.
        #[arg(long, value_name = "PATH", requires = "cert")]
        key: Option<PathBuf>,
        /// PEM signer certificate.
        #[arg(long, value_name = "PATH", requires = "key")]
        cert: Option<PathBuf>,
        /// Extra PEM certificates appended to the chain.
        #[arg(long, value_name = "PATH", requires_all = ["key", "cert"])]
        chain: Option<PathBuf>,
        /// Password-protected PKCS#12 identity.
        #[arg(long, value_name = "PATH")]
        pfx: Option<PathBuf>,
        /// PKCS#12 password (empty when omitted).
        #[arg(long, value_name = "PASSWORD", default_value = "")]
        password: String,
        /// One-based page for the invisible widget.
        #[arg(long, default_value_t = 1)]
        page: usize,
        /// Signature field name.
        #[arg(long, default_value = "Signature1")]
        field: String,
        /// Signing reason stored in `/Reason`.
        #[arg(long)]
        reason: Option<String>,
        /// Signing location stored in `/Location`.
        #[arg(long)]
        location: Option<String>,
        /// Signing time in Unix seconds (default: current time).
        #[arg(long, value_name = "UNIX")]
        time: Option<u64>,
        /// `/Contents` reservation in bytes.
        #[arg(long, default_value_t = pdfxi_security::DEFAULT_CONTENTS_RESERVATION)]
        reserve: usize,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Verify the last signature in a PDF.
    Verify {
        /// Signed PDF.
        file: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Actions of the `signature` command.
#[derive(Debug, Subcommand)]
enum SignatureCommand {
    /// Create a one-page PDF with a contract-checked signature field.
    Create {
        /// Destination PDF.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Field name.
        #[arg(long, default_value = "Signature1")]
        field: String,
        /// Signer name.
        #[arg(long)]
        signer: Option<String>,
        /// Signing reason.
        #[arg(long)]
        reason: Option<String>,
        /// Signing location.
        #[arg(long)]
        location: Option<String>,
        /// Widget rectangle as `x,y,w,h` in points.
        #[arg(long, default_value = "72,72,240,48")]
        rect: String,
        /// `/Contents` placeholder size in hex characters.
        #[arg(long, default_value_t = 8192)]
        placeholder_bytes: usize,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Plan the byte range a signer must cover.
    Plan {
        /// Prepared signature PDF.
        file: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Verify caller-supplied signature bytes structurally.
    Verify {
        /// Prepared signature PDF.
        file: PathBuf,
        /// Signature bytes as hex.
        #[arg(long, value_name = "HEX")]
        contents: String,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Actions of the `handoff` command.
#[derive(Debug, Subcommand)]
enum HandoffCommand {
    /// Resolve a local file (or `file:` URL) and print the handoff plan.
    Open {
        /// Local file or `file:` URL.
        file: String,
        /// Declared MIME type from the allowlist.
        #[arg(long, value_name = "TYPE")]
        mime: Option<String>,
        /// Launch the platform browser after validation.
        #[arg(long)]
        launch: bool,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Parse a path or URL and print the resolved plan.
    Parse {
        /// Local path or `file:` URL.
        input: String,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Build the HTML `file:` URL with an optional fragment.
    Link {
        /// Local HTML file.
        file: PathBuf,
        /// URL fragment.
        #[arg(long, value_name = "FRAGMENT")]
        fragment: Option<String>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Actions of the `shell` command.
#[derive(Debug, Subcommand)]
enum ShellCommand {
    /// Print the validated descriptor document.
    Plan {
        /// Executable to register; defaults to the running binary.
        #[arg(long, value_name = "PATH")]
        exe: Option<PathBuf>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Write the per-user association and protocol keys.
    Register {
        /// Per-user registry root (must be under HKCU\Software\).
        #[arg(long, default_value = "HKCU\\Software\\Classes", value_name = "KEY")]
        root: String,
        /// Executable to register; defaults to the running binary.
        #[arg(long, value_name = "PATH")]
        exe: Option<PathBuf>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Remove the per-user association and protocol keys.
    Unregister {
        /// Per-user registry root (must be under HKCU\Software\).
        #[arg(long, default_value = "HKCU\\Software\\Classes", value_name = "KEY")]
        root: String,
        /// Executable whose command is reported; defaults to the running binary.
        #[arg(long, value_name = "PATH")]
        exe: Option<PathBuf>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Report whether the keys are installed.
    Status {
        /// Per-user registry root (must be under HKCU\Software\).
        #[arg(long, default_value = "HKCU\\Software\\Classes", value_name = "KEY")]
        root: String,
        /// Executable whose command is reported; defaults to the running binary.
        #[arg(long, value_name = "PATH")]
        exe: Option<PathBuf>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Actions of the `policy` command.
#[derive(Debug, Subcommand)]
enum PolicyCommand {
    /// Print a deployment policy document.
    Show {
        /// Policy file.
        file: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Write a deployment policy document.
    Write {
        /// Destination file.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Base theme: `system`, `light` or `dark`.
        #[arg(long, default_value = "system")]
        theme: String,
        /// Base locale tag.
        #[arg(long, default_value = "en-US")]
        locale: String,
        /// Base recent-document count (0-100).
        #[arg(long, default_value_t = 10)]
        recent_count: u32,
        /// Base toolbar visibility.
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        show_toolbar: bool,
        /// Base default zoom percent (25-400).
        #[arg(long, default_value_t = 100)]
        zoom: u16,
        /// Base autosave interval in seconds (0-3600).
        #[arg(long, default_value_t = 300)]
        autosave: u32,
        /// Update channel: `stable`, `beta`, `nightly` or `off`.
        #[arg(long, default_value = "stable")]
        channel: String,
        /// Feature toggle as `NAME=true|false`; repeatable.
        #[arg(long = "toggle", value_name = "NAME=BOOL")]
        toggle: Vec<String>,
        /// Allowed local path prefix; repeatable.
        #[arg(long = "allow-path", value_name = "PATH")]
        allow_path: Vec<String>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Merge an overlay policy over a base policy.
    Merge {
        /// Base policy file.
        base: PathBuf,
        /// Overlay policy file.
        overlay: PathBuf,
        /// Write the merged document here.
        #[arg(long, value_name = "PATH")]
        out: Option<PathBuf>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Check requested settings against a deployment policy.
    Check {
        /// Policy file.
        file: PathBuf,
        /// Requested theme.
        #[arg(long)]
        theme: Option<String>,
        /// Requested update channel.
        #[arg(long)]
        channel: Option<String>,
        /// Requested feature toggle; repeatable.
        #[arg(long = "toggle", value_name = "NAME")]
        toggle: Vec<String>,
        /// Requested local path; repeatable.
        #[arg(long = "path", value_name = "PATH")]
        path: Vec<String>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Parse a plain preferences document.
    Preferences {
        /// Preferences file.
        file: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Actions of the `media` command.
#[derive(Debug, Subcommand)]
enum MediaCommand {
    /// List `/Sound`, `/Movie` and `/Screen` annotations and their plans.
    List {
        /// PDF file.
        file: PathBuf,
        /// Directory for extracted embedded payloads.
        #[arg(long, value_name = "DIR")]
        extract: Option<PathBuf>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Write the local-only HTML5 `<audio>`/`<video>` envelope.
    Html5 {
        /// Source as `SRC=MIME`; a leading `@` marks an embedded stream.
        #[arg(long = "source", value_name = "SRC=MIME", required = true)]
        source: Vec<String>,
        /// Poster image reference.
        #[arg(long, value_name = "SRC")]
        poster: Option<String>,
        /// Show native playback controls.
        #[arg(long)]
        controls: bool,
        /// Start playback automatically.
        #[arg(long)]
        autoplay: bool,
        /// Loop playback.
        #[arg(long = "loop")]
        loop_play: bool,
        /// Start muted.
        #[arg(long)]
        muted: bool,
        /// Element width in CSS pixels (0 = unspecified).
        #[arg(long, default_value_t = 0)]
        width: u32,
        /// Element height in CSS pixels (0 = unspecified).
        #[arg(long, default_value_t = 0)]
        height: u32,
        /// Write the complete HTML document here; stdout receives the element.
        #[arg(long, value_name = "PATH")]
        out: Option<PathBuf>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Actions of the `geo` command.
#[derive(Debug, Subcommand)]
enum GeoCommand {
    /// Report the page's geospatial viewports and plan.
    Info {
        /// PDF file.
        file: PathBuf,
        /// One-based page.
        #[arg(long, value_name = "N")]
        page: Option<usize>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Map a PDF-space point to longitude/latitude.
    ToGeo {
        /// PDF file.
        file: PathBuf,
        /// One-based page.
        #[arg(long, value_name = "N")]
        page: Option<usize>,
        /// PDF x coordinate.
        #[arg(long, value_name = "X", allow_hyphen_values = true)]
        x: f64,
        /// PDF y coordinate.
        #[arg(long, value_name = "Y", allow_hyphen_values = true)]
        y: f64,
        /// Zero-based viewport; the first by default.
        #[arg(long, value_name = "N")]
        viewport: Option<usize>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Map longitude/latitude to a PDF-space point.
    ToPdf {
        /// PDF file.
        file: PathBuf,
        /// One-based page.
        #[arg(long, value_name = "N")]
        page: Option<usize>,
        /// Longitude in degrees.
        #[arg(long, value_name = "DEG", allow_hyphen_values = true)]
        lon: f64,
        /// Latitude in degrees.
        #[arg(long, value_name = "DEG", allow_hyphen_values = true)]
        lat: f64,
        /// Zero-based viewport; the containing viewport by default.
        #[arg(long, value_name = "N")]
        viewport: Option<usize>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Actions of the `color` command.
#[derive(Debug, Subcommand)]
enum ColorCommand {
    /// Validate one ICC profile file and report its header, tags and plan.
    Icc {
        /// ICC profile file.
        file: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// List `/OutputIntents` and validate the embedded destination profile.
    Intents {
        /// PDF file.
        file: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Convert one colour through a profile's analytic plan.
    Convert {
        /// ICC profile file.
        profile: PathBuf,
        /// Source space: `gray`, `rgb` or `cmyk`.
        #[arg(long, default_value = "rgb")]
        space: String,
        /// Comma-separated normalised components in 0..=1.
        #[arg(long, value_name = "C1,C2,...")]
        values: String,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Actions of the `home` command.
#[derive(Debug, Subcommand)]
enum HomeCommand {
    /// List recents, favorites and collections.
    List {
        /// State file.
        #[arg(long, value_name = "PATH")]
        state: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Record a document as recently opened.
    Add {
        /// Document path.
        file: PathBuf,
        /// State file.
        #[arg(long, value_name = "PATH")]
        state: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Add or remove a favorite.
    Favorite {
        /// Document path.
        file: PathBuf,
        /// State file.
        #[arg(long, value_name = "PATH")]
        state: PathBuf,
        /// Remove the favorite instead of adding it.
        #[arg(long)]
        remove: bool,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Create, edit or delete a collection.
    Collection {
        /// Collection name.
        name: String,
        /// State file.
        #[arg(long, value_name = "PATH")]
        state: PathBuf,
        /// Add a document to the collection.
        #[arg(long, value_name = "PATH")]
        add: Option<PathBuf>,
        /// Remove a document from the collection.
        #[arg(long, value_name = "PATH")]
        remove: Option<PathBuf>,
        /// Delete the collection.
        #[arg(long)]
        delete: bool,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Actions of the `unicode` command.
#[derive(Debug, Subcommand)]
enum UnicodeCommand {
    /// Analyze one PDF's text or a literal `--text` string.
    Analyze {
        /// Source PDF; omit when using `--text`.
        file: Option<PathBuf>,
        /// Literal text to analyze instead of a PDF.
        #[arg(long, value_name = "TEXT", conflicts_with = "file")]
        text: Option<String>,
        /// Font candidate as `FAMILY=script1,script2`; repeat for a chain.
        #[arg(long = "font", value_name = "FAMILY=SCRIPTS")]
        font: Vec<String>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Actions of the `transparency` command.
#[derive(Debug, Subcommand)]
enum TransparencyCommand {
    /// Inspect page groups and graphics-state blend modes.
    Inspect {
        /// Path to the PDF file.
        file: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Actions of the `script` command.
#[derive(Debug, Subcommand)]
enum ScriptCommand {
    /// Execute one script file.
    Run {
        /// JavaScript file.
        script: PathBuf,
        /// Seed a form field as `NAME=VALUE`; repeat for several fields.
        #[arg(long = "field", value_name = "NAME=VALUE")]
        field: Vec<String>,
        /// Seed `event.key`.
        #[arg(long, value_name = "KEY")]
        event_key: Option<String>,
        /// Seed `event.value`.
        #[arg(long, value_name = "VALUE")]
        event_value: Option<String>,
        /// Override the default 100000-step CPU budget.
        #[arg(long, value_name = "N")]
        max_steps: Option<u64>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Actions of the `office` command.
#[derive(Debug, Subcommand)]
enum OfficeCommand {
    /// Validate the envelope and print the deterministic adapter steps.
    Plan {
        /// Integration manifest JSON.
        manifest: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Execute the plan and re-read the written output.
    Convert {
        /// Integration manifest JSON.
        manifest: PathBuf,
        /// Destination PDF, DOCX, XLSX or PPTX.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Operations on the `pdfxi create` command group.
#[derive(Debug, Subcommand)]
enum CreateCommand {
    /// Create a one-page PDF from a PNG or JPEG image.
    Image {
        /// Path to the source PNG or JPEG image.
        input: PathBuf,
        /// Destination path for the generated PDF.
        output: PathBuf,
        /// Page geometry: `letter` (default) or `a4`.
        #[arg(long, value_enum, default_value = "letter")]
        page_size: CreatePageSize,
        /// Uniform margin on every side, in PDF points.
        #[arg(long, value_name = "PT", default_value_t = 72.0)]
        margin: f64,
        /// Body font size in points (accepted for uniformity; images ignore it).
        #[arg(long, value_name = "PT", default_value_t = 11.0)]
        font_size: f64,
        /// Line-height multiplier (accepted for uniformity; images ignore it).
        #[arg(long, value_name = "FACTOR", default_value_t = 1.2)]
        line_height: f64,
    },
    /// Create a paginated PDF from plain text or the documented Markdown subset.
    Text {
        /// Path to the source UTF-8 text or Markdown file.
        input: PathBuf,
        /// Destination path for the generated PDF.
        output: PathBuf,
        /// Page geometry: `letter` (default) or `a4`.
        #[arg(long, value_enum, default_value = "letter")]
        page_size: CreatePageSize,
        /// Uniform margin on every side, in PDF points.
        #[arg(long, value_name = "PT", default_value_t = 72.0)]
        margin: f64,
        /// Body font size in points.
        #[arg(long, value_name = "PT", default_value_t = 11.0)]
        font_size: f64,
        /// Line-height multiplier.
        #[arg(long, value_name = "FACTOR", default_value_t = 1.2)]
        line_height: f64,
        /// Parse as Markdown even without an .md/.markdown extension.
        #[arg(long)]
        markdown: bool,
    },
    /// Overlay an invisible searchable-text layer from OCR words.
    ///
    /// `--words` is the caller-supplied recognition result (one page, top-left
    /// pixel boxes); the command embeds the image and writes invisible text at
    /// the planned positions. No OCR engine or process is bundled.
    Ocr {
        /// Scanned PNG or JPEG page.
        image: PathBuf,
        /// Destination PDF.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// JSON words file.
        #[arg(long, value_name = "PATH")]
        words: PathBuf,
        /// Declared image resolution in dots per inch.
        #[arg(long, default_value_t = 300.0)]
        dpi: f64,
        /// Override the detected language tag.
        #[arg(long, value_name = "TAG")]
        language: Option<String>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Assemble a PDF portfolio from a manifest and real files.
    ///
    /// The manifest JSON carries `title` and `entries` with the in-portfolio
    /// `name`, optional `description`/`order`, and the source `file` on disk.
    /// Entry names must be relative and traversal-free; every embedded stream is
    /// re-read from the written PDF and compared with its source bytes.
    Portfolio {
        /// Portfolio manifest JSON.
        manifest: PathBuf,
        /// Destination PDF.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },

    /// Convert an Office intake bundle into a PDF.
    ///
    /// The bundle JSON carries `kind` (`word`, `excel`, `powerpoint`), `title`
    /// and already-extracted HTML/XML `markup`. No OOXML archive or Office
    /// process is involved; the extractor remains host-side.
    Office {
        /// Bundle manifest JSON.
        bundle: PathBuf,
        /// Destination PDF.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Reject bundles with unsupported constructs instead of reporting them.
        #[arg(long)]
        strict: bool,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Assemble scanned PNG page images into a PDF.
    Scan {
        /// Destination PDF.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Scanned PNG page; repeat in scan order.
        #[arg(long = "scan", value_name = "PNG", required = true)]
        scans: Vec<PathBuf>,
        /// Declared scanner resolution in dots per inch.
        #[arg(long, default_value_t = 300.0)]
        dpi: f64,
        /// Transport: simplex, front-first or back-first.
        #[arg(long, default_value = "simplex")]
        duplex: String,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Create a PDF from the supported PostScript or EPS subset.
    ///
    /// DSC headers and a bounded text/line/rectangle operator subset are
    /// ingested. Constructs outside that subset are reported (or rejected with
    /// `--strict`) rather than guessed; this is not a PostScript interpreter.
    Postscript {
        /// Path to the PostScript or EPS source.
        input: PathBuf,
        /// Destination path for the generated PDF.
        output: PathBuf,
        /// Reject any construct outside the supported subset.
        #[arg(long)]
        strict: bool,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Operations on the `pdfxi export` command group.
#[derive(Debug, Subcommand)]
enum ExportCommand {
    /// Export accessible text as a structured representation.
    Tagged {
        /// Path to the source PDF.
        input: PathBuf,
        /// Destination path for the exported text.
        output: PathBuf,
        /// Representation to write: `structured` (default) or `plain`.
        #[arg(long, value_enum, default_value = "structured")]
        format: TaggedOutputFormat,
    },
    /// Export positioned page text as DSC PostScript.
    ///
    /// Text runs keep their page coordinates, font size and page geometry.
    /// Vector art, images and annotations are not represented and are listed
    /// as omitted; this is the documented text subset, not a full converter.
    Postscript {
        /// Path to the source PDF.
        input: PathBuf,
        /// Destination `.ps` file.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Normalize a PDF for a PDF/A, PDF/X or PDF/E target.
    ///
    /// This is subset normalization: it raises the header version when needed,
    /// adds the required XMP and output-intent placeholders, checks for
    /// forbidden catalog entries, rewrites the xref deterministically, and
    /// reports every enforced and reported-only constraint. It never claims
    /// certification.
    Conformance {
        /// Path to the source PDF.
        input: PathBuf,
        /// Destination path; required unless `--plan` is used.
        #[arg(long, value_name = "PATH")]
        out: Option<PathBuf>,
        /// Target profile, for example pdfa-2b, pdfx-4 or pdfe-1.
        #[arg(long, value_name = "PROFILE")]
        profile: String,
        /// Forbidden-entry policy: report (default) or reject.
        #[arg(long, default_value = "report")]
        policy: String,
        /// Explicit output-intent subtype, for example GTS_PDFA1.
        #[arg(long, value_name = "SUBTYPE")]
        output_intent: Option<String>,
        /// Do not raise the header version to the target minimum.
        #[arg(long)]
        no_normalize_version: bool,
        /// Do not add the XMP metadata placeholder.
        #[arg(long)]
        no_xmp: bool,
        /// Do not add the output-intent placeholder.
        #[arg(long)]
        no_output_intent: bool,
        /// Print the byte-free plan instead of exporting.
        #[arg(long)]
        plan: bool,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
}

/// Page-level edit operations exposed by `pdfxi pages`.
#[derive(Debug, Subcommand)]
enum PagesCommand {
    /// Keep the selected pages, in the order given.
    Extract {
        /// Source PDF.
        input: PathBuf,
        /// Page selection: `all`, `1,3` or `2-4`.
        #[arg(long, value_name = "SPEC")]
        pages: String,
        /// Destination PDF.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
    },
    /// Remove the selected pages.
    Delete {
        /// Source PDF.
        input: PathBuf,
        /// Page selection: `all`, `1,3` or `2-4`.
        #[arg(long, value_name = "SPEC")]
        pages: String,
        /// Destination PDF.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
    },
    /// Move one page to a new one-based position.
    Move {
        /// Source PDF.
        input: PathBuf,
        /// One-based position of the page to move.
        #[arg(long, value_name = "N")]
        from: usize,
        /// One-based position the page should end up at.
        #[arg(long, value_name = "N")]
        to: usize,
        /// Destination PDF.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
    },
    /// Reorder all pages with an explicit permutation.
    Reorder {
        /// Source PDF.
        input: PathBuf,
        /// One-based permutation, e.g. `3,1,2`.
        #[arg(long, value_name = "ORDER")]
        order: String,
        /// Destination PDF.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
    },
    /// Set the crop box of the selected pages.
    Crop {
        /// Source PDF.
        input: PathBuf,
        /// Page selection; defaults to every page.
        #[arg(long, value_name = "SPEC", default_value = "all")]
        pages: String,
        /// Crop box as four points: `left,bottom,right,top`.
        #[arg(long, value_name = "BOX")]
        bounds: String,
        /// Destination PDF.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
    },
    /// Resize the selected pages, scaling their content to fit.
    Resize {
        /// Source PDF.
        input: PathBuf,
        /// Page selection; defaults to every page.
        #[arg(long, value_name = "SPEC", default_value = "all")]
        pages: String,
        /// Target size in points as `WIDTHxHEIGHT`, e.g. `612x792`.
        #[arg(long, value_name = "WxH")]
        size: String,
        /// Destination PDF.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
    },
    /// Rotate the selected pages clockwise.
    Rotate {
        /// Source PDF.
        input: PathBuf,
        /// Page selection; defaults to every page.
        #[arg(long, value_name = "SPEC", default_value = "all")]
        pages: String,
        /// Clockwise angle in degrees: 90, 180 or 270 (negative angles allowed).
        #[arg(long, value_name = "DEG", allow_negative_numbers = true)]
        angle: i32,
        /// Destination PDF.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
    },
    /// Insert every page of another document after a position (0 = first).
    Insert {
        /// Source PDF.
        input: PathBuf,
        /// One-based position to insert after; 0 puts the pages first.
        #[arg(long, value_name = "N")]
        after: usize,
        /// Document whose pages are inserted.
        other: PathBuf,
        /// Destination PDF.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
    },
    /// Replace one page with a page from another document.
    Replace {
        /// Source PDF.
        input: PathBuf,
        /// One-based page to replace.
        #[arg(long, value_name = "N")]
        page: usize,
        /// Document that provides the replacement page.
        other: PathBuf,
        /// One-based page of `other` to use.
        #[arg(long, value_name = "N", default_value_t = 1)]
        source_page: usize,
        /// Destination PDF.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
    },
    /// Concatenate documents in the order given.
    Merge {
        /// Two or more source PDFs.
        #[arg(required = true, num_args = 2..)]
        inputs: Vec<PathBuf>,
        /// Destination PDF.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
    },
    /// Split into numbered parts of `every` pages.
    Split {
        /// Source PDF.
        input: PathBuf,
        /// Pages per part.
        #[arg(long, value_name = "N", default_value_t = 1)]
        every: usize,
        /// Directory that receives `<stem>-01.pdf`, `<stem>-02.pdf`, ...
        #[arg(long, value_name = "DIR")]
        out_dir: PathBuf,
    },
}

/// Virtual-printer operations exposed by `pdfxi printer`.
#[derive(Debug, Subcommand)]
enum PrinterCommand {
    /// Create the capture port and the `PDFXI PDF` queue.
    Install {
        /// Queue name shown in the Windows print dialog.
        #[arg(long, default_value = printer::DEFAULT_PRINTER)]
        name: String,
        /// Absolute path of the capture file the spooler writes.
        #[arg(long, value_name = "PATH")]
        port: Option<PathBuf>,
        /// Emit the result as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Report whether the queue and capture port exist.
    Status {
        /// Queue name.
        #[arg(long, default_value = printer::DEFAULT_PRINTER)]
        name: String,
        /// Absolute path of the capture file.
        #[arg(long, value_name = "PATH")]
        port: Option<PathBuf>,
        /// Emit machine-readable JSON with stable field names.
        #[arg(long)]
        json: bool,
    },
    /// Remove the queue and its capture port.
    Remove {
        /// Queue name.
        #[arg(long, default_value = printer::DEFAULT_PRINTER)]
        name: String,
        /// Absolute path of the capture file.
        #[arg(long, value_name = "PATH")]
        port: Option<PathBuf>,
    },
    /// Wait for one finished print job and copy it to `--out`.
    Watch {
        /// Queue name being watched.
        #[arg(long, default_value = printer::DEFAULT_PRINTER)]
        name: String,
        /// Absolute path of the capture file.
        #[arg(long, value_name = "PATH")]
        port: Option<PathBuf>,
        /// Destination path for the captured PDF.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Give up after this many seconds.
        #[arg(long, value_name = "SECS", default_value_t = 300)]
        timeout: u64,
    },
}

/// Page geometry accepted by `create image` and `create text`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum CreatePageSize {
    /// US Letter, 612 by 792 points.
    #[default]
    Letter,
    /// ISO A4.
    A4,
}

impl CreatePageSize {
    /// Map to the CLI module's page-size choice.
    const fn choice(self) -> create::PageSizeChoice {
        match self {
            Self::Letter => create::PageSizeChoice::Letter,
            Self::A4 => create::PageSizeChoice::A4,
        }
    }
}

/// Tagged-text representation accepted by `export tagged`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum TaggedOutputFormat {
    /// Nested, indented outline with `@role` markers.
    #[default]
    Structured,
    /// Flat text with `[role]` markers and optional alt text.
    Plain,
}

impl TaggedOutputFormat {
    /// Map to the CLI module's tagged-format choice.
    const fn format(self) -> export::TaggedFormat {
        match self {
            Self::Structured => export::TaggedFormat::Structured,
            Self::Plain => export::TaggedFormat::Plain,
        }
    }
}

/// Image encoding selected by `pdfxi render --format`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum OutputFormat {
    /// 8-bit RGBA PNG using uncompressed deflate blocks.
    #[default]
    Png,
    /// Binary P6 Portable Pixmap (alpha discarded).
    Ppm,
}

#[cfg(feature = "pdfium")]
impl OutputFormat {
    /// Return the uppercase name used in status and error messages.
    const fn label(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Ppm => "PPM",
        }
    }
}

/// Stable, machine-readable shape of the `info --json` output.
#[derive(Debug, Serialize)]
struct InfoOutput {
    /// Display path of the input file.
    file: String,
    /// Declared PDF version, for example `1.7`.
    pdf_version: String,
    /// Length of the source file in bytes.
    bytes: usize,
    /// Number of leaf pages resolved from the page tree.
    pages: usize,
}

/// Stable, machine-readable shape of the `search --json` output.
#[derive(Debug, Serialize)]
struct SearchOutput {
    /// Display path of the input file.
    file: String,
    /// The query exactly as supplied on the command line.
    query: String,
    /// Every match, ordered by page and then character offset.
    matches: Vec<MatchOutput>,
}

/// One match in the `search --json` output.
#[derive(Debug, Serialize)]
struct MatchOutput {
    /// One-based page number the match was found on.
    page: usize,
    /// Zero-based character offset into the page's concatenated text.
    offset: usize,
    /// The matched text, preserving its original casing.
    text: String,
}

fn main() -> anyhow::Result<()> {
    // The clap command tree is one deeply nested builder expression. In a debug
    // build, constructing it can exhaust the default main-thread stack as more
    // commands are added, so the CLI runs on a worker thread with a generous
    // stack. Behaviour and output are unchanged.
    let worker = std::thread::Builder::new()
        .name("pdfxi-cli".to_owned())
        .stack_size(16 * 1024 * 1024)
        .spawn(run_cli)?;
    match worker.join() {
        Ok(result) => result,
        Err(_) => {
            eprintln!("pdfxi: internal error: the CLI worker panicked");
            std::process::exit(101);
        }
    }
}

fn run_cli() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Info { file, json } => print_info(&file, json),
        Command::Compat { file, json, strict } => print_compatibility(&file, json, strict),
        Command::Text { file } => extract_text(&file),
        Command::Search {
            file,
            query,
            case_sensitive,
            whole_word,
            json,
        } => search(&file, &query, case_sensitive, whole_word, json),
        Command::Replace {
            file,
            out,
            find,
            replacement,
            case_sensitive,
            whole_word,
            diacritic_insensitive,
            limit,
            json,
        } => replace::run_replace(replace::ReplaceRequest {
            file: &file,
            out: &out,
            find: &find,
            replacement: &replacement,
            options: pdfxi_edit::SearchOptions {
                case_sensitive,
                whole_word,
                diacritic_insensitive,
            },
            limit,
            json,
        }),
        Command::Measure {
            file,
            page,
            points,
            unit,
            dpi,
            decimals,
            json,
        } => measure::run_measure(&file, page, &points, unit.as_deref(), dpi, decimals, json),
        Command::Render {
            file,
            page,
            scale,
            out,
            format,
        } => render(&file, page, scale, &out, format),
        Command::Layers { file, json } => layers::run_layers(&file, json),
        Command::Links { file, json } => links::run_links(&file, json),
        Command::Attachments {
            file,
            json,
            extract,
        } => links::run_attachments(&file, json, extract.as_deref()),
        Command::Timestamp { command } => match command {
            TimestampCommand::Sign {
                content,
                out,
                key,
                cert,
                digest,
                policy,
                serial,
                time,
                nonce,
                ordering,
                json,
            } => timestamp::sign(
                &content, &out, &key, &cert, &digest, &policy, serial, time, nonce, ordering, json,
            ),
            TimestampCommand::Verify {
                token,
                content,
                cert,
                anchor,
                chain,
                nonce,
                at,
                allow_any_eku,
                json,
            } => timestamp::verify(
                &token,
                &content,
                &cert,
                &anchor,
                &chain,
                nonce,
                at,
                allow_any_eku,
                json,
            ),
        },
        Command::Envelope { command } => match command {
            EnvelopeCommand::Seal {
                input,
                out,
                recipient,
                permissions,
                key_file,
                key_hex,
                json,
            } => envelope::seal(
                &input,
                &out,
                &recipient,
                &permissions,
                key_file.as_deref(),
                key_hex.as_deref(),
                json,
            ),
            EnvelopeCommand::Open {
                envelope,
                out,
                key_file,
                key_hex,
                recipient,
                require,
                json,
            } => envelope::open(
                &envelope,
                &out,
                key_file.as_deref(),
                key_hex.as_deref(),
                recipient.as_deref(),
                require.as_deref(),
                json,
            ),
            EnvelopeCommand::Verify {
                envelope,
                key_file,
                key_hex,
                recipient,
                require,
                json,
            } => envelope::verify(
                &envelope,
                key_file.as_deref(),
                key_hex.as_deref(),
                recipient.as_deref(),
                require.as_deref(),
                json,
            ),
        },
        Command::Id { command } => match command {
            IdCommand::Inspect {
                identity,
                label,
                time,
                json,
            } => id::inspect(&identity.into_files(), &label, time, json),
            IdCommand::Import {
                store,
                label,
                identity,
                time,
                json,
            } => id::import(&store, &label, &identity.into_files(), time, json),
            IdCommand::List { store, time, json } => id::list(&store, time, json),
            IdCommand::Remove { store, label, json } => id::remove(&store, &label, json),
        },
        Command::Sign { command } => match command {
            SignCommand::Create {
                file,
                out,
                key,
                cert,
                chain,
                pfx,
                password,
                page,
                field,
                reason,
                location,
                time,
                reserve,
                json,
            } => sign::create(
                &file,
                &out,
                key.as_deref(),
                cert.as_deref(),
                chain.as_deref(),
                pfx.as_deref(),
                &password,
                page,
                &field,
                reason.as_deref(),
                location.as_deref(),
                time,
                reserve,
                json,
            ),
            SignCommand::Verify { file, json } => sign::verify(&file, json),
        },
        Command::Signature { command } => match command {
            SignatureCommand::Create {
                out,
                field,
                signer,
                reason,
                location,
                rect,
                placeholder_bytes,
                json,
            } => signature::create(
                &out,
                &field,
                signer.as_deref(),
                reason.as_deref(),
                location.as_deref(),
                &rect,
                placeholder_bytes,
                json,
            ),
            SignatureCommand::Plan { file, json } => signature::plan(&file, json),
            SignatureCommand::Verify {
                file,
                contents,
                json,
            } => signature::verify(&file, &contents, json),
        },
        Command::Handoff { command } => match command {
            HandoffCommand::Open {
                file,
                mime,
                launch,
                json,
            } => handoff::open(&file, mime.as_deref(), launch, json),
            HandoffCommand::Parse { input, json } => handoff::parse(&input, json),
            HandoffCommand::Link {
                file,
                fragment,
                json,
            } => handoff::link(&file, fragment.as_deref(), json),
        },
        Command::Protect {
            file,
            out,
            password,
            owner,
            method,
            permissions,
            revision,
            encrypt_metadata,
            json,
        } => protect::protect(
            &file,
            &out,
            &password,
            owner.as_deref(),
            &method,
            &permissions,
            revision,
            encrypt_metadata,
            json,
        ),
        Command::Unprotect {
            file,
            out,
            password,
            json,
        } => protect::unprotect(&file, &out, &password, json),
        Command::Trust { command } => match command {
            TrustCommand::Verify {
                anchor,
                leaf,
                intermediate,
                crl,
                time,
                strict,
                json,
            } => trust::verify(&anchor, &leaf, &intermediate, &crl, time, strict, json),
        },
        Command::Shell { command } => match command {
            ShellCommand::Plan { exe, json } => shell::plan(exe.as_deref(), json),
            ShellCommand::Register { root, exe, json } => {
                shell::register(&root, exe.as_deref(), json)
            }
            ShellCommand::Unregister { root, exe, json } => {
                shell::unregister(&root, exe.as_deref(), json)
            }
            ShellCommand::Status { root, exe, json } => shell::show(&root, exe.as_deref(), json),
        },
        Command::Policy { command } => match command {
            PolicyCommand::Show { file, json } => policy::show(&file, json),
            PolicyCommand::Write {
                out,
                theme,
                locale,
                recent_count,
                show_toolbar,
                zoom,
                autosave,
                channel,
                toggle,
                allow_path,
                json,
            } => policy::write(
                &out,
                &theme,
                &locale,
                recent_count,
                show_toolbar,
                zoom,
                autosave,
                &channel,
                &toggle,
                &allow_path,
                json,
            ),
            PolicyCommand::Merge {
                base,
                overlay,
                out,
                json,
            } => policy::merge(&base, &overlay, out.as_deref(), json),
            PolicyCommand::Check {
                file,
                theme,
                channel,
                toggle,
                path,
                json,
            } => policy::check(
                &file,
                theme.as_deref(),
                channel.as_deref(),
                &toggle,
                &path,
                json,
            ),
            PolicyCommand::Preferences { file, json } => policy::preferences(&file, json),
        },
        Command::Ink { file, json } => separations::ink(&file, json),
        Command::Separations { file, plate, json } => {
            separations::separations(&file, plate.as_deref(), json)
        }
        Command::Media { command } => match command {
            MediaCommand::List {
                file,
                extract,
                json,
            } => media::list(&file, extract.as_deref(), json),
            MediaCommand::Html5 {
                source,
                poster,
                controls,
                autoplay,
                loop_play,
                muted,
                width,
                height,
                out,
                json,
            } => media::html5(
                &source,
                poster.as_deref(),
                controls,
                autoplay,
                loop_play,
                muted,
                width,
                height,
                out.as_deref(),
                json,
            ),
        },
        Command::Interactions { file, strict, json } => {
            interactions::interactions(&file, strict, json)
        }
        Command::Media3d {
            file,
            extract,
            json,
        } => media3d::info(&file, extract.as_deref(), json),
        Command::Geo { command } => match command {
            GeoCommand::Info { file, page, json } => geo::info(&file, page, json),
            GeoCommand::ToGeo {
                file,
                page,
                x,
                y,
                viewport,
                json,
            } => geo::to_geo(&file, page, x, y, viewport, json),
            GeoCommand::ToPdf {
                file,
                page,
                lon,
                lat,
                viewport,
                json,
            } => geo::to_pdf(&file, page, lon, lat, viewport, json),
        },
        Command::Color { command } => match command {
            ColorCommand::Icc { file, json } => color::icc(&file, json),
            ColorCommand::Intents { file, json } => color::intents(&file, json),
            ColorCommand::Convert {
                profile,
                space,
                values,
                json,
            } => color::convert(&profile, &space, &values, json),
        },
        Command::Home { command } => match command {
            HomeCommand::List { state, json } => home::list(&state, json),
            HomeCommand::Add { file, state, json } => home::add(&file, &state, json),
            HomeCommand::Favorite {
                file,
                state,
                remove,
                json,
            } => home::favorite(&file, &state, remove, json),
            HomeCommand::Collection {
                name,
                state,
                add,
                remove,
                delete,
                json,
            } => home::collection(
                &name,
                &state,
                add.as_deref(),
                remove.as_deref(),
                delete,
                json,
            ),
        },
        Command::Unicode { command } => match command {
            UnicodeCommand::Analyze {
                file,
                text,
                font,
                json,
            } => unicode::analyze(file.as_deref(), text.as_deref(), &font, json),
        },
        Command::FindFiles {
            inputs,
            query,
            case_sensitive,
            whole_word,
            max_results,
            max_per_file,
            json,
        } => find_files::find_files(
            &inputs,
            &query,
            case_sensitive,
            whole_word,
            max_results,
            max_per_file,
            json,
        ),
        Command::Transparency { command } => match command {
            TransparencyCommand::Inspect { file, json } => transparency::inspect(&file, json),
        },
        Command::Restyle {
            file,
            out,
            page,
            run,
            size,
            scale,
            char_spacing,
            word_spacing,
            color,
            json,
        } => restyle::run_restyle(
            &file,
            &out,
            page,
            run,
            size,
            scale,
            char_spacing,
            word_spacing,
            color.as_deref(),
            json,
        ),
        Command::Portfolio {
            file,
            json,
            extract,
            out,
            add,
            rename,
            describe,
            remove,
        } => {
            let managed = out.is_some()
                || !add.is_empty()
                || !rename.is_empty()
                || !describe.is_empty()
                || !remove.is_empty();
            if managed {
                let out = out.context("management requires --out PATH")?;
                portfolio::manage_portfolio(
                    &file,
                    &portfolio::ManageRequest {
                        out: &out,
                        add: &add,
                        rename: &rename,
                        describe: &describe,
                        remove: &remove,
                        json,
                    },
                )
            } else {
                portfolio::run_portfolio(&file, json, extract.as_deref())
            }
        }
        Command::Compare {
            left,
            right,
            json,
            lines,
            hunks,
            fail_on_difference,
        } => compare::run_compare(&left, &right, json, lines, hunks, fail_on_difference),
        Command::Reflow {
            file,
            width,
            page,
            json,
        } => reflow::run_reflow(&file, width, page, json),
        Command::ReadAloud {
            file,
            wpm,
            page,
            seek,
            voice,
            speak,
            voices,
            json,
        } => read_aloud::run_read_aloud(&read_aloud::ReadAloudRequest {
            file: file.as_deref(),
            wpm,
            page,
            seek,
            voice: &voice,
            speak,
            voices,
            json,
        }),
        Command::Optimize {
            file,
            out,
            remove_metadata,
            remove_optional,
            no_compress,
            json,
        } => optimize::run_optimize(
            &file,
            &out,
            remove_metadata,
            remove_optional,
            !no_compress,
            json,
        ),
        Command::Annotate { command } => match command {
            AnnotateCommand::Add {
                file,
                out,
                page,
                kind,
                rect,
                text,
                anchor,
                attachment,
                name,
                ink,
                stroke_width,
                stamp_name,
                font_size,
                author,
                json,
            } => annotate::run_add(
                &file,
                &out,
                page,
                kind,
                rect.as_deref(),
                text.as_deref(),
                anchor.as_deref(),
                font_size,
                &author,
                attachment
                    .as_deref()
                    .map(|path| path.to_string_lossy())
                    .as_deref(),
                name.as_deref(),
                ink.as_deref(),
                stroke_width,
                stamp_name.as_deref(),
                json,
            ),
            AnnotateCommand::List { file, page, json } => annotate::run_list(&file, page, json),
        },
        Command::Outline { command } => match command {
            OutlineCommand::List { file, json } => navigation::run_outline_list(&file, json),
            OutlineCommand::Add {
                file,
                out,
                title,
                page,
                parent,
                index,
                json,
            } => navigation::run_outline_add(navigation::OutlineAddRequest {
                file: &file,
                out: &out,
                title: &title,
                page,
                parent: parent.as_deref(),
                index,
                json,
            }),
            OutlineCommand::Rename {
                file,
                out,
                path,
                title,
                json,
            } => navigation::run_outline_rename(&file, &out, &path, &title, json),
            OutlineCommand::Move {
                file,
                out,
                path,
                parent,
                index,
                json,
            } => navigation::run_outline_move(&file, &out, &path, parent.as_deref(), index, json),
            OutlineCommand::Remove {
                file,
                out,
                path,
                json,
            } => navigation::run_outline_remove(&file, &out, &path, json),
        },
        Command::Link { command } => match command {
            LinkCommand::Add {
                file,
                out,
                page,
                rect,
                uri,
                to_page,
                highlight,
                json,
            } => navigation::run_link_add(navigation::LinkAddRequest {
                file: &file,
                out: &out,
                page,
                rect: &rect,
                uri: uri.as_deref(),
                to_page,
                highlight: &highlight,
                json,
            }),
            LinkCommand::Remove {
                file,
                out,
                page,
                index,
                json,
            } => navigation::run_link_remove(&file, &out, page, index, json),
        },
        Command::Attach { command } => match command {
            AttachCommand::Add {
                file,
                out,
                attachment,
                name,
                description,
                mime,
                json,
            } => navigation::run_attach_add(navigation::AttachAddRequest {
                file: &file,
                out: &out,
                attachment: &attachment,
                name: name.as_deref(),
                description: &description,
                mime: &mime,
                json,
            }),
            AttachCommand::Remove {
                file,
                out,
                name,
                json,
            } => navigation::run_attach_remove(&file, &out, &name, json),
        },
        Command::Form { command } => match command {
            FormCommand::Create { out, fields, json } => form::run_create(&out, &fields, json),
            FormCommand::Recognize {
                manifest,
                out,
                json,
            } => form::run_recognize(&manifest, &out, json),
            FormCommand::List { file, json } => form::run_list(&file, json),
            FormCommand::Validate {
                file,
                assignments,
                json,
            } => form::run_validate(&file, &assignments, json),
            FormCommand::Calculate {
                file,
                assignments,
                sums,
                products,
                averages,
                mins,
                maxs,
                decimals,
                json,
            } => {
                let mut requests: Vec<form::CalculationRequest<'_>> = Vec::new();
                for spec in &sums {
                    requests.push(form::CalculationRequest {
                        spec: spec.as_str(),
                        op: pdfxi_forms::CalculationOp::Sum,
                    });
                }
                for spec in &products {
                    requests.push(form::CalculationRequest {
                        spec: spec.as_str(),
                        op: pdfxi_forms::CalculationOp::Product,
                    });
                }
                for spec in &averages {
                    requests.push(form::CalculationRequest {
                        spec: spec.as_str(),
                        op: pdfxi_forms::CalculationOp::Average,
                    });
                }
                for spec in &mins {
                    requests.push(form::CalculationRequest {
                        spec: spec.as_str(),
                        op: pdfxi_forms::CalculationOp::Min,
                    });
                }
                for spec in &maxs {
                    requests.push(form::CalculationRequest {
                        spec: spec.as_str(),
                        op: pdfxi_forms::CalculationOp::Max,
                    });
                }
                form::run_calculate(&file, &assignments, &requests, decimals, json)
            }
            FormCommand::Tabs {
                file,
                policy,
                include_read_only,
                no_wrap,
                json,
            } => {
                let nav = pdfxi_forms::NavPolicy::default()
                    .with_read_only(!include_read_only)
                    .with_wrap(!no_wrap);
                form::run_tabs(&file, policy.as_deref(), nav, json)
            }
            FormCommand::Actions { file, json } => form::run_actions(&file, json),
            FormCommand::ReaderEnable {
                file,
                out,
                rights,
                reason,
                placeholder_bytes,
                json,
            } => form::run_reader_enable(
                &file,
                &out,
                &rights,
                reason.as_deref(),
                placeholder_bytes,
                json,
            ),
            FormCommand::Distribute {
                file,
                out,
                form_id,
                title,
                version,
                json,
            } => form::run_distribute(
                &file,
                &out,
                form_id.as_deref(),
                title.as_deref(),
                version,
                json,
            ),
            FormCommand::Collect {
                file,
                responses,
                json,
            } => form::run_collect(&file, &responses, json),
        },
        Command::Accessibility { command } => match command {
            AccessibilityCommand::Order { file, json } => a11y_edit::show_order(&file, json),
            AccessibilityCommand::Move {
                file,
                out,
                node,
                before,
                json,
            } => a11y_edit::move_node(&file, &out, node, before, json),
            AccessibilityCommand::Alt {
                file,
                out,
                node,
                text,
                clear,
                json,
            } => a11y_edit::set_alt(
                &file,
                &out,
                node,
                if clear { None } else { text.as_deref() },
                json,
            ),
            AccessibilityCommand::Table {
                file,
                out,
                node,
                scope,
                json,
            } => a11y_edit::set_scope(&file, &out, node, scope.as_deref(), json),
            AccessibilityCommand::Tree {
                file,
                page,
                focus,
                high_contrast,
                json,
            } => a11y_nav::tree(&file, page, focus.as_deref(), high_contrast, json),
            AccessibilityCommand::Nav {
                theme,
                tabs,
                reverse,
                json,
            } => a11y_nav::nav(&theme, tabs, reverse, json),
            AccessibilityCommand::Layout {
                file,
                page,
                viewport,
                font_size,
                line_spacing,
                margin,
                json,
            } => a11y_nav::layout(
                &file,
                page,
                &viewport,
                font_size,
                line_spacing,
                margin,
                json,
            ),
            AccessibilityCommand::Check {
                file,
                title,
                lang,
                json,
            } => accessibility::run_check(&file, title.as_deref(), lang.as_deref(), json),
        },
        Command::Versions { command } => match command {
            VersionsCommand::Snapshot {
                file,
                store,
                label,
                json,
            } => versions::run_snapshot(&file, &store, &label, json),
            VersionsCommand::List { store, json } => versions::run_list(&store, json),
            VersionsCommand::Diff {
                left,
                right,
                store,
                json,
            } => versions::run_diff(&left, &right, store.as_deref(), json),
            VersionsCommand::Remove { id, store, json } => versions::run_remove(id, &store, json),
        },
        Command::Font { command } => match command {
            FontCommand::AddText {
                file,
                out,
                text,
                rect,
                page,
                font_size,
                align,
                json,
            } => font::run_add_text(font::AddTextRequest {
                file: &file,
                out: &out,
                page,
                rect: &rect,
                text: &text,
                font_size,
                align: &align,
                json,
            }),
        },
        Command::Structure { command } => match command {
            StructureCommand::Apply {
                file,
                out,
                tree,
                json,
            } => structure::run_apply(&file, &out, &tree, json),
        },
        Command::Update {
            file,
            out,
            title,
            author,
            subject,
            keywords,
            producer,
            json,
        } => update::run_update(update::UpdateRequest {
            file: &file,
            out: &out,
            title: title.as_deref(),
            author: author.as_deref(),
            subject: subject.as_deref(),
            keywords: keywords.as_deref(),
            producer: producer.as_deref(),
            json,
        }),
        Command::Linearize {
            file,
            out,
            version,
            json,
        } => layout::run_linearize(&file, &out, &version, json),
        Command::Pack {
            file,
            out,
            no_compress,
            max_objects,
            json,
        } => layout::run_pack(&file, &out, !no_compress, max_objects, json),
        Command::Decorate { command } => match command {
            DecorateCommand::HeaderFooter {
                file,
                out,
                header,
                footer,
                side,
                font_size,
                margin,
                title,
                date,
                json,
            } => decorate::run_header_footer(
                &file,
                &out,
                &decorate::HeaderFooterRequest {
                    header: header.as_deref(),
                    footer: footer.as_deref(),
                    side,
                    font_size,
                    margin,
                    title: &title,
                    date: &date,
                    json,
                },
            ),
            DecorateCommand::Watermark {
                file,
                out,
                text,
                font_size,
                opacity,
                rotation,
                tile,
                json,
            } => decorate::run_watermark(
                &file,
                &out,
                &decorate::WatermarkRequest {
                    text: &text,
                    font_size,
                    opacity,
                    rotation,
                    tile,
                    json,
                },
            ),
            DecorateCommand::Bates {
                file,
                out,
                prefix,
                suffix,
                start,
                digits,
                increment,
                corner,
                font_size,
                margin,
                json,
            } => decorate::run_bates(
                &file,
                &out,
                &decorate::BatesRequest {
                    prefix: &prefix,
                    suffix: &suffix,
                    start,
                    digits,
                    increment,
                    corner,
                    font_size,
                    margin,
                    json,
                },
            ),
        },
        Command::Pages { command } => match command {
            PagesCommand::Extract { input, pages, out } => pages::extract(&input, &pages, &out),
            PagesCommand::Delete { input, pages, out } => pages::delete(&input, &pages, &out),
            PagesCommand::Move {
                input,
                from,
                to,
                out,
            } => pages::move_page(&input, from, to, &out),
            PagesCommand::Reorder { input, order, out } => pages::reorder(&input, &order, &out),
            PagesCommand::Crop {
                input,
                pages,
                bounds,
                out,
            } => pages::crop(&input, &pages, &bounds, &out),
            PagesCommand::Resize {
                input,
                pages,
                size,
                out,
            } => {
                let (width, height) = pages::parse_size(&size)?;
                pages::resize(&input, &pages, width, height, &out)
            }
            PagesCommand::Rotate {
                input,
                pages,
                angle,
                out,
            } => pages::rotate(&input, &pages, angle / 90, &out),
            PagesCommand::Insert {
                input,
                after,
                other,
                out,
            } => pages::insert(&input, after, &other, &out),
            PagesCommand::Replace {
                input,
                page,
                other,
                source_page,
                out,
            } => pages::replace(&input, page, &other, source_page, &out),
            PagesCommand::Merge { inputs, out } => pages::merge(&inputs, &out),
            PagesCommand::Split {
                input,
                every,
                out_dir,
            } => pages::split(&input, every, &out_dir),
        },
        Command::Printer { command } => match command {
            PrinterCommand::Install { name, port, json } => {
                let path = printer::install(&name, port.as_deref())?;
                if json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "printer": name,
                            "port": path,
                            "installed": true,
                        })
                    );
                } else {
                    println!("printer: {name}");
                    println!("port: {}", path.display());
                    println!("installed: yes");
                }
                Ok(())
            }
            PrinterCommand::Status { name, port, json } => {
                printer::status(&name, port.as_deref(), json)
            }
            PrinterCommand::Remove { name, port } => {
                printer::remove(&name, port.as_deref())?;
                println!("removed: {name}");
                Ok(())
            }
            PrinterCommand::Watch {
                name,
                port,
                out,
                timeout,
            } => {
                let path = printer::watch(
                    &name,
                    port.as_deref(),
                    &out,
                    std::time::Duration::from_secs(timeout),
                )?;
                println!("captured: {}", path.display());
                Ok(())
            }
        },
        Command::Create { command } => match command {
            CreateCommand::Image {
                input,
                output,
                page_size,
                margin,
                font_size,
                line_height,
            } => create::create_image(
                &input,
                &output,
                create::CreateSettings {
                    page_size: page_size.choice(),
                    margin_pt: margin,
                    font_size_pt: font_size,
                    line_height,
                },
            ),
            CreateCommand::Text {
                input,
                output,
                page_size,
                margin,
                font_size,
                line_height,
                markdown,
            } => create::create_text(
                &input,
                &output,
                create::CreateSettings {
                    page_size: page_size.choice(),
                    margin_pt: margin,
                    font_size_pt: font_size,
                    line_height,
                },
                markdown,
            ),
            CreateCommand::Postscript {
                input,
                output,
                strict,
                json,
            } => create::create_postscript(&input, &output, strict, json),
            CreateCommand::Ocr {
                image,
                out,
                words,
                dpi,
                language,
                json,
            } => ocr::create_ocr(&image, &out, &words, dpi, language.as_deref(), json),
            CreateCommand::Portfolio {
                manifest,
                out,
                json,
            } => create_portfolio::create_portfolio(&manifest, &out, json),
            CreateCommand::Office {
                bundle,
                out,
                strict,
                json,
            } => office::create_office(&bundle, &out, strict, json),
            CreateCommand::Scan {
                out,
                scans,
                dpi,
                duplex,
                json,
            } => scan::create_scan(&scans, &out, dpi, &duplex, json),
        },
        Command::Script { command } => match command {
            ScriptCommand::Run {
                script,
                field,
                event_key,
                event_value,
                max_steps,
                json,
            } => script::run_script_file(
                &script,
                &field,
                event_key.as_deref(),
                event_value.as_deref(),
                max_steps,
                json,
            ),
        },
        Command::Office { command } => match command {
            OfficeCommand::Plan { manifest, json } => {
                office_roundtrip::plan_office(&manifest, json)
            }
            OfficeCommand::Convert {
                manifest,
                out,
                json,
            } => office_roundtrip::convert_office(&manifest, &out, json),
        },
        Command::Export { command } => match command {
            ExportCommand::Postscript { input, out, json } => {
                export_postscript::export_postscript_file(&input, &out, json)
            }
            ExportCommand::Tagged {
                input,
                output,
                format,
            } => export::export_tagged(&input, &output, format.format()),
            ExportCommand::Conformance {
                input,
                out,
                profile,
                policy,
                output_intent,
                no_normalize_version,
                no_xmp,
                no_output_intent,
                plan,
                json,
            } => conformance::run_conformance(conformance::ExportRequest {
                input: &input,
                out: out.as_deref(),
                profile: &profile,
                policy: &policy,
                output_intent: output_intent.as_deref(),
                normalize_version: !no_normalize_version,
                add_xmp_metadata: !no_xmp,
                add_output_intent: !no_output_intent,
                plan_only: plan,
                json,
            }),
        },
        Command::Action { command } => match command {
            ActionCommand::Create {
                file,
                name,
                description,
                step,
                param,
                force,
                json,
            } => action::create_action(&file, &name, &description, &step, &param, force, json),
            ActionCommand::List { file } => action::list_actions(&file),
            ActionCommand::Show { file, name } => action::show_action(&file, &name),
            ActionCommand::Run {
                file,
                name,
                input,
                output_dir,
                execute,
                json,
            } => action::run_action(&file, &name, &input, output_dir.as_deref(), execute, json),
            ActionCommand::Sequence {
                input,
                step,
                out_dir,
                json,
            } => sequence::run_sequence_batch(&input, &step, out_dir.as_deref(), json),
        },
        Command::Droplet {
            action_file,
            name,
            inputs,
            json,
        } => action::run_action(&action_file, &name, &inputs, None, false, json),
        Command::Preflight {
            profile,
            input,
            fix,
            output,
            json,
        } => preflight::run_preflight(&profile, &input, fix, output.as_deref(), json),
        Command::Loupe {
            input,
            page,
            zoom,
            lens,
            shape,
            viewport,
            center,
            commands,
            json,
        } => loupe::inspect(
            &input,
            page,
            zoom,
            &lens,
            &shape,
            viewport.as_deref(),
            center.as_deref(),
            &commands,
            json,
        ),
        Command::Present {
            input,
            commands,
            screen,
            fit,
            duration,
            transition,
            slides,
            json,
        } => present::run(
            &input,
            &commands,
            &screen,
            &fit,
            duration,
            &transition,
            slides.as_deref(),
            json,
        ),
        Command::Protection { command } => match command {
            ProtectionCommand::Apply {
                file,
                out,
                name,
                password,
                owner,
                handler,
                permissions,
                recipient,
                expiry,
                now,
                watermark,
                watermark_text,
                json,
            } => protection::apply(
                &file,
                &out,
                &name,
                &password,
                owner.as_deref(),
                &handler,
                &permissions,
                &recipient,
                expiry,
                now,
                watermark,
                &watermark_text,
                json,
            ),
        },
        Command::Prepress { command } => match command {
            PrepressCommand::Plan {
                input,
                preset,
                trap_width,
                black_width,
                black_color,
                join,
                sliding,
                dpi,
                balance,
                overprint,
                max_depth,
                json,
            } => prepress::plan(
                &input,
                &preset,
                trap_width,
                black_width,
                &black_color,
                &join,
                &sliding,
                dpi,
                &balance,
                &overprint,
                max_depth,
                json,
            ),
        },
        Command::Production { command } => match command {
            ProductionCommand::Plan { options } => production::plan(
                &options.input,
                &options.bleed,
                &options.marks,
                &options.separations,
                options.intent.as_deref(),
                options.json,
            ),
            ProductionCommand::Apply { options, out } => production::apply(
                &options.input,
                &out,
                &options.bleed,
                &options.marks,
                &options.separations,
                options.intent.as_deref(),
                options.json,
            ),
        },
        Command::Marks {
            input,
            output,
            crop_marks,
            registration,
            colour_bars,
            page_info,
        } => marks::apply_marks(
            &input,
            &output,
            marks::MarkSelection {
                crop_marks,
                registration,
                colour_bars,
                page_info,
            },
        ),
        Command::Repair {
            input,
            output,
            json,
        } => repair::run_repair(&input, &output, json),
        Command::Api { once } => api::run_api(once),
        Command::Ops { command } => match command {
            OpsCommand::List { json } => ops::run_list(json),
            OpsCommand::Resolve {
                name,
                assignments,
                json,
            } => ops::run_resolve(&name, &assignments, json),
        },
        Command::Watch {
            folder,
            out,
            extensions,
            suffix,
            markers,
            poll_every,
            base_ticks,
            max_ticks,
            multiplier,
            ticks,
            json,
        } => watch::run_watch(
            &folder,
            &out,
            &extensions,
            &suffix,
            markers.as_deref(),
            poll_every,
            base_ticks,
            max_ticks,
            multiplier,
            ticks,
            json,
        ),
        Command::Plugin { command } => match command {
            PluginCommand::Plan {
                manifest,
                operation,
                grants,
                expect,
                max_steps,
                max_memory_bytes,
                max_wall_clock_ms,
                max_text_bytes,
                max_bytes,
                max_total_bytes,
                json,
            } => plugin::run_plan(
                &manifest,
                &operation,
                &grants,
                &expect,
                max_steps,
                max_memory_bytes,
                max_wall_clock_ms,
                max_text_bytes,
                max_bytes,
                max_total_bytes,
                json,
            ),
            PluginCommand::Output {
                r#type,
                plugin_id,
                operation,
                text,
                text_len,
                int,
                bool,
                bytes_len,
                expect,
                max_text_bytes,
                max_bytes,
                max_total_bytes,
                json,
            } => {
                let expected = expect.unwrap_or_else(|| r#type.clone());
                plugin::run_output(
                    &plugin_id,
                    &operation,
                    &r#type,
                    text.as_deref(),
                    text_len,
                    int,
                    bool,
                    bytes_len,
                    &expected,
                    max_text_bytes,
                    max_bytes,
                    max_total_bytes,
                    json,
                )
            }
        },
        Command::Toolset { command } => match command {
            ToolsetCommand::List { file, json } => toolset::run_list(&file, json),
            ToolsetCommand::Validate { file, json } => toolset::run_validate(&file, json),
            ToolsetCommand::Show { file, alias, json } => toolset::run_show(&file, &alias, json),
            ToolsetCommand::Run {
                file,
                alias,
                inputs,
                output_dir,
                json,
            } => toolset::run_tool(&file, &alias, &inputs, output_dir.as_deref(), json),
        },
    }
}

/// Load a document and print its structural summary.
fn print_info(file: &PathBuf, json: bool) -> anyhow::Result<()> {
    let doc = load(file)?;
    let info = InfoOutput {
        file: file.display().to_string(),
        pdf_version: doc.version().to_owned(),
        bytes: doc.len(),
        pages: doc.page_count()?,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&info)?);
    } else {
        println!("file: {}", info.file);
        println!("pdf_version: {}", info.pdf_version);
        println!("bytes: {}", info.bytes);
        println!("pages: {}", info.pages);
    }
    Ok(())
}

/// One version-dependent feature gate in a [`CompatOutput`].
#[derive(Debug, Serialize)]
struct CompatFeature {
    /// Human-readable feature label, stable across releases.
    feature: &'static str,
    /// First PDF version that provides the feature.
    min_version: String,
    /// Whether the document's effective version provides it.
    supported: bool,
}

/// Machine-readable result of `pdfxi compat --json`.
#[derive(Debug, Serialize)]
struct CompatOutput {
    /// File that was inspected.
    file: String,
    /// Version declared by the `%PDF-` header.
    header_version: String,
    /// Version that applies after a catalog `/Version` override.
    effective_version: String,
    /// Where the effective version came from: `header` or `catalog`.
    source: &'static str,
    /// Byte offset of the `%PDF-` marker, when one was found.
    header_offset: Option<usize>,
    /// Whether the effective version is inside the supported 1.0-2.0 range.
    supported: bool,
    /// Non-fatal header or version irregularities, in a fixed order.
    warnings: Vec<String>,
    /// One entry per [`pdfxi_core::compatibility::PdfFeature::ALL`] gate.
    features: Vec<CompatFeature>,
    /// Constructs whose tokens are present in the file, in gate order.
    detected: Vec<&'static str>,
}

/// Load a document and print its version-compatibility report.
fn print_compatibility(file: &PathBuf, json: bool, strict: bool) -> anyhow::Result<()> {
    use pdfxi_core::compatibility::{VersionSource, detect_features};

    let doc = load(file)?;
    let report = doc.compatibility_report();
    let gates = report.feature_gate_report();
    let detected = detect_features(doc.bytes());
    let source = match report.source() {
        VersionSource::Header => "header",
        VersionSource::Catalog => "catalog",
    };
    let features = gates
        .entries()
        .iter()
        .map(|entry| CompatFeature {
            feature: entry.feature().label(),
            min_version: entry.feature().min_version().to_string(),
            supported: entry.support().is_supported(),
        })
        .collect::<Vec<_>>();
    let detected_labels = detected
        .iter()
        .map(|feature| feature.label())
        .collect::<Vec<_>>();
    let output = CompatOutput {
        file: file.display().to_string(),
        header_version: report.header_version().to_string(),
        effective_version: report.effective_version().to_string(),
        source,
        header_offset: report.header_offset(),
        supported: report.is_supported(),
        warnings: report.warnings().iter().map(ToString::to_string).collect(),
        features,
        detected: detected_labels,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("file: {}", output.file);
        println!("header_version: {}", output.header_version);
        println!("effective_version: {}", output.effective_version);
        println!("source: {}", output.source);
        match output.header_offset {
            Some(offset) => println!("header_offset: {offset}"),
            None => println!("header_offset: missing"),
        }
        println!("supported: {}", if output.supported { "yes" } else { "no" });
        println!("gates: {}", gates.summary());
        println!("features:");
        for feature in &output.features {
            if feature.supported {
                println!("  {}: supported", feature.feature);
            } else {
                println!(
                    "  {}: not supported (needs {})",
                    feature.feature, feature.min_version
                );
            }
        }
        if output.detected.is_empty() {
            println!("detected: none");
        } else {
            println!("detected: {}", output.detected.join(", "));
        }
        if output.warnings.is_empty() {
            println!("warnings: none");
        } else {
            println!("warnings:");
            for warning in &output.warnings {
                println!("  - {warning}");
            }
        }
    }

    if strict {
        let blocked = detected
            .iter()
            .filter(|feature| !report.effective_version().supports(**feature))
            .map(|feature| feature.label())
            .collect::<Vec<_>>();
        if !blocked.is_empty() {
            eprintln!(
                "strict: effective version {} does not allow: {}",
                report.effective_version(),
                blocked.join(", ")
            );
            std::process::exit(3);
        }
    }
    Ok(())
}

/// Load a document and print the text of every page in order.
fn extract_text(file: &PathBuf) -> anyhow::Result<()> {
    let doc = load(file)?;
    for index in 0..doc.page_count()? {
        println!("{}", doc.page_text(index)?);
    }
    Ok(())
}

/// Build a text index from every page and print all matches of `query`.
///
/// Each page's extracted text is placed in a single [`TextSpan`], so the
/// character offsets reported by the index are plain indices into that page's
/// concatenated text. Matches are emitted in page-then-offset order because
/// [`SearchIndex`] sorts its entries by page and `find` orders matches within a
/// page by ascending offset. A query with no matches is not an error: the
/// command exits successfully, prints no lines to stdout, and writes a
/// `no matches` note to stderr in both text and JSON modes.
fn search(
    file: &PathBuf,
    query: &str,
    case_sensitive: bool,
    whole_word: bool,
    json: bool,
) -> anyhow::Result<()> {
    let doc = load(file)?;
    let page_count = doc.page_count()?;
    let mut pages = Vec::with_capacity(page_count);
    for index in 0..page_count {
        let text = doc.page_text(index).with_context(|| {
            format!(
                "failed to extract text from page {} of '{}'",
                index + 1,
                file.display()
            )
        })?;
        pages.push((
            index,
            TextPage::new(index, vec![TextSpan::new(text, 0.0, 0.0)]),
        ));
    }

    let index = SearchIndex::new(pages);
    let options = SearchOptions::with_options(case_sensitive, whole_word);
    let found = index.find(query, &options);

    let matches: Vec<MatchOutput> = found
        .iter()
        .flat_map(|page_matches| {
            page_matches.matches().iter().map(|m| MatchOutput {
                page: m.page() + 1,
                offset: m.start(),
                text: m.text().to_owned(),
            })
        })
        .collect();

    if matches.is_empty() {
        eprintln!("no matches");
        return Ok(());
    }

    if json {
        let output = SearchOutput {
            file: file.display().to_string(),
            query: query.to_owned(),
            matches,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        for m in &matches {
            println!("page {} offset {} {}", m.page, m.offset, m.text);
        }
    }
    Ok(())
}

/// Rasterize one page and write the encoded image to `out`.
///
/// This variant is compiled when the `pdfium` feature is off. It reports the
/// missing capability as an actionable error so scripts get a non-zero exit
/// status with a clear explanation rather than a link or build failure.
#[cfg(not(feature = "pdfium"))]
fn render(
    _file: &Path,
    _page: usize,
    _scale: f32,
    _out: &Path,
    _format: OutputFormat,
) -> anyhow::Result<()> {
    bail!(
        "pdfxi was built without pdfium support: rebuild with `--features pdfium` to use the 'render' command"
    )
}

/// Rasterize one page and write the encoded image to `out`.
///
/// Inputs are validated before the renderer is invoked: `scale` must be finite
/// and within `(0, MAX_RENDER_SCALE]`, and `page` must be smaller than the
/// document's page count. Renderer, encoder, and file-write failures all carry
/// context identifying the page and path involved.
#[cfg(feature = "pdfium")]
fn render(
    file: &Path,
    page: usize,
    scale: f32,
    out: &Path,
    format: OutputFormat,
) -> anyhow::Result<()> {
    if !scale.is_finite() || scale <= 0.0 || scale > MAX_RENDER_SCALE {
        bail!("invalid scale {scale}: expected a finite value in (0, {MAX_RENDER_SCALE}]");
    }

    let renderer = PdfiumRenderer::from_file(file)
        .with_context(|| format!("failed to open '{}' for rendering", file.display()))?;
    let page_count = renderer
        .page_count()
        .with_context(|| format!("failed to read the page count of '{}'", file.display()))?;
    if page >= page_count {
        bail!(
            "page {page} is out of range: '{}' has {page_count} page(s); valid pages are 0..{}",
            file.display(),
            page_count
        );
    }

    let rendered = renderer
        .render_page(
            PageIndex::new(page),
            scale,
            &CancellationToken::new(),
            &mut |_| {},
        )
        .with_context(|| {
            format!(
                "failed to render page {page} of '{}' at scale {scale}",
                file.display()
            )
        })?;

    let bytes = match format {
        OutputFormat::Png => {
            image::encode_png(rendered.width(), rendered.height(), rendered.pixels())
        }
        OutputFormat::Ppm => {
            image::encode_ppm(rendered.width(), rendered.height(), rendered.pixels())
        }
    }
    .with_context(|| format!("failed to encode page {page} as {}", format.label()))?;

    std::fs::write(out, bytes).with_context(|| format!("failed to write '{}'", out.display()))?;
    println!(
        "wrote {} (page {page}, scale {scale}, {})",
        out.display(),
        format.label()
    );
    Ok(())
}

/// Read and parse a PDF from `file`, adding path context to I/O failures.
fn load(file: &PathBuf) -> anyhow::Result<Document> {
    let bytes =
        std::fs::read(file).with_context(|| format!("failed to read '{}'", file.display()))?;
    Ok(Document::from_bytes(bytes)?)
}
