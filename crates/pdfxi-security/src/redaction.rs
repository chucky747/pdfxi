//! Redaction plans and verification of post-redaction extracted text.
//!
//! A [`RedactionPlan`] is a canonical, ordered set of page-scoped regions.
//! Rectangular regions remove a rectangle of page content; text-pattern regions
//! remove every literal occurrence of a string. A rectangle may carry a
//! `removed_text` label so verification can check that specific text is absent
//! from the text extracted after redaction.
//!
//! Verification is deliberately caller-supplied: this crate never opens or
//! mutates a PDF. The caller performs the redaction, extracts the *post-redaction*
//! text of each page, and hands it to [`RedactionReport::verify`]. [`RedactionReport::check`]
//! enforces strict mode. Proving that rasterised content inside a rectangle was
//! removed requires image extraction by the renderer and is out of scope for
//! this dependency-free model; no secrets or document contents are logged.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;
use std::num::NonZeroU32;

use crate::error::{Result, SecurityError};

/// A 1-based PDF page number.
///
/// Page zero is invalid and is rejected by [`Self::new`]. Values are ordered
/// numerically so plans and reports have a stable page-major ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PageNumber(NonZeroU32);

impl PageNumber {
    /// Create a page number, rejecting zero.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::InvalidPageNumber`] when `page` is zero.
    pub fn new(page: u32) -> Result<Self> {
        NonZeroU32::new(page)
            .map(Self)
            .ok_or(SecurityError::InvalidPageNumber)
    }

    /// Return the page value as `u32` (always at least 1).
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

impl fmt::Display for PageNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Axis-aligned page dimensions used to validate rectangle regions.
///
/// Both dimensions must be finite and strictly positive; see [`Self::new`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageBounds {
    width: f64,
    height: f64,
}

impl PageBounds {
    /// Create page bounds from a finite, positive width and height.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::InvalidPageBounds`] if either dimension is not
    /// finite or is not greater than zero.
    pub fn new(width: f64, height: f64) -> Result<Self> {
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            return Err(SecurityError::InvalidPageBounds);
        }
        Ok(Self { width, height })
    }

    /// Page width in user-space units.
    pub const fn width(&self) -> f64 {
        self.width
    }

    /// Page height in user-space units.
    pub const fn height(&self) -> f64 {
        self.height
    }

    /// Return `true` when `rect` lies fully inside the positive quadrant of
    /// this page, inclusive of the page edges.
    fn contains(&self, rect: &RedactionRect) -> bool {
        rect.x0 >= 0.0 && rect.y0 >= 0.0 && rect.x1 <= self.width && rect.y1 <= self.height
    }
}

/// An axis-aligned rectangle in PDF user-space coordinates.
///
/// Coordinates are ordered as `(x0, y0, x1, y1)` with `x1 > x0` and `y1 > y0`.
/// Equality and ordering use [`f64::total_cmp`], so the type is a total order
/// even for `-0.0` versus `0.0`; non-finite coordinates are rejected at
/// construction and cannot be present.
#[derive(Debug, Clone, Copy)]
pub struct RedactionRect {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl RedactionRect {
    /// Create a rectangle, validating finiteness and non-empty area.
    ///
    /// # Errors
    ///
    /// - [`SecurityError::NonFiniteCoordinate`] if any coordinate is NaN or
    ///   infinite.
    /// - [`SecurityError::EmptyRedactionRegion`] if the width or height is not
    ///   strictly positive.
    pub fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Result<Self> {
        if ![x0, y0, x1, y1].iter().all(|value| value.is_finite()) {
            return Err(SecurityError::NonFiniteCoordinate);
        }
        if x1 <= x0 || y1 <= y0 {
            return Err(SecurityError::EmptyRedactionRegion);
        }
        Ok(Self { x0, y0, x1, y1 })
    }

    /// Left edge (minimum x).
    pub const fn x0(&self) -> f64 {
        self.x0
    }

