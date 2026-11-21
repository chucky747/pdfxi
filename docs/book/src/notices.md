# Third-party notices

PDFXI is an independent clean-room implementation and is not affiliated with
Adobe Inc. No Adobe source code, binaries, icons, or help text are included. The
authoritative notices shipped with a release are:

- `NOTICE` at the repository root, for the project statement and bundled fonts;
- [docs/THIRD_PARTY.md](../../THIRD_PARTY.md), for components, versions, and
  licence sets;
- `deny.toml`, the licence allowlist enforced by `cargo deny`.

This page adds the parts that the book needs and records a known gap. It is a
summary, not a licence text; where this page and the files above disagree, the
files above win.

## Bundled font

Courier Prime Regular is bundled at
`crates/pdfxi-edit/assets/CourierPrime-Regular.ttf`.

- Copyright 2015 The Courier Prime Project Authors
  (<https://github.com/quoteunquoteapps/CourierPrime>).
- Licensed under the **SIL Open Font License, Version 1.1**.
- The full licence text ships next to the font at
  `crates/pdfxi-edit/assets/CourierPrime-OFL.txt`.
- The font is embedded only in documents the user edits. It is not sold
  separately and is not renamed.

`NOTICE` and [docs/THIRD_PARTY.md](../../THIRD_PARTY.md) already list this font.

## Core components

These are recorded in [docs/THIRD_PARTY.md](../../THIRD_PARTY.md) with their
licences and, where relevant, hashes:

- **PDFium** (`pdfium.dll`) — BSD-3-Clause, with the third-party notices from
  the pinned archive redistributed in `pdfium-licenses\`.
- **Rust crates** including `lopdf` (MIT), `pdfium-render`, `thiserror`, `clap`,
  `serde`/`serde_json`/`toml`, and `egui`/`eframe` (MIT OR Apache-2.0 for the
  latter group).
- **External tools** (LibreOffice, a headless browser, Ghostscript, veraPDF) are
  optional or CI-only and are not bundled by default.

## Assistant client crates

The assistant client (`pdfxi-assistant`) declares these direct dependencies for
its local HTTP and WebSocket connection. They are permissively licensed; the
repository allowlist in `deny.toml` is the check, and `cargo deny check` fails on
a licence outside it.

| Crate | Role |
|---|---|
| `ureq` | Blocking HTTP/1.1 client for `/api/info`, `/api/provider`, `/api/model`, and session calls |
| `rustls` | TLS implementation, configured with the operating system trust store |
| `rustls-native-certs` | Loads the OS certificate store for `rustls` |
| `tungstenite` | Blocking WebSocket client for the client-MCP attach route |
| `url` | URL parsing and validation |
| `base64` | Encoding for HTTP Basic credentials and protocol fields |
| `http` | Shared HTTP types |

**Known gap.** [docs/THIRD_PARTY.md](../../THIRD_PARTY.md) does **not** list these
seven assistant crates yet. They are recorded here so the omission is visible;
the integrator owns that file and will merge the rows. This page does not
reproduce the upstream licence texts; consult each crate's published licence and
the allowlist in `deny.toml`.

## Redistributing a build

If you pass a build on to someone else, include:

- `LICENSE`, `NOTICE`, and `THIRD_PARTY.md`;
- the `pdfium-licenses\` directory next to `pdfium.dll`;
- `crates/pdfxi-edit/assets/CourierPrime-OFL.txt` when the font is shipped.

The packaging script already stages the PDFium licence set; see
[docs/THIRD_PARTY.md](../../THIRD_PARTY.md) for the exact payload.

## Close-out confirmation

The assistant's local HTTP/TLS stack is built on **`ureq`** and **`rustls`**
(with `rustls-native-certs` for the OS trust store); the read-only client-MCP
bridge lives at `crates/pdfxi-assistant/src/mcp.rs` and is a library capability,
not an app-turn tool path. The bundled **Courier Prime Regular** font is licensed
under the **SIL Open Font License, Version 1.1**, with its licence text at
`crates/pdfxi-edit/assets/CourierPrime-OFL.txt`. These are recorded above and in
`deny.toml`; the authoritative component table is
[docs/THIRD_PARTY.md](../../THIRD_PARTY.md).

## Limits

- This page is a summary and is not legal advice.
- It does not replace the full licence texts or the per-crate table.
- The assistant crate rows are pending in the authoritative file; until they
  land, treat this book page and `deny.toml` as the visible record.
- The clean-room statement covers Adobe assets only. Other trademarks belong to
  their owners.

## Related pages

[Installing PDFXI](install.md) · [Desktop viewer](gui.md) ·
[Security](security.md)
