//! Page operations, content editing, annotations, watermarks, Bates numbering, and redaction.
//!
//! This crate owns the authoring-side document model. It is renderer-agnostic
//! and depends only on the standard library: [`geometry`] defines validated
//! page-space primitives, [`annotation`] defines the review and drawing
//! annotation model with hit-testing, bounds, and clamping helpers,
//! [`document_annotations`] writes real note, highlight, underline, strikeout
//! and ink annotations into a
//! [`pdfxi_core::Document`] (dictionary, `/Annots` entry and appearance stream)
//! with XFDF import/export, and [`pdf`]
//! maps annotations to and from a dependency-free PDF dictionary intermediate
//! with validation and deterministic appearance geometry, and [`navigation`]
//! models bookmarks, links, and attachments with the same dictionary
//! round-trip approach; `navigation::edit` adds bookmark tree editing by path,
//! link-annotation storage, and attachment add/remove. [`markup`] provides typed
//! options and deterministic appearance geometry for the drawing tools
//! (free text, callout, line, arrow, shape, cloud, ink, stamp, and file
//! attachment), each round-tripping through a private dictionary namespace.
//! [`review`] models comment threads, workflow states, and a filterable review
//! registry with a dictionary round-trip, printable/participant views, and a
//! network-free local shared-review file with deterministic merge and conflict
//! reporting. [`compare`] deterministically diffs two already-extracted
//! page-text documents into bounded added/removed/changed hunks. [`measurement`]
//! computes validated distance/area/perimeter geometry with explicit scale and
//! units. [`paragraph_edit`] models paragraph text edits with deterministic
//! line-break reflow against caller-supplied glyph metrics (a model only: it
//! never reads or writes content streams). [`text_style`] models text run styles
//! (font, size, scaling, spacing, color) with validation and deterministic
//! first-run-wins paragraph resolution. [`find_replace`] searches
//! caller-supplied extracted text with literal, bounded matching and builds a
//! deterministic, non-overlapping, capped replace-all plan, while
//! [`image_edit`] models image placement (fit/fill/stretch), validated
//! replace/remove/move/resize operations, and a deterministic host-writer plan.
//! [`header_footer`]
//! models six-slot headers, footers, and page numbering and renders them as
//! additive content-stream overlays, including whole-page-set application with
//! filename tokens and alternate page selection. [`watermark`] models text,
//! vector, and image watermarks and backgrounds with placement, rotation,
//! opacity, page selection, page-set application, and copy-on-write composition
//! helpers. [`bates`] models Bates numbering as a deterministic, host-consumed
//! plan of ordered stamps (number, text, and validated position) with zero
//! padding, prefix/suffix, per-page increment, a nine-position anchor grid,
//! page-set selection, and caller-time date stamps; it never mutates PDF bytes.
//! [`redaction`] applies a
//! plan of rectangles and text patterns to content streams, removing the
//! affected operators (not merely covering them), stripping annotations and
//! metadata, and verifying removal with a typed report. It also exposes a
//! permanent-removal contract ([`redaction::PermanentRemovalPlan`]) that pairs
//! deduplicated, canonically ordered marks with a caller-supplied
//! [`redaction::ContentRemover`] (the content-stream removal step owned by
//! pdfxi-security) and verifies the result against caller-supplied text.
//! [`flatten()`] replaces
//! annotations and form widgets with permanent content-stream placements,
//! removing them from `/Annots` (and the AcroForm only when every widget is
//! gone) with a typed report that also exposes the appearance bytes emitted as
//! `Do` references, plus a dry-run mode. [`content_edit`] reads a real page's
//! content stream into geometry-aware text blocks and writes text replacement
//! with reflow, inserted text boxes and image XObjects through the core object
//! model; it also lists, moves, resizes, replaces and deletes existing text
//! blocks and image XObjects with fail-closed shared-object checks. The
//! replacement path embeds a bundled SIL Open Font License fallback
//! font when the source font is subset or otherwise cannot encode the new text.
//!
//! Edits in this crate are copy-on-write by contract; no API here mutates
//! source document bytes in place.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod annotation;
pub mod bates;
pub mod compare;
pub mod content_edit;
pub mod document_annotations;
pub mod find_replace;
pub mod flatten;
pub mod geometry;
pub mod header_footer;
pub mod image_edit;
pub mod markup;
pub mod measurement;
pub mod navigation;
pub mod paragraph_edit;
pub mod pdf;
pub mod redact_document;
pub mod redaction;
pub mod review;
pub mod text_style;
pub mod watermark;

