//! Typed view commands and their keyboard mapping.
//!
//! Keeping command identity and shortcut resolution in this dependency-free
//! module lets the UI layer translate `egui` key events once and dispatch a
//! single [`ViewCommand`], while the mapping itself is asserted in window-free
//! unit tests. The commands cover both rotation and the page display controls
//! (fit page, fit width, actual size), so every toolbar action has a keyboard
//! path.

/// A view or page-display command triggered by a toolbar action or shortcut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewCommand {
    /// Rotate the view 90 degrees clockwise.
    RotateClockwise,
    /// Rotate the view 90 degrees counterclockwise.
    RotateCounterclockwise,
    /// Reset the per-tab view rotation to zero.
    ResetRotation,
    /// Fit the whole (rotated) page in the viewport.
    FitPage,
    /// Fit the width of the (rotated) page to the viewport.
    FitWidth,
    /// Show the page at 100% actual size.
    ActualSize,
    /// Increase zoom by one step.
    ZoomIn,
    /// Decrease zoom by one step.
    ZoomOut,
}

impl ViewCommand {
    /// A stable, human-readable shortcut label for this command.
    ///
    /// These labels are shown next to toolbar actions and are pure strings so
    /// they can be asserted without a GUI context.
    pub fn shortcut_label(self) -> &'static str {
        match self {
            Self::RotateClockwise => "Ctrl+R",
            Self::RotateCounterclockwise => "Ctrl+Shift+R",
            Self::ResetRotation => "Ctrl+Shift+0",
            Self::FitPage => "Ctrl+0",
            Self::FitWidth => "Ctrl+2",
            Self::ActualSize => "Ctrl+1",
            Self::ZoomIn => "Ctrl+=",
            Self::ZoomOut => "Ctrl+-",
        }
    }
}

/// The key of a keyboard shortcut, independent of any GUI toolkit's key type.
///
/// The UI layer translates its native key enum into these values before calling
/// [`map_shortcut`], keeping the mapping deterministic and testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutKey {
    /// The `R` key.
    R,
    /// The `0` key.
    Zero,
    /// The `1` key.
    One,
    /// The `2` key.
    Two,
    /// The `=` / `+` key.
    Equals,
    /// The `-` key.
    Minus,
}

/// Modifier keys held alongside a [`ShortcutKey`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ShortcutModifiers {
    /// Control is held.
    pub ctrl: bool,
    /// Shift is held.
    pub shift: bool,
    /// Alt is held.
    pub alt: bool,
}

/// Resolve a keyboard shortcut into a [`ViewCommand`].
///
/// All view shortcuts require Ctrl and none use Alt. Shift disambiguates the
/// counterclockwise rotation and the rotation reset from their base commands.
/// Unrecognized combinations return `None` so the UI can leave them to other
/// widgets (for example text entry in the find bar).
pub fn map_shortcut(key: ShortcutKey, modifiers: ShortcutModifiers) -> Option<ViewCommand> {
    let ShortcutModifiers { ctrl, shift, alt } = modifiers;
    if !ctrl || alt {
        return None;
    }
    match (key, shift) {
        (ShortcutKey::R, false) => Some(ViewCommand::RotateClockwise),
        (ShortcutKey::R, true) => Some(ViewCommand::RotateCounterclockwise),
        (ShortcutKey::Zero, false) => Some(ViewCommand::FitPage),
        (ShortcutKey::Zero, true) => Some(ViewCommand::ResetRotation),
        (ShortcutKey::One, false) => Some(ViewCommand::ActualSize),
        (ShortcutKey::Two, false) => Some(ViewCommand::FitWidth),
        (ShortcutKey::Equals, false) => Some(ViewCommand::ZoomIn),
        (ShortcutKey::Minus, false) => Some(ViewCommand::ZoomOut),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(ctrl: bool, shift: bool, alt: bool) -> ShortcutModifiers {
        ShortcutModifiers { ctrl, shift, alt }
    }

    #[test]
    fn rotation_shortcuts_map_with_shift_disambiguation() {
        assert_eq!(
            map_shortcut(ShortcutKey::R, mods(true, false, false)),
            Some(ViewCommand::RotateClockwise)
        );
        assert_eq!(
            map_shortcut(ShortcutKey::R, mods(true, true, false)),
            Some(ViewCommand::RotateCounterclockwise)
        );
        assert_eq!(
            map_shortcut(ShortcutKey::Zero, mods(true, true, false)),
            Some(ViewCommand::ResetRotation)
        );
    }

    #[test]
    fn display_control_shortcuts_map_to_zoom_commands() {
        assert_eq!(
            map_shortcut(ShortcutKey::Zero, mods(true, false, false)),
            Some(ViewCommand::FitPage)
        );
        assert_eq!(
            map_shortcut(ShortcutKey::One, mods(true, false, false)),
            Some(ViewCommand::ActualSize)
        );
        assert_eq!(
            map_shortcut(ShortcutKey::Two, mods(true, false, false)),
            Some(ViewCommand::FitWidth)
        );
        assert_eq!(
            map_shortcut(ShortcutKey::Equals, mods(true, false, false)),
            Some(ViewCommand::ZoomIn)
        );
        assert_eq!(
            map_shortcut(ShortcutKey::Minus, mods(true, false, false)),
            Some(ViewCommand::ZoomOut)
        );
    }

    #[test]
    fn shortcuts_require_ctrl_and_reject_alt() {
        assert_eq!(map_shortcut(ShortcutKey::R, mods(false, false, false)), None);
        assert_eq!(map_shortcut(ShortcutKey::R, mods(true, false, true)), None);
        assert_eq!(
            map_shortcut(ShortcutKey::One, mods(true, true, false)),
            None,
            "Shift+1 is not a display command"
        );
    }

    #[test]
    fn every_command_has_a_stable_shortcut_label() {
        let commands = [
            ViewCommand::RotateClockwise,
            ViewCommand::RotateCounterclockwise,
            ViewCommand::ResetRotation,
            ViewCommand::FitPage,
            ViewCommand::FitWidth,
            ViewCommand::ActualSize,
            ViewCommand::ZoomIn,
            ViewCommand::ZoomOut,
        ];
        for command in commands {
            assert!(!command.shortcut_label().is_empty());
        }
        assert_eq!(ViewCommand::RotateClockwise.shortcut_label(), "Ctrl+R");
    }
}
