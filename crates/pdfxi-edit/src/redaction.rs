//! Content-stream redaction with post-removal verification.
//!
//! This module applies a [`RedactionPlan`] to PDF page content streams and
//! proves that the targeted content is **gone**, not merely covered. Unlike a
//! drawing-only overlay, the text-showing operators whose text matches a
//! [`TextPattern`], or whose laid-out position falls inside a [`RedactionRect`],
//! are removed from the stream bytes. An opaque rectangle is then painted so
//! the area is visually blank as well.
//!
//! # Scope
//!
//! - Operands and operators are tokenized into operations that retain their
//!   exact source bytes, so unaffected content is preserved verbatim.
//! - Text is decoded from `Tj`, `TJ`, `'`, and `"` operators (literal strings,
//!   hex strings, and `TJ` number adjustments). Text clipping render modes do
//!   not hide text from removal: every text-showing operator is a candidate.
//! - `Do` references to [`FormXObject`]s are recursed into with a bounded depth
//!   and a cycle check; inline images (`BI … ID … EI`) are opaque and preserved.
//! - Page dictionaries are stripped of annotations and links whose rectangle
//!   intersects a redaction rectangle or whose text/URI matches a pattern, and
//!   of metadata references when text redaction is requested.
//!
//! # Verification
//!
//! After applying, the module re-extracts text from the produced streams using
//! a simple content scan and records any [`Residual`] in a typed
//! [`RedactionReport`]. Callers can supply their own extractor with
//! [`AppliedRedaction::verify_with`]; [`RedactionReport::check`] enforces the
//! strict policy.
//!
//! # No hidden state
//!
//! Edits are copy-on-write: input bytes and dictionaries are borrowed and the
//! redacted copies are returned in [`AppliedRedaction`]. There is no unsafe
//! code, no external dependency, and every long loop checks the cancellation
//! closure.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashSet;
use std::fmt;
use std::num::NonZeroU32;

use crate::geometry::Rect;
use crate::pdf::{Dict, Value};

/// A failure while applying or verifying a redaction plan.
#[derive(Debug, Clone, PartialEq)]
pub enum RedactionError {
    /// The caller's cancellation closure returned `true`.
    Cancelled,
    /// A page number was zero (page numbers are 1-based).
    InvalidPageNumber,
    /// A text pattern's needle was empty or whitespace-only.
    EmptyPattern,
    /// A coordinate was `NaN` or infinite.
    NonFiniteCoordinate,
    /// A rectangle had non-positive width or height.
    EmptyRegion,
    /// A rectangle referenced a page not present in the input.
    UnknownPage {
        /// The offending 1-based page number.
        page: u32,
    },
    /// A rectangle exceeded the declared page bounds.
    RegionOutOfBounds {
        /// The offending 1-based page number.
        page: u32,
    },
    /// Form XObject recursion exceeded the configured depth limit.
    FormDepthExceeded {
        /// Resource name of the form.
        name: String,
        /// Maximum configured depth.
        max_depth: usize,
    },
    /// A Form XObject referenced itself through `/Do`.
    FormCycle {
        /// Resource name of the form that closed the cycle.
        name: String,
    },
    /// A content stream was structurally malformed.
    MalformedContent {
        /// Short, stable description of the defect.
        detail: &'static str,
    },
    /// Strict verification found redacted content still present.
    VerificationFailed {
        /// Page whose extracted text still contained the content.
        page: u32,
        /// Byte offset of the first residual.
        offset: usize,
    },
}

impl fmt::Display for RedactionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => write!(f, "redaction was cancelled"),
            Self::InvalidPageNumber => {
                write!(f, "redaction page numbers are 1-based and cannot be zero")
            }
            Self::EmptyPattern => write!(f, "redaction text pattern must not be empty"),
            Self::NonFiniteCoordinate => {
                write!(f, "redaction rectangle has a non-finite coordinate")
            }
            Self::EmptyRegion => write!(f, "redaction rectangle must have positive area"),
            Self::UnknownPage { page } => {
                write!(f, "redaction region references unknown page {page}")
            }
            Self::RegionOutOfBounds { page } => {
                write!(
                    f,
                    "redaction rectangle on page {page} exceeds the page bounds"
                )
            }
            Self::FormDepthExceeded { name, max_depth } => write!(
                f,
                "form XObject /{name} exceeded the maximum recursion depth of {max_depth}"
            ),
            Self::FormCycle { name } => {
                write!(f, "form XObject /{name} references itself through /Do")
            }
            Self::MalformedContent { detail } => write!(f, "malformed content stream: {detail}"),
            Self::VerificationFailed { page, offset } => write!(
                f,
                "redaction verification failed: content remains on page {page} at byte {offset}"
            ),
        }
    }
}

impl std::error::Error for RedactionError {}

/// A 2D affine transform in PDF's `[a b c d e f]` form.
///
/// The transform maps `(x, y)` to `(a·x + c·y + e, b·x + d·y + f)`. It is used
/// to track the current transformation matrix from `cm` operators and form
/// matrices so text positions can be tested against page-space rectangles.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Matrix {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
}

impl Matrix {
    /// The identity transform.
    pub const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    /// Build a transform from its six PDF coefficients.
    ///
    /// No validation is required: the coefficients are consumed as-is and a
    /// non-finite coefficient simply makes transformed positions fail the
    /// rectangle containment test (never a panic).
    pub const fn new(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Self {
        Self { a, b, c, d, e, f }
    }

    /// Build a pure translation by `(tx, ty)`.
    pub const fn translate(tx: f64, ty: f64) -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: tx,
            f: ty,
        }
    }

    /// Concatenate `self` with `other` (`self × other`, PDF `cm` order).
    pub const fn mul(self, other: Self) -> Self {
        Self {
            a: self.a * other.a + self.c * other.b,
            b: self.b * other.a + self.d * other.b,
            c: self.a * other.c + self.c * other.d,
            d: self.b * other.c + self.d * other.d,
            e: self.a * other.e + self.c * other.f + self.e,
            f: self.b * other.e + self.d * other.f + self.f,
        }
    }

    /// Apply this transform to a point.
    pub const fn transform_point(self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }
}

/// A 1-based PDF page number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PageNumber(NonZeroU32);

impl PageNumber {
    /// Create a page number, rejecting zero.
    ///
    /// # Errors
    ///
    /// Returns [`RedactionError::InvalidPageNumber`] when `page` is zero.
    pub fn new(page: u32) -> Result<Self, RedactionError> {
        NonZeroU32::new(page)
            .map(Self)
            .ok_or(RedactionError::InvalidPageNumber)
    }

    /// Return the 1-based page value.
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

impl fmt::Display for PageNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// An axis-aligned rectangle in PDF user space.
///
/// Coordinates are `(x0, y0, x1, y1)` with `x1 > x0` and `y1 > y0`. The type is
/// a total order so plans are deterministic regardless of insertion order.
#[derive(Debug, Clone, Copy)]
pub struct RedactionRect {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl RedactionRect {
    /// Create a rectangle, validating finiteness and positive area.
    ///
    /// # Errors
    ///
    /// Returns [`RedactionError::NonFiniteCoordinate`] for `NaN`/infinite input
    /// or [`RedactionError::EmptyRegion`] for a non-positive extent.
    pub fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Result<Self, RedactionError> {
        if ![x0, y0, x1, y1].iter().all(|v| v.is_finite()) {
            return Err(RedactionError::NonFiniteCoordinate);
        }
        if x1 <= x0 || y1 <= y0 {
            return Err(RedactionError::EmptyRegion);
        }
        Ok(Self { x0, y0, x1, y1 })
    }

    /// Left edge.
    pub const fn x0(&self) -> f64 {
        self.x0
    }

    /// Bottom edge.
    pub const fn y0(&self) -> f64 {
        self.y0
    }

    /// Right edge.
    pub const fn x1(&self) -> f64 {
        self.x1
    }

    /// Top edge.
    pub const fn y1(&self) -> f64 {
        self.y1
    }

    /// Width, always strictly positive.
    pub const fn width(&self) -> f64 {
        self.x1 - self.x0
    }

    /// Height, always strictly positive.
    pub const fn height(&self) -> f64 {
        self.y1 - self.y0
    }

    /// Whether `(x, y)` lies inside the rectangle, edges inclusive.
    pub const fn contains_point(&self, x: f64, y: f64) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }

    /// Whether the horizontal span `[left, right]` at height `y` overlaps.
    ///
    /// Used to decide whether a text-showing operator falls in this rectangle.
    pub const fn overlaps_text(&self, left: f64, right: f64, y: f64) -> bool {
        self.x0 < right && left < self.x1 && y >= self.y0 && y <= self.y1
    }
}

impl PartialEq for RedactionRect {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == std::cmp::Ordering::Equal
    }
}

impl Eq for RedactionRect {}

