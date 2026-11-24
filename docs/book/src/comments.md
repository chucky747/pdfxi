# Comments

Comments are PDF annotations written into the document. They are stored in the
file when you save, so they travel with the PDF. This page covers the annotation
types the shell creates today and the review actions around them.

## What you can add

| Type | What it writes |
|---|---|
| Text note (`/Text`) | A note annotation with your comment text |
| Highlight (`/Highlight`) | A highlight annotation over the selected text |
| Underline (`/Underline`) | A line under the selected text |
| Strikeout (`/StrikeOut`) | A line through the selected text |
| Squiggly (`/Squiggly`) | A wavy line under the selected text |

The Comments panel shows one button per markup kind. Select text on the page
first; choosing a tool applies it to every selected page and then clears the
selection.

Adding a comment writes:

- **`/T`** — the author, taken from the local `display_name` setting. A fresh
  install uses the neutral `Reader`, not your OS account name; see
  [Privacy and local data](privacy.md). This is a self-asserted label, not a
  verified identity.
- **`/M`** — the modification time, formatted in local time.
- **`/Contents`** — the text you typed (for a note) or the comment attached to
  the highlight.

Replies and resolve state are stored as standard annotation keys:

- a reply is an annotation with **`/IRT`** pointing at its parent;
- **Resolve** sets **`/State`** to `Accepted` on the annotation.

## Working with comments

- **Select** a comment in the Comments panel or on the page to act on it.
- **Edit** changes the text of an existing comment.
- **Delete** removes the annotation from the document.
- **Reply** adds a child annotation through `/IRT`.
- **Resolve** marks the annotation `/State Accepted`. There is no Rejected or
  Cancelled state in this build; resolve is the only state transition offered.
- **Undo/redo** covers adding, editing, deleting, replying, and resolving. An
  undone change is not written to the file unless you redo it and save.

Nothing is written to disk until you save; see
[Open, save, and print](open-save-print.md).

## Permission gate

Commenting requires the document's **ANNOTATE** permission. When the permission
mask denies it, the comment controls are disabled and the document behaves as
read-only for annotations. Permissions are advisory at the file level; the
viewer enforces this in its own commands and says so, rather than presenting the
file as cryptographically locked ([Security](security.md)).

## Limits

- Only text notes and text-markup annotations are created by the shell. Drawing,
  ink, stamps, and measurement annotations are **not** available here.
- Author names are labels, not authenticated identities.
- A comment is a PDF annotation, not a shared-review message. Nothing is
  transmitted; **Shared** is a placeholder and does not carry comments between
  people ([Library](library.md)).
- Resolve has one target state (`Accepted`); there is no accept/reject workflow,
  assignment, or due date.
- Annotation appearance is the viewer's own; other readers may draw the same
  annotation differently.

## Related pages

[Pages panel](pages-panel.md) · [Search](search.md) ·
[Open, save, and print](open-save-print.md) · [Security](security.md)
