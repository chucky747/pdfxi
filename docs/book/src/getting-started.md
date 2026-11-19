# Getting started

PDFXI is a clean-room, fully documented Rust functional remake of Adobe
Acrobat XI Pro. It is built as a headless core plus feature crates, a command-line
binary (`pdfxi`), and an optional desktop shell. There are no Adobe assets,
services, or trademarks, and normal operation needs no network access.

> **Status:** milestone M1 (parse, render, navigate, search, text, CLI, save) is
> verified; milestone M2 (authoring) and later milestones are landing row by
> row. A capability is only "shipped" when its row in the
> [feature matrix](../../FEATURES.md) is `verified`. The pages in this book say
> which rows are implemented and which are still in progress.

## Prerequisites

- A pinned stable Rust toolchain with `rustfmt` and `clippy` (see
  `rust-toolchain.toml`).
- On Windows, a working linker. `scripts/bootstrap.ps1` locates or installs the
  toolchain non-interactively.
- PDFium is optional. It is only needed for raster output (`pdfxi render`) and
  for the PDFium-backed renderer in the desktop shell.

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
| Encrypt, sign, and redact | [Security](security.md) |
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
cargo xtask goal-verify --milestone M1
```

Only a green `cargo xtask goal-verify --milestone <milestone>` counts as done.
The full contract is in [GOAL.md](../../GOAL.md).
