//! Window-free full-screen presentation model.
//!
//! This module is the pure state machine behind presentation mode. It knows
//! nothing about `egui`, native windows, timers, or audio: the host supplies
//! elapsed seconds through [`PresentationModel::tick`] and translates the
//! returned [`PresentationOutcome`] into screen/sink changes. That keeps every
//! transition, auto-advance decision, and full-screen layout computation
//! deterministic and testable without a GUI.
//!
//! # Document page order
//!
//! A presentation plays a *sequence* of slides. By default
//! [`PresentationModel::new`] builds one slide per document page in ascending
//! page order. A host may supply a subsequence through
//! [`PresentationModel::from_slides`], but slides must still be in strictly
//! increasing document page order (pages may be skipped, never reordered), so
//! the presentation always honours the document's own page order.
//!
//! # Auto-advance timing
//!
//! Each slide carries a duration in seconds. A duration of `0.0` means the
//! slide is manual: auto-advance never leaves it on its own. There are no
//! threads and no sleeps — the caller feeds elapsed time and the model carries
//! the remainder across slide boundaries, so the same sequence of ticks always
//! produces the same outcome.

use crate::canvas::{CanvasRect, CanvasSize, PageSize};

/// Tolerance, in seconds, used when comparing accumulated time against a slide
/// duration so floating-point drift cannot make an exactly-timed advance flaky.
const TIME_EPSILON: f32 = 1e-4;

/// The visual transition used when entering a slide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Transition {
    /// No transition; the new slide appears immediately.
    #[default]
    None,
    /// A cross-fade from the previous slide.
    Fade,
    /// A directional push that slides the previous slide out.
    Push,
}

impl Transition {
    /// A stable, human-readable label for menus and tooltips.
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Fade => "Fade",
            Self::Push => "Push",
        }
    }
}

impl std::fmt::Display for Transition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Typed errors produced while constructing a presentation model.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PresentationError {
    /// The document has no pages, so a presentation cannot be built.
    #[error("presentation requires at least one page")]
    EmptyDocument,
    /// A slide has a duration that is not finite and non-negative.
    #[error("invalid duration {1} for page {0}: must be finite and non-negative")]
    InvalidDuration(usize, f32),
    /// Slides are not in strictly increasing document page order.
    #[error("presentation slides must follow document page order")]
    SequenceOutOfOrder,
}

/// One slide in a presentation.
///
/// `page` is a zero-based document page index, `duration` is seconds until the
/// auto-advance timer moves on (`0.0` means manual), and `transition` describes
/// how the slide enters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Slide {
    /// Zero-based document page index shown by this slide.
    pub page: usize,
    /// Seconds before auto-advance; `0.0` keeps the slide manual.
    pub duration: f32,
    /// Transition applied when entering this slide.
    pub transition: Transition,
}

impl Slide {
    /// Build a slide, rejecting a non-finite or negative `duration`.
    pub fn new(
        page: usize,
        duration: f32,
        transition: Transition,
    ) -> Result<Self, PresentationError> {
        if !duration.is_finite() || duration < 0.0 {
            return Err(PresentationError::InvalidDuration(page, duration));
        }
        Ok(Self {
            page,
            duration,
            transition,
        })
    }

    /// Build a manual slide (`duration == 0.0`, no transition).
    pub fn manual(page: usize) -> Self {
        Self {
            page,
            duration: 0.0,
            transition: Transition::None,
        }
    }
}

/// State of the auto-advance state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AdvanceState {
    /// Auto-advance is off; only manual commands change slides.
    #[default]
    Stopped,
    /// Auto-advance is running and accumulating elapsed time.
    Playing,
    /// Auto-advance reached the end of the sequence.
    Finished,
    /// The presentation was exited.
    Exited,
}

/// What a [`PresentationModel`] operation did, for the host to act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationOutcome {
    /// Nothing changed.
    Nothing,
    /// The active slide index changed to the enclosed value.
    PageChanged(usize),
    /// Auto-advance started.
    AutoAdvanceStarted,
    /// Auto-advance was stopped before reaching the end.
    AutoAdvanceStopped,
    /// Auto-advance reached the final slide.
    Finished,
    /// The presentation was exited.
    Exited,
}

