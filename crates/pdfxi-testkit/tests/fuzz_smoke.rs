//! Deterministic smoke tests over committed fuzz seeds and seeded random input.
//!
//! These tests call the exact functions used by `fuzz/fuzz_targets/*.rs`, so a
//! regression in any fuzz entry point fails the fast test suite without needing
//! libFuzzer, nightly, a network, or a GUI.

use std::path::{Path, PathBuf};

use pdfxi_core::{Filter, decode_stream_with_limit, extract_text};
use pdfxi_forms::parse_xfdf;
use pdfxi_testkit::fuzz::{
    FuzzOutcome, MAX_FUZZ_INPUT, fuzz_content, fuzz_crypto, fuzz_filters, fuzz_forms, fuzz_parser,
};

type Target = (&'static str, fn(&[u8]) -> FuzzOutcome);

const TARGETS: &[Target] = &[
    ("parser", fuzz_parser),
    ("filters", fuzz_filters),
    ("content", fuzz_content),
    ("forms", fuzz_forms),
    ("crypto", fuzz_crypto),
];

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fuzz")
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
    let length = usize::from(next_byte(state)) % 4096;
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
fn seeded_random_inputs_run_without_panic() {
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    for (name, target) in TARGETS {
        for _ in 0..64 {
            let input = random_input(&mut state);
            let outcome = target(&input);
            assert_eq!(outcome.input_len, input.len(), "{name} random input length");
        }
    }
}

#[test]
fn oversized_and_empty_inputs_are_safe() {
    for (name, target) in TARGETS {
        let empty = target(&[]);
        assert_eq!(empty.input_len, 0, "{name} empty input");

        let oversized = vec![0xA5_u8; MAX_FUZZ_INPUT + 1024];
        let outcome = target(&oversized);
        assert_eq!(outcome.input_len, MAX_FUZZ_INPUT, "{name} bounded input");
    }
}

#[test]
fn valid_seeds_exercise_real_parsers() {
    let parser_seeds = seeds("parser");
    let minimal = parser_seeds
        .iter()
        .find(|(name, _)| name == "minimal.pdf")
        .expect("minimal.pdf parser seed");
    assert!(
        fuzz_parser(&minimal.1).accepted >= 1,
        "valid PDF must be parsed"
    );

    let content_seeds = seeds("content");
    let operators = content_seeds
        .iter()
        .find(|(name, _)| name == "operators.bin")
        .expect("operators.bin content seed");
    let text = extract_text(&operators.1).expect("valid content stream");
    assert!(
        text.contains("Hello"),
        "content tokenizer must read Tj strings"
    );

    let filter_seeds = seeds("filters");
    let zlib = filter_seeds
        .iter()
        .find(|(name, _)| name == "zlib.bin")
        .expect("zlib.bin filter seed");
    let decoded =
        decode_stream_with_limit(&zlib.1, &[Filter::FlateDecode(Default::default())], 1024)
            .expect("valid raw DEFLATE seed");
    assert_eq!(decoded, b"Hi!!");

    let form_seeds = seeds("forms");
    let xfdf = form_seeds
        .iter()
        .find(|(name, _)| name == "xfdf.xml")
        .expect("xfdf.xml form seed");
    let text = std::str::from_utf8(&xfdf.1).expect("XFDF seed is UTF-8");
    assert_eq!(parse_xfdf(text).expect("valid XFDF seed").len(), 1);
    assert!(fuzz_forms(&xfdf.1).accepted >= 1);

    let crypto_seeds = seeds("crypto");
    let der = crypto_seeds
        .iter()
        .find(|(name, _)| name == "der.bin")
        .expect("der.bin crypto seed");
    assert!(
        fuzz_crypto(&der.1).accepted >= 4,
        "valid DER seed must be read"
    );
}
