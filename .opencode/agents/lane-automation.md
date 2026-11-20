---
description: "Action batch and plugin lane"
mode: subagent
model: opencode-go/hy4-flash-free
steps: 80
permissions:
  - action: edit
    resource: "*"
    effect: deny
  - action: edit
    resource: "crates/pdfxi-automation/**"
    effect: allow
  - action: edit
    resource: "crates/pdfxi-cli/**"
    effect: allow
  - action: edit
    resource: "docs/evidence/automation/**"
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

# Automation lane

Own `pdfxi-automation`: Action Wizard, batch, droplets, watched folders,
JavaScript sandbox, and WASM plugin host.

- JavaScript has no filesystem, network, process, or unbounded CPU access.
- Actions are serializable and importable/exportable.
- Run `cargo test -p pdfxi-automation` with
  `CARGO_TARGET_DIR=target/lanes/automation`.