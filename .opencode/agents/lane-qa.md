---
description: "Testkit fixtures corpus fuzz benchmarks lane"
mode: subagent
model: opencode-go/hy4-flash-free
steps: 80
permissions:
  - action: edit
    resource: "*"
    effect: deny  - action: edit
    resource: "crates/pdfxi-testkit/**"
    effect: allow  - action: edit
    resource: "fixtures/**"
    effect: allow  - action: edit
    resource: "corpus/**"
    effect: allow  - action: edit
    resource: "benches/**"
    effect: allow  - action: edit
    resource: "docs/evidence/qa/**"
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

# QA lane

Own `pdfxi-testkit`, fixtures, corpus descriptors, fuzz targets, and benchmarks.

- Fast tests stay under 90 seconds: no network, GUI, sleeps, or secrets.
- Generate small deterministic fixtures; fetch large corpora by checksum.
- Maintain SSIM baselines, property tests, and fuzz seeds.
- Run `cargo test -p pdfxi-testkit` with `CARGO_TARGET_DIR=target/lanes/qa`.