    /// Bottom edge (minimum y).
    pub const fn y0(&self) -> f64 {
        self.y0
    }

    /// Right edge (maximum x).
    pub const fn x1(&self) -> f64 {
        self.x1
    }

    /// Top edge (maximum y).
    pub const fn y1(&self) -> f64 {
        self.y1
    }

    /// Rectangle width (`x1 - x0`), always strictly positive.
    pub fn width(&self) -> f64 {
        self.x1 - self.x0
    }

    /// Rectangle height (`y1 - y0`), always strictly positive.
    pub fn height(&self) -> f64 {
        self.y1 - self.y0
    }

    /// Return `true` when `(x, y)` is inside this rectangle, edges inclusive.
    pub fn contains_point(&self, x: f64, y: f64) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }

    /// Re-run the constructor invariants defensively.
    fn validate_geometry(&self) -> Result<()> {
        Self::new(self.x0, self.y0, self.x1, self.y1).map(|_| ())
    }
}

impl PartialEq for RedactionRect {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for RedactionRect {}

impl PartialOrd for RedactionRect {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RedactionRect {
    fn cmp(&self, other: &Self) -> Ordering {
        self.x0
            .total_cmp(&other.x0)
            .then_with(|| self.y0.total_cmp(&other.y0))
            .then_with(|| self.x1.total_cmp(&other.x1))
            .then_with(|| self.y1.total_cmp(&other.y1))
    }
}

/// A literal string to redact, with an explicit case-sensitivity policy.
///
/// Matching is literal (no regular expressions), so patterns cannot introduce
/// catastrophic backtracking when applied to untrusted extracted text.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextPattern {
    needle: String,
    case_sensitive: bool,
}

impl TextPattern {
    /// Create a pattern, rejecting empty or whitespace-only needles.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::EmptyRedactionPattern`] if `needle` trims to an
    /// empty string.
    pub fn new(needle: impl Into<String>, case_sensitive: bool) -> Result<Self> {
        let needle = needle.into();
        if needle.trim().is_empty() {
            return Err(SecurityError::EmptyRedactionPattern);
        }
        Ok(Self {
            needle,
            case_sensitive,
        })
    }

    /// Create a case-sensitive pattern.
    pub fn case_sensitive(needle: impl Into<String>) -> Result<Self> {
        Self::new(needle, true)
    }

    /// Create a case-insensitive pattern.
    pub fn case_insensitive(needle: impl Into<String>) -> Result<Self> {
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
}

/// A page-scoped redaction region.
///
/// Ordering is by page, then variant (rectangles before text), then region
/// contents. This makes a [`RedactionPlan`] canonical regardless of insertion
/// order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum RedactionRegion {
    /// A rectangular region, optionally labelled with the text it removed.
    Rect {
        /// 1-based page containing the rectangle.
        page: PageNumber,
        /// Validated rectangle in user-space coordinates.
        rect: RedactionRect,
        /// Text known to have been inside the rectangle, when available.
        ///
        /// An empty label is ignored during verification.
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
    /// Build a rectangular region with no removed-text label.
    pub fn rect(page: PageNumber, rect: RedactionRect) -> Self {
        Self::Rect {
            page,
            rect,
            removed_text: None,
        }
    }

    /// Build a rectangular region labelled with the text it removed.
    ///
    /// The label is checked during verification in the same way as a text
    /// pattern (case-sensitively). Empty labels are ignored.
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

    /// 1-based page this region applies to.
    pub fn page(&self) -> PageNumber {
        match self {
            Self::Rect { page, .. } | Self::Text { page, .. } => *page,
        }
    }

    /// Rectangle of a rectangular region, if this is one.
    pub const fn as_rect(&self) -> Option<&RedactionRect> {
        match self {
            Self::Rect { rect, .. } => Some(rect),
            Self::Text { .. } => None,
        }
    }

    /// Pattern of a text region, if this is one.
    pub const fn as_pattern(&self) -> Option<&TextPattern> {
        match self {
            Self::Text { pattern, .. } => Some(pattern),
            Self::Rect { .. } => None,
        }
    }

    /// Removed-text label of a rectangular region, if present.
    pub fn removed_text(&self) -> Option<&str> {
        match self {
            Self::Rect { removed_text, .. } => removed_text.as_deref(),
            Self::Text { .. } => None,
        }
    }
}

/// A canonical, validated plan describing what to redact.
///
/// Regions are sorted on construction and on every [`Self::add`], so iteration
/// order is deterministic and independent of insertion order.
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