/// A keyboard path for a presentation command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationKey {
    /// The Space bar (also Enter / Page Down behaviour).
    Space,
    /// The right arrow key.
    ArrowRight,
    /// The left arrow key.
    ArrowLeft,
    /// The `Home` key.
    Home,
    /// The `End` key.
    End,
    /// The Escape key.
    Escape,
    /// The `A` key (toggle auto-advance).
    A,
}

/// Modifier keys held with a [`PresentationKey`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PresentationModifiers {
    /// Control is held.
    pub ctrl: bool,
    /// Shift is held.
    pub shift: bool,
    /// Alt is held.
    pub alt: bool,
}

/// A high-level presentation command with a keyboard and gesture path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationCommand {
    /// Go to the next slide.
    Next,
    /// Go to the previous slide.
    Previous,
    /// Go to the first slide.
    First,
    /// Go to the last slide.
    Last,
    /// Toggle auto-advance on/off.
    ToggleAutoAdvance,
    /// Exit presentation mode.
    Exit,
}

impl PresentationCommand {
    /// A stable shortcut label for menus and tooltips.
    pub fn shortcut_label(self) -> &'static str {
        match self {
            Self::Next => "Space / Right",
            Self::Previous => "Left",
            Self::First => "Home",
            Self::Last => "End",
            Self::ToggleAutoAdvance => "A",
            Self::Exit => "Esc",
        }
    }
}

/// A toolkit-independent swipe gesture for presentation navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationGesture {
    /// Swipe left: advance to the next slide.
    SwipeLeft,
    /// Swipe right: return to the previous slide.
    SwipeRight,
    /// A long press: toggle auto-advance.
    LongPress,
}

/// Resolve a presentation keyboard shortcut.
///
/// Function-style navigation keys ignore Ctrl and Shift but reject Alt. The
/// `A` key toggles auto-advance only with no modifiers. Unrecognised
/// combinations return `None` so the host can leave them to focused widgets.
pub fn map_presentation_key(
    key: PresentationKey,
    modifiers: PresentationModifiers,
) -> Option<PresentationCommand> {
    let PresentationModifiers { ctrl, shift, alt } = modifiers;
    if alt {
        return None;
    }
    match key {
        PresentationKey::A if !ctrl && !shift => Some(PresentationCommand::ToggleAutoAdvance),
        PresentationKey::A => None,
        PresentationKey::Space | PresentationKey::ArrowRight if !ctrl => {
            Some(PresentationCommand::Next)
        }
        PresentationKey::ArrowLeft if !ctrl => Some(PresentationCommand::Previous),
        PresentationKey::Home if !ctrl => Some(PresentationCommand::First),
        PresentationKey::End if !ctrl => Some(PresentationCommand::Last),
        PresentationKey::Escape if !ctrl && !shift => Some(PresentationCommand::Exit),
        _ => None,
    }
}

/// Resolve a presentation swipe gesture.
pub fn map_presentation_gesture(gesture: PresentationGesture) -> PresentationCommand {
    match gesture {
        PresentationGesture::SwipeLeft => PresentationCommand::Next,
        PresentationGesture::SwipeRight => PresentationCommand::Previous,
        PresentationGesture::LongPress => PresentationCommand::ToggleAutoAdvance,
    }
}

/// How a single page is placed inside the full-screen viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FitMode {
    /// Scale the page uniformly so it fits entirely inside the viewport
    /// (letterboxing where the aspect ratios differ), then centre it.
    Fit,
    /// Draw the page at 1:1 scale (1 point = 1 screen unit), centred.
    Actual,
}

/// The deterministic result of a full-screen layout calculation.
///
/// Coordinates are in the same units as the supplied viewport; the origin is
/// the viewport's top-left corner, `x` increases right and `y` increases down.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FullscreenLayout {
    /// Left edge of the placed page.
    pub x: f32,
    /// Top edge of the placed page.
    pub y: f32,
    /// Displayed page width.
    pub width: f32,
    /// Displayed page height.
    pub height: f32,
    /// Scale from PDF points to screen units.
    pub scale: f32,
}

impl FullscreenLayout {
    /// Centre a rect of size `width` x `height` inside `viewport`.
    fn centered(viewport: CanvasSize, width: f32, height: f32, scale: f32) -> Self {
        Self {
            x: (viewport.width - width) * 0.5,
            y: (viewport.height - height) * 0.5,
            width,
            height,
            scale,
        }
    }
}

