//! `pdfxi accessibility check`: run the accessibility checker and the PDF/UA-1
//! preflight profile over a real PDF.
//!
//! Two independent checks are reported together:
//!
//! 1. `pdfxi-accessibility::checker::check` on an accessibility tree derived
//!    from the document's extracted page text (document root, page nodes and
//!    paragraph nodes). The accessibility crate models a tree; it does not read
//!    an existing `/StructTreeRoot`, so this tree describes the recovered
//!    content and its document metadata rather than the file's own tags.
//! 2. `pdfxi-standards`' PDF/UA-1 preflight profile through the existing
//!    byte-level adapter. This one reads the real catalog, `/MarkInfo`,
//!    `/StructTreeRoot`, language, title, annotations and structure facts.
//!
//! Findings from both are merged into one deterministic report. Nothing here
//! certifies conformance; the report always states that.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_accessibility::{
    A11yNode, A11yTree, BBox, CheckOptions, NodeId, TagRole, build_fix_plan, check, suggest_fixups,
};
use pdfxi_convert::preflight::{PreflightOptions, preflight_bytes};
use pdfxi_core::{Document, Object};
use serde::Serialize;

/// Exit code used when either checker reports an error.
pub(crate) const EXIT_NOT_ACCESSIBLE: i32 = 2;

/// One finding in the merged report.
#[derive(Serialize)]
struct FindingRow {
    /// `tree` for the accessibility checker, `pdfua` for the preflight profile.
    source: &'static str,
    /// Stable rule or finding code.
    code: String,
    /// `error`, `warning` or `info`.
    severity: String,
    /// Human-readable message.
    message: String,
    /// One-based page number, when the finding is page-specific.
    #[serde(skip_serializing_if = "Option::is_none")]
    page: Option<usize>,
    /// Object or catalog location, when the profile reports one.
    #[serde(skip_serializing_if = "Option::is_none")]
    location: Option<String>,
}

/// JSON shape of `pdfxi accessibility check`.
#[derive(Serialize)]
struct AccessibilityReport {
    /// Inspected file.
    file: String,
    /// Title used by the checker (document metadata or `--title`).
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    /// Language used by the checker (catalog `/Lang` or `--lang`).
    #[serde(skip_serializing_if = "Option::is_none")]
    lang: Option<String>,
    /// Pages in the document.
    pages: usize,
    /// Paragraph nodes recovered from page text.
    paragraphs: usize,
    /// Merged findings in canonical order.
    findings: Vec<FindingRow>,
    /// Error-severity count across both checks.
    errors: usize,
    /// Warning-severity count across both checks.
    warnings: usize,
    /// Whether neither check reported an error.
    valid: bool,
    /// Deterministic fix-plan lines for the accessibility findings.
    fix_plan: Vec<String>,
    /// Always `false`: this command never certifies conformance.
    certification_claimed: bool,
    /// Scope statement.
    scope_note: &'static str,
}

/// The catalog `/Lang` entry, decoded.
fn catalog_lang(document: &Document) -> anyhow::Result<Option<String>> {
    let root = document
        .root()
        .map_err(|error| anyhow::anyhow!("read catalog: {error}"))?;
    let catalog = document
        .object(root)
        .map_err(|error| anyhow::anyhow!("load catalog: {error}"))?;
    let Some(catalog) = catalog.as_dictionary() else {
        return Ok(None);
    };
    let value = match catalog.get("Lang") {
        Some(Object::String(bytes)) => Some(pdfxi_core::decode_text_string(bytes)),
        Some(Object::Name(name)) => Some(name.clone()),
        _ => None,
    };
    Ok(value.filter(|lang| !lang.trim().is_empty()))
}

/// Build an accessibility tree from the recovered page text.
fn derived_tree(document: &Document) -> anyhow::Result<(A11yTree, usize)> {
    let pages = document
        .pages()
        .map_err(|error| anyhow::anyhow!("read pages: {error}"))?;
    let mut nodes = Vec::new();
    let mut page_ids = Vec::new();
    let mut next = 2_u64;
    let mut paragraphs = 0;
    for (index, _page) in pages.iter().enumerate() {
        let text = document
            .page_text(index)
            .map_err(|error| anyhow::anyhow!("read page {} text: {error}", index + 1))?;
        let mut children = Vec::new();
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
            let id = NodeId::new(next);
            next += 1;
            let bbox = BBox::new(0.0, 0.0, 1.0, 1.0)
                .map_err(|error| anyhow::anyhow!("build paragraph box: {error}"))?;
            nodes.push(
                A11yNode::new(id, TagRole::Paragraph, index, bbox, line, Vec::new())
                    .with_actual_text(line),
            );
            children.push(id);
            paragraphs += 1;
        }
        let page_id = NodeId::new(next);
        next += 1;
        let bbox = BBox::new(0.0, 0.0, 1.0, 1.0)
            .map_err(|error| anyhow::anyhow!("build page box: {error}"))?;
        nodes.push(A11yNode::new(
            page_id,
            TagRole::Page,
            index,
            bbox,
            String::new(),
            children,
        ));
        page_ids.push(page_id);
    }
    let bbox = BBox::new(0.0, 0.0, 1.0, 1.0)
        .map_err(|error| anyhow::anyhow!("build document box: {error}"))?;
    nodes.push(A11yNode::new(
        NodeId::new(1),
        TagRole::Document,
        0,
        bbox,
        String::new(),
        page_ids,
    ));
    let tree = A11yTree::new(nodes, vec![NodeId::new(1)])
        .map_err(|error| anyhow::anyhow!("build accessibility tree: {error}"))?;
    Ok((tree, paragraphs))
}

