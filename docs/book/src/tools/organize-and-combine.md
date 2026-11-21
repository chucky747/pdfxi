# Organize pages and combine files

Two windows over the page planner: **Organize Pages** rearranges the pages of one
document; **Combine Files** merges several PDFs into one
([T-0231](../../../evidence/ui/T-0231.md)).

## Organize Pages

Open **Organize** from the Tools panel or command. With no document the status is
"Open a document to organize". While the card is open, clicking a thumbnail in
the Pages panel feeds its selection: plain click replaces, Ctrl toggles, Shift
fills from the nearest selected page. Outside the card the same click navigates.

| Button | Effect |
| --- | --- |
| Rotate left / right | Rotates the selected pages |
| Delete | Removes the selected pages |
| Move up / down | Reorders the selection |
| Extract selected | Writes the selected pages to a new PDF and opens it |
| Split each page | Writes one PDF per page |

## Combine Files

Add files with the native multi-select (PDFs only), remove rows, choose an output
path (default `combined.pdf`), then press **Combine**. It reads each source as a
document, appends the remaining documents to the first, writes the result, and
reopens it with the merged page count. Fewer than two sources is a typed error
that writes nothing.

## How changes are written

A page plan rebuilds the page tree with fresh object numbers, so it is a full
rewrite and commits through `DocumentSession::edit_full`. The tab becomes dirty
and Save/Save As work, but the rewrite clears undo history: undo does not cross
that boundary ([ADR 0010](../../../decisions/0010-document-session-and-write-path.md)).

## Limits

- No blank-page insert and no pointer-drag reorder; use Move up/down.
- Combine accepts PDFs only; images are not converted into the merge list.
- Extracted and split files are new tabs; the source is left unchanged.
- Page operations clear undo at the full-rewrite boundary.

[Evidence: T-0231](../../../evidence/ui/T-0231.md) ·
[Pages panel](../pages-panel.md) ·
[Open, save, and print](../open-save-print.md)
