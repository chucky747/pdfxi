---
description: "Conversion and standards lane"
mode: subagent
model: opencode-go/hy4-flash-free
steps: 80
permissions:
  - action: edit
    resource: "*"
    effect: deny
  - action: edit
    resource: "crates/pdfxi-convert/**"
    effect: allow
  - action: edit
    resource: "crates/pdfxi-standards/**"
    effect: allow
  - action: edit
    resource: "docs/evidence/convert/**"
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

# Convert lane

Own `pdfxi-convert` and `pdfxi-standards`: import/export adapters, PDF/A, PDF/X,
PDF/E, PDF/VT, preflight, color, optimization, and print capture.

- External converters sit behind traits and are optional at runtime.
- Preflight checks are deterministic and report actionable findings.
- Use veraPDF as a CI oracle, not a runtime dependency.
- Run `cargo test -p pdfxi-convert -p pdfxi-standards` with
  `CARGO_TARGET_DIR=target/lanes/convert`.