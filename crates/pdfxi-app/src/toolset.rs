//! The set of interaction tools available on the viewer canvas.

/// The tool currently active on the document canvas.
///
/// Each variant carries a short label and keyboard hint so a future UI layer
/// can build a toolbar where every tool also has a keyboard path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Toolset {
    /// Select and manipulate page objects.
    #[default]
    Select,
    /// Pan the canvas by dragging.
    Hand,
    /// Select and copy text.
    TextSelection,
    /// Add highlight annotations.
    Highlight,
    /// Fill interactive form fields.
    FormFill,
    /// Take measurements on the page.
    Measure,
}

impl Toolset {
    /// A stable, human-readable label for this tool.
    pub fn label(self) -> &'static str {
        match self {
            Self::Select => "Select",
            Self::Hand => "Hand",
            Self::TextSelection => "Select Text",
            Self::Highlight => "Highlight",
            Self::FormFill => "Fill Form",
            Self::Measure => "Measure",
        }
    }

    /// A suggested single-key keyboard shortcut for this tool.
    pub fn keyboard_hint(self) -> &'static str {
        match self {
            Self::Select => "V",
            Self::Hand => "H",
            Self::TextSelection => "T",
            Self::Highlight => "Y",
            Self::FormFill => "F",
            Self::Measure => "M",
        }
    }

    /// All tools, in toolbar order.
    pub const ALL: [Self; 6] = [
        Self::Select,
        Self::Hand,
        Self::TextSelection,
        Self::Highlight,
        Self::FormFill,
        Self::Measure,
    ];
}

impl std::fmt::Display for Toolset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toolset_selection_and_labels() {
        assert_eq!(Toolset::default(), Toolset::Select);
        assert_eq!(Toolset::ALL.len(), 6);
        assert_eq!(Toolset::Hand.label(), "Hand");
        assert_eq!(Toolset::Measure.keyboard_hint(), "M");
        // Every tool has a non-empty label and keyboard hint (keyboard path).
        for tool in Toolset::ALL {
            assert!(!tool.label().is_empty());
            assert_eq!(tool.keyboard_hint().len(), 1);
        }
    }
}
