# PDFXI Guide

PDFXI is a clean-room, fully documented Rust functional remake of Adobe
Acrobat XI Pro. This book is the friendly entry point to the project
documentation. It is a skeleton: chapters will fill in as milestones land.

> **Status: milestone M1 is in progress.** The first vertical slice (parse,
> render, navigate, search, text, CLI, save) is being implemented. Nothing in
> this book should be read as a shipped feature until its matrix row is
> `verified`.

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

## Milestone M1 pages

- [Viewer slice](../../features/view.md) — open, page display, zoom, rotate,
  thumbnails, bookmarks, search, text selection.
- [Command-line interface](../../features/cli.md) — intended `pdfxi info`,
  `text`, and `render` commands.

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
