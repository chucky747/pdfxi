//! Product shell design system: themes, layout math, and vector icons.
//!
//! The shell is deliberately data-driven so the look can be unit-tested: the
//! palette is a plain struct, [`shell_layout`] is pure geometry, and
//! [`contrast_ratio`] lets tests enforce readable text on every surface. No
//! widget in this module touches document state.

use egui::{Color32, Rect, Stroke, Vec2, pos2, vec2};

/// Which palette the shell uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeMode {
    /// Warm, paper-like surfaces with a red accent.
    #[default]
    Light,
    /// Deep charcoal surfaces with a brighter red accent.
    Dark,
}

impl ThemeMode {
    /// The opposite theme, for the title-bar toggle.
    pub fn toggled(self) -> Self {
        match self {
            Self::Light => Self::Dark,
            Self::Dark => Self::Light,
        }
    }

    /// Human-readable name.
    pub fn label(self) -> &'static str {
        match self {
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }
}

/// One complete color set for the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// Window background behind the panels.
    pub app_bg: Color32,
    /// Panel and card background.
    pub panel: Color32,
    /// Slightly raised surface (inputs, inactive tabs).
    pub surface: Color32,
    /// Hover/pressed surface.
    pub surface_hover: Color32,
    /// Workspace behind the document page.
    pub canvas: Color32,
    /// Hairline borders.
    pub border: Color32,
    /// Primary text.
    pub text: Color32,
    /// Secondary text.
    pub text_muted: Color32,
    /// Accent red used for primary actions.
    pub accent: Color32,
    /// Soft pink tint used for active selections and cards.
    pub accent_soft: Color32,
    /// Border for soft accent surfaces.
    pub accent_border: Color32,
    /// Success green for signature and check marks.
    pub success: Color32,
    /// Page paper color (always near-white).
    pub paper: Color32,
    /// Shadow color under floating cards.
    pub shadow: Color32,
}

impl Palette {
    /// Neutral, paper-like light theme.
    #[must_use]
    pub fn light() -> Self {
        Self {
            app_bg: Color32::from_rgb(0xf5, 0xf5, 0xf5),
            panel: Color32::from_rgb(0xff, 0xff, 0xff),
            surface: Color32::from_rgb(0xf2, 0xf2, 0xf2),
            surface_hover: Color32::from_rgb(0xe8, 0xe8, 0xe8),
            canvas: Color32::from_rgb(0xf0, 0xf0, 0xf0),
            border: Color32::from_rgb(0xe0, 0xe0, 0xe0),
            text: Color32::from_rgb(0x1a, 0x1a, 0x1a),
            text_muted: Color32::from_rgb(0x6e, 0x6e, 0x6e),
            accent: Color32::from_rgb(0xe4, 0x00, 0x2b),
            accent_soft: Color32::from_rgb(0xfc, 0xe8, 0xe8),
            accent_border: Color32::from_rgb(0xf5, 0xc2, 0xc7),
            success: Color32::from_rgb(0x2e, 0x7d, 0x32),
            paper: Color32::from_rgb(0xff, 0xff, 0xff),
            shadow: Color32::from_black_alpha(24),
        }
    }

    /// Neutral charcoal dark theme.
    #[must_use]
    pub fn dark() -> Self {
        Self {
            app_bg: Color32::from_rgb(0x12, 0x12, 0x12),
            panel: Color32::from_rgb(0x1e, 0x1e, 0x1e),
            surface: Color32::from_rgb(0x26, 0x26, 0x26),
            surface_hover: Color32::from_rgb(0x30, 0x30, 0x30),
            canvas: Color32::from_rgb(0x0e, 0x0e, 0x0e),
            border: Color32::from_rgb(0x33, 0x33, 0x33),
            text: Color32::from_rgb(0xf5, 0xf5, 0xf5),
            text_muted: Color32::from_rgb(0xa0, 0xa0, 0xa0),
            accent: Color32::from_rgb(0xff, 0x4b, 0x4b),
            accent_soft: Color32::from_rgb(0x3a, 0x1f, 0x24),
            accent_border: Color32::from_rgb(0x5c, 0x2a, 0x30),
            success: Color32::from_rgb(0x4c, 0xaf, 0x50),
            paper: Color32::from_rgb(0xfa, 0xfa, 0xfa),
            shadow: Color32::from_black_alpha(120),
        }
    }

    /// The palette for a mode.
    #[must_use]
    pub fn for_mode(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Light => Self::light(),
            ThemeMode::Dark => Self::dark(),
        }
    }
}

/// Colors for the product title bar.
///
/// The title bar follows the shell theme: light in light mode, charcoal in dark
/// mode, so its contrast is independent of the body palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TitlePalette {
    /// Bar background.
    pub background: Color32,
    /// Active tab and raised control surface.
    pub surface: Color32,
    /// Hover surface.
    pub hover: Color32,
    /// Hairline borders.
    pub border: Color32,
    /// Primary title text.
    pub text: Color32,
    /// Secondary title text.
    pub text_muted: Color32,
    /// Accent for the logo, active tab and close hover.
    pub accent: Color32,
    /// Soft accent for the avatar.
    pub accent_soft: Color32,
    /// Saved-state green.
    pub success: Color32,
}

