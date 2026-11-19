//! Deterministic smoke runner for every fuzz target.
//!
//! Feeds the committed seed corpus plus a fixed pseudo-random byte stream to
//! all five fuzz entry points. No network, GUI, sleeps, or secrets are used,
//! and the output is byte-for-byte reproducible on any machine.
//!
//! Run with:
//! `cargo run -p pdfxi-testkit --example fuzz_smoke`

use std::path::{Path, PathBuf};

use pdfxi_testkit::fuzz::{
    FuzzOutcome, fuzz_content, fuzz_crypto, fuzz_filters, fuzz_forms, fuzz_parser,
};

/// A target name paired with its plain fuzz function.
type Target = (&'static str, fn(&[u8]) -> FuzzOutcome);

const TARGETS: &[Target] = &[
    ("parser", fuzz_parser),
    ("filters", fuzz_filters),
    ("content", fuzz_content),
    ("forms", fuzz_forms),
    ("crypto", fuzz_crypto),
];

/// Number of seeded pseudo-random inputs generated per target.
const RANDOM_INPUTS: usize = 48;

/// Locate the committed corpus relative to this crate's manifest.
fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fuzz")
        .join("corpus")
}

/// Advance a deterministic xorshift-style generator by one byte.
fn next_byte(state: &mut u64) -> u8 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    (*state >> 33) as u8
}

/// Produce one reproducible pseudo-random input of a bounded length.
fn random_input(state: &mut u64) -> Vec<u8> {
    let length = usize::from(next_byte(state)) % 2048;
    let mut input = Vec::with_capacity(length);
    for _ in 0..length {
        input.push(next_byte(state));
    }
    input
}

/// Read every seed file for one target, sorted by file name.
fn seeds_for(target: &str) -> Vec<(String, Vec<u8>)> {
    let directory = corpus_root().join(target);
    let mut seeds = Vec::new();
    if let Ok(entries) = std::fs::read_dir(directory) {
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .collect();
        paths.sort();
        for path in paths {
            if let (Some(name), Ok(bytes)) = (
                path.file_name().and_then(|value| value.to_str()),
                std::fs::read(&path),
            ) {
                seeds.push((name.to_owned(), bytes));
            }
        }
    }
    seeds
}

fn main() {
    let mut state = 0x243F_6A88_85A3_08D3_u64;
    let mut total_accepted = 0_usize;
    let mut total_rejected = 0_usize;
    let mut total_inputs = 0_usize;
    let mut corpus_root_found = false;

    println!("fuzz_smoke corpus={}", corpus_root().display());

    for (name, target) in TARGETS {
        let seeds = seeds_for(name);
        if !seeds.is_empty() {
            corpus_root_found = true;
        }
        let mut accepted = 0_usize;
        let mut rejected = 0_usize;
        let mut cases = 0_usize;

        for (seed_name, bytes) in &seeds {
            let outcome = target(bytes);
            println!(
                "  {name}/seed/{seed_name}: len={} accepted={} rejected={}",
                outcome.input_len, outcome.accepted, outcome.rejected
            );
            accepted += outcome.accepted;
            rejected += outcome.rejected;
            cases += 1;
        }

        for index in 0..RANDOM_INPUTS {
            let input = random_input(&mut state);
            let outcome = target(&input);
            accepted += outcome.accepted;
            rejected += outcome.rejected;
            cases += 1;
            if index + 1 == RANDOM_INPUTS {
                println!(
                    "  {name}/random: cases={RANDOM_INPUTS} accepted={accepted} rejected={rejected}"
                );
            }
        }

        let empty = target(&[]);
        accepted += empty.accepted;
        rejected += empty.rejected;
        cases += 1;

        println!("  {name}/total: cases={cases} accepted={accepted} rejected={rejected}");
        total_accepted += accepted;
        total_rejected += rejected;
        total_inputs += cases;
    }

    println!("fuzz_smoke corpus_found={corpus_root_found}");
    println!(
        "fuzz_smoke OK targets={} inputs={total_inputs} accepted={total_accepted} rejected={total_rejected}",
        TARGETS.len()
    );
}
