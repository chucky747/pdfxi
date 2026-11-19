# Third-party components

PDFXI uses permissively licensed components. This page is updated as
dependencies land; `cargo deny check` enforces the allowlist in `deny.toml`.

## PDFium

- Version: see `%LOCALAPPDATA%\pdfxi\pdfium\VERSION` (fetched by
  `scripts/fetch-pdfium.ps1`).
- License: BSD-3-Clause plus the third-party notices shipped in the PDFium
  package's `licenses/` directory.
- Use: page rendering, text extraction, form introspection.
- Distribution: the DLL is not committed; packaging copies it beside the
  executable and includes the PDFium `licenses/` directory.
- Threading: PDFium is not thread-safe. PDFXI serializes access or uses a
  bounded pool.

## Rust crates

| Crate | License | Use |
|---|---|---|
| `lopdf` | MIT | PDF object manipulation (planned) |
| `pdfium-render` | MIT OR Apache-2.0 | PDFium bindings (optional `pdfium` feature) |
| `thiserror` | MIT OR Apache-2.0 | Library errors |
| `clap` | MIT OR Apache-2.0 | CLI |
| `serde` / `serde_json` / `toml` | MIT OR Apache-2.0 | Config and schemas |
| `egui` / `eframe` | MIT OR Apache-2.0 | Desktop UI (optional `ui` feature) |

## External tools (optional, not bundled)

- LibreOffice (MPL-2.0) for Office conversion.
- A headless browser for HTML capture.
- Ghostscript (AGPL) is not bundled; any integration is opt-in and external.
- veraPDF is used as a CI oracle, not a runtime dependency.

## Clean-room statement

PDFXI contains no Adobe source code, binaries, icons, help text, or proprietary
assets. Adobe and Acrobat are trademarks of Adobe Inc. PDFXI is independent.
