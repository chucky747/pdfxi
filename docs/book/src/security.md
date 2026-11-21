# Security

PDFXI treats every PDF as hostile input. Security status is stated per
capability: some operations are reachable in the installed viewer, others are
implemented and tested on real documents but have no viewer command in this
build. A row is `verified` only when an end user can reach it
([ADR 0009](../../decisions/0009-feature-status-honesty.md)); every capability
below names its feature row, so the status is checkable in the
[feature matrix](../../FEATURES.md). Do not read "no viewer control" as "not
implemented": the tests cited here run on real PDF bytes, not on mocks.

## What the assistant sends and when

The assistant sends **nothing before you ask**. Opening a document or the panel,
and connecting, make no document request. At **Ask** it sends your question plus
a bounded excerpt of the open document's text to the local OpenCode service only.
That excerpt is wrapped in an `<untrusted-document-text>` fence with the closing
tag neutralised, capped at 24,000 characters, and taken from a 64-page / 256 KiB
snapshot ([ADR 0013](../../decisions/0013-assistant-document-grounding-fallback.md)).
The session uses a wildcard **deny** ruleset with five specific `pdfxi_*` allow
rules, read back and verified before use; no tool server is attached to the app
turn, so there is no shell, file, edit, or network tool path. PDFXI itself makes
**no network calls**: the only destination is the OpenCode service running on
this machine, over loopback ([ADR 0011](../../decisions/0011-ai-assistant-through-opencode.md)).
See [What the assistant sends](security/assistant-data.md).

## Signature trust

Verification proves **digest validity over the declared byte range** and reports
whether the range has **whole-file coverage** and whether bytes were appended
after the signature. The green `E-signed` chip requires all of
`Valid && bytes_match && covers_entire_file && !appended`. It proves the bytes
under the embedded certificate are unchanged, not who signed them: certificate
**chain trust is `None`**, so identity is **not verified** without a trust anchor;
no CRL/OCSP revocation and no RFC 3161 timestamp are checked
([T-0241](../../evidence/hardening/T-0241.md), T-0267 in progress).
See [Signatures: integrity and identity](security/signatures.md).

## Redaction limits

Redaction **removes content rather than covering it**, as a full rewrite to a new
file, and the result is verified by independently reparsing the saved bytes:
page-text re-extraction, a whole-file and stream byte scan, and surviving
rectangle checks. `clean` is true only when no secret survives. Two limits
matter: **vector paths and drawing graphics under a mark are not removed**, and
the viewer is **selection-only** (no area-drawing or search-and-redact tool).
Form XObjects are recursed and inline images decoded, but fonts, image streams,
and vector content remain outside the text/image scan
([T-0237](../../evidence/hardening/T-0237.md),
[T-0268](../../evidence/hardening/T-0268.md)). See [Redaction](security/redaction.md).

## What this build exposes

| Capability | Installed viewer | Implemented and tested | Row and source |
|---|---|---|---|
| Open an encrypted PDF | Masked password modal; wrong password retries with a typed message; Cancel opens no tab | Decrypt through `pdfxi-document` and `pdfxi_security::open_encrypted` | `SEC-001`, [T-0222](../../evidence/ui/T-0222.md) |
| Honour read-only rights | The tab is read-only when `/P` denies `MODIFY` | Permission mask decoded from the handler | `SEC-001`, [T-0222](../../evidence/ui/T-0222.md) |
| Save an encrypted PDF | Refused with a typed message; the tab stays open | `SessionError::EncryptedSaveUnsupported` | [T-0222](../../evidence/ui/T-0222.md), [ADR 0010](../../decisions/0010-document-session-and-write-path.md) |
| Sign and verify | **Live**: **Tools → Sign** card (also reachable from the command bar); `.pfx`/`.p12` import; typed bottom-right note with a visible overlay block; incremental signature; verification readout; green `✓ E-signed` chip when the badge predicate holds; encrypted documents refused | CMS/PKCS#7 over the byte range, independently verified | `SEC-005`, [T-0241](../../evidence/hardening/T-0241.md) — see [Signatures](security/signatures.md) |
| Apply or remove password protection | **Protect PDF** reports that it is not available; T-0263 is pending | `protect_document` / `remove_protection`, PDFium reopen | `SEC-012`, [T-0238](../../evidence/hardening/T-0238.md), [T-0256](../../evidence/hardening/T-0256.md) |
| Redaction marks | **Redact** / **Protect PDF** reports that it is not available; T-0263 is pending | Text, annotation/form/link and image-pixel removal plus a verifier | `EDIT-013`, [T-0237](../../evidence/hardening/T-0237.md) — see [Redaction](security/redaction.md) |
| Sanitize hidden information | Not available; library-level until T-0257/T-0263 | Category-plan model over a real `Document` | `SEC-008`, `SEC-009` — see [Redaction](security/redaction.md) |
| Assistant | Live: asks about the open document; question plus bounded fenced excerpt sent only at Ask time; no tool server attached | Locked OpenCode session client; excerpt grounding | `SEC-013`, [T-0254](../../evidence/hardening/T-0254.md), [ADR 0013](../../decisions/0013-assistant-document-grounding-fallback.md) — see [What the assistant sends](security/assistant-data.md) |

