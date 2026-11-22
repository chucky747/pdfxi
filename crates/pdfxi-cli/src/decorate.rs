//! `pdfxi decorate`: stamp headers and footers on every page.
//!
//! The header/footer model lives in `pdfxi-edit` (`HeaderFooterLayout`,
//! `render_page_set`); this module is the host that turns its per-page content
//! streams into a saved document. Each page's overlay becomes a
//! [`StampOverlay`](pdfxi_core::StampOverlay) and is applied with
//! `PageOperation::StampPages`, so the overlay is real page content: it is
//! rendered, searched and extracted like any other text.
//!
//! Text placeholders are declared with braces: `{page}`, `{pages}`, `{date}`,
//! `{title}` and `{filename}`. Everything else is literal. An unknown
//! placeholder is a typed error instead of silently printing braces.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_core::{Document, PageOperation, PagePlan, StampOverlay};
use pdfxi_edit::{
    BatesAnchor, BatesColor, BatesConfig, BatesMargins, BatesPageBounds, BatesStyle,
    HeaderFooterLayout, Margins, MarkContent, PageGeometry, PageIndex, PageSetContext, Placement,
    Rect, Slot, SlotContent, StandardFont, TextColor, TextStyle, Token, Watermark,
    WatermarkPosition, WatermarkStyle, build_bates_plan, escape_literal, render_watermark_page_set,
    transform_for_page,
};
use serde::Serialize;

use clap::ValueEnum;

/// Which side of the header or footer a template occupies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Side {
    /// Left-aligned slot.
    Left,
    /// Centre slot.
    Center,
    /// Right-aligned slot.
    Right,
}

impl Side {
    /// Header slot for this side.
    fn header_slot(self) -> Slot {
        match self {
            Self::Left => Slot::HeaderLeft,
            Self::Center => Slot::HeaderCenter,
            Self::Right => Slot::HeaderRight,
        }
    }

    /// Footer slot for this side.
    fn footer_slot(self) -> Slot {
        match self {
            Self::Left => Slot::FooterLeft,
            Self::Center => Slot::FooterCenter,
            Self::Right => Slot::FooterRight,
        }
    }
}

/// JSON shape of `pdfxi decorate header-footer --json`.
#[derive(Serialize)]
struct HeaderFooterReport {
    /// Input file.
    file: String,
    /// Written file.
    out: String,
    /// Pages in the document.
    pages: usize,
    /// Pages that received an overlay.
    stamped: usize,
    /// Pages whose overlay was empty because a page filter suppressed them.
    suppressed: usize,
    /// Header template, when given.
    header: Option<String>,
    /// Footer template, when given.
    footer: Option<String>,
    /// Font size in points.
    font_size: f32,
}

fn load(file: &Path) -> anyhow::Result<Document> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))
}

/// One `decorate header-footer` request.
pub(crate) struct HeaderFooterRequest<'a> {
    /// Header template.
    pub header: Option<&'a str>,
    /// Footer template.
    pub footer: Option<&'a str>,
    /// Slot side for both templates.
    pub side: Side,
    /// Font size in points.
    pub font_size: f32,
    /// Page margin in points.
    pub margin: f32,
    /// `{title}` substitution.
    pub title: &'a str,
    /// `{date}` substitution.
    pub date: &'a str,
    /// Emit JSON.
    pub json: bool,
}

