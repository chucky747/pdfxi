//! No-panic robustness tests for the committed malformed-PDF corpus.
//!
//! Every file under `fixtures/malformed/` is opened through the public
//! [`pdfxi_core::Document`] API and exercised across the whole read surface and
//! the save path. A panic is a test failure: each fixture is run inside
//! `catch_unwind` only so the failing file name appears in the message.
//!
//! The manifest declares a `category` for one specific `probe` operation. The
//! test runs every probe, then checks that the declared operation produced a
//! structured error from the expected family — or succeeded when the category
//! is `ok`. `pdfxi_core::Error` is a closed `thiserror` enum, so any error value
//! is already a structured variant by construction.

use std::env;
use std::fs;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;

use pdfxi_core::{Document, Error};
use pdfxi_testkit::hash::sha256_hex;
use pdfxi_testkit::malformed::{
    MALFORMED_CORPUS_SIZE_BUDGET, MALFORMED_COUNT, MALFORMED_FILE_MAX_BYTES, MalformedEntry, Probe,
    parse_manifest,
};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .expect("workspace root")
        .to_path_buf()
}

fn corpus_dir() -> PathBuf {
    workspace_root().join("fixtures").join("malformed")
}

fn committed_manifest() -> Vec<MalformedEntry> {
    let path = corpus_dir().join("manifest.toml");
    let source =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    parse_manifest(&source)
}

