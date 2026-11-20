//! Smoke tests for the real-producer corpus in `fixtures/realworld/`.
//!
//! Every PDF in the directory is listed in `fixtures/realworld/manifest.toml`.
//! For each entry the suite:
//!
//! 1. verifies the committed bytes against the manifest SHA-256;
//! 2. opens the file through the real [`pdfxi_core::Document`];
//! 3. asserts the leaf page count; and
//! 4. asserts marker words in `Document::page_text`, unless the entry records a
//!    current core limitation.
//!
//! `known_failure` entries assert today's failure instead of hiding it. The
//! suite flips when the parser is fixed, forcing the manifest to be updated.
//! The encrypted entry additionally asserts the typed error the core currently
//! returns (`Error::UnexpectedToken`): the core has no password/decryption
//! layer yet, so the encrypted content stream is parsed as plaintext.
//!
//! The suite is deterministic, offline, and reads only files in the repository.

use std::fs;
use std::path::PathBuf;

use pdfxi_core::{Document, Error};
use pdfxi_testkit::hash::sha256_hex;
#[derive(Debug, Clone, Default)]
struct Fixture {
    name: String,
    producer: String,
    sha256: String,
    pages: usize,
    markers: Vec<String>,
    password: String,
    expected_error: String,
    known_failure: String,
}

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root")
        .join("fixtures")
        .join("realworld")
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value[1..value.len() - 1].to_owned()
    } else {
        value.to_owned()
    }
}

fn parse_markers(value: &str) -> Vec<String> {
    let value = value.trim();
    let inner = value
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or("");
    inner
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(unquote)
        .collect()
}

fn parse_manifest(source: &str) -> Vec<Fixture> {
    let mut fixtures: Vec<Fixture> = Vec::new();
    let mut current: Option<Fixture> = None;
    for raw in source.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "[[fixture]]" {
            if let Some(fixture) = current.take() {
                fixtures.push(fixture);
            }
            current = Some(Fixture::default());
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let Some(fixture) = current.as_mut() else {
            continue;
        };
        match key.trim() {
            "name" => fixture.name = unquote(value),
            "producer" => fixture.producer = unquote(value),
            "sha256" => fixture.sha256 = unquote(value),
            "pages" => fixture.pages = value.trim().parse().expect("numeric pages"),
            "markers" => fixture.markers = parse_markers(value),
            "password" => fixture.password = unquote(value),
            "expected_error" => fixture.expected_error = unquote(value),
            "known_failure" => fixture.known_failure = unquote(value),
            _ => {}
        }
    }
    if let Some(fixture) = current {
        fixtures.push(fixture);
    }
    fixtures
}

fn manifest() -> Vec<Fixture> {
    let path = corpus_dir().join("manifest.toml");
    let source = fs::read_to_string(&path).expect("read real-world manifest");
    let fixtures = parse_manifest(&source);
    assert!(!fixtures.is_empty(), "manifest has no fixtures");
    fixtures
}
#[test]
fn manifest_covers_every_pdf_in_the_directory() {
    let mut on_disk: Vec<String> = fs::read_dir(corpus_dir())
        .expect("read fixtures/realworld")
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.ends_with(".pdf").then_some(name)
        })
        .collect();
    on_disk.sort();

    let mut listed: Vec<String> = manifest().into_iter().map(|fixture| fixture.name).collect();
    listed.sort();

    assert_eq!(listed, on_disk, "manifest must list every PDF exactly once");
}

#[test]
fn manifest_entries_are_well_formed() {
    let fixtures = manifest();
    for fixture in &fixtures {
        assert!(!fixture.producer.is_empty(), "{}: producer", fixture.name);
        assert_eq!(fixture.sha256.len(), 64, "{}: sha256 length", fixture.name);
        assert!(fixture.pages > 0, "{}: pages", fixture.name);
        assert!(
            fixture
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "{}: sha256 must be lower-case hex",
            fixture.name
        );
        if fixture.expected_error.is_empty() {
            assert!(
                !fixture.markers.is_empty(),
                "{}: a non-failing fixture needs marker words",
                fixture.name
            );
        }
        if !fixture.password.is_empty() {
            assert!(
                !fixture.known_failure.is_empty(),
                "{}: a password fixture records the current core limitation",
                fixture.name
            );
        }
    }
}

#[test]
fn checksums_match_the_committed_bytes() {
    for fixture in manifest() {
        let path = corpus_dir().join(&fixture.name);
        let bytes =
            fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        assert_eq!(
            sha256_hex(&bytes),
            fixture.sha256,
            "{}: SHA-256 differs from the manifest",
            fixture.name
        );
    }
}
#[test]
fn opens_every_fixture_with_the_core_parser() {
    for fixture in manifest() {
        let path = corpus_dir().join(&fixture.name);
        let bytes =
            fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let document = Document::from_bytes(bytes)
            .unwrap_or_else(|error| panic!("{}: from_bytes failed: {error}", fixture.name));
        assert_eq!(
            document.page_count().expect("page count"),
            fixture.pages,
            "{}: page count",
            fixture.name
        );
    }
}

#[test]
fn extracts_marker_words_or_asserts_the_recorded_limitation() {
    for fixture in manifest() {
        let path = corpus_dir().join(&fixture.name);
        let bytes =
            fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let document = Document::from_bytes(bytes).expect("from_bytes");

        if !fixture.expected_error.is_empty() {
            assert_eq!(
                fixture.expected_error, "UnexpectedToken",
                "{}: only UnexpectedToken is modelled in this round",
                fixture.name
            );
            let mut checked = 0;
            for index in 0..fixture.pages {
                match document.page_text(index) {
                    Err(Error::UnexpectedToken(_)) => checked += 1,
                    other => panic!(
                        "{}: page {index} expected Error::UnexpectedToken (no decrypt layer), got {other:?}",
                        fixture.name
                    ),
                }
            }
            assert_eq!(checked, fixture.pages, "{}: pages checked", fixture.name);
            continue;
        }

        let mut text = String::new();
        for index in 0..fixture.pages {
            let page = document.page_text(index).unwrap_or_else(|error| {
                panic!("{}: page {index} text failed: {error}", fixture.name)
            });
            text.push_str(&page);
            text.push('\n');
        }

        if !fixture.known_failure.is_empty() {
            for marker in &fixture.markers {
                assert!(
                    !text.contains(marker),
                    "{}: marker {marker:?} is now extracted; update or remove known_failure \
                     ({})",
                    fixture.name,
                    fixture.known_failure
                );
            }
            assert!(
                !text.trim().is_empty(),
                "{}: known_failure expects some text, got none",
                fixture.name
            );
            continue;
        }

        for marker in &fixture.markers {
            assert!(
                text.contains(marker),
                "{}: missing marker {marker:?} in extracted text: {text:?}",
                fixture.name
            );
        }
    }
}
