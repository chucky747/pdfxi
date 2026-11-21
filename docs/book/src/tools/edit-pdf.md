# Edit PDF

**Edit PDF** changes objects on an existing page: move, resize, delete, replace
text in place, and add text or images. It edits the open document; it does not
create new PDFs. The tool is backed by `pdfxi-edit::content_edit` and the UI in
`crates/pdfxi-app/src/shell/edit_tool.rs` ([T-0264](../../../evidence/ui/T-0264.md)).

## Getting there

Activate **Tools - Edit PDF** (the `EditDocument` command) with a document open.
The status becomes `Edit PDF: click an object`; dispatching again closes the tool.
Where the Tools row is capability-gated, the Edit command and the tool's launch
route reach the same state.

## Selecting an object

- Click an object to select the topmost item whose bounding box contains the
  point. The card shows its kind, page, and rectangle.
- Click empty space to clear the selection (`Edit PDF: no object at that point`).
- A selection rectangle with four corner handles is drawn over the page raster.

## Edits

| Action | How | Effect |
| --- | --- | --- |
| Move | Drag the selection | Object moves by the PDF-space delta |
| Resize | Drag a corner handle | Object scales about its lower-left origin |
| Delete | Press Delete | Object removed; selection cleared |
| Add text | Type text, then place | New text box at a clamped 220x28 pt box |
| In-place text | Double-click a text object, then Apply text | Object text replaced |
| Add image | Add image, pick PNG/JPEG, then click to place | Image at a 120x120 pt box |

## How changes are written

Each edit clones the document, applies the matching `content_edit` operation,
and commits through `DocumentSession::edit`, followed by the standard post-edit
refresh. It is one incremental revision, so Undo/Redo covers it. Read-only tabs
and documents whose permissions deny modification are refused before any change.
Save to write the edits to disk ([Open, save, and print](../open-save-print.md)).

## Limits

- No font picker, size, or colour control; text replacement keeps the object's
  existing styling.
- Resize anchors at the lower-left, so dragging a north/west handle grows the
  object rather than moving the opposite edge.
- No multi-select, grouping, z-order control, snapping, or alignment guides.
- Object ids are positional; deleting a block renumbers later blocks, so the
  selection is cleared after a delete.
- Image replacement in place is not offered: delete the old image, then add one.

[Evidence: T-0264](../../../evidence/ui/T-0264.md) ·
[Desktop viewer](../gui.md) · [Security](../security.md)
