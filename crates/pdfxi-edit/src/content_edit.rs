//! Text and image editing on a real [`pdfxi_core::Document`].
//!
//! This module turns a page's content stream into a geometry-aware text model
//! and writes real edits back through the core object model. It is the library
//! half of the Edit PDF acceptance items A-17 and A-23: click a paragraph, read
//! its blocks, replace run text with reflow, add text boxes and place images,
//! then save and reopen so another reader (and text extraction) sees the
//! change.
//!
//! # Text model
//!
//! [`text_blocks`] decodes the page content stream with the same filters the
//! core reader uses, tracks `q`/`Q`, `cm`, `BT`/`ET`, `Tm`/`Td`/`TD`/`T*`,
//! `Tf`, `Tz`, `Tc`, `Tw` and the four text-showing operators, and groups runs
//! into lines and blocks. When the page carries marked-content paragraphs
//! (`/P <</MCID n>> BDC`), each paragraph is one block; otherwise runs are
//! clustered by baseline and vertical proximity. Runs carry the font resource
//! name, font size, text-space matrix, baseline origin and an approximate
//! baseline rectangle in default user space.
//!
//! # Replacement
//!
//! [`replace_run_text`] removes every text-showing operator in the target block
//! and re-emits the whole block at the first run's position, so other content
//! stream operators (clips, colours, marked content) are preserved. Reflow is a
//! greedy word wrap at the block width with one font for the whole block. The
//! run's own font is reused only when it is a non-subset simple font that can
//! encode every character; otherwise the bundled fallback font is embedded as
//! a `/Type0`/`/TrueType` CID font with a `/ToUnicode` CMap, a `/CIDToGIDMap`
//! stream and CID-keyed `/W` widths, and the result reports that choice.
//!
//! # Limits
//!
//! One font per reflowed block, greedy (no justified) wrapping, horizontal text
//! only, and no alpha channel for inserted images. Redaction and annotation
//! appearance belong to their own modules.
//!
//! # Object operations
//!
//! [`list_objects`] enumerates text blocks and image XObjects drawn directly by
//! the page with stable `text:<block>` and `image:<resource>:<occurrence>`
//! identifiers. [`move_object`], [`resize_object`], [`replace_object`] and
//! [`delete_object`] edit one object at a time. Moves and resizes rewrite only
//! the placement operators; replace reuses the reflow/font path for text and
//! swaps the image XObject stream for images; delete removes the owning text
//! object or the `Do` usage and unlinks an unshared image. Shared text objects,
//! shared image placements and shared image resources fail closed rather than
//! changing content the caller did not select. Identifiers are only stable for
//! the current page revision; re-list after every edit.
//!
//! All edits are copy-on-write through [`pdfxi_core::Document::set_object`];
//! source bytes are never mutated.

use std::collections::{BTreeMap, BTreeSet};

use pdfxi_core::{
    CidFontOptions, Dictionary, Document, EmbeddingDecision, Filter, FlateParams, FontProgram,
    FontProgramFormat, GlyphWidth, Object, ObjectId, ToUnicodeEntry, ToUnicodeMap, decode_stream,
};
use thiserror::Error;

use crate::geometry::{GeometryError, Point, Rect};
use crate::paragraph_edit::{EditLimits, ReflowOptions, reflow_paragraph};
use crate::truetype::TrueTypeFont;

/// The bundled fallback font: Courier Prime Regular, SIL Open Font License 1.1.
const FALLBACK_FONT: &[u8] = include_bytes!("../assets/CourierPrime-Regular.ttf");
/// Base font name written for the bundled fallback.
pub const FALLBACK_BASE_FONT: &str = "CourierPrime";
/// Default usable width of a text block when a run has no measurable advance.
const MIN_BLOCK_WIDTH: f64 = 24.0;
/// Multiplier from font size to the line advance used by reflow.
const LINE_HEIGHT_FACTOR: f64 = 1.2;

/// Horizontal alignment of an inserted text box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextAlign {
    /// Lines start at the box's left edge.
    Left,
    /// Lines are centred in the box.
    Center,
    /// Lines end at the box's right edge.
    Right,
}

/// Image formats accepted by [`add_image`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    /// Eight-bit PNG (greyscale, RGB, RGBA, grey+alpha or palette).
    Png,
    /// Baseline or progressive JPEG, embedded as a `DCTDecode` stream.
    Jpeg,
}

/// One continuous run of shown text and its placement.
#[derive(Clone, Debug, PartialEq)]
pub struct TextRun {
    /// Zero-based index of this run in the page model's reading order.
    pub index: usize,
    /// Zero-based block index that contains this run.
    pub block: usize,
    /// Zero-based line index inside the block.
    pub line: usize,
    /// Font resource name without its leading slash, as selected by `Tf`.
    pub font: String,
    /// Font size in text space units.
    pub font_size: f64,
    /// Decoded Unicode text shown by the run.
    pub text: String,
    /// Baseline origin in default user space.
    pub start: Point,
    /// Text matrix at the start of the run in PDF `[a b c d e f]` order.
    pub matrix: [f64; 6],
    /// Approximate baseline bounding rectangle in default user space.
    pub rect: Rect,
    /// Byte offset of the first operand of the showing operator.
    pub(crate) show_start: usize,
    /// Byte offset just past the showing operator.
    pub(crate) show_end: usize,
}

/// One line inside a [`TextBlock`].
#[derive(Clone, Debug, PartialEq)]
pub struct TextLine {
    /// Bounding rectangle of the line's runs.
    pub rect: Rect,
    /// Global run indices on this line, left to right.
    pub run_indices: Vec<usize>,
}

/// One paragraph or geometrically clustered block of text.
#[derive(Clone, Debug, PartialEq)]
pub struct TextBlock {
    /// Zero-based page index.
    pub page_index: usize,
    /// Zero-based block index in reading order.
    pub index: usize,
    /// Union of every run rectangle in the block.
    pub rect: Rect,
    /// Width used as the reflow target, in default user space units.
    pub width: f32,
    /// Lines in reading order.
    pub lines: Vec<TextLine>,
    /// Runs in reading order.
    pub runs: Vec<TextRun>,
}

impl TextBlock {
    /// Return the run with the supplied global index, when it belongs here.
    pub fn run(&self, index: usize) -> Option<&TextRun> {
        self.runs.iter().find(|run| run.index == index)
    }
}

/// The text model of one page.
#[derive(Clone, Debug, PartialEq)]
pub struct PageTextModel {
    /// Zero-based page index.
    pub page_index: usize,
    /// Blocks in reading order (top to bottom, then left to right).
    pub blocks: Vec<TextBlock>,
}

impl PageTextModel {
    /// Iterate every run in reading order.
    pub fn runs(&self) -> impl Iterator<Item = &TextRun> {
        self.blocks.iter().flat_map(|block| block.runs.iter())
    }

    /// Return a run by its global index.
    pub fn run(&self, index: usize) -> Option<&TextRun> {
        self.runs().find(|run| run.index == index)
    }

    /// Return the block that contains a run.
    pub fn block_for_run(&self, index: usize) -> Option<&TextBlock> {
        self.blocks.iter().find(|block| block.run(index).is_some())
    }
}

/// Which font produced a replacement's new text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontChoice {
    /// Page resource name used by the emitted content stream.
    pub resource: String,
    /// PDF base font name.
    pub base_font: String,
    /// `true` when the bundled fallback font was used.
    pub embedded: bool,
}

/// Result of [`replace_run_text`].
#[derive(Clone, Debug, PartialEq)]
pub struct ReplaceOutcome {
    /// Zero-based page index.
    pub page_index: usize,
    /// Block that was rebuilt.
    pub block_index: usize,
    /// Text written for the target run before reflow.
    pub new_text: String,
    /// Number of reflowed lines emitted for the block.
    pub lines: usize,
    /// Font used for the rebuilt block.
    pub font: FontChoice,
}

/// Result of [`add_text_box`].
#[derive(Clone, Debug, PartialEq)]
pub struct TextPlacement {
    /// Zero-based page index.
    pub page_index: usize,
    /// Requested box.
    pub rect: Rect,
    /// Number of emitted lines.
    pub lines: usize,
    /// Font used for the inserted text.
    pub font: FontChoice,
}

/// Result of [`add_image`].
#[derive(Clone, Debug, PartialEq)]
pub struct ImagePlacement {
    /// Zero-based page index.
    pub page_index: usize,
    /// Placed rectangle in default user space.
    pub rect: Rect,
    /// Decoded pixel width.
    pub pixel_width: u32,
    /// Decoded pixel height.
    pub pixel_height: u32,
    /// Page resource name of the image XObject.
    pub resource: String,
}

/// Kind of a selectable object on a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectKind {
    /// A text block from [`text_blocks`]: one paragraph or geometrically
    /// clustered group of runs.
    TextBlock,
    /// An image XObject drawn directly by the page content stream.
    Image,
}

/// One selectable object on a page.
///
/// # Identifier stability
///
/// Identifiers are derived from the current page state, never from object
/// numbers:
///
/// - [`ObjectKind::TextBlock`] uses `text:<block-index>`, the zero-based block
///   index in [`PageTextModel::blocks`] reading order.
/// - [`ObjectKind::Image`] uses `image:<resource-name>:<occurrence>`, the page
///   resource name without its leading slash and the zero-based occurrence of a
///   `Do` operator that names that resource, in content-stream order.
///
/// An identifier stays valid while the page content and its text/image model are
/// unchanged. Every move or delete rewrites page content, so callers must call
/// [`list_objects`] again after an edit; an identifier from an earlier revision
/// may name a different object or may no longer exist. A move does not change
/// the number or order of text-showing operators, so its own identifier remains
/// valid for a subsequent delete of the same object in round A.
#[derive(Clone, Debug, PartialEq)]
pub struct PageObject {
    /// Zero-based page index.
    pub page: usize,
    /// Stable identifier for the current page state; see the type documentation.
    pub id: String,
    /// Kind of object this identifier names.
    pub kind: ObjectKind,
    /// Placed rectangle in default user space.
    pub rect: Rect,
    /// Short preview of the object's text; empty for images.
    pub text_preview: String,
}

