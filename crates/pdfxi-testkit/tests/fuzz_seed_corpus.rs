//! Integration tests for the deterministic fuzz seed corpus (T-0144).
//!
//! These verify that the committed seeds under `fuzz/corpus/<target>/` match the
//! generated, byte-stable set and its SHA-256 descriptor. Regeneration is
//! deterministic, so a stale or hand-edited seed fails the fast test suite
//! without needing libFuzzer, nightly, a network, or a GUI.

use std::fs;

use pdfxi_testkit::fuzz::{
    MAX_FUZZ_INPUT, fuzz_content, fuzz_crypto, fuzz_filters, fuzz_forms, fuzz_parser,
};
use pdfxi_testkit::seed_corpus::{
    ManifestError, TARGETS, corpus_root, manifest_path, manifest_text, parse_manifest, seeds,
    verify_corpus,
};

type Target = (&'static str, fn(&[u8]) -> pdfxi_testkit::fuzz::FuzzOutcome);

const TARGET_FUNCTIONS: [Target; 5] = [
    ("parser", fuzz_parser),
    ("filters", fuzz_filters),
    ("content", fuzz_content),
    ("forms", fuzz_forms),
    ("crypto", fuzz_crypto),
];

fn committed_manifest() -> Vec<pdfxi_testkit::seed_corpus::ManifestEntry> {
    let text = fs::read_to_string(manifest_path()).expect("fuzz/corpus/manifest.toml exists");
    parse_manifest(&text).expect("committed seed manifest parses")
}

#[test]
fn generated_seeds_are_byte_identical_to_committed_files() {
    let generated = seeds();
    for seed in &generated {
        let path = corpus_root().join(seed.target).join(seed.name);
        let committed =
            fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        assert_eq!(
            committed, seed.bytes,
            "regenerated seed {}/{} is stale",
            seed.target, seed.name
        );
    }
}

#[test]
fn committed_manifest_matches_committed_seed_bytes() {
    let audit = verify_corpus(&corpus_root(), &committed_manifest())
        .expect("every listed seed exists with the recorded size and hash");
    assert_eq!(audit.len(), seeds().len());
    for (target, name, _) in &audit {
        assert!(
            TARGETS.contains(&target.as_str()),
            "manifest lists unknown target {target}"
        );
        assert!(!name.is_empty(), "manifest seed name is empty");
    }
}

#[test]
fn generated_manifest_is_byte_stable_and_matches_committed() {
    let generated = manifest_text();
    assert_eq!(
        generated,
        manifest_text(),
        "manifest generation is not stable"
    );
    let committed = fs::read_to_string(manifest_path()).expect("committed manifest");
    assert_eq!(
        generated, committed,
        "fuzz/corpus/manifest.toml is stale; regenerate seeds"
    );
}

#[test]
fn every_target_has_generated_seeds_and_runs_without_panic() {
    for (target, function) in TARGET_FUNCTIONS {
        let matching: Vec<_> = seeds()
            .into_iter()
            .filter(|seed| seed.target == target)
            .collect();
        assert!(!matching.is_empty(), "no generated seeds for {target}");
        for seed in matching {
            let outcome = function(&seed.bytes);
            assert_eq!(
                outcome.input_len,
                seed.bytes.len().min(MAX_FUZZ_INPUT),
                "{}/{}",
                seed.target,
                seed.name
            );
        }
    }
}

/// The manifest deliberately describes only generated seeds. Pre-existing
/// hand-seeded files in the same directories must be ignored, but a wrong hash,
/// wrong size or missing listed file must still fail (no weakened check).
#[test]
fn verification_ignores_unlisted_files_but_catches_real_mismatches() {
    let root = corpus_root();
    let entries = committed_manifest();

    for (target, name) in [
        ("crypto", "der.bin"),
        ("crypto", "oid.bin"),
        ("crypto", "random.bin"),
    ] {
        assert!(
            root.join(target).join(name).is_file(),
            "pre-existing seed {target}/{name} must still exist"
        );
        assert!(
            !entries
                .iter()
                .any(|entry| entry.target == target && entry.name == name),
            "pre-existing seed {target}/{name} must not be in the generated manifest"
        );
    }
    assert_eq!(
        verify_corpus(&root, &entries)
            .expect("unlisted files are ignored")
            .len(),
        entries.len()
    );

    let mut wrong_hash = entries.clone();
    wrong_hash[0].sha256 = "00".repeat(32);
    assert!(matches!(
        verify_corpus(&root, &wrong_hash),
        Err(ManifestError::HashMismatch(_))
    ));

    let mut wrong_size = entries.clone();
    wrong_size[0].bytes = wrong_size[0].bytes.saturating_add(1);
    assert!(matches!(
        verify_corpus(&root, &wrong_size),
        Err(ManifestError::SizeMismatch(_))
    ));

    let mut missing = entries.clone();
    missing[0].name = "definitely-absent-seed.bin".to_owned();
    assert!(matches!(
        verify_corpus(&root, &missing),
        Err(ManifestError::Missing(_))
    ));
}

#[test]
fn generated_seeds_stay_small_and_bounded() {
    for seed in seeds() {
        assert!(
            seed.bytes.len() <= MAX_FUZZ_INPUT,
            "{}/{} exceeds the fuzz input bound",
            seed.target,
            seed.name
        );
    }
}
