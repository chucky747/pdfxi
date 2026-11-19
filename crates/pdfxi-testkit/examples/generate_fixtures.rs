//! Regenerate the committed PDF fixtures from the `pdfxi-testkit` builders.
//!
//! Run from anywhere with:
//!
//! ```text
//! cargo run -p pdfxi-testkit --example generate_fixtures
//! ```
//!
//! The example resolves the workspace root from `CARGO_MANIFEST_DIR`, writes
//! each fixture in [`pdfxi_testkit::fixtures::COMMITTED_FIXTURES`] into
//! `fixtures/`, and prints its byte length and SHA-256 digest so the values can
//! be pasted into `fixtures/manifest.toml`.

use std::env;
use std::fs;
use std::path::PathBuf;

use pdfxi_testkit::fixtures;
use pdfxi_testkit::hash::sha256_hex;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(|crates| crates.parent())
        .ok_or("could not resolve workspace root from CARGO_MANIFEST_DIR")?;
    let fixtures_dir = workspace_root.join("fixtures");
    fs::create_dir_all(&fixtures_dir)?;

    for fixture in fixtures::COMMITTED_FIXTURES {
        let bytes = fixtures::build(fixture.file_name)
            .ok_or_else(|| format!("no builder for {}", fixture.file_name))?;
        let path = fixtures_dir.join(fixture.file_name);
        fs::write(&path, &bytes)?;
        println!(
            "wrote {} | builder={} | bytes={} | sha256={}",
            path.display(),
            fixture.builder,
            bytes.len(),
            sha256_hex(&bytes)
        );
    }

    Ok(())
}