    /// Return `true` when the plan contains no regions.
    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    /// Number of regions in the plan.
    pub fn len(&self) -> usize {
        self.regions.len()
    }

    /// Validate constructor invariants of every region without page context.
    ///
    /// This is defensive: [`RedactionRect::new`] and [`TextPattern::new`] already
    /// enforce these invariants, but a plan received from another layer is
    /// rechecked before verification.
    ///
    /// # Errors
    ///
    /// Propagates the structured geometry error of the offending region.
    pub fn validate_geometry(&self) -> Result<()> {
        for region in &self.regions {
            match region {
                RedactionRegion::Rect { rect, .. } => rect.validate_geometry()?,
                RedactionRegion::Text { pattern, .. } => {
                    TextPattern::new(pattern.literal(), pattern.is_case_sensitive())?;
                }
            }
        }
        Ok(())
    }

    /// Validate geometry and page bounds of every region.
    ///
    /// Rectangular regions must reference a page declared in `page_bounds` and
    /// lie fully inside it. Text patterns have no coordinates and are only
    /// geometry-checked; they may target pages that are not in `page_bounds`.
    ///
    /// # Errors
    ///
    /// - [`SecurityError::UnknownPage`] if a rectangle references an undeclared
    ///   page.
    /// - [`SecurityError::RedactionRegionOutOfBounds`] if a rectangle exceeds
    ///   the declared page bounds.
    pub fn validate(&self, page_bounds: &BTreeMap<PageNumber, PageBounds>) -> Result<()> {
        self.validate_geometry()?;
        for region in &self.regions {
            if let RedactionRegion::Rect { page, rect, .. } = region {
                let bounds = page_bounds
                    .get(page)
                    .ok_or(SecurityError::UnknownPage { page: page.get() })?;
                if !bounds.contains(rect) {
                    return Err(SecurityError::RedactionRegionOutOfBounds { page: page.get() });
                }
            }
        }
        Ok(())
    }
}

/// Origin of a residual occurrence reported by [`RedactionReport`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ResidualSource {
    /// The occurrence matched a text-pattern region.
    Pattern(TextPattern),
    /// The occurrence matched the removed-text label of a rectangle region.
    RectangleLabel,
}

/// One residual occurrence of redacted content in post-redaction text.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ResidualOccurrence {
    page: PageNumber,
    offset: usize,
    source: ResidualSource,
}

impl ResidualOccurrence {
    /// Build a residual occurrence.
    pub fn new(page: PageNumber, offset: usize, source: ResidualSource) -> Self {
        Self {
            page,
            offset,
            source,
        }
    }

    /// Page whose extracted text contained the residual.
    pub const fn page(&self) -> PageNumber {
        self.page
    }

    /// Byte offset into that page's supplied text where the residual begins.
    pub const fn offset(&self) -> usize {
        self.offset
    }

    /// Which plan region produced this residual.
    pub fn source(&self) -> &ResidualSource {
        &self.source
    }
}

/// Result of verifying redaction against caller-supplied post-redaction text.
///
/// [`Self::verify`] always produces a report; it never fails merely because a
/// residual was found. Call [`Self::check`] to enforce strict mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactionReport {
    residuals: Vec<ResidualOccurrence>,
    strict: bool,
}

