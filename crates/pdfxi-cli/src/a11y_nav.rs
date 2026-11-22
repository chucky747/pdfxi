//! `pdfxi accessibility tree|nav|layout`: window-free accessibility hosts.
//!
//! [`pdfxi_app::a11y`] builds the deterministic accessibility tree a host
//! publishes to AccessKit, and [`pdfxi_app::nav`] models the Tab ring, the
//! accessibility shortcut map, the WCAG high-contrast palette and reflow
//! metrics. This host feeds them from a real PDF (page count and page geometry)
//! and prints the result. It performs no GUI calls and talks to no screen
//! reader; it is the data a platform bridge consumes.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_app::a11y::{A11yTree, AccessibilitySnapshot};
use pdfxi_app::nav::{
    FocusOrder, FocusTarget, HIGH_CONTRAST_TARGET, MIN_CONTRAST_RATIO, NavCommand, ReflowRequest,
    Theme, audited_contrast_pairs, contrast_ratio, reflow_metrics, tokens,
};
use pdfxi_core::Document;
use serde::Serialize;

/// One accessibility node.
#[derive(Serialize)]
struct NodeRow {
    /// Stable node id.
    id: u64,
    /// AccessKit role name.
    role: &'static str,
    /// Accessible label.
    label: String,
    /// Optional description.
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    /// Whether the node accepts focus.
    focusable: bool,
    /// Whether the node is disabled.
    disabled: bool,
    /// Child node ids.
    children: Vec<u64>,
}

/// JSON shape of `pdfxi accessibility tree`.
#[derive(Serialize)]
struct TreeReport {
    /// Source PDF.
    file: String,
    /// Page count from the real page tree.
    pages: usize,
    /// One-based current page.
    current_page: usize,
    /// Whether the high-contrast theme is active.
    high_contrast: bool,
    /// Root node id.
    root: u64,
    /// Focused node id.
    focus: u64,
    /// What a screen reader would announce for the focus.
    focus_announcement: String,
    /// Whether the tree is structurally valid.
    well_formed: bool,
    /// Nodes in stable id order.
    nodes: Vec<NodeRow>,
}

/// One audited contrast pair.
#[derive(Serialize)]
struct ContrastRow {
    /// Pair name.
    name: &'static str,
    /// Foreground `#rrggbb`.
    foreground: String,
    /// Background `#rrggbb`.
    background: String,
    /// WCAG contrast ratio.
    ratio: f32,
    /// Required ratio for this theme.
    required: f32,
    /// Whether the pair meets the requirement.
    passes: bool,
}

/// One shortcut row.
#[derive(Serialize)]
struct ShortcutRow {
    /// Command name.
    command: String,
    /// Display shortcut.
    shortcut: &'static str,
}

/// JSON shape of `pdfxi accessibility nav`.
#[derive(Serialize)]
struct NavReport {
    /// Theme name.
    theme: &'static str,
    /// Audited contrast pairs.
    contrasts: Vec<ContrastRow>,
    /// Whether every audited pair passes.
    contrast_ok: bool,
    /// The default Tab ring.
    focus_order: Vec<String>,
    /// Focus sequence produced by N forward Tabs.
    traversal: Vec<String>,
    /// The accessibility shortcut map.
    shortcuts: Vec<ShortcutRow>,
}

/// JSON shape of `pdfxi accessibility layout`.
#[derive(Serialize)]
struct LayoutReport {
    /// Source PDF.
    file: String,
    /// One-based page.
    page: usize,
    /// Page width in points.
    page_width: f64,
    /// Page height in points.
    page_height: f64,
    /// Viewport width in points.
    viewport_width: f32,
    /// Viewport height in points.
    viewport_height: f32,
    /// Base font size.
    font_size: f32,
    /// Line spacing factor.
    line_spacing: f32,
    /// Margin in points.
    margin: f32,
    /// Content width after margins.
    content_width: f32,
    /// Content height after margins.
    content_height: f32,
    /// Scale applied to the source text.
    scale: f32,
    /// Reflowed line height.
    line_height: f32,
    /// Approximate glyphs per line.
    approx_chars_per_line: usize,
    /// Whole lines that fit one viewport page.
    max_lines_per_page: usize,
}

/// Format a colour as `#rrggbb`.
fn hex(colour: pdfxi_app::nav::Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", colour.r, colour.g, colour.b)
}

/// Parse a focus target label.
fn parse_focus(text: &str) -> anyhow::Result<FocusTarget> {
    let normalised = text.trim().to_ascii_lowercase();
    let target = match normalised.as_str() {
        "menubar" | "menu" => FocusTarget::MenuBar,
        "toolbar" => FocusTarget::Toolbar,
        "tabstrip" | "tabs" => FocusTarget::TabStrip,
        "thumbnails" | "thumbnailpane" => FocusTarget::ThumbnailPane,
        "canvas" | "page" | "pagecanvas" => FocusTarget::PageCanvas,
        "findbar" | "find" => FocusTarget::FindBar,
        "layers" | "layerspanel" => FocusTarget::LayersPanel,
        "bookmarks" | "bookmarkspanel" => FocusTarget::BookmarksPanel,
        "attachments" | "attachmentspanel" => FocusTarget::AttachmentsPanel,
        "links" | "linkspanel" => FocusTarget::LinksPanel,
        "statusbar" | "status" => FocusTarget::StatusBar,
        other => bail!(
            "unknown focus target {other:?}; expected menubar, toolbar, tabs, thumbnails, \
             canvas, find, layers, bookmarks, attachments, links or status"
        ),
    };
    Ok(target)
}

