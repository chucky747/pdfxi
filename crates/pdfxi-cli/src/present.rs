//! `pdfxi present`: window-free presentation-mode planning and stepping.
//!
//! [`pdfxi_app::presentation`] is the pure presentation state machine: slides in
//! document page order, per-slide durations, transitions, keyboard/gesture
//! mapping and full-screen layout. This host drives that model against a real
//! document and reports every step. It never opens a window; the GUI shell only
//! has to display the reported slide and layout.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_app::canvas::{CanvasSize, PageSize};
use pdfxi_app::presentation::{
    AdvanceState, FitMode, FullscreenLayout, PresentationCommand, PresentationGesture,
    PresentationKey, PresentationModel, PresentationModifiers, PresentationOutcome, Slide,
    Transition, map_presentation_gesture, map_presentation_key,
};
use pdfxi_core::Document;
use serde::Serialize;

/// JSON shape of one presentation step.
#[derive(Debug, Serialize)]
struct StepJson {
    /// Input token.
    command: String,
    /// Stable outcome label.
    outcome: String,
    /// Active zero-based slide index.
    index: usize,
    /// Active one-based document page.
    page: usize,
    /// Auto-advance state.
    state: String,
    /// Current full-screen layout, when geometry is valid.
    layout: Option<LayoutJson>,
}

/// JSON shape of a full-screen layout.
#[derive(Debug, Serialize)]
struct LayoutJson {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    scale: f32,
}

/// JSON shape of a slide.
#[derive(Debug, Serialize)]
struct SlideJson {
    /// One-based document page.
    page: usize,
    /// Auto-advance seconds (`0` means manual).
    duration: f32,
    /// Transition label.
    transition: String,
}

/// JSON shape of `pdfxi present`.
#[derive(Debug, Serialize)]
struct PresentJson {
    /// Source PDF.
    input: String,
    /// Document pages.
    pages: usize,
    /// Screen width.
    screen_width: f32,
    /// Screen height.
    screen_height: f32,
    /// Fit mode.
    fit: String,
    /// Slides in presentation order.
    slides: Vec<SlideJson>,
    /// Executed steps.
    steps: Vec<StepJson>,
    /// Final index/page/state.
    final_index: usize,
    final_page: usize,
    final_state: String,
    /// Final layout.
    final_layout: Option<LayoutJson>,
    /// Whether the presentation was exited.
    exited: bool,
}

/// Parse `WxH`.
fn parse_screen(text: &str) -> anyhow::Result<(f32, f32)> {
    let (width, height) = text
        .split_once(['x', 'X', ','])
        .with_context(|| format!("--screen {text:?} needs WxH"))?;
    let width = width
        .trim()
        .parse::<f32>()
        .with_context(|| format!("--screen width {width:?} is not a number"))?;
    let height = height
        .trim()
        .parse::<f32>()
        .with_context(|| format!("--screen height {height:?} is not a number"))?;
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        bail!("--screen needs finite positive dimensions");
    }
    Ok((width, height))
}

/// Parse a transition name.
fn parse_transition(text: &str) -> anyhow::Result<Transition> {
    match text.trim().to_ascii_lowercase().as_str() {
        "none" | "cut" => Ok(Transition::None),
        "fade" => Ok(Transition::Fade),
        "push" => Ok(Transition::Push),
        other => bail!("unknown transition {other:?}; expected none, fade or push"),
    }
}

/// Parse a comma-separated page list into zero-based page indices.
fn parse_pages(text: Option<&str>, page_count: usize) -> anyhow::Result<Vec<usize>> {
    let Some(text) = text else {
        return Ok((0..page_count).collect());
    };
    let mut pages = Vec::new();
    for part in text
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let page = part
            .parse::<usize>()
            .with_context(|| format!("--slides value {part:?} is not a page number"))?;
        if page == 0 || page > page_count {
            bail!("--slides page {page} is outside 1..={page_count}");
        }
        let index = page - 1;
        if pages.last().is_some_and(|last| *last >= index) {
            bail!("--slides pages must be strictly increasing");
        }
        pages.push(index);
    }
    if pages.is_empty() {
        bail!("--slides must name at least one page");
    }
    Ok(pages)
}

