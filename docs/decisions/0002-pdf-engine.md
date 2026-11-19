# ADR 0002 — PDF engine split

## Status

Accepted.

## Context

No mature Rust-native renderer matches PDFium's coverage. `lopdf` is strong for
object-level manipulation but does not render. A single library cannot provide
both fidelity and structural editing.

## Decision

- `pdfxi-render` wraps PDFium behind a `Renderer` trait.
- `pdfxi-core` and `pdfxi-edit` own the object graph and mutations.
- `pdfxi-text` owns extraction and search.
- The renderer is replaceable; callers never depend on a PDFium type.

## Consequences

- Native PDFium binaries must be bundled per platform.
- PDFium is not thread-safe; use a bounded pool.
- Rendering fidelity is high; editing remains under our control.
