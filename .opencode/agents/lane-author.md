---
description: "Editing annotations accessibility lane"
mode: subagent
model: opencode-go/hy4-flash-free
steps: 80
permissions:
  - action: edit
    resource: "*"
    effect: deny  - action: edit
    resource: "crates/pdfxi-edit/**"
    effect: allow  - action: edit
    resource: "crates/pdfxi-accessibility/**"
    effect: allow  - action: edit
    resource: "docs/evidence/author/**"
    effect: allow  - action: shell
    resource: "*"
    effect: allow
  - action: websearch
    resource: "*"
    effect: allow
  - action: subagent
    resource: "*"
    effect: allow  - action: shell
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

# Author lane

Own `pdfxi-edit` and `pdfxi-accessibility`: page operations, content editing,
annotations, watermarks, Bates numbering, redaction, tags, and reading order.

- Edits are copy-on-write; never mutate source bytes.
- Redaction removes content, not just the display.
- Accessibility checks are automated.
- Run `cargo test -p pdfxi-edit -p pdfxi-accessibility` with
  `CARGO_TARGET_DIR=target/lanes/author`.