/// Parse one command token.
fn parse_command(token: &str) -> anyhow::Result<Command> {
    let normalised = token.trim().to_ascii_lowercase();
    if let Some(seconds) = normalised.strip_prefix("tick:") {
        let seconds = seconds
            .trim()
            .parse::<f32>()
            .with_context(|| format!("tick value {seconds:?} is not a number"))?;
        if !seconds.is_finite() || seconds < 0.0 {
            bail!("tick seconds must be finite and non-negative");
        }
        return Ok(Command::Tick(seconds));
    }
    if let Some(key) = normalised.strip_prefix("key:") {
        let key = match key.trim() {
            "space" | "enter" => PresentationKey::Space,
            "right" | "arrowright" => PresentationKey::ArrowRight,
            "left" | "arrowleft" => PresentationKey::ArrowLeft,
            "home" => PresentationKey::Home,
            "end" => PresentationKey::End,
            "escape" | "esc" => PresentationKey::Escape,
            "a" => PresentationKey::A,
            other => bail!("unknown presentation key {other:?}"),
        };
        let command = map_presentation_key(key, PresentationModifiers::default())
            .with_context(|| format!("presentation key {key:?} has no command"))?;
        return Ok(Command::Mapped(command));
    }
    if let Some(gesture) = normalised.strip_prefix("gesture:") {
        let gesture = match gesture.trim() {
            "swipe-left" | "left" => PresentationGesture::SwipeLeft,
            "swipe-right" | "right" => PresentationGesture::SwipeRight,
            "long-press" | "long_press" => PresentationGesture::LongPress,
            other => bail!("unknown presentation gesture {other:?}"),
        };
        return Ok(Command::Mapped(map_presentation_gesture(gesture)));
    }
    let command = match normalised.as_str() {
        "next" => PresentationCommand::Next,
        "prev" | "previous" | "back" => PresentationCommand::Previous,
        "first" => PresentationCommand::First,
        "last" => PresentationCommand::Last,
        "auto" | "toggle" => PresentationCommand::ToggleAutoAdvance,
        "exit" | "escape" => PresentationCommand::Exit,
        other => bail!("unknown presentation command {other:?}"),
    };
    Ok(Command::Mapped(command))
}

/// One parsed step.
enum Command {
    /// A model command (direct, key-mapped or gesture-mapped).
    Mapped(PresentationCommand),
    /// Advance the caller-supplied clock.
    Tick(f32),
}

/// Stable state label.
fn state_label(state: AdvanceState) -> &'static str {
    match state {
        AdvanceState::Stopped => "stopped",
        AdvanceState::Playing => "playing",
        AdvanceState::Finished => "finished",
        AdvanceState::Exited => "exited",
    }
}

/// Stable outcome label.
fn outcome_label(outcome: PresentationOutcome) -> String {
    match outcome {
        PresentationOutcome::Nothing => "nothing".to_owned(),
        PresentationOutcome::PageChanged(index) => format!("page_changed:{index}"),
        PresentationOutcome::AutoAdvanceStarted => "auto_advance_started".to_owned(),
        PresentationOutcome::AutoAdvanceStopped => "auto_advance_stopped".to_owned(),
        PresentationOutcome::Finished => "finished".to_owned(),
        PresentationOutcome::Exited => "exited".to_owned(),
    }
}

/// Layout for the active slide, when the page exists.
fn layout_for(
    model: &PresentationModel,
    document: &Document,
    screen: CanvasSize,
    fit: FitMode,
) -> anyhow::Result<Option<FullscreenLayout>> {
    let page = model.current_slide().map(|slide| slide.page);
    let Some(page) = page else {
        return Ok(None);
    };
    let frame = document
        .page_frame(page)
        .map_err(|error| anyhow::anyhow!("page {} frame: {error}", page + 1))?;
    let size = PageSize::new(
        (frame.media[2] - frame.media[0]) as f32,
        (frame.media[3] - frame.media[1]) as f32,
    );
    Ok(model.layout(screen, size, fit))
}

