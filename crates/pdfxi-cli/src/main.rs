//! Command-line interface for PDFXI.
//!
//! The CLI is the headless surface for automation and CI. It shares the same
//! core and feature crates as the desktop application, so every operation that
//! can be scripted is also available to the UI.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use pdfxi_core::Document;
use serde::Serialize;

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

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Info { file, json } => print_info(&file, json),
        Command::Text { file } => extract_text(&file),
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

/// Read and parse a PDF from `file`, adding path context to I/O failures.
fn load(file: &PathBuf) -> anyhow::Result<Document> {
    let bytes =
        std::fs::read(file).with_context(|| format!("failed to read '{}'", file.display()))?;
    Ok(Document::from_bytes(bytes)?)
}
