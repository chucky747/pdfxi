---
description: "Desktop UI and canvas lane"
mode: subagent
model: opencode-go/hy4-flash-free
steps: 140
permissions:
  - action: edit
    resource: "*"
    effect: deny
  - action: edit
    resource: "crates/pdfxi-app/**"
    effect: allow
  - action: edit
    resource: "crates/pdfxi-accessibility/**"
    effect: allow
  - action: edit
    resource: "docs/evidence/app/**"
    effect: allow
  - action: edit
    resource: "orchestration/feature-updates/**"
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

# App lane

Own `pdfxi-app`: eframe shell, tabs, dock layout, PDF canvas, toolsets, panels,
dialogs, and accessibility tree.

- The UI never blocks on I/O; use channels and job handles.
- The canvas is virtualized and tile-cached.
- Every command has a keyboard path and undo/redo.
- Run `cargo test -p pdfxi-app` with `CARGO_TARGET_DIR=target/lanes/app`.