//! `pdfxi replace`: literal find-and-replace across a document.
//!
//! [`pdfxi_edit::find_replace`] is a model over extracted text: it finds matches
//! and produces a non-overlapping plan, but never reads or rewrites content
//! streams. This host closes that boundary. It locates matches in the page text
//! model, maps a match to the single text run that contains it, rewrites that
//! run through [`pdfxi_edit::replace_run_text`], saves the document and reopens
//! it to report how many matches remain.
//!
//! Only matches contained inside one text run are replaced. A match that spans
//! two runs (for example across a line break or a style change) is left
//! untouched and still counted in `remaining`; rewriting text across runs would
//! require block-level reflow semantics this host does not claim.
//!
//! Matching is literal and bounded by [`pdfxi_edit::SearchLimits`]. One
//! replacement is applied per block per pass, because replacing a run reflows
//! its whole block and invalidates that block's text model. Passes and total
//! replacements are capped, so pathological inputs (such as a replacement that
//! contains the query) stop with `limit_reached: true` instead of looping.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_core::Document;
use pdfxi_edit::{
    SearchDocument, SearchLimits, SearchOptions, SearchPage, TextBlock, find_all, replace_run_text,
    text_blocks,
};
use serde::Serialize;

/// Default maximum replacements.
const DEFAULT_LIMIT: usize = 1_000;

/// One `pdfxi replace` request.
pub(crate) struct ReplaceRequest<'a> {
    /// Input file.
    pub file: &'a Path,
    /// Output file.
    pub out: &'a Path,
    /// Literal text to find.
    pub find: &'a str,
    /// Replacement text.
    pub replacement: &'a str,
    /// Matching flags.
    pub options: SearchOptions,
    /// Maximum replacements; zero means [`DEFAULT_LIMIT`].
    pub limit: usize,
    /// Emit JSON.
    pub json: bool,
}

/// A run edit selected from one block.
struct BlockEdit {
    /// Run to rewrite.
    run_index: usize,
    /// New run text.
    new_text: String,
}

/// One accepted replacement.
#[derive(Serialize)]
struct ReplaceRow {
    /// One-based page.
    page: usize,
    /// Block index.
    block: usize,
    /// Run index.
    run: usize,
}

/// JSON shape of `pdfxi replace`.
#[derive(Serialize)]
struct ReplaceReport {
    /// Input file.
    file: String,
    /// Written file.
    out: String,
    /// Query.
    find: String,
    /// Replacement.
    replacement: String,
    /// Whether matching was case-sensitive.
    case_sensitive: bool,
    /// Whether matching required word boundaries.
    whole_word: bool,
    /// Whether diacritics were folded.
    diacritic_insensitive: bool,
    /// Pages in the document.
    pages: usize,
    /// Matches found before editing.
    initial_matches: usize,
    /// Replacements applied.
    replaced: usize,
    /// Matches still present after reopening the saved file.
    remaining: usize,
    /// Replacement cap.
    limit: usize,
    /// Whether the replacement cap stopped the run.
    limit_reached: bool,
    /// Blocks that needed the embedded fallback font.
    embedded_font_blocks: usize,
    /// Every accepted replacement, in application order.
    rows: Vec<ReplaceRow>,
    /// Bytes written.
    bytes: usize,
}

/// Build the match options with the caller's flags.
fn search_options(request: &ReplaceRequest<'_>) -> SearchOptions {
    SearchOptions {
        case_sensitive: request.options.case_sensitive,
        whole_word: request.options.whole_word,
        diacritic_insensitive: request.options.diacritic_insensitive,
    }
}

/// Extracted lines of every page, as the search model sees them.
fn document_search(document: &Document) -> anyhow::Result<SearchDocument> {
    let count = document
        .page_count()
        .map_err(|error| anyhow::anyhow!("count pages: {error}"))?;
    let mut pages = Vec::with_capacity(count);
    for page in 0..count {
        let text = document
            .page_text(page)
            .map_err(|error| anyhow::anyhow!("extract page {} text: {error}", page + 1))?;
        pages.push(SearchPage {
            page,
            lines: text.lines().map(str::to_owned).collect(),
        });
    }
    SearchDocument::new(pages).map_err(|error| anyhow::anyhow!("search document: {error}"))
}

/// Select the first replacement inside `block`, if one can be expressed.
///
/// A hit is accepted only when both offsets lie inside one run. Whole-run
/// deletions are not expressible through `replace_run_text` (the resulting run
/// would be empty), so those hits are skipped and stay counted as remaining.
fn first_edit(
    block: &TextBlock,
    query: &str,
    options: &SearchOptions,
    replacement: &str,
) -> Option<BlockEdit> {
    for line in &block.lines {
        let mut text = String::new();
        let mut spans: Vec<(usize, usize, usize)> = Vec::new();
        for run_index in &line.run_indices {
            let Some(run) = block.run(*run_index) else {
                continue;
            };
            let start = text.chars().count();
            text.push_str(&run.text);
            spans.push((run.index, start, run.text.chars().count()));
        }
        if text.is_empty() {
            continue;
        }
        let search = SearchDocument::new(vec![SearchPage {
            page: block.page_index,
            lines: vec![text.clone()],
        }])
        .ok()?;
        let hits = find_all(&search, query, options, &SearchLimits::default()).ok()?;
        for hit in hits {
            let Some((run_index, run_start, run_len)) = spans
                .iter()
                .copied()
                .find(|(_, start, len)| hit.start >= *start && hit.end <= start + len)
            else {
                continue;
            };
            let run = block.run(run_index)?;
            let characters: Vec<char> = run.text.chars().collect();
            let local_start = hit.start - run_start;
            let local_end = hit.end - run_start;
            if local_end > characters.len() || local_start > local_end {
                continue;
            }
            let mut new_text: String = String::new();
            new_text.extend(characters[..local_start].iter());
            new_text.push_str(replacement);
            new_text.extend(characters[local_end..].iter());
            if new_text.trim().is_empty() {
                continue;
            }
            debug_assert!(run_len <= characters.len());
            return Some(BlockEdit {
                run_index,
                new_text,
            });
        }
    }
    None
}

