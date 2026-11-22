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
mod api;

// PDF creation (`create`) and accessible-text export (`export`) built on
// pdfxi-convert and the accessibility tree builder.
mod create;
mod export;

// Byte-level subset preflight (`preflight`) and printer-mark placement
// (`marks`) added for T-0131. Both use the core application API.
mod marks;
mod preflight;
// Virtual PDF printer control: install/status/remove the "PDFXI PDF" queue and
// capture one finished job (`printer`).
mod printer;
// Page-level editing: extract, delete, move, reorder, rotate, insert, replace,
// merge and split (`pages`).
mod pages;
// Link annotations and embedded-file attachments (`links`, `attachments`).
mod links;
/// `pdfxi portfolio`: list and extract a PDF portfolio collection.
mod portfolio;
// Optional-content layers (`layers`).
mod layers;
mod repair;

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use clap::{Parser, Subcommand, ValueEnum};
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
}

/// Operations on the `pdfxi action` command group.
#[derive(Debug, Subcommand)]
enum ActionCommand {
    /// List every action in a file, in file order.
    List {
        /// Path to the action file.
        file: PathBuf,
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
        Command::Portfolio {
            file,
            json,
            extract,
        } => portfolio::run_portfolio(&file, json, extract.as_deref()),
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
        },
        Command::Export { command } => match command {
            ExportCommand::Tagged {
                input,
                output,
                format,
            } => export::export_tagged(&input, &output, format.format()),
        },
        Command::Action { command } => match command {
            ActionCommand::List { file } => action::list_actions(&file),
            ActionCommand::Show { file, name } => action::show_action(&file, &name),
            ActionCommand::Run {
                file,
                name,
                input,
                output_dir,
                json,
            } => action::run_action(&file, &name, &input, output_dir.as_deref(), json),
        },
        Command::Droplet {
            action_file,
            name,
            inputs,
            json,
        } => action::run_action(&action_file, &name, &inputs, None, json),
        Command::Preflight {
            profile,
            input,
            fix,
            output,
            json,
        } => preflight::run_preflight(&profile, &input, fix, output.as_deref(), json),
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
