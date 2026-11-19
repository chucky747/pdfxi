//! Regenerate the committed malformed-PDF corpus and its manifest.
//!
//! Run from the workspace root:
//!
//! ```text
//! cargo run -p pdfxi-testkit --example generate_malformed
//! ```
//!
//! The builders in `pdfxi_testkit::malformed` are the single source of truth, so
//! regenerating is deterministic and byte-identical to the committed files.

use std::env;
use std::fs;
use std::path::PathBuf;

use pdfxi_testkit::malformed::{self, MALFORMED_FILE_MAX_BYTES};

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .expect("workspace root")
        .to_path_buf();
    let dir = root.join("fixtures").join("malformed");
    fs::create_dir_all(&dir).expect("create fixtures/malformed");

    let fixtures = malformed::all();
    for fixture in &fixtures {
        let bytes = (fixture.build)();
        assert!(
            bytes.len() < MALFORMED_FILE_MAX_BYTES,
            "{} exceeds the per-file budget",
            fixture.name
        );
        let path = dir.join(fixture.name);
        fs::write(&path, &bytes).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        println!("wrote {} ({} bytes)", path.display(), bytes.len());
    }

    let manifest = malformed::emit_manifest(&fixtures);
    let manifest_path = dir.join("manifest.toml");
    fs::write(&manifest_path, manifest)
        .unwrap_or_else(|e| panic!("write {}: {e}", manifest_path.display()));
    println!("wrote {}", manifest_path.display());
}