/// `pdfxi replace`.
pub(crate) fn run_replace(request: ReplaceRequest<'_>) -> anyhow::Result<()> {
    if request.find.is_empty() {
        bail!("--find must not be empty");
    }
    if request.replacement.chars().count() > 4096 {
        bail!("--replace is longer than the 4096-character limit");
    }
    let options = search_options(&request);
    let limit = if request.limit == 0 {
        DEFAULT_LIMIT
    } else {
        request.limit
    };
    let bytes =
        std::fs::read(request.file).with_context(|| format!("read {}", request.file.display()))?;
    let mut document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", request.file.display()))?;
    let page_count = document
        .page_count()
        .map_err(|error| anyhow::anyhow!("count pages: {error}"))?;
    if page_count == 0 {
        bail!("{} has no pages", request.file.display());
    }
    let source_text = document_search(&document)?;
    let initial_matches = find_all(
        &source_text,
        request.find,
        &options,
        &SearchLimits::default(),
    )
    .map_err(|error| anyhow::anyhow!("find text: {error}"))?
    .len();

    let mut rows = Vec::new();
    let mut replaced = 0usize;
    let mut embedded_font_blocks = 0usize;
    let mut limit_reached = false;
    loop {
        if replaced >= limit {
            limit_reached = true;
            break;
        }
        let mut found: Option<(usize, usize, BlockEdit)> = None;
        'scan: for page in 0..page_count {
            let model = text_blocks(&document, page)
                .map_err(|error| anyhow::anyhow!("read page {} text: {error}", page + 1))?;
            for block in &model.blocks {
                if let Some(edit) = first_edit(block, request.find, &options, request.replacement) {
                    found = Some((page, block.index, edit));
                    break 'scan;
                }
            }
        }
        let Some((page, block_index, edit)) = found else {
            break;
        };
        let outcome = replace_run_text(&mut document, page, edit.run_index, &edit.new_text)
            .map_err(|error| {
                anyhow::anyhow!(
                    "replace run {} on page {}: {error}",
                    edit.run_index,
                    page + 1
                )
            })?;
        if outcome.font.embedded {
            embedded_font_blocks += 1;
        }
        replaced += 1;
        rows.push(ReplaceRow {
            page: page + 1,
            block: block_index,
            run: edit.run_index,
        });
        // `replace_run_text` stages the rebuilt page as a new content stream, so
        // the in-memory document still reports the pre-edit text. Reparsing the
        // written bytes makes the next scan see the edit instead of repeatedly
        // matching the same stale run. A full save is used rather than an
        // incremental one so a long replacement run cannot exhaust the core
        // reader's bounded xref-section chain.
        let staged = document
            .save()
            .map_err(|error| anyhow::anyhow!("stage replacement: {error}"))?;
        document = Document::from_bytes(staged)
            .map_err(|error| anyhow::anyhow!("reload after replace: {error}"))?;
    }

    if replaced == 0 && initial_matches > 0 {
        // Every match spans runs or would delete a whole run. Saving unchanged
        // bytes would be misleading, so say so instead of writing a new file.
        bail!(
            "found {initial_matches} match(es) but none is contained in a single text run; nothing was replaced"
        );
    }

    let saved = document
        .save()
        .with_context(|| format!("save {}", request.out.display()))?;
    std::fs::write(request.out, &saved)
        .with_context(|| format!("write {}", request.out.display()))?;
    let reopened = Document::from_bytes(saved.clone())
        .map_err(|error| anyhow::anyhow!("reopen {}: {error}", request.out.display()))?;
    let reopened_search = document_search(&reopened)?;
    let remaining = find_all(
        &reopened_search,
        request.find,
        &options,
        &SearchLimits::default(),
    )
    .map_err(|error| anyhow::anyhow!("find text after replace: {error}"))?
    .len();
    if replaced > 0 && !request.replacement.contains(request.find) && remaining >= initial_matches {
        bail!(
            "replaced {replaced} match(es) but {remaining} of {initial_matches} still extract; the edit did not reach the page text"
        );
    }

    let report = ReplaceReport {
        file: request.file.display().to_string(),
        out: request.out.display().to_string(),
        find: request.find.to_owned(),
        replacement: request.replacement.to_owned(),
        case_sensitive: options.case_sensitive,
        whole_word: options.whole_word,
        diacritic_insensitive: options.diacritic_insensitive,
        pages: page_count,
        initial_matches,
        replaced,
        remaining,
        limit,
        limit_reached,
        embedded_font_blocks,
        rows,
        bytes: saved.len(),
    };
    if request.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("file: {}", report.file);
    println!("out:  {}", report.out);
    println!(
        "find: {:?}  replace: {:?}  case_sensitive: {}  whole_word: {}  diacritics: {}",
        report.find,
        report.replacement,
        report.case_sensitive,
        report.whole_word,
        report.diacritic_insensitive
    );
    println!(
        "matches: {} initial, {} replaced, {} remaining",
        report.initial_matches, report.replaced, report.remaining
    );
    println!(
        "limit: {}{}",
        report.limit,
        if report.limit_reached {
            " (reached)"
        } else {
            ""
        }
    );
    println!("embedded_font_blocks: {}", report.embedded_font_blocks);
    println!("bytes: {}", report.bytes);
    Ok(())
}