/// Stamp a header and/or footer on every page.
pub(crate) fn run_header_footer(
    file: &Path,
    out: &Path,
    request: &HeaderFooterRequest<'_>,
) -> anyhow::Result<()> {
    let HeaderFooterRequest {
        header,
        footer,
        side,
        font_size,
        margin,
        title,
        date,
        json,
    } = *request;
    if header.is_none() && footer.is_none() {
        bail!("give --header, --footer, or both");
    }
    if !(4.0..=48.0).contains(&font_size) {
        bail!("--font-size must be between 4 and 48 points");
    }
    if margin < 0.0 {
        bail!("--margin must not be negative");
    }

    let document = load(file)?;
    let count = document
        .page_count()
        .with_context(|| format!("count pages of {}", file.display()))?;
    if count == 0 {
        bail!("{} has no pages", file.display());
    }

    let mut layout = HeaderFooterLayout::new()
        .with_style(TextStyle::new(
            StandardFont::Helvetica,
            font_size,
            TextColor::BLACK,
        )?)
        .with_margins(Margins::uniform(margin)?);
    if let Some(header) = header {
        layout = layout.with_slot(side.header_slot(), SlotContent::new(parse_tokens(header)?));
    }
    if let Some(footer) = footer {
        layout = layout.with_slot(side.footer_slot(), SlotContent::new(parse_tokens(footer)?));
    }

    let geometries = page_geometries(&document, count)?;

    let context = PageSetContext::new(title, date, file_name(file));
    let overlays = pdfxi_edit::render_page_set(&layout, &geometries, &context)
        .context("render header/footer overlays")?;

    let mut stamps = Vec::new();
    let mut suppressed = 0_usize;
    for (index, overlay) in overlays.iter().enumerate() {
        if overlay.content_bytes().is_empty() {
            suppressed += 1;
            continue;
        }
        stamps.push((
            index,
            StampOverlay {
                content: overlay.content_bytes().to_vec(),
                fonts: overlay
                    .fonts()
                    .iter()
                    .map(|(name, font)| (name.clone(), font.base_name().to_owned()))
                    .collect(),
                ext_gstates: Vec::new(),
                front: true,
            },
        ));
    }
    let stamped = stamps.len();
    let plan = PagePlan::identity()
        .then(PageOperation::StampPages { overlays: stamps })
        .context("build the header/footer plan")?;
    let bytes = document
        .apply_page_plan(&plan)
        .context("apply the header/footer plan")?;
    std::fs::write(out, &bytes).with_context(|| format!("write {}", out.display()))?;

    let report = HeaderFooterReport {
        file: file.display().to_string(),
        out: out.display().to_string(),
        pages: count,
        stamped,
        suppressed,
        header: header.map(str::to_owned),
        footer: footer.map(str::to_owned),
        font_size,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("file: {}", report.file);
        println!("out:  {}", report.out);
        println!("pages: {}", report.pages);
        println!("stamped: {}", report.stamped);
        println!("suppressed: {}", report.suppressed);
        println!("font size: {}", report.font_size);
    }
    Ok(())
}

/// Split a template into literal text and the supported placeholder tokens.
fn parse_tokens(template: &str) -> anyhow::Result<Vec<Token>> {
    let mut tokens = Vec::new();
    let mut literal = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        literal.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            bail!("unclosed placeholder in {template:?}");
        };
        let name = &after[..end];
        let token = match name {
            "page" => Token::Page,
            "pages" => Token::Pages,
            "date" => Token::Date,
            "title" => Token::Title,
            "filename" => Token::FileName,
            other => bail!(
                "unknown placeholder {{{other}}}; use {{page}}, {{pages}}, {{date}}, {{title}} or {{filename}}"
            ),
        };
        if !literal.is_empty() {
            tokens.push(Token::Literal(std::mem::take(&mut literal)));
        }
        tokens.push(token);
        rest = &after[end + 1..];
    }
    if rest.contains('}') {
        bail!("stray '}}' in {template:?}");
    }
    literal.push_str(rest);
    if !literal.is_empty() {
        tokens.push(Token::Literal(literal));
    }
    if tokens.is_empty() {
        bail!("a header or footer template must not be empty");
    }
    Ok(tokens)
}

/// Which corner or edge the Bates number is anchored to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum BatesCorner {
    /// Lower-right corner (the conventional position).
    BottomRight,
    /// Bottom centre.
    BottomCenter,
    /// Lower-left corner.
    BottomLeft,
    /// Upper-right corner.
    TopRight,
    /// Top centre.
    TopCenter,
    /// Upper-left corner.
    TopLeft,
}

impl BatesCorner {
    /// The model anchor for this corner.
    fn anchor(self) -> BatesAnchor {
        match self {
            Self::BottomRight => BatesAnchor::BottomRight,
            Self::BottomCenter => BatesAnchor::BottomCenter,
            Self::BottomLeft => BatesAnchor::BottomLeft,
            Self::TopRight => BatesAnchor::TopRight,
            Self::TopCenter => BatesAnchor::TopCenter,
            Self::TopLeft => BatesAnchor::TopLeft,
        }
    }
}

/// One `decorate bates` request.
pub(crate) struct BatesRequest<'a> {
    /// Number prefix.
    pub prefix: &'a str,
    /// Number suffix.
    pub suffix: &'a str,
    /// First number.
    pub start: u64,
    /// Zero-padded digit width.
    pub digits: usize,
    /// Per-page increment.
    pub increment: u64,
    /// Anchor corner.
    pub corner: BatesCorner,
    /// Font size in points.
    pub font_size: f32,
    /// Page margin in points.
    pub margin: f32,
    /// Emit JSON.
    pub json: bool,
}

