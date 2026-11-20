//! Regenerate the committed hostile-input corpus and its manifest.
//!
//! Run from the workspace root:
//!
//! ```text
//! cargo run -p pdfxi-testkit --example generate_hostile
//! ```
//!
//! The builders in `pdfxi_testkit::hostile` are the single source of truth, so
//! regenerating is deterministic and byte-identical to the committed files.
//! The on-disk manifest records a lowercase SHA-256 digest and the expected
//! structure (category, probe, shape marker) for every fixture.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use pdfxi_testkit::hostile::{self, HOSTILE_FILE_MAX_BYTES};

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let dir = root.join("fixtures").join("hostile");
    if let Err(error) = fs::create_dir_all(&dir) {
        eprintln!("create {}: {error}", dir.display());
        std::process::exit(1);
    }

    let fixtures = hostile::all();
    for fixture in &fixtures {
        let bytes = (fixture.build)();
        if bytes.len() >= HOSTILE_FILE_MAX_BYTES {
            eprintln!("{} exceeds the per-file budget", fixture.name);
            std::process::exit(1);
        }
        let path = dir.join(fixture.name);
        if let Err(error) = fs::write(&path, &bytes) {
            eprintln!("write {}: {error}", path.display());
            std::process::exit(1);
        }
        println!("wrote {} ({} bytes)", path.display(), bytes.len());
    }

    let manifest = hostile::emit_manifest(&fixtures);
    let manifest_path = dir.join("manifest.toml");
    if let Err(error) = fs::write(&manifest_path, manifest) {
        eprintln!("write {}: {error}", manifest_path.display());
        std::process::exit(1);
    }
    println!("wrote {}", manifest_path.display());
}
