# Security

Security-relevant code lives in `pdfxi-security`, with content-removal
transformations in `pdfxi-edit::redaction`. No secret is logged and no code uses
`unsafe`.

Status follows [ADR 0009](../../decisions/0009-feature-status-honesty.md): a row
is `verified` only when it runs on real document bytes and an end user can reach
it. The security rows are currently **`library`** or **`model`**, not
`verified`. They are tested implementations and building blocks, but none of
them is available as a viewer or CLI command. Do not describe encryption,
signing, redaction, or sanitization as shipped features.

| Row | Status | Meaning |
|---|---|---|
| SEC-001 | `library` | Standard security handler and encryption writer; no GUI/CLI caller |
| SEC-002 | `library` | AES/RC4 primitives and AES-256 object encryption; no caller |
| SEC-003 | `model` | Certificate encryption (AES-KW); recipient key unwrap is a provider boundary |
| SEC-004 | `model` | X.509 parsing and digital-ID store; no PKCS#12 import or management UI |
| SEC-005 | `model` | CMS SignedData subset; the RSA path is test-only, not a production signer |
| SEC-006 | `model` | Timestamp/trust model; no RFC 3161 client and no network |
| SEC-008 | `library` | Sanitizer over a real `Document`; no caller |
| SEC-009 | `model` | Metadata/XMP scrubbing model; not wired to a document writer |
| EDIT-013 | `model` | Redaction over content-stream fragments; no document, CLI, or GUI caller |

## Passwords and encryption

- `SEC-001` implements PDF standard-security revisions 2 through 6: MD5 key
  derivation, `/O` and `/U` computation and owner recovery, revision 5
  SHA-256, revision 6 hardened hashing, constant-time password comparison,
  owner-role bypass, and per-object keys for RC4, AESV2, and AESV3.
- `SEC-002` provides dependency-free AES-128/192/256 (CBC, PKCS#7), RC4 40/128,
  MD5, and SHA-256/384/512, checked against FIPS and NIST vectors, plus
  complete AES-256 (AESV3) object encryption for revisions 5 and 6.
- The viewer detects a password-protected document and reports that it is not
  supported. There is no password prompt and no end-to-end open of an encrypted
  document. `save_incremental()` also rejects `/Encrypt` sources.

`EncryptionMethod` in `pdfxi-security::encryption` names none, RC4-40/128,
AES-128/256.

## CMS signatures

`SEC-005` implements a deterministic CMS/PKCS#7 SignedData subset with
`encapContentInfo`, `signerInfos`, and signed attributes (`contentType`,
`messageDigest`, `signingTime`). A `SignatureProvider` trait keeps keys and
certificates out of the crate. Verification distinguishes tampered content,
tampered signed attributes, bad signatures, truncation, and unsupported
algorithms.

This is a model, not a signing feature. The RSA exponentiation used by the tests
is a test-only helper; there is no production RSA/PKCS#1 provider, no document
signing command, and no signature field in the viewer. `SEC-006` encodes and
decodes a `signingTime` attribute but requests no trusted timestamp and performs
no revocation checking.

## X.509 digital IDs

`SEC-004` provides strict DER certificate parsing, deterministic name display,
the common v3 extensions, caller-clocked validity, and caller-trusted chain
verification through a `CertificateVerifier` with a `TrustStore`. A
`DigitalIdStore` keeps private keys behind an opaque `KeyHandle` and never logs
secrets. PKCS#12/PFX import and a desktop management UI are not implemented.

## Redaction

`pdfxi-edit::redaction` matches `Tj`/`TJ`/`'`/`"` operators, recurses into Form
XObjects, strips intersecting annotations, paints an opaque rectangle, and
re-extracts text to verify that no residual remains. The intent is removal, not
covering, per RULES section 8.

The implementation is not wired to `pdfxi-core`, the CLI, or the viewer, so
`EDIT-013` is `model`. It cannot be applied to a document you open, and raster
image removal from redacted areas is not implemented. Do not use it as a
redaction tool yet.

## Sanitization

- `SEC-008` builds category-based plans for metadata, embedded files,
  annotations, JavaScript, and form values, with deterministic ordering and
  strict/permissive verification. It operates on a real `Document` but has no
  caller.
- `SEC-009` scrubs the Info dictionary and XMP metadata copy-on-write, with
  deterministic replace, remove, and selective strategies over an allowlist.
  Real PDF object/stream deletion and byte output are not wired.

## Untrusted input and runtime policy

- Every PDF is treated as hostile. The parser, filters, content streams, forms,
  and crypto code have fuzz targets; the fast suite includes malformed and
  real-world fixtures.
- **Input size cap:** a document larger than 2 GiB is rejected before loading,
  in both the viewer and the library. The decision is unit-tested.
- **PDFium loading:** release builds load `pdfium.dll` only from the directory
  of the running executable. The current directory, `Path`, and environment
  overrides are not searched in release, which prevents DLL planting next to a
  document. Developer builds additionally accept `PDFIUM_PATH` and the per-user
  cache.
- **JavaScript:** the engine has step, loop-iteration, and wall-clock budgets.
  A test runs a real `while (true)` loop with the other limits raised and
  asserts that the wall-clock budget stops it. The CLI action executor only
  stages inputs (`EchoOperations`), so this is not exposed as an end-user
  scripting surface.
- Paths used by attachments, exports, and future plugins must reject traversal
  and symlink escapes; that remains a requirement for the code that is not yet
  wired.

## Related pages

[Getting started](getting-started.md) · [Forms](forms.md) ·
[Standards and preflight](standards.md) · [Feature matrix](../../FEATURES.md)
