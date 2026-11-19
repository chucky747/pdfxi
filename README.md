# PDFXI

PDFXI is a clean-room, Rust-native functional remake of Adobe Acrobat XI Pro:
view, create, edit, export, secure, sign, review, OCR, automate, and print PDFs
without Adobe services or code.

## Status

Phase 0 (bootstrap) is in progress. The authoritative plan is
[`docs/PLAN.md`](docs/PLAN.md), the goal contract is
[`docs/GOAL.md`](docs/GOAL.md), and the live work board is
[`orchestration/BOARD.md`](orchestration/BOARD.md).

The project is developed by an asynchronous multi-agent Orchestra: a DeepSeek
V4.1 Flash orchestrator and parallel Hy4 Flash Free lane agents. There are no
worktrees or checkouts. Read [`RULES.md`](RULES.md) before contributing.

## Quick start (Windows)

```powershell
.\scripts\bootstrap.ps1
cargo xtask verify
cargo xtask goal-verify --milestone M0
```

## Documentation

- [Plan](docs/PLAN.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Feature matrix](docs/FEATURES.md)
- [Orchestration](docs/ORCHESTRATION.md)
- [Testing](docs/TESTING.md)
- [Memory](docs/MEMORY.md)

## License

Original code is dual-licensed under MIT or Apache-2.0. Third-party components
retain their own licenses; see `NOTICE`. PDFXI is not affiliated with Adobe.
