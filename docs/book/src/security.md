# Security

PDFXI treats every PDF as hostile input. Security status is stated per
capability: some operations are reachable in the installed viewer, others are
implemented and tested on real documents but have no viewer control in this
build. A row is `verified` only when an end user can reach it
([ADR 0009](../../decisions/0009-feature-status-honesty.md)); every capability
below names its feature row, so the status is checkable in the
[feature matrix](../../FEATURES.md). Do not read "no viewer control" as "not
implemented": the tests cited here run on real PDF bytes, not on mocks.

## What this build exposes

| Capability | Installed viewer | Implemented and tested | Row and source |
|---|---|---|---|
| Open an encrypted PDF | Masked password modal; wrong password retries with a typed message; Cancel opens no tab | Decrypt through `pdfxi-document` and `pdfxi_security::open_encrypted` | `SEC-001`, [T-0222](../../evidence/ui/T-0222.md) |
| Honour read-only rights | The tab is read-only when `/P` denies `MODIFY` | Permission mask decoded from the handler | `SEC-001`, [T-0222](../../evidence/ui/T-0222.md) |
| Save an encrypted PDF | Refused with a typed message; the tab stays open | `SessionError::EncryptedSaveUnsupported` | [T-0222](../../evidence/ui/T-0222.md), [ADR 0010](../../decisions/0010-document-session-and-write-path.md) |
| Apply or remove password protection | **Protect PDF** reports that it is not available | `protect_document` / `remove_protection`, PDFium reopen | `SEC-012`, [T-0238](../../evidence/hardening/T-0238.md), [T-0256](../../evidence/hardening/T-0256.md) |
| Sign and verify | **Sign** reports that it is not available | CMS/PKCS#7 over the byte range, independently verified | `SEC-005`, [T-0241](../../evidence/hardening/T-0241.md) |
| Redaction marks | **Protect PDF** reports that it is not available | Text, annotation/form/link and image-pixel removal plus a verifier | `EDIT-013`, [T-0237](../../evidence/hardening/T-0237.md) |
| Sanitize hidden information | Not available | Category-plan model over a real `Document` | `SEC-008`, `SEC-009` |
| Assistant | Panel is present and inert | Locked OpenCode session client | `SEC-013`, [T-0254](../../evidence/hardening/T-0254.md) — see [Privacy and local data](privacy.md) |

The "not available" messages are the shell's honest placeholder
(`crates/pdfxi-app/src/shell/commands.rs`); these commands report a status
message instead of silently doing nothing.

## Passwords and encryption

**What is written.** Password protection defaults to AES-256, revision 6, with
every permission granted. Revisions 5 and 6 use a fresh random 32-byte file key,
four fresh 8-byte validation/key salts, fresh 4-byte `/Perms` padding and a fresh
16-byte IV for every encrypted stream and string, all drawn from the operating
system CSPRNG on each save. Identical plaintexts therefore do not share
ciphertext prefixes, and two protects of the same source with the same passwords
produce different files. If the entropy source fails the operation stops with
`RandomnessUnavailable`; there is no deterministic fallback. Revision 6 uses the
ISO 32000-2 Algorithms 8–10 wrapping through `/U`, `/UE`, `/O`, `/OE` and
`/Perms`. The `/Info` reference is carried through protect and remove-protection
([T-0256](../../evidence/hardening/T-0256.md)). RC4-40/128 and AES-128 remain
available only for compatibility ([T-0238](../../evidence/hardening/T-0238.md)).

**What is proven.** Protect, reopen with the user password, wrong-password and
missing-password rejection, owner-role access, the permission mask, unchanged
page text and preserved `/Info` are tested on the Excel 2019 sample. The output
also opens in PDFium as an independent reader, extracts matching text and reads
the title, and PDFium refuses the file with no password or a wrong one; a
non-ASCII password round-trips through both this crate and PDFium
([T-0256](../../evidence/hardening/T-0256.md)). Passwords longer than 127 bytes
and contradictory permission grants are rejected with typed errors rather than
truncated or written ([T-0238](../../evidence/hardening/T-0238.md)).

**Permissions are advisory.** The `/P` mask and the AES-256 `/Perms` block are
authenticated, but a reader that ignores them opens the document anyway, and the
owner password always bypasses the mask. `pdfxi-core` text extraction does not
consult `Permissions`; the viewer enforces read-only in its own commands and says
so, and it does not pretend the file is cryptographically read-only
([T-0238 §9](../../evidence/hardening/T-0238.md),
[T-0256 §8](../../evidence/hardening/T-0256.md)). The installed viewer opens an
encrypted file through the password modal, marks a document read-only when
`MODIFY` is denied, and refuses to save an encrypted document because
re-encryption on save does not exist in this build
([T-0222](../../evidence/ui/T-0222.md)).

