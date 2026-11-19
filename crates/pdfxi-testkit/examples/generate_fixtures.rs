//! Regenerate the committed PDF fixtures and their manifest from the
//! `pdfxi-testkit` builders.
//!
//! Run from anywhere with:
//!
//! ```text
//! cargo run -p pdfxi-testkit --example generate_fixtures
//! ```
//!
//! The example resolves the workspace root from `CARGO_MANIFEST_DIR`, writes
//! every fixture in [`pdfxi_testkit::fixtures::all`] into `fixtures/`, and
//! regenerates `fixtures/manifest.toml` from the same descriptors. Because the
//! SHA-256 digests are computed from the bytes actually written, the manifest
//! can never drift from the files. Regeneration is byte-stable.

use std::env;
use std::fs;
use std::path::PathBuf;

use pdfxi_testkit::fixtures;
use pdfxi_testkit::hash::sha256_hex;

const MANIFEST_HEADER: &str = "\
# Committed PDF fixture manifest (T-0016, expanded to 50 fixtures in T-0024).
#
# These are small, deterministic, synthetic PDFs generated from pdfxi-testkit
# builders. Regenerate with:
#   cargo run -p pdfxi-testkit --example generate_fixtures
# The generator writes this file from the same descriptors, so hashes, page
# counts, builders, and text markers always agree with the files on disk.
# The integration test `tests/committed_fixtures.rs` verifies every value.

version = 1
";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(|crates| crates.parent())
        .ok_or("could not resolve workspace root from CARGO_MANIFEST_DIR")?;
    let fixtures_dir = workspace_root.join("fixtures");
    fs::create_dir_all(&fixtures_dir)?;

    let all = fixtures::all();
    let mut manifest = String::from(MANIFEST_HEADER);
    manifest.push('\n');

    for fixture in &all {
        let bytes = fixture.build();
        let path = fixtures_dir.join(&fixture.file_name);
        fs::write(&path, &bytes)?;

        manifest.push_str("[[fixture]]\n");
        manifest.push_str(&format!(
            "name = {}\n",
            fixtures::toml_quote(&fixture.file_name)
        ));
        manifest.push_str(&format!(
            "builder = {}\n",
            fixtures::toml_quote(&fixture.builder)
        ));
        manifest.push_str(&format!(
            "sha256 = {}\n",
            fixtures::toml_quote(&sha256_hex(&bytes))
        ));
        manifest.push_str(&format!("pages = {}\n", fixture.pages));
        let text = fixture.text_markers.join("|");
        manifest.push_str(&format!("text = {}\n\n", fixtures::toml_quote(&text)));

        println!(
            "wrote {} | pages={} | builder={} | bytes={} | sha256={}",
            path.display(),
            fixture.pages,
            fixture.builder,
            bytes.len(),
            sha256_hex(&bytes)
        );
    }

    let manifest_path = fixtures_dir.join("manifest.toml");
    fs::write(&manifest_path, &manifest)?;
    println!(
        "wrote {} | fixtures={} | manifest_bytes={}",
        manifest_path.display(),
        all.len(),
        manifest.len()
    );

    Ok(())
}