/// A window-free presentation state machine.
///
/// The model owns its slides, the active slide index, and the auto-advance
/// clock. All mutations are driven by caller-supplied commands and elapsed
/// time; no I/O, threads, or platform APIs are involved.
#[derive(Debug, Clone, PartialEq)]
pub struct PresentationModel {
    slides: Vec<Slide>,
    current: usize,
    state: AdvanceState,
    elapsed: f32,
}

impl PresentationModel {
    /// Build a presentation that shows every document page in order, each as a
    /// manual slide with no transition.
    ///
    /// Returns [`PresentationError::EmptyDocument`] when `page_count` is zero.
    pub fn new(page_count: usize) -> Result<Self, PresentationError> {
        if page_count == 0 {
            return Err(PresentationError::EmptyDocument);
        }
        let slides = (0..page_count).map(Slide::manual).collect();
        Ok(Self {
            slides,
            current: 0,
            state: AdvanceState::Stopped,
            elapsed: 0.0,
        })
    }

    /// Build a presentation from an explicit slide list.
    ///
    /// The list must be non-empty and its pages must be strictly increasing so
    /// document page order is preserved; each slide's duration is validated by
    /// [`Slide::new`].
    pub fn from_slides(slides: Vec<Slide>) -> Result<Self, PresentationError> {
        if slides.is_empty() {
            return Err(PresentationError::EmptyDocument);
        }
        // `Slide` has public fields, so a host can bypass `Slide::new` with a
        // struct literal. Re-validate every duration here: a NaN duration in
        // particular would make the `tick` comparisons below always false and
        // corrupt the deterministic remainder carry into a runaway advance.
        for slide in &slides {
            if !slide.duration.is_finite() || slide.duration < 0.0 {
                return Err(PresentationError::InvalidDuration(
                    slide.page,
                    slide.duration,
                ));
            }
        }
        if slides.windows(2).any(|pair| pair[0].page >= pair[1].page) {
            return Err(PresentationError::SequenceOutOfOrder);
        }
        Ok(Self {
            slides,
            current: 0,
            state: AdvanceState::Stopped,
            elapsed: 0.0,
        })
    }

    /// All slides, in presentation order.
    pub fn slides(&self) -> &[Slide] {
        &self.slides
    }

    /// Number of slides in the presentation (always at least one).
    pub fn len(&self) -> usize {
        self.slides.len()
    }

    /// Always `false` for a successfully built presentation.
    pub fn is_empty(&self) -> bool {
        self.slides.is_empty()
    }

    /// Zero-based index of the active slide.
    pub fn current_index(&self) -> usize {
        self.current
    }

    /// The document page shown by the active slide.
    pub fn current_page(&self) -> usize {
        // Invariant: `current` is always a valid index into the non-empty
        // `slides` list, maintained by every constructor and mutation below.
        self.slides
            .get(self.current)
            .map(|slide| slide.page)
            .unwrap_or(0)
    }

    /// The active slide.
    pub fn current_slide(&self) -> Option<Slide> {
        self.slides.get(self.current).copied()
    }

    /// Current auto-advance state.
    pub fn state(&self) -> AdvanceState {
        self.state
    }

    /// Seconds accumulated toward the active slide's duration.
    pub fn elapsed(&self) -> f32 {
        self.elapsed
    }

    /// Advance the auto-advance clock by a caller-supplied `elapsed` seconds.
    ///
    /// Non-finite or non-positive inputs are ignored. When the accumulated time
    /// reaches the active slide's (positive) duration, the remainder carries
    /// over to the next slide, so a large tick may cross several slides in one
    /// call. A slide with duration `0.0` is manual and stops auto-advance there.
    /// Reaching the end sets [`AdvanceState::Finished`].
    pub fn tick(&mut self, elapsed: f32) -> PresentationOutcome {
        if self.state != AdvanceState::Playing || !elapsed.is_finite() || elapsed <= 0.0 {
            return PresentationOutcome::Nothing;
        }
        self.elapsed += elapsed;
        let mut moved = false;
        loop {
            // Invariant: `current` is always in bounds; `.get` keeps this total
            // even if a host mutates internals through a future API.
            let duration = match self.slides.get(self.current) {
                Some(slide) => slide.duration,
                None => return PresentationOutcome::Nothing,
            };
            if duration <= 0.0 {
                self.elapsed = 0.0;
                break;
            }
            if self.elapsed + TIME_EPSILON < duration {
                break;
            }
            self.elapsed -= duration;
            if self.current + 1 < self.slides.len() {
                self.current += 1;
                moved = true;
            } else {
                self.state = AdvanceState::Finished;
                self.elapsed = 0.0;
                return PresentationOutcome::Finished;
            }
        }
        if moved {
            PresentationOutcome::PageChanged(self.current)
        } else {
            PresentationOutcome::Nothing
        }
    }

