//! Definitions for the small PDF fixtures committed under `fixtures/`.
//!
//! [`all`] is the single source of truth used by both the `generate_fixtures`
//! example (which writes the files and the manifest) and the integration test
//! (which verifies their hashes and structure on disk). Every fixture is
//! produced by one of the in-memory builders in [`crate`]; nothing here opens a
//! network connection or reads a real document.
//!
//! The M1 corpus contains exactly [`FIXTURE_COUNT`] fixtures. It spans the page
//! counts `1/2/3/10/100` alongside edge counts (`0` and `4..=9`), five named
//! page-size families (Letter, A4, landscape, tiny, large), and several text
//! variants (plain ASCII, punctuation, long strings, and the distinct per-page
//! text emitted by [`crate::multipage_pdf`]).

use crate::{minimal_pdf, multipage_pdf, pdf_with_page_size, text_pdf};

/// Text drawn by the `text.pdf` committed fixture (kept stable from T-0016).
pub const TEXT_FIXTURE_TEXT: &str = "PDFXI golden fixture text";

/// Expected number of fixtures in the M1 corpus.
pub const FIXTURE_COUNT: usize = 50;

/// Total committed corpus byte budget (200 KiB).
pub const CORPUS_SIZE_BUDGET: usize = 200 * 1024;

/// Text used by [`crate::minimal_pdf`] and [`crate::pdf_with_page_size`].
const DEFAULT_TEXT: &str = "Hello, PDFXI!";

/// A deliberately long, ASCII-only string used by `text-long.pdf`.
const LONG_TEXT: &str = "PDFXI long text fixture. The quick brown fox jumps over the lazy dog. \
     0123456789 ABCDEFGHIJKLMNOPQRSTUVWXYZ abcdefghijklmnopqrstuvwxyz. \
     This string is intentionally repeated to exercise longer uncompressed content streams \
     while keeping the committed fixture well under the corpus size budget.";

/// Selects which in-memory builder produces a fixture's bytes.
#[derive(Debug, Clone, PartialEq)]
pub enum FixtureKind {
    /// [`crate::minimal_pdf`] — the canonical single-page baseline.
    Minimal,
    /// [`crate::text_pdf`] with the supplied literal text.
    Text(String),
    /// [`crate::multipage_pdf`] with the supplied page count.
    Multipage(usize),
    /// [`crate::pdf_with_page_size`] with the supplied media box.
    PageSize {
        /// Media-box width in PDF points.
        width: f64,
        /// Media-box height in PDF points.
        height: f64,
    },
}

/// A committed fixture: its file name, originating builder expression, the
/// builder parameters, expected page count, and the text markers that must
/// appear in its content streams.
#[derive(Debug, Clone, PartialEq)]
pub struct CommittedFixture {
    /// File name relative to the `fixtures/` directory.
    pub file_name: String,
    /// Human-readable builder expression, mirrored in `fixtures/manifest.toml`.
    pub builder: String,
    /// The builder parameters, used to regenerate the bytes.
    pub kind: FixtureKind,
    /// Expected number of `/Page` objects.
    pub pages: usize,
    /// Text fragments that must be present in the PDF bytes.
    pub text_markers: Vec<String>,
}

impl CommittedFixture {
    /// Regenerate this fixture's bytes from its [`FixtureKind`].
    pub fn build(&self) -> Vec<u8> {
        build(self)
    }
}

/// Quote `s` as a TOML/Rust basic string, escaping `\`, `"`, and newlines.
///
/// The integration test's manifest parser applies the exact inverse unescaping,
/// so manifest round-trips are byte-stable.
pub fn toml_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// Format an f64 the same way the builders do (integral values lose `.0`).
fn num(value: f64) -> String {
    if value.fract() == 0.0 {
        (value as i64).to_string()
    } else {
        value.to_string()
    }
}

/// Build a descriptor, deriving its builder expression, page count, and markers
/// from a single [`FixtureKind`] so the three can never drift apart.
fn make(file_name: &str, kind: FixtureKind) -> CommittedFixture {
    let builder = match &kind {
        FixtureKind::Minimal => "minimal_pdf()".to_string(),
        FixtureKind::Text(text) => format!("text_pdf({})", toml_quote(text)),
        FixtureKind::Multipage(pages) => format!("multipage_pdf({pages})"),
        FixtureKind::PageSize { width, height } => {
            format!("pdf_with_page_size({}, {})", num(*width), num(*height))
        }
    };

    let pages = match &kind {
        FixtureKind::Multipage(pages) => *pages,
        _ => 1,
    };

    let text_markers: Vec<String> = match &kind {
        FixtureKind::Minimal | FixtureKind::PageSize { .. } => vec![DEFAULT_TEXT.to_string()],
        FixtureKind::Text(text) => vec![text.clone()],
        FixtureKind::Multipage(count) => {
            if *count == 0 {
                Vec::new()
            } else if *count <= 10 {
                (1..=*count)
                    .map(|page| format!("Page {page} of {count}"))
                    .collect()
            } else {
                // Keep the manifest readable for large documents while still
                // bracketing the first and last distinct page texts.
                vec![
                    format!("Page 1 of {count}"),
                    format!("Page {count} of {count}"),
                ]
            }
        }
    };

    CommittedFixture {
        file_name: file_name.to_string(),
        builder,
        kind,
        pages,
        text_markers,
    }
}

