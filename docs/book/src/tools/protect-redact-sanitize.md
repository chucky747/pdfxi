# Protect PDF, redact and sanitize

Three related operations from two windows. Protect adds a password and permission
mask; Redact removes selected content; Sanitize removes hidden information. All
three write a **new file** and never overwrite the source in place
([T-0263](../../../evidence/ui/T-0263.md)).

## Protect PDF

The **Protect PDF** card has masked user and owner password fields, checkboxes
for printing, copying, annotations, and form filling, and a **Protect and save
as...** action (default `<stem>-protected.pdf`). Empty user and owner passwords
are a typed configuration error. Encryption is AES-256 (revision 6) with fresh
random keys and salts on every save.

The saved file is reopened with the security reader to confirm that the correct
password works, the wrong and missing passwords are rejected, and the permission
bits match the selected grants. A **Remove protection...** section reads a
protected file with a password and writes a decrypted copy, then opens it.

Permissions are advisory: a reader that ignores the `/P` mask can still open the
file. See [Security](../security.md).

## Redact

**Redact** is enabled only when the current page selection has quads. Each quad
becomes a mark; the tool runs `apply_redaction`, verifies the result, and writes
`<stem>-redacted.pdf`, opened as a new tab. Checkboxes default to redacting
images and removing intersecting annotations, links, and form fields; metadata
removal is off unless enabled.

- Redaction is **selection-only**: no area-drawing tool, no search-and-redact,
  and no review list before applying.
- It is a **full rewrite**, never an incremental append, so earlier bytes are
  not left reachable in an unreferenced revision.
- The status reports "Redacted N area(s) on M page(s); verification: clean" or
  the survivor details.

## Sanitize

**Sanitize and save as...** writes `<stem>-sanitized.pdf` and reports the removed
categories (document info, XMP metadata, attachments, JavaScript, optional
content, thumbnails). Default-hidden optional content fails closed rather than
being revealed ([T-0257](../../../evidence/hardening/T-0257.md)).

## What verification proves, and its limits

Removal is verified by independently reparsing the saved bytes: page text
re-extraction, a whole-file and stream byte scan, and surviving annotation and
field rectangles. `clean` is true only when there are no survivors. Form XObjects
are recursed and inline images are decoded and refilled
([T-0268](../../../evidence/hardening/T-0268.md)).

**Vector paths and drawing graphics under a mark are not removed.** They are the
main recovery surface. The byte scan also excludes image streams, so image
redaction rests on the supported-format checks and the image tests. Treat a
redaction mark as a request to remove content, not as proof of erasure; require
`clean` and verify an independent reader when it matters. See
[Redaction](../security/redaction.md).

[Evidence: T-0263](../../../evidence/ui/T-0263.md) ·
[Security](../security.md) ·
[Open, save, and print](../open-save-print.md)