impl TitlePalette {
    /// Title-bar colors for a shell theme.
    #[must_use]
    pub fn for_mode(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Light => Self {
                background: Color32::from_rgb(0xf5, 0xf5, 0xf5),
                surface: Color32::from_rgb(0xff, 0xff, 0xff),
                hover: Color32::from_rgb(0xe9, 0xe9, 0xe9),
                border: Color32::from_rgb(0xe0, 0xe0, 0xe0),
                text: Color32::from_rgb(0x1a, 0x1a, 0x1a),
                text_muted: Color32::from_rgb(0x6e, 0x6e, 0x6e),
                accent: Color32::from_rgb(0xe4, 0x00, 0x2b),
                accent_soft: Color32::from_rgb(0xfc, 0xe8, 0xe8),
                success: Color32::from_rgb(0x2e, 0x7d, 0x32),
            },
            ThemeMode::Dark => Self {
                background: Color32::from_rgb(0x14, 0x16, 0x1a),
                surface: Color32::from_rgb(0x25, 0x29, 0x30),
                hover: Color32::from_rgb(0x30, 0x35, 0x3e),
                border: Color32::from_rgb(0x31, 0x36, 0x3f),
                text: Color32::from_rgb(0xf3, 0xf1, 0xee),
                text_muted: Color32::from_rgb(0xa3, 0x9d, 0x96),
                accent: Color32::from_rgb(0xf2, 0x55, 0x4a),
                accent_soft: Color32::from_rgb(0x3a, 0x22, 0x21),
                success: Color32::from_rgb(0x45, 0xc0, 0x7a),
            },
        }
    }
}

/// Colors used inside the document page.
///
/// A page is paper, not chrome: its ink and highlights must stay readable in
/// both shell themes, so these values are deliberately independent of
/// [`Palette`]. The dark theme only softens the paper white.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaperPalette {
    /// Paper background.
    pub paper: Color32,
    /// Primary document ink.
    pub ink: Color32,
    /// Secondary document text.
    pub muted: Color32,
    /// Hairline rules inside the page.
    pub rule: Color32,
    /// Highlight behind a marked phrase.
    pub highlight: Color32,
    /// Success green on paper.
    pub success: Color32,
    /// Pale success background for badges.
    pub success_bg: Color32,
    /// Accent red on paper.
    pub accent: Color32,
    /// Top of the cover gradient.
    pub cover_top: Color32,
    /// Bottom of the cover gradient.
    pub cover_bottom: Color32,
    /// Raised card printed on the page.
    pub card: Color32,
    /// Border around cards printed on the page.
    pub card_border: Color32,
    /// Shadow under cards printed on the page.
    pub shadow: Color32,
}

impl PaperPalette {
    /// Paper colors for a shell theme.
    #[must_use]
    pub fn for_mode(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Light => Self {
                paper: Color32::from_rgb(0xff, 0xff, 0xff),
                ink: Color32::from_rgb(0x24, 0x1f, 0x1c),
                muted: Color32::from_rgb(0x6f, 0x68, 0x62),
                rule: Color32::from_rgb(0xe4, 0xdd, 0xd6),
                highlight: Color32::from_rgb(0xfb, 0xe3, 0xdf),
                success: Color32::from_rgb(0x1f, 0x8a, 0x54),
                success_bg: Color32::from_rgb(0xe6, 0xf4, 0xec),
                accent: Color32::from_rgb(0xc8, 0x34, 0x2b),
                cover_top: Color32::from_rgb(0xc9, 0x2f, 0x27),
                cover_bottom: Color32::from_rgb(0xf0, 0x8a, 0x7c),
                card: Color32::from_rgb(0xff, 0xff, 0xff),
                card_border: Color32::from_rgb(0xea, 0xdf, 0xd7),
                shadow: Color32::from_black_alpha(20),
            },
            ThemeMode::Dark => Self {
                paper: Color32::from_rgb(0x1c, 0x1c, 0x1c),
                ink: Color32::from_rgb(0xf5, 0xf5, 0xf5),
                muted: Color32::from_rgb(0xa8, 0xa8, 0xa8),
                rule: Color32::from_rgb(0x33, 0x33, 0x33),
                highlight: Color32::from_rgb(0x4a, 0x20, 0x24),
                success: Color32::from_rgb(0x66, 0xd6, 0x8a),
                success_bg: Color32::from_rgb(0x1d, 0x3a, 0x28),
                accent: Color32::from_rgb(0xff, 0x5c, 0x5c),
                cover_top: Color32::from_rgb(0xb8, 0x24, 0x2a),
                cover_bottom: Color32::from_rgb(0x5e, 0x12, 0x18),
                card: Color32::from_rgb(0x26, 0x26, 0x26),
                card_border: Color32::from_rgb(0x36, 0x36, 0x36),
                shadow: Color32::from_black_alpha(90),
            },
        }
    }
}

