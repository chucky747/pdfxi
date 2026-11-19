# ADR 0007 — Splitting viewer rows that bundle independent capabilities

- Status: accepted
- Date: 2026-11-19

## Context

`VIEW-002` (page layouts) bundled page arrangement with the thumbnail pane, and
`VIEW-003` (zoom/pan/fit) bundled zoom, pan, and fit modes with the loupe
magnifier and gesture navigation. The arrangement, zoom, pan, and fit work is
implemented and tested end-to-end with real rasters; the thumbnail pane and the
loupe are separate UI capabilities.

Keeping them bundled forces a permanent `in-progress` label on finished work,
which makes the matrix lie in the other direction: it understates completed
functionality and hides which specific capability is missing.

## Decision

1. `VIEW-002` means the four page layouts (single, continuous, two-page,
   two-page-continuous), hit testing, coordinate mapping, scroll clamping, and
   raster placement. It is verified.
2. `VIEW-003` means zoom in/out, fit page/width, custom zoom, pan clamping, and
   render-scale invalidation. It is verified.
3. The thumbnail pane stays with `VIEW-007`.
4. The loupe magnifier and gesture navigation move to the new `VIEW-018`
   (P2, planned).
5. A split must be recorded in this ADR index and in the row notes; rows are
   never silently retitled.

## Consequences

- The M1 gate can verify navigation without claiming the loupe.
- `goal-verify --all` still requires `VIEW-018` to be implemented or explicitly
  deferred with a note.
- Future rows that bundle capabilities must be split before the first half is
  promoted to verified.
