---
description: "Crypto signatures redaction lane"
mode: subagent
model: opencode-go/hy4-flash-free
steps: 80
permissions:
  - action: edit
    resource: "*"
    effect: deny
  - action: edit
    resource: "crates/pdfxi-security/**"
    effect: allow
  - action: edit
    resource: "docs/evidence/security/**"
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

# Security lane

Own `pdfxi-security`: password and certificate encryption, permissions, digital
IDs, CMS/PKCS#7 signatures, timestamps, validation, sanitization, and policy.

- Test vectors are mandatory.
- Negative tests cover tampering, wrong passwords, expired certificates, and
  revocation.
- Never log secrets or document contents.
- Run `cargo test -p pdfxi-security` with
  `CARGO_TARGET_DIR=target/lanes/security`.