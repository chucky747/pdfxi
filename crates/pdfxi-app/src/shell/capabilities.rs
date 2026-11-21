//! Capability registry and the controls the shell may draw.
//!
//! A capability is a feature family that is actually implemented today. The
//! shell never draws a control for an unavailable capability: there are no
//! disabled ghosts. Later tasks flip entries on in [`Capabilities::available`]
//! when their feature lands.
//!
//! The registry also owns the small ordered catalogues the shell needs: rail
//! items, command-bar feature buttons, and tools-panel rows. Tests exercise
//! these catalogues directly, so they are behaviour (which commands exist) and
//! not string comparisons against a mock-up.

use crate::design::Icon;

use super::commands::AppCommand;

/// A feature family the shell can expose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    /// The document workspace: open, canvas, thumbnails, zoom, navigation.
    Documents,
    /// Comments, highlights and review markup.
    Comments,
    /// Edit PDF text and images (A-17/A-23; not available yet).
    Edit,
    /// AcroForm filling.
    Forms,
    /// Certificate and e-signature workflows.
    Signing,
    /// Export, convert, combine and organise-page tools.
    Export,
    /// Printing.
    Print,
    /// Password, permissions and redaction.
    Protect,
    /// The OpenCode-backed assistant conversation.
    ///
    /// This is enabled at runtime only while the connection screen reports a
    /// usable service, so no assistant control appears before there is a
    /// service to talk to. The connection screen itself is reached through
    /// [`Capability::AssistantConnect`].
    AiAssistant,
    /// The OpenCode connection screen.
    ///
    /// Always available in this build: the screen is how a user connects, so
    /// hiding it behind a successful connection would make the feature
    /// unreachable. It renders a real connect panel, never a status-only stub.
    AssistantConnect,
    /// The recent-file list.
    Recent,
    /// The local Files library.
    Files,
    /// Starred documents.
    Starred,
    /// The chosen shared folder.
    Shared,
    /// Bookmarks navigation.
    Bookmarks,
}

/// The set of capabilities enabled in this build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    bits: u16,
}

impl Default for Capabilities {
    /// Today's build: documents, recent files, and bookmark navigation.
    fn default() -> Self {
        Self::available()
    }
}

impl Capabilities {
    /// Capabilities that are enabled today.
    ///
    /// The document workspace, recent files, bookmarks navigation, the local
    /// Files library, starring, comments, AcroForm filling, the certificate
    /// signing card and the OpenCode connection screen exist. Shared is added by
    /// the shell when a shared folder is configured; [`Capability::AiAssistant`]
    /// is added by the shell only after the connection screen reports a usable
    /// service. Edit, export, print and protect remain off until their task lands.
    #[must_use]
    pub fn available() -> Self {
        Self::none()
            .with(Capability::Documents)
            .with(Capability::Recent)
            .with(Capability::Bookmarks)
            .with(Capability::Files)
            .with(Capability::Starred)
            .with(Capability::Comments)
            .with(Capability::Forms)
            .with(Capability::Signing)
            .with(Capability::AssistantConnect)
    }

    /// No capabilities at all, for tests and for a deliberately bare shell.
    #[must_use]
    pub fn none() -> Self {
        Self { bits: 0 }
    }

    /// Enable one more capability.
    #[must_use]
    pub fn with(mut self, capability: Capability) -> Self {
        self.bits |= 1u16 << (capability as u16);
        self
    }

    /// Disable one capability.
    #[must_use]
    pub fn without(mut self, capability: Capability) -> Self {
        self.bits &= !(1u16 << (capability as u16));
        self
    }

    /// Whether `capability` is enabled.
    #[must_use]
    pub fn contains(self, capability: Capability) -> bool {
        self.bits & (1u16 << (capability as u16)) != 0
    }
}

/// One entry in the left icon rail, in draw order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RailItem {
    /// Home / workspace overview.
    #[default]
    Home,
    /// Local library folder.
    Files,
    /// Persisted recent files.
    Recent,
    /// Starred documents.
    Starred,
    /// Shared folder.
    Shared,
    /// Page thumbnails.
    Pages,
    /// Comments and review.
    Comments,
    /// OpenCode assistant.
    AiAssistant,
}

impl RailItem {
    /// Capability that must be enabled for this rail item to exist.
    #[must_use]
    pub fn capability(self) -> Capability {
        match self {
            Self::Home | Self::Pages => Capability::Documents,
            Self::Files => Capability::Files,
            Self::Recent => Capability::Recent,
            Self::Starred => Capability::Starred,
            Self::Shared => Capability::Shared,
            Self::Comments => Capability::Comments,
            // The rail entry is the way to the connection screen; the
            // conversation controls use `Capability::AiAssistant`.
            Self::AiAssistant => Capability::AssistantConnect,
        }
    }

    /// Visible label under the icon.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::Files => "Files",
            Self::Recent => "Recent",
            Self::Starred => "Starred",
            Self::Shared => "Shared",
            Self::Pages => "Pages",
            Self::Comments => "Comments",
            Self::AiAssistant => "AI Assistant",
        }
    }

    /// Rail icon.
    #[must_use]
    pub fn icon(self) -> Icon {
        match self {
            Self::Home => Icon::Home,
            Self::Files => Icon::File,
            Self::Recent => Icon::Recent,
            Self::Starred => Icon::Star,
            Self::Shared => Icon::Shared,
            Self::Pages => Icon::Grid,
            Self::Comments => Icon::Comment,
            Self::AiAssistant => Icon::Sparkle,
        }
    }
}

