# PDFXI Guide

PDFXI is a clean-room, fully documented Rust functional remake of Adobe
Acrobat XI Pro. This book is the friendly entry point to the project
documentation: a getting-started path, task-oriented chapters for the tools that
are actually reachable in the current build, and a reference section.

> **Status.** Only a `verified` row in the [feature matrix](../../FEATURES.md)
> means an end user can reach a feature; `replaced` rows are recorded in
> [ADR 0009](../../decisions/0009-feature-status-honesty.md). The matrix is not
> fully green, so nothing in this book should be read as a promise beyond the
> row it names. Each chapter states the limits of what it describes.

## Start here

- [Installing PDFXI](install.md) — installer, portable zip, and the pinned
  PDFium policy.
- [Getting started](getting-started.md) — build, run the first commands against
  `fixtures/minimal.pdf`, and find the right chapter.
- [Desktop viewer](gui.md) — the shipped shell: which surfaces exist, and which
  commands are honestly unavailable.
- [CLI reference](cli.md) — commands, flags, exit codes, JSON output, and
  environment variables, with a link to the authoritative [docs/CLI.md](../../CLI.md).

## Using the viewer

- [Open, save, and print](open-save-print.md) — picker, drag and drop,
  password-protected files, atomic save, and printing.
- [Library: Recent, Starred, Files](library.md) — the default folder, recent
  list, stars, storage meter, and the Shared placeholder.
- [Pages panel](pages-panel.md) — thumbnails, list and grid views, and
  navigation.
- [Comments](comments.md) — text notes, highlights, replies, resolve, and undo.
- [Fill & Sign](fill-and-sign.md) — filling form fields, reset, flatten, and
  XFDF/FDF exchange.
- [Search](search.md) — find options, match navigation, and the result cap.

## Security and privacy

- [Signing and verification](signing.md) — the certificate Sign card, its
  visible overlay block, the green `✓ E-signed` chip, and what the readout
  proves.
- [Signatures: integrity and identity](security/signatures.md) — digest validity
  versus identity, trust anchors, tamper behaviour, and the badge predicate.
- [Assistant](assistant.md) — the AI Assistant panel and the session lockdown.
- [What the assistant sends](security/assistant-data.md) — the exact probe, the
  fenced excerpt bounds, and what is never sent.
- [Redaction](security/redaction.md) — what marking removes, what an independent
  reader can still recover, and how removal is verified.
- [Security](security.md) — encryption, permissions, signatures, redaction, and
  untrusted input.
- [Privacy and local data](privacy.md) — the local files PDFXI keeps and what is
  never sent.

## Reference

- [Third-party notices](notices.md) — the bundled font and the Rust crates used
  by the assistant, with pointers to the authoritative notices.
- [Goal and plan](plan.md) · [Architecture](architecture.md) ·
  [Feature matrix](features.md) · [Testing and performance](testing.md) ·
  [Project memory](memory.md).
- [Goal and end condition](../../GOAL.md) · [Plan](../../PLAN.md) ·
  [Architecture](../../ARCHITECTURE.md) · [Feature matrix](../../FEATURES.md) ·
  [Third-party components](../../THIRD_PARTY.md).

The narrative companions are [viewer slice](../../features/view.md) and
[CLI overview](../../features/cli.md).

## What this build does not do

These are stated once here and repeated where they matter, so a page cannot be
mistaken for a claim that the tool exists:

- **Edit PDF**, **Export**, and **Organize Pages** are unavailable; their
  commands report that they are not available in this build.
- **Protect PDF** and **Redact** are unavailable. The protection and redaction
  libraries are real and tested, but no viewer command invokes them until T-0263.
- **Shared review** is not implemented. The Shared panel appears only when a
  shared folder is configured, and it is a placeholder.
- An encrypted document opens read-only and cannot be saved or signed;
  re-encryption on save does not exist.
- Signatures prove integrity of the signed bytes, not the identity of the
  signer: no trust anchor is configured in this build.

## How to verify

```text
cargo xtask verify
cargo xtask docs
cargo xtask goal-verify --milestone M1
```

`cargo xtask docs` checks that the required documentation pages exist. Only a
green `cargo xtask goal-verify --milestone <milestone>` counts as done.
