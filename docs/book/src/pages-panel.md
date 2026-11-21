# Pages panel

The pages panel is the document outline on the left of the canvas. It is a
navigation surface, not an editor.

## Thumbnails

- Thumbnails are **real page renders** produced by PDFium, not placeholders or a
  fixed icon. They reflect the page content of the open document.
- The panel can be shown as a **list** or a **grid**.
- Selecting a thumbnail navigates the canvas to that page.

## What the panel does

| Action | Available |
|---|---|
| Show page thumbnails | Yes |
| Switch list / grid view | Yes |
| Click a page to navigate | Yes |
| Add a page | No — hidden in this build |
| Page context menu | No — hidden in this build |
| Reorder, delete, rotate, or extract pages | No |

**Add page** and the page menu are not drawn. That is deliberate: Organize Pages
has no working implementation in the shipped shell, so the controls are absent
rather than painted and inert. The command reports that it is not available; see
[Desktop viewer](gui.md).

Rotation in the command bar rotates the **view**. It does not rotate the page in
the file, does not mark the document dirty, and does not change what Save writes.

## Limits

- The panel navigates only within the current document; it cannot move pages
  between documents.
- Thumbnail rendering is page work like any other render, so the cost grows with
  the document and the current zoom rather than being free.
- No page labels are edited and no page boxes are changed from this panel. Page
  box authoring belongs to [Authoring](authoring.md).

## Related pages

[Open, save, and print](open-save-print.md) · [Search](search.md) ·
[Desktop viewer](gui.md)
