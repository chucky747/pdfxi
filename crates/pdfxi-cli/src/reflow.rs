//! `pdfxi reflow`: lay a page's extracted text out as a narrow column.
//!
//! Reflow is the reading half of the accessibility pair; the speaking half is
//! `pdfxi read-aloud` in [`crate::read_aloud`]. Both read the same selected page
//! text through [`selected_pages`], so the column a user reads and the sentences
//! a speech backend is handed come from one extraction.
//!
//! The layout is intentionally simple and deterministic: paragraphs are split on
//! blank lines, a soft line break is just whitespace, and each paragraph is
//! greedily wrapped to at most `--width` characters. A word longer than the
//! width is never split, so the only way a line exceeds the width is that single
//! word.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_core::Document;
use serde::Serialize;

/// Narrowest accepted column width.
pub(crate) const MIN_WIDTH: usize = 20;
/// Widest accepted column width.
pub(crate) const MAX_WIDTH: usize = 200;
/// Default column width.
const DEFAULT_WIDTH: usize = 80;

/// One reflowed page in the JSON report.
#[derive(Serialize)]
struct ReflowPage {
    /// One-based page number.
    page: usize,
    /// Number of paragraphs found on the page.
    paragraphs: usize,
    /// Wrapped lines in reading order.
    lines: Vec<String>,
}

/// JSON shape of `pdfxi reflow --json`.
#[derive(Serialize)]
struct ReflowReport {
    /// File that was inspected.
    file: String,
    /// Column width used for wrapping.
    width: usize,
    /// One row per selected page.
    pages: Vec<ReflowPage>,
    /// Total number of wrapped lines.
    line_count: usize,
}

fn load(file: &Path) -> anyhow::Result<Document> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))
}

/// The requested pages as `(one-based number, extracted text)`.
///
/// `page` is a one-based page number; `None` selects every page. An out-of-range
/// page is a typed error rather than an empty report.
pub(crate) fn selected_pages(
    document: &Document,
    file: &Path,
    page: Option<usize>,
) -> anyhow::Result<Vec<(usize, String)>> {
    let count = document
        .page_count()
        .with_context(|| format!("count pages of {}", file.display()))?;
    let indices: Vec<usize> = match page {
        Some(number) => {
            if number == 0 || number > count {
                bail!(
                    "page {} is out of range: {} has {} page(s)",
                    number,
                    file.display(),
                    count
                );
            }
            vec![number - 1]
        }
        None => (0..count).collect(),
    };
    let mut pages = Vec::with_capacity(indices.len());
    for index in indices {
        let text = document.page_text(index).with_context(|| {
            format!("extract text from page {} of {}", index + 1, file.display())
        })?;
        pages.push((index + 1, text));
    }
    Ok(pages)
}

/// Print the reflowed column for the selected pages.
pub(crate) fn run_reflow(
    file: &Path,
    width: usize,
    page: Option<usize>,
    json: bool,
) -> anyhow::Result<()> {
    if !(MIN_WIDTH..=MAX_WIDTH).contains(&width) {
        bail!("--width must be between {MIN_WIDTH} and {MAX_WIDTH} characters");
    }
    let document = load(file)?;
    let pages = selected_pages(&document, file, page)?;

    let mut report_pages = Vec::with_capacity(pages.len());
    let mut line_count = 0_usize;
    for (number, text) in &pages {
        let paragraphs = segment_paragraphs(text);
        let mut lines = Vec::new();
        for paragraph in &paragraphs {
            lines.extend(wrap_paragraph(paragraph, width));
        }
        line_count += lines.len();
        report_pages.push(ReflowPage {
            page: *number,
            paragraphs: paragraphs.len(),
            lines,
        });
    }

    let report = ReflowReport {
        file: file.display().to_string(),
        width,
        pages: report_pages,
        line_count,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        for page in &report.pages {
            println!("page: {}", page.page);
            for line in &page.lines {
                println!("  {line}");
            }
            println!();
        }
        println!("lines: {}", report.line_count);
    }
    Ok(())
}

/// Split text into paragraphs on blank lines.
///
/// A blank line is a line containing only whitespace; a run of blank lines is
/// one paragraph break. A single newline between non-blank lines is a soft break
/// and becomes a space, matching the reflow model in the desktop shell.
pub(crate) fn segment_paragraphs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !current.is_empty() {
                out.push(current.join(" "));
                current.clear();
            }
        } else {
            current.push(trimmed);
        }
    }
    if !current.is_empty() {
        out.push(current.join(" "));
    }
    out
}

/// Greedily wrap one paragraph to at most `width` characters per line.
///
/// Words are kept whole; a word longer than `width` occupies its own line. An
/// all-whitespace paragraph produces no lines.
pub(crate) fn wrap_paragraph(paragraph: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in paragraph.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
            continue;
        }
        if current.chars().count() + 1 + word.chars().count() <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// Split text into sentences at `.`, `!`, `?` and slide bullets.
///
/// A terminator only ends a sentence when it is followed by whitespace or the
/// end of the text, so `3.14` and `a.m.` stay whole. `•` is always a boundary:
/// the shipped slide fixtures use it as a bullet marker between a title and its
/// sentence, and a speech backend should not read `Slide•The` as one word.
pub(crate) fn split_sentences(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut current = String::new();
    for (index, &character) in chars.iter().enumerate() {
        if character == '•' {
            push_sentence(&mut out, &mut current);
            continue;
        }
        current.push(character);
        if matches!(character, '.' | '!' | '?') {
            let next_is_break = chars.get(index + 1).is_none_or(|next| next.is_whitespace());
            if next_is_break {
                push_sentence(&mut out, &mut current);
            }
        }
    }
    push_sentence(&mut out, &mut current);
    out
}

/// Push the trimmed sentence when it has content, then clear the buffer.
fn push_sentence(out: &mut Vec<String>, current: &mut String) {
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        out.push(trimmed.to_owned());
    }
    current.clear();
}

/// Default column width used when `--width` is not given.
pub(crate) fn default_width() -> usize {
    DEFAULT_WIDTH
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paragraphs_split_on_blank_lines_and_keep_soft_breaks_as_spaces() {
        assert_eq!(
            segment_paragraphs("first line\nsecond line\n\nnext paragraph\n   \nlast"),
            vec!["first line second line", "next paragraph", "last"]
        );
        assert!(segment_paragraphs("  \n\t\n").is_empty());
    }

    #[test]
    fn wrapping_breaks_on_word_boundaries_and_never_splits_a_word() {
        assert_eq!(
            wrap_paragraph("alpha beta gamma delta", 10),
            vec!["alpha beta", "gamma", "delta"]
        );
        assert_eq!(
            wrap_paragraph("supercalifragilistic", 5),
            vec!["supercalifragilistic"]
        );
        assert!(wrap_paragraph("   ", 10).is_empty());
    }

    #[test]
    fn sentences_split_on_terminators_and_slide_bullets() {
        assert_eq!(
            split_sentences("One. Two! Three? Done"),
            vec!["One.", "Two!", "Three?", "Done"]
        );
        assert_eq!(
            split_sentences("Version 3.14 is out. Next"),
            vec!["Version 3.14 is out.", "Next"]
        );
        assert_eq!(
            split_sentences("Lantern Slide•The marker sentence."),
            vec!["Lantern Slide", "The marker sentence."]
        );
        assert!(split_sentences("   ").is_empty());
    }
}