/// The rail in its fixed order, filtered to enabled capabilities.
#[must_use]
pub fn rail_items(capabilities: Capabilities) -> Vec<RailItem> {
    [
        RailItem::Home,
        RailItem::Files,
        RailItem::Recent,
        RailItem::Starred,
        RailItem::Shared,
        RailItem::Pages,
        RailItem::Comments,
        RailItem::AiAssistant,
    ]
    .into_iter()
    .filter(|item| capabilities.contains(item.capability()))
    .collect()
}

/// A command-bar feature button, drawn only when its capability is enabled.
#[derive(Debug, Clone, PartialEq)]
pub struct FeatureButton {
    /// Capability gate.
    pub capability: Capability,
    /// Button label.
    pub label: &'static str,
    /// Icon.
    pub icon: Icon,
    /// Command emitted by the button.
    pub command: AppCommand,
}

/// Feature buttons in command-bar order, filtered to enabled capabilities.
#[must_use]
pub fn command_bar_features(capabilities: Capabilities) -> Vec<FeatureButton> {
    [
        FeatureButton {
            capability: Capability::Edit,
            label: "Edit",
            icon: Icon::Edit,
            command: AppCommand::EditDocument,
        },
        FeatureButton {
            capability: Capability::Comments,
            label: "Comment",
            icon: Icon::Comment,
            command: AppCommand::Comment,
        },
        FeatureButton {
            capability: Capability::Comments,
            label: "Highlight",
            icon: Icon::Highlight,
            command: AppCommand::Highlight,
        },
        FeatureButton {
            capability: Capability::Signing,
            label: "Sign",
            icon: Icon::Sign,
            command: AppCommand::Sign,
        },
        FeatureButton {
            capability: Capability::Export,
            label: "Organize",
            icon: Icon::Organize,
            command: AppCommand::Organize,
        },
        FeatureButton {
            capability: Capability::Export,
            label: "Export",
            icon: Icon::Export,
            command: AppCommand::Export,
        },
        FeatureButton {
            // The pill opens the assistant panel in every state: disconnected,
            // it shows the "Connect to OpenCode" card; connected, the
            // conversation. The conversation itself stays gated by
            // `Capability::AiAssistant` inside the panel.
            capability: Capability::AssistantConnect,
            label: "AI Assistant",
            icon: Icon::Sparkle,
            command: AppCommand::AiAssistant,
        },
    ]
    .into_iter()
    .filter(|button| capabilities.contains(button.capability))
    .collect()
}

/// One row in the right tools panel.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolRow {
    /// Row label, also the search text.
    pub label: &'static str,
    /// Capability gate.
    pub capability: Capability,
    /// Icon.
    pub icon: Icon,
    /// Command emitted by the row.
    pub command: AppCommand,
}

/// Tools-panel rows in draw order, before capability and search filtering.
#[must_use]
pub fn tool_rows() -> Vec<ToolRow> {
    vec![
        ToolRow {
            label: "Edit PDF",
            capability: Capability::Edit,
            icon: Icon::Edit,
            command: AppCommand::EditDocument,
        },
        ToolRow {
            label: "Comment",
            capability: Capability::Comments,
            icon: Icon::Comment,
            command: AppCommand::Comment,
        },
        ToolRow {
            label: "Fill & Sign",
            capability: Capability::Forms,
            icon: Icon::Sign,
            command: AppCommand::FillAndSign,
        },
        ToolRow {
            label: "Place signature",
            capability: Capability::Signing,
            icon: Icon::Sign,
            command: AppCommand::PlaceSignature,
        },
        ToolRow {
            label: "Request signature",
            capability: Capability::Signing,
            icon: Icon::Sign,
            command: AppCommand::RequestSignature,
        },
        ToolRow {
            label: "Organize Pages",
            capability: Capability::Export,
            icon: Icon::Organize,
            command: AppCommand::Organize,
        },
        ToolRow {
            label: "Export PDF",
            capability: Capability::Export,
            icon: Icon::Export,
            command: AppCommand::Export,
        },
        ToolRow {
            label: "Convert to PDF",
            capability: Capability::Export,
            icon: Icon::Convert,
            command: AppCommand::ConvertToPdf,
        },
        ToolRow {
            label: "Combine Files",
            capability: Capability::Export,
            icon: Icon::Combine,
            command: AppCommand::CombineFiles,
        },
        ToolRow {
            label: "Protect PDF",
            capability: Capability::Protect,
            icon: Icon::Protect,
            command: AppCommand::Protect,
        },
        ToolRow {
            label: "AI Assistant",
            capability: Capability::AssistantConnect,
            icon: Icon::Sparkle,
            command: AppCommand::AiAssistant,
        },
    ]
}

/// Rows that are both enabled and match `query` (case-insensitive substring).
#[must_use]
pub fn tool_rows_for(capabilities: Capabilities, query: &str) -> Vec<ToolRow> {
    let needle = query.trim().to_lowercase();
    tool_rows()
        .into_iter()
        .filter(|row| capabilities.contains(row.capability))
        .filter(|row| needle.is_empty() || row.label.to_lowercase().contains(&needle))
        .collect()
}
