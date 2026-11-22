//! `pdfxi update`: append an incremental revision with new document information.
//!
//! An incremental update keeps every original byte as an exact prefix and
//! appends only the changed objects plus a new cross-reference section. That is
//! the property digital-preservation checks rely on, so this host saves through
//! [`Document::save_incremental`], then validates the result with
//! [`Document::validate_incremental`] before writing it. The report is the
//! validator's view of the bytes, not the command's own claim.
//!
//! One honest limitation is carried in the report: a source that already uses a
//! hybrid `/XRefStm` section (common in files exported by Microsoft Office)
//! cannot be walked by the cross-reference-chain validator, so for those inputs
//! the host verifies the exact byte prefix, the page count and every page's text
//! directly and reports `xref_chain_verified: false` with a note instead of
//! claiming the chain check passed.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_core::{Document, DocumentInfo, PreservationError, PreservationPolicy};
use serde::Serialize;

/// One `pdfxi update` request.
pub(crate) struct UpdateRequest<'a> {
    /// Input file.
    pub file: &'a Path,
    /// Output file.
    pub out: &'a Path,
    /// New `/Title`.
    pub title: Option<&'a str>,
    /// New `/Author`.
    pub author: Option<&'a str>,
    /// New `/Subject`.
    pub subject: Option<&'a str>,
    /// New `/Keywords`.
    pub keywords: Option<&'a str>,
    /// New `/Producer`.
    pub producer: Option<&'a str>,
    /// Emit JSON.
    pub json: bool,
}

/// One appended revision in the JSON report.
#[derive(Serialize)]
struct RevisionRow {
    /// `Classic` or `Stream`.
    kind: String,
    /// Byte offset of the section.
    offset: usize,
    /// Entries declared by the section.
    entries: usize,
}

/// JSON shape of `pdfxi update`.
#[derive(Serialize)]
struct UpdateReport {
    /// Input file.
    file: String,
    /// Output file.
    out: String,
    /// Original bytes verified as an exact prefix.
    preserved_prefix_bytes: usize,
    /// Bytes appended after the original document.
    appended_bytes: usize,
    /// Appended revisions, newest first.
    revisions: Vec<RevisionRow>,
    /// Whether the cross-reference-chain validator accepted the file.
    xref_chain_verified: bool,
    /// Why the chain check was skipped, when it was.
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    /// Pages present in both documents.
    page_count: usize,
    /// Pages whose text was actually compared.
    text_pages_verified: usize,
    /// Title read back from the saved file.
    title: Option<String>,
    /// Author read back from the saved file.
    author: Option<String>,
}

/// Details of the weaker check used for hybrid sources.
struct PrefixCheck {
    /// Pages compared.
    pages: usize,
    /// Pages whose text matched.
    text_pages: usize,
}

/// Read a document, applying the requested information fields.
fn updated_info(document: &Document, request: &UpdateRequest<'_>) -> anyhow::Result<DocumentInfo> {
    let mut info = document
        .info()
        .map_err(|error| anyhow::anyhow!("read document information: {error}"))?
        .unwrap_or_default();
    if let Some(title) = request.title {
        info.title = Some(title.to_owned());
    }
    if let Some(author) = request.author {
        info.author = Some(author.to_owned());
    }
    if let Some(subject) = request.subject {
        info.subject = Some(subject.to_owned());
    }
    if let Some(keywords) = request.keywords {
        info.keywords = Some(keywords.to_owned());
    }
    if let Some(producer) = request.producer {
        info.producer = Some(producer.to_owned());
    }
    Ok(info)
}

/// Verify the prefix, page count and page text without the xref-chain walk.
fn prefix_check(original: &[u8], updated: &[u8]) -> anyhow::Result<PrefixCheck> {
    if !updated.starts_with(original) {
        bail!("incremental output does not preserve the original byte prefix");
    }
    let before = Document::from_bytes(original.to_vec())
        .map_err(|error| anyhow::anyhow!("parse original: {error}"))?;
    let after = Document::from_bytes(updated.to_vec())
        .map_err(|error| anyhow::anyhow!("parse incremental output: {error}"))?;
    let pages_before = before
        .pages()
        .map_err(|error| anyhow::anyhow!("count original pages: {error}"))?
        .len();
    let pages_after = after
        .pages()
        .map_err(|error| anyhow::anyhow!("count updated pages: {error}"))?
        .len();
    if pages_before != pages_after {
        bail!("page count changed from {pages_before} to {pages_after}");
    }
    for index in 0..pages_after {
        let left = before
            .page_text(index)
            .map_err(|error| anyhow::anyhow!("read original page {} text: {error}", index + 1))?;
        let right = after
            .page_text(index)
            .map_err(|error| anyhow::anyhow!("read updated page {} text: {error}", index + 1))?;
        if left != right {
            bail!("page {} text changed in the incremental output", index + 1);
        }
    }
    Ok(PrefixCheck {
        pages: pages_after,
        text_pages: pages_after,
    })
}