**Full rewrite.** Protect and remove-protection are full rewrites. They flatten
incremental revisions and invalidate any signature that covered earlier bytes;
[ADR 0010](../../decisions/0010-document-session-and-write-path.md) point 5
requires the UI to say so before the operation. An already-encrypted document
is rejected as input, and a plaintext document is rejected by
`remove_protection`, so ciphertext is never encrypted twice and no silent
downgrade is possible ([T-0238](../../evidence/hardening/T-0238.md)).

## Signatures: integrity is not identity

`pdfxi-security` produces a real detached CMS/PKCS#7 signature over the PDF byte
range: `SignedData` version 1, SHA-256 digest, `sha256WithRSAEncryption`,
`contentType`/`signingTime`/`messageDigest` signed attributes, `/Filter
/Adobe.PPKLite`, `/SubFilter /adbe.pkcs7.detached`, a reserved `/Contents`
window and an invisible signature widget registered in `/Annots` and
`/AcroForm` ([T-0241](../../evidence/hardening/T-0241.md)). Signing and
verification are tested on a real PDF, with negative tests for a flipped byte
inside the range, appended bytes, a corrupted signature value, a certificate
that does not match the signature, bad PEM input, a missing signature and an
undersized reservation. An independent .NET `SignedCms` / RSA / SHA-256 check
confirms the RSA signature over the signed attributes and that the
`messageDigest` equals SHA-256 over the declared ranges
([T-0241 §5](../../evidence/hardening/T-0241.md)).

**A passing check proves the bytes are unmodified under the embedded
certificate.** It proves that the file has a signature dictionary and a
four-integer `/ByteRange` starting at zero, that `/Contents` decodes to a CMS
with one signer using SHA-256 and RSA PKCS#1 v1.5, that the RSA signature
verifies over the signed attributes with the embedded signer certificate whose
issuer and serial match, and that the digest matches the ranges. Verification
also reports whether the ranges cover the whole file, how many bytes follow the
signature, the subject, and the SHA-256 fingerprint
([T-0241 §4](../../evidence/hardening/T-0241.md)).

**It does not prove who signed, or that the signer should be trusted:**

- certificate-chain trust is `None` — the PDF signature path consults no trust
  anchors, so it does not return `Some(true)`;
- no revocation source is checked: no CRL, no OCSP;
- `signingTime` is asserted by the signer; there is no RFC 3161 timestamp token,
  so backdating is undetectable;
- the identity is a caller-supplied PEM key — there is no PFX/PKCS#12 import, no
  Windows certificate store and no user-facing key protection;
- there is no `/Perms`/DocMDP certification, no field lock, one signature per
  revision, and no visible signature appearance;
- the test identity is self-signed (`CN=PDFXI Test Signer, O=PDFXI`), not a
  public certificate.

All of this is stated in [T-0241 §4](../../evidence/hardening/T-0241.md);
`SEC-005` is the row. A verification result therefore has two separate parts:
integrity, which PDFXI can assert when the digest matches, and identity/trust,
which this build cannot assert and which is therefore unverified. There is no
signature panel in the viewer — the **Sign** command reports that it is not
available — so nothing in this build presents a signer as trusted or identified.
Timestamp and revocation support are not claimed for this build: `SEC-006` and
`SEC-007` remain `model` rows, and the trust-anchor, Windows-store, RFC 3161 and
revocation work is scoped in T-0267, which has not landed. PFX/PKCS#12 identity
is scoped in T-0249; it is not available in this build, its task is still
`queued`, and it has no committed evidence file.

**What later edits do to a signature.** A mutation is an incremental append, so
the earlier revision's byte range is never rewritten and an existing signature
stays verifiable. Redaction, sanitize, optimize, protect and remove-protection
are full rewrites; they invalidate signatures by construction and the UI must
say so ([ADR 0010](../../decisions/0010-document-session-and-write-path.md)
point 3 and its consequences,
[T-0238 §5](../../evidence/hardening/T-0238.md)). Verify a signature in an
independent reader when identity or trust matters; PDFXI's own result is an
integrity result.

## Redaction: removal, not covering

`pdfxi_edit::redact_document` operates on a real `pdfxi_core::Document`. A mark
is a page and a rectangle; removal is by content, not by painting a box over it
([T-0237](../../evidence/hardening/T-0237.md)).

