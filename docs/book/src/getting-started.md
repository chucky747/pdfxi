# Getting started

PDFXI is a clean-room, fully documented Rust functional remake of Adobe
Acrobat XI Pro. It is built as a headless core plus feature crates, a command-line
binary (`pdfxi`), and an optional desktop shell. There are no Adobe assets,
services, or trademarks, and normal operation needs no network access.

> **Status:** 

> `replaced` rows (ADR 0009). Only `verified` means an end user can reach the
> capability on a real document. M1 is **not** fully verified after the honest
> reclassification: many viewer rows that were demonstrated with demo data are
> `model`, and the milestone gates for M1-M7 require rows that are still
> `library` or `model`. `cargo xtask goal-verify --all` reports 115 unfinished
> rows and does not print `GOAL VERIFIED`. The pages in this book state which
> rows are which; the [feature matrix](../../FEATURES.md) is authoritative.

## Prerequisites

- A pinned stable Rust toolchain with `rustfmt` and `clippy` (see
  `rust-toolchain.toml`).
- On Windows, a working linker. `scripts/bootstrap.ps1` locates or installs the
  toolchain non-interactively.
- PDFium is needed for raster output (`pdfxi render`) and for the desktop
  viewer. Fetch the pinned build with `scripts/fetch-pdfium.ps1`. Release
  builds load `pdfium.dll` only from the executable's directory; developer
  builds and tests additionally honour `PDFIUM_PATH` and
  `%LOCALAPPDATA%\pdfxi\pdfium\bin`. Documents larger than 2 GiB are rejected
  before loading.

## Build

```text
cargo build --workspace
cargo build -p pdfxi-cli                    # no PDFium dependency
cargo build -p pdfxi-cli --features pdfium  # enables `pdfxi render`
```

Use a lane-local target directory when working alongside other agents:

```text
$env:CARGO_TARGET_DIR = 'target/lanes/docs'
```

`CARGO_TARGET_DIR` is build-only; the installed binary does not read it.

## First commands

The committed fixture `fixtures/minimal.pdf` is a one-page, 595-byte PDF whose
text is `Hello, PDFXI!`. Run the commands from the repository root:

```powershell
pdfxi info .\fixtures\minimal.pdf
# file: fixtures/minimal.pdf
# pdf_version: 1.7
# bytes: 595
# pages: 1

pdfxi text .\fixtures\minimal.pdf
# Hello, PDFXI!

pdfxi search .\fixtures\minimal.pdf PDFXI
# page 1 offset 7 PDFXI

pdfxi info --json .\fixtures\minimal.pdf
```

`pdfxi render` requires `PDFIUM_PATH` (or the standard per-user PDFium install)
and a `pdfium` build. See the [CLI reference](cli.md) for every command and flag,
or the authoritative [docs/CLI.md](../../CLI.md) for exact JSON shapes.

## Where to go next

| Goal | Page |
|---|---|
| Drive PDFXI from scripts and CI | [CLI reference](cli.md) |
| Create and combine PDFs, edit pages, add headers, watermarks, print | [Authoring](authoring.md) |
| Fill, exchange, and flatten forms | [Forms](forms.md) |
| Encrypt, sign, redact, and review privacy | [Security](security.md), [Privacy and local data](privacy.md) |
| Tags, reading order, and tagged PDF | [Accessibility](accessibility.md) |
| Actions, droplets, and the JavaScript subset | [Automation](automation.md) |
| Preflight and standards profiles | [Standards and preflight](standards.md) |

Background material: [architecture](architecture.md),
[goal and plan](plan.md), [testing and performance](testing.md), and the
[feature matrix](features.md).

## Verify your work

```text
cargo xtask verify
cargo xtask docs
cargo xtask goal-verify --all
```

`cargo xtask verify` is the working gate for a change: format, clippy with
`-D warnings`, workspace tests, the `ui,pdfium` tests, the PDFium render tests,
rustdoc, the documentation check, and a check that every cited feature test
exists. It needs PDFium (see above).

`cargo xtask goal-verify --all` is the end-condition gate. It currently fails on
the 115 feature rows that are not `verified` or `replaced`, so it is not green and AC-04
is not met. The full contract is in [GOAL.md](../../GOAL.md) and the
classification rule is in
[ADR 0009](../../decisions/0009-feature-status-honesty.md).
