//! `pdfxi linearize` and `pdfxi pack`: choose the on-disk object layout.
//!
//! `linearize` writes a complete linearized file through
//! [`Document::save_linearized`] and then re-reads the `/Linearized` dictionary,
//! checking the declared length and page count against the bytes it just wrote.
//! `pack` writes object streams plus an xref stream through
//! [`Document::save_with_object_streams`], which keeps stream objects standalone
//! and packs the rest. Both hosts save a standalone file, reopen it, and compare
//! every page's text with the source before reporting success.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_core::{Document, LinearizationOptions, ObjectStreamOptions};
use serde::Serialize;

/// Largest value `/ObjStm` permits for the member count of one stream.
const MAX_OBJECTS_PER_STREAM: usize = 65_535;

fn load(file: &Path) -> anyhow::Result<Document> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))
}

/// Compare every page's text between two documents.
fn assert_page_text_matches(source: &Document, saved: &Document) -> anyhow::Result<usize> {
    let pages = source
        .pages()
        .map_err(|error| anyhow::anyhow!("count source pages: {error}"))?
        .len();
    let saved_pages = saved
        .pages()
        .map_err(|error| anyhow::anyhow!("count saved pages: {error}"))?
        .len();
    if pages != saved_pages {
        bail!("page count changed from {pages} to {saved_pages}");
    }
    for index in 0..pages {
        let before = source
            .page_text(index)
            .map_err(|error| anyhow::anyhow!("read source page {} text: {error}", index + 1))?;
        let after = saved
            .page_text(index)
            .map_err(|error| anyhow::anyhow!("read saved page {} text: {error}", index + 1))?;
        if before != after {
            bail!("page {} text changed in the saved file", index + 1);
        }
    }
    Ok(pages)
}

/// JSON shape of `pdfxi linearize`.
#[derive(Serialize)]
struct LinearizeReport {
    /// Input file.
    file: String,
    /// Written file.
    out: String,
    /// Output size in bytes.
    bytes: u64,
    /// Declared `/L`.
    declared_length: u64,
    /// Declared `/N`.
    page_count: Option<u32>,
    /// First page object number (`/O`).
    first_page_object: u32,
    /// End of the first-page section (`/E`).
    first_page_end: Option<u64>,
    /// Start of the main xref (`/T`).
    main_xref_offset: Option<u64>,
    /// Offset of the hint stream.
    hint_offset: u64,
    /// Length of the hint stream.
    hint_length: u64,
    /// Pages whose text was compared.
    text_pages_verified: usize,
}