**Text operators are rewritten glyph by glyph.** Each marked page's decoded
content stream is tokenized, text state (`q`/`Q`, `cm`, `BT`/`ET`, `Tm`/`Td`/
`TD`/`T*`, `Tf`, `TL`, `Tc`, `Tw`, `Tz`) is tracked, and `Tj`, `TJ`, `'` and `"`
are analysed with the page's real font resources and widths. A glyph is removed
when its transformed text-space box shares positive area with the mark. Kept
glyphs keep their original encoded bytes inside a `TJ` array; removed glyphs
become numeric adjustments of the same advance, so surviving text does not slide
into the redacted area and no font is re-encoded. A partial run splits at glyph
boundaries, and the test marks the middle of `ABCDEFGHIJ`, leaving `ABCJ`. If the
font resource is missing, the size or horizontal scale is zero, or an advance is
non-finite, the whole operator is neutralised and reported `whole: true,
fallback: true` rather than leaving part of a secret
([T-0237](../../evidence/hardening/T-0237.md)).

**Annotations, links, fields and metadata are removed.** An annotation whose
`/Rect` intersects a mark is replaced with `null` when its class is enabled:
ordinary annotations by `remove_annotations`, `/Link` by `remove_links`, form
widgets by `remove_form_fields`. Indirect objects reachable from `/AP` are also
nulled, so appearance streams do not survive. Removed widgets are filtered out
of `/AcroForm /Fields`, emptied parent fields are nulled, and `/AcroForm /CO`
drops references to removed fields and widgets. `remove_metadata` clears the
`/Info` title, author, subject, keywords, creator and producer and removes the
catalog `/Metadata` XMP stream
([T-0237](../../evidence/hardening/T-0237.md)).

**Image pixels are destroyed in place.** For each `Do` naming an image XObject
that intersects a mark, the mark is mapped into the image's normalised square
through the inverse CTM and converted to a pixel rectangle. Covered pixels are
filled opaque black and an `/SMask` has its alpha set to 255 in the same region;
the sample buffer is re-encoded as Flate in the same object, replacing the
original compressed bytes. Covered formats at 8 bits per component are
DeviceGray, DeviceRGB, Indexed and DCTDecode JPEG; ICCBased profiles with one or
three components are accepted and filled with zero samples. A 4-component
ICCBased CMYK profile is rejected because zero samples mean no ink, not black
([T-0237](../../evidence/hardening/T-0237.md)).

**Unsupported content fails closed.** With `keep_images = false` — the default —
the whole request returns `UnsupportedImage` before anything is staged for:
JPXDecode, CCITTFaxDecode, JBIG2Decode and unknown filters; Separation,
DeviceN, ImageMask, `/Decode` and non-8-bit samples; ICCBased with more than
four components or a 4-component CMYK profile; an inline image (`BI … ID … EI`);
an intersecting form XObject; a missing resource name, an unresolvable
reference, a direct object or a non-stream object; corrupt sample buffers; and
an image over 100 megapixels or 64 MiB of decoded samples
(`ImageLimitExceeded`). Validation, decoding, filling and re-encoding all happen
in a preparation phase before any page content or image object is staged, so a
rejected request does not write a half-redacted stream. With `keep_images =
true` an intersecting image or form is reported in `surviving_spans` and `clean`
is false; no error is returned, so the caller must read the report
([T-0237](../../evidence/hardening/T-0237.md)).

**The operation is a full rewrite, never an incremental append.** The first
existing `/Contents` stream object is overwritten with the rewritten combined
stream and additional content streams are staged as `null`; leaving the old
stream unreferenced would still leak its bytes. Saving writes every live object.
This is [ADR 0010](../../decisions/0010-document-session-and-write-path.md)
point 5: a redacted byte range must not remain recoverable, so history is
cleared and the revision lineage starts again.

**You must verify before treating a file as clean.** `verify_redaction` performs
a full save, reparses the saved bytes and checks three independent surfaces:
page text re-extraction for every secret, a byte scan of the stored file and of
every text-candidate stream in UTF-8 and UTF-16LE with Flate decompression, and
surviving annotation/field rectangles against the marks. A text-candidate Flate
stream that cannot be decompressed is reported `UnreadableStream`, so the result
is fail-closed. `clean` is true only when there are no survivors, and a test
places a secret outside the mark to prove the verifier is not always-true
([T-0237](../../evidence/hardening/T-0237.md)). The caller must pass the real
secrets and require `clean`, and must treat an `UnsupportedImage` error as "not
redacted", not as a warning.

Known limits of this implementation, all recorded in
[T-0237](../../evidence/hardening/T-0237.md):

- form XObjects are not recursed and inline images are not decoded; both fail
  closed. Recursion and inline-image decoding are T-0268, which has not landed,
  so the fail-closed behaviour is what this build does;
- the byte scan deliberately excludes image streams, so image redaction rests
  on the supported-format checks and the image tests, not on the secret scan;
