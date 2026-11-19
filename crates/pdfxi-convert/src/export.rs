//! Export-target classification for output adapters.
//!
//! [`ExportKind`] is the output counterpart of [`crate::SourceKind`]. It is
//! derived from a requested target extension or file name and is used to route
//! an exported PDF to a concrete adapter. Conformance targets such as PDF/A or
//! PDF/X are produced by choosing [`ExportKind::Pdf`] and then applying the
//! checks and fixups from `pdfxi-standards`; this crate stays independent of
//! that crate on purpose.

use std::fmt;
use std::path::Path;

/// A classification of a file that can be produced from a PDF.
///
/// The variants mirror the source families so round-trip conversions can be
/// expressed symmetrically. [`ExportKind::Unknown`] is the total fallback for a
/// missing or unrecognized target extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExportKind {
    /// A PDF file (`*.pdf`), optionally conforming to a standard after
    /// preflight and fixups.
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
    /// A target whose format is missing or not recognized.
    Unknown,
}

impl ExportKind {
    /// Every recognized export target, in a stable order.
    ///
    /// [`ExportKind::Unknown`] is not included.
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

    /// Returns every recognized export target in a stable order.
    #[must_use]
    pub fn all() -> &'static [Self] {
        Self::ALL
    }

    /// Classifies a file name extension.
    ///
    /// A leading dot is optional and ASCII case is ignored. An unrecognized
    /// extension yields [`ExportKind::Unknown`].
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

    /// Classifies a target path by its final extension.
    ///
    /// Paths without an extension, or with a non-UTF-8 extension, are
    /// [`ExportKind::Unknown`]. No file system access is performed.
    #[must_use]
    pub fn from_path(path: &Path) -> Self {
        match path.extension().and_then(|ext| ext.to_str()) {
            Some(ext) => Self::from_extension(ext),
            None => Self::Unknown,
        }
    }

    /// Returns `true` unless this kind is [`ExportKind::Unknown`].
    #[must_use]
    pub fn is_known(self) -> bool {
        self != Self::Unknown
    }

    /// Returns `true` when the target is a PDF.
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

impl fmt::Display for ExportKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_kind_from_extension_is_case_insensitive_and_optional_dot() {
        assert_eq!(ExportKind::from_extension("PDF"), ExportKind::Pdf);
        assert_eq!(ExportKind::from_extension(".pdf"), ExportKind::Pdf);
        assert_eq!(ExportKind::from_extension(".HTML"), ExportKind::Html);
        assert_eq!(ExportKind::from_extension("docx"), ExportKind::Word);
        assert_eq!(ExportKind::from_extension("csv"), ExportKind::Spreadsheet);
        assert_eq!(ExportKind::from_extension("jpg"), ExportKind::Image);
        assert_eq!(ExportKind::from_extension("bin"), ExportKind::Unknown);
    }

    #[test]
    fn export_kind_from_path_handles_missing_and_unknown_extensions() {
        assert_eq!(
            ExportKind::from_path(Path::new("out/out.RTF")),
            ExportKind::Rtf
        );
        assert_eq!(
            ExportKind::from_path(Path::new("no-extension")),
            ExportKind::Unknown
        );
        assert_eq!(
            ExportKind::from_path(Path::new("a.b/unknown.xyz")),
            ExportKind::Unknown
        );
    }

    #[test]
    fn export_kind_labels_and_flags_are_stable() {
        for target in ExportKind::all() {
            assert!(target.is_known());
            assert!(!target.label().is_empty());
            assert_eq!(target.to_string(), target.label());
        }
        assert!(!ExportKind::Unknown.is_known());
        assert!(ExportKind::Pdf.is_pdf());
        assert!(!ExportKind::Image.is_pdf());
    }

    #[test]
    fn export_kind_all_has_no_unknown_and_no_duplicates() {
        assert_eq!(ExportKind::all().len(), 8);
        for target in ExportKind::all() {
            assert_ne!(*target, ExportKind::Unknown);
        }
    }
}