    /// Apply a presentation command (from a keyboard, gesture, or UI action).
    ///
    /// Manual navigation resets the auto-advance timer for the new slide but
    /// leaves a running auto-advance playing. Navigating away from a finished
    /// presentation returns it to [`AdvanceState::Stopped`]. Commands issued
    /// after exit are no-ops.
    pub fn command(&mut self, command: PresentationCommand) -> PresentationOutcome {
        if self.state == AdvanceState::Exited && command != PresentationCommand::Exit {
            return PresentationOutcome::Nothing;
        }
        match command {
            PresentationCommand::Next => self.go_to(self.current.saturating_add(1)),
            PresentationCommand::Previous => self.go_to(self.current.saturating_sub(1)),
            PresentationCommand::First => self.go_to(0),
            PresentationCommand::Last => self.go_to(self.slides.len().saturating_sub(1)),
            PresentationCommand::Exit => {
                self.state = AdvanceState::Exited;
                self.elapsed = 0.0;
                PresentationOutcome::Exited
            }
            PresentationCommand::ToggleAutoAdvance => self.toggle_auto_advance(),
        }
    }

    fn go_to(&mut self, target: usize) -> PresentationOutcome {
        if target >= self.slides.len() || target == self.current {
            return PresentationOutcome::Nothing;
        }
        self.current = target;
        self.elapsed = 0.0;
        if self.state == AdvanceState::Finished {
            self.state = AdvanceState::Stopped;
        }
        PresentationOutcome::PageChanged(self.current)
    }

    fn toggle_auto_advance(&mut self) -> PresentationOutcome {
        match self.state {
            AdvanceState::Playing => {
                self.state = AdvanceState::Stopped;
                self.elapsed = 0.0;
                PresentationOutcome::AutoAdvanceStopped
            }
            AdvanceState::Stopped => {
                self.state = AdvanceState::Playing;
                self.elapsed = 0.0;
                PresentationOutcome::AutoAdvanceStarted
            }
            AdvanceState::Finished => {
                // Restarting from the end begins again at the first slide.
                self.current = 0;
                self.state = AdvanceState::Playing;
                self.elapsed = 0.0;
                PresentationOutcome::AutoAdvanceStarted
            }
            AdvanceState::Exited => PresentationOutcome::Nothing,
        }
    }

    /// Compute a deterministic full-screen layout for `page` inside `viewport`.
    ///
    /// Returns `None` when the viewport or page has non-positive, non-finite
    /// dimensions. [`FitMode::Fit`] uses the smaller of the width and height
    /// ratios (uniform scale, letterboxed and centred); [`FitMode::Actual`]
    /// places the page at 1:1, also centred.
    pub fn layout(
        &self,
        viewport: CanvasSize,
        page: PageSize,
        mode: FitMode,
    ) -> Option<FullscreenLayout> {
        let dims = [
            viewport.width,
            viewport.height,
            page.width_pt,
            page.height_pt,
        ];
        if !dims.iter().all(|value| value.is_finite())
            || viewport.width <= 0.0
            || viewport.height <= 0.0
            || page.width_pt <= 0.0
            || page.height_pt <= 0.0
        {
            return None;
        }
        match mode {
            FitMode::Fit => {
                let scale = viewport.width / page.width_pt;
                let scale = scale.min(viewport.height / page.height_pt);
                let width = page.width_pt * scale;
                let height = page.height_pt * scale;
                Some(FullscreenLayout::centered(viewport, width, height, scale))
            }
            FitMode::Actual => Some(FullscreenLayout::centered(
                viewport,
                page.width_pt,
                page.height_pt,
                1.0,
            )),
        }
    }