fn read_fixture(entry: &MalformedEntry) -> Vec<u8> {
    let path = corpus_dir().join(&entry.name);
    fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Errors observed from the four probed operations, plus save-reparse status.
#[derive(Debug, Default)]
struct Outcomes {
    open: Option<Error>,
    pages: Option<Error>,
    content: Option<Error>,
    save: Option<Error>,
    /// `true` when save returned bytes that could not be reparsed.
    save_unreparseable: bool,
}

impl Outcomes {
    fn for_probe(&self, probe: Probe) -> Option<&Error> {
        match probe {
            Probe::Open => self.open.as_ref(),
            Probe::Pages => self.pages.as_ref(),
            Probe::Content => self.content.as_ref(),
            Probe::Save => self.save.as_ref(),
        }
    }
}

/// Run every probe against one fixture's bytes.
fn exercise(bytes: Vec<u8>) -> Outcomes {
    let document = match Document::from_bytes(bytes) {
        Ok(document) => Some(document),
        Err(error) => {
            return Outcomes {
                open: Some(error),
                ..Outcomes::default()
            };
        }
    };
    let Some(document) = document else {
        return Outcomes::default();
    };

    let pages = document.page_count().err();
    let content = document.page_content(0).err();
    let (save, save_unreparseable) = match document.save() {
        Err(error) => (Some(error), false),
        Ok(saved) => (
            None,
            !Document::from_bytes(saved)
                .and_then(|reparsed| reparsed.page_count())
                .is_ok(),
        ),
    };

    Outcomes {
        open: None,
        pages,
        content,
        save,
        save_unreparseable,
    }
}

fn assert_category(entry: &MalformedEntry, error: Option<&Error>) {
    let observed = || format!("{} (category {}): {error:?}", entry.name, entry.category);
    let is = |matches: fn(&Error) -> bool| error.is_some_and(matches);
    match entry.category.as_str() {
        "ok" => assert!(error.is_none(), "{}", observed()),
        "invalid_header" => assert!(is(|e| matches!(e, Error::InvalidHeader)), "{}", observed()),
        "missing_startxref" => {
            assert!(
                is(|e| matches!(e, Error::MissingStartXref)),
                "{}",
                observed()
            );
        }
        "invalid_startxref" => {
            assert!(
                is(|e| matches!(e, Error::InvalidStartXref(_))),
                "{}",
                observed()
            );
        }
        "startxref_out_of_bounds" => {
            assert!(
                is(|e| matches!(e, Error::StartXrefOutOfBounds { .. })),
                "{}",
                observed()
            );
        }
        "malformed_xref" => {
            assert!(
                is(|e| matches!(e, Error::MalformedXref(_))),
                "{}",
                observed()
            );
        }
        "unexpected_token" => {
            assert!(
                is(|e| matches!(e, Error::UnexpectedToken(_))),
                "{}",
                observed()
            );
        }
        "unexpected_eof" => {
            assert!(is(|e| matches!(e, Error::UnexpectedEof)), "{}", observed());
        }
        "nesting_too_deep" => {
            assert!(
                is(|e| matches!(e, Error::NestingTooDeep(_))),
                "{}",
                observed()
            );
        }
        "malformed_page_tree" => {
            assert!(
                is(|e| matches!(e, Error::MalformedPageTree(_))),
                "{}",
                observed()
            );
        }
        "unsupported_state" => {
            assert!(
                is(|e| matches!(e, Error::UnsupportedState(_))),
                "{}",
                observed()
            );
        }
        "free_object" => {
            assert!(is(|e| matches!(e, Error::FreeObject(_))), "{}", observed());
        }
        other => panic!("{} uses unknown category {other:?}", entry.name),
    }
}

#[test]
fn manifest_categories_and_probes_are_well_formed() {
    const CATEGORIES: &[&str] = &[
        "ok",
        "invalid_header",
        "missing_startxref",
        "invalid_startxref",
        "startxref_out_of_bounds",
        "malformed_xref",
        "unexpected_token",
        "unexpected_eof",
        "nesting_too_deep",
        "malformed_page_tree",
        "unsupported_state",
        "free_object",
    ];
    for entry in committed_manifest() {
        assert!(!entry.name.is_empty(), "manifest entry with empty name");
        assert!(
            !entry.intent.is_empty(),
            "{} has no intent text",
            entry.name
        );
        assert!(
            CATEGORIES.contains(&entry.category.as_str()),
            "{} has unknown category {:?}",
            entry.name,
            entry.category
        );
        assert!(
            Probe::parse(&entry.probe).is_some(),
            "{} has unknown probe {:?}",
            entry.name,
            entry.probe
        );
    }
}

#[test]
fn malformed_corpus_has_expected_file_count() {
    let entries = committed_manifest();
    assert_eq!(entries.len(), MALFORMED_COUNT);
    assert!((15..=25).contains(&entries.len()));
    let files: Vec<PathBuf> = fs::read_dir(corpus_dir())
        .expect("read corpus dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("pdf"))
        .collect();
    assert_eq!(
        files.len(),
        entries.len(),
        "manifest and corpus file count disagree"
    );
    for entry in &entries {
        assert!(
            corpus_dir().join(&entry.name).is_file(),
            "{} is missing",
            entry.name
        );
    }
}

#[test]
fn every_malformed_file_is_within_the_size_budget() {
    let mut total = 0u64;
    for entry in committed_manifest() {
        let path = corpus_dir().join(&entry.name);
        let metadata =
            fs::metadata(&path).unwrap_or_else(|e| panic!("stat {}: {e}", path.display()));
        assert!(
            metadata.len() < MALFORMED_FILE_MAX_BYTES as u64,
            "{} is {} bytes, per-file limit is {}",
            entry.name,
            metadata.len(),
            MALFORMED_FILE_MAX_BYTES
        );
        total += metadata.len();
    }
    assert!(
        total < MALFORMED_CORPUS_SIZE_BUDGET as u64,
        "malformed corpus is {total} bytes, budget is {MALFORMED_CORPUS_SIZE_BUDGET}"
    );
}

#[test]
fn every_malformed_file_matches_its_manifest_hash() {
    for entry in committed_manifest() {
        let bytes = read_fixture(&entry);
        assert_eq!(entry.sha256.len(), 64, "hash length for {}", entry.name);
        assert!(
            entry
                .sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "hash must be lower-case hex for {}",
            entry.name
        );
        assert_eq!(
            sha256_hex(&bytes),
            entry.sha256,
            "SHA-256 mismatch for {}",
            entry.name
        );
    }
}

#[test]
fn no_malformed_file_panics_and_each_declared_probe_matches() {
    for entry in committed_manifest() {
        let bytes = read_fixture(&entry);
        let probe = Probe::parse(&entry.probe)
            .unwrap_or_else(|| panic!("{} has invalid probe", entry.name));

        let outcomes = panic::catch_unwind(AssertUnwindSafe(|| exercise(bytes)))
            .unwrap_or_else(|_| panic!("{} panicked during parse, read, or save", entry.name));

        assert!(
            !outcomes.save_unreparseable,
            "{}: save produced a document that does not reparse",
            entry.name
        );
        if probe != Probe::Open {
            assert!(
                outcomes.open.is_none(),
                "{}: declared probe {:?} but from_bytes failed: {:?}",
                entry.name,
                probe,
                outcomes.open
            );
        }
        assert_category(&entry, outcomes.for_probe(probe));
    }
}

#[test]
fn save_on_parsed_documents_never_panics() {
    let mut reached_save = 0usize;
    for entry in committed_manifest() {
        let bytes = read_fixture(&entry);
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            let Ok(document) = Document::from_bytes(bytes) else {
                return None;
            };
            match document.save() {
                Err(_) => Some(true),
                Ok(saved) => Some(Document::from_bytes(saved).is_ok()),
            }
        }))
        .unwrap_or_else(|_| panic!("{} panicked during the save second pass", entry.name));

        if let Some(reparses) = result {
            reached_save += 1;
            assert!(reparses, "{}: save output does not reparse", entry.name);
        }
    }
    assert!(
        reached_save > 0,
        "expected at least one recoverable malformed document to reach save()"
    );
}
