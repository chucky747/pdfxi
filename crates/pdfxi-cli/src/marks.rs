//! `pdfxi marks` — apply printer marks to a real PDF through the core API.
//!
//! The command loads a document with [`pdfxi_core::Document`], applies
//! [`pdfxi_core::Document::apply_printer_marks`] to every page, saves a
//! standalone PDF and reparses it before writing the output. Output bytes are
//! deterministic for a given input and mark selection.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_core::{Document, PrinterMarkOptions};

/// The four printer-mark families exposed as command-line switches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkSelection {
    /// Draw crop marks just outside the TrimBox.
    pub crop_marks: bool,
    /// Draw registration targets on the MediaBox corners.
    pub registration: bool,
    /// Draw the CMYK colour bar.
    pub colour_bars: bool,
    /// Draw page-information text.
    pub page_info: bool,
}

impl MarkSelection {
    /// Returns `true` when at least one family was explicitly requested.
    #[must_use]
    pub const fn any(self) -> bool {
        self.crop_marks || self.registration || self.colour_bars || self.page_info
    }

    fn options(self) -> PrinterMarkOptions {
        PrinterMarkOptions {
            crop_marks: self.crop_marks,
            registration_marks: self.registration,
            color_bars: self.colour_bars,
            page_info: self.page_info,
            ..PrinterMarkOptions::default()
        }
    }
}

/// Run the `marks` command.
///
/// When no family switch is supplied, all four families are applied (the
/// [`PrinterMarkOptions::default`] selection). When at least one is supplied,
/// only the requested families are drawn.
pub fn apply_marks(input: &Path, output: &Path, selection: MarkSelection) -> anyhow::Result<()> {
    let bytes =
        std::fs::read(input).with_context(|| format!("failed to read '{}'", input.display()))?;
    let mut document = Document::from_bytes(bytes)
        .with_context(|| format!("failed to parse '{}'", input.display()))?;
    let page_count = document
        .page_count()
        .context("failed to read the page tree")?;
    if page_count == 0 {
        bail!("cannot apply printer marks to a document with no pages");
    }
    let options = if selection.any() {
        selection.options()
    } else {
        PrinterMarkOptions::default()
    };

    for index in 0..page_count {
        document
            .apply_printer_marks(index, &options)
            .with_context(|| format!("failed to apply printer marks to page {index}"))?;
    }

    let saved = document
        .save()
        .context("failed to save the marked document")?;
    let reparsed =
        Document::from_bytes(saved.clone()).context("marked output failed to reparse")?;
    let reparsed_count = reparsed
        .page_count()
        .context("marked output has no page tree")?;
    if reparsed_count != page_count {
        bail!("marked output has {reparsed_count} page(s), source had {page_count}");
    }

    std::fs::write(output, &saved)
        .with_context(|| format!("failed to write '{}'", output.display()))?;
    println!(
        "wrote {} ({} page(s), crop={} registration={} colour_bars={} page_info={})",
        output.display(),
        page_count,
        options.crop_marks,
        options.registration_marks,
        options.color_bars,
        options.page_info
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_selection_means_all_families() {
        let none = MarkSelection {
            crop_marks: false,
            registration: false,
            colour_bars: false,
            page_info: false,
        };
        assert!(!none.any());
        assert!(PrinterMarkOptions::default().crop_marks);
        assert!(PrinterMarkOptions::default().registration_marks);
        assert!(PrinterMarkOptions::default().color_bars);
        assert!(PrinterMarkOptions::default().page_info);
    }

    #[test]
    fn partial_selection_enables_only_requested_families() {
        let selection = MarkSelection {
            crop_marks: true,
            registration: false,
            colour_bars: false,
            page_info: true,
        };
        assert!(selection.any());
        let options = selection.options();
        assert!(options.crop_marks);
        assert!(options.page_info);
        assert!(!options.registration_marks);
        assert!(!options.color_bars);
    }
}