    /// The rectangle the page occupies in the viewport under `mode`, or `None`
    /// for invalid geometry. This is the placed rect form of [`Self::layout`].
    pub fn layout_rect(
        &self,
        viewport: CanvasSize,
        page: PageSize,
        mode: FitMode,
    ) -> Option<CanvasRect> {
        self.layout(viewport, page, mode)
            .map(|layout| CanvasRect::new(layout.x, layout.y, layout.width, layout.height))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewport(width: f32, height: f32) -> CanvasSize {
        CanvasSize::new(width, height)
    }

    fn page(width: f32, height: f32) -> PageSize {
        PageSize::new(width, height)
    }

    fn timed_slides() -> Vec<Slide> {
        vec![
            Slide::new(1, 2.0, Transition::Fade).expect("valid slide"),
            Slide::new(3, 3.0, Transition::Push).expect("valid slide"),
            Slide::new(7, 4.0, Transition::None).expect("valid slide"),
        ]
    }

    #[test]
    fn new_builds_document_order_and_rejects_empty_document() {
        let model = PresentationModel::new(4).expect("valid document");
        assert_eq!(model.len(), 4);
        assert!(!model.is_empty());
        let pages: Vec<usize> = model.slides().iter().map(|slide| slide.page).collect();
        assert_eq!(pages, vec![0, 1, 2, 3]);
        assert_eq!(model.current_page(), 0);
        assert_eq!(model.state(), AdvanceState::Stopped);
        assert_eq!(
            PresentationModel::new(0),
            Err(PresentationError::EmptyDocument)
        );
        assert_eq!(
            PresentationModel::from_slides(Vec::new()),
            Err(PresentationError::EmptyDocument)
        );
    }

    #[test]
    fn from_slides_honours_document_order_and_rejects_reordering() {
        let model = PresentationModel::from_slides(timed_slides()).expect("valid slides");
        let pages: Vec<usize> = model.slides().iter().map(|slide| slide.page).collect();
        assert_eq!(pages, vec![1, 3, 7]);

        let reordered = vec![Slide::manual(5), Slide::manual(2)];
        assert_eq!(
            PresentationModel::from_slides(reordered),
            Err(PresentationError::SequenceOutOfOrder)
        );
        let duplicated = vec![Slide::manual(2), Slide::manual(2)];
        assert_eq!(
            PresentationModel::from_slides(duplicated),
            Err(PresentationError::SequenceOutOfOrder)
        );
    }

    #[test]
    fn invalid_durations_are_rejected() {
        for bad in [f32::NAN, f32::INFINITY, -1.0] {
            match Slide::new(4, bad, Transition::None) {
                Err(PresentationError::InvalidDuration(page, reported)) => {
                    assert_eq!(page, 4);
                    if bad.is_nan() {
                        assert!(reported.is_nan());
                    } else {
                        assert_eq!(reported, bad);
                    }
                }
                other => panic!("expected InvalidDuration, got {other:?}"),
            }
        }
        let zero = Slide::new(0, 0.0, Transition::None).expect("zero is valid");
        assert_eq!(zero.duration, 0.0);
    }

    #[test]
    fn transition_and_command_labels_are_stable() {
        assert_eq!(Transition::None.to_string(), "None");
        assert_eq!(Transition::Fade.to_string(), "Fade");
        assert_eq!(Transition::Push.to_string(), "Push");
        assert!(!PresentationCommand::Exit.shortcut_label().is_empty());
        assert_eq!(PresentationCommand::First.shortcut_label(), "Home");
    }

    #[test]
    fn navigation_commands_walk_slides_and_clamp_at_boundaries() {
        let mut model = PresentationModel::new(3).expect("valid");
        assert_eq!(
            model.command(PresentationCommand::Next),
            PresentationOutcome::PageChanged(1)
        );
        assert_eq!(
            model.command(PresentationCommand::Last),
            PresentationOutcome::PageChanged(2)
        );
        assert_eq!(
            model.command(PresentationCommand::Next),
            PresentationOutcome::Nothing
        );
        assert_eq!(
            model.command(PresentationCommand::Previous),
            PresentationOutcome::PageChanged(1)
        );
        assert_eq!(
            model.command(PresentationCommand::First),
            PresentationOutcome::PageChanged(0)
        );
        assert_eq!(
            model.command(PresentationCommand::Previous),
            PresentationOutcome::Nothing
        );
    }

    #[test]
    fn exit_is_terminal_for_other_commands() {
        let mut model = PresentationModel::new(2).expect("valid");
        assert_eq!(
            model.command(PresentationCommand::Exit),
            PresentationOutcome::Exited
        );
        assert_eq!(model.state(), AdvanceState::Exited);
        assert_eq!(
            model.command(PresentationCommand::Next),
            PresentationOutcome::Nothing
        );
        assert_eq!(model.current_index(), 0);
    }

    #[test]
    fn keyboard_and_gesture_mapping_resolve_all_commands() {
        let none = PresentationModifiers::default();
        assert_eq!(
            map_presentation_key(PresentationKey::Space, none),
            Some(PresentationCommand::Next)
        );
        assert_eq!(
            map_presentation_key(PresentationKey::ArrowLeft, none),
            Some(PresentationCommand::Previous)
        );
        assert_eq!(
            map_presentation_key(PresentationKey::Home, none),
            Some(PresentationCommand::First)
        );
        assert_eq!(
            map_presentation_key(PresentationKey::End, none),
            Some(PresentationCommand::Last)
        );
        assert_eq!(
            map_presentation_key(PresentationKey::Escape, none),
            Some(PresentationCommand::Exit)
        );
        assert_eq!(
            map_presentation_key(PresentationKey::A, none),
            Some(PresentationCommand::ToggleAutoAdvance)
        );
        assert_eq!(
            map_presentation_key(
                PresentationKey::Space,
                PresentationModifiers {
                    alt: true,
                    ..PresentationModifiers::default()
                }
            ),
            None
        );
        assert_eq!(
            map_presentation_gesture(PresentationGesture::SwipeLeft),
            PresentationCommand::Next
        );
        assert_eq!(
            map_presentation_gesture(PresentationGesture::SwipeRight),
            PresentationCommand::Previous
        );
    }

    #[test]
    fn auto_advance_accumulates_and_carries_remainder_deterministically() {
        let mut a = PresentationModel::from_slides(timed_slides()).expect("valid");
        let mut b = PresentationModel::from_slides(timed_slides()).expect("valid");
        for model in [&mut a, &mut b] {
            assert_eq!(
                model.command(PresentationCommand::ToggleAutoAdvance),
                PresentationOutcome::AutoAdvanceStarted
            );
            assert_eq!(model.state(), AdvanceState::Playing);
            // Below the first duration: no change, time accumulates.
            assert_eq!(model.tick(1.5), PresentationOutcome::Nothing);
            assert!((model.elapsed() - 1.5).abs() < 1e-5);
            // Crossing 2.0 s leaves 0.5 s of remainder on slide 1.
            assert_eq!(model.tick(1.0), PresentationOutcome::PageChanged(1));
            assert_eq!(model.current_page(), 3);
            assert!((model.elapsed() - 0.5).abs() < 1e-5);
        }
        assert_eq!(a, b, "identical tick sequences must be identical");
    }

    #[test]
    fn a_large_tick_can_cross_multiple_slides_and_finishes() {
        let mut model = PresentationModel::from_slides(timed_slides()).expect("valid");
        model.command(PresentationCommand::ToggleAutoAdvance);
        // 2 + 3 + 4 = 9 seconds crosses all three slides in one call.
        assert_eq!(model.tick(9.2), PresentationOutcome::Finished);
        assert_eq!(model.state(), AdvanceState::Finished);
        assert_eq!(model.current_index(), 2);
    }

    #[test]
    fn manual_zero_duration_slide_stops_auto_advance() {
        let slides = vec![Slide::manual(0)];
        let mut model = PresentationModel::from_slides(slides).expect("valid");
        model.command(PresentationCommand::ToggleAutoAdvance);
        assert_eq!(model.tick(100.0), PresentationOutcome::Nothing);
        assert_eq!(model.state(), AdvanceState::Playing);
        assert_eq!(model.current_index(), 0);
    }

    #[test]
    fn invalid_or_negative_ticks_are_ignored() {
        let mut model = PresentationModel::from_slides(timed_slides()).expect("valid");
        model.command(PresentationCommand::ToggleAutoAdvance);
        assert_eq!(model.tick(f32::NAN), PresentationOutcome::Nothing);
        assert_eq!(model.tick(f32::INFINITY), PresentationOutcome::Nothing);
        assert_eq!(model.tick(-5.0), PresentationOutcome::Nothing);
        assert_eq!(model.elapsed(), 0.0);
        // Ticks while stopped are ignored too.
        model.command(PresentationCommand::ToggleAutoAdvance);
        assert_eq!(model.tick(3.0), PresentationOutcome::Nothing);
        assert_eq!(model.elapsed(), 0.0);
    }

    #[test]
    fn toggle_from_finished_restarts_at_first_slide() {
        let mut model = PresentationModel::from_slides(timed_slides()).expect("valid");
        model.command(PresentationCommand::ToggleAutoAdvance);
        assert_eq!(model.tick(9.0), PresentationOutcome::Finished);
        assert_eq!(
            model.command(PresentationCommand::ToggleAutoAdvance),
            PresentationOutcome::AutoAdvanceStarted
        );
        assert_eq!(model.current_index(), 0);
        assert_eq!(model.state(), AdvanceState::Playing);
    }

    #[test]
    fn layout_fit_and_actual_math_is_deterministic_and_centered() {
        let model = PresentationModel::new(1).expect("valid");
        let screen = viewport(1920.0, 1080.0);
        let letter = page(612.0, 792.0);
        let fit = model
            .layout(screen, letter, FitMode::Fit)
            .expect("valid geometry");
        // Height is the limiting dimension (1080/792 < 1920/612).
        let expected_scale = 1080.0 / 792.0;
        assert!((fit.scale - expected_scale).abs() < 1e-5);
        assert!((fit.height - 1080.0).abs() < 1e-4);
        assert!((fit.width - 612.0 * expected_scale).abs() < 1e-4);
        assert!((fit.x - (1920.0 - fit.width) * 0.5).abs() < 1e-4);
        assert!((fit.y).abs() < 1e-4);
        assert_eq!(
            fit,
            model.layout(screen, letter, FitMode::Fit).expect("valid")
        );

        let actual = model
            .layout(screen, letter, FitMode::Actual)
            .expect("valid geometry");
        assert_eq!(actual.scale, 1.0);
        assert_eq!(actual.width, 612.0);
        assert_eq!(actual.height, 792.0);
        assert!((actual.x - 654.0).abs() < 1e-4);
        assert!((actual.y - 144.0).abs() < 1e-4);
    }

    #[test]
    fn layout_rejects_invalid_geometry() {
        let model = PresentationModel::new(1).expect("valid");
        assert_eq!(
            model.layout(viewport(0.0, 100.0), page(10.0, 10.0), FitMode::Fit),
            None
        );
        assert_eq!(
            model.layout(
                viewport(100.0, 100.0),
                page(f32::NAN, 10.0),
                FitMode::Actual
            ),
            None
        );
        assert_eq!(
            model.layout(viewport(100.0, 100.0), page(10.0, -1.0), FitMode::Fit),
            None
        );
        assert!(
            model
                .layout_rect(viewport(800.0, 600.0), page(400.0, 300.0), FitMode::Fit)
                .is_some()
        );
    }

    #[test]
    fn from_slides_rejects_durations_that_bypass_slide_new() {
        // Slide fields are public, so a struct literal can carry a duration
        // that Slide::new would have rejected. from_slides must catch it.
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.5] {
            let slides = vec![
                Slide::manual(0),
                Slide {
                    page: 2,
                    duration: bad,
                    transition: Transition::Fade,
                },
            ];
            match PresentationModel::from_slides(slides) {
                Err(PresentationError::InvalidDuration(page, reported)) => {
                    assert_eq!(page, 2);
                    if bad.is_nan() {
                        assert!(reported.is_nan());
                    } else {
                        assert_eq!(reported, bad);
                    }
                }
                other => panic!("expected InvalidDuration for {bad}, got {other:?}"),
            }
        }
        // A finite struct-literal slide remains valid and keeps document order.
        let valid = vec![Slide {
            page: 4,
            duration: 1.25,
            transition: Transition::Push,
        }];
        let model = PresentationModel::from_slides(valid).expect("finite duration is valid");
        assert_eq!(model.current_page(), 4);
    }

