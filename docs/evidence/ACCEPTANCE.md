# Acceptance criteria tracker

Source: `docs/GOAL.md`. A criterion is checked only when the listed command
output or file evidence is current at HEAD. This file is the integrator's
single view of the end condition; task evidence lives in `docs/evidence/`.

Last updated: 2026-11-19 (M1 verified; 13 verified feature rows of 140).

| ID | Criterion | Status | Evidence | Blocker |
|---|---|---|---|---|
| AC-01 | Reproducible bootstrap | partial | `scripts/bootstrap.ps1`, `scripts/env.ps1`, `docs/decisions/0001-toolchain.md`; user-local Rust/GCC/MinGit bootstrap works, winget elevation path not exercised | Needs a clean-machine dry run record |
| AC-02 | Build quality | partial | per-crate `clippy -D warnings` green for core, render, app, text, edit, forms, security, convert, standards, automation, testkit, cli, xtask; workspace-wide gate pending M1 exit | `cargo build --workspace --release` not yet run end-to-end |
| AC-03 | Tests | partial | unit/integration tests across all lanes; doctests host-limited (ADR-0005/`docs/ASSUMPTIONS.md`); fuzz targets not yet created | Fuzz smoke and full profile pending |
| AC-04 | Feature parity | not met | `docs/features.toml` 140 rows, 13 verified | 127 rows open |
| AC-05 | Performance | not met | raw timings only (`bench_open` in T-0024/T-0030) | Budgets in `docs/TESTING.md` not yet measured |
| AC-06 | Security | partial | encryption/permission/signature model with tests (T-0018) | Real ciphers, signing, redaction pending |
| AC-07 | Accessibility | not met | reading-order model only (T-0013) | Tagged PDF, PDF/UA checks, accesskit pending |
| AC-08 | Documentation | partial | rustdoc on all crates, mdBook skeleton, ADRs 0001â€“0006, `cargo xtask docs` green | CLI reference and full mdBook content pending |
| AC-09 | Orchestration evidence | partial | board, claims, 22 verified tasks, per-task evidence files, memory inbox notes | Every verified row needs a completed task; audit at goal end |
| AC-10 | Packaging and legal | partial | `docs/THIRD_PARTY.md`, `deny.toml`, PDFium license retained out-of-tree | Installer/portable archive not built |

## Rule

No row moves to `partial` or `met` without pasted command output or an
in-repo artifact referenced from this table. `cargo xtask goal-verify --all`
remains the final authority.
