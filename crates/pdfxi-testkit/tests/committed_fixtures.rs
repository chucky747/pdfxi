//! Integration tests for the committed fixture corpus.
//!
//! These read the PDF files under `fixtures/`, verify their SHA-256 digests
//! against `fixtures/manifest.toml`, and assert page and text structure. They
//! also confirm the generator builders still reproduce the committed bytes.

use std::env;
use std::fs;
use std::path::PathBuf;

use pdfxi_testkit::fixtures::{self, CommittedFixture};
use pdfxi_testkit::hash::sha256_hex;

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
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

/// Strip surrounding double quotes and unescape `\"` from a TOML scalar.
fn unquote(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value[1..value.len() - 1].replace("\\\"", "\"")
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
    for marker in fixture.text_markers {
        assert!(
            text.contains(&format!("({marker}) Tj")),
            "{} missing text marker {marker:?}",
            fixture.file_name
        );
    }
}

fn committed_fixture_for(name: &str) -> &'static CommittedFixture {
    fixtures::COMMITTED_FIXTURES
        .iter()
        .find(|f| f.file_name == name)
        .unwrap_or_else(|| panic!("manifest references unknown fixture {name}"))
}

#[test]
fn manifest_covers_every_committed_fixture() {
    let root = workspace_root();
    let source = fs::read_to_string(root.join("fixtures/manifest.toml"))
        .expect("fixtures/manifest.toml exists");
    let entries = parse_manifest(&source);
    assert_eq!(entries.len(), fixtures::COMMITTED_FIXTURES.len());

    for fixture in fixtures::COMMITTED_FIXTURES {
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
    let root = workspace_root();
    let source = fs::read_to_string(root.join("fixtures/manifest.toml"))
        .expect("fixtures/manifest.toml exists");
    let entries = parse_manifest(&source);

    for entry in &entries {
        let fixture = committed_fixture_for(&entry.name);
        let path = root.join("fixtures").join(&entry.name);
        let bytes = fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));

        assert_eq!(
            sha256_hex(&bytes),
            entry.sha256,
            "SHA-256 mismatch for {}",
            entry.name
        );
        assert_structure(&bytes, fixture);

        let markers: Vec<&str> = entry.text.split('|').collect();
        assert_eq!(
            markers, fixture.text_markers,
            "text markers for {}",
            entry.name
        );
    }
}

#[test]
fn generators_reproduce_committed_bytes() {
    let root = workspace_root();
    for fixture in fixtures::COMMITTED_FIXTURES {
        let path = root.join("fixtures").join(fixture.file_name);
        let on_disk = fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let built = fixtures::build(fixture.file_name)
            .unwrap_or_else(|| panic!("no builder for {}", fixture.file_name));
        assert_eq!(
            on_disk, built,
            "{} is stale; regenerate fixtures",
            fixture.file_name
        );
    }
}

#[test]
fn manifest_hash_is_lowercase_hex_of_expected_length() {
    let root = workspace_root();
    let source = fs::read_to_string(root.join("fixtures/manifest.toml"))
        .expect("fixtures/manifest.toml exists");
    for entry in parse_manifest(&source) {
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