impl RedactionReport {
    /// Verify that every text-bearing region in `plan` is absent from `page_text`.
    ///
    /// `page_text` maps 1-based page numbers to the text extracted *after*
    /// redaction. Pages not present in the map are skipped. Rectangle regions
    /// without a removed-text label cannot be verified from text alone and are
    /// skipped; their raster coverage must be checked by the renderer.
    ///
    /// # Errors
    ///
    /// Returns a structured geometry error if the plan fails
    /// [`RedactionPlan::validate_geometry`]. This method does not fail on
    /// residual content; use [`Self::check`] for that.
    pub fn verify(
        plan: &RedactionPlan,
        page_text: &BTreeMap<PageNumber, String>,
        strict: bool,
    ) -> Result<Self> {
        plan.validate_geometry()?;
        let mut residuals = Vec::new();

        for region in plan.regions() {
            let page = region.page();
            let Some(text) = page_text.get(&page) else {
                continue;
            };
            match region {
                RedactionRegion::Rect { removed_text, .. } => {
                    let Some(label) = removed_text.as_deref() else {
                        continue;
                    };
                    if label.trim().is_empty() {
                        continue;
                    }
                    let pattern = TextPattern::case_sensitive(label)?;
                    push_residuals(
                        &mut residuals,
                        page,
                        text,
                        &pattern,
                        ResidualSource::RectangleLabel,
                    );
                }
                RedactionRegion::Text { pattern, .. } => {
                    push_residuals(
                        &mut residuals,
                        page,
                        text,
                        pattern,
                        ResidualSource::Pattern(pattern.clone()),
                    );
                }
            }
        }

        residuals.sort();
        residuals.dedup();
        Ok(Self { residuals, strict })
    }

    /// Residual occurrences in canonical `(page, offset, source)` order.
    pub fn residuals(&self) -> &[ResidualOccurrence] {
        &self.residuals
    }

    /// Return `true` when no residual content was found.
    pub fn is_clean(&self) -> bool {
        self.residuals.is_empty()
    }

    /// Whether this report enforces strict verification via [`Self::check`].
    pub const fn strict(&self) -> bool {
        self.strict
    }

    /// Enforce the verification policy.
    ///
    /// In strict mode, returns [`SecurityError::RedactionVerificationFailed`]
    /// describing the first residual page and byte offset. In permissive mode
    /// this always succeeds; residuals remain available via [`Self::residuals`].
    ///
    /// # Errors
    ///
    /// See above.
    pub fn check(&self) -> Result<()> {
        if self.strict
            && let Some(first) = self.residuals.first()
        {
            return Err(SecurityError::RedactionVerificationFailed {
                page: first.page().get(),
                offset: first.offset(),
            });
        }
        Ok(())
    }
}

fn push_residuals(
    out: &mut Vec<ResidualOccurrence>,
    page: PageNumber,
    text: &str,
    pattern: &TextPattern,
    source: ResidualSource,
) {
    for offset in find_occurrences(text, pattern.literal(), pattern.is_case_sensitive()) {
        out.push(ResidualOccurrence::new(page, offset, source.clone()));
    }
}

