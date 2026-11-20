# ADR 0010 — One document session, one write path

- Status: accepted
- Date: 2026-11-20

## Context

Every document feature of the feature-acceptance campaign
(`docs/campaigns/2026-11-20-feature-acceptance.md`) needs the same three things:
the current file bytes, the parsed document, and a way to change both so that the
next frame renders the change and the saved file contains it. Today none of that
exists in one place:

- `crates/pdfxi-app` depends only on `pdfxi-render` and `pdfxi-text`. The viewer
  owns open document bytes itself, and its edit, comment, form and signature
  panes are models fed from the UI (`docs/decisions/0009-feature-status-honesty.md`).
- `pdfxi-edit`, `pdfxi-forms` and `pdfxi-security` have no dependent crate. They
  operate on caller-supplied page, annotation or field models, not on a real
  `pdfxi_core::Document`.
- `pdfxi-core` owns the object model, the parser and incremental save (T-0207),
  and full `save()` / `optimize()` still reject cross-reference-stream sources
  (T-0213).
- PDFium is already bundled and is the renderer. It can reopen a document from
  memory, so a mutation written by `pdfxi-core` can be rendered without a file
  round-trip.

Without one owner for the open document, every feature invents its own save path,
undo becomes a per-feature afterthought, and signatures are invalidated by any
rewrite. Without one write path, the project has two PDF writers that must agree.

## Decision

1. **One session crate owns the open document.** `crates/pdfxi-document`
   (`pdfxi-document`) owns the file bytes, the parsed `pdfxi_core::Document`, the
   password and encryption parameters, the dirty flag, the current revision, and
   the undo/redo stack. The application talks only to this crate for anything that
   reads or changes a document; it does not hold bytes or a `Document` of its own.
2. **`pdfxi-core` is the only write path.** Every byte that reaches a file is
   produced by `pdfxi-core`. `pdfxi-edit`, `pdfxi-forms` and `pdfxi-security` are
   refactored from "operate on a caller-supplied model" to "operate on the real
   `Document` through the session"; they gain a `pdfxi-core` dependency (directly,
   or one session-facing trait implemented by `pdfxi-document`) instead of their
   own serializers.
3. **A mutation is an incremental append, then a PDFium reload from memory.** The
   session applies the change to the in-memory `Document`, appends an incremental
   update through `pdfxi-core` incremental save (the path T-0207 extended to
   cross-reference-stream sources), records the new revision length, and reloads
   PDFium from the resulting bytes. The bytes before the append are never
   rewritten, which is what keeps an earlier signature valid.
4. **Undo truncates to the previous revision; redo re-appends.** Because every
   mutation is an append, the previous revision is a byte length and a PDFium
   reload, not a stored snapshot. Undo drops the appended bytes and reloads; redo
   re-applies the recorded operation and reloads again. Memory use is bounded by
   the document plus the redo log, not by one full copy per step.

5. **Some operations must rewrite the whole file and never keep earlier
   revisions.** Redaction, sanitize, optimize and removing encryption cannot be
   expressed as an append that leaves the earlier bytes intact: a redacted byte
   range must not remain recoverable. The session performs a full `pdfxi-core`
   rewrite to a new file, clears the undo/redo history, and reports that the
   revision lineage starts again. The UI says so before the operation.
6. **Saves are atomic and detect external modification.** Save writes a temporary
   file in the destination directory, flushes it, and renames it over the target;
   Save As does the same at the new path. Before writing, the session compares the
   on-disk identity recorded at load (length, last-write time, and a content hash
   of the bytes it loaded) and refuses with a typed conflict error when the file
   changed underneath the editor, offering "save a copy" instead of silently
   overwriting.
7. **Escape hatch, by ADR only.** If a lane cannot get a `pdfxi-core` mutation
   through the PDFium interop check (reopen the saved bytes in PDFium and assert
   the change is present and nothing else changed) within its rounds, the
   orchestrator may approve the PDFium editing API for that one feature. That
   approval is a new ADR naming the feature and the reason; it is never a silent
   per-lane choice, and it does not create a second general write path.

## Consequences

- **`crates/pdfxi-document` becomes a prerequisite for saving, undo, comments,
  forms, signing, redaction and protect.** T-0214 creates it (load including
  RC4 / AES-128 / AES-256 decryption, mutation append, undo/redo by revision,
  atomic save and save-as, external-change detection, PDFium reload from memory).
  Phase 2 tasks consume it instead of adding ad-hoc persistence.
- **`pdfxi-core` must accept every source its sessions are opened from.** Full
  rewrite for xref-stream, object-stream and hybrid sources is T-0213; until it
  lands, `optimize`, page plans and the full-rewrite exceptions of point 5 fail on
  those sources with a typed error rather than a silent downgrade.
- **Crate dependency edges change.** `pdfxi-edit`, `pdfxi-forms` and
  `pdfxi-security` gain `pdfxi-core` (and possibly `pdfxi-document`); `pdfxi-app`
  gains `pdfxi-document`. The workspace `[workspace.dependencies]` table gains the
  entries with their first dependent.
- **Signature semantics.** Signatures stay verifiable across later edits because
  the signed byte range is never rewritten. This is the reason for point 3; a
  feature that needs a full rewrite invalidates signatures by construction and
  must say so in the UI.
- **Undo is not a snapshot store.** Operations that cannot be inverted by
  truncation (the point 5 exceptions) clear history. This is a deliberate limit,
  documented to the user at the moment it happens.
- **Tests get a fixed shape.** Every mutation test works on real PDF bytes,
  performs the mutation through the same session call the UI uses, saves, and
  reopens the result with PDFium (an independent reader) before asserting.

## Follow-up work

1. T-0214: create `pdfxi-document` with the session API and the tests above
   (encrypted and plain fixtures: mutate, undo, redo, save, reopen in PDFium).
2. T-0213: full rewrite for xref-stream, object-stream and hybrid sources, so the
   point 5 exceptions work on real-world files.
3. Per-feature ADRs where a lane takes the escape hatch in point 7.
4. Replace the string-based password check in the app with the typed PDFium error
   (`PdfiumInternalError::PasswordError`) when the session lands, so the prompt is
   driven by the real load failure.