impl PartialOrd for RedactionRect {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RedactionRect {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.x0
            .total_cmp(&other.x0)
            .then_with(|| self.y0.total_cmp(&other.y0))
            .then_with(|| self.x1.total_cmp(&other.x1))
            .then_with(|| self.y1.total_cmp(&other.y1))
    }
}

/// A literal text pattern with an explicit case-sensitivity policy.
///
/// Matching is literal (no regular expressions), so patterns cannot introduce
/// catastrophic backtracking when applied to untrusted extracted text.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextPattern {
    needle: String,
    case_sensitive: bool,
}

impl TextPattern {
    /// Create a pattern, rejecting an empty or whitespace-only needle.
    ///
    /// # Errors
    ///
    /// Returns [`RedactionError::EmptyPattern`] when `needle` trims to empty.
    pub fn new(needle: impl Into<String>, case_sensitive: bool) -> Result<Self, RedactionError> {
        let needle = needle.into();
        if needle.trim().is_empty() {
            return Err(RedactionError::EmptyPattern);
        }
        Ok(Self {
            needle,
            case_sensitive,
        })
    }

    /// Create a case-sensitive pattern.
    ///
    /// # Errors
    ///
    /// See [`Self::new`].
    pub fn case_sensitive(needle: impl Into<String>) -> Result<Self, RedactionError> {
        Self::new(needle, true)
    }

    /// Create a case-insensitive pattern.
    ///
    /// # Errors
    ///
    /// See [`Self::new`].
    pub fn case_insensitive(needle: impl Into<String>) -> Result<Self, RedactionError> {
        Self::new(needle, false)
    }

    /// The literal string to match.
    pub fn literal(&self) -> &str {
        &self.needle
    }

    /// Whether matching distinguishes upper and lower case.
    pub const fn is_case_sensitive(&self) -> bool {
        self.case_sensitive
    }

    /// Whether this pattern occurs in `text`.
    pub fn matches(&self, text: &str) -> bool {
        if self.case_sensitive {
            text.contains(&self.needle)
        } else {
            !find_occurrences(text, &self.needle, false).is_empty()
        }
    }
}

/// A page-scoped redaction region.
///
/// Ordering is by page, then kind (rectangles before text), then contents, so a
/// [`RedactionPlan`] is canonical regardless of insertion order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum RedactionRegion {
    /// A rectangular region, optionally labelled with the text it removed.
    Rect {
        /// 1-based page containing the rectangle.
        page: PageNumber,
        /// Validated rectangle in user space.
        rect: RedactionRect,
        /// Text known to have been inside the rectangle, when available.
        removed_text: Option<String>,
    },
    /// Every literal occurrence of a text pattern on a page.
    Text {
        /// 1-based page containing the text.
        page: PageNumber,
        /// Validated literal pattern to remove.
        pattern: TextPattern,
    },
}

impl RedactionRegion {
    /// Build a rectangle region with no removed-text label.
    pub fn rect(page: PageNumber, rect: RedactionRect) -> Self {
        Self::Rect {
            page,
            rect,
            removed_text: None,
        }
    }

    /// Build a rectangle region labelled with the text it removed.
    ///
    /// The label is checked during verification the same way as a text pattern.
    pub fn rect_with_text(
        page: PageNumber,
        rect: RedactionRect,
        removed_text: impl Into<String>,
    ) -> Self {
        Self::Rect {
            page,
            rect,
            removed_text: Some(removed_text.into()),
        }
    }

    /// Build a text-pattern region.
    pub fn text(page: PageNumber, pattern: TextPattern) -> Self {
        Self::Text { page, pattern }
    }

    /// The 1-based page this region applies to.
    pub const fn page(&self) -> PageNumber {
        match self {
            Self::Rect { page, .. } | Self::Text { page, .. } => *page,
        }
    }

    /// The rectangle of a rectangle region, if this is one.
    pub const fn as_rect(&self) -> Option<&RedactionRect> {
        match self {
            Self::Rect { rect, .. } => Some(rect),
            Self::Text { .. } => None,
        }
    }

    /// The pattern of a text region, if this is one.
    pub const fn as_pattern(&self) -> Option<&TextPattern> {
        match self {
            Self::Text { pattern, .. } => Some(pattern),
            Self::Rect { .. } => None,
        }
    }

    /// The removed-text label of a rectangle region, if present.
    pub fn removed_text(&self) -> Option<&str> {
        match self {
            Self::Rect { removed_text, .. } => removed_text.as_deref(),
            Self::Text { .. } => None,
        }
    }
}

/// A canonical, validated plan describing what to redact.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RedactionPlan {
    regions: Vec<RedactionRegion>,
}

impl RedactionPlan {
    /// Build a plan from regions, sorting them into canonical order.
    pub fn new(regions: impl IntoIterator<Item = RedactionRegion>) -> Self {
        let mut regions: Vec<RedactionRegion> = regions.into_iter().collect();
        regions.sort();
        Self { regions }
    }

    /// Build an empty plan.
    pub const fn empty() -> Self {
        Self {
            regions: Vec::new(),
        }
    }

    /// Add a region and restore canonical ordering.
    pub fn add(&mut self, region: RedactionRegion) {
        self.regions.push(region);
        self.regions.sort();
    }

    /// Regions in canonical `(page, kind, contents)` order.
    pub fn regions(&self) -> &[RedactionRegion] {
        &self.regions
    }

    /// Whether the plan has no regions.
    pub const fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    /// Number of regions in the plan.
    pub fn len(&self) -> usize {
        self.regions.len()
    }

    /// Validate the plan against an input's pages and optional page bounds.
    ///
    /// A rectangle must reference a page present in `input.pages` and, when a
    /// bounds entry exists for that page, lie inside it.
    ///
    /// # Errors
    ///
    /// See [`RedactionError::UnknownPage`] and [`RedactionError::RegionOutOfBounds`].
    pub fn validate(&self, input: &RedactionInput) -> Result<(), RedactionError> {
        for region in &self.regions {
            let page = region.page().get();
            if let Some(rect) = region.as_rect() {
                if !input.pages.contains_key(&page) {
                    return Err(RedactionError::UnknownPage { page });
                }
                if let Some(bounds) = input.page_bounds.get(&page) {
                    let inside = rect.x0() >= bounds.x0() as f64
                        && rect.y0() >= bounds.y0() as f64
                        && rect.x1() <= bounds.x1() as f64
                        && rect.y1() <= bounds.y1() as f64;
                    if !inside {
                        return Err(RedactionError::RegionOutOfBounds { page });
                    }
                }
            }
        }
        Ok(())
    }
}

/// A form XObject's content stream and its placement matrix.
#[derive(Debug, Clone, PartialEq)]
pub struct FormXObject {
    /// Decoded content-stream bytes of the form.
    pub content: Vec<u8>,
    /// The form's `/Matrix` (identity when omitted).
    pub matrix: Matrix,
}

impl FormXObject {
    /// Build a form XObject from content bytes and a matrix.
    pub fn new(content: Vec<u8>, matrix: Matrix) -> Self {
        Self { content, matrix }
    }
}

/// Options controlling redaction application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactionOptions {
    /// Maximum recursion depth when following `/Do` into form XObjects.
    pub max_form_depth: usize,
    /// Whether rectangles should be painted with an opaque box.
    pub paint_boxes: bool,
    /// Whether verification failures make [`AppliedRedaction::check`] fail.
    pub strict_verification: bool,
}

impl Default for RedactionOptions {
    fn default() -> Self {
        Self {
            max_form_depth: 8,
            paint_boxes: true,
            strict_verification: true,
        }
    }
}

/// Input to [`apply`]: the pages, their dictionaries, referenced forms, and
/// optional per-page media bounds used for rectangle validation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RedactionInput {
    /// Map of 1-based page number to content-stream bytes.
    pub pages: BTreeMap<u32, Vec<u8>>,
    /// Map of 1-based page number to the page dictionary to strip.
    pub dictionaries: BTreeMap<u32, Dict>,
    /// Form XObjects keyed by the resource name used by `/Do`.
    pub forms: BTreeMap<String, FormXObject>,
    /// Optional page media bounds keyed by 1-based page number.
    pub page_bounds: BTreeMap<u32, Rect>,
}

/// The redacted content and dictionary of one page.
#[derive(Debug, Clone, PartialEq)]
pub struct AppliedPage {
    /// The redacted content-stream bytes.
    pub content: Vec<u8>,
    /// The stripped page dictionary.
    pub dictionary: Dict,
}

