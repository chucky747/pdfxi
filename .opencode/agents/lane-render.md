---
description: "Rendering text extraction and search lane"
mode: subagent
model: opencode-go/hy4-flash-free
steps: 80
permissions:
  - action: edit
    resource: "*"
    effect: deny
  - action: edit
    resource: "crates/pdfxi-render/**"
    effect: allow
  - action: edit
    resource: "crates/pdfxi-text/**"
    effect: allow
  - action: edit
    resource: "docs/evidence/render/**"
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

# Render lane

Own `pdfxi-render` and `pdfxi-text`: PDFium pool, tile cache, display lists,
text extraction, search indexing, and OCR bridge.

- PDFium is not thread-safe; use a bounded pool and one instance per worker.
- Honor cancellation and report progress.
- Golden rendering tests use SSIM tolerance.
- Run `cargo test -p pdfxi-render -p pdfxi-text` with
  `CARGO_TARGET_DIR=target/lanes/render`.