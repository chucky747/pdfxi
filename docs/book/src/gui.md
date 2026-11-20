# Desktop viewer

`pdfxi-ui` is the native viewer built on `egui`/`eframe`. It is a thin window
over the tested `pdfxi-app` state model, so every command below also has a
window-free unit test.

## Install

```powershell
powershell -NoProfile -File target\package\staging\install.ps1 `
    -TargetDir "$env:LOCALAPPDATA\Programs\PDFXI" -DesktopShortcut
```

The installer copies `pdfxi-ui.exe` and `pdfxi.exe`, then creates a Start-Menu
**PDFXI** link (viewer), **PDFXI CLI**, and an optional Desktop shortcut. It
never elevates and never writes the registry.

## Open a document

- **Open…** opens the path dialog. Type or paste a PDF path; Enter or **Open**
  submits. Empty input, control characters, non-`.pdf` extensions and missing
  files are rejected with a status message before anything is loaded.
- **Drag and drop** one or more PDFs from Explorer onto the window. Non-PDFs are
  ignored; a batch with no PDFs says so.
- **Command line**: `pdfxi-ui.exe C:\docs\report.pdf` opens that document at
  startup. Flags are ignored, so a bad path reports a status instead of
  blocking launch.
- **Open Demo** loads the bundled `fixtures/minimal.pdf` when the layout ships
  fixtures; the path is resolved at runtime from the executable or working
  directory, never baked in at build time.

## Rendering

Pages are rasterized through PDFium (`PDFIUM_PATH` or
`%LOCALAPPDATA%\pdfxi\pdfium\bin\pdfium.dll`). Safety limits:

- a single page raster is clamped to 16 Mpx (64 MiB RGBA);
- the LRU raster cache is bounded to 64 entries and 256 MiB;
- at most two new rasters render per frame, so large spreads never freeze the
  window;
- the cache and GPU textures are cleared and outstanding renders cancelled when
  the active document changes.

Without PDFium the viewer still opens, lays out pages and draws placeholders.

## Panels and commands

Thumbnail navigation, layers, bookmarks and attachments are side panels.
The toolbar and keyboard cover fit page/width/actual size, zoom, rotation,
single/continuous/two-page view modes, split/tile panes, the find bar,
text selection with Ctrl+C, and the print dialog (which drives the deterministic
print model and never contacts a printer).

## Not yet in the GUI

Many verified library features are exposed through the CLI or library only:
forms filling, redaction preview/apply, export dialogs, portfolio browsing,
batch/action wizard, and the accessibility editor. See the release plan for the
follow-on order.