    #[test]
    fn tick_boundary_tolerance_and_chunked_input_are_deterministic() {
        let mut chunked = PresentationModel::from_slides(timed_slides()).expect("valid");
        let mut single = PresentationModel::from_slides(timed_slides()).expect("valid");
        for model in [&mut chunked, &mut single] {
            assert_eq!(
                model.command(PresentationCommand::ToggleAutoAdvance),
                PresentationOutcome::AutoAdvanceStarted
            );
        }
        // Chunks that add up to each duration cross at the same points as one
        // large tick per slide, with the same remainder and final state.
        let steps = [
            (1.0, PresentationOutcome::Nothing),
            (1.0, PresentationOutcome::PageChanged(1)),
            (2.5, PresentationOutcome::Nothing),
            (0.5, PresentationOutcome::PageChanged(2)),
        ];
        for (elapsed, expected) in steps {
            assert_eq!(chunked.tick(elapsed), expected);
        }
        assert_eq!(single.tick(2.0), PresentationOutcome::PageChanged(1));
        assert_eq!(single.tick(3.0), PresentationOutcome::PageChanged(2));
        assert_eq!(chunked.current_index(), single.current_index());
        assert_eq!(chunked.elapsed(), single.elapsed());
        assert_eq!(chunked.state(), single.state());
        assert_eq!(chunked, single);

        // A tick just under a duration still advances inside TIME_EPSILON,
        // which keeps exactly-timed presentation steps from being flaky.
        let mut tolerant = PresentationModel::from_slides(vec![
            Slide::new(0, 2.0, Transition::None).expect("valid"),
        ])
        .expect("valid");
        tolerant.command(PresentationCommand::ToggleAutoAdvance);
        assert_eq!(
            tolerant.tick(2.0 - TIME_EPSILON * 0.5),
            PresentationOutcome::Finished
        );
    }