/// `pdfxi linearize`.
pub(crate) fn run_linearize(
    file: &Path,
    out: &Path,
    version: &str,
    json: bool,
) -> anyhow::Result<()> {
    if version.is_empty() || !version.chars().all(|ch| ch.is_ascii_digit() || ch == '.') {
        bail!("--version must contain only ASCII digits and dots, for example 1.7");
    }
    let document = load(file)?;
    let options = LinearizationOptions {
        version: version.to_owned(),
    };
    let bytes = document
        .save_linearized(&options)
        .map_err(|error| anyhow::anyhow!("linearize {}: {error}", file.display()))?;
    let reopened = Document::from_bytes(bytes.clone())
        .map_err(|error| anyhow::anyhow!("reopen linearized output: {error}"))?;
    let linearization = reopened
        .linearization()
        .map_err(|error| anyhow::anyhow!("read linearization hint: {error}"))?
        .context("the linearized output has no /Linearized dictionary")?;
    if linearization.file_length != bytes.len() as u64 {
        bail!(
            "declared /L {} does not match the {} written bytes",
            linearization.file_length,
            bytes.len()
        );
    }
    let pages = assert_page_text_matches(&document, &reopened)?;
    if let Some(declared) = linearization.page_count
        && declared as usize != pages
    {
        bail!("declared /N {declared} does not match the {pages} pages");
    }
    let hint_end = linearization
        .hint
        .end()
        .context("hint stream range overflows")?;
    if hint_end > bytes.len() as u64 {
        bail!(
            "hint stream ends at {hint_end}, past the {} written bytes",
            bytes.len()
        );
    }
    std::fs::write(out, &bytes).with_context(|| format!("write {}", out.display()))?;

    let report = LinearizeReport {
        file: file.display().to_string(),
        out: out.display().to_string(),
        bytes: bytes.len() as u64,
        declared_length: linearization.file_length,
        page_count: linearization.page_count,
        first_page_object: linearization.first_page_object,
        first_page_end: linearization.first_page_end,
        main_xref_offset: linearization.main_xref_offset,
        hint_offset: linearization.hint.offset,
        hint_length: linearization.hint.length,
        text_pages_verified: pages,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("file: {}", report.file);
    println!("out:  {}", report.out);
    println!("bytes: {}", report.bytes);
    println!("declared_length: {}", report.declared_length);
    println!(
        "pages: {} (text verified: {})",
        report.page_count.unwrap_or_default(),
        report.text_pages_verified
    );
    println!("first_page_object: {}", report.first_page_object);
    if let Some(end) = report.first_page_end {
        println!("first_page_end: {end}");
    }
    if let Some(offset) = report.main_xref_offset {
        println!("main_xref_offset: {offset}");
    }
    println!(
        "hint: offset {} length {}",
        report.hint_offset, report.hint_length
    );
    Ok(())
}

/// JSON shape of `pdfxi pack`.
#[derive(Serialize)]
struct PackReport {
    /// Input file.
    file: String,
    /// Written file.
    out: String,
    /// Input size in bytes.
    bytes_before: usize,
    /// Output size in bytes.
    bytes_after: usize,
    /// Whether the output contains an `/ObjStm`.
    object_streams: bool,
    /// Whether the output contains an `/XRef` stream.
    xref_stream: bool,
    /// Maximum objects per object stream.
    max_objects_per_stream: usize,
    /// Whether generated streams carry `/FlateDecode`.
    compressed: bool,
    /// Pages whose text was compared.
    text_pages_verified: usize,
}

/// `pdfxi pack`.
pub(crate) fn run_pack(
    file: &Path,
    out: &Path,
    compress: bool,
    max_objects: usize,
    json: bool,
) -> anyhow::Result<()> {
    if max_objects == 0 || max_objects > MAX_OBJECTS_PER_STREAM {
        bail!("--max-objects must be between 1 and {MAX_OBJECTS_PER_STREAM}");
    }
    let document = load(file)?;
    let before = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let options = ObjectStreamOptions {
        compress,
        max_objects_per_stream: max_objects,
        ..ObjectStreamOptions::default()
    };
    let bytes = document
        .save_with_object_streams(&options)
        .map_err(|error| anyhow::anyhow!("pack {}: {error}", file.display()))?;
    let reopened = Document::from_bytes(bytes.clone())
        .map_err(|error| anyhow::anyhow!("reopen packed output: {error}"))?;
    let pages = assert_page_text_matches(&document, &reopened)?;
    let object_streams = bytes.windows(7).any(|window| window == b"/ObjStm");
    let xref_stream = bytes.windows(5).any(|window| window == b"/XRef");
    if !object_streams {
        bail!("the packed output contains no object stream");
    }
    if !xref_stream {
        bail!("the packed output contains no xref stream");
    }
    std::fs::write(out, &bytes).with_context(|| format!("write {}", out.display()))?;

    let report = PackReport {
        file: file.display().to_string(),
        out: out.display().to_string(),
        bytes_before: before.len(),
        bytes_after: bytes.len(),
        object_streams,
        xref_stream,
        max_objects_per_stream: max_objects,
        compressed: compress,
        text_pages_verified: pages,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("file: {}", report.file);
    println!("out:  {}", report.out);
    println!("bytes: {} -> {}", report.bytes_before, report.bytes_after);
    println!("object_streams: {}", report.object_streams);
    println!("xref_stream: {}", report.xref_stream);
    println!("max_objects_per_stream: {}", report.max_objects_per_stream);
    println!("compressed: {}", report.compressed);
    println!("text_pages_verified: {}", report.text_pages_verified);
    Ok(())
}
