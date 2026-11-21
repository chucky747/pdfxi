# Redaction

Redaction removes content instead of covering it. In this build the capability
is **library-level only**: the viewer's **Redact** and **Protect PDF** commands
report that they are not available, and T-0263 is pending. This page states what
the implementation removes, what an independent reader can still recover, and
how the removal is verified. The API and evidence are in
[T-0237](../../../evidence/hardening/T-0237.md); the surrounding security model
is in [Security](../security.md).

## A mark is a rectangle on a page

`pdfxi_edit::redact_document` takes marks, each a page and a rectangle. Removal
is by content: the marked content is rewritten, not painted over. Anything
outside a mark is deliberately left alone.

## What redaction removes

- **Text.** Each marked page's content stream is tokenized with its text state
  tracked. Glyphs whose transformed text-space box shares positive area with the
  mark are removed; surviving glyphs keep their encoded bytes inside `TJ`
  adjustments so text does not slide into the redacted area. A partial run splits
  at glyph boundaries. If a font resource is missing, a size or horizontal scale
  is zero, or an advance is non-finite, the whole operator is neutralised rather
  than leaving part of a secret.
- **Annotations, links, and form widgets.** An annotation whose `/Rect`
  intersects a mark is nulled when its class is enabled: ordinary annotations,
  `/Link`, and form widgets. Indirect objects reachable from `/AP` are nulled so
  appearance streams do not survive. Removed widgets are filtered out of
  `/AcroForm /Fields` and `/CO`; emptied parent fields are nulled.
- **Metadata, when requested.** `remove_metadata` clears the `/Info` title,
  author, subject, keywords, creator and producer, and removes the catalog
  `/Metadata` XMP stream. It is a flag, not an unconditional behaviour.
- **Image pixels, for supported formats.** For an image XObject intersecting a
  mark, the mark is mapped through the inverse CTM and covered pixels are filled
  opaque black; an `/SMask` has its alpha set in the same region. The sample
  buffer is re-encoded as Flate in the same object, replacing the original
  compressed bytes. Supported at 8 bits per component: DeviceGray, DeviceRGB,
  Indexed and DCTDecode JPEG; ICCBased with one or three components. A
  4-component ICCBased CMYK profile is rejected.
- **The old revision.** Redaction is a **full rewrite**, never an incremental
  append. The first existing `/Contents` stream is overwritten with the rewritten
  stream and additional content streams are staged as `null`. Saving writes every
  live object, so history is cleared rather than left reachable through an
  unreferenced stream ([ADR 0010](../../../decisions/0010-document-session-and-write-path.md)
  point 5).

## What an independent reader can still recover

Redaction is scoped to the marks and to the content classes above. It is **not**
a whole-document sanitizer. After a redaction, an independent reader can still
see:

- **Vector paths and graphics under a mark.** A rectangle drawn over a mark is
  not removed by this implementation. Lines, shapes, and other path operators can
  still reveal the redacted geometry or values. Do not treat a mark as erasing
  vector content.
- **Content outside the mark**, including text that visually overlaps the mark
  but whose glyph box does not share positive area with it. Marks are geometric,
  not semantic.
- **Annotations, form fields, and images that do not intersect a mark.** They
  are left in place by design.
- **Metadata when `remove_metadata` is not enabled.** Author, title, XMP, and
  producer survive unless the flag is set.
- **Bytes outside the rewritten content.** The full rewrite clears the old
  content stream, but it does not claim that every unrelated object is gone.
- **Hidden structures not covered by T-0237.** Thumbnails, optional content,
  revision remnants, and embedded files are T-0257 work and are not removed.
  Sanitization is library-level until T-0257/T-0263 and is not invoked by
  redaction.

Some content is not silently left behind: unsupported images and intersecting
form XObjects fail the request closed (see below), so the operation reports "not
redacted" rather than producing a file with a known survivor.

## Fail-closed image handling

With `keep_images = false` — the default — the whole request returns
`UnsupportedImage` before anything is staged for:

- JPXDecode, CCITTFaxDecode, JBIG2Decode, and unknown filters;
- Separation, DeviceN, ImageMask, `/Decode`, and non-8-bit samples;
- ICCBased with more than four components or a 4-component CMYK profile;
- inline images (`BI … ID … EI`);
- intersecting form XObjects;
- a missing resource, an unresolvable reference, a direct or non-stream object;
- corrupt sample buffers;
- an image over 100 megapixels or 64 MiB of decoded samples
  (`ImageLimitExceeded`).

Validation, decoding, filling, and re-encoding happen before any page content or
image object is staged, so a rejected request does not write a half-redacted
stream. With `keep_images = true`, an intersecting image or form is reported in
`surviving_spans`, `clean` is false, and the caller must read the report; no
error is returned.

## Verify by re-extraction and byte scan

`verify_redaction` performs a full save, reparses the saved bytes, and checks
three independent surfaces:

1. **Page text re-extraction** for every secret.
2. **A byte scan** of the stored file and of every text-candidate stream in
   UTF-8 and UTF-16LE with Flate decompression. A text-candidate Flate stream
   that cannot be decompressed is `UnreadableStream`, which fails closed.
3. **Surviving annotation and field rectangles** against the marks.

`clean` is true only when there are no survivors. A test places a secret outside
the mark to prove the verifier is not always-true.

The caller must pass the **real secrets** and require `clean`. Treat
`UnsupportedImage` as "not redacted", not as a warning. For content that matters,
also open the saved file in an independent reader and search for the secret
there; that second-reader step is T-0257 and is not part of this build's
automated verification.

## Limits

- **No viewer or CLI entry point.** T-0263 is pending; `EDIT-013` remains a
  `library` row.
- **Vector paths under a mark are not removed.** This is the most important
  recovery surface.
- Form XObjects are not recursed and inline images are not decoded; both fail
  closed. Recursion and inline-image decoding are T-0268, which has not landed.
- The byte scan deliberately excludes image streams, so image redaction rests on
  the supported-format checks and the image tests, not on the secret scan.
- ICCBased and Indexed handling is narrow (no indirect Indexed lookup, no CMYK
  fill) and fails closed rather than guessing.
- Broader sanitisation — thumbnails, optional content, revision remnants,
  embedded files — and independent second-reader verification are T-0257, which
  has not landed.
- A mark is not proof of erasure. Require `clean` and verify independently.

## Related pages

[Security](../security.md) · [Signatures](signatures.md) ·
[What the assistant sends](assistant-data.md) ·
[Open, save, and print](../open-save-print.md)
