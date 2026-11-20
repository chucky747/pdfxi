# Third-party components

PDFXI uses permissively licensed components. This page is updated as
dependencies land; `cargo deny check` enforces the allowlist in `deny.toml`.

## PDFium

- Version: see `%LOCALAPPDATA%\pdfxi\pdfium\VERSION` (fetched by
  `scripts/fetch-pdfium.ps1`).
- License: BSD-3-Clause plus the third-party notices shipped in the PDFium
  package's `licenses/` directory.
- Use: page rendering, text extraction, form introspection.
- Distribution: the DLL is not committed and is NOT redistributed by the
  release package. `scripts/package.ps1` bundles only the notice text in this
  document; users who enable the optional `pdfium` feature fetch the DLL with
  `scripts/fetch-pdfium.ps1` and supply their own licensed copy. No PDFium
  `licenses/` payload is shipped inside the portable archive.
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

## Release package (T-0123 / AC-10)

`scripts/package.ps1` produces a reproducible Windows package under
`target/package/`:

- `pdfxi-portable-<version>-windows-x86_64.zip` - the portable payload.
- `staging/` - the uncompressed payload: CLI `pdfxi.exe`, `LICENSE`, `NOTICE`,
  `README.md`, `THIRD_PARTY.md`, `deny.toml`, `install.ps1`, `manifest.json`.
- `manifest.json` - SHA-256 manifest plus the exact reproducibility inputs:
  `rustc`/`cargo` versions, pinned toolchain channel, edition/MSRV, the
  `Cargo.lock` SHA-256, the full `name@version` crate list read from
  `Cargo.lock`, the binary names (`pdfxi.exe`), and per-file SHA-256 and byte
  length. No wall-clock timestamp is recorded, so identical inputs produce a
  byte-identical manifest.
- `<zipname>.sha256` - SHA-256 sidecar for the archive.

`install.ps1` copies the payload to a caller-supplied `-TargetDir` and creates
a per-user Start Menu shortcut (no elevation, no network). The package contains
no Adobe assets and no PDFium DLL; PDFium is notice-only. Only an explicit file
allowlist is staged, and every staged file is screened against secret/private
filename patterns before an archive is written.

Modes: the default builds the workspace release binaries; `-SkipBuild` packages
binaries already in the release target dir; `-Check` validates layout, notices,
the allowlist and any existing manifest without building; `-SelfTest` runs the
deterministic validation suite (7 checks, no network, no GUI, no sleeps).

## Clean-room statement

PDFXI contains no Adobe source code, binaries, icons, help text, or proprietary
assets. Adobe and Acrobat are trademarks of Adobe Inc. PDFXI is independent.