The "not available" messages for protection and redaction are the shell's
honest placeholder (`crates/pdfxi-app/src/shell/commands.rs`); those commands
report a status message instead of silently doing nothing. The **Sign** command
is no longer one of them.

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
([T-0222](../../evidence/ui/T-0222.md)). Signing an encrypted document is
refused for the same reason: the signature is a write, and the session cannot
re-encrypt the result.

**Full rewrite.** Protect and remove-protection are full rewrites. They flatten
incremental revisions and invalidate any signature that covered earlier bytes;
[ADR 0010](../../decisions/0010-document-session-and-write-path.md) point 5
requires the UI to say so before the operation. No viewer command exposes them
in this build (T-0263). An already-encrypted document is rejected as input, and
a plaintext document is rejected by `remove_protection`, so ciphertext is never
encrypted twice and no silent downgrade is possible
([T-0238](../../evidence/hardening/T-0238.md)).

## Signatures: integrity is not identity

The certificate **Sign** card is live. It imports a PKCS#12 bundle (`.pfx` or
`.p12`), signs the open document incrementally, and shows the verification
readout. It is separate from Fill & Sign; see [Signing and verification](signing.md)
for the workflow and [Signatures: integrity and identity](security/signatures.md)
for the guarantees and limits.

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
  anchors, so the readout says **identity not verified**;
- no revocation source is checked: no CRL, no OCSP;
- `signingTime` is asserted by the signer; there is no RFC 3161 timestamp token,
  so backdating is undetectable;
- the signing identity is the imported PKCS#12 bundle. There is no Windows
  certificate-store picker and no HSM integration;
- there is no `/Perms`/DocMDP certification, no field lock, and one signature
  per revision;
- the test identity is self-signed (`CN=PDFXI Test Signer, O=PDFXI`), not a
  public certificate.

The **Sign** card is the signature panel. It shows **Status**, **Signer**,
**SHA-256 fingerprint**, **asserted time**, **whole-file coverage**,
**appended-after-signature**, and **Chain**. A green **`✓ E-signed`** chip is
shown only when all of these are true:

```text
Valid && bytes_match && covers_entire_file && !appended
```

The badge is a statement about integrity and coverage, not about identity. Edit
the document, undo, redo, or switch tabs and the badge clears, because the result
no longer describes the current bytes. The readout is scoped to its tab, and
opening a document re-runs verification on its existing signatures.

The page shows a visible overlay block at the bottom-right, but the PDF itself
carries a typed `/Text` annotation as the signature appearance. Another reader
may show that object as a note rather than as a signature block; do not treat the
overlay as a portable appearance. See
[Signatures: integrity and identity](security/signatures.md).

**What later edits do to a signature.** A mutation is an incremental append, so
the earlier revision's byte range is never rewritten and an existing signature
stays verifiable. Appended bytes are reported, and the full-coverage condition
fails, so the `✓ E-signed` chip is not shown. Redaction, sanitize, optimize,
protect and remove-protection are full rewrites; they invalidate signatures by
construction and the UI must say so
([ADR 0010](../../decisions/0010-document-session-and-write-path.md) point 3 and
its consequences, [T-0238 §5](../../evidence/hardening/T-0238.md)). Verify a
signature in an independent reader when identity or trust matters; PDFXI's own
result is an integrity result.

## Redaction: removal, not covering

`pdfxi_edit::redact_document` operates on a real `pdfxi_core::Document`. A mark
is a page and a rectangle; removal is by content, not by painting a box over it
([T-0237](../../evidence/hardening/T-0237.md)). **Redaction is not reachable in
the viewer in this build**: Redact and Protect PDF report that they are not
available, and T-0263 is pending. The library below is real and tested, but no
user command invokes it. The full guarantees, recovery surface, and limits are
in [Redaction](security/redaction.md).

- **Text operators are rewritten glyph by glyph.** A glyph intersecting the mark
  is removed; kept glyphs keep their encoded bytes inside `TJ` adjustments, so
  surviving text does not slide into the redacted area. A missing or unusable
  font neutralises the whole operator rather than leaving part of a secret.
