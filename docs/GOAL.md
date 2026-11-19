# GOAL

## Objective

Build **PDFXI**, a performant, fully documented Rust functional remake of Adobe
Acrobat XI Pro, using `docs/PLAN.md`, `docs/ARCHITECTURE.md`, and
`docs/features.toml` as the execution contract. The implementation is a
clean-room replacement: no Adobe code, assets, services, or trademarks.

## End condition

```text
cargo xtask goal-verify --all
```

must exit `0` in a clean worktree at `HEAD` and print:

```text
GOAL VERIFIED: <git-sha> <unix-time>
```

The reviewer must be shown the complete command output. A claim without evidence
does not count.

## Acceptance criteria

### AC-01 — Reproducible bootstrap

A fresh Windows machine can run `scripts/bootstrap.ps1` without interactive
decisions. The script installs or locates Git, Rust, and a working linker, then
`cargo xtask goal-verify --milestone M0` passes.

### AC-02 — Build quality

`cargo build --workspace --release` succeeds. `cargo fmt --all --check` and
`cargo clippy --workspace --all-targets --all-features -- -D warnings` pass with
no warnings in first-party code.

### AC-03 — Tests

Fast unit/integration tests, doc tests, the full profile, fuzz smoke tests, and
the corpus smoke suite pass. No test requires network access, a GUI, Adobe
software, or a secret.

### AC-04 — Feature parity

Every row in `docs/features.toml` is `verified` or `replaced` with a local
functional equivalent. No P0, P1, or P2 row may be deferred without an ADR.
`docs/FEATURES.md` and the generated matrix agree.

### AC-05 — Performance

The budgets in `docs/TESTING.md` pass: open, first-page render, scroll, search,
OCR, incremental save, memory, and batch throughput. No tracked benchmark
regresses by more than 10%.

### AC-06 — Security

Password and certificate encryption, permissions, signature create/validate,
redaction, sanitization, and JavaScript sandbox tests pass. Redaction removes
content from text and image extraction, not only from the display.

### AC-07 — Accessibility

Tagged PDF, reading order, alt text, PDF/UA checks, keyboard navigation, high
contrast, and a screen-reader smoke test pass.

### AC-08 — Documentation

Rustdoc, mdBook, CLI reference, architecture, ADRs, feature docs, and
`docs/THIRD_PARTY.md` build with warnings denied and valid links.

### AC-09 — Orchestration evidence

The board contains a completed task for every delivered feature. Every verified
row has tests, docs, and an evidence path. No worktree, checkout, branch, reset,
or stash violation exists in the audit log. The worktree is clean.

### AC-10 — Packaging and legal

A reproducible Windows package (portable archive and installer) is produced.
`LICENSE`, `NOTICE`, `deny.toml`, third-party notices, and the clean-room audit
are complete.

## Campaigns

| Campaign | Milestone | Focus |
|---|---|---|
| C0 | M0 | Bootstrap, rules, memory, orchestration, workspace, goal gate |
| C1 | M1 | Vertical slice: parse, render, navigate, search, text, CLI, save |
| C2 | M2 | Authoring: create, combine, split, pages, annotations, export |
| C3 | M3 | Editing, forms, signatures, redaction, sanitization |
| C4 | M4 | Encryption, standards, preflight, color, optimization |
| C5 | M5 | Conversion, Office/HTML, OCR, scanner, print driver, portfolios |
| C6 | M6 | Actions, review, accessibility, compare, 3D/multimedia |
| C7 | M7 | Hardening, packaging, parity audit, release |

## Stop rule

Do not stop for a user decision. Record the blocker in
`orchestration/blockers/`, continue every independent lane, and let the goal
controller resume after round caps. The only valid terminal states are
`GOAL VERIFIED` or an explicitly documented external blocker that prevents every
lane.
