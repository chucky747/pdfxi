# ADR 0008 — Local equivalents for retired services and host integrations

- Status: accepted
- Date: 2026-11-19

## Context

`docs/ASSUMPTIONS.md` already records that Adobe's retired remote services
(FormsCentral, EchoSign, Acrobat.com, SendNow, LiveCycle Rights Management) are
not recreated as network services. Several parity rows nevertheless describe
network or host-specific integration: shared review, forms distribution,
SharePoint-style storage, browser capture, email-to-PDF, Office COM automation,
and cloud policy protection.

Two failure modes must be avoided:

1. Marking a row `replaced` only to satisfy `goal-verify`, without a real local
   equivalent.
2. Keeping a row `in-progress` forever because a remote service cannot exist in
   a clean-room local product.

## Decision

1. Every retired-service row is delivered as a **local, file-based equivalent**
   inside the existing crates and CLI, not as a network client:
   - Shared review and comment exchange: review registry import/export files,
     with deterministic merge and conflict reporting.
   - Forms distribution and response collection: local FDF/XFDF bundles and a
     response-folder importer.
   - SharePoint-style storage: a documented folder-sync contract over local
     paths; no credentialed network code.
   - Browser page capture: a local HTML-file or explicitly supplied markup
     import path through `pdfxi-convert`; no remote fetch and no extension.
   - Email-to-PDF: watched-folder ingestion of message files saved by the user's
     mail client.
   - Office integration: file-level import/export adapters (DOCX/XLSX/PPTX
     subsets) without COM, macros, or Office installation.
   - Policy protection: local permission policies enforced by the security
     crate, without a rights-management server.
2. A row may be marked `replaced` only when the local equivalent compiles, has
   tests that fail without it, has rustdoc/mdBook coverage, and cites pasted
   evidence in `docs/evidence/`. The row notes must name the replacement.
3. A row that is only planned future work stays `planned`; a row with a partial
   local equivalent stays `in-progress`.
4. No P0, P1, or P2 row is deferred without an ADR. This ADR covers only the
   replacement mechanism; each row still needs its own task and evidence.

## Consequences

- The product stays fully offline and deterministic; no credentials or network
  access enter tests.
- Some parity claims become narrower than the Adobe original (for example, no
  server-side review), and that narrower scope is recorded in the row notes.
- `docs/FEATURES.md` remains the human summary; `docs/features.toml` keeps the
  machine-enforced status.
