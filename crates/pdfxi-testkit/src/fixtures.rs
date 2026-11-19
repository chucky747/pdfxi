//! Definitions for the small PDF fixtures committed under `fixtures/`.
//!
//! [`COMMITTED_FIXTURES`] is the single source of truth used by both the
//! `generate_fixtures` example (which writes the files) and the integration
//! test (which verifies their hashes and structure on disk).

use crate::{minimal_pdf, multipage_pdf, text_pdf};

/// Text drawn by the `text.pdf` committed fixture.
pub const TEXT_FIXTURE_TEXT: &str = "PDFXI golden fixture text";

/// A committed fixture: its file name, originating builder, expected page count,
/// and the text fragments that must appear in its content streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommittedFixture {
    /// File name relative to the `fixtures/` directory.
    pub file_name: &'static str,
    /// Human-readable builder expression, mirrored in `fixtures/manifest.toml`.
    pub builder: &'static str,
    /// Expected number of `/Page` objects.
    pub pages: usize,
    /// Text fragments that must be present in the PDF bytes.
    pub text_markers: &'static [&'static str],
}

/// The complete set of committed fixtures for the M1 corpus.
pub const COMMITTED_FIXTURES: &[CommittedFixture] = &[
    CommittedFixture {
        file_name: "minimal.pdf",
        builder: "minimal_pdf()",
        pages: 1,
        text_markers: &["Hello, PDFXI!"],
    },
    CommittedFixture {
        file_name: "text.pdf",
        builder: "text_pdf(\"PDFXI golden fixture text\")",
        pages: 1,
        text_markers: &[TEXT_FIXTURE_TEXT],
    },
    CommittedFixture {
        file_name: "multipage-3.pdf",
        builder: "multipage_pdf(3)",
        pages: 3,
        text_markers: &["Page 1 of 3", "Page 2 of 3", "Page 3 of 3"],
    },
];

/// Build the bytes for a committed fixture by file name.
///
/// Returns `None` for an unknown file name.
pub fn build(file_name: &str) -> Option<Vec<u8>> {
    match file_name {
        "minimal.pdf" => Some(minimal_pdf()),
        "text.pdf" => Some(text_pdf(TEXT_FIXTURE_TEXT)),
        "multipage-3.pdf" => Some(multipage_pdf(3)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_committed_fixture_has_a_builder() {
        for fixture in COMMITTED_FIXTURES {
            let bytes = build(fixture.file_name)
                .unwrap_or_else(|| panic!("no builder for {}", fixture.file_name));
            assert!(bytes.starts_with(b"%PDF-1.7"), "{}", fixture.file_name);
            assert!(bytes.ends_with(b"%%EOF\n"), "{}", fixture.file_name);
        }
    }

    #[test]
    fn unknown_fixture_name_has_no_builder() {
        assert!(build("does-not-exist.pdf").is_none());
    }

    #[test]
    fn fixture_names_are_unique() {
        let mut names: Vec<&str> = COMMITTED_FIXTURES.iter().map(|f| f.file_name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before);
    }
}
