# PDFXI Guide

PDFXI is a clean-room, fully documented Rust functional remake of Adobe
Acrobat XI Pro. This book is the friendly entry point to the project
documentation: it has a getting-started path, task-oriented M2 chapters, and a
reference section.

> **Status:** milestone M1 (parse, render, navigate, search, text, CLI, save) is
> verified; M2 authoring and later milestones are landing row by row. Nothing in
> this book should be read as a shipped feature until its matrix row is
> `verified`. Each chapter states the status of the rows it covers.

## Start here

- [Getting started](getting-started.md) — build, run the first commands against
  `fixtures/minimal.pdf`, and find the right chapter.
- [CLI reference](cli.md) — commands, flags, exit codes, JSON output, and
  environment variables, with a link to the authoritative [docs/CLI.md](../../CLI.md).

## Task-oriented chapters

- [Authoring](authoring.md) — create from text and images, page boxes,
  headers/footers, watermarks, optimize, print.
- [Forms](forms.md) — the AcroForm model, XFDF/FDF exchange, and flattening.
- [Security](security.md) — AES-256, CMS signatures, X.509 digital IDs, and
  redaction.
- [Accessibility](accessibility.md) — tag trees, reading order, and the tagged
  writer.
- [Automation](automation.md) — actions, droplets, and the sandboxed JavaScript
  subset.
- [Standards and preflight](standards.md) — profiles, finding codes, and opt-in
  fixups.

## Reference map

The canonical documents live at the repository root of `docs/`. Each link below
also has a short in-book stub reachable from [SUMMARY.md](SUMMARY.md).

- [Goal and end condition](../../GOAL.md) — objective, acceptance criteria,
  campaigns, and stop rule.
- [Plan](../../PLAN.md) — milestones M0–M7, lanes, and risks.
- [Architecture](../../ARCHITECTURE.md) — crate boundaries, core contracts,
  UI/concurrency model.
- [Feature matrix](../../FEATURES.md) — human summary; the machine-checkable
  matrix is `docs/features.toml`.
- [Testing and performance](../../TESTING.md) — test pyramid, budgets, corpus.
- [Project memory](../../MEMORY.md) — durable project notes and pointers.

Narrative companions: [viewer slice](../../features/view.md) and
[CLI overview](../../features/cli.md).

## Decisions

Architectural decisions are recorded as ADRs in
`docs/decisions/` (for example the toolchain, PDF engine, and UI choices).

## How to verify

```text
cargo xtask verify
cargo xtask docs
cargo xtask goal-verify --milestone M1
```

`cargo xtask docs` checks that the required documentation pages exist. Only a
green `cargo xtask goal-verify --milestone <milestone>` counts as done.