/// Load a document and its page count.
fn load(file: &Path) -> anyhow::Result<(Document, usize)> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))?;
    let pages = document
        .page_count()
        .map_err(|error| anyhow::anyhow!("count pages: {error}"))?;
    if pages == 0 {
        bail!("{} has no pages", file.display());
    }
    Ok((document, pages))
}

/// `pdfxi accessibility tree`.
pub(crate) fn tree(
    file: &Path,
    page: Option<usize>,
    focus: Option<&str>,
    high_contrast: bool,
    json: bool,
) -> anyhow::Result<()> {
    let (_, pages) = load(file)?;
    let current = page.unwrap_or(1);
    if current == 0 || current > pages {
        bail!("--page {current} is outside 1..={pages}");
    }
    let focus_target = match focus {
        Some(text) => parse_focus(text)?,
        None => FocusTarget::PageCanvas,
    };
    let title = file
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("document")
        .to_owned();
    let snapshot = AccessibilitySnapshot::new(
        "PDFXI",
        vec![title],
        Some(0),
        pages,
        current - 1,
        focus_target,
        high_contrast,
    );
    let tree = A11yTree::from_snapshot(&snapshot);
    let announcement = tree.focus_announcement();
    let report = TreeReport {
        file: file.display().to_string(),
        pages,
        current_page: current,
        high_contrast,
        root: tree.root(),
        focus: tree.focus(),
        focus_announcement: announcement.spoken_text(),
        well_formed: tree.is_well_formed(),
        nodes: tree
            .nodes()
            .iter()
            .map(|node| NodeRow {
                id: node.id,
                role: node.role.accesskit_name(),
                label: node.label.clone(),
                description: (!node.description.is_empty()).then(|| node.description.clone()),
                focusable: node.focusable,
                disabled: node.disabled,
                children: node.children.clone(),
            })
            .collect(),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!(
        "file: {}  pages: {}  current: {}  high-contrast: {}",
        report.file, report.pages, report.current_page, report.high_contrast
    );
    println!(
        "root: {}  focus: {}  well-formed: {}",
        report.root, report.focus, report.well_formed
    );
    println!("announcement: {}", report.focus_announcement);
    for node in &report.nodes {
        let flags = match (node.focusable, node.disabled) {
            (true, true) => " focusable disabled",
            (true, false) => " focusable",
            (false, true) => " disabled",
            (false, false) => "",
        };
        println!(
            "{}\t{}{}\t{}{}",
            node.id,
            node.role,
            flags,
            node.label,
            if node.children.is_empty() {
                String::new()
            } else {
                format!(" -> {:?}", node.children)
            }
        );
    }
    Ok(())
}

/// `pdfxi accessibility nav`.
pub(crate) fn nav(theme_text: &str, tabs: usize, reverse: bool, json: bool) -> anyhow::Result<()> {
    let theme = match theme_text.trim().to_ascii_lowercase().as_str() {
        "standard" | "default" => Theme::Standard,
        "high-contrast" | "highcontrast" | "high" => Theme::HighContrast,
        other => bail!("unknown theme {other:?}; expected standard or high-contrast"),
    };
    let required = if matches!(theme, Theme::HighContrast) {
        HIGH_CONTRAST_TARGET
    } else {
        MIN_CONTRAST_RATIO
    };
    let palette = tokens(theme);
    let contrasts: Vec<ContrastRow> = audited_contrast_pairs(palette)
        .into_iter()
        .map(|(name, foreground, background)| {
            let ratio = contrast_ratio(foreground, background);
            ContrastRow {
                name,
                foreground: hex(foreground),
                background: hex(background),
                ratio,
                required,
                passes: ratio >= required,
            }
        })
        .collect();
    let contrast_ok = contrasts.iter().all(|row| row.passes);

    let order = FocusOrder::default_order();
    let focus_order: Vec<String> = order
        .entries()
        .iter()
        .map(|entry| entry.target.label().to_owned())
        .collect();
    let mut walk = FocusOrder::default_order();
    let mut traversal = Vec::with_capacity(tabs);
    for _ in 0..tabs {
        if let Some(target) = walk.tab(!reverse) {
            traversal.push(target.label().to_owned());
        }
    }

    let mut shortcuts = Vec::new();
    for command in [
        NavCommand::NextPane,
        NavCommand::PreviousPane,
        NavCommand::FocusCanvas,
        NavCommand::FocusFind,
        NavCommand::ToggleHighContrast,
        NavCommand::ReadAloud,
    ] {
        shortcuts.push(ShortcutRow {
            command: format!("{command:?}"),
            shortcut: command.shortcut_label(),
        });
    }

    let report = NavReport {
        theme: if matches!(theme, Theme::HighContrast) {
            "high-contrast"
        } else {
            "standard"
        },
        contrasts,
        contrast_ok,
        focus_order,
        traversal,
        shortcuts,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!(
        "theme: {}  contrast: {}",
        report.theme,
        if report.contrast_ok { "pass" } else { "fail" }
    );
    for row in &report.contrasts {
        println!(
            "  {}  {} on {}  ratio {:.2} (need {:.1})  {}",
            row.name,
            row.foreground,
            row.background,
            row.ratio,
            row.required,
            if row.passes { "pass" } else { "fail" }
        );
    }
    println!("focus order: {}", report.focus_order.join(" -> "));
    println!("tab traversal: {}", report.traversal.join(" -> "));
    for row in &report.shortcuts {
        println!("  {}  {}", row.shortcut, row.command);
    }
    Ok(())
}

/// Parse a `WxH` viewport.
fn parse_viewport(text: &str) -> anyhow::Result<(u32, u32)> {
    let (width, height) = text
        .split_once(['x', 'X'])
        .with_context(|| format!("viewport {text:?} must be WIDTHxHEIGHT"))?;
    let width: u32 = width
        .trim()
        .parse()
        .with_context(|| format!("viewport width {width:?} is not a number"))?;
    let height: u32 = height
        .trim()
        .parse()
        .with_context(|| format!("viewport height {height:?} is not a number"))?;
    if width == 0 || height == 0 {
        bail!("viewport dimensions must be positive");
    }
    Ok((width, height))
}

/// `pdfxi accessibility layout`.
pub(crate) fn layout(
    file: &Path,
    page: Option<usize>,
    viewport: &str,
    font_size: f32,
    line_spacing: f32,
    margin: f32,
    json: bool,
) -> anyhow::Result<()> {
    let (document, pages) = load(file)?;
    let current = page.unwrap_or(1);
    if current == 0 || current > pages {
        bail!("--page {current} is outside 1..={pages}");
    }
    let (viewport_width, viewport_height) = parse_viewport(viewport)?;
    let page = document
        .page(current - 1)
        .map_err(|error| anyhow::anyhow!("read page {current}: {error}"))?;
    let page_width = page.width();
    let page_height = page.height();
    let metrics = reflow_metrics(ReflowRequest {
        page_width: page_width as f32,
        viewport_width: viewport_width as f32,
        viewport_height: viewport_height as f32,
        font_size,
        line_spacing,
        margin,
    })
    .context("invalid reflow geometry")?;
    let report = LayoutReport {
        file: file.display().to_string(),
        page: current,
        page_width,
        page_height,
        viewport_width: viewport_width as f32,
        viewport_height: viewport_height as f32,
        font_size,
        line_spacing,
        margin,
        content_width: metrics.content_width,
        content_height: metrics.content_height,
        scale: metrics.scale,
        line_height: metrics.line_height,
        approx_chars_per_line: metrics.approx_chars_per_line,
        max_lines_per_page: metrics.max_lines_per_page,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!(
        "file: {}  page: {}  page-size: {:.1}x{:.1}",
        report.file, report.page, report.page_width, report.page_height
    );
    println!(
        "viewport: {}x{}  content: {:.1}x{:.1}  scale: {:.3}",
        report.viewport_width,
        report.viewport_height,
        report.content_width,
        report.content_height,
        report.scale
    );
    println!(
        "font: {}  line-spacing: {}  margin: {}  line-height: {:.2}",
        report.font_size, report.line_spacing, report.margin, report.line_height
    );
    println!(
        "approx chars/line: {}  max lines/page: {}",
        report.approx_chars_per_line, report.max_lines_per_page
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_regions_and_viewports_are_validated() {
        assert_eq!(
            parse_focus("canvas").expect("canvas"),
            FocusTarget::PageCanvas
        );
        assert!(parse_focus("F6").is_err());
        assert_eq!(
            parse_focus("status").expect("status"),
            FocusTarget::StatusBar
        );
        assert!(parse_focus("sidebar").is_err());
        assert_eq!(parse_viewport("400x700").expect("viewport"), (400, 700));
        assert_eq!(parse_viewport("1X2").expect("viewport"), (1, 2));
        assert!(parse_viewport("0x700").is_err());
        assert!(parse_viewport("400").is_err());
        assert!(parse_viewport("axb").is_err());
    }

    #[test]
    fn colours_format_as_hex() {
        assert_eq!(hex(pdfxi_app::nav::Rgb::new(0, 84, 168)), "#0054a8");
        assert_eq!(hex(pdfxi_app::nav::Rgb::new(255, 255, 255)), "#ffffff");
    }
}