/// Relative luminance of an sRGB color, per WCAG 2.1.
#[must_use]
pub fn relative_luminance(color: Color32) -> f64 {
    let channel = |value: u8| {
        let c = f64::from(value) / 255.0;
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
}

/// WCAG contrast ratio between two opaque colors.
#[must_use]
pub fn contrast_ratio(first: Color32, second: Color32) -> f64 {
    let a = relative_luminance(first);
    let b = relative_luminance(second);
    let (lighter, darker) = if a >= b { (a, b) } else { (b, a) };
    (lighter + 0.05) / (darker + 0.05)
}

/// Install the shell's fonts, spacing, rounding and visuals on the context.
pub fn apply(ctx: &egui::Context, mode: ThemeMode) {
    let palette = Palette::for_mode(mode);
    let mut style = (*ctx.style()).clone();

    style.spacing.item_spacing = vec2(8.0, 8.0);
    style.spacing.button_padding = vec2(10.0, 6.0);
    style.spacing.menu_margin = egui::Margin::same(8);
    style.spacing.window_margin = egui::Margin::same(14);
    style.spacing.indent = 16.0;

    style.visuals = match mode {
        ThemeMode::Light => egui::Visuals::light(),
        ThemeMode::Dark => egui::Visuals::dark(),
    };
    style.visuals.panel_fill = palette.panel;
    style.visuals.window_fill = palette.panel;
    style.visuals.extreme_bg_color = palette.surface;
    style.visuals.faint_bg_color = palette.surface;
    style.visuals.override_text_color = Some(palette.text);
    style.visuals.window_stroke = Stroke::new(1.0_f32, palette.border);
    style.visuals.selection.bg_fill = palette.accent_soft;
    style.visuals.selection.stroke = Stroke::new(1.0_f32, palette.accent);
    style.visuals.hyperlink_color = palette.accent;
    style.visuals.widgets.noninteractive.bg_fill = palette.panel;
    style.visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, palette.text_muted);
    style.visuals.widgets.inactive.bg_fill = palette.surface;
    style.visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, palette.text);
    style.visuals.widgets.inactive.weak_bg_fill = palette.surface;
    style.visuals.widgets.hovered.bg_fill = palette.surface_hover;
    style.visuals.widgets.hovered.weak_bg_fill = palette.surface_hover;
    style.visuals.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, palette.text);
    style.visuals.widgets.active.bg_fill = palette.accent_soft;
    style.visuals.widgets.active.weak_bg_fill = palette.accent_soft;
    style.visuals.widgets.active.fg_stroke = Stroke::new(1.0_f32, palette.accent);

    for (corner, radius) in [
        (&mut style.visuals.widgets.inactive.corner_radius, 8u8),
        (&mut style.visuals.widgets.hovered.corner_radius, 8),
        (&mut style.visuals.widgets.active.corner_radius, 8),
        (&mut style.visuals.widgets.noninteractive.corner_radius, 8),
    ] {
        *corner = egui::CornerRadius::same(radius);
    }

    ctx.set_style(style);
}

/// Icon glyphs drawn with the painter (no icon font dependency).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    /// House outline.
    Home,
    /// Document outline.
    File,
    /// Clock outline.
    Recent,
    /// Star outline.
    Star,
    /// Two-person outline.
    Shared,
    /// Grid of pages.
    Pages,
    /// Speech bubble.
    Comment,
    /// Four-point sparkle.
    Sparkle,
    /// Pen nib.
    Edit,
    /// Highlighter marker.
    Highlight,
    /// Signature swoosh.
    Sign,
    /// Stacked squares (organize).
    Organize,
    /// Upload arrow out of a tray (export).
    Export,
    /// Convert arrows.
    Convert,
    /// Two overlapping documents.
    Combine,
    /// Shield.
    Protect,
    /// Magnifier.
    Search,
    /// Bell.
    Bell,
    /// Question mark in a circle.
    Help,
    /// Sun.
    Sun,
    /// Moon.
    Moon,
    /// Chevron pointing left.
    ChevronLeft,
    /// Chevron pointing right.
    ChevronRight,
    /// Chevron pointing down.
    ChevronDown,
    /// Plus.
    Plus,
    /// Minus.
    Minus,
    /// Grid layout toggle.
    Grid,
    /// List layout toggle.
    List,
    /// Zoom in.
    ZoomIn,
    /// Zoom out.
    ZoomOut,
    /// Fit page.
    FitPage,
    /// Full screen corners.
    Fullscreen,
    /// Check mark.
    Check,
    /// Arrow left (undo/back).
    ArrowLeft,
    /// Arrow right (redo/forward).
    ArrowRight,
    /// Close cross.
    Close,
    /// Minimize dash.
    Minimize,
    /// Maximize square.
    Maximize,
    /// Restore: two overlapping squares.
    Restore,
    /// Settings gear.
    Gear,
    /// Send arrow.
    Send,
    /// Share nodes.
    Share,
    /// Three-dot overflow.
    More,
    /// Padlock.
    Lock,
    /// Chevron pointing up.
    ChevronUp,
    /// Printer.
    Print,
}