/// Stamp a Bates number on every page.
///
/// The plan comes from `pdfxi-edit`'s Bates model; the text is drawn through
/// the same display-to-page transform the header/footer renderer uses, so a
/// rotated page receives upright, correctly placed numbers.
pub(crate) fn run_bates(file: &Path, out: &Path, request: &BatesRequest<'_>) -> anyhow::Result<()> {
    let BatesRequest {
        prefix,
        suffix,
        start,
        digits,
        increment,
        corner,
        font_size,
        margin,
        json,
    } = *request;
    if !(4.0..=200.0).contains(&font_size) {
        bail!("--font-size must be between 4 and 200 points");
    }
    if margin < 0.0 {
        bail!("--margin must not be negative");
    }
    if start == 0 {
        bail!("--start must be at least 1");
    }
    if increment == 0 {
        bail!("--increment must be at least 1");
    }

    let document = load(file)?;
    let count = document
        .page_count()
        .with_context(|| format!("count pages of {}", file.display()))?;
    if count == 0 {
        bail!("{} has no pages", file.display());
    }
    let geometries = page_geometries(&document, count)?;

    let config = BatesConfig::new(prefix, suffix, start, digits)?
        .with_increment(increment)
        .with_anchor(corner.anchor())
        .with_style(BatesStyle::new(
            StandardFont::Helvetica,
            font_size,
            BatesColor::new(0.0, 0.0, 0.0)?,
            BatesMargins::uniform(margin)?,
        )?);
    let bounds: Vec<BatesPageBounds> = geometries
        .iter()
        .enumerate()
        .map(|(index, geometry)| {
            let box_rect = geometry.media_box();
            BatesPageBounds::new(
                index,
                box_rect.x0(),
                box_rect.y0(),
                box_rect.x1(),
                box_rect.y1(),
            )
        })
        .collect();
    // No date stamp is configured, so the clock value is never read; passing a
    // constant keeps the command deterministic.
    let bates_plan = build_bates_plan(&config, &bounds, 0)?;
    let style_font_size = config.style().font_size();
    let first_number = bates_plan
        .stamps()
        .first()
        .map(|stamp| stamp.text().to_owned())
        .unwrap_or_default();
    let last_number = bates_plan
        .stamps()
        .last()
        .map(|stamp| stamp.text().to_owned())
        .unwrap_or_default();

    let mut stamps = Vec::with_capacity(bates_plan.len());
    for stamp in bates_plan.stamps() {
        let Some(geometry) = geometries.get(stamp.page_index()) else {
            bail!(
                "Bates plan targets page {} of {}",
                stamp.page_index() + 1,
                geometries.len()
            );
        };
        let position = stamp.position();
        let content = format!(
            "q\n{} cm\nBT /F1 {size} Tf {x} {y} Td ({text}) Tj ET\nQ\n",
            transform_for_page(geometry).to_pdf(),
            size = style_font_size,
            x = position.x0(),
            y = position.y0(),
            text = escape_literal(stamp.text()),
        );
        stamps.push((
            stamp.page_index(),
            StampOverlay {
                content: content.into_bytes(),
                fonts: vec![(
                    "F1".to_owned(),
                    StandardFont::Helvetica.base_name().to_owned(),
                )],
                ext_gstates: Vec::new(),
                front: true,
            },
        ));
    }

    let stamped = stamps.len();
    let plan = PagePlan::identity()
        .then(PageOperation::StampPages { overlays: stamps })
        .context("build the Bates plan")?;
    let bytes = document
        .apply_page_plan(&plan)
        .context("apply the Bates plan")?;
    std::fs::write(out, &bytes).with_context(|| format!("write {}", out.display()))?;

    let report = BatesReport {
        file: file.display().to_string(),
        out: out.display().to_string(),
        pages: count,
        stamped,
        prefix: prefix.to_owned(),
        suffix: suffix.to_owned(),
        start,
        digits,
        increment,
        first: first_number,
        last: last_number,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("file: {}", report.file);
        println!("out:  {}", report.out);
        println!("pages: {}", report.pages);
        println!("stamped: {}", report.stamped);
        println!("numbers: {} .. {}", report.first, report.last);
    }
    Ok(())
}

