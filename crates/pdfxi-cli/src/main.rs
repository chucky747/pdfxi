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

#![deny(missing_docs)]
#![forbid(unsafe_code)]

// The encoders are only reachable from the PDFium-backed `render` path; gating
// the module keeps default builds free of unused-code warnings.
#[cfg(feature = "pdfium")]
mod image;

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
#[command(name = "pdfxi", version, about = "PDFXI — a Rust PDF workbench")]
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
