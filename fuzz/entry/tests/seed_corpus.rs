//! Deterministic seed-corpus tests for the `redaction` and `edit` fuzz targets.
//!
//! These call the exact functions used by `fuzz/fuzz_targets/redaction.rs` and
//! `fuzz/fuzz_targets/edit.rs`. They run on a host without nightly Rust,
//! `cargo-fuzz`, or a buildable `libfuzzer-sys`, so a regression in either entry
//! point still fails the fast suite and every committed seed is exercised.

use std::path::{Path, PathBuf};

use pdfxi_core::Document;
use pdfxi_edit::geometry::Rect;
use pdfxi_edit::{
    RedactionMark, RedactionRequest, add_highlight, apply_redaction, list_annotations,
};
use pdfxi_fuzz_entry::{fuzz_edit, fuzz_redaction};
use pdfxi_testkit::fuzz::{FuzzOutcome, MAX_FUZZ_INPUT};

type Target = (&'static str, fn(&[u8]) -> FuzzOutcome);

const TARGETS: &[Target] = &[("redaction", fuzz_redaction), ("edit", fuzz_edit)];

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("corpus")
}

fn seeds(target: &str) -> Vec<(String, Vec<u8>)> {
    let directory = corpus_root().join(target);
    assert!(
        directory.is_dir(),
        "missing seed directory {}",
        directory.display()
    );
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", directory.display()))
        .map(|entry| entry.expect("readable directory entry").path())
        .filter(|path| path.is_file())
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no seeds in {}", directory.display());
    paths
        .into_iter()
        .map(|path| {
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .expect("UTF-8 seed file name")
                .to_owned();
            let bytes = std::fs::read(&path).expect("readable seed file");
            (name, bytes)
        })
        .collect()
}

fn next_byte(state: &mut u64) -> u8 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    (*state >> 33) as u8
}

fn random_input(state: &mut u64) -> Vec<u8> {
    let length = usize::from(next_byte(state)) % 2048;
    let mut input = Vec::with_capacity(length);
    for _ in 0..length {
        input.push(next_byte(state));
    }
    input
}

#[test]
fn every_target_has_a_committed_seed_corpus() {
    for (name, _) in TARGETS {
        let files = seeds(name);
        assert!(
            files.iter().all(|(_, bytes)| !bytes.is_empty()),
            "{name} seeds must be non-empty"
        );
    }
}

#[test]
fn seeds_run_without_panic_and_report_bounded_lengths() {
    for (name, target) in TARGETS {
        for (seed_name, bytes) in seeds(name) {
            let outcome = target(&bytes);
            assert_eq!(
                outcome.input_len,
                bytes.len().min(MAX_FUZZ_INPUT),
                "{name}/{seed_name}"
            );
        }
    }
}

#[test]
fn seeds_report_their_accepted_and_rejected_counts() {
    for (name, target) in TARGETS {
        for (seed_name, bytes) in seeds(name) {
            let outcome = target(&bytes);
            println!(
                "{name}/seed/{seed_name}: len={} accepted={} rejected={}",
                outcome.input_len, outcome.accepted, outcome.rejected
            );
            assert!(
                outcome.accepted >= 1,
                "{name}/{seed_name} must parse as a real PDF"
            );
        }
    }
}

#[test]
fn seeded_random_inputs_run_without_panic() {
    let mut state = 0x5DEE_CE66_D1CF_9F5B_u64;
    for (name, target) in TARGETS {
        for _ in 0..48 {
            let input = random_input(&mut state);
            let outcome = target(&input);
            assert_eq!(outcome.input_len, input.len(), "{name} random input length");
        }
    }
}

#[test]
fn empty_and_oversized_inputs_are_safe() {
    for (name, target) in TARGETS {
        let empty = target(&[]);
        assert_eq!(empty.input_len, 0, "{name} empty input");
        assert_eq!(empty.accepted, 0, "{name} empty input is not a PDF");

        let oversized = vec![0xA5_u8; MAX_FUZZ_INPUT + 1024];
        let outcome = target(&oversized);
        assert_eq!(outcome.input_len, MAX_FUZZ_INPUT, "{name} bounded input");
    }
}

fn seed_bytes(target: &str, name: &str) -> Vec<u8> {
    seeds(target)
        .into_iter()
        .find(|(seed_name, _)| seed_name == name)
        .unwrap_or_else(|| panic!("missing seed {target}/{name}"))
        .1
}

/// A full-page mark must remove the text run rather than cover it, and the
/// removal must survive a full save and reparse.
#[test]
fn redaction_seed_removes_text_from_a_real_page() {
    let mut document =
        Document::from_bytes(seed_bytes("redaction", "text-hello.pdf")).expect("valid seed");
    let media_box = document.page(0).expect("one page").media_box;
    let rect = Rect::new(
        media_box.left as f32 + 1.0,
        media_box.bottom as f32 + 1.0,
        media_box.right as f32 - 1.0,
        media_box.top as f32 - 1.0,
    )
    .expect("in-page rectangle");
    let request = RedactionRequest {
        marks: vec![RedactionMark::new(0, rect)],
        ..RedactionRequest::default()
    };
    let report = apply_redaction(&mut document, &request).expect("redaction applies");
    assert!(
        !report.removed_runs.is_empty(),
        "the Hello run must be removed"
    );

    let saved = document.save().expect("full rewrite");
    let reparsed = Document::from_bytes(saved).expect("reparse");
    let text = reparsed.page_text(0).expect("page text");
    assert!(!text.contains("Hello"), "redacted text survived: {text:?}");
}

/// A highlight must be stored as a real annotation and survive a save/reparse.
#[test]
fn edit_seed_stores_a_highlight_annotation() {
    let mut document =
        Document::from_bytes(seed_bytes("edit", "authoring-annotations.pdf")).expect("valid seed");
    let before = list_annotations(&document, 0).expect("list initial").len();
    let quad = Rect::new(72.0, 100.0, 300.0, 120.0).expect("in-page quad");
    add_highlight(&mut document, 0, &[quad], "fuzz", "fuzz").expect("highlight applies");
    let after = list_annotations(&document, 0).expect("list added").len();
    assert_eq!(after, before + 1, "highlight must be appended");

    let saved = document.save().expect("save");
    let reparsed = Document::from_bytes(saved).expect("reparse");
    assert_eq!(
        list_annotations(&reparsed, 0).expect("list reparsed").len(),
        after,
        "highlight must survive a save/reparse"
    );
}
