# ARCHITECTURE

## Principles

1. **Core is headless.** `pdfxi-core` has no UI, rendering, or platform
   dependencies. Every other crate depends inward.
2. **Traits at process boundaries.** Rendering, OCR, conversion, signing, and
   storage are traits with a default implementation and test doubles.
3. **Copy-on-write documents.** Edits never mutate the source bytes in place.
   Incremental save is a deliberate operation with an explicit revision.
4. **Cancellation and backpressure everywhere.** Long operations accept a
   cancellation token and report progress.
5. **Untrusted PDFs.** Parsers are bounds-checked, fuzzed, and deny panics.
6. **No global mutable state.** Services are passed explicitly; UI state is
   owned by the document session.

## Crate map

```text
pdfxi-app ───────────────┐
pdfxi-cli ───────────────┤
pdfxi-automation ────────┤
pdfxi-standards ─────────┤
pdfxi-convert ───────────┤
pdfxi-security ──────────┤
pdfxi-forms ─────────────┤
pdfxi-edit ──────────────┤
pdfxi-accessibility ─────┤
pdfxi-text ──────────────┤
pdfxi-render ────────────┤
                         ▼
                    pdfxi-core
                         ▲
                    pdfxi-testkit
```

Dependency rules:

- `pdfxi-core` depends on `thiserror` plus small, audited utility crates (for
  example `flate2` for FlateDecode); it never depends on renderer or UI types.
- `pdfxi-render` may depend on `pdfium-render` (optional `pdfium` feature),
  `image`, `rayon`, and `pdfxi-core`.
- `pdfxi-text`, `pdfxi-edit`, `pdfxi-forms`, `pdfxi-security`, `pdfxi-convert`,
  `pdfxi-standards`, and `pdfxi-automation` stay dependency-free beyond
  `thiserror` until a documented need appears.
- `pdfxi-app` may depend on every feature crate; `eframe`/`egui` are optional
  behind the `ui` feature. Feature crates never depend on `pdfxi-app`.
- `pdfxi-testkit` is test-only and may depend on all core crates.

## Core contracts

### Document

```rust
pub struct Document { /* source bytes, version, object store */ }

impl Document {
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self>;
    pub fn version(&self) -> &str;
    pub fn page_count(&self) -> Result<usize>;
    pub fn save_incremental(&self) -> Result<Vec<u8>>;
}
```

### Renderer

```rust
pub trait Renderer {
    fn render_page(&self, page: PageIndex, scale: f32) -> Result<RenderedPage>;
    fn text_page(&self, page: PageIndex) -> Result<TextPage>;
}
```

The default backend is PDFium. PDFium is not thread-safe; the implementation
uses a bounded pool of instances, one per worker. A native compositor can be
added later without changing callers.

### OcrEngine

```rust
pub trait OcrEngine {
    fn recognize(&self, image: &RenderedPage, languages: &[String]) -> Result<OcrPage>;
}
```

### Converter

```rust
pub trait Converter {
    fn can_import(&self, kind: SourceKind) -> bool;
    fn import(&self, input: &Path) -> Result<Vec<u8>>;
    fn export(&self, doc: &Document, target: ExportKind) -> Result<Vec<u8>>;
}
```

### Signer / Crypto

```rust
pub trait Signer {
    fn sign(&self, input: &[u8], options: &SignOptions) -> Result<Vec<u8>>;
    fn verify(&self, input: &[u8]) -> Result<SignatureReport>;
}
```

## UI architecture

- `pdfxi-app` owns the eframe application, document tabs, dock layout, and
  commands.
- The canvas is a virtualized tile surface: visible tiles are rendered through
  the bounded raster cache and uploaded as `egui` textures. Rendering is
  synchronous on the UI thread today; off-thread rendering is planned work, not
  a current guarantee.
- Selection, annotations, links, and form widgets are overlay layers in page
  coordinates.
- Commands are typed actions with undo/redo; the UI never mutates a document
  directly.
- Accessibility uses `accesskit`; shell controls publish accessible names and
  every modal has a keyboard path.

## Concurrency

- There is no render pool or `JobHandle` in the viewer yet. The assistant chat,
  assistant connect, and print flows use dedicated worker threads with
  `mpsc` channels and poll their receivers from the UI.
- OCR, conversion, and long document jobs remain synchronous in this release;
  they are the next candidates for bounded worker pools.
- UI threads never block on network or print waits; those runs return through
  channels.

## Persistence

- User settings: `%APPDATA%\PDFXI\settings.toml` (TOML, versioned).
- Recent documents, stars, and the local library: `%APPDATA%\PDFXI\library.toml`
  (TOML, atomic replace). No SQLite database is used.
- There is no autosave sidecar in this release. Unsaved work is held in memory
  and protected by the close prompt.
- No telemetry by default.

## Error model

- Library errors use `thiserror` enums with actionable variants.
- The app maps errors to user-facing messages and structured logs.
- `tracing` is the logging facade; logs never contain document contents or
  secrets.
- Panics are reserved for proven invariants. The UI does not install a
  `catch_unwind` boundary; an unexpected panic terminates the viewer process and
  is recorded in the per-user log.

## Clean-room and dependencies

- PDF specification, ISO 32000, is the primary reference.
- PDFium, lopdf, egui, Tesseract, and other dependencies retain their licenses.
- Optional external tools (LibreOffice, Ghostscript, headless browser) are not
  bundled and are detected at runtime.
- `deny.toml` enforces the license allowlist.

## ADR index

ADRs live in `docs/decisions/`. The first set:

- 0001: Windows-first, GNU bootstrap fallback when MSVC requires elevation.
- 0002: PDFium for rendering; lopdf for object manipulation.
- 0003: egui/eframe for the desktop shell.
- 0004: one canonical workspace; claims instead of worktrees.
- 0005: native OpenCode goal plus a controller for round-cap resume.