/// JSON shape of `pdfxi decorate bates --json`.
#[derive(Serialize)]
struct BatesReport {
    /// Input file.
    file: String,
    /// Written file.
    out: String,
    /// Pages in the document.
    pages: usize,
    /// Pages that received a number.
    stamped: usize,
    /// Number prefix.
    prefix: String,
    /// Number suffix.
    suffix: String,
    /// First number.
    start: u64,
    /// Zero-padded digit width.
    digits: usize,
    /// Per-page increment.
    increment: u64,
    /// Full text of the first stamp.
    first: String,
    /// Full text of the last stamp.
    last: String,
}
///
/// `opacity` is written as an `/ExtGState` alpha and merged into the page's own
/// resources; `--tile` repeats the mark on a grid instead of centring it.
pub(crate) struct WatermarkRequest<'a> {
    /// Mark text.
    pub text: &'a str,
    /// Font size in points.
    pub font_size: f32,
    /// Alpha in 0..=1.
    pub opacity: f32,
    /// Rotation in degrees.
    pub rotation: f32,
    /// Repeat the mark on a grid.
    pub tile: bool,
    /// Emit JSON.
    pub json: bool,
}

/// Apply a default centred watermark to an in-memory document.
///
/// Used by `action run --execute`; the mark is the requested text at 72 pt,
/// 15% alpha, rotated 45 degrees, in front of the page content. Returns the
/// complete saved PDF bytes.
pub(crate) fn apply_default_watermark(document: &Document, text: &str) -> anyhow::Result<Vec<u8>> {
    let count = document.page_count().context("count pages")?;
    if count == 0 {
        bail!("document has no pages");
    }
    let geometries = page_geometries(document, count)?;
    let watermark = Watermark::new(MarkContent::Text(text.to_owned()))
        .with_style(WatermarkStyle::new(
            StandardFont::Helvetica,
            72.0,
            TextColor::BLACK,
            0.15,
        )?)
        .with_position(WatermarkPosition::Center)
        .with_rotation_degrees(45.0)
        .with_placement(Placement::Front);
    let overlays =
        render_watermark_page_set(&watermark, &geometries).context("render watermark overlays")?;
    let mut stamps = Vec::new();
    for (index, overlay) in overlays.iter().enumerate() {
        if overlay.content_bytes().is_empty() {
            continue;
        }
        stamps.push((
            index,
            StampOverlay {
                content: overlay.content_bytes().to_vec(),
                fonts: overlay
                    .fonts()
                    .iter()
                    .map(|(name, font)| (name.clone(), font.base_name().to_owned()))
                    .collect(),
                ext_gstates: overlay
                    .alpha()
                    .map(|(name, alpha)| vec![(name.to_owned(), f64::from(alpha))])
                    .unwrap_or_default(),
                front: true,
            },
        ));
    }
    let plan = PagePlan::identity()
        .then(PageOperation::StampPages { overlays: stamps })
        .context("build the watermark plan")?;
    document
        .apply_page_plan(&plan)
        .context("apply the watermark plan")
}