/// `pdfxi present`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run(
    input: &Path,
    commands: &str,
    screen: &str,
    fit: &str,
    duration: f32,
    transition: &str,
    slides: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let (screen_width, screen_height) = parse_screen(screen)?;
    let screen = CanvasSize::new(screen_width, screen_height);
    let fit = match fit.trim().to_ascii_lowercase().as_str() {
        "fit" => FitMode::Fit,
        "actual" | "1:1" => FitMode::Actual,
        other => bail!("unknown fit mode {other:?}; expected fit or actual"),
    };
    let transition = parse_transition(transition)?;
    let bytes = std::fs::read(input).with_context(|| format!("read {}", input.display()))?;
    let document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", input.display()))?;
    let pages = document
        .page_count()
        .map_err(|error| anyhow::anyhow!("count pages: {error}"))?;
    if pages == 0 {
        bail!("cannot present a document with no pages");
    }
    let selected = parse_pages(slides, pages)?;
    let mut model = if slides.is_some() || duration > 0.0 || transition != Transition::None {
        let built = selected
            .iter()
            .map(|index| Slide::new(*index, duration, transition))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| anyhow::anyhow!("slide: {error}"))?;
        PresentationModel::from_slides(built)
            .map_err(|error| anyhow::anyhow!("presentation: {error}"))?
    } else {
        PresentationModel::new(pages).map_err(|error| anyhow::anyhow!("presentation: {error}"))?
    };
    let slide_rows = model
        .slides()
        .iter()
        .map(|slide| SlideJson {
            page: slide.page + 1,
            duration: slide.duration,
            transition: slide.transition.label().to_owned(),
        })
        .collect::<Vec<_>>();
    let tokens: Vec<&str> = commands
        .split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .collect();
    let mut steps = Vec::new();
    for token in tokens {
        let command = parse_command(token)?;
        let outcome = match command {
            Command::Mapped(command) => model.command(command),
            Command::Tick(seconds) => model.tick(seconds),
        };
        steps.push(StepJson {
            command: token.to_owned(),
            outcome: outcome_label(outcome),
            index: model.current_index(),
            page: model.current_page() + 1,
            state: state_label(model.state()).to_owned(),
            layout: layout_for(&model, &document, screen, fit)?.map(|layout| LayoutJson {
                x: layout.x,
                y: layout.y,
                width: layout.width,
                height: layout.height,
                scale: layout.scale,
            }),
        });
    }
    let final_layout = layout_for(&model, &document, screen, fit)?;
    let report = PresentJson {
        input: input.display().to_string(),
        pages,
        screen_width,
        screen_height,
        fit: match fit {
            FitMode::Fit => "fit".to_owned(),
            FitMode::Actual => "actual".to_owned(),
        },
        slides: slide_rows,
        steps,
        final_index: model.current_index(),
        final_page: model.current_page() + 1,
        final_state: state_label(model.state()).to_owned(),
        final_layout: final_layout.map(|layout| LayoutJson {
            x: layout.x,
            y: layout.y,
            width: layout.width,
            height: layout.height,
            scale: layout.scale,
        }),
        exited: model.state() == AdvanceState::Exited,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!(
        "input: {}  pages: {}  slides: {}  screen: {}x{}  fit: {}",
        report.input,
        report.pages,
        report.slides.len(),
        report.screen_width,
        report.screen_height,
        report.fit
    );
    for step in &report.steps {
        println!(
            "{} -> {}  slide {} page {}  {}",
            step.command, step.outcome, step.index, step.page, step.state
        );
    }
    println!(
        "final: slide {}  page {}  state: {}  exited: {}",
        report.final_index, report.final_page, report.final_state, report.exited
    );
    if let Some(layout) = &report.final_layout {
        println!(
            "layout: x={} y={} {}x{} scale={}",
            layout.x, layout.y, layout.width, layout.height, layout.scale
        );
    }
    Ok(())
}
