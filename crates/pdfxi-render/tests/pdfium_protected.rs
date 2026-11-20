//! Independent-reader check for T-0256: PDFium, not `pdfxi-security`, must
//! open the protected output with its password and extract page text.
//!
//! Compiled only with `--features pdfium`. PDFium is discovered through the
//! same locations the backend uses: the executable directory, `PDFIUM_PATH`
//! (debug builds), then `%LOCALAPPDATA%\pdfxi\pdfium\bin`.

#![cfg(feature = "pdfium")]

use std::path::{Path, PathBuf};

use pdfium_render::prelude::{PdfDocumentMetadataTagType, Pdfium, PdfiumError};
use pdfxi_core::Document;
use pdfxi_security::{ProtectOptions, ProtectPermissions, protect_document};

const FIXTURE: &str = "microsoft-excel-2019-sample.pdf";
const PASSWORD: &str = "pdfium-t0256-password";
const TITLE: &str = "PDFXI T-0256 PDFium title";
const NON_ASCII_PASSWORD: &str = "pässwörd-测试";

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf()
}

/// Bind PDFium from the same candidate list the render backend documents.
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
    let fixture = workspace_root()
        .join("fixtures")
        .join("realworld")
        .join(FIXTURE);
    let bytes = std::fs::read(&fixture).expect("read Excel fixture");
    Document::from_bytes(bytes).expect("fixture parses")
}

fn assert_text_and_pages(
    pdfium: &Pdfium,
    bytes: &[u8],
    password: Option<&str>,
    pages: usize,
) -> String {
    let document = pdfium
        .load_pdf_from_byte_slice(bytes, password)
        .expect("PDFium must open the protected file with its password");
    assert_eq!(
        document.pages().len() as usize,
        pages,
        "PDFium page count must match the source"
    );
    let page = document.pages().get(0).expect("PDFium page 0");
    let text = page.text().expect("PDFium page text").all();
    assert!(
        text.chars().any(char::is_alphanumeric),
        "PDFium extracted no text from the decrypted page"
    );
    text
}

#[test]
fn pdfium_opens_with_password_and_fails_without_it() {
    let mut source = excel_fixture();
    let mut info = source.info().expect("read info").unwrap_or_default();
    info.title = Some(TITLE.to_owned());
    source.set_info(&info).expect("stage title");
    let source_pages = source.page_count().expect("page count");
    let source_text = source.page_text(0).expect("source page text");

    let protected = protect_document(
        &source,
        ProtectOptions::new(PASSWORD)
            .with_owner_password(b"pdfium-t0256-owner")
            .with_permissions(ProtectPermissions::READ_ONLY),
    )
    .expect("protect the Excel fixture");

    let pdfium = load_pdfium();

    // With the password: PDFium decrypts, reads the page tree, extracts text,
    // and sees the /Info title the writer carried into the protected trailer.
    let text = assert_text_and_pages(&pdfium, &protected, Some(PASSWORD), source_pages);
    let normalize = |value: &str| {
        value
            .chars()
            .filter(|character| character.is_alphanumeric())
            .collect::<String>()
    };
    let source_normalized = normalize(&source_text);
    let pdfium_normalized = normalize(&text);
    assert!(
        !source_normalized.is_empty(),
        "the source page text must not be empty"
    );
    assert!(
        pdfium_normalized.contains(&source_normalized)
            || source_normalized.contains(&pdfium_normalized),
        "PDFium text must match the source text after punctuation/whitespace normalization: pdfium={pdfium_normalized:?} source={source_normalized:?}"
    );

    let document = pdfium
        .load_pdf_from_byte_slice(&protected, Some(PASSWORD))
        .expect("PDFium reopen for metadata");
    let pdfium_title = document
        .metadata()
        .get(PdfDocumentMetadataTagType::Title)
        .map(|tag| tag.value().to_owned());
    assert_eq!(
        pdfium_title.as_deref(),
        Some(TITLE),
        "PDFium must see the preserved /Info title"
    );
    drop(document);

    // Without the password: the independent reader must refuse the file.
    assert!(
        pdfium.load_pdf_from_byte_slice(&protected, None).is_err(),
        "PDFium opened an encrypted document without a password"
    );
    // A wrong password must also be refused.
    assert!(
        pdfium
            .load_pdf_from_byte_slice(&protected, Some("wrong-t0256-password"))
            .is_err(),
        "PDFium accepted a wrong password"
    );
}

#[test]
fn pdfium_opens_with_a_non_ascii_password() {
    let source = excel_fixture();
    let source_pages = source.page_count().expect("page count");
    let protected = protect_document(
        &source,
        ProtectOptions::new(NON_ASCII_PASSWORD.as_bytes())
            .with_permissions(ProtectPermissions::READ_ONLY),
    )
    .expect("protect with a non-ASCII password");

    let pdfium = load_pdfium();
    assert_text_and_pages(&pdfium, &protected, Some(NON_ASCII_PASSWORD), source_pages);
    assert!(
        pdfium.load_pdf_from_byte_slice(&protected, None).is_err(),
        "PDFium opened the non-ASCII-password file without a password"
    );
}