/// Find non-overlapping occurrences of `needle`, returning byte offsets into
/// `haystack`. Case-insensitive matching lowers both sides char-by-char while
/// retaining the original byte offset of each lowercased haystack character, so
/// reported offsets remain valid even when Unicode case folding changes length.
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

    fn text_map(entries: &[(u32, &str)]) -> BTreeMap<PageNumber, String> {
        entries
            .iter()
            .map(|(number, text)| (page(*number), (*text).to_owned()))
            .collect()
    }

    fn bounds(width: f64, height: f64) -> BTreeMap<PageNumber, PageBounds> {
        [(
            page(1),
            PageBounds::new(width, height).expect("valid test bounds"),
        )]
        .into_iter()
        .collect()
    }

    #[test]
    fn rect_validation_rejects_empty_and_non_finite_coordinates() {
        assert_eq!(
            RedactionRect::new(1.0, 1.0, 1.0, 4.0),
            Err(SecurityError::EmptyRedactionRegion)
        );
        assert_eq!(
            RedactionRect::new(1.0, 4.0, 0.0, 1.0),
            Err(SecurityError::EmptyRedactionRegion)
        );
        assert_eq!(
            RedactionRect::new(f64::NAN, 0.0, 1.0, 1.0),
            Err(SecurityError::NonFiniteCoordinate)
        );
        assert_eq!(
            RedactionRect::new(0.0, 0.0, f64::INFINITY, 1.0),
            Err(SecurityError::NonFiniteCoordinate)
        );
        let good = rect(2.0, 3.0, 8.0, 9.0);
        assert_eq!((good.width(), good.height()), (6.0, 6.0));
        assert!(good.contains_point(2.0, 3.0));
        assert!(!good.contains_point(9.0, 9.0));
    }

    #[test]
    fn rect_equality_is_a_total_order_for_signed_zero() {
        let positive = rect(0.0, 0.0, 1.0, 1.0);
        let negative = rect(-0.0, 0.0, 1.0, 1.0);
        assert_ne!(positive, negative);
        assert!(negative < positive);
    }

    #[test]
    fn page_bounds_and_page_number_are_validated() {
        assert_eq!(PageNumber::new(0), Err(SecurityError::InvalidPageNumber));
        assert_eq!(page(3).get(), 3);
        assert_eq!(
            PageBounds::new(0.0, 10.0),
            Err(SecurityError::InvalidPageBounds)
        );
        assert_eq!(
            PageBounds::new(f64::NAN, 10.0),
            Err(SecurityError::InvalidPageBounds)
        );
        let bounds = PageBounds::new(612.0, 792.0).expect("valid bounds");
        assert_eq!((bounds.width(), bounds.height()), (612.0, 792.0));
    }

    #[test]
    fn text_pattern_rejects_empty_and_preserves_case_policy() {
        assert_eq!(
            TextPattern::new("   ", true),
            Err(SecurityError::EmptyRedactionPattern)
        );
        let sensitive = TextPattern::case_sensitive("Secret").expect("valid pattern");
        assert_eq!(sensitive.literal(), "Secret");
        assert!(sensitive.is_case_sensitive());
        let insensitive = TextPattern::case_insensitive("Secret").expect("valid pattern");
        assert!(!insensitive.is_case_sensitive());
    }

    #[test]
    fn plan_validation_enforces_page_bounds_and_known_pages() {
        let inside =
            RedactionPlan::new([RedactionRegion::rect(page(1), rect(10.0, 10.0, 20.0, 20.0))]);
        assert!(inside.validate(&bounds(100.0, 100.0)).is_ok());

        let outside = RedactionPlan::new([RedactionRegion::rect(
            page(1),
            rect(90.0, 90.0, 110.0, 110.0),
        )]);
        assert_eq!(
            outside.validate(&bounds(100.0, 100.0)),
            Err(SecurityError::RedactionRegionOutOfBounds { page: 1 })
        );

        let negative =
            RedactionPlan::new([RedactionRegion::rect(page(1), rect(-1.0, 1.0, 2.0, 2.0))]);
        assert_eq!(
            negative.validate(&bounds(100.0, 100.0)),
            Err(SecurityError::RedactionRegionOutOfBounds { page: 1 })
        );

        let unknown =
            RedactionPlan::new([RedactionRegion::rect(page(7), rect(1.0, 1.0, 2.0, 2.0))]);
        assert_eq!(
            unknown.validate(&bounds(100.0, 100.0)),
            Err(SecurityError::UnknownPage { page: 7 })
        );

        let text_only = RedactionPlan::new([RedactionRegion::text(
            page(9),
            TextPattern::case_sensitive("x").expect("valid pattern"),
        )]);
        assert!(text_only.validate(&bounds(100.0, 100.0)).is_ok());
    }

    #[test]
    fn plan_orders_regions_by_page_then_kind_then_contents() {
        let pattern_a = TextPattern::case_sensitive("alpha").expect("valid pattern");
        let pattern_b = TextPattern::case_sensitive("beta").expect("valid pattern");
        let mut plan = RedactionPlan::empty();
        plan.add(RedactionRegion::text(page(2), pattern_b));
        plan.add(RedactionRegion::rect(page(1), rect(5.0, 5.0, 9.0, 9.0)));
        plan.add(RedactionRegion::text(page(1), pattern_a));
        plan.add(RedactionRegion::rect(page(1), rect(1.0, 1.0, 4.0, 4.0)));

        let pages: Vec<u32> = plan
            .regions()
            .iter()
            .map(|region| region.page().get())
            .collect();
        assert_eq!(pages, vec![1, 1, 1, 2]);
        assert!(plan.regions()[0].as_rect().is_some());
        assert!(plan.regions()[1].as_rect().is_some());
        assert!(plan.regions()[2].as_pattern().is_some());
        assert!(plan.regions()[0].as_rect() < plan.regions()[1].as_rect());
        assert_eq!(
            plan.regions()[3].as_pattern().map(TextPattern::literal),
            Some("beta")
        );
    }

    #[test]
    fn pattern_redaction_coverage_detects_every_occurrence() {
        let pattern = TextPattern::case_sensitive("SECRET").expect("valid pattern");
        let plan = RedactionPlan::new([RedactionRegion::text(page(1), pattern)]);
        let report =
            RedactionReport::verify(&plan, &text_map(&[(1, "SECRET here, SECRET there")]), false)
                .expect("verification succeeds");
        let offsets: Vec<usize> = report
            .residuals()
            .iter()
            .map(ResidualOccurrence::offset)
            .collect();
        assert_eq!(offsets, vec![0, 13]);
        assert!(report.residuals().iter().all(|hit| hit.page().get() == 1));
    }

    #[test]
    fn residual_detection_strict_mode_fails() {
        let pattern = TextPattern::case_sensitive("token").expect("valid pattern");
        let plan = RedactionPlan::new([RedactionRegion::text(page(1), pattern)]);
        let report = RedactionReport::verify(&plan, &text_map(&[(1, "prefix token suffix")]), true)
            .expect("verification produces a report");
        assert!(!report.is_clean());
        assert!(report.strict());
        assert_eq!(
            report.check(),
            Err(SecurityError::RedactionVerificationFailed { page: 1, offset: 7 })
        );
    }

    #[test]
    fn residual_detection_permissive_mode_reports_without_failing() {
        let pattern = TextPattern::case_sensitive("token").expect("valid pattern");
        let plan = RedactionPlan::new([RedactionRegion::text(page(1), pattern)]);
        let report =
            RedactionReport::verify(&plan, &text_map(&[(1, "prefix token suffix")]), false)
                .expect("verification produces a report");
        assert!(!report.is_clean());
        assert!(!report.strict());
        assert!(report.check().is_ok());
        assert_eq!(report.residuals().len(), 1);
    }

    #[test]
    fn multi_page_ordering_of_residuals_is_page_major() {
        let pattern = TextPattern::case_sensitive("hit").expect("valid pattern");
        let plan = RedactionPlan::new([
            RedactionRegion::text(page(2), pattern.clone()),
            RedactionRegion::text(page(1), pattern),
        ]);
        let report =
            RedactionReport::verify(&plan, &text_map(&[(2, "hit hit"), (1, "xx hit")]), true)
                .expect("verification succeeds");
        let observed: Vec<(u32, usize)> = report
            .residuals()
            .iter()
            .map(|hit| (hit.page().get(), hit.offset()))
            .collect();
        assert_eq!(observed, vec![(1, 3), (2, 0), (2, 4)]);
    }

    #[test]
    fn case_insensitive_matching_reports_original_byte_offsets() {
        let pattern = TextPattern::case_insensitive("SECRET").expect("valid pattern");
        let plan = RedactionPlan::new([RedactionRegion::text(page(1), pattern)]);
        let report =
            RedactionReport::verify(&plan, &text_map(&[(1, "Nothing; sEcReT; secret")]), false)
                .expect("verification succeeds");
        let offsets: Vec<usize> = report
            .residuals()
            .iter()
            .map(ResidualOccurrence::offset)
            .collect();
        assert_eq!(offsets, vec![9, 17]);
    }

    #[test]
    fn case_insensitive_offsets_survive_unicode_case_expansion() {
        // U+0130 lowercases into two chars (i + combining dot), so the lowered
        // stream is longer than the original; offsets must still point at the
        // original byte position (1) of the capital I-with-dot.
        let pattern = TextPattern::case_insensitive("i").expect("valid pattern");
        let plan = RedactionPlan::new([RedactionRegion::text(page(1), pattern)]);
        let report = RedactionReport::verify(&plan, &text_map(&[(1, "a\u{0130}b")]), false)
            .expect("verification succeeds");
        let offsets: Vec<usize> = report
            .residuals()
            .iter()
            .map(ResidualOccurrence::offset)
            .collect();
        assert_eq!(offsets, vec![1]);
    }

    #[test]
    fn rectangle_removed_text_is_verified_and_label_source_is_reported() {
        let plan = RedactionPlan::new([RedactionRegion::rect_with_text(
            page(1),
            rect(0.0, 0.0, 10.0, 10.0),
            "CONFIDENTIAL",
        )]);
        let clean = RedactionReport::verify(&plan, &text_map(&[(1, "public information")]), true)
            .expect("verification succeeds");
        assert!(clean.is_clean());
        assert!(clean.check().is_ok());

        let dirty =
            RedactionReport::verify(&plan, &text_map(&[(1, "see CONFIDENTIAL here")]), true)
                .expect("verification succeeds");
        assert_eq!(dirty.residuals().len(), 1);
        assert_eq!(
            dirty.residuals()[0].source(),
            &ResidualSource::RectangleLabel
        );
        assert!(dirty.check().is_err());
    }

    #[test]
    fn empty_plan_verifies_clean_in_both_modes() {
        let plan = RedactionPlan::empty();
        assert!(plan.is_empty());
        assert_eq!(plan.len(), 0);
        assert!(plan.regions().is_empty());
        for strict in [true, false] {
            let report = RedactionReport::verify(&plan, &text_map(&[(1, "anything")]), strict)
                .expect("verification succeeds");
            assert!(report.is_clean());
            assert!(report.check().is_ok());
        }
    }

    #[test]
    fn plan_is_deterministic_regardless_of_insertion_order() {
        let make = |reverse: bool| {
            let mut regions = vec![
                RedactionRegion::rect(page(3), rect(1.0, 1.0, 2.0, 2.0)),
                RedactionRegion::text(
                    page(1),
                    TextPattern::case_insensitive("Z").expect("valid pattern"),
                ),
                RedactionRegion::text(
                    page(1),
                    TextPattern::case_sensitive("a").expect("valid pattern"),
                ),
                RedactionRegion::rect(page(2), rect(3.0, 3.0, 4.0, 4.0)),
            ];
            if reverse {
                regions.reverse();
            }
            RedactionPlan::new(regions)
        };
        assert_eq!(make(false), make(true));
        assert_eq!(make(false).regions(), make(true).regions());
    }

    #[test]
    fn residual_source_distinguishes_patterns_from_rectangle_labels() {
        let pattern = TextPattern::case_sensitive("x").expect("valid pattern");
        let plan = RedactionPlan::new([
            RedactionRegion::text(page(1), pattern),
            RedactionRegion::rect_with_text(page(1), rect(0.0, 0.0, 5.0, 5.0), "x"),
        ]);
        let report = RedactionReport::verify(&plan, &text_map(&[(1, "x")]), false)
            .expect("verification succeeds");
        assert_eq!(report.residuals().len(), 2);
        assert!(
            matches!(report.residuals()[0].source(), ResidualSource::Pattern(_)),
            "Pattern is the first enum variant and sorts first"
        );
        assert_eq!(
            report.residuals()[1].source(),
            &ResidualSource::RectangleLabel
        );
    }
}