- ICCBased and Indexed handling is narrow (no indirect Indexed lookup, no
  CMYK fill) and fails closed rather than guessing;
- broader sanitisation — thumbnails, optional content, revision remnants,
  embedded files — and independent second-reader verification are T-0257, which
  has not landed;
- there is no viewer or CLI entry point; `EDIT-013` is a `library` row.

## Digital IDs

`SEC-004` is a `model` row for local digital-ID handling: a validated PKCS#8 key
shape, a key/certificate algorithm match, a deterministic label-keyed store with
caller-clocked validity (`Expired` / `NotYetValid` / `Valid`), duplicate
rejection, rotation, and a password-protected store envelope built from the
existing AES-256-CBC and SHA-256 primitives with a constant-time HMAC check. A
wrong password is indistinguishable from tampering. It performs no filesystem,
network or wall-clock access. PKCS#12/PFX bundle parsing, an OS keystore or HSM
key handle, and any management UI are outside this row and not available in this
build ([T-0143](../../evidence/security/T-0143.md), `SEC-004`).

## Sanitization

`SEC-008` builds category-based plans for metadata, embedded files, annotations,
JavaScript and form values over a real `Document`, with deterministic ordering
and strict/permissive verification, but it has no caller. `SEC-009` scrubs the
`/Info` dictionary and XMP metadata copy-on-write as a model; real object and
stream deletion is not wired to a document writer. Removing hidden information
as an end-user operation, and verifying it with a second reader, is T-0257, which
has not landed. The redaction API above does not invoke the sanitizer; its own
`remove_metadata` flag is the only metadata removal it performs
([T-0237](../../evidence/hardening/T-0237.md)).

## Untrusted input and runtime policy

- **Every PDF is hostile.** The parser, filters, content streams, forms and
  crypto code have bounds and typed errors, and five fuzz targets exercise the
  same entry points: `parser`, `filters`, `content`, `forms` and `crypto`. Inputs
  are capped at 16 KiB, decompression at 64 KiB, and no target panics or uses
  `unsafe`. The committed deterministic smoke runner runs them in the fast suite;
  a clean 24-hour run of every target is a release requirement, not something
  this build records ([fuzz/README.md](../../../fuzz/README.md),
  [RULES §8](../../../RULES.md)).
- **Input size cap.** A document larger than 2 GiB is rejected before loading,
  in the viewer and in the renderer, so a hostile or accidental huge file cannot
  cause an unbounded whole-file allocation
  (`crates/pdfxi-render/src/pdfium_backend.rs`,
  [T-0222](../../evidence/ui/T-0222.md)).
- **PDFium loading.** Release builds load `pdfium.dll` only from the directory
  of the running executable and never from the working directory, `PATH` or the
  system search path, so a planted DLL in an unrelated directory cannot be
  loaded. Debug builds additionally accept `PDFIUM_PATH` and the per-user
  developer cache; the working directory and system path are still excluded.
  The candidate list is unit-tested (`library_search_path_order`,
  `crates/pdfxi-render/src/pdfium_backend.rs`). The shipped DLL is third-party,
  unmodified and unsigned ([install](install.md)).
- **JavaScript runs in a capability-free sandbox.** The interpreter has no
  filesystem, network, process, environment, timer or global-object access. The
  only predefined functions are pure number/string helpers and `Math`; a script
  can reach the outside world only through an explicitly registered `HostObject`,
  and the default `NoHostObject` rejects every call. Runs are bounded by CPU
  steps, call depth, total calls, per-loop and total iterations, string/array/
  object size, AST depth, wall-clock and cancellation; a test stops a real
  `while (true)` with the wall-clock budget
  (`crates/pdfxi-automation/src/js/mod.rs`,
  `crates/pdfxi-automation/src/js/host.rs`,
  `crates/pdfxi-automation/src/js/tests.rs`).
- **Documents cannot run macros.** There is no macro facility: PDFXI executes no
  macro and no document-supplied native code, and the importers execute no active
  content. The HTML/CSS converter performs no script execution and no network
  fetch (`CREATE-005`), and the Office contract reports active content as a typed
  import error (`CREATE-006`) in [docs/features.toml](../../features.toml). The
  only script engine is the bounded JavaScript subset above. The CLI action
  executor is a staging-only adapter and mutates no document
  (`crates/pdfxi-cli/src/action.rs`).
- **Assistant sessions are restricted to five read-only document tools.** See
  [Privacy and local data](privacy.md) for the session ruleset, the version
  gate, the text caps and the live hostile-document result.

## Related pages

[Privacy and local data](privacy.md) · [Forms](forms.md) ·
[Authoring](authoring.md) · [Feature matrix](../../FEATURES.md) ·
[Installing PDFXI](install.md)