/// Draw an [`Icon`] centered in `rect` with a 1.7 pt stroke.
pub fn icon(painter: &egui::Painter, rect: Rect, glyph: Icon, color: Color32) {
    let stroke = Stroke::new(1.7_f32, color);
    let thin = Stroke::new(1.4_f32, color);
    let c = rect.center();
    let s = rect.width().min(rect.height());
    let half = s * 0.5;
    let p = |x: f32, y: f32| pos2(c.x + x * s, c.y + y * s);
    match glyph {
        Icon::Home => {
            painter.add(egui::Shape::line(
                vec![p(-0.36, -0.02), p(0.0, -0.34), p(0.36, -0.02)],
                stroke,
            ));
            painter.add(egui::Shape::line(
                vec![
                    p(-0.28, -0.04),
                    p(-0.28, 0.32),
                    p(0.28, 0.32),
                    p(0.28, -0.04),
                ],
                stroke,
            ));
        }
        Icon::File => {
            let r = Rect::from_center_size(c, Vec2::splat(half * 1.25));
            painter.rect_stroke(r, 3.0, stroke, egui::StrokeKind::Inside);
            painter.add(egui::Shape::line(vec![p(-0.16, 0.02), p(0.16, 0.02)], thin));
            painter.add(egui::Shape::line(vec![p(-0.16, 0.14), p(0.10, 0.14)], thin));
        }
        Icon::Recent => {
            painter.circle_stroke(c, half * 0.62, stroke);
            painter.add(egui::Shape::line(vec![c, p(0.0, -0.28)], thin));
            painter.add(egui::Shape::line(vec![c, p(0.20, 0.10)], thin));
        }
        Icon::Star => {
            let mut points = Vec::with_capacity(10);
            for index in 0..10 {
                let angle =
                    -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * 2.0 * index as f32 / 10.0;
                let radius = if index % 2 == 0 { 0.38 } else { 0.16 };
                points.push(pos2(
                    c.x + angle.cos() * radius * s,
                    c.y + angle.sin() * radius * s,
                ));
            }
            painter.add(egui::Shape::closed_line(points, stroke));
        }
        Icon::Shared => {
            painter.circle_stroke(p(-0.16, -0.16), half * 0.22, stroke);
            painter.circle_stroke(p(0.18, -0.16), half * 0.22, stroke);
            painter.add(egui::Shape::line(
                vec![p(-0.34, 0.28), p(-0.16, 0.04), p(0.02, 0.24)],
                thin,
            ));
            painter.add(egui::Shape::line(
                vec![p(0.04, 0.24), p(0.18, 0.04), p(0.36, 0.28)],
                thin,
            ));
        }
        Icon::Pages => {
            for dx in [-0.22, 0.0, 0.22] {
                for dy in [-0.22, 0.22] {
                    let r = Rect::from_center_size(p(dx, dy), Vec2::splat(half * 0.5));
                    painter.rect_filled(r, 1.5, color);
                }
            }
        }
        Icon::Comment => {
            let r = Rect::from_center_size(p(0.0, -0.04), Vec2::new(half * 1.6, half * 1.15));
            painter.rect_stroke(r, 6.0, stroke, egui::StrokeKind::Inside);
            painter.add(egui::Shape::line(
                vec![p(-0.20, 0.46), p(-0.02, 0.24)],
                thin,
            ));
        }
        Icon::Sparkle => {
            let mut points = Vec::with_capacity(8);
            for index in 0..8 {
                let angle = std::f32::consts::PI * 2.0 * index as f32 / 8.0;
                let radius = if index % 2 == 0 { 0.42 } else { 0.12 };
                points.push(pos2(
                    c.x + angle.cos() * radius * s,
                    c.y + angle.sin() * radius * s,
                ));
            }
            painter.add(egui::Shape::closed_line(points, stroke));
        }
        Icon::Edit => {
            painter.add(egui::Shape::line(
                vec![p(-0.30, 0.30), p(0.20, -0.24)],
                stroke,
            ));
            painter.add(egui::Shape::line(
                vec![p(0.10, -0.34), p(0.32, -0.12)],
                stroke,
            ));
            painter.add(egui::Shape::line(
                vec![p(-0.34, 0.34), p(-0.12, 0.30)],
                thin,
            ));
        }
        Icon::Highlight => {
            painter.add(egui::Shape::line(
                vec![p(-0.30, 0.12), p(0.06, -0.26)],
                stroke,
            ));
            painter.add(egui::Shape::line(
                vec![p(-0.10, 0.30), p(0.26, -0.08)],
                stroke,
            ));
            painter.add(egui::Shape::line(vec![p(-0.34, 0.34), p(0.30, 0.34)], thin));
        }
        Icon::Sign => {
            let mut points = Vec::new();
            for step in 0..=24 {
                let t = step as f32 / 24.0;
                let x = -0.36 + t * 0.72;
                let y = -0.10 + (t * 6.0).sin() * 0.10 + t * 0.18;
                points.push(p(x, y));
            }
            painter.add(egui::Shape::line(points, stroke));
        }
        Icon::Organize => {
            let back = Rect::from_center_size(p(-0.10, -0.10), Vec2::splat(half * 1.2));
            let front = Rect::from_center_size(p(0.10, 0.10), Vec2::splat(half * 1.2));
            painter.rect_stroke(back, 2.0, thin, egui::StrokeKind::Inside);
            painter.rect_stroke(front, 2.0, stroke, egui::StrokeKind::Inside);
        }
        Icon::Export => {
            painter.add(egui::Shape::line(vec![p(0.0, -0.36), p(0.0, 0.10)], stroke));
            painter.add(egui::Shape::line(
                vec![p(-0.20, -0.12), p(0.0, 0.12), p(0.20, -0.12)],
                stroke,
            ));
            painter.add(egui::Shape::line(
                vec![p(-0.30, 0.20), p(-0.30, 0.34), p(0.30, 0.34), p(0.30, 0.20)],
                thin,
            ));
        }
        Icon::Convert => {
            painter.add(egui::Shape::line(
                vec![p(-0.30, -0.14), p(0.22, -0.14)],
                stroke,
            ));
            painter.add(egui::Shape::line(
                vec![p(0.06, -0.30), p(0.24, -0.14), p(0.06, 0.02)],
                stroke,
            ));
            painter.add(egui::Shape::line(
                vec![p(0.30, 0.16), p(-0.22, 0.16)],
                stroke,
            ));
            painter.add(egui::Shape::line(
                vec![p(-0.06, 0.02), p(-0.24, 0.16), p(-0.06, 0.30)],
                stroke,
            ));
        }
        Icon::Combine => {
            let left = Rect::from_center_size(p(-0.12, 0.0), Vec2::splat(half * 1.1));
            let right = Rect::from_center_size(p(0.12, 0.0), Vec2::splat(half * 1.1));
            painter.rect_filled(left, 2.0, Color32::TRANSPARENT);
            painter.rect_stroke(left, 2.0, stroke, egui::StrokeKind::Inside);
            painter.rect_stroke(right, 2.0, thin, egui::StrokeKind::Inside);
        }
        Icon::Protect => {
            painter.add(egui::Shape::closed_line(
                vec![
                    p(0.0, -0.36),
                    p(0.30, -0.22),
                    p(0.30, 0.06),
                    p(0.0, 0.36),
                    p(-0.30, 0.06),
                    p(-0.30, -0.22),
                ],
                stroke,
            ));
        }
        Icon::Search => {
            painter.circle_stroke(p(-0.08, -0.08), half * 0.36, stroke);
            painter.add(egui::Shape::line(
                vec![p(0.14, 0.14), p(0.34, 0.34)],
                stroke,
            ));
        }
        Icon::Bell => {
            painter.add(egui::Shape::line(
                vec![
                    p(-0.28, 0.16),
                    p(-0.20, 0.02),
                    p(-0.20, -0.16),
                    p(0.0, -0.30),
                    p(0.20, -0.16),
                    p(0.20, 0.02),
                    p(0.28, 0.16),
                    p(-0.28, 0.16),
                ],
                stroke,
            ));
            painter.add(egui::Shape::line(vec![p(-0.06, 0.26), p(0.06, 0.26)], thin));
        }
        Icon::Help => {
            painter.circle_stroke(c, half * 0.66, stroke);
            let mut points = Vec::new();
            for step in 0..=12 {
                let angle = -2.2 + step as f32 / 12.0 * 2.4;
                points.push(pos2(
                    c.x + angle.cos() * half * 0.20,
                    c.y - half * 0.10 + angle.sin() * half * 0.20,
                ));
            }
            painter.add(egui::Shape::line(points, thin));
            painter.circle_filled(p(0.0, 0.20), 1.3, color);
        }
        Icon::Sun => {
            painter.circle_stroke(c, half * 0.30, stroke);
            for index in 0..8 {
                let angle = std::f32::consts::PI * 2.0 * index as f32 / 8.0;
                let inner = 0.44;
                let outer = 0.60;
                painter.add(egui::Shape::line(
                    vec![
                        pos2(c.x + angle.cos() * inner * s, c.y + angle.sin() * inner * s),
                        pos2(c.x + angle.cos() * outer * s, c.y + angle.sin() * outer * s),
                    ],
                    thin,
                ));
            }
        }
        Icon::Moon => {
            // A solid crescent: a tapered band between the outer rim and an
            // inner arc opening to the right. Each slice is a convex quad, so
            // the shape fills cleanly at title-bar sizes. (The previous
            // two-arc outline crossed itself and rendered as a scribble.)
            let radius = 0.40_f32;
            let thickness = 0.24_f32;
            let start = 55.0_f32.to_radians();
            let sweep = 250.0_f32.to_radians();
            const SLICES: usize = 20;
            for index in 0..SLICES {
                let a0 = start + sweep * index as f32 / SLICES as f32;
                let a1 = start + sweep * (index + 1) as f32 / SLICES as f32;
                let u0 = index as f32 / SLICES as f32;
                let u1 = (index + 1) as f32 / SLICES as f32;
                let inner0 = radius - thickness * (std::f32::consts::PI * u0).sin();
                let inner1 = radius - thickness * (std::f32::consts::PI * u1).sin();
                let point =
                    |r: f32, angle: f32| pos2(c.x + angle.cos() * r * s, c.y + angle.sin() * r * s);
                painter.add(egui::Shape::convex_polygon(
                    vec![
                        point(radius, a0),
                        point(radius, a1),
                        point(inner1, a1),
                        point(inner0, a0),
                    ],
                    color,
                    Stroke::NONE,
                ));
            }
        }
        Icon::ChevronLeft => {
            painter.add(egui::Shape::line(
                vec![p(0.12, -0.24), p(-0.12, 0.0), p(0.12, 0.24)],
                stroke,
            ));
        }
        Icon::ChevronRight => {
            painter.add(egui::Shape::line(
                vec![p(-0.12, -0.24), p(0.12, 0.0), p(-0.12, 0.24)],
                stroke,
            ));
        }
        Icon::ChevronDown => {
            painter.add(egui::Shape::line(
                vec![p(-0.24, -0.10), p(0.0, 0.14), p(0.24, -0.10)],
                stroke,
            ));
        }
        Icon::Plus => {
            painter.add(egui::Shape::line(vec![p(0.0, -0.26), p(0.0, 0.26)], stroke));
            painter.add(egui::Shape::line(vec![p(-0.26, 0.0), p(0.26, 0.0)], stroke));
        }
        Icon::Minus => {
            painter.add(egui::Shape::line(vec![p(-0.26, 0.0), p(0.26, 0.0)], stroke));
        }
        Icon::Grid => {
            for dx in [-0.22, 0.22] {
                for dy in [-0.22, 0.22] {
                    painter.rect_stroke(
                        Rect::from_center_size(p(dx, dy), Vec2::splat(half * 0.62)),
                        1.5,
                        thin,
                        egui::StrokeKind::Inside,
                    );
                }
            }
        }
        Icon::List => {
            for dy in [-0.22, 0.0, 0.22] {
                painter.add(egui::Shape::line(vec![p(-0.28, dy), p(0.28, dy)], thin));
            }
        }
        Icon::ZoomIn | Icon::ZoomOut => {
            painter.circle_stroke(p(-0.06, -0.06), half * 0.34, stroke);
            painter.add(egui::Shape::line(
                vec![p(0.14, 0.14), p(0.34, 0.34)],
                stroke,
            ));
            painter.add(egui::Shape::line(
                vec![p(-0.20, -0.06), p(0.08, -0.06)],
                thin,
            ));
            if glyph == Icon::ZoomIn {
                painter.add(egui::Shape::line(
                    vec![p(-0.06, -0.20), p(-0.06, 0.08)],
                    thin,
                ));
            }
        }
        Icon::FitPage => {
            let r = Rect::from_center_size(c, Vec2::splat(half * 1.30));
            painter.rect_stroke(r, 3.0, thin, egui::StrokeKind::Inside);
            for (x, y, sx, sy) in [
                (-0.25, -0.25, -1.0, -1.0),
                (0.25, -0.25, 1.0, -1.0),
                (-0.25, 0.25, -1.0, 1.0),
                (0.25, 0.25, 1.0, 1.0),
            ] {
                painter.add(egui::Shape::line(
                    vec![p(x, y), p(x + sx * 0.10, y), p(x, y + sy * 0.10)],
                    thin,
                ));
            }
        }
        Icon::Fullscreen => {
            for (x, y, sx, sy) in [
                (-0.32, -0.32, 1.0, 1.0),
                (0.32, -0.32, -1.0, 1.0),
                (-0.32, 0.32, 1.0, -1.0),
                (0.32, 0.32, -1.0, -1.0),
            ] {
                painter.add(egui::Shape::line(
                    vec![p(x, y), p(x + sx * 0.16, y), p(x, y + sy * 0.16)],
                    stroke,
                ));
            }
        }
        Icon::Check => {
            painter.add(egui::Shape::line(
                vec![p(-0.26, 0.02), p(-0.06, 0.22), p(0.28, -0.20)],
                Stroke::new(2.2_f32, color),
            ));
        }
        Icon::ArrowLeft => {
            painter.add(egui::Shape::line(
                vec![p(0.18, -0.28), p(-0.18, 0.0), p(0.18, 0.28)],
                stroke,
            ));
            painter.add(egui::Shape::line(vec![p(-0.18, 0.0), p(0.34, 0.0)], thin));
        }
        Icon::ArrowRight => {
            painter.add(egui::Shape::line(
                vec![p(-0.18, -0.28), p(0.18, 0.0), p(-0.18, 0.28)],
                stroke,
            ));
            painter.add(egui::Shape::line(vec![p(-0.34, 0.0), p(0.18, 0.0)], thin));
        }
        Icon::Close => {
            painter.add(egui::Shape::line(
                vec![p(-0.22, -0.22), p(0.22, 0.22)],
                thin,
            ));
            painter.add(egui::Shape::line(
                vec![p(0.22, -0.22), p(-0.22, 0.22)],
                thin,
            ));
        }
        Icon::Minimize => {
            painter.add(egui::Shape::line(vec![p(-0.24, 0.0), p(0.24, 0.0)], thin));
        }
        Icon::Maximize => {
            painter.rect_stroke(
                Rect::from_center_size(c, Vec2::splat(half * 1.1)),
                1.0,
                thin,
                egui::StrokeKind::Inside,
            );
        }
        Icon::Restore => {
            // Back window: top and right edges only, so the front square
            // reads as overlapping it rather than as a second closed box.
            painter.add(egui::Shape::line(
                vec![
                    p(-0.04, -0.32),
                    p(0.32, -0.32),
                    p(0.32, 0.04),
                    p(0.08, 0.04),
                ],
                thin,
            ));
            painter.rect_stroke(
                Rect::from_min_max(p(-0.32, -0.06), p(0.06, 0.32)),
                0.5,
                thin,
                egui::StrokeKind::Inside,
            );
        }
        Icon::Gear => {
            // Simple outline cog: ring, six separated teeth, and a wide centre
            // hole, all at the same hairline weight as the Find magnifier.
            let ring = half * 0.56;
            painter.circle_stroke(c, ring, thin);
            painter.circle_stroke(c, half * 0.28, thin);
            for index in 0..6 {
                let angle = std::f32::consts::TAU * index as f32 / 6.0;
                painter.add(egui::Shape::line(
                    vec![
                        pos2(c.x + angle.cos() * ring, c.y + angle.sin() * ring),
                        pos2(
                            c.x + angle.cos() * half * 0.80,
                            c.y + angle.sin() * half * 0.80,
                        ),
                    ],
                    thin,
                ));
            }
        }
        Icon::Send => {
            painter.add(egui::Shape::closed_line(
                vec![p(-0.30, -0.28), p(0.34, 0.0), p(-0.30, 0.28), p(-0.18, 0.0)],
                stroke,
            ));
        }
        Icon::Share => {
            painter.circle_filled(p(0.24, -0.24), half * 0.15, color);
            painter.circle_filled(p(-0.28, 0.0), half * 0.15, color);
            painter.circle_filled(p(0.24, 0.24), half * 0.15, color);
            painter.add(egui::Shape::line(
                vec![p(-0.16, -0.04), p(0.12, -0.18)],
                thin,
            ));
            painter.add(egui::Shape::line(vec![p(-0.16, 0.04), p(0.12, 0.18)], thin));
        }
        Icon::More => {
            for dy in [-0.22, 0.0, 0.22] {
                painter.circle_filled(p(0.0, dy), half * 0.12, color);
            }
        }
        Icon::Lock => {
            let body = Rect::from_center_size(p(0.0, 0.12), Vec2::new(half * 1.0, half * 0.82));
            painter.rect_stroke(body, 3.0, stroke, egui::StrokeKind::Inside);
            let mut points = Vec::new();
            for step in 0..=16 {
                let angle = std::f32::consts::PI + step as f32 / 16.0 * std::f32::consts::PI;
                points.push(pos2(
                    c.x + angle.cos() * half * 0.32,
                    c.y + 0.06 * s + angle.sin() * half * 0.32,
                ));
            }
            painter.add(egui::Shape::line(points, thin));
            painter.circle_filled(p(0.0, 0.12), 1.4, color);
        }
        Icon::ChevronUp => {
            painter.add(egui::Shape::line(
                vec![p(-0.24, 0.10), p(0.0, -0.14), p(0.24, 0.10)],
                stroke,
            ));
        }
        Icon::Print => {
            painter.rect_stroke(
                Rect::from_center_size(p(0.0, 0.10), Vec2::new(half * 1.15, half * 0.62)),
                3.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.rect_stroke(
                Rect::from_center_size(p(0.0, -0.26), Vec2::new(half * 0.78, half * 0.42)),
                2.0,
                thin,
                egui::StrokeKind::Inside,
            );
            painter.rect_stroke(
                Rect::from_center_size(p(0.0, 0.30), Vec2::new(half * 0.78, half * 0.34)),
                2.0,
                thin,
                egui::StrokeKind::Inside,
            );
        }
    }
}

/// Fixed chrome sizes shared by the layout and the widgets.
pub mod metrics {
    /// Title bar height.
    pub const TITLE_BAR: f32 = 46.0;
    /// Command toolbar height.
    pub const TOOL_BAR: f32 = 56.0;
    /// Left icon rail width.
    pub const RAIL: f32 = 78.0;
    /// Pages panel width.
    pub const PAGES: f32 = 206.0;
    /// Tools panel width.
    pub const TOOLS: f32 = 248.0;
    /// Reserved bottom inset; the page controls float over the canvas.
    pub const STATUS_BAR: f32 = 0.0;
    /// Minimum canvas width before panels collapse.
    pub const MIN_CANVAS: f32 = 320.0;
}

/// Rectangles for every chrome region at one window size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShellLayout {
    /// Application title bar.
    pub title: Rect,
    /// Command toolbar below the title bar.
    pub tools_bar: Rect,
    /// Icon rail on the far left.
    pub rail: Rect,
    /// Pages thumbnails panel.
    pub pages: Rect,
    /// Document canvas.
    pub canvas: Rect,
    /// Right-hand tools panel.
    pub tools: Rect,
    /// Bottom control bar.
    pub status: Rect,
}

