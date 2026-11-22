# Standards and preflight

`pdfxi-standards` holds the preflight model: typed inputs, stable finding codes,
profiles, severity-ordered reports, and opt-in fixups. It is dependency-light and
does not mutate a document unless a fixup is explicitly selected.

## Model

- `standard.rs` — `Standard` and `StandardFamily` identify a target profile.
- `input.rs` — `PreflightInput` with catalog, font, and page facts gathered from
  a document; the analyzer consumes facts rather than raw objects.
- `finding.rs` — `Severity`, `PreflightFinding`, and `sort_findings`. Finding
  codes are stable strings such as `PDFA1B.NO_OUTPUT_INTENT` so scripts can match
  on them.
- `profile.rs` — `PreflightProfile`, `HeaderProfile`, `StaticProfile`, and
  `PreflightReport`.
- `document_profile.rs` — real profiles: `PdfA1bProfile`, `PdfA2bProfile`,
  `PdfA3bProfile`, `PdfX1aProfile`, `PdfX4Profile`, `PdfE1Profile`, and
  `PdfUa1Profile`.
- `fixup.rs` — `apply_fixups`, `suggested_fixups`, and `FixupPolicy`. Fixups are
  never silent: a fixup must be opted into and is reported in the result.

There is no file named `preflight.rs`; preflight lives in `profile.rs` and
`document_profile.rs`.

## What the profiles check

Representative rules, all covered by tests:

- PDF/A reports unembedded fonts, missing output intents, and transparency;
  encryption is prohibited. PDF/A-3b allows embedded files where PDF/A-2b does
  not.
- PDF/X reports a missing output intent.
- PDF/VT-1 runs the same decidable PDF/X-4 checks and additionally requires a
  catalog `/DPartRoot` and a `/DPart` association on every page
  (`PDFVT1.DPART_ROOT_MISSING`, `PDFVT1.DPART_PAGE_UNASSIGNED`).
- Header and version mismatches are reported with deterministic severity and
  order.
- A clean input passes all named profiles.
- Suggested fixups resolve only mechanical findings (for example adding XMP
  metadata); semantic failures are reported, not auto-corrected.

## Row status

| Area | Rows | Status |
|---|---|---|
| Classic xref table and `startxref` parser | `STD-011` | verified |
| Page tree traversal with inherited attributes | `STD-012` | verified |
| PDF 1.0–2.0, xref streams, object streams, fonts, transparency | `STD-001`–`STD-005` | in-progress |
| Tagged PDF and PDF/A/X/UA conformance | `STD-006`–`STD-009` | in-progress |
| Incremental update and preservation | `STD-010` | in-progress |
| Preflight profiles, checks, fixups | `PRINT-001` | in-progress |
| PDF/A, PDF/X, PDF/E, PDF/VT validation | `PRINT-002` | verified (`pdfxi preflight <profile> <file>`) |
| ICC colour management, separations, ink manager, trapping | `PRINT-003`–`PRINT-006` | planned |
| Page boxes, bleed, crop, printer marks | `PRINT-007` | in-progress |
| Droplets, hot folders, batch preflight | `PRINT-008` | planned |

`PRINT-001` adds PDF/A-3b and PDF/E-1 profiles and ordered annotation and
forbidden-entry rules. ICC validation, a byte-level fixup adapter, and a
veraPDF CI oracle remain. Evidence for the standards work is in
`../../evidence/convert/T-0079.md`.

`PRINT-002` is hosted by `pdfxi preflight <PROFILE> <FILE>`: the command builds a
`PreflightInput` snapshot from real bytes (`pdfxi_convert::preflight`), resolves
the profile label through `ConformanceProfile`, and exits 0 for a pass or 2 for
error-severity findings. Profiles are PDF/A-1b/2b/3b, PDF/X-1a/4, PDF/E-1,
PDF/UA-1, and PDF/VT-1. The verdict is a subset preflight, never a conformance
certification. Evidence is in `../../evidence/cli/T-0345.md`.

## Design rules

- Findings are data, not exceptions: a preflight run always returns a report.
- Codes and ordering are part of the public contract and must not drift.
- Fixups are opt-in and reported; a fixup must never hide a semantic failure.
- The analyzer consumes facts, so a hostile PDF never reaches profile code with
  raw, unbounded objects.

## Related pages

[Getting started](getting-started.md) · [Authoring](authoring.md) ·
[Accessibility](accessibility.md) · [Feature matrix](../../FEATURES.md)
