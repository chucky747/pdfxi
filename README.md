# PDFXI

PDFXI is a clean-room, Rust-native remake of the Adobe Acrobat XI Pro feature
set. It is organised as a headless core, feature libraries, a command-line
binary (`pdfxi`), and a native desktop viewer (`pdfxi-ui`). It uses no Adobe
code, assets, services, or trademarks, and normal operation needs no network
access.

## Status

The feature matrix has 

3 `replaced`**. Under
[ADR 0009](docs/decisions/0009-feature-status-honesty.md), `verified` means the
capability runs on real document bytes and an end user can reach it through the
viewer or the CLI. `library` means a tested byte-level implementation with no
caller; `model` means an in-memory model, plan, or host-boundary stub.

`cargo xtask goal-verify --all` therefore does **not** print `GOAL VERIFIED`.
AC-04 (every row verified or replaced) is not met: 115 rows are unfinished. The
authoritative row data is in [`docs/FEATURES.md`](docs/FEATURES.md) and
[`docs/features.toml`](docs/features.toml).

What works end to end today:

- **Viewer**: an empty Home screen; opening real PDFs through the native file
  picker, drag-and-drop, or a path argument; tabs; real page thumbnails; page
  navigation; zoom, fit page/width, actual size, and view rotation; find over
  the document's extracted text; light/dark/follow-system theme and Windows or
  macOS window controls persisted to `%APPDATA%\PDFXI\settings.toml`; and a
  dismissible error banner. Password-protected PDFs are not supported.
- **CLI**: the commands backed by `verified` rows — `info`, `text`,
  `create text`, `create image`, `repair`, `export tagged`, `preflight`, and
  `marks`.
- **Packaging**: a GUI-subsystem executable with icon, version resource, and
  DPI manifest; pinned PDFium bundled next to the executable; a portable zip
  and a per-user Inno Setup installer with an uninstall entry. All builds are
  unsigned, so SmartScreen may warn.

Everything else in the matrix is library- or model-level. That includes forms,
redaction, signing, encryption, sanitization, optimisation, page operations,
incremental save, most export paths, accessibility authoring, automation, and
most standards work. See the matrix before relying on a capability.

## Install and run (Windows, user scope)

```powershell
# Pinned PDFium for tests and packaging
powershell -NoProfile -File scripts/fetch-pdfium.ps1

# Viewer and CLI
cargo build --release -p pdfxi-app --bin pdfxi-ui --features ui,pdfium
cargo build --release -p pdfxi-cli

# Portable zip, plus the installer when Inno Setup's ISCC.exe is available
powershell -NoProfile -File scripts/package.ps1 -SkipBuild
```

Artefacts are written under `target/package/`:

- `pdfxi-setup-0.1.4-windows-x86_64.exe` — per-user installer;
- `pdfxi-portable-0.1.4-windows-x86_64.zip` — portable payload.

Release notes and known limits are in
[docs/releases/0.1.4.md](docs/releases/0.1.4.md).

The installer needs no elevation and creates a Start Menu group, an optional
desktop icon, and an uninstall entry under
`HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall`. The portable
archive can be extracted and run directly; keep `pdfium.dll` next to
`pdfxi-ui.exe`.

The renderer loads PDFium from the executable's directory in release builds
(developer builds also honour `PDFIUM_PATH` and
`%LOCALAPPDATA%\pdfxi\pdfium\bin`). Documents larger than 2 GiB are rejected.

Nothing is code-signed: no certificate exists in the build environment
([blocker](orchestration/blockers/T-0202-code-signing.md)). SmartScreen
warnings are expected.

The full procedure, including uninstall and the resource-compiler note, is in
[Installing PDFXI](docs/book/src/install.md).

### Using the viewer

- **Open a PDF** or `Ctrl+O` opens the native picker; several files become tabs.
- Drag one or more PDFs from Explorer onto the window.
- `pdfxi-ui.exe <file.pdf>` opens that document.
- The gear menu sets Light/Dark/Follow system and Windows/macOS window controls;
  `--light`, `--dark`, and `--controls=windows|macos` override them for one run
  without being saved.

The viewer has no sample document, AI assistant, cloud storage, comments,
tools panel, export card, left rail, print dialog, or
layers/bookmarks/attachments/links panes. They were removed because nothing
real was behind them. See [Desktop viewer](docs/book/src/gui.md).

### CLI

```powershell
pdfxi --help
pdfxi info document.pdf
pdfxi text document.pdf
pdfxi create text notes.txt out.pdf
```

See [`docs/CLI.md`](docs/CLI.md) for the full command reference.

## Build and verify

```powershell
.\scripts\bootstrap.ps1
cargo xtask verify
cargo xtask docs
```

`cargo xtask verify` runs formatting, clippy with `-D warnings`, the workspace
tests, the `ui,pdfium` application tests, the PDFium render tests, rustdoc, and
the documentation check. It also fails when a cited feature test does not
exist. It requires PDFium; `scripts/fetch-pdfium.ps1` installs the pinned copy.

`cargo xtask goal-verify --all` is the end-condition gate. It currently stops on
the 114 unfinished feature rows and must not be reported as green.

The project is developed by an asynchronous multi-agent orchestra in one
canonical workspace. Read [`RULES.md`](RULES.md) before contributing.

## Documentation

- [Installing PDFXI](docs/book/src/install.md)
- [Desktop viewer](docs/book/src/gui.md)
- [Plan](docs/PLAN.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Feature matrix](docs/FEATURES.md)
- [Feature status decision (ADR 0009)](docs/decisions/0009-feature-status-honesty.md)
- [Platforms](docs/PLATFORMS.md)
- [Orchestration](docs/ORCHESTRATION.md)
- [Testing](docs/TESTING.md)

## License

Original code is dual-licensed under MIT or Apache-2.0. Third-party components
retain their own licenses; see `NOTICE`. PDFXI is not affiliated with Adobe.