- **Annotations, links, form widgets, their appearance streams and — when asked —
  metadata are removed.** An annotation whose `/Rect` intersects a mark is
  nulled, and emptied form and `/CO` entries are cleaned up.
- **Supported image pixels are destroyed in place.** The sample buffer is
  re-encoded with the covered pixels filled, replacing the original compressed
  bytes. Unsupported image formats fail the whole request closed instead of
  silently leaving the image.
- **The operation is a full rewrite, never an incremental append.** The old
  content stream is overwritten and additional streams are nulled, so the
  previous bytes are not left reachable through an unreferenced object.

`verify_redaction` saves, reparses the saved bytes, re-extracts page text for
every secret, scans the stored file and every text-candidate stream in UTF-8 and
UTF-16LE with Flate decompression, and checks surviving annotation and field
rectangles against the marks. `clean` is true only when there are no survivors.
A text-candidate stream that cannot be decompressed is `UnreadableStream`, so
the result is fail-closed. The caller must pass the real secrets and require
`clean`. A redaction mark alone is not proof of erasure.

Known limits, also in [Redaction](security/redaction.md):

- **vector paths and graphics under a mark are not removed**; only text,
  annotations/forms/links, metadata and supported image samples are addressed;
- content outside the mark, and annotations/forms/images that do not intersect
  it, remain by design;
- form XObjects are not recursed and inline images are not decoded; both fail
  closed (T-0268 pending);
- the byte scan deliberately excludes image streams, so image redaction rests on
  the supported-format checks and the image tests;
- broader sanitisation — thumbnails, optional content, revision remnants,
  embedded files — and independent second-reader verification are T-0257, which
  has not landed. Sanitize is library-level until T-0257/T-0263 and is not
  invoked by redaction;
- there is no viewer or CLI entry point; `EDIT-013` is a `library` row.

## Digital IDs

`SEC-004` is a `model` row for a managed local digital-ID store: a validated
PKCS#8 key shape, a key/certificate algorithm match, a deterministic
label-keyed store with caller-clocked validity (`Expired` / `NotYetValid` /
`Valid`), duplicate rejection, rotation, and a password-protected store
envelope built from the existing AES-256-CBC and SHA-256 primitives with a
constant-time HMAC check. A wrong password is indistinguishable from tampering.
It performs no filesystem, network or wall-clock access. The **Sign** card's
PKCS#12 import is a signing-identity import, not this managed store: it has no
labels, rotation, or SEC-004 store envelope. A Windows certificate-store
picker, an HSM key handle, and management UI remain unavailable
([T-0143](../../evidence/security/T-0143.md), `SEC-004`).

## Sanitization

`SEC-008` builds category-based plans for metadata, embedded files, annotations,
JavaScript and form values over a real `Document`, with deterministic ordering
and strict/permissive verification, but it has no caller. `SEC-009` scrubs the
`/Info` dictionary and XMP metadata copy-on-write as a model; real object and
stream deletion is not wired to a document writer. Removing hidden information
as an end-user operation, and verifying it with a second reader, is T-0257, which
has not landed; the end-user surface is T-0263. The redaction API above does not
invoke the sanitizer; its own `remove_metadata` flag is the only metadata removal
it performs ([T-0237](../../evidence/hardening/T-0237.md)).

## Untrusted input and runtime policy

- **Every PDF is hostile.** The parser, filters, content streams, forms and
  crypto code have bounds and typed errors, and seven fuzz targets exercise the
  same entry points: `parser`, `filters`, `content`, `forms`, `crypto`,
  `redaction` and `edit`. Inputs are capped at 16 KiB, decompression at 64 KiB,
  and no target panics or uses `unsafe`. The committed deterministic smoke runner
  runs them in the fast suite; a clean 24-hour run of every target is a release
  requirement, not something this build records
  ([fuzz/README.md](../../../fuzz/README.md), [RULES §8](../../../RULES.md)).
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
- **Assistant runs in a locked, deny-all session with no tool execution path.**
  Grounding uses a bounded, fenced `<untrusted-document-text>` excerpt of the
  open document, not tool calls. The app turn attaches no tool server in this
  build, so the five `pdfxi_*` allow rules remain a library capability, not an
  execution path. See [What the assistant sends](security/assistant-data.md) and
  [Privacy and local data](privacy.md) for the ruleset, version gate, excerpt
  bounds and live hostile-document result.

## Related pages

[Signing and verification](signing.md) ·
[Signatures: integrity and identity](security/signatures.md) ·
[What the assistant sends](security/assistant-data.md) ·
[Redaction](security/redaction.md) · [Privacy and local data](privacy.md) ·
[Forms](forms.md) · [Authoring](authoring.md) ·
[Feature matrix](../../FEATURES.md) · [Installing PDFXI](install.md)
