# Architecture

PDFXI keeps a headless core and puts traits at every process boundary.

- [Full architecture](../../ARCHITECTURE.md) — crate map, contracts (`Document`,
  `Renderer`, `OcrEngine`, `Converter`, `Signer`), UI, concurrency, persistence.
- [ADRs](../../decisions/) — cross-lane and irreversible decisions.

The M1 slice uses `Renderer::render_page` and `Renderer::text_page` for both the
[viewer](../../features/view.md) and the [CLI](../../features/cli.md).
