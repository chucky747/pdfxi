//! Integration tests for the committed fixture corpus.
//!
//! These read the PDF files under `fixtures/`, verify their SHA-256 digests
//! against `fixtures/manifest.toml`, and assert page and text structure. They
//! also confirm the generator builders still reproduce the committed bytes and
//! that regeneration is byte-identical.
//!
//! # Why no `pdfxi-core` dependency
//!
//! `pdfxi-core` already depends on `pdfxi-testkit` as a dev-dependency. Adding
//! the reverse edge here would create a dependency cycle, so these tests assert
//! structure at the PDF byte level (header, trailer, `startxref`, xref target,
//! object counts, and text markers) instead of calling a core parser. The
//! parser's own tests in `pdfxi-core` cover these same fixtures from the other
//! direction. This keeps `cargo test -p pdfxi-testkit` cycle-free and fast.

use std::env;
use std::fs;
use std::path::PathBuf;

use pdfxi_testkit::fixtures::{self, CORPUS_SIZE_BUDGET, CommittedFixture, FIXTURE_COUNT};
use pdfxi_testkit::hash::sha256_hex;
use pdfxi_testkit::seed_corpus::{
    corpus_root, manifest_path, parse_manifest as parse_seed_manifest, verify_corpus,
};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .expect("workspace root")
        .to_path_buf()
}

#[derive(Debug, Clone)]
struct ManifestFixture {
    name: String,
    builder: String,
    sha256: String,
    pages: usize,
    text: String,
}

/// Reverse the escaping performed by `fixtures::toml_quote`.
fn unescape(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Strip surrounding double quotes from a TOML basic-string scalar.
fn unquote(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        unescape(&value[1..value.len() - 1])
    } else {
        value.to_string()
    }
}

/// Minimal parser for the flat `[[fixture]]` tables in manifest.toml.
/// Avoids adding a TOML dependency to the test-only crate.
fn parse_manifest(source: &str) -> Vec<ManifestFixture> {
    let mut entries: Vec<ManifestFixture> = Vec::new();
    let mut current: Option<ManifestFixture> = None;
    for raw in source.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "[[fixture]]" {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            current = Some(ManifestFixture {
                name: String::new(),
                builder: String::new(),
                sha256: String::new(),
                pages: 0,
                text: String::new(),
            });
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let Some(entry) = current.as_mut() else {
            continue;
        };
        match key.trim() {
            "name" => entry.name = unquote(value),
            "builder" => entry.builder = unquote(value),
            "sha256" => entry.sha256 = unquote(value),
            "pages" => {
                entry.pages = value
                    .trim()
                    .parse()
                    .expect("numeric pages value in manifest");
            }
            "text" => entry.text = unquote(value),
            _ => {}
        }
    }
    if let Some(entry) = current {
        entries.push(entry);
    }
    entries
}

fn as_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Read the byte offset written after the `startxref` keyword.
fn startxref(bytes: &[u8]) -> usize {
    let text = as_text(bytes);
    let marker = "startxref\n";
    let at = text.find(marker).expect("startxref keyword");
    let tail = &text[at + marker.len()..];
    let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().expect("numeric startxref")
}

fn assert_structure(bytes: &[u8], fixture: &CommittedFixture) {
    assert!(
        bytes.starts_with(b"%PDF-1.7"),
        "{} header",
        fixture.file_name
    );
    assert!(bytes.ends_with(b"%%EOF\n"), "{} EOF", fixture.file_name);

    let offset = startxref(bytes);
    assert!(
        offset < bytes.len(),
        "{} startxref range",
        fixture.file_name
    );
    assert_eq!(
        &bytes[offset..offset + 4],
        b"xref",
        "{} startxref target",
        fixture.file_name
    );

    let text = as_text(bytes);
    assert_eq!(
        text.matches("/Type /Page ").count(),
        fixture.pages,
        "{} page object count",
        fixture.file_name
    );
    assert!(
        text.contains(&format!("/Count {}", fixture.pages)),
        "{} page count",
        fixture.file_name
    );
    // Two root objects + two objects per page + one shared font.
    assert_eq!(
        text.matches(" 0 obj").count(),
        2 + 2 * fixture.pages + 1,
        "{} object count",
        fixture.file_name
    );
    for marker in &fixture.text_markers {
        assert!(
            text.contains(&format!("({marker}) Tj")),
            "{} missing text marker {marker:?}",
            fixture.file_name
        );
    }
}

fn committed_fixture_for(name: &str) -> CommittedFixture {
    fixtures::by_name(name).unwrap_or_else(|| panic!("manifest references unknown fixture {name}"))
}

fn manifest_source() -> String {
    fs::read_to_string(workspace_root().join("fixtures/manifest.toml"))
        .expect("fixtures/manifest.toml exists")
}

#[test]
fn corpus_contains_fifty_fixtures() {
    let all = fixtures::all();
    assert_eq!(all.len(), FIXTURE_COUNT);
    assert_eq!(parse_manifest(&manifest_source()).len(), FIXTURE_COUNT);
}

#[test]
fn manifest_covers_every_committed_fixture() {
    let all = fixtures::all();
    let entries = parse_manifest(&manifest_source());
    assert_eq!(entries.len(), all.len());

    for fixture in &all {
        let entry = entries
            .iter()
            .find(|e| e.name == fixture.file_name)
            .unwrap_or_else(|| panic!("manifest missing {}", fixture.file_name));
        assert_eq!(
            entry.builder, fixture.builder,
            "builder for {}",
            fixture.file_name
        );
        assert_eq!(
            entry.pages, fixture.pages,
            "pages for {}",
            fixture.file_name
        );
    }
}