/// What was removed or painted during redaction application.
#[derive(Debug, Clone, PartialEq)]
pub enum RemovalKind {
    /// A text-showing operator was removed.
    TextOperator {
        /// PDF operator name (`Tj`, `TJ`, `'`, or `"`).
        operator: String,
        /// Number of decoded characters the operator would have shown.
        char_count: usize,
    },
    /// An opaque rectangle was painted.
    PaintedBox {
        /// Left edge in page space.
        x0: f64,
        /// Bottom edge in page space.
        y0: f64,
        /// Right edge in page space.
        x1: f64,
        /// Top edge in page space.
        y1: f64,
    },
    /// An annotation or link was removed from the page dictionary.
    AnnotationRemoved {
        /// The annotation's `/Subtype`, or `Unknown` when absent.
        subtype: String,
    },
    /// A metadata reference was removed from the page dictionary.
    MetadataKeyRemoved {
        /// The dictionary key that was removed.
        key: String,
    },
}

/// A single recorded removal.
#[derive(Debug, Clone, PartialEq)]
pub struct Removal {
    /// 1-based page containing the removal.
    pub page: u32,
    /// Resource name of the form when the removal happened inside one.
    pub form: Option<String>,
    /// What was removed.
    pub kind: RemovalKind,
}

/// Which plan region produced a residual after verification.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ResidualSource {
    /// A text-pattern region.
    Pattern(String),
    /// The removed-text label of a rectangle region.
    RectangleLabel(String),
}

/// One residual occurrence of redacted content in extracted text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Residual {
    /// Page whose text contained the residual.
    pub page: u32,
    /// Byte offset into that page's extracted text.
    pub offset: usize,
    /// Which plan region produced it.
    pub source: ResidualSource,
}

/// Typed report of removals and residuals produced by redaction.
#[derive(Debug, Clone, PartialEq)]
pub struct RedactionReport {
    removals: Vec<Removal>,
    residuals: Vec<Residual>,
    strict: bool,
}

impl RedactionReport {
    /// Recorded removals in application order.
    pub fn removals(&self) -> &[Removal] {
        &self.removals
    }

    /// Residual occurrences in canonical `(page, offset, source)` order.
    pub fn residuals(&self) -> &[Residual] {
        &self.residuals
    }

    /// Whether no residual content was found.
    pub const fn is_clean(&self) -> bool {
        self.residuals.is_empty()
    }

    /// Whether strict verification is enforced by [`Self::check`].
    pub const fn strict(&self) -> bool {
        self.strict
    }

    /// Number of removed text-showing operators.
    pub fn removed_operator_count(&self) -> usize {
        self.removals
            .iter()
            .filter(|r| matches!(r.kind, RemovalKind::TextOperator { .. }))
            .count()
    }

    /// Distinct pages that had at least one removal or residual, in order.
    pub fn pages_touched(&self) -> Vec<u32> {
        let mut pages = BTreeSet::new();
        for removal in &self.removals {
            pages.insert(removal.page);
        }
        for residual in &self.residuals {
            pages.insert(residual.page);
        }
        pages.into_iter().collect()
    }

    /// Enforce the verification policy.
    ///
    /// # Errors
    ///
    /// Returns [`RedactionError::VerificationFailed`] in strict mode when at
    /// least one residual remains. Permissive mode always succeeds.
    pub fn check(&self) -> Result<(), RedactionError> {
        if self.strict
            && let Some(first) = self.residuals.first()
        {
            return Err(RedactionError::VerificationFailed {
                page: first.page,
                offset: first.offset,
            });
        }
        Ok(())
    }
}

/// Result of applying a redaction plan.
#[derive(Debug, Clone, PartialEq)]
pub struct AppliedRedaction {
    pages: BTreeMap<u32, AppliedPage>,
    forms: BTreeMap<String, FormXObject>,
    report: RedactionReport,
}

impl AppliedRedaction {
    /// The redacted content of a page, if the page was in the input.
    pub fn page_content(&self, page: u32) -> Option<&[u8]> {
        self.pages.get(&page).map(|p| p.content.as_slice())
    }

    /// The stripped dictionary of a page.
    pub fn page_dictionary(&self, page: u32) -> Option<&Dict> {
        self.pages.get(&page).map(|p| &p.dictionary)
    }

    /// All redacted pages, keyed by 1-based page number.
    pub fn pages(&self) -> &BTreeMap<u32, AppliedPage> {
        &self.pages
    }

    /// All redacted form XObjects, keyed by resource name.
    pub fn forms(&self) -> &BTreeMap<String, FormXObject> {
        &self.forms
    }

    /// The typed removal/verification report.
    pub fn report(&self) -> &RedactionReport {
        &self.report
    }

    /// Enforce strict verification.
    ///
    /// # Errors
    ///
    /// See [`RedactionReport::check`].
    pub fn check(&self) -> Result<(), RedactionError> {
        self.report.check()
    }

    /// Re-verify with a caller-supplied extractor.
    ///
    /// `extractor` is called once per redacted page and must return the text as
    /// it would be seen by the user (for example from a full text extractor
    /// that also follows form XObjects). Residuals in the report are replaced.
    /// Call [`Self::check`] afterwards to enforce strict mode.
    pub fn verify_with<F>(
        &mut self,
        plan: &RedactionPlan,
        extractor: F,
        strict: bool,
    ) -> Result<&RedactionReport, RedactionError>
    where
        F: Fn(u32, &[u8]) -> String,
    {
        let mut page_text = BTreeMap::new();
        for (page, applied) in &self.pages {
            page_text.insert(*page, extractor(*page, &applied.content));
        }
        let residuals = residuals_for_plan(plan, &page_text)?;
        self.report.residuals = residuals;
        self.report.strict = strict;
        Ok(&self.report)
    }

    /// Re-verify with the built-in simple content scan.
    ///
    /// Call [`Self::check`] afterwards to enforce strict mode.
    pub fn verify_scan(
        &mut self,
        plan: &RedactionPlan,
        strict: bool,
    ) -> Result<&RedactionReport, RedactionError> {
        self.verify_with(
            plan,
            |_page, content| scan_text(content).unwrap_or_default(),
            strict,
        )
    }

    /// Consume the result and return its report.
    pub fn into_report(self) -> RedactionReport {
        self.report
    }
}

/// Apply `plan` to every page in `input` and verify with the simple scan.
///
/// The returned [`AppliedRedaction`] holds copy-on-write redacted bytes and
/// stripped dictionaries; `input` is never mutated. Call
/// [`AppliedRedaction::check`] to enforce strict verification.
///
/// # Errors
///
/// Propagates plan validation, malformed content, form recursion, cancellation,
/// and verification errors as [`RedactionError`].
pub fn apply(
    plan: &RedactionPlan,
    input: &RedactionInput,
    options: &RedactionOptions,
    cancel: &impl Fn() -> bool,
) -> Result<AppliedRedaction, RedactionError> {
    plan.validate(input)?;
    let mut removals = Vec::new();
    let mut redacted_forms = BTreeMap::new();
    let mut pages = BTreeMap::new();

    for (&page, content) in &input.pages {
        if cancel() {
            return Err(RedactionError::Cancelled);
        }
        let mut visiting = Vec::new();
        let redacted = redact_stream(
            plan,
            page,
            content,
            &input.forms,
            Matrix::IDENTITY,
            0,
            None,
            options,
            cancel,
            &mut removals,
            &mut redacted_forms,
            &mut visiting,
        )?;
        let dictionary = input
            .dictionaries
            .get(&page)
            .map(|dict| strip_dictionary(dict, page, plan, &mut removals))
            .unwrap_or_default();
        pages.insert(
            page,
            AppliedPage {
                content: redacted,
                dictionary,
            },
        );
    }

    let mut page_text = BTreeMap::new();
    for (&page, applied) in &pages {
        page_text.insert(page, scan_text(&applied.content)?);
    }
    let residuals = residuals_for_plan(plan, &page_text)?;
    Ok(AppliedRedaction {
        pages,
        forms: redacted_forms,
        report: RedactionReport {
            removals,
            residuals,
            strict: options.strict_verification,
        },
    })
}

/// Apply a plan to a single page and enforce strict verification.
///
/// This is a convenience wrapper around [`apply`] for callers that do not need
/// custom dictionaries, bounds, forms, or extractors.
///
/// # Errors
///
/// See [`apply`] and [`AppliedRedaction::check`].
pub fn redact(
    plan: &RedactionPlan,
    page: u32,
    content: &[u8],
    options: &RedactionOptions,
    cancel: &impl Fn() -> bool,
) -> Result<AppliedRedaction, RedactionError> {
    let mut input = RedactionInput::default();
    input.pages.insert(page, content.to_vec());
    let applied = apply(plan, &input, options, cancel)?;
    applied.check()?;
    Ok(applied)
}

