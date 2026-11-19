# Viewer — M1 slice

> **Status: in progress.** The M1 viewer slice is being implemented. The
> behaviours on this page describe the intended M1 experience; several are not
> available yet. See [Feature and milestone status](#feature-and-milestone-status)
> and the canonical [feature matrix](../FEATURES.md) before relying on any item.

This page documents the viewing and navigation surface delivered by milestone
M1 (the "vertical slice"). It is a human-facing companion to
`docs/features.toml`, not a substitute for it. Implementation contracts live in
[ARCHITECTURE.md](../ARCHITECTURE.md); the milestone scope lives in
[PLAN.md](../PLAN.md).

## Scope

M1 proves that PDFXI can open a real PDF and provide a usable reading loop:
open a document, display pages, change zoom and rotation, navigate with
thumbnails and bookmarks, search, and select/copy text. Rendering is provided
by PDFium behind the `Renderer` trait; text extraction and search run through
`pdfxi-text`. Both are called by the headless core, so the same capabilities
back the desktop app and the CLI.

## Behaviours

### Open

- Open a local PDF from disk; the document bytes are read into `pdfxi-core`
  and parsed into a `Document` (`Document::from_bytes`).
- The UI opens documents in tabs (tabbed interface itself is `VIEW-005`, a P1
  item beyond the minimum M1 read loop).
- Opening reports the PDF version, byte length, and page count. These are the
  same values printed by `pdfxi info` (see [cli.md](cli.md)).
- Encrypted PDFs are out of scope for M1 (`SEC-001` and `SEC-002` are later
  milestones). Opening an encrypted document is expected to fail with an
  actionable error, not silently render.

Intended/partially available at M1: basic open and metadata reporting is
wired in the core and CLI; the desktop tab shell depends on the `pdfxi-app`
lane.

### Page display

- One page at a time in a continuous vertical strip; two-page and single-page
  layouts are tracked by `VIEW-002`.
- Pages render through PDFium into tiles cached by `(page, scale, rotation)`,
  then upload to the GPU (see [ARCHITECTURE.md](../ARCHITECTURE.md), "UI
  architecture"). Rendering off-thread keeps the UI responsive.
- Targets from [TESTING.md](../TESTING.md): first page under 300 ms and a full
  500-page open under 1.5 s. These are budgets to verify, not claims of current
  performance.

### Zoom

- Fit modes (fit page, fit width), zoom in/out, and an explicit zoom level.
  Tracked by `VIEW-003` ("Zoom pan loupe and fit modes"); the pan and loupe
  sub-features are not required for the first slice.
- Zoom changes the render scale passed to `Renderer::render_page` and
  invalidates the tile cache for the affected scale.

### Rotate

- Rotate the view in 90-degree steps; page rotation is part of the tile cache
  key so rotated and unrotated tiles do not thrash. Tracked by `VIEW-004`
  ("Rotate view and page display").
- This is a *view* rotation for M1. Persisted page rotation is an editing
  feature (`EDIT-006`) in a later milestone.

### Thumbnails

- A navigation pane shows page thumbnails for quick jumping; the active page
  is highlighted. Tracked by `VIEW-007` ("Page thumbnails and navigation
  pane").
- Thumbnails render at a low fixed scale and share the same renderer pool as
  the main canvas.

### Bookmarks

- The navigation pane shows the document outline and lets the user jump to a
  bookmark destination. Tracked by `VIEW-008` ("Bookmarks destinations and
  article threads"); article threads are not in the M1 slice.
- Bookmark destinations are resolved to page indices by `pdfxi-core`.

### Search

- Find text within the open document, with next/previous result navigation and
  a match count. Results come from the `text_page` path in the `Renderer`
  contract and are highlighted on the canvas.
- **Matrix gap:** `docs/features.toml` has no dedicated in-document search row.
  `MANAGE-003` is cross-file search and `EDIT-003` is find-and-replace; neither
  is the M1 read-only find. This gap is being reported to the integrator rather
  than filled with an invented feature ID.

### Text selection

- Drag-select text on a page and copy it to the clipboard as plain text.
  Selection geometry lives in page coordinates as an overlay layer; the text
  itself comes from `Renderer::text_page`.
- Copy-preserves-reading-order is a fidelity goal; exact ordering depends on
  PDFium's text-page output and is covered by text-extraction tests in the
  `pdfxi-text` lane.

## Feature and milestone status

| Capability | Matrix row | Priority | M1 status |
|---|---|---|---|
| Open / metadata | `VIEW-001` | P0 | in progress |
| Page layouts | `VIEW-002` | P0 | in progress |
| Zoom / pan / fit | `VIEW-003` | P0 | in progress |
| Rotate view | `VIEW-004` | P0 | in progress |
| Thumbnails / navigation pane | `VIEW-007` | P0 | in progress |
| Bookmarks / destinations | `VIEW-008` | P1 | in progress |
| In-document search | — (gap) | — | in progress, no row |
| Text selection / copy | — (gap) | — | in progress, no row |

No row in this slice is `verified` yet. A row becomes `verified` only with a
test identifier, a docs path, and an evidence path, recorded through a lane
fragment in `orchestration/feature-updates/` and merged by the integrator.

## Verification

The M1 exit gate is `cargo xtask goal-verify --milestone M1`, together with the
open/first-page budgets listed in [TESTING.md](../TESTING.md). Until that gate
is green, treat everything above as intended behaviour rather than shipped
behaviour.