/// Compute the shell rectangles.
///
/// Panels keep their nominal width until the canvas would fall below
/// [`metrics::MIN_CANVAS`]; then the pages panel collapses first, followed by
/// the tools panel. The title bar, toolbar, rail and status bar are always
/// present.
#[must_use]
pub fn shell_layout(available: Rect, show_pages: bool, show_tools: bool) -> ShellLayout {
    let width = available.width().max(0.0);
    let height = available.height().max(0.0);
    let title = Rect::from_min_size(available.min, vec2(width, metrics::TITLE_BAR.min(height)));
    let tools_bar = Rect::from_min_size(
        pos2(available.min.x, title.max.y),
        vec2(
            width,
            metrics::TOOL_BAR.min((height - title.height()).max(0.0)),
        ),
    );
    let status_height =
        metrics::STATUS_BAR.min((height - title.height() - tools_bar.height()).max(0.0));
    let status = Rect::from_min_size(
        pos2(available.min.x, available.max.y - status_height),
        vec2(width, status_height),
    );
    let body_top = tools_bar.max.y;
    let body_bottom = status.min.y.max(body_top);
    let body_height = (body_bottom - body_top).max(0.0);

    let rail_width = metrics::RAIL.min(width);
    let rail = Rect::from_min_size(
        pos2(available.min.x, body_top),
        vec2(rail_width, body_height),
    );

    let mut pages_width = if show_pages { metrics::PAGES } else { 0.0 };
    let mut tools_width = if show_tools { metrics::TOOLS } else { 0.0 };

    if width - rail_width - pages_width - tools_width < metrics::MIN_CANVAS {
        pages_width = 0.0;
    }
    if width - rail_width - pages_width - tools_width < metrics::MIN_CANVAS {
        tools_width = 0.0;
    }

    let pages = Rect::from_min_size(pos2(rail.max.x, body_top), vec2(pages_width, body_height));
    let tools = Rect::from_min_size(
        pos2(available.max.x - tools_width, body_top),
        vec2(tools_width, body_height),
    );
    let canvas_width = (tools.min.x - pages.max.x).max(0.0);
    let canvas = Rect::from_min_size(pos2(pages.max.x, body_top), vec2(canvas_width, body_height));

    ShellLayout {
        title,
        tools_bar,
        rail,
        pages,
        canvas,
        tools,
        status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(width: f32, height: f32) -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(width, height))
    }

    #[test]
    fn layout_regions_are_ordered_and_do_not_overlap() {
        let layout = shell_layout(window(1440.0, 900.0), true, true);
        assert_eq!(layout.title.min, pos2(0.0, 0.0));
        assert_eq!(layout.title.height(), metrics::TITLE_BAR);
        assert_eq!(layout.tools_bar.min.y, layout.title.max.y);
        assert_eq!(layout.rail.min.y, layout.tools_bar.max.y);
        assert_eq!(layout.pages.min.x, layout.rail.max.x);
        assert_eq!(layout.canvas.min.x, layout.pages.max.x);
        assert_eq!(layout.tools.max.x, 1440.0);
        assert_eq!(layout.canvas.max.x, layout.tools.min.x);
        assert_eq!(layout.status.max.y, 900.0);
        assert_eq!(layout.canvas.max.y, layout.status.min.y);
        assert!(layout.canvas.width() >= metrics::MIN_CANVAS);
    }

    #[test]
    fn collapsing_rules_keep_a_usable_canvas() {
        let narrow = shell_layout(window(700.0, 700.0), true, true);
        assert_eq!(narrow.pages.width(), 0.0, "pages collapse first");
        assert!(narrow.canvas.width() >= metrics::MIN_CANVAS);

        let tiny = shell_layout(window(500.0, 600.0), true, true);
        assert_eq!(tiny.pages.width(), 0.0);
        assert_eq!(tiny.tools.width(), 0.0, "tools collapse second");
        assert!(tiny.canvas.width() >= 0.0);
    }

    #[test]
    fn layout_survives_degenerate_window_sizes() {
        for (width, height) in [(0.0, 0.0), (1.0, 1.0), (320.0, 200.0)] {
            let layout = shell_layout(window(width, height), true, true);
            assert!(layout.title.width() >= 0.0);
            assert!(layout.canvas.width() >= 0.0);
            assert!(layout.canvas.height() >= 0.0);
        }
    }

    #[test]
    fn light_theme_text_is_readable_on_every_surface() {
        let palette = Palette::light();
        for surface in [
            palette.panel,
            palette.surface,
            palette.canvas,
            palette.accent_soft,
        ] {
            assert!(
                contrast_ratio(palette.text, surface) >= 7.0,
                "light text contrast {} on {surface:?}",
                contrast_ratio(palette.text, surface)
            );
        }
        assert!(contrast_ratio(palette.text_muted, palette.panel) >= 4.5);
        assert!(contrast_ratio(palette.accent, palette.panel) >= 3.0);
    }

    #[test]
    fn dark_theme_text_is_readable_on_every_surface() {
        let palette = Palette::dark();
        for surface in [
            palette.panel,
            palette.surface,
            palette.canvas,
            palette.accent_soft,
        ] {
            assert!(
                contrast_ratio(palette.text, surface) >= 7.0,
                "dark text contrast {} on {surface:?}",
                contrast_ratio(palette.text, surface)
            );
        }
        assert!(contrast_ratio(palette.text_muted, palette.panel) >= 4.5);
        assert!(contrast_ratio(palette.accent, palette.panel) >= 3.0);
    }

    #[test]
    fn paper_ink_stays_readable_in_both_themes() {
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            let paper = PaperPalette::for_mode(mode);
            assert!(
                contrast_ratio(paper.ink, paper.paper) >= 12.0,
                "paper ink contrast {} in {mode:?}",
                contrast_ratio(paper.ink, paper.paper)
            );
            assert!(contrast_ratio(paper.muted, paper.paper) >= 4.5);
            assert!(contrast_ratio(paper.accent, paper.paper) >= 3.0);
        }
    }

    #[test]
    fn title_bar_text_is_readable_on_the_dark_bar() {
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            let palette = TitlePalette::for_mode(mode);
            assert!(
                contrast_ratio(palette.text, palette.background) >= 7.0,
                "title text contrast {} in {mode:?}",
                contrast_ratio(palette.text, palette.background)
            );
            assert!(contrast_ratio(palette.text_muted, palette.background) >= 4.5);
            assert!(contrast_ratio(palette.accent, palette.background) >= 3.0);
        }
    }
}
