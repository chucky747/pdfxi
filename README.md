# PDFXI

PDFXI is a clean-room, Rust-native functional remake of Adobe Acrobat XI Pro:
view, create, edit, export, secure, sign, review, OCR, automate, and print PDFs
without Adobe services or code.

## Status

Delivery is complete at the library and CLI level: **140 feature rows (137
verified, 3 local replacements)** and `cargo xtask goal-verify --all` prints
`GOAL VERIFIED` at the current HEAD. The authoritative plan is
[`docs/PLAN.md`](docs/PLAN.md), the goal contract is
[`docs/GOAL.md`](docs/GOAL.md), and the live board is
[`orchestration/BOARD.md`](orchestration/BOARD.md).

The desktop viewer (`pdfxi-ui`) opens real PDFs, renders pages through PDFium,
and ships in the portable package. Most of the remaining feature surface is
library/CLI-level; wiring more of it into the GUI is tracked in the release
plan.

## Install and run (Windows, user scope)

Build the portable package and install it without elevation:

```powershell
cargo build --workspace --release
cargo build --release -p pdfxi-app --bin pdfxi-ui --features ui,pdfium
powershell -NoProfile -File scripts/package.ps1 -SkipBuild -TargetDir target
powershell -NoProfile -File target\package\staging\install.ps1 `
    -TargetDir "$env:LOCALAPPDATA\Programs\PDFXI" -DesktopShortcut
```

That creates `%LOCALAPPDATA%\Programs\PDFXI`, a Start-Menu **PDFXI** viewer
link, **PDFXI CLI**, and an optional Desktop shortcut. The portable archive is
`target\package\pdfxi-portable-<version>-windows-x86_64.zip` with a SHA-256
sidecar and manifest.

The renderer loads PDFium from `PDFIUM_PATH` or
`%LOCALAPPDATA%\pdfxi\pdfium\bin\pdfium.dll`; without it the viewer runs with
placeholder pages and the CLI still works.

### Using the viewer

- **Open…** in the toolbar opens the path dialog; type or paste a PDF path.
- Drag one or more PDFs from Explorer onto the window.
- Launching `pdfxi-ui.exe <file.pdf>` opens that document (also what the
  desktop shortcut and "Open with" pass).
- The thumbnail pane, fit/zoom, rotation, find bar, text selection/copy, print
  dialog, layers, bookmarks and attachments are all available.

### CLI

```powershell
pdfxi --help
pdfxi info document.pdf
pdfxi text document.pdf
pdfxi create text notes.txt out.pdf
```

See [`docs/CLI.md`](docs/CLI.md) for the full command reference.

## Build from source

```powershell
.\scripts\bootstrap.ps1
cargo xtask verify
cargo xtask goal-verify --all
```

The project is developed by an asynchronous multi-agent orchestra: a DeepSeek
V4.1 Flash orchestrator and parallel lane agents in one canonical workspace.
Read [`RULES.md`](RULES.md) before contributing.

## Documentation

- [Plan](docs/PLAN.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Feature matrix](docs/FEATURES.md)
- [Platforms](docs/PLATFORMS.md)
- [Orchestration](docs/ORCHESTRATION.md)
- [Testing](docs/TESTING.md)
- [Memory](docs/MEMORY.md)
- [GUI guide](docs/book/src/gui.md)

## License

Original code is dual-licensed under MIT or Apache-2.0. Third-party components
retain their own licenses; see `NOTICE`. PDFXI is not affiliated with Adobe.
