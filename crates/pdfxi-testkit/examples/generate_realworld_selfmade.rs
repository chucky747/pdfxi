//! Regenerate the self-produced fixtures in `fixtures/realworld/`.
//!
//! Run with:
//!
//! ```text
//! cargo run -p pdfxi-testkit --example generate_realworld_selfmade
//! ```
//!
//! Two corpus slots have no installed external producer on the reference host:
//! an AcroForm PDF and a genuinely password-protected PDF. This example writes
//! both from the project's own code so the corpus stays reproducible and the
//! provenance is explicit.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use pdfxi_core::Document;
use pdfxi_security::encryption_writer::{EncryptOptions, encrypt_document};
use pdfxi_security::{EncryptionMethod, Permission};
use pdfxi_testkit::authoring;
use pdfxi_testkit::hash::sha256_hex;
/// Documented user (open) password of the self-produced encrypted fixture.
const USER_PASSWORD: &[u8] = b"pdfxi-test";
/// Documented owner password of the self-produced encrypted fixture.
const OWNER_PASSWORD: &[u8] = b"pdfxi-owner";

fn realworld_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root")
        .join("fixtures")
        .join("realworld")
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> Result<(), Box<dyn Error>> {
    let path = dir.join(name);
    fs::write(&path, bytes)?;
    println!(
        "wrote {} | bytes={} | sha256={}",
        path.display(),
        bytes.len(),
        sha256_hex(bytes)
    );
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let dir = realworld_dir();
    fs::create_dir_all(&dir)?;

    let forms = authoring::by_name("authoring-forms.pdf")
        .ok_or("authoring forms fixture is missing")?
        .build();
    write(&dir, "pdfxi-forms-selfmade.pdf", &forms)?;

    let source = pdfxi_testkit::text_pdf("Encrypted selfmade page marker");
    let document = Document::from_bytes(source)?;
    let options = EncryptOptions::new(
        EncryptionMethod::Aes128,
        USER_PASSWORD,
        OWNER_PASSWORD,
        Permission::ALL,
    )?
    .with_id_seed(b"pdfxi-realworld-encrypted".to_vec());
    let (encrypted, report) = encrypt_document(&document, &options)?;
    println!(
        "encrypted method={} revision={} bit_length={} objects={}",
        report.method.as_id(),
        report.revision,
        report.key_length_bits,
        report.encrypted_objects
    );
    write(&dir, "pdfxi-encrypted-selfmade.pdf", &encrypted)?;

    Ok(())
}
