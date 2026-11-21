//! Integration tests for the checksum-pinned external corpus.
//!
//! These tests are strictly offline: they parse `corpus/manifest.toml` and
//! inspect only files that are already present in the corpus cache. An absent
//! cached file is a skip; a cached file whose digest or size disagrees with the
//! manifest is a hard failure. Fetching is covered separately by the
//! `fetch_corpus` example and is never invoked here.

use std::fs;
use std::process;

use pdfxi_testkit::external_corpus::{
    CorpusEntry, CorpusError, MAX_CORPUS_BYTES, cache_dir, cached_path, fetch_entry,
    is_lowercase_hex, load_manifest, manifest_path, smoke_cached, verify_cached,
};

fn pinned() -> Vec<CorpusEntry> {
    load_manifest()
        .expect("corpus manifest loads")
        .into_iter()
        .filter(|e| e.is_pinned())
        .collect()
}

#[test]
fn manifest_is_readable_and_non_empty() {
    assert!(manifest_path().ends_with("corpus/manifest.toml"));
    let entries = load_manifest().expect("corpus manifest parses");
    assert!(!entries.is_empty(), "manifest has no descriptors");
    assert!(!pinned().is_empty(), "manifest has no fetchable entries");
}

#[test]
fn manifest_entries_are_well_formed() {
    for entry in load_manifest().expect("manifest loads") {
        assert!(!entry.license.is_empty(), "{}: license", entry.id);
        assert!(!entry.source.is_empty(), "{}: source", entry.id);
        if entry.is_pinned() {
            assert!(
                is_lowercase_hex(&entry.sha256),
                "{}: lowercase hex digest",
                entry.id
            );
            assert_eq!(entry.sha256.len(), 64, "{}: digest length", entry.id);
            assert!(
                entry.bytes > 0 && entry.bytes <= MAX_CORPUS_BYTES,
                "{}: bounded size",
                entry.id
            );
            assert!(entry.pages > 0, "{}: pages", entry.id);
            assert!(!entry.markers.is_empty(), "{}: markers", entry.id);
            assert!(entry.url.starts_with("https://"), "{}: https URL", entry.id);
            assert!(
                !entry.url.contains("example.invalid"),
                "{}: resolved URL",
                entry.id
            );
            assert_eq!(entry.status, "approved", "{}: status", entry.id);
        } else {
            assert_eq!(
                entry.sha256, "",
                "{}: unpinned digest must be empty",
                entry.id
            );
            assert_eq!(entry.status, "unapproved", "{}: unpinned status", entry.id);
        }
    }
}

#[test]
fn cached_files_match_manifest_hashes() {
    let entries = pinned();
    let mut checked = 0usize;
    let mut skipped = 0usize;
    for entry in &entries {
        let path = cached_path(entry).expect("safe cache path");
        if !path.exists() {
            println!("SKIP {} (not cached at {})", entry.id, path.display());
            skipped += 1;
            continue;
        }
        let resolved = verify_cached(entry).unwrap_or_else(|error| panic!("{}", error));
        assert_eq!(resolved, path, "{}: verified path", entry.id);
        println!("HASH OK {} sha256={}", entry.id, entry.sha256);
        checked += 1;
    }
    assert_eq!(
        checked + skipped,
        entries.len(),
        "every pinned entry was checked or skipped"
    );
}

#[test]
fn a_cached_file_with_the_wrong_hash_fails_clearly() {
    // Use a unique id so this negative test cannot collide with a real cache
    // entry or with a parallel test run.
    let mut entry = pinned()[0].clone();
    entry.id = format!("wrong-hash-test-{}", process::id());
    entry.file = "wrong.pdf".to_owned();
    entry.bytes = 4;
    entry.sha256 = "0".repeat(64);
    let path = cached_path(&entry).expect("safe cache path");
    fs::create_dir_all(path.parent().expect("cache parent")).expect("create cache parent");
    fs::write(&path, b"nope").expect("write deliberately wrong bytes");

    let error = verify_cached(&entry).unwrap_err();
    let message = error.to_string();
    assert!(
        matches!(error, CorpusError::HashMismatch { .. }),
        "wrong digest detected: {message}"
    );
    assert!(
        message.contains(&entry.id),
        "error names the corpus id: {message}"
    );
    assert!(
        message.contains("SHA-256 mismatch"),
        "error explains the mismatch: {message}"
    );
    let _ = fs::remove_file(&path);
    let _ = fs::remove_dir(path.parent().expect("cache parent"));
}

#[test]
fn fetch_refuses_unpinned_descriptors_without_network() {
    for entry in load_manifest()
        .expect("manifest loads")
        .into_iter()
        .filter(|e| !e.is_pinned())
    {
        let error = fetch_entry(&entry).unwrap_err();
        assert!(
            matches!(error, CorpusError::NotPinned { .. }),
            "{} must fail closed",
            entry.id
        );
    }
}

#[test]
fn fetch_refuses_a_manifest_declared_size_above_the_bound() {
    let mut entry = pinned()[0].clone();
    entry.bytes = MAX_CORPUS_BYTES + 1;
    let error = fetch_entry(&entry).unwrap_err();
    assert!(
        matches!(error, CorpusError::TooLarge { .. }),
        "oversize entry is refused"
    );
}

#[test]
fn cache_paths_reject_traversal() {
    let mut entry = pinned()[0].clone();
    entry.file = "../../escape.pdf".to_owned();
    assert!(matches!(
        cached_path(&entry),
        Err(CorpusError::UnsafePath { .. })
    ));
    entry.file = "ok.pdf".to_owned();
    entry.id = "../escape".to_owned();
    assert!(matches!(
        cached_path(&entry),
        Err(CorpusError::UnsafePath { .. })
    ));
    entry.id = "safe-id".to_owned();
    assert_eq!(
        cached_path(&entry).expect("safe path"),
        cache_dir().join("safe-id").join("ok.pdf")
    );
}

#[test]
fn offline_smoke_skips_absent_files_and_reports_no_failures() {
    let summary = smoke_cached(&pinned());
    for id in &summary.skipped {
        println!("SKIP {id} (not cached; run the fetch command)");
    }
    for report in &summary.checked {
        println!(
            "SMOKE {} pages={} path={}",
            report.id,
            report.pages,
            report.path.display()
        );
    }
    assert!(
        summary.failures.is_empty(),
        "cached corpus must pass verification and smoke"
    );
    assert_eq!(
        summary.checked.len() + summary.skipped.len(),
        pinned().len()
    );
}
