---
description: "PDF object model parser and CLI lane"
mode: subagent
model: opencode-go/hy4-flash-free
steps: 140
permissions:
  - action: edit
    resource: "*"
    effect: deny
  - action: edit
    resource: "crates/pdfxi-core/**"
    effect: allow
  - action: edit
    resource: "crates/pdfxi-cli/**"
    effect: allow
  - action: edit
    resource: "docs/evidence/core/**"
    effect: allow
  - action: edit
    resource: "orchestration/feature-updates/**"
    effect: allow
  - action: edit
    resource: "crates/pdfxi-document/**"
    effect: allow
  - action: edit
    resource: "docs/evidence/hardening/**"
    effect: allow
  - action: edit
    resource: "orchestration/blockers/**"
    effect: allow
  - action: shell
    resource: "*"
    effect: allow
  - action: websearch
    resource: "*"
    effect: allow
  - action: subagent
    resource: "*"
    effect: allow
  - action: shell
    resource: "git checkout *"
    effect: deny
  - action: shell
    resource: "git switch *"
    effect: deny
  - action: shell
    resource: "git worktree *"
    effect: deny
  - action: shell
    resource: "git reset --hard *"
    effect: deny
  - action: shell
    resource: "git clean *"
    effect: deny
  - action: shell
    resource: "git stash *"
    effect: deny
  - action: shell
    resource: "git clone *"
    effect: deny
---

# Core lane

Own `pdfxi-core` and `pdfxi-cli`: object model, xref tables/streams, filters,
crypto hooks, incremental save, and the headless CLI.

- Keep the core UI-free and rendering-free.
- Parse untrusted input with bounds checks and fuzz targets.
- Every public item has rustdoc.
- Run `cargo test -p pdfxi-core -p pdfxi-cli` with
  `CARGO_TARGET_DIR=target/lanes/core`.
- Write a feature-update fragment for every verified feature.
