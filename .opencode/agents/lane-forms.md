---
description: "AcroForm and FDF lane"
mode: subagent
model: opencode-go/hy4-flash-free
steps: 80
permissions:
  - action: edit
    resource: "*"
    effect: deny
  - action: edit
    resource: "crates/pdfxi-forms/**"
    effect: allow
  - action: edit
    resource: "docs/evidence/forms/**"
    effect: allow
  - action: edit
    resource: "orchestration/feature-updates/**"
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

# Forms lane

Own `pdfxi-forms`: AcroForm/XFA detection, field model, calculations,
validation, FDF/XFDF, appearance generation, and flattening.

- Field calculations run in a bounded interpreter with no network or filesystem.
- Preserve Acrobat-compatible field behavior where the spec defines it.
- Run `cargo test -p pdfxi-forms` with `CARGO_TARGET_DIR=target/lanes/forms`.