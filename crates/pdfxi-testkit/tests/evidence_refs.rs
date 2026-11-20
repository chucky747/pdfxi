//! Guards the acceptance-gate evidence references (T-0154).
//!
//! These are cheap, offline, std-only checks that keep the QA evidence pack
//! honest: every `docs/evidence/.../T-*.md` path cited by `docs/TESTING.md`
//! must exist, the T-0154 evidence file and status fragment must be present,
//! and the evidence file must cite every command the gate promises to run.
//! A broken link or a missing transcript fails fast in `cargo test -p
//! pdfxi-testkit` instead of being discovered during release review.

use std::env;
use std::fs;
use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .expect("workspace root")
        .to_path_buf()
}

fn read_root_relative(relative: &str) -> String {
    let path = workspace_root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

/// Collect the `docs/evidence/<lane>/<id>.md` paths referenced in a document.
fn evidence_links(source: &str) -> Vec<String> {
    const PREFIX: &str = "docs/evidence/";
    let mut links = Vec::new();
    let mut rest = source;
    while let Some(at) = rest.find(PREFIX) {
        let tail = &rest[at..];
        let end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '`' | ')' | ']' | ','))
            .unwrap_or(tail.len());
        let candidate = &tail[..end];
        if candidate.ends_with(".md") && !links.iter().any(|link: &String| link == candidate) {
            links.push(candidate.to_owned());
        }
        rest = &tail[end.max(1)..];
    }
    links
}

#[test]
fn documented_evidence_links_exist() {
    let testing = read_root_relative("docs/TESTING.md");
    let links = evidence_links(&testing);
    assert!(
        links.len() >= 2,
        "docs/TESTING.md should cite at least two evidence files, found {links:?}"
    );
    for link in links {
        let path = workspace_root().join(&link);
        assert!(
            path.is_file(),
            "documented evidence link does not exist: {link}"
        );
    }
}

#[test]
fn t0154_evidence_pack_files_exist() {
    for relative in [
        "docs/evidence/qa/T-0154.md",
        "orchestration/feature-updates/T-0154.toml",
    ] {
        let path = workspace_root().join(relative);
        assert!(
            path.is_file(),
            "T-0154 evidence pack file is missing: {relative}"
        );
    }
}

#[test]
fn t0154_evidence_cites_every_gate_command() {
    let evidence = read_root_relative("docs/evidence/qa/T-0154.md");
    for command in [
        "cargo build --workspace --release",
        "cargo fmt --all -- --check",
        "cargo clippy --workspace --all-targets --all-features",
        "cargo test --workspace",
        "cargo xtask corpus smoke",
        "cargo xtask docs",
    ] {
        assert!(
            evidence.contains(command),
            "evidence file does not cite required command: {command}"
        );
    }
}