mod truetype;

pub use annotation::{
    Annotation, AnnotationError, AnnotationId, AnnotationKind, Color, bounds_union,
};
pub use bates::{
    BatesAnchor, BatesColor, BatesColumn, BatesConfig, BatesDateFormat, BatesDateStamp, BatesError,
    BatesMargins, BatesPageBounds, BatesPageSet, BatesPlan, BatesRow, BatesStamp, BatesStyle,
    MAX_DIGIT_WIDTH, MAX_FONT_SIZE, bates_text_extent, build as build_bates_plan,
};
pub use compare::{
    CompareError, CompareOptions, CompareResult, CompareSummary, Hunk, HunkKind, TextDocument,
    TextPage, compare_documents,
};
pub use content_edit::{
    FALLBACK_BASE_FONT, FontChoice, ImageFormat, ImagePlacement, ObjectEditError, ObjectKind,
    ObjectReplaceOutcome, ObjectReplacement, PageObject, PageTextModel, ReplaceOutcome,
    RunStyleOverride, TextAlign, TextBlock, TextEditError, TextLine, TextPlacement, TextRun,
    add_image, add_text_box, delete_object, list_objects, move_object, replace_object,
    replace_run_text, resize_object, restyle_run_text, text_blocks,
};
pub use document_annotations::{
    ARROW_HEAD_LENGTH, AnnotationEditError, AnnotationRef, AnnotationState, AnnotationSubtype,
    AnnotationView, MARKUP_STROKE_COLOR, MARKUP_STROKE_WIDTH, MAX_ATTACHMENT_BYTES, MAX_INK_PATHS,
    MAX_INK_POINTS, MAX_INK_STROKE_WIDTH, MAX_MARKUP_QUADS, MAX_POLYGON_VERTICES,
    MAX_STAMP_TEXT_CHARS, MIN_INK_STROKE_WIDTH, NOTE_ICON_SIZE, STAMP_COLOR, STAMP_NAMES,
    ShapeStyle, add_callout, add_circle, add_circle_styled, add_cloud, add_file_attachment,
    add_free_text, add_highlight, add_ink, add_line, add_line_styled, add_polygon,
    add_polygon_styled, add_square, add_square_styled, add_squiggly, add_stamp, add_strikeout,
    add_text_note, add_underline, delete_annotation, export_xfdf, import_xfdf, list_annotations,
    reply_to, set_state, update_contents,
};
pub use find_replace::{
    FindReplaceError, ReplaceAllPlan, ReplaceLimits, ReplaceStep, SearchDocument, SearchHit,
    SearchLimits, SearchOptions, SearchPage, build_replace_all_plan, find_all,
};
pub use flatten::{
    Appearance, AppearanceResource, FlattenError, FlattenEvent, FlattenInput, FlattenOptions,
    FlattenReport, FlattenedDocument, FlattenedPage, PageBox, SkipReason, Widget, flatten,
};
pub use geometry::{GeometryError, PageGeometry, PageIndex, Point, Rect};
pub use header_footer::{
    ContentMatrix, HeaderFooterError, HeaderFooterLayout, Margins, NumberFormat, Overlay,
    PageContext, PageFilter, PageNumbering, PageSelection, PageSetContext, Slot, SlotContent,
    StandardFont, TextColor, TextStyle, Token, compose_content, compose_page_set,
    display_dimensions, escape_literal, render_overlay, render_page_set, transform_for_page,
};
pub use image_edit::{
    ImageAction, ImageEdit, ImageEditError, ImageEditPlan, ImageEditStep, ImageId, ImageOperation,
    PageBounds, PlacedImage, PlacementMode, place_image,
};
pub use markup::{
    ArrowHead, ArrowOptions, AttachmentIcon, CalloutOptions, CloudOptions, DrawingKind,
    DrawingOptions, FileAttachmentOptions, FreeTextOptions, InkOptions, LineOptions,
    MAX_ATTACHMENT_NAME_BYTES, MAX_CLOUD_BUMPS, MarkupAppearance, MarkupError, ShapeKind,
    ShapeOptions, StampCatalogEntry, StampName, StampOptions, StampPreset, TextAlignment,
    stamp_catalog, validate_attachment_reference,
};
pub use measurement::{
    MeasureScale, MeasureUnit, Measurement, MeasurementAppearance, MeasurementError,
    MeasurementKind,
};
pub use navigation::{
    Attachment, Bookmark, BookmarkPath, Destination, FitType, Link, LinkBorder, LinkHighlight,
    LinkStore, LinkTarget, MAX_NAME_BYTES, NameTree, NavigationError, NavigationWriteError,
    Outline, StagedAttachment, StagedLink, add_attachment, add_link, dict_to_link,
    dict_to_name_tree, dict_to_outline, link_rect, link_to_dict, name_tree_to_dict,
    outline_to_dict, page_destination, read_outline, remove_attachment, remove_link, write_outline,
};
pub use paragraph_edit::{
    EditError, EditLimits, EditPlan, EditStep, FixedMetrics, FnMetrics, GlyphMetrics,
    ParagraphEdit, ReflowOptions, ReflowedLine, Replacement, TextSpan, reflow_paragraph,
};
pub use redact_document::{
    RedactionError as DocumentRedactionError, RedactionMark,
    RedactionReport as DocumentRedactionReport, RedactionRequest, RedactionVerification,
    apply_redaction, verify_redaction,
};
pub use redaction::{
    AppliedPage, AppliedRedaction, ContentRemover, FormXObject, Matrix, OverlapPolicy, PageNumber,
    PermanentRemovalPlan, PermanentRemovalResult, RedactionError, RedactionInput,
    RedactionMetadata, RedactionOptions, RedactionPlan, RedactionRect, RedactionRegion,
    RedactionReport, Removal, RemovalKind, Residual, ResidualSource, StreamContentRemover,
    TextPattern, apply, redact, scan_text, verify_extracted_text,
};
pub use review::{
    CommentThread, HistoryEntry, ParticipantStatusSummary, ParticipantSummary, PrintLine,
    REVIEW_REGISTRY_VERSION, Reply, ReplyId, ReviewError, ReviewFilter, ReviewRegistry,
    ReviewState, ReviewStatus, ReviewSummary, ReviewTracker, ThreadId, dict_to_registry,
    export_registry, import_registry, import_registry_checked, participant_status_summary,
    participants, print_registry, registry_to_dict, registry_to_print_lines, registry_to_text,
    registry_to_typed_print_lines, root_annotation_ids, typed_print_lines_to_text,
};
pub use text_style::{
    ColorComponent, FillColor, ParagraphStylePlan, Run, RunStyle, SpacingField, StyleError,
    StyleLimits, StyleOverride, StyledRun, apply_style, resolve_paragraph_style, style_paragraph,
    validate_style,
};
pub use watermark::{
    ImageMark, MarkContent, PageRange, Placement, ResourceExtGState, ResourceFont, ResourcePatch,
    Suppression, VectorMark, Watermark, WatermarkError, WatermarkOverlay, WatermarkPosition,
    WatermarkStyle, apply_resource_patch, compose_content as compose_watermark_content,
    render_watermark, render_watermark_page_set,
};

/// Return the implementation name for diagnostics.
pub fn engine_name() -> &'static str {
    "pdfxi-edit"
}