/// Typed failures from the text and image edit API.
#[derive(Debug, Error)]
pub enum TextEditError {
    /// The core reader or writer rejected the document.
    #[error(transparent)]
    Core(#[from] pdfxi_core::Error),
    /// A rectangle was invalid.
    #[error(transparent)]
    Geometry(#[from] GeometryError),
    /// The embedded font plan was rejected by the core font builder.
    #[error(transparent)]
    Font(#[from] pdfxi_core::font::FontError),
    /// Paragraph reflow rejected the replacement text.
    #[error(transparent)]
    Reflow(#[from] crate::paragraph_edit::EditError),
    /// The bundled font program could not be read.
    #[error("fallback font program error: {0}")]
    FontProgram(String),
    /// The requested run does not exist.
    #[error("run {index} does not exist (page has {runs} runs)")]
    RunNotFound {
        /// Requested global run index.
        index: usize,
        /// Number of runs in the page model.
        runs: usize,
    },
    /// The replacement or insertion text was empty.
    #[error("text must not be empty")]
    EmptyText,
    /// A font size was not finite or not positive.
    #[error("font size must be finite and positive, got {0}")]
    InvalidFontSize(f64),
    /// A rectangle was outside the page or degenerate.
    #[error("invalid edit rectangle: {0}")]
    InvalidRect(&'static str),
    /// The fallback font has no glyph for a character.
    #[error("the fallback font has no glyph for U+{character:04X}")]
    UnsupportedCharacter {
        /// Rejected Unicode scalar.
        character: u32,
    },
    /// An existing font cannot represent the replacement text.
    #[error("font '{font}' cannot encode the replacement text")]
    UnencodableText {
        /// Font resource name.
        font: String,
    },
    /// The content stream was structurally malformed.
    #[error("malformed content stream: {0}")]
    MalformedContent(String),
    /// A page had no usable content or resources.
    #[error("page {0} has no usable content stream")]
    UnsupportedPage(usize),
    /// The PDF object-number space is exhausted.
    #[error("the PDF object number space is exhausted")]
    ObjectSpaceExhausted,
    /// A PNG image was malformed or used an unsupported feature.
    #[error("unsupported PNG image: {0}")]
    Png(String),
    /// A JPEG image was malformed or used an unsupported feature.
    #[error("unsupported JPEG image: {0}")]
    Jpeg(String),
}

/// Replacement payload for [`replace_object`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ObjectReplacement<'a> {
    /// New contents for a text block: reflowed with the block's existing width
    /// and font rules. Line breaks separate paragraphs and are joined by one
    /// space.
    Text(&'a str),
    /// New image bytes for an image XObject. The resource name and the draw's
    /// placement are kept; only the XObject stream and dictionary change.
    Image {
        /// Encoded PNG or JPEG bytes.
        bytes: &'a [u8],
        /// Encoding of `bytes`.
        format: ImageFormat,
    },
}

/// Result of [`replace_object`].
#[derive(Clone, Debug, PartialEq)]
pub struct ObjectReplaceOutcome {
    /// Zero-based page index.
    pub page_index: usize,
    /// Identifier supplied by the caller.
    pub id: String,
    /// Kind of object that was replaced.
    pub kind: ObjectKind,
    /// Reflowed line count for a text block; `None` for an image.
    pub text_lines: Option<usize>,
    /// Decoded pixel width for an image; `None` for text.
    pub pixel_width: Option<u32>,
    /// Decoded pixel height for an image; `None` for text.
    pub pixel_height: Option<u32>,
}

/// Typed failures from the object enumeration and edit API.
#[derive(Debug, Error)]
pub enum ObjectEditError {
    /// The core reader or writer rejected the document.
    #[error(transparent)]
    Core(#[from] pdfxi_core::Error),
    /// The text model or content scanner rejected the page.
    #[error(transparent)]
    Text(#[from] TextEditError),
    /// No object with this identifier exists on the page.
    #[error("page {page} has no object with id '{id}'")]
    ObjectNotFound {
        /// Zero-based page index.
        page: usize,
        /// Identifier supplied by the caller.
        id: String,
    },
    /// The identifier did not use the documented `text:` or `image:` form.
    #[error("object id '{id}' is not valid on page {page}")]
    InvalidObjectId {
        /// Zero-based page index.
        page: usize,
        /// Rejected identifier.
        id: String,
    },
    /// A move delta was not finite.
    #[error("the move delta must be finite")]
    NonFiniteDelta,
    /// A move delta was too large to be a plausible page edit.
    #[error("the move delta is too large: ({dx}, {dy})")]
    DeltaOutOfRange {
        /// Requested horizontal delta.
        dx: f64,
        /// Requested vertical delta.
        dy: f64,
    },
    /// The moved object would no longer overlap its page.
    #[error("the moved object would leave page {page}")]
    MoveOutOfBounds {
        /// Zero-based page index.
        page: usize,
    },
    /// A resize scale was not finite, not positive, or implausibly large.
    #[error("the resize scale must be finite, positive and at most 1000, got ({sx}, {sy})")]
    InvalidScale {
        /// Requested horizontal scale.
        sx: f64,
        /// Requested vertical scale.
        sy: f64,
    },
    /// The resized object would no longer overlap its page.
    #[error("the resized object would leave page {page}")]
    ResizeOutOfBounds {
        /// Zero-based page index.
        page: usize,
    },
    /// The replacement payload does not match the target object's kind.
    #[error("the replacement payload does not match the target object kind")]
    ReplacementKindMismatch,
    /// An image XObject is shared, so replacing it would change content the
    /// caller did not name.
    #[error("image resource '{resource}' is shared; refusing to replace it")]
    SharedImageResource {
        /// Page resource name without the leading slash.
        resource: String,
    },
    /// A coordinate computed for a move was outside the supported range.
    #[error("the moved object's coordinates are outside the supported range")]
    CoordinateOutOfRange,
    /// The text block shares a text object with content that is not in the
    /// block, so shifting or deleting the block would change unrelated text.
    #[error(
        "text block {block_index} shares a text object with other content; refusing to rewrite it"
    )]
    SharedTextObject {
        /// Zero-based block index.
        block_index: usize,
    },
    /// The page content used a construct this round cannot rewrite safely.
    #[error("unsupported page content: {0}")]
    UnsupportedContent(String),
    /// The content stream was structurally malformed.
    #[error("malformed content stream: {0}")]
    MalformedContent(String),
    /// A byte span computed for the rewrite was outside the content stream.
    #[error("content rewrite span {start}..{end} is outside the stream")]
    InvalidSpan {
        /// First byte of the rejected span.
        start: usize,
        /// Byte just past the rejected span.
        end: usize,
    },
    /// The rewritten content stream would exceed the addressable size.
    #[error("the rewritten content stream is too large")]
    ContentTooLarge,
    /// The document's reference graph exceeded the traversal caps.
    #[error("the document reference graph is too large to inspect safely")]
    ReferenceGraphTooLarge,
    /// The document's reference graph was cyclic or deeper than the cap.
    #[error("the document reference graph is malformed")]
    MalformedReferenceGraph,
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Build the geometry-aware text model of one page.
///
/// The model is read-only and derived from the document's current effective
/// objects. Callers normally pass a document returned by a session load or by a
/// previous save/reparse.
pub fn text_blocks(document: &Document, page_index: usize) -> Result<PageTextModel, TextEditError> {
    let page = document.page(page_index)?;
    let fonts = load_fonts(document, &page)?;
    let content = document.page_content(page_index)?;
    let raw = scan_runs(&content, &fonts)?;
    Ok(build_model(page_index, &page, raw))
}

/// Replace the text of one run and reflow its block.
///
/// Every text-showing operator in the block is removed and the whole block is
/// re-emitted at the first run's baseline. The run's own font is reused only
/// when it is a simple, non-subset font that can encode the new text; otherwise
/// the bundled fallback font is embedded and [`ReplaceOutcome::font`] reports
/// it. Other content stream operators are preserved.
pub fn replace_run_text(
    document: &mut Document,
    page_index: usize,
    run_index: usize,
    new_text: &str,
) -> Result<ReplaceOutcome, TextEditError> {
    if new_text.trim().is_empty() {
        return Err(TextEditError::EmptyText);
    }
    let effective = effective_document(document)?;
    let model = text_blocks(&effective, page_index)?;
    let run_count = model.runs().count();
    let block = model
        .block_for_run(run_index)
        .ok_or(TextEditError::RunNotFound {
            index: run_index,
            runs: run_count,
        })?;
    let target = block.run(run_index).ok_or(TextEditError::RunNotFound {
        index: run_index,
        runs: run_count,
    })?;

    // Assemble the paragraph with the target run replaced.
    let mut lines: Vec<String> = Vec::new();
    for line in &block.lines {
        let mut text = String::new();
        for index in &line.run_indices {
            if let Some(run) = block.run(*index) {
                if run.index == run_index {
                    text.push_str(new_text);
                } else {
                    text.push_str(&run.text);
                }
            }
        }
        lines.push(text);
    }
    let paragraph = lines.join(" ");
    let target_width = f64::from(block.width.max(1.0));
    rebuild_block(
        document,
        &effective,
        page_index,
        block,
        target,
        &paragraph,
        new_text,
        target_width,
    )
}

/// Re-emit one block with `paragraph` reflowed at `target_width`.
///
/// This is the shared implementation of [`replace_run_text`] and the text arm
/// of [`replace_object`]. Every show operator of `block` is replaced; the
/// replacement is written at `anchor`'s baseline with `anchor`'s font size.
#[allow(clippy::too_many_arguments)]
fn rebuild_block(
    document: &mut Document,
    effective: &Document,
    page_index: usize,
    block: &TextBlock,
    anchor: &TextRun,
    paragraph: &str,
    reported_text: &str,
    target_width: f64,
) -> Result<ReplaceOutcome, TextEditError> {
    let fonts = load_fonts(effective, &effective.page(page_index)?)?;
    let existing = fonts.get(&anchor.font);

    let reuse = existing.is_some_and(|font| font.can_encode(paragraph, reported_text));
    let metrics;
    let mut choice;
    if reuse {
        let font = existing.ok_or(TextEditError::UnencodableText {
            font: anchor.font.clone(),
        })?;
        metrics = LayoutMetrics::Existing(font.clone(), anchor.font_size);
        choice = FontChoice {
            resource: anchor.font.clone(),
            base_font: font.base_font.clone(),
            embedded: false,
        };
    } else {
        let ttf = fallback_font()?;
        let chars: BTreeSet<char> = paragraph
            .chars()
            .filter(|character| !character.is_control())
            .collect();
        let fallback = FallbackFont::new(ttf, &chars)?;
        metrics = LayoutMetrics::Fallback(fallback, anchor.font_size);
        choice = FontChoice {
            resource: String::new(),
            base_font: FALLBACK_BASE_FONT.to_owned(),
            embedded: true,
        };
    }

    let options = ReflowOptions {
        target_width,
        preserve_leading_whitespace: false,
        preserve_trailing_whitespace: false,
    };
    let reflowed = {
        let measure = |character: char| metrics.advance(character);
        let adapter = crate::paragraph_edit::FnMetrics::new(measure);
        reflow_paragraph(paragraph, &options, &adapter, &EditLimits::default())?
    };
    if reflowed.is_empty() {
        return Err(TextEditError::EmptyText);
    }

    // Stage the content stream and any resources the chosen font needs.
    let mut editor = PageEditor::new(effective, page_index)?;
    if reuse {
        choice.resource = anchor.font.clone();
    } else {
        let font_id = embed_fallback_font(document, &mut editor, &metrics)?;
        choice.resource = editor
            .resource_name_for_font(font_id)
            .ok_or(TextEditError::UnsupportedPage(page_index))?
            .to_owned();
    }

    let start_x = f64::from(anchor.start.x);
    let first_baseline = f64::from(anchor.start.y);
    let line_step = anchor.font_size * LINE_HEIGHT_FACTOR;
    let mut generated = String::new();
    generated.push_str(&format!(
        "0 Tc 0 Tw 100 Tz 0 Ts 0 g /{} {:.4} Tf ",
        choice.resource, anchor.font_size
    ));
    for (line_index, line) in reflowed.iter().enumerate() {
        let baseline = first_baseline - line_index as f64 * line_step;
        let mut shown = line.text.clone();
        if line_index + 1 < reflowed.len() {
            shown.push(' ');
        }
        let encoded = encode_line(&metrics, &shown)?;
        generated.push_str(&format!(
            "1 0 0 1 {:.4} {:.4} Tm <{}> Tj ",
            start_x,
            baseline,
            hex_encode(&encoded)
        ));
    }

    let spans: Vec<(usize, usize)> = block
        .runs
        .iter()
        .map(|run| (run.show_start, run.show_end))
        .collect();
    let content = effective.page_content(page_index)?;
    let spliced = splice_spans(&content, &spans, generated.as_bytes());
    let stream_id = allocate_ids(document, 1)?
        .first()
        .copied()
        .ok_or(TextEditError::ObjectSpaceExhausted)?;
    document.set_object(
        stream_id,
        Object::Stream {
            dictionary: Dictionary::new(),
            data: spliced,
        },
    )?;
    editor.set_contents(vec![stream_id]);
    editor.commit(document)?;

    Ok(ReplaceOutcome {
        page_index,
        block_index: block.index,
        new_text: reported_text.to_owned(),
        lines: reflowed.len(),
        font: choice,
    })
}

/// Insert a wrapped text box with a real appearance and the embedded fallback font.
///
/// Newlines in `text` start a new line; each paragraph is greedily wrapped to
/// the box width. The text is appended as a new content stream so existing page
/// content is untouched.
pub fn add_text_box(
    document: &mut Document,
    page_index: usize,
    rect: Rect,
    text: &str,
    font_size: f64,
    align: TextAlign,
) -> Result<TextPlacement, TextEditError> {
    if text.trim().is_empty() {
        return Err(TextEditError::EmptyText);
    }
    if !font_size.is_finite() || font_size <= 0.0 {
        return Err(TextEditError::InvalidFontSize(font_size));
    }
    if rect.width() <= 1.0 || rect.height() <= 1.0 {
        return Err(TextEditError::InvalidRect("rectangle is degenerate"));
    }
    let effective = effective_document(document)?;
    let page = effective.page(page_index)?;
    if rect.x0() < page.media_box.left as f32
        || rect.y0() < page.media_box.bottom as f32
        || rect.x1() > page.media_box.right as f32
        || rect.y1() > page.media_box.top as f32
    {
        return Err(TextEditError::InvalidRect("rectangle leaves the page"));
    }

    let ttf = fallback_font()?;
    let chars: BTreeSet<char> = text
        .chars()
        .filter(|character| !character.is_control())
        .collect();
    let fallback = FallbackFont::new(ttf, &chars)?;
    let metrics = LayoutMetrics::Fallback(fallback, font_size);
    let mut editor = PageEditor::new(&effective, page_index)?;
    let mut laid_out: Vec<(f64, String)> = Vec::new();
    let mut baseline = f64::from(rect.y1()) - font_size;
    for paragraph in text.split('\n') {
        let options = ReflowOptions {
            target_width: f64::from(rect.width()),
            preserve_leading_whitespace: false,
            preserve_trailing_whitespace: false,
        };
        let measure = |character: char| metrics.advance(character);
        let adapter = crate::paragraph_edit::FnMetrics::new(measure);
        let lines = reflow_paragraph(paragraph, &options, &adapter, &EditLimits::default())?;
        for line in lines {
            laid_out.push((baseline, line.text));
            baseline -= font_size * LINE_HEIGHT_FACTOR;
        }
    }
    if laid_out.is_empty() {
        return Err(TextEditError::EmptyText);
    }
    let font_id = embed_fallback_font(document, &mut editor, &metrics)?;
    let resource = editor
        .resource_name_for_font(font_id)
        .ok_or(TextEditError::UnsupportedPage(page_index))?
        .to_owned();

    let mut content = String::new();
    content.push_str("q\nBT\n0 g\n");
    content.push_str(&format!("/{} {:.4} Tf\n", resource, font_size));
    for (index, (line_baseline, line)) in laid_out.iter().enumerate() {
        let width = metrics.measure(line);
        let x = match align {
            TextAlign::Left => f64::from(rect.x0()),
            TextAlign::Center => {
                f64::from(rect.x0()) + (f64::from(rect.width()) - width).max(0.0) / 2.0
            }
            TextAlign::Right => f64::from(rect.x1()) - width.min(f64::from(rect.width())),
        };
        let mut shown = line.clone();
        if index + 1 < laid_out.len() {
            shown.push(' ');
        }
        let encoded = encode_line(&metrics, &shown)?;
        content.push_str(&format!(
            "1 0 0 1 {x:.4} {line_baseline:.4} Tm <{}> Tj\n",
            hex_encode(&encoded)
        ));
    }
    content.push_str("ET\nQ\n");

    let stream_id = allocate_ids(document, 1)?
        .first()
        .copied()
        .ok_or(TextEditError::ObjectSpaceExhausted)?;
    document.set_object(
        stream_id,
        Object::Stream {
            dictionary: Dictionary::new(),
            data: content.into_bytes(),
        },
    )?;
    let mut contents = effective.page(page_index)?.contents.clone();
    contents.push(stream_id);
    editor.set_contents(contents);
    editor.commit(document)?;

    Ok(TextPlacement {
        page_index,
        rect,
        lines: laid_out.len(),
        font: FontChoice {
            resource,
            base_font: FALLBACK_BASE_FONT.to_owned(),
            embedded: true,
        },
    })
}

/// Decode an image and place it in `rect` as an image XObject.
///
/// PNG is decoded to eight-bit greyscale or RGB samples (an alpha channel is
/// ignored for round A) and stored unfiltered. JPEG bytes are embedded
/// unchanged with `/Filter /DCTDecode`. The image is appended as a new content
/// stream and a new `/XObject` resource.
pub fn add_image(
    document: &mut Document,
    page_index: usize,
    rect: Rect,
    image_bytes: &[u8],
    format: ImageFormat,
) -> Result<ImagePlacement, TextEditError> {
    if rect.width() <= 1.0 || rect.height() <= 1.0 {
        return Err(TextEditError::InvalidRect("rectangle is degenerate"));
    }
    let effective = effective_document(document)?;
    let page = effective.page(page_index)?;
    if rect.x0() < page.media_box.left as f32
        || rect.y0() < page.media_box.bottom as f32
        || rect.x1() > page.media_box.right as f32
        || rect.y1() > page.media_box.top as f32
    {
        return Err(TextEditError::InvalidRect("rectangle leaves the page"));
    }

    let image = match format {
        ImageFormat::Png => ImageData::from_png(image_bytes)?,
        ImageFormat::Jpeg => ImageData::from_jpeg(image_bytes)?,
    };
    let dictionary = image_stream_dictionary(&image);
    let image_id = allocate_ids(document, 1)?
        .first()
        .copied()
        .ok_or(TextEditError::ObjectSpaceExhausted)?;
    document.set_object(
        image_id,
        Object::Stream {
            dictionary,
            data: image.data,
        },
    )?;

    let mut editor = PageEditor::new(&effective, page_index)?;
    let resource = editor.ensure_xobject(image_id)?;
    let content = format!(
        "q\n{:.4} 0 0 {:.4} {:.4} {:.4} cm\n/{} Do\nQ\n",
        rect.width(),
        rect.height(),
        rect.x0(),
        rect.y0(),
        resource
    );
    let stream_id = allocate_ids(document, 1)?
        .first()
        .copied()
        .ok_or(TextEditError::ObjectSpaceExhausted)?;
    document.set_object(
        stream_id,
        Object::Stream {
            dictionary: Dictionary::new(),
            data: content.into_bytes(),
        },
    )?;
    let mut contents = page.contents.clone();
    contents.push(stream_id);
    editor.set_contents(contents);
    editor.commit(document)?;

    Ok(ImagePlacement {
        page_index,
        rect,
        pixel_width: image.width,
        pixel_height: image.height,
        resource,
    })
}

// ---------------------------------------------------------------------------
// Object enumeration, move, and delete
// ---------------------------------------------------------------------------

/// Maximum absolute move delta in default user space units.
const MAX_MOVE_DELTA: f64 = 1_000_000.0;
/// Maximum accepted resize scale in either axis.
const MAX_SCALE: f64 = 1000.0;
/// Maximum coordinate accepted after a move.
const MAX_COORDINATE: f64 = 1_000_000_000.0;
/// Maximum depth walked while counting references to an image XObject.
const MAX_REFERENCE_DEPTH: usize = 64;
/// Maximum number of objects walked while counting image references.
const MAX_REFERENCE_OBJECTS: usize = 200_000;
/// Longest text preview returned by [`list_objects`], in characters.
const MAX_PREVIEW_CHARS: usize = 80;

/// List the selectable text blocks and image XObjects of one page.
///
/// Text blocks come first, in [`PageTextModel`] reading order, followed by the
/// image XObjects drawn directly by the page content stream in content order.
/// Images inside a `/Form` XObject and inline images are not enumerated in this
/// round; `Do` operators whose resource is not an image are ignored.
///
/// The returned rectangles are in default user space. A text rectangle is the
/// union of its run rectangles; an image rectangle is the image's unit square
/// transformed by the content-stream matrix in effect at its `Do` operator.
///
/// Identifiers are derived from the current page state; see [`PageObject`] for
/// the exact stability contract. Re-list after every edit.
pub fn list_objects(
    document: &Document,
    page_index: usize,
) -> Result<Vec<PageObject>, ObjectEditError> {
    let effective = effective_document(document)?;
    let model = text_blocks(&effective, page_index)?;
    let mut objects = Vec::with_capacity(model.blocks.len());
    for block in &model.blocks {
        objects.push(PageObject {
            page: page_index,
            id: text_object_id(block.index),
            kind: ObjectKind::TextBlock,
            rect: block.rect,
            text_preview: block_preview(block),
        });
    }
    for image in scan_images(&effective, page_index)? {
        objects.push(PageObject {
            page: page_index,
            id: image.id(),
            kind: ObjectKind::Image,
            rect: image.rect,
            text_preview: String::new(),
        });
    }
    Ok(objects)
}

/// Move one existing text block or image XObject by `(dx, dy)`.
///
/// # Text blocks
///
/// The block's placement operators are rewritten in place: every `Tm` that
/// positions the block is translated, and a block positioned only by relative
/// `Td`/`TD` operators or by the default text matrix gets one translated `Td`.
/// The text, fonts, colours, clips, marked content and every other block are
/// left byte-identical. A block whose text object also contains visible text
/// outside the block is rejected with [`ObjectEditError::SharedTextObject`]
/// rather than risking unrelated content.
///
/// # Images
///
/// The translation is added to the `cm` that places the image immediately
/// before its `Do`, inside the draw's existing `q`/`Q` pair. Only that draw
/// moves; other images, including other usages of the same XObject, keep their
/// placement. A draw inside a text object, without its own `q`/`Q` pair, sharing
/// its placement matrix with another draw, or under a scaled or rotated parent
/// transform is rejected as unsupported rather than moved by an approximate
/// amount.
///
/// A move that would leave the page entirely is rejected with
/// [`ObjectEditError::MoveOutOfBounds`]. Both coordinates of the delta must be
/// finite; their absolute values may not exceed 1,000,000 units.
pub fn move_object(
    document: &mut Document,
    page_index: usize,
    id: &str,
    dx: f64,
    dy: f64,
) -> Result<(), ObjectEditError> {
    validate_delta(dx, dy)?;
    let parsed = parse_object_id(id, page_index)?;
    let effective = effective_document(document)?;
    let page = effective.page(page_index)?;
    let media_box = page_media_box(&page);
    match parsed {
        ParsedObjectId::Text(block_index) => move_text_block(
            document,
            &effective,
            page_index,
            media_box,
            block_index,
            dx,
            dy,
        ),
        ParsedObjectId::Image {
            resource,
            occurrence,
        } => move_image(
            document,
            &effective,
            page_index,
            media_box,
            (&resource, occurrence),
            dx,
            dy,
        ),
    }
}

/// Resize one existing text block or image XObject by `(sx, sy)` about the
/// object's lower-left origin.
///
/// # Text blocks
///
/// Text keeps its font resource and stays extractable. Every `Tf` in the
/// block's text objects is scaled to `size * sy`, every `Tm` horizontal term is
/// corrected by `sx / sy` (the larger font already scales advance by `sy`),
/// `Tm` translations are mapped about the block origin, and `Td`/`TD` operands
/// are multiplied by `sy`. Glyphs, advances, line pitch and the reported block
/// rectangle therefore scale by `(sx, sy)` while other blocks stay
/// byte-identical.
///
/// # Images
///
/// The placement `cm` is rewritten in place. For `[a b c d e f]` the axes become
/// `[a*sx b*sx c*sy d*sy]` and the translation `e f` is kept, so an ordinary
/// axis-aligned image box scales about its lower-left origin. Rotation or shear
/// already present in the matrix is kept on the same axis vectors; a uniform
/// scale preserves the angle, while an anisotropic scale changes it exactly as
/// scaling the box would.
///
/// # Fail-closed rules
///
/// Shared text objects are rejected with [`ObjectEditError::SharedTextObject`].
/// Rotated or relative-only text, text without a `Tf`, an image draw inside a
/// text object, an image without its own placement matrix, a shared placement,
/// an image outside a `q`/`Q` pair and a non-identity parent transform are
/// [`ObjectEditError::UnsupportedContent`]. Both scales must be finite and in
/// `(0, 1000]`; a resize that would leave the page entirely is
/// [`ObjectEditError::ResizeOutOfBounds`].
pub fn resize_object(
    document: &mut Document,
    page_index: usize,
    id: &str,
    sx: f64,
    sy: f64,
) -> Result<(), ObjectEditError> {
    validate_scale(sx, sy)?;
    let parsed = parse_object_id(id, page_index)?;
    let effective = effective_document(document)?;
    let page = effective.page(page_index)?;
    let media_box = page_media_box(&page);
    match parsed {
        ParsedObjectId::Text(block_index) => resize_text_block(
            document,
            &effective,
            page_index,
            media_box,
            block_index,
            sx,
            sy,
        ),
        ParsedObjectId::Image {
            resource,
            occurrence,
        } => resize_image(
            document,
            &effective,
            page_index,
            media_box,
            (&resource, occurrence),
            sx,
            sy,
        ),
    }
}

/// Replace the contents of one existing text block or image XObject.
///
/// # Text blocks
///
/// The whole block is rebuilt through the T-0233 reflow and font path at its
/// existing width. Line breaks in the replacement separate paragraphs and are
/// joined by one space. The block's first run is the anchor for baseline and
/// font size; the run's own font is reused only when it can encode the new
/// text, otherwise the bundled fallback font is embedded.
///
/// # Images
///
/// The new PNG or JPEG is decoded and written into the existing image XObject:
/// the resource name and the `Do` placement stay byte-identical, while
/// `Width`, `Height`, `ColorSpace`, `BitsPerComponent`, `/Filter` and the pixel
/// bytes come from the replacement.
///
/// # Fail-closed rules
///
/// A payload whose variant does not match the target's kind is
/// [`ObjectEditError::ReplacementKindMismatch`]. An image whose XObject has
/// more than one usage or reference, or whose page resource / XObject
/// dictionary is shared with another page, is
/// [`ObjectEditError::SharedImageResource`]. An image stored directly in the
/// resource dictionary has no stable object to replace and is
/// [`ObjectEditError::UnsupportedContent`]. Decoder failures surface as
/// [`TextEditError::Png`] / [`TextEditError::Jpeg`] through
/// [`ObjectEditError::Text`].
pub fn replace_object(
    document: &mut Document,
    page_index: usize,
    id: &str,
    replacement: ObjectReplacement<'_>,
) -> Result<ObjectReplaceOutcome, ObjectEditError> {
    let parsed = parse_object_id(id, page_index)?;
    let effective = effective_document(document)?;
    match (parsed, replacement) {
        (ParsedObjectId::Text(block_index), ObjectReplacement::Text(text)) => {
            let outcome = replace_text_block(document, &effective, page_index, block_index, text)?;
            Ok(ObjectReplaceOutcome {
                page_index,
                id: id.to_owned(),
                kind: ObjectKind::TextBlock,
                text_lines: Some(outcome.lines),
                pixel_width: None,
                pixel_height: None,
            })
        }
        (
            ParsedObjectId::Image {
                resource,
                occurrence,
            },
            ObjectReplacement::Image { bytes, format },
        ) => {
            let (pixel_width, pixel_height) = replace_image(
                document, &effective, page_index, &resource, occurrence, bytes, format,
            )?;
            Ok(ObjectReplaceOutcome {
                page_index,
                id: id.to_owned(),
                kind: ObjectKind::Image,
                text_lines: None,
                pixel_width: Some(pixel_width),
                pixel_height: Some(pixel_height),
            })
        }
        _ => Err(ObjectEditError::ReplacementKindMismatch),
    }
}

/// Delete one existing text block or image XObject.
///
/// # Text blocks
///
/// Every text object fully owned by the block is removed, including its `BT`
/// and `ET` operators, so no show or positioning operator from the block
/// survives. Surrounding marked content, graphics state, clips and every other
/// block are preserved. A shared text object is rejected with
/// [`ObjectEditError::SharedTextObject`].
///
/// # Images
///
/// The `Do` usage is removed. When this was the only usage of the image XObject
/// in the document and neither the page resources nor the `/XObject`
/// dictionary is shared with another page, the resource entry is removed and
/// the XObject is replaced with `null`, leaving no orphan object. A shared
/// image keeps its object and resource entry; only this page's usage is
/// removed.
///
/// Deleting a text block does not remove its font resources, because a font can
/// be shared with content that is still present.
pub fn delete_object(
    document: &mut Document,
    page_index: usize,
    id: &str,
) -> Result<(), ObjectEditError> {
    let parsed = parse_object_id(id, page_index)?;
    let effective = effective_document(document)?;
    match parsed {
        ParsedObjectId::Text(block_index) => {
            delete_text_block(document, &effective, page_index, block_index)
        }
        ParsedObjectId::Image {
            resource,
            occurrence,
        } => delete_image(document, &effective, page_index, &resource, occurrence),
    }
}

/// A parsed object identifier.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ParsedObjectId {
    /// A text block index.
    Text(usize),
    /// An image resource name and occurrence index.
    Image {
        /// Resource name without the leading slash.
        resource: String,
        /// Zero-based occurrence of a `Do` naming the resource.
        occurrence: usize,
    },
}

/// Build the stable identifier of a text block.
fn text_object_id(block_index: usize) -> String {
    format!("text:{block_index}")
}

/// Build the stable identifier of an image usage.
fn image_object_id(resource: &str, occurrence: usize) -> String {
    format!("image:{resource}:{occurrence}")
}

/// Parse an object identifier, rejecting unknown prefixes and empty names.
fn parse_object_id(id: &str, page_index: usize) -> Result<ParsedObjectId, ObjectEditError> {
    if let Some(index) = id.strip_prefix("text:") {
        let block_index = index
            .parse::<usize>()
            .map_err(|_| ObjectEditError::InvalidObjectId {
                page: page_index,
                id: id.to_owned(),
            })?;
        return Ok(ParsedObjectId::Text(block_index));
    }
    if let Some(rest) = id.strip_prefix("image:") {
        let (resource, occurrence) =
            rest.rsplit_once(':')
                .ok_or_else(|| ObjectEditError::InvalidObjectId {
                    page: page_index,
                    id: id.to_owned(),
                })?;
        if resource.is_empty() {
            return Err(ObjectEditError::InvalidObjectId {
                page: page_index,
                id: id.to_owned(),
            });
        }
        let occurrence =
            occurrence
                .parse::<usize>()
                .map_err(|_| ObjectEditError::InvalidObjectId {
                    page: page_index,
                    id: id.to_owned(),
                })?;
        return Ok(ParsedObjectId::Image {
            resource: resource.to_owned(),
            occurrence,
        });
    }
    Err(ObjectEditError::InvalidObjectId {
        page: page_index,
        id: id.to_owned(),
    })
}

/// Build a bounded single-line preview from a text block's runs.
fn block_preview(block: &TextBlock) -> String {
    let joined = block
        .runs
        .iter()
        .map(|run| run.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let mut preview = String::new();
    for (index, character) in joined.chars().enumerate() {
        if index == MAX_PREVIEW_CHARS {
            preview.push('…');
            break;
        }
        preview.push(character);
    }
    preview
}

/// Convert a core page's media box into the edit crate's rectangle.
fn page_media_box(page: &pdfxi_core::Page) -> Rect {
    Rect::saturating(
        page.media_box.left as f32,
        page.media_box.bottom as f32,
        page.media_box.right as f32,
        page.media_box.top as f32,
    )
}

/// Reject a move delta that is not finite or is implausibly large.
fn validate_delta(dx: f64, dy: f64) -> Result<(), ObjectEditError> {
    if !dx.is_finite() || !dy.is_finite() {
        return Err(ObjectEditError::NonFiniteDelta);
    }
    if dx.abs() > MAX_MOVE_DELTA || dy.abs() > MAX_MOVE_DELTA {
        return Err(ObjectEditError::DeltaOutOfRange { dx, dy });
    }
    Ok(())
}

/// Translate a rectangle, rejecting non-finite or out-of-range coordinates.
fn translate_rect(rect: Rect, dx: f64, dy: f64) -> Result<Rect, ObjectEditError> {
    let coordinates = [
        f64::from(rect.x0()) + dx,
        f64::from(rect.y0()) + dy,
        f64::from(rect.x1()) + dx,
        f64::from(rect.y1()) + dy,
    ];
    if coordinates
        .iter()
        .any(|value| !value.is_finite() || value.abs() > MAX_COORDINATE)
    {
        return Err(ObjectEditError::CoordinateOutOfRange);
    }
    Ok(Rect::saturating(
        coordinates[0] as f32,
        coordinates[1] as f32,
        coordinates[2] as f32,
        coordinates[3] as f32,
    ))
}

/// Return `true` when any part of `rect` lies on the page.
fn rect_on_page(media_box: Rect, rect: Rect) -> bool {
    media_box.intersects(&rect) || media_box.contains_rect(&rect)
}

/// Reject a resize scale that is not finite, positive or within range.
fn validate_scale(sx: f64, sy: f64) -> Result<(), ObjectEditError> {
    if !sx.is_finite()
        || !sy.is_finite()
        || sx <= 0.0
        || sy <= 0.0
        || sx > MAX_SCALE
        || sy > MAX_SCALE
    {
        return Err(ObjectEditError::InvalidScale { sx, sy });
    }
    Ok(())
}

/// Scale `rect` about `(origin_x, origin_y)` in default user space.
fn scale_rect_about(
    rect: Rect,
    origin_x: f64,
    origin_y: f64,
    sx: f64,
    sy: f64,
) -> Result<Rect, ObjectEditError> {
    let coordinates = [
        origin_x,
        origin_y,
        origin_x + (f64::from(rect.x1()) - origin_x) * sx,
        origin_y + (f64::from(rect.y1()) - origin_y) * sy,
    ];
    if coordinates
        .iter()
        .any(|value| !value.is_finite() || value.abs() > MAX_COORDINATE)
    {
        return Err(ObjectEditError::CoordinateOutOfRange);
    }
    Ok(Rect::saturating(
        coordinates[0] as f32,
        coordinates[1] as f32,
        coordinates[2] as f32,
        coordinates[3] as f32,
    ))
}

/// Rewrite one page's content into a new unfiltered stream.
fn rewrite_page_content(
    document: &mut Document,
    effective: &Document,
    page_index: usize,
    content: Vec<u8>,
) -> Result<(), ObjectEditError> {
    let mut editor = PageEditor::new(effective, page_index)?;
    let stream_id = allocate_ids(document, 1)?
        .first()
        .copied()
        .ok_or(TextEditError::ObjectSpaceExhausted)?;
    document.set_object(
        stream_id,
        Object::Stream {
            dictionary: Dictionary::new(),
            data: content,
        },
    )?;
    editor.set_contents(vec![stream_id]);
    editor.commit(document)?;
    Ok(())
}

/// Move a text block by rewriting only the placement operators that own it.
fn move_text_block(
    document: &mut Document,
    effective: &Document,
    page_index: usize,
    media_box: Rect,
    block_index: usize,
    dx: f64,
    dy: f64,
) -> Result<(), ObjectEditError> {
    let model = text_blocks(effective, page_index)?;
    let block = model
        .blocks
        .iter()
        .find(|block| block.index == block_index)
        .ok_or_else(|| ObjectEditError::ObjectNotFound {
            page: page_index,
            id: text_object_id(block_index),
        })?;
    let moved = translate_rect(block.rect, dx, dy)?;
    if !rect_on_page(media_box, moved) {
        return Err(ObjectEditError::MoveOutOfBounds { page: page_index });
    }

    let content = effective.page_content(page_index)?;
    let fonts = load_fonts(effective, &effective.page(page_index)?)?;
    let text_objects = scan_text_objects(&content, &fonts, &model)?;
    let edits = plan_text_move(&text_objects, block_index, dx, dy)?;
    let rewritten = apply_edits(&content, edits)?;
    rewrite_page_content(document, effective, page_index, rewritten)
}

/// Resize a text block about its rectangle origin.
fn resize_text_block(
    document: &mut Document,
    effective: &Document,
    page_index: usize,
    media_box: Rect,
    block_index: usize,
    sx: f64,
    sy: f64,
) -> Result<(), ObjectEditError> {
    let model = text_blocks(effective, page_index)?;
    let block = model
        .blocks
        .iter()
        .find(|block| block.index == block_index)
        .ok_or_else(|| ObjectEditError::ObjectNotFound {
            page: page_index,
            id: text_object_id(block_index),
        })?;
    let origin_x = f64::from(block.rect.x0());
    let origin_y = f64::from(block.rect.y0());
    let scaled = scale_rect_about(block.rect, origin_x, origin_y, sx, sy)?;
    if !rect_on_page(media_box, scaled) {
        return Err(ObjectEditError::ResizeOutOfBounds { page: page_index });
    }

    let content = effective.page_content(page_index)?;
    let fonts = load_fonts(effective, &effective.page(page_index)?)?;
    let text_objects = scan_text_objects(&content, &fonts, &model)?;
    let edits = plan_text_resize(&text_objects, block_index, (origin_x, origin_y), sx, sy)?;
    let rewritten = apply_edits(&content, edits)?;
    rewrite_page_content(document, effective, page_index, rewritten)
}

/// Move one image usage by wrapping that `Do` in a translated graphics state.
fn move_image(
    document: &mut Document,
    effective: &Document,
    page_index: usize,
    media_box: Rect,
    target: (&str, usize),
    dx: f64,
    dy: f64,
) -> Result<(), ObjectEditError> {
    let (resource, occurrence) = target;
    let images = scan_images(effective, page_index)?;
    let image = images
        .iter()
        .find(|image| image.resource == resource && image.occurrence == occurrence)
        .ok_or_else(|| ObjectEditError::ObjectNotFound {
            page: page_index,
            id: image_object_id(resource, occurrence),
        })?;
    if image.in_text_object {
        return Err(ObjectEditError::UnsupportedContent(
            "an image draw inside a text object cannot be moved safely".to_owned(),
        ));
    }
    let Some(placement) = image.placement_cm.as_ref() else {
        return Err(ObjectEditError::UnsupportedContent(
            "an image without its own placement matrix cannot be moved safely".to_owned(),
        ));
    };
    if image.cm_shared {
        return Err(ObjectEditError::UnsupportedContent(
            "the image shares its placement matrix with another draw".to_owned(),
        ));
    }
    if !image.wrapped {
        return Err(ObjectEditError::UnsupportedContent(
            "an image outside a q/Q pair cannot be moved safely".to_owned(),
        ));
    }
    if !linear_identity(placement.prior_ctm) {
        return Err(ObjectEditError::UnsupportedContent(
            "an image under a scaled or rotated parent transform cannot be moved safely".to_owned(),
        ));
    }
    let moved = translate_rect(image.rect, dx, dy)?;
    if !rect_on_page(media_box, moved) {
        return Err(ObjectEditError::MoveOutOfBounds { page: page_index });
    }

    let content = effective.page_content(page_index)?;
    let values = placement.values;
    let replacement = format!(
        "{} {} {} {} {} {} cm",
        format_number(values[0])?,
        format_number(values[1])?,
        format_number(values[2])?,
        format_number(values[3])?,
        format_number(values[4] + dx)?,
        format_number(values[5] + dy)?,
    );
    let edits = vec![ByteEdit {
        start: placement.start,
        end: placement.end,
        replacement: replacement.into_bytes(),
    }];
    let rewritten = apply_edits(&content, edits)?;
    rewrite_page_content(document, effective, page_index, rewritten)
}

/// Resize one image usage by scaling its placement matrix about its origin.
fn resize_image(
    document: &mut Document,
    effective: &Document,
    page_index: usize,
    media_box: Rect,
    target: (&str, usize),
    sx: f64,
    sy: f64,
) -> Result<(), ObjectEditError> {
    let (resource, occurrence) = target;
    let images = scan_images(effective, page_index)?;
    let image = images
        .iter()
        .find(|image| image.resource == resource && image.occurrence == occurrence)
        .ok_or_else(|| ObjectEditError::ObjectNotFound {
            page: page_index,
            id: image_object_id(resource, occurrence),
        })?;
    if image.in_text_object {
        return Err(ObjectEditError::UnsupportedContent(
            "an image draw inside a text object cannot be resized safely".to_owned(),
        ));
    }
    let Some(placement) = image.placement_cm.as_ref() else {
        return Err(ObjectEditError::UnsupportedContent(
            "an image without its own placement matrix cannot be resized safely".to_owned(),
        ));
    };
    if image.cm_shared {
        return Err(ObjectEditError::UnsupportedContent(
            "the image shares its placement matrix with another draw".to_owned(),
        ));
    }
    if !image.wrapped {
        return Err(ObjectEditError::UnsupportedContent(
            "an image outside a q/Q pair cannot be resized safely".to_owned(),
        ));
    }
    if !linear_identity(placement.prior_ctm) {
        return Err(ObjectEditError::UnsupportedContent(
            "an image under a scaled or rotated parent transform cannot be resized safely"
                .to_owned(),
        ));
    }

    let values = placement.values;
    let scaled = [
        values[0] * sx,
        values[1] * sx,
        values[2] * sy,
        values[3] * sy,
        values[4],
        values[5],
    ];
    if scaled
        .iter()
        .any(|value| !value.is_finite() || value.abs() > MAX_COORDINATE)
    {
        return Err(ObjectEditError::CoordinateOutOfRange);
    }
    let new_ctm = Matrix::multiply(Matrix(scaled), placement.prior_ctm);
    let rect = unit_square_rect(new_ctm);
    let coordinates = [
        f64::from(rect.x0()),
        f64::from(rect.y0()),
        f64::from(rect.x1()),
        f64::from(rect.y1()),
    ];
    if coordinates
        .iter()
        .any(|value| !value.is_finite() || value.abs() > MAX_COORDINATE)
    {
        return Err(ObjectEditError::CoordinateOutOfRange);
    }
    if !rect_on_page(media_box, rect) {
        return Err(ObjectEditError::ResizeOutOfBounds { page: page_index });
    }

    let content = effective.page_content(page_index)?;
    let replacement = format!(
        "{} {} {} {} {} {} cm",
        format_number(scaled[0])?,
        format_number(scaled[1])?,
        format_number(scaled[2])?,
        format_number(scaled[3])?,
        format_number(scaled[4])?,
        format_number(scaled[5])?,
    );
    let edits = vec![ByteEdit {
        start: placement.start,
        end: placement.end,
        replacement: replacement.into_bytes(),
    }];
    let rewritten = apply_edits(&content, edits)?;
    rewrite_page_content(document, effective, page_index, rewritten)
}

/// Delete a text block by removing every text object it fully owns.
fn delete_text_block(
    document: &mut Document,
    effective: &Document,
    page_index: usize,
    block_index: usize,
) -> Result<(), ObjectEditError> {
    let model = text_blocks(effective, page_index)?;
    if !model.blocks.iter().any(|block| block.index == block_index) {
        return Err(ObjectEditError::ObjectNotFound {
            page: page_index,
            id: text_object_id(block_index),
        });
    }
    let content = effective.page_content(page_index)?;
    let fonts = load_fonts(effective, &effective.page(page_index)?)?;
    let text_objects = scan_text_objects(&content, &fonts, &model)?;
    let targets: Vec<&TextObjectInfo> = text_objects
        .iter()
        .filter(|object| object.owners.contains(&block_index))
        .collect();
    if targets.is_empty() {
        return Err(ObjectEditError::UnsupportedContent(
            "the text block has no locatable text object".to_owned(),
        ));
    }
    for object in &targets {
        if object.owners.len() > 1 || object.owners.contains(&usize::MAX) {
            return Err(ObjectEditError::SharedTextObject { block_index });
        }
    }
    let mut edits = Vec::with_capacity(targets.len());
    for object in targets {
        edits.push(ByteEdit {
            start: object.bt_start,
            end: object.et_end,
            replacement: Vec::new(),
        });
    }
    let rewritten = apply_edits(&content, edits)?;
    rewrite_page_content(document, effective, page_index, rewritten)
}

/// Delete one image usage and, when unshared, its resource entry and object.
fn delete_image(
    document: &mut Document,
    effective: &Document,
    page_index: usize,
    resource: &str,
    occurrence: usize,
) -> Result<(), ObjectEditError> {
    let images = scan_images(effective, page_index)?;
    let image = images
        .iter()
        .find(|image| image.resource == resource && image.occurrence == occurrence)
        .ok_or_else(|| ObjectEditError::ObjectNotFound {
            page: page_index,
            id: image_object_id(resource, occurrence),
        })?;
    if image.in_text_object {
        return Err(ObjectEditError::UnsupportedContent(
            "an image draw inside a text object cannot be deleted safely".to_owned(),
        ));
    }

    let content = effective.page_content(page_index)?;
    let rewritten = apply_edits(
        &content,
        vec![ByteEdit {
            start: image.do_start,
            end: image.do_end,
            replacement: Vec::new(),
        }],
    )?;

    let mut editor = PageEditor::new(effective, page_index)?;
    let mut detach = image.object.is_none();
    if let Some(image_id) = image.object {
        let references = reference_count(effective, image_id)?;
        let usages = image_usage_count(effective, image_id)?;
        let shared_dictionaries = page_resource_dictionary_shared(effective, page_index)?;
        detach = references <= 1 && usages <= 1 && !shared_dictionaries;
    }
    if detach {
        editor.remove_xobject(resource);
    }

    let stream_id = allocate_ids(document, 1)?
        .first()
        .copied()
        .ok_or(TextEditError::ObjectSpaceExhausted)?;
    document.set_object(
        stream_id,
        Object::Stream {
            dictionary: Dictionary::new(),
            data: rewritten,
        },
    )?;
    editor.set_contents(vec![stream_id]);
    editor.commit(document)?;
    if detach && let Some(image_id) = image.object {
        document.set_object(image_id, Object::Null)?;
    }
    Ok(())
}

/// Replace one text block through the shared reflow and font path.
fn replace_text_block(
    document: &mut Document,
    effective: &Document,
    page_index: usize,
    block_index: usize,
    text: &str,
) -> Result<ReplaceOutcome, ObjectEditError> {
    if text.trim().is_empty() {
        return Err(TextEditError::EmptyText.into());
    }
    let paragraph = text
        .split(['\n', '\r'])
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if paragraph.is_empty() {
        return Err(TextEditError::EmptyText.into());
    }

    let model = text_blocks(effective, page_index)?;
    let block = model
        .blocks
        .iter()
        .find(|block| block.index == block_index)
        .ok_or_else(|| ObjectEditError::ObjectNotFound {
            page: page_index,
            id: text_object_id(block_index),
        })?;
    let anchor = block.runs.first().ok_or_else(|| {
        ObjectEditError::UnsupportedContent("the text block has no runs to rebuild".to_owned())
    })?;
    let target_width = f64::from(block.width.max(1.0));
    Ok(rebuild_block(
        document,
        effective,
        page_index,
        block,
        anchor,
        &paragraph,
        &paragraph,
        target_width,
    )?)
}

/// Replace the stream and dictionary of one image XObject in place.
fn replace_image(
    document: &mut Document,
    effective: &Document,
    page_index: usize,
    resource: &str,
    occurrence: usize,
    bytes: &[u8],
    format: ImageFormat,
) -> Result<(u32, u32), ObjectEditError> {
    let image_data = match format {
        ImageFormat::Png => ImageData::from_png(bytes)?,
        ImageFormat::Jpeg => ImageData::from_jpeg(bytes)?,
    };
    let width = image_data.width;
    let height = image_data.height;

    let images = scan_images(effective, page_index)?;
    let image = images
        .iter()
        .find(|image| image.resource == resource && image.occurrence == occurrence)
        .ok_or_else(|| ObjectEditError::ObjectNotFound {
            page: page_index,
            id: image_object_id(resource, occurrence),
        })?;
    let Some(image_id) = image.object else {
        return Err(ObjectEditError::UnsupportedContent(
            "an image stored directly in the resource dictionary cannot be replaced".to_owned(),
        ));
    };
    if image_usage_count(effective, image_id)? > 1
        || reference_count(effective, image_id)? > 1
        || page_resource_dictionary_shared(effective, page_index)?
    {
        return Err(ObjectEditError::SharedImageResource {
            resource: resource.to_owned(),
        });
    }

    document.set_object(
        image_id,
        Object::Stream {
            dictionary: image_stream_dictionary(&image_data),
            data: image_data.data,
        },
    )?;
    Ok((width, height))
}

/// Build the image XObject dictionary for decoded image bytes.
fn image_stream_dictionary(image: &ImageData) -> Dictionary {
    let mut dictionary = Dictionary::new();
    dictionary.insert("Type".to_owned(), Object::Name("XObject".to_owned()));
    dictionary.insert("Subtype".to_owned(), Object::Name("Image".to_owned()));
    dictionary.insert("Width".to_owned(), Object::Integer(i64::from(image.width)));
    dictionary.insert(
        "Height".to_owned(),
        Object::Integer(i64::from(image.height)),
    );
    dictionary.insert(
        "ColorSpace".to_owned(),
        Object::Name(image.color_space.to_owned()),
    );
    dictionary.insert("BitsPerComponent".to_owned(), Object::Integer(8));
    if image.jpeg {
        dictionary.insert("Filter".to_owned(), Object::Name("DCTDecode".to_owned()));
    }
    dictionary
}

/// A byte replacement in a content stream.
struct ByteEdit {
    start: usize,
    end: usize,
    replacement: Vec<u8>,
}

/// Apply non-overlapping byte edits, oldest position last.
fn apply_edits(content: &[u8], mut edits: Vec<ByteEdit>) -> Result<Vec<u8>, ObjectEditError> {
    edits.sort_by_key(|edit| edit.start);
    let mut added = 0usize;
    for edit in &edits {
        added = added
            .checked_add(edit.replacement.len())
            .ok_or(ObjectEditError::ContentTooLarge)?;
    }
    let mut out = Vec::with_capacity(content.len().saturating_add(added));
    let mut cursor = 0usize;
    for edit in edits {
        if edit.end < edit.start || edit.end > content.len() || edit.start < cursor {
            return Err(ObjectEditError::InvalidSpan {
                start: edit.start,
                end: edit.end,
            });
        }
        out.extend_from_slice(&content[cursor..edit.start]);
        out.extend_from_slice(&edit.replacement);
        cursor = edit.end;
    }
    out.extend_from_slice(&content[cursor..]);
    Ok(out)
}

/// Format a finite coordinate without exponent notation or negative zero.
fn format_number(value: f64) -> Result<String, ObjectEditError> {
    if !value.is_finite() {
        return Err(ObjectEditError::NonFiniteDelta);
    }
    let mut text = format!("{value:.6}");
    if text.contains('.') {
        while text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
    }
    if text == "-0" {
        text = "0".to_owned();
    }
    Ok(text)
}

/// One image `Do` usage found in a page content stream.
struct ImageHit {
    resource: String,
    occurrence: usize,
    rect: Rect,
    do_start: usize,
    do_end: usize,
    object: Option<ObjectId>,
    in_text_object: bool,
    /// The most recent `cm` before the draw in the current graphics state.
    placement_cm: Option<ImagePlacementMatrix>,
    /// `true` when the draw is inside at least one `q`/`Q` pair.
    wrapped: bool,
    /// `true` when more than one draw shares the placement matrix.
    cm_shared: bool,
}

impl ImageHit {
    fn id(&self) -> String {
        image_object_id(&self.resource, self.occurrence)
    }
}

/// One `cm` operator that places an image.
#[derive(Clone)]
struct ImagePlacementMatrix {
    start: usize,
    end: usize,
    values: [f64; 6],
    prior_ctm: Matrix,
}

/// Enumerate the image XObjects drawn directly by one page's content stream.
fn scan_images(document: &Document, page_index: usize) -> Result<Vec<ImageHit>, ObjectEditError> {
    let page = document.page(page_index)?;
    let Some(resources) = page_resources(document, &page)? else {
        return Ok(Vec::new());
    };
    let Some(resources) = resources.as_dictionary() else {
        return Ok(Vec::new());
    };
    let Some(Object::Dictionary(xobjects)) = resolve(document, resources.get("XObject"))? else {
        return Ok(Vec::new());
    };
    let mut images: BTreeMap<String, Option<ObjectId>> = BTreeMap::new();
    for (name, value) in &xobjects {
        let Some(object) = resolve(document, Some(value))? else {
            continue;
        };
        let is_image = object
            .as_dictionary()
            .and_then(|dictionary| dictionary.get("Subtype"))
            .and_then(Object::as_name)
            .is_some_and(|subtype| subtype == "Image");
        if is_image {
            images.insert(name.clone(), value.as_reference());
        }
    }
    if images.is_empty() {
        return Ok(Vec::new());
    }

    let content = document.page_content(page_index)?;
    let operators = tokenize(&content)?;
    let mut state = GraphicsState::new();
    let mut saved: Vec<GraphicsState> = Vec::new();
    let mut in_text = 0usize;
    let mut occurrences: BTreeMap<String, usize> = BTreeMap::new();
    let mut last_cm: Option<ImagePlacementMatrix> = None;
    let mut cm_uses: BTreeMap<usize, usize> = BTreeMap::new();
    let mut hits = Vec::new();
    for operator in operators {
        match operator.name.as_str() {
            "q" => {
                saved.push(state.clone());
                last_cm = None;
            }
            "Q" => {
                if let Some(previous) = saved.pop() {
                    state = previous;
                }
                last_cm = None;
            }
            "cm" => {
                if let Some(matrix) = matrix_from(&operator.operands) {
                    let prior_ctm = state.ctm;
                    state.ctm = Matrix::multiply(matrix, state.ctm);
                    last_cm = Some(ImagePlacementMatrix {
                        start: operator.start,
                        end: operator.end,
                        values: matrix.0,
                        prior_ctm,
                    });
                }
            }
            "BT" => in_text = in_text.saturating_add(1),
            "ET" => in_text = in_text.saturating_sub(1),
            "Do" => {
                if let Some(Operand::Name(name)) = operator.operands.last()
                    && let Some(object) = images.get(name)
                {
                    let occurrence = occurrences.entry(name.clone()).or_insert(0);
                    let index = *occurrence;
                    *occurrence = occurrence.saturating_add(1);
                    if let Some(cm) = &last_cm {
                        let uses = cm_uses.entry(cm.start).or_insert(0);
                        *uses = uses.saturating_add(1);
                    }
                    hits.push(ImageHit {
                        resource: name.clone(),
                        occurrence: index,
                        rect: unit_square_rect(state.ctm),
                        do_start: operator.start,
                        do_end: operator.end,
                        object: *object,
                        in_text_object: in_text > 0,
                        placement_cm: last_cm.clone(),
                        wrapped: !saved.is_empty(),
                        cm_shared: false,
                    });
                }
            }
            _ => {}
        }
    }
    for hit in &mut hits {
        hit.cm_shared = hit
            .placement_cm
            .as_ref()
            .is_some_and(|cm| cm_uses.get(&cm.start).copied().unwrap_or(0) > 1);
    }
    Ok(hits)
}

/// Transform the image unit square by `ctm` and return its bounding box.
fn unit_square_rect(ctm: Matrix) -> Rect {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for (x, y) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
        let px = ctm.0[0] * x + ctm.0[2] * y + ctm.0[4];
        let py = ctm.0[1] * x + ctm.0[3] * y + ctm.0[5];
        min_x = min_x.min(px);
        min_y = min_y.min(py);
        max_x = max_x.max(px);
        max_y = max_y.max(py);
    }
    Rect::saturating(min_x as f32, min_y as f32, max_x as f32, max_y as f32)
}

/// Return `true` when a matrix has no scale, rotation or skew.
fn linear_identity(matrix: Matrix) -> bool {
    const EPSILON: f64 = 1e-6;
    (matrix.0[0] - 1.0).abs() <= EPSILON
        && matrix.0[1].abs() <= EPSILON
        && matrix.0[2].abs() <= EPSILON
        && (matrix.0[3] - 1.0).abs() <= EPSILON
}

/// Convert a default user-space delta into a text-space delta for `ctm`.
///
/// A text matrix translation is applied before the content matrix, so the
/// requested user-space shift must be mapped back through the content matrix.
fn text_space_delta(ctm: Matrix, dx: f64, dy: f64) -> Result<(f64, f64), ObjectEditError> {
    let (a, b, c, d) = (ctm.0[0], ctm.0[1], ctm.0[2], ctm.0[3]);
    let determinant = a * d - b * c;
    if !determinant.is_finite() || determinant.abs() < 1e-9 {
        return Err(ObjectEditError::UnsupportedContent(
            "a singular content transform cannot be translated".to_owned(),
        ));
    }
    let x = (dx * d - dy * c) / determinant;
    let y = (dy * a - dx * b) / determinant;
    if !x.is_finite() || !y.is_finite() {
        return Err(ObjectEditError::CoordinateOutOfRange);
    }
    Ok((x, y))
}

/// One placement operator inside a text object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlacementKind {
    /// An absolute text matrix (`Tm`).
    Tm,
    /// A relative line move (`Td`).
    Td,
    /// A relative line move that also sets leading (`TD`).
    TdUpper,
}

/// One `Tm`/`Td`/`TD` operator with the graphics matrix in effect.
struct PlacementInfo {
    start: usize,
    end: usize,
    kind: PlacementKind,
    values: [f64; 6],
    ctm: Matrix,
}

/// One `Tf` operator inside a text object.
struct FontSizeInfo {
    start: usize,
    end: usize,
    resource: String,
    size: f64,
}

/// One text object (`BT` ... `ET`) with its placement and ownership facts.
struct TextObjectInfo {
    bt_start: usize,
    et_end: usize,
    first_show: Option<usize>,
    first_show_ctm: Matrix,
    placements: Vec<PlacementInfo>,
    font_sizes: Vec<FontSizeInfo>,
    owners: BTreeSet<usize>,
}

/// Scan text objects and attribute their visible shows to text blocks.
///
/// `usize::MAX` in [`TextObjectInfo::owners`] marks a visible show that the
/// text model did not record, so a block that shares a text object with it is
/// rejected rather than shifted or deleted.
fn scan_text_objects(
    content: &[u8],
    fonts: &BTreeMap<String, FontInfo>,
    model: &PageTextModel,
) -> Result<Vec<TextObjectInfo>, ObjectEditError> {
    let mut owners_by_show = BTreeMap::new();
    for block in &model.blocks {
        for run in &block.runs {
            owners_by_show.insert(run.show_start, block.index);
        }
    }

    let operators = tokenize(content)?;
    let mut state = GraphicsState::new();
    let mut saved: Vec<GraphicsState> = Vec::new();
    let mut current: Option<TextObjectInfo> = None;
    let mut objects = Vec::new();
    for operator in operators {
        match operator.name.as_str() {
            "q" => saved.push(state.clone()),
            "Q" => {
                if let Some(previous) = saved.pop() {
                    state = previous;
                }
            }
            "cm" => {
                if let Some(matrix) = matrix_from(&operator.operands) {
                    state.ctm = Matrix::multiply(matrix, state.ctm);
                }
            }
            "BT" => {
                if current.is_some() {
                    return Err(ObjectEditError::MalformedContent(
                        "nested text object".to_owned(),
                    ));
                }
                current = Some(TextObjectInfo {
                    bt_start: operator.start,
                    et_end: operator.end,
                    first_show: None,
                    first_show_ctm: state.ctm,
                    placements: Vec::new(),
                    font_sizes: Vec::new(),
                    owners: BTreeSet::new(),
                });
            }
            "Tf" => {
                if let Some(Operand::Name(name)) = operator.operands.first() {
                    state.font = name.clone();
                }
                if let Some(size) = operator.operands.get(1).and_then(Operand::as_number) {
                    state.font_size = size;
                }
                if let Some(object) = current.as_mut()
                    && let Some(Operand::Name(resource)) = operator.operands.first()
                    && let Some(size) = operator.operands.get(1).and_then(Operand::as_number)
                {
                    object.font_sizes.push(FontSizeInfo {
                        start: operator.start,
                        end: operator.end,
                        resource: resource.clone(),
                        size,
                    });
                }
            }
            "Tm" => {
                if let Some(object) = current.as_mut()
                    && let Some(matrix) = matrix_from(&operator.operands)
                {
                    object.placements.push(PlacementInfo {
                        start: operator.start,
                        end: operator.end,
                        kind: PlacementKind::Tm,
                        values: matrix.0,
                        ctm: state.ctm,
                    });
                }
            }
            "Td" | "TD" => {
                if let Some(object) = current.as_mut() {
                    let tx = operator
                        .operands
                        .first()
                        .and_then(Operand::as_number)
                        .unwrap_or(0.0);
                    let ty = operator
                        .operands
                        .get(1)
                        .and_then(Operand::as_number)
                        .unwrap_or(0.0);
                    object.placements.push(PlacementInfo {
                        start: operator.start,
                        end: operator.end,
                        kind: if operator.name == "TD" {
                            PlacementKind::TdUpper
                        } else {
                            PlacementKind::Td
                        },
                        values: [tx, ty, 0.0, 0.0, 0.0, 0.0],
                        ctm: state.ctm,
                    });
                }
            }
            "Tj" | "TJ" | "'" | "\"" => {
                if let Some(object) = current.as_mut() {
                    if object.first_show.is_none() {
                        object.first_show = Some(operator.start);
                        object.first_show_ctm = state.ctm;
                    }
                    match owners_by_show.get(&operator.start) {
                        Some(block) => {
                            object.owners.insert(*block);
                        }
                        None => {
                            if show_has_visible_text(&operator, fonts.get(&state.font)) {
                                object.owners.insert(usize::MAX);
                            }
                        }
                    }
                }
            }
            "ET" => {
                let mut object = current.take().ok_or_else(|| {
                    ObjectEditError::MalformedContent("unmatched ET operator".to_owned())
                })?;
                object.et_end = operator.end;
                objects.push(object);
            }
            _ => {}
        }
    }
    if current.is_some() {
        return Err(ObjectEditError::MalformedContent(
            "unterminated text object".to_owned(),
        ));
    }
    Ok(objects)
}

/// Return `true` when a show operator carries non-whitespace text.
fn show_has_visible_text(operator: &Operator, font: Option<&FontInfo>) -> bool {
    let has_visible = |bytes: &[u8]| match font {
        Some(font) => !font.decode(bytes).trim().is_empty(),
        None => bytes.iter().any(|byte| !byte.is_ascii_whitespace()),
    };
    match operator.name.as_str() {
        "Tj" | "'" | "\"" => match operator.operands.last() {
            Some(Operand::Text(bytes)) => has_visible(bytes),
            _ => false,
        },
        "TJ" => match operator.operands.last() {
            Some(Operand::Array(items)) => items.iter().any(|item| match item {
                Operand::Text(bytes) => has_visible(bytes),
                _ => false,
            }),
            _ => false,
        },
        _ => false,
    }
}

/// Build the placement edits that move a text block by `(dx, dy)`.
fn plan_text_move(
    objects: &[TextObjectInfo],
    block_index: usize,
    dx: f64,
    dy: f64,
) -> Result<Vec<ByteEdit>, ObjectEditError> {
    let targets: Vec<&TextObjectInfo> = objects
        .iter()
        .filter(|object| object.owners.contains(&block_index))
        .collect();
    if targets.is_empty() {
        return Err(ObjectEditError::UnsupportedContent(
            "the text block has no locatable text object".to_owned(),
        ));
    }
    for object in &targets {
        if object.owners.len() > 1 || object.owners.contains(&usize::MAX) {
            return Err(ObjectEditError::SharedTextObject { block_index });
        }
    }

    let mut edits = Vec::new();
    for object in targets {
        let first_show = object.first_show.ok_or_else(|| {
            ObjectEditError::UnsupportedContent("a text object has no show operator".to_owned())
        })?;
        let has_tm_before_first = object
            .placements
            .iter()
            .any(|placement| placement.kind == PlacementKind::Tm && placement.start < first_show);
        if !has_tm_before_first {
            let first_relative = object
                .placements
                .iter()
                .filter(|placement| {
                    placement.kind != PlacementKind::Tm && placement.start < first_show
                })
                .min_by_key(|placement| placement.start);
            match first_relative {
                Some(placement) => {
                    let (shift_x, shift_y) = text_space_delta(placement.ctm, dx, dy)?;
                    let name = match placement.kind {
                        PlacementKind::TdUpper => "TD",
                        _ => "Td",
                    };
                    edits.push(ByteEdit {
                        start: placement.start,
                        end: placement.end,
                        replacement: format!(
                            "{} {} {}",
                            format_number(placement.values[0] + shift_x)?,
                            format_number(placement.values[1] + shift_y)?,
                            name
                        )
                        .into_bytes(),
                    });
                }
                None => {
                    let (shift_x, shift_y) = text_space_delta(object.first_show_ctm, dx, dy)?;
                    edits.push(ByteEdit {
                        start: first_show,
                        end: first_show,
                        replacement: format!(
                            "1 0 0 1 {} {} Td\n",
                            format_number(shift_x)?,
                            format_number(shift_y)?
                        )
                        .into_bytes(),
                    });
                }
            }
        }
        for placement in &object.placements {
            if placement.kind != PlacementKind::Tm {
                continue;
            }
            let (shift_x, shift_y) = text_space_delta(placement.ctm, dx, dy)?;
            let values = placement.values;
            edits.push(ByteEdit {
                start: placement.start,
                end: placement.end,
                replacement: format!(
                    "{} {} {} {} {} {} Tm",
                    format_number(values[0])?,
                    format_number(values[1])?,
                    format_number(values[2])?,
                    format_number(values[3])?,
                    format_number(values[4] + shift_x)?,
                    format_number(values[5] + shift_y)?,
                )
                .into_bytes(),
            });
        }
    }
    Ok(edits)
}

/// Build the placement and font-size edits that resize a text block.
///
/// The block origin is `(origin_x, origin_y)` in default user space. Text is
/// kept extractable: `Tf` sizes scale by `sy`, the `Tm` horizontal term is
/// corrected by `sx / sy`, `Tm` translations are mapped about the origin, and
/// relative line moves scale by `sy`.
fn plan_text_resize(
    objects: &[TextObjectInfo],
    block_index: usize,
    origin: (f64, f64),
    sx: f64,
    sy: f64,
) -> Result<Vec<ByteEdit>, ObjectEditError> {
    let targets: Vec<&TextObjectInfo> = objects
        .iter()
        .filter(|object| object.owners.contains(&block_index))
        .collect();
    if targets.is_empty() {
        return Err(ObjectEditError::UnsupportedContent(
            "the text block has no locatable text object".to_owned(),
        ));
    }
    for object in &targets {
        if object.owners.len() > 1 || object.owners.contains(&usize::MAX) {
            return Err(ObjectEditError::SharedTextObject { block_index });
        }
    }

    let (origin_x, origin_y) = origin;
    let horizontal_ratio = sx / sy;
    let mut edits = Vec::new();
    for object in targets {
        let has_tm = object
            .placements
            .iter()
            .any(|placement| placement.kind == PlacementKind::Tm);
        if !has_tm {
            return Err(ObjectEditError::UnsupportedContent(
                "a text object positioned without Tm cannot be resized safely".to_owned(),
            ));
        }
        if object.font_sizes.is_empty() {
            return Err(ObjectEditError::UnsupportedContent(
                "a text object without Tf cannot be resized safely".to_owned(),
            ));
        }
        for placement in &object.placements {
            if !linear_identity(placement.ctm) {
                return Err(ObjectEditError::UnsupportedContent(
                    "a text block under a scaled or rotated content transform cannot be resized safely"
                        .to_owned(),
                ));
            }
            let values = placement.values;
            let ctm_x = placement.ctm.0[4];
            let ctm_y = placement.ctm.0[5];
            match placement.kind {
                PlacementKind::Tm => {
                    let (a, b, c, d) = (values[0], values[1], values[2], values[3]);
                    if b.abs() > 1e-6 || c.abs() > 1e-6 {
                        return Err(ObjectEditError::UnsupportedContent(
                            "rotated or skewed text cannot be resized safely".to_owned(),
                        ));
                    }
                    let scaled_a = a * horizontal_ratio;
                    let translated_x =
                        origin_x + (a * ctm_x + values[4] - origin_x) * sx - scaled_a * ctm_x;
                    let translated_y =
                        origin_y + (d * ctm_y + values[5] - origin_y) * sy - d * ctm_y;
                    edits.push(ByteEdit {
                        start: placement.start,
                        end: placement.end,
                        replacement: format!(
                            "{} {} {} {} {} {} Tm",
                            format_number(scaled_a)?,
                            format_number(b * horizontal_ratio)?,
                            format_number(c)?,
                            format_number(d)?,
                            format_number(translated_x)?,
                            format_number(translated_y)?,
                        )
                        .into_bytes(),
                    });
                }
                PlacementKind::Td | PlacementKind::TdUpper => {
                    let name = if placement.kind == PlacementKind::TdUpper {
                        "TD"
                    } else {
                        "Td"
                    };
                    edits.push(ByteEdit {
                        start: placement.start,
                        end: placement.end,
                        replacement: format!(
                            "{} {} {}",
                            format_number(values[0] * sy)?,
                            format_number(values[1] * sy)?,
                            name
                        )
                        .into_bytes(),
                    });
                }
            }
        }
        for font in &object.font_sizes {
            edits.push(ByteEdit {
                start: font.start,
                end: font.end,
                replacement: format!("/{} {} Tf", font.resource, format_number(font.size * sy)?)
                    .into_bytes(),
            });
        }
    }
    Ok(edits)
}
fn reference_count(document: &Document, target: ObjectId) -> Result<usize, ObjectEditError> {
    let root = document.root()?;
    let mut counts: BTreeMap<ObjectId, usize> = BTreeMap::new();
    let mut visited: BTreeSet<ObjectId> = BTreeSet::new();
    let mut stack = vec![(root, 0usize)];
    while let Some((id, depth)) = stack.pop() {
        if depth > MAX_REFERENCE_DEPTH {
            return Err(ObjectEditError::MalformedReferenceGraph);
        }
        if !visited.insert(id) {
            continue;
        }
        if visited.len() > MAX_REFERENCE_OBJECTS {
            return Err(ObjectEditError::ReferenceGraphTooLarge);
        }
        let object = document.object(id)?;
        collect_references(&object, depth, &mut counts, &mut visited, &mut stack);
    }
    Ok(counts.get(&target).copied().unwrap_or(0))
}

/// Walk one object's immediate references and schedule its children.
fn collect_references(
    object: &Object,
    depth: usize,
    counts: &mut BTreeMap<ObjectId, usize>,
    visited: &mut BTreeSet<ObjectId>,
    stack: &mut Vec<(ObjectId, usize)>,
) {
    match object {
        Object::Reference(id) => {
            *counts.entry(*id).or_insert(0) += 1;
            if !visited.contains(id) {
                stack.push((*id, depth.saturating_add(1)));
            }
        }
        Object::Array(items) => {
            for item in items {
                collect_references(item, depth, counts, visited, stack);
            }
        }
        Object::Dictionary(dictionary) | Object::Stream { dictionary, .. } => {
            for value in dictionary.values() {
                collect_references(value, depth, counts, visited, stack);
            }
        }
        _ => {}
    }
}

/// Count how many `Do` usages in the document resolve to `target`.
fn image_usage_count(document: &Document, target: ObjectId) -> Result<usize, ObjectEditError> {
    let page_count = document.page_count()?;
    let mut total = 0usize;
    for page_index in 0..page_count {
        for image in scan_images(document, page_index)? {
            if image.object == Some(target) {
                total = total.saturating_add(1);
            }
        }
    }
    Ok(total)
}

/// Return `true` when another page shares this page's resource or XObject dict.
fn page_resource_dictionary_shared(
    document: &Document,
    page_index: usize,
) -> Result<bool, ObjectEditError> {
    let (resources_id, xobjects_id) = page_resource_identities(document, page_index)?;
    if resources_id.is_none() && xobjects_id.is_none() {
        return Ok(false);
    }
    let page_count = document.page_count()?;
    for other in 0..page_count {
        if other == page_index {
            continue;
        }
        let (other_resources, other_xobjects) = page_resource_identities(document, other)?;
        if resources_id.is_some() && resources_id == other_resources {
            return Ok(true);
        }
        if xobjects_id.is_some() && xobjects_id == other_xobjects {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Return the indirect object identities of a page's resources and XObject dict.
fn page_resource_identities(
    document: &Document,
    page_index: usize,
) -> Result<(Option<ObjectId>, Option<ObjectId>), ObjectEditError> {
    let page = document.page(page_index)?;
    let page_object = document.object(page.id)?;
    let (resources_id, resources) = match page_object.as_dictionary() {
        Some(dictionary) => match dictionary.get("Resources") {
            Some(Object::Reference(id)) => match document.object(*id)? {
                Object::Dictionary(dictionary) => (Some(*id), Some(dictionary)),
                _ => (Some(*id), None),
            },
            Some(Object::Dictionary(dictionary)) => (None, Some(dictionary.clone())),
            _ => (None, None),
        },
        None => (None, None),
    };
    let (resources_id, resources) = match resources {
        Some(resources) => (resources_id, Some(resources)),
        None => match page.resources {
            Some(id) => match document.object(id)? {
                Object::Dictionary(dictionary) => (Some(id), Some(dictionary)),
                _ => (Some(id), None),
            },
            None => (None, None),
        },
    };
    let xobjects_id = resources
        .as_ref()
        .and_then(|resources| resources.get("XObject"))
        .and_then(Object::as_reference);
    Ok((resources_id, xobjects_id))
}

// ---------------------------------------------------------------------------
// Effective document and layout metrics
// ---------------------------------------------------------------------------

/// Reparse the staged document through the core incremental writer.
fn effective_document(document: &Document) -> Result<Document, TextEditError> {
    let bytes = document.save_incremental()?;
    Ok(Document::from_bytes(bytes)?)
}

/// The bundled fallback font parsed for metrics and glyph identifiers.
fn fallback_font() -> Result<TrueTypeFont, TextEditError> {
    TrueTypeFont::parse(FALLBACK_FONT)
        .map_err(|error| TextEditError::FontProgram(error.to_string()))
}

/// The bundled fallback font parsed for metrics and glyph identifiers, plus the
/// deterministic CID assignment used by the embedded Type0 font.
///
/// CIDs are assigned in sorted character order starting at 1; CID 0 is left for
/// the notdef glyph. The same assignment drives `/W`, `/ToUnicode`, the encoded
/// content stream and the `/CIDToGIDMap` stream, so no reader has to guess how a
/// CID relates to a glyph.
#[derive(Clone)]
struct FallbackFont {
    ttf: TrueTypeFont,
    cid_of: BTreeMap<char, u16>,
}

impl FallbackFont {
    fn new(ttf: TrueTypeFont, characters: &BTreeSet<char>) -> Result<Self, TextEditError> {
        let mut cid_of = BTreeMap::new();
        let mut cid = 1u16;
        for character in characters {
            if character.is_control() {
                continue;
            }
            ttf.glyph_id(*character)
                .map_err(|_| TextEditError::UnsupportedCharacter {
                    character: *character as u32,
                })?;
            if cid == 0 {
                return Err(TextEditError::FontProgram(
                    "too many distinct characters for a CID font".to_owned(),
                ));
            }
            cid_of.insert(*character, cid);
            cid = cid.checked_add(1).ok_or_else(|| {
                TextEditError::FontProgram("too many distinct characters for a CID font".to_owned())
            })?;
        }
        if cid_of.is_empty() {
            return Err(TextEditError::EmptyText);
        }
        Ok(Self { ttf, cid_of })
    }

    fn advance_for_char(&self, character: char, font_size: f64) -> f64 {
        self.ttf.advance_for_char(character, font_size)
    }

    fn encode(&self, text: &str) -> Result<Vec<u8>, TextEditError> {
        let mut bytes = Vec::with_capacity(text.len().saturating_mul(2));
        for character in text.chars() {
            let cid = self.cid_of.get(&character).copied().ok_or(
                TextEditError::UnsupportedCharacter {
                    character: character as u32,
                },
            )?;
            bytes.extend_from_slice(&cid.to_be_bytes());
        }
        Ok(bytes)
    }
}

/// Metrics used for greedy wrapping and encoded line widths.
#[derive(Clone)]
enum LayoutMetrics {
    /// An existing simple font from the page.
    Existing(FontInfo, f64),
    /// The bundled fallback font with its CID assignment.
    Fallback(FallbackFont, f64),
}

impl LayoutMetrics {
    fn advance(&self, character: char) -> f64 {
        match self {
            Self::Existing(font, size) => font.advance_for_char(character, *size),
            Self::Fallback(font, size) => font.advance_for_char(character, *size),
        }
    }

    fn measure(&self, text: &str) -> f64 {
        text.chars().map(|character| self.advance(character)).sum()
    }
}

/// Encode one line using the selected font.
fn encode_line(metrics: &LayoutMetrics, text: &str) -> Result<Vec<u8>, TextEditError> {
    match metrics {
        LayoutMetrics::Fallback(font, _) => font.encode(text),
        LayoutMetrics::Existing(font, _) => {
            let mut bytes = Vec::with_capacity(text.len());
            for character in text.chars() {
                let code = u32::from(character);
                if code > 0xFF || !font.can_encode_char(character) {
                    return Err(TextEditError::UnencodableText {
                        font: font.base_font.clone(),
                    });
                }
                bytes.push(code as u8);
            }
            Ok(bytes)
        }
    }
}

// ---------------------------------------------------------------------------
// Page editor: resources and contents
// ---------------------------------------------------------------------------

/// A named resource category (`/Font` or `/XObject`), direct or indirect.
struct NamedSlot {
    indirect: Option<ObjectId>,
    dictionary: Dictionary,
    changed: bool,
}

impl NamedSlot {
    fn new() -> Self {
        Self {
            indirect: None,
            dictionary: Dictionary::new(),
            changed: false,
        }
    }
}

/// Read and stage one page's resource and content entries.
struct PageEditor {
    page_id: ObjectId,
    dictionary: Dictionary,
    stream_data: Option<Vec<u8>>,
    resources_indirect: Option<ObjectId>,
    resources: Dictionary,
    fonts: NamedSlot,
    xobjects: NamedSlot,
    contents: Vec<ObjectId>,
}

impl PageEditor {
    fn new(document: &Document, page_index: usize) -> Result<Self, TextEditError> {
        let page = document.page(page_index)?;
        let (dictionary, stream_data) = match document.object(page.id)? {
            Object::Dictionary(dictionary) => (dictionary, None),
            Object::Stream { dictionary, data } => (dictionary, Some(data)),
            _ => return Err(TextEditError::UnsupportedPage(page_index)),
        };
        let (resources_indirect, resources) = match dictionary.get("Resources") {
            Some(Object::Reference(id)) => match document.object(*id)? {
                Object::Dictionary(dictionary) => (Some(*id), dictionary),
                _ => (None, Dictionary::new()),
            },
            Some(Object::Dictionary(dictionary)) => (None, dictionary.clone()),
            _ => match page.resources {
                Some(id) => match document.object(id)? {
                    Object::Dictionary(dictionary) => (Some(id), dictionary),
                    _ => (None, Dictionary::new()),
                },
                None => (None, Dictionary::new()),
            },
        };
        let fonts = read_named(document, &resources, "Font")?;
        let xobjects = read_named(document, &resources, "XObject")?;
        Ok(Self {
            page_id: page.id,
            dictionary,
            stream_data,
            resources_indirect,
            resources,
            fonts,
            xobjects,
            contents: page.contents.clone(),
        })
    }

    fn resource_name_for_font(&self, font_id: ObjectId) -> Option<&str> {
        self.fonts
            .dictionary
            .iter()
            .find(|(_, value)| value.as_reference() == Some(font_id))
            .map(|(name, _)| name.as_str())
    }

    fn ensure_xobject(&mut self, image_id: ObjectId) -> Result<String, TextEditError> {
        let name = unique_name(&self.xobjects.dictionary, "Im");
        self.xobjects
            .dictionary
            .insert(name.clone(), Object::Reference(image_id));
        self.xobjects.changed = true;
        Ok(name)
    }

    fn set_contents(&mut self, contents: Vec<ObjectId>) {
        self.contents = contents;
    }

    /// Remove one `/XObject` resource entry when present.
    fn remove_xobject(&mut self, name: &str) -> bool {
        if self.xobjects.dictionary.remove(name).is_some() {
            self.xobjects.changed = true;
            true
        } else {
            false
        }
    }

    fn commit(mut self, document: &mut Document) -> Result<(), TextEditError> {
        if self.fonts.changed {
            commit_slot(document, &mut self.resources, "Font", self.fonts)?;
        }
        if self.xobjects.changed {
            commit_slot(document, &mut self.resources, "XObject", self.xobjects)?;
        }
        match self.resources_indirect {
            Some(id) => document.set_object(id, Object::Dictionary(self.resources))?,
            None => {
                self.dictionary
                    .insert("Resources".to_owned(), Object::Dictionary(self.resources));
            }
        }
        let contents: Vec<Object> = self.contents.into_iter().map(Object::Reference).collect();
        self.dictionary
            .insert("Contents".to_owned(), Object::Array(contents));
        let page = match self.stream_data {
            Some(data) => Object::Stream {
                dictionary: self.dictionary,
                data,
            },
            None => Object::Dictionary(self.dictionary),
        };
        document.set_object(self.page_id, page)?;
        Ok(())
    }
}

/// Read a named page resource category, resolving an indirect dictionary.
fn read_named(
    document: &Document,
    resources: &Dictionary,
    key: &str,
) -> Result<NamedSlot, TextEditError> {
    match resources.get(key) {
        Some(Object::Reference(id)) => match document.object(*id)? {
            Object::Dictionary(dictionary) => Ok(NamedSlot {
                indirect: Some(*id),
                dictionary,
                changed: false,
            }),
            _ => Ok(NamedSlot::new()),
        },
        Some(Object::Dictionary(dictionary)) => Ok(NamedSlot {
            indirect: None,
            dictionary: dictionary.clone(),
            changed: false,
        }),
        _ => Ok(NamedSlot::new()),
    }
}

/// Write a named resource category into the parent resource dictionary.
fn commit_slot(
    document: &mut Document,
    resources: &mut Dictionary,
    key: &str,
    slot: NamedSlot,
) -> Result<(), TextEditError> {
    match slot.indirect {
        Some(id) => document.set_object(id, Object::Dictionary(slot.dictionary))?,
        None => {
            resources.insert(key.to_owned(), Object::Dictionary(slot.dictionary));
        }
    }
    Ok(())
}

/// Choose a deterministic, unused resource name with the given prefix.
fn unique_name(dictionary: &Dictionary, prefix: &str) -> String {
    for index in 0..100_000u32 {
        let candidate = format!("{prefix}{index}");
        if !dictionary.contains_key(&candidate) {
            return candidate;
        }
    }
    format!("{prefix}fallback")
}

// ---------------------------------------------------------------------------
// Font embedding
// ---------------------------------------------------------------------------

/// Embed the fallback font with the CID assignment used by the content stream.
///
/// A `/CIDToGIDMap` stream maps each CID to its real glyph identifier, `/W` is
/// emitted in the valid PDF forms (`c_first c_last w` or `c [w...]`), and the
/// widths are converted from font units to thousandths of a text space unit.
fn embed_fallback_font(
    document: &mut Document,
    editor: &mut PageEditor,
    metrics: &LayoutMetrics,
) -> Result<ObjectId, TextEditError> {
    let LayoutMetrics::Fallback(fallback, _) = metrics else {
        return Err(TextEditError::FontProgram(
            "fallback embedding needs fallback metrics".to_owned(),
        ));
    };
    let ttf = &fallback.ttf;
    let units_per_em = ttf.units_per_em();
    if units_per_em == 0 {
        return Err(TextEditError::FontProgram(
            "fallback font has no unitsPerEm".to_owned(),
        ));
    }

    let mut widths = Vec::with_capacity(fallback.cid_of.len());
    let mut unicode = Vec::with_capacity(fallback.cid_of.len());
    let mut cid_to_gid = BTreeMap::new();
    for (character, cid) in &fallback.cid_of {
        let glyph = ttf
            .glyph_id(*character)
            .map_err(|_| TextEditError::UnsupportedCharacter {
                character: *character as u32,
            })?;
        let width = scale_to_1000(ttf.advance_units(glyph), units_per_em);
        widths.push(GlyphWidth { cid: *cid, width });
        unicode.push(ToUnicodeEntry {
            cid: *cid,
            unicode: character.to_string(),
        });
        cid_to_gid.insert(*cid, glyph);
    }
    if widths.is_empty() {
        return Err(TextEditError::EmptyText);
    }

    let program = FontProgram::new(
        FALLBACK_BASE_FONT,
        FontProgramFormat::TrueType,
        FALLBACK_FONT.to_vec(),
    )?;
    let ids = allocate_ids(document, 6)?;
    let base_id = ids
        .first()
        .copied()
        .ok_or(TextEditError::ObjectSpaceExhausted)?;
    let cid_map_id = ids
        .get(5)
        .copied()
        .ok_or(TextEditError::ObjectSpaceExhausted)?;
    if cid_map_id.number != base_id.number.saturating_add(5) {
        return Err(TextEditError::ObjectSpaceExhausted);
    }

    let options = CidFontOptions {
        base_font: FALLBACK_BASE_FONT.to_owned(),
        decision: EmbeddingDecision::Embed,
        widths: widths.clone(),
        default_width: 600,
        unicode,
        ..CidFontOptions::default()
    };
    let mut plan = pdfxi_core::font::build_cid_font(&program, &options, base_id.number)?;
    if let Object::Stream { dictionary, data } = &mut plan.font_file {
        dictionary.insert("Length1".to_owned(), Object::Integer(data.len() as i64));
    }
    // The core helper emits the two-number form `c w` for a single glyph, which
    // PDF readers parse as a range. Replace it with the spec forms: a triple for
    // a run of equal widths, or `c [w]` for a one-CID run.
    plan.cid_font
        .insert("W".to_owned(), Object::Array(build_width_array(&widths)));
    plan.cid_font
        .insert("CIDToGIDMap".to_owned(), Object::Reference(cid_map_id));

    let font_id = pdfxi_core::font::stage_cid_font(document, &plan, base_id.number)?;
    let mut map_bytes =
        vec![0u8; (usize::from(cid_to_gid.keys().next_back().copied().unwrap_or(0),) + 1) * 2];
    for (cid, glyph) in &cid_to_gid {
        let offset = usize::from(*cid) * 2;
        if let Some(slot) = map_bytes.get_mut(offset..offset + 2) {
            slot.copy_from_slice(&glyph.to_be_bytes());
        }
    }
    document.set_object(
        cid_map_id,
        Object::Stream {
            dictionary: Dictionary::new(),
            data: map_bytes,
        },
    )?;

    let name = unique_name(&editor.fonts.dictionary, "FXF");
    editor
        .fonts
        .dictionary
        .insert(name, Object::Reference(font_id));
    editor.fonts.changed = true;
    Ok(font_id)
}

/// Convert a font-design-unit advance to thousandths of a text space unit.
fn scale_to_1000(advance: u16, units_per_em: u16) -> u16 {
    let numerator = u32::from(advance) * 1000 + u32::from(units_per_em) / 2;
    (numerator / u32::from(units_per_em)).min(u32::from(u16::MAX)) as u16
}

/// Build a spec-valid `/W` array from CID widths.
///
/// A run of two or more contiguous CIDs with the same width becomes
/// `c_first c_last w`; a one-CID run becomes `c [w]`.
fn build_width_array(widths: &[GlyphWidth]) -> Vec<Object> {
    let mut sorted = widths.to_vec();
    sorted.sort_by_key(|entry| entry.cid);
    let mut out = Vec::new();
    let mut index = 0usize;
    while index < sorted.len() {
        let start = sorted[index].cid;
        let width = sorted[index].width;
        let mut end = start;
        let mut run_len = 1usize;
        while index + run_len < sorted.len()
            && sorted[index + run_len].cid == end.saturating_add(1)
            && sorted[index + run_len].width == width
        {
            end = sorted[index + run_len].cid;
            run_len += 1;
        }
        if run_len >= 2 {
            out.push(Object::Integer(i64::from(start)));
            out.push(Object::Integer(i64::from(end)));
            out.push(Object::Integer(i64::from(width)));
        } else {
            out.push(Object::Integer(i64::from(start)));
            out.push(Object::Array(vec![Object::Integer(i64::from(width))]));
        }
        index += run_len;
    }
    out
}

// ---------------------------------------------------------------------------
// Font resource parsing
// ---------------------------------------------------------------------------

/// Parsed metrics and decoding information for one page font resource.
#[derive(Clone, Debug)]
pub(crate) struct FontInfo {
    base_font: String,
    composite: bool,
    subset: bool,
    encoding: Option<String>,
    first_char: u32,
    widths: Vec<u16>,
    cid_widths: BTreeMap<u32, u16>,
    default_width: u16,
    missing_width: u16,
    to_unicode: Option<ToUnicodeMap>,
}

impl FontInfo {
    fn can_encode(&self, paragraph: &str, replacement: &str) -> bool {
        if self.composite || self.subset {
            return false;
        }
        if !self.encoding.as_deref().is_none_or(|name| {
            name.eq_ignore_ascii_case("WinAnsiEncoding")
                || name.eq_ignore_ascii_case("MacRomanEncoding")
                || name.eq_ignore_ascii_case("StandardEncoding")
                || name.eq_ignore_ascii_case("PDFDocEncoding")
        }) {
            return false;
        }
        paragraph
            .chars()
            .chain(replacement.chars())
            .all(|character| self.can_encode_char(character))
    }

    fn can_encode_char(&self, character: char) -> bool {
        let code = u32::from(character);
        if !(0x20..=0xFF).contains(&code) {
            return false;
        }
        self.widths.is_empty()
            || code
                .checked_sub(self.first_char)
                .is_some_and(|index| (index as usize) < self.widths.len())
    }

    pub(crate) fn decode(&self, bytes: &[u8]) -> String {
        match &self.to_unicode {
            Some(map) => map.decode(bytes),
            None if self.composite => {
                let mut text = String::new();
                for pair in bytes.chunks(2) {
                    let code = match pair {
                        [high, low] => u32::from(u16::from_be_bytes([*high, *low])),
                        [single] => u32::from(*single),
                        _ => 0,
                    };
                    text.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                }
                text
            }
            None => bytes.iter().map(|byte| char::from(*byte)).collect(),
        }
    }

    pub(crate) fn code_units(&self, bytes: &[u8]) -> Vec<(u32, usize)> {
        if self.composite {
            bytes
                .chunks(2)
                .map(|pair| match pair {
                    [high, low] => (u32::from(u16::from_be_bytes([*high, *low])), 2),
                    [single] => (u32::from(*single), 1),
                    _ => (0, 1),
                })
                .collect()
        } else {
            bytes.iter().map(|byte| (u32::from(*byte), 1)).collect()
        }
    }

    pub(crate) fn width_of_code(&self, code: u32) -> f64 {
        let width = if self.composite {
            self.cid_widths
                .get(&code)
                .copied()
                .unwrap_or(self.default_width)
        } else {
            code.checked_sub(self.first_char)
                .and_then(|index| self.widths.get(index as usize).copied())
                .unwrap_or(self.missing_width)
        };
        f64::from(width)
    }

    fn advance_for_char(&self, character: char, font_size: f64) -> f64 {
        let code = u32::from(character);
        let width = self.width_of_code(code) / 1000.0 * font_size;
        if width <= 0.0 { font_size * 0.5 } else { width }
    }
}

/// Load every font resource in a page's resource dictionary.
pub(crate) fn load_fonts(
    document: &Document,
    page: &pdfxi_core::Page,
) -> Result<BTreeMap<String, FontInfo>, TextEditError> {
    let resources = page_resources(document, page)?;
    load_fonts_from_resources(document, resources.as_ref())
}

/// Load every `/Font` entry from an already resolved resource dictionary.
///
/// Form XObjects carry their own `/Resources`; when they do, those resources
/// replace (not merge with) the parent page's. Callers pass the form's resolved
/// `/Resources` value here, or pass `None` to inherit the parent's font map.
pub(crate) fn load_fonts_from_resources(
    document: &Document,
    resources: Option<&Object>,
) -> Result<BTreeMap<String, FontInfo>, TextEditError> {
    let mut fonts = BTreeMap::new();
    let Some(resources) = resources.and_then(Object::as_dictionary) else {
        return Ok(fonts);
    };
    let font_dictionary = match resolve(document, resources.get("Font"))? {
        Some(Object::Dictionary(dictionary)) => dictionary,
        _ => return Ok(fonts),
    };
    for (name, value) in &font_dictionary {
        let Some(Object::Dictionary(dictionary)) = resolve(document, Some(value))? else {
            continue;
        };
        fonts.insert(name.clone(), parse_font(document, &dictionary)?);
    }
    Ok(fonts)
}

/// Resolve a page's resource dictionary, including inherited resources.
pub(crate) fn page_resources(
    document: &Document,
    page: &pdfxi_core::Page,
) -> Result<Option<Object>, TextEditError> {
    let page_object = document.object(page.id)?;
    if let Some(dictionary) = page_object.as_dictionary() {
        match dictionary.get("Resources") {
            Some(Object::Reference(id)) => return Ok(Some(document.object(*id)?)),
            Some(value) => return Ok(Some(value.clone())),
            None => {}
        }
    }
    match page.resources {
        Some(id) => Ok(Some(document.object(id)?)),
        None => Ok(None),
    }
}

/// Resolve an indirect reference once; other values are cloned.
pub(crate) fn resolve(
    document: &Document,
    value: Option<&Object>,
) -> Result<Option<Object>, TextEditError> {
    match value {
        None | Some(Object::Null) => Ok(None),
        Some(Object::Reference(id)) => Ok(Some(document.object(*id)?)),
        Some(value) => Ok(Some(value.clone())),
    }
}

/// Parse one font dictionary into the metrics and decoding information we need.
fn parse_font(document: &Document, dictionary: &Dictionary) -> Result<FontInfo, TextEditError> {
    let base_font = dictionary
        .get("BaseFont")
        .and_then(Object::as_name)
        .unwrap_or("")
        .to_owned();
    let subtype = dictionary
        .get("Subtype")
        .and_then(Object::as_name)
        .unwrap_or("");
    let composite = subtype.eq_ignore_ascii_case("Type0");
    let subset = base_font.split_once('+').is_some_and(|(prefix, _)| {
        prefix.len() == 6 && prefix.bytes().all(|byte| byte.is_ascii_uppercase())
    });
    let encoding = dictionary
        .get("Encoding")
        .and_then(Object::as_name)
        .map(str::to_owned);
    let first_char = dictionary
        .get("FirstChar")
        .and_then(Object::as_i64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(0);
    let widths = match resolve(document, dictionary.get("Widths"))? {
        Some(Object::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_f64()
                    .filter(|value| *value >= 0.0)
                    .map(|value| value.round().min(f64::from(u16::MAX)) as u16)
                    .unwrap_or(0)
            })
            .collect(),
        _ => Vec::new(),
    };
    let (cid_widths, default_width) = if composite {
        parse_cid_widths(document, dictionary)?
    } else {
        (BTreeMap::new(), 0)
    };
    let missing_width = match resolve(document, dictionary.get("FontDescriptor"))? {
        Some(Object::Dictionary(descriptor)) => descriptor
            .get("MissingWidth")
            .and_then(Object::as_i64)
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(0),
        _ => 0,
    };
    let to_unicode = match dictionary.get("ToUnicode") {
        Some(Object::Reference(id)) => parse_to_unicode(document, *id)?,
        Some(Object::Stream { dictionary, data }) => parse_to_unicode_stream(dictionary, data),
        _ => None,
    };
    Ok(FontInfo {
        base_font,
        composite,
        subset,
        encoding,
        first_char,
        widths,
        cid_widths,
        default_width,
        missing_width,
        to_unicode,
    })
}

/// Parse `/W` and `/DW` from a Type0 font's descendant CIDFont.
///
/// This handles both `[c [w1 w2 ...]]` and `[c_first c_last w]` forms.
fn parse_cid_widths(
    document: &Document,
    dictionary: &Dictionary,
) -> Result<(BTreeMap<u32, u16>, u16), TextEditError> {
    let descendant = match resolve(document, dictionary.get("DescendantFonts"))? {
        Some(Object::Array(items)) => match items.first() {
            Some(Object::Reference(id)) => document.object(*id)?,
            Some(value) => value.clone(),
            None => return Ok((BTreeMap::new(), 1000)),
        },
        _ => return Ok((BTreeMap::new(), 1000)),
    };
    let Some(descendant) = descendant.as_dictionary() else {
        return Ok((BTreeMap::new(), 1000));
    };
    let default_width = descendant
        .get("DW")
        .and_then(Object::as_i64)
        .and_then(|value| u16::try_from(value).ok())
        .unwrap_or(1000);
    let mut widths = BTreeMap::new();
    let Some(Object::Array(items)) = descendant.get("W") else {
        return Ok((widths, default_width));
    };
    let mut index = 0usize;
    while index < items.len() {
        let Some(first) = items[index].as_i64() else {
            index += 1;
            continue;
        };
        match items.get(index + 1) {
            Some(Object::Array(values)) => {
                for (offset, value) in values.iter().enumerate() {
                    if let Some(width) = value.as_f64() {
                        let cid = first.saturating_add(offset as i64);
                        if let Ok(cid) = u32::try_from(cid) {
                            widths.insert(cid, width.round().clamp(0.0, 65535.0) as u16);
                        }
                    }
                }
                index += 2;
            }
            Some(second) => {
                let Some(last) = second.as_i64() else {
                    index += 1;
                    continue;
                };
                let Some(width) = items.get(index + 2).and_then(Object::as_f64) else {
                    index += 1;
                    continue;
                };
                if first <= last {
                    for cid in first..=last.min(first.saturating_add(65_535)) {
                        if let Ok(cid) = u32::try_from(cid) {
                            widths.insert(cid, width.round().clamp(0.0, 65535.0) as u16);
                        }
                    }
                }
                index += 3;
            }
            None => break,
        }
    }
    Ok((widths, default_width))
}

/// Decode and parse a `/ToUnicode` stream object.
fn parse_to_unicode(
    document: &Document,
    id: ObjectId,
) -> Result<Option<ToUnicodeMap>, TextEditError> {
    let Ok(Object::Stream { dictionary, data }) = document.object(id) else {
        return Ok(None);
    };
    Ok(parse_to_unicode_stream(&dictionary, &data))
}

/// Decode one `/ToUnicode` stream payload with Flate support.
fn parse_to_unicode_stream(dictionary: &Dictionary, data: &[u8]) -> Option<ToUnicodeMap> {
    let decoded = match dictionary.get("Filter") {
        None => data.to_vec(),
        Some(Object::Name(name)) if name == "FlateDecode" => {
            decode_stream(data, &[Filter::FlateDecode(FlateParams::default())]).ok()?
        }
        _ => return None,
    };
    ToUnicodeMap::parse(&decoded).ok()
}

// ---------------------------------------------------------------------------
// Content stream scanner
// ---------------------------------------------------------------------------

/// One text-showing operator as found in the content stream.
#[derive(Clone, Debug)]
struct RawRun {
    text: String,
    font: String,
    font_size: f64,
    start_x: f64,
    start_y: f64,
    end_x: f64,
    matrix: [f64; 6],
    block: Option<String>,
    show_start: usize,
    show_end: usize,
}

/// A 2D affine matrix in PDF `[a b c d e f]` order.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Matrix(pub(crate) [f64; 6]);

impl Matrix {
    pub(crate) fn identity() -> Self {
        Self([1.0, 0.0, 0.0, 1.0, 0.0, 0.0])
    }

    pub(crate) fn translate(tx: f64, ty: f64) -> Self {
        Self([1.0, 0.0, 0.0, 1.0, tx, ty])
    }

    pub(crate) fn multiply(left: Self, right: Self) -> Self {
        let a = left.0;
        let b = right.0;
        Self([
            a[0] * b[0] + a[1] * b[2],
            a[0] * b[1] + a[1] * b[3],
            a[2] * b[0] + a[3] * b[2],
            a[2] * b[1] + a[3] * b[3],
            a[4] * b[0] + a[5] * b[2] + b[4],
            a[4] * b[1] + a[5] * b[3] + b[5],
        ])
    }

    pub(crate) fn point(self) -> (f64, f64) {
        (self.0[4], self.0[5])
    }
}

/// One operand parsed from a content stream.
#[derive(Clone, Debug)]
pub(crate) enum Operand {
    Number(f64),
    Name(String),
    Text(Vec<u8>),
    Array(Vec<Operand>),
    Dictionary(Vec<(String, Operand)>),
    Other,
}

impl Operand {
    pub(crate) fn as_name(&self) -> Option<&str> {
        match self {
            Self::Name(name) => Some(name),
            _ => None,
        }
    }

    pub(crate) fn as_number(&self) -> Option<f64> {
        match self {
            Self::Number(value) => Some(*value),
            _ => None,
        }
    }
}

/// Text state saved by `q` and restored by `Q`.
#[derive(Clone)]
struct GraphicsState {
    ctm: Matrix,
    font: String,
    font_size: f64,
    leading: f64,
    char_spacing: f64,
    word_spacing: f64,
    horizontal_scale: f64,
}

impl GraphicsState {
    fn new() -> Self {
        Self {
            ctm: Matrix::identity(),
            font: String::new(),
            font_size: 0.0,
            leading: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            horizontal_scale: 1.0,
        }
    }
}

/// One marked-content section.
#[derive(Clone)]
struct BlockMark {
    key: String,
}

/// A tokenized operator with its byte span.
pub(crate) struct Operator {
    pub(crate) name: String,
    pub(crate) operands: Vec<Operand>,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

/// Scan a content stream into text-showing runs with geometry.
fn scan_runs(
    content: &[u8],
    fonts: &BTreeMap<String, FontInfo>,
) -> Result<Vec<RawRun>, TextEditError> {
    let operators = tokenize(content)?;
    let mut runs = Vec::new();
    let mut state = GraphicsState::new();
    let mut saved: Vec<GraphicsState> = Vec::new();
    let mut text_matrix = Matrix::identity();
    let mut line_matrix = Matrix::identity();
    let mut marks: Vec<BlockMark> = Vec::new();
    let mut mark_counter = 0usize;

    for operator in operators {
        match operator.name.as_str() {
            "q" => saved.push(state.clone()),
            "Q" => {
                if let Some(previous) = saved.pop() {
                    state = previous;
                }
            }
            "cm" => {
                if let Some(matrix) = matrix_from(&operator.operands) {
                    state.ctm = Matrix::multiply(matrix, state.ctm);
                }
            }
            "BT" => {
                text_matrix = Matrix::identity();
                line_matrix = Matrix::identity();
            }
            "Tf" => {
                if let Some(Operand::Name(name)) = operator.operands.first() {
                    state.font = name.clone();
                }
                if let Some(size) = operator.operands.get(1).and_then(Operand::as_number) {
                    state.font_size = size;
                }
            }
            "Td" | "TD" => {
                let tx = operator
                    .operands
                    .first()
                    .and_then(Operand::as_number)
                    .unwrap_or(0.0);
                let ty = operator
                    .operands
                    .get(1)
                    .and_then(Operand::as_number)
                    .unwrap_or(0.0);
                if operator.name == "TD" {
                    state.leading = -ty;
                }
                line_matrix = Matrix::multiply(Matrix::translate(tx, ty), line_matrix);
                text_matrix = line_matrix;
            }
            "Tm" => {
                if let Some(matrix) = matrix_from(&operator.operands) {
                    text_matrix = matrix;
                    line_matrix = matrix;
                }
            }
            "T*" => {
                line_matrix = Matrix::multiply(Matrix::translate(0.0, -state.leading), line_matrix);
                text_matrix = line_matrix;
            }
            "TL" => {
                if let Some(leading) = operator.operands.first().and_then(Operand::as_number) {
                    state.leading = leading;
                }
            }
            "Tc" => {
                if let Some(value) = operator.operands.first().and_then(Operand::as_number) {
                    state.char_spacing = value;
                }
            }
            "Tw" => {
                if let Some(value) = operator.operands.first().and_then(Operand::as_number) {
                    state.word_spacing = value;
                }
            }
            "Tz" => {
                if let Some(value) = operator.operands.first().and_then(Operand::as_number) {
                    state.horizontal_scale = value / 100.0;
                }
            }
            "Ts" => {}
            "Tj" => {
                if let Some(Operand::Text(bytes)) = operator.operands.last() {
                    let start_matrix = text_matrix.0;
                    let start = Matrix::multiply(text_matrix, state.ctm).point();
                    let text = fonts.get(&state.font).map(|font| font.decode(bytes));
                    advance_over(bytes, fonts.get(&state.font), &state, &mut text_matrix);
                    let end = Matrix::multiply(text_matrix, state.ctm).point();
                    push_run(
                        &mut runs,
                        ShownRun {
                            text,
                            state: state.clone(),
                            start,
                            end,
                            matrix: start_matrix,
                            block: current_block(&marks),
                            show_start: operator.start,
                            show_end: operator.end,
                        },
                    );
                }
            }
            "TJ" => {
                if let Some(Operand::Array(items)) = operator.operands.last() {
                    let start_matrix = text_matrix.0;
                    let start = Matrix::multiply(text_matrix, state.ctm).point();
                    let mut text = String::new();
                    for item in items {
                        match item {
                            Operand::Text(bytes) => {
                                if let Some(font) = fonts.get(&state.font) {
                                    text.push_str(&font.decode(bytes));
                                }
                                advance_over(
                                    bytes,
                                    fonts.get(&state.font),
                                    &state,
                                    &mut text_matrix,
                                );
                            }
                            Operand::Number(adjustment) => {
                                let shift =
                                    -adjustment / 1000.0 * state.font_size * state.horizontal_scale;
                                text_matrix =
                                    Matrix::multiply(Matrix::translate(shift, 0.0), text_matrix);
                            }
                            _ => {}
                        }
                    }
                    let end = Matrix::multiply(text_matrix, state.ctm).point();
                    push_run(
                        &mut runs,
                        ShownRun {
                            text: Some(text),
                            state: state.clone(),
                            start,
                            end,
                            matrix: start_matrix,
                            block: current_block(&marks),
                            show_start: operator.start,
                            show_end: operator.end,
                        },
                    );
                }
            }
            "'" => {
                line_matrix = Matrix::multiply(Matrix::translate(0.0, -state.leading), line_matrix);
                text_matrix = line_matrix;
                if let Some(Operand::Text(bytes)) = operator.operands.last() {
                    let start_matrix = text_matrix.0;
                    let start = Matrix::multiply(text_matrix, state.ctm).point();
                    let text = fonts.get(&state.font).map(|font| font.decode(bytes));
                    advance_over(bytes, fonts.get(&state.font), &state, &mut text_matrix);
                    let end = Matrix::multiply(text_matrix, state.ctm).point();
                    push_run(
                        &mut runs,
                        ShownRun {
                            text,
                            state: state.clone(),
                            start,
                            end,
                            matrix: start_matrix,
                            block: current_block(&marks),
                            show_start: operator.start,
                            show_end: operator.end,
                        },
                    );
                }
            }
            "\"" => {
                if operator.operands.len() >= 3 {
                    if let Some(value) = operator.operands[0].as_number() {
                        state.word_spacing = value;
                    }
                    if let Some(value) = operator.operands[1].as_number() {
                        state.char_spacing = value;
                    }
                }
                line_matrix = Matrix::multiply(Matrix::translate(0.0, -state.leading), line_matrix);
                text_matrix = line_matrix;
                if let Some(Operand::Text(bytes)) = operator.operands.last() {
                    let start_matrix = text_matrix.0;
                    let start = Matrix::multiply(text_matrix, state.ctm).point();
                    let text = fonts.get(&state.font).map(|font| font.decode(bytes));
                    advance_over(bytes, fonts.get(&state.font), &state, &mut text_matrix);
                    let end = Matrix::multiply(text_matrix, state.ctm).point();
                    push_run(
                        &mut runs,
                        ShownRun {
                            text,
                            state: state.clone(),
                            start,
                            end,
                            matrix: start_matrix,
                            block: current_block(&marks),
                            show_start: operator.start,
                            show_end: operator.end,
                        },
                    );
                }
            }
            "BDC" | "BMC" => {
                mark_counter = mark_counter.saturating_add(1);
                let tag = operator
                    .operands
                    .first()
                    .and_then(Operand::as_name)
                    .unwrap_or("")
                    .to_owned();
                let mcid = operator
                    .operands
                    .get(1)
                    .and_then(|operand| match operand {
                        Operand::Dictionary(entries) => entries
                            .iter()
                            .find(|(key, _)| key == "MCID")
                            .and_then(|(_, value)| value.as_number()),
                        _ => None,
                    })
                    .map(|value| value as i64);
                let key = match mcid {
                    Some(mcid) => format!("{tag}#{mcid}"),
                    None => format!("{tag}#{mark_counter}"),
                };
                marks.push(BlockMark { key });
            }
            "EMC" => {
                marks.pop();
            }
            _ => {}
        }
    }
    Ok(runs)
}

/// Name of the current marked-content paragraph, when one is open.
fn current_block(marks: &[BlockMark]) -> Option<String> {
    marks.last().map(|mark| mark.key.clone())
}

/// One shown string ready to be recorded.
struct ShownRun {
    text: Option<String>,
    state: GraphicsState,
    start: (f64, f64),
    end: (f64, f64),
    matrix: [f64; 6],
    block: Option<String>,
    show_start: usize,
    show_end: usize,
}

/// Record one shown string as a run when it has visible text.
fn push_run(runs: &mut Vec<RawRun>, shown: ShownRun) {
    let Some(text) = shown.text else {
        return;
    };
    if text.trim().is_empty() {
        return;
    }
    runs.push(RawRun {
        text,
        font: shown.state.font,
        font_size: shown.state.font_size,
        start_x: shown.start.0,
        start_y: shown.start.1,
        end_x: shown.end.0,
        matrix: shown.matrix,
        block: shown.block,
        show_start: shown.show_start,
        show_end: shown.show_end,
    });
}

/// Advance `text_matrix` over one shown string and return the text-space advance.
fn advance_over(
    bytes: &[u8],
    font: Option<&FontInfo>,
    state: &GraphicsState,
    text_matrix: &mut Matrix,
) -> f64 {
    let Some(font) = font else {
        return 0.0;
    };
    let mut advance = 0.0;
    for (code, _) in font.code_units(bytes) {
        let mut width = font.width_of_code(code) / 1000.0 * state.font_size;
        width += state.char_spacing;
        if code == 32 {
            width += state.word_spacing;
        }
        advance += width;
    }
    advance *= state.horizontal_scale;
    *text_matrix = Matrix::multiply(Matrix::translate(advance, 0.0), *text_matrix);
    advance
}

/// Build a matrix from the six leading numeric operands.
fn matrix_from(operands: &[Operand]) -> Option<Matrix> {
    if operands.len() < 6 {
        return None;
    }
    let values: Option<Vec<f64>> = operands.iter().take(6).map(Operand::as_number).collect();
    let values = values?;
    Some(Matrix([
        values[0], values[1], values[2], values[3], values[4], values[5],
    ]))
}

// ---------------------------------------------------------------------------
// Content tokenizer
// ---------------------------------------------------------------------------

/// Tokenize a content stream into operators with their byte spans.
///
/// Inline image data (`BI ... ID ... EI`) is skipped so binary bytes never
/// become operators. Malformed nesting and truncated strings are typed errors.
pub(crate) fn tokenize(content: &[u8]) -> Result<Vec<Operator>, TextEditError> {
    let mut parser = ContentParser {
        data: content,
        position: 0,
    };
    let mut operators = Vec::new();
    let mut operands = Vec::new();
    let mut first_operand: Option<usize> = None;
    loop {
        parser.skip_whitespace();
        if parser.position >= parser.data.len() {
            break;
        }
        let token_start = parser.position;
        let Some(operand) = parser.parse_operand()? else {
            break;
        };
        match operand {
            ParsedToken::Operator(name) => {
                let end = parser.position;
                let start = first_operand.take().unwrap_or(token_start);
                let inline_image = name == "BI";
                operators.push(Operator {
                    name,
                    operands: std::mem::take(&mut operands),
                    start,
                    end,
                });
                if inline_image {
                    parser.skip_inline_image()?;
                }
            }
            ParsedToken::Operand(value) => {
                if first_operand.is_none() {
                    first_operand = Some(token_start);
                }
                operands.push(value);
            }
        }
    }
    Ok(operators)
}

/// One parsed token.
enum ParsedToken {
    Operand(Operand),
    Operator(String),
}

/// A bounds-checked content-stream lexer.
struct ContentParser<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> ContentParser<'a> {
    fn byte(&self) -> Option<u8> {
        self.data.get(self.position).copied()
    }

    fn skip_whitespace(&mut self) {
        while let Some(byte) = self.byte() {
            match byte {
                b'\0' | b'\t' | b'\n' | b'\x0C' | b'\r' | b' ' => self.position += 1,
                b'%' => {
                    while let Some(byte) = self.byte() {
                        self.position += 1;
                        if byte == b'\n' || byte == b'\r' {
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
    }

    fn parse_operand(&mut self) -> Result<Option<ParsedToken>, TextEditError> {
        self.skip_whitespace();
        let Some(byte) = self.byte() else {
            return Ok(None);
        };
        match byte {
            b'/' => {
                self.position += 1;
                Ok(Some(ParsedToken::Operand(Operand::Name(self.read_name()?))))
            }
            b'(' => {
                self.position += 1;
                Ok(Some(ParsedToken::Operand(Operand::Text(
                    self.read_literal_string()?,
                ))))
            }
            b'<' if self.data.get(self.position + 1) == Some(&b'<') => {
                self.position += 2;
                Ok(Some(ParsedToken::Operand(Operand::Dictionary(
                    self.read_dictionary()?,
                ))))
            }
            b'<' => {
                self.position += 1;
                Ok(Some(ParsedToken::Operand(Operand::Text(
                    self.read_hex_string()?,
                ))))
            }
            b'[' => {
                self.position += 1;
                Ok(Some(ParsedToken::Operand(Operand::Array(
                    self.read_array()?,
                ))))
            }
            b']' | b'>' | b')' | b'}' | b'{' => {
                self.position += 1;
                Ok(None)
            }
            _ => {
                let start = self.position;
                while let Some(byte) = self.byte() {
                    if is_delimiter(byte) || byte.is_ascii_whitespace() {
                        break;
                    }
                    self.position += 1;
                }
                if start == self.position {
                    self.position += 1;
                    return Ok(None);
                }
                let token = &self.data[start..self.position];
                if let Ok(text) = std::str::from_utf8(token) {
                    if let Ok(value) = text.parse::<f64>() {
                        return Ok(Some(ParsedToken::Operand(Operand::Number(value))));
                    }
                    if text == "true" || text == "false" || text == "null" {
                        return Ok(Some(ParsedToken::Operand(Operand::Other)));
                    }
                    return Ok(Some(ParsedToken::Operator(text.to_owned())));
                }
                Ok(None)
            }
        }
    }

    fn read_name(&mut self) -> Result<String, TextEditError> {
        let mut name = String::new();
        while let Some(byte) = self.byte() {
            if is_delimiter(byte) || byte.is_ascii_whitespace() {
                break;
            }
            if byte == b'#'
                && let (Some(high), Some(low)) = (
                    self.data.get(self.position + 1).copied(),
                    self.data.get(self.position + 2).copied(),
                )
                && let (Some(high), Some(low)) = (hex_value(high), hex_value(low))
            {
                name.push(char::from(high * 16 + low));
                self.position += 3;
                continue;
            }
            name.push(char::from(byte));
            self.position += 1;
        }
        Ok(name)
    }

    fn read_literal_string(&mut self) -> Result<Vec<u8>, TextEditError> {
        let mut out = Vec::new();
        let mut depth = 1usize;
        while let Some(byte) = self.byte() {
            self.position += 1;
            match byte {
                b'\\' => self.read_escape(&mut out)?,
                b'(' => {
                    depth += 1;
                    out.push(byte);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(out);
                    }
                    out.push(byte);
                }
                _ => out.push(byte),
            }
        }
        Err(TextEditError::MalformedContent(
            "unterminated literal string".to_owned(),
        ))
    }

    fn read_escape(&mut self, out: &mut Vec<u8>) -> Result<(), TextEditError> {
        let Some(byte) = self.byte() else {
            return Err(TextEditError::MalformedContent(
                "truncated string escape".to_owned(),
            ));
        };
        self.position += 1;
        match byte {
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'b' => out.push(0x08),
            b'f' => out.push(0x0C),
            b'(' | b')' | b'\\' => out.push(byte),
            b'\n' => {}
            b'\r' => {
                if self.byte() == Some(b'\n') {
                    self.position += 1;
                }
            }
            b'0'..=b'7' => {
                let mut value = u16::from(byte - b'0');
                for _ in 0..2 {
                    match self.byte() {
                        Some(next @ b'0'..=b'7') => {
                            value = value * 8 + u16::from(next - b'0');
                            self.position += 1;
                        }
                        _ => break,
                    }
                }
                out.push((value & 0xFF) as u8);
            }
            other => out.push(other),
        }
        Ok(())
    }

    fn read_hex_string(&mut self) -> Result<Vec<u8>, TextEditError> {
        let mut digits = Vec::new();
        while let Some(byte) = self.byte() {
            self.position += 1;
            if byte == b'>' {
                break;
            }
            if byte.is_ascii_whitespace() {
                continue;
            }
            let Some(value) = hex_value(byte) else {
                return Err(TextEditError::MalformedContent(
                    "invalid hex string digit".to_owned(),
                ));
            };
            digits.push(value);
        }
        if digits.len() % 2 == 1 {
            digits.push(0);
        }
        Ok(digits
            .chunks(2)
            .map(|pair| (pair[0] << 4) | pair[1])
            .collect())
    }

    fn read_array(&mut self) -> Result<Vec<Operand>, TextEditError> {
        let mut array = Vec::new();
        loop {
            self.skip_whitespace();
            match self.byte() {
                None => {
                    return Err(TextEditError::MalformedContent(
                        "unterminated array".to_owned(),
                    ));
                }
                Some(b']') => {
                    self.position += 1;
                    return Ok(array);
                }
                _ => {}
            }
            match self.parse_operand()? {
                Some(ParsedToken::Operand(value)) => array.push(value),
                Some(ParsedToken::Operator(_)) | None => {
                    return Err(TextEditError::MalformedContent(
                        "operator inside array".to_owned(),
                    ));
                }
            }
        }
    }

    fn read_dictionary(&mut self) -> Result<Vec<(String, Operand)>, TextEditError> {
        let mut dictionary = Vec::new();
        loop {
            self.skip_whitespace();
            if self.data.get(self.position..self.position + 2) == Some(b">>") {
                self.position += 2;
                return Ok(dictionary);
            }
            let Some(ParsedToken::Operand(Operand::Name(key))) = self.parse_operand()? else {
                return Err(TextEditError::MalformedContent(
                    "dictionary key is not a name".to_owned(),
                ));
            };
            let Some(ParsedToken::Operand(value)) = self.parse_operand()? else {
                return Err(TextEditError::MalformedContent(
                    "dictionary value is missing".to_owned(),
                ));
            };
            dictionary.push((key, value));
        }
    }

    fn skip_inline_image(&mut self) -> Result<(), TextEditError> {
        // Find the `ID` operator after the image dictionary, then the first
        // whitespace-delimited `EI`.
        loop {
            self.skip_whitespace();
            let start = self.position;
            while let Some(byte) = self.byte() {
                if is_delimiter(byte) || byte.is_ascii_whitespace() {
                    break;
                }
                self.position += 1;
            }
            if start == self.position {
                return Err(TextEditError::MalformedContent(
                    "inline image header is truncated".to_owned(),
                ));
            }
            if &self.data[start..self.position] == b"ID" {
                if self.byte().is_some() {
                    self.position += 1;
                }
                break;
            }
        }
        while self.position + 1 < self.data.len() {
            if self.data[self.position].is_ascii_whitespace()
                && &self.data[self.position + 1..self.position + 3] == b"EI"
                && self
                    .data
                    .get(self.position + 3)
                    .is_none_or(|byte| is_delimiter(*byte) || byte.is_ascii_whitespace())
            {
                self.position += 3;
                return Ok(());
            }
            self.position += 1;
        }
        Err(TextEditError::MalformedContent(
            "unterminated inline image".to_owned(),
        ))
    }
}

/// Return `true` for a PDF delimiter byte.
fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

/// Decode one hexadecimal digit.
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Model construction
// ---------------------------------------------------------------------------

/// Turn raw runs into ordered lines and blocks.
fn build_model(page_index: usize, page: &pdfxi_core::Page, raw: Vec<RawRun>) -> PageTextModel {
    let mut blocks: Vec<Vec<RawRun>> = Vec::new();
    let mut grouped: BTreeMap<String, Vec<RawRun>> = BTreeMap::new();
    let mut unstructured: Vec<RawRun> = Vec::new();
    for run in raw {
        match &run.block {
            Some(key) => grouped.entry(key.clone()).or_default().push(run),
            None => unstructured.push(run),
        }
    }
    // Marked-content paragraphs keep their logical order of first appearance.
    let mut keys: Vec<(usize, String)> = grouped
        .keys()
        .map(|key| {
            let position = grouped
                .get(key)
                .and_then(|runs| runs.first())
                .map(|run| run.show_start)
                .unwrap_or(usize::MAX);
            (position, key.clone())
        })
        .collect();
    keys.sort();
    for (_, key) in keys {
        if let Some(runs) = grouped.remove(&key) {
            blocks.push(runs);
        }
    }
    if !unstructured.is_empty() {
        blocks.extend(cluster_blocks(unstructured));
    }

    let mut model_blocks = Vec::new();
    let mut run_index = 0usize;
    for (block_index, raw_runs) in blocks.into_iter().enumerate() {
        let mut sorted = raw_runs;
        sorted.sort_by(|left, right| {
            right
                .start_y
                .partial_cmp(&left.start_y)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    left.start_x
                        .partial_cmp(&right.start_x)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });
        let lines = group_lines(&sorted);
        let mut runs = Vec::new();
        let mut block_rect = None;
        let mut text_lines = Vec::new();
        for (line_index, line) in lines.iter().enumerate() {
            let mut line_rect = None;
            let mut line_indices = Vec::new();
            for raw in line {
                let size = raw.font_size.max(1.0);
                let height = size * 0.8;
                let descent = size * 0.2;
                let rect = Rect::saturating(
                    raw.start_x as f32,
                    (raw.start_y - descent) as f32,
                    raw.end_x as f32,
                    (raw.start_y + height) as f32,
                );
                line_rect = Some(match line_rect {
                    Some(existing) => union(existing, rect),
                    None => rect,
                });
                block_rect = Some(match block_rect {
                    Some(existing) => union(existing, rect),
                    None => rect,
                });
                line_indices.push(run_index);
                runs.push(TextRun {
                    index: run_index,
                    block: block_index,
                    line: line_index,
                    font: raw.font.clone(),
                    font_size: raw.font_size,
                    text: raw.text.clone(),
                    start: Point::new(raw.start_x as f32, raw.start_y as f32),
                    matrix: raw.matrix,
                    rect,
                    show_start: raw.show_start,
                    show_end: raw.show_end,
                });
                run_index += 1;
            }
            if let Some(rect) = line_rect {
                text_lines.push(TextLine {
                    rect,
                    run_indices: line_indices,
                });
            }
        }
        let rect = block_rect.unwrap_or_else(|| {
            Rect::saturating(
                page.media_box.left as f32,
                page.media_box.bottom as f32,
                page.media_box.left as f32,
                page.media_box.bottom as f32,
            )
        });
        model_blocks.push(TextBlock {
            page_index,
            index: block_index,
            rect,
            width: rect.width().max(MIN_BLOCK_WIDTH as f32),
            lines: text_lines,
            runs,
        });
    }
    PageTextModel {
        page_index,
        blocks: model_blocks,
    }
}

/// Geometric fallback grouping for content without marked content.
fn cluster_blocks(runs: Vec<RawRun>) -> Vec<Vec<RawRun>> {
    let mut sorted = runs;
    sorted.sort_by(|left, right| {
        right
            .start_y
            .partial_cmp(&left.start_y)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                left.start_x
                    .partial_cmp(&right.start_x)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    let mut blocks: Vec<Vec<RawRun>> = Vec::new();
    for run in sorted {
        let mut placed = false;
        if let Some(block) = blocks.last_mut()
            && let Some(previous) = block.last()
        {
            let size = previous.font_size.max(1.0);
            let gap = previous.start_y - run.start_y;
            let horizontal_overlap =
                previous.start_x.min(run.end_x) - run.start_x.max(previous.start_x);
            let same_line = gap.abs() <= size * 0.5;
            let within_paragraph = gap >= 0.0 && gap <= size * 1.8;
            if same_line || (within_paragraph && horizontal_overlap >= 0.0) {
                block.push(run.clone());
                placed = true;
            }
        }
        if !placed {
            blocks.push(vec![run]);
        }
    }
    blocks
}

/// Split a block's baseline-sorted runs into lines.
fn group_lines(runs: &[RawRun]) -> Vec<Vec<RawRun>> {
    let mut lines: Vec<Vec<RawRun>> = Vec::new();
    for run in runs {
        let tolerance = run.font_size.max(1.0) * 0.5;
        if let Some(line) = lines.last_mut()
            && let Some(first) = line.first()
            && (first.start_y - run.start_y).abs() <= tolerance
        {
            line.push(run.clone());
            continue;
        }
        lines.push(vec![run.clone()]);
    }
    lines
}

/// Union of two rectangles.
fn union(left: Rect, right: Rect) -> Rect {
    Rect::saturating(
        left.x0().min(right.x0()),
        left.y0().min(right.y0()),
        left.x1().max(right.x1()),
        left.y1().max(right.y1()),
    )
}

// ---------------------------------------------------------------------------
// Byte helpers
// ---------------------------------------------------------------------------

/// Replace the first span with `replacement` and delete the remaining spans.
fn splice_spans(content: &[u8], spans: &[(usize, usize)], replacement: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(content.len() + replacement.len());
    let mut cursor = 0usize;
    for (index, (start, end)) in spans.iter().enumerate() {
        if *start < cursor || *end > content.len() || start >= end {
            continue;
        }
        out.extend_from_slice(&content[cursor..*start]);
        if index == 0 {
            out.extend_from_slice(replacement);
        }
        cursor = *end;
    }
    out.extend_from_slice(&content[cursor..]);
    out
}

/// Lowercase hexadecimal encoding, deterministic and byte-bounded.
fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Allocate `count` unused object identifiers.
fn allocate_ids(document: &Document, count: usize) -> Result<Vec<ObjectId>, TextEditError> {
    let mut candidate = document.allocate_object_number()?;
    let mut ids = Vec::with_capacity(count);
    while ids.len() < count {
        if candidate == 0 || document.is_object_number_used(candidate) {
            candidate = candidate
                .checked_add(1)
                .ok_or(TextEditError::ObjectSpaceExhausted)?;
            continue;
        }
        ids.push(ObjectId::new(candidate, 0));
        candidate = candidate
            .checked_add(1)
            .ok_or(TextEditError::ObjectSpaceExhausted)?;
    }
    Ok(ids)
}

// ---------------------------------------------------------------------------
// Image decoding
// ---------------------------------------------------------------------------

/// One decoded image ready to embed.
struct ImageData {
    width: u32,
    height: u32,
    color_space: &'static str,
    data: Vec<u8>,
    jpeg: bool,
}

impl ImageData {
    /// Decode an eight-bit PNG into greyscale or RGB samples.
    fn from_png(bytes: &[u8]) -> Result<Self, TextEditError> {
        if bytes.len() < 8 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
            return Err(TextEditError::Png("missing signature".to_owned()));
        }
        let mut position = 8usize;
        let mut width = 0u32;
        let mut height = 0u32;
        let mut depth = 0u8;
        let mut color_type = 0u8;
        let mut palette: Vec<[u8; 3]> = Vec::new();
        let mut idat = Vec::new();
        let mut saw_header = false;
        while position + 8 <= bytes.len() {
            let length = u32::from_be_bytes([
                bytes[position],
                bytes[position + 1],
                bytes[position + 2],
                bytes[position + 3],
            ]) as usize;
            let kind = &bytes[position + 4..position + 8];
            let start = position + 8;
            let end = start
                .checked_add(length)
                .ok_or_else(|| TextEditError::Png("chunk length overflow".to_owned()))?;
            if end + 4 > bytes.len() {
                return Err(TextEditError::Png("truncated chunk".to_owned()));
            }
            let payload = &bytes[start..end];
            match kind {
                b"IHDR" => {
                    if payload.len() < 13 {
                        return Err(TextEditError::Png("truncated IHDR".to_owned()));
                    }
                    width = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
                    height = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
                    depth = payload[8];
                    color_type = payload[9];
                    if payload[10] != 0 || payload[11] != 0 || payload[12] != 0 {
                        return Err(TextEditError::Png(
                            "unsupported compression, filter or interlace".to_owned(),
                        ));
                    }
                    saw_header = true;
                }
                b"PLTE" => {
                    palette = payload
                        .chunks(3)
                        .filter(|chunk| chunk.len() == 3)
                        .map(|chunk| [chunk[0], chunk[1], chunk[2]])
                        .collect();
                }
                b"IDAT" => idat.extend_from_slice(payload),
                b"IEND" => break,
                _ => {}
            }
            position = end + 4;
        }
        if !saw_header {
            return Err(TextEditError::Png("missing IHDR".to_owned()));
        }
        if width == 0 || height == 0 || width > 20_000 || height > 20_000 {
            return Err(TextEditError::Png("unsupported dimensions".to_owned()));
        }
        if depth != 8 {
            return Err(TextEditError::Png(format!(
                "unsupported bit depth {depth}; only 8 is supported"
            )));
        }
        let (components, color_space, palette_indexed) = match color_type {
            0 => (1usize, "DeviceGray", false),
            2 => (3usize, "DeviceRGB", false),
            3 => (1usize, "DeviceRGB", true),
            4 => (1usize, "DeviceGray", false),
            6 => (3usize, "DeviceRGB", false),
            other => {
                return Err(TextEditError::Png(format!(
                    "unsupported color type {other}"
                )));
            }
        };
        if palette_indexed && palette.is_empty() {
            return Err(TextEditError::Png("palette image has no PLTE".to_owned()));
        }
        let stride = (width as usize)
            .checked_mul(components)
            .ok_or_else(|| TextEditError::Png("row stride overflow".to_owned()))?;
        let expected = (stride + 1)
            .checked_mul(height as usize)
            .ok_or_else(|| TextEditError::Png("image size overflow".to_owned()))?;
        let raw = decode_stream(&idat, &[Filter::FlateDecode(FlateParams::default())])?;
        if raw.len() < expected {
            return Err(TextEditError::Png("inflated data is truncated".to_owned()));
        }
        let mut samples = vec![0u8; stride * height as usize];
        unfilter_png(&raw, &mut samples, stride, height as usize, components)?;
        let data = if palette_indexed {
            let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
            for index in samples {
                let entry = palette
                    .get(usize::from(index))
                    .copied()
                    .unwrap_or([0, 0, 0]);
                rgb.extend_from_slice(&entry);
            }
            rgb
        } else {
            samples
        };
        Ok(Self {
            width,
            height,
            color_space,
            data,
            jpeg: false,
        })
    }

    /// Read dimensions and colour space from JPEG markers without re-encoding.
    fn from_jpeg(bytes: &[u8]) -> Result<Self, TextEditError> {
        if bytes.len() < 4 || bytes[..2] != [0xFF, 0xD8] {
            return Err(TextEditError::Jpeg("missing SOI marker".to_owned()));
        }
        let mut position = 2usize;
        while position + 4 <= bytes.len() {
            if bytes[position] != 0xFF {
                position += 1;
                continue;
            }
            let marker = bytes[position + 1];
            if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) {
                position += 2;
                continue;
            }
            let length = u16::from_be_bytes([bytes[position + 2], bytes[position + 3]]) as usize;
            if length < 2 || position + 2 + length > bytes.len() {
                return Err(TextEditError::Jpeg("truncated marker segment".to_owned()));
            }
            if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
                let payload = &bytes[position + 4..position + 2 + length];
                if payload.len() < 6 {
                    return Err(TextEditError::Jpeg("truncated frame header".to_owned()));
                }
                let precision = payload[0];
                if precision != 8 {
                    return Err(TextEditError::Jpeg(
                        "only 8-bit JPEG samples are supported".to_owned(),
                    ));
                }
                let height = u32::from(u16::from_be_bytes([payload[1], payload[2]]));
                let width = u32::from(u16::from_be_bytes([payload[3], payload[4]]));
                let components = payload[5];
                let color_space = match components {
                    1 => "DeviceGray",
                    3 => "DeviceRGB",
                    4 => "DeviceCMYK",
                    _ => {
                        return Err(TextEditError::Jpeg(format!(
                            "unsupported component count {components}"
                        )));
                    }
                };
                if width == 0 || height == 0 {
                    return Err(TextEditError::Jpeg("zero image dimension".to_owned()));
                }
                return Ok(Self {
                    width,
                    height,
                    color_space,
                    data: bytes.to_vec(),
                    jpeg: true,
                });
            }
            position += 2 + length;
        }
        Err(TextEditError::Jpeg(
            "no frame header (SOF) found".to_owned(),
        ))
    }
}

/// Reverse PNG scanline filters in place.
fn unfilter_png(
    raw: &[u8],
    output: &mut [u8],
    stride: usize,
    height: usize,
    bytes_per_pixel: usize,
) -> Result<(), TextEditError> {
    let mut previous = vec![0u8; stride];
    let mut current = vec![0u8; stride];
    for row in 0..height {
        let filter = raw[row * (stride + 1)];
        let start = row * (stride + 1) + 1;
        current.copy_from_slice(&raw[start..start + stride]);
        match filter {
            0 => {}
            1 => {
                for index in bytes_per_pixel..stride {
                    current[index] = current[index].wrapping_add(current[index - bytes_per_pixel]);
                }
            }
            2 => {
                for index in 0..stride {
                    current[index] = current[index].wrapping_add(previous[index]);
                }
            }
            3 => {
                for index in 0..stride {
                    let left = if index >= bytes_per_pixel {
                        u16::from(current[index - bytes_per_pixel])
                    } else {
                        0
                    };
                    let up = u16::from(previous[index]);
                    current[index] = current[index].wrapping_add(((left + up) / 2) as u8);
                }
            }
            4 => {
                for index in 0..stride {
                    current[index] = current[index].wrapping_add(paeth(
                        if index >= bytes_per_pixel {
                            current[index - bytes_per_pixel]
                        } else {
                            0
                        },
                        previous[index],
                        if index >= bytes_per_pixel {
                            previous[index - bytes_per_pixel]
                        } else {
                            0
                        },
                    ));
                }
            }
            other => {
                return Err(TextEditError::Png(format!(
                    "unsupported scanline filter {other}"
                )));
            }
        }
        output[row * stride..(row + 1) * stride].copy_from_slice(&current);
        previous.copy_from_slice(&current);
    }
    Ok(())
}

/// PNG Paeth predictor.
fn paeth(left: u8, up: u8, upper_left: u8) -> u8 {
    let left = i16::from(left);
    let up = i16::from(up);
    let upper_left = i16::from(upper_left);
    let estimate = left + up - upper_left;
    let distance_left = (estimate - left).abs();
    let distance_up = (estimate - up).abs();
    let distance_upper_left = (estimate - upper_left).abs();
    if distance_left <= distance_up && distance_left <= distance_upper_left {
        left as u8
    } else if distance_up <= distance_upper_left {
        up as u8
    } else {
        upper_left as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfxi_core::Document;

    fn fixture(name: &str) -> Document {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/realworld")
            .join(name);
        let bytes =
            std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        Document::from_bytes(bytes)
            .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
    }

    fn output_path(name: &str) -> std::path::PathBuf {
        let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/lanes/author2/t0233-out");
        std::fs::create_dir_all(&directory).expect("create output directory");
        directory.join(name)
    }

    #[test]
    fn fixture_model_has_heading_and_sentence_blocks() {
        let document = fixture("microsoft-excel-2019-sample.pdf");
        let model = text_blocks(&document, 0).expect("text model");
        assert!(!model.blocks.is_empty());
        let heading = model
            .blocks
            .iter()
            .find(|block| {
                block
                    .runs
                    .iter()
                    .any(|run| run.text.contains("PDFXI Real-World Sample"))
            })
            .expect("heading block");
        let sentence = model
            .blocks
            .iter()
            .find(|block| {
                block
                    .runs
                    .iter()
                    .any(|run| run.text.contains("orchestra rehearsed"))
            })
            .expect("sentence block");
        assert!(heading.rect.y0() > sentence.rect.y1());
        assert!(heading.rect.x0() >= 0.0);
        assert!(heading.rect.x1() <= 612.0);
        assert!(heading.rect.y1() <= 792.0);
        assert!(sentence.rect.x0() >= 0.0);
        assert!(sentence.rect.x1() <= 612.0);
        assert!(!heading.rect.intersects(&sentence.rect));
        assert_eq!(heading.runs[0].font, "F1");
        assert!((heading.runs[0].font_size - 11.04).abs() < 1e-6);
        assert!(heading.runs[0].rect.width() > 50.0);
    }

    #[test]
    fn replace_sentence_reflows_and_extracts() {
        let mut document = fixture("microsoft-excel-2019-sample.pdf");
        let model = text_blocks(&document, 0).expect("text model");
        let sentence = model
            .blocks
            .iter()
            .find(|block| {
                block
                    .runs
                    .iter()
                    .any(|run| run.text.contains("orchestra rehearsed"))
            })
            .expect("sentence block");
        let run = sentence.runs.first().expect("sentence run");
        let new_text =
            "The replacement sentence was written by PDFXI and survives a full save and reopen.";
        let outcome = replace_run_text(&mut document, 0, run.index, new_text).expect("replace run");
        assert_eq!(outcome.block_index, sentence.index);
        assert!(outcome.font.embedded);
        assert_eq!(outcome.font.base_font, FALLBACK_BASE_FONT);
        assert!(
            outcome.lines > 1,
            "the long replacement must exercise wrapping, got {} line(s)",
            outcome.lines
        );
        let saved = document.save().expect("full save");
        let reparse = Document::from_bytes(saved.clone()).expect("reparse");
        let text = reparse.page_text(0).expect("page text");
        assert!(text.contains(new_text), "extracted text was {text:?}");
        assert!(!text.contains("orchestra"), "old text survived: {text:?}");
        let heading = text.find("PDFXI Real-World Sample").expect("heading order");
        let replacement = text.find(new_text).expect("replacement order");
        assert!(heading < replacement, "heading must precede body");
    }

    #[test]
    fn add_text_and_png_on_page_two() {
        let mut document = fixture("microsoft-excel-2019-sample.pdf");
        let text = add_text_box(
            &mut document,
            1,
            Rect::saturating(72.0, 600.0, 420.0, 660.0),
            "Edited by PDFXI",
            14.0,
            TextAlign::Left,
        )
        .expect("add text");
        assert_eq!(text.lines, 1);
        assert!(text.font.embedded);
        let png = test_png(8, 8, [220, 40, 90]);
        let image = add_image(
            &mut document,
            1,
            Rect::saturating(72.0, 480.0, 172.0, 560.0),
            &png,
            ImageFormat::Png,
        )
        .expect("add image");
        assert_eq!(image.pixel_width, 8);
        assert_eq!(image.pixel_height, 8);
        let saved = document.save().expect("full save");
        let reparse = Document::from_bytes(saved.clone()).expect("reparse");
        let page_text = reparse.page_text(1).expect("page text");
        assert!(page_text.contains("Edited by PDFXI"));
        let page = reparse.page(1).expect("page");
        let resources = page_resources(&reparse, &page)
            .expect("resources")
            .expect("resource object");
        let resources = resources.as_dictionary().expect("resource dictionary");
        let xobjects = resolve(&reparse, resources.get("XObject"))
            .expect("xobject")
            .expect("xobject object");
        let xobjects = xobjects.as_dictionary().expect("xobject dictionary");
        assert!(xobjects.values().any(|value| {
            value
                .as_reference()
                .and_then(|id| reparse.object(id).ok())
                .and_then(|object| {
                    object
                        .as_dictionary()
                        .and_then(|dictionary| dictionary.get("Subtype"))
                        .and_then(Object::as_name)
                        .map(|name| name == "Image")
                })
                .unwrap_or(false)
        }));
    }

    #[test]
    fn replace_heading_keeps_page_geometry() {
        let mut document = fixture("microsoft-excel-2019-sample.pdf");
        let model = text_blocks(&document, 0).expect("text model");
        let heading = model
            .blocks
            .iter()
            .find(|block| {
                block
                    .runs
                    .iter()
                    .any(|run| run.text.contains("PDFXI Real-World Sample"))
            })
            .expect("heading block");
        let run = heading.runs.first().expect("heading run");
        let outcome =
            replace_run_text(&mut document, 0, run.index, "A New Heading").expect("replace");
        assert_eq!(outcome.lines, 1);
        let saved = document.save().expect("save");
        let reparse = Document::from_bytes(saved).expect("reparse");
        let text = reparse.page_text(0).expect("text");
        assert!(text.contains("A New Heading"));
        assert!(text.contains("orchestra rehearsed"));
    }

    #[test]
    fn full_artifact_has_replacement_text_and_added_image() {
        let mut document = fixture("microsoft-excel-2019-sample.pdf");
        let model = text_blocks(&document, 0).expect("text model");
        let sentence = model
            .blocks
            .iter()
            .find(|block| {
                block
                    .runs
                    .iter()
                    .any(|run| run.text.contains("orchestra rehearsed"))
            })
            .expect("sentence block");
        let run = sentence.runs.first().expect("sentence run");
        replace_run_text(
            &mut document,
            0,
            run.index,
            "PDFXI replaced this sentence and reflowed it on the first page.",
        )
        .expect("replace sentence");
        add_text_box(
            &mut document,
            1,
            Rect::saturating(72.0, 600.0, 420.0, 660.0),
            "Edited by PDFXI",
            14.0,
            TextAlign::Left,
        )
        .expect("add text");
        let png = test_png(8, 8, [20, 120, 220]);
        add_image(
            &mut document,
            1,
            Rect::saturating(72.0, 480.0, 172.0, 560.0),
            &png,
            ImageFormat::Png,
        )
        .expect("add image");
        let saved = document.save().expect("full save");
        let reparse = Document::from_bytes(saved.clone()).expect("reparse");
        let first = reparse.page_text(0).expect("page one text");
        assert!(first.contains("PDFXI replaced this sentence and reflowed it on the first page."));
        assert!(!first.contains("orchestra"));
        let second = reparse.page_text(1).expect("page two text");
        assert!(second.contains("Edited by PDFXI"));
        std::fs::write(output_path("edited.pdf"), saved).expect("write artifact");
    }

    #[test]
    fn cid_assignment_and_width_array_use_spec_forms() {
        let ttf = TrueTypeFont::parse(FALLBACK_FONT).expect("bundled font must parse");
        let characters = BTreeSet::from(['a', 'b', ' ']);
        let fallback = FallbackFont::new(ttf, &characters).expect("CID assignment");
        assert_eq!(fallback.cid_of.get(&' '), Some(&1));
        assert_eq!(fallback.cid_of.get(&'a'), Some(&2));
        assert_eq!(fallback.cid_of.get(&'b'), Some(&3));
        assert_eq!(
            fallback.encode(" ab").expect("encode"),
            vec![0, 1, 0, 2, 0, 3]
        );

        let widths = [
            GlyphWidth { cid: 1, width: 600 },
            GlyphWidth { cid: 2, width: 600 },
            GlyphWidth { cid: 4, width: 500 },
        ];
        assert_eq!(
            build_width_array(&widths),
            vec![
                Object::Integer(1),
                Object::Integer(2),
                Object::Integer(600),
                Object::Integer(4),
                Object::Array(vec![Object::Integer(500)]),
            ]
        );
        assert_eq!(scale_to_1000(1228, 2048), 600);
    }

    #[test]
    fn png_decoding_rejects_bad_input() {
        assert!(matches!(
            ImageData::from_png(b"not png"),
            Err(TextEditError::Png(_))
        ));
        assert!(matches!(
            ImageData::from_jpeg(b"not jpeg"),
            Err(TextEditError::Jpeg(_))
        ));
    }

    #[test]
    fn list_objects_reports_added_text_and_image() {
        let mut document = fixture("microsoft-excel-2019-sample.pdf");
        add_text_box(
            &mut document,
            1,
            Rect::saturating(72.0, 600.0, 420.0, 660.0),
            "Edited by PDFXI",
            14.0,
            TextAlign::Left,
        )
        .expect("add text");
        let png = test_png(8, 8, [220, 40, 90]);
        add_image(
            &mut document,
            1,
            Rect::saturating(72.0, 480.0, 172.0, 560.0),
            &png,
            ImageFormat::Png,
        )
        .expect("add image");

        let objects = list_objects(&document, 1).expect("list objects");
        let text = objects
            .iter()
            .find(|object| object.text_preview.contains("Edited by PDFXI"))
            .expect("text block is listed");
        assert_eq!(text.kind, ObjectKind::TextBlock);
        assert_eq!(text.page, 1);
        assert!((text.rect.x0() - 72.0).abs() < 2.0);
        assert!(text.rect.y1() > 640.0 && text.rect.y1() <= 660.0);
        assert!(text.rect.width() > 50.0);

        let image = objects
            .iter()
            .find(|object| object.kind == ObjectKind::Image)
            .expect("image is listed");
        assert!(image.id.starts_with("image:"));
        assert!(image.text_preview.is_empty());
        assert!((image.rect.x0() - 72.0).abs() < 1e-3);
        assert!((image.rect.y0() - 480.0).abs() < 1e-3);
        assert!((image.rect.x1() - 172.0).abs() < 1e-3);
        assert!((image.rect.y1() - 560.0).abs() < 1e-3);
    }

    #[test]
    fn move_text_block_updates_geometry_and_extraction() {
        let mut document = fixture("microsoft-excel-2019-sample.pdf");
        add_text_box(
            &mut document,
            1,
            Rect::saturating(72.0, 600.0, 300.0, 640.0),
            "Move me",
            14.0,
            TextAlign::Left,
        )
        .expect("add text");
        let before = list_objects(&document, 1)
            .expect("list")
            .into_iter()
            .find(|object| object.text_preview.contains("Move me"))
            .expect("text block");
        move_object(&mut document, 1, &before.id, 30.0, -40.0).expect("move text");
        let after = list_objects(&document, 1)
            .expect("list after move")
            .into_iter()
            .find(|object| object.text_preview.contains("Move me"))
            .expect("moved text block");
        assert!((after.rect.x0() - (before.rect.x0() + 30.0)).abs() < 0.5);
        assert!((after.rect.y0() - (before.rect.y0() - 40.0)).abs() < 0.5);

        let saved = document.save().expect("save");
        let reparse = Document::from_bytes(saved).expect("reparse");
        let text = reparse.page_text(1).expect("text");
        assert!(text.contains("Move me"), "extracted text was {text:?}");
        let block = text_blocks(&reparse, 1)
            .expect("model")
            .blocks
            .into_iter()
            .find(|block| block.runs.iter().any(|run| run.text.contains("Move me")))
            .expect("moved block");
        assert!((block.rect.x0() - (before.rect.x0() + 30.0)).abs() < 0.5);
        assert!((block.rect.y0() - (before.rect.y0() - 40.0)).abs() < 0.5);
    }

    #[test]
    fn move_existing_fixture_heading_keeps_other_text() {
        let mut document = fixture("microsoft-excel-2019-sample.pdf");
        let heading = list_objects(&document, 0)
            .expect("list")
            .into_iter()
            .find(|object| object.text_preview.contains("PDFXI Real-World Sample"))
            .expect("heading");
        let sentence = list_objects(&document, 0)
            .expect("list")
            .into_iter()
            .find(|object| object.text_preview.contains("orchestra rehearsed"))
            .expect("sentence");
        move_object(&mut document, 0, &heading.id, 30.0, -40.0).expect("move heading");
        let saved = document.save().expect("save");
        let reparse = Document::from_bytes(saved).expect("reparse");
        let model = text_blocks(&reparse, 0).expect("model");
        let moved = model
            .blocks
            .iter()
            .find(|block| {
                block
                    .runs
                    .iter()
                    .any(|run| run.text.contains("PDFXI Real-World Sample"))
            })
            .expect("moved heading");
        assert!((moved.rect.x0() - (heading.rect.x0() + 30.0)).abs() < 0.75);
        assert!((moved.rect.y0() - (heading.rect.y0() - 40.0)).abs() < 0.75);
        let unchanged = model
            .blocks
            .iter()
            .find(|block| {
                block
                    .runs
                    .iter()
                    .any(|run| run.text.contains("orchestra rehearsed"))
            })
            .expect("sentence");
        assert!((unchanged.rect.x0() - sentence.rect.x0()).abs() < 0.01);
        assert!((unchanged.rect.y0() - sentence.rect.y0()).abs() < 0.01);
        let text = reparse.page_text(0).expect("text");
        assert!(text.contains("PDFXI Real-World Sample"));
        assert!(text.contains("orchestra rehearsed"));
    }

    #[test]
    fn move_image_changes_only_the_selected_usage() {
        let mut document = fixture("microsoft-excel-2019-sample.pdf");
        let first = add_image(
            &mut document,
            1,
            Rect::saturating(72.0, 400.0, 172.0, 500.0),
            &test_png(8, 8, [220, 40, 90]),
            ImageFormat::Png,
        )
        .expect("first image");
        let second = add_image(
            &mut document,
            1,
            Rect::saturating(300.0, 400.0, 400.0, 500.0),
            &test_png(8, 8, [20, 120, 220]),
            ImageFormat::Png,
        )
        .expect("second image");
        let objects = list_objects(&document, 1).expect("list");
        let first_object = objects
            .iter()
            .find(|object| object.id == image_object_id(&first.resource, 0))
            .expect("first image object");
        let second_object = objects
            .iter()
            .find(|object| object.id == image_object_id(&second.resource, 0))
            .expect("second image object");

        move_object(&mut document, 1, &first_object.id, 40.0, -30.0).expect("move image");
        let moved = list_objects(&document, 1).expect("list after move");
        let moved_first = moved
            .iter()
            .find(|object| object.id == first_object.id)
            .expect("moved first image");
        let unmoved_second = moved
            .iter()
            .find(|object| object.id == second_object.id)
            .expect("second image");
        assert!((moved_first.rect.x0() - (first_object.rect.x0() + 40.0)).abs() < 0.01);
        assert!((moved_first.rect.y0() - (first_object.rect.y0() - 30.0)).abs() < 0.01);
        assert!((unmoved_second.rect.x0() - second_object.rect.x0()).abs() < 0.01);
        assert!((unmoved_second.rect.y0() - second_object.rect.y0()).abs() < 0.01);
    }

    #[test]
    fn delete_text_block_keeps_the_rest_of_the_page() {
        let mut document = fixture("microsoft-excel-2019-sample.pdf");
        add_text_box(
            &mut document,
            1,
            Rect::saturating(72.0, 600.0, 300.0, 640.0),
            "Keep me",
            14.0,
            TextAlign::Left,
        )
        .expect("keep text");
        add_text_box(
            &mut document,
            1,
            Rect::saturating(72.0, 540.0, 300.0, 580.0),
            "Delete me",
            14.0,
            TextAlign::Left,
        )
        .expect("delete text");
        let target = list_objects(&document, 1)
            .expect("list")
            .into_iter()
            .find(|object| object.text_preview.contains("Delete me"))
            .expect("target block");
        let keep = list_objects(&document, 1)
            .expect("list")
            .into_iter()
            .find(|object| object.text_preview.contains("Keep me"))
            .expect("keep block");
        delete_object(&mut document, 1, &target.id).expect("delete block");
        let saved = document.save().expect("save");
        let reparse = Document::from_bytes(saved).expect("reparse");
        let text = reparse.page_text(1).expect("text");
        assert!(text.contains("Keep me"), "extracted text was {text:?}");
        assert!(
            !text.contains("Delete me"),
            "deleted text survived: {text:?}"
        );
        let model = text_blocks(&reparse, 1).expect("model");
        assert!(
            !model
                .blocks
                .iter()
                .any(|block| block.runs.iter().any(|run| run.text.contains("Delete me")))
        );
        let kept = model
            .blocks
            .iter()
            .find(|block| block.runs.iter().any(|run| run.text.contains("Keep me")))
            .expect("kept block");
        assert!((kept.rect.x0() - keep.rect.x0()).abs() < 0.01);
        assert!((kept.rect.y0() - keep.rect.y0()).abs() < 0.01);
    }

    #[test]
    fn delete_unshared_image_removes_resource_and_object() {
        let mut document = fixture("microsoft-excel-2019-sample.pdf");
        let placement = add_image(
            &mut document,
            1,
            Rect::saturating(72.0, 480.0, 172.0, 560.0),
            &test_png(8, 8, [220, 40, 90]),
            ImageFormat::Png,
        )
        .expect("add image");
        let image_id = image_object_id(&placement.resource, 0);
        let image_object =
            image_object_id_for(&document, 1, &placement.resource).expect("image object reference");
        delete_object(&mut document, 1, &image_id).expect("delete image");
        let saved = document.save().expect("save");
        let reparse = Document::from_bytes(saved).expect("reparse");
        assert!(matches!(reparse.object(image_object), Ok(Object::Null)));
        let page = reparse.page(1).expect("page");
        let resources = page_resources(&reparse, &page)
            .expect("resources")
            .expect("resource dictionary");
        let resources = resources.as_dictionary().expect("resources dictionary");
        let xobjects = match resolve(&reparse, resources.get("XObject")) {
            Ok(Some(value)) => value.as_dictionary().cloned().unwrap_or_default(),
            _ => Dictionary::new(),
        };
        let image_count = xobjects
            .values()
            .filter(|value| {
                resolve(&reparse, Some(value))
                    .ok()
                    .flatten()
                    .and_then(|object| {
                        object
                            .as_dictionary()
                            .and_then(|dictionary| dictionary.get("Subtype"))
                            .and_then(Object::as_name)
                            .map(|name| name == "Image")
                    })
                    .unwrap_or(false)
            })
            .count();
        assert_eq!(image_count, 0, "unshared image resource survived");
        assert!(
            xobjects.keys().all(|name| name != &placement.resource),
            "resource entry {0} survived",
            placement.resource
        );
    }

    #[test]
    fn delete_shared_image_keeps_object_and_other_usage() {
        let mut document = fixture("microsoft-excel-2019-sample.pdf");
        let placement = add_image(
            &mut document,
            1,
            Rect::saturating(72.0, 480.0, 172.0, 560.0),
            &test_png(8, 8, [220, 40, 90]),
            ImageFormat::Png,
        )
        .expect("add image");
        let image_object =
            image_object_id_for(&document, 1, &placement.resource).expect("image object reference");
        append_page_content(
            &mut document,
            1,
            format!("q\n20 0 0 20 300 300 cm\n/{} Do\nQ\n", placement.resource).as_bytes(),
        )
        .expect("append second usage");
        let objects = list_objects(&document, 1).expect("list");
        let usages: Vec<&PageObject> = objects
            .iter()
            .filter(|object| object.kind == ObjectKind::Image)
            .collect();
        assert_eq!(usages.len(), 2);
        assert_eq!(usages[0].id, image_object_id(&placement.resource, 0));
        assert_eq!(usages[1].id, image_object_id(&placement.resource, 1));
        delete_object(&mut document, 1, &usages[0].id).expect("delete first usage");
        assert!(
            !matches!(document.object(image_object), Ok(Object::Null)),
            "shared image object must not be nulled"
        );
        let remaining = list_objects(&document, 1).expect("list after delete");
        assert_eq!(
            remaining
                .iter()
                .filter(|object| object.kind == ObjectKind::Image)
                .count(),
            1
        );
    }

    #[test]
    fn object_identifiers_are_validated() {
        let mut document = fixture("microsoft-excel-2019-sample.pdf");
        assert!(matches!(
            move_object(&mut document, 0, "paragraph:1", 1.0, 1.0),
            Err(ObjectEditError::InvalidObjectId { .. })
        ));
        assert!(matches!(
            move_object(&mut document, 0, "text:9999", 1.0, 1.0),
            Err(ObjectEditError::ObjectNotFound { .. })
        ));
        assert!(matches!(
            move_object(&mut document, 0, "text:0", f64::NAN, 1.0),
            Err(ObjectEditError::NonFiniteDelta)
        ));
        assert!(matches!(
            move_object(&mut document, 0, "text:0", 2_000_000.0, 0.0),
            Err(ObjectEditError::DeltaOutOfRange { .. })
        ));
        let heading = list_objects(&document, 0)
            .expect("list")
            .into_iter()
            .find(|object| object.kind == ObjectKind::TextBlock)
            .expect("first text block");
        assert!(matches!(
            move_object(&mut document, 0, &heading.id, 10_000.0, 0.0),
            Err(ObjectEditError::MoveOutOfBounds { .. })
        ));
    }

    #[test]
    fn session_undo_restores_text_object_geometry() {
        use pdfxi_document::{DocumentSession, SessionError};

        let bytes = std::fs::read(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/realworld/microsoft-excel-2019-sample.pdf"),
        )
        .expect("read fixture");
        let mut session = DocumentSession::open_bytes(&bytes).expect("open session");
        session
            .edit(|document| {
                add_text_box(
                    document,
                    1,
                    Rect::saturating(72.0, 600.0, 300.0, 640.0),
                    "Undo me",
                    14.0,
                    TextAlign::Left,
                )
                .map(|_| ())
                .map_err(session_error)
            })
            .expect("add text through session");
        let added = session.bytes().to_vec();
        let before = list_objects(session.document(), 1)
            .expect("list")
            .into_iter()
            .find(|object| object.text_preview.contains("Undo me"))
            .expect("text block");
        session
            .edit(|document| {
                move_object(document, 1, &before.id, 30.0, -40.0).map_err(session_error)
            })
            .expect("move through session");
        let moved = list_objects(session.document(), 1)
            .expect("list after move")
            .into_iter()
            .find(|object| object.text_preview.contains("Undo me"))
            .expect("moved block");
        assert!((moved.rect.x0() - (before.rect.x0() + 30.0)).abs() < 0.5);
        assert!(session.bytes().len() > added.len());

        session.undo().expect("undo move");
        assert_eq!(session.bytes(), added.as_slice());
        let restored = list_objects(session.document(), 1)
            .expect("list after undo")
            .into_iter()
            .find(|object| object.text_preview.contains("Undo me"))
            .expect("restored block");
        assert!((restored.rect.x0() - before.rect.x0()).abs() < 0.01);
        assert!((restored.rect.y0() - before.rect.y0()).abs() < 0.01);

        session.redo().expect("redo move");
        let redone = list_objects(session.document(), 1)
            .expect("list after redo")
            .into_iter()
            .find(|object| object.text_preview.contains("Undo me"))
            .expect("redone block");
        assert!((redone.rect.x0() - (before.rect.x0() + 30.0)).abs() < 0.5);

        fn session_error(error: impl std::fmt::Display) -> SessionError {
            SessionError::Io(std::io::Error::other(error.to_string()))
        }
    }

    #[test]
    fn resize_text_block_scales_geometry_and_extraction() {
        let mut document = fixture("microsoft-excel-2019-sample.pdf");
        add_text_box(
            &mut document,
            1,
            Rect::saturating(72.0, 540.0, 272.0, 584.0),
            "Resize this paragraph and keep every word extractable",
            12.0,
            TextAlign::Left,
        )
        .expect("add text to resize");
        add_text_box(
            &mut document,
            1,
            Rect::saturating(72.0, 420.0, 300.0, 460.0),
            "Keep me untouched",
            10.0,
            TextAlign::Left,
        )
        .expect("add kept text");
        let before = list_objects(&document, 1)
            .expect("list")
            .into_iter()
            .find(|object| object.text_preview.contains("Resize this paragraph"))
            .expect("text block to resize");
        let kept_before = list_objects(&document, 1)
            .expect("list")
            .into_iter()
            .find(|object| object.text_preview.contains("Keep me untouched"))
            .expect("kept block");

        resize_object(&mut document, 1, &before.id, 1.5, 2.0).expect("resize text");

        let after = list_objects(&document, 1)
            .expect("list after resize")
            .into_iter()
            .find(|object| object.text_preview.contains("Resize this paragraph"))
            .expect("resized block");
        assert!(
            (after.rect.width() - before.rect.width() * 1.5).abs() < 1.0,
            "width was {} expected {}",
            after.rect.width(),
            before.rect.width() * 1.5
        );
        assert!(
            (after.rect.height() - before.rect.height() * 2.0).abs() < 2.0,
            "height was {} expected {}",
            after.rect.height(),
            before.rect.height() * 2.0
        );
        assert!((after.rect.x0() - before.rect.x0()).abs() < 0.5);
        assert!((after.rect.y0() - before.rect.y0()).abs() < 0.5);
        let kept_after = list_objects(&document, 1)
            .expect("list after resize")
            .into_iter()
            .find(|object| object.text_preview.contains("Keep me untouched"))
            .expect("kept block");
        assert!((kept_after.rect.x0() - kept_before.rect.x0()).abs() < 0.01);
        assert!((kept_after.rect.y0() - kept_before.rect.y0()).abs() < 0.01);

        let saved = document.save().expect("save");
        let reparse = Document::from_bytes(saved).expect("reparse");
        let text = reparse.page_text(1).expect("text");
        assert!(text.contains("Resize this paragraph"), "text was {text:?}");
        assert!(text.contains("extractable"), "text was {text:?}");
        assert!(text.contains("Keep me untouched"), "text was {text:?}");
    }

    #[test]
    fn resize_image_scales_placement_only() {
        let mut document = fixture("microsoft-excel-2019-sample.pdf");
        let first = add_image(
            &mut document,
            1,
            Rect::saturating(72.0, 300.0, 172.0, 400.0),
            &test_png(8, 8, [220, 40, 90]),
            ImageFormat::Png,
        )
        .expect("first image");
        let second = add_image(
            &mut document,
            1,
            Rect::saturating(300.0, 300.0, 400.0, 400.0),
            &test_png(8, 8, [20, 120, 220]),
            ImageFormat::Png,
        )
        .expect("second image");
        let objects = list_objects(&document, 1).expect("list");
        let first_id = image_object_id(&first.resource, 0);
        let second_id = image_object_id(&second.resource, 0);
        let first_before = objects
            .iter()
            .find(|object| object.id == first_id)
            .expect("first image object")
            .clone();
        let second_before = objects
            .iter()
            .find(|object| object.id == second_id)
            .expect("second image object")
            .clone();

        resize_object(&mut document, 1, &first_id, 1.5, 0.5).expect("resize image");

        let after = list_objects(&document, 1).expect("list after resize");
        let first_after = after
            .iter()
            .find(|object| object.id == first_id)
            .expect("resized image");
        let second_after = after
            .iter()
            .find(|object| object.id == second_id)
            .expect("untouched image");
        assert!((first_after.rect.width() - first_before.rect.width() * 1.5).abs() < 0.01);
        assert!((first_after.rect.height() - first_before.rect.height() * 0.5).abs() < 0.01);
        assert!((first_after.rect.x0() - first_before.rect.x0()).abs() < 0.01);
        assert!((first_after.rect.y0() - first_before.rect.y0()).abs() < 0.01);
        assert!((second_after.rect.x0() - second_before.rect.x0()).abs() < 0.01);
        assert!((second_after.rect.y0() - second_before.rect.y0()).abs() < 0.01);
    }

    #[test]
    fn resize_rejects_invalid_scales_and_shared_placement() {
        let mut document = fixture("microsoft-excel-2019-sample.pdf");
        add_text_box(
            &mut document,
            1,
            Rect::saturating(72.0, 540.0, 272.0, 584.0),
            "Scale guard",
            12.0,
            TextAlign::Left,
        )
        .expect("add text");
        let text = list_objects(&document, 1)
            .expect("list")
            .into_iter()
            .find(|object| object.text_preview.contains("Scale guard"))
            .expect("text block");
        for (sx, sy) in [(0.0, 1.0), (-1.0, 1.0), (f64::NAN, 1.0), (1.0, 1001.0)] {
            assert!(
                matches!(
                    resize_object(&mut document, 1, &text.id, sx, sy),
                    Err(ObjectEditError::InvalidScale { .. })
                ),
                "scale ({sx}, {sy}) must be rejected"
            );
        }

        let first = add_image(
            &mut document,
            1,
            Rect::saturating(72.0, 300.0, 172.0, 400.0),
            &test_png(8, 8, [220, 40, 90]),
            ImageFormat::Png,
        )
        .expect("first image");
        let second = add_image(
            &mut document,
            1,
            Rect::saturating(300.0, 300.0, 400.0, 400.0),
            &test_png(8, 8, [20, 120, 220]),
            ImageFormat::Png,
        )
        .expect("second image");
        append_page_content(
            &mut document,
            1,
            format!(
                "q\n10 0 0 10 100 100 cm\n/{} Do\n/{} Do\nQ\n",
                first.resource, second.resource
            )
            .as_bytes(),
        )
        .expect("append shared placement");
        let shared_first = image_object_id(&first.resource, 1);
        assert!(matches!(
            resize_object(&mut document, 1, &shared_first, 2.0, 2.0),
            Err(ObjectEditError::UnsupportedContent(_))
        ));
    }

    #[test]
    fn replace_text_block_rewrites_content_and_extracts() {
        let mut document = fixture("microsoft-excel-2019-sample.pdf");
        add_text_box(
            &mut document,
            1,
            Rect::saturating(72.0, 540.0, 300.0, 584.0),
            "Old words here",
            12.0,
            TextAlign::Left,
        )
        .expect("add old text");
        add_text_box(
            &mut document,
            1,
            Rect::saturating(72.0, 420.0, 300.0, 460.0),
            "Keep this block",
            10.0,
            TextAlign::Left,
        )
        .expect("add kept text");
        let target = list_objects(&document, 1)
            .expect("list")
            .into_iter()
            .find(|object| object.text_preview.contains("Old words here"))
            .expect("target block");
        let kept_before = list_objects(&document, 1)
            .expect("list")
            .into_iter()
            .find(|object| object.text_preview.contains("Keep this block"))
            .expect("kept block");

        let outcome = replace_object(
            &mut document,
            1,
            &target.id,
            ObjectReplacement::Text(
                "Replacement text with several words that must reflow inside the same block",
            ),
        )
        .expect("replace text");
        assert_eq!(outcome.kind, ObjectKind::TextBlock);
        assert!(outcome.text_lines.is_some_and(|lines| lines >= 1));
        assert!(outcome.pixel_width.is_none());

        let saved = document.save().expect("save");
        let reparse = Document::from_bytes(saved).expect("reparse");
        let text = reparse.page_text(1).expect("text");
        assert!(text.contains("Replacement text"), "text was {text:?}");
        assert!(text.contains("reflow"), "text was {text:?}");
        assert!(text.contains("Keep this block"), "text was {text:?}");
        assert!(
            !text.contains("Old words here"),
            "old text survived: {text:?}"
        );
        let kept_after = list_objects(&reparse, 1)
            .expect("list after replace")
            .into_iter()
            .find(|object| object.text_preview.contains("Keep this block"))
            .expect("kept block");
        assert!((kept_after.rect.x0() - kept_before.rect.x0()).abs() < 0.01);
        assert!((kept_after.rect.y0() - kept_before.rect.y0()).abs() < 0.01);
    }

    #[test]
    fn replace_image_swaps_stream_and_keeps_placement() {
        let mut document = fixture("microsoft-excel-2019-sample.pdf");
        let placement = add_image(
            &mut document,
            1,
            Rect::saturating(72.0, 300.0, 172.0, 400.0),
            &test_png(8, 8, [220, 40, 90]),
            ImageFormat::Png,
        )
        .expect("add image");
        let image_id = image_object_id(&placement.resource, 0);
        let object_id =
            image_object_id_for(&document, 1, &placement.resource).expect("image object reference");
        let before = list_objects(&document, 1)
            .expect("list")
            .into_iter()
            .find(|object| object.id == image_id)
            .expect("image object");

        let outcome = replace_object(
            &mut document,
            1,
            &image_id,
            ObjectReplacement::Image {
                bytes: &test_png(4, 4, [20, 120, 220]),
                format: ImageFormat::Png,
            },
        )
        .expect("replace image");
        assert_eq!(outcome.kind, ObjectKind::Image);
        assert_eq!(outcome.pixel_width, Some(4));
        assert_eq!(outcome.pixel_height, Some(4));
        assert!(outcome.text_lines.is_none());

        let after = list_objects(&document, 1)
            .expect("list after replace")
            .into_iter()
            .find(|object| object.id == image_id)
            .expect("replaced image");
        assert!((after.rect.x0() - before.rect.x0()).abs() < 0.01);
        assert!((after.rect.y0() - before.rect.y0()).abs() < 0.01);
        assert!((after.rect.width() - before.rect.width()).abs() < 0.01);
        assert!((after.rect.height() - before.rect.height()).abs() < 0.01);

        let saved = document.save().expect("save");
        let reparse = Document::from_bytes(saved).expect("reparse");
        match reparse.object(object_id).expect("image object") {
            Object::Stream { dictionary, data } => {
                assert_eq!(dictionary.get("Width").and_then(Object::as_i64), Some(4));
                assert_eq!(dictionary.get("Height").and_then(Object::as_i64), Some(4));
                assert_eq!(
                    dictionary.get("ColorSpace").and_then(Object::as_name),
                    Some("DeviceRGB")
                );
                assert_eq!(data.len(), 4 * 4 * 3);
                assert_eq!(&data[..3], &[20, 120, 220]);
            }
            other => panic!("expected an image stream, got {other:?}"),
        }
    }

    #[test]
    fn replace_image_rejects_shared_resource() {
        let mut document = fixture("microsoft-excel-2019-sample.pdf");
        let placement = add_image(
            &mut document,
            1,
            Rect::saturating(72.0, 300.0, 172.0, 400.0),
            &test_png(8, 8, [220, 40, 90]),
            ImageFormat::Png,
        )
        .expect("add image");
        append_page_content(
            &mut document,
            1,
            format!("q\n20 0 0 20 300 300 cm\n/{} Do\nQ\n", placement.resource).as_bytes(),
        )
        .expect("append second usage");
        assert!(matches!(
            replace_object(
                &mut document,
                1,
                &image_object_id(&placement.resource, 0),
                ObjectReplacement::Image {
                    bytes: &test_png(4, 4, [20, 120, 220]),
                    format: ImageFormat::Png,
                },
            ),
            Err(ObjectEditError::SharedImageResource { .. })
        ));
    }

    #[test]
    fn session_undo_restores_resized_and_replaced_objects() {
        use pdfxi_document::{DocumentSession, SessionError};

        let bytes = std::fs::read(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/realworld/microsoft-excel-2019-sample.pdf"),
        )
        .expect("read fixture");
        let mut session = DocumentSession::open_bytes(&bytes).expect("open session");
        session
            .edit(|document| {
                add_text_box(
                    document,
                    1,
                    Rect::saturating(72.0, 540.0, 272.0, 584.0),
                    "Round B undo",
                    12.0,
                    TextAlign::Left,
                )
                .map(|_| ())
                .map_err(session_error)
            })
            .expect("add text through session");
        let added = session.bytes().to_vec();
        let target = list_objects(session.document(), 1)
            .expect("list")
            .into_iter()
            .find(|object| object.text_preview.contains("Round B undo"))
            .expect("text block");
        let before = target.rect;

        session
            .edit(|document| {
                resize_object(document, 1, &target.id, 1.5, 1.5).map_err(session_error)
            })
            .expect("resize through session");
        let resized = list_objects(session.document(), 1)
            .expect("list after resize")
            .into_iter()
            .find(|object| object.text_preview.contains("Round B undo"))
            .expect("resized block")
            .rect;
        assert!((resized.width() - before.width() * 1.5).abs() < 1.0);

        session.undo().expect("undo resize");
        assert_eq!(session.bytes(), added.as_slice());
        let restored = list_objects(session.document(), 1)
            .expect("list after undo")
            .into_iter()
            .find(|object| object.text_preview.contains("Round B undo"))
            .expect("restored block")
            .rect;
        assert!((restored.width() - before.width()).abs() < 0.01);

        session
            .edit(|document| {
                replace_object(
                    document,
                    1,
                    &target.id,
                    ObjectReplacement::Text("Round B replaced text"),
                )
                .map(|_| ())
                .map_err(session_error)
            })
            .expect("replace through session");
        let replaced = session.document().page_text(1).expect("text");
        assert!(replaced.contains("Round B replaced text"));
        session.undo().expect("undo replace");
        assert_eq!(session.bytes(), added.as_slice());
        let restored_text = session.document().page_text(1).expect("text after undo");
        assert!(restored_text.contains("Round B undo"));

        fn session_error(error: impl std::fmt::Display) -> SessionError {
            SessionError::Io(std::io::Error::other(error.to_string()))
        }
    }

    /// Find the object reference behind a page image resource name.
    fn image_object_id_for(
        document: &Document,
        page_index: usize,
        resource: &str,
    ) -> Option<ObjectId> {
        let effective = effective_document(document).ok()?;
        let page = effective.page(page_index).ok()?;
        let resources = page_resources(&effective, &page).ok()??;
        let resources = resources.as_dictionary()?;
        let xobjects = resolve(&effective, resources.get("XObject")).ok()??;
        let xobjects = xobjects.as_dictionary()?;
        xobjects.get(resource).and_then(Object::as_reference)
    }

    /// Append one raw content stream to a page, preserving its resources.
    fn append_page_content(
        document: &mut Document,
        page_index: usize,
        content: &[u8],
    ) -> Result<(), TextEditError> {
        let effective = effective_document(document)?;
        let page = effective.page(page_index)?;
        let page_object = effective.object(page.id)?;
        let mut dictionary = page_object
            .as_dictionary()
            .cloned()
            .ok_or(TextEditError::UnsupportedPage(page_index))?;
        let stream_id = ObjectId::new(document.allocate_object_number()?, 0);
        document.set_object(
            stream_id,
            Object::Stream {
                dictionary: Dictionary::new(),
                data: content.to_vec(),
            },
        )?;
        let mut contents = page.contents.clone();
        contents.push(stream_id);
        dictionary.insert(
            "Contents".to_owned(),
            Object::Array(contents.into_iter().map(Object::Reference).collect()),
        );
        document.set_object(page.id, Object::Dictionary(dictionary))?;
        Ok(())
    }

    fn test_png(width: u32, height: u32, color: [u8; 3]) -> Vec<u8> {
        let mut raw = Vec::new();
        for _ in 0..height {
            raw.resize(raw.len() + 1, 0);
            for _ in 0..width {
                raw.extend_from_slice(&color);
            }
        }
        let mut zlib = vec![0x78, 0x01];
        let mut offset = 0usize;
        while offset < raw.len() {
            let length = (raw.len() - offset).min(65_535);
            let last = offset + length == raw.len();
            zlib.push(if last { 1 } else { 0 });
            zlib.extend_from_slice(&(length as u16).to_le_bytes());
            zlib.extend_from_slice(&(!(length as u16)).to_le_bytes());
            zlib.extend_from_slice(&raw[offset..offset + length]);
            offset += length;
        }
        zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        push_chunk(&mut png, b"IHDR", &ihdr);
        push_chunk(&mut png, b"IDAT", &zlib);
        push_chunk(&mut png, b"IEND", &[]);
        png
    }

    fn push_chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        png.extend_from_slice(kind);
        png.extend_from_slice(data);
        png.extend_from_slice(&[0, 0, 0, 0]);
    }

    fn adler32(data: &[u8]) -> u32 {
        let mut a = 1u32;
        let mut b = 0u32;
        for byte in data {
            a = (a + u32::from(*byte)) % 65_521;
            b = (b + a) % 65_521;
        }
        (b << 16) | a
    }
}