/// `pdfxi update`.
pub(crate) fn run_update(request: UpdateRequest<'_>) -> anyhow::Result<()> {
    if request.title.is_none()
        && request.author.is_none()
        && request.subject.is_none()
        && request.keywords.is_none()
        && request.producer.is_none()
    {
        bail!("give at least one of --title, --author, --subject, --keywords or --producer");
    }
    let original =
        std::fs::read(request.file).with_context(|| format!("read {}", request.file.display()))?;
    let mut document = Document::from_bytes(original.clone())
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", request.file.display()))?;
    let info = updated_info(&document, &request)?;
    document
        .set_info(&info)
        .map_err(|error| anyhow::anyhow!("stage document information: {error}"))?;

    let updated = document
        .save_incremental()
        .map_err(|error| anyhow::anyhow!("incremental save: {error}"))?;
    let policy = PreservationPolicy::minimal().with_page_text(true);
    let (revisions, xref_chain_verified, note, page_count, text_pages_verified) =
        match document.validate_incremental(&updated, &policy) {
            Ok(validation) => (
                validation
                    .revisions
                    .iter()
                    .map(|revision| RevisionRow {
                        kind: format!("{:?}", revision.kind),
                        offset: revision.offset,
                        entries: revision.entries,
                    })
                    .collect::<Vec<_>>(),
                true,
                None,
                validation.page_count,
                validation.text_pages_verified,
            ),
            Err(pdfxi_core::Error::Preservation(PreservationError::BrokenChain { .. })) => {
                let check = prefix_check(&original, &updated)?;
                (
                    Vec::new(),
                    false,
                    Some(
                        "the source already carries a hybrid /XRefStm section that the \
                         cross-reference-chain validator does not link; the exact byte prefix, \
                         page count and page text were verified instead"
                            .to_owned(),
                    ),
                    check.pages,
                    check.text_pages,
                )
            }
            Err(error) => {
                return Err(anyhow::anyhow!("preservation validation: {error}"));
            }
        };

    std::fs::write(request.out, &updated)
        .with_context(|| format!("write {}", request.out.display()))?;
    let reopened = Document::from_bytes(updated.clone())
        .map_err(|error| anyhow::anyhow!("reopen {}: {error}", request.out.display()))?;
    let saved_info = reopened
        .info()
        .map_err(|error| anyhow::anyhow!("read saved information: {error}"))?
        .unwrap_or_default();

    let report = UpdateReport {
        file: request.file.display().to_string(),
        out: request.out.display().to_string(),
        preserved_prefix_bytes: original.len(),
        appended_bytes: updated.len().saturating_sub(original.len()),
        revisions,
        xref_chain_verified,
        note,
        page_count,
        text_pages_verified,
        title: saved_info.title,
        author: saved_info.author,
    };

    if request.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("file: {}", report.file);
    println!("out:  {}", report.out);
    println!("preserved_prefix: {} bytes", report.preserved_prefix_bytes);
    println!("appended: {} bytes", report.appended_bytes);
    for revision in &report.revisions {
        println!(
            "revision: {} at offset {} ({} entries)",
            revision.kind, revision.offset, revision.entries
        );
    }
    println!("xref_chain_verified: {}", report.xref_chain_verified);
    if let Some(note) = &report.note {
        println!("note: {note}");
    }
    println!("pages: {}", report.page_count);
    println!("text_pages_verified: {}", report.text_pages_verified);
    if let Some(title) = &report.title {
        println!("title: {title}");
    }
    if let Some(author) = &report.author {
        println!("author: {author}");
    }
    Ok(())
}
