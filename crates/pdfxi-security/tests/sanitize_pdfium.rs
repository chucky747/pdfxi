//! T-0257 independent-reader check: PDFium must reopen the sanitized bytes,
//! extract the surviving page text, and report no document title or author.
//!
//! Compiled only with `--features pdfium`. PDFium is discovered through the
//! same locations the render backend documents: the executable directory,
//! `PDFIUM_PATH` (debug builds), then `%LOCALAPPDATA%\pdfxi\pdfium\bin`.

#![cfg(feature = "pdfium")]

use std::path::{Path, PathBuf};

use pdfium_render::prelude::{PdfDocumentMetadataTagType, Pdfium, PdfiumError};
use pdfxi_core::Document;
use pdfxi_security::{SanitizeRequest, sanitize_document_full, verify_sanitized};

const SECRET_TITLE: &str = "PDFXI T-0257 SECRET TITLE";
const SECRET_AUTHOR: &str = "PDFXI T-0257 SECRET AUTHOR";

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf()
}

fn load_pdfium() -> Pdfium {
    let library_name = Pdfium::pdfium_platform_library_name();
    let mut candidates = Vec::new();
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        candidates.push(directory.join(&library_name));
    }
    if cfg!(debug_assertions) {
        if let Some(path) = std::env::var_os("PDFIUM_PATH") {
            candidates.push(PathBuf::from(path));
        }
        if let Some(base) = std::env::var_os("LOCALAPPDATA") {
            candidates.push(
                PathBuf::from(base)
                    .join("pdfxi")
                    .join("pdfium")
                    .join("bin")
                    .join(&library_name),
            );
        }
    }
    for candidate in candidates {
        match Pdfium::bind_to_library(&candidate) {
            Ok(bindings) => return Pdfium::new(bindings),
            Err(PdfiumError::PdfiumLibraryBindingsAlreadyInitialized) => {
                return Pdfium::default();
            }
            Err(_) => {}
        }
    }
    panic!(
        "PDFium library not found; set PDFIUM_PATH or install it in %LOCALAPPDATA%\\pdfxi\\pdfium\\bin"
    );
}

fn excel_fixture() -> Document {
    let path = workspace_root()
        .join("fixtures")
        .join("realworld")
        .join("microsoft-excel-2019-sample.pdf");
    let bytes = std::fs::read(&path).expect("read Excel fixture");
    Document::from_bytes(bytes).expect("fixture parses")
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect()
}

#[test]
fn pdfium_reopens_sanitized_bytes_and_sees_no_identifying_metadata() {
    let mut source = excel_fixture();
    let source_pages = source.page_count().expect("source page count");
    let source_text = source.page_text(0).expect("source text");
    let source_normalized = normalize(&source_text);
    assert!(
        !source_normalized.is_empty(),
        "the source page text must not be empty"
    );

    let mut info = source.info().expect("read info").unwrap_or_default();
    info.title = Some(SECRET_TITLE.to_owned());
    info.author = Some(SECRET_AUTHOR.to_owned());
    source.set_info(&info).expect("stage identifying info");

    let sanitized =
        sanitize_document_full(&source, &SanitizeRequest::all()).expect("sanitize fixture");
    let verification = verify_sanitized(sanitized.bytes(), &[SECRET_TITLE, SECRET_AUTHOR])
        .expect("independent verifier");
    assert!(
        verification.clean,
        "verification survivors: {:?}",
        verification.survivors
    );

    let pdfium = load_pdfium();
    let document = pdfium
        .load_pdf_from_byte_slice(sanitized.bytes(), None)
        .expect("PDFium must reopen the sanitized bytes");
    assert_eq!(
        document.pages().len() as usize,
        source_pages,
        "PDFium page count must match the source"
    );
    let page_text = document
        .pages()
        .get(0)
        .expect("PDFium page 0")
        .text()
        .expect("PDFium page text")
        .all();
    let pdfium_normalized = normalize(&page_text);
    assert!(
        pdfium_normalized.contains(&source_normalized)
            || source_normalized.contains(&pdfium_normalized),
        "PDFium text must match the surviving source text: pdfium={pdfium_normalized:?} source={source_normalized:?}"
    );

    let metadata = document.metadata();
    let title = metadata
        .get(PdfDocumentMetadataTagType::Title)
        .map(|tag| tag.value().trim().to_owned());
    let author = metadata
        .get(PdfDocumentMetadataTagType::Author)
        .map(|tag| tag.value().trim().to_owned());
    assert!(
        title.as_deref().is_none_or(str::is_empty),
        "PDFium still sees a title: {title:?}"
    );
    assert!(
        author.as_deref().is_none_or(str::is_empty),
        "PDFium still sees an author: {author:?}"
    );
}
