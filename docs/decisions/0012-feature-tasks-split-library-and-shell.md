# ADR 0012 - Feature tasks split into a library task and a shell task

- Status: accepted
- Date: 2026-11-20

## Context

Phase 2 of `docs/campaigns/2026-11-20-feature-acceptance.md` adds comments,
organise, bookmarks, edit-PDF, forms, signing, request-signature, redaction,
protect, export and convert-to-pdf. The obvious task shape - "own the feature
crate and wire it into the viewer" - gives every Phase 2 task the same write
surface: `crates/pdfxi-app/src/shell/**`, plus the `shell/mod.rs`,
`shell/commands.rs` and `ui.rs` registration points.

That shape collides by construction. Two lanes editing
`crates/pdfxi-app/src/shell/` at once cannot both hold a claim under RULES.md
section 4, and serialising whole features would make the app lane the single
bottleneck for the whole phase. It would also mix two different kinds of risk in
one task: PDF semantics on real bytes, and egui shell wiring.

## Decision

Every Phase 2 feature is split into two tasks against the same acceptance item.

### 1. Library task

- Owns only its crate: `crates/<crate>/**` plus its own tests, plus
  `docs/evidence/hardening/<task>.md` and
  `orchestration/feature-updates/<task>.toml`. It never owns
  `crates/pdfxi-app/**`.
- Exposes the feature through the `pdfxi-document` session of ADR 0010
  (`docs/decisions/0010-document-session-and-write-path.md`): the entry point
  takes the session (or the document bytes and password it holds), mutates
  through `pdfxi-core`, and returns something the shell can call without
  re-implementing PDF semantics.
- Proves the feature on real fixtures (`fixtures/realworld/**`, read-only) and
  saves and reopens the result through an independent reader where the ADR 0010
  shape requires it. Generated test bytes are allowed for unit cases but are not
  the proof of the feature.
- Reports feature-matrix rows as `library` when those rows were previously
  `model`; it does not promote them to `verified`.

### 2. Shell integration task

- Lane `app`. Owns `crates/pdfxi-app/src/shell/<feature>*.rs` plus the one
  registration edit in each of `shell/mod.rs`, `shell/commands.rs`,
  `shell/capabilities.rs` and `ui.rs`.
- Calls the library task's API. It may not change library semantics: no parsing,
  no PDF mutation logic, no duplicate of a rule the library already owns. A
  missing capability is a library follow-up, not a shell patch.
- Carries the acceptance item to the end user and lands the feature-matrix rows:
  a row becomes `verified` only when its shell task lands.

### 3. Ordering

Library task first; its shell task opens only after the library task is
`verified` and its claim is released. The shell task depends on the library task
in `depends_on`, so `cargo xtask ready` enforces the order. Both tasks cite the
same acceptance item from `docs/design/FEATURE_ACCEPTANCE.md`; neither is done
while the item is `open`.

### 4. Pairs

| Feature | Library lane / crate | Shell lane | Acceptance items |
|---|---|---|---|
| Comments and markup | author / `pdfxi-edit` | app | A-11, A-15, A-18, A-19, A-24, A-32 |
| Organise pages and bookmarks | core + author / `pdfxi-core`, `pdfxi-edit` | app | A-13, A-14, A-26, A-29, O-03 |
| Edit PDF | author / `pdfxi-edit` | app | A-17, A-23 |
| Forms | forms / `pdfxi-forms` | app | A-25, O-05 |
| Signing | security / `pdfxi-security` | app | A-20 |
| Request signature | security + app / `pdfxi-security` | app | A-31 |
| Redaction | author + security / `pdfxi-edit`, `pdfxi-security` | app | O-06 |
| Protect | security / `pdfxi-security` | app | A-30, O-04 |
| Export | convert / `pdfxi-convert` | app | A-27 |
| Convert to PDF | convert / `pdfxi-convert` | app | A-28 |

The ids in `docs/design/FEATURE_ACCEPTANCE.md` name the feature. The library
task opens first and may reuse the acceptance id; the serialised shell task is
opened later with its own id once its library task is verified.

## Consequences

- Two claims, never overlapping: library tasks meet only on shared crates, and
  those pairs are serialised (`edit`+`security` for redaction, `core`+`edit` for
  organise). Shell tasks meet only in the app lane and are serialised there.
- The matrix tells the truth about reachability: a library-only row stays
  `library` until its shell task lands, so `goal-verify` cannot go green on
  library work alone (ADR 0009).
- The app lane cannot start a shell task early to "unblock" the UI; it waits for
  the library acceptance, which is the point.
- Every library task has the same evidence shape: real fixture, session call,
  save, reopen, assert. Reviewers can compare tasks directly.

## Follow-up work

1. Open wave A of the library tasks (comments, forms, export, signing) now, so
   they can start as soon as T-0214 lands the session.
2. Open each shell task only after its library task is verified; give it the
   `depends_on` edge and the acceptance id.
3. Add the Phase 2 rows for the remaining features (organise, bookmarks,
   edit-PDF, protect, redaction, request-signature, convert-to-pdf) under the
   same split when their wave opens.