#[test]
fn committed_fixtures_match_manifest_hashes_and_structure() {
    let entries = parse_manifest(&manifest_source());
    for entry in &entries {
        let fixture = committed_fixture_for(&entry.name);
        let path = workspace_root().join("fixtures").join(&entry.name);
        let bytes = fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));

        assert_eq!(
            sha256_hex(&bytes),
            entry.sha256,
            "SHA-256 mismatch for {}",
            entry.name
        );
        assert_eq!(
            entry.pages, fixture.pages,
            "manifest page count for {}",
            entry.name
        );
        assert_structure(&bytes, &fixture);

        let markers: Vec<&str> = if entry.text.is_empty() {
            Vec::new()
        } else {
            entry.text.split('|').collect()
        };
        assert_eq!(
            markers,
            fixture
                .text_markers
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            "text markers for {}",
            entry.name
        );
    }
}

#[test]
fn generators_reproduce_committed_bytes() {
    let root = workspace_root();
    for fixture in fixtures::all() {
        let path = root.join("fixtures").join(&fixture.file_name);
        let on_disk = fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let built = fixture.build();
        assert_eq!(
            on_disk, built,
            "{} is stale; regenerate fixtures",
            fixture.file_name
        );
        assert_eq!(
            built,
            fixture.build(),
            "{} regeneration is not byte-identical",
            fixture.file_name
        );
    }
}

#[test]
fn manifest_hash_is_lowercase_hex_of_expected_length() {
    for entry in parse_manifest(&manifest_source()) {
        assert_eq!(entry.sha256.len(), 64, "hash length for {}", entry.name);
        assert!(
            entry
                .sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "hash must be lower-case hex for {}",
            entry.name
        );
    }
}

#[test]
fn required_page_counts_are_covered() {
    let pages: Vec<usize> = parse_manifest(&manifest_source())
        .into_iter()
        .map(|entry| entry.pages)
        .collect();
    for required in [1, 2, 3, 10, 100] {
        assert!(
            pages.contains(&required),
            "no fixture with {required} pages"
        );
    }
}

#[test]
fn total_corpus_size_is_within_budget() {
    let root = workspace_root();
    let total: u64 = fixtures::all()
        .iter()
        .map(|fixture| {
            fs::metadata(root.join("fixtures").join(&fixture.file_name))
                .unwrap_or_else(|e| panic!("stat {}: {e}", fixture.file_name))
                .len()
        })
        .sum();
    assert!(
        total < CORPUS_SIZE_BUDGET as u64,
        "committed corpus is {total} bytes, budget is {}",
        CORPUS_SIZE_BUDGET
    );
}

#[test]
fn corpus_manifest_triggers_no_downloads() {
    // The external-corpus descriptors are documentation only for M1. A fetch
    // tool must refuse them because none carries a verified digest. This keeps
    // `cargo test -p pdfxi-testkit` network-free and fail-closed.
    let source = fs::read_to_string(workspace_root().join("corpus/manifest.toml"))
        .expect("corpus/manifest.toml exists");
    let mut in_corpus = false;
    let mut descriptors = 0usize;
    for raw in source.lines() {
        let line = raw.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if line == "[[corpus]]" {
            in_corpus = true;
            descriptors += 1;
            continue;
        }
        if line.starts_with('[') {
            in_corpus = false;
            continue;
        }
        if in_corpus && line.starts_with("sha256") {
            let value = line
                .split_once('=')
                .map(|(_, v)| unquote(v))
                .unwrap_or_default();
            assert!(
                value.is_empty(),
                "corpus descriptor has a fetch-ready digest; downloads are disabled for M1"
            );
        }
    }
    assert!(
        descriptors > 0,
        "expected at least one documented corpus descriptor"
    );
}

/// The generated fuzz seeds are part of the smoke corpus. Verify each listed
/// seed against its recorded SHA-256 and print the audit trail so a `cargo
/// xtask corpus smoke` run reports the manifest hashes (with `--nocapture`).
#[test]
fn fuzz_seed_manifest_hashes_are_valid() {
    let text = fs::read_to_string(manifest_path()).expect("fuzz seed manifest exists");
    let entries = parse_seed_manifest(&text).expect("fuzz seed manifest parses");
    let audit = verify_corpus(&corpus_root(), &entries).expect("fuzz seeds match manifest");
    for (target, name, sha256) in &audit {
        println!("SEED {target}/{name} sha256={sha256}");
    }
    assert_eq!(
        audit.len(),
        entries.len(),
        "every manifest row was verified"
    );
}

/// The hostile-input corpus (T-0183) is part of the smoke audit. Its manifest
/// records a lowercase SHA-256 and the expected structure for every generated
/// adversarial file; this test keeps `cargo xtask corpus smoke` honest about
/// the additive corpus without changing the valid 50-fixture count above.
#[test]
fn hostile_corpus_manifest_hashes_and_structure_are_valid() {
    use pdfxi_testkit::hostile::{HOSTILE_COUNT, all, committed_manifest, emit_manifest};

    let on_disk = committed_manifest();
    assert_eq!(on_disk.len(), HOSTILE_COUNT);
    assert_eq!(on_disk, parse_seed_hostile(&emit_manifest(&all())));
    for entry in &on_disk {
        assert_eq!(entry.sha256.len(), 64, "hash length for {}", entry.name);
        assert!(
            entry
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "hash must be lowercase hex for {}",
            entry.name
        );
        let path = workspace_root()
            .join("fixtures")
            .join("hostile")
            .join(&entry.name);
        let bytes = fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        assert_eq!(
            sha256_hex(&bytes),
            entry.sha256,
            "SHA-256 mismatch for {}",
            entry.name
        );
    }
}

fn parse_seed_hostile(source: &str) -> Vec<pdfxi_testkit::hostile::HostileFixture> {
    pdfxi_testkit::hostile::parse_manifest(source)
}
