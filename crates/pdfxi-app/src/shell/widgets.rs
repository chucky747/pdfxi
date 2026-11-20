//! Shared shell widgets: panel frames, icon buttons, and caption chrome.

use crate::design::{Icon, Palette, TitlePalette, icon};
use crate::settings::WindowControlStyle;

/// Panel background frame with horizontal padding.
pub(crate) fn shell_panel_frame(palette: Palette, horizontal: i8) -> egui::Frame {
    egui::Frame::new()
        .fill(palette.panel)
        .inner_margin(egui::Margin {
            left: horizontal,
            right: horizontal,
            top: 0,
            bottom: 0,
        })
}

/// A borderless icon button used across the shell.
pub(crate) fn shell_icon_button(
    ui: &mut egui::Ui,
    palette: Palette,
    glyph: Icon,
    size: f32,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, 7.0, palette.surface);
    }
    let color = if response.hovered() {
        palette.text
    } else {
        palette.text_muted
    };
    icon(ui.painter(), rect.shrink(size * 0.27), glyph, color);
    response
}

/// A macOS-style traffic light in the custom title bar.
///
/// The three lights are the window controls: red closes, amber minimizes and
/// green toggles maximize. The glyph appears on hover so the bar stays quiet.
pub(crate) fn traffic_light(
    ui: &mut egui::Ui,
    fill: egui::Color32,
    glyph: Icon,
    hover_text: &str,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
    let center = rect.center();
    let lit = if response.hovered() {
        fill.gamma_multiply(1.08)
    } else {
        fill
    };
    ui.painter().circle_filled(center, 6.0, lit);
    if response.hovered() {
        icon(
            ui.painter(),
            egui::Rect::from_center_size(center, egui::Vec2::splat(7.0)),
            glyph,
            egui::Color32::from_black_alpha(160),
        );
    }
    response.on_hover_text(hover_text)
}

/// Width of one Windows caption button in the title bar, in UI points.
pub const CAPTION_BUTTON_WIDTH: f32 = 46.0;

/// A window-control action drawn in the custom title bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CaptionAction {
    /// Minimize the window.
    Minimize,
    /// Toggle between maximized and restored.
    ToggleMaximize,
    /// Close the window.
    Close,
}

/// The edge of the title bar that holds the caption buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptionSide {
    /// Left edge (macOS traffic lights).
    Left,
    /// Right edge (Windows caption buttons).
    Right,
}

/// Ordered caption buttons for a control style, and the side they occupy.
///
/// The array is ordered left to right exactly as it is drawn.
#[must_use]
pub fn caption_layout(style: WindowControlStyle) -> (CaptionSide, [CaptionAction; 3]) {
    match style {
        WindowControlStyle::Windows => (
            CaptionSide::Right,
            [
                CaptionAction::Minimize,
                CaptionAction::ToggleMaximize,
                CaptionAction::Close,
            ],
        ),
        WindowControlStyle::MacOs => (
            CaptionSide::Left,
            [
                CaptionAction::Close,
                CaptionAction::Minimize,
                CaptionAction::ToggleMaximize,
            ],
        ),
    }
}

/// The viewport command a caption button sends.
#[must_use]
pub fn caption_command(action: CaptionAction, maximized: bool) -> egui::ViewportCommand {
    match action {
        CaptionAction::Minimize => egui::ViewportCommand::Minimized(true),
        CaptionAction::ToggleMaximize => egui::ViewportCommand::Maximized(!maximized),
        CaptionAction::Close => egui::ViewportCommand::Close,
    }
}

/// Draw a native-weight Windows caption glyph.
///
/// Glyphs are 10 pt wide with a one-physical-pixel stroke, snapped to half
/// pixels so they stay crisp hairlines at any scale. The general icon set is
/// deliberately not used here: its strokes are too heavy for caption buttons.
pub(crate) fn caption_glyph(
    painter: &egui::Painter,
    center: egui::Pos2,
    action: CaptionAction,
    maximized: bool,
    color: egui::Color32,
) {
    let pixels = painter.pixels_per_point();
    let snap = |value: f32| ((value * pixels - 0.5).round() + 0.5) / pixels;
    let stroke = egui::Stroke::new(1.0 / pixels, color);
    let cx = snap(center.x);
    let cy = snap(center.y);
    let left = cx - 5.0;
    let right = cx + 5.0;
    let top = cy - 5.0;
    let bottom = cy + 5.0;
    match action {
        CaptionAction::Minimize => {
            painter.line_segment([egui::pos2(left, cy), egui::pos2(right, cy)], stroke);
        }
        CaptionAction::ToggleMaximize if maximized => {
            // Restore: a back square offset up and right behind a front one.
            painter.rect_stroke(
                egui::Rect::from_min_max(
                    egui::pos2(left + 2.0, top),
                    egui::pos2(right, bottom - 2.0),
                ),
                0.0,
                stroke,
                egui::StrokeKind::Middle,
            );
            painter.rect_stroke(
                egui::Rect::from_min_max(
                    egui::pos2(left, top + 2.0),
                    egui::pos2(right - 2.0, bottom),
                ),
                0.0,
                stroke,
                egui::StrokeKind::Middle,
            );
        }
        CaptionAction::ToggleMaximize => {
            painter.rect_stroke(
                egui::Rect::from_min_max(egui::pos2(left, top), egui::pos2(right, bottom)),
                0.0,
                stroke,
                egui::StrokeKind::Middle,
            );
        }
        CaptionAction::Close => {
            painter.line_segment([egui::pos2(left, top), egui::pos2(right, bottom)], stroke);
            painter.line_segment([egui::pos2(right, top), egui::pos2(left, bottom)], stroke);
        }
    }
}

/// A borderless icon button on the dark title bar.
pub(crate) fn title_icon_button(
    ui: &mut egui::Ui,
    palette: TitlePalette,
    glyph: Icon,
    size: f32,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, 7.0, palette.hover);
    }
    let color = if response.hovered() {
        palette.text
    } else {
        palette.text_muted
    };
    icon(ui.painter(), rect.shrink(size * 0.27), glyph, color);
    response
}

/// Toolbar button with an icon and label.
pub(crate) fn shell_tool_button(
    ui: &mut egui::Ui,
    palette: Palette,
    glyph: Icon,
    label: &str,
    active: bool,
) -> egui::Response {
    let font = egui::FontId::proportional(12.0);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), palette.text);
    let width = galley.size().x + 40.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 32.0), egui::Sense::click());
    let background = if active {
        palette.accent_soft
    } else if response.hovered() {
        palette.surface
    } else {
        egui::Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 8.0, background);
    let color = if active { palette.accent } else { palette.text };
    icon(
        ui.painter(),
        egui::Rect::from_center_size(
            egui::pos2(rect.left() + 15.0, rect.center().y),
            egui::Vec2::splat(14.0),
        ),
        glyph,
        color,
    );
    ui.painter().text(
        egui::pos2(rect.left() + 27.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        font,
        color,
    );
    response
}

/// Thin vertical separator.
pub(crate) fn shell_divider(ui: &mut egui::Ui, palette: Palette) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(9.0, 22.0), egui::Sense::hover());
    ui.painter().line_segment(
        [
            egui::pos2(rect.center().x, rect.top()),
            egui::pos2(rect.center().x, rect.bottom()),
        ],
        egui::Stroke::new(1.0_f32, palette.border),
    );
}