    #[test]
    fn manual_navigation_while_playing_resets_elapsed_and_keeps_auto_advance() {
        let mut model = PresentationModel::from_slides(timed_slides()).expect("valid");
        model.command(PresentationCommand::ToggleAutoAdvance);
        assert_eq!(model.tick(1.0), PresentationOutcome::Nothing);
        assert!(model.elapsed() > 0.0);
        assert_eq!(
            model.command(PresentationCommand::Next),
            PresentationOutcome::PageChanged(1)
        );
        assert_eq!(model.elapsed(), 0.0);
        assert_eq!(model.state(), AdvanceState::Playing);
        // The newly selected slide's own duration governs the next tick.
        assert_eq!(model.tick(2.9), PresentationOutcome::Nothing);
        assert_eq!(model.tick(0.1), PresentationOutcome::PageChanged(2));
        assert_eq!(
            model.command(PresentationCommand::Previous),
            PresentationOutcome::PageChanged(1)
        );
        assert_eq!(model.elapsed(), 0.0);
        assert_eq!(model.state(), AdvanceState::Playing);
    }

    #[test]
    fn exit_from_finished_and_repeated_exit_are_terminal() {
        let mut model = PresentationModel::from_slides(timed_slides()).expect("valid");
        model.command(PresentationCommand::ToggleAutoAdvance);
        assert_eq!(model.tick(9.0), PresentationOutcome::Finished);
        assert_eq!(
            model.command(PresentationCommand::Exit),
            PresentationOutcome::Exited
        );
        assert_eq!(
            model.command(PresentationCommand::Exit),
            PresentationOutcome::Exited
        );
        assert_eq!(
            model.command(PresentationCommand::ToggleAutoAdvance),
            PresentationOutcome::Nothing
        );
        assert_eq!(model.tick(1.0), PresentationOutcome::Nothing);
        assert_eq!(model.state(), AdvanceState::Exited);
    }

    #[test]
    fn layout_fit_uses_width_limit_for_wide_pages() {
        let model = PresentationModel::new(1).expect("valid");
        let screen = viewport(800.0, 800.0);
        let wide = page(1000.0, 200.0);
        let fit = model
            .layout(screen, wide, FitMode::Fit)
            .expect("valid geometry");
        // Width is limiting here (800/1000 < 800/200).
        assert_eq!(fit.scale, 0.8);
        assert_eq!(fit.width, 800.0);
        assert_eq!(fit.height, 160.0);
        assert_eq!(fit.x, 0.0);
        assert_eq!(fit.y, 320.0);
        let rect = model
            .layout_rect(screen, wide, FitMode::Fit)
            .expect("valid geometry");
        assert_eq!(rect, CanvasRect::new(fit.x, fit.y, fit.width, fit.height));
    }
}
