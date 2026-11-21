//! Fetch and verify the checksum-pinned external corpus.
//!
//! Usage:
//!
//! ```text
//! cargo run -p pdfxi-testkit --example fetch_corpus
//! cargo run -p pdfxi-testkit --example fetch_corpus -- --offline
//! ```
//!
//! The default mode downloads only missing, approved entries into the corpus
//! cache and verifies each one. `--offline` never touches the network: it
//! verifies files that are already cached and reports absent ones as skips.
//! Cached files live outside the repository; see `corpus/README.md`.

use std::process::ExitCode;

use pdfxi_testkit::external_corpus::{
    CorpusError, FetchOutcome, cache_dir, fetch_entry, load_manifest, verify_cached,
};

fn main() -> ExitCode {
    let offline = std::env::args().any(|arg| arg == "--offline");
    let entries = match load_manifest() {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!("FAIL {error}");
            return ExitCode::FAILURE;
        }
    };
    println!("cache: {}", cache_dir().display());

    let mut failures = 0usize;
    let mut checked = 0usize;
    let mut skipped = 0usize;

    for entry in &entries {
        if !entry.is_pinned() {
            println!("SKIP {} (documentation only; no pinned digest)", entry.id);
            skipped += 1;
            continue;
        }
        if offline {
            match verify_cached(entry) {
                Ok(path) => {
                    println!(
                        "VERIFIED {} sha256={} path={}",
                        entry.id,
                        entry.sha256,
                        path.display()
                    );
                    checked += 1;
                }
                Err(CorpusError::NotCached { .. }) => {
                    println!("SKIP {} (not cached; run without --offline)", entry.id);
                    skipped += 1;
                }
                Err(error) => {
                    eprintln!("FAIL {error}");
                    failures += 1;
                }
            }
            continue;
        }
        match fetch_entry(entry) {
            Ok(FetchOutcome::Downloaded) => {
                println!("DOWNLOADED {} sha256={}", entry.id, entry.sha256);
                checked += 1;
            }
            Ok(FetchOutcome::AlreadyCached) => {
                println!("CACHED {} sha256={}", entry.id, entry.sha256);
                checked += 1;
            }
            Err(error) => {
                eprintln!("FAIL {error}");
                failures += 1;
            }
        }
    }

    println!("SUMMARY checked={checked} skipped={skipped} failures={failures}");
    if failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
