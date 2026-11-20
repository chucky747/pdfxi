# Third-party components

PDFXI uses permissively licensed components. This page is updated as
dependencies land; `cargo deny check` enforces the allowlist in `deny.toml`.

## PDFium

- Version: pinned release `chromium/8165` (PDFium `160.0.8165.0`) of the
  prebuilt Windows x64 binaries from bblanchon/pdfium-binaries.
- Source URL:
  `https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F8165/pdfium-win-x64.tgz`
- Archive SHA-256:
  `ca7338ae7779b8b1b82f4580305c4d762d214f8df1d54b47647fe3cc75989ded`
- `pdfium.dll` SHA-256:
  `31d249b8535dd5f351ae7f4b714dc4bb1d76b6069b321a309bc99271dd65f566`
- License: BSD-3-Clause (PDFium) plus the third-party notices in the package's
  `licenses/` directory; the wrapper binaries are MIT (Benoit Blanchon).
- Use: page rendering, text extraction, form introspection.
- Distribution: the release package **redistributes** `pdfium.dll` next to
  `pdfxi-ui.exe`, together with `pdfium-licenses\` (the archive's `LICENSE` as
  `LICENSE.txt` plus its complete `licenses\*` set). `scripts/fetch-pdfium.ps1`
  downloads this exact archive, verifies the SHA-256 above before extracting,
  and fails closed on a mismatch. `scripts/package.ps1` re-verifies the DLL
  SHA-256 before staging. The DLL is not committed to version control.
- Loading: release builds load `pdfium.dll` only from the directory of the
  running executable (`crates/pdfxi-render/src/pdfium_backend.rs`); the current
  working directory, `PATH`, and the system search path are never used. Debug
  builds and tests may additionally use `PDFIUM_PATH` and the per-user cache at
  `%LOCALAPPDATA%\pdfxi\pdfium\bin`.
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

- Inno Setup 6 (Inno Setup License, freeware, redistribution allowed) as a
  build tool for `installer/pdfxi.iss`; not bundled and not required at runtime.
- LibreOffice (MPL-2.0) for Office conversion.
- A headless browser for HTML capture.
- Ghostscript (AGPL) is not bundled; any integration is opt-in and external.
- veraPDF is used as a CI oracle, not a runtime dependency.

## Release package (T-0123 / AC-10)

`scripts/package.ps1` produces a reproducible Windows package under
`target/package/`:

- `pdfxi-portable-<version>-windows-x86_64.zip` - the portable payload.
- `staging/` - the uncompressed payload: `pdfxi.exe` (CLI), `pdfxi-ui.exe`
  (GUI viewer, Windows GUI subsystem, embedded icon/VERSIONINFO/manifest),
  `pdfium.dll`, `pdfium-licenses\`, `LICENSE`, `NOTICE`, `README.md`,
  `THIRD_PARTY.md`, `deny.toml`, `install.ps1`, `manifest.json`.
- `manifest.json` - SHA-256 manifest plus the exact reproducibility inputs:
  `rustc`/`cargo` versions, pinned toolchain channel, edition/MSRV, the
  `Cargo.lock` SHA-256, the full `name@version` crate list read from
  `Cargo.lock`, the shipped binaries (`pdfxi.exe`, `pdfxi-ui.exe`,
  `pdfium.dll`), the pinned PDFium tag/hash, and per-file SHA-256 and byte
  length. No wall-clock timestamp is recorded, so identical inputs produce a
  byte-identical manifest.
- `<zipname>.sha256` - SHA-256 sidecar for the archive.
- `pdfxi-setup-<version>-windows-x86_64.exe` and its `.sha256` - per-user Inno
  Setup installer, produced when `ISCC.exe` is available (or `-Installer` is
  passed). It installs to `{localappdata}\Programs\PDFXI` without elevation,
  writes an HKCU uninstall entry, never claims the `.pdf` handler, and leaves
  user settings under `%APPDATA%\PDFXI` untouched on uninstall.

`install.ps1` copies the payload to a caller-supplied `-TargetDir` and creates
a per-user Start Menu shortcut (no elevation, no network). The package contains
no Adobe assets. The pinned PDFium DLL is bundled with its complete licence set;
its SHA-256 is checked before staging, and the viewer binary is rejected unless
it is a Windows GUI image with a PDFXI version resource and an icon group. Only
an explicit file allowlist is staged, and every staged file is screened against
secret/private filename patterns before an archive is written.

Modes: the default builds the workspace release binaries; `-SkipBuild` packages
binaries already in the release target dir; `-Check` validates layout, notices,
the allowlist and any existing manifest without building; `-SelfTest` runs the
deterministic validation suite (7 checks, no network, no GUI, no sleeps).

## Clean-room statement

PDFXI contains no Adobe source code, binaries, icons, help text, or proprietary
assets. Adobe and Acrobat are trademarks of Adobe Inc. PDFXI is independent.
