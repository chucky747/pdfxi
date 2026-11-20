//! Regenerate the deterministic fuzz seed corpus and its SHA-256 manifest.
//!
//! This example is intentionally a no-op unless the [`GENERATE_ENV`] flag is
//! set to `1`. That prevents an accidental `cargo run` from overwriting
//! committed seeds. The bytes are produced by `pdfxi_testkit::seed_corpus`, so
//! generation is identical on every host and needs no binary blobs in the diff.
//!
//! ```powershell
//! $env:PDFXI_GENERATE_FUZZ_SEEDS = '1'
//! cargo run -p pdfxi-testkit --example generate_fuzz_seeds
//! Remove-Item Env:\PDFXI_GENERATE_FUZZ_SEEDS
//! ```

use std::path::Path;

use pdfxi_testkit::seed_corpus::{
    GENERATE_ENV, corpus_root, manifest_text, parse_manifest, seeds, write_corpus,
};

fn main() {
    let enabled = std::env::var(GENERATE_ENV)
        .map(|value| value == "1")
        .unwrap_or(false);
    if !enabled {
        println!(
            "generate_fuzz_seeds NO-OP: set {GENERATE_ENV}=1 to write {} seeds under {}",
            seeds().len(),
            corpus_root().display()
        );
        return;
    }

    let root = corpus_root();
    match write_corpus(&root) {
        Ok(count) => {
            println!("generate_fuzz_seeds corpus={}", root.display());
            let audit = parse_manifest(&manifest_text())
                .expect("generated manifest is valid")
                .into_iter()
                .map(|entry| (entry.target, entry.name, entry.sha256))
                .collect::<Vec<_>>();
            for (target, name, sha256) in &audit {
                println!("SEED {target}/{name} sha256={sha256}");
            }
            println!(
                "generate_fuzz_seeds OK seeds={count} targets={} manifest={}",
                pdfxi_testkit::seed_corpus::TARGETS.len(),
                Path::new("manifest.toml").display()
            );
        }
        Err(error) => {
            eprintln!("generate_fuzz_seeds FAILED: {error}");
            std::process::exit(1);
        }
    }
}
