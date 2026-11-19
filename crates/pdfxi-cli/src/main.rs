//! Command-line interface for PDFXI.
//!
//! The CLI is the headless surface for automation and CI. It shares the same
//! core and feature crates as the desktop application, so every operation that
//! can be scripted is also available to the UI.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use pdfxi_core::Document;

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
    },
    /// Extract plain text from a PDF file.
    ///
    /// Text extraction is implemented in milestone M1. This command currently
    /// validates the document and reports that extraction is pending.
    Text {
        /// Path to the PDF file.
        file: PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Info { file } => {
            let bytes = std::fs::read(&file)?;
            let doc = Document::from_bytes(bytes)?;
            println!("file: {}", file.display());
            println!("pdf_version: {}", doc.version());
            println!("bytes: {}", doc.len());
            println!("pages: {}", doc.page_count()?);
        }
        Command::Text { file } => {
            let bytes = std::fs::read(&file)?;
            let _doc = Document::from_bytes(bytes)?;
            anyhow::bail!("text extraction lands in milestone M1");
        }
    }
    Ok(())
}
