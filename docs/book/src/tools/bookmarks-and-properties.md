# Bookmarks and document properties

Two small document-management surfaces: the **Bookmarks** tree in the left side
panel, and the read-only **Document properties** window. Both read and write real
PDF structures; nothing is invented or defaulted
([T-0255](../../../evidence/ui/T-0255.md)).

## Bookmarks

The Bookmarks group sits at the bottom of the Pages panel when a document is
open. The tree is loaded from the document outline on open, on tab activation,
and after every edit. A missing or empty `/Outlines` shows "No bookmarks".

- A row with a destination shows **Page N**; clicking it navigates to that page.
- A row without a usable destination is weak-styled and explains the reason by
  title (missing destination, non-GoTo action, unresolved named destination,
  invalid destination, dangling page reference). It stays selectable so it can
  be renamed or deleted.

Actions, enabled only for an active writable tab:

| Action | Effect |
| --- | --- |
| Add bookmark | Root bookmark at the current page with Fit, then inline rename |
| Rename | Inline edit; Enter or Save commits, Cancel closes; empty titles rejected |
| Delete | Removes the node and its children |
| Move up / down | Reorders among siblings; disabled at the first/last sibling |

Each change clones the document, edits the outline, writes it back, and commits
through `DocumentSession::edit` plus the post-edit refresh. Save to persist it;
a rejected edit keeps the previous outline and reports the error.

## Document properties

Open the **Document properties** window from the Tools row (available only with
an open document). It shows real values: file name, page count, file size, PDF
version, `/Info` Title/Author/Subject/Keywords/Creator/Producer/CreationDate/
ModDate, whether the file is encrypted, and whether XMP is present. Absent
`/Info` values read **not set**; a failed metadata read reads **unavailable**.
Dates are shown as the raw stored text. The dialog is read-only.

## Limits

- No drag-reorder, drag-to-nest, multi-select, cut/copy/paste, or nesting change.
- New bookmarks use "current page + Fit" only; existing destinations and fit
  types cannot be retargeted.
- Issue reasons match by title, so duplicate titles can show the first issue.
- The properties window cannot edit `/Info` or XMP.

[Evidence: T-0255](../../../evidence/ui/T-0255.md) ·
[Pages panel](../pages-panel.md) · [Desktop viewer](../gui.md)
