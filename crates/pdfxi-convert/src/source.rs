//! Source-format classification for import adapters.
//!
//! [`SourceKind`] is a coarse, dependency-free classification derived from a
//! file name. It deliberately does not sniff file contents: external adapters
//! decide whether a candidate is actually loadable, while this enum lets
//! callers route paths to the right converter and report actionable errors.
//!
//! Unknown extensions map to [`SourceKind::Unknown`] instead of producing an
//! error, so an application can still display the file name while reporting
//! that no import adapter is available.

use std::fmt;
use std::path::Path;

/// A coarse classification of a file that can be imported into a PDF.
///
/// The variants cover the document families targeted by the conversion
/// milestones: PDF itself, plain text, HTML, RTF, raster images, and the three
/// Office application families. [`SourceKind::Unknown`] is the total fallback
/// for missing, unrecognized, or non-UTF-8 extensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceKind {
    /// A Portable Document Format file (`*.pdf`).
    Pdf,
    /// Plain text (`*.txt`, `*.text`, `*.log`, `*.md`).
    Text,
    /// HyperText Markup Language (`*.html`, `*.htm`, `*.xhtml`).
    Html,
    /// Rich Text Format (`*.rtf`).
    Rtf,
    /// A raster image (`*.png`, `*.jpg`, `*.jpeg`, `*.gif`, `*.bmp`,
    /// `*.tif`, `*.tiff`, `*.webp`).
    Image,
    /// A word-processing document (`*.doc`, `*.docx`, `*.odt`).
    Word,
    /// A spreadsheet (`*.xls`, `*.xlsx`, `*.ods`, `*.csv`).
    Spreadsheet,
    /// A presentation (`*.ppt`, `*.pptx`, `*.odp`).
    Presentation,
    /// A source whose format is missing or not recognized.
    Unknown,
}

impl SourceKind {
    /// Every recognized source kind, in a stable order.
    ///
    /// [`SourceKind::Unknown`] is not included.
    pub const ALL: &'static [Self] = &[
        Self::Pdf,
        Self::Text,
        Self::Html,
        Self::Rtf,
        Self::Image,
        Self::Word,
        Self::Spreadsheet,
        Self::Presentation,
    ];

    /// Returns every recognized source kind in a stable order.
    #[must_use]
    pub fn all() -> &'static [Self] {
        Self::ALL
    }

    /// Classifies a file name extension.
    ///
    /// A leading dot is optional and ASCII case is ignored. An unrecognized
    /// extension yields [`SourceKind::Unknown`].
    #[must_use]
    pub fn from_extension(extension: &str) -> Self {
        let ext = match extension.strip_prefix('.') {
            Some(rest) => rest,
            None => extension,
        };
        let ext = ext.to_ascii_lowercase();
        match ext.as_str() {
            "pdf" => Self::Pdf,
            "txt" | "text" | "log" | "md" => Self::Text,
            "html" | "htm" | "xhtml" => Self::Html,
            "rtf" => Self::Rtf,
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "tif" | "tiff" | "webp" => Self::Image,
            "doc" | "docx" | "odt" => Self::Word,
            "xls" | "xlsx" | "ods" | "csv" => Self::Spreadsheet,
            "ppt" | "pptx" | "odp" => Self::Presentation,
            _ => Self::Unknown,
        }
    }

    /// Classifies a path by its final extension.
    ///
    /// Paths without an extension, or with a non-UTF-8 extension, are
    /// [`SourceKind::Unknown`]. This function never inspects the file system.
    #[must_use]
    pub fn from_path(path: &Path) -> Self {
        match path.extension().and_then(|ext| ext.to_str()) {
            Some(ext) => Self::from_extension(ext),
            None => Self::Unknown,
        }
    }

    /// Returns `true` unless this kind is [`SourceKind::Unknown`].
    #[must_use]
    pub fn is_known(self) -> bool {
        self != Self::Unknown
    }

    /// Returns `true` when the source is already a PDF.
    #[must_use]
    pub fn is_pdf(self) -> bool {
        self == Self::Pdf
    }

    /// Returns a short human-readable label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Pdf => "pdf",
            Self::Text => "text",
            Self::Html => "html",
            Self::Rtf => "rtf",
            Self::Image => "image",
            Self::Word => "word",
            Self::Spreadsheet => "spreadsheet",
            Self::Presentation => "presentation",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for SourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_kind_from_extension_is_case_insensitive_and_optional_dot() {
        assert_eq!(SourceKind::from_extension("PDF"), SourceKind::Pdf);
        assert_eq!(SourceKind::from_extension(".pdf"), SourceKind::Pdf);
        assert_eq!(SourceKind::from_extension(".Xlsx"), SourceKind::Spreadsheet);
        assert_eq!(SourceKind::from_extension("pptx"), SourceKind::Presentation);
        assert_eq!(SourceKind::from_extension("tiff"), SourceKind::Image);
        assert_eq!(SourceKind::from_extension("odt"), SourceKind::Word);
    }

    #[test]
    fn source_kind_from_path_handles_missing_and_unknown_extensions() {
        assert_eq!(
            SourceKind::from_path(Path::new("a/b/report.PDF")),
            SourceKind::Pdf
        );
        assert_eq!(
            SourceKind::from_path(Path::new("README")),
            SourceKind::Unknown
        );
        assert_eq!(
            SourceKind::from_path(Path::new("file.bin")),
            SourceKind::Unknown
        );
        assert_eq!(
            SourceKind::from_path(Path::new("dir.with.dot/file")),
            SourceKind::Unknown
        );
    }

    #[test]
    fn source_kind_labels_and_flags_are_stable() {
        for kind in SourceKind::all() {
            assert!(kind.is_known());
            assert!(!kind.label().is_empty());
            assert_eq!(kind.to_string(), kind.label());
        }
        assert!(!SourceKind::Unknown.is_known());
        assert!(SourceKind::Pdf.is_pdf());
        assert!(!SourceKind::Text.is_pdf());
        assert_eq!(SourceKind::Unknown.label(), "unknown");
    }

    #[test]
    fn source_kind_all_has_no_unknown_and_no_duplicates() {
        assert_eq!(SourceKind::all().len(), 8);
        for kind in SourceKind::all() {
            assert_ne!(*kind, SourceKind::Unknown);
        }
    }
}
