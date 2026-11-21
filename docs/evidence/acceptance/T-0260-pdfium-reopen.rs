//! T-0260 independent PDFium reopen of the signed scenario output.
//!
//! Uses `pdfxi-render`'s PDFium backend (built with `--features pdfium`) to open
//! the saved signed file as an independent reader: page count from PDFium's own
//! document metadata and page 1 dimensions from PDFium's page object.

use std::fs;
use std::path::PathBuf;

use pdfxi_render::PageIndex;
use pdfxi_render::pdfium_backend::PdfiumRenderer;

fn main() {
    let path = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "target/tmp/t0260/t0260-signed.pdf".to_owned()),
    );
    let bytes = fs::read(&path).expect("read the signed PDF");
    println!(
        "pdfium_reopen path={} bytes={}",
        path.display(),
        bytes.len()
    );
    let renderer = PdfiumRenderer::from_bytes(bytes).expect("PDFium opens the signed bytes");
    let meta = renderer.document_meta().expect("PDFium document metadata");
    let size = renderer
        .page_size(PageIndex(0))
        .expect("PDFium page 1 size");
    println!(
        "pdfium pages={} title={:?} security_revision={} page0_size={:?}",
        meta.page_count, meta.title, meta.security_revision, size
    );
    if meta.page_count == 0 {
        println!("PDFIUM FAIL pages=0");
        std::process::exit(1);
    }
    println!("PDFIUM OK pages={}", meta.page_count);
}
