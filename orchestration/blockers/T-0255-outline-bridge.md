# T-0255 blocker: no real-PDF outline reader/writer bridge

Status: library gap resolved by T-0255A. The bookmarks app pane and the
document-properties dialog remain T-0255 app-lane work (T-0255 is `queued` and
now depends on T-0255A); the properties dialog was always independent.

## Resolution (T-0255A)

`crates/pdfxi-edit/src/navigation/document_outline.rs` now supplies
`read_outline(&Document)` and `write_outline(&mut Document, &Outline)`, closing
the exact gap described below on real PDFs. Evidence:
`docs/evidence/hardening/T-0255A.md` - 13 `document_outline` tests, a
read -> edit -> write -> PDFium-reopen round trip on the PowerPoint fixture, a
no-`/Outlines` case that gains one on first add, and typed errors for malformed
trees. PDFium has no bookmark traversal binding in this workspace, so it is the
independent file-validity check while PDFXI asserts the outline. The visible
bookmarks pane and properties dialog are still outstanding app work.

## Exact gap

`crates/pdfxi-edit/src/navigation/{outline.rs,edit.rs}` provides an id-based
`Outline`/`BookmarkSummary` model, `insert_bookmark`/`rename_bookmark`/
`remove_bookmark`/`move_bookmark`, and `outline_to_dict`/`dict_to_outline`.
It does not provide a bridge to and from a real `pdfxi_core::Document`:

- no reader that walks the catalog `/Outlines` tree, resolves `/Dest`/`/A`
  page references to zero-based page indexes, and builds a `BookmarkSummary`;
- no writer that takes an edited `Outline` and installs it as the catalog
  `/Outlines` dictionary with the real page references, preserving or creating
  the `/Parent`/`/First`/`/Last`/`/Next`/`/Prev` links.

The app's `bookmarks.rs` `BookmarkInfo` requires a `Destination`, and the app's
`BookmarkModel` is built from a `BookmarkSummary`, so items without a
destination cannot be represented; a real outline reader must supply the
destination or reject the item honestly.

## Consequence

The T-0255 app lane cannot implement add/rename/move/delete persistence from
`crates/pdfxi-app/**` alone. It stopped after adding a partial
`BookmarkOpen` command and an empty `bookmarks_panel` module; T-0245B removes
those stubs so the tree compiles.

## Next viable step

Split a library slice (lane author/core, `crates/pdfxi-edit/src/navigation/**`
or `crates/pdfxi-core/src/**`) that:

1. parses a real document's `/Outlines` into the existing `Outline` model,
   resolving page references through the document's page tree;
2. serializes an edited `Outline` back into the catalog, with round-trip tests
   on the real fixtures (read → edit → write → reopen with PDFium → same order);
3. handles a document with no `/Outlines` (create on first add) and a malformed
   outline (typed error, no panic).

Then T-0255's UI/persistence and the properties dialog proceed in the app lane.