/// Extract the text shown by text-showing operators using a simple scan.
///
/// Text from `Tj`, `TJ`, `'`, and `"` is concatenated with single spaces
/// between operations. This is intentionally dependency-free: callers that need
/// glyph-accurate extraction can use [`AppliedRedaction::verify_with`] instead.
///
/// # Errors
///
/// Returns [`RedactionError::MalformedContent`] when the stream cannot be
/// tokenized.
pub fn scan_text(content: &[u8]) -> Result<String, RedactionError> {
    let operations = parse_operations(content)?;
    let mut out = String::new();
    for op in &operations {
        if let Some(text) = shown_text(op) {
            if !out.is_empty() && !text.is_empty() {
                out.push(' ');
            }
            for byte in text {
                out.push(char::from(byte));
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Content-stream tokenization
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Object {
    Number(f64),
    String(Vec<u8>),
    Name(String),
    Array(Vec<Object>),
    Other(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq)]
enum TokenKind {
    Object(Object),
    Keyword(String),
}

#[derive(Debug, Clone)]
struct Token {
    start: usize,
    end: usize,
    kind: TokenKind,
}

#[derive(Debug, Clone)]
struct Operation {
    operands: Vec<Object>,
    operator: String,
    raw: Vec<u8>,
    inline_image: bool,
}

struct Parser<'a> {
    data: &'a [u8],
    pos: usize,
}

fn is_whitespace(byte: u8) -> bool {
    matches!(byte, 0 | 9 | 10 | 12 | 13 | 32)
}

fn is_delimiter(byte: u8) -> bool {
    is_whitespace(byte)
        || matches!(
            byte,
            b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
        )
}

impl<'a> Parser<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn skip_space(&mut self) -> Vec<u8> {
        let start = self.pos;
        while self.pos < self.data.len() {
            let byte = self.data[self.pos];
            if is_whitespace(byte) {
                self.pos += 1;
            } else if byte == b'%' {
                while self.pos < self.data.len() && !matches!(self.data[self.pos], b'\r' | b'\n') {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
        self.data[start..self.pos].to_vec()
    }

    fn next_token(&mut self) -> Result<(Vec<u8>, Option<Token>), RedactionError> {
        let gap = self.skip_space();
        if self.pos >= self.data.len() {
            return Ok((gap, None));
        }
        let start = self.pos;
        let byte = self.data[self.pos];
        let kind = match byte {
            b'(' => TokenKind::Object(self.read_literal()?),
            b'<' if self.data.get(self.pos + 1) == Some(&b'<') => {
                TokenKind::Object(self.read_dict_raw()?)
            }
            b'<' => TokenKind::Object(self.read_hex()?),
            b'[' => TokenKind::Object(self.read_array()?),
            b'/' => TokenKind::Object(self.read_name()),
            b'>' | b']' | b')' | b'{' | b'}' => {
                return Err(RedactionError::MalformedContent {
                    detail: "unexpected closing delimiter",
                });
            }
            _ => {
                let mut end = self.pos;
                while end < self.data.len() && !is_delimiter(self.data[end]) {
                    end += 1;
                }
                let raw = &self.data[start..end];
                self.pos = end;
                if matches!(raw, b"true" | b"false" | b"null") {
                    TokenKind::Object(Object::Other(raw.to_vec()))
                } else if let Some(number) = std::str::from_utf8(raw)
                    .ok()
                    .and_then(|text| text.parse::<f64>().ok())
                {
                    TokenKind::Object(Object::Number(number))
                } else {
                    let keyword = String::from_utf8_lossy(raw).into_owned();
                    TokenKind::Keyword(keyword)
                }
            }
        };
        Ok((
            gap,
            Some(Token {
                start,
                end: self.pos,
                kind,
            }),
        ))
    }

    fn read_literal(&mut self) -> Result<Object, RedactionError> {
        // `self.pos` points at the opening '('.
        let mut out = Vec::new();
        let mut depth = 1usize;
        self.pos += 1;
        while self.pos < self.data.len() {
            let byte = self.data[self.pos];
            self.pos += 1;
            match byte {
                b'\\' => {
                    if self.pos >= self.data.len() {
                        break;
                    }
                    let escaped = self.data[self.pos];
                    self.pos += 1;
                    match escaped {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0c),
                        b'\r' => {
                            if self.data.get(self.pos) == Some(&b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {}
                        b'0'..=b'7' => {
                            let mut value = (escaped - b'0') as u32;
                            let mut digits = 1;
                            while digits < 3
                                && self.pos < self.data.len()
                                && matches!(self.data[self.pos], b'0'..=b'7')
                            {
                                value = value * 8 + (self.data[self.pos] - b'0') as u32;
                                self.pos += 1;
                                digits += 1;
                            }
                            out.push((value & 0xff) as u8);
                        }
                        other => out.push(other),
                    }
                }
                b'(' => {
                    depth += 1;
                    out.push(byte);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(Object::String(out));
                    }
                    out.push(byte);
                }
                other => out.push(other),
            }
        }
        Err(RedactionError::MalformedContent {
            detail: "unterminated literal string",
        })
    }

    fn read_hex(&mut self) -> Result<Object, RedactionError> {
        let mut nibbles = Vec::new();
        self.pos += 1; // consume '<'
        while self.pos < self.data.len() && self.data[self.pos] != b'>' {
            let byte = self.data[self.pos];
            self.pos += 1;
            if let Some(value) = hex_value(byte) {
                nibbles.push(value);
            } else if !is_whitespace(byte) {
                return Err(RedactionError::MalformedContent {
                    detail: "invalid hex string digit",
                });
            }
        }
        if self.data.get(self.pos) != Some(&b'>') {
            return Err(RedactionError::MalformedContent {
                detail: "unterminated hex string",
            });
        }
        self.pos += 1;
        let mut out = Vec::with_capacity(nibbles.len() / 2 + 1);
        let (pairs, remainder) = nibbles.as_chunks::<2>();
        for pair in pairs {
            out.push((pair[0] << 4) | pair[1]);
        }
        if let [odd] = remainder {
            out.push(odd << 4);
        }
        Ok(Object::String(out))
    }

    fn read_name(&mut self) -> Object {
        self.pos += 1; // consume '/'
        let start = self.pos;
        let mut decoded = Vec::new();
        while self.pos < self.data.len() && !is_delimiter(self.data[self.pos]) {
            if self.data[self.pos] == b'#'
                && self.pos + 2 < self.data.len()
                && let (Some(hi), Some(lo)) = (
                    hex_value(self.data[self.pos + 1]),
                    hex_value(self.data[self.pos + 2]),
                )
            {
                decoded.push((hi << 4) | lo);
                self.pos += 3;
            } else {
                decoded.push(self.data[self.pos]);
                self.pos += 1;
            }
        }
        if decoded.is_empty() && self.pos > start {
            // Should be unreachable, but keep the name object well-formed.
            decoded.extend_from_slice(&self.data[start..self.pos]);
        }
        Object::Name(String::from_utf8_lossy(&decoded).into_owned())
    }

    fn read_array(&mut self) -> Result<Object, RedactionError> {
        self.pos += 1; // consume '['
        let mut items = Vec::new();
        loop {
            let (_, token) = self.next_token()?;
            match token {
                None => {
                    return Err(RedactionError::MalformedContent {
                        detail: "unterminated array",
                    });
                }
                Some(Token { kind, .. }) => match kind {
                    TokenKind::Object(object) => items.push(object),
                    TokenKind::Keyword(keyword) => items.push(Object::Other(keyword.into_bytes())),
                },
            }
            if self.data.get(self.pos) == Some(&b']') {
                self.pos += 1;
                return Ok(Object::Array(items));
            }
        }
    }

    fn read_dict_raw(&mut self) -> Result<Object, RedactionError> {
        let start = self.pos;
        self.pos += 2; // consume '<<'
        let mut depth = 1usize;
        while self.pos < self.data.len() {
            if self.data[self.pos] == b'<' && self.data.get(self.pos + 1) == Some(&b'<') {
                depth += 1;
                self.pos += 2;
            } else if self.data[self.pos] == b'>' && self.data.get(self.pos + 1) == Some(&b'>') {
                depth -= 1;
                self.pos += 2;
                if depth == 0 {
                    return Ok(Object::Other(self.data[start..self.pos].to_vec()));
                }
            } else {
                self.pos += 1;
            }
        }
        Err(RedactionError::MalformedContent {
            detail: "unterminated dictionary",
        })
    }

    fn read_inline_image(&mut self, start: usize) -> Result<Vec<u8>, RedactionError> {
        // `start` is the byte of the `BI` keyword and `self.pos` is just after
        // it. Scan forward to the `ID` delimiter, then to the `EI` delimiter.
        let mut cursor = self.pos;
        let mut id_after = None;
        while cursor + 1 < self.data.len() {
            if self.data[cursor] == b'I'
                && self.data[cursor + 1] == b'D'
                && (cursor == self.pos
                    || is_whitespace(self.data[cursor - 1])
                    || is_delimiter(self.data[cursor - 1]))
                && (cursor + 2 == self.data.len()
                    || is_whitespace(self.data[cursor + 2])
                    || is_delimiter(self.data[cursor + 2]))
            {
                id_after = Some(cursor + 2);
                break;
            }
            cursor += 1;
        }
        let mut data_start = id_after.ok_or(RedactionError::MalformedContent {
            detail: "inline image has no ID delimiter",
        })?;
        // Per the spec exactly one whitespace byte follows `ID`.
        if data_start < self.data.len() && is_whitespace(self.data[data_start]) {
            data_start += 1;
        }
        let mut scan = data_start;
        let mut found = None;
        while scan + 1 < self.data.len() {
            if self.data[scan] == b'E'
                && self.data[scan + 1] == b'I'
                && (scan == data_start || is_whitespace(self.data[scan - 1]))
                && (scan + 2 == self.data.len()
                    || is_whitespace(self.data[scan + 2])
                    || is_delimiter(self.data[scan + 2]))
            {
                found = Some(scan + 2);
                break;
            }
            scan += 1;
        }
        let end = found.ok_or(RedactionError::MalformedContent {
            detail: "unterminated inline image",
        })?;
        let raw = self.data[start..end].to_vec();
        self.pos = end;
        Ok(raw)
    }
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn parse_operations(content: &[u8]) -> Result<Vec<Operation>, RedactionError> {
    let mut parser = Parser::new(content);
    let mut operations: Vec<Operation> = Vec::new();
    let mut operands: Vec<Object> = Vec::new();
    let mut raw: Vec<u8> = Vec::new();
    let mut prefix: Vec<u8> = Vec::new();

    loop {
        let (gap, token) = parser.next_token()?;
        let Some(token) = token else {
            if operations.is_empty() && raw.is_empty() && operands.is_empty() {
                prefix.extend_from_slice(&gap);
            } else {
                raw.extend_from_slice(&gap);
                flush_operation(&mut operations, &mut operands, &mut raw);
            }
            break;
        };

        if raw.is_empty() && operands.is_empty() && operations.is_empty() {
            prefix.extend_from_slice(&gap);
        } else {
            raw.extend_from_slice(&gap);
        }

        match token.kind {
            TokenKind::Object(object) => {
                raw.extend_from_slice(&content[token.start..token.end]);
                operands.push(object);
            }
            TokenKind::Keyword(keyword) if keyword == "BI" => {
                let image = parser.read_inline_image(token.start)?;
                raw.extend_from_slice(&image);
                operations.push(Operation {
                    operands: Vec::new(),
                    operator: "BI".to_owned(),
                    raw: std::mem::take(&mut raw),
                    inline_image: true,
                });
            }
            TokenKind::Keyword(keyword) => {
                raw.extend_from_slice(&content[token.start..token.end]);
                operations.push(Operation {
                    operands: std::mem::take(&mut operands),
                    operator: keyword,
                    raw: std::mem::take(&mut raw),
                    inline_image: false,
                });
            }
        }
    }

    if !prefix.is_empty()
        && let Some(first) = operations.first_mut()
    {
        let mut merged = prefix;
        merged.extend_from_slice(&first.raw);
        first.raw = merged;
    }
    Ok(operations)
}

fn flush_operation(operations: &mut Vec<Operation>, operands: &mut Vec<Object>, raw: &mut Vec<u8>) {
    if !operands.is_empty() || !raw.is_empty() {
        operations.push(Operation {
            operands: std::mem::take(operands),
            operator: String::new(),
            raw: std::mem::take(raw),
            inline_image: false,
        });
    }
}

// ---------------------------------------------------------------------------
// Applying to content streams
// ---------------------------------------------------------------------------

struct TextState {
    matrix: Matrix,
    leading: f64,
    font_size: f64,
}

impl Default for TextState {
    fn default() -> Self {
        Self {
            matrix: Matrix::IDENTITY,
            leading: 0.0,
            font_size: 0.0,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn redact_stream(
    plan: &RedactionPlan,
    page: u32,
    content: &[u8],
    forms: &BTreeMap<String, FormXObject>,
    ctm: Matrix,
    depth: usize,
    form: Option<&str>,
    options: &RedactionOptions,
    cancel: &impl Fn() -> bool,
    removals: &mut Vec<Removal>,
    redacted_forms: &mut BTreeMap<String, FormXObject>,
    visiting: &mut Vec<String>,
) -> Result<Vec<u8>, RedactionError> {
    let operations = parse_operations(content)?;
    let mut out = Vec::with_capacity(content.len());
    let mut state = TextState::default();
    let mut graphics_stack: Vec<Matrix> = Vec::new();
    let mut effective_ctm = ctm;

    for op in &operations {
        if cancel() {
            return Err(RedactionError::Cancelled);
        }
        if op.inline_image {
            out.extend_from_slice(&op.raw);
            continue;
        }

        match op.operator.as_str() {
            "q" => {
                graphics_stack.push(effective_ctm);
                out.extend_from_slice(&op.raw);
            }
            "Q" => {
                if let Some(previous) = graphics_stack.pop() {
                    effective_ctm = previous;
                }
                out.extend_from_slice(&op.raw);
            }
            "cm" => {
                let numbers = number_operands(&op.operands, 6);
                if numbers.len() == 6 {
                    let matrix = Matrix::new(
                        numbers[0], numbers[1], numbers[2], numbers[3], numbers[4], numbers[5],
                    );
                    effective_ctm = matrix.mul(effective_ctm);
                }
                out.extend_from_slice(&op.raw);
            }
            "BT" => {
                state.matrix = Matrix::IDENTITY;
                out.extend_from_slice(&op.raw);
            }
            "Tm" => {
                let numbers = number_operands(&op.operands, 6);
                if numbers.len() == 6 {
                    state.matrix = Matrix::new(
                        numbers[0], numbers[1], numbers[2], numbers[3], numbers[4], numbers[5],
                    );
                }
                out.extend_from_slice(&op.raw);
            }
            "Td" => {
                let numbers = number_operands(&op.operands, 2);
                if numbers.len() == 2 {
                    state.matrix = Matrix::translate(numbers[0], numbers[1]).mul(state.matrix);
                }
                out.extend_from_slice(&op.raw);
            }
            "TD" => {
                let numbers = number_operands(&op.operands, 2);
                if numbers.len() == 2 {
                    state.leading = -numbers[1];
                    state.matrix = Matrix::translate(numbers[0], numbers[1]).mul(state.matrix);
                }
                out.extend_from_slice(&op.raw);
            }
            "TL" => {
                if let Some(value) = first_number(&op.operands) {
                    state.leading = value;
                }
                out.extend_from_slice(&op.raw);
            }
            "Tf" => {
                let numbers = number_operands(&op.operands, 2);
                if numbers.len() == 2 {
                    state.font_size = numbers[1];
                }
                out.extend_from_slice(&op.raw);
            }
            "T*" => {
                state.matrix = Matrix::translate(0.0, -state.leading).mul(state.matrix);
                out.extend_from_slice(&op.raw);
            }
            "Tj" | "TJ" | "'" | "\"" => {
                let (text, advance) = decode_shown(&op.operator, &op.operands, state.font_size);
                let page_point = effective_ctm.transform_point(state.matrix.e, state.matrix.f);
                let right = page_point.0 + advance.abs();
                let affected = pattern_matches(plan, page, &text)
                    || rect_matches(plan, page, page_point.0, right, page_point.1);
                if op.operator == "'" || op.operator == "\"" {
                    state.matrix = Matrix::translate(0.0, -state.leading).mul(state.matrix);
                }
                if affected {
                    removals.push(Removal {
                        page,
                        form: form.map(str::to_owned),
                        kind: RemovalKind::TextOperator {
                            operator: op.operator.clone(),
                            char_count: text.len(),
                        },
                    });
                } else {
                    out.extend_from_slice(&op.raw);
                }
                state.matrix = Matrix::translate(advance, 0.0).mul(state.matrix);
            }
            "Do" => {
                if let Some(name) = first_name(&op.operands)
                    && let Some(form_object) = forms.get(&name)
                {
                    if visiting.iter().any(|v| v == &name) {
                        return Err(RedactionError::FormCycle { name });
                    }
                    if depth >= options.max_form_depth {
                        return Err(RedactionError::FormDepthExceeded {
                            name,
                            max_depth: options.max_form_depth,
                        });
                    }
                    if !redacted_forms.contains_key(&name) {
                        visiting.push(name.clone());
                        let form_ctm = form_object.matrix.mul(effective_ctm);
                        let redacted = redact_stream(
                            plan,
                            page,
                            &form_object.content,
                            forms,
                            form_ctm,
                            depth + 1,
                            Some(&name),
                            options,
                            cancel,
                            removals,
                            redacted_forms,
                            visiting,
                        )?;
                        visiting.pop();
                        redacted_forms
                            .insert(name.clone(), FormXObject::new(redacted, form_object.matrix));
                    }
                }
                out.extend_from_slice(&op.raw);
            }
            _ => out.extend_from_slice(&op.raw),
        }
    }

    if options.paint_boxes {
        for region in plan.regions() {
            if region.page().get() != page {
                continue;
            }
            if let Some(rect) = region.as_rect() {
                let box_bytes = format!(
                    "\nq\n0 0 0 rg\n{} {} {} {} re f\nQ\n",
                    rect.x0(),
                    rect.y0(),
                    rect.width(),
                    rect.height()
                );
                out.extend_from_slice(box_bytes.as_bytes());
                removals.push(Removal {
                    page,
                    form: form.map(str::to_owned),
                    kind: RemovalKind::PaintedBox {
                        x0: rect.x0(),
                        y0: rect.y0(),
                        x1: rect.x1(),
                        y1: rect.y1(),
                    },
                });
            }
        }
    }

    Ok(out)
}

fn number_operands(operands: &[Object], max: usize) -> Vec<f64> {
    let mut numbers = Vec::new();
    for operand in operands.iter().take(max) {
        if let Object::Number(value) = operand {
            numbers.push(*value);
        } else {
            break;
        }
    }
    numbers
}

fn first_number(operands: &[Object]) -> Option<f64> {
    operands.first().and_then(|value| match value {
        Object::Number(number) => Some(*number),
        _ => None,
    })
}

fn first_name(operands: &[Object]) -> Option<String> {
    operands.first().and_then(|value| match value {
        Object::Name(name) => Some(name.clone()),
        _ => None,
    })
}

/// Decode the text shown by a text-showing operator and its horizontal advance.
fn decode_shown(operator: &str, operands: &[Object], font_size: f64) -> (Vec<u8>, f64) {
    let estimate = |text: &[u8]| text.len() as f64 * font_size * 0.6;
    match operator {
        "Tj" | "'" | "\"" => {
            let index = if operator == "\"" { 2 } else { 0 };
            match operands.get(index) {
                Some(Object::String(text)) => (text.clone(), estimate(text)),
                _ => (Vec::new(), 0.0),
            }
        }
        "TJ" => {
            let mut text = Vec::new();
            let mut advance = 0.0;
            if let Some(Object::Array(items)) = operands.first() {
                for item in items {
                    match item {
                        Object::String(bytes) => {
                            advance += estimate(bytes);
                            text.extend_from_slice(bytes);
                        }
                        Object::Number(value) => advance -= *value / 1000.0 * font_size,
                        _ => {}
                    }
                }
            }
            (text, advance)
        }
        _ => (Vec::new(), 0.0),
    }
}

fn shown_text(operation: &Operation) -> Option<Vec<u8>> {
    match operation.operator.as_str() {
        "Tj" | "'" | "\"" => {
            let index = if operation.operator == "\"" { 2 } else { 0 };
            match operation.operands.get(index) {
                Some(Object::String(text)) => Some(text.clone()),
                _ => None,
            }
        }
        "TJ" => match operation.operands.first() {
            Some(Object::Array(items)) => {
                let mut text = Vec::new();
                for item in items {
                    if let Object::String(bytes) = item {
                        text.extend_from_slice(bytes);
                    }
                }
                Some(text)
            }
            _ => None,
        },
        _ => None,
    }
}

fn pattern_matches(plan: &RedactionPlan, page: u32, text: &[u8]) -> bool {
    let needle = String::from_utf8_lossy(text);
    plan.regions().iter().any(|region| {
        region.page().get() == page
            && region
                .as_pattern()
                .is_some_and(|pattern| pattern.matches(&needle))
    })
}

fn rect_matches(plan: &RedactionPlan, page: u32, left: f64, right: f64, y: f64) -> bool {
    plan.regions().iter().any(|region| {
        region.page().get() == page
            && region
                .as_rect()
                .is_some_and(|rect| rect.overlaps_text(left, right, y))
    })
}

// ---------------------------------------------------------------------------
// Page dictionary stripping
// ---------------------------------------------------------------------------

fn strip_dictionary(
    dict: &Dict,
    page: u32,
    plan: &RedactionPlan,
    removals: &mut Vec<Removal>,
) -> Dict {
    let mut out = dict.clone();
    let patterns: Vec<&TextPattern> = plan
        .regions()
        .iter()
        .filter(|region| region.page().get() == page)
        .filter_map(RedactionRegion::as_pattern)
        .collect();
    let rects: Vec<&RedactionRect> = plan
        .regions()
        .iter()
        .filter(|region| region.page().get() == page)
        .filter_map(RedactionRegion::as_rect)
        .collect();

    if let Some(Value::Array(annotations)) = out.get_mut("Annots") {
        annotations.retain(|annotation| {
            let Some(entry) = annotation.as_dict() else {
                return true;
            };
            let subtype = entry
                .get("Subtype")
                .and_then(Value::as_name)
                .unwrap_or("Unknown")
                .to_owned();
            let mut remove = false;
            if let Some((first, second, third, fourth)) =
                numeric_rect(entry.get("Rect").and_then(Value::as_array))
            {
                let page_rect =
                    Rect::saturating(first as f32, second as f32, third as f32, fourth as f32);
                remove = rects.iter().any(|rect| {
                    let target = Rect::saturating(
                        rect.x0() as f32,
                        rect.y0() as f32,
                        rect.x1() as f32,
                        rect.y1() as f32,
                    );
                    page_rect.intersects(&target)
                });
            }
            let mut text_fields = Vec::new();
            for key in ["Contents", "T"] {
                if let Some(text) = entry.get(key).and_then(Value::as_text) {
                    text_fields.push(text.to_owned());
                }
            }
            if let Some(action) = entry.get("A").and_then(Value::as_dict)
                && let Some(uri) = action.get("URI").and_then(Value::as_text)
            {
                text_fields.push(uri.to_owned());
            }
            if patterns
                .iter()
                .any(|pattern| text_fields.iter().any(|text| pattern.matches(text)))
            {
                remove = true;
            }
            if remove {
                removals.push(Removal {
                    page,
                    form: None,
                    kind: RemovalKind::AnnotationRemoved { subtype },
                });
            }
            !remove
        });
    }

    if !patterns.is_empty() {
        for key in ["PieceInfo", "Metadata"] {
            if out.remove(key).is_some() {
                removals.push(Removal {
                    page,
                    form: None,
                    kind: RemovalKind::MetadataKeyRemoved {
                        key: key.to_owned(),
                    },
                });
            }
        }
    }
    out
}

fn numeric_rect(array: Option<&[Value]>) -> Option<(f64, f64, f64, f64)> {
    let values = array?;
    if values.len() < 4 {
        return None;
    }
    Some((
        values[0].as_number()?,
        values[1].as_number()?,
        values[2].as_number()?,
        values[3].as_number()?,
    ))
}

// ---------------------------------------------------------------------------
// Verification
// ---------------------------------------------------------------------------

fn residuals_for_plan(
    plan: &RedactionPlan,
    page_text: &BTreeMap<u32, String>,
) -> Result<Vec<Residual>, RedactionError> {
    let mut residuals = Vec::new();
    for region in plan.regions() {
        let page = region.page().get();
        let Some(text) = page_text.get(&page) else {
            continue;
        };
        match region {
            RedactionRegion::Rect { removed_text, .. } => {
                if let Some(label) = removed_text
                    && !label.trim().is_empty()
                {
                    let pattern = TextPattern::case_sensitive(label.clone())?;
                    for offset in find_occurrences(text, pattern.literal(), true) {
                        residuals.push(Residual {
                            page,
                            offset,
                            source: ResidualSource::RectangleLabel(label.clone()),
                        });
                    }
                }
            }
            RedactionRegion::Text { pattern, .. } => {
                for offset in find_occurrences(text, pattern.literal(), pattern.is_case_sensitive())
                {
                    residuals.push(Residual {
                        page,
                        offset,
                        source: ResidualSource::Pattern(pattern.literal().to_owned()),
                    });
                }
            }
        }
    }
    residuals.sort_by(|a, b| {
        a.page
            .cmp(&b.page)
            .then(a.offset.cmp(&b.offset))
            .then(a.source.cmp(&b.source))
    });
    let mut seen = HashSet::new();
    residuals.retain(|r| seen.insert((r.page, r.offset, r.source.clone())));
    Ok(residuals)
}

/// Find non-overlapping occurrences of `needle`, returning byte offsets.
///
/// Case-insensitive matching lowers both sides char-by-char while retaining the
/// original byte offset of each lowered character, so reported offsets stay
/// valid even when Unicode case folding changes length.
fn find_occurrences(haystack: &str, needle: &str, case_sensitive: bool) -> Vec<usize> {
    if needle.is_empty() {
        return Vec::new();
    }
    if case_sensitive {
        return haystack
            .match_indices(needle)
            .map(|(index, _)| index)
            .collect();
    }
    let haystack_chars: Vec<(char, usize)> = haystack
        .char_indices()
        .flat_map(|(index, character)| {
            character
                .to_lowercase()
                .map(move |lowered| (lowered, index))
        })
        .collect();
    let needle_chars: Vec<char> = needle.chars().flat_map(char::to_lowercase).collect();
    if needle_chars.is_empty() {
        return Vec::new();
    }
    let mut offsets = Vec::new();
    let mut cursor = 0;
    while cursor + needle_chars.len() <= haystack_chars.len() {
        let slice = &haystack_chars[cursor..cursor + needle_chars.len()];
        if slice
            .iter()
            .zip(needle_chars.iter())
            .all(|((actual, _), expected)| actual == expected)
        {
            offsets.push(slice[0].1);
            cursor += needle_chars.len();
        } else {
            cursor += 1;
        }
    }
    offsets
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(value: u32) -> PageNumber {
        PageNumber::new(value).expect("test page numbers are non-zero")
    }

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> RedactionRect {
        RedactionRect::new(x0, y0, x1, y1).expect("test rectangles are valid")
    }

    fn apply_single(
        plan: &RedactionPlan,
        content: &[u8],
        forms: &BTreeMap<String, FormXObject>,
        dict: Option<Dict>,
        options: &RedactionOptions,
    ) -> AppliedRedaction {
        let mut input = RedactionInput::default();
        input.pages.insert(1, content.to_vec());
        input.forms = forms.clone();
        if let Some(dict) = dict {
            input.dictionaries.insert(1, dict);
        }
        let never = || false;
        apply(plan, &input, options, &never).expect("test application succeeds")
    }

    fn content_str(applied: &AppliedRedaction, page: u32) -> String {
        String::from_utf8_lossy(applied.page_content(page).unwrap_or_default()).into_owned()
    }

    #[test]
    fn rectangle_removes_tj_text() {
        let content = b"BT /F1 12 Tf 100 700 Td (SECRET) Tj ET\n";
        let plan = RedactionPlan::new([RedactionRegion::rect(
            page(1),
            rect(90.0, 690.0, 220.0, 710.0),
        )]);
        let applied = apply_single(
            &plan,
            content,
            &BTreeMap::new(),
            None,
            &RedactionOptions::default(),
        );
        let output = content_str(&applied, 1);
        assert!(
            !output.contains("SECRET"),
            "text operator must be gone: {output}"
        );
        assert!(output.contains("BT"));
        assert!(output.contains("ET"));
        assert_eq!(applied.report().removed_operator_count(), 1);
        assert!(scan_text(applied.page_content(1).unwrap_or_default()).is_ok());
    }

    #[test]
    fn pattern_removes_all_occurrences() {
        let content = b"BT (SECRET) Tj 0 -20 Td (SECRET) Tj ET\n";
        let pattern = TextPattern::case_sensitive("SECRET").expect("valid pattern");
        let plan = RedactionPlan::new([RedactionRegion::text(page(1), pattern)]);
        let applied = apply_single(
            &plan,
            content,
            &BTreeMap::new(),
            None,
            &RedactionOptions::default(),
        );
        assert_eq!(applied.report().removed_operator_count(), 2);
        assert!(!content_str(&applied, 1).contains("SECRET"));
        assert!(applied.report().is_clean());
        assert!(applied.check().is_ok());
    }

    #[test]
    fn tj_arrays_are_redacted() {
        let content = b"BT /F1 12 Tf 100 700 Td [(SEC) -50 (RET)] TJ (public) Tj ET\n";
        let pattern = TextPattern::case_sensitive("SECRET").expect("valid pattern");
        let plan = RedactionPlan::new([RedactionRegion::text(page(1), pattern)]);
        let applied = apply_single(
            &plan,
            content,
            &BTreeMap::new(),
            None,
            &RedactionOptions::default(),
        );
        let output = content_str(&applied, 1);
        assert!(!output.contains("SEC"));
        assert!(!output.contains("RET"));
        assert!(output.contains("(public) Tj"));
        assert_eq!(applied.report().removed_operator_count(), 1);
    }

    #[test]
    fn quote_operators_are_redacted() {
        let content = b"BT /F1 12 Tf 10 TL (SECRET) ' (show) \"\n";
        let pattern = TextPattern::case_sensitive("SECRET").expect("valid pattern");
        let plan = RedactionPlan::new([RedactionRegion::text(page(1), pattern)]);
        let applied = apply_single(
            &plan,
            content,
            &BTreeMap::new(),
            None,
            &RedactionOptions::default(),
        );
        assert_eq!(applied.report().removed_operator_count(), 1);
        assert!(!content_str(&applied, 1).contains("SECRET"));
    }

    #[test]
    fn form_xobject_recursion_redacts_nested_content() {
        let mut forms = BTreeMap::new();
        forms.insert(
            "Fm".to_owned(),
            FormXObject::new(
                b"BT /F1 12 Tf 5 5 Td (SECRET) Tj ET\n".to_vec(),
                Matrix::IDENTITY,
            ),
        );
        let content = b"q 1 0 0 1 0 0 cm /Fm Do Q\n";
        let pattern = TextPattern::case_sensitive("SECRET").expect("valid pattern");
        let plan = RedactionPlan::new([RedactionRegion::text(page(1), pattern)]);
        let applied = apply_single(&plan, content, &forms, None, &RedactionOptions::default());
        let form_content = String::from_utf8_lossy(&applied.forms()["Fm"].content);
        assert!(!form_content.contains("SECRET"));
        assert!(content_str(&applied, 1).contains("/Fm Do"));
        assert_eq!(applied.report().removed_operator_count(), 1);
        assert!(
            applied
                .report()
                .removals()
                .iter()
                .all(|r| r.form.as_deref() == Some("Fm"))
        );
    }

    #[test]
    fn form_depth_limit_and_cycles_are_typed_errors() {
        let mut forms = BTreeMap::new();
        forms.insert(
            "Loop".to_owned(),
            FormXObject::new(b"/Loop Do\n".to_vec(), Matrix::IDENTITY),
        );
        let content = b"/Loop Do\n";
        let plan = RedactionPlan::empty();
        let never = || false;
        let options = RedactionOptions::default();
        let mut input = RedactionInput::default();
        input.pages.insert(1, content.to_vec());
        input.forms = forms;
        let err = apply(&plan, &input, &options, &never).expect_err("cycle is detected");
        assert!(matches!(err, RedactionError::FormCycle { .. }));

        let nested = RedactionOptions {
            max_form_depth: 0,
            ..RedactionOptions::default()
        };
        let mut forms = BTreeMap::new();
        forms.insert(
            "Fm".to_owned(),
            FormXObject::new(b"BT (x) Tj ET\n".to_vec(), Matrix::IDENTITY),
        );
        let mut input = RedactionInput::default();
        input.pages.insert(1, b"/Fm Do\n".to_vec());
        input.forms = forms;
        let err = apply(&plan, &input, &nested, &never).expect_err("depth limit is enforced");
        assert!(matches!(err, RedactionError::FormDepthExceeded { .. }));
    }

    #[test]
    fn inline_image_is_untouched() {
        let image = b"BI /W 2 /H 2 /CS /RGB /BPC 8 ID \x01\x02\x03\x04 EI\n";
        let mut content = Vec::new();
        content.extend_from_slice(b"BT (public) Tj ET\n");
        content.extend_from_slice(image);
        let plan = RedactionPlan::new([RedactionRegion::text(
            page(1),
            TextPattern::case_sensitive("never").expect("valid pattern"),
        )]);
        let applied = apply_single(
            &plan,
            &content,
            &BTreeMap::new(),
            None,
            &RedactionOptions::default(),
        );
        let output = content_str(&applied, 1);
        assert!(output.contains(String::from_utf8_lossy(image).as_ref()));
        assert!(output.contains("(public) Tj"));
        assert!(applied.report().removals().is_empty());
    }

    #[test]
    fn non_target_text_is_preserved_byte_for_byte() {
        let content = b"BT /F1 12 Tf 10 10 Td (public) Tj 0 -2 Td (SECRET) Tj ET\n";
        let pattern = TextPattern::case_sensitive("SECRET").expect("valid pattern");
        let plan = RedactionPlan::new([RedactionRegion::text(page(1), pattern)]);
        let applied = apply_single(
            &plan,
            content,
            &BTreeMap::new(),
            None,
            &RedactionOptions::default(),
        );
        let output = content_str(&applied, 1);
        assert!(output.contains("BT /F1 12 Tf 10 10 Td (public) Tj"));
    }

    #[test]
    fn opaque_box_is_painted_for_rectangles() {
        let target = rect(10.0, 20.0, 110.0, 60.0);
        let plan = RedactionPlan::new([RedactionRegion::rect(page(1), target)]);
        let applied = apply_single(
            &plan,
            b"BT ET\n",
            &BTreeMap::new(),
            None,
            &RedactionOptions::default(),
        );
        let output = content_str(&applied, 1);
        assert!(output.contains("0 0 0 rg"));
        assert!(output.contains("10 20 100 40 re f"));
        assert!(
            applied
                .report()
                .removals()
                .iter()
                .any(|r| matches!(r.kind, RemovalKind::PaintedBox { .. }))
        );
    }

    #[test]
    fn annotations_links_and_metadata_are_stripped() {
        let mut link = Dict::new();
        link.insert("Subtype".into(), Value::Name("Link".into()));
        link.insert(
            "Rect".into(),
            Value::Array(vec![
                Value::Real(5.0),
                Value::Real(5.0),
                Value::Real(15.0),
                Value::Real(15.0),
            ]),
        );
        let mut action = Dict::new();
        action.insert(
            "URI".into(),
            Value::Text("https://example.test/SECRET".into()),
        );
        link.insert("A".into(), Value::Dictionary(action));
        let mut far = Dict::new();
        far.insert("Subtype".into(), Value::Name("Text".into()));
        far.insert(
            "Rect".into(),
            Value::Array(vec![
                Value::Real(100.0),
                Value::Real(100.0),
                Value::Real(120.0),
                Value::Real(120.0),
            ]),
        );
        let mut dict = Dict::new();
        dict.insert(
            "Annots".into(),
            Value::Array(vec![Value::Dictionary(link), Value::Dictionary(far)]),
        );
        dict.insert("PieceInfo".into(), Value::Dictionary(Dict::new()));
        dict.insert("Metadata".into(), Value::Int(7));

        let pattern = TextPattern::case_sensitive("SECRET").expect("valid pattern");
        let plan = RedactionPlan::new([
            RedactionRegion::rect(page(1), rect(0.0, 0.0, 20.0, 20.0)),
            RedactionRegion::text(page(1), pattern),
        ]);
        let applied = apply_single(
            &plan,
            b"BT ET\n",
            &BTreeMap::new(),
            Some(dict),
            &RedactionOptions::default(),
        );
        let stripped = applied.page_dictionary(1).expect("page dictionary exists");
        let annotations = stripped["Annots"].as_array().expect("annots array");
        assert_eq!(annotations.len(), 1, "only the far annotation remains");
        assert_eq!(
            annotations[0]
                .as_dict()
                .and_then(|d| d.get("Subtype"))
                .and_then(Value::as_name),
            Some("Text")
        );
        assert!(!stripped.contains_key("PieceInfo"));
        assert!(!stripped.contains_key("Metadata"));
        assert!(
            applied
                .report()
                .removals()
                .iter()
                .any(|r| matches!(r.kind, RemovalKind::AnnotationRemoved { .. }))
        );
    }

    #[test]
    fn verification_fails_when_labeled_content_remains() {
        // The rectangle does not cover the drawn text, so the label is still
        // present after application and strict verification must fail.
        let content = b"BT /F1 12 Tf 300 300 Td (SECRET) Tj ET\n";
        let plan = RedactionPlan::new([RedactionRegion::rect_with_text(
            page(1),
            rect(10.0, 10.0, 40.0, 40.0),
            "SECRET",
        )]);
        let applied = apply_single(
            &plan,
            content,
            &BTreeMap::new(),
            None,
            &RedactionOptions::default(),
        );
        assert!(!applied.report().is_clean());
        let err = applied.check().expect_err("strict verification fails");
        assert!(matches!(
            err,
            RedactionError::VerificationFailed { page: 1, .. }
        ));
    }

    #[test]
    fn supplied_extractor_controls_verification() {
        let content = b"BT (SECRET) Tj ET\n";
        let pattern = TextPattern::case_sensitive("SECRET").expect("valid pattern");
        let plan = RedactionPlan::new([RedactionRegion::text(page(1), pattern)]);
        let options = RedactionOptions::default();
        let applied = apply_single(&plan, content, &BTreeMap::new(), None, &options);
        assert!(applied.check().is_ok());
        let mut borrowed = applied.clone();
        let report = borrowed
            .verify_with(
                &plan,
                |_page, _content| "still SECRET here".to_owned(),
                true,
            )
            .expect("verification runs");
        assert!(!report.is_clean());
        assert!(borrowed.check().is_err());
    }

    #[test]
    fn output_is_deterministic_regardless_of_insertion_order() {
        let pattern = TextPattern::case_sensitive("SECRET").expect("valid pattern");
        let region_a = RedactionRegion::rect(page(1), rect(10.0, 10.0, 40.0, 40.0));
        let region_b = RedactionRegion::text(page(1), pattern);
        let mut plan_a = RedactionPlan::empty();
        plan_a.add(region_a.clone());
        plan_a.add(region_b.clone());
        let mut plan_b = RedactionPlan::empty();
        plan_b.add(region_b);
        plan_b.add(region_a);
        assert_eq!(plan_a, plan_b);

        let content = b"BT /F1 12 Tf 10 10 Td (SECRET) Tj ET\n";
        let content_options = RedactionOptions::default();
        let first = apply_single(&plan_a, content, &BTreeMap::new(), None, &content_options);
        let second = apply_single(&plan_b, content, &BTreeMap::new(), None, &content_options);
        assert_eq!(
            first.page_content(1).unwrap_or_default(),
            second.page_content(1).unwrap_or_default()
        );
        assert_eq!(first.report(), second.report());
    }

    #[test]
    fn cancellation_is_a_typed_error() {
        let content = b"BT (x) Tj ET\n";
        let plan = RedactionPlan::empty();
        let options = RedactionOptions::default();
        let mut input = RedactionInput::default();
        input.pages.insert(1, content.to_vec());
        let cancelled = || true;
        let err = apply(&plan, &input, &options, &cancelled).expect_err("cancelled");
        assert_eq!(err, RedactionError::Cancelled);
    }

    #[test]
    fn validation_rejects_bad_plans_and_geometry() {
        assert_eq!(
            RedactionRect::new(0.0, 0.0, 0.0, 1.0),
            Err(RedactionError::EmptyRegion)
        );
        assert_eq!(
            RedactionRect::new(f64::NAN, 0.0, 1.0, 1.0),
            Err(RedactionError::NonFiniteCoordinate)
        );
        assert_eq!(
            TextPattern::new("   ", true),
            Err(RedactionError::EmptyPattern)
        );
        assert_eq!(PageNumber::new(0), Err(RedactionError::InvalidPageNumber));

        let mut input = RedactionInput::default();
        input.pages.insert(1, Vec::new());
        input
            .page_bounds
            .insert(1, Rect::new(0.0, 0.0, 100.0, 100.0).expect("valid bounds"));
        let unknown =
            RedactionPlan::new([RedactionRegion::rect(page(2), rect(0.0, 0.0, 1.0, 1.0))]);
        assert_eq!(
            unknown.validate(&input),
            Err(RedactionError::UnknownPage { page: 2 })
        );
        let outside = RedactionPlan::new([RedactionRegion::rect(
            page(1),
            rect(90.0, 90.0, 110.0, 110.0),
        )]);
        assert_eq!(
            outside.validate(&input),
            Err(RedactionError::RegionOutOfBounds { page: 1 })
        );
    }

    #[test]
    fn clipping_render_mode_does_not_hide_text() {
        // Text clipping render modes (4..=7) still carry the secret bytes in the
        // content stream, so they must be removed like any other text operator.
        let content = b"BT /F1 12 Tf 4 Tr (SECRET) Tj ET\n";
        let pattern = TextPattern::case_sensitive("SECRET").expect("valid pattern");
        let plan = RedactionPlan::new([RedactionRegion::text(page(1), pattern)]);
        let applied = apply_single(
            &plan,
            content,
            &BTreeMap::new(),
            None,
            &RedactionOptions::default(),
        );
        assert_eq!(applied.report().removed_operator_count(), 1);
        assert!(!content_str(&applied, 1).contains("SECRET"));
    }

    #[test]
    fn matrix_updates_affect_rectangle_matching() {
        // A `cm` translation is folded into the CTM, so a rectangle in page
        // space finds text that a naive local-coordinate test would miss.
        let content = b"q 1 0 0 1 200 0 cm BT (SECRET) Tj ET Q\n";
        let plan = RedactionPlan::new([RedactionRegion::rect(
            page(1),
            rect(190.0, -5.0, 300.0, 5.0),
        )]);
        let options = RedactionOptions {
            paint_boxes: false,
            ..RedactionOptions::default()
        };
        let applied = apply_single(&plan, content, &BTreeMap::new(), None, &options);
        assert_eq!(applied.report().removed_operator_count(), 1);
    }
}