/// One finding from the accessibility checker.
fn tree_findings(report: &pdfxi_accessibility::Report) -> Vec<FindingRow> {
    report
        .findings()
        .iter()
        .map(|finding| FindingRow {
            source: "tree",
            code: finding.rule.as_str().to_owned(),
            severity: finding.severity.to_string(),
            message: finding.message.clone(),
            page: finding.page.map(|page| page + 1),
            location: None,
        })
        .collect()
}

/// `pdfxi accessibility check`.
pub(crate) fn run_check(
    file: &Path,
    title_override: Option<&str>,
    lang_override: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let document = Document::from_bytes(bytes.clone())
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))?;
    let pages = document
        .pages()
        .map_err(|error| anyhow::anyhow!("read pages: {error}"))?
        .len();
    if pages == 0 {
        bail!("{} has no pages", file.display());
    }
    let info = document
        .info()
        .map_err(|error| anyhow::anyhow!("read document info: {error}"))?;
    let title = title_override
        .map(str::to_owned)
        .or_else(|| info.and_then(|info| info.title))
        .filter(|title| !title.trim().is_empty());
    let lang = lang_override
        .map(str::to_owned)
        .or(catalog_lang(&document)?)
        .filter(|lang| !lang.trim().is_empty());

    let (tree, paragraphs) = derived_tree(&document)?;
    let options = CheckOptions::new(title.as_deref(), lang.as_deref(), pages);
    let tree_report =
        check(&tree, &options).map_err(|error| anyhow::anyhow!("accessibility check: {error}"))?;
    let suggestions = suggest_fixups(&tree_report, &tree);
    let fix_plan = build_fix_plan(&tree_report, &tree)
        .map(|plan| {
            plan.steps()
                .iter()
                .map(|step| format!("{} {}", step.rule, step.description))
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|_| {
            suggestions
                .iter()
                .map(|suggestion| suggestion.description.clone())
                .collect()
        });

    let ua = preflight_bytes(&bytes, "PDF/UA-1", &PreflightOptions::default())
        .map_err(|error| anyhow::anyhow!("PDF/UA-1 preflight: {error}"))?;
    let mut findings = tree_findings(&tree_report);
    for finding in ua.final_report().findings() {
        findings.push(FindingRow {
            source: "pdfua",
            code: finding.code().to_owned(),
            severity: finding.severity().label().to_owned(),
            message: finding.message().to_owned(),
            page: None,
            location: finding.location().map(str::to_owned),
        });
    }
    let errors = tree_report.summary().errors
        + ua.final_report()
            .findings()
            .iter()
            .filter(|finding| finding.severity().is_error())
            .count();
    let warnings = tree_report.summary().warnings
        + ua.final_report()
            .findings()
            .iter()
            .filter(|finding| finding.severity().label() == "warning")
            .count();
    let valid = errors == 0;
    let report = AccessibilityReport {
        file: file.display().to_string(),
        title,
        lang,
        pages,
        paragraphs,
        findings,
        errors,
        warnings,
        valid,
        fix_plan,
        certification_claimed: false,
        scope_note: "The accessibility tree is derived from recovered page text; the PDF/UA-1 \
                     profile checks the file's real catalog and structure. No conformance \
                     certification is claimed.",
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("file: {}", report.file);
        println!("pages: {}", report.pages);
        println!("paragraphs: {}", report.paragraphs);
        if let Some(title) = &report.title {
            println!("title: {title}");
        }
        if let Some(lang) = &report.lang {
            println!("lang: {lang}");
        }
        for finding in &report.findings {
            let page = finding
                .page
                .map(|page| format!(" page {page}"))
                .unwrap_or_default();
            let location = finding
                .location
                .as_ref()
                .map(|location| format!(" ({location})"))
                .unwrap_or_default();
            println!(
                "[{}] {}/{} {}{}{}",
                finding.severity, finding.source, finding.code, finding.message, page, location
            );
        }
        for line in &report.fix_plan {
            println!("fix: {line}");
        }
        println!("errors: {}", report.errors);
        println!("warnings: {}", report.warnings);
        println!("valid: {}", report.valid);
        println!("certification_claimed: false");
        println!("scope: {}", report.scope_note);
    }
    if !valid {
        std::process::exit(EXIT_NOT_ACCESSIBLE);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::EXIT_NOT_ACCESSIBLE;

    #[test]
    fn non_accessible_exit_code_is_distinct() {
        assert_ne!(EXIT_NOT_ACCESSIBLE, 0);
        assert_ne!(EXIT_NOT_ACCESSIBLE, 1);
    }
}