/// All committed M1 fixtures, in manifest order.
///
/// The first three entries (`minimal.pdf`, `text.pdf`, `multipage-3.pdf`) keep
/// the names, builders, and bytes introduced in T-0016.
pub fn all() -> Vec<CommittedFixture> {
    vec![
        // Baseline builders (names stable from T-0016).
        make("minimal.pdf", FixtureKind::Minimal),
        make("text.pdf", FixtureKind::Text(TEXT_FIXTURE_TEXT.to_string())),
        make("multipage-3.pdf", FixtureKind::Multipage(3)),
        // Text variants: ASCII, punctuation, long strings, symbols, digits.
        make(
            "text-hello.pdf",
            FixtureKind::Text("Hello from PDFXI".to_string()),
        ),
        make(
            "text-ascii-short.pdf",
            FixtureKind::Text("ABC 123".to_string()),
        ),
        make(
            "text-words.pdf",
            FixtureKind::Text("The quick brown fox jumps over the lazy dog".to_string()),
        ),
        make(
            "text-punctuation.pdf",
            FixtureKind::Text("Hello, world! Is this OK? Yes; 42% & more...".to_string()),
        ),
        make("text-long.pdf", FixtureKind::Text(LONG_TEXT.to_string())),
        make(
            "text-digits.pdf",
            FixtureKind::Text("0123456789 9876543210 1234567890".to_string()),
        ),
        make(
            "text-mixed-case.pdf",
            FixtureKind::Text("The Quick Brown Fox Jumps Over The Lazy Dog".to_string()),
        ),
        make(
            "text-symbols.pdf",
            FixtureKind::Text("#42 @ home 100% $5 + more = fun".to_string()),
        ),
        make(
            "text-pathlike.pdf",
            FixtureKind::Text("C:/PDFXI/fixtures/report.txt".to_string()),
        ),
        make(
            "text-numbers.pdf",
            FixtureKind::Text("3.14159 -42 +17 0.001 1e10".to_string()),
        ),
        make(
            "text-greeting.pdf",
            FixtureKind::Text("Welcome to PDFXI!".to_string()),
        ),
        make(
            "text-lorem.pdf",
            FixtureKind::Text(
                "Lorem ipsum dolor sit amet, consectetur adipiscing elit.".to_string(),
            ),
        ),
        make(
            "text-uppercase.pdf",
            FixtureKind::Text("PDFXI UPPERCASE TEXT FIXTURE".to_string()),
        ),
        make(
            "text-lowercase.pdf",
            FixtureKind::Text("pdfxi lowercase text fixture".to_string()),
        ),
        make(
            "text-escape-safe.pdf",
            FixtureKind::Text("Brackets [and] braces {and} are safe".to_string()),
        ),
        make(
            "text-hyphen.pdf",
            FixtureKind::Text("well-known state-of-the-art PDFXI fixture".to_string()),
        ),
        make(
            "text-underscore.pdf",
            FixtureKind::Text("snake_case_text_with_underscores_42".to_string()),
        ),
        make(
            "text-equals.pdf",
            FixtureKind::Text("key=value; a+b=c; 1<2 && 3>2".to_string()),
        ),
        make(
            "text-periods.pdf",
            FixtureKind::Text("First. Second. Third. Fourth. Fifth.".to_string()),
        ),
        // Page-size variants: Letter, A4, landscape, tiny, large, and friends.
        make(
            "size-letter.pdf",
            FixtureKind::PageSize {
                width: 612.0,
                height: 792.0,
            },
        ),
        make(
            "size-letter-landscape.pdf",
            FixtureKind::PageSize {
                width: 792.0,
                height: 612.0,
            },
        ),
        make(
            "size-a4.pdf",
            FixtureKind::PageSize {
                width: 595.28,
                height: 841.89,
            },
        ),
        make(
            "size-a4-integer.pdf",
            FixtureKind::PageSize {
                width: 595.0,
                height: 842.0,
            },
        ),
        make(
            "size-a4-landscape.pdf",
            FixtureKind::PageSize {
                width: 841.89,
                height: 595.28,
            },
        ),
        make(
            "size-a5.pdf",
            FixtureKind::PageSize {
                width: 420.0,
                height: 595.0,
            },
        ),
        make(
            "size-a3.pdf",
            FixtureKind::PageSize {
                width: 842.0,
                height: 1191.0,
            },
        ),
        make(
            "size-legal.pdf",
            FixtureKind::PageSize {
                width: 612.0,
                height: 1008.0,
            },
        ),
        make(
            "size-tabloid.pdf",
            FixtureKind::PageSize {
                width: 792.0,
                height: 1224.0,
            },
        ),
        make(
            "size-tiny.pdf",
            FixtureKind::PageSize {
                width: 72.0,
                height: 72.0,
            },
        ),
        make(
            "size-square.pdf",
            FixtureKind::PageSize {
                width: 500.0,
                height: 500.0,
            },
        ),
        make(
            "size-large.pdf",
            FixtureKind::PageSize {
                width: 2000.0,
                height: 3000.0,
            },
        ),
        make(
            "size-b5.pdf",
            FixtureKind::PageSize {
                width: 499.0,
                height: 709.0,
            },
        ),
        make(
            "size-executive.pdf",
            FixtureKind::PageSize {
                width: 522.0,
                height: 756.0,
            },
        ),
        make(
            "size-ledger.pdf",
            FixtureKind::PageSize {
                width: 1224.0,
                height: 792.0,
            },
        ),
        make(
            "size-poster.pdf",
            FixtureKind::PageSize {
                width: 3000.0,
                height: 4000.0,
            },
        ),
        make(
            "size-micro.pdf",
            FixtureKind::PageSize {
                width: 10.0,
                height: 10.0,
            },
        ),
        // Page-count variants, including the required 1/2/3/10/100 set.
        make("multipage-0.pdf", FixtureKind::Multipage(0)),
        make("multipage-1.pdf", FixtureKind::Multipage(1)),
        make("multipage-2.pdf", FixtureKind::Multipage(2)),
        make("multipage-4.pdf", FixtureKind::Multipage(4)),
        make("multipage-5.pdf", FixtureKind::Multipage(5)),
        make("multipage-6.pdf", FixtureKind::Multipage(6)),
        make("multipage-7.pdf", FixtureKind::Multipage(7)),
        make("multipage-8.pdf", FixtureKind::Multipage(8)),
        make("multipage-9.pdf", FixtureKind::Multipage(9)),
        make("multipage-10.pdf", FixtureKind::Multipage(10)),
        make("multipage-100.pdf", FixtureKind::Multipage(100)),
    ]
}

