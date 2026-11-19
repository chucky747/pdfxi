//! Regenerate the committed M2 authoring fixtures and their manifest.
//!
//! Run from anywhere with:
//!
//! ```text
//! cargo run -p pdfxi-testkit --example generate_authoring_fixtures
//! ```
//!
//! The example resolves the workspace root from `CARGO_MANIFEST_DIR`, writes
//! every fixture from [`pdfxi_testkit::authoring::all`] into
//! `fixtures/authoring/`, and regenerates `fixtures/authoring-manifest.toml`
//! from the same descriptors. Sizes and SHA-256 digests are computed from the
//! bytes actually written, so the manifest can never drift from the files.
//! Regeneration is byte-stable and needs no network access.

use std::error::Error;
use std::fs;

use pdfxi_testkit::authoring;
use pdfxi_testkit::hash::sha256_hex;

fn main() -> Result<(), Box<dyn Error>> {
    let fixtures_dir =
        authoring::corpus_dir().ok_or("could not resolve the fixtures/authoring directory")?;
    let manifest_path =
        authoring::manifest_path().ok_or("could not resolve the authoring manifest path")?;
    fs::create_dir_all(&fixtures_dir)?;

    let fixtures = authoring::all();
    for fixture in &fixtures {
        let bytes = fixture.build();
        let path = fixtures_dir.join(fixture.file_name);
        fs::write(&path, &bytes)?;
        println!(
            "wrote {} | feature={} | pages={} | bytes={} | sha256={}",
            path.display(),
            fixture.feature.as_str(),
            fixture.pages,
            bytes.len(),
            sha256_hex(&bytes)
        );
    }

    let manifest = authoring::emit_manifest(&fixtures);
    fs::write(&manifest_path, &manifest)?;
    println!(
        "wrote {} | fixtures={} | manifest_bytes={}",
        manifest_path.display(),
        fixtures.len(),
        manifest.len()
    );
    Ok(())
}