pub(crate) fn run_watermark(
    file: &Path,
    out: &Path,
    request: &WatermarkRequest<'_>,
) -> anyhow::Result<()> {
    let WatermarkRequest {
        text,
        font_size,
        opacity,
        rotation,
        tile,
        json,
    } = *request;
    if text.trim().is_empty() {
        bail!("--text must not be empty");
    }
    if !(4.0..=200.0).contains(&font_size) {
        bail!("--font-size must be between 4 and 200 points");
    }
    if !(0.0..=1.0).contains(&opacity) || !opacity.is_finite() {
        bail!("--opacity must be between 0 and 1");
    }
    if !rotation.is_finite() || rotation.abs() > 360.0 {
        bail!("--rotation must be a finite value between -360 and 360 degrees");
    }

    let document = load(file)?;
    let count = document
        .page_count()
        .with_context(|| format!("count pages of {}", file.display()))?;
    if count == 0 {
        bail!("{} has no pages", file.display());
    }
    let geometries = page_geometries(&document, count)?;

    let position = if tile {
        WatermarkPosition::Tile {
            spacing_x: 200.0,
            spacing_y: 200.0,
        }
    } else {
        WatermarkPosition::Center
    };
    let watermark = Watermark::new(MarkContent::Text(text.to_owned()))
        .with_style(WatermarkStyle::new(
            StandardFont::Helvetica,
            font_size,
            TextColor::BLACK,
            opacity,
        )?)
        .with_position(position)
        .with_rotation_degrees(rotation)
        .with_placement(Placement::Front);
    let overlays =
        render_watermark_page_set(&watermark, &geometries).context("render watermark overlays")?;

    let mut stamps = Vec::new();
    let mut suppressed = 0_usize;
    for (index, overlay) in overlays.iter().enumerate() {
        if overlay.content_bytes().is_empty() {
            suppressed += 1;
            continue;
        }
        stamps.push((
            index,
            StampOverlay {
                content: overlay.content_bytes().to_vec(),
                fonts: overlay
                    .fonts()
                    .iter()
                    .map(|(name, font)| (name.clone(), font.base_name().to_owned()))
                    .collect(),
                ext_gstates: overlay
                    .alpha()
                    .map(|(name, alpha)| vec![(name.to_owned(), f64::from(alpha))])
                    .unwrap_or_default(),
                front: true,
            },
        ));
    }
    let stamped = stamps.len();
    let plan = PagePlan::identity()
        .then(PageOperation::StampPages { overlays: stamps })
        .context("build the watermark plan")?;
    let bytes = document
        .apply_page_plan(&plan)
        .context("apply the watermark plan")?;
    std::fs::write(out, &bytes).with_context(|| format!("write {}", out.display()))?;

    let report = WatermarkReport {
        file: file.display().to_string(),
        out: out.display().to_string(),
        pages: count,
        stamped,
        suppressed,
        text: text.to_owned(),
        font_size,
        opacity,
        rotation,
        tile,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("file: {}", report.file);
        println!("out:  {}", report.out);
        println!("pages: {}", report.pages);
        println!("stamped: {}", report.stamped);
        println!("suppressed: {}", report.suppressed);
        println!(
            "text: {:?}  size: {}  opacity: {}  rotation: {}  tile: {}",
            report.text, report.font_size, report.opacity, report.rotation, report.tile
        );
    }
    Ok(())
}

/// Per-page geometry used by the overlay renderers.
fn page_geometries(document: &Document, count: usize) -> anyhow::Result<Vec<PageGeometry>> {
    let mut geometries = Vec::with_capacity(count);
    for index in 0..count {
        let frame = document
            .page_frame(index)
            .with_context(|| format!("read page {} geometry", index + 1))?;
        let media = Rect::new(
            frame.media[0] as f32,
            frame.media[1] as f32,
            frame.media[2] as f32,
            frame.media[3] as f32,
        )
        .with_context(|| format!("page {} media box is not a rectangle", index + 1))?;
        geometries.push(
            PageGeometry::with_crop_box(PageIndex::new(index), media, media, frame.rotation)
                .with_context(|| format!("page {} geometry", index + 1))?,
        );
    }
    Ok(geometries)
}

/// JSON shape of `pdfxi decorate watermark --json`.
#[derive(Serialize)]
struct WatermarkReport {
    /// Input file.
    file: String,
    /// Written file.
    out: String,
    /// Pages in the document.
    pages: usize,
    /// Pages that received the mark.
    stamped: usize,
    /// Pages the mark's page range suppressed.
    suppressed: usize,
    /// Mark text.
    text: String,
    /// Font size in points.
    font_size: f32,
    /// Alpha used for the mark.
    opacity: f32,
    /// Configured rotation in degrees.
    rotation: f32,
    /// Whether the mark is tiled.
    tile: bool,
}

/// The file name component used for `{filename}`.
fn file_name(file: &Path) -> String {
    file.file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_split_into_literals_and_placeholders() {
        assert_eq!(
            parse_tokens("Page {page} of {pages}").expect("tokens"),
            vec![
                Token::Literal("Page ".to_owned()),
                Token::Page,
                Token::Literal(" of ".to_owned()),
                Token::Pages,
            ]
        );
        assert_eq!(parse_tokens("{title}").expect("tokens"), vec![Token::Title]);
        assert_eq!(
            parse_tokens("plain text").expect("tokens"),
            vec![Token::Literal("plain text".to_owned())]
        );
    }

    #[test]
    fn invalid_templates_are_rejected() {
        assert!(parse_tokens("{unknown}").is_err());
        assert!(parse_tokens("{page").is_err());
        assert!(parse_tokens("brace }").is_err());
        assert!(parse_tokens("").is_err());
    }
}