/// Build the bytes for a fixture descriptor.
pub fn build(fixture: &CommittedFixture) -> Vec<u8> {
    match &fixture.kind {
        FixtureKind::Minimal => minimal_pdf(),
        FixtureKind::Text(text) => text_pdf(text),
        FixtureKind::Multipage(pages) => multipage_pdf(*pages),
        FixtureKind::PageSize { width, height } => pdf_with_page_size(*width, *height),
    }
}

/// Look up a committed fixture by file name.
pub fn by_name(name: &str) -> Option<CommittedFixture> {
    all().into_iter().find(|fixture| fixture.file_name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_has_expected_fixture_count() {
        assert_eq!(all().len(), FIXTURE_COUNT);
    }

    #[test]
    fn every_fixture_has_a_valid_header_and_eof() {
        for fixture in all() {
            let bytes = fixture.build();
            assert!(bytes.starts_with(b"%PDF-1.7"), "{}", fixture.file_name);
            assert!(bytes.ends_with(b"%%EOF\n"), "{}", fixture.file_name);
            assert!(!bytes.is_empty(), "{}", fixture.file_name);
        }
    }

    #[test]
    fn fixture_names_are_unique() {
        let mut names: Vec<String> = all().into_iter().map(|f| f.file_name).collect();
        names.sort();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before);
        assert_eq!(before, FIXTURE_COUNT);
    }

    #[test]
    fn required_page_counts_are_present() {
        let pages: Vec<usize> = all().into_iter().map(|f| f.pages).collect();
        for required in [1, 2, 3, 10, 100] {
            assert!(pages.contains(&required), "missing page count {required}");
        }
        assert!(pages.contains(&0), "missing zero-page structural edge case");
    }

    #[test]
    fn by_name_finds_stable_fixtures() {
        for name in ["minimal.pdf", "text.pdf", "multipage-3.pdf"] {
            let found = by_name(name).unwrap_or_else(|| panic!("missing {name}"));
            assert_eq!(found.file_name, name);
        }
        assert!(by_name("does-not-exist.pdf").is_none());
    }

    #[test]
    fn total_generated_corpus_fits_the_budget() {
        let total: usize = all().iter().map(|f| f.build().len()).sum();
        assert!(total < CORPUS_SIZE_BUDGET, "corpus is {total} bytes");
    }

    #[test]
    fn builders_are_byte_identical_across_runs() {
        for fixture in all() {
            assert_eq!(
                fixture.build(),
                fixture.build(),
                "{} is not byte-stable",
                fixture.file_name
            );
        }
    }
}
